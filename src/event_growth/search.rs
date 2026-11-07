//! Incremental bounds over effective fixed bonuses. Historical witnesses use
//! small integer domains; stable records are searched before widening growth.
use super::profiles::{AxisCache, Constraints, Difference, Generator, Offsets, Relation};
use super::*;
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashSet},
};
pub(super) type MarginalBank = Arc<std::sync::Mutex<BTreeMap<(usize, usize, i32), (i64, i64)>>>;
pub(super) type DeltaBank =
    Arc<std::sync::Mutex<BTreeMap<(Stat3, i32, i32, i32), Arc<Vec<(i64, i64)>>>>>;
type PatternKey = (usize, i64, [(i32, i32); 5]);
pub(super) type KernelBank = Arc<std::sync::Mutex<Vec<Option<Arc<Vec<i64>>>>>>;

#[derive(Clone)]
pub(super) struct Ranked {
    pub configuration: Configuration,
    variation: i32,
    dimensions: usize,
}
#[derive(Clone, Eq, PartialEq)]
pub(super) struct Bound {
    variation: i32,
    dimensions: usize,
    vip: i32,
    instruments: Vec<(i32, i32)>,
}
impl Ord for Bound {
    fn cmp(&self, other: &Self) -> Ordering {
        self.variation
            .cmp(&other.variation)
            .then(self.dimensions.cmp(&other.dimensions))
            .then(other.vip.cmp(&self.vip))
            .then_with(|| {
                self.instruments
                    .iter()
                    .zip(&other.instruments)
                    .map(|(a, b)| b.0.cmp(&a.0).then(b.1.cmp(&a.1)))
                    .find(|order| !order.is_eq())
                    .unwrap_or(Ordering::Equal)
            })
    }
}
impl PartialOrd for Bound {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ranked {
    pub fn bound(&self) -> Bound {
        Bound {
            variation: self.variation,
            dimensions: self.dimensions,
            vip: self.configuration.vip_bonus,
            instruments: self
                .configuration
                .instruments
                .iter()
                .map(|row| (row.min_total, row.total))
                .collect(),
        }
    }
    pub fn compare(&self, other: &Self) -> Ordering {
        self.bound().cmp(&other.bound())
    }
}
#[derive(Clone)]
struct Node {
    lo: Vec<i32>,
    hi: Vec<i32>,
    bound: Bound,
    order: u64,
    shift: i32,
    candidate: Option<Arc<Ranked>>,
    offsets: Option<Arc<Offsets>>,
    flat_search: Option<Box<FlatSearch>>,
    hint: Option<i64>,
    total: Option<Total>,
    sum_range: Option<Range>,
    profile: Option<Box<Generator>>,
}
impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.order == other.order && self.bound == other.bound
    }
}
impl Eq for Node {}
impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        if let (Some(a), Some(b)) = (self.hint, other.hint) {
            return b.cmp(&a).then(self.order.cmp(&other.order));
        }
        other
            .bound
            .cmp(&self.bound)
            .then(self.candidate.is_some().cmp(&other.candidate.is_some()))
            .then(self.flat_search.is_some().cmp(&other.flat_search.is_some()))
            .then(other.order.cmp(&self.order))
    }
}
#[derive(Clone)]
struct Canonical {
    levels: Vec<i32>,
    total: Total,
    range: Range,
}
#[derive(Clone, Copy)]
struct Bounds {
    total: Total,
    min: i32,
    max: i32,
}

