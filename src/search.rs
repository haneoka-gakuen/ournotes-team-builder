//! Exact top-K search over leaders, character-distinct member sets and snap
//! assignments. Bounds prune only when the retained legal teams justify it.
use crate::{
    assignment::Scratch,
    master::Master,
    power::{self, LeaderProfile, Member, Music, Player, SlotBase, Snap},
};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
#[derive(Clone, Copy, Default)]
pub struct Totals {
    pub power: f64,
    pub skill: f64,
    pub skill_low: f64,
    pub bonus: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Team {
    pub members: Vec<usize>,
    pub snaps: Vec<i32>,
}
pub struct Exact {
    pub key: f64,
    pub detail: Value,
}
pub enum Evaluation {
    Exact(Exact),
    Pruned(f64),
}
#[derive(Clone, Copy)]
pub struct Cutoff {
    pub key: f64,
    pub prune_equal: bool,
}
#[derive(Clone, Copy)]
pub struct JointTarget {
    pub bonus: f64,
    pub product: f64,
}
pub trait Objective {
    fn score_ties(&self) -> bool {
        false
    }
    fn complete_score_tie(&mut self, _hit: &mut Hit) -> Result<(), String> {
        Ok(())
    }
    fn tie_rank(&self, hit: &Hit) -> [f64; 2] {
        [
            hit.detail["scores"]["mean"]
                .as_f64()
                .unwrap_or(f64::NEG_INFINITY),
            0.0,
        ]
    }
    /// Only evaluated, retained legal hits may tighten objective-local tie bounds.
    fn observe_hit(&mut self, _hit: &Hit) {}
    fn seed_objective(&self) -> Option<Box<dyn Objective>> {
        None
    }
    fn seed_ceiling(&self) -> f64 {
        f64::INFINITY
    }
    fn seed_rank(&self, key: f64, _detail: &Value) -> [f64; 3] {
        [key, 0.0, 0.0]
    }
    fn seed_skill(&mut self, member: usize, snap: i32) -> f64 {
        self.skill(member, snap).1
    }
    fn seed_direct_bound(&self, _totals: Totals) -> Option<f64> {
        None
    }
    fn seed_ready(&self, _detail: &Value) -> bool {
        false
    }
    fn conditional_snap_dominance(&self) -> bool {
        false
    }
    fn exchange_snap_equivalent(&mut self, _member: usize, _a: i32, _b: i32) -> bool {
        false
    }
    fn canonical_leader(&self) -> bool {
        false
    }
    fn live_floor(&self) -> bool {
        false
    }
    fn needs_power_cap(&self) -> bool {
        false
    }
    fn prepare_dominance(&mut self, _power_cap: f64) {}
    fn numeric_member_dominance(
        &mut self,
        _better: usize,
        _worse: usize,
        _power_delta: f64,
    ) -> bool {
        false
    }
    fn numeric_snap_dominance(
        &mut self,
        _member: usize,
        _better: i32,
        _worse: i32,
        _power_delta: f64,
    ) -> bool {
        false
    }
    fn linear_assignment(&self) -> Option<(f64, f64, f64)> {
        None
    }
    fn skill(&mut self, member: usize, snap: i32) -> (f64, f64);
    fn member_bonus(&self, _member: usize) -> f64 {
        0.0
    }
    fn snap_bonus(&self, _snap: usize) -> f64 {
        0.0
    }
    fn snap_dominance(&mut self, _member: usize, _snap: i32) -> Vec<f64> {
        Vec::new()
    }
    fn preserve_snap_options(&self) -> bool {
        false
    }
    fn bound(&mut self, totals: Totals) -> f64;
    fn cheap_assignment_bound(
        &mut self,
        _members: &[usize],
        totals: Totals,
        _assigned: &[i32],
        _from: usize,
    ) -> f64 {
        self.bound(totals)
    }
    fn member_bound(&mut self, _members: &[usize], _totals: Totals, _left: usize) -> Option<f64> {
        None
    }
    fn prepare_members(&mut self, _characters: &[Vec<usize>]) {}
    fn prepare_member_domains(&mut self, _power: &[f64], _options: &[Vec<i32>]) {}
    fn prepare_member_domains_at(&mut self, _leader: usize, power: &[f64], options: &[Vec<i32>]) {
        self.prepare_member_domains(power, options);
    }
    /// A roster-only upper bound available before the generic joint cuts.
    fn early_member_bound(&mut self, _members: &[usize], _from: usize, _limit: f64) -> Option<f64> {
        None
    }
    fn member_bound_at(
        &mut self,
        members: &[usize],
        totals: Totals,
        left: usize,
        _from: usize,
        _limit: f64,
    ) -> Option<f64> {
        self.member_bound(members, totals, left)
    }
    fn prepare_assignment(&mut self, _members: &[usize], _options: &[Vec<i32>]) {}
    /// Each slot's domains must be disjoint and cover its supplied options.
    fn assignment_domains(
        &mut self,
        _members: &[usize],
        _options: &[Vec<i32>],
    ) -> Option<Vec<Vec<Vec<i32>>>> {
        None
    }
    fn prepare_assignment_domain(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
        _complete: bool,
    ) {
        self.prepare_assignment(members, options);
    }
    fn roster_bound(&mut self, _members: &[usize], _totals: Totals, _limit: f64) -> Option<f64> {
        None
    }
    fn prepare_power(&mut self, _snapshots: Arc<Vec<f64>>, _width: usize) {}
    fn prepare_assignment_power(&mut self, _power: f64) {}
    /// A reused rejection must cover this roster's full snapshot domains at `power`.
    fn reused_assignment_bound(
        &mut self,
        _members: &[usize],
        _options: &[Vec<i32>],
        _power: f64,
        _limit: f64,
    ) -> Option<f64> {
        None
    }
    fn assignment_options(
        &self,
        _members: &[usize],
        _assigned: &[i32],
        _from: usize,
        _limit: f64,
    ) -> Option<Vec<i32>> {
        None
    }
    fn assignment_bound(
        &mut self,
        _members: &[usize],
        _totals: Totals,
        _assigned: &[i32],
        _from: usize,
    ) -> Option<f64> {
        None
    }
    fn assignment_order_bound(
        &mut self,
        _members: &[usize],
        _totals: Totals,
        _assigned: &[i32],
        _from: usize,
        _limit: f64,
    ) -> Option<f64> {
        None
    }
    fn generic_assignment_product(&self) -> bool {
        true
    }
    fn member_snapshot_prices(&self) -> bool {
        false
    }
    fn product_base(&self) -> Option<f64> {
        None
    }
    fn joint_targets(&mut self, _limit: f64) -> Option<Vec<JointTarget>> {
        None
    }
    /// A team bonus below this cannot reach `limit` (or the objective's own fixed target) at any score rank.
    fn bonus_floor(&mut self, _limit: f64) -> Option<f64> {
        None
    }
    fn prefer_snap(&self, _member: usize, _snap: i32) -> f64 {
        0.0
    }
    fn prefer_member(&self, _member: usize) -> f64 {
        0.0
    }
    fn leaf_bound(&mut self, _team: &Team, _totals: Totals) -> Option<f64> {
        None
    }
    fn interval(&mut self, team: &Team, totals: Totals) -> (f64, f64);
    fn exact(&mut self, team: &Team, power: f64, slot_powers: &[f64]) -> Result<Exact, String>;
    fn evaluate(
        &mut self,
        team: &Team,
        power: f64,
        slot_powers: &[f64],
        _cutoff: Cutoff,
    ) -> Result<Evaluation, String> {
        self.exact(team, power, slot_powers).map(Evaluation::Exact)
    }
}
#[derive(Clone)]
pub struct MemberFloor {
    pub values: Vec<f64>,
    pub minimum: f64,
    pub maximum: f64,
}
#[derive(Default, Clone)]
pub struct Constraints {
    pub required_members: Vec<usize>,
    pub excluded_members: Vec<usize>,
    pub required_snaps: Vec<usize>,
    pub excluded_snaps: Vec<usize>,
    pub leader: Option<i32>,
    pub bindings: std::collections::HashMap<usize, i32>,
    pub no_snaps: bool,
    pub min_bonus: Option<f64>,
    pub max_bonus: Option<f64>,
    pub member_floor: Option<MemberFloor>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub team: Team,
    pub power: f64,
    pub slot_powers: Vec<f64>,
    pub key: f64,
    pub detail: Value,
    #[serde(skip)]
    pub totals: Totals,
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub leaders: usize,
    pub member_nodes: usize,
    pub snap_nodes: usize,
    pub candidates: usize,
    pub exact: usize,
    pub elapsed_ms: f64,
}
impl Stats {
    pub fn add(&mut self, other: &Self) {
        self.leaders += other.leaders;
        self.member_nodes += other.member_nodes;
        self.snap_nodes += other.snap_nodes;
        self.candidates += other.candidates;
        self.exact += other.exact;
        self.elapsed_ms += other.elapsed_ms;
    }
}
#[derive(Serialize)]
pub struct Output {
    pub hits: Vec<Hit>,
    pub proven: bool,
    pub bound: Option<f64>,
    pub stats: Stats,
}
pub type Ownership<'a> = &'a dyn Fn(&[usize]) -> bool;
pub type CandidateListener<'a> = &'a mut dyn FnMut(&[Hit]);
pub struct Input<'a, O: Objective + ?Sized> {
    pub master: &'a Master,
    pub player: &'a Player,
    pub members: &'a [Member],
    pub snaps: &'a [Snap],
    pub music: Option<&'a Music>,
    pub objective: &'a mut O,
    pub constraints: Constraints,
    pub k: usize,
    pub deadline: f64,
    pub now: &'a dyn Fn() -> f64,
    pub seeds: Vec<Team>,
    pub shard: Option<(usize, usize)>,
    pub owns: Option<Ownership<'a>>,
    pub floor: f64,
    pub floor_exclusive: bool,
    pub streaming: bool,
    pub disable_snap_dominance: bool,
    pub member_power_percent: Option<&'a dyn Fn(&Member) -> i32>,
    pub snap_power_percent: Option<&'a dyn Fn(&Snap) -> i32>,
    pub on_candidate: Option<CandidateListener<'a>>,
    pub progress: Option<&'a mut dyn FnMut(usize, usize)>,
}
struct Frontier {
    k: usize,
    best: HashMap<String, f64>,
    sorted: Vec<f64>,
}
impl Frontier {
    fn threshold(&self) -> f64 {
        if self.sorted.len() >= self.k {
            self.sorted[self.k - 1]
        } else {
            f64::NEG_INFINITY
        }
    }
    fn offer(&mut self, key: &str, value: f64) {
        let previous = self.best.get(key).copied();
        if previous.is_some_and(|p| p >= value)
            || (previous.is_none() && self.sorted.len() >= self.k && value <= self.threshold())
        {
            return;
        }
        self.best.insert(key.to_owned(), value);
        if let Some(previous) = previous {
            if let Some(i) = self.sorted.iter().position(|&v| v == previous) {
                self.sorted.remove(i);
            }
        }
        let mut i = self.sorted.len();
        while i > 0 && self.sorted[i - 1] < value {
            i -= 1;
        }
        self.sorted.insert(i, value);
        if self.sorted.len() > self.k {
            let removed = self.sorted.pop().unwrap();
            if let Some(key) = self
                .best
                .iter()
                .find(|(_, v)| **v == removed)
                .map(|(k, _)| k.clone())
            {
                self.best.remove(&key);
            }
        }
    }
}
#[derive(Clone)]
struct Candidate {
    set_key: String,
    team: Team,
    power: f64,
    slot_powers: Vec<f64>,
    totals: Totals,
    low: f64,
    high: f64,
}
#[derive(Clone, Copy, Default)]
struct Best {
    power: f64,
    weight: f64,
    bonus: f64,
}
#[derive(Default)]
struct Tables {
    count: usize,
    columns: usize,
    column_snap: Vec<usize>,
    power: Vec<Vec<f64>>,
    weight: Vec<Vec<f64>>,
    bonus: Vec<Vec<f64>>,
    blended: Vec<Vec<f64>>,
    joint: Vec<Vec<f64>>,
}
impl Tables {
    fn new(from: usize, width: usize) -> Self {
        let rows = || vec![vec![0.0; width]; 5 - from];
        Self {
            count: 0,
            columns: 0,
            column_snap: vec![0; width],
            power: rows(),
            weight: rows(),
            bonus: rows(),
            blended: rows(),
            joint: rows(),
        }
    }
}
struct Assignment {
    team: Vec<usize>,
    set_key: String,
    base_power: Vec<f64>,
    options: Vec<Vec<i32>>,
    allowed: Vec<Vec<bool>>,
    required_snaps: Vec<usize>,
    used: Vec<bool>,
    assigned: Vec<i32>,
    max_power: Vec<f64>,
    max_weight: Vec<f64>,
    power_order: Vec<Vec<i32>>,
    skill_order: Vec<Vec<i32>>,
    max_bonus: Vec<f64>,
    set_low: f64,
    exact_low: f64,
}
struct ColumnCut {
    constant: f64,
    members: Vec<f64>,
    suffix: Vec<Vec<f64>>,
}
struct MemberCut {
    mu: f64,
    column: Option<ColumnCut>,
    constant: f64,
    members: Vec<f64>,
    suffix: Vec<Vec<f64>>,
}
struct BonusSuffix {
    width: usize,
    values: Vec<f64>,
}
impl BonusSuffix {
    fn get(&self, from: usize, left: usize, bonus: usize) -> f64 {
        self.values[(from * 5 + left) * self.width + bonus]
    }
    fn new(characters: &[Vec<usize>], bonuses: &[f64], weights: &[f64], target: usize) -> Self {
        let width = target + 1;
        let stride = 5 * width;
        let mut values = vec![f64::NEG_INFINITY; (characters.len() + 1) * stride];
        values[characters.len() * stride] = 0.0;
        for from in (0..characters.len()).rev() {
            let (at, next) = (from * stride, (from + 1) * stride);
            values.copy_within(next..next + stride, at);
            for &member in &characters[from] {
                let bonus = bonuses[member] as usize;
                for left in 1..=4 {
                    for total in bonus..=target {
                        let remaining = values[next + (left - 1) * width + total - bonus];
                        if remaining == f64::NEG_INFINITY {
                            continue;
                        }
                        let value = (weights[member] + remaining).next_up();
                        let cell = &mut values[at + left * width + total];
                        *cell = cell.max(value);
                    }
                }
            }
        }
        Self { width, values }
    }
}
struct BonusBounds {
    target: usize,
    power: BonusSuffix,
    skill: BonusSuffix,
    cuts: Vec<BonusSuffix>,
    columns: Vec<Option<BonusSuffix>>,
}
struct Members {
    leader: usize,
    characters: Vec<Vec<usize>>,
    forced: usize,
    slot_power: Vec<f64>,
    char_power: Vec<Vec<f64>>,
    char_skill: Vec<Vec<f64>>,
    char_bonus: Vec<Vec<f64>>,
    char_member_bonus: Vec<Vec<f64>>,
    chosen: Vec<usize>,
    cuts: Vec<MemberCut>,
    cut_sums: [[f64; 12]; 5],
    column_sums: [[f64; 12]; 5],
    bonus_bounds: Option<BonusBounds>,
}
impl Members {
    fn choose(&mut self, member: usize) {
        let depth = self.chosen.len();
        for (index, cut) in self.cuts.iter().enumerate() {
            self.cut_sums[depth + 1][index] =
                (self.cut_sums[depth][index] + cut.members[member]).next_up();
            if let Some(column) = &cut.column {
                self.column_sums[depth + 1][index] =
                    (self.column_sums[depth][index] + column.members[member]).next_up();
            }
        }
        self.chosen.push(member);
    }
}
const NONE: f64 = -1e15;
struct Runner<'a, O: Objective + ?Sized> {
    input: Input<'a, O>,
    started: f64,
    last_notify: f64,
    generation: u64,
    width: usize,
    bases: Vec<SlotBase>,
    snap_power: Arc<Vec<f64>>,
    skill: Vec<f64>,
    skill_low: Vec<f64>,
    member_bonus: Vec<f64>,
    snap_bonus: Vec<f64>,
    any_snap_bonus: bool,
    bonus_order: Vec<usize>,
    snap_bonus_top5: f64,
    required: HashSet<usize>,
    required_snaps: HashSet<usize>,
    excluded_members: HashSet<usize>,
    best: Vec<Best>,
    options: Vec<Vec<i32>>,
    conditional_dominance: bool,
    dominators: Vec<Vec<Vec<usize>>>,
    exchange_equivalent: HashMap<(usize, usize, usize), bool>,
    /// The bonus floor the snap options were last restricted to.
    snap_floor: f64,
    profiles: HashMap<usize, Arc<LeaderProfile>>,
    frontier: Frontier,
    candidates: Vec<Candidate>,
    hits: Vec<Hit>,
    hit_keys: Vec<String>,
    option_scratch: [Vec<i32>; 5],
    retained: HashMap<String, f64>,
    compact_at: usize,
    timed_out: bool,
    unexplored: f64,
    stats: Stats,
    pools: Vec<Tables>,
    matching: Scratch,
    column_of: Vec<usize>,
    column_stamp: Vec<u64>,
    stamp: u64,
    target_limit: f64,
    member_targets: Option<Vec<(f64, f64)>>,
}
pub(crate) fn valid_seed(members: &[Member], snaps: &[Snap], c: &Constraints, seed: &Team) -> bool {
    if seed.members.len() != 5
        || seed.snaps.len() != 5
        || seed.members.iter().any(|&member| member >= members.len())
        || seed
            .snaps
            .iter()
            .any(|&snap| snap < -1 || snap >= snaps.len() as i32)
    {
        return false;
    }
    if seed.members.iter().copied().collect::<HashSet<_>>().len() != 5
        || seed
            .members
            .iter()
            .any(|member| c.excluded_members.contains(member))
        || c.leader
            .is_some_and(|leader| seed.members[0] as i32 != leader)
        || seed
            .members
            .iter()
            .map(|&member| members[member].card.character_id)
            .collect::<HashSet<_>>()
            .len()
            != 5
        || c.required_members
            .iter()
            .chain(c.bindings.keys())
            .any(|member| !seed.members.contains(member))
        || c.member_floor.as_ref().is_some_and(|floor| {
            seed.members
                .iter()
                .map(|&member| floor.values[member])
                .sum::<f64>()
                < floor.minimum
        })
    {
        return false;
    }
    let used: Vec<_> = seed
        .snaps
        .iter()
        .filter(|&&snap| snap >= 0)
        .map(|&snap| snap as usize)
        .collect();
    if used.iter().copied().collect::<HashSet<_>>().len() != used.len()
        || c.no_snaps && !used.is_empty()
        || used.iter().any(|snap| c.excluded_snaps.contains(snap))
        || c.required_snaps.iter().any(|snap| !used.contains(snap))
    {
        return false;
    }
    seed.members.iter().enumerate().all(|(slot, member)| {
        c.bindings.get(member).map_or_else(
            || seed.snaps[slot] < 0 || !c.bindings.values().any(|&snap| snap == seed.snaps[slot]),
            |&snap| snap == seed.snaps[slot],
        )
    })
}

