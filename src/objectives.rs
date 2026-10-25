//! Ordinary score and songless-potential objective adapters.
use crate::{
    chart::{self, Chart},
    live::{self, Input, Scores, Slot},
    master::Master,
    model::{self, Linear, PlayBounds, Weights, RELATIVE_ERROR},
    num::window_ms,
    power::{Member, Snap},
    search::{Exact, Objective, Team, Totals},
    skills,
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};
#[derive(Clone, Copy, PartialEq)]
pub enum Criterion {
    Mean,
    Min,
    Max,
}
impl Criterion {
    pub fn from_value(v: &Value) -> Self {
        match v.as_str() {
            Some("min") => Self::Min,
            Some("max") => Self::Max,
            _ => Self::Mean,
        }
    }
    pub fn key(self, scores: &Scores) -> f64 {
        match self {
            Self::Mean => scores.mean,
            Self::Min => scores.min,
            Self::Max => scores.max,
        }
    }
}
#[derive(Clone)]
pub(crate) struct Pair {
    pub high: Weights,
    pub low: Weights,
}
pub struct LiveCore {
    pub input: Input,
    pub high: Linear,
    pub low: Linear,
    pub factor_cap: f64,
    pub score_cap: f64,
    pub max_recovery: i32,
    pub drains: bool,
    pub slots: Vec<Slot>,
    ids: Vec<usize>,
    width: usize,
    weights: Vec<Option<Pair>>,
    team_models: HashMap<i32, (Linear, HashMap<usize, f64>)>,
    play_bounds: Option<PlayBounds>,
    scores: HashMap<(i32, [usize; 5]), Arc<Scores>>,
    score_order: VecDeque<(i32, [usize; 5])>,
    snap_power: Arc<Vec<f64>>,
    power_width: usize,
    base_power: f64,
    max_frontiers: Vec<[Vec<(f64, f64)>; 5]>,
    member_profiles: HashMap<usize, [f64; 5]>,
    suffix_profiles: Vec<[f64; 5]>,
    dominance_margin: f64,
    member_dominance: HashMap<(usize, usize), bool>,
}
/// Resolved skills of every member × snap pair and their shape ids, shared by the objectives of one engine call.
type SlotTable = Arc<skills::Prepared>;
/// Addresses and lengths of the master and the card slices the table was resolved from.
type SlotTableKey = (usize, usize, usize, usize, usize);
// The outer Option is present only while Engine::run owns the request scope.
type RequestSlotTable = Option<(SlotTableKey, SlotTable)>;
thread_local! {
    static SLOT_TABLE: std::cell::RefCell<Option<RequestSlotTable>> =
        const { std::cell::RefCell::new(None) };
}
/// Clears the current request's pointer lookup; the content cache validates itself.
pub fn reset_slot_table() {
    SLOT_TABLE.with(|t| {
        if let Some(table) = t.borrow_mut().as_mut() {
            table.take();
        }
    });
}
pub(crate) struct SlotTableScope(Option<RequestSlotTable>);
impl Drop for SlotTableScope {
    fn drop(&mut self) {
        SLOT_TABLE.with(|t| t.replace(self.0.take()));
    }
}
pub(crate) fn slot_table_scope() -> SlotTableScope {
    SlotTableScope(SLOT_TABLE.with(|t| t.replace(Some(None))))
}
fn slot_table(master: &Master, members: &[Member], snaps: &[Snap]) -> SlotTable {
    let key = (
        master as *const Master as usize,
        members.as_ptr() as usize,
        members.len(),
        snaps.as_ptr() as usize,
        snaps.len(),
    );
    if let Some(table) = SLOT_TABLE.with(|t| {
        t.borrow()
            .as_ref()
            .and_then(|table| table.as_ref())
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| Arc::clone(v))
    }) {
        return table;
    }
    let table = skills::prepare(master, members, snaps);
    SLOT_TABLE.with(|t| {
        if let Some(cache) = t.borrow_mut().as_mut() {
            *cache = Some((key, Arc::clone(&table)));
        }
    });
    table
}
impl LiveCore {
    pub fn new(
        master: &Master,
        chart: &Chart,
        members: &[Member],
        snaps: &[Snap],
        play: &Value,
    ) -> Self {
        let width = snaps.len() + 1;
        let table = slot_table(master, members, snaps);
        let mut slots = table.slots.clone();
        let judgements = live::judgements(chart.times.len(), play);
        let input = chart::prepare(master, chart, 0, Vec::new(), judgements, 1.0, 100.0);
        let mut raise = 0.0_f64;
        let mut recovery = 0;
        let mut longest = 0;
        for slot in &slots {
            if input.judgements.is_some() {
                raise = raise.max(
                    slot.effects
                        .iter()
                        .fold(0.0, |sum, e| sum + e.delta as f64 / 100000.0),
                );
                recovery = recovery.max(slot.recovery);
            }
            for effect in &slot.effects {
                longest = longest.max(window_ms(effect.seconds, slot.extension_ms));
            }
        }
        let mut overlap = 1;
        for &time in &input.skill_times {
            overlap = overlap.max(
                input
                    .skill_times
                    .iter()
                    .filter(|&&other| other <= time && time < other + longest)
                    .count(),
            );
        }
        let isolated =
            input.skill_times.iter().enumerate().all(|(i, &time)| {
                input.skill_times.iter().enumerate().all(|(j, &other)| {
                    i == j || (time as i64 - other as i64).abs() > longest as i64
                })
            });
        let (ids, intern_len) = if input.judgements.is_none() && isolated {
            let mut intern = HashMap::<Vec<u64>, usize>::new();
            let mut signature = Vec::new();
            let mut ids = Vec::with_capacity(slots.len());
            for slot in &mut slots {
                slot.recovery = 0;
                slot.convert = None;
                slot.effects
                    .retain(|effect| !effect.gate.blocks(input.life_base));
                for effect in &mut slot.effects {
                    effect.gate = crate::live::Gate {
                        kind: 0,
                        value: 0.0,
                        positive: true,
                    };
                }
                if slot.effects.is_empty() {
                    slot.extension_ms = 0.0;
                }
                slot.write_signature(&mut signature);
                let id = if let Some(&id) = intern.get(signature.as_slice()) {
                    id
                } else {
                    let id = intern.len();
                    intern.insert(std::mem::take(&mut signature), id);
                    id
                };
                ids.push(id);
            }
            (ids, intern.len())
        } else {
            let (ids, intern_len) = table.shapes();
            (ids.clone(), *intern_len)
        };
        let factor_cap = 1.0 + overlap as f64 * raise;
        let high = if let Some(j) = &input.judgements {
            model::linear_play(&input, j, true, 0, 1.0, recovery)
        } else {
            model::linear(&input)
        };
        let low = if let Some(j) = &input.judgements {
            model::linear_play(&input, j, false, 0, 1.0, 0)
        } else {
            high.clone()
        };
        let play_bounds = input
            .judgements
            .as_ref()
            .map(|j| PlayBounds::new(&input, j));
        let drains = play_bounds
            .as_ref()
            .is_some_and(|b| b.damage.iter().fold(0.0, |sum, r| sum + r.1) >= input.life_base);
        Self {
            input,
            high,
            low,
            factor_cap,
            score_cap: f64::INFINITY,
            max_recovery: recovery,
            drains,
            slots,
            ids,
            width,
            weights: vec![None; intern_len],
            team_models: HashMap::new(),
            play_bounds,
            scores: HashMap::new(),
            score_order: VecDeque::new(),
            snap_power: Arc::new(Vec::new()),
            power_width: 0,
            base_power: 0.0,
            max_frontiers: Vec::new(),
            member_profiles: HashMap::new(),
            suffix_profiles: Vec::new(),
            dominance_margin: f64::INFINITY,
            member_dominance: HashMap::new(),
        }
    }
    fn at(&self, member: usize, snap: i32) -> usize {
        member * self.width + (snap + 1) as usize
    }
    pub fn slot(&self, member: usize, snap: i32) -> &Slot {
        &self.slots[self.at(member, snap)]
    }
    pub(crate) fn weight(&mut self, member: usize, snap: i32) -> Pair {
        let index = self.at(member, snap);
        let id = self.ids[index];
        if let Some(pair) = &self.weights[id] {
            return pair.clone();
        }
        let pair = {
            let skill = &self.slots[index];
            let mut upper = model::skill_weights(&self.high, &self.input, skill);
            if let Some(j) = &self.input.judgements {
                if skill.convert.is_some() {
                    let extra = model::conversion_weights(&self.input, j, skill, self.factor_cap);
                    for (event, &value) in extra.iter().enumerate() {
                        upper.per_event[event] += value;
                    }
                    upper.mean = upper.per_event.iter().fold(0.0, |sum, v| sum + v) / 5.0;
                    upper.best = upper
                        .per_event
                        .iter()
                        .copied()
                        .fold(f64::NEG_INFINITY, f64::max);
                }
            }
            let lower = if self.input.judgements.is_none() {
                upper.clone()
            } else {
                model::skill_weights(&self.low, &self.input, skill)
            };
            Pair {
                high: upper,
                low: lower,
            }
        };
        self.weights[id] = Some(pair.clone());
        pair
    }
    pub(crate) fn shape(&self, member: usize, snap: i32) -> usize {
        self.ids[self.at(member, snap)]
    }
    pub fn score_high(&self, t: Totals) -> f64 {
        t.power * (self.high.total + t.skill) / self.input.converted_count as f64
            * (1.0 + RELATIVE_ERROR)
    }
    pub fn score_low(&self, t: Totals) -> f64 {
        (t.power * (self.low.total + t.skill_low) / self.input.converted_count as f64
            * (1.0 - RELATIVE_ERROR)
            - self.low.absolute_error)
            .max(0.0)
    }
    pub fn evaluate(&mut self, team: &Team, power: i32) -> Result<Arc<Scores>, String> {
        let ids: [usize; 5] =
            std::array::from_fn(|slot| self.ids[self.at(team.members[slot], team.snaps[slot])]);
        let key = (power, ids);
        if let Some(scores) = self.scores.get(&key) {
            return Ok(Arc::clone(scores));
        }
        self.input.power = power;
        self.input.slots = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &m)| self.slot(m, team.snaps[slot]).clone())
            .collect();
        let scores = Arc::new(live::score(&self.input)?);
        if self.scores.len() >= 4096 {
            if let Some(old) = self.score_order.pop_front() {
                self.scores.remove(&old);
            }
        }
        self.score_order.push_back(key);
        self.scores.insert(key, Arc::clone(&scores));
        Ok(scores)
    }
    pub fn order_bounds(&mut self, team: &Team, power: f64) -> (Vec<f64>, Vec<f64>) {
        let mut lows = vec![0.0; 120];
        let mut highs = vec![0.0; 120];
        if let Some(mut bounds) = self.play_bounds.take() {
            let skills: Vec<_> = team
                .members
                .iter()
                .enumerate()
                .map(|(slot, &m)| self.slot(m, team.snaps[slot]).clone())
                .collect();
            let ids: Vec<_> = team
                .members
                .iter()
                .enumerate()
                .map(|(slot, &m)| self.ids[self.at(m, team.snaps[slot])])
                .collect();
            let j = self.input.judgements.as_ref().unwrap();
            for (index, order) in live::orders().iter().enumerate() {
                let (low, high) = bounds.order(&self.input, j, &skills, &ids, order, power);
                lows[index] = low;
                highs[index] = high;
            }
            self.play_bounds = Some(bounds);
        } else {
            let weights: Vec<_> = team
                .members
                .iter()
                .enumerate()
                .map(|(slot, &m)| self.weight(m, team.snaps[slot]))
                .collect();
            for (index, order) in live::orders().iter().enumerate() {
                let (mut low, mut high) = (0.0, 0.0);
                for (event, &slot) in order.iter().enumerate() {
                    low += weights[slot].low.per_event[event];
                    high += weights[slot].high.per_event[event];
                }
                lows[index] = self.score_low(Totals {
                    power,
                    skill_low: low,
                    ..Totals::default()
                });
                highs[index] = self.score_high(Totals {
                    power,
                    skill: high,
                    ..Totals::default()
                });
            }
        }
        (lows, highs)
    }
    pub(crate) fn prepare_dominance(&mut self, power_cap: f64) {
        if self.input.judgements.is_none()
            && self
                .slots
                .iter()
                .all(|s| s.effects.iter().all(|e| e.delta >= 0))
            && self.high.total > 0.0
        {
            let base = self.high.total / self.input.converted_count as f64;
            self.dominance_margin = ((2.0 * RELATIVE_ERROR * power_cap
                + self.low.absolute_error / base)
                / (1.0 - RELATIVE_ERROR))
                .next_up()
                + 1.0;
        }
    }
    pub(crate) fn numeric_member_dominance(
        &mut self,
        better: usize,
        worse: usize,
        power_delta: f64,
    ) -> bool {
        if power_delta <= self.dominance_margin {
            return false;
        }
        if let Some(&answer) = self.member_dominance.get(&(better, worse)) {
            return answer;
        }
        let answer = (-1..self.width as i32 - 1).all(|snap| {
            let a = self.weight(better, snap).low.per_event;
            let b = self.weight(worse, snap).high.per_event;
            a.iter().zip(b).all(|(&a, b)| a >= b)
        });
        self.member_dominance.insert((better, worse), answer);
        answer
    }
    pub(crate) fn numeric_dominance(
        &mut self,
        member: usize,
        better: i32,
        worse: i32,
        power_delta: f64,
    ) -> bool {
        if power_delta <= self.dominance_margin {
            return false;
        }
        let a = self.weight(member, better).low.per_event;
        let b = self.weight(member, worse).high.per_event;
        a.iter().zip(b).all(|(&a, b)| a >= b)
    }
    fn global_profile(&mut self, member: usize) -> [f64; 5] {
        if let Some(&row) = self.member_profiles.get(&member) {
            return row;
        }
        let mut row = [0.0_f64; 5];
        for snap in -1..self.width as i32 - 1 {
            let weights = self.weight(member, snap).high.per_event;
            for event in 0..5 {
                row[event] = row[event].max(weights[event]);
            }
        }
        self.member_profiles.insert(member, row);
        row
    }
    fn prepare_max_members(&mut self, characters: &[Vec<usize>]) {
        self.suffix_profiles = vec![[0.0; 5]; characters.len() + 1];
        for i in (0..characters.len()).rev() {
            let mut row = self.suffix_profiles[i + 1];
            for &member in &characters[i] {
                let profile = self.global_profile(member);
                for event in 0..5 {
                    row[event] = row[event].max(profile[event]);
                }
            }
            self.suffix_profiles[i] = row;
        }
    }
    fn member_max_bound(&mut self, members: &[usize], totals: Totals, from: usize) -> Option<f64> {
        let suffix = *self.suffix_profiles.get(from)?;
        let matrix: [[f64; 5]; 5] = std::array::from_fn(|slot| {
            if slot < members.len() {
                self.global_profile(members[slot])
            } else {
                suffix
            }
        });
        let mut dp = [f64::NEG_INFINITY; 32];
        dp[0] = 0.0;
        for mask in 1usize..32 {
            let slot = mask.count_ones() as usize - 1;
            for event in 0..5 {
                if mask & (1 << event) != 0 {
                    dp[mask] =
                        dp[mask].max((dp[mask ^ (1 << event)] + matrix[slot][event]).next_up());
                }
            }
        }
        Some(self.score_high(Totals {
            skill: dp[31],
            ..totals
        }))
    }
    fn prepare_max(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.max_frontiers.clear();
        for (slot, &member) in members.iter().enumerate() {
            let mut frontiers: [Vec<(f64, f64)>; 5] = std::array::from_fn(|_| Vec::new());
            for &snap in &options[slot] {
                let power = self.snap_power[member * self.power_width
                    + if snap < 0 {
                        self.power_width - 1
                    } else {
                        snap as usize
                    }];
                let weights = self.weight(member, snap).high.per_event;
                for event in 0..5 {
                    frontiers[event].push((power, weights[event]));
                }
            }
            for frontier in &mut frontiers {
                frontier.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.total_cmp(&a.1)));
                let mut high = f64::NEG_INFINITY;
                frontier.retain(|&(_, weight)| {
                    if weight > high {
                        high = weight;
                        true
                    } else {
                        false
                    }
                });
            }
            self.max_frontiers.push(frontiers);
        }
    }
    fn maximum_bound(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        totals: Totals,
    ) -> Option<f64> {
        if self.max_frontiers.len() != 5 {
            return None;
        }
        let lambda = ((self.high.total + totals.skill) / totals.power.max(1.0)).sqrt();
        if !lambda.is_finite() || lambda <= 0.0 {
            return None;
        }
        let mut matrix = [[0.0; 5]; 5];
        for slot in 0..5 {
            if slot < from {
                let member = members[slot];
                let snap = assigned[slot];
                let power = self.snap_power[member * self.power_width
                    + if snap < 0 {
                        self.power_width - 1
                    } else {
                        snap as usize
                    }];
                let weights = self.weight(member, snap).high.per_event;
                for (event, &weight) in weights.iter().enumerate() {
                    matrix[slot][event] = (lambda * power).next_up() + (weight / lambda).next_up();
                }
            } else {
                for (event, value) in matrix[slot].iter_mut().enumerate() {
                    *value = self.max_frontiers[slot][event]
                        .iter()
                        .map(|&(power, weight)| {
                            ((lambda * power).next_up() + (weight / lambda).next_up()).next_up()
                        })
                        .fold(f64::NEG_INFINITY, f64::max);
                }
            }
        }
        let mut dp = [f64::NEG_INFINITY; 32];
        dp[0] = 0.0;
        for mask in 1usize..32 {
            let slot = mask.count_ones() as usize - 1;
            for event in 0..5 {
                if mask & (1 << event) != 0 {
                    dp[mask] =
                        dp[mask].max((dp[mask ^ (1 << event)] + matrix[slot][event]).next_up());
                }
            }
        }
        let sum =
            ((lambda * self.base_power).next_up() + (self.high.total / lambda).next_up() + dp[31])
                .next_up();
        Some(
            ((sum * sum).next_up() / (4.0 * self.input.converted_count as f64)).next_up()
                * (1.0 + RELATIVE_ERROR),
        )
    }
    fn order_high(&mut self, team: &Team, power: f64) -> f64 {
        let rows: Vec<_> = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &m)| self.weight(m, team.snaps[slot]).high.per_event)
            .collect();
        let mut dp = [f64::NEG_INFINITY; 32];
        dp[0] = 0.0;
        for mask in 1usize..32 {
            let slot = mask.count_ones() as usize - 1;
            for event in 0..5 {
                if mask & (1 << event) != 0 {
                    dp[mask] =
                        dp[mask].max((dp[mask ^ (1 << event)] + rows[slot][event]).next_up());
                }
            }
        }
        self.score_high(Totals {
            power,
            skill: dp[31],
            ..Totals::default()
        })
    }
    pub fn team_high(&mut self, team: &Team, t: Totals) -> f64 {
        let Some(j) = &self.input.judgements else {
            return self.score_high(t);
        };
        let recovery = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &m)| self.slot(m, team.snaps[slot]).recovery)
            .max()
            .unwrap_or(0)
            .max(0);
        if recovery >= self.max_recovery {
            return self.score_high(t);
        }
        let entry = self.team_models.entry(recovery).or_insert_with(|| {
            (
                model::linear_play(&self.input, j, true, 0, 1.0, recovery),
                HashMap::new(),
            )
        });
        let mut skill = 0.0;
        for (slot, &member) in team.members.iter().enumerate() {
            let index = member * self.width + (team.snaps[slot] + 1) as usize;
            let id = self.ids[index];
            skill += *entry.1.entry(id).or_insert_with(|| {
                let value = &self.slots[index];
                let mut weights = model::skill_weights(&entry.0, &self.input, value);
                if value.convert.is_some() {
                    let extra = model::conversion_weights(&self.input, j, value, self.factor_cap);
                    for (event, &value) in extra.iter().enumerate() {
                        weights.per_event[event] += value;
                    }
                }
                weights
                    .per_event
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max)
            });
        }
        t.power * (entry.0.total + skill) / self.input.converted_count as f64
            * (1.0 + RELATIVE_ERROR)
    }
}
pub struct LiveObjective {
    pub core: LiveCore,
    pub criterion: Criterion,
    /// Replaces the criterion when set.
    pub target: Option<Target>,
}
impl LiveObjective {
    fn max_bounds(&self) -> bool {
        self.criterion == Criterion::Max && self.target.is_none()
            || self
                .target
                .is_some_and(|t| !t.capped && self.core.score_cap >= t.score)
    }
    fn target_max_bound(&self, high: f64) -> f64 {
        self.target
            .map_or(high, |t| t.ceiling(high.min(self.core.score_cap)))
    }
}
impl Objective for LiveObjective {
    fn score_ties(&self) -> bool {
        self.target.is_none() && self.criterion != Criterion::Mean
    }
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        self.core.shape(member, a) == self.core.shape(member, b)
    }
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn canonical_leader(&self) -> bool {
        true
    }
    fn numeric_member_dominance(&mut self, better: usize, worse: usize, power_delta: f64) -> bool {
        self.core
            .numeric_member_dominance(better, worse, power_delta)
    }
    fn needs_power_cap(&self) -> bool {
        self.core.input.judgements.is_none()
    }
    fn prepare_dominance(&mut self, power_cap: f64) {
        self.core.prepare_dominance(power_cap);
    }
    fn numeric_snap_dominance(
        &mut self,
        member: usize,
        better: i32,
        worse: i32,
        power_delta: f64,
    ) -> bool {
        self.core
            .numeric_dominance(member, better, worse, power_delta)
    }

    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        if self.max_bounds() {
            self.core.prepare_max_members(characters);
        }
    }
    fn member_bound_at(
        &mut self,
        members: &[usize],
        totals: Totals,
        _left: usize,
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        if self.max_bounds() {
            let high = self.core.member_max_bound(members, totals, from)?;
            Some(self.target_max_bound(high))
        } else {
            None
        }
    }
    fn prepare_power(&mut self, power: Arc<Vec<f64>>, width: usize) {
        self.core.snap_power = power;
        self.core.power_width = width;
    }
    fn prepare_assignment_power(&mut self, power: f64) {
        self.core.base_power = power;
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        if self.max_bounds() {
            self.core.prepare_max(members, options);
        }
    }
    fn assignment_order_bound(
        &mut self,
        members: &[usize],
        totals: Totals,
        assigned: &[i32],
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        if self.max_bounds() {
            let high = self.core.maximum_bound(members, assigned, from, totals)?;
            Some(self.target_max_bound(high))
        } else {
            None
        }
    }
    fn snap_dominance(&mut self, member: usize, snap: i32) -> Vec<f64> {
        let shape = self.core.shape(member, snap) as f64;
        vec![shape, -shape]
    }
    fn skill(&mut self, member: usize, snap: i32) -> (f64, f64) {
        let w = self.core.weight(member, snap);
        if self.target.is_some() {
            return (w.low.mean, w.high.mean);
        }
        match self.criterion {
            Criterion::Min => (w.low.worst, w.high.mean),
            Criterion::Max => (w.low.mean, w.high.best),
            Criterion::Mean => (w.low.mean, w.high.mean),
        }
    }
    fn bound(&mut self, t: Totals) -> f64 {
        let high = self.core.score_high(t);
        self.target.map_or(high, |target| {
            if !target.capped && self.core.score_cap < target.score {
                TIE * high.min(self.core.score_cap)
            } else {
                target.mean_ceiling(high.min(self.core.score_cap))
            }
        })
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<crate::search::JointTarget>> {
        let mut score = limit;
        if let Some(target) = self.target {
            score = if !target.capped && self.core.score_cap < target.score {
                (limit.next_down() / TIE).next_down()
            } else {
                target.mean_floor(limit)
            };
        }
        if !score.is_finite() {
            return None;
        }
        Some(vec![crate::search::JointTarget {
            bonus: 0.0,
            product: (score.max(0.0) * self.core.input.converted_count as f64
                / (1.0 + RELATIVE_ERROR))
                .next_down()
                .max(0.0),
        }])
    }
    fn product_base(&self) -> Option<f64> {
        Some(self.core.high.total)
    }
    fn leaf_bound(&mut self, team: &Team, t: Totals) -> Option<f64> {
        if self.core.input.judgements.is_some() {
            let high = self.core.team_high(team, t);
            Some(
                self.target
                    .map_or(high, |target| target.ceiling(high.min(self.core.score_cap))),
            )
        } else {
            None
        }
    }
    fn prefer_snap(&self, member: usize, snap: i32) -> f64 {
        if self.core.drains {
            self.core.slot(member, snap).recovery as f64
        } else {
            0.0
        }
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
        if let Some(target) = self.target {
            if self.core.score_cap < target.score {
                let low = self.core.score_low(t);
                let high = self.core.score_high(t).min(self.core.score_cap);
                let value = |mean: f64| {
                    if target.capped {
                        mean + TIE * mean
                    } else {
                        TIE * mean
                    }
                };
                return (value(low), value(high));
            }

            let (lows, highs) = self.core.order_bounds(team, t.power);
            return (target.value(&lows), target.value(&highs));
        }
        if self.criterion == Criterion::Mean && self.core.input.judgements.is_none() {
            return (self.core.score_low(t), self.core.score_high(t));
        }
        let (lows, highs) = self.core.order_bounds(team, t.power);
        match self.criterion {
            Criterion::Mean => (
                lows.iter().fold(0.0, |sum, v| sum + v) / 120.0,
                highs.iter().fold(0.0, |sum, v| sum + v) / 120.0,
            ),
            Criterion::Min => (
                lows.into_iter().fold(f64::INFINITY, f64::min),
                highs.into_iter().fold(f64::INFINITY, f64::min),
            ),
            Criterion::Max => (
                lows.into_iter().fold(f64::NEG_INFINITY, f64::max),
                highs.into_iter().fold(f64::NEG_INFINITY, f64::max),
            ),
        }
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        let scores = self.core.evaluate(team, power as i32)?;
        Ok(Exact {
            key: self.target.map_or_else(
                || self.criterion.key(&scores),
                |target| target.value(&scores.scores),
            ),
            detail: json!({"scores":&*scores}),
        })
    }
}
pub struct Potential {
    areas: Vec<f64>,
    width: usize,
    window: f64,
}
impl Potential {
    pub fn new(master: &Master, members: &[Member], snaps: &[Snap], window: f64) -> Self {
        let width = snaps.len() + 1;
        let mut areas = Vec::with_capacity(members.len() * width);
        let table = skills::prepare(master, members, snaps);
        for skill in &table.slots {
            let mut area = 0.0;
            for effect in &skill.effects {
                if (effect.kind == 2000 || effect.judgements.contains(&5))
                    && !effect.gate.blocks(master.live.life_base)
                {
                    area += effect.delta as f64 / 100000.0
                        * (window_ms(effect.seconds, skill.extension_ms) as f64 / 1000.0);
                }
            }
            areas.push(area);
        }
        Self {
            areas,
            width,
            window,
        }
    }
    fn area(&self, member: usize, snap: i32) -> f64 {
        self.areas[member * self.width + (snap + 1) as usize]
    }
    fn key(&self, power: f64, area: f64) -> f64 {
        power * (1.0 + area / self.window)
    }
}
impl Objective for Potential {
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn canonical_leader(&self) -> bool {
        true
    }
    fn skill(&mut self, member: usize, snap: i32) -> (f64, f64) {
        let area = self.area(member, snap);
        (area, area)
    }
    fn bound(&mut self, t: Totals) -> f64 {
        self.key(t.power, t.skill) * (1.0 + 1e-12)
    }
    fn interval(&mut self, _team: &Team, t: Totals) -> (f64, f64) {
        (self.key(t.power, t.skill_low), self.key(t.power, t.skill))
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        let area = team
            .members
            .iter()
            .enumerate()
            .fold(0.0, |sum, (slot, &m)| sum + self.area(m, team.snaps[slot]));
        let value = self.key(power, area);
        Ok(Exact {
            key: value,
            detail: json!({"potential":value,"area":area}),
        })
    }
}

