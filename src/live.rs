//! Ordinary live scoring over the canonical 120 skill orders. The score path
//! follows every binary32 operation and time boundary of the website engine.
use crate::num::{f, window_ms};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Clone, Deserialize, Serialize, PartialEq)]
pub struct Gate {
    pub kind: u8,
    pub value: f64,
    pub positive: bool,
}
impl Gate {
    pub(crate) fn blocks(&self, life: f64) -> bool {
        self.kind == 1 && (life >= self.value) != self.positive
    }
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
pub struct Effect {
    pub kind: i32,
    pub delta: i32,
    pub seconds: f64,
    pub gate: Gate,
    pub judgements: Vec<usize>,
    #[serde(default)]
    pub release_judgements: Vec<usize>,
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
pub struct Conversion {
    pub judgements: Vec<usize>,
    pub limit: Option<f64>,
    #[serde(default = "perfect")]
    pub judgement: usize,
    #[serde(default)]
    pub seconds: Option<f64>,
    #[serde(default)]
    pub at_start: bool,
}
fn perfect() -> usize { 5 }
/// A note factor that climbs with the matching judgements inside one activation:
/// every `unit` judgements add `value/10000`, at most `cap` steps and `maximum` mill.
#[derive(Clone, Deserialize, Serialize, PartialEq)]
pub struct Cumulative {
    pub judgements: Vec<usize>,
    pub unit: i32,
    pub value: i32,
    pub maximum: i32,
    pub cap: i32,
    pub seconds: f64,
}
const FINITE_CUMULATIVE_MAX_BP: i32 = 24_000;
const FINITE_CUMULATIVE_MAX_STEPS: i32 = 12;

impl Cumulative {
    /// Native mill of the factor after `count` matching judgements.
    pub(crate) fn mill(&self, count: i32) -> i32 {
        let steps = (count / self.unit).min(self.cap);
        let mut value = self.value.wrapping_mul(steps);
        if self.maximum > 0 && self.maximum < value {
            value = self.maximum;
        }
        crate::full::score::note_factor_mill(f(f(value as f64) / 10000.0))
    }

    pub(crate) fn at_cap(&self) -> i32 {
        // This is a search bound, not a replayed counter. A large or unlimited
        // cap can wrap the native product before the effect maximum is applied.
        let mut value = self
            .value
            .checked_mul(self.cap)
            .map_or(i32::MAX, |value| value.max(0));
        if self.maximum > 0 && self.maximum < value {
            value = self.maximum;
        }
        crate::full::score::note_factor_mill(f(f(value as f64) / 10000.0))
    }

    fn magnitude_cap(&self) -> f64 {
        let Some(mut value) = self.value.checked_mul(self.cap) else {
            return (i32::MIN as f64).abs();
        };
        if self.maximum > 0 && self.maximum < value {
            value = self.maximum;
        }
        (crate::full::score::note_factor_mill(f(f(value as f64) / 10000.0)) as f64).abs()
    }

    pub(crate) fn requires_unbounded_bounds(&self, notes: usize) -> bool {
        if self.value == 0 { return false; }
        let count = notes.min(i32::MAX as usize) as i32;
        self.value < 0
            || self.maximum <= 0
            || !(1..=FINITE_CUMULATIVE_MAX_STEPS).contains(&self.cap)
            || self.value.saturating_mul(self.cap).min(self.maximum) > FINITE_CUMULATIVE_MAX_BP
            || self.value.checked_mul((count / self.unit).min(self.cap)).is_none()
            || self.mill(count) == i32::MIN
    }

    pub(crate) fn duration_ms(&self) -> i32 {
        window_ms(self.seconds, 0.0)
    }

