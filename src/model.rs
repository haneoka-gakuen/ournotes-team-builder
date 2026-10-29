//! Linear skill and played-order bounds used before exact scoring.
use crate::{
    live::{self, Input, Slot},
    num::{f, window_ms},
};
use std::collections::HashMap;
pub const RELATIVE_ERROR: f64 = 2e-6;
#[derive(Clone)]
pub struct Linear {
    pub prefix: Vec<f64>,
    pub perfect_prefix: Vec<f64>,
    pub total: f64,
    pub life_at: HashMap<usize, f64>,
    pub absolute_error: f64,
}
fn combo(table: &[(usize, f64)], count: usize) -> f64 {
    let mut value = 0.0;
    for &(required, cumulative) in table {
        if required > count {
            break;
        }
        value = cumulative;
    }
    f(value.min(1.0) + 1.0)
}
pub fn linear(input: &Input) -> Linear {
    let unit = input.adjustment * input.difficulty_factor * input.judge_frac[5];
    let mut prefix = vec![0.0; input.times.len() + 1];
    let mut sum = 0.0;
    for i in 0..input.times.len() {
        sum += unit * input.note_frac[i] * input.ap_combo[i];
        prefix[i + 1] = sum;
    }
    Linear {
        perfect_prefix: prefix.clone(),
        prefix,
        total: sum,
        life_at: (0..input.skill_times.len())
            .map(|e| (e, input.life_base))
            .collect(),
        absolute_error: input.times.len() as f64 + 1.0,
    }
}
pub fn linear_play(
    input: &Input,
    judgements: &[usize],
    high: bool,
    conversions: usize,
    factor_cap: f64,
    max_recovery: i32,
) -> Linear {
    let n = input.times.len();
    let unit = input.adjustment * input.difficulty_factor;
    let (mut prefix, mut perfect_prefix) = (vec![0.0; n + 1], vec![0.0; n + 1]);
    let mut gains = Vec::new();
    let (mut sum, mut perfect_sum) = (0.0, 0.0);
    let mut life = input.life_base;
    let (mut before, mut pending, mut last) = (0, 0, -1);
    let mut life_at = HashMap::new();
    let mut cursor = 0;
    let mut events: Vec<_> = (0..input.skill_times.len()).collect();
    events.sort_by_key(|&i| (input.skill_times[i], i));
    for (i, &judgement) in judgements.iter().enumerate() {
        let time = input.times[i];
        while cursor < events.len() && input.skill_times[events[cursor]] <= time {
            life_at.insert(events[cursor], if high { input.life_base } else { life });
            if high {
                life = (input.life_base * 2.0).min(life + max_recovery as f64);
            }
            cursor += 1;
        }
        if time != last {
            before = pending;
            last = time;
        }
        let life_factor = if life > 0.0 { 1.0 } else { input.life_onus };
        let base = unit * input.note_frac[i] * combo(&input.combo_table, before) * life_factor;
        sum += base * input.judge_frac[judgement];
        if judgement == 5 {
            perfect_sum += base * input.judge_frac[5];
        }
        if high && (judgement == 3 || judgement == 4) {
            gains.push(base * (input.judge_frac[5] - input.judge_frac[judgement]));
        }
        prefix[i + 1] = sum;
        perfect_prefix[i + 1] = perfect_sum;
        pending = if judgement <= 2 { 0 } else { pending + 1 };
        life = (life - input.damage[judgement]).max(0.0);
    }
    while cursor < events.len() {
        life_at.insert(events[cursor], if high { input.life_base } else { life });
        cursor += 1;
    }
    if high && conversions > 0 {
        gains.sort_by(|a, b| b.total_cmp(a));
        sum += factor_cap * gains.iter().take(conversions).fold(0.0, |sum, v| sum + v);
    }
    Linear {
        prefix,
        perfect_prefix,
        total: sum,
        life_at,
        absolute_error: n as f64 + 1.0,
    }
}
pub fn window_weight(prefix: &[f64], input: &Input, start: i32, length: i32) -> f64 {
    let a = input.times.partition_point(|&t| t < start);
    let b = input.times.partition_point(|&t| t < start + length);
    prefix[b] - prefix[a]
}
#[derive(Clone)]
pub struct Weights {
    pub mean: f64,
    pub best: f64,
    pub worst: f64,
    pub per_event: [f64; 5],
}
pub fn skill_weights(
    model: &Linear,
    input: &Input,
    skill: &Slot,
    include_cumulative: bool,
) -> Weights {
    let mut per_event = [0.0; 5];
    for effect in &skill.effects {
        if effect.kind == 2004 && !effect.judgements.contains(&5) {
            continue;
        }
        let factor = f(effect.delta as f64 / 100000.0);
        let length = window_ms(effect.seconds, skill.extension_ms);
        let prefix = if effect.kind == 2004 {
            &model.perfect_prefix
        } else {
            &model.prefix
        };
        for (event, &start) in input.skill_times.iter().enumerate().take(5) {
            if effect.gate.blocks(model.life_at[&event]) {
                continue;
            }
            per_event[event] += factor * window_weight(prefix, input, start, length);
        }
    }
    if include_cumulative {
        for factor in &skill.cumulative {
            let delta = f(factor.at_cap() as f64 / 100000.0);
            let length = factor.duration_ms();
            for (event, &start) in input.skill_times.iter().enumerate().take(5) {
                per_event[event] += delta * window_weight(&model.prefix, input, start, length);
            }
        }
    }
    let total = per_event.iter().fold(0.0, |sum, v| sum + v);
    Weights {
        mean: total / 5.0,
        best: per_event.iter().copied().fold(0.0, f64::max),
        worst: per_event.iter().copied().fold(f64::INFINITY, f64::min),
        per_event,
    }
}
pub fn conversion_weights(
    input: &Input,
    judgements: &[usize],
    skill: &Slot,
    factor_cap: f64,
) -> [f64; 5] {
    let mut out = [0.0; 5];
    let Some(convert) = &skill.convert else {
        return out;
    };
    let unit = input.adjustment * input.difficulty_factor;
    let perfect = input.judge_frac[5];
    for (event, &time) in input.skill_times.iter().enumerate().take(5) {
        let start = input.times.partition_point(|&t| t < time);
        let end = input.times.partition_point(|&t| {
            t < time
                + window_ms(
                    skill.effects.first().map(|e| e.seconds).unwrap_or(5.0),
                    skill.extension_ms,
                )
        });
        let mut values: Vec<_> = (start..end)
            .filter(|&i| convert.judgements.contains(&judgements[i]))
            .map(|i| unit * input.note_frac[i] * 2.0 * perfect)
            .collect();
        values.sort_by(|a, b| b.total_cmp(a));
        let limit = convert.limit.unwrap_or(f64::INFINITY);
        let mut sum = 0.0;
        for (i, &value) in values.iter().enumerate() {
            if i as f64 >= limit {
                break;
            }
            sum += value;
        }
        out[event] = sum * factor_cap;
    }
    out
}
#[derive(Clone)]
struct Window {
    start: usize,
    end: usize,
    general: f64,
    perfect: f64,
}
#[derive(Clone)]
struct Contribution {
    value: f64,
    high_value: f64,
    windows: Vec<Window>,
}
struct LifeBounds {
    raw: Vec<f64>,
    perfect: Vec<f64>,
    penalty: Vec<f64>,
    gates: HashMap<usize, f64>,
    contributions: HashMap<(usize, usize), Contribution>,
}
pub struct PlayBounds {
    pub raw: Vec<f64>,
    pub perfect_low: Vec<f64>,
    pub perfect_base: Vec<f64>,
    pub damage: Vec<(usize, f64)>,
    pub event_index: Vec<usize>,
    life: HashMap<[i32; 5], LifeBounds>,
}
impl PlayBounds {
    pub fn new(input: &Input, judgements: &[usize]) -> Self {
        let n = input.times.len();
        let unit = input.adjustment * input.difficulty_factor;
        let (mut raw, mut perfect_low, mut perfect_base) =
            (vec![0.0; n + 1], vec![0.0; n + 1], vec![0.0; n]);
        let mut damage = Vec::new();
        let (mut before, mut pending, mut last) = (0, 0, -1);
        for (i, &judgement) in judgements.iter().enumerate() {
            let time = input.times[i];
            if time != last {
                before = pending;
                last = time;
            }
            let base = unit * input.note_frac[i] * combo(&input.combo_table, before);
            raw[i + 1] = raw[i] + base * input.judge_frac[judgement];
            perfect_low[i + 1] = perfect_low[i]
                + if judgement == 5 {
                    base * input.judge_frac[5]
                } else {
                    0.0
                };
            perfect_base[i] = base * input.judge_frac[5];
            if input.damage[judgement] != 0.0 {
                damage.push((i, input.damage[judgement]));
            }
            pending = if judgement <= 2 { 0 } else { pending + 1 };
        }
        Self {
            raw,
            perfect_low,
            perfect_base,
            damage,
            event_index: input
                .skill_times
                .iter()
                .map(|&time| input.times.partition_point(|&t| t < time))
                .collect(),
            life: HashMap::new(),
        }
    }
    pub fn order(
        &mut self,
        input: &Input,
        judgements: &[usize],
        slots: &[Slot],
        slot_ids: &[usize],
        order: &[usize; 5],
        power: f64,
    ) -> (f64, f64) {
        if slots.iter().flat_map(|slot| &slot.cumulative)
            .any(|factor| factor.requires_unbounded_bounds(input.times.len()))
        {
            return (f64::NEG_INFINITY, f64::INFINITY);
        }
        let n = input.times.len();
        let key = order.map(|slot| slots.get(slot).map(|s| s.recovery).unwrap_or(0));
        let bounds = self.life.entry(key).or_insert_with(|| {
            let lives = live::lives(input, judgements, order, slots);
            let (mut raw, mut perfect, mut penalty) =
                (vec![0.0; n + 1], vec![0.0; n + 1], vec![0.0; n]);
            for i in 0..n {
                let factor = if lives.note[i] > 0.0 {
                    1.0
                } else {
                    input.life_onus
                };
                penalty[i] = f(factor);
                raw[i + 1] = raw[i] + factor * (self.raw[i + 1] - self.raw[i]);
                perfect[i + 1] =
                    perfect[i] + factor * (self.perfect_low[i + 1] - self.perfect_low[i]);
            }
            LifeBounds {
                raw,
                perfect,
                penalty,
                gates: lives.gate,
                contributions: HashMap::new(),
            }
        });
        let events = input.skill_times.len().min(5);
        let mut contributions = Vec::with_capacity(events);
        let mut low = bounds.raw[n];
        let mut converting = false;
        for (event, &slot_index) in order.iter().enumerate().take(events) {
            let Some(slot) = slots.get(slot_index) else {
                continue;
            };
            let contribution = bounds
                .contributions
                .entry((event, slot_ids[slot_index]))
                .or_insert_with(|| {
                    let start = self.event_index[event];
                    let mut windows = Vec::new();
                    let mut total = 0.0;
                    let mut high_total = 0.0;
                    for effect in &slot.effects {
                        if effect.gate.blocks(bounds.gates[&event])
                            || (effect.kind == 2004 && !effect.judgements.contains(&5))
                        {
                            continue;
                        }
                        let end = input.times.partition_point(|&t| {
                            t < input.skill_times[event]
                                + window_ms(effect.seconds, slot.extension_ms)
                        });
                        let factor = f(effect.delta as f64 / 100000.0);
                        let prefix = if effect.kind == 2000 {
                            &bounds.raw
                        } else {
                            &bounds.perfect
                        };
                        if end > start {
                            let contribution = factor * (prefix[end] - prefix[start]);
                            total += contribution;
                            high_total += contribution;
                        }
                        windows.push(if effect.kind == 2000 {
                            Window {
                                start,
                                end,
                                general: factor,
                                perfect: 0.0,
                            }
                        } else {
                            Window {
                                start,
                                end,
                                general: 0.0,
                                perfect: factor,
                            }
                        });
                    }
                    for factor in &slot.cumulative {
                        let end = input.times.partition_point(|&t| {
                            t < input.skill_times[event]
                                + factor.duration_ms()
                        });
                        let delta = f(factor.at_cap() as f64 / 100000.0);
                        if end > start {
                            high_total += delta * (bounds.raw[end] - bounds.raw[start]);
                        }
                        windows.push(Window {
                            start,
                            end,
                            general: delta,
                            perfect: 0.0,
                        });
                    }
                    Contribution {
                        value: total,
                        high_value: high_total,
                        windows,
                    }
                })
                .clone();
            low += contribution.value;
            if slot.convert.is_some() {
                converting = true;
            }
            contributions.push(contribution);
        }
        let mut high = contributions
            .iter()
            .fold(bounds.raw[n], |sum, contribution| {
                sum + contribution.high_value
            });
        if converting {
            let windows: Vec<_> = contributions
                .iter()
                .flat_map(|c| c.windows.iter())
                .collect();
            let perfect = input.judge_frac[5];
            for (event, &slot_index) in order.iter().enumerate().take(events) {
                let Some(slot) = slots.get(slot_index) else {
                    continue;
                };
                let Some(convert) = &slot.convert else {
                    continue;
                };
                let start = self.event_index[event];
                let end = input.times.partition_point(|&t| {
                    t < input.skill_times[event]
                        + window_ms(
                            slot.effects.first().map(|e| e.seconds).unwrap_or(5.0),
                            slot.extension_ms,
                        )
                        + 17
                });
                let mut gains = Vec::new();
                for (i, &judgement) in judgements.iter().enumerate().take(end).skip(start) {
                    if !convert.judgements.contains(&judgement) {
                        continue;
                    }
                    let (mut general, mut perfect_up) = (1.0, 0.0);
                    for window in &windows {
                        if window.start <= i && i < window.end {
                            general += window.general;
                            perfect_up += window.perfect;
                        }
                    }
                    let base = self.perfect_base[i] / perfect;
                    gains.push(
                        bounds.penalty[i]
                            * base
                            * ((perfect - input.judge_frac[judgement]) * general
                                + perfect * perfect_up),
                    );
                }
                gains.sort_by(|a, b| b.total_cmp(a));
                let limit = convert.limit.unwrap_or(f64::INFINITY);
                for (i, &gain) in gains.iter().enumerate() {
                    if i as f64 >= limit {
                        break;
                    }
                    high += gain;
                }
            }
        }
        let divisor = input.converted_count as f64;
        (
            (power * low / divisor * (1.0 - RELATIVE_ERROR) - (n as f64 + 1.0)).max(0.0),
            power * high / divisor * (1.0 + RELATIVE_ERROR),
        )
    }
}