pub struct ScoreCap {
    pub core: LiveCore,
}
impl Objective for ScoreCap {
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        self.core.shape(member, a) == self.core.shape(member, b)
    }
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn canonical_leader(&self) -> bool {
        true
    }
    fn needs_power_cap(&self) -> bool {
        self.core.input.judgements.is_none()
    }
    fn prepare_dominance(&mut self, power_cap: f64) {
        self.core.prepare_dominance(power_cap);
    }
    fn numeric_snap_dominance(&mut self, m: usize, a: i32, b: i32, delta: f64) -> bool {
        self.core.numeric_dominance(m, a, b, delta)
    }
    fn numeric_member_dominance(&mut self, a: usize, b: usize, delta: f64) -> bool {
        self.core.numeric_member_dominance(a, b, delta)
    }
    fn snap_dominance(&mut self, m: usize, s: i32) -> Vec<f64> {
        let shape = self.core.shape(m, s) as f64;
        vec![shape, -shape]
    }
    fn prepare_power(&mut self, power: Arc<Vec<f64>>, width: usize) {
        self.core.snap_power = power;
        self.core.power_width = width;
    }
    fn prepare_assignment_power(&mut self, power: f64) {
        self.core.base_power = power;
    }
    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        self.core.prepare_max_members(characters);
    }
    fn member_bound_at(
        &mut self,
        members: &[usize],
        t: Totals,
        _left: usize,
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        self.core.member_max_bound(members, t, from)
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.core.prepare_max(members, options);
    }
    fn assignment_order_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        self.core.maximum_bound(members, assigned, from, t)
    }
    fn skill(&mut self, m: usize, s: i32) -> (f64, f64) {
        let best = self.core.weight(m, s).high.best;
        (best, best)
    }
    fn bound(&mut self, t: Totals) -> f64 {
        self.core.score_high(t)
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<crate::search::JointTarget>> {
        Some(vec![crate::search::JointTarget {
            bonus: 0.0,
            product: (limit.max(0.0) * self.core.input.converted_count as f64
                / (1.0 + RELATIVE_ERROR))
                .next_down()
                .max(0.0),
        }])
    }
    fn product_base(&self) -> Option<f64> {
        Some(self.core.high.total)
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
        let value = self.core.order_high(team, t.power);
        (value, value)
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        Ok(Exact {
            key: self.core.order_high(team, power),
            detail: Value::Null,
        })
    }
}

