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
    pub ignore_gates: bool,
    pub nominal_cumulative: bool,
    pub conversion_combo: Vec<f64>,
    pub release_windows: Vec<ReleaseWindow>,
}
#[derive(Clone)]
pub struct ReleaseWindow { event: Option<usize>, from: i32, until: i32, sources: u8, target: usize }
pub fn prepare_releases(input: &Input, slots: &[Slot]) -> Vec<ReleaseWindow> {
    let mut seen = std::collections::HashSet::new();
    let mut windows = Vec::new();
    for slot in slots {
        let Some(convert) = &slot.convert else {
            continue;
        };
        let duration = convert.seconds.map_or_else(
            || {
                window_ms(
                    slot.effects.first().map(|e| e.seconds).unwrap_or(5.0),
                    slot.extension_ms,
                )
            },
            |seconds| window_ms(seconds, 0.0),
        );
        let sources = convert
            .judgements
            .iter()
            .fold(0u8, |mask, &grade| mask | (1 << grade));
        for event in 0..if convert.at_start {
            1
        } else {
            input.skill_times.len().min(5)
        } {
            let id = (!convert.at_start).then_some(event);
            let from = input
                .clock
                .frame_time(id.map_or(0, |i| input.skill_times[i]));
            let until = input.clock.frame_time(from.saturating_add(duration));
            if seen.insert((id, from, until, sources, convert.judgement)) {
                windows.push(ReleaseWindow {
                    event: id,
                    from,
                    until,
                    sources,
                    target: convert.judgement,
                });
            }
        }
    }
    windows
}
fn release_end(
    model: &Linear,
    input: &Input,
    slot: &Slot,
    effect: &live::Effect,
    event: usize,
    start: i32,
    length: i32,
) -> i32 {
    let end = start.saturating_add(length);
    let Some(grades) = input.judgements.as_ref() else {
        return end;
    };
    if effect.release_judgements.is_empty() {
        return end;
    }
    let mask = effect
        .release_judgements
        .iter()
        .fold(0u8, |mask, &grade| mask | (1 << grade));
    let relaxed = |candidate| {
        if model.release_windows.iter().any(|window| {
            window.event != Some(event)
                && window.until >= start
                && window.from < candidate
                && window.sources & mask != 0
                && mask & (1 << window.target) == 0
        }) {
            end
        } else {
            candidate
        }
    };
    let convert = slot
        .convert
        .as_ref()
        .filter(|convert| mask & (1 << convert.judgement) == 0);
    let mut budget = convert.map_or(0.0, |convert| {
        convert.limit.unwrap_or(f64::INFINITY).ceil().max(1.0)
    });
    let from = convert.map_or(0, |convert| {
        input
            .clock
            .frame_time(if convert.at_start { 0 } else { start })
    });
    let until = convert.map_or(0, |convert| {
        let duration = convert.seconds.map_or_else(
            || {
                window_ms(
                    slot.effects.first().map(|e| e.seconds).unwrap_or(5.0),
                    slot.extension_ms,
                )
            },
            |seconds| window_ms(seconds, 0.0),
        );
        input.clock.frame_time(from.saturating_add(duration))
    });
    let after_execute = input
        .clock
        .frame_time(input.clock.frame_time(start).saturating_add(1));
    let deadline = input.clock.frame_time(end.saturating_add(1));
    let mut first = input
        .times
        .partition_point(|&time| input.clock.frame_time(time) <= after_execute);
    while first < input.times.len() {
        let frame = input.clock.frame_time(input.times[first]);
        if frame > deadline {
            break;
        }
        let after = first
            + input.times[first..].partition_point(|&time| input.clock.frame_time(time) <= frame);
        let mut last = None;
        let mut unavoidable = false;
        for (i, &grade) in grades.iter().enumerate().take(after).skip(first) {
            if mask & (1 << grade) == 0 {
                continue;
            }
            last = Some(i);
            if budget > 0.0
                && input.times[i] > from
                && input.times[i] <= until
                && convert.is_some_and(|convert| convert.judgements.contains(&grade))
            {
                budget -= 1.0;
            } else {
                unavoidable = true;
            }
        }
        if unavoidable {
            return relaxed(end.min(input.times[last.unwrap()]));
        }
        first = after;
    }
    end
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
        ignore_gates: false,
        nominal_cumulative: false,
        conversion_combo: Vec::new(),
        release_windows: Vec::new(),
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
        ignore_gates: false,
        nominal_cumulative: false,
        conversion_combo: Vec::new(),
        release_windows: Vec::new(),
    }
}
pub fn relaxed_play(input: &Input, grades: &[usize], slots: &[Slot], width: usize) -> Linear {
    let n = input.times.len();
    let mut repairs = Vec::new();
    let mut heals = Vec::new();
    for column in 1..width {
        let mut repair = 0usize;
        let mut heal = 0i32;
        for slot in slots.iter().skip(column).step_by(width) {
            heal = heal.max(slot.recovery);
            if let Some(convert) = &slot.convert {
                if convert.judgements.iter().any(|&grade| grade <= 2) {
                    repair = repair.max(convert.limit.map_or(n, |limit| {
                        if limit.is_finite() {
                            limit.ceil().max(1.0).min(n as f64) as usize
                        } else {
                            n
                        }
                    }));
                }
            }
        }
        repairs.push(repair);
        heals.push(heal);
    }
    relaxed_play_limits(input, grades, repairs, heals)
}
pub(crate) fn relaxed_play_limits(
    input: &Input,
    grades: &[usize],
    mut repairs: Vec<usize>,
    mut heals: Vec<i32>,
) -> Linear {
    let n = input.times.len();
    repairs.sort_unstable_by(|a, b| b.cmp(a));
    heals.sort_unstable_by(|a, b| b.cmp(a));
    let budget = repairs.into_iter().take(5).sum::<usize>().min(n);
    let mut healing = [0.0; 6];
    for (i, heal) in heals.into_iter().take(5).enumerate() {
        healing[i + 1] = healing[i] + heal as f64;
    }
    for i in 1..=5 {
        healing[i] = healing[i].max(healing[i - 1]);
    }
    let mut damage_order: Vec<_> = (1..=6).collect();
    damage_order.sort_unstable_by(|&a, &b| input.damage[b].total_cmp(&input.damage[a]));
    let resets: Vec<_> = grades
        .iter()
        .enumerate()
        .filter_map(|(i, &grade)| (grade <= 2).then_some(i))
        .collect();
    let mut counts = [0usize; 7];
    let mut damage_prefix = vec![0.0; n + 1];
    let mut total_damage = 0.0;
    for (i, &grade) in grades.iter().enumerate() {
        counts[grade] += 1;
        total_damage += input.damage[grade];
        let mut remaining = budget;
        let mut saved = 0.0;
        for &kind in &damage_order {
            let used = remaining.min(counts[kind]);
            saved += used as f64 * input.damage[kind];
            remaining -= used;
        }
        damage_prefix[i + 1] = (total_damage - saved).max(0.0);
    }
    let magnitude = input.life_base + total_damage + healing[5];
    let exact_life = input.life_base.fract() == 0.0
        && input.damage.iter().all(|damage| damage.fract() == 0.0)
        && magnitude < 2.0_f64.powi(52);
    let life_slack = if exact_life {
        0.0
    } else {
        (n as f64 * 16.0 + 64.0) * f64::EPSILON * magnitude
    };
    let heal_at = |time: i32| {
        healing[input
            .skill_times
            .iter()
            .take(5)
            .filter(|&&start| start <= time)
            .count()]
    };
    let mut prefix = vec![0.0; n + 1];
    let mut perfect_prefix = vec![0.0; n + 1];
    let mut conversion_combo = Vec::with_capacity(n);
    let (mut before, mut last, mut dead) = (0, -1, None);
    let unit = input.adjustment * input.difficulty_factor;
    for (i, &grade) in grades.iter().enumerate() {
        let time = input.times[i];
        if time != last {
            before = i;
            last = time;
        }
        let count = resets.partition_point(|&index| index < before);
        let combo_count = if count > budget {
            before - resets[count - budget - 1] - 1
        } else {
            before
        };
        let upper_life = input.life_base + heal_at(time) - damage_prefix[i + 1] + life_slack;
        if upper_life <= 0.0 && dead.is_none() {
            dead = Some(i);
        }
        let penalty = if dead.is_some() { input.life_onus } else { 1.0 };
        let multiplier = combo(&input.combo_table, combo_count) * penalty;
        conversion_combo.push(multiplier);
        let value = unit * input.note_frac[i] * multiplier;
        prefix[i + 1] = prefix[i] + value * input.judge_frac[grade];
        perfect_prefix[i + 1] = perfect_prefix[i]
            + if grade == 5 {
                value * input.judge_frac[5]
            } else {
                0.0
            };
    }
    let life_at = input
        .skill_times
        .iter()
        .enumerate()
        .map(|(event, &time)| {
            let frame = input.clock.frame_time(time);
            let after = input.times.partition_point(|&note| note <= frame);
            let life = if dead.is_some_and(|index| index < after) {
                0.0
            } else {
                (input.life_base + heal_at(frame) - damage_prefix[after] + life_slack)
                    .clamp(0.0, input.life_base * 2.0)
            };
            (event, life)
        })
        .collect();
    Linear {
        total: prefix[n],
        prefix,
        perfect_prefix,
        life_at,
        absolute_error: n as f64 + 1.0,
        ignore_gates: true,
        nominal_cumulative: false,
        conversion_combo,
        release_windows: Vec::new(),
    }
}
pub fn window_weight(prefix: &[f64], input: &Input, start: i32, length: i32) -> f64 {
    let a = input.times.partition_point(|&t| t < start);
    let b = input.times.partition_point(|&t| t < start + length);
    prefix[b] - prefix[a]
}
pub(crate) fn cumulative_upper(
    prefix: &[f64],
    input: &Input,
    factor: &live::Cumulative,
    start: i32,
    grades: Option<&[usize]>,
) -> f64 {
    if factor.judgements.is_empty() {
        return 0.0;
    }
    let end = start.saturating_add(factor.bound_duration_ms());
    let activation = input.clock.frame_time(start);
    let mut first = input
        .times
        .partition_point(|&time| input.clock.frame_time(time) < activation);
    let mut cursor = input.times.partition_point(|&time| time < start);
    let finish = input.times.partition_point(|&time| time < end);
    let (mut count, mut applied, mut total) = (0, 0, 0.0);
    while first < input.times.len() {
        let frame = input.clock.frame_time(input.times[first]);
        if frame > end {
            break;
        }
        let after = first
            + input.times[first..].partition_point(|&time| input.clock.frame_time(time) <= frame);
        count += (first..after)
            .filter(|&index| grades.is_none_or(|grades| factor.judgements.contains(&grades[index])))
            .count() as i32;
        let mill = factor.mill(count);
        if mill != applied {
            let time = if frame == activation { start } else { frame };
            let next = input.times.partition_point(|&note| note < time).min(finish);
            total += f(applied as f64 / 100000.0) * (prefix[next] - prefix[cursor]);
            cursor = next;
            applied = mill;
        }
        first = after;
    }
    total + f(applied as f64 / 100000.0) * (prefix[finish] - prefix[cursor])
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
    let mut gated = Vec::new();
    for (index, effect) in skill.effects.iter().enumerate() {
        let mut contribution = [0.0; 5];
        if effect.kind == 2004 && !effect.judgements.contains(&5) && !model.ignore_gates {
            continue;
        }
        let factor = f(effect.delta as f64 / 100000.0);
        let length = window_ms(effect.seconds, skill.extension_ms);
        let prefix = if effect.kind == 2004
            && (!model.ignore_gates
                || effect
                    .judgements
                    .iter()
                    .all(|&grade| grade == 5 || grade == 6 || input.judge_frac[grade] == 0.0))
        {
            &model.perfect_prefix
        } else {
            &model.prefix
        };
        for (event, &start) in input.skill_times.iter().enumerate().take(5) {
            if !model.ignore_gates && effect.gate.blocks(model.life_at[&event]) {
                continue;
            }
            if model.ignore_gates
                && effect.gate.kind == 1
                && effect.gate.positive
                && effect.gate.blocks(model.life_at[&event])
            {
                continue;
            }
            let length = if model.ignore_gates {
                release_end(model, input, skill, effect, event, start, length) - start
            } else {
                length
            };
            contribution[event] = factor * window_weight(prefix, input, start, length);
        }
        if model.ignore_gates && effect.gate.kind == 1 {
            gated.push((index, contribution));
        } else {
            for event in 0..5 {
                per_event[event] += contribution[event];
            }
        }
    }
    if !gated.is_empty() {
        for (event, total) in per_event
            .iter_mut()
            .enumerate()
            .take(input.skill_times.len())
        {
            let upper = model.life_at[&event];
            let values = std::iter::once(0.0).chain(
                gated
                    .iter()
                    .map(|&(index, _)| skill.effects[index].gate.value)
                    .filter(|&value| value >= 0.0 && value <= upper),
            );
            let best = values
                .map(|life| {
                    gated
                        .iter()
                        .filter(|&&(index, _)| !skill.effects[index].gate.blocks(life))
                        .map(|(_, values)| values[event])
                        .fold(0.0_f64, |sum, value| (sum + value).next_up())
                })
                .fold(0.0_f64, f64::max);
            *total = (*total + best).next_up();
        }
    }
    if include_cumulative {
        for factor in &skill.cumulative {
            let delta = f(factor.at_cap() as f64 / 100000.0);
            let length = factor.bound_duration_ms();
            for (event, &start) in input.skill_times.iter().enumerate().take(5) {
                per_event[event] += if model.nominal_cumulative {
                    if factor.judgements.contains(&5) {
                        cumulative_upper(&model.prefix, input, factor, start, None)
                    } else {
                        0.0
                    }
                } else if model.ignore_gates {
                    cumulative_upper(&model.prefix, input, factor, start, None)
                } else {
                    delta * window_weight(&model.prefix, input, start, length)
                };
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
    perfect_cap: Option<f64>,
    conversion_combo: &[f64],
) -> [f64; 5] {
    let mut out = [0.0; 5];
    let Some(convert) = &skill.convert else {
        return out;
    };
    let unit = input.adjustment * input.difficulty_factor;
    let perfect = input.judge_frac[5];
    for (event, &time) in input.skill_times.iter().enumerate().take(5) {
        let from = input.clock.frame_time(time);
        let duration = convert.seconds.map_or_else(
            || window_ms(skill.effects.first().map(|e| e.seconds).unwrap_or(5.0), skill.extension_ms),
            |seconds| window_ms(seconds, 0.0),
        );
        let start = if convert.at_start { 0 } else { input.times.partition_point(|&t| t <= from) };
        let until = input.clock.frame_time(from.saturating_add(duration));
        let end = if convert.at_start { input.times.len() } else { input.times.partition_point(|&t| t <= until) };
        let mut values: Vec<_> = (start..end)
            .filter(|&i| convert.judgements.contains(&judgements[i]))
            .map(|i| match perfect_cap {
                Some(cap) => unit * input.note_frac[i] * input.ap_combo[i]
                    * ((perfect - input.judge_frac[judgements[i]]).max(0.0) * factor_cap
                        + input.judge_frac[judgements[i]] * cap),
                None => unit * input.note_frac[i] * conversion_combo.get(i).copied().unwrap_or(2.0)
                    * if conversion_combo.is_empty() {perfect} else {input.judge_frac[convert.judgement]} * factor_cap,
            })
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
        out[event] = sum;
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
        if slots.iter().any(Slot::changes_play_timeline) || slots.iter().flat_map(|slot| &slot.cumulative)
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
                                + factor.bound_duration_ms()
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

pub(crate) fn played_linear(input: &Input, grades: &[usize], life: &live::LifeSummary) -> Linear {
    let n = input.times.len();
    let mut prefix = vec![0.0; n + 1];
    let mut perfect_prefix = vec![0.0; n + 1];
    let (mut pending, mut before, mut last) = (0, 0, -1);
    for (index, &grade) in grades.iter().enumerate() {
        if input.times[index] != last {
            before = pending;
            last = input.times[index];
        }
        let penalty = if index < life.first_dead {
            1.0
        } else {
            input.life_onus
        };
        let value = input.adjustment
            * input.difficulty_factor
            * input.note_frac[index]
            * combo(&input.combo_table, before)
            * input.judge_frac[grade]
            * penalty;
        prefix[index + 1] = prefix[index] + value;
        perfect_prefix[index + 1] = perfect_prefix[index] + if grade == 5 { value } else { 0.0 };
        pending = if grade <= 2 { 0 } else { pending + 1 };
    }
    Linear {
        total: prefix[n],
        prefix,
        perfect_prefix,
        life_at: input
            .skill_times
            .iter()
            .take(5)
            .enumerate()
            .map(|(event, _)| (event, life.gates[event]))
            .collect(),
        absolute_error: n as f64 + 1.0,
        ignore_gates: false,
        nominal_cumulative: false,
        conversion_combo: Vec::new(),
        release_windows: Vec::new(),
    }
}
pub(crate) fn played_weight(
    input: &Input,
    model: &Linear,
    grades: &[usize],
    slot: &Slot,
    event: usize,
) -> f64 {
    let mut value = played_effect_weight(input, model, grades, slot, event);
    for factor in &slot.cumulative {
        value += cumulative_upper(&model.prefix, input, factor, input.skill_times[event], Some(grades));
    }
    value
}
pub(crate) fn played_effect_weight(
    input: &Input,
    model: &Linear,
    grades: &[usize],
    slot: &Slot,
    event: usize,
) -> f64 {
    let start = input.skill_times[event];
    let mut value = 0.0;
    let mut gated = Vec::new();
    for (effect_index, effect) in slot.effects.iter().enumerate() {
        let mut contribution = 0.0;
        if !model.ignore_gates && effect.gate.blocks(model.life_at[&event]) {
            continue;
        }
        if model.ignore_gates
            && effect.gate.kind == 1
            && effect.gate.positive
            && effect.gate.blocks(model.life_at[&event])
        {
            continue;
        }
        let end = live::effect_end(input, slot, effect, start, Some(grades));
        let first = input.times.partition_point(|&time| time < start);
        let last = input.times.partition_point(|&time| time < end);
        let prefix = if effect.kind == 2000 {
            Some(&model.prefix)
        } else if effect.judgements == [5] {
            Some(&model.perfect_prefix)
        } else {
            None
        };
        if let Some(prefix) = prefix.filter(|_| {
            last >= first
                && input.adjustment >= 0.0
                && input.difficulty_factor >= 0.0
                && model.prefix[last].is_finite()
                && model.prefix[last] >= 0.0
        }) {
            // Cover each subtraction and accumulation in the per-note coefficient.
            let error = 4.0 * grades.len() as f64 * f64::EPSILON * model.prefix[last];
            let mass = ((prefix[last] - prefix[first]).next_up() + error).next_up();
            contribution = (f(effect.delta as f64 / 100000.0) * mass).next_up();
        } else {
            for (index, &grade) in grades.iter().enumerate().take(last).skip(first) {
                if effect.kind == 2000 || effect.judgements.contains(&grade) {
                    contribution += f(effect.delta as f64 / 100000.0)
                        * (model.prefix[index + 1] - model.prefix[index]);
                }
            }
        }
        if model.ignore_gates && effect.gate.kind == 1 {
            gated.push((effect_index, contribution));
        } else {
            value += contribution;
        }
    }
    if !gated.is_empty() {
        let upper = model.life_at[&event];
        let values = std::iter::once(0.0).chain(
            gated
                .iter()
                .map(|&(index, _)| slot.effects[index].gate.value)
                .filter(|&value| value >= 0.0 && value <= upper),
        );
        let best = values
            .map(|life| {
                gated
                    .iter()
                    .filter(|&&(index, _)| !slot.effects[index].gate.blocks(life))
                    .map(|&(_, value)| value)
                    .fold(0.0_f64, |sum, value| (sum + value).next_up())
            })
            .fold(0.0_f64, f64::max);
        value = (value + best).next_up();
    }
    value
}