    /// Upper bounds start at the nominal event; support activation can be one 60 Hz frame later.
    pub(crate) fn bound_duration_ms(&self) -> i32 {
        self.duration_ms().saturating_add(17)
    }
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
pub struct Slot {
    pub effects: Vec<Effect>,
    #[serde(default)]
    pub cumulative: Vec<Cumulative>,
    pub extension_ms: f64,
    pub recovery: i32,
    pub convert: Option<Conversion>,
}
impl Slot {
    /// Identifies equal skill shapes: the same fields with the same float bits.
    pub fn signature(&self) -> Vec<u64> {
        let mut out = Vec::new();
        self.write_signature(&mut out);
        out
    }
    pub(crate) fn write_signature(&self, out: &mut Vec<u64>) {
        out.clear();
        let len = 4
            + self.effects.iter().map(|e| 8 + e.judgements.len() + e.release_judgements.len()).sum::<usize>()
            + self
                .cumulative
                .iter()
                .map(|c| 6 + c.judgements.len())
                .sum::<usize>()
            + self.convert.as_ref().map_or(1, |c| 5 + c.judgements.len());
        out.reserve(len);
        out.extend([
            self.extension_ms.to_bits(),
            self.recovery as u64,
            self.effects.len() as u64,
            self.cumulative.len() as u64,
        ]);
        for e in &self.effects {
            out.extend([
                e.kind as u64,
                e.delta as u64,
                e.seconds.to_bits(),
                e.gate.kind as u64,
                e.gate.value.to_bits(),
                e.gate.positive as u64,
                e.judgements.len() as u64,
                e.release_judgements.len() as u64,
            ]);
            out.extend(e.judgements.iter().map(|&j| j as u64));
            out.extend(e.release_judgements.iter().map(|&j| j as u64));
        }
        for c in &self.cumulative {
            out.extend([
                c.unit as u64,
                c.value as u64,
                c.maximum as u64,
                c.cap as u64,
                c.seconds.to_bits(),
                c.judgements.len() as u64,
            ]);
            out.extend(c.judgements.iter().map(|&j| j as u64));
        }
        match &self.convert {
            None => out.push(u64::MAX),
            Some(c) => {
                out.extend([
                    c.judgements.len() as u64,
                    c.limit.map_or(u64::MAX - 1, f64::to_bits),
                    c.judgement as u64,
                    c.seconds.map_or(u64::MAX - 2, f64::to_bits),
                    c.at_start as u64,
                ]);
                out.extend(c.judgements.iter().map(|&j| j as u64));
            }
        }
    }
    pub(crate) fn changes_play_timeline(&self) -> bool {
        self.effects.iter().any(|effect| !effect.release_judgements.is_empty())
            || self.convert.as_ref().is_some_and(|convert| {
                convert.at_start || convert.judgement != 5
                    || convert.judgements.iter().any(|&judgement| judgement <= 2)
            })
    }
}
pub(crate) fn rounding_profile(slot: &Slot, life: Option<f64>) -> (usize, f64) {
    let mut actions = [0usize; 4];
    let mut absolute = [0.0_f64; 4];
    for effect in &slot.effects {
        if life.is_some_and(|life| effect.gate.blocks(life)) {
            continue;
        }
        let delta = f(effect.delta as f64 / 100000.0).abs();
        for (channel, judgement) in [3, 4, 5, 6].into_iter().enumerate() {
            let count = if effect.kind == 2000 {
                1
            } else {
                effect
                    .judgements
                    .iter()
                    .filter(|&&j| j == judgement)
                    .count()
            };
            actions[channel] += count;
            absolute[channel] = (absolute[channel] + (delta * count as f64).next_up()).next_up();
        }
    }
    // A climbing factor changes once per reachable step. Budget every step and
    // the subtraction/add cycles used by the played-judgement path.
    for factor in &slot.cumulative {
        let steps = usize::try_from(factor.cap).unwrap_or(usize::MAX);
        let delta = f(factor.magnitude_cap() / 100000.0);
        for &judgement in &factor.judgements {
            let Some(channel) = [3, 4, 5, 6].iter().position(|&j| j == judgement) else {
                continue;
            };
            actions[channel] = actions[channel].saturating_add(steps.saturating_add(1));
            absolute[channel] = (absolute[channel] + (delta * steps as f64 * 3.0).next_up())
                .next_up();
        }
    }
    (
        actions.into_iter().max().unwrap(),
        absolute.into_iter().fold(0.0, f64::max),
    )
}
#[derive(Deserialize)]
pub struct Input {
    pub times: Vec<i32>,
    pub note_frac: Vec<f64>,
    pub ap_combo: Vec<f64>,
    pub combo_table: Vec<(usize, f64)>,
    pub judge_frac: [f64; 7],
    pub converted_count: usize,
    pub difficulty_factor: f64,
    pub adjustment: f64,
    pub skill_times: Vec<i32>,
    pub life_base: f64,
    pub life_onus: f64,
    pub damage: [f64; 7],
    pub power: i32,
    pub slots: Vec<Slot>,
    pub judgements: Option<Vec<usize>>,
    pub assist: f64,
    pub luck_percent: f64,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Scores {
    pub power: i32,
    pub scores: Vec<f64>,
    pub mean: f64,
    pub min: f64,
    pub max: f64,
    pub min_order: usize,
    pub max_order: usize,
}
static ORDERS: OnceLock<Vec<[usize; 5]>> = OnceLock::new();
pub fn orders() -> &'static [[usize; 5]] {
    ORDERS.get_or_init(|| {
        fn visit(out: &mut Vec<[usize; 5]>, order: &mut [usize; 5], depth: usize, used: u8) {
            if depth == 5 {
                out.push(*order);
                return;
            }
            for i in 0..5 {
                if used & (1 << i) == 0 {
                    order[depth] = i;
                    visit(out, order, depth + 1, used | (1 << i));
                }
            }
        }
        let mut out = Vec::with_capacity(120);
        visit(&mut out, &mut [0; 5], 0, 0);
        out
    })
}
pub(crate) struct OrderPlan {
    pub(crate) representatives: Vec<usize>,
    pub(crate) sources: Vec<usize>,
}
static FULL_PLAN: OnceLock<Arc<OrderPlan>> = OnceLock::new();
static PLANS: OnceLock<Mutex<HashMap<u16, Arc<OrderPlan>>>> = OnceLock::new();
pub(crate) fn plan(slots: &[Slot]) -> Arc<OrderPlan> {
    let mut groups = [0; 5];
    let mut classes = 0;
    let mut key = 0u16;
    for slot in 0..5 {
        let prior = (0..slot).find(|&p| slots.get(p) == slots.get(slot));
        if let Some(p) = prior {
            groups[slot] = groups[p];
        } else {
            groups[slot] = classes;
            classes += 1;
        }
        key = key * 5 + groups[slot] as u16;
    }
    if classes == 5 {
        return Arc::clone(FULL_PLAN.get_or_init(|| {
            Arc::new(OrderPlan {
                representatives: (0..120).collect(),
                sources: (0..120).collect(),
            })
        }));
    }
    let mut cache = PLANS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    Arc::clone(cache.entry(key).or_insert_with(|| {
        let mut first = HashMap::new();
        let mut representatives = Vec::new();
        let mut sources = Vec::with_capacity(120);
        for (i, order) in orders().iter().enumerate() {
            let sequence = order.map(|slot| groups[slot]);
            let index = *first.entry(sequence).or_insert_with(|| {
                representatives.push(i);
                i
            });
            sources.push(index);
        }
        Arc::new(OrderPlan {
            representatives,
            sources,
        })
    }))
}
pub(crate) fn summarize(power: i32, scores: Vec<f64>) -> Scores {
    let mut sorted = scores.clone();
    sorted.sort_by(f64::total_cmp);
    let total = sorted.iter().fold(0.0, |a, b| a + b);
    let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut min_order, mut max_order) = (0, 0);
    for (i, &value) in scores.iter().enumerate() {
        if value < min {
            min = value;
            min_order = i;
        }
        if value > max {
            max = value;
            max_order = i;
        }
    }
    Scores {
        power,
        scores,
        mean: total / 120.0,
        min,
        max,
        min_order,
        max_order,
    }
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
#[derive(Clone)]
struct Command {
    time: i32,
    owner: usize,
    channel: usize,
    delta: i32,
}
fn effect_end(input: &Input, slot: &Slot, effect: &Effect, start: i32, judgements: Option<&[usize]>) -> i32 {
    let end = start + window_ms(effect.seconds, slot.extension_ms);
    if effect.release_judgements.is_empty() { return end; }
    let after_execute = frame_time(frame_time(start).saturating_add(1));
    let deadline = frame_time(end.saturating_add(1));
    let mut first = input.times.partition_point(|&time| frame_time(time) <= after_execute);
    while first < input.times.len() {
        let frame = frame_time(input.times[first]);
        if frame > deadline { break; }
        let after = input.times.partition_point(|&time| frame_time(time) <= frame);
        if let Some(index) = (first..after).rfind(|&index| {
            effect.release_judgements.contains(&judgements.map_or(5, |grades| grades[index]))
        }) {
            return end.min(input.times[index]);
        }
        first = after;
    }
    end
}
fn cumulative_commands(
    input: &Input,
    order: &[usize; 5],
    judgements: Option<&[usize]>,
) -> Vec<Command> {
    let mut commands = Vec::new();
    for (event, &start) in input.skill_times.iter().take(5).enumerate() {
        let Some(slot) = input.slots.get(order[event]) else {
            continue;
        };
        for factor in &slot.cumulative {
            let activation = frame_time(start);
            let end = activation + factor.duration_ms();
            let (mut count, mut applied) = (0, 0);
            let mut first = input
                .times
                .partition_point(|&time| frame_time(time) < activation);
            while first < input.times.len() {
                let frame = frame_time(input.times[first]);
                if frame > end {
                    break;
                }
                let after = input
                    .times
                    .partition_point(|&time| frame_time(time) <= frame);
                count += (first..after)
                    .filter(|&i| {
                        factor
                            .judgements
                            .contains(&judgements.map_or(5, |grades| grades[i]))
                    })
                    .count() as i32;
                let mill = factor.mill(count);
                if mill != applied {
                    let time = frame;
                    commands.push(Command {
                        time,
                        owner: event,
                        channel: 0,
                        delta: applied.wrapping_neg(),
                    });
                    commands.push(Command {
                        time,
                        owner: event,
                        channel: 0,
                        delta: mill,
                    });
                    applied = mill;
                }
                first = after;
            }
            if applied != 0 {
                commands.push(Command {
                    time: end,
                    owner: event,
                    channel: 0,
                    delta: applied.wrapping_neg(),
                });
            }
        }
    }
    commands.sort_by_key(|command| (command.time, command.owner));
    commands
}
fn commands(input: &Input, order: &[usize; 5]) -> Vec<Command> {
    let mut commands = Vec::new();
    for (event, &start) in input.skill_times.iter().take(5).enumerate() {
        let Some(slot) = input.slots.get(order[event]) else {
            continue;
        };
        for effect in &slot.effects {
            if effect.gate.blocks(input.life_base) {
                continue;
            }
            let end = effect_end(input, slot, effect, start, None);
            let channels = if effect.kind == 2000 {
                vec![0]
            } else {
                effect.judgements.clone()
            };
            for channel in channels {
                commands.push(Command {
                    time: start,
                    owner: event,
                    channel,
                    delta: effect.delta,
                });
                commands.push(Command {
                    time: end,
                    owner: event,
                    channel,
                    delta: -effect.delta,
                });
            }
        }
    }
    commands.extend(cumulative_commands(input, order, None));
    commands.sort_by_key(|c| (c.time, c.owner));
    commands
}
fn prefix<'a>(
    cache: &'a mut HashMap<u64, Vec<f64>>,
    base: &[f64],
    factor: f64,
    luck: f64,
    assist: f64,
    count: usize,
) -> &'a [f64] {
    cache.entry(factor.to_bits()).or_insert_with(|| {
        let mut out = Vec::with_capacity(base.len() + 1);
        out.push(0.0);
        let mut total = 0.0;
        for &note in base {
            let mut x = f(f(luck * f(note * factor)) / count as f64).floor();
            if assist != 1.0 {
                x = f(assist * x).floor();
            }
            total += x;
            out.push(total);
        }
        out
    })
}
fn ap(input: &Input) -> Scores {
    let n = input.times.len();
    let t = f(f(input.adjustment * input.power as f64) * input.difficulty_factor);
    let perfect = input.judge_frac[5];
    let base: Vec<_> = (0..n)
        .map(|i| f(f(perfect * f(input.note_frac[i] * t)) * input.ap_combo[i]))
        .collect();
    let luck = f(input.luck_percent.min(200.0) / 100.0);
    let assist = f(input.assist);
    let mut prefixes: HashMap<u64, Vec<f64>> = HashMap::new();
    let mut scores = vec![0.0; 120];
    let plan = plan(&input.slots);
    for &index in &plan.representatives {
        let (mut general, mut perfect_up) = (f(1.0), f(0.0));
        let mut at = 0;
        let mut score = 0.0;
        for command in commands(input, &orders()[index]) {
            let end = input.times.partition_point(|&time| time < command.time);
            if end > at {
                let sums = prefix(
                    &mut prefixes,
                    &base,
                    f(general + perfect_up),
                    luck,
                    assist,
                    input.converted_count,
                );
                score += sums[end] - sums[at];
                at = end;
            }
            let diff = f(command.delta as f64 / 100000.0);
            if command.channel == 0 {
                general = f(general + diff);
            } else if command.channel == 5 {
                perfect_up = f(perfect_up + diff);
            }
        }
        if at < n {
            let sums = prefix(
                &mut prefixes,
                &base,
                f(general + perfect_up),
                luck,
                assist,
                input.converted_count,
            );
            score += sums[n] - sums[at];
        }
        scores[index] = score;
    }
    for (index, &source) in plan.sources.iter().enumerate() {
        scores[index] = scores[source];
    }
    summarize(input.power, scores)
}
pub fn frame_time(time: i32) -> i32 {
    let mut k = ((time as f64 * 60.0 / 1000.0).ceil() as i32 - 1).max(0);
    while (k as f64 * 1000.0 / 60.0).floor() < (time as f64) {
        k += 1;
    }
    (k as f64 * 1000.0 / 60.0).floor() as i32
}
#[derive(Clone)]
struct Recovery {
    time: i32,
    frame: i32,
    amount: i32,
}
pub(crate) struct Lives {
    pub(crate) note: Vec<f64>,
    pub(crate) gate: HashMap<usize, f64>,
}
struct LifeReplay<'a> {
    input: &'a Input,
    judgements: &'a [usize],
    pending: Vec<Recovery>,
    processed: usize,
    life: f64,
    note: Vec<f64>,
}
impl LifeReplay<'_> {
    fn settle(&mut self, time: i64) {
        let mut r = 0;
        while r < self.pending.len() {
            if self.pending[r].frame as i64 >= time {
                r += 1;
                continue;
            }
            let rec = self.pending.remove(r);
            let mut from = self.processed;
            while from > 0 && self.input.times[from - 1] > rec.time {
                from -= 1;
            }
            let mut value = if from > 0 {
                self.note[from - 1]
            } else {
                self.input.life_base
            };
            if value > 0.0 {
                value = (self.input.life_base * 2.0).min(value + rec.amount as f64);
            }
            for j in from..self.processed {
                let damage = self.input.damage[self.judgements[j]];
                if damage != 0.0 {
                    value = (value - damage).max(0.0);
                }
                self.note[j] = value;
            }
            self.life = value;
        }
    }
}
fn firing(input: &Input) -> Vec<usize> {
    let mut out: Vec<_> = (0..input.skill_times.len()).filter(|&i| i < 5).collect();
    out.sort_by_key(|&i| (input.skill_times[i], i));
    out
}
pub(crate) fn lives(
    input: &Input,
    judgements: &[usize],
    order: &[usize; 5],
    slots: &[Slot],
) -> Lives {
    let n = input.times.len();
    let mut replay = LifeReplay {
        input,
        judgements,
        pending: Vec::new(),
        processed: 0,
        life: input.life_base,
        note: vec![0.0; n],
    };
    let firing = firing(input);
    let mut gate = HashMap::new();
    let mut cursor = 0;
    for i in 0..=n {
        let time = if i < n {
            input.times[i] as i64
        } else {
            1 << 53
        };
        while cursor < firing.len() && input.skill_times[firing[cursor]] as i64 <= time {
            let event = firing[cursor];
            let start = input.skill_times[event];
            let recovery = slots.get(order[event]).map(|s| s.recovery).unwrap_or(0);
            if recovery != 0 {
                replay.pending.push(Recovery {
                    time: start,
                    frame: frame_time(start),
                    amount: recovery,
                });
            }
            let frame = frame_time(start);
            replay.settle(frame as i64);
            let mut due: Vec<_> = replay
                .pending
                .iter()
                .filter(|rec| rec.frame <= frame)
                .cloned()
                .collect();
            due.sort_by_key(|rec| rec.time);
            let mut gate_life = replay.life;
            let mut r = 0;
            for (j, &judgement) in judgements.iter().enumerate().take(n).skip(i) {
                if input.times[j] > frame {
                    break;
                }
                while r < due.len() && due[r].time < input.times[j] {
                    if gate_life > 0.0 {
                        gate_life = (input.life_base * 2.0).min(gate_life + due[r].amount as f64);
                    }
                    r += 1;
                }
                let damage = input.damage[judgement];
                if damage != 0.0 {
                    gate_life = (gate_life - damage).max(0.0);
                }
            }
            while r < due.len() {
                if gate_life > 0.0 {
                    gate_life = (input.life_base * 2.0).min(gate_life + due[r].amount as f64);
                }
                r += 1;
            }
            gate.insert(event, gate_life);
            cursor += 1;
        }
        if i == n {
            break;
        }
        replay.settle(time);
        let damage = input.damage[judgements[i]];
        if damage != 0.0 {
            replay.life = (replay.life - damage).max(0.0);
        }
        replay.note[i] = replay.life;
        replay.processed = i + 1;
    }
    Lives {
        note: replay.note,
        gate,
    }
}
struct Active {
    end: i32,
    channel: usize,
    delta: i32,
}
struct Convert {
    from: i32,
    until: i32,
    judgements: Vec<usize>,
    left: f64,
    judgement: usize,
}
fn conversion(slot: &Slot, convert: &Conversion, start: i32) -> Convert {
    let from = frame_time(start);
    let duration = convert.seconds.map_or_else(
        || window_ms(slot.effects.first().map(|effect| effect.seconds).unwrap_or(5.0), slot.extension_ms),
        |seconds| window_ms(seconds, 0.0),
    );
    Convert {
        from,
        until: frame_time(from + duration),
        judgements: convert.judgements.clone(),
        left: convert.limit.unwrap_or(f64::INFINITY),
        judgement: convert.judgement,
    }
}
fn converted_judgements(input: &Input, order: &[usize; 5], judgements: &[usize]) -> Vec<usize> {
    let mut converted = judgements.to_vec();
    if input.slots.iter().all(|slot| slot.convert.is_none()) {
        return converted;
    }
    let firing = firing(input);
    let mut cursor = 0;
    let mut converts: Vec<Convert> = Vec::new();
    for slot in &input.slots {
        if let Some(convert) = &slot.convert {
            if convert.at_start { converts.insert(0, conversion(slot, convert, 0)); }
        }
    }
    let mut first = 0;
    while first < input.times.len() {
        let time = input.times[first];
        while cursor < firing.len() && input.skill_times[firing[cursor]] <= time {
            let event = firing[cursor];
            let start = input.skill_times[event];
            if let Some(slot) = input.slots.get(order[event]) {
                if let Some(convert) = &slot.convert {
                    if !convert.at_start { converts.insert(0, conversion(slot, convert, start)); }
                }
            }
            cursor += 1;
        }
        let after = input.times.partition_point(|&t| t <= time);
        for target in &mut converted[first..after] {
            let mut c = 0;
            while c < converts.len() {
                let conv = &mut converts[c];
                if conv.until < time {
                    converts.remove(c);
                    continue;
                }
                if time <= conv.from || !conv.judgements.contains(target) {
                    c += 1;
                    continue;
                }
                if *target == conv.judgement { c += 1; continue; }
                *target = conv.judgement;
                conv.left -= 1.0;
                if conv.left <= 0.0 {
                    converts.remove(c);
                }
                break;
            }
        }
        first = after;
    }
    converted
}
struct Factors {
    general: f64,
    channel: [f64; 7],
    active: Vec<Active>,
}
impl Factors {
    fn expire(&mut self, time: i64) {
        for i in (0..self.active.len()).rev() {
            if self.active[i].end as i64 <= time {
                let effect = self.active.remove(i);
                let diff = f(effect.delta as f64 / 100000.0);
                if effect.channel == 0 {
                    self.general = f(self.general - diff);
                } else {
                    self.channel[effect.channel] = f(self.channel[effect.channel] - diff);
                }
            }
        }
    }
}
fn play(input: &Input, judgements: &[usize]) -> Result<Scores, String> {
    let generation = crate::cancellation_generation();
    let n = input.times.len();
    let t = f(f(input.adjustment * input.power as f64) * input.difficulty_factor);
    let luck = f(input.luck_percent.min(200.0) / 100.0);
    let assist = f(input.assist);
    let note: Vec<_> = input.note_frac.iter().map(|&v| f(v * t)).collect();
    let firing = firing(input);
    let mut scores = vec![0.0; 120];
    let plan = plan(&input.slots);
    let mut life_cache = HashMap::new();
    for &index in &plan.representatives {
        if generation != crate::cancellation_generation() {
            return Err("cancelled".into());
        }
        let order = &orders()[index];
        let recoveries = order.map(|slot| input.slots.get(slot).map(|s| s.recovery).unwrap_or(0));
        let converted = converted_judgements(input, order, judgements);
        let lives = life_cache
            .entry((recoveries, converted.clone()))
            .or_insert_with(|| lives(input, &converted, order, &input.slots));
        let mut combos = Vec::with_capacity(n);
        let (mut pending_combo, mut before, mut last) = (0, 0, -1);
        for (i, &judgement) in converted.iter().enumerate() {
            if input.times[i] != last { before = pending_combo; last = input.times[i]; }
            combos.push(combo(&input.combo_table, before));
            pending_combo = if judgement <= 2 { 0 } else { pending_combo + 1 };
        }
        let mut factors = Factors {
            general: f(1.0),
            channel: [0.0; 7],
            active: Vec::new(),
        };
        let mut cursor = 0;
        let cumulative = cumulative_commands(input, order, Some(&converted));
        let mut pending = cumulative.into_iter().peekable();
        let mut score = 0.0;
        for i in 0..=n {
            let time = if i < n {
                input.times[i] as i64
            } else {
                1 << 53
            };
            while cursor < firing.len() && input.skill_times[firing[cursor]] as i64 <= time {
                let event = firing[cursor];
                let start = input.skill_times[event];
                while pending.peek().is_some_and(|command| command.time <= start) {
                    let command = pending.next().unwrap();
                    factors.general = f(factors.general + f(command.delta as f64 / 100000.0));
                }
                factors.expire(start as i64);
                if let Some(slot) = input.slots.get(order[event]) {
                    for effect in &slot.effects {
                        if effect.gate.blocks(lives.gate[&event]) {
                            continue;
                        }
                        let end = effect_end(input, slot, effect, start, Some(&converted));
                        let diff = f(effect.delta as f64 / 100000.0);
                        let channels = if effect.kind == 2000 {
                            vec![0]
                        } else {
                            effect.judgements.clone()
                        };
                        for which in channels {
                            if which == 0 {
                                factors.general = f(factors.general + diff);
                            } else {
                                factors.channel[which] = f(factors.channel[which] + diff);
                            }
                            factors.active.push(Active {
                                end,
                                channel: which,
                                delta: effect.delta,
                            });
                        }
                    }
                }
                cursor += 1;
            }
            if i == n {
                break;
            }
            factors.expire(time);
            while pending
                .peek()
                .is_some_and(|command| command.time as i64 <= time)
            {
                let command = pending.next().unwrap();
                factors.general = f(factors.general + f(command.delta as f64 / 100000.0));
            }
            let judgement = converted[i];
            let up = f(factors.general + factors.channel[judgement]);
            let mut x = f(
                f(luck * f(f(f(input.judge_frac[judgement] * note[i]) * combos[i]) * up))
                    / input.converted_count as f64,
            )
            .floor();
            if lives.note[i] <= 0.0 {
                x = f(input.life_onus * x).floor();
            }
            if assist != 1.0 {
                x = f(assist * x).floor();
            }
            score += x;
        }
        scores[index] = score;
    }
    for (index, &source) in plan.sources.iter().enumerate() {
        scores[index] = scores[source];
    }
    Ok(summarize(input.power, scores))
}
pub fn score(input: &Input) -> Result<Scores, String> {
    let n = input.times.len();
    if input.note_frac.len() != n
        || input.ap_combo.len() != n
        || input.converted_count == 0
        || input.slots.len() > 5
    {
        return Err("invalid ordinary-live shape".into());
    }
    if input
        .slots
        .iter()
        .flat_map(|s| &s.effects)
        .flat_map(|e| e.judgements.iter().chain(&e.release_judgements))
        .any(|&j| j >= 7)
    {
        return Err("invalid effect judgement".into());
    }
    if input.slots.iter().filter_map(|slot| slot.convert.as_ref()).any(|convert| {
        !(1..=6).contains(&convert.judgement) || convert.judgements.iter().any(|&j| !(1..=6).contains(&j))
    }) {
        return Err("invalid conversion judgement".into());
    }
    if let Some(judgements) = &input.judgements {
        if judgements.len() != n || judgements.iter().any(|&j| !(1..=6).contains(&j)) {
            return Err("invalid play judgements".into());
        }
        play(input, judgements)
    } else {
        let native_cumulative = input
            .slots
            .iter()
            .flat_map(|slot| &slot.cumulative)
            .any(|factor| factor.requires_unbounded_bounds(n));
        if native_cumulative {
            play(input, &vec![5; n])
        } else {
            Ok(ap(input))
        }
    }
}

