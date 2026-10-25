//! Event settlement and ordinary live reward bounds, shared by native and WASM.
use crate::{
    chart::{self, Chart},
    master::{EventEffect, Master, Reward},
    model::RELATIVE_ERROR,
    num::{i32, imul},
    objectives::LiveCore,
    power::{self, Member, Snap},
    search::{Exact, JointTarget, Objective, Team, Totals},
};
use rustc_hash::FxHashMap as HashMap;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

#[derive(Clone)]
struct CurvePoint {
    x: f64,
    y: f64,
    integer: i128,
}
#[derive(Clone)]
pub(crate) struct RewardCurve {
    points: Vec<CurvePoint>,
    steps: Vec<(f64, f64)>,
    error: f64,
}
impl RewardCurve {
    pub(crate) fn new(rows: Vec<(i32, f64)>) -> Option<Self> {
        let mut values = BTreeMap::<i32, f64>::new();
        for (x, y) in rows {
            if x < 0 || !y.is_finite() || y < 0.0 {
                return None;
            }
            values.entry(x).and_modify(|v| *v = v.max(y)).or_insert(y);
        }
        let mut high = 0.0_f64;
        for y in values.values_mut() {
            high = high.max(*y);
            *y = high;
        }
        let parts = |y: f64| {
            let bits = y.to_bits();
            let exponent = ((bits >> 52) & 2047) as i32;
            let fraction = bits & ((1u64 << 52) - 1);
            let (mantissa, power) = if exponent == 0 {
                (fraction, -1074)
            } else {
                ((1u64 << 52) | fraction, exponent - 1075)
            };
            if mantissa == 0 {
                (0, 0)
            } else {
                let zeros = mantissa.trailing_zeros();
                (mantissa >> zeros, power + zeros as i32)
            }
        };
        let exponent = values
            .values()
            .filter(|&&y| y > 0.0)
            .map(|&y| parts(y).1)
            .min()
            .unwrap_or(0);
        let steps: Vec<_> = values.iter().map(|(&x, &y)| (x as f64, y)).collect();
        let mut points = Vec::<CurvePoint>::new();
        for (x, y) in values {
            let (mantissa, power) = parts(y);
            let integer = if mantissa == 0 {
                0
            } else {
                let shift = power - exponent;
                if !(0..=36).contains(&shift) {
                    // Between adjacent rank thresholds the upper envelope is linear.
                    // Round its values outward once, rather than scanning pairs per bound.
                    let mut envelope = Vec::with_capacity(steps.len());
                    let mut previous = 0.0_f64;
                    for (i, &(x, y)) in steps.iter().enumerate() {
                        let mut upper = y;
                        for &(ax, ay) in &steps[..i] {
                            for &(bx, by) in &steps[i + 1..] {
                                let fraction = ((x - ax).next_up() / (bx - ax)).next_up();
                                let value =
                                    (ay + ((by - ay).next_up() * fraction).next_up()).next_up();
                                upper = upper.max(value);
                            }
                        }
                        previous = previous.max(upper).min(high);
                        envelope.push(CurvePoint {
                            x,
                            y: previous,
                            integer: 0,
                        });
                    }
                    return Some(Self {
                        points: envelope,
                        steps,
                        error: high * (256.0 * f64::EPSILON) / (1.0 - 256.0 * f64::EPSILON),
                    });
                }
                (mantissa as i128) << shift
            };
            let point = CurvePoint {
                x: x as f64,
                y,
                integer,
            };
            while points.len() >= 2 {
                let a = &points[points.len() - 2];
                let b = &points[points.len() - 1];
                if (b.integer - a.integer) * (point.x as i128 - b.x as i128)
                    > (point.integer - b.integer) * (b.x as i128 - a.x as i128)
                {
                    break;
                }
                points.pop();
            }
            points.push(point);
        }
        let error = if points.iter().all(|p| p.integer <= (1i128 << 53) / 120) {
            0.0
        } else {
            high * (256.0 * f64::EPSILON) / (1.0 - 256.0 * f64::EPSILON)
        };
        (!points.is_empty()).then_some(Self {
            points,
            steps,
            error,
        })
    }
    pub(crate) fn upper(&self, score: f64) -> f64 {
        let last = self.points.last().unwrap();
        if !score.is_finite() || score >= last.x {
            return last.y;
        }
        let i = self.points.partition_point(|p| p.x <= score);
        if i == 0 {
            return self.points[0].y;
        }
        let (a, b) = (&self.points[i - 1], &self.points[i]);
        let fraction = ((score - a.x).next_up() / (b.x - a.x)).next_up();
        ((a.y + ((b.y - a.y).next_up() * fraction).next_up()).next_up() + self.error)
            .next_up()
            .min(last.y)
    }
    pub(crate) fn required(&self, reward: f64) -> f64 {
        let reward = (reward - self.error).max(0.0);
        let i = self.points.partition_point(|p| p.y < reward);
        if i == 0 {
            return 0.0;
        }
        if i == self.points.len() {
            return f64::INFINITY;
        }
        let (a, b) = (&self.points[i - 1], &self.points[i]);
        let value = (((reward - a.y).next_down() * (b.x - a.x)).next_down()
            / (b.y - a.y).next_up())
        .next_down();
        (a.x + value).next_down().max(0.0)
    }
    pub(crate) fn orders(&self, coefficients: &[f64], power: f64) -> f64 {
        let mut before = 0;
        let mut total = 0.0;
        let lowest = power * coefficients[0];
        let rank = self
            .steps
            .partition_point(|&(required, _)| required <= lowest)
            .saturating_sub(1);
        let mut value = self.steps[rank].1;
        for &(required, next) in self.steps.iter().skip(rank + 1) {
            if before == coefficients.len() {
                break;
            }
            let count = before
                + coefficients[before..]
                    .partition_point(|&coefficient| power * coefficient < required);
            total += (count - before) as f64 * value;
            before = count;
            value = next;
        }
        total += (coefficients.len() - before) as f64 * value;
        let mean = total / coefficients.len() as f64;
        if self.error == 0.0 {
            mean
        } else {
            (mean.next_up() + self.error)
                .next_up()
                .min(self.steps.last().unwrap().1)
        }
    }
    pub(crate) fn unordered_orders(&self, coefficients: &[f64], power: f64) -> f64 {
        self.partial_orders(coefficients, power, coefficients.len())
    }
    pub(crate) fn partial_orders(&self, coefficients: &[f64], power: f64, count: usize) -> f64 {
        let lowest = power * coefficients.iter().copied().fold(f64::INFINITY, f64::min);
        let rank = self
            .steps
            .partition_point(|&(required, _)| required <= lowest)
            .saturating_sub(1);
        let mut before = 0;
        let mut total = 0.0;
        let mut value = self.steps[rank].1;
        for &(required, next) in self.steps.iter().skip(rank + 1) {
            if before == count {
                break;
            }
            let count = coefficients
                .iter()
                .filter(|&&value| power * value < required)
                .count();
            total += (count - before) as f64 * value;
            before = count;
            value = next;
        }
        total += (count - before) as f64 * value;
        let mean = total / count as f64;
        if self.error == 0.0 {
            mean
        } else {
            (mean.next_up() + self.error)
                .next_up()
                .min(self.steps.last().unwrap().1)
        }
    }
    pub(crate) fn limited_orders(&self, coefficient: f64, power: f64, count: usize) -> f64 {
        let score = power * coefficient;
        let rank = self
            .steps
            .partition_point(|&(required, _)| required <= score)
            .saturating_sub(1);
        let maximum = self.steps.last().unwrap().1;
        let low = self.steps[rank].1 * count as f64;
        let high = maximum * (120 - count) as f64;
        if self.error == 0.0 {
            (low + high) / 120.0
        } else {
            ((((low.next_up() + high.next_up()).next_up() / 120.0).next_up() + self.error)
                .next_up())
            .min(maximum)
        }
    }

