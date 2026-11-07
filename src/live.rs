//! Ordinary live scoring over the canonical 120 skill orders. The score path
//! follows every binary32 operation and time boundary of the website engine.
use crate::num::{f, window_ms};
mod frame_optimization;
mod timing_constraints;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BinaryHeap, HashMap},
    rc::Rc,
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

    pub(crate) fn bound_duration_ms(&self) -> i32 {
        self.duration_ms()
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
    pub(crate) fn conversion_signature(&self) -> Option<Vec<u64>> {
        let convert = self.convert.as_ref()?;
        let mut signature = vec![
            convert.at_start as u64,
            convert.judgement as u64,
            convert.limit.map_or(u64::MAX, f64::to_bits),
            convert.seconds.map_or_else(
                || {
                    window_ms(
                        self.effects
                            .first()
                            .map(|effect| effect.seconds)
                            .unwrap_or(5.0),
                        self.extension_ms,
                    )
                },
                |seconds| window_ms(seconds, 0.0),
            ) as u64,
        ];
        signature.extend(convert.judgements.iter().map(|&grade| grade as u64));
        Some(signature)
    }
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
#[derive(Clone, Copy, Deserialize)]
#[serde(from = "u16")]
pub struct FrameClock(u16);

impl Default for FrameClock {
    fn default() -> Self { Self(60) }
}

impl From<u16> for FrameClock {
    fn from(hz: u16) -> Self {
        Self(if matches!(hz, 30 | 60 | 90 | 120) { hz } else { 60 })
    }
}

impl FrameClock {
    pub fn for_play(play: &serde_json::Value) -> Self {
        Self::from(play["frameRate"].as_u64().filter(|hz| matches!(hz, 30 | 60 | 90 | 120)).unwrap_or(60) as u16)
    }

    #[inline]
    pub fn frame_time(self, time: i32) -> i32 {
        let hz = i64::from(self.0);
        let time = i64::from(time.max(0));
        let frame = (time * hz + 999) / 1000;
        (frame * 1000 / hz).min(i64::from(i32::MAX)) as i32
    }

    pub fn advance_ms(self) -> i32 { (1000 + i32::from(self.0) - 1) / i32::from(self.0) }
}

#[derive(Clone, Deserialize)]
pub struct Input {
    #[serde(default, rename = "frameRate", alias = "frame_rate")]
    pub clock: FrameClock,
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
impl Input {
    fn frame_time(&self, time: i32) -> i32 { self.clock.frame_time(time) }
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
pub(crate) fn effect_end(input: &Input, slot: &Slot, effect: &Effect, start: i32, judgements: Option<&[usize]>) -> i32 {
    let end = start + window_ms(effect.seconds, slot.extension_ms);
    if effect.release_judgements.is_empty()
        || judgements.is_none() && !effect.release_judgements.contains(&5)
    {
        return end;
    }
    let after_execute = input.frame_time(input.frame_time(start).saturating_add(1));
    let deadline = input.frame_time(end.saturating_add(1));
    let mut first = input.times.partition_point(|&time| input.frame_time(time) <= after_execute);
    while first < input.times.len() {
        let frame = input.frame_time(input.times[first]);
        if frame > deadline { break; }
        let after = input.times.partition_point(|&time| input.frame_time(time) <= frame);
        if let Some(index) = (first..after).rfind(|&index| {
            effect.release_judgements.contains(&judgements.map_or(5, |grades| grades[index]))
        }) {
            return end.min(input.times[index]);
        }
        first = after;
    }
    end
}
/// Notes in judgement-delivery order: each note's frame and chart index. Without
/// offsets this is chart order, because frames never decrease with note time.
#[derive(Clone)]
struct Delivery {
    frames: Vec<i32>,
    notes: Vec<usize>,
}

/// Extremize cumulative delivery timing for each skill order. Ordinary score,
/// combo, conversions and member-skill windows retain their chart-time contract.
pub fn score_timed(input: &Input, frames: &[Vec<i32>], best: bool) -> Result<Scores, String> {
    Ok(summarize(input.power, timed_scores(input, frames, best, None, None)?))
}
pub fn score_reachable(input: &Input, timing: &crate::input_timing::InputTiming, best: bool) -> Result<Scores, String> {
    Ok(summarize(input.power, timed_scores(input, &timing.frames, best, Some(timing), None)?))
}
pub(crate) fn score_reachable_order(input: &Input, timing: &crate::input_timing::InputTiming, best: bool, index: usize) -> Result<f64, String> {
    Ok(timed_scores(input, &timing.frames, best, Some(timing), Some(index))?[index])
}
fn timed_scores(input: &Input, frames: &[Vec<i32>], best: bool, timing: Option<&crate::input_timing::InputTiming>, only_order: Option<usize>) -> Result<Vec<f64>, String> {
    let nominal = score(input)?;
    if input.slots.iter().all(|slot| slot.cumulative.is_empty()) { return Ok(nominal.scores); }
    if frames.len() != input.times.len() || frames.iter().any(Vec::is_empty) {
        return Err("missing ordinary judgement timing".into());
    }
    // (activation, expiry, judgement mask, monotonicity of the native counter).
    type Window = (i32, i32, u64, i8);
    fn dominates(a: i32, b: i32, windows: &[Window], best: bool) -> bool {
        windows.iter().all(|&(start, end, _, trend)| {
            if trend == 0 { return false; }
            let (inside_a, inside_b) = (start <= a && a <= end, start <= b && b <= end);
            let earlier = (trend > 0) == best;
            match (inside_a, inside_b, earlier) {
                (true, true, true) => a <= b,
                (true, true, false) => a >= b,
                (false, false, _) => true,
                (_, _, true) => inside_a,
                (_, _, false) => !inside_a,
            }
        })
    }
    fn deliveries<D: AsRef<[i32]>>(input: &Input, frames: &[D], windows: &[Window], grades: &[usize], best: bool) -> Vec<Delivery> {
        let mut selected = Vec::with_capacity(frames.len());
        let mut choices = Vec::new();
        for (i, allowed) in frames.iter().enumerate() {
            let allowed = allowed.as_ref();
            let nominal = input.frame_time(input.times[i]);
            if !windows.iter().any(|window| window.2 & (1 << grades[i]) != 0 && allowed.iter().any(|&at| window.0 <= at && at <= window.1)) {
                selected.push(*allowed.iter().min_by_key(|&&frame| (i64::from(frame) - i64::from(nominal)).abs()).unwrap());
                continue;
            }
            let applicable: Vec<_> = windows.iter().copied().filter(|window| window.2 & (1 << grades[i]) != 0
                && allowed.iter().any(|&at| window.0 <= at && at <= window.1)).collect();
            // Earlier/later delivery dominates only when every affected counter
            // agrees. Keep all remaining choices, including overlapping windows.
            let equivalent_key = |frame: i32| ((i64::from(frame) - i64::from(nominal)).abs(), frame);
            let options: Vec<_> = allowed.iter().copied().filter(|&frame| {
                !allowed.iter().any(|&other| other != frame && dominates(other, frame, &applicable, best)
                    && (!dominates(frame, other, &applicable, best) || equivalent_key(other) < equivalent_key(frame)))
            }).collect();
            selected.push(options[0]);
            if options.len() > 1 { choices.push((i, options)); }
        }
        fn visit(at: usize, choices: &[(usize, Vec<i32>)], selected: &mut [i32], out: &mut Vec<Delivery>) {
            if at == choices.len() {
                let mut rows: Vec<_> = selected.iter().copied().enumerate().map(|(i, f)| (f, i)).collect();
                rows.sort_unstable();
                out.push(Delivery { frames: rows.iter().map(|r| r.0).collect(), notes: rows.iter().map(|r| r.1).collect() });
                return;
            }
            let (i, options) = &choices[at];
            for &frame in options {
                selected[*i] = frame;
                visit(at + 1, choices, selected, out);
            }
        }
        let mut out = Vec::new();
        visit(0, &choices, &mut selected, &mut out);
        out
    }
    let n = input.times.len();
    let mut schedules = HashMap::<Vec<i64>, Vec<Delivery>>::new();
    let mut ap = input.judgements.is_none().then(|| ApScorer::new(input));
    let mut played = input.judgements.as_ref().map(|grades| PlayScorer::new(input, grades));
    let mut scores = vec![0.0; 120];
    let plan = plan(&input.slots);
    for &index in &plan.representatives {
        if only_order.is_some_and(|only| plan.sources[only] != index) { continue; }
        let order = &orders()[index];
        let grades = input.judgements.as_ref().map_or_else(|| vec![5; n], |g| converted_judgements(input, order, g));
        let mut windows = Vec::<Window>::new();
        for (event, &start) in input.skill_times.iter().take(5).enumerate() {
            let Some(slot) = input.slots.get(order[event]) else { continue };
            for factor in &slot.cumulative {
                if factor.judgements.is_empty() || factor.value == 0 { continue; }
                let values: Vec<_> = (0..=n).map(|count| factor.mill(count as i32)).collect();
                let trend = if values.windows(2).all(|w| w[0] <= w[1]) { 1 }
                    else if values.windows(2).all(|w| w[0] >= w[1]) { -1 } else { 0 };
                let activation = input.frame_time(start);
                let mask = factor.judgements.iter().fold(0, |mask, &g| mask | (1u64 << g));
                windows.push((activation, start + factor.duration_ms(), mask, trend));
            }
        }
        windows.sort_unstable();
        windows.dedup();
        let mut key: Vec<_> = grades.iter().map(|&g| g as i64).collect();
        for &(start, end, mask, trend) in &windows { key.extend([start as i64, end as i64, mask as i64, trend as i64]); }
        let candidates = schedules.entry(key).or_insert_with(|| deliveries(input, frames, &windows, &grades, best));
        let mut evaluate = |delivery: &Delivery| match (&mut ap, &mut played) {
            (Some(scorer), _) => scorer.order(order, Some(delivery)),
            (_, Some(scorer)) => scorer.order(order, Some(delivery)),
            _ => unreachable!(),
        };
        let mut extreme = if best { f64::NEG_INFINITY } else { f64::INFINITY };
        let mut chosen = candidates[0].clone();
        for delivery in candidates {
            let value = evaluate(delivery);
            if if best { value > extreme } else { value < extreme } {
                extreme = value;
                chosen = delivery.clone();
            }
        }
        scores[index] = if let Some(timing) = timing {
            reachable_extreme(input, timing, frames, best, (extreme, chosen, index),
                |domains, evaluate| {
                    let mut value = if best { f64::NEG_INFINITY } else { f64::INFINITY };
                    let mut selected = None;
                    for delivery in deliveries(input, domains, &windows, &grades, best) {
                        let score = evaluate(&delivery);
                        if if best { score > value } else { score < value } {
                            value = score;
                            selected = Some(delivery);
                        }
                    }
                    (value, selected.unwrap())
                }, &mut evaluate)?
        } else { extreme };
    }
    for (index, &source) in plan.sources.iter().enumerate() { scores[index] = scores[source]; }
    Ok(scores)
}
type FrameDomain = Rc<[i32]>;
fn restrict_domain(domain: &mut FrameDomain, keep: impl Fn(i32) -> bool) {
    if domain.iter().all(|&at| keep(at)) { return; }
    *domain = domain.iter().copied().filter(|&at| keep(at)).collect::<Vec<_>>().into();
}
struct TimingNode {
    priority: f64,
    serial: usize,
    bound: f64,
    domains: Vec<(usize, FrameDomain)>,
    delivery: Vec<(usize, i32)>,
}
impl TimingNode {
    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.domains.capacity() * std::mem::size_of::<(usize, FrameDomain)>()
            + self.domains.iter().map(|(_, domain)| domain.len() * std::mem::size_of::<i32>()).sum::<usize>()
            + self.delivery.capacity() * std::mem::size_of::<(usize, i32)>()
    }
}
impl PartialEq for TimingNode {
    fn eq(&self, other: &Self) -> bool { self.serial == other.serial && self.priority.to_bits() == other.priority.to_bits() }
}
impl Eq for TimingNode {}
impl PartialOrd for TimingNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}
impl Ord for TimingNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.priority.total_cmp(&other.priority).then_with(|| self.serial.cmp(&other.serial))
    }
}
fn reachable_extreme<E: FnMut(&Delivery) -> f64>(
    input: &Input,
    timing: &crate::input_timing::InputTiming,
    frames: &[Vec<i32>],
    best: bool,
    initial: (f64, Delivery, usize),
    mut relax: impl FnMut(&[FrameDomain], &mut E) -> (f64, Delivery),
    evaluate: &mut E,
) -> Result<f64, String> {
    fn prefer_nominal<E: FnMut(&Delivery) -> f64, D: AsRef<[i32]>>(preferred: &[i32], windows: &[(i32, i32)], domains: &[D], score: f64, mut delivery: Delivery, evaluate: &mut E) -> Delivery {
        // Saturated counters leave timing ties. Keep the exact bound while
        // avoiding unnecessary input conflicts on those notes.
        let mut positions = vec![0; delivery.notes.len()];
        for (position, &note) in delivery.notes.iter().enumerate() { positions[note] = position; }
        for (i, domain) in domains.iter().enumerate() {
            let domain = domain.as_ref();
            let nominal = preferred[i];
            let nearest = *domain.iter().min_by_key(|&&at| ((i64::from(at) - i64::from(nominal)).abs(), at)).unwrap();
            let previous = delivery.frames[positions[i]];
            if previous == nearest { continue; }
            delivery.move_note(i, nearest, &mut positions);
            let observed = windows.iter().any(|&(start, end)| (start <= previous && previous <= end) || (start <= nearest && nearest <= end));
            if observed && evaluate(&delivery) != score { delivery.move_note(i, previous, &mut positions); }
        }
        delivery
    }
    let (bound, delivery, order_index) = initial;
    let windows: Vec<_> = input.skill_times.iter().take(5).enumerate().flat_map(|(event, &start)| {
        input.slots[orders()[order_index][event]].cumulative.iter().filter(|factor| !factor.judgements.is_empty() && factor.value != 0)
            .map(move |factor| (input.frame_time(start), start + factor.duration_ms()))
    }).collect();
    let nominal: Vec<_> = input.times.iter().map(|&time| input.frame_time(time)).collect();
    let mut preferred = nominal.clone();
    let mut incumbent = if best { f64::NEG_INFINITY } else { f64::INFINITY };
    for witness in timing.witnesses() {
        let score = evaluate(&Delivery::from_frames(&witness));
        if if best { score > incumbent } else { score < incumbent } { incumbent = score; preferred = witness; }
    }
    let delivery = prefer_nominal(&preferred, &windows, frames, bound, delivery, evaluate);
    let generation = crate::cancellation_generation();
    match timing.conflict(&delivery.selected()) {
        Ok(None) => return Ok(bound),
        Ok(Some(_)) => {}
        Err(error) => {
            if error != "cancelled" {
                if let Some(value) = frame_optimization::score(input, timing, order_index, best)? { return Ok(value); }
            }
            return Err(error);
        }
    }
    if matches!(timing.conflict(&nominal), Ok(None)) {
        let value = evaluate(&Delivery::from_frames(&nominal));
        incumbent = if best { incumbent.max(value) } else { incumbent.min(value) };
    }
    let priority = |score: f64| if best { score } else { -score };
    let delivery_changes = |delivery: Delivery| delivery.selected().into_iter().enumerate()
        .filter(|&(i, at)| at != nominal[i]).collect::<Vec<_>>();
    let mut queue = BinaryHeap::new();
    let base_domains: Vec<FrameDomain> = frames.iter().map(|domain| Rc::from(domain.as_slice())).collect();
    let root = TimingNode { priority: priority(bound), serial: 0, bound, domains: Vec::new(), delivery: delivery_changes(delivery) };
    let mut queue_bytes = root.bytes();
    queue.push(root);
    let mut serial = 0;
    let mut constraints = timing_constraints::Constraints::new(&base_domains, timing.ordering_pairs());
    let mut visited = 0usize;
    while let Some(node) = queue.pop() {
        visited += 1;
        if visited == 64 {
            if let Some(value) = frame_optimization::score(input, timing, order_index, best)? { return Ok(value); }
        }
        queue_bytes -= node.bytes();
        crate::clock::milliseconds();
        if generation != crate::cancellation_generation() { return Err("cancelled".into()); }
        if if best { node.bound <= incumbent } else { node.bound >= incumbent } { continue; }
        let mut prefix = base_domains.clone();
        for (note, domain) in node.domains { prefix[note] = domain; }
        if !constraints.propagate(&mut prefix) { continue; }
        let mut selected = nominal.clone();
        for &(note, at) in &node.delivery { selected[note] = at; }
        let mut bound = node.bound;
        if selected.iter().enumerate().any(|(note, at)| !prefix[note].contains(at)) {
            let (value, delivery) = relax(&prefix, evaluate);
            bound = value;
            if if best { bound <= incumbent } else { bound >= incumbent } { continue; }
            selected = prefer_nominal(&preferred, &windows, &prefix, bound, delivery, evaluate).selected();
        }
        let checked = timing.conflict(&selected);
        if let Err(error) = checked {
            if error != "cancelled" {
                if let Some(value) = frame_optimization::score(input, timing, order_index, best)? { return Ok(value); }
            }
            return Err(error);
        }
        let Some(conflict) = checked.unwrap() else {
            incumbent = bound;
            preferred = selected;
            continue;
        };
        {
            let nogood: Vec<_> = conflict.notes.iter().map(|&note| {
                let forbidden = frames[note].iter().copied().filter(|&frame| conflict.interval.map_or(frame == selected[note], |(start, end)| {
                    frame < start && selected[note] < start || frame > end && selected[note] > end || frame == selected[note]
                })).collect();
                (note, forbidden)
            }).collect();
            if incumbent.is_finite() && nogood.iter().all(|(note, forbidden): &(usize, Vec<i32>)| forbidden.contains(&preferred[*note])) {
                return Err("native input conflict proof contradiction".into());
            }
            constraints.add(nogood);
        }
        for note in conflict.notes {
            let same = |frame: i32| conflict.interval.map_or(frame == selected[note], |(start, end)| {
                frame < start && selected[note] < start || frame > end && selected[note] > end || frame == selected[note]
            });
            let mut domains = prefix.clone();
            restrict_domain(&mut domains[note], |frame| !same(frame));
            restrict_domain(&mut prefix[note], same);
            if domains[note].is_empty() { continue; }
            if !constraints.propagate(&mut domains) { continue; }
            let (bound, delivery) = relax(&domains, evaluate);
            if if best { bound <= incumbent } else { bound >= incumbent } { continue; }
            let delivery = prefer_nominal(&preferred, &windows, &domains, bound, delivery, evaluate);
            serial += 1;
            let node = TimingNode { priority: priority(bound), serial, bound,
                domains: domains.into_iter().enumerate().filter(|(i, domain)| !Rc::ptr_eq(domain, &base_domains[*i])).collect(),
                delivery: delivery_changes(delivery),
            };
            queue_bytes += node.bytes();
            if queue_bytes > 16 * 1_048_576 { return Err("native timing search budget exceeded".into()); }
            queue.push(node);
        }
    }
    if incumbent.is_finite() { Ok(incumbent) } else { Err("unreachable native input sequence".into()) }
}
impl Delivery {
    fn move_note(&mut self, note: usize, time: i32, positions: &mut [usize]) {
        let mut position = positions[note];
        self.frames[position] = time;
        while position > 0 && (self.frames[position], self.notes[position]) < (self.frames[position - 1], self.notes[position - 1]) {
            self.frames.swap(position, position - 1);
            self.notes.swap(position, position - 1);
            positions[self.notes[position]] = position;
            positions[self.notes[position - 1]] = position - 1;
            position -= 1;
        }
        while position + 1 < self.frames.len() && (self.frames[position], self.notes[position]) > (self.frames[position + 1], self.notes[position + 1]) {
            self.frames.swap(position, position + 1);
            self.notes.swap(position, position + 1);
            positions[self.notes[position]] = position;
            positions[self.notes[position + 1]] = position + 1;
            position += 1;
        }
    }
    fn from_frames(frames: &[i32]) -> Self {
        let mut rows: Vec<_> = frames.iter().copied().enumerate().map(|(i, frame)| (frame, i)).collect();
        rows.sort_unstable();
        Self { frames: rows.iter().map(|row| row.0).collect(), notes: rows.iter().map(|row| row.1).collect() }
    }
    fn selected(&self) -> Vec<i32> {
        let mut selected = vec![0; self.frames.len()];
        for (&frame, &note) in self.frames.iter().zip(&self.notes) { selected[note] = frame; }
        selected
    }
    fn new(input: &Input, offsets: &[i32]) -> Self {
        let times = &input.times;
        let mut rows: Vec<_> = times
            .iter()
            .zip(offsets)
            .enumerate()
            .map(|(i, (&time, &offset))| (input.frame_time(time.saturating_add(offset)), i))
            .collect();
        rows.sort_unstable();
        Self {
            frames: rows.iter().map(|row| row.0).collect(),
            notes: rows.iter().map(|row| row.1).collect(),
        }
    }
}
fn partition(len: usize, mut below: impl FnMut(usize) -> bool) -> usize {
    let (mut low, mut high) = (0, len);
    while low < high {
        let middle = (low + high) / 2;
        if below(middle) {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}
fn slot_cumulative_commands(
    input: &Input,
    slot: &Slot,
    event: usize,
    judgements: Option<&[usize]>,
    delivery: Option<&Delivery>,
) -> Vec<Command> {
    let n = input.times.len();
    let start = input.skill_times[event];
    let frame_at =
        |k: usize| delivery.map_or_else(|| input.frame_time(input.times[k]), |d| d.frames[k]);
    let note_at = |k: usize| delivery.map_or(k, |d| d.notes[k]);
    let mut commands = Vec::new();
    for factor in &slot.cumulative {
        let activation = input.frame_time(start);
        let end = start + factor.duration_ms();
        let (mut count, mut applied) = (0, 0);
        let mut first = partition(n, |k| frame_at(k) < activation);
        while first < n {
            let frame = frame_at(first);
            if frame > end {
                break;
            }
            let after = first + partition(n - first, |k| frame_at(first + k) <= frame);
            count += (first..after)
                .filter(|&k| {
                    factor
                        .judgements
                        .contains(&judgements.map_or(5, |grades| grades[note_at(k)]))
                })
                .count() as i32;
            let mill = factor.mill(count);
            if mill != applied {
                let time = if frame == activation { start } else { frame };
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
    commands
}
fn cumulative_commands(
    input: &Input,
    order: &[usize; 5],
    judgements: Option<&[usize]>,
    delivery: Option<&Delivery>,
) -> Vec<Command> {
    let mut commands = Vec::new();
    for (event, &position) in order.iter().enumerate().take(input.skill_times.len()) {
        if let Some(slot) = input.slots.get(position) {
            commands.extend(slot_cumulative_commands(
                input, slot, event, judgements, delivery,
            ));
        }
    }
    commands.sort_by_key(|command| (command.time, command.owner));
    commands
}

fn commands(input: &Input, order: &[usize; 5], delivery: Option<&Delivery>) -> Vec<Command> {
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
    commands.extend(cumulative_commands(input, order, None, delivery));
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
/// All-perfect scoring with the factor prefix sums shared across orders.
type ApBlockScores = rustc_hash::FxHashMap<(usize, u64, u64), (f64, f64, f64)>;
struct ApBlocks {
    events: Vec<usize>,
    starts: Vec<usize>,
    ends: Vec<usize>,
    scores: Vec<ApBlockScores>,
}
struct ApScorer<'a> {
    input: &'a Input,
    base: Vec<f64>,
    luck: f64,
    assist: f64,
    prefixes: HashMap<u64, Vec<f64>>,
    prefix_work: rustc_hash::FxHashMap<u64, usize>,
    commands: Option<Vec<Vec<Vec<Command>>>>,
    ranges: rustc_hash::FxHashMap<(u64, usize, usize), f64>,
    magnitude: f64,
    blocks: Option<ApBlocks>,
    blocks_checked: bool,
}
impl<'a> ApScorer<'a> {
    fn new(input: &'a Input) -> Self {
        let n = input.times.len();
        let t = f(f(input.adjustment * input.power as f64) * input.difficulty_factor);
        let perfect = input.judge_frac[5];
        let base: Vec<_> = (0..n)
            .map(|i| f(f(perfect * f(input.note_frac[i] * t)) * input.ap_combo[i]))
            .collect();
        let magnitude = base.iter().fold(0.0_f64, |value, note| {
            if note.is_finite() { value.max(note.abs()) } else { f64::INFINITY }
        });
        Self {
            input,
            base,
            luck: f(input.luck_percent.min(200.0) / 100.0),
            assist: f(input.assist),
            prefixes: HashMap::new(),
            prefix_work: rustc_hash::FxHashMap::default(),
            commands: None,
            ranges: rustc_hash::FxHashMap::default(),
            magnitude,
            blocks: None,
            blocks_checked: false,
        }
    }
    fn integer_bound(&self, factor: f64) -> f64 {
        (f(self.assist.abs() * (f(f(self.luck.abs()
            * f(self.magnitude * factor.abs())) / self.input.converted_count as f64).abs()
            + 1.0)).abs() + 1.0) * self.base.len() as f64
    }
    fn sum(&mut self, start: usize, end: usize, factor: f64) -> f64 {
        // Below the integer precision limit, summing a range is identical to
        // subtracting full-chart prefixes. Large inputs retain prefix order.
        if self.integer_bound(factor) < (1u64 << 52) as f64 {
            if let Some(sums) = self.prefixes.get(&factor.to_bits()) { return sums[end] - sums[start]; }
            if self.ranges.len() >= 4096 { self.ranges.clear(); }
            let key = (factor.to_bits(), start, end);
            if let Some(&value) = self.ranges.get(&key) { return value; }
            let work = self.prefix_work.entry(factor.to_bits()).or_default();
            *work = work.saturating_add(end - start);
            if *work >= self.base.len().max(32) {
                let sums = self.factor_prefix(factor);
                return sums[end] - sums[start];
            }
            let value = self.base[start..end].iter().map(|&note| {
                    let score = f(f(self.luck * f(note * factor)) / self.input.converted_count as f64).floor();
                    if self.assist == 1.0 { score } else { f(self.assist * score).floor() }
                }).sum();
            self.ranges.insert(key, value);
            value
        } else {
            let sums = self.factor_prefix(factor);
            sums[end] - sums[start]
        }
    }
    fn factor_prefix(&mut self, factor: f64) -> &[f64] {
        let limit = (8 * 1_048_576 / ((self.base.len() + 1) * std::mem::size_of::<f64>())).max(1);
        if self.prefixes.len() >= limit && !self.prefixes.contains_key(&factor.to_bits()) {
            self.prefixes.clear();
            self.prefix_work.clear();
        }
        prefix(&mut self.prefixes, &self.base, factor, self.luck, self.assist, self.input.converted_count)
    }
    fn prepare_blocks(&mut self) {
        self.blocks_checked = true;
        let table = self.commands.as_ref().unwrap();
        if table.is_empty() { return; }
        let mut events: Vec<_> = (0..table.len()).collect();
        events.sort_by_key(|&event| (self.input.skill_times[event], event));
        for (position, &event) in events.iter().enumerate() {
            let start = self.input.skill_times[event];
            let next = events.get(position + 1).map(|&next| self.input.skill_times[next]);
            if next.is_some_and(|next| next <= start) || table[event].iter().any(|commands| {
                commands.first().is_some_and(|command| command.time < start)
                    || next.is_some_and(|next| commands.last().is_some_and(|command| command.time >= next))
            }) {
                return;
            }
        }
        let factor = table.iter().flatten().flatten().fold(1.0_f32, |factor, command| {
            (factor + (command.delta as f32 / 100000.0_f32).abs()).next_up()
        }).next_up() as f64;
        let bound = self.integer_bound(factor);
        if !bound.is_finite() || bound >= (1u64 << 50) as f64 { return; }
        let starts: Vec<_> = events.iter().map(|&event| {
            self.input.times.partition_point(|&time| time < self.input.skill_times[event])
        }).collect();
        let ends = starts.iter().copied().skip(1).chain(std::iter::once(self.base.len())).collect();
        let scores = (0..events.len()).map(|_| rustc_hash::FxHashMap::default()).collect();
        self.blocks = Some(ApBlocks { events, starts, ends, scores });
    }
    fn block_order(&mut self, order: &[usize; 5]) -> f64 {
        let mut blocks = self.blocks.take().unwrap();
        let mut score = self.sum(0, blocks.starts[0], 1.0);
        let (mut general, mut perfect) = (1.0_f64, 0.0_f64);
        for (position, &event) in blocks.events.iter().enumerate() {
            let slot = order[event];
            let key = (slot, general.to_bits(), perfect.to_bits());
            let (value, next_general, next_perfect) = if let Some(&cached) = blocks.scores[position].get(&key) {
                cached
            } else {
                let commands = self.commands.as_ref().unwrap()[event].get(slot).cloned().unwrap_or_default();
                let mut at = blocks.starts[position];
                let mut value = 0.0;
                for command in commands {
                    let end = self.input.times.partition_point(|&time| time < command.time);
                    if end > at {
                        value += self.sum(at, end, f(general + perfect));
                        at = end;
                    }
                    let delta = f(command.delta as f64 / 100000.0);
                    if command.channel == 0 { general = f(general + delta); }
                    else if command.channel == 5 { perfect = f(perfect + delta); }
                }
                if at < blocks.ends[position] {
                    value += self.sum(at, blocks.ends[position], f(general + perfect));
                }
                let cached = (value, general, perfect);
                blocks.scores[position].insert(key, cached);
                cached
            };
            score += value;
            general = next_general;
            perfect = next_perfect;
        }
        self.blocks = Some(blocks);
        score
    }
    fn order(&mut self, order: &[usize; 5], delivery: Option<&Delivery>) -> f64 {
        let input = self.input;
        let n = input.times.len();
        let (mut general, mut perfect_up) = (f(1.0), f(0.0));
        let mut at = 0;
        let mut score = 0.0;
        let commands = if delivery.is_some() {
            commands(input, order, delivery)
        } else {
            self.commands.get_or_insert_with(|| {
                input.skill_times.iter().take(5).enumerate().map(|(event, _)| {
                    (0..input.slots.len()).map(|slot| {
                        let mut order = [usize::MAX; 5];
                        order[event] = slot;
                        commands(input, &order, None)
                    }).collect()
                }).collect()
            });
            if !self.blocks_checked { self.prepare_blocks(); }
            if self.blocks.is_some() { return self.block_order(order); }
            let table = self.commands.as_ref().unwrap();
            let mut combined = Vec::new();
            for (event, &slot) in order.iter().enumerate().take(table.len()) {
                if let Some(commands) = table[event].get(slot) {
                    combined.extend_from_slice(commands);
                }
            }
            combined.sort_by_key(|command| (command.time, command.owner));
            combined
        };
        for command in commands {
            let end = input.times.partition_point(|&time| time < command.time);
            if end > at {
                score += self.sum(at, end, f(general + perfect_up));
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
            score += self.sum(at, n, f(general + perfect_up));
        }
        score
    }
}
fn ap(input: &Input) -> Scores {
    let mut scorer = ApScorer::new(input);
    let mut scores = vec![0.0; 120];
    let plan = plan(&input.slots);
    for &index in &plan.representatives {
        scores[index] = scorer.order(&orders()[index], None);
    }
    for (index, &source) in plan.sources.iter().enumerate() {
        scores[index] = scores[source];
    }
    summarize(input.power, scores)
}
pub fn frame_time(time: i32) -> i32 {
    FrameClock::default().frame_time(time)
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
/// Integer LIFE changes only at damage and recovery events. Zero-damage notes
/// share the preceding value; gates additionally see damage through their frame.
pub(crate) struct LifeProgram {
    damage: Vec<(usize, f64)>,
    prefix: Vec<f64>,
    events: Vec<usize>,
    boundaries: [usize; 5],
    delivered: [usize; 5],
}
pub(crate) struct LifeSummary {
    pub(crate) first_dead: usize,
    pub(crate) gates: [f64; 5],
}
impl LifeProgram {
    pub(crate) fn new(input: &Input, grades: &[usize]) -> Option<Self> {
        if !input.life_base.is_finite()
            || input.life_base < 0.0
            || input.life_base.fract() != 0.0
            || input
                .damage
                .iter()
                .any(|value| !value.is_finite() || *value < 0.0 || value.fract() != 0.0)
            || input
                .skill_times
                .iter()
                .take(5)
                .any(|&time| input.frame_time(time) >= input.times.last().copied().unwrap_or(0))
        {
            return None;
        }
        let damage: Vec<_> = grades
            .iter()
            .enumerate()
            .filter_map(|(index, &grade)| {
                let value = input.damage[grade];
                (value != 0.0).then_some((index, value))
            })
            .collect();
        let mut prefix = Vec::with_capacity(damage.len() + 1);
        prefix.push(0.0);
        for &(_, value) in &damage {
            prefix.push(prefix.last().unwrap() + value);
        }
        if input.life_base * 2.0 + prefix.last().unwrap() >= (1u64 << 52) as f64 {
            return None;
        }
        let boundaries = std::array::from_fn(|event| {
            input
                .skill_times
                .get(event)
                .map_or(input.times.len(), |&start| {
                    input.times.partition_point(|&time| time <= start)
                })
        });
        let delivered = std::array::from_fn(|event| {
            input.skill_times.get(event).map_or(damage.len(), |&start| {
                damage.partition_point(|&(index, _)| input.times[index] <= input.frame_time(start))
            })
        });
        Some(Self {
            damage,
            prefix,
            events: firing(input),
            boundaries,
            delivered,
        })
    }
    pub(crate) fn summary(&self, input: &Input, recoveries: [i32; 5]) -> Option<LifeSummary> {
        if recoveries.iter().any(|&value| value < 0)
            || input.life_base * 2.0
                + self.prefix.last().unwrap()
                + recoveries.iter().map(|&value| value as f64).sum::<f64>()
                >= (1u64 << 52) as f64
        {
            return None;
        }
        let mut out = LifeSummary {
            first_dead: input.times.len(),
            gates: [0.0; 5],
        };
        if input.life_base == 0.0 {
            out.first_dead = 0;
            return Some(out);
        }
        let (mut damage, mut life) = (0, input.life_base);
        for event in self
            .events
            .iter()
            .copied()
            .map(Some)
            .chain(std::iter::once(None))
        {
            let boundary = event.map_or(input.times.len(), |event| self.boundaries[event]);
            let end = self.damage.partition_point(|&(index, _)| index < boundary);
            let loss = self.prefix[end] - self.prefix[damage];
            if life <= loss {
                let target = self.prefix[damage] + life;
                let index = self.prefix[damage + 1..=end].partition_point(|&value| value < target);
                out.first_dead = self.damage[damage + index].0;
                return Some(out);
            }
            life -= loss;
            damage = end;
            if let Some(event) = event {
                life = (input.life_base * 2.0).min(life + recoveries[event] as f64);
                out.gates[event] =
                    (life - (self.prefix[self.delivered[event]] - self.prefix[damage])).max(0.0);
            }
        }
        Some(out)
    }
    pub(crate) fn play(&self, input: &Input, recoveries: [i32; 5]) -> Option<Lives> {
        if recoveries.iter().any(|&value| value < 0)
            || input.life_base * 2.0
                + self.prefix.last().unwrap()
                + recoveries.iter().map(|&value| value as f64).sum::<f64>()
                >= (1u64 << 52) as f64
        {
            return None;
        }
        let mut note = vec![0.0; input.times.len()];
        let mut gate = HashMap::with_capacity(self.events.len());
        let (mut cursor, mut damage, mut life) = (0, 0, input.life_base);
        for event in self
            .events
            .iter()
            .copied()
            .map(Some)
            .chain(std::iter::once(None))
        {
            let boundary = event.map_or(note.len(), |event| self.boundaries[event]);
            while damage < self.damage.len() && self.damage[damage].0 < boundary {
                let (index, amount) = self.damage[damage];
                note[cursor..index].fill(life);
                life = (life - amount).max(0.0);
                note[index] = life;
                if life == 0.0 {
                    for &event in &self.events {
                        gate.entry(event).or_insert(0.0);
                    }
                    return Some(Lives { note, gate });
                }
                cursor = index + 1;
                damage += 1;
            }
            note[cursor..boundary].fill(life);
            cursor = boundary;
            if let Some(event) = event {
                if life > 0.0 {
                    life = (input.life_base * 2.0).min(life + recoveries[event] as f64);
                }
                gate.insert(
                    event,
                    (life - (self.prefix[self.delivered[event]] - self.prefix[damage])).max(0.0),
                );
            }
        }
        Some(Lives { note, gate })
    }
}
pub(crate) fn lives(
    input: &Input,
    judgements: &[usize],
    order: &[usize; 5],
    slots: &[Slot],
) -> Lives {
    if let Some(lives) = LifeProgram::new(input, judgements).and_then(|program| program.play(input,
        order.map(|slot| slots.get(slot).map_or(0, |slot| slot.recovery)))) {
        return lives;
    }
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
                    frame: input.frame_time(start),
                    amount: recovery,
                });
            }
            let frame = input.frame_time(start);
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
fn conversion(input: &Input, slot: &Slot, convert: &Conversion, start: i32) -> Convert {
    let from = input.frame_time(start);
    let duration = convert.seconds.map_or_else(
        || window_ms(slot.effects.first().map(|effect| effect.seconds).unwrap_or(5.0), slot.extension_ms),
        |seconds| window_ms(seconds, 0.0),
    );
    Convert {
        from,
        until: input.frame_time(from + duration),
        judgements: convert.judgements.clone(),
        left: convert.limit.unwrap_or(f64::INFINITY),
        judgement: convert.judgement,
    }
}
pub(crate) fn converted_judgements(input: &Input, order: &[usize; 5], judgements: &[usize]) -> Vec<usize> {
    let mut converted = judgements.to_vec();
    if input.slots.iter().all(|slot| slot.convert.is_none()) {
        return converted;
    }
    let firing = firing(input);
    let mut cursor = 0;
    let mut converts: Vec<Convert> = Vec::new();
    for slot in &input.slots {
        if let Some(convert) = &slot.convert {
            if convert.at_start { converts.insert(0, conversion(input, slot, convert, 0)); }
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
                    if !convert.at_start { converts.insert(0, conversion(input, slot, convert, start)); }
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
pub(crate) fn isolated_judgements(
    input: &Input,
    slot: &Slot,
    event: usize,
    grades: &[usize],
) -> Vec<usize> {
    let mut result = grades.to_vec();
    let Some(program) = &slot.convert else {
        return result;
    };
    let mut convert = conversion(input, slot, program, input.skill_times[event]);
    for (index, grade) in result.iter_mut().enumerate() {
        let time = input.times[index];
        if time > convert.until {
            break;
        }
        if time > convert.from && *grade != convert.judgement && convert.judgements.contains(grade)
        {
            *grade = convert.judgement;
            convert.left -= 1.0;
            if convert.left <= 0.0 {
                break;
            }
        }
    }
    result
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
/// Custom-judgement scoring of one order; life replays are shared across calls.
type PlayedRangeCache =
    rustc_hash::FxHashMap<(usize, usize, [u64; 8], usize, usize), (f64, Option<f64>)>;
struct PlayScorer<'a> {
    input: &'a Input,
    judgements: &'a [usize],
    note: Vec<f64>,
    firing: Vec<usize>,
    luck: f64,
    assist: f64,
    converter_ids: [usize; 5],
    trajectories: Vec<(Vec<usize>, Vec<f64>, Option<LifeProgram>)>,
    trajectory_ids: HashMap<[usize; 5], usize>,
    grade_buckets: HashMap<u64, Vec<usize>>,
    life_cache: HashMap<([i32; 5], usize), (Lives, usize)>,
    ranges: PlayedRangeCache,
    cumulative_cache: rustc_hash::FxHashMap<(usize, usize, usize), Vec<Command>>,
    cache_ranges: bool,
    curve_enabled: bool,
    curve_safe: bool,
    last_curve: Option<f64>,
}
impl<'a> PlayScorer<'a> {
    fn new(input: &'a Input, judgements: &'a [usize]) -> Self {
        let t = f(f(input.adjustment * input.power as f64) * input.difficulty_factor);
        let mut programs = HashMap::new();
        let converter_ids = std::array::from_fn(|slot| {
            let Some(value) = input.slots.get(slot).and_then(|slot| slot.convert.as_ref()) else {
                return 0;
            };
            // Start-of-live programs retain their fixed actor order in this scorer.
            if value.at_start {
                return 0;
            }
            let skill = &input.slots[slot];
            let duration = value.seconds.map_or_else(
                || {
                    window_ms(
                        skill
                            .effects
                            .first()
                            .map(|effect| effect.seconds)
                            .unwrap_or(5.0),
                        skill.extension_ms,
                    )
                },
                |seconds| window_ms(seconds, 0.0),
            );
            let mut key = vec![
                duration as u64,
                value.limit.map_or(u64::MAX, f64::to_bits),
                value.judgement as u64,
            ];
            key.extend(value.judgements.iter().map(|&grade| grade as u64));
            let next = programs.len() + 1;
            *programs.entry(key).or_insert(next)
        });
        let note: Vec<_> = input.note_frac.iter().map(|&v| f(v * t)).collect();
        let mut actions = 0usize;
        let mut magnitude = 1.0_f64;
        for slot in &input.slots {
            for effect in &slot.effects {
                let count = if effect.kind == 2000 {
                    1
                } else {
                    effect.judgements.len()
                };
                actions = actions.saturating_add(count.saturating_mul(2));
                magnitude = (magnitude
                    + f(effect.delta as f64 / 100000.0).abs() * count as f64 * 2.0)
                    .next_up();
            }
            for factor in &slot.cumulative {
                let count = usize::try_from(factor.cap)
                    .unwrap_or(usize::MAX)
                    .saturating_add(1);
                actions = actions.saturating_add(count.saturating_mul(2));
                magnitude =
                    (magnitude + factor.magnitude_cap() / 100000.0 * count as f64 * 3.0).next_up();
            }
        }
        let note_bound = note
            .iter()
            .map(|value| {
                if value.is_finite() {
                    value.abs()
                } else {
                    f64::INFINITY
                }
            })
            .fold(0.0, f64::max);
        let judge_bound = input
            .judge_frac
            .iter()
            .map(|value| {
                if value.is_finite() {
                    value.abs()
                } else {
                    f64::INFINITY
                }
            })
            .fold(0.0, f64::max);
        let raw = f(f(input.luck_percent.min(200.0).abs() / 100.0
            * f(note_bound * judge_bound * 2.0 * magnitude * 2.0))
            / input.converted_count as f64)
        .abs()
            + 1.0;
        let life = f(input.life_onus.abs().max(1.0) * raw).abs() + 1.0;
        let bound = (f(input.assist.abs() * life).abs() + 1.0) * input.times.len() as f64;
        let cache_ranges = actions as f64 * 2.0_f64.powi(-24) < 0.25
            && bound.is_finite()
            && bound < (1u64 << 50) as f64
            && input
                .combo_table
                .iter()
                .all(|row| row.1.is_finite() && row.1 >= 0.0);
        let max_scalar = f(f(input.adjustment * i32::MAX as f64) * input.difficulty_factor).abs();
        let max_fraction = input
            .note_frac
            .iter()
            .copied()
            .map(f64::abs)
            .fold(0.0, f64::max);
        let curve_safe = actions as f64 * 2.0_f64.powi(-24) < 0.25
            && max_scalar.is_finite()
            && max_scalar * max_fraction * judge_bound * 4.0 * magnitude < (f32::MAX as f64) / 4.0;
        Self {
            input,
            judgements,
            note,
            firing: firing(input),
            luck: f(input.luck_percent.min(200.0) / 100.0),
            assist: f(input.assist),
            converter_ids,
            trajectories: Vec::new(),
            trajectory_ids: HashMap::new(),
            grade_buckets: HashMap::new(),
            life_cache: HashMap::new(),
            ranges: rustc_hash::FxHashMap::default(),
            cumulative_cache: rustc_hash::FxHashMap::default(),
            cache_ranges,
            curve_enabled: false,
            curve_safe,
            last_curve: None,
        }
    }
    fn trajectory(&mut self, order: &[usize; 5]) -> usize {
        let key = order.map(|slot| self.converter_ids[slot]);
        if let Some(&index) = self.trajectory_ids.get(&key) {
            return index;
        }
        let converted = converted_judgements(self.input, order, self.judgements);
        use std::hash::{Hash, Hasher};
        let mut hash = rustc_hash::FxHasher::default();
        converted.hash(&mut hash);
        let hash = hash.finish();
        if let Some(indices) = self.grade_buckets.get(&hash) {
            if let Some(&index) = indices
                .iter()
                .find(|&&index| self.trajectories[index].0 == converted)
            {
                self.trajectory_ids.insert(key, index);
                return index;
            }
        }
        let mut combos = Vec::with_capacity(converted.len());
        let (mut pending, mut before, mut last) = (0, 0, -1);
        for (i, &grade) in converted.iter().enumerate() {
            if self.input.times[i] != last {
                before = pending;
                last = self.input.times[i];
            }
            combos.push(combo(&self.input.combo_table, before));
            pending = if grade <= 2 { 0 } else { pending + 1 };
        }
        let index = self.trajectories.len();
        let life = LifeProgram::new(self.input, &converted);
        self.trajectories.push((converted, combos, life));
        self.trajectory_ids.insert(key, index);
        self.grade_buckets.entry(hash).or_default().push(index);
        index
    }
    fn order(&mut self, order: &[usize; 5], delivery: Option<&Delivery>) -> f64 {
        let mut curve = self.curve_enabled.then_some(0.0_f64);
        let trajectory = self.trajectory(order);
        let (input, firing, note) = (self.input, &self.firing, &self.note);
        let (luck, assist) = (self.luck, self.assist);
        let n = input.times.len();
        let recoveries = order.map(|slot| input.slots.get(slot).map(|s| s.recovery).unwrap_or(0));
        let (converted, combos, life_program) = &self.trajectories[trajectory];
        let fallback_life_id = n + 1 + self.life_cache.len();
        let (lives, life_id) = self
            .life_cache
            .entry((recoveries, trajectory))
            .or_insert_with(|| {
                let lives = life_program
                    .as_ref()
                    .and_then(|program| program.play(input, recoveries))
                    .unwrap_or_else(|| lives(input, converted, order, &input.slots));
                let first = lives.note.iter().position(|life| *life <= 0.0).unwrap_or(n);
                let id = if lives.note[first..].iter().all(|life| *life <= 0.0) {
                    first
                } else {
                    fallback_life_id
                };
                (lives, id)
            });
        let mut factors = Factors {
            general: f(1.0),
            channel: [0.0; 7],
            active: Vec::new(),
        };
        let mut cursor = 0;
        let cumulative = if delivery.is_none() {
            let mut commands = Vec::new();
            for (event, &position) in order.iter().enumerate().take(input.skill_times.len()) {
                let Some(slot) = input
                    .slots
                    .get(position)
                    .filter(|slot| !slot.cumulative.is_empty())
                else {
                    continue;
                };
                if self.cumulative_cache.len() >= 512 {
                    self.cumulative_cache.clear();
                }
                commands.extend_from_slice(
                    self.cumulative_cache
                        .entry((trajectory, event, position))
                        .or_insert_with(|| {
                            slot_cumulative_commands(input, slot, event, Some(converted), None)
                        }),
                );
            }
            commands.sort_by_key(|command| (command.time, command.owner));
            commands
        } else {
            cumulative_commands(input, order, Some(converted), delivery)
        };
        let mut pending = cumulative.into_iter().peekable();
        let mut score = 0.0;
        let mut i = 0;
        while i <= n {
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
                        let end = effect_end(input, slot, effect, start, Some(converted));
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
            let next = if self.cache_ranges {
                let mut boundary = 1i64 << 53;
                if let Some(&event) = firing.get(cursor) {
                    boundary = boundary.min(input.skill_times[event] as i64);
                }
                if let Some(command) = pending.peek() {
                    boundary = boundary.min(command.time as i64);
                }
                for effect in &factors.active {
                    boundary = boundary.min(effect.end as i64);
                }
                input
                    .times
                    .partition_point(|&time| (time as i64) < boundary)
                    .max(i + 1)
            } else {
                i + 1
            };
            let collect_curve = self.curve_enabled;
            let sum = || {
                (i..next).fold(
                    (0.0, collect_curve.then_some(0.0_f64)),
                    |(sum, curve), index| {
                        let judgement = converted[index];
                        let up = f(factors.general + factors.channel[judgement]);
                        let raw = f(f(luck
                            * f(
                                f(f(input.judge_frac[judgement] * note[index]) * combos[index])
                                    * up,
                            ))
                            / input.converted_count as f64);
                        let mut upper = raw;
                        if lives.note[index] <= 0.0 {
                            upper = f(input.life_onus * upper);
                        }
                        if assist != 1.0 {
                            upper = f(assist * upper);
                        }
                        let mut value = raw.floor();
                        if lives.note[index] <= 0.0 {
                            value = f(input.life_onus * value).floor();
                        }
                        if assist != 1.0 {
                            value = f(assist * value).floor();
                        }
                        (
                            sum + value,
                            curve.and_then(|total| {
                                (upper.is_finite() && upper >= 0.0)
                                    .then(|| (total + upper).next_up())
                            }),
                        )
                    },
                )
            };
            let (value, upper) = if self.cache_ranges {
                if self.ranges.len() >= 4096 {
                    self.ranges.clear();
                }
                let state = std::array::from_fn(|channel| {
                    if channel == 0 {
                        factors.general.to_bits()
                    } else {
                        factors.channel[channel - 1].to_bits()
                    }
                });
                *self
                    .ranges
                    .entry((trajectory, *life_id, state, i, next))
                    .or_insert_with(sum)
            } else {
                sum()
            };
            score += value;
            curve = curve
                .zip(upper)
                .map(|(total, value)| (total + value).next_up());
            i = next;
        }
        self.last_curve = curve;
        score
    }
}
fn play(
    input: &Input,
    judgements: &[usize],
    collect_curve: bool,
) -> Result<(Scores, Option<[f64; 3]>), String> {
    let generation = crate::cancellation_generation();
    let mut scorer = PlayScorer::new(input, judgements);
    scorer.curve_enabled = collect_curve
        && scorer.curve_safe
        && input.power > 0
        && input.assist == 1.0
        && input.luck_percent == 100.0
        && (1e-6..=1e6).contains(&input.adjustment)
        && (1e-6..=1e6).contains(&input.difficulty_factor)
        && (0.0..=1.0).contains(&input.life_onus)
        && input
            .note_frac
            .iter()
            .all(|&value| value == 0.0 || (1e-6..=1e6).contains(&value))
        && input
            .judge_frac
            .iter()
            .all(|&value| value == 0.0 || (1e-6..=10.0).contains(&value));
    let mut scores = vec![0.0; 120];
    let mut curves = vec![None; 120];
    let plan = plan(&input.slots);
    for &index in &plan.representatives {
        if generation != crate::cancellation_generation() {
            return Err("cancelled".into());
        }
        scores[index] = scorer.order(&orders()[index], None);
        curves[index] = scorer.last_curve;
    }
    for (index, &source) in plan.sources.iter().enumerate() {
        scores[index] = scores[source];
        curves[index] = curves[source];
    }
    let curve = curves
        .iter()
        .copied()
        .collect::<Option<Vec<_>>>()
        .map(|values| {
            [
                (values
                    .iter()
                    .fold(0.0_f64, |sum, value| (sum + value).next_up())
                    / 120.0)
                    .next_up(),
                values.iter().copied().fold(f64::INFINITY, f64::min),
                values.iter().copied().fold(0.0, f64::max),
            ]
        });
    Ok((summarize(input.power, scores), curve))
}
pub fn score(input: &Input) -> Result<Scores, String> {
    score_internal(input, false).map(|value| value.0)
}
pub(crate) fn score_with_curve(input: &Input) -> Result<(Scores, Option<[f64; 3]>), String> {
    score_internal(input, true)
}fn score_internal(input:&Input,collect_curve:bool)->Result<(Scores,Option<[f64;3]>),String> {
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
        play(input, judgements,collect_curve)
    } else {
        let native_cumulative = input
            .slots
            .iter()
            .flat_map(|slot| &slot.cumulative)
            .any(|factor| factor.requires_unbounded_bounds(n));
        if native_cumulative {
            play(input, &vec![5; n],collect_curve)
        } else {
            Ok((ap(input),None))
        }
    }
}

/// Evenly distribute custom judgements in chart order with the same priority
/// and open-note selection as the native accuracy builder.
pub fn native_judgement(kind: i32, grade: usize) -> usize {
    if matches!(kind, 2 | 15 | 21 | 22) && matches!(grade, 2..=4) { 5 } else { grade }
}
pub fn judgements(count: usize, play: &serde_json::Value) -> Option<Vec<usize>> {
    judgement_pattern(count, play, None)
}
pub fn judgements_for_types(kinds: &[i32], play: &serde_json::Value) -> Option<Vec<usize>> {
    judgement_pattern(kinds.len(), play, Some(kinds))
}
fn judgement_pattern(count: usize, play: &serde_json::Value, kinds: Option<&[i32]>) -> Option<Vec<usize>> {
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
    for (judgement, fraction) in [(4, great), (3, good), (2, bad), (1, miss)] {
        let open: Vec<_> = out
            .iter()
            .enumerate()
            .filter(|(i, j)| **j == 5 && kinds.is_none_or(|types| native_judgement(types[*i], judgement) == judgement))
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

/// One step of a counting window and the chart notes that complete it.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingStep {
    pub step: i32,
    pub time_ms: i32,
    pub notes: usize,
    pub note_indices: Vec<usize>,
    pub gain: f64,
}
/// A cumulative snapshot's counting window: its counted notes and steps.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingWindow {
    pub event: usize,
    pub start_ms: i32,
    pub end_ms: i32,
    pub unit: i32,
    pub notes: Vec<i32>,
    pub steps: Vec<TimingStep>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Timing {
    pub score: f64,
    pub best: f64,
    pub guide_available: bool,
    pub advance_ms: i32,
    pub windows: Vec<TimingWindow>,
}
/// One 60 Hz frame: hitting a note this much early always crosses the frame
/// boundary before it, whatever the phase of the frame clock.
pub const TIMING_ADVANCE_MS: i32 = 17;

/// Cumulative snapshots count judgements in the frame that delivers them.
/// The first frame's factor starts at the skill event's chart time; later
/// changes start at their delivery frame. Notes score at their chart time.
/// Moving a completing note one frame early can put its score after the change;
/// moving the window's first notes early can drop them before activation.
/// `before_ms` follows `input.times` and allows only inputs retaining their base judgement.
pub fn timing(input: &Input, order_index: usize, before_ms: &[i32]) -> Option<Timing> {
    timing_with_inputs(input, order_index, before_ms, None)
}
pub fn timing_with_inputs(input: &Input, order_index: usize, before_ms: &[i32], inputs: Option<&crate::input_timing::InputTiming>) -> Option<Timing> {
    let order = orders().get(order_index)?;
    let n = input.times.len();
    let advance_ms = input.clock.advance_ms();
    let judgements = match &input.judgements {
        Some(grades) => Some(grades.clone()),
        None if input.slots.iter().flat_map(|slot| &slot.cumulative).any(|factor| factor.requires_unbounded_bounds(n)) => Some(vec![5; n]),
        None => None,
    };
    let converted = judgements.as_ref().map(|grades| converted_judgements(input, order, grades));
    let grade = |i: usize| converted.as_ref().map_or(5, |grades| grades[i]);
    let mut windows = Vec::new();
    let mut groups: Vec<(usize, usize, Vec<usize>)> = Vec::new();
    for (event, &start) in input.skill_times.iter().take(5).enumerate() {
        let Some(slot) = input.slots.get(order[event]) else { continue };
        for factor in slot.cumulative.iter().filter(|factor| !factor.judgements.is_empty()) {
            let activation = input.frame_time(start);
            let end = start + factor.duration_ms();
            let mut window = TimingWindow { event, start_ms: activation, end_ms: end, unit: factor.unit, notes: Vec::new(), steps: Vec::new() };
            let (mut count, mut applied) = (0, 0);
            let mut first = input.times.partition_point(|&time| input.frame_time(time) < activation);
            while first < n {
                let frame = input.frame_time(input.times[first]);
                if frame > end {
                    break;
                }
                let after = input.times.partition_point(|&time| input.frame_time(time) <= frame);
                let matching: Vec<usize> = (first..after).filter(|&i| factor.judgements.contains(&grade(i))).collect();
                window.notes.extend(matching.iter().map(|&i| input.times[i]));
                count += matching.len() as i32;
                let mill = factor.mill(count);
                let chord: Vec<usize> = (first..after).filter(|&i| before_ms[i] >= advance_ms).collect();
                if mill != applied && !chord.is_empty() {
                    let last = input.times[*chord.last().unwrap()];
                    groups.push((windows.len(), window.steps.len(), chord.clone()));
                    window.steps.push(TimingStep { step: (count / factor.unit).min(factor.cap), time_ms: last, notes: chord.len(), note_indices: chord, gain: 0.0 });
                }
                applied = mill;
                first = after;
            }
            windows.push(window);
        }
    }
    if windows.is_empty() {
        return None;
    }
    enum Scorer<'a> {
        Ap(ApScorer<'a>),
        Play(PlayScorer<'a>),
    }
    let mut scorer = match &judgements {
        Some(grades) => Scorer::Play(PlayScorer::new(input, grades)),
        None => Scorer::Ap(ApScorer::new(input)),
    };
    let mut evaluate = |offsets: &[i32], check_inputs: bool| {
        let delivery = Delivery::new(input, offsets);
        if let Some(inputs) = inputs.filter(|_| check_inputs) {
            if !matches!(inputs.conflict(&delivery.selected()), Ok(None)) { return None; }
        }
        Some(match &mut scorer {
            Scorer::Ap(scorer) => scorer.order(order, Some(&delivery)),
            Scorer::Play(scorer) => scorer.order(order, Some(&delivery)),
        })
    };
    let mut offsets = vec![0; n];
    let score = evaluate(&offsets, false)?;
    if evaluate(&offsets, true).is_none() {
        for window in &mut windows { window.steps.clear(); }
        return Some(Timing { score, best: score, guide_available: false, advance_ms, windows });
    }
    let mut best = score;
    for (window, step, chord) in groups {
        let previous: Vec<_> = chord.iter().map(|&i| offsets[i]).collect();
        for &i in &chord {
            offsets[i] = -advance_ms;
        }
        let Some(value) = evaluate(&offsets, true) else {
            for (&i, offset) in chord.iter().zip(previous) { offsets[i] = offset; }
            continue;
        };
        if value > best {
            windows[window].steps[step].gain = value - best;
            best = value;
        } else {
            for (&i, offset) in chord.iter().zip(previous) {
                offsets[i] = offset;
            }
        }
    }
    for window in &mut windows {
        window.steps.retain(|step| step.gain > 0.0);
    }
    if let Some(inputs) = inputs { let _ = inputs.conflict(&Delivery::new(input, &offsets).selected()); }
    Some(Timing { score, best, guide_available: true, advance_ms, windows })
}
#[cfg(test)]
mod delivery_extremes_tests {
    use super::*;
    fn input(value: i32, overlap: bool) -> Input {
        let factor = Cumulative { judgements: vec![5], unit: 2, value, maximum: 0, cap: 3, seconds: 0.2 };
        let slot = Slot { effects: vec![], cumulative: vec![factor], extension_ms: 0.0, recovery: 0, convert: None };
        Input { clock: FrameClock::default(), times: vec![105, 155, 205, 255], note_frac: vec![1.0, 0.1, 1.0, 1.0], ap_combo: vec![1.0; 4], combo_table: vec![], judge_frac: [0.0, 0.0, 0.0, 0.0, 0.8, 1.0, 1.0], converted_count: 4, difficulty_factor: 1.0, adjustment: 1.0, skill_times: if overlap { vec![100, 160] } else { vec![100, 400] }, life_base: 1000.0, life_onus: 1.0, damage: [0.0; 7], power: 1373, slots: vec![slot.clone(), slot], judgements: None, assist: 1.0, luck_percent: 100.0 }
    }
    #[test]
    fn timing_extremes_match_every_schedule_including_overlaps_and_negative_counters() {
        for overlap in [false, true] {
            for value in [1, 2000, -2000, 12345] {
                let input = input(value, overlap);
                let frames: Vec<Vec<i32>> = input.times.iter().enumerate().map(|(i, &t)| if i == 1 { vec![frame_time(t)] } else { vec![frame_time(t - 40), frame_time(t), frame_time(t + 40)] }).collect();
                let low = score_timed(&input, &frames, false).unwrap();
                let high = score_timed(&input, &frames, true).unwrap();
                let mut scorer = ApScorer::new(&input);
                for (index, order) in orders().iter().enumerate() {
                    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
                    for &a in &frames[0] { for &b in &frames[1] { for &c in &frames[2] { for &d in &frames[3] {
                        let offsets: Vec<_> = [a,b,c,d].iter().zip(&input.times).map(|(f,t)| f-t).collect();
                        let score = scorer.order(order, Some(&Delivery::new(&input, &offsets)));
                        lo = lo.min(score); hi = hi.max(score);
                    }}}}
                    assert_eq!(low.scores[index], lo, "minimum, overlap={overlap}, value={value}, order={index}");
                    assert_eq!(high.scores[index], hi, "maximum, overlap={overlap}, value={value}, order={index}");
                }
            }
        }
    }
}