/// Evenly distribute custom judgements in chart order with the same priority
/// and open-note selection as the native accuracy builder.
pub fn judgements(count: usize, play: &serde_json::Value) -> Option<Vec<usize>> {
    let number = |key: &str| crate::master::number(&play[key], 0.0);
    let (great, good, bad, miss, every) = (
        number("great"),
        number("good"),
        number("bad"),
        number("miss"),
        number("missEvery"),
    );
    if great == 0.0 && good == 0.0 && bad == 0.0 && miss == 0.0 && every <= 0.0 {
        return None;
    }
    fn choose(length: usize, wanted: usize, mut select: impl FnMut(usize)) {
        if wanted == 0 || length == 0 {
            return;
        }
        let mut remainder = 0;
        for i in 0..length {
            remainder += wanted;
            if remainder >= length {
                remainder -= length;
                select(i);
            }
        }
    }
    let mut out = vec![5; count];
    let wanted =
        |fraction: f64| crate::num::round(fraction.clamp(0.0, 1.0) * count as f64) as usize;
    choose(count, wanted(great), |i| out[i] = 4);
    for (judgement, fraction) in [(3, good), (2, bad), (1, miss)] {
        let open: Vec<_> = out
            .iter()
            .enumerate()
            .filter(|(_, j)| **j == 5)
            .map(|(i, _)| i)
            .collect();
        choose(open.len(), wanted(fraction).min(open.len()), |i| {
            out[open[i]] = judgement
        });
    }
    let every = every.floor() as usize;
    if every > 0 {
        for i in ((every - 1)..count).step_by(every) {
            out[i] = 1;
        }
    }
    Some(out)
}