    pub(crate) fn shifted_sum(
        &self,
        coefficients: &[f64],
        power: f64,
        offset: f64,
        absolute: f64,
    ) -> f64 {
        let mut before = 0;
        let mut total = 0.0;
        let mut value = self.steps[0].1;
        for &(required, next) in self.steps.iter().skip(1) {
            let below = |coefficient: f64| {
                (((coefficient + offset).next_up() * power).next_up() + absolute).next_up()
                    < required
            };
            let count =
                if power > 0.0 && power.is_finite() && offset.is_finite() && absolute.is_finite() {
                    let hint = (required - absolute) / power - offset;
                    let mut count = coefficients.partition_point(|&coefficient| coefficient < hint);
                    if count > 0 && !below(coefficients[count - 1]) {
                        count = coefficients[..count]
                            .partition_point(|&coefficient| below(coefficient));
                    } else if count < coefficients.len() && below(coefficients[count]) {
                        count += coefficients[count..]
                            .partition_point(|&coefficient| below(coefficient));
                    }
                    count
                } else {
                    coefficients.partition_point(|&coefficient| below(coefficient))
                };
            total += (count - before) as f64 * value;
            before = count;
            value = next;
        }
        total + (coefficients.len() - before) as f64 * value
    }
    fn scores(&self, scores: &[f64]) -> f64 {
        let total: f64 = scores
            .iter()
            .map(|&score| {
                let i = self
                    .steps
                    .partition_point(|&(required, _)| required <= score)
                    .saturating_sub(1);
                self.steps[i].1
            })
            .sum();
        let mean = total / scores.len() as f64;
        if self.error == 0.0 {
            mean
        } else {
            (mean.next_up() + self.error)
                .next_up()
                .min(self.steps.last().unwrap().1)
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum RouteKind {
    Live,
    Challenge,
    Skip,
}
#[derive(Clone)]
pub struct Route {
    pub kind: RouteKind,
    pub rate: f64,
    pub reward_rate: f64,
    pub points: BTreeMap<i32, i32>,
    pub rewards: Vec<Reward>,
    pub item_id: i32,
    pub challenge_points: BTreeMap<i32, i32>,
    pub effects: Vec<EventEffect>,
    /// The tables above by score rank, read on every bound.
    ranks: Vec<RankRow>,
}
#[derive(Clone, Default)]
struct RankRow {
    points: i32,
    challenge_points: i32,
    /// Item count and probability of each matching reward row, in table order.
    items: Vec<(i32, f64)>,
}
impl Route {
    pub fn new(
        master: &Master,
        event_id: i32,
        kind: RouteKind,
        consumption: i32,
    ) -> Result<Self, String> {
        let event = master
            .events
            .get(&event_id)
            .ok_or_else(|| format!("unknown-event:{event_id}"))?;
        let challenge =
            kind == RouteKind::Challenge || (kind == RouteKind::Skip && consumption > 10);
        if !challenge && ![0, 1, 2, 3, 4, 5, 10].contains(&consumption) {
            return Err("unsupported-live-boost-cost".into());
        }
        let table = if challenge {
            &master.challenge_boosts
        } else {
            &master.boosts
        };
        let boost = table.iter().find(|row| row.consumed == consumption);
        let rate = if !challenge && consumption < 1 {
            1.0
        } else {
            boost.map(|r| r.event_point_rate).unwrap_or(1.0)
        };
        let reward_rate = if !challenge && consumption < 1 {
            1.0
        } else {
            boost.map(|r| r.reward_rate).unwrap_or(1.0)
        };
        let mut route = Self {
            kind,
            rate,
            reward_rate,
            points: if challenge {
                event.challenge_points.clone()
            } else {
                event.live_points.clone()
            },
            rewards: if challenge {
                event.challenge_rewards.clone()
            } else {
                event.live_rewards.clone()
            },
            item_id: event.item_id,
            challenge_points: (*master.live_challenge_points).clone(),
            effects: event.effects.clone(),
            ranks: Vec::new(),
        };
        let top = route
            .points
            .keys()
            .chain(route.challenge_points.keys())
            .copied()
            .chain(route.rewards.iter().map(|r| r.score_rank))
            .max()
            .unwrap_or(0)
            .clamp(0, 64);
        route.ranks = (0..=top)
            .map(|rank| RankRow {
                points: *route.points.get(&rank).unwrap_or(&0),
                challenge_points: *route.challenge_points.get(&rank).unwrap_or(&0),
                items: route
                    .rewards
                    .iter()
                    .filter(|r| {
                        r.score_rank == rank
                            && (route.item_id == 0 || r.resource_id == route.item_id)
                    })
                    .map(|r| (r.count, r.probability))
                    .collect(),
            })
            .collect();
        Ok(route)
    }
    fn rank_row(&self, rank: i32) -> Option<&RankRow> {
        usize::try_from(rank).ok().and_then(|i| self.ranks.get(i))
    }
    pub fn points(&self, bonus: f64, rank: i32) -> f64 {
        let value = self.rank_row(rank).map_or(0, |row| row.points);
        let rate = i32(self.rate);
        let bonus = i32(bonus + 10000.0);
        let product = if self.kind == RouteKind::Challenge {
            imul(imul(value, rate), bonus)
        } else {
            imul(imul(bonus, rate), value)
        };
        (product as f64 / 10000.0).trunc()
    }
    pub fn items(&self, bonus: f64, rank: i32) -> f64 {
        let Some(row) = self.rank_row(rank) else {
            return 0.0;
        };
        row.items.iter().fold(0.0, |sum, &(count, probability)| {
            sum + (imul(imul(count, i32(bonus + 10000.0)), i32(self.reward_rate)) as f64 / 10000.0)
                .trunc()
                * probability
                / 10000.0
        })
    }
    pub fn cp(&self, rank: i32) -> f64 {
        if self.kind == RouteKind::Live {
            self.rank_row(rank).map_or(0, |row| row.challenge_points) as f64 * self.rate
        } else {
            0.0
        }
    }
}
fn top_five(values: impl Iterator<Item = f64>) -> f64 {
    let mut rows: Vec<_> = values.collect();
    rows.sort_by(|a, b| b.total_cmp(a));
    rows.into_iter().take(5).sum()
}
pub(crate) fn top_character_bonus(members: &[Member], values: &[f64]) -> f64 {
    let mut characters = HashMap::<i32, f64>::default();
    for (member, &value) in members.iter().zip(values) {
        characters
            .entry(member.card.character_id)
            .and_modify(|best| *best = best.max(value))
            .or_insert(value);
    }
    top_five(characters.into_values())
}
pub struct EventObjective {
    pub core: Option<LiveCore>,
    pub chart: Option<Chart>,
    pub route: Route,
    pub points_measure: bool,
    pub skip_rank: i32,
    pub cp_weight: f64,
    pub exchange: Option<(f64, f64)>,
    pub score_cap: f64,
    member_bonus: Vec<f64>,
    snap_bonus: Vec<f64>,
    other_member: Vec<f64>,
    other_snap: Vec<f64>,
    tie: f64,
    max_other: f64,
    other_snap_top: f64,
    other_member_suffix: Vec<Vec<f64>>,
    max_bonus: f64,
    suffix: [f64; 6],
    ordered: Vec<usize>,
    targets: HashMap<u64, Vec<JointTarget>>,
    curves: HashMap<(u64, u64), Option<RewardCurve>>,
    other_curves: HashMap<u64, Option<RewardCurve>>,
    profiles: Vec<[f64; 5]>,
    profile_ids: HashMap<[u64; 5], u32>,
    slot_profiles: HashMap<usize, u32>,
    profile_options: Vec<Vec<i32>>,
    member_profiles: Vec<u32>,
    assignment_profiles: [u32; 5],
    coefficients: HashMap<[u32; 5], Arc<Vec<f64>>>,
    coefficient_order: VecDeque<[u32; 5]>,
    global_profiles: Vec<Option<u32>>,
    suffix_profiles: Vec<u32>,
    snap_power: Arc<Vec<f64>>,
    snap_width: usize,
    base_power: f64,
    frontiers: Vec<[Vec<(f64, f64)>; 5]>,
    mean_frontiers: Vec<Vec<(f64, f64)>>,
}
impl EventObjective {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        master: &Master,
        members: &[Member],
        snaps: &[Snap],
        chart: Option<&Chart>,
        route: Route,
        points_measure: bool,
        play: &Value,
        primary_only: bool,
        cp_weight: f64,
        exchange: Option<(f64, f64)>,
        score_cap: f64,
    ) -> Self {
        let kind = if points_measure { 0 } else { 1 };
        let member_bonus: Vec<_> = members
            .iter()
            .map(|m| power::member_event_percent(&route.effects, m, kind) as f64)
            .collect();
        let snap_bonus: Vec<_> = snaps
            .iter()
            .map(|s| power::snap_event_percent(&route.effects, s, kind) as f64)
            .collect();
        let other_member: Vec<_> = members
            .iter()
            .map(|m| power::member_event_percent(&route.effects, m, 1 - kind) as f64)
            .collect();
        let other_snap: Vec<_> = snaps
            .iter()
            .map(|s| power::snap_event_percent(&route.effects, s, 1 - kind) as f64)
            .collect();
        let tie = if primary_only { 0.0 } else { 1.0 / 16777216.0 };
        let other_snap_top = top_five(other_snap.iter().map(|v| v.max(0.0)));
        let max_other = top_character_bonus(members, &other_member) + other_snap_top;
        let max_bonus =
            top_character_bonus(members, &member_bonus) + top_five(snap_bonus.iter().copied());
        let played = route.kind != RouteKind::Skip;
        Self {
            core: chart
                .filter(|_| played)
                .map(|c| LiveCore::new(master, c, members, snaps, play)),
            chart: chart.filter(|_| played).cloned(),
            route,
            points_measure,
            skip_rank: master.params.skip_rank,
            cp_weight,
            exchange,
            score_cap,
            member_bonus,
            snap_bonus,
            other_member,
            other_snap,
            tie,
            max_other,
            other_snap_top,
            other_member_suffix: Vec::new(),
            max_bonus,
            suffix: [0.0; 6],
            ordered: Vec::new(),
            targets: HashMap::default(),
            curves: HashMap::default(),
            other_curves: HashMap::default(),
            profiles: Vec::new(),
            profile_ids: HashMap::default(),
            slot_profiles: HashMap::default(),
            profile_options: vec![Vec::new(); members.len()],
            member_profiles: vec![u32::MAX; members.len()],
            assignment_profiles: [0; 5],
            coefficients: HashMap::default(),
            coefficient_order: VecDeque::new(),
            global_profiles: vec![None; members.len()],
            suffix_profiles: Vec::new(),
            snap_power: Arc::new(Vec::new()),
            snap_width: snaps.len() + 1,
            base_power: 0.0,
            frontiers: (0..members.len())
                .map(|_| std::array::from_fn(|_| Vec::new()))
                .collect(),
            mean_frontiers: vec![Vec::new(); members.len()],
        }
    }
    /// Ranks by `measure + cp·CP + other·(the other reward)`, the weights already divided by the measure's weight.
    pub fn with_weights(mut self, cp: f64, other: f64) -> Self {
        self.targets.clear();
        self.curves.clear();
        self.other_curves.clear();
        self.cp_weight = cp;
        self.exchange = None;
        self.tie = other;
        self
    }
    fn direct(&self, bonus: f64, rank: i32) -> f64 {
        if self.points_measure {
            self.route.points(bonus, rank)
        } else {
            self.route.items(bonus, rank)
        }
    }
    fn other(&self, bonus: f64, rank: i32) -> f64 {
        if self.points_measure {
            self.route.items(bonus, rank)
        } else {
            self.route.points(bonus, rank)
        }
    }
    fn value(&self, bonus: f64, rank: i32) -> f64 {
        let direct = self.direct(bonus, rank);
        let cp = self.route.cp(rank);
        if let Some((n, d)) = self.exchange {
            direct * d + cp * n
        } else {
            direct + self.cp_weight * cp
        }
    }
    fn reward_bound(&self, bonus: f64, rank: i32) -> f64 {
        self.value(bonus, rank) + self.tie * self.other(self.max_other, rank)
    }
    fn rank_points(&self) -> Vec<(i32, i32)> {
        std::iter::once((2, 0))
            .chain(
                self.chart
                    .as_ref()
                    .unwrap()
                    .ranks
                    .iter()
                    .filter(|r| r.rank > 2 && r.required as f64 <= self.score_cap)
                    .map(|r| (r.rank, r.required)),
            )
            .collect()
    }
    fn curve(&mut self, bonus: f64, other: f64) -> Option<&RewardCurve> {
        let key = (
            bonus.to_bits(),
            if self.tie == 0.0 { 0 } else { other.to_bits() },
        );
        if !self.curves.contains_key(&key) {
            let mut curve = RewardCurve::new(
                self.rank_points()
                    .into_iter()
                    .map(|(rank, score)| {
                        (
                            score,
                            self.value(bonus, rank) + self.tie * self.other(other, rank),
                        )
                    })
                    .collect(),
            );
            if self.tie != 0.0 {
                if let Some(curve) = &mut curve {
                    curve.error = curve.error.max(
                        curve.points.last().unwrap().y * (256.0 * f64::EPSILON)
                            / (1.0 - 256.0 * f64::EPSILON),
                    );
                }
            }
            if self.curves.len() >= 256 {
                self.curves.clear();
            }
            self.curves.insert(key, curve);
        }
        self.curves.get(&key).and_then(Option::as_ref)
    }
    fn mean_bound(&mut self, bonus: f64, other: f64, score: f64) -> f64 {
        self.curve(bonus, other)
            .map_or(f64::INFINITY, |curve| curve.upper(score))
    }
    fn other_mean_bound(&mut self, bonus: f64, score: f64) -> f64 {
        let key = bonus.to_bits();
        if !self.other_curves.contains_key(&key) {
            let curve = RewardCurve::new(
                self.rank_points()
                    .into_iter()
                    .map(|(rank, score)| (score, self.other(bonus, rank)))
                    .collect(),
            );
            if self.other_curves.len() >= 256 {
                self.other_curves.clear();
            }
            self.other_curves.insert(key, curve);
        }
        self.other_curves
            .get(&key)
            .and_then(Option::as_ref)
            .map_or(f64::INFINITY, |curve| curve.upper(score))
    }
    fn intern_profile(&mut self, row: [f64; 5]) -> u32 {
        let key = row.map(f64::to_bits);
        let next = self.profiles.len() as u32;
        *self.profile_ids.entry(key).or_insert_with(|| {
            self.profiles.push(row);
            next
        })
    }
    fn slot_profile(&mut self, member: usize, snap: i32) -> u32 {
        let shape = self.core.as_ref().unwrap().shape(member, snap);
        if let Some(&id) = self.slot_profiles.get(&shape) {
            return id;
        }
        let row = self
            .core
            .as_mut()
            .unwrap()
            .weight(member, snap)
            .high
            .per_event;
        let id = self.intern_profile(row);
        self.slot_profiles.insert(shape, id);
        id
    }
    fn order_coefficients(&mut self, mut ids: [u32; 5]) -> Arc<Vec<f64>> {
        ids.sort_unstable();
        if let Some(coefficients) = self.coefficients.get(&ids) {
            return Arc::clone(coefficients);
        }
        let core = self.core.as_ref().unwrap();
        let mut coefficients: Vec<_> = crate::live::orders()
            .iter()
            .map(|order| {
                let skill = order
                    .iter()
                    .enumerate()
                    .map(|(event, &slot)| self.profiles[ids[slot] as usize][event])
                    .sum::<f64>();
                (core.high.total + skill) / core.input.converted_count as f64
                    * (1.0 + RELATIVE_ERROR)
                    * (1.0 + 16.0 * f64::EPSILON)
            })
            .collect();
        coefficients.sort_by(f64::total_cmp);
        let coefficients = Arc::new(coefficients);
        if self.coefficients.len() >= 2048 {
            if let Some(key) = self.coefficient_order.pop_front() {
                self.coefficients.remove(&key);
            }
        }
        self.coefficient_order.push_back(ids);
        self.coefficients.insert(ids, Arc::clone(&coefficients));
        coefficients
    }
    pub(crate) fn score_upper_orders(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        power: f64,
    ) -> Option<[f64; 120]> {
        if self.core.as_ref()?.input.judgements.is_some() {
            return None;
        }
        let ids = std::array::from_fn(|slot| {
            if slot < from {
                self.slot_profile(members[slot], assigned[slot])
            } else {
                self.assignment_profiles[slot]
            }
        });
        let coefficients = self.order_coefficients(ids);
        Some(std::array::from_fn(|i| {
            (power * coefficients[i]).min(self.score_cap)
        }))
    }
    fn projection(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        t: Totals,
        other: f64,
    ) -> Option<(f64, f64)> {
        if self.core.as_ref()?.input.judgements.is_some() {
            return None;
        }
        let ids = std::array::from_fn(|slot| {
            if slot < from {
                self.slot_profile(members[slot], assigned[slot])
            } else {
                self.assignment_profiles[slot]
            }
        });
        let coefficients = self.order_coefficients(ids);
        let primary = self
            .curve(t.bonus, other)
            .map_or(f64::INFINITY, |curve| curve.orders(&coefficients, t.power));
        let key = other.to_bits();
        self.other_mean_bound(other, f64::INFINITY);
        let secondary = self
            .other_curves
            .get(&key)
            .and_then(Option::as_ref)
            .map_or(f64::INFINITY, |curve| curve.orders(&coefficients, t.power));
        Some((primary, secondary))
    }
    fn paired_projection(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        t: Totals,
        other: f64,
    ) -> Option<(f64, f64, f64)> {
        let core = self.core.as_ref()?;
        if core.input.judgements.is_some() {
            return None;
        }
        let (base, converted) = (core.high.total, core.input.converted_count as f64);
        let lambda = ((base + t.skill) / t.power).sqrt();
        if !lambda.is_finite() || lambda <= 0.0 {
            return None;
        }
        let mut rows = [[0.0; 5]; 5];
        for slot in 0..5 {
            let member = members[slot];
            if slot < from {
                let snap = assigned[slot];
                let id = self.slot_profile(member, snap);
                let power = self.snap_power[member * self.snap_width
                    + if snap < 0 {
                        self.snap_width - 1
                    } else {
                        snap as usize
                    }];
                for (event, value) in rows[slot].iter_mut().enumerate() {
                    *value = ((lambda * power).next_up()
                        + (self.profiles[id as usize][event] / lambda).next_up())
                    .next_up();
                }
            } else {
                for (event, value) in rows[slot].iter_mut().enumerate() {
                    *value = self.frontiers[member][event]
                        .iter()
                        .map(|&(power, weight)| {
                            ((lambda * power).next_up() + (weight / lambda).next_up()).next_up()
                        })
                        .fold(0.0, f64::max);
                }
            }
        }
        let constant = ((lambda * self.base_power).next_up() + (base / lambda).next_up()).next_up();
        let scores: [f64; 120] = std::array::from_fn(|i| {
            let sum = crate::live::orders()[i]
                .iter()
                .enumerate()
                .fold(constant, |sum, (event, &slot)| {
                    (sum + rows[slot][event]).next_up()
                });
            (((sum * sum).next_up() / (4.0 * converted)).next_up() * (1.0 + RELATIVE_ERROR))
                .next_up()
        });
        let mut mean_sum = constant;
        for slot in 0..5 {
            let member = members[slot];
            let value = if slot < from {
                let snap = assigned[slot];
                let id = self.slot_profile(member, snap);
                let power = self.snap_power[member * self.snap_width
                    + if snap < 0 {
                        self.snap_width - 1
                    } else {
                        snap as usize
                    }];
                ((lambda * power).next_up()
                    + (self.profiles[id as usize].iter().sum::<f64>() / 5.0 / lambda).next_up())
                .next_up()
            } else {
                self.mean_frontiers[member]
                    .iter()
                    .map(|&(power, weight)| {
                        ((lambda * power).next_up() + (weight / lambda).next_up()).next_up()
                    })
                    .fold(0.0, f64::max)
            };
            mean_sum = (mean_sum + value).next_up();
        }
        let mean_bound = (((mean_sum * mean_sum).next_up() / (4.0 * converted)).next_up()
            * (1.0 + RELATIVE_ERROR))
            .next_up();
        let mean = self.core.as_ref().unwrap().score_high(t).min(mean_bound);
        let primary = self
            .curve(t.bonus, other)
            .map_or(f64::INFINITY, |curve| curve.scores(&scores));
        self.other_mean_bound(other, f64::INFINITY);
        let secondary = self
            .other_curves
            .get(&other.to_bits())
            .and_then(Option::as_ref)
            .map_or(f64::INFINITY, |curve| curve.scores(&scores));
        Some((primary, secondary, mean))
    }
    fn global_profile(&mut self, member: usize) -> u32 {
        if let Some(profile) = self.global_profiles[member] {
            return profile;
        }
        let mut row = [0.0_f64; 5];
        for snap in -1..self.snap_bonus.len() as i32 {
            let profile = self.slot_profile(member, snap);
            for (event, value) in row.iter_mut().enumerate() {
                *value = value.max(self.profiles[profile as usize][event]);
            }
        }
        let profile = self.intern_profile(row);
        self.global_profiles[member] = Some(profile);
        profile
    }
    fn prepare_profile_members(&mut self, characters: &[Vec<usize>]) {
        if self
            .core
            .as_ref()
            .is_none_or(|core| core.input.judgements.is_some())
        {
            return;
        }
        let mut suffix = [0.0_f64; 5];
        self.suffix_profiles = vec![0; characters.len() + 1];
        self.suffix_profiles[characters.len()] = self.intern_profile(suffix);
        for (from, members) in characters.iter().enumerate().rev() {
            for &member in members {
                let id = self.global_profile(member);
                for (event, value) in suffix.iter_mut().enumerate() {
                    *value = value.max(self.profiles[id as usize][event]);
                }
            }
            self.suffix_profiles[from] = self.intern_profile(suffix);
        }
    }
    fn member_projection(
        &mut self,
        members: &[usize],
        t: Totals,
        from: usize,
        other: f64,
    ) -> Option<(f64, f64)> {
        if self.core.as_ref()?.input.judgements.is_some() {
            return None;
        }
        let ids = std::array::from_fn(|slot| {
            if slot < members.len() {
                self.global_profile(members[slot])
            } else {
                self.suffix_profiles[from]
            }
        });
        let coefficients = self.order_coefficients(ids);
        let primary = self
            .curve(t.bonus, other)
            .map_or(f64::INFINITY, |curve| curve.orders(&coefficients, t.power));
        self.other_mean_bound(other, f64::INFINITY);
        let secondary = self
            .other_curves
            .get(&other.to_bits())
            .and_then(Option::as_ref)
            .map_or(f64::INFINITY, |curve| curve.orders(&coefficients, t.power));
        Some((primary, secondary))
    }
    fn mean_targets(&mut self, limit: f64) -> Option<Vec<JointTarget>> {
        let targets = self.joint_targets(limit)?;
        let bonus = targets
            .iter()
            .map(|target| target.bonus)
            .fold(f64::INFINITY, f64::min);
        let scale =
            self.core.as_ref().unwrap().input.converted_count as f64 / (1.0 + RELATIVE_ERROR);
        let required = self
            .curve(self.max_bonus, self.max_other)
            .map_or(0.0, |curve| curve.required(limit));
        Some(if targets.is_empty() {
            Vec::new()
        } else {
            vec![JointTarget {
                bonus,
                product: (required * scale).next_down().max(0.0),
            }]
        })
    }
    fn other_snap(&self, snap: i32) -> f64 {
        if snap < 0 {
            0.0
        } else {
            self.other_snap[snap as usize]
        }
    }
    fn other_bonus(&self, team: &Team) -> f64 {
        team.members
            .iter()
            .map(|&m| self.other_member[m])
            .sum::<f64>()
            + team.snaps.iter().map(|&s| self.other_snap(s)).sum::<f64>()
    }
    fn assignment_other_bonus(&self, members: &[usize], assigned: &[i32], from: usize) -> f64 {
        let distinct = self
            .ordered
            .iter()
            .filter(|&&snap| !assigned[..from].contains(&(snap as i32)))
            .take(5 - from)
            .map(|&snap| self.other_snap[snap])
            .sum::<f64>();
        members
            .iter()
            .map(|&member| self.other_member[member])
            .sum::<f64>()
            + assigned[..from]
                .iter()
                .map(|&snap| self.other_snap(snap))
                .sum::<f64>()
            + self.suffix[from].min(distinct)
    }
    fn team_bonus(&self, team: &Team) -> f64 {
        team.members
            .iter()
            .map(|&m| self.member_bonus[m])
            .sum::<f64>()
            + team
                .snaps
                .iter()
                .filter(|&&s| s >= 0)
                .map(|&s| self.snap_bonus[s as usize])
                .sum::<f64>()
    }
    fn rank(&self, score: f64) -> i32 {
        chart::score_rank(self.chart.as_ref().unwrap(), score)
    }
    fn rank_bound(&self, score: f64) -> i32 {
        self.rank(score.min(self.score_cap))
    }
}
impl Objective for EventObjective {
    fn seed_objective(&self) -> Option<Box<dyn Objective>> {
        (!self.points_measure
            && self
                .core
                .as_ref()
                .is_some_and(|core| core.input.judgements.is_none()))
        .then(|| {
            Box::new(BonusSurrogate {
                members: self.member_bonus.clone(),
                snaps: self.snap_bonus.clone(),
                weight: 16777216.0,
            }) as Box<dyn Objective>
        })
    }
    fn seed_ceiling(&self) -> f64 {
        self.reward_bound(self.max_bonus, self.rank_bound(self.score_cap))
    }
    fn seed_rank(&self, key: f64, detail: &Value) -> [f64; 3] {
        [
            key,
            detail["otherMean"].as_f64().unwrap_or(0.0),
            detail["scores"]["mean"].as_f64().unwrap_or(0.0),
        ]
    }
    fn seed_skill(&mut self, member: usize, snap: i32) -> f64 {
        self.core
            .as_mut()
            .map_or(0.0, |core| core.weight(member, snap).high.mean)
    }
    fn seed_direct_bound(&self, totals: Totals) -> Option<f64> {
        if self.cp_weight <= 0.0 && self.exchange.is_none_or(|(numerator, _)| numerator <= 0.0) {
            return None;
        }
        self.core
            .as_ref()
            .map(|core| self.direct(totals.bonus, self.rank_bound(core.score_high(totals))))
    }
    fn seed_ready(&self, detail: &Value) -> bool {
        self.tie == 0.0
            && detail["bonus"].as_f64() == Some(self.max_bonus)
            && ["6", "7"].iter().any(|rank| {
                detail["ranks"][rank].as_u64() == Some(crate::live::orders().len() as u64)
            })
    }
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        self.core
            .as_ref()
            .is_none_or(|core| core.shape(member, a) == core.shape(member, b))
    }
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn canonical_leader(&self) -> bool {
        true
    }
    fn numeric_member_dominance(&mut self, better: usize, worse: usize, power_delta: f64) -> bool {
        self.other_member[better] >= self.other_member[worse]
            && self
                .core
                .as_mut()
                .is_some_and(|c| c.numeric_member_dominance(better, worse, power_delta))
    }
    fn needs_power_cap(&self) -> bool {
        self.core
            .as_ref()
            .is_some_and(|c| c.input.judgements.is_none())
    }
    fn prepare_dominance(&mut self, power_cap: f64) {
        if let Some(core) = &mut self.core {
            core.prepare_dominance(power_cap);
        }
    }
    fn numeric_snap_dominance(
        &mut self,
        member: usize,
        better: i32,
        worse: i32,
        power_delta: f64,
    ) -> bool {
        self.other_snap(better) >= self.other_snap(worse)
            && self
                .core
                .as_mut()
                .is_some_and(|c| c.numeric_dominance(member, better, worse, power_delta))
    }

    fn skill(&mut self, member: usize, snap: i32) -> (f64, f64) {
        if let Some(core) = &mut self.core {
            let w = core.weight(member, snap);
            (w.low.worst, w.high.best)
        } else {
            let bonus = if self.tie != 0.0 {
                self.other_member[member] + self.other_snap(snap)
            } else {
                0.0
            };
            (bonus, bonus)
        }
    }
    fn member_bonus(&self, member: usize) -> f64 {
        self.member_bonus[member]
    }
    fn snap_bonus(&self, snap: usize) -> f64 {
        self.snap_bonus[snap]
    }
    fn snap_dominance(&mut self, member: usize, snap: i32) -> Vec<f64> {
        let mut values = if self.tie != 0.0 {
            vec![self.other_snap(snap)]
        } else {
            Vec::new()
        };
        if let Some(core) = &self.core {
            let shape = core.shape(member, snap) as f64;
            values.extend([shape, -shape]);
        }
        values
    }
    fn prepare_power(&mut self, snapshots: Arc<Vec<f64>>, width: usize) {
        if *self.snap_power != *snapshots {
            self.member_profiles.fill(u32::MAX);
        }
        self.snap_power = snapshots;
        self.snap_width = width;
    }
    fn prepare_assignment_power(&mut self, power: f64) {
        self.base_power = power;
    }
    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        self.prepare_profile_members(characters);
        if self.tie != 0.0 {
            self.other_member_suffix = crate::search::suffix_tops(
                &characters
                    .iter()
                    .map(|members| {
                        members
                            .iter()
                            .map(|&member| self.other_member[member])
                            .fold(0.0, f64::max)
                    })
                    .collect::<Vec<_>>(),
            );
        }
    }
    fn member_bound_at(
        &mut self,
        members: &[usize],
        t: Totals,
        left: usize,
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        let other = if self.tie == 0.0 {
            self.max_other
        } else {
            members
                .iter()
                .map(|&member| self.other_member[member])
                .sum::<f64>()
                + self.other_member_suffix[from][left]
                + self.other_snap_top
        };
        self.member_projection(members, t, from, other)
            .map(|bounds| bounds.0)
    }
    fn product_base(&self) -> Option<f64> {
        self.core.as_ref().map(|c| c.high.total)
    }
    fn bound(&mut self, t: Totals) -> f64 {
        if let Some(core) = &self.core {
            self.reward_bound(t.bonus, self.rank_bound(core.score_high(t)))
        } else {
            self.value(t.bonus, self.skip_rank) + self.tie * self.other(t.skill, self.skip_rank)
        }
    }
    fn cheap_assignment_bound(
        &mut self,
        members: &[usize],
        totals: Totals,
        assigned: &[i32],
        from: usize,
    ) -> f64 {
        if self.tie == 0.0 || self.core.is_none() {
            return self.bound(totals);
        }
        let other = self.assignment_other_bonus(members, assigned, from);
        let rank = self.rank_bound(self.core.as_ref().unwrap().score_high(totals));
        self.value(totals.bonus, rank) + self.tie * self.other(other, rank)
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<JointTarget>> {
        let scale = self.core.as_ref()?.input.converted_count as f64 / (1.0 + RELATIVE_ERROR);
        if let Some(rows) = self.targets.get(&limit.to_bits()) {
            return Some(rows.clone());
        }
        let mut targets = Vec::new();
        for (rank, required) in self.rank_points() {
            if self.reward_bound(self.max_bonus, rank) < limit {
                continue;
            }
            let (mut low, mut high) = (0i64, self.max_bonus as i64);
            while low < high {
                let mid = (low + high) >> 1;
                if self.reward_bound(mid as f64, rank) >= limit {
                    high = mid;
                } else {
                    low = mid + 1;
                }
            }
            targets.push(JointTarget {
                bonus: low as f64,
                product: (required as f64 * scale).next_down().max(0.0),
            });
        }
        if self.targets.len() >= 64 {
            self.targets.clear();
        }
        self.targets.insert(limit.to_bits(), targets.clone());
        Some(targets)
    }
    fn bonus_floor(&mut self, limit: f64) -> Option<f64> {
        self.joint_targets(limit).map(|targets| {
            targets
                .iter()
                .map(|t| t.bonus)
                .fold(f64::INFINITY, f64::min)
        })
    }
    fn prepare_assignment(&mut self, _members: &[usize], options: &[Vec<i32>]) {
        if self.core.is_some() {
            for (slot, choices) in options.iter().enumerate() {
                let member = _members[slot];
                if self.member_profiles[member] == u32::MAX
                    || self.profile_options[member] != *choices
                {
                    let mut row = [0.0_f64; 5];
                    let mut frontiers: [Vec<(f64, f64)>; 5] = std::array::from_fn(|_| Vec::new());
                    let mut mean_frontier = Vec::new();
                    for &snap in choices {
                        let profile = self.slot_profile(member, snap);
                        let power = self.snap_power[member * self.snap_width
                            + if snap < 0 {
                                self.snap_width - 1
                            } else {
                                snap as usize
                            }];
                        mean_frontier.push((
                            power,
                            self.profiles[profile as usize].iter().sum::<f64>() / 5.0,
                        ));
                        for (event, frontier) in frontiers.iter_mut().enumerate() {
                            frontier.push((power, self.profiles[profile as usize][event]));
                        }
                        for (event, value) in row.iter_mut().enumerate() {
                            *value = value.max(self.profiles[profile as usize][event]);
                        }
                    }
                    for frontier in &mut frontiers {
                        frontier
                            .sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| b.1.total_cmp(&a.1)));
                        let mut high = f64::NEG_INFINITY;
                        frontier.retain(|&(_, value)| {
                            if value > high {
                                high = value;
                                true
                            } else {
                                false
                            }
                        });
                    }
                    mean_frontier.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.total_cmp(&a.1)));
                    let mut high = f64::NEG_INFINITY;
                    mean_frontier.retain(|&(_, value)| {
                        if value > high {
                            high = value;
                            true
                        } else {
                            false
                        }
                    });
                    self.mean_frontiers[member] = mean_frontier;
                    self.frontiers[member] = frontiers;
                    self.member_profiles[member] = self.intern_profile(row);
                    self.profile_options[member] = choices.clone();
                }
                self.assignment_profiles[slot] = self.member_profiles[member];
            }
        }
        if self.tie == 0.0 || self.core.is_none() {
            return;
        }
        self.suffix = [0.0; 6];
        for slot in (0..5).rev() {
            self.suffix[slot] = self.suffix[slot + 1]
                + options[slot]
                    .iter()
                    .map(|&s| self.other_snap(s))
                    .fold(f64::NEG_INFINITY, f64::max);
        }
        self.ordered = options
            .iter()
            .flatten()
            .filter(|&&s| s >= 0 && self.other_snap(s) > 0.0)
            .map(|&s| s as usize)
            .collect();
        self.ordered.sort_unstable();
        self.ordered.dedup();
        self.ordered
            .sort_by(|&a, &b| self.other_snap[b].total_cmp(&self.other_snap[a]));
    }
    fn assignment_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
    ) -> Option<f64> {
        if self.tie == 0.0 || self.core.is_none() {
            return None;
        }
        let score = self
            .core
            .as_ref()
            .unwrap()
            .score_high(t)
            .min(self.score_cap);
        let bonus = self.assignment_other_bonus(members, assigned, from);
        Some(self.mean_bound(t.bonus, bonus, score))
    }
    fn assignment_order_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
        limit: f64,
    ) -> Option<f64> {
        let other = if self.tie == 0.0 {
            self.max_other
        } else {
            self.assignment_other_bonus(members, assigned, from)
        };
        let upper = self.projection(members, assigned, from, t, other)?.0;
        if upper <= limit {
            return Some(upper);
        }
        Some(
            self.paired_projection(members, assigned, from, t, other)
                .map_or(upper, |bounds| upper.min(bounds.0)),
        )
    }
    fn leaf_bound(&mut self, team: &Team, t: Totals) -> Option<f64> {
        let other = self.other_bonus(team);
        if let Some(bounds) = self.projection(&team.members, &team.snaps, 5, t, other) {
            return Some(bounds.0);
        }

        if let Some(core) = &mut self.core {
            let skill = team
                .members
                .iter()
                .enumerate()
                .map(|(slot, &m)| core.weight(m, team.snaps[slot]).high.mean)
                .sum();
            let score = core
                .team_high(team, Totals { skill, ..t })
                .min(self.score_cap);
            Some(self.mean_bound(t.bonus, other, score))
        } else {
            Some(self.value(t.bonus, self.skip_rank) + self.tie * self.other(other, self.skip_rank))
        }
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
        let other = self.other_bonus(team);
        let Some(core) = &self.core else {
            let key =
                self.value(t.bonus, self.skip_rank) + self.tie * self.other(other, self.skip_rank);
            return (key, key);
        };
        let rank = self.rank(core.score_low(t));
        let score = core.score_high(t).min(self.score_cap);
        let low = self
            .rank_points()
            .into_iter()
            .filter(|&(r, _)| r >= rank)
            .map(|(r, _)| self.value(t.bonus, r) + self.tie * self.other(other, r))
            .fold(f64::INFINITY, f64::min);
        (low, self.mean_bound(t.bonus, other, score))
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        let bonus = self.team_bonus(team);
        let other = self.other_bonus(team);
        let Some(core) = &mut self.core else {
            let mean = self.value(bonus, self.skip_rank);
            let direct = self.direct(bonus, self.skip_rank);
            let other_mean = self.other(other, self.skip_rank);
            return Ok(Exact {
                key: mean + self.tie * other_mean,
                detail: json!({"mean":mean,"bonus":bonus,"directMean":direct,"otherMean":other_mean,"directMin":direct,"directMax":direct}),
            });
        };
        let scores = core.evaluate(team, power as i32)?;
        let mut per_order = Vec::with_capacity(scores.scores.len());
        let mut ranks = BTreeMap::<i32, i32>::new();
        let (mut total, mut cp, mut reward_sum, mut direct_min, mut direct_max, mut other_sum) =
            (0.0, 0.0, 0.0, f64::INFINITY, f64::NEG_INFINITY, 0.0);
        for &score in &scores.scores {
            let rank = self.rank(score);
            *ranks.entry(rank).or_default() += 1;
            let value = self.value(bonus, rank);
            per_order.push(value);
            total += value;
            cp += self.route.cp(rank);
            let reward = self.direct(bonus, rank);
            reward_sum += reward;
            direct_min = direct_min.min(reward);
            direct_max = direct_max.max(reward);
            other_sum += self.other(other, rank);
        }
        let count = scores.scores.len() as f64;
        let mean = (total / count).clamp(
            per_order.iter().copied().fold(f64::INFINITY, f64::min),
            per_order.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        );
        let mut other_min = f64::INFINITY;
        let mut other_max = f64::NEG_INFINITY;
        let mut key_min = f64::INFINITY;
        let mut key_max = f64::NEG_INFINITY;
        for &score in &scores.scores {
            let rank = self.rank(score);
            let other = self.other(other, rank);
            other_min = other_min.min(other);
            other_max = other_max.max(other);
            let key = self.value(bonus, rank) + self.tie * other;
            key_min = key_min.min(key);
            key_max = key_max.max(key);
        }
        let other_mean = (other_sum / count).clamp(other_min, other_max);
        Ok(Exact {
            key: (mean + self.tie * other_mean).clamp(key_min, key_max),
            detail: json!({"scores":&*scores,"perOrder":per_order,"mean":mean,"bonus":bonus,"ranks":ranks,"rewardSum":reward_sum,"comparisonSum":total,"directMin":direct_min,"directMax":direct_max,"directMean":reward_sum/count,"otherMean":other_mean,"challengePoints":cp/count}),
        })
    }
}