pub(super) struct Tier {
    problem: Arc<Problem>,
    pub vip: i32,
    vip_rank: i32,
    minimums: Vec<i32>,
    item_min: i32,
    rank_span: i32,
    item_span: i32,
    relaxed: bool,
    queue: BinaryHeap<Node>,
    kernels: KernelBank,
    deltas: DeltaBank,
    active_offsets: Option<Arc<Offsets>>,
    active_total: Option<Total>,
    pattern_cache: BTreeMap<PatternKey, bool>,
    axis_cache: AxisCache,
    marginals: MarginalBank,
    movement: Vec<i64>,
    global_span: i32,
    seen: HashSet<String>,
    order: u64,
    pub visited: usize,
    pub pruned: usize,
    budget: i32,
    scout: bool,
    pub latest: Vec<Arc<Ranked>>,
}
impl Tier {
    pub fn new(
        problem: Arc<Problem>,
        vip: i32,
        vip_rank: i32,
        relaxed: bool,
        budget: i32,
        scout: bool,
    ) -> Self {
        let minimums = vec![if relaxed { 1 } else { 15 }; problem.caps.len()];
        let kernels = Arc::clone(&problem.kernels[&vip]);
        let mut tier = Self {
            deltas: Arc::clone(&problem.deltas),
            active_offsets: None,
            active_total: None,
            pattern_cache: BTreeMap::new(),
            axis_cache: BTreeMap::new(),
            marginals: Arc::clone(&problem.marginals[&vip]),
            movement: Vec::new(),
            global_span: 0,
            problem,
            vip,
            vip_rank,
            minimums,
            item_min: if relaxed { 0 } else { 50 },
            rank_span: if scout {
                1
            } else {
                (budget / 5).min(super::RANK_SPAN)
            },
            item_span: if scout {
                3
            } else {
                budget.min(super::ITEM_SPAN)
            },
            relaxed,
            queue: BinaryHeap::new(),
            kernels,
            seen: HashSet::new(),
            order: 0,
            visited: 0,
            pruned: 0,
            budget,
            scout,
            latest: Vec::new(),
        };
        if tier
            .problem
            .caps
            .iter()
            .zip(&tier.minimums)
            .any(|(cap, min)| cap < min)
            || tier
                .problem
                .bands
                .iter()
                .any(|band| band.cap < tier.item_min)
            || budget == 0 && tier.problem.conflict
        {
            return tier;
        }
        let minimum_total = tier.minimums.iter().sum::<i32>() + tier.problem.unseen_count;
        let physical_span = tier.problem.unseen_span
            + tier
                .problem
                .caps
                .iter()
                .zip(&tier.minimums)
                .map(|(cap, min)| (cap - min).clamp(0, super::RANK_SPAN))
                .sum::<i32>();
        tier.global_span = tier
            .problem
            .totals
            .iter()
            .filter(|row| row.max >= minimum_total)
            .flat_map(|a| {
                tier.problem
                    .totals
                    .iter()
                    .filter(move |b| {
                        b.max >= minimum_total && b.min.max(minimum_total) - a.max <= physical_span
                    })
                    .map(move |b| b.bonus - a.bonus)
            })
            .max()
            .unwrap_or(0);
        if !scout && budget >= 5 {
            for observation in 0..tier.problem.readings.len() {
                let ranges = tier.budget_ranges(observation);
                tier.movement
                    .push(ranges.iter().flatten().map(|row| row.1).max().unwrap_or(0));
                for other in 0..observation {
                    if tier.problem.readings[other].signature
                        != tier.problem.readings[observation].signature
                    {
                        continue;
                    }
                    let delta = tier.problem.readings[observation].target
                        - tier.problem.readings[other].target;
                    if !ranges
                        .iter()
                        .flatten()
                        .any(|&(lo, hi)| lo <= delta && delta <= hi)
                    {
                        tier.pruned += 1;
                        return tier;
                    }
                }
            }
        }
        let root = Node {
            lo: tier
                .minimums
                .iter()
                .map(|rank| rank - 1)
                .chain(tier.problem.bands.iter().map(|_| tier.item_min))
                .collect(),
            hi: tier
                .problem
                .caps
                .iter()
                .map(|rank| rank - 1 + tier.problem.totals.last().unwrap().bonus / 5)
                .chain(tier.problem.bands.iter().map(|band| band.cap))
                .collect(),
            bound: tier.empty_bound(),
            order: 0,
            shift: 0,
            candidate: None,
            offsets: None,
            flat_search: None,
            hint: None,
            total: None,
            sum_range: None,
            profile: None,
        };
        if scout {
            let mut node = root;
            if tier.tighten(&mut node) {
                tier.queue.push(node);
            } else {
                tier.pruned += 1;
            }
        } else {
            let roles = tier.problem.ids.len();
            let bands = tier.problem.bands.len();
            let records = tier.problem.readings.len();
            let maxima: Vec<_> = tier
                .problem
                .caps
                .iter()
                .zip(&tier.minimums)
                .map(|(cap, min)| (cap - min).clamp(0, super::RANK_SPAN))
                .chain(
                    tier.problem
                        .bands
                        .iter()
                        .map(|band| (band.cap - tier.item_min).clamp(0, super::ITEM_SPAN)),
                )
                .chain([tier.global_span / 5])
                .collect();
            let weights: Vec<_> = (0..roles)
                .map(|_| 5)
                .chain((0..bands).map(|_| 1))
                .chain([5])
                .collect();
            profiles::spans(&maxima, &weights, budget, &mut |spans| {
                let Some(rules) = tier.profile_relations(spans) else {
                    tier.pruned += 1;
                    return;
                };
                if let Some(generator) =
                    Generator::new(records, roles, bands, spans, rules, &mut tier.axis_cache)
                {
                    let mut node = root.clone();
                    node.profile = Some(Box::new(generator));
                    node.bound.dimensions = spans.iter().filter(|&&value| value != 0).count();
                    node.bound.instruments = tier
                        .problem
                        .bands
                        .iter()
                        .enumerate()
                        .map(|(b, band)| (band.cap - spans[roles + b], band.cap))
                        .collect();
                    tier.order += 1;
                    node.order = tier.order;
                    tier.queue.push(node);
                }
            });
        }
        tier
    }
    fn axis_range(&mut self, observation: usize, axis: usize, steps: i32) -> (i64, i64) {
        if steps == 0 {
            return (0, 0);
        }
        let n = steps.abs();
        let ctx = Arc::clone(&self.problem);
        let reading = &ctx.readings[observation];
        let mut range = (0, 0);
        if axis < ctx.ids.len() {
            if let Some(slot) = reading.characters.iter().position(|&role| role == axis) {
                range = self.rank_marginal(observation, slot, n);
            }
        } else if axis < ctx.ids.len() + ctx.bands.len() {
            let band = axis - ctx.ids.len();
            for slot in 0..5 {
                if reading.bands[slot] == band {
                    let curve = self.delta_curve(observation, slot, n);
                    range.0 += curve.iter().map(|r| r.0).min().unwrap();
                    range.1 += curve.iter().map(|r| r.1).max().unwrap();
                }
            }
        } else {
            for slot in 0..5 {
                let (lo, hi) = self.rank_marginal(observation, slot, n);
                range.0 += lo;
                range.1 += hi;
            }
        }
        if steps > 0 {
            range
        } else {
            (-range.1, -range.0)
        }
    }
    fn profile_relations(&mut self, spans: &[i32]) -> Option<Constraints> {
        fn merge(mut values: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
            values.sort_unstable();
            let mut result: Vec<(i64, i64)> = Vec::new();
            for (lo, hi) in values {
                if let Some(last) = result.last_mut().filter(|last| lo <= last.1 + 1) {
                    last.1 = last.1.max(hi);
                } else {
                    result.push((lo, hi));
                }
            }
            result
        }
        let ctx = Arc::clone(&self.problem);
        let mut rules = vec![Vec::new(); spans.len()];
        let mut differences = Vec::new();
        for b in 1..ctx.readings.len() {
            for a in 0..b {
                if ctx.readings[a].signature != ctx.readings[b].signature {
                    continue;
                }
                let target = ctx.readings[a].target - ctx.readings[b].target;
                let axes: Vec<_> = spans
                    .iter()
                    .enumerate()
                    .filter(|(_, span)| **span > 0)
                    .map(|(axis, &span)| {
                        let values: Vec<_> = (-span..=span)
                            .map(|n| self.axis_range(a, axis, n))
                            .collect();
                        (axis, values)
                    })
                    .collect();
                if axes.is_empty() {
                    if target != 0 {
                        return None;
                    }
                    continue;
                }
                for (position, (axis, values)) in axes.iter().enumerate() {
                    let mut others = vec![(0, 0)];
                    for (other, (_, choices)) in axes.iter().enumerate() {
                        if other != position {
                            others = merge(
                                others
                                    .iter()
                                    .flat_map(|&(x, y)| {
                                        choices.iter().map(move |&(lo, hi)| (x + lo, y + hi))
                                    })
                                    .collect(),
                            );
                        }
                    }
                    let allowed: Vec<_> = values
                        .iter()
                        .map(|&(lo, hi)| {
                            others
                                .iter()
                                .any(|&(a, b)| lo + a <= target && target <= hi + b)
                        })
                        .collect();
                    if !allowed.iter().any(|&v| v) {
                        return None;
                    }
                    if allowed.iter().any(|&v| !v) {
                        rules[*axis].push(Relation { a, b, allowed });
                    }
                }
                let mut ranges = vec![vec![(0, 0)]; spans.len()];
                for (axis, values) in axes {
                    ranges[axis] = values;
                }
                differences.push(Difference {
                    a,
                    b,
                    target,
                    ranges,
                });
            }
        }
        Some((rules, differences))
    }
    fn pattern_feasible(&mut self, offsets: &Offsets) -> bool {
        let ctx = Arc::clone(&self.problem);
        for b in 1..ctx.readings.len() {
            let Some(a) = (0..b).find(|&a| ctx.readings[a].signature == ctx.readings[b].signature)
            else {
                continue;
            };
            let reading = &ctx.readings[a];
            let target = reading.target - ctx.readings[b].target;
            let changes = std::array::from_fn(|slot| {
                let role = reading.characters[slot];
                let band = reading.bands[slot];
                (
                    offsets.flat(a, role) - offsets.flat(b, role),
                    offsets[a][band] - offsets[b][band],
                )
            });
            let key = (a, target, changes);
            let possible = if let Some(&possible) = self.pattern_cache.get(&key) {
                possible
            } else {
                let (mut low, mut high) = (0, 0);
                for (slot, &(df, dg)) in changes.iter().enumerate() {
                    if df != 0 {
                        let (lo, hi) = self.rank_marginal(a, slot, df.abs());
                        if df > 0 {
                            low += lo;
                            high += hi;
                        } else {
                            low -= hi;
                            high -= lo;
                        }
                    }
                    if dg != 0 {
                        let curve = self.delta_curve(a, slot, dg);
                        low += curve.iter().map(|r| r.0).min().unwrap();
                        high += curve.iter().map(|r| r.1).max().unwrap();
                    }
                }
                let possible = low <= target && target <= high;
                self.pattern_cache.insert(key, possible);
                possible
            };
            if !possible {
                return false;
            }
        }
        true
    }
    fn seed(&mut self, mut node: Node) {
        self.order += 1;
        node.order = self.order;
        if node.lo.iter().zip(&node.hi).any(|(lo, hi)| lo > hi)
            || node.sum_range.is_some_and(|range| range.min > range.max)
            || !self.tighten(&mut node)
        {
            self.pruned += 1;
        } else {
            self.queue.push(node);
        }
    }
    fn seed_gauges(&mut self, root: Node) {
        let ctx = Arc::clone(&self.problem);
        let offsets = root.offsets.as_ref().unwrap();
        let size = ctx.ids.len();
        let lower: Vec<_> = (0..size)
            .map(|j| self.minimums[j] - offsets.ranks.iter().map(|r| r[j]).min().unwrap())
            .collect();
        let upper: Vec<_> = (0..size)
            .map(|j| ctx.caps[j] - offsets.ranks.iter().map(|r| r[j]).max().unwrap())
            .collect();
        let mut gauges = Vec::new();
        for &total in &ctx.totals {
            let mut a = i32::MIN;
            let mut b = i32::MAX;
            let mut valid = true;
            for r in 0..ctx.readings.len() {
                let Some(actual) = ctx
                    .totals
                    .iter()
                    .find(|t| t.bonus == total.bonus + offsets.bonus[r])
                else {
                    valid = false;
                    break;
                };
                let own = offsets.ranks[r].iter().sum::<i32>();
                a = a.max(actual.min - own);
                b = b.min(actual.max - own);
            }
            if !valid || a - b > ctx.unseen_span {
                continue;
            }
            let offset = total.bonus / 5;
            let range = Range {
                min: a - ctx.unseen_cap + size as i32 * offset - size as i32,
                max: b - ctx.unseen_count + size as i32 * offset - size as i32,
            };
            gauges.push((total, range));
        }
        for (index, &(total, range)) in gauges.iter().enumerate() {
            let offset = total.bonus / 5;
            let mut base = root.clone();
            base.total = Some(total);
            for j in 0..size {
                base.lo[j] = base.lo[j].max(lower[j] - 1 + offset);
                base.hi[j] = base.hi[j].min(upper[j] - 1 + offset);
            }
            if index == 0 {
                base.sum_range = Some(range);
                self.seed(base);
                continue;
            }
            let (previous, previous_range) = gauges[index - 1];
            let previous_offset = previous.bonus / 5;
            let mut sum_branch = base.clone();
            sum_branch.sum_range = Some(Range {
                min: range.min.max(previous_range.max + 1),
                max: range.max,
            });
            self.seed(sum_branch);
            if range.min > previous_range.max {
                continue;
            }
            for j in 0..size {
                let mut overflow = base.clone();
                overflow.sum_range = Some(Range {
                    min: range.min,
                    max: range.max.min(previous_range.max),
                });
                for (k, &cap) in upper.iter().enumerate().take(j) {
                    overflow.hi[k] = overflow.hi[k].min(cap - 1 + previous_offset);
                }
                overflow.lo[j] = overflow.lo[j].max(upper[j] + previous_offset);
                self.seed(overflow);
            }
        }
    }
    pub fn finished(&self) -> bool {
        self.queue.is_empty()
    }
    pub fn ready_rank(&self) -> u8 {
        self.queue
            .peek()
            .map(|node| {
                if node.candidate.is_some() {
                    2
                } else {
                    u8::from(node.flat_search.is_some())
                }
            })
            .unwrap_or(0)
    }
    fn kernel(&mut self, observation: usize, slot: usize, flat: i32) -> Arc<Vec<i64>> {
        let index = (observation * 5 + slot) * (self.problem.max_flat as usize + 1) + flat as usize;
        if let Some(cached) = self.kernels.lock().unwrap()[index].as_ref() {
            return Arc::clone(cached);
        }
        let ctx = &self.problem;
        let reading = &ctx.readings[observation];
        let member = &reading.team[slot];
        let snap = reading.snaps[slot].as_ref();
        let mut player = empty_player();
        let zero = ctx
            .master
            .total_rank_bonus
            .iter()
            .filter(|row| row.0 <= 0)
            .map(|row| row.1)
            .next_back()
            .unwrap_or(0);
        player
            .character_memory
            .insert(member.card.character_id, flat * 5 - zero);
        player.vip_rank = self.vip_rank;
        let effects = &ctx.master.events[&ctx.event].effects;
        let base = power::slot_base(
            &ctx.master,
            &player,
            member,
            Some(&reading.music),
            power::member_event_percent(effects, member, 2),
        );
        let fixed = base.fixed_points
            + power::snap_points(
                &ctx.master,
                &base,
                member,
                snap,
                snap.map(|snap| power::snap_event_percent(effects, snap, 2))
                    .unwrap_or(0),
            )
            + power::leader_points(&base, &reading.percents[slot]);
        let values = Arc::new(
            (0..=ctx.bands[reading.bands[slot]].cap)
                .map(|sum| {
                    fixed + power::points(&power::pct_floor(&base.common, &[sum as i64 * 10; 3]))
                })
                .collect(),
        );
        self.kernels.lock().unwrap()[index] = Some(Arc::clone(&values));
        values
    }
    fn options(&self, node: &Node) -> Vec<Bounds> {
        self.problem
            .totals
            .iter()
            .filter_map(|&total| {
                if node
                    .total
                    .is_some_and(|selected| selected.bonus != total.bonus)
                {
                    return None;
                }
                let offset = total.bonus / 5;
                let low: Vec<_> = self
                    .minimums
                    .iter()
                    .enumerate()
                    .map(|(i, &min)| min.max(node.lo[i] - offset + 1))
                    .collect();
                let high: Vec<_> = self
                    .problem
                    .caps
                    .iter()
                    .enumerate()
                    .map(|(i, &cap)| cap.min(node.hi[i] - offset + 1))
                    .collect();
                if low.iter().zip(&high).any(|(lo, hi)| lo > hi) {
                    return None;
                }
                let low_sum = low.iter().sum::<i32>();
                let min = total.min.max(low_sum + self.problem.unseen_count);
                let max = total
                    .max
                    .min(high.iter().sum::<i32>() + self.problem.unseen_cap);
                (min <= max).then_some(Bounds { total, min, max })
            })
            .collect()
    }
    pub fn bound(&self) -> Option<&Bound> {
        self.queue.peek().map(|node| &node.bound)
    }
    fn empty_bound(&self) -> Bound {
        let global = self.global_span;
        let maximum = global.max(super::RANK_SPAN * 5).max(super::ITEM_SPAN);
        Bound {
            variation: self.budget,
            dimensions: ((self.budget + maximum - 1) / maximum) as usize,
            vip: self.vip,
            instruments: Vec::new(),
        }
    }
    fn lower_bound(&self, node: &Node) -> Bound {
        let mut bound = self.empty_bound();
        if let Some(offsets) = &node.offsets {
            bound.dimensions = offsets.dimensions();
        }
        bound.instruments = self
            .problem
            .bands
            .iter()
            .enumerate()
            .map(|(i, band)| {
                let upper = node.hi[self.problem.ids.len() + i];
                if let Some(offsets) = &node.offsets {
                    (
                        upper + offsets.iter().map(|row| row[i]).min().unwrap(),
                        upper + offsets.iter().map(|row| row[i]).max().unwrap(),
                    )
                } else {
                    (upper, band.cap.min(upper + self.item_span))
                }
            })
            .collect();
        bound
    }
    fn value(
        &mut self,
        observation: usize,
        node: &Node,
        upper: bool,
        variable: Option<(usize, i32)>,
    ) -> i64 {
        let problem = Arc::clone(&self.problem);
        let reading = &problem.readings[observation];
        let drift = observation != 0;
        let pick = |index: usize| {
            variable
                .filter(|row| row.0 == index)
                .map(|row| row.1)
                .unwrap_or(if upper {
                    node.hi[index]
                } else {
                    node.lo[index]
                })
        };
        let direction = if upper { 1 } else { -1 };
        let rectangular = (0..5)
            .map(|slot| {
                let flat = (pick(reading.characters[slot])
                    + if let Some(offsets) = &node.offsets {
                        offsets.flat(observation, reading.characters[slot])
                    } else if drift {
                        direction * node.shift
                    } else {
                        0
                    })
                .clamp(0, problem.max_flat);
                let band = reading.bands[slot];
                let gear = (pick(problem.ids.len() + band)
                    + if let Some(offsets) = &node.offsets {
                        offsets[observation][band]
                    } else if drift {
                        direction * self.item_span
                    } else {
                        0
                    })
                .clamp(self.item_min, problem.bands[band].cap);
                self.kernel(observation, slot, flat)[gear as usize]
            })
            .sum::<i64>();
        if drift && node.offsets.is_none() && !self.scout && !self.movement.is_empty() {
            let baseline = (0..5)
                .map(|slot| {
                    let flat = pick(reading.characters[slot]);
                    let band = reading.bands[slot];
                    self.kernel(observation, slot, flat)[pick(problem.ids.len() + band) as usize]
                })
                .sum::<i64>();
            if upper {
                rectangular.min(baseline + self.movement[observation])
            } else {
                rectangular.max(baseline - self.movement[observation])
            }
        } else {
            rectangular
        }
    }
    fn rank_marginal(&mut self, observation: usize, slot: usize, steps: i32) -> (i64, i64) {
        let key = (observation, slot, steps);
        if let Some(&range) = self.marginals.lock().unwrap().get(&key) {
            return range;
        }
        let mut range = (i64::MAX, i64::MIN);
        for flat in 0..=self.problem.max_flat - steps {
            let a = self.kernel(observation, slot, flat);
            let b = self.kernel(observation, slot, flat + steps);
            for (&a, &b) in a.iter().zip(b.iter()) {
                let value = b - a;
                range.0 = range.0.min(value);
                range.1 = range.1.max(value);
            }
        }
        self.marginals.lock().unwrap().insert(key, range);
        range
    }
    fn budget_ranges(&mut self, observation: usize) -> Vec<Vec<(i64, i64)>> {
        let ctx = Arc::clone(&self.problem);
        let reading = &ctx.readings[observation];
        let mut axes = Vec::new();
        for slot in 0..5 {
            let role = reading.characters[slot];
            let mut options = vec![(0, 0, 0)];
            for n in 1..=(ctx.caps[role] - self.minimums[role])
                .min(super::RANK_SPAN)
                .min(self.budget / 5)
            {
                let (lo, hi) = self.rank_marginal(observation, slot, n);
                options.push((n * 5, lo, hi));
                options.push((n * 5, -hi, -lo));
            }
            axes.push(options);
        }
        let mut options = vec![(0, 0, 0)];
        for n in 1..=(self.global_span / 5).min(self.budget / 5) {
            let mut range = (0, 0);
            for slot in 0..5 {
                let (lo, hi) = self.rank_marginal(observation, slot, n);
                range.0 += lo;
                range.1 += hi;
            }
            options.push((n * 5, range.0, range.1));
            options.push((n * 5, -range.1, -range.0));
        }
        axes.push(options);
        for band in 0..ctx.bands.len() {
            let mut options = vec![(0, 0, 0)];
            for n in 1..=super::ITEM_SPAN
                .min(self.budget)
                .min(ctx.bands[band].cap - self.item_min)
            {
                let mut range = (0, 0);
                for slot in 0..5 {
                    if reading.bands[slot] == band {
                        let curve = self.delta_curve(observation, slot, n);
                        range.0 += curve.iter().map(|row| row.0).min().unwrap();
                        range.1 += curve.iter().map(|row| row.1).max().unwrap();
                    }
                }
                options.push((n, range.0, range.1));
                options.push((n, -range.1, -range.0));
            }
            axes.push(options);
        }
        let mut ranges = vec![Vec::new(); self.budget as usize + 1];
        ranges[0].push((0i64, 0i64));
        for options in axes {
            let mut next = vec![Vec::new(); ranges.len()];
            for (spent, values) in ranges.iter().enumerate() {
                for &(cost, lo, hi) in &options {
                    if spent + (cost as usize) < next.len() {
                        next[spent + cost as usize]
                            .extend(values.iter().map(|&(a, b)| (a + lo, b + hi)));
                    }
                }
            }
            for values in &mut next {
                values.sort_unstable();
                let mut merged: Vec<(i64, i64)> = Vec::new();
                for &(lo, hi) in values.iter() {
                    if let Some(last) = merged.last_mut().filter(|last| lo <= last.1 + 1) {
                        last.1 = last.1.max(hi);
                    } else {
                        merged.push((lo, hi));
                    }
                }
                *values = merged;
            }
            ranges = next;
        }
        ranges
    }
    fn delta_curve(&mut self, observation: usize, slot: usize, delta: i32) -> Arc<Vec<(i64, i64)>> {
        let ctx = &self.problem;
        let reading = &ctx.readings[observation];
        let member = &reading.team[slot];
        let initial = power::slot_base(
            &ctx.master,
            &empty_player(),
            member,
            None,
            power::member_event_percent(&ctx.master.events[&ctx.event].effects, member, 2),
        )
        .common;
        let zero = ctx
            .master
            .total_rank_bonus
            .iter()
            .filter(|row| row.0 <= 0)
            .map(|row| row.1)
            .next_back()
            .unwrap_or(0) as i64
            * 10000;
        let common = initial.map(|value| value - zero);
        let cap = ctx.bands[reading.bands[slot]].cap as usize;
        let key = (common, cap as i32, delta, ctx.max_flat);
        if let Some(curve) = self.deltas.lock().unwrap().get(&key) {
            return Arc::clone(curve);
        }
        let step = delta.unsigned_abs() as usize;
        let curve = Arc::new(
            (0..=ctx.max_flat)
                .map(|flat| {
                    let common = common.map(|value| value + flat as i64 * 5 * 10000);
                    let (mut low, mut high) = (i64::MAX, i64::MIN);
                    for gear in 0..=cap - step {
                        let value =
                            power::points(&power::pct_floor(
                                &common,
                                &[(gear + step) as i64 * 10; 3],
                            )) - power::points(&power::pct_floor(&common, &[gear as i64 * 10; 3]));
                        low = low.min(value);
                        high = high.max(value);
                    }
                    if delta > 0 {
                        (low, high)
                    } else {
                        (-high, -low)
                    }
                })
                .collect::<Vec<_>>(),
        );
        self.deltas.lock().unwrap().insert(key, Arc::clone(&curve));
        curve
    }
    fn tighten_differences(&mut self, node: &mut Node) -> Option<bool> {
        let Some(offsets) = node.offsets.clone() else {
            return Some(false);
        };
        if offsets.non_gear() {
            return self.pattern_feasible(&offsets).then_some(false);
        }
        let ctx = Arc::clone(&self.problem);
        let mut changed = false;
        for b in 1..ctx.readings.len() {
            let Some(a) = (0..b).find(|&a| ctx.readings[a].signature == ctx.readings[b].signature)
            else {
                continue;
            };
            let reading = &ctx.readings[a];
            let target = reading.target - ctx.readings[b].target;
            let mut curves = Vec::new();
            for slot in 0..5 {
                let band = reading.bands[slot];
                let delta = offsets[a][band] - offsets[b][band];
                if delta != 0 {
                    curves.push((reading.characters[slot], self.delta_curve(a, slot, delta)));
                }
            }
            let bounds: Vec<_> = curves
                .iter()
                .map(|(id, curve)| {
                    curve[node.lo[*id] as usize..=node.hi[*id] as usize]
                        .iter()
                        .copied()
                        .fold((i64::MAX, i64::MIN), |(lo, hi), (a, b)| {
                            (lo.min(a), hi.max(b))
                        })
                })
                .collect();
            let low = bounds.iter().map(|row| row.0).sum::<i64>();
            let high = bounds.iter().map(|row| row.1).sum::<i64>();
            if target < low || target > high {
                return None;
            }
            for (position, (id, curve)) in curves.iter().enumerate() {
                let others_low = low - bounds[position].0;
                let others_high = high - bounds[position].1;
                let possible: Vec<_> = (node.lo[*id]..=node.hi[*id])
                    .filter(|&flat| {
                        let (lo, hi) = curve[flat as usize];
                        others_low + lo <= target && target <= others_high + hi
                    })
                    .collect();
                let (first, last) = (*possible.first()?, *possible.last()?);
                changed |= first != node.lo[*id] || last != node.hi[*id];
                node.lo[*id] = first;
                node.hi[*id] = last;
            }
        }
        Some(changed)
    }
    fn tighten_totals(&self, node: &mut Node) -> Option<bool> {
        let ctx = &self.problem;
        let size = ctx.ids.len();
        let mut lower = vec![i32::MAX; size];
        let mut upper = vec![i32::MIN; size];
        for total in &ctx.totals {
            if node
                .total
                .is_some_and(|selected| selected.bonus != total.bonus)
            {
                continue;
            }
            let offset = total.bonus / 5;
            let lo: Vec<_> = (0..size)
                .map(|i| self.minimums[i].max(node.lo[i] - offset + 1))
                .collect();
            let hi: Vec<_> = (0..size)
                .map(|i| ctx.caps[i].min(node.hi[i] - offset + 1))
                .collect();
            let sum_lo = lo.iter().sum::<i32>();
            let sum_hi = hi.iter().sum::<i32>();
            if lo.iter().zip(&hi).any(|(lo, hi)| lo > hi)
                || sum_lo + ctx.unseen_count > total.max
                || sum_hi + ctx.unseen_cap < total.min
            {
                continue;
            }
            for i in 0..size {
                lower[i] = lower[i]
                    .min(lo[i].max(total.min - ctx.unseen_cap - (sum_hi - hi[i])) - 1 + offset);
                upper[i] = upper[i]
                    .max(hi[i].min(total.max - ctx.unseen_count - (sum_lo - lo[i])) - 1 + offset);
            }
        }
        if lower[0] == i32::MAX {
            return None;
        }
        let mut changed = false;
        for i in 0..size {
            let lo = node.lo[i].max(lower[i]);
            let hi = node.hi[i].min(upper[i]);
            changed |= lo != node.lo[i] || hi != node.hi[i];
            node.lo[i] = lo;
            node.hi[i] = hi;
        }
        if let Some(range) = node.sum_range {
            let sum_lo = node.lo[..size].iter().sum::<i32>();
            let sum_hi = node.hi[..size].iter().sum::<i32>();
            if sum_lo > range.max || sum_hi < range.min {
                return None;
            }
            for i in 0..size {
                let lo = node.lo[i].max(range.min - (sum_hi - node.hi[i]));
                let hi = node.hi[i].min(range.max - (sum_lo - node.lo[i]));
                if lo > hi {
                    return None;
                }
                changed |= lo != node.lo[i] || hi != node.hi[i];
                node.lo[i] = lo;
                node.hi[i] = hi;
            }
        }
        Some(changed)
    }
    fn tighten(&mut self, node: &mut Node) -> bool {
        let options = self.options(node);
        if options.is_empty() {
            return false;
        }
        node.shift = 0;
        if self.budget >= 5 && node.offsets.is_none() {
            let distance = self.problem.master.characters.len() as i32 * super::RANK_SPAN;
            for option in &options {
                for total in &self.problem.totals {
                    if total.min <= option.max + distance
                        && total.max >= option.min - distance
                        && (total.bonus - option.total.bonus).abs() <= self.budget
                    {
                        node.shift = node
                            .shift
                            .max(self.rank_span + (total.bonus - option.total.bonus).abs() / 5);
                    }
                }
            }
        }
        for _ in 0..4 {
            let mut changed = match self.tighten_totals(node) {
                Some(changed) => changed,
                None => return false,
            };
            changed |= match self.tighten_differences(node) {
                Some(changed) => changed,
                None => return false,
            };
            for observation in 0..self.problem.readings.len() {
                let target = self.problem.readings[observation].target;
                if target < self.value(observation, node, false, None)
                    || target > self.value(observation, node, true, None)
                {
                    return false;
                }
                for variable in 0..node.lo.len() {
                    if variable < self.problem.ids.len()
                        && !self.problem.readings[observation]
                            .characters
                            .contains(&variable)
                    {
                        continue;
                    }
                    let (mut lo, mut hi) = (node.lo[variable], node.hi[variable]);
                    while lo < hi {
                        let mid = (lo + hi) / 2;
                        if self.value(observation, node, true, Some((variable, mid))) < target {
                            lo = mid + 1;
                        } else {
                            hi = mid;
                        }
                    }
                    if lo > node.lo[variable] {
                        node.lo[variable] = lo;
                        changed = true;
                    }
                    (lo, hi) = (node.lo[variable], node.hi[variable]);
                    while lo < hi {
                        let mid = (lo + hi + 1) / 2;
                        if self.value(observation, node, false, Some((variable, mid))) > target {
                            hi = mid - 1;
                        } else {
                            lo = mid;
                        }
                    }
                    if lo < node.hi[variable] {
                        node.hi[variable] = lo;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        if self.options(node).is_empty() {
            return false;
        }
        node.bound = self.lower_bound(node);
        if self.scout {
            let size = self.problem.ids.len();
            node.hint = Some(
                (0..size)
                    .flat_map(|i| (i + 1..size).map(move |j| (i, j)))
                    .map(|(i, j)| {
                        let gap = (node.lo[i] - node.hi[j])
                            .max(node.lo[j] - node.hi[i])
                            .max(0) as i64;
                        gap * gap
                    })
                    .sum(),
            );
        }
        true
    }
    fn canonical(&self, flat: &[i32]) -> Vec<Canonical> {
        let options: Vec<_> = self
            .problem
            .totals
            .iter()
            .filter_map(|&total| {
                if self
                    .active_total
                    .is_some_and(|selected| selected.bonus != total.bonus)
                {
                    return None;
                }
                let levels: Vec<_> = flat
                    .iter()
                    .map(|value| value - total.bonus / 5 + 1)
                    .collect();
                if levels
                    .iter()
                    .enumerate()
                    .any(|(i, &value)| value < self.minimums[i] || value > self.problem.caps[i])
                {
                    return None;
                }
                let sum = levels.iter().sum::<i32>();
                let range = Range {
                    min: total.min.max(sum + self.problem.unseen_count),
                    max: total.max.min(sum + self.problem.unseen_cap),
                };
                (range.min <= range.max).then_some(Canonical {
                    levels,
                    total,
                    range,
                })
            })
            .collect();
        options
    }
    pub fn advance(&mut self) -> Option<Arc<Ranked>> {
        let mut node = self.queue.pop()?;
        self.visited += 1;
        if let Some(candidate) = node.candidate {
            if self.seen.insert(candidate.configuration.id.clone()) {
                return Some(candidate);
            }
            return None;
        }
        if let Some(mut generator) = node.profile.take() {
            if let Some(offsets) = generator.current() {
                if generator.feasible() && self.pattern_feasible(offsets) {
                    let mut domain = node.clone();
                    domain.offsets = Some(Arc::new(offsets.clone()));
                    let size = self.problem.ids.len();
                    for band in 0..self.problem.bands.len() {
                        let low = offsets.iter().map(|row| row[band]).min().unwrap();
                        let high = offsets.iter().map(|row| row[band]).max().unwrap();
                        domain.lo[size + band] = self.item_min - low;
                        domain.hi[size + band] = self.problem.bands[band].cap - high;
                    }
                    if self.tighten_differences(&mut domain).is_some() {
                        self.seed_gauges(domain);
                    } else {
                        self.pruned += 1;
                    }
                } else {
                    self.pruned += 1;
                }
            }
            generator.advance();
            if generator.current().is_some() {
                node.profile = Some(generator);
                self.order += 1;
                node.order = self.order;
                self.queue.push(node);
            }
            return None;
        }
        let size = self.problem.ids.len();
        let gear_split = (size..node.lo.len())
            .filter(|&i| node.hi[i] > node.lo[i])
            .max_by_key(|&i| node.hi[i] - node.lo[i]);
        if gear_split.is_none() && node.offsets.is_some() {
            if node.flat_search.is_none() {
                node.flat_search = self.flat_search(&node).map(Box::new);
            }
            if let Some(search) = &mut node.flat_search {
                if let Some(flat) = search.next_flat() {
                    self.enqueue_candidates(&flat, &node.lo[size..], &node);
                    self.queue.push(node);
                }
                return None;
            }
        }
        let split = if self.scout {
            (0..node.lo.len())
                .filter(|&i| node.hi[i] > node.lo[i])
                .max_by_key(|&i| (node.hi[i] - node.lo[i]) * if i < size { 1 } else { 2 })
        } else {
            gear_split.or_else(|| {
                (0..size)
                    .filter(|&i| node.hi[i] > node.lo[i])
                    .max_by_key(|&i| node.hi[i] - node.lo[i])
            })
        };
        if let Some(index) = split {
            let mid = (node.lo[index] + node.hi[index]) / 2;
            for high in [true, false] {
                self.order += 1;
                let mut child = Node {
                    lo: node.lo.clone(),
                    hi: node.hi.clone(),
                    bound: self.empty_bound(),
                    order: self.order,
                    shift: 0,
                    candidate: None,
                    offsets: node.offsets.clone(),
                    flat_search: None,
                    hint: None,
                    total: node.total,
                    sum_range: node.sum_range,
                    profile: None,
                };
                if high {
                    child.lo[index] = mid + 1;
                } else {
                    child.hi[index] = mid;
                }
                if self.tighten(&mut child) {
                    self.queue.push(child);
                } else {
                    self.pruned += 1;
                }
            }
            return None;
        }
        if self.value(0, &node, false, None) != self.problem.readings[0].target {
            return None;
        }
        self.enqueue_candidates(&node.lo[..size], &node.lo[size..], &node);
        None
    }
    fn flat_search(&mut self, node: &Node) -> Option<FlatSearch> {
        let ctx = Arc::clone(&self.problem);
        let offsets = node.offsets.as_ref().unwrap();
        let mut observations: Vec<usize> = Vec::new();
        for i in 0..ctx.readings.len() {
            if !observations.iter().any(|&other| {
                ctx.readings[other].signature == ctx.readings[i].signature
                    && ctx.readings[other].target == ctx.readings[i].target
                    && offsets[other] == offsets[i]
                    && offsets.ranks[other] == offsets.ranks[i]
                    && offsets.bonus[other] == offsets.bonus[i]
            }) {
                observations.push(i);
            }
        }
        let targets: Vec<i64> = observations
            .iter()
            .map(|&i| ctx.readings[i].target)
            .collect();
        let mut domains = Vec::new();
        for id in 0..ctx.ids.len() {
            let mut domain = Vec::new();
            for flat in node.lo[id]..=node.hi[id] {
                let mut values: Vec<i64> = observations
                    .iter()
                    .map(|&observation| {
                        let reading = &ctx.readings[observation];
                        reading
                            .characters
                            .iter()
                            .position(|&character| character == id)
                            .map(|slot| {
                                let band = reading.bands[slot];
                                let gear =
                                    node.lo[ctx.ids.len() + band] + offsets[observation][band];
                                self.kernel(observation, slot, flat + offsets.flat(observation, id))
                                    [gear as usize]
                            })
                            .unwrap_or(0)
                    })
                    .collect();
                values.push(flat as i64);
                domain.push((flat, values));
            }
            domains.push(domain);
        }
        let sum_range = node.sum_range.unwrap_or(Range {
            min: node.lo[..ctx.ids.len()].iter().sum(),
            max: node.hi[..ctx.ids.len()].iter().sum(),
        });
        if !propagate_vectors(&mut domains, &targets, sum_range) {
            self.pruned += 1;
            return Some(FlatSearch::empty());
        }
        let mut left = Vec::new();
        let mut right = Vec::new();
        let (mut a, mut b) = (1usize, 1usize);
        let mut roles: Vec<_> = (0..ctx.ids.len()).collect();
        roles.sort_by_key(|&id| std::cmp::Reverse(domains[id].len()));
        for id in roles {
            let width = domains[id].len();
            if a <= b {
                a = a.saturating_mul(width);
                left.push(id);
            } else {
                b = b.saturating_mul(width);
                right.push(id);
            }
        }
        if a.max(b) > 250_000 {
            return None;
        }
        Some(FlatSearch::new(domains, left, right, targets, sum_range))
    }
    fn enqueue_candidates(&mut self, flat: &[i32], gears: &[i32], node: &Node) {
        self.active_offsets = node.offsets.clone();
        self.active_total = node.total;
        for option in self.canonical(flat) {
            for mut candidate in self.configurations(gears, &option) {
                if !self.scout && candidate.variation != self.budget {
                    continue;
                }
                self.add_equivalents(&mut candidate);
                let candidate = Arc::new(candidate);
                self.latest.push(Arc::clone(&candidate));
                if self.scout {
                    return;
                }
                self.order += 1;
                self.queue.push(Node {
                    lo: node.lo.clone(),
                    hi: node.hi.clone(),
                    bound: candidate.bound(),
                    order: self.order,
                    shift: node.shift,
                    candidate: Some(candidate),
                    offsets: node.offsets.clone(),
                    flat_search: None,
                    hint: None,
                    total: node.total,
                    sum_range: node.sum_range,
                    profile: None,
                });
            }
        }
    }
    fn add_equivalents(&self, ranked: &mut Ranked) {
        let ctx = &self.problem;
        let configuration = &mut ranked.configuration;
        let mut equivalents = Vec::new();
        for shift in -ctx.caps.iter().max().unwrap()..=*ctx.caps.iter().max().unwrap() {
            let mut witnesses = Vec::new();
            let mut outside = Range {
                min: ctx.unseen_count,
                max: ctx.unseen_cap,
            };
            for original in &configuration.witnesses {
                let levels: Vec<_> = original.levels.iter().map(|rank| rank + shift).collect();
                if levels
                    .iter()
                    .enumerate()
                    .any(|(i, rank)| *rank < self.minimums[i] || *rank > ctx.caps[i])
                {
                    break;
                }
                let old = ctx
                    .totals
                    .iter()
                    .find(|total| {
                        total.min <= original.total_value && original.total_value <= total.max
                    })
                    .unwrap();
                let Some(total) = ctx
                    .totals
                    .iter()
                    .find(|total| total.bonus == old.bonus - shift * 5)
                else {
                    break;
                };
                let sum = levels.iter().sum::<i32>();
                let range = Range {
                    min: total.min.max(sum + ctx.unseen_count),
                    max: total.max.min(sum + ctx.unseen_cap),
                };
                if range.min > range.max {
                    break;
                }
                outside.min = outside.min.max(range.min - sum);
                outside.max = outside.max.min(range.max - sum + ctx.unseen_span);
                witnesses.push(Witness {
                    levels,
                    total_rank: range,
                    ..original.clone()
                });
            }
            if witnesses.len() != configuration.witnesses.len() || outside.min > outside.max {
                continue;
            }
            for witness in &mut witnesses {
                let sum = witness.levels.iter().sum::<i32>();
                witness.total_rank.min = witness
                    .total_rank
                    .min
                    .max(sum + outside.min - ctx.unseen_span);
                witness.total_rank.max = witness.total_rank.max.min(sum + outside.max);
                witness.total_value = witness.total_rank.min;
            }
            let valid = witnesses.iter().all(|witness| {
                let (_, model) = ctx
                    .originals
                    .iter()
                    .find(|row| row.0 == witness.challenge_id)
                    .unwrap();
                let reading = &ctx.readings[*model];
                let player = ctx.player(
                    &witness.levels,
                    &witness.instruments,
                    witness.total_value,
                    self.vip_rank,
                );
                power::team_power(
                    &ctx.master,
                    &player,
                    &reading.team,
                    &reading.snaps,
                    2,
                    Some(&reading.music),
                    &ctx.master.events[&ctx.event].effects,
                )
                .is_ok_and(|power| power.total == reading.target)
            });
            if !valid {
                continue;
            }
            equivalents.push(Equivalent {
                characters: ctx
                    .ids
                    .iter()
                    .enumerate()
                    .map(|(i, &id)| Character {
                        id,
                        min_level: witnesses.iter().map(|row| row.levels[i]).min().unwrap(),
                        level: witnesses.iter().map(|row| row.levels[i]).max().unwrap(),
                        cap: ctx.caps[i],
                    })
                    .collect(),
                instruments: configuration.instruments.clone(),
                total_rank: witnesses[0].total_rank,
                witnesses,
            });
        }
        equivalents.sort_by_key(|row| row.total_rank.min);
        for (i, character) in configuration.characters.iter_mut().enumerate() {
            character.min_level = equivalents
                .iter()
                .map(|row| row.characters[i].min_level)
                .min()
                .unwrap();
            character.level = equivalents
                .iter()
                .map(|row| row.characters[i].level)
                .max()
                .unwrap();
        }
        configuration.total_rank = Range {
            min: equivalents
                .iter()
                .map(|row| row.total_rank.min)
                .min()
                .unwrap(),
            max: equivalents
                .iter()
                .map(|row| row.total_rank.max)
                .max()
                .unwrap(),
        };
        configuration.equivalents = equivalents;
    }
    fn configurations(&mut self, gears: &[i32], option: &Canonical) -> Vec<Ranked> {
        let anchor = Witness {
            challenge_id: self.problem.readings[0].challenge,
            context_challenge_id: self.problem.readings[0].context_challenge,
            levels: option.levels.clone(),
            instruments: gears.to_vec(),
            total_rank: option.range,
            total_value: option.range.min,
        };
        let own = option.levels.iter().sum::<i32>();
        let mut local: Vec<Option<Vec<Witness>>> = vec![None; self.problem.readings.len()];
        let mut witnesses = vec![anchor];
        let outside = Range {
            min: option.range.min - own,
            max: self
                .problem
                .unseen_cap
                .min(option.range.max - own + self.problem.unseen_span),
        };
        let mut accepted = Vec::new();
        self.join(
            1,
            option,
            gears,
            &mut local,
            &mut witnesses,
            &option.levels,
            &option.levels,
            gears,
            gears,
            outside,
            &mut accepted,
        );
        accepted
            .into_iter()
            .filter_map(|rows| self.ranked(gears, rows))
            .collect()
    }
    fn ranked(&self, gears: &[i32], accepted: Vec<Witness>) -> Option<Ranked> {
        let size = self.problem.ids.len();
        let minimum: Vec<_> = (0..size)
            .map(|i| accepted.iter().map(|row| row.levels[i]).min().unwrap())
            .collect();
        let maximum: Vec<_> = (0..size)
            .map(|i| accepted.iter().map(|row| row.levels[i]).max().unwrap())
            .collect();
        let minimum_items: Vec<_> = (0..gears.len())
            .map(|i| accepted.iter().map(|row| row.instruments[i]).min().unwrap())
            .collect();
        let maximum_items: Vec<_> = (0..gears.len())
            .map(|i| accepted.iter().map(|row| row.instruments[i]).max().unwrap())
            .collect();
        let ctx = &self.problem;
        for witness in &accepted {
            let reading = ctx
                .readings
                .iter()
                .find(|reading| reading.challenge == witness.challenge_id)?;
            let player = ctx.player(
                &witness.levels,
                &witness.instruments,
                witness.total_value,
                self.vip_rank,
            );
            if power::team_power(
                &ctx.master,
                &player,
                &reading.team,
                &reading.snaps,
                2,
                Some(&reading.music),
                &ctx.master.events[&ctx.event].effects,
            )
            .ok()?
            .total
                != reading.target
            {
                return None;
            }
        }
        let globals: Vec<_> = accepted
            .iter()
            .map(|row| {
                ctx.totals
                    .iter()
                    .find(|total| total.min <= row.total_value && total.max >= row.total_value)
                    .unwrap()
                    .bonus
            })
            .collect();
        let global_span = globals.iter().max().unwrap() - globals.iter().min().unwrap();
        let variation = minimum
            .iter()
            .zip(&maximum)
            .map(|(lo, hi)| (hi - lo) * 5)
            .sum::<i32>()
            + minimum_items
                .iter()
                .zip(&maximum_items)
                .map(|(lo, hi)| hi - lo)
                .sum::<i32>()
            + global_span;
        let dimensions = minimum
            .iter()
            .zip(&maximum)
            .filter(|(lo, hi)| lo != hi)
            .count()
            + minimum_items
                .iter()
                .zip(&maximum_items)
                .filter(|(lo, hi)| lo != hi)
                .count()
            + usize::from(global_span != 0);
        let id = format!(
            "v{}r{}",
            self.vip,
            ctx.originals
                .iter()
                .map(|&(challenge, model)| {
                    let row = accepted
                        .iter()
                        .find(|row| row.challenge_id == ctx.readings[model].challenge)
                        .unwrap();
                    let bonus = ctx
                        .totals
                        .iter()
                        .find(|total| total.min <= row.total_value && row.total_value <= total.max)
                        .unwrap()
                        .bonus;
                    format!(
                        "{}:{}:{}:{}",
                        challenge,
                        (bonus - globals[0]) / 5,
                        row.levels
                            .iter()
                            .map(|level| (level - 1 + bonus / 5).to_string())
                            .collect::<Vec<_>>()
                            .join("-"),
                        row.instruments
                            .iter()
                            .map(i32::to_string)
                            .collect::<Vec<_>>()
                            .join("-")
                    )
                })
                .collect::<Vec<_>>()
                .join("|")
        );
        let configuration = Configuration {
            id,
            characters: ctx
                .ids
                .iter()
                .enumerate()
                .map(|(i, &id)| Character {
                    id,
                    level: maximum[i],
                    min_level: minimum[i],
                    cap: ctx.caps[i],
                })
                .collect(),
            instruments: ctx
                .bands
                .iter()
                .enumerate()
                .map(|(i, band)| Instrument {
                    id: band.id,
                    total: maximum_items[i],
                    min_total: minimum_items[i],
                    cap: band.cap,
                })
                .collect(),
            total_rank: accepted[0].total_rank,
            vip_bonus: self.vip,
            matched_challenges: ctx
                .originals
                .iter()
                .filter_map(|row| (row.0 != 0).then_some(row.0))
                .collect(),
            matched_total: ctx.originals.iter().any(|row| row.0 == 0),
            relaxed: self.relaxed,
            changed: variation != 0,
            change_cost: variation,
            changed_dimensions: dimensions,
            equivalents: Vec::new(),
            witnesses: ctx
                .originals
                .iter()
                .map(|&(challenge, model)| {
                    let mut row = accepted
                        .iter()
                        .find(|row| row.challenge_id == ctx.readings[model].challenge)
                        .unwrap()
                        .clone();
                    row.challenge_id = challenge;
                    if challenge != 0 {
                        row.context_challenge_id = challenge;
                    }
                    row
                })
                .collect(),
        };
        Some(Ranked {
            configuration,
            variation,
            dimensions,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn join(
        &mut self,
        index: usize,
        option: &Canonical,
        gears: &[i32],
        local: &mut [Option<Vec<Witness>>],
        witnesses: &mut Vec<Witness>,
        min_levels: &[i32],
        max_levels: &[i32],
        min_items: &[i32],
        max_items: &[i32],
        outside: Range,
        accepted_rows: &mut Vec<Vec<Witness>>,
    ) {
        let globals: Vec<_> = witnesses
            .iter()
            .map(|row| {
                self.problem
                    .totals
                    .iter()
                    .find(|total| total.min <= row.total_value && row.total_value <= total.max)
                    .unwrap()
                    .bonus
            })
            .collect();
        let global_span = globals.iter().max().unwrap() - globals.iter().min().unwrap();
        let cost = (
            min_levels
                .iter()
                .zip(max_levels)
                .map(|(lo, hi)| (hi - lo) * 5)
                .sum::<i32>()
                + min_items
                    .iter()
                    .zip(max_items)
                    .map(|(lo, hi)| hi - lo)
                    .sum::<i32>()
                + global_span,
            min_levels
                .iter()
                .zip(max_levels)
                .filter(|(lo, hi)| lo != hi)
                .count()
                + min_items
                    .iter()
                    .zip(max_items)
                    .filter(|(lo, hi)| lo != hi)
                    .count()
                + usize::from(global_span != 0),
        );
        if cost.0 > self.budget || self.scout && !accepted_rows.is_empty() {
            return;
        }
        if index == self.problem.readings.len() {
            let own = option.levels.iter().sum::<i32>();
            let upper = (option.range.max - own).clamp(outside.min, outside.max);
            let accepted: Vec<_> = witnesses
                .iter()
                .map(|witness| {
                    let sum = witness.levels.iter().sum::<i32>();
                    Witness {
                        total_value: witness
                            .total_rank
                            .min
                            .max(sum + upper - self.problem.unseen_span),
                        total_rank: Range {
                            min: witness
                                .total_rank
                                .min
                                .max(sum + outside.min - self.problem.unseen_span),
                            max: witness.total_rank.max.min(sum + outside.max),
                        },
                        ..witness.clone()
                    }
                })
                .collect();
            accepted_rows.push(accepted);
            return;
        }
        if local[index].is_none() {
            local[index] = Some(self.local_witnesses(index, option, gears));
        }
        for position in 0..local[index].as_ref().unwrap().len() {
            let witness = local[index].as_ref().unwrap()[position].clone();
            if witness.levels.iter().enumerate().any(|(i, &value)| {
                max_levels[i].max(value) - min_levels[i].min(value) > self.rank_span
            }) || witness.instruments.iter().enumerate().any(|(i, &value)| {
                max_items[i].max(value) - min_items[i].min(value) > self.item_span
            }) {
                continue;
            }
            let sum = witness.levels.iter().sum::<i32>();
            let range = Range {
                min: outside.min.max(witness.total_rank.min - sum),
                max: outside
                    .max
                    .min(witness.total_rank.max - sum + self.problem.unseen_span),
            };
            if range.min > range.max {
                continue;
            }
            let lo: Vec<_> = min_levels
                .iter()
                .zip(&witness.levels)
                .map(|(a, b)| (*a).min(*b))
                .collect();
            let hi: Vec<_> = max_levels
                .iter()
                .zip(&witness.levels)
                .map(|(a, b)| (*a).max(*b))
                .collect();
            let items_lo: Vec<_> = min_items
                .iter()
                .zip(&witness.instruments)
                .map(|(a, b)| (*a).min(*b))
                .collect();
            let items_hi: Vec<_> = max_items
                .iter()
                .zip(&witness.instruments)
                .map(|(a, b)| (*a).max(*b))
                .collect();
            witnesses.push(witness);
            self.join(
                index + 1,
                option,
                gears,
                local,
                witnesses,
                &lo,
                &hi,
                &items_lo,
                &items_hi,
                range,
                accepted_rows,
            );
            witnesses.pop();
        }
    }
    fn local_witnesses(
        &mut self,
        observation: usize,
        option: &Canonical,
        gears: &[i32],
    ) -> Vec<Witness> {
        let ctx = Arc::clone(&self.problem);
        if let Some(offsets) = &self.active_offsets {
            let levels: Vec<_> = option
                .levels
                .iter()
                .enumerate()
                .map(|(i, &level)| level + offsets.ranks[observation][i])
                .collect();
            if levels
                .iter()
                .enumerate()
                .any(|(i, &level)| level < self.minimums[i] || level > ctx.caps[i])
            {
                return Vec::new();
            }
            let Some(total) = ctx
                .totals
                .iter()
                .find(|total| total.bonus == option.total.bonus + offsets.bonus[observation])
            else {
                return Vec::new();
            };
            let instruments: Vec<_> = gears
                .iter()
                .enumerate()
                .map(|(band, &level)| level + offsets[observation][band])
                .collect();
            if instruments
                .iter()
                .enumerate()
                .any(|(i, &level)| level < self.item_min || level > ctx.bands[i].cap)
            {
                return Vec::new();
            }
            let sum = levels.iter().sum::<i32>();
            let range = Range {
                min: total.min.max(sum + ctx.unseen_count),
                max: total.max.min(sum + ctx.unseen_cap),
            };
            if range.min > range.max {
                return Vec::new();
            }
            return vec![Witness {
                challenge_id: ctx.readings[observation].challenge,
                context_challenge_id: ctx.readings[observation].context_challenge,
                levels,
                instruments,
                total_rank: range,
                total_value: range.min,
            }];
        }

        let totals: Vec<_> = ctx
            .totals
            .iter()
            .copied()
            .filter(|total| {
                total.min
                    <= option.range.max + ctx.master.characters.len() as i32 * super::RANK_SPAN
                    && total.max
                        >= option.range.min - ctx.master.characters.len() as i32 * super::RANK_SPAN
            })
            .collect();
        let mut totals: Vec<_> = totals
            .into_iter()
            .filter(|total| (total.bonus - option.total.bonus).abs() <= self.budget)
            .collect();
        totals.sort_by_key(|total| (total.bonus - option.total.bonus).abs());
        let reading = &ctx.readings[observation];
        let mut min_gear: Vec<_> = gears
            .iter()
            .map(|value| self.item_min.max(value - self.item_span))
            .collect();
        let mut max_gear: Vec<_> = ctx
            .bands
            .iter()
            .zip(gears)
            .map(|(band, value)| band.cap.min(value + self.item_span))
            .collect();
        if let Some(offsets) = &self.active_offsets {
            for band in 0..gears.len() {
                min_gear[band] = gears[band] + offsets[observation][band];
                max_gear[band] = min_gear[band];
            }
        }
        let min_level: Vec<_> = option
            .levels
            .iter()
            .enumerate()
            .map(|(i, value)| self.minimums[i].max(value - self.rank_span))
            .collect();
        let max_level: Vec<_> = option
            .levels
            .iter()
            .enumerate()
            .map(|(i, value)| ctx.caps[i].min(value + self.rank_span))
            .collect();
        let mut result = Vec::new();
        let order: Vec<_> = reading
            .characters
            .iter()
            .copied()
            .chain((0..ctx.ids.len()).filter(|id| !reading.characters.contains(id)))
            .collect();
        for total in totals {
            let values: Vec<Vec<_>> = (0..5)
                .map(|slot| {
                    (min_level[reading.characters[slot]]..=max_level[reading.characters[slot]])
                        .map(|level| self.kernel(observation, slot, level - 1 + total.bonus / 5))
                        .collect()
                })
                .collect();
            let mut levels = option.levels.clone();
            let mut stack = vec![(0usize, 0usize)];
            while let Some((position, domain_index)) = stack.pop() {
                if position < order.len() {
                    let character = order[position];
                    let mut domain: Vec<_> =
                        (min_level[character]..=max_level[character]).collect();
                    domain.sort_by_key(|level| ((level - option.levels[character]).abs(), -level));
                    if domain_index >= domain.len() {
                        levels[character] = option.levels[character];
                        continue;
                    }
                    levels[character] = domain[domain_index];
                    let rank_cost = order[..=position]
                        .iter()
                        .map(|&i| (levels[i] - option.levels[i]).abs() * 5)
                        .sum::<i32>();
                    stack.push((position, domain_index + 1));
                    let mut low = 0;
                    let mut high = 0;
                    for slot in 0..5 {
                        let character = reading.characters[slot];
                        let l = if slot <= position {
                            levels[character]
                        } else {
                            min_level[character]
                        };
                        let h = if slot <= position {
                            levels[character]
                        } else {
                            max_level[character]
                        };
                        low += values[slot][(l - min_level[character]) as usize]
                            [min_gear[reading.bands[slot]] as usize];
                        high += values[slot][(h - min_level[character]) as usize]
                            [max_gear[reading.bands[slot]] as usize];
                    }
                    if low <= reading.target
                        && high >= reading.target
                        && rank_cost + (total.bonus - option.total.bonus).abs() <= self.budget
                    {
                        stack.push((position + 1, 0));
                    }
                    continue;
                }
                let sum = levels.iter().sum::<i32>();
                let range = Range {
                    min: total.min.max(sum + ctx.unseen_count),
                    max: total.max.min(sum + ctx.unseen_cap),
                };
                if range.min > range.max {
                    continue;
                }
                let arrays: Vec<Vec<i64>> = ctx
                    .bands
                    .iter()
                    .enumerate()
                    .map(|(band, _)| {
                        (min_gear[band]..=max_gear[band])
                            .map(|gear| {
                                (0..5)
                                    .filter(|&slot| reading.bands[slot] == band)
                                    .map(|slot| {
                                        values[slot][(levels[reading.characters[slot]]
                                            - min_level[reading.characters[slot]])
                                            as usize][gear as usize]
                                    })
                                    .sum()
                            })
                            .collect()
                    })
                    .collect();
                let mut items = vec![0; gears.len()];
                collect_items(
                    0,
                    reading.target,
                    &arrays,
                    &min_gear,
                    gears,
                    &mut items,
                    &mut |instruments| {
                        let cost = levels
                            .iter()
                            .zip(&option.levels)
                            .map(|(a, b)| (a - b).abs() * 5)
                            .sum::<i32>()
                            + instruments
                                .iter()
                                .zip(gears)
                                .map(|(a, b)| (a - b).abs())
                                .sum::<i32>()
                            + (total.bonus - option.total.bonus).abs();
                        if cost > self.budget {
                            return;
                        }
                        result.push(Witness {
                            challenge_id: reading.challenge,
                            context_challenge_id: reading.context_challenge,
                            levels: levels.clone(),
                            instruments: instruments.to_vec(),
                            total_rank: range,
                            total_value: range.min,
                        });
                    },
                );
            }
        }
        result
    }
}
fn collect_items(
    index: usize,
    remaining: i64,
    arrays: &[Vec<i64>],
    minimums: &[i32],
    preferred: &[i32],
    current: &mut [i32],
    emit: &mut impl FnMut(&[i32]),
) {
    let values = &arrays[index];
    if index + 1 == arrays.len() {
        let mut offset = values.partition_point(|value| *value < remaining);
        while offset < values.len() && values[offset] == remaining {
            current[index] = offset as i32 + minimums[index];
            emit(current);
            offset += 1;
        }
    } else {
        let rest_low: i64 = arrays[index + 1..].iter().map(|array| array[0]).sum();
        let rest_high: i64 = arrays[index + 1..]
            .iter()
            .map(|array| *array.last().unwrap())
            .sum();
        let mut order: Vec<_> = (0..values.len()).collect();
        order.sort_by_key(|&offset| {
            (
                (offset as i32 + minimums[index] - preferred[index]).abs(),
                offset,
            )
        });
        for offset in order {
            let next = remaining - values[offset];
            if next < rest_low || next > rest_high {
                continue;
            }
            current[index] = offset as i32 + minimums[index];
            collect_items(index + 1, next, arrays, minimums, preferred, current, emit);
        }
    }
}

#[derive(Clone)]
struct FlatSearch {
    domains: Vec<Vec<(i32, Vec<i64>)>>,
    left_roles: Vec<usize>,
    right_roles: Vec<usize>,
    sums: rustc_hash::FxHashMap<Vec<i64>, Vec<(u32, i32)>>,
    target: Vec<i64>,
    cursors: Vec<usize>,
    bucket: usize,
    done: bool,
    sum_range: Range,
}
type FlatBounds<'a> = (&'a [Vec<i64>], &'a [Vec<i64>], &'a [i64], &'a [i64]);
fn propagate_vectors(
    domains: &mut [Vec<(i32, Vec<i64>)>],
    targets: &[i64],
    sum_range: Range,
) -> bool {
    fn gcd(mut a: i64, mut b: i64) -> i64 {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a.abs()
    }
    let mut constraints: Vec<_> = targets
        .iter()
        .enumerate()
        .map(|(i, &target)| (i, None, target, target))
        .collect();
    for a in 0..targets.len() {
        for b in a + 1..targets.len() {
            let target = targets[a] - targets[b];
            constraints.push((a, Some(b), target, target));
        }
    }
    constraints.push((
        targets.len(),
        None,
        sum_range.min as i64,
        sum_range.max as i64,
    ));
    loop {
        let mut changed = false;
        for &(a, b, target_lo, target_hi) in &constraints {
            let value = |row: &(i32, Vec<i64>)| row.1[a] - b.map(|b| row.1[b]).unwrap_or(0);
            let bounds: Vec<_> = domains
                .iter()
                .map(|domain| {
                    let base = value(&domain[0]);
                    domain
                        .iter()
                        .map(&value)
                        .fold((i64::MAX, i64::MIN, base, 0i64), |(lo, hi, base, g), v| {
                            (lo.min(v), hi.max(v), base, gcd(g, v - base))
                        })
                })
                .collect();
            let lo = bounds.iter().map(|r| r.0).sum::<i64>();
            let hi = bounds.iter().map(|r| r.1).sum::<i64>();
            let base = bounds.iter().map(|r| r.2).sum::<i64>();
            if lo > target_hi || hi < target_lo {
                return false;
            }
            for (i, domain) in domains.iter_mut().enumerate() {
                let other_lo = lo - bounds[i].0;
                let other_hi = hi - bounds[i].1;
                let other_base = base - bounds[i].2;
                let divisor = bounds
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .fold(0, |g, (_, r)| gcd(g, r.3));
                let before = domain.len();
                domain.retain(|row| {
                    let v = value(row);
                    if v + other_lo > target_hi || v + other_hi < target_lo {
                        return false;
                    }
                    if target_lo == target_hi {
                        let remaining = target_lo - v - other_base;
                        if divisor == 0 {
                            return remaining == 0;
                        }
                        if remaining % divisor != 0 {
                            return false;
                        }
                    }
                    true
                });
                if domain.is_empty() {
                    return false;
                }
                changed |= domain.len() != before;
            }
        }
        if !changed {
            return true;
        }
    }
}
impl FlatSearch {
    fn empty() -> Self {
        Self {
            domains: Vec::new(),
            left_roles: Vec::new(),
            right_roles: Vec::new(),
            sums: rustc_hash::FxHashMap::default(),
            target: Vec::new(),
            cursors: Vec::new(),
            bucket: 0,
            done: true,
            sum_range: Range { min: 0, max: 0 },
        }
    }
    fn new(
        domains: Vec<Vec<(i32, Vec<i64>)>>,
        left_roles: Vec<usize>,
        right_roles: Vec<usize>,
        target: Vec<i64>,
        sum_range: Range,
    ) -> Self {
        let mut sums = rustc_hash::FxHashMap::default();
        let equations = target.len() + 1;
        let mut target_low = target.clone();
        target_low.push(sum_range.min as i64);
        let mut target_high = target.clone();
        target_high.push(sum_range.max as i64);
        let mut lower = vec![vec![0i64; equations]; left_roles.len() + 1];
        let mut upper = lower.clone();
        for &role in &right_roles {
            for equation in 0..equations {
                lower[left_roles.len()][equation] += domains[role]
                    .iter()
                    .map(|row| row.1[equation])
                    .min()
                    .unwrap();
                upper[left_roles.len()][equation] += domains[role]
                    .iter()
                    .map(|row| row.1[equation])
                    .max()
                    .unwrap();
            }
        }
        for depth in (0..left_roles.len()).rev() {
            for equation in 0..equations {
                lower[depth][equation] = lower[depth + 1][equation]
                    + domains[left_roles[depth]]
                        .iter()
                        .map(|row| row.1[equation])
                        .min()
                        .unwrap();
                upper[depth][equation] = upper[depth + 1][equation]
                    + domains[left_roles[depth]]
                        .iter()
                        .map(|row| row.1[equation])
                        .max()
                        .unwrap();
            }
        }
        fn enumerate(
            depth: usize,
            roles: &[usize],
            domains: &[Vec<(i32, Vec<i64>)>],
            sum: &mut [i64],
            code: u32,
            bounds: FlatBounds<'_>,
            map: &mut rustc_hash::FxHashMap<Vec<i64>, Vec<(u32, i32)>>,
        ) {
            if bounds.2.iter().enumerate().any(|(i, &target)| {
                sum[i] + bounds.0[depth][i] > bounds.3[i] || sum[i] + bounds.1[depth][i] < target
            }) {
                return;
            }
            if depth == roles.len() {
                let last = sum.len() - 1;
                if let Some(rows) = map.get_mut(&sum[..last]) {
                    rows.push((code, sum[last] as i32));
                } else {
                    map.insert(sum[..last].to_vec(), vec![(code, sum[last] as i32)]);
                }
                return;
            }
            let width = domains[roles[depth]].len() as u32;
            for (index, (_, values)) in domains[roles[depth]].iter().enumerate() {
                for (i, &value) in values.iter().enumerate() {
                    sum[i] += value;
                }
                enumerate(
                    depth + 1,
                    roles,
                    domains,
                    sum,
                    code * width + index as u32,
                    bounds,
                    map,
                );
                for (i, &value) in values.iter().enumerate() {
                    sum[i] -= value;
                }
            }
        }
        enumerate(
            0,
            &left_roles,
            &domains,
            &mut vec![0; equations],
            0,
            (&lower, &upper, &target_low, &target_high),
            &mut sums,
        );
        Self {
            cursors: vec![0; right_roles.len()],
            domains,
            left_roles,
            right_roles,
            sums,
            target,
            bucket: 0,
            done: false,
            sum_range,
        }
    }
    fn next_flat(&mut self) -> Option<Vec<i32>> {
        let mut needed = self.target.clone();
        while !self.done {
            needed.copy_from_slice(&self.target);
            let mut right_sum = 0;
            for (i, &role) in self.right_roles.iter().enumerate() {
                for (j, &value) in self.domains[role][self.cursors[i]].1.iter().enumerate() {
                    if j < needed.len() {
                        needed[j] -= value;
                    } else {
                        right_sum += value as i32;
                    }
                }
            }
            if let Some(left) = self
                .sums
                .get(&needed)
                .and_then(|rows| rows.get(self.bucket))
            {
                self.bucket += 1;
                let mut flat = vec![0; self.domains.len()];
                let (left_code, left_sum) = *left;
                if left_sum + right_sum < self.sum_range.min
                    || left_sum + right_sum > self.sum_range.max
                {
                    continue;
                }
                let mut code = left_code as usize;
                for &role in self.left_roles.iter().rev() {
                    let width = self.domains[role].len();
                    flat[role] = self.domains[role][code % width].0;
                    code /= width;
                }
                for (i, &role) in self.right_roles.iter().enumerate() {
                    flat[role] = self.domains[role][self.cursors[i]].0;
                }
                return Some(flat);
            }
            self.bucket = 0;
            let mut position = self.cursors.len();
            loop {
                if position == 0 {
                    self.done = true;
                    break;
                }
                position -= 1;
                self.cursors[position] += 1;
                if self.cursors[position] < self.domains[self.right_roles[position]].len() {
                    break;
                }
                self.cursors[position] = 0;
            }
        }
        None
    }
}
