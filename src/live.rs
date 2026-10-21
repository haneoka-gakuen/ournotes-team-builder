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
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
pub struct Conversion {
    pub judgements: Vec<usize>,
    pub limit: Option<f64>,
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
pub struct Slot {
    pub effects: Vec<Effect>,
    pub extension_ms: f64,
    pub recovery: i32,
    pub convert: Option<Conversion>,
}
impl Slot {
    /// Identifies equal skill shapes: the same fields with the same float bits.
    pub fn signature(&self) -> Vec<u64> {
        let mut out = vec![
            self.extension_ms.to_bits(),
            self.recovery as u64,
            self.effects.len() as u64,
        ];
        for e in &self.effects {
            out.extend([
                e.kind as u64,
                e.delta as u64,
                e.seconds.to_bits(),
                e.gate.kind as u64,
                e.gate.value.to_bits(),
                e.gate.positive as u64,
                e.judgements.len() as u64,
            ]);
            out.extend(e.judgements.iter().map(|&j| j as u64));
        }
        match &self.convert {
            None => out.push(u64::MAX),
            Some(c) => {
                out.extend([
                    c.judgements.len() as u64,
                    c.limit.map_or(u64::MAX - 1, f64::to_bits),
                ]);
                out.extend(c.judgements.iter().map(|&j| j as u64));
            }
        }
        out
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
            let end = start + window_ms(effect.seconds, slot.extension_ms);
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
    if input.slots.iter().any(|s| {
        s.convert
            .as_ref()
            .is_some_and(|c| c.judgements.iter().any(|&j| j <= 2))
    }) || input.damage[3] != 0.0
        || input.damage[4] != 0.0
    {
        return Err("conversions that change damage need the full simulation".into());
    }
    let generation = crate::cancellation_generation();
    let n = input.times.len();
    let t = f(f(input.adjustment * input.power as f64) * input.difficulty_factor);
    let luck = f(input.luck_percent.min(200.0) / 100.0);
    let assist = f(input.assist);
    let note: Vec<_> = input.note_frac.iter().map(|&v| f(v * t)).collect();
    let mut combos = Vec::with_capacity(n);
    let (mut pending, mut before, mut last) = (0, 0, -1);
    for (i, &judgement) in judgements.iter().enumerate().take(n) {
        if input.times[i] != last {
            before = pending;
            last = input.times[i];
        }
        combos.push(combo(&input.combo_table, before));
        pending = if judgement <= 2 { 0 } else { pending + 1 };
    }
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
        let lives = life_cache
            .entry(recoveries)
            .or_insert_with(|| lives(input, judgements, order, &input.slots));
        let mut factors = Factors {
            general: f(1.0),
            channel: [0.0; 7],
            active: Vec::new(),
        };
        let mut cursor = 0;
        let mut converts: Vec<Convert> = Vec::new();
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
                factors.expire(start as i64);
                if let Some(slot) = input.slots.get(order[event]) {
                    for effect in &slot.effects {
                        if effect.gate.blocks(lives.gate[&event]) {
                            continue;
                        }
                        let end = start + window_ms(effect.seconds, slot.extension_ms);
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
                    if let Some(convert) = &slot.convert {
                        converts.insert(
                            0,
                            Convert {
                                from: frame_time(start),
                                until: frame_time(
                                    start
                                        + window_ms(
                                            slot.effects.first().map(|e| e.seconds).unwrap_or(5.0),
                                            slot.extension_ms,
                                        ),
                                ),
                                judgements: convert.judgements.clone(),
                                left: convert.limit.unwrap_or(f64::INFINITY),
                            },
                        );
                    }
                }
                cursor += 1;
            }
            if i == n {
                break;
            }
            factors.expire(time);
            let mut judgement = judgements[i];
            let mut c = 0;
            while c < converts.len() {
                let conv = &mut converts[c];
                if (conv.until as i64) < time {
                    converts.remove(c);
                    continue;
                }
                if time <= conv.from as i64 || !conv.judgements.contains(&judgement) {
                    c += 1;
                    continue;
                }
                judgement = 5;
                conv.left -= 1.0;
                if conv.left <= 0.0 {
                    converts.remove(c);
                }
                break;
            }
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
        .flat_map(|e| &e.judgements)
        .any(|&j| j >= 7)
    {
        return Err("invalid effect judgement".into());
    }
    if let Some(judgements) = &input.judgements {
        if judgements.len() != n || judgements.iter().any(|&j| !(1..=6).contains(&j)) {
            return Err("invalid play judgements".into());
        }
        play(input, judgements)
    } else {
        Ok(ap(input))
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