/// Refine an exact primary-reward group by the other reward, then mean score.
pub struct ScoreTie {
    pub event: EventObjective,
    pub mean: crate::objectives::LiveObjective,
    pub target: f64,
    pub excluded: Vec<Vec<usize>>,
    pub rank_other: bool,
    pub allow_dominance: bool,
    pub other_target: Option<f64>,
    other_suffix: [f64; 6],
    member_suffix: Vec<Vec<f64>>,
    other_snaps: f64,
    other_ordered: Vec<i32>,
    other_order: Vec<i32>,
    other_allowed: Vec<bool>,
    prepared_options: Vec<Vec<i32>>,
    floor: Option<Option<f64>>,
    reward_targets: Option<Vec<JointTarget>>,
    mean_targets: Option<Vec<JointTarget>>,
}
impl ScoreTie {
    pub fn new(
        mut event: EventObjective,
        mean: crate::objectives::LiveObjective,
        target: f64,
        teams: &[Team],
    ) -> Self {
        let excluded = teams
            .iter()
            .map(|t| {
                let mut key = t.members.clone();
                key.sort_unstable();
                key
            })
            .collect();
        let other_snaps = top_five(event.other_snap.iter().copied());
        let mut other_order: Vec<_> = (0..event.other_snap.len()).map(|i| i as i32).collect();
        other_order.sort_by(|&a, &b| event.other_snap(b).total_cmp(&event.other_snap(a)));
        let other_allowed = vec![false; event.other_snap.len()];
        let reward_targets = event.joint_targets(target);
        let mean_targets = event.mean_targets(target);
        Self {
            event,
            mean,
            target,
            excluded,
            rank_other: false,
            allow_dominance: false,
            other_target: None,
            other_suffix: [0.0; 6],
            member_suffix: Vec::new(),
            other_snaps,
            other_ordered: Vec::new(),
            other_order,
            other_allowed,
            prepared_options: Vec::new(),
            floor: None,
            reward_targets,
            mean_targets,
        }
    }
    fn other_upper(&mut self, t: Totals, bonus: f64) -> f64 {
        let score = self.mean.core.score_high(t).min(self.event.score_cap);
        if self.rank_other {
            self.event.other(bonus, self.event.rank_bound(score))
        } else {
            self.event.other_mean_bound(bonus, score)
        }
    }
    fn ranked_upper(&mut self, t: Totals, bonus: f64) -> f64 {
        if (if self.rank_other {
            &self.reward_targets
        } else {
            &self.mean_targets
        })
        .as_ref()
        .is_some_and(|targets| {
            let product = t.power * (self.mean.core.high.total + t.skill);
            !targets
                .iter()
                .any(|target| t.bonus >= target.bonus && product >= target.product)
        }) {
            return -1.0;
        }
        let upper = self.other_upper(t, bonus);
        if self.other_target.is_some_and(|target| upper < target) {
            -1.0
        } else if self.rank_other {
            upper
        } else {
            self.mean.bound(t)
        }
    }
    fn eligible(&self, team: &Team) -> bool {
        !self.excludes(&team.members)
    }
    fn excludes(&self, members: &[usize]) -> bool {
        if self.excluded.is_empty() {
            return false;
        }
        let mut key = [0; 5];
        let key = &mut key[..members.len()];
        key.copy_from_slice(members);
        key.sort_unstable();
        self.excluded.iter().any(|team| team[..] == key[..])
    }
    fn event_totals(&mut self, team: &Team, t: Totals) -> Totals {
        let (mut low, mut high) = (0.0, 0.0);
        for (slot, &member) in team.members.iter().enumerate() {
            let (a, b) = self.event.skill(member, team.snaps[slot]);
            low += a;
            high += b;
        }
        Totals {
            skill: high,
            skill_low: low,
            ..t
        }
    }
}
impl Objective for ScoreTie {
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        self.mean.core.shape(member, a) == self.mean.core.shape(member, b)
    }
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn canonical_leader(&self) -> bool {
        true
    }
    fn numeric_member_dominance(&mut self, better: usize, worse: usize, power_delta: f64) -> bool {
        self.excluded.is_empty()
            && self
                .event
                .numeric_member_dominance(better, worse, power_delta)
    }
    fn needs_power_cap(&self) -> bool {
        self.event.needs_power_cap()
    }
    fn prepare_dominance(&mut self, power_cap: f64) {
        self.event.prepare_dominance(power_cap);
    }
    fn numeric_snap_dominance(
        &mut self,
        member: usize,
        better: i32,
        worse: i32,
        power_delta: f64,
    ) -> bool {
        self.event
            .numeric_snap_dominance(member, better, worse, power_delta)
    }

    fn preserve_snap_options(&self) -> bool {
        !self.allow_dominance
    }
    fn snap_dominance(&mut self, member: usize, snap: i32) -> Vec<f64> {
        let shape = self.mean.core.shape(member, snap) as f64;
        vec![self.event.other_snap(snap), shape, -shape]
    }
    fn skill(&mut self, m: usize, s: i32) -> (f64, f64) {
        if self.rank_other {
            self.event.skill(m, s)
        } else {
            self.mean.skill(m, s)
        }
    }
    fn member_bonus(&self, m: usize) -> f64 {
        self.event.member_bonus(m)
    }
    fn snap_bonus(&self, s: usize) -> f64 {
        self.event.snap_bonus(s)
    }
    fn bound(&mut self, t: Totals) -> f64 {
        self.ranked_upper(t, self.event.max_other)
    }
    fn prepare_power(&mut self, snapshots: Arc<Vec<f64>>, width: usize) {
        self.event.prepare_power(snapshots, width);
    }
    fn prepare_assignment_power(&mut self, power: f64) {
        self.event.prepare_assignment_power(power);
    }
    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        self.event.prepare_profile_members(characters);
        self.member_suffix = crate::search::suffix_tops(
            &characters
                .iter()
                .map(|members| {
                    members
                        .iter()
                        .map(|&m| self.event.other_member[m])
                        .fold(f64::NEG_INFINITY, f64::max)
                })
                .collect::<Vec<_>>(),
        );
    }
    fn member_bound_at(
        &mut self,
        members: &[usize],
        t: Totals,
        left: usize,
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        if self
            .bonus_floor(f64::NEG_INFINITY)
            .is_some_and(|floor| t.bonus < floor - 1e-6)
        {
            return Some(-1.0);
        }
        let bonus = members
            .iter()
            .map(|&m| self.event.other_member[m])
            .sum::<f64>()
            + self.member_suffix[from][left]
            + self.other_snaps;
        if let Some((primary, other)) = self.event.member_projection(members, t, from, bonus) {
            if primary < self.target || self.other_target.is_some_and(|target| other < target) {
                return Some(-1.0);
            }
            if self.rank_other {
                return Some(other);
            }
        }
        Some(self.ranked_upper(t, bonus))
    }
    fn product_base(&self) -> Option<f64> {
        self.mean.product_base()
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<JointTarget>> {
        if self.rank_other {
            self.reward_targets.clone()
        } else {
            let score = (limit.max(0.0) * self.mean.core.input.converted_count as f64
                / (1.0 + RELATIVE_ERROR))
                .next_down()
                .max(0.0);
            self.mean_targets.as_ref().map(|targets| {
                targets
                    .iter()
                    .map(|t| JointTarget {
                        bonus: t.bonus,
                        product: t.product.max(score),
                    })
                    .collect()
            })
        }
    }
    fn bonus_floor(&mut self, _limit: f64) -> Option<f64> {
        let target = self.target;
        let event = &mut self.event;
        *self.floor.get_or_insert_with(|| {
            event.joint_targets(target).map(|targets| {
                targets
                    .iter()
                    .map(|t| t.bonus)
                    .fold(f64::INFINITY, f64::min)
            })
        })
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.event.prepare_assignment(members, options);
        if self.prepared_options != options {
            self.other_allowed.fill(false);
            for &s in options.iter().flatten() {
                if s >= 0 {
                    self.other_allowed[s as usize] = true;
                }
            }
            self.other_ordered.clear();
            self.other_ordered.extend(
                self.other_order
                    .iter()
                    .copied()
                    .filter(|&s| self.other_allowed[s as usize]),
            );
            self.other_suffix = [0.0; 6];
            for slot in (0..5).rev() {
                self.other_suffix[slot] = self.other_suffix[slot + 1]
                    + options[slot]
                        .iter()
                        .map(|&s| self.event.other_snap(s))
                        .fold(f64::NEG_INFINITY, f64::max);
            }
            self.prepared_options = options.to_vec();
        }
    }
    fn assignment_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
    ) -> Option<f64> {
        if self.excludes(members) {
            return Some(-1.0);
        }
        let bound = if self.rank_other {
            self.event
                .assignment_bound(members, t, assigned, from)
                .unwrap_or_else(|| self.event.bound(t))
        } else {
            f64::INFINITY
        };
        let distinct = self
            .other_ordered
            .iter()
            .filter(|s| !assigned[..from].contains(s))
            .take(5 - from)
            .map(|&s| self.event.other_snap(s))
            .sum::<f64>();
        let bonus = members
            .iter()
            .map(|&m| self.event.other_member[m])
            .sum::<f64>()
            + assigned
                .iter()
                .take(from)
                .map(|&s| self.event.other_snap(s))
                .sum::<f64>()
            + self.other_suffix[from].min(distinct);
        Some(if bound < self.target {
            -1.0
        } else {
            self.ranked_upper(t, bonus)
        })
    }
    fn assignment_order_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
        limit: f64,
    ) -> Option<f64> {
        let distinct = self
            .other_ordered
            .iter()
            .filter(|s| !assigned[..from].contains(s))
            .take(5 - from)
            .map(|&s| self.event.other_snap(s))
            .sum::<f64>();
        let bonus = members
            .iter()
            .map(|&m| self.event.other_member[m])
            .sum::<f64>()
            + assigned[..from]
                .iter()
                .map(|&s| self.event.other_snap(s))
                .sum::<f64>()
            + self.other_suffix[from].min(distinct);
        let (primary, other) = self.event.projection(members, assigned, from, t, bonus)?;
        let upper =
            if primary < self.target || self.other_target.is_some_and(|target| other < target) {
                -1.0
            } else if self.rank_other {
                other
            } else {
                self.mean.bound(t)
            };
        if upper <= limit {
            return Some(upper);
        }
        if let Some((primary, other, mean)) = self
            .event
            .paired_projection(members, assigned, from, t, bonus)
        {
            if primary < self.target || self.other_target.is_some_and(|target| other < target) {
                return Some(-1.0);
            }
            return Some(upper.min(if self.rank_other { other } else { mean }));
        }
        Some(upper)
    }
    fn leaf_bound(&mut self, team: &Team, t: Totals) -> Option<f64> {
        if !self.eligible(team) {
            return Some(-1.0);
        }
        let totals = self.event_totals(team, t);
        let bound = self
            .event
            .leaf_bound(team, totals)
            .unwrap_or_else(|| self.event.bound(totals));
        Some(if bound < self.target {
            -1.0
        } else {
            self.ranked_upper(t, self.event.other_bonus(team))
        })
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
        if !self.eligible(team) {
            return (-1.0, -1.0);
        }
        let totals = self.event_totals(team, t);
        let (low, high) = self.event.interval(team, totals);
        if high < self.target || low > self.target {
            return (-1.0, -1.0);
        }
        let bonus = self.event.other_bonus(team);
        let (lows, highs) = self
            .event
            .core
            .as_mut()
            .unwrap()
            .order_bounds(team, t.power);
        let other_low = lows
            .iter()
            .map(|&v| self.event.other(bonus, self.event.rank(v)))
            .sum::<f64>()
            / 120.0;
        let other_high = highs
            .iter()
            .map(|&v| self.event.other(bonus, self.event.rank_bound(v)))
            .sum::<f64>()
            / 120.0;
        if self
            .other_target
            .is_some_and(|target| other_low > target || other_high < target)
        {
            return (-1.0, -1.0);
        }
        if self.rank_other {
            return (
                if low == self.target && high == self.target {
                    other_low
                } else {
                    -1.0
                },
                other_high,
            );
        }
        let (a, b) = self.mean.interval(team, t);
        (
            if low == self.target
                && high == self.target
                && self
                    .other_target
                    .is_none_or(|target| other_low == target && other_high == target)
            {
                a
            } else {
                -1.0
            },
            b,
        )
    }
    fn exact(&mut self, team: &Team, power: f64, slots: &[f64]) -> Result<Exact, String> {
        let result = self.event.exact(team, power, slots)?;
        let other = crate::master::number(&result.detail["otherMean"], 0.0);
        let key = if self.eligible(team)
            && result.key == self.target
            && self.other_target.is_none_or(|target| target == other)
        {
            if self.rank_other {
                other
            } else {
                crate::master::number(&result.detail["scores"]["mean"], 0.0)
            }
        } else {
            -1.0
        };
        Ok(Exact {
            key,
            detail: result.detail,
        })
    }
}