/// A score target over the 120 member orders: the share of orders that reach `score`, or the mean score capped at
/// `score`. Exact ties keep the higher mean score, at a weight below one order's share.
#[derive(Clone, Copy)]
pub struct Target {
    pub score: f64,
    pub capped: bool,
    peak: f64,
}
const TIE: f64 = 1e-12;
impl Target {
    pub fn from_value(v: &Value) -> Option<Self> {
        let score = v["score"].as_f64().filter(|s| s.is_finite() && *s > 0.0)?;
        match v["kind"].as_str() {
            Some("at-least") => Some(Self {
                score,
                capped: false,
                peak: 1.0,
            }),
            Some("capped") => Some(Self {
                score,
                capped: true,
                peak: (0..120).fold(0.0, |sum, _| sum + score) / 120.0,
            }),
            _ => None,
        }
    }
    fn value(self, scores: &[f64]) -> f64 {
        let n = scores.len().max(1) as f64;
        let mean = scores.iter().fold(0.0, |sum, v| sum + v) / n;
        let main = if self.capped {
            scores.iter().fold(0.0, |sum, &v| sum + v.min(self.score)) / n
        } else {
            scores.iter().filter(|&&v| v >= self.score).count() as f64 / n
        };
        main + TIE * mean
    }
    fn mean_ceiling(self, high: f64) -> f64 {
        if high == 0.0 {
            return 0.0;
        }
        let main = if self.capped {
            high.min(self.peak)
        } else {
            (high / self.score).next_up().min(1.0)
        };
        (main + (TIE * high).next_up()).next_up()
    }
    fn mean_floor(self, limit: f64) -> f64 {
        let lower = limit.next_down();
        let slope = if self.capped {
            (1.0 + TIE).next_up()
        } else {
            ((1.0 / self.score).next_up() + TIE).next_up()
        };
        let mut floor = (lower / slope).next_down();
        if lower > self.peak {
            floor = floor.max(((lower - self.peak).next_down() / TIE).next_down());
        }
        floor.max(0.0)
    }
    /// Upper bound from an upper bound on every order's score.
    fn ceiling(self, high: f64) -> f64 {
        let main = if self.capped {
            return self.mean_ceiling(high);
        } else if high >= self.score {
            1.0
        } else {
            0.0
        };
        main + TIE * high
    }
}