pub(crate) fn suffix_tops(values: &[f64]) -> Vec<Vec<f64>> {
    let mut tops = vec![vec![0.0; 5]; values.len() + 1];
    let mut best = vec![f64::NEG_INFINITY; 4];
    for i in (0..values.len()).rev() {
        best.push(values[i]);
        best.sort_by(|a, b| b.total_cmp(a));
        best.truncate(4);
        for c in 1..=4 {
            tops[i][c] = tops[i][c - 1]
                + if best[c - 1] == f64::NEG_INFINITY {
                    0.0
                } else {
                    best[c - 1]
                };
        }
    }
    tops
}
fn top_sum(table: &[Vec<f64>], from: usize, count: usize) -> f64 {
    table[from.min(table.len() - 1)][count.min(4)]
}
fn suffix(values: &[f64], from: usize) -> f64 {
    values[from..].iter().rev().fold(0.0, |sum, v| sum + v)
}
impl<'a, O: Objective + ?Sized> Runner<'a, O> {
    fn new(mut input: Input<'a, O>) -> Self {
        let started = (input.now)();
        let generation = crate::cancellation_generation();
        input.k = input.k.max(1);
        let n = input.members.len();
        let s = input.snaps.len();
        let width = s + 1;
        let excluded_members = input.constraints.excluded_members.iter().copied().collect();
        let excluded_snaps: HashSet<_> = input.constraints.excluded_snaps.iter().copied().collect();
        let bases: Vec<_> = input
            .members
            .iter()
            .map(|m| {
                power::slot_base(
                    input.master,
                    input.player,
                    m,
                    input.music,
                    input.member_power_percent.map(|f| f(m)).unwrap_or(0),
                )
            })
            .collect();
        let snap_event: Vec<_> = input
            .snaps
            .iter()
            .map(|s| input.snap_power_percent.map(|f| f(s)).unwrap_or(0))
            .collect();
        let allowed: Vec<_> = (0..s)
            .filter(|j| !excluded_snaps.contains(j) && !input.constraints.no_snaps)
            .collect();
        let bound_elsewhere: HashSet<_> = input
            .constraints
            .bindings
            .values()
            .filter(|&&j| j >= 0)
            .map(|&j| j as usize)
            .collect();
        let raw_options: Vec<Vec<i32>> = (0..n)
            .map(|i| {
                input.constraints.bindings.get(&i).map_or_else(
                    || {
                        allowed
                            .iter()
                            .filter(|j| !bound_elsewhere.contains(j))
                            .map(|&j| j as i32)
                            .chain(std::iter::once(-1))
                            .collect()
                    },
                    |&bound| {
                        if bound < 0 || allowed.contains(&(bound as usize)) {
                            vec![bound]
                        } else {
                            Vec::new()
                        }
                    },
                )
            })
            .collect();
        let (mut snap_power, mut skill, mut skill_low) = (
            vec![0.0; n * width],
            vec![0.0; n * width],
            vec![0.0; n * width],
        );
        for i in 0..n {
            for &j in &raw_options[i] {
                let col = if j < 0 { s } else { j as usize };
                if j >= 0 {
                    snap_power[i * width + col] = power::snap_points(
                        input.master,
                        &bases[i],
                        &input.members[i],
                        Some(&input.snaps[col]),
                        snap_event[col],
                    ) as f64;
                }
                let (low, high) = input.objective.skill(i, j);
                skill_low[i * width + col] = low;
                skill[i * width + col] = high;
            }
        }
        let member_bonus: Vec<_> = (0..n).map(|i| input.objective.member_bonus(i)).collect();
        let snap_bonus: Vec<_> = (0..s).map(|i| input.objective.snap_bonus(i)).collect();
        let any_snap_bonus = snap_bonus.iter().any(|&v| v != 0.0);
        let mut bonus_order: Vec<_> = allowed
            .iter()
            .copied()
            .filter(|&j| snap_bonus[j] > 0.0)
            .collect();
        bonus_order.sort_by(|&a, &b| snap_bonus[b].total_cmp(&snap_bonus[a]));
        let snap_bonus_top5 = bonus_order
            .iter()
            .take(5)
            .fold(0.0, |sum, &j| sum + snap_bonus[j]);
        let mut required: HashSet<_> = input.constraints.required_members.iter().copied().collect();
        required.extend(input.constraints.bindings.keys().copied());
        let mut required_snaps: HashSet<_> =
            input.constraints.required_snaps.iter().copied().collect();
        for &snap in input.constraints.bindings.values() {
            if snap >= 0 {
                required_snaps.remove(&(snap as usize));
            }
        }
        let best: Vec<_> = (0..n)
            .map(|i| {
                let mut best = Best::default();
                for &j in &raw_options[i] {
                    let col = if j < 0 { s } else { j as usize };
                    if col > s {
                        continue;
                    }
                    best.power = best.power.max(snap_power[i * width + col]);
                    best.weight = best.weight.max(skill[i * width + col]);
                    best.bonus = best
                        .bonus
                        .max(if j < 0 { 0.0 } else { snap_bonus[j as usize] });
                }
                best
            })
            .collect();
        let mut profiles = HashMap::default();
        if input.objective.needs_power_cap() {
            let mut maximum = 0.0_f64;
            for leader in 0..n {
                let profile = Arc::new(power::leader_profile(input.master, &input.members[leader]));
                for member in 0..n {
                    maximum = maximum.max(
                        bases[member].fixed_points as f64
                            + power::leader_points(
                                &bases[member],
                                &power::leader_percent_bound(
                                    input.master,
                                    &profile,
                                    &input.members[member],
                                ),
                            ) as f64
                            + best[member].power,
                    );
                }
                profiles.insert(leader, profile);
            }
            input.objective.prepare_dominance(5.0 * maximum);
        }
        let conditional_dominance = input.objective.conditional_snap_dominance()
            && !input.disable_snap_dominance
            && !input.objective.preserve_snap_options();
        let mut dominators = vec![vec![Vec::new(); width]; n];
        let options = (0..n)
            .map(|i| {
                if let Some(&bound) = input.constraints.bindings.get(&i) {
                    return if bound < 0 || allowed.contains(&(bound as usize)) {
                        vec![bound]
                    } else {
                        Vec::new()
                    };
                }
                let list = &raw_options[i];
                let values: Vec<Vec<f64>> = list
                    .iter()
                    .map(|&j| {
                        let col = if j < 0 { s } else { j as usize };
                        let mut value = vec![
                            snap_power[i * width + col],
                            skill[i * width + col],
                            skill_low[i * width + col],
                            if j < 0 { 0.0 } else { snap_bonus[j as usize] },
                        ];
                        value.extend(input.objective.snap_dominance(i, j));
                        value
                    })
                    .collect();
                let mut kept: Vec<_> = list
                    .iter()
                    .enumerate()
                    .filter(|&(a, &j)| {
                        if input.disable_snap_dominance
                            || input.objective.preserve_snap_options()
                            || (j >= 0 && required_snaps.contains(&(j as usize)))
                        {
                            return true;
                        }
                        let mut dominated = 0;
                        for (b, &other) in list.iter().enumerate() {
                            if b == a || other < 0 {
                                continue;
                            }
                            let (x, y) = (&values[a], &values[b]);
                            if (input.constraints.max_bonus.is_none() || y[3] == x[3])
                                && y[..4].iter().zip(&x[..4]).all(|(y, x)| y >= x)
                                && (y[4..].iter().zip(&x[4..]).all(|(y, x)| y >= x)
                                    || input.objective.numeric_snap_dominance(
                                        i,
                                        other,
                                        j,
                                        y[0] - x[0],
                                    ))
                            {
                                let equal = y == x;
                                if !equal || b < a {
                                    if conditional_dominance && j >= 0 {
                                        dominators[i][j as usize].push(other as usize);
                                    }
                                    dominated += 1;
                                    if dominated >= 5 {
                                        break;
                                    }
                                }
                            }
                        }
                        dominated < 5
                    })
                    .map(|(_, &j)| j)
                    .collect();
                if conditional_dominance {
                    for &snap in &kept {
                        if snap >= 0 {
                            dominators[i][snap as usize]
                                .retain(|&other| kept.contains(&(other as i32)));
                        }
                    }
                }
                kept.sort_by(|&a, &b| {
                    let preference =
                        input.objective.prefer_snap(i, b) - input.objective.prefer_snap(i, a);
                    if preference != 0.0 {
                        preference.total_cmp(&0.0)
                    } else {
                        snap_power[i * width + if b < 0 { s } else { b as usize }]
                            .total_cmp(&snap_power[i * width + if a < 0 { s } else { a as usize }])
                    }
                });
                kept
            })
            .collect();
        let k = input.k;
        let snap_power = Arc::new(snap_power);
        input
            .objective
            .prepare_power(Arc::clone(&snap_power), width);
        Self {
            input,
            started,
            last_notify: started,
            generation,
            width,
            bases,
            snap_power,
            skill,
            skill_low,
            member_bonus,
            snap_bonus,
            any_snap_bonus,
            bonus_order,
            snap_bonus_top5,
            required,
            required_snaps,
            excluded_members,
            best,
            options,
            conditional_dominance,
            dominators,
            exchange_equivalent: HashMap::default(),
            snap_floor: f64::NEG_INFINITY,
            profiles,
            frontier: Frontier {
                k,
                best: HashMap::default(),
                sorted: Vec::new(),
            },
            candidates: Vec::new(),
            hits: Vec::new(),
            hit_keys: Vec::new(),
            option_scratch: std::array::from_fn(|_| Vec::new()),
            retained: HashMap::default(),
            compact_at: 4096,
            timed_out: false,
            unexplored: f64::NEG_INFINITY,
            stats: Stats::default(),
            pools: (0..5).map(|from| Tables::new(from, s + 5)).collect(),
            matching: Scratch::default(),
            column_of: vec![0; s],
            column_stamp: vec![0; s],
            stamp: 0,
            target_limit: f64::NAN,
            member_targets: None,
        }
    }
    fn expired(&self) -> bool {
        self.generation != crate::cancellation_generation()
            || self.input.deadline != f64::INFINITY && (self.input.now)() > self.input.deadline
    }
    fn report_pending(&mut self) {
        if self.input.on_candidate.is_none() || self.hits.is_empty() {
            return;
        }
        let now = (self.input.now)();
        if now - self.last_notify < 250.0 {
            return;
        }
        self.last_notify = now;
        if let Some(callback) = self.input.on_candidate.as_deref_mut() {
            callback(&self.hits);
        }
    }
    fn limit(&self) -> f64 {
        let value = self.frontier.threshold().max(self.input.floor);
        if self.input.objective.live_floor() {
            if self.hits.len() >= self.input.k {
                crate::publish_search_hit(&self.hits[self.input.k - 1]);
            }
            value.max(crate::search_floor())
        } else {
            value
        }
    }
    fn necessary_limit(&self, limit: f64) -> f64 {
        if self.cannot_improve(limit) {
            limit.next_up()
        } else {
            limit
        }
    }
    fn assignment_necessary_limit(&self, a: &Assignment) -> f64 {
        let limit = self.assignment_limit(a);
        if self.cannot_improve_set(a, limit) {
            limit.next_up()
        } else {
            limit
        }
    }
    fn cannot_improve(&self, upper: f64) -> bool {
        (self.input.objective.live_floor()
            && !self.input.objective.score_ties()
            && upper <= crate::search_floor())
            || (self.input.floor_exclusive && upper <= self.input.floor)
            || (self.input.streaming
                && self.hits.len() >= self.input.k
                && (upper < self.hits[self.input.k - 1].key
                    || !self.input.objective.score_ties()
                        && upper == self.hits[self.input.k - 1].key))
    }
    fn compare_hit(&mut self, hit: &mut Hit, index: usize) -> Result<std::cmp::Ordering, String> {
        let primary = hit.key.total_cmp(&self.hits[index].key);
        if !primary.is_eq() || !self.input.objective.score_ties() {
            return Ok(primary);
        }
        if hit.power == self.hits[index].power
            && hit.team.members == self.hits[index].team.members
            && hit.team.snaps == self.hits[index].team.snaps
        {
            return Ok(std::cmp::Ordering::Equal);
        }
        self.input.objective.complete_score_tie(hit)?;
        self.input
            .objective
            .complete_score_tie(&mut self.hits[index])?;
        let a = self.input.objective.tie_rank(hit);
        let b = self.input.objective.tie_rank(&self.hits[index]);
        Ok(a[0].total_cmp(&b[0]).then_with(|| a[1].total_cmp(&b[1])))
    }
    fn insert_position(&mut self, hit: &mut Hit) -> Result<usize, String> {
        let mut i = self.hits.len();
        while i > 0 && self.compare_hit(hit, i - 1)?.is_gt() {
            i -= 1;
        }
        Ok(i)
    }
    fn set_key(&self, team: &[usize]) -> String {
        let mut keys: Vec<_> = team
            .iter()
            .map(|&i| self.input.members[i].key.as_str())
            .collect();
        keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        keys.join("|")
    }
    fn profile(&mut self, leader: usize) -> Arc<LeaderProfile> {
        self.profiles
            .entry(leader)
            .or_insert_with(|| {
                Arc::new(power::leader_profile(
                    self.input.master,
                    &self.input.members[leader],
                ))
            })
            .clone()
    }
    fn retain(&mut self, candidate: Candidate, evaluated: Option<Exact>) -> Result<(), String> {
        if !self.input.streaming {
            self.candidates.push(candidate);
            return Ok(());
        }
        if evaluated.is_none() && self.hits.len() >= self.input.k && self.expired() {
            self.timed_out = true;
            self.unexplored = self.unexplored.max(candidate.high);
            return Ok(());
        }
        if self.cannot_improve(candidate.high) {
            return Ok(());
        }
        let existing = self
            .hit_keys
            .iter()
            .position(|key| key == &candidate.set_key);
        if existing.is_some_and(|i| {
            self.hits[i].key > candidate.high
                || !self.input.objective.score_ties() && self.hits[i].key == candidate.high
        }) {
            return Ok(());
        }
        let local = (if self.hits.len() >= self.input.k {
            self.hits[self.input.k - 1].key
        } else {
            f64::NEG_INFINITY
        })
        .max(
            existing
                .map(|i| self.hits[i].key)
                .unwrap_or(f64::NEG_INFINITY),
        );
        let cutoff = Cutoff {
            key: local.max(self.input.floor),
            prune_equal: !self.input.objective.score_ties()
                && local >= self.input.floor
                && local.is_finite(),
        };
        let was_evaluated = evaluated.is_some();
        let exact = if let Some(exact) = evaluated {
            Evaluation::Exact(exact)
        } else {
            self.input.objective.evaluate(
                &candidate.team,
                candidate.power,
                &candidate.slot_powers,
                cutoff,
            )?
        };
        let exact = match exact {
            Evaluation::Exact(exact) => exact,
            Evaluation::Pruned(bound) => {
                if !(bound < cutoff.key || (cutoff.prune_equal && bound == cutoff.key)) {
                    return Err("unsafe-objective-cutoff".into());
                }
                return Ok(());
            }
        };
        if !was_evaluated {
            self.stats.exact += 1;
        }
        self.frontier.offer(&candidate.set_key, exact.key);
        let mut hit = Hit {
            team: candidate.team,
            power: candidate.power,
            slot_powers: candidate.slot_powers,
            totals: candidate.totals,
            key: exact.key,
            detail: exact.detail,
        };
        if let Some(index) = existing {
            if !self.compare_hit(&mut hit, index)?.is_gt() {
                return Ok(());
            }
            self.hits.remove(index);
            self.hit_keys.remove(index);
        }
        let i = self.insert_position(&mut hit)?;
        self.hits.insert(i, hit);
        self.hit_keys.insert(i, candidate.set_key.clone());
        self.retained.insert(candidate.set_key, exact.key);
        if self.hits.len() > self.input.k {
            self.hits.pop();
            let key = self.hit_keys.pop().unwrap();
            self.retained.remove(&key);
        }
        if let Some(hit) = self.hits.first() {
            self.input.objective.observe_hit(hit);
        }
        if let Some(callback) = self.input.on_candidate.as_deref_mut() {
            callback(&self.hits);
        }
        Ok(())
    }
    fn seed_valid(&self, seed: &Team) -> bool {
        valid_seed(
            self.input.members,
            self.input.snaps,
            &self.input.constraints,
            seed,
        )
    }
    fn retain_seed(&mut self, candidate: Candidate, exact: Exact) -> Result<(), String> {
        if self.input.objective.score_ties() {
            return self.retain(candidate, Some(exact));
        }
        let rank = self.input.objective.seed_rank(exact.key, &exact.detail);
        let existing = self
            .hit_keys
            .iter()
            .position(|key| key == &candidate.set_key);
        let replace = existing.or_else(|| {
            if self.hits.len() < self.input.k
                || self.hits.last().is_some_and(|hit| hit.key < exact.key)
            {
                return None;
            }
            self.hits
                .iter()
                .enumerate()
                .filter(|(_, hit)| hit.key == exact.key)
                .min_by(|(_, a), (_, b)| {
                    self.input
                        .objective
                        .seed_rank(a.key, &a.detail)
                        .partial_cmp(&self.input.objective.seed_rank(b.key, &b.detail))
                        .unwrap()
                })
                .map(|(index, _)| index)
        });
        if let Some(index) = replace {
            let old = &self.hits[index];
            if old.key == exact.key && rank > self.input.objective.seed_rank(old.key, &old.detail) {
                self.retained.remove(&self.hit_keys[index]);
                self.hit_keys[index] = candidate.set_key.clone();
                self.retained.insert(candidate.set_key, exact.key);
                self.hits[index] = Hit {
                    team: candidate.team,
                    power: candidate.power,
                    slot_powers: candidate.slot_powers,
                    totals: candidate.totals,
                    key: exact.key,
                    detail: exact.detail,
                };
                return Ok(());
            }
        }
        self.retain(candidate, Some(exact))
    }
    fn seed_values(&mut self, seed: &Team) -> (Totals, Vec<f64>) {
        let profile = self.profile(seed.members[0]);
        let members: Vec<_> = seed
            .members
            .iter()
            .map(|&i| self.input.members[i].clone())
            .collect();
        let percentages =
            power::leader_percents(self.input.master, &profile, &members, 0, self.input.music);
        let powers: Vec<f64> = seed
            .members
            .iter()
            .enumerate()
            .map(|(slot, &i)| {
                self.bases[i].fixed_points as f64
                    + power::leader_points(&self.bases[i], &percentages[slot]) as f64
                    + self.snap_power[i * self.width
                        + if seed.snaps[slot] < 0 {
                            self.width - 1
                        } else {
                            seed.snaps[slot] as usize
                        }]
            })
            .collect();
        let power = powers.iter().fold(0.0, |a, b| a + b);
        let mut totals = Totals {
            power,
            ..Totals::default()
        };
        for (slot, &i) in seed.members.iter().enumerate() {
            let j = seed.snaps[slot];
            let col = if j < 0 { self.width - 1 } else { j as usize };
            totals.skill += self.skill[i * self.width + col];
            totals.skill_low += self.skill_low[i * self.width + col];
            totals.bonus += self.member_bonus[i]
                + if j < 0 {
                    0.0
                } else {
                    self.snap_bonus[j as usize]
                };
        }
        (totals, powers)
    }
    fn evaluate_seed(&mut self, seed: Team) -> Result<(), String> {
        let (totals, powers) = self.seed_values(&seed);
        let power = totals.power;
        if self
            .input
            .constraints
            .min_bonus
            .is_some_and(|b| totals.bonus < b)
            || self
                .input
                .constraints
                .max_bonus
                .is_some_and(|b| totals.bonus > b)
        {
            return Ok(());
        }
        let exact = self.input.objective.exact(&seed, power, &powers)?;
        self.stats.exact += 1;
        let set_key = self.set_key(&seed.members);
        self.frontier.offer(&set_key, exact.key);
        self.retain_seed(
            Candidate {
                set_key,
                team: seed,
                power,
                slot_powers: powers,
                totals,
                low: exact.key,
                high: exact.key,
            },
            exact,
        )
    }
    fn rematch_seed(
        &mut self,
        team: &Team,
        snapshots: &[i32],
        lambda: f64,
        linear: bool,
    ) -> Option<Team> {
        let weights: Vec<Vec<f64>> = team
            .members
            .iter()
            .map(|&member| {
                snapshots
                    .iter()
                    .map(|&snap| {
                        if !self.options[member].contains(&snap) {
                            return NONE;
                        }
                        let col = if snap < 0 {
                            self.width - 1
                        } else {
                            snap as usize
                        };
                        let power = self.snap_power[member * self.width + col];
                        if linear {
                            lambda * power + self.input.objective.seed_skill(member, snap) / lambda
                        } else {
                            power
                        }
                    })
                    .collect()
            })
            .collect();
        let mut assigned = [0; 5];
        if self.matching.maximum(&weights, 5, Some(&mut assigned)) <= NONE / 2.0 {
            return None;
        }
        Some(Team {
            members: team.members.clone(),
            snaps: assigned.iter().map(|&col| snapshots[col]).collect(),
        })
    }
    fn warm_seed(&mut self, owned_leaders: Option<&[usize]>) -> Result<(), String> {
        let Some(mut objective) = self.input.objective.seed_objective() else {
            return Ok(());
        };
        let ceiling = self.input.objective.seed_ceiling();
        if self
            .hits
            .first()
            .is_some_and(|hit| hit.key >= ceiling || self.input.objective.seed_ready(&hit.detail))
        {
            return Ok(());
        }
        let alternate = self
            .input
            .objective
            .seed_direct_bound(Totals::default())
            .is_some();
        let mut direct_start: Option<(Team, [f64; 3])> = None;
        let mut leaders = HashMap::default();
        for leader in 0..self.input.members.len() {
            if self.excluded_members.contains(&leader)
                || owned_leaders.is_some_and(|leaders| !leaders.contains(&leader))
                || self
                    .input
                    .constraints
                    .leader
                    .is_some_and(|required| required != leader as i32)
            {
                continue;
            }
            let profile = self.profile(leader);
            let mut identity =
                power::leader_identity(self.input.master, &profile, &self.input.members[leader]);
            for row in &mut identity {
                row[1] = 0;
            }
            identity.sort_unstable();
            let strength = profile
                .effects
                .iter()
                .map(|effect| effect.value as f64)
                .sum::<f64>();
            let score = strength * 16777216.0
                + self.bases[leader].fixed_points as f64
                + self.best[leader].power;
            leaders
                .entry(identity)
                .and_modify(|best: &mut (usize, f64)| {
                    if score > best.1 {
                        *best = (leader, score);
                    }
                })
                .or_insert((leader, score));
        }
        let mut leaders: Vec<_> = leaders.into_values().collect();
        leaders.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (leader, _) in leaders {
            if self.expired() {
                break;
            }
            let mut constraints = self.input.constraints.clone();
            constraints.leader = Some(leader as i32);
            let warm = search(Input {
                master: self.input.master,
                player: self.input.player,
                members: self.input.members,
                snaps: self.input.snaps,
                music: self.input.music,
                objective: objective.as_mut(),
                constraints,
                k: 1,
                deadline: self.input.deadline,
                now: self.input.now,
                seeds: Vec::new(),
                shard: None,
                owns: self.input.owns,
                floor: f64::NEG_INFINITY,
                floor_exclusive: false,
                streaming: true,
                disable_snap_dominance: false,
                member_power_percent: self.input.member_power_percent,
                snap_power_percent: self.input.snap_power_percent,
                on_candidate: None,
                progress: None,
            })?;
            self.stats.add(&warm.stats);
            for hit in warm.hits {
                if self.seed_valid(&hit.team) {
                    self.evaluate_seed(hit.team.clone())?;
                    if alternate {
                        let (totals, powers) = self.seed_values(&hit.team);
                        let exact = self
                            .input
                            .objective
                            .exact(&hit.team, totals.power, &powers)?;
                        self.stats.exact += 1;
                        let rank = [
                            exact.detail["directMean"].as_f64().unwrap_or(0.0),
                            exact.detail["otherMean"].as_f64().unwrap_or(0.0),
                            exact.detail["scores"]["mean"].as_f64().unwrap_or(0.0),
                        ];
                        if direct_start.as_ref().is_none_or(|(_, best)| rank > *best) {
                            direct_start = Some((hit.team, rank));
                        }
                    }
                }
                if self.hits.first().is_some_and(|hit| {
                    hit.key >= ceiling || self.input.objective.seed_ready(&hit.detail)
                }) {
                    return Ok(());
                }
            }
        }
        let mut starts = self.input.seeds.clone();
        if let Some(best) = self.hits.first() {
            starts.push(best.team.clone());
        }
        if let Some((team, _)) = direct_start {
            starts.push(team);
        }
        for direct in [false, true] {
            if direct && !alternate {
                continue;
            }
            let rating = |exact: &Exact| {
                (
                    if direct {
                        exact.detail["directMean"].as_f64().unwrap_or(exact.key)
                    } else {
                        exact.key
                    },
                    exact.detail["otherMean"].as_f64().unwrap_or(0.0),
                    exact.detail["scores"]["mean"].as_f64().unwrap_or(0.0),
                )
            };
            let mut seen = HashSet::default();
            for origin in starts.iter().cloned() {
                if self.expired() {
                    break;
                }
                if !self.seed_valid(&origin)
                    || owned_leaders.is_some_and(|leaders| !leaders.contains(&origin.members[0]))
                    || !seen.insert((origin.members.clone(), origin.snaps.clone()))
                {
                    continue;
                }
                let (totals, powers) = self.seed_values(&origin);
                if self
                    .input
                    .constraints
                    .min_bonus
                    .is_some_and(|bonus| totals.bonus < bonus)
                    || self
                        .input
                        .constraints
                        .max_bonus
                        .is_some_and(|bonus| totals.bonus > bonus)
                {
                    continue;
                }
                let initial = self.input.objective.exact(&origin, totals.power, &powers)?;
                self.stats.exact += 1;
                let mut team = origin;
                let mut value = rating(&initial);
                while !self.expired() && (direct || value.0 < self.input.objective.seed_ceiling()) {
                    let mut next_value = value;
                    let (current_totals, _) = self.seed_values(&team);
                    let lambda = ((self.input.objective.product_base().unwrap_or(0.0)
                        + current_totals.skill)
                        / current_totals.power.max(1.0))
                    .sqrt();
                    let lambda = if lambda > 0.0 && lambda.is_finite() {
                        lambda
                    } else {
                        1.0
                    };
                    let mut neighbors = Vec::new();
                    for slot in 0..5 {
                        for member in 0..self.input.members.len() {
                            if member == team.members[slot] {
                                continue;
                            }
                            let mut snapshots = vec![team.snaps[slot]];
                            for occupied in [false, true] {
                                for axis in 0..3 {
                                    let metric = |snap: i32| {
                                        let col = if snap < 0 {
                                            self.width - 1
                                        } else {
                                            snap as usize
                                        };
                                        let power = self.snap_power[member * self.width + col];
                                        let linear = lambda * power
                                            + self.skill[member * self.width + col] / lambda;
                                        match axis {
                                            0 => power,
                                            1 => linear,
                                            _ => {
                                                linear
                                                    + if snap < 0 {
                                                        0.0
                                                    } else {
                                                        self.snap_bonus[snap as usize] * 16777216.0
                                                    }
                                            }
                                        }
                                    };
                                    if let Some(&snap) = self.options[member]
                                        .iter()
                                        .filter(|&&snap| {
                                            occupied
                                                || snap < 0
                                                || snap == team.snaps[slot]
                                                || !team.snaps.contains(&snap)
                                        })
                                        .max_by(|&&a, &&b| metric(a).total_cmp(&metric(b)))
                                    {
                                        if !snapshots.contains(&snap) {
                                            snapshots.push(snap);
                                        }
                                    }
                                }
                            }
                            for snap in snapshots {
                                let mut next = team.clone();
                                next.members[slot] = member;
                                if snap >= 0 {
                                    if let Some(other) =
                                        team.snaps.iter().position(|&held| held == snap)
                                    {
                                        next.snaps[other] = team.snaps[slot];
                                    }
                                }
                                next.snaps[slot] = snap;
                                for leader in 0..5 {
                                    let mut directed = next.clone();
                                    directed.members.swap(0, leader);
                                    directed.snaps.swap(0, leader);
                                    neighbors.push(directed);
                                }
                            }
                        }
                        for &snap in &self.options[team.members[slot]] {
                            if snap == team.snaps[slot] {
                                continue;
                            }
                            let mut next = team.clone();
                            next.snaps[slot] = snap;
                            neighbors.push(next);
                        }
                        for other in 0..slot {
                            if team.snaps[slot] != team.snaps[other] {
                                let mut next = team.clone();
                                next.snaps.swap(slot, other);
                                neighbors.push(next);
                            }
                        }
                        if slot > 0 {
                            let mut next = team.clone();
                            next.members.swap(0, slot);
                            next.snaps.swap(0, slot);
                            neighbors.push(next);
                        }
                    }
                    let mut best = None;
                    for rematch in [false, true] {
                        if rematch {
                            let mut sets = vec![team.snaps.clone()];
                            for slot in 0..5 {
                                if team.snaps[slot] >= 0
                                    && self.required_snaps.contains(&(team.snaps[slot] as usize))
                                {
                                    continue;
                                }
                                for snap in -1..self.input.snaps.len() as i32 {
                                    if snap == team.snaps[slot]
                                        || snap >= 0
                                            && (self.input.constraints.no_snaps
                                                || team.snaps.contains(&snap))
                                    {
                                        continue;
                                    }
                                    let mut next = team.snaps.clone();
                                    next[slot] = snap;
                                    sets.push(next);
                                }
                            }
                            for snapshots in sets {
                                for linear in [false, true] {
                                    if let Some(next) =
                                        self.rematch_seed(&team, &snapshots, lambda, linear)
                                    {
                                        neighbors.push(next);
                                    }
                                }
                            }
                        }
                        for next in neighbors.drain(..) {
                            if self.expired() {
                                break;
                            }
                            if !self.seed_valid(&next)
                                || owned_leaders
                                    .is_some_and(|leaders| !leaders.contains(&next.members[0]))
                            {
                                continue;
                            }
                            let (totals, powers) = self.seed_values(&next);
                            if self
                                .input
                                .constraints
                                .min_bonus
                                .is_some_and(|bonus| totals.bonus < bonus)
                                || self
                                    .input
                                    .constraints
                                    .max_bonus
                                    .is_some_and(|bonus| totals.bonus > bonus)
                                || (if direct {
                                    self.input.objective.seed_direct_bound(totals).unwrap()
                                } else {
                                    self.input.objective.bound(totals)
                                }) < next_value.0
                                || !direct
                                    && self
                                        .input
                                        .objective
                                        .leaf_bound(&next, totals)
                                        .is_some_and(|upper| upper < next_value.0)
                            {
                                continue;
                            }
                            let exact = self.input.objective.exact(&next, totals.power, &powers)?;
                            self.stats.exact += 1;
                            let candidate = rating(&exact);
                            if candidate > next_value {
                                next_value = candidate;
                                best = Some((next, totals, powers, exact));
                            }
                        }
                        if best.is_some() {
                            break;
                        }
                    }
                    let Some((next, totals, powers, exact)) = best else {
                        break;
                    };
                    team = next;
                    value = next_value;
                    let set_key = self.set_key(&team.members);
                    self.frontier.offer(&set_key, exact.key);
                    self.retain_seed(
                        Candidate {
                            set_key,
                            team: team.clone(),
                            power: totals.power,
                            slot_powers: powers,
                            totals,
                            low: exact.key,
                            high: exact.key,
                        },
                        exact,
                    )?;
                    if self
                        .hits
                        .first()
                        .is_some_and(|hit| self.input.objective.seed_ready(&hit.detail))
                    {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }
    fn member_floor_possible(&self, chosen: &[usize], left: usize) -> bool {
        let Some(floor) = &self.input.constraints.member_floor else {
            return true;
        };
        let current: f64 = chosen.iter().map(|&i| floor.values[i]).sum();
        current + floor.maximum * left as f64 >= floor.minimum
    }
    fn member_price(
        &self,
        member: usize,
        snap: i32,
        slot_power: &[f64],
        lambda: f64,
        mu: f64,
    ) -> f64 {
        let c = if snap < 0 {
            self.width - 1
        } else {
            snap as usize
        };
        let p = (slot_power[member] + self.snap_power[member * self.width + c]).next_up();
        let b = self.member_bonus[member]
            + if snap < 0 {
                0.0
            } else {
                self.snap_bonus[snap as usize]
            };
        (((lambda * p).next_up() + (self.skill[member * self.width + c] / lambda).next_up())
            .next_up()
            + (mu * b).next_up())
        .next_up()
    }
    fn column_cut(
        &self,
        leader: usize,
        slot_power: &[f64],
        characters: &[Vec<usize>],
        unpriced: &[f64],
        parameters: (f64, f64),
        constant: f64,
    ) -> Option<ColumnCut> {
        let (lambda, mu) = parameters;
        let mut representatives: Vec<_> = characters
            .iter()
            .filter_map(|group| {
                group
                    .iter()
                    .copied()
                    .max_by(|&a, &b| unpriced[a].total_cmp(&unpriced[b]))
            })
            .collect();
        representatives.sort_by(|&a, &b| unpriced[b].total_cmp(&unpriced[a]));
        representatives.truncate(4);
        representatives.push(leader);
        let columns = self.width - 1 + 5;
        let matrix: Vec<_> = representatives
            .iter()
            .map(|&member| {
                let mut row = vec![-1e100; columns];
                for &snap in &self.options[member] {
                    let value = self.member_price(member, snap, slot_power, lambda, mu);
                    if snap < 0 {
                        row[self.width - 1..].fill(value);
                    } else {
                        row[snap as usize] = value;
                    }
                }
                row
            })
            .collect();
        let mut matching = Scratch::default();
        let mut selected = [0; 5];
        matching.maximum(&matrix, columns, Some(&mut selected));
        let mut prices = matching.column_prices(columns);
        prices[self.width - 1..].fill(0.0);
        let price_sum = prices
            .iter()
            .filter(|&&value| value > 0.0)
            .fold(0.0, |sum, &value| (sum + value).next_up());
        if price_sum == 0.0 || !price_sum.is_finite() {
            return None;
        }
        let values: Vec<_> = (0..self.input.members.len())
            .map(|member| {
                self.options[member]
                    .iter()
                    .map(|&snap| {
                        let col = if snap < 0 {
                            self.width - 1
                        } else {
                            snap as usize
                        };
                        (self.member_price(member, snap, slot_power, lambda, mu) - prices[col])
                            .next_up()
                    })
                    .fold(f64::NEG_INFINITY, f64::max)
            })
            .collect();
        let mut suffix = vec![vec![0.0; 5]; characters.len() + 1];
        let mut top = Vec::new();
        for from in (0..characters.len()).rev() {
            top.push(
                characters[from]
                    .iter()
                    .map(|&member| values[member])
                    .fold(f64::NEG_INFINITY, f64::max),
            );
            top.sort_by(|a, b| b.total_cmp(a));
            top.truncate(4);
            for count in 1..=4 {
                suffix[from][count] = (suffix[from][count - 1]
                    + top.get(count - 1).copied().unwrap_or(0.0))
                .next_up();
            }
        }
        let cut = ColumnCut {
            constant: (constant + price_sum).next_up(),
            members: values,
            suffix,
        };
        Some(cut)
    }
    fn member_cuts(
        &mut self,
        leader: usize,
        slot_power: &[f64],
        characters: &[Vec<usize>],
        power: f64,
        skill: f64,
        bonus: f64,
    ) -> Vec<MemberCut> {
        let Some(base) = self.input.objective.product_base() else {
            return Vec::new();
        };
        if self
            .input
            .objective
            .joint_targets(self.necessary_limit(self.limit()).max(0.0))
            .is_none()
        {
            return Vec::new();
        }
        let lambda = ((base + skill) / power).sqrt();
        if !lambda.is_finite() || lambda <= 0.0 || base < 0.0 {
            return Vec::new();
        }
        let spread = (lambda * power + (base + skill) / lambda) / bonus.abs().max(1.0);
        let with_bonus = self.any_snap_bonus || self.member_bonus.iter().any(|&b| b != 0.0);
        let mus: &[f64] = if with_bonus && self.member_bonus_target().is_none() {
            &[0.0, 0.25, 1.0, 4.0]
        } else {
            &[0.0]
        };
        let mut cuts = Vec::new();
        let scales: &[f64] = if self.input.objective.generic_assignment_product() {
            &[1.0, 0.75, 1.5]
        } else {
            &[1.0, 0.75]
        };
        for &scale in scales {
            let l = lambda * scale;
            for &factor in mus {
                let mu = factor * spread;
                let values: Vec<_> = (0..self.input.members.len())
                    .map(|m| {
                        self.options[m]
                            .iter()
                            .map(|&s| self.member_price(m, s, slot_power, l, mu))
                            .fold(f64::NEG_INFINITY, f64::max)
                    })
                    .collect();
                let constant = (base / l).next_up();
                let column = if self.input.objective.member_snapshot_prices() {
                    self.column_cut(leader, slot_power, characters, &values, (l, mu), constant)
                } else {
                    None
                };
                let mut suffix = vec![vec![0.0; 5]; characters.len() + 1];
                let mut top = Vec::new();
                for from in (0..characters.len()).rev() {
                    top.push(
                        characters[from]
                            .iter()
                            .map(|&m| values[m])
                            .fold(f64::NEG_INFINITY, f64::max),
                    );
                    top.sort_by(|a, b| b.total_cmp(a));
                    top.truncate(4);
                    for count in 1..=4 {
                        suffix[from][count] = (suffix[from][count - 1]
                            + top.get(count - 1).copied().unwrap_or(0.0))
                        .next_up();
                    }
                }
                cuts.push(MemberCut {
                    mu,
                    column,
                    constant,
                    members: values,
                    suffix,
                });
            }
        }
        cuts
    }
    fn member_bonus_target(&self) -> Option<usize> {
        let target = self.input.constraints.max_bonus?;
        if self.any_snap_bonus
            || self.input.constraints.min_bonus != Some(target)
            || !(0.0..=128.0).contains(&target)
            || target.fract() != 0.0
            || self
                .member_bonus
                .iter()
                .any(|bonus| *bonus < 0.0 || bonus.fract() != 0.0)
        {
            return None;
        }
        Some(target as usize)
    }
    fn member_bonus_bounds(
        &self,
        slot_power: &[f64],
        characters: &[Vec<usize>],
        cuts: &[MemberCut],
    ) -> Option<BonusBounds> {
        let target = self.member_bonus_target()?;
        let weights: Vec<_> = slot_power
            .iter()
            .enumerate()
            .map(|(member, value)| value + self.best[member].power)
            .collect();
        let power = BonusSuffix::new(characters, &self.member_bonus, &weights, target);
        let weights: Vec<_> = self.best.iter().map(|best| best.weight).collect();
        let skill = BonusSuffix::new(characters, &self.member_bonus, &weights, target);
        let tables = cuts
            .iter()
            .map(|cut| BonusSuffix::new(characters, &self.member_bonus, &cut.members, target))
            .collect();
        let columns = cuts
            .iter()
            .map(|cut| {
                cut.column.as_ref().map(|column| {
                    BonusSuffix::new(characters, &self.member_bonus, &column.members, target)
                })
            })
            .collect();
        Some(BonusBounds {
            target,
            power,
            skill,
            cuts: tables,
            columns,
        })
    }
    fn member_joint_possible(
        &mut self,
        m: &Members,
        from: usize,
        left: usize,
        member_bonus: f64,
    ) -> bool {
        if m.cuts.is_empty() || !self.limit().is_finite() {
            return true;
        }
        let limit = self.necessary_limit(self.limit());
        if limit != self.target_limit {
            self.target_limit = limit;
            self.member_targets = self.input.objective.joint_targets(limit).map(|targets| {
                targets
                    .into_iter()
                    .map(|t| {
                        (
                            (2.0 * t.product.max(0.0).sqrt()).next_down(),
                            t.bonus.max(
                                self.input
                                    .constraints
                                    .min_bonus
                                    .unwrap_or(f64::NEG_INFINITY),
                            ),
                        )
                    })
                    .collect()
            });
        }
        let Some(targets) = &self.member_targets else {
            return true;
        };
        let mut upper = [0.0; 12];
        let remaining = m
            .bonus_bounds
            .as_ref()
            .map(|table| table.target - member_bonus as usize);
        for (i, cut) in m.cuts.iter().enumerate() {
            let sum = m.cut_sums[m.chosen.len()][i];
            let suffix = if let Some(bonus) = remaining {
                m.bonus_bounds.as_ref().unwrap().cuts[i].get(from, left, bonus)
            } else {
                cut.suffix[from.min(cut.suffix.len() - 1)][left]
            };
            upper[i] = (sum + suffix).next_up();
            if let Some(column) = &cut.column {
                let sum = m.column_sums[m.chosen.len()][i];
                let suffix = if let Some(bonus) = remaining {
                    m.bonus_bounds.as_ref().unwrap().columns[i]
                        .as_ref()
                        .unwrap()
                        .get(from, left, bonus)
                } else {
                    column.suffix[from.min(column.suffix.len() - 1)][left]
                };
                upper[i] = upper[i].min((sum + suffix).next_up());
            }
        }
        targets.iter().any(|&(root, bonus)| {
            m.cuts
                .iter()
                .enumerate()
                .all(|(i, cut)| upper[i] >= (root + (cut.mu * bonus).next_down()).next_down())
        })
    }
    fn member_set_bound(
        &mut self,
        members: &[usize],
        totals: Totals,
        left: usize,
        from: usize,
    ) -> f64 {
        let plain = self.input.objective.bound(totals);
        if plain < self.limit() {
            return plain;
        }
        let limit = self.necessary_limit(self.limit());
        self.input
            .objective
            .member_bound_at(members, totals, left, from, limit)
            .map(|bound| plain.min(bound))
            .unwrap_or(plain)
    }
    fn member_visit(
        &mut self,
        m: &mut Members,
        index: usize,
        power: f64,
        weight: f64,
        bonus: f64,
        member_part: f64,
    ) -> Result<(), String> {
        let left = 4 - m.chosen.len();
        let mut selected = [0; 5];
        selected[0] = m.leader;
        selected[1..=m.chosen.len()].copy_from_slice(&m.chosen);
        let chosen = &selected[..m.chosen.len() + 1];
        let (bonus_power, bonus_skill) = if let Some(table) = &m.bonus_bounds {
            if member_part > table.target as f64 {
                return Ok(());
            }
            let bonus = table.target - member_part as usize;
            let value = table.power.get(index, left, bonus);
            if value == f64::NEG_INFINITY {
                return Ok(());
            }
            let selected_skill = chosen.iter().fold(0.0, |sum, &member| {
                (sum + self.best[member].weight).next_up()
            });
            (
                Some((power + value).next_up()),
                Some((selected_skill + table.skill.get(index, left, bonus)).next_up()),
            )
        } else {
            (None, None)
        };
        if self.input.constraints.member_floor.is_some()
            && !self.member_floor_possible(chosen, left)
        {
            return Ok(());
        }
        if self.timed_out {
            let totals = Totals {
                power: bonus_power.unwrap_or_else(|| power + top_sum(&m.char_power, index, left)),
                skill: bonus_skill.unwrap_or_else(|| weight + top_sum(&m.char_skill, index, left)),
                skill_low: 0.0,
                bonus: bonus + top_sum(&m.char_bonus, index, left),
            };
            let bound = self.input.objective.bound(totals);
            self.unexplored = self.unexplored.max(bound);
            return Ok(());
        }
        self.stats.member_nodes += 1;
        if self.stats.member_nodes & 1023 == 0 {
            self.report_pending();
        }
        if left == 0 {
            let limit = self.necessary_limit(self.limit());
            if let Some(upper) = self
                .input
                .objective
                .early_member_bound(chosen, index, limit)
            {
                if upper < self.limit() || self.cannot_improve(upper) {
                    return Ok(());
                }
            }
            if !self.member_joint_possible(m, index, 0, member_part) {
                return Ok(());
            }

            let team: Vec<_> = std::iter::once(m.leader)
                .chain(m.chosen.iter().copied())
                .collect();
            if self.input.owns.is_none_or(|owns| owns(&team)) {
                self.assign(team)?;
            }
            return Ok(());
        }
        if m.characters.len() - index < left {
            return Ok(());
        }
        if index < m.forced {
            for option in 0..m.characters[index].len() {
                let i = m.characters[index][option];
                m.choose(i);
                self.member_visit(
                    m,
                    index + 1,
                    power + m.slot_power[i] + self.best[i].power,
                    weight + self.best[i].weight,
                    bonus + self.member_bonus[i] + self.best[i].bonus,
                    member_part + self.member_bonus[i],
                )?;
                m.chosen.pop();
            }
            return Ok(());
        }
        let totals = Totals {
            power: bonus_power.unwrap_or_else(|| power + top_sum(&m.char_power, index, left)),
            skill: bonus_skill.unwrap_or_else(|| weight + top_sum(&m.char_skill, index, left)),
            skill_low: 0.0,
            bonus: (bonus + top_sum(&m.char_bonus, index, left)).min(
                member_part + top_sum(&m.char_member_bonus, index, left) + self.snap_bonus_top5,
            ),
        };
        let plain = self.input.objective.bound(totals);
        if plain < self.limit() || self.cannot_improve(plain) {
            return Ok(());
        }
        let limit = self.necessary_limit(self.limit());
        if let Some(upper) = self
            .input
            .objective
            .early_member_bound(chosen, index, limit)
        {
            if upper < self.limit() || self.cannot_improve(upper) {
                return Ok(());
            }
        }
        if !self.member_joint_possible(m, index, left, member_part) {
            return Ok(());
        }
        let bound = self.member_set_bound(chosen, totals, left, index);
        if bound < self.limit()
            || self.cannot_improve(bound)
            || self
                .input
                .constraints
                .min_bonus
                .is_some_and(|b| bonus + top_sum(&m.char_bonus, index, left) < b)
        {
            return Ok(());
        }
        let snap_bonus = self.best[m.leader].bonus
            + m.chosen
                .iter()
                .fold(0.0, |sum, &i| sum + self.best[i].bonus);
        if self
            .input
            .constraints
            .max_bonus
            .is_some_and(|b| bonus - snap_bonus > b)
        {
            return Ok(());
        }
        if self.expired() {
            self.timed_out = true;
            self.unexplored = self.unexplored.max(bound);
            return Ok(());
        }
        for option in 0..m.characters[index].len() {
            let i = m.characters[index][option];
            m.choose(i);
            self.member_visit(
                m,
                index + 1,
                power + m.slot_power[i] + self.best[i].power,
                weight + self.best[i].weight,
                bonus + self.member_bonus[i] + self.best[i].bonus,
                member_part + self.member_bonus[i],
            )?;
            m.chosen.pop();
        }
        self.member_visit(m, index + 1, power, weight, bonus, member_part)
    }
    fn assignment_limit(&self, a: &Assignment) -> f64 {
        self.limit().max(a.set_low).max(a.exact_low)
    }
    fn cannot_improve_set(&self, a: &Assignment, upper: f64) -> bool {
        self.cannot_improve(upper)
            || self.input.streaming
                && (a.exact_low > upper
                    || !self.input.objective.score_ties() && a.exact_low == upper)
    }
    fn assignment_bound(&mut self, a: &Assignment, totals: Totals, from: usize) -> f64 {
        self.input
            .objective
            .assignment_bound(&a.team, totals, &a.assigned, from)
            .unwrap_or_else(|| self.input.objective.bound(totals))
    }
    fn tables(&mut self, t: &mut Tables, a: &Assignment, from: usize) {
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.column_stamp.fill(0);
            self.stamp = 1;
        }
        let mut count = 0;
        for options in &a.options[from..] {
            for &j in options {
                if j >= 0 && !a.used[j as usize] && self.column_stamp[j as usize] != self.stamp {
                    self.column_stamp[j as usize] = self.stamp;
                    self.column_of[j as usize] = count;
                    t.column_snap[count] = j as usize;
                    count += 1;
                }
            }
        }
        t.count = count;
        t.columns = count + 5 - from;
        for row in 0..5 - from {
            let slot = from + row;
            let member = a.team[slot];
            t.power[row][..t.columns].fill(NONE);
            t.weight[row][..t.columns].fill(NONE);
            t.bonus[row][..t.columns].fill(NONE);
            for &j in &a.options[slot] {
                if j >= 0 && a.used[j as usize] {
                    continue;
                }
                let col = if j < 0 { self.width - 1 } else { j as usize };
                let power = self.snap_power[member * self.width + col];
                let weight = self.skill[member * self.width + col];
                let bonus = if j < 0 {
                    0.0
                } else {
                    self.snap_bonus[j as usize]
                };
                if j < 0 {
                    for empty in count..t.columns {
                        t.power[row][empty] = power;
                        t.weight[row][empty] = weight;
                        t.bonus[row][empty] = bonus;
                    }
                } else {
                    let at = self.column_of[j as usize];
                    t.power[row][at] = power;
                    t.weight[row][at] = weight;
                    t.bonus[row][at] = bonus;
                }
            }
        }
    }
    fn blend(t: &mut Tables, lambda: f64) {
        for r in 0..t.power.len() {
            for c in 0..t.columns {
                t.blended[r][c] = if t.power[r][c] <= NONE / 2.0 {
                    NONE
                } else {
                    lambda * t.power[r][c] + t.weight[r][c] / lambda
                };
            }
        }
    }
    fn joint_feasible(
        &mut self,
        t: &mut Tables,
        totals: Totals,
        room: Totals,
        base: f64,
        target: JointTarget,
    ) -> bool {
        let (power, skill, bonus) = (room.power, room.skill, room.bonus);
        let x = totals.power;
        let y = base + totals.skill;
        if totals.bonus + bonus < target.bonus || (x + power) * (y + skill) < target.product {
            return false;
        }
        if totals.bonus >= target.bonus || target.product <= 0.0 {
            return true;
        }
        let lambda = ((y + skill) / (x + power).max(1e-9)).sqrt();
        if !lambda.is_finite() || lambda <= 0.0 {
            return true;
        }
        let k = 2.0 * target.product.sqrt() - lambda * x - y / lambda;
        Self::blend(t, lambda);
        let spread = t
            .blended
            .iter()
            .flatten()
            .filter(|&&v| v > NONE / 2.0)
            .fold(0.0_f64, |best, &v| best.max(v));
        for factor in [0.25, 1.0, 4.0] {
            let mu = factor * spread / (target.bonus - totals.bonus).max(1.0);
            for r in 0..t.blended.len() {
                for c in 0..t.columns {
                    t.joint[r][c] = if t.blended[r][c] <= NONE / 2.0 {
                        NONE
                    } else {
                        t.blended[r][c] + mu * t.bonus[r][c]
                    };
                }
            }
            if self.matching.maximum(&t.joint, t.columns, None)
                < k + mu * (target.bonus - totals.bonus)
            {
                return false;
            }
        }
        true
    }
    fn remaining_bound(&mut self, a: &Assignment, from: usize, totals: Totals) -> f64 {
        let mut table = std::mem::take(&mut self.pools[from]);
        self.tables(&mut table, a, from);
        let power = self.matching.maximum(&table.power, table.columns, None);
        let skill = if self.input.objective.generic_assignment_product() {
            self.matching.maximum(&table.weight, table.columns, None)
        } else {
            table.weight.iter().fold(0.0, |sum, row| {
                (sum + row[..table.columns]
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max))
                .next_up()
            })
        };
        let bonus = if self.any_snap_bonus {
            self.matching.maximum(&table.bonus, table.columns, None)
        } else {
            0.0
        };
        let result = (|| {
            if power <= NONE / 2.0 || skill <= NONE / 2.0 {
                return f64::NEG_INFINITY;
            }
            let order_limit = self.assignment_necessary_limit(a);
            let objective_bound = self.input.objective.assignment_order_bound(
                &a.team,
                Totals {
                    power: totals.power + power,
                    skill: totals.skill + skill,
                    skill_low: 0.0,
                    bonus: totals.bonus + bonus,
                },
                &a.assigned,
                from,
                order_limit,
            );
            if objective_bound.is_some_and(|bound| {
                bound < self.assignment_limit(a) || self.cannot_improve_set(a, bound)
            }) {
                return f64::NEG_INFINITY;
            }
            if !self.input.objective.generic_assignment_product() {
                return objective_bound.unwrap_or_else(|| {
                    self.assignment_bound(
                        a,
                        Totals {
                            power: totals.power + power,
                            skill: totals.skill + skill,
                            bonus: totals.bonus + bonus,
                            skill_low: 0.0,
                        },
                        from,
                    )
                });
            }
            let base = self.input.objective.product_base();
            if self.any_snap_bonus {
                if let Some(base) = base {
                    if let Some(targets) = self
                        .input
                        .objective
                        .joint_targets(self.assignment_necessary_limit(a))
                    {
                        if !targets.iter().any(|&target| {
                            self.joint_feasible(
                                &mut table,
                                totals,
                                Totals {
                                    power,
                                    skill,
                                    bonus,
                                    skill_low: 0.0,
                                },
                                base,
                                target,
                            )
                        }) {
                            return f64::NEG_INFINITY;
                        }
                    }
                }
            }
            let Some(base) = base else {
                return self.assignment_bound(
                    a,
                    Totals {
                        power: totals.power + power,
                        skill: totals.skill + skill,
                        skill_low: 0.0,
                        bonus: totals.bonus + bonus,
                    },
                    from,
                );
            };
            let x = totals.power;
            let y = base + totals.skill;
            if y + skill <= 0.0
                || x + power <= 0.0
                || !(y + skill).is_finite()
                || !(x + power).is_finite()
            {
                return self.assignment_bound(
                    a,
                    Totals {
                        power: x + power,
                        skill: totals.skill + skill,
                        skill_low: 0.0,
                        bonus: totals.bonus + bonus,
                    },
                    from,
                );
            }
            let mut product = (x + power) * (y + skill);
            let lambda = ((y + skill) / (x + power).max(1e-9)).sqrt();
            for scale in [1.0, 1.02, 1.0 / 1.02] {
                let l = lambda * scale;
                Self::blend(&mut table, l);
                let m = self.matching.maximum(&table.blended, table.columns, None);
                product = product.min((l * x + y / l + m).powi(2) / 4.0);
            }
            let upper = self.input.objective.bound(Totals {
                power: product / (y + skill),
                skill: totals.skill + skill,
                skill_low: 0.0,
                bonus: totals.bonus + bonus,
            });
            if upper < self.assignment_limit(a) || self.cannot_improve_set(a, upper) {
                return f64::NEG_INFINITY;
            }
            upper.min(self.assignment_bound(
                a,
                Totals {
                    power: x + power,
                    skill: totals.skill + skill,
                    skill_low: 0.0,
                    bonus: totals.bonus + bonus,
                },
                from,
            ))
        })();
        self.pools[from] = table;
        result
    }
    fn candidate(
        &mut self,
        a: &mut Assignment,
        picks: Vec<i32>,
        totals: Totals,
    ) -> Result<(), String> {
        let team = Team {
            members: a.team.clone(),
            snaps: picks,
        };
        if self
            .input
            .objective
            .leaf_bound(&team, totals)
            .is_some_and(|v| v < self.assignment_limit(a) || self.cannot_improve_set(a, v))
        {
            return Ok(());
        }
        let (low, high) = self.input.objective.interval(&team, totals);
        if high < self.assignment_limit(a) || self.cannot_improve_set(a, high) {
            return Ok(());
        }
        self.frontier.offer(&a.set_key, low);
        a.set_low = a.set_low.max(low);
        self.stats.candidates += 1;
        if self.candidates.len() > self.compact_at {
            self.compact();
        }
        let powers = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &i)| {
                a.base_power[slot]
                    + self.snap_power[i * self.width
                        + if team.snaps[slot] < 0 {
                            self.width - 1
                        } else {
                            team.snaps[slot] as usize
                        }]
            })
            .collect();
        self.retain(
            Candidate {
                set_key: a.set_key.clone(),
                team,
                power: totals.power,
                slot_powers: powers,
                totals,
                low,
                high,
            },
            None,
        )?;
        a.exact_low = a.exact_low.max(
            self.retained
                .get(&a.set_key)
                .copied()
                .unwrap_or(f64::NEG_INFINITY),
        );
        Ok(())
    }
    fn assign(&mut self, team: Vec<usize>) -> Result<(), String> {
        let set_key = self.set_key(&team);
        let exact_low = self
            .retained
            .get(&set_key)
            .copied()
            .unwrap_or(f64::NEG_INFINITY);
        let profile = self.profile(team[0]);
        let percents = if profile.simple {
            None
        } else {
            let members: Vec<_> = team
                .iter()
                .map(|&i| self.input.members[i].clone())
                .collect();
            Some(power::leader_percents(
                self.input.master,
                &profile,
                &members,
                0,
                self.input.music,
            ))
        };
        let base_power: Vec<_> = team
            .iter()
            .enumerate()
            .map(|(slot, &i)| {
                self.bases[i].fixed_points as f64
                    + power::leader_points(
                        &self.bases[i],
                        &percents.as_ref().map(|p| p[slot]).unwrap_or_else(|| {
                            power::leader_percent_bound(
                                self.input.master,
                                &profile,
                                &self.input.members[i],
                            )
                        }),
                    ) as f64
            })
            .collect();
        let member_bonus = team.iter().fold(0.0, |sum, &i| sum + self.member_bonus[i]);
        let totals = Totals {
            power: base_power.iter().sum::<f64>()
                + team
                    .iter()
                    .map(|&member| self.best[member].power)
                    .sum::<f64>(),
            skill: team.iter().map(|&member| self.best[member].weight).sum(),
            skill_low: 0.0,
            bonus: member_bonus
                + team
                    .iter()
                    .map(|&member| self.best[member].bonus)
                    .sum::<f64>()
                    .min(self.snap_bonus_top5),
        };
        let limit = self.limit();
        let required = self.necessary_limit(limit);
        if self
            .input
            .objective
            .roster_bound(&team, totals, required)
            .is_some_and(|upper| upper < required)
        {
            return Ok(());
        }
        let necessary = self.necessary_limit(self.limit());
        let floor = self
            .input
            .objective
            .bonus_floor(necessary)
            .into_iter()
            .chain(self.input.constraints.min_bonus)
            .fold(f64::NEG_INFINITY, f64::max);
        if floor == f64::INFINITY {
            return Ok(());
        }
        let mut options: Vec<_> = team.iter().map(|&i| self.options[i].clone()).collect();
        if self.required_snaps.len() == 5 {
            for list in &mut options {
                list.retain(|snap| *snap >= 0 && self.required_snaps.contains(&(*snap as usize)));
                if list.is_empty() {
                    return Ok(());
                }
            }
        }
        if self.any_snap_bonus && floor.is_finite() {
            let maxima: Vec<_> = options
                .iter()
                .map(|list| {
                    list.iter()
                        .map(|&s| {
                            if s < 0 {
                                0.0
                            } else {
                                self.snap_bonus[s as usize]
                            }
                        })
                        .fold(f64::NEG_INFINITY, f64::max)
                })
                .collect();
            for (slot, list) in options.iter_mut().enumerate() {
                let rest = member_bonus
                    + maxima
                        .iter()
                        .enumerate()
                        .filter(|&(i, _)| i != slot)
                        .map(|(_, value)| value)
                        .sum::<f64>();
                list.retain(|&s| {
                    rest + if s < 0 {
                        0.0
                    } else {
                        self.snap_bonus[s as usize]
                    } >= floor - 1e-6
                });
                if list.is_empty() {
                    return Ok(());
                }
            }
        }
        if self
            .input
            .objective
            .reused_assignment_bound(&team, &options, base_power.iter().sum(), required)
            .is_some_and(|upper| upper < limit || self.cannot_improve(upper))
        {
            return Ok(());
        }
        self.input.objective.prepare_assignment(&team, &options);
        let mut used = vec![false; self.input.snaps.len()];
        for (&member, &snap) in &self.input.constraints.bindings {
            if snap >= 0 && !team.contains(&member) {
                used[snap as usize] = true;
            }
        }
        let maxima = |values: &[f64]| {
            team.iter()
                .zip(&options)
                .map(|(&member, list)| {
                    list.iter()
                        .map(|&s| {
                            values[member * self.width
                                + if s < 0 { self.width - 1 } else { s as usize }]
                        })
                        .fold(f64::NEG_INFINITY, f64::max)
                })
                .collect()
        };
        let max_power = maxima(&self.snap_power);
        let max_weight = maxima(&self.skill);
        let max_bonus = options
            .iter()
            .map(|list| {
                list.iter()
                    .map(|&s| {
                        if s < 0 {
                            0.0
                        } else {
                            self.snap_bonus[s as usize]
                        }
                    })
                    .fold(f64::NEG_INFINITY, f64::max)
            })
            .collect();
        let allowed: Vec<Vec<bool>> = options
            .iter()
            .map(|list| {
                let mut mask = vec![false; self.width];
                for &snap in list {
                    if snap >= 0 {
                        mask[snap as usize] = true;
                    }
                }
                mask
            })
            .collect();
        let mut required_snaps: Vec<_> = self.required_snaps.iter().copied().collect();
        required_snaps.sort_unstable();
        if self.any_snap_bonus && floor.is_finite() {
            let best: Vec<_> = self
                .bonus_order
                .iter()
                .copied()
                .filter(|&snap| allowed.iter().any(|row: &Vec<bool>| row[snap]))
                .take(6)
                .collect();
            for &snap in best.iter().take(5) {
                let without = member_bonus
                    + best
                        .iter()
                        .copied()
                        .filter(|&other| other != snap)
                        .take(5)
                        .map(|other| self.snap_bonus[other])
                        .sum::<f64>();
                if without < floor - 1e-6 && !required_snaps.contains(&snap) {
                    required_snaps.push(snap);
                }
            }
        }
        let mut a = Assignment {
            team,
            set_key,
            base_power,
            options,
            allowed,
            required_snaps,
            used,
            assigned: vec![-1; 5],
            max_power,
            max_weight,
            power_order: Vec::new(),
            skill_order: Vec::new(),
            max_bonus,
            set_low: f64::NEG_INFINITY,
            exact_low,
        };
        let base_total = a.base_power.iter().fold(0.0, |sum, v| sum + v);
        self.input.objective.prepare_assignment_power(base_total);
        if self
            .input
            .constraints
            .max_bonus
            .is_some_and(|b| member_bonus > b)
        {
            return Ok(());
        }
        if let Some(minimum) = self.input.constraints.min_bonus {
            let bonus = if self.any_snap_bonus {
                let mut table = std::mem::take(&mut self.pools[0]);
                self.tables(&mut table, &a, 0);
                let bonus = self.matching.maximum(&table.bonus, table.columns, None);
                self.pools[0] = table;
                bonus
            } else {
                0.0
            };
            if member_bonus + bonus < minimum {
                return Ok(());
            }
        }
        let upper = self.remaining_bound(
            &a,
            0,
            Totals {
                power: base_total,
                bonus: member_bonus,
                ..Totals::default()
            },
        );
        if upper < self.assignment_limit(&a) || self.cannot_improve_set(&a, upper) {
            return Ok(());
        }
        if self.input.objective.linear_assignment().is_some()
            && (a.required_snaps.is_empty()
                || a.required_snaps.len() == 5
                    && a.options.iter().all(|choices| {
                        choices
                            .iter()
                            .all(|&snap| snap >= 0 && a.required_snaps.contains(&(snap as usize)))
                    }))
            && (!self.any_snap_bonus
                || self.input.constraints.min_bonus.is_none()
                    && self.input.constraints.max_bonus.is_none())
        {
            let mut table = std::mem::take(&mut self.pools[0]);
            self.tables(&mut table, &a, 0);
            let (p, s, b) = self.input.objective.linear_assignment().unwrap();
            for row in 0..table.power.len() {
                for col in 0..table.columns {
                    table.blended[row][col] = if table.power[row][col] <= NONE / 2.0 {
                        NONE
                    } else {
                        p * table.power[row][col]
                            + s * table.weight[row][col]
                            + b * table.bonus[row][col]
                    };
                }
            }
            let mut columns = vec![0; 5];
            let upper = self
                .matching
                .maximum(&table.blended, table.columns, Some(&mut columns));
            let picks: Vec<_> = columns
                .iter()
                .map(|&c| {
                    if c < table.count {
                        table.column_snap[c] as i32
                    } else {
                        -1
                    }
                })
                .collect();
            self.pools[0] = table;
            if upper > NONE / 2.0 {
                let mut totals = Totals {
                    power: base_total,
                    bonus: member_bonus,
                    ..Totals::default()
                };
                for (slot, &member) in a.team.iter().enumerate() {
                    let col = if picks[slot] < 0 {
                        self.width - 1
                    } else {
                        picks[slot] as usize
                    };
                    totals.power += self.snap_power[member * self.width + col];
                    totals.skill += self.skill[member * self.width + col];
                    totals.skill_low += self.skill_low[member * self.width + col];
                    totals.bonus += if picks[slot] < 0 {
                        0.0
                    } else {
                        self.snap_bonus[picks[slot] as usize]
                    };
                }
                self.candidate(&mut a, picks, totals)?;
                return Ok(());
            }
        }
        if let Some(base) = self.input.objective.product_base() {
            if a.required_snaps.is_empty() {
                let mut t = std::mem::take(&mut self.pools[0]);
                self.tables(&mut t, &a, 0);
                let p = self.matching.maximum(&t.power, t.columns, None);
                let s = self.matching.maximum(&t.weight, t.columns, None);
                let lambda = ((base + s) / (base_total + p).max(1e-9)).sqrt();
                let mut columns = vec![0; 5];
                let incumbent = if lambda.is_finite() && lambda > 0.0 {
                    Self::blend(&mut t, lambda);
                    self.matching
                        .maximum(&t.blended, t.columns, Some(&mut columns))
                } else {
                    f64::NEG_INFINITY
                };
                let picks: Vec<_> = columns
                    .iter()
                    .map(|&c| {
                        if c < t.count {
                            t.column_snap[c] as i32
                        } else {
                            -1
                        }
                    })
                    .collect();
                self.pools[0] = t;
                if incumbent > NONE / 2.0 {
                    let mut totals = Totals {
                        power: base_total,
                        bonus: member_bonus,
                        ..Totals::default()
                    };
                    for (slot, &i) in a.team.iter().enumerate() {
                        let j = picks[slot];
                        let col = if j < 0 { self.width - 1 } else { j as usize };
                        totals.power += self.snap_power[i * self.width + col];
                        totals.skill += self.skill[i * self.width + col];
                        totals.skill_low += self.skill_low[i * self.width + col];
                        totals.bonus += if j < 0 {
                            0.0
                        } else {
                            self.snap_bonus[j as usize]
                        };
                    }
                    if self
                        .input
                        .constraints
                        .min_bonus
                        .is_none_or(|b| totals.bonus >= b)
                        && self
                            .input
                            .constraints
                            .max_bonus
                            .is_none_or(|b| totals.bonus <= b)
                    {
                        self.candidate(&mut a, picks, totals)?;
                    }
                }
            }
        }
        let totals = Totals {
            power: base_total,
            bonus: member_bonus,
            ..Totals::default()
        };
        if let Some(domains) = self.input.objective.assignment_domains(&a.team, &a.options) {
            return self.snap_domains(&mut a, &domains, 0, &mut Vec::new(), totals);
        }
        self.walk_domain(&mut a, totals)
    }
    fn snap_domains(
        &mut self,
        a: &mut Assignment,
        domains: &[Vec<Vec<i32>>],
        slot: usize,
        selected: &mut Vec<Vec<i32>>,
        totals: Totals,
    ) -> Result<(), String> {
        if self.expired() {
            self.timed_out = true;
            self.unexplored = f64::INFINITY;
            return Ok(());
        }
        if slot > 0 {
            self.stats.snap_nodes += 1;
            if self.stats.snap_nodes & 1023 == 0 {
                self.report_pending();
            }
            let fixed = selected
                .iter()
                .filter_map(|domain| (domain.len() == 1 && domain[0] >= 0).then_some(domain[0]))
                .collect::<Vec<_>>();
            let mut options = selected.clone();
            options.extend(
                domains[slot..]
                    .iter()
                    .map(|classes| classes.iter().flatten().copied().collect()),
            );
            for choices in &mut options {
                if choices.len() > 1 {
                    choices.retain(|snap| {
                        *snap < 0 || !a.used[*snap as usize] && !fixed.contains(snap)
                    });
                }
                if choices.is_empty() {
                    return Ok(());
                }
            }
            if a.required_snaps
                .iter()
                .any(|&snap| !options.iter().any(|row| row.contains(&(snap as i32))))
            {
                return Ok(());
            }
            a.options = options;
            a.allowed = a
                .options
                .iter()
                .map(|list| {
                    let mut row = vec![false; self.width];
                    for &snap in list {
                        if snap >= 0 {
                            row[snap as usize] = true;
                        }
                    }
                    row
                })
                .collect();
            a.max_power = a
                .team
                .iter()
                .zip(&a.options)
                .map(|(&member, choices)| {
                    choices
                        .iter()
                        .map(|&snap| {
                            self.snap_power[member * self.width
                                + if snap < 0 {
                                    self.width - 1
                                } else {
                                    snap as usize
                                }]
                        })
                        .fold(f64::NEG_INFINITY, f64::max)
                })
                .collect();
            a.max_weight = a
                .team
                .iter()
                .zip(&a.options)
                .map(|(&member, choices)| {
                    choices
                        .iter()
                        .map(|&snap| {
                            self.skill[member * self.width
                                + if snap < 0 {
                                    self.width - 1
                                } else {
                                    snap as usize
                                }]
                        })
                        .fold(f64::NEG_INFINITY, f64::max)
                })
                .collect();
            a.max_bonus = a
                .options
                .iter()
                .map(|choices| {
                    choices
                        .iter()
                        .map(|&snap| {
                            if snap < 0 {
                                0.0
                            } else {
                                self.snap_bonus[snap as usize]
                            }
                        })
                        .fold(f64::NEG_INFINITY, f64::max)
                })
                .collect();
            a.assigned.fill(-1);
            self.input.objective.prepare_assignment_domain(
                &a.team,
                &a.options,
                slot == domains.len(),
            );
            let order_limit = self.assignment_necessary_limit(a);
            let bound = self.input.objective.assignment_order_bound(
                &a.team,
                Totals {
                    power: a
                        .max_power
                        .iter()
                        .fold(totals.power, |sum, &value| (sum + value).next_up()),
                    skill: a
                        .max_weight
                        .iter()
                        .fold(totals.skill, |sum, &value| (sum + value).next_up()),
                    skill_low: 0.0,
                    bonus: a
                        .max_bonus
                        .iter()
                        .fold(0.0, |sum, &value| (sum + value).next_up())
                        .min(self.snap_bonus_top5.next_up())
                        + totals.bonus,
                },
                &a.assigned,
                0,
                order_limit,
            );
            if bound.is_some_and(|upper| {
                upper < self.assignment_limit(a) || self.cannot_improve_set(a, upper)
            }) {
                return Ok(());
            }
            if slot == domains.len() {
                let upper = self.remaining_bound(a, 0, totals);
                if upper < self.assignment_limit(a) || self.cannot_improve_set(a, upper) {
                    return Ok(());
                }
                return self.walk_domain(a, totals);
            }
        }
        if slot < domains.len() {
            for domain in &domains[slot] {
                if domain.len() == 1
                    && domain[0] >= 0
                    && (a.used[domain[0] as usize]
                        || selected
                            .iter()
                            .any(|old| old.len() == 1 && old[0] == domain[0]))
                {
                    continue;
                }
                selected.push(domain.clone());
                self.snap_domains(a, domains, slot + 1, selected, totals)?;
                selected.pop();
            }
        }
        Ok(())
    }
    fn walk_domain(&mut self, a: &mut Assignment, totals: Totals) -> Result<(), String> {
        let ordered = |values: &[f64]| {
            a.team
                .iter()
                .zip(&a.options)
                .map(|(&member, list)| {
                    let mut list = list.clone();
                    list.sort_unstable_by(|&x, &y| {
                        let index = |snap: i32| {
                            member * self.width
                                + if snap < 0 {
                                    self.width - 1
                                } else {
                                    snap as usize
                                }
                        };
                        values[index(y)].total_cmp(&values[index(x)])
                    });
                    list
                })
                .collect()
        };
        a.power_order = ordered(&self.snap_power);
        a.skill_order = ordered(&self.skill);
        self.snap_walk(a, 0, totals)
    }
    fn snap_walk(&mut self, a: &mut Assignment, slot: usize, totals: Totals) -> Result<(), String> {
        if !self.timed_out {
            self.stats.snap_nodes += 1;
            if self.stats.snap_nodes & 1023 == 0 {
                self.report_pending();
                if self.expired() {
                    self.timed_out = true;
                }
            }
        }
        if self.timed_out {
            let bound = self.assignment_bound(
                a,
                Totals {
                    power: totals.power + suffix(&a.max_power, slot),
                    skill: totals.skill + suffix(&a.max_weight, slot),
                    skill_low: 0.0,
                    bonus: totals.bonus + suffix(&a.max_bonus, slot),
                },
                0,
            );
            self.unexplored = self.unexplored.max(bound);
            return Ok(());
        }
        let required_left = a.required_snaps.iter().filter(|&&j| !a.used[j]).count();
        if slot == 5 {
            if required_left > 0
                || self
                    .input
                    .constraints
                    .min_bonus
                    .is_some_and(|b| totals.bonus < b)
                || self
                    .input
                    .constraints
                    .max_bonus
                    .is_some_and(|b| totals.bonus > b)
                || self.assignment_bound(a, totals, 5) < self.assignment_limit(a)
            {
                return Ok(());
            }
            return self.candidate(a, a.assigned.clone(), totals);
        }
        let mut count = 5 - slot;
        let mut unused_bonus = 0.0;
        for &j in &self.bonus_order {
            if count == 0 {
                break;
            }
            if a.used[j] {
                continue;
            }
            unused_bonus += self.snap_bonus[j];
            count -= 1;
        }
        let mut power = totals.power;
        let mut skill = totals.skill;
        for at in slot..5 {
            let available = |order: &[i32]| {
                order
                    .iter()
                    .copied()
                    .find(|&s| s < 0 || !a.used[s as usize])
            };
            let Some(p) = available(&a.power_order[at]) else {
                return Ok(());
            };
            let Some(s) = available(&a.skill_order[at]) else {
                return Ok(());
            };
            let index = |snap: i32| {
                a.team[at] * self.width
                    + if snap < 0 {
                        self.width - 1
                    } else {
                        snap as usize
                    }
            };
            power = (power + self.snap_power[index(p)]).next_up();
            skill = (skill + self.skill[index(s)]).next_up();
        }
        let optimistic = self.input.objective.cheap_assignment_bound(
            &a.team,
            Totals {
                power,
                skill,
                skill_low: 0.0,
                bonus: totals.bonus + suffix(&a.max_bonus, slot).min(unused_bonus),
            },
            &a.assigned,
            slot,
        );
        if optimistic < self.assignment_limit(a)
            || self.cannot_improve_set(a, optimistic)
            || (slot > 0
                && slot < 5
                && self.remaining_bound(a, slot, totals) < self.assignment_limit(a))
            || 5 - slot < required_left
        {
            return Ok(());
        }
        let floor = if self.any_snap_bonus {
            let limit = self.assignment_necessary_limit(a);
            self.input.objective.bonus_floor(limit)
        } else {
            None
        };
        let mut rest = 0.0;
        if floor.is_some() {
            let mut count = 4 - slot;
            for &j in &self.bonus_order {
                if count == 0 {
                    break;
                }
                if !a.used[j] {
                    rest += self.snap_bonus[j];
                    count -= 1;
                }
            }
        }
        let member = a.team[slot];
        let mut options = std::mem::take(&mut self.option_scratch[slot]);
        if let Some(selected) = self.input.objective.assignment_options(
            &a.team,
            &a.assigned,
            slot,
            self.assignment_limit(a),
        ) {
            options = selected;
        } else {
            options.clone_from(&a.options[slot]);
        }
        if self.conditional_dominance {
            let remaining = 5 - slot;
            options.retain(|&snap| {
                if snap < 0 || a.required_snaps.contains(&(snap as usize)) {
                    return true;
                }
                let dominators = &self.dominators[member][snap as usize];
                let available = |other: usize| !a.used[other] && a.allowed[slot][other];
                if dominators
                    .iter()
                    .filter(|&&other| available(other))
                    .take(remaining)
                    .count()
                    >= remaining
                {
                    return false;
                }
                for &other in dominators {
                    if !available(other) {
                        continue;
                    }
                    let delta = self.snap_power[member * self.width + other]
                        - self.snap_power[member * self.width + snap as usize];
                    let mut interchangeable = true;
                    for next in slot..5 {
                        if !a.allowed[next][other] {
                            continue;
                        }
                        let owner = a.team[next];
                        if !a.allowed[next][snap as usize]
                            || self.snap_power[owner * self.width + other]
                                - self.snap_power[owner * self.width + snap as usize]
                                > delta
                        {
                            interchangeable = false;
                            break;
                        }
                        let key = (owner, other, snap as usize);
                        let equivalent = if let Some(&answer) = self.exchange_equivalent.get(&key) {
                            answer
                        } else {
                            let answer = self.input.objective.exchange_snap_equivalent(
                                owner,
                                other as i32,
                                snap,
                            );
                            self.exchange_equivalent.insert(key, answer);
                            answer
                        };
                        if !equivalent {
                            interchangeable = false;
                            break;
                        }
                    }
                    if interchangeable {
                        return false;
                    }
                }
                true
            });
        }

        for &j in &options {
            if required_left == 5 - slot && (j < 0 || !a.required_snaps.contains(&(j as usize))) {
                continue;
            }
            if j >= 0 && a.used[j as usize] {
                continue;
            }
            if j >= 0
                && self.conditional_dominance
                && !self.input.disable_snap_dominance
                && !self.input.objective.preserve_snap_options()
            {
                let mut swapped = false;
                for previous in 0..slot {
                    let other = a.assigned[previous];
                    if other < 0
                        || !a.allowed[previous][j as usize]
                        || !a.allowed[slot][other as usize]
                    {
                        continue;
                    }
                    let owner = a.team[previous];
                    let delta = self.snap_power[owner * self.width + j as usize]
                        + self.snap_power[member * self.width + other as usize]
                        - self.snap_power[owner * self.width + other as usize]
                        - self.snap_power[member * self.width + j as usize];
                    if delta < 0.0 || delta == 0.0 && other < j {
                        continue;
                    }
                    let mut equivalent = true;
                    for who in [owner, member] {
                        let key = (who, j as usize, other as usize);
                        let same = if let Some(&value) = self.exchange_equivalent.get(&key) {
                            value
                        } else {
                            let value =
                                self.input.objective.exchange_snap_equivalent(who, j, other);
                            self.exchange_equivalent.insert(key, value);
                            value
                        };
                        if !same {
                            equivalent = false;
                            break;
                        }
                    }
                    if equivalent {
                        swapped = true;
                        break;
                    }
                }
                if swapped {
                    continue;
                }
            }
            if let Some(floor) = floor {
                let own = if j < 0 {
                    0.0
                } else {
                    self.snap_bonus[j as usize]
                };
                if totals.bonus + own + rest < floor - 1e-6 {
                    continue;
                }
            }
            let col = if j < 0 { self.width - 1 } else { j as usize };
            if j >= 0 {
                a.used[j as usize] = true;
            }
            a.assigned[slot] = j;
            self.snap_walk(
                a,
                slot + 1,
                Totals {
                    power: totals.power + self.snap_power[member * self.width + col],
                    skill: totals.skill + self.skill[member * self.width + col],
                    skill_low: totals.skill_low + self.skill_low[member * self.width + col],
                    bonus: totals.bonus
                        + if j < 0 {
                            0.0
                        } else {
                            self.snap_bonus[j as usize]
                        },
                },
            )?;
            if j >= 0 {
                a.used[j as usize] = false;
            }
        }
        self.option_scratch[slot] = options;
        a.assigned[slot] = -1;
        Ok(())
    }
    fn compact(&mut self) {
        let threshold = self.limit();
        let mut lows: HashMap<String, f64> = HashMap::default();
        for c in &self.candidates {
            lows.entry(c.set_key.clone())
                .and_modify(|v| *v = v.max(c.low))
                .or_insert(c.low);
        }
        self.candidates
            .retain(|c| c.high >= threshold && c.high >= lows[&c.set_key]);
        self.compact_at = 4096.max(self.candidates.len() * 2);
    }
    /// Drops every snap, and the empty slot, that cannot be part of a team whose bonus reaches `floor`, even with
    /// the best members and the best other snaps, and tightens each member's best snap to what remains.
    fn restrict_snaps(&mut self, floor: f64) {
        if floor <= self.snap_floor + 1e-6 || floor.is_nan() {
            return;
        }
        self.snap_floor = floor;
        let mut members: Vec<f64> = (0..self.member_bonus.len())
            .filter(|i| !self.excluded_members.contains(i))
            .map(|i| self.member_bonus[i])
            .collect();
        members.sort_by(|a, b| b.total_cmp(a));
        let member_top: f64 = members.iter().take(5).sum();
        let order = self.bonus_order.clone();
        let snap_bonus = &self.snap_bonus;
        let top_without = |skip: Option<usize>, count: usize| -> f64 {
            order
                .iter()
                .filter(|&&j| Some(j) != skip)
                .take(count)
                .map(|&j| snap_bonus[j])
                .sum()
        };
        let empty_ok = top_without(None, 4) + member_top >= floor - 1e-6;
        let fits = |j: i32| {
            if j < 0 {
                empty_ok
            } else {
                let j = j as usize;
                self.required_snaps.contains(&j)
                    || snap_bonus[j] + top_without(Some(j), 4) + member_top >= floor - 1e-6
            }
        };
        let width = self.width;
        for i in 0..self.options.len() {
            if self.input.constraints.bindings.contains_key(&i) {
                continue;
            }
            let kept: Vec<i32> = self.options[i]
                .iter()
                .copied()
                .filter(|&j| fits(j))
                .collect();
            if kept.len() == self.options[i].len() {
                continue;
            }
            let mut best = Best::default();
            for &j in &kept {
                let col = if j < 0 { width - 1 } else { j as usize };
                best.power = best.power.max(self.snap_power[i * width + col]);
                best.weight = best.weight.max(self.skill[i * width + col]);
                best.bonus = best
                    .bonus
                    .max(if j < 0 { 0.0 } else { snap_bonus[j as usize] });
            }
            if kept.is_empty() {
                best = Best {
                    power: NONE,
                    weight: NONE,
                    bonus: NONE,
                };
            }
            self.best[i] = best;
            self.options[i] = kept;
        }
        self.bonus_order.retain(|&j| fits(j as i32));
        self.snap_bonus_top5 = self
            .bonus_order
            .iter()
            .take(5)
            .map(|&j| self.snap_bonus[j])
            .sum();
    }
    fn run(mut self) -> Result<Output, String> {
        let n = self.input.members.len();
        let mut leaders: Vec<_> = (0..n)
            .filter(|i| {
                !self.excluded_members.contains(i)
                    && self.input.constraints.leader.is_none_or(|l| l == *i as i32)
            })
            .collect();
        leaders.sort_by(|&a, &b| {
            let hint =
                self.input.objective.prefer_member(b) - self.input.objective.prefer_member(a);
            if hint != 0.0 {
                hint.total_cmp(&0.0)
            } else {
                (self.bases[b].fixed_points as f64 + self.best[b].power)
                    .total_cmp(&(self.bases[a].fixed_points as f64 + self.best[a].power))
            }
        });
        let mut leader_group = vec![usize::MAX; n];
        let mut leader_rank = vec![usize::MAX; n];
        if self.input.objective.canonical_leader() {
            let mut groups = HashMap::default();
            for (rank, &leader) in leaders.iter().enumerate() {
                let profile = self.profile(leader);
                let identity = power::leader_identity(
                    self.input.master,
                    &profile,
                    &self.input.members[leader],
                );
                let next = groups.len();
                leader_group[leader] = *groups.entry(identity).or_insert(next);
                leader_rank[leader] = rank;
            }
        }
        if let Some((index, count)) = self.input.shard {
            if count > 1 {
                leaders = leaders
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| i % count == index)
                    .map(|(_, leader)| leader)
                    .collect();
            }
        }
        if leaders.is_empty() {
            return Ok(Output {
                hits: Vec::new(),
                proven: true,
                bound: None,
                stats: self.stats,
            });
        }
        let mut seen = HashSet::default();
        for seed in self.input.seeds.clone() {
            if self.expired() {
                break;
            }
            if self.seed_valid(&seed) {
                let key = format!("{:?}|{:?}", seed.members, seed.snaps);
                if seen.insert(key) {
                    self.evaluate_seed(seed)?;
                }
            }
        }
        let owned_leaders = self
            .input
            .shard
            .filter(|(_, count)| *count > 1)
            .map(|_| leaders.as_slice());
        self.warm_seed(owned_leaders)?;
        for (position, &leader) in leaders.iter().enumerate() {
            if let Some(callback) = self.input.progress.as_deref_mut() {
                callback(position, leaders.len());
            }
            if self.expired() {
                self.timed_out = true;
            }
            self.stats.leaders += 1;
            if self.any_snap_bonus {
                let limit = self.necessary_limit(self.limit());
                if let Some(floor) = self.input.objective.bonus_floor(limit) {
                    if floor == f64::INFINITY {
                        break;
                    }
                    self.restrict_snaps(floor);
                }
            }
            let profile = self.profile(leader);
            let character = self.input.members[leader].card.character_id;
            let equivalent = |member: usize| {
                leader_group[leader] != usize::MAX
                    && leader_group[member] == leader_group[leader]
                    && leader_rank[member] < leader_rank[leader]
            };
            if self
                .required
                .iter()
                .any(|&member| member < n && equivalent(member))
            {
                continue;
            }

            if self.required.iter().any(|&i| {
                i != leader && (i >= n || self.input.members[i].card.character_id == character)
            }) {
                continue;
            }
            let slot_power: Vec<_> = (0..n)
                .map(|i| {
                    self.bases[i].fixed_points as f64
                        + power::leader_points(
                            &self.bases[i],
                            &power::leader_percent_bound(
                                self.input.master,
                                &profile,
                                &self.input.members[i],
                            ),
                        ) as f64
                })
                .collect();
            let mut groups: Vec<(i32, Vec<usize>)> = Vec::new();
            for i in 0..n {
                if i == leader
                    || equivalent(i)
                    || self.excluded_members.contains(&i)
                    || self.input.members[i].card.character_id == character
                {
                    continue;
                }
                let ch = self.input.members[i].card.character_id;
                if let Some((_, list)) = groups.iter_mut().find(|(c, _)| *c == ch) {
                    list.push(i);
                } else {
                    groups.push((ch, vec![i]));
                }
            }
            if self.input.objective.needs_power_cap()
                && !self.input.objective.preserve_snap_options()
            {
                for (_, list) in &mut groups {
                    let original = list.clone();
                    list.retain(|&worse| {
                        if self.required.contains(&worse)
                            || self.input.constraints.bindings.contains_key(&worse)
                        {
                            return true;
                        }
                        let mut witnesses = 0;
                        for &better in &original {
                            if better == worse
                                || self.input.constraints.bindings.contains_key(&better)
                                || self.member_bonus[better] < self.member_bonus[worse]
                                || self.input.constraints.max_bonus.is_some()
                                    && self.member_bonus[better] != self.member_bonus[worse]
                                || self
                                    .input
                                    .constraints
                                    .member_floor
                                    .as_ref()
                                    .is_some_and(|f| f.values[better] < f.values[worse])
                            {
                                continue;
                            }
                            let same_deck = profile.simple
                                || power::leader_replacement_equivalent(
                                    self.input.master,
                                    &profile,
                                    &self.input.members[better],
                                    &self.input.members[worse],
                                );
                            if !same_deck {
                                continue;
                            }
                            let base_delta = if profile.simple {
                                slot_power[better] - slot_power[worse]
                            } else {
                                if self.bases[better]
                                    .common
                                    .iter()
                                    .zip(self.bases[worse].common)
                                    .any(|(&a, b)| a < b)
                                    || profile.effects.iter().any(|e| e.value < 0)
                                {
                                    continue;
                                }
                                (self.bases[better].fixed_points - self.bases[worse].fixed_points)
                                    as f64
                            };
                            let snap_delta = (0..self.width)
                                .map(|snap| {
                                    self.snap_power[better * self.width + snap]
                                        - self.snap_power[worse * self.width + snap]
                                })
                                .fold(f64::INFINITY, f64::min);
                            if self.input.objective.numeric_member_dominance(
                                better,
                                worse,
                                base_delta + snap_delta,
                            ) {
                                witnesses += 1;
                                if witnesses >= self.input.k {
                                    return false;
                                }
                            }
                        }
                        true
                    });
                }
            }
            let (mut forced, mut free) = (Vec::new(), Vec::new());
            let mut valid = true;
            for (_, list) in groups {
                let must: Vec<_> = list
                    .iter()
                    .copied()
                    .filter(|i| self.required.contains(i))
                    .collect();
                if must.len() > 1 {
                    valid = false;
                    break;
                }
                let required = !must.is_empty();
                let mut list = if required { must } else { list };
                list.sort_by(|&a, &b| {
                    let hint = self.input.objective.prefer_member(b)
                        - self.input.objective.prefer_member(a);
                    if hint != 0.0 {
                        hint.total_cmp(&0.0)
                    } else {
                        (slot_power[b] + self.best[b].power)
                            .total_cmp(&(slot_power[a] + self.best[a].power))
                    }
                });
                if required {
                    forced.push(list);
                } else {
                    free.push(list);
                }
            }
            if !valid || forced.len() > 4 {
                continue;
            }
            free.sort_by(|a, b| {
                let hint = self.input.objective.prefer_member(b[0])
                    - self.input.objective.prefer_member(a[0]);
                if hint != 0.0 {
                    hint.total_cmp(&0.0)
                } else {
                    (slot_power[b[0]] + self.best[b[0]].power)
                        .total_cmp(&(slot_power[a[0]] + self.best[a[0]].power))
                }
            });
            let forced_count = forced.len();
            let characters: Vec<_> = forced.into_iter().chain(free).collect();
            if characters.len() < 4 {
                continue;
            }
            let char_power = suffix_tops(
                &characters
                    .iter()
                    .map(|list| {
                        list.iter()
                            .map(|&i| slot_power[i] + self.best[i].power)
                            .fold(f64::NEG_INFINITY, f64::max)
                    })
                    .collect::<Vec<_>>(),
            );
            let char_skill = suffix_tops(
                &characters
                    .iter()
                    .map(|list| {
                        list.iter()
                            .map(|&i| self.best[i].weight)
                            .fold(f64::NEG_INFINITY, f64::max)
                    })
                    .collect::<Vec<_>>(),
            );
            let char_bonus = suffix_tops(
                &characters
                    .iter()
                    .map(|list| {
                        list.iter()
                            .map(|&i| self.member_bonus[i] + self.best[i].bonus)
                            .fold(f64::NEG_INFINITY, f64::max)
                    })
                    .collect::<Vec<_>>(),
            );
            let char_member_bonus = suffix_tops(
                &characters
                    .iter()
                    .map(|list| {
                        list.iter()
                            .map(|&i| self.member_bonus[i])
                            .fold(f64::NEG_INFINITY, f64::max)
                    })
                    .collect::<Vec<_>>(),
            );
            self.input
                .objective
                .prepare_member_domains_at(leader, &slot_power, &self.options);
            self.input.objective.prepare_members(&characters);
            let leader_totals = Totals {
                power: slot_power[leader] + self.best[leader].power,
                skill: self.best[leader].weight,
                skill_low: 0.0,
                bonus: self.member_bonus[leader] + self.best[leader].bonus,
            };
            let bound = self.member_set_bound(
                &[leader],
                Totals {
                    power: leader_totals.power + top_sum(&char_power, 0, 4),
                    skill: leader_totals.skill + top_sum(&char_skill, 0, 4),
                    skill_low: 0.0,
                    bonus: (leader_totals.bonus + top_sum(&char_bonus, 0, 4)).min(
                        self.member_bonus[leader]
                            + top_sum(&char_member_bonus, 0, 4)
                            + self.snap_bonus_top5,
                    ),
                },
                4,
                0,
            );
            if bound < self.limit() || self.cannot_improve(bound) {
                continue;
            }
            if self.timed_out {
                self.unexplored = self.unexplored.max(bound);
                continue;
            }
            if self
                .input
                .constraints
                .min_bonus
                .is_some_and(|b| leader_totals.bonus + top_sum(&char_bonus, 0, 4) < b)
            {
                continue;
            }
            let cuts = self.member_cuts(
                leader,
                &slot_power,
                &characters,
                leader_totals.power + top_sum(&char_power, 0, 4),
                leader_totals.skill + top_sum(&char_skill, 0, 4),
                leader_totals.bonus + top_sum(&char_bonus, 0, 4),
            );
            let mut cut_sums = [[0.0; 12]; 5];
            let mut column_sums = [[0.0; 12]; 5];
            for (index, cut) in cuts.iter().enumerate() {
                cut_sums[0][index] = (cut.constant + cut.members[leader]).next_up();
                if let Some(column) = &cut.column {
                    column_sums[0][index] = (column.constant + column.members[leader]).next_up();
                }
            }
            let bonus_bounds = self.member_bonus_bounds(&slot_power, &characters, &cuts);
            let mut members = Members {
                leader,
                characters,
                forced: forced_count,
                slot_power,
                char_power,
                char_skill,
                char_bonus,
                char_member_bonus,
                chosen: Vec::with_capacity(4),
                cuts,
                cut_sums,
                column_sums,
                bonus_bounds,
            };
            self.member_visit(
                &mut members,
                0,
                leader_totals.power,
                leader_totals.skill,
                leader_totals.bonus,
                self.member_bonus[leader],
            )?;
        }
        if !self.input.streaming {
            let threshold = self.limit();
            let mut pool = std::mem::take(&mut self.candidates);
            pool.retain(|c| c.high >= threshold);
            pool.sort_by(|a, b| b.high.total_cmp(&a.high));
            let mut known: HashMap<String, f64> = HashMap::default();
            for c in pool {
                let kth = if self.hits.len() < self.input.k {
                    f64::NEG_INFINITY
                } else {
                    self.hits[self.input.k - 1].key
                };
                if c.high < kth {
                    break;
                }
                if self.expired() && self.hits.len() >= self.input.k {
                    self.timed_out = true;
                    self.unexplored = self.unexplored.max(c.high);
                    break;
                }
                if known.get(&c.set_key).is_some_and(|&v| {
                    v > c.high || !self.input.objective.score_ties() && v == c.high
                }) {
                    continue;
                }
                self.stats.exact += 1;
                let exact = self
                    .input
                    .objective
                    .exact(&c.team, c.power, &c.slot_powers)?;
                if known.get(&c.set_key).is_some_and(|&v| {
                    v > exact.key || !self.input.objective.score_ties() && v == exact.key
                }) {
                    continue;
                }
                known.insert(c.set_key.clone(), exact.key);
                let existing = self.hit_keys.iter().position(|key| key == &c.set_key);
                let mut hit = Hit {
                    team: c.team,
                    power: c.power,
                    slot_powers: c.slot_powers,
                    totals: c.totals,
                    key: exact.key,
                    detail: exact.detail,
                };
                if let Some(index) = existing {
                    if !self.compare_hit(&mut hit, index)?.is_gt() {
                        continue;
                    }
                    self.hits.remove(index);
                    self.hit_keys.remove(index);
                }
                let i = self.insert_position(&mut hit)?;
                self.hits.insert(i, hit);
                self.hit_keys.insert(i, c.set_key);
            }
            self.hits.truncate(self.input.k);
            self.hit_keys.truncate(self.input.k);
        }
        self.stats.elapsed_ms = (self.input.now)() - self.started;
        Ok(Output {
            hits: self.hits,
            proven: !self.timed_out,
            bound: if self.timed_out {
                Some(self.unexplored)
            } else {
                None
            },
            stats: self.stats,
        })
    }
}
pub fn search<O: Objective + ?Sized>(input: Input<'_, O>) -> Result<Output, String> {
    search_at(input, crate::cancellation_generation())
}
fn feasible<O: Objective + ?Sized>(input: &Input<'_, O>) -> bool {
    let c = &input.constraints;
    if c.leader
        .is_some_and(|i| i < 0 || i as usize >= input.members.len())
    {
        return false;
    }
    let required: HashSet<_> = c
        .required_members
        .iter()
        .copied()
        .chain(c.bindings.keys().copied())
        .chain(c.leader.map(|i| i as usize))
        .collect();
    if required.len() > 5
        || required
            .iter()
            .any(|&i| i >= input.members.len() || c.excluded_members.contains(&i))
        || required
            .iter()
            .map(|&i| input.members[i].card.character_id)
            .collect::<HashSet<_>>()
            .len()
            != required.len()
        || input
            .members
            .iter()
            .enumerate()
            .filter(|(i, _)| !c.excluded_members.contains(i))
            .map(|(_, m)| m.card.character_id)
            .collect::<HashSet<_>>()
            .len()
            < 5
    {
        return false;
    }
    let mut snaps: HashSet<_> = c.required_snaps.iter().copied().collect();
    let mut bound = HashSet::default();
    let mut empty = 0;
    for &snap in c.bindings.values() {
        if snap == -1 {
            empty += 1;
        } else if snap < -1 || !bound.insert(snap as usize) {
            return false;
        } else {
            snaps.insert(snap as usize);
        }
    }
    snaps.len() <= 5 - empty
        && (!c.no_snaps || snaps.is_empty())
        && snaps
            .iter()
            .all(|&i| i < input.snaps.len() && !c.excluded_snaps.contains(&i))
}
pub(crate) fn search_at<O: Objective + ?Sized>(
    input: Input<'_, O>,
    generation: u64,
) -> Result<Output, String> {
    if generation != crate::cancellation_generation() || (input.now)() >= input.deadline {
        return Ok(Output {
            hits: Vec::new(),
            proven: false,
            bound: None,
            stats: Stats::default(),
        });
    }
    if !feasible(&input) {
        return Ok(Output {
            hits: Vec::new(),
            proven: true,
            bound: None,
            stats: Stats::default(),
        });
    }
    let mut runner = Runner::new(input);
    runner.generation = generation;
    runner.run()
}