pub struct SkipTie {
    pub event: EventObjective,
    pub target: f64,
}
impl Objective for SkipTie {
    fn canonical_leader(&self) -> bool {
        true
    }
    fn preserve_snap_options(&self) -> bool {
        true
    }
    fn skill(&mut self, m: usize, s: i32) -> (f64, f64) {
        let bonus = self.event.other_member[m] + self.event.other_snap(s);
        (bonus, bonus)
    }
    fn member_bonus(&self, m: usize) -> f64 {
        self.event.member_bonus[m]
    }
    fn snap_bonus(&self, s: usize) -> f64 {
        self.event.snap_bonus[s]
    }
    fn bound(&mut self, t: Totals) -> f64 {
        if self.event.reward_bound(t.bonus, self.event.skip_rank) < self.target {
            -1.0
        } else {
            self.event.other(t.skill, self.event.skip_rank)
        }
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
        let primary = self.event.value(t.bonus, self.event.skip_rank)
            + self.event.tie * self.event.other(t.skill, self.event.skip_rank);
        let key = if primary == self.target {
            self.event
                .other(self.event.other_bonus(team), self.event.skip_rank)
        } else {
            -1.0
        };
        (key, key)
    }
    fn exact(&mut self, team: &Team, power: f64, slots: &[f64]) -> Result<Exact, String> {
        let mut result = self.event.exact(team, power, slots)?;
        result.key = if result.key == self.target {
            crate::master::number(&result.detail["otherMean"], 0.0)
        } else {
            -1.0
        };
        Ok(result)
    }
}

pub struct BonusSurrogate {
    pub members: Vec<f64>,
    pub snaps: Vec<f64>,
    pub weight: f64,
}
impl Objective for BonusSurrogate {
    fn exchange_snap_equivalent(&mut self, _member: usize, _a: i32, _b: i32) -> bool {
        true
    }
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn canonical_leader(&self) -> bool {
        true
    }
    fn linear_assignment(&self) -> Option<(f64, f64, f64)> {
        Some((1.0, 0.0, self.weight))
    }
    fn skill(&mut self, _m: usize, _s: i32) -> (f64, f64) {
        (0.0, 0.0)
    }
    fn member_bonus(&self, m: usize) -> f64 {
        self.members[m]
    }
    fn snap_bonus(&self, s: usize) -> f64 {
        self.snaps[s]
    }
    fn bound(&mut self, t: Totals) -> f64 {
        t.bonus * self.weight + t.power
    }
    fn interval(&mut self, _team: &Team, t: Totals) -> (f64, f64) {
        let key = self.bound(t);
        (key, key)
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        let bonus = team.members.iter().map(|&m| self.members[m]).sum::<f64>()
            + team
                .snaps
                .iter()
                .filter(|&&s| s >= 0)
                .map(|&s| self.snaps[s as usize])
                .sum::<f64>();
        Ok(Exact {
            key: bonus * self.weight + power,
            detail: Value::Null,
        })
    }
}
pub struct ChallengePoints {
    pub mean: crate::objectives::LiveObjective,
    pub chart: Chart,
    pub route: Route,
    curve: Option<RewardCurve>,
}
impl ChallengePoints {
    pub fn new(mean: crate::objectives::LiveObjective, chart: Chart, route: Route) -> Self {
        let curve = RewardCurve::new(
            std::iter::once((2, 0))
                .chain(
                    chart
                        .ranks
                        .iter()
                        .filter(|r| r.rank > 2)
                        .map(|r| (r.rank, r.required)),
                )
                .map(|(rank, score)| {
                    (
                        score,
                        *route.challenge_points.get(&rank).unwrap_or(&0) as f64 * route.rate,
                    )
                })
                .collect(),
        );
        Self {
            mean,
            chart,
            route,
            curve,
        }
    }
    fn value(&self, score: f64) -> f64 {
        *self
            .route
            .challenge_points
            .get(&chart::score_rank(&self.chart, score))
            .unwrap_or(&0) as f64
            * self.route.rate
    }
}
impl Objective for ChallengePoints {
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        self.mean.exchange_snap_equivalent(member, a, b)
    }
    fn needs_power_cap(&self) -> bool {
        self.mean.needs_power_cap()
    }
    fn prepare_dominance(&mut self, power: f64) {
        self.mean.prepare_dominance(power);
    }
    fn numeric_snap_dominance(&mut self, m: usize, a: i32, b: i32, power: f64) -> bool {
        self.mean.numeric_snap_dominance(m, a, b, power)
    }
    fn numeric_member_dominance(&mut self, a: usize, b: usize, power: f64) -> bool {
        self.mean.numeric_member_dominance(a, b, power)
    }
    fn snap_dominance(&mut self, m: usize, s: i32) -> Vec<f64> {
        self.mean.snap_dominance(m, s)
    }

    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn canonical_leader(&self) -> bool {
        true
    }
    fn skill(&mut self, m: usize, s: i32) -> (f64, f64) {
        self.mean.skill(m, s)
    }
    fn bound(&mut self, t: Totals) -> f64 {
        let score = self.mean.bound(t);
        self.curve
            .as_ref()
            .map_or(f64::INFINITY, |curve| curve.upper(score))
            + score / 68719476736.0
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
        let (lows, highs) = self.mean.core.order_bounds(team, t.power);
        let count = lows.len() as f64;
        (
            lows.iter().map(|&s| self.value(s)).sum::<f64>() / count
                + lows.iter().sum::<f64>() / count / 68719476736.0,
            highs.iter().map(|&s| self.value(s)).sum::<f64>() / count
                + highs.iter().sum::<f64>() / count / 68719476736.0,
        )
    }
    fn exact(&mut self, team: &Team, power: f64, slots: &[f64]) -> Result<Exact, String> {
        let result = self.mean.exact(team, power, slots)?;
        let scores = &result.detail["scores"];
        let per_order: Vec<_> = crate::master::rows(&scores["scores"])
            .into_iter()
            .map(|s| self.value(crate::master::number(s, 0.0)))
            .collect();
        let mean = per_order.iter().sum::<f64>() / per_order.len() as f64;
        Ok(Exact {
            key: mean + crate::master::number(&scores["mean"], 0.0) / 68719476736.0,
            detail: json!({"scores":scores,"perOrder":per_order,"mean":mean,"bonus":0,"challengePoints":mean}),
        })
    }
}
