//! Explicit reward sums and ordered reward-only tie levels.
use crate::{
    chart::Chart,
    event::{EventObjective, Route, RouteKind},
    power::{self, Member, Snap},
    rewards::RewardObjective,
    search::{self, Cutoff, Evaluation, Exact, Hit, JointTarget, Objective, Team, Totals},
};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{ToPrimitive, Zero};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Metric {
    Points,
    ChallengePoints,
    Items,
}
impl Metric {
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Points => 0,
            Self::ChallengePoints => 1,
            Self::Items => 2,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Term {
    pub metric: Metric,
    pub weight: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Song {
    pub song_id: i32,
    pub difficulty: i32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AutoCp {
    pub songs: Vec<Song>,
    pub consumption: i32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Plan {
    pub unit: u32,
    pub levels: Vec<Vec<Term>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_cp: Option<AutoCp>,
}
impl Plan {
    pub(crate) fn parse(value: &Value) -> Result<Self, String> {
        let plan: Self = serde_json::from_value(value.clone()).map_err(|_| "reward-plan-format")?;
        if plan.unit == 0
            || plan.unit > 1_000_000_000
            || plan.levels.is_empty()
            || plan.levels.len() > 3
        {
            return Err("reward-plan-levels".into());
        }
        let mut used = [false; 3];
        for (index, level) in plan.levels.iter().enumerate() {
            if level.is_empty() || level.len() > 3 || index > 0 && level.len() != 1 {
                return Err("reward-plan-levels".into());
            }
            for term in level {
                if term.weight == 0
                    || term.weight > 9_007_199_254_740_991
                    || used[term.metric.index()]
                {
                    return Err("reward-plan-terms".into());
                }
                used[term.metric.index()] = true;
            }
        }
        if plan.levels[0][0].weight != plan.unit as u64 {
            return Err("reward-plan-unit".into());
        }
        if let Some(auto) = &plan.auto_cp {
            if auto.songs.is_empty()
                || auto.consumption <= 0
                || used[Metric::ChallengePoints.index()]
            {
                return Err("reward-plan-challenge".into());
            }
        }
        Ok(plan)
    }
    pub(crate) fn uses(&self, metric: Metric) -> bool {
        self.levels
            .iter()
            .flatten()
            .any(|term| term.metric == metric)
    }
    pub(crate) fn anchor(&self) -> Metric {
        self.levels
            .iter()
            .flatten()
            .find(|term| term.metric != Metric::ChallengePoints)
            .map_or(Metric::Points, |term| term.metric)
    }
    pub(crate) fn rank(&self, rewards: &[Fraction; 3]) -> Vec<Fraction> {
        self.levels
            .iter()
            .map(|level| {
                Fraction(level.iter().fold(BigRational::zero(), |sum, term| {
                    sum + &rewards[term.metric.index()].0 * BigInt::from(term.weight)
                }))
            })
            .collect()
    }
    pub(crate) fn upper(&self, rewards: [f64; 3]) -> Vec<f64> {
        self.levels
            .iter()
            .map(|level| {
                level.iter().fold(0.0, |sum, term| {
                    let raw = rewards[term.metric.index()];
                    let product = raw * term.weight as f64;
                    let product = if raw.fract() == 0.0 && product <= (1u64 << 53) as f64 {
                        product
                    } else {
                        product.next_up()
                    };
                    let total = sum + product;
                    if sum.fract() == 0.0 && product.fract() == 0.0 && total <= (1u64 << 53) as f64
                    {
                        total
                    } else {
                        total.next_up()
                    }
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct Fraction(BigRational);
impl Fraction {
    pub(crate) fn new(numerator: u128, denominator: u128) -> Self {
        Self(BigRational::new(
            BigInt::from(numerator),
            BigInt::from(denominator),
        ))
    }
    pub(crate) fn from_value(value: f64) -> Option<Self> {
        BigRational::from_float(value).map(Self)
    }
    pub(crate) fn value(&self) -> f64 {
        self.0.to_f64().unwrap_or(f64::INFINITY)
    }
    pub(crate) fn lower(&self) -> f64 {
        let value = self.value();
        if BigRational::from_float(value).is_none_or(|rounded| rounded > self.0) {
            value.next_down()
        } else {
            value
        }
    }
    pub(crate) fn upper(&self) -> f64 {
        let value = self.value();
        if BigRational::from_float(value).is_some_and(|rounded| rounded < self.0) {
            value.next_up()
        } else {
            value
        }
    }
    pub(crate) fn divided(&self, value: u32) -> Self {
        Self(&self.0 / BigInt::from(value))
    }
    pub(crate) fn rounded(&self) -> Option<u64> {
        self.0.round().to_integer().to_u64()
    }
    pub(crate) fn json(&self) -> Value {
        json!({"numerator":self.0.numer().to_string(), "denominator":self.0.denom().to_string(), "value":self.value()})
    }
    pub(crate) fn from_json(value: &Value) -> Option<Self> {
        let numerator: BigInt = value["numerator"].as_str()?.parse().ok()?;
        let denominator: BigInt = value["denominator"].as_str()?.parse().ok()?;
        (numerator >= BigInt::zero() && denominator > BigInt::zero())
            .then(|| Self(BigRational::new(numerator, denominator)))
    }
}

pub(crate) fn ranking(value: &Value) -> Option<Vec<Fraction>> {
    value.as_array()?.iter().map(Fraction::from_json).collect()
}
pub(crate) fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    Some(ranking(a)?.cmp(&ranking(b)?))
}

pub(crate) enum Source<'a> {
    Ordinary(Box<EventObjective>),
    Gekiso(Box<RewardObjective<'a>>),
}
impl<'a> Source<'a> {
    fn objective(&mut self) -> &mut (dyn Objective + 'a) {
        match self {
            Self::Ordinary(value) => value.as_mut(),
            Self::Gekiso(value) => value.as_mut(),
        }
    }
    fn view(&self) -> &(dyn Objective + 'a) {
        match self {
            Self::Ordinary(value) => value.as_ref(),
            Self::Gekiso(value) => value.as_ref(),
        }
    }
    fn score_upper(&self, totals: Totals) -> f64 {
        match self {
            Self::Ordinary(value) => value.score_upper(totals),
            Self::Gekiso(value) => value.score_upper(totals),
        }
    }
}

pub(crate) struct Context<'a> {
    pub plan: Plan,
    pub route: Route,
    pub chart: Option<&'a Chart>,
    pub members: &'a [Member],
    pub snaps: &'a [Snap],
    pub k: usize,
    pub floor: f64,
    pub skip_rank: i32,
    pub observations: usize,
}
type BonusCache = rustc_hash::FxHashMap<(u64, i32), f64>;
type TeamKey = ([usize; 5], [i32; 5], u64);
pub(crate) struct Ranked<'a> {
    source: Source<'a>,
    context: Context<'a>,
    anchor: usize,
    member_bonus: [Vec<f64>; 2],
    snap_bonus: [Vec<f64>; 2],
    bonus_max: [f64; 2],
    snap_top: [f64; 2],
    member_suffix: [Vec<Vec<f64>>; 2],
    choice_suffix: [[f64; 6]; 2],
    payout_cache: [BonusCache; 2],
    caps: [Fraction; 3],
    cap_levels: Vec<Fraction>,
    incumbent: Option<Vec<Fraction>>,
    raw_floor: f64,
    rounding: f64,
    integer_rewards: bool,
    last: Option<(TeamKey, [f64; 3])>,
}
impl<'a> Ranked<'a> {
    pub(crate) fn new(source: Source<'a>, context: Context<'a>) -> Self {
        let anchor = usize::from(context.plan.anchor() == Metric::Items);
        let member_bonus = std::array::from_fn(|kind| {
            context
                .members
                .iter()
                .map(|member| {
                    power::member_event_percent(&context.route.effects, member, kind as i32) as f64
                })
                .collect::<Vec<_>>()
        });
        let snap_bonus = std::array::from_fn(|kind| {
            context
                .snaps
                .iter()
                .map(|snap| {
                    power::snap_event_percent(&context.route.effects, snap, kind as i32) as f64
                })
                .collect::<Vec<_>>()
        });
        let snap_top = snap_bonus.each_ref().map(|row| {
            let mut row = row.clone();
            row.sort_unstable_by(|a, b| b.total_cmp(a));
            row.into_iter()
                .take(5)
                .map(|value| value.max(0.0))
                .sum::<f64>()
        });
        let bonus_max = std::array::from_fn(|kind| {
            crate::event::top_character_bonus(context.members, &member_bonus[kind]) + snap_top[kind]
        });
        let global_rank = if context.route.kind == RouteKind::Skip {
            context.skip_rank
        } else if let Source::Ordinary(value) = &source {
            context
                .chart
                .unwrap()
                .ranks
                .iter()
                .filter(|row| row.required as f64 <= value.score_cap)
                .map(|row| row.rank)
                .fold(2, i32::max)
        } else {
            7
        };
        let mut units = [0u128; 3];
        for rank in 2..=global_rank {
            let row = context.route.reward_units(bonus_max[0], bonus_max[1], rank);
            for (high, value) in units.iter_mut().zip(row) {
                *high = (*high).max(value);
            }
        }
        let caps = units.map(|value| Fraction::new(value, 10_000));
        let cap_levels = context.plan.rank(&caps);
        let fractional = anchor == 1 && context.route.fractional_items();
        let steps = (context.observations as f64 * 8.0 + 1024.0) * f64::EPSILON;
        let rounding = caps[if anchor == 0 { 0 } else { 2 }].upper() * steps / (1.0 - steps);
        let integer_rewards = !fractional
            && caps[if anchor == 0 { 0 } else { 2 }].upper() * context.observations as f64
                <= (1u64 << 53) as f64;
        let mut result = Self {
            source,
            context,
            anchor,
            member_bonus,
            snap_bonus,
            bonus_max,
            snap_top,
            member_suffix: std::array::from_fn(|_| Vec::new()),
            choice_suffix: [[0.0; 6]; 2],
            payout_cache: std::array::from_fn(|_| BonusCache::default()),
            caps,
            cap_levels,
            incumbent: None,
            raw_floor: f64::NEG_INFINITY,
            rounding,
            integer_rewards,
            last: None,
        };
        result.update_floor();
        result
    }
    fn key(team: &Team, power: f64) -> TeamKey {
        (
            std::array::from_fn(|i| team.members[i]),
            std::array::from_fn(|i| team.snaps[i]),
            power.to_bits(),
        )
    }
    fn snap_value(&self, kind: usize, snap: i32) -> f64 {
        if snap < 0 {
            0.0
        } else {
            self.snap_bonus[kind][snap as usize]
        }
    }
    fn bonuses(&self, members: &[usize], assigned: &[i32], from: usize) -> [f64; 2] {
        std::array::from_fn(|kind| {
            members
                .iter()
                .map(|&member| self.member_bonus[kind][member])
                .sum::<f64>()
                + assigned
                    .iter()
                    .take(from)
                    .map(|&snap| self.snap_value(kind, snap))
                    .sum::<f64>()
                + self.choice_suffix[kind][from]
        })
    }
    fn rank_cap(&self, score: f64) -> i32 {
        if self.context.route.kind == RouteKind::Skip {
            return self.context.skip_rank;
        }
        let gekiso = matches!(self.source, Source::Gekiso(_));
        self.context
            .chart
            .unwrap()
            .ranks
            .iter()
            .filter(|row| {
                (if gekiso {
                    row.battle_required
                } else {
                    row.required
                }) as f64
                    <= score
            })
            .map(|row| row.rank)
            .fold(2, i32::max)
    }
    fn raw_upper(&mut self, totals: Totals, bonuses: [f64; 2], anchor_upper: f64) -> [f64; 3] {
        let rank = self.rank_cap(self.source.score_upper(totals));
        let mut result = [0.0_f64; 3];
        for (kind, &bonus) in bonuses.iter().enumerate() {
            if self.payout_cache[kind].len() >= 4096 {
                self.payout_cache[kind].clear();
            }
            let key = (bonus.floor().to_bits(), rank);
            let value = *self.payout_cache[kind].entry(key).or_insert_with(|| {
                let mut maximum = 0;
                for grade in 2..=rank {
                    maximum = maximum.max(
                        self.context.route.reward_units(
                            if kind == 0 { bonus.floor() } else { 0.0 },
                            if kind == 1 { bonus.floor() } else { 0.0 },
                            grade,
                        )[if kind == 0 { 0 } else { 2 }],
                    );
                }
                Fraction::new(maximum, 10_000).upper()
            });
            result[if kind == 0 { 0 } else { 2 }] = value;
        }
        result[1] = (2..=rank)
            .map(|grade| self.context.route.cp(grade))
            .fold(0.0, f64::max);
        let at = if self.anchor == 0 { 0 } else { 2 };
        let source = if self.integer_rewards && anchor_upper.fract() == 0.0 {
            anchor_upper
        } else {
            (anchor_upper + self.rounding).next_up()
        };
        result[at] = result[at].min(source);
        result
    }
    fn upper(&self, rewards: [f64; 3]) -> f64 {
        let values = self.context.plan.upper(rewards);
        if let Some(incumbent) = &self.incumbent {
            if *incumbent == self.cap_levels {
                return incumbent[0].lower().next_down();
            }
            for (&upper, known) in values.iter().zip(incumbent) {
                match upper.total_cmp(&known.lower()) {
                    Ordering::Greater => return values[0],
                    Ordering::Less => return values[0].min(incumbent[0].lower().next_down()),
                    Ordering::Equal => {}
                }
            }
            return values[0].min(incumbent[0].lower().next_down());
        }
        values[0]
    }
    fn primary_floor(&self, value: f64) -> f64 {
        let level = &self.context.plan.levels[0];
        let Some(own) = level
            .iter()
            .find(|term| term.metric == self.context.plan.anchor())
        else {
            return f64::NEG_INFINITY;
        };
        if !value.is_finite() {
            return f64::NEG_INFINITY;
        }
        let Some(value) = Fraction::from_value(value.next_down()) else {
            return f64::NEG_INFINITY;
        };
        let others = level
            .iter()
            .filter(|term| term.metric != own.metric)
            .fold(BigRational::zero(), |sum, term| {
                sum + &self.caps[term.metric.index()].0 * BigInt::from(term.weight)
            });
        Fraction((value.0 - others) / BigInt::from(own.weight)).lower() - self.rounding
    }
    fn update_floor(&mut self) {
        self.raw_floor = self.primary_floor(self.context.floor);
        let Some(incumbent) = &self.incumbent else {
            return;
        };
        for (index, level) in self.context.plan.levels.iter().enumerate() {
            if let Some(own) = level
                .iter()
                .find(|term| term.metric == self.context.plan.anchor())
            {
                let others = level
                    .iter()
                    .filter(|term| term.metric != own.metric)
                    .fold(BigRational::zero(), |sum, term| {
                        sum + &self.caps[term.metric.index()].0 * BigInt::from(term.weight)
                    });
                let floor = Fraction((&incumbent[index].0 - others) / BigInt::from(own.weight))
                    .lower()
                    - self.rounding;
                self.raw_floor = self.raw_floor.max(floor);
                break;
            }
            if self.cap_levels[index] > incumbent[index] {
                break;
            }
        }
    }
    fn finish(&mut self, team: &Team, mut exact: Exact) -> Result<Exact, String> {
        let bonuses: [f64; 2] = std::array::from_fn(|kind| {
            team.members
                .iter()
                .enumerate()
                .map(|(slot, &member)| {
                    self.member_bonus[kind][member] + self.snap_value(kind, team.snaps[slot])
                })
                .sum()
        });
        let gekiso = matches!(self.source, Source::Gekiso(_));
        let settlement = if gekiso {
            &exact.detail["settlement"]
        } else {
            &exact.detail
        };
        let ranks = settlement["ranks"].as_object().and_then(|rows| {
            rows.iter()
                .map(|(rank, count)| Some((rank.parse::<i32>().ok()?, count.as_u64()?)))
                .collect::<Option<Vec<_>>>()
        });
        let rewards = if self.context.route.kind == RouteKind::Skip {
            self.context
                .route
                .reward_units(bonuses[0], bonuses[1], self.context.skip_rank)
                .map(|value| Fraction::new(value, 10_000))
        } else if let Some(rows) = ranks.filter(|rows| rows.iter().any(|&(_, count)| count > 0)) {
            let mut totals = [0u128; 3];
            let mut observations = 0u128;
            for (rank, count) in rows {
                let units = self
                    .context
                    .route
                    .reward_units(bonuses[0], bonuses[1], rank);
                for (total, value) in totals.iter_mut().zip(units) {
                    *total += value * count as u128;
                }
                observations += count as u128;
            }
            totals.map(|total| Fraction::new(total, observations * 10_000))
        } else {
            let (direct, other, cp) = if gekiso {
                let count = settlement["observations"]
                    .as_f64()
                    .ok_or("reward-plan-observations")?;
                (
                    settlement["rewardSum"]
                        .as_f64()
                        .ok_or("reward-plan-value")?
                        / count,
                    settlement["otherMean"]
                        .as_f64()
                        .ok_or("reward-plan-value")?,
                    settlement["cpSum"].as_f64().ok_or("reward-plan-value")? / count,
                )
            } else {
                (
                    settlement["directMean"]
                        .as_f64()
                        .ok_or("reward-plan-value")?,
                    settlement["otherMean"]
                        .as_f64()
                        .ok_or("reward-plan-value")?,
                    settlement["challengePoints"]
                        .as_f64()
                        .ok_or("reward-plan-value")?,
                )
            };
            let values = if self.anchor == 0 {
                [direct, cp, other]
            } else {
                [other, cp, direct]
            };
            let mut caps = [0u128; 3];
            for rank in 2..=7 {
                for (cap, value) in caps.iter_mut().zip(
                    self.context
                        .route
                        .reward_units(bonuses[0], bonuses[1], rank),
                ) {
                    *cap = (*cap).max(value);
                }
            }
            let rows = values
                .into_iter()
                .zip(caps)
                .map(|(value, cap)| {
                    Fraction::from_value(value.min(Fraction::new(cap, 10_000).value()))
                        .ok_or("reward-plan-value")
                })
                .collect::<Result<Vec<_>, _>>()?;
            rows.try_into().unwrap()
        };
        let ranked = self.context.plan.rank(&rewards);
        exact.key = ranked[0].value();
        exact.detail["rewardRanking"] =
            json!(ranked.iter().map(Fraction::json).collect::<Vec<_>>());
        exact.detail["rewardValues"] = json!({
            "points":rewards[0].json(), "challenge-points":rewards[1].json(), "items":rewards[2].json()
        });
        exact.detail["rewardPlan"] = json!(self.context.plan);
        exact.detail["planMean"] = json!(ranked[0].divided(self.context.plan.unit).value());
        Ok(exact)
    }
    fn cp_members(&mut self, rewards: &mut [f64; 3], members: &[usize], from: usize, power: f64) {
        if self.context.plan.uses(Metric::ChallengePoints) {
            if let Source::Ordinary(source) = &mut self.source {
                if let Some(bound) = source.cp_member_upper(members, from, power) {
                    rewards[1] = rewards[1].min(if bound.fract() == 0.0 {
                        bound
                    } else {
                        bound.next_up()
                    });
                }
            }
        }
    }
    fn cp_assignment(
        &mut self,
        rewards: &mut [f64; 3],
        members: &[usize],
        assigned: &[i32],
        from: usize,
        power: f64,
    ) {
        if self.context.plan.uses(Metric::ChallengePoints) {
            if let Source::Ordinary(source) = &mut self.source {
                if let Some(bound) = source.cp_assignment_upper(members, assigned, from, power) {
                    rewards[1] = rewards[1].min(if bound.fract() == 0.0 {
                        bound
                    } else {
                        bound.next_up()
                    });
                }
            }
        }
    }
    fn team_upper(&mut self, team: &Team, totals: Totals, anchor_upper: f64) -> [f64; 3] {
        let bonuses = self.bonuses(&team.members, &team.snaps, 5);
        let mut result = self.raw_upper(totals, bonuses, anchor_upper);
        self.cp_assignment(&mut result, &team.members, &team.snaps, 5, totals.power);
        self.last = Some((Self::key(team, totals.power), result));
        result
    }
    fn prepare_choices(&mut self, options: &[Vec<i32>]) {
        for kind in 0..2 {
            self.choice_suffix[kind][5] = 0.0;
            for slot in (0..5).rev() {
                let high = options[slot]
                    .iter()
                    .map(|&snap| self.snap_value(kind, snap))
                    .fold(f64::NEG_INFINITY, f64::max);
                self.choice_suffix[kind][slot] =
                    (self.choice_suffix[kind][slot + 1] + high).next_up();
            }
        }
    }
}

impl Objective for Ranked<'_> {
    fn score_ties(&self) -> bool {
        true
    }
    fn seed_objective(&self) -> Option<Box<dyn Objective>> {
        self.source.view().seed_objective()
    }
    fn compare_rank(&self, a: &Hit, b: &Hit) -> Option<Ordering> {
        compare(&a.detail["rewardRanking"], &b.detail["rewardRanking"])
    }
    fn observe_hits(&mut self, hits: &[Hit]) {
        self.incumbent = hits
            .get(self.context.k - 1)
            .and_then(|hit| ranking(&hit.detail["rewardRanking"]));
        self.update_floor();
    }
    fn seed_ceiling(&self) -> f64 {
        if self.context.plan.levels.len() == 1 {
            self.cap_levels[0].value()
        } else {
            f64::INFINITY
        }
    }
    fn seed_ready(&self, detail: &Value) -> bool {
        ranking(&detail["rewardRanking"]).is_some_and(|values| {
            for (index, level) in self.context.plan.levels.iter().enumerate() {
                if values[index] != self.cap_levels[index] {
                    return false;
                }
                if level
                    .iter()
                    .any(|term| term.metric == self.context.plan.anchor())
                {
                    return true;
                }
            }
            false
        })
    }

    fn seed_rank(&self, key: f64, detail: &Value) -> [f64; 3] {
        [
            key,
            detail["rewardRanking"][1]["value"].as_f64().unwrap_or(0.0),
            detail["rewardRanking"][2]["value"].as_f64().unwrap_or(0.0),
        ]
    }
    fn seed_skill(&mut self, member: usize, snap: i32) -> f64 {
        self.source.objective().seed_skill(member, snap)
    }
    fn conditional_snap_dominance(&self) -> bool {
        self.source.view().conditional_snap_dominance()
    }
    fn canonical_leader(&self) -> bool {
        self.source.view().canonical_leader()
    }
    fn needs_power_cap(&self) -> bool {
        self.source.view().needs_power_cap()
    }
    fn prepare_dominance(&mut self, cap: f64) {
        self.source.objective().prepare_dominance(cap);
    }
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        [Metric::Points, Metric::Items]
            .iter()
            .enumerate()
            .all(|(kind, &metric)| {
                !self.context.plan.uses(metric)
                    || self.snap_value(kind, a) == self.snap_value(kind, b)
            })
            && self
                .source
                .objective()
                .exchange_snap_equivalent(member, a, b)
    }
    fn numeric_member_dominance(&mut self, better: usize, worse: usize, delta: f64) -> bool {
        [Metric::Points, Metric::Items]
            .iter()
            .enumerate()
            .all(|(kind, &metric)| {
                !self.context.plan.uses(metric)
                    || self.member_bonus[kind][better] >= self.member_bonus[kind][worse]
            })
            && self
                .source
                .objective()
                .numeric_member_dominance(better, worse, delta)
    }
    fn numeric_snap_dominance(
        &mut self,
        member: usize,
        better: i32,
        worse: i32,
        delta: f64,
    ) -> bool {
        [Metric::Points, Metric::Items]
            .iter()
            .enumerate()
            .all(|(kind, &metric)| {
                !self.context.plan.uses(metric)
                    || self.snap_value(kind, better) >= self.snap_value(kind, worse)
            })
            && self
                .source
                .objective()
                .numeric_snap_dominance(member, better, worse, delta)
    }
    fn skill(&mut self, member: usize, snap: i32) -> (f64, f64) {
        self.source.objective().skill(member, snap)
    }
    fn member_bonus(&self, member: usize) -> f64 {
        self.source.view().member_bonus(member)
    }
    fn snap_bonus(&self, snap: usize) -> f64 {
        self.source.view().snap_bonus(snap)
    }
    fn snap_dominance(&mut self, member: usize, snap: i32) -> Vec<f64> {
        let mut values = self.source.objective().snap_dominance(member, snap);
        for (kind, metric) in [Metric::Points, Metric::Items].into_iter().enumerate() {
            if self.context.plan.uses(metric) {
                values.push(self.snap_value(kind, snap));
            }
        }
        values
    }
    fn preserve_snap_options(&self) -> bool {
        self.source.view().preserve_snap_options()
    }
    fn bound(&mut self, totals: Totals) -> f64 {
        let raw = self.source.objective().bound(totals);
        let mut bonuses = self.bonus_max;
        bonuses[self.anchor] = bonuses[self.anchor].min(totals.bonus);
        let rewards = self.raw_upper(totals, bonuses, raw);
        self.upper(rewards)
    }
    fn cheap_assignment_bound(
        &mut self,
        members: &[usize],
        totals: Totals,
        assigned: &[i32],
        from: usize,
    ) -> f64 {
        let raw = self
            .source
            .objective()
            .cheap_assignment_bound(members, totals, assigned, from);
        let bonuses = self.bonuses(members, assigned, from);
        let mut rewards = self.raw_upper(totals, bonuses, raw);
        self.cp_assignment(&mut rewards, members, assigned, from, totals.power);
        self.upper(rewards)
    }
    fn prepare_power(&mut self, power: Arc<Vec<f64>>, width: usize) {
        self.source.objective().prepare_power(power, width);
    }
    fn prepare_assignment_power(&mut self, power: f64) {
        self.source.objective().prepare_assignment_power(power);
    }
    fn prepare_member_domains_at(&mut self, leader: usize, power: &[f64], options: &[Vec<i32>]) {
        self.source
            .objective()
            .prepare_member_domains_at(leader, power, options);
    }
    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        self.source.objective().prepare_members(characters);
        self.member_suffix = std::array::from_fn(|kind| {
            search::suffix_tops(
                &characters
                    .iter()
                    .map(|group| {
                        group
                            .iter()
                            .map(|&member| self.member_bonus[kind][member])
                            .fold(0.0, f64::max)
                    })
                    .collect::<Vec<_>>(),
            )
        });
    }
    fn member_bound_at(
        &mut self,
        members: &[usize],
        totals: Totals,
        left: usize,
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        let floor = self.raw_floor;
        let raw = self
            .source
            .objective()
            .member_bound_at(members, totals, left, from, floor)
            .unwrap_or_else(|| self.source.objective().bound(totals));
        let bonuses = std::array::from_fn(|kind| {
            members
                .iter()
                .map(|&member| self.member_bonus[kind][member])
                .sum::<f64>()
                + self.member_suffix[kind][from][left]
                + self.snap_top[kind]
        });
        let mut rewards = self.raw_upper(totals, bonuses, raw);
        self.cp_members(&mut rewards, members, from, totals.power);
        Some(self.upper(rewards))
    }
    fn early_member_bound(&mut self, members: &[usize], from: usize, _limit: f64) -> Option<f64> {
        let floor = self.raw_floor;
        let raw = self
            .source
            .objective()
            .early_member_bound(members, from, floor)?;
        let bonuses = std::array::from_fn(|kind| {
            members
                .iter()
                .map(|&member| self.member_bonus[kind][member])
                .sum::<f64>()
                + self.member_suffix[kind][from][5 - members.len()]
                + self.snap_top[kind]
        });
        let totals = Totals {
            power: f64::INFINITY,
            bonus: bonuses[self.anchor],
            ..Totals::default()
        };
        let rewards = self.raw_upper(totals, bonuses, raw);
        Some(self.upper(rewards))
    }
    fn roster_bound(&mut self, members: &[usize], totals: Totals, _limit: f64) -> Option<f64> {
        let floor = self.raw_floor;
        let raw = self
            .source
            .objective()
            .roster_bound(members, totals, floor)
            .unwrap_or_else(|| self.source.objective().bound(totals));
        let bonuses = std::array::from_fn(|kind| {
            members
                .iter()
                .map(|&member| self.member_bonus[kind][member])
                .sum::<f64>()
                + self.snap_top[kind]
        });
        let mut rewards = self.raw_upper(totals, bonuses, raw);
        self.cp_members(&mut rewards, members, 0, totals.power);
        Some(self.upper(rewards))
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.source.objective().prepare_assignment(members, options);
        self.prepare_choices(options);
    }
    fn prepare_assignment_domain(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
        complete: bool,
    ) {
        self.source
            .objective()
            .prepare_assignment_domain(members, options, complete);
        self.prepare_choices(options);
    }
    fn assignment_order_bound(
        &mut self,
        members: &[usize],
        totals: Totals,
        assigned: &[i32],
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        let floor = self.raw_floor;
        let raw = self
            .source
            .objective()
            .assignment_order_bound(members, totals, assigned, from, floor)
            .unwrap_or_else(|| self.source.objective().bound(totals));
        let bonuses = self.bonuses(members, assigned, from);
        let mut rewards = self.raw_upper(totals, bonuses, raw);
        self.cp_assignment(&mut rewards, members, assigned, from, totals.power);
        Some(self.upper(rewards))
    }
    fn generic_assignment_product(&self) -> bool {
        self.source.view().generic_assignment_product()
    }
    fn member_snapshot_prices(&self) -> bool {
        self.source.view().member_snapshot_prices()
    }
    fn product_base(&self) -> Option<f64> {
        self.source.view().product_base()
    }
    fn joint_targets(&mut self, _limit: f64) -> Option<Vec<JointTarget>> {
        self.source.objective().joint_targets(self.raw_floor)
    }
    fn bonus_floor(&mut self, _limit: f64) -> Option<f64> {
        self.source.objective().bonus_floor(self.raw_floor)
    }
    fn prefer_snap(&self, member: usize, snap: i32) -> f64 {
        self.source.view().prefer_snap(member, snap)
    }
    fn prefer_member(&self, member: usize) -> f64 {
        self.source.view().prefer_member(member)
    }
    fn leaf_bound(&mut self, team: &Team, totals: Totals) -> Option<f64> {
        let raw = self
            .source
            .objective()
            .leaf_bound(team, totals)
            .unwrap_or_else(|| self.source.objective().bound(totals));
        let rewards = self.team_upper(team, totals, raw);
        Some(self.upper(rewards))
    }
    fn interval(&mut self, team: &Team, totals: Totals) -> (f64, f64) {
        let (_, raw) = self.source.objective().interval(team, totals);
        let rewards = self.team_upper(team, totals, raw);
        (0.0, self.upper(rewards))
    }
    fn exact(&mut self, team: &Team, power: f64, slots: &[f64]) -> Result<Exact, String> {
        let result = self.source.objective().exact(team, power, slots)?;
        self.finish(team, result)
    }
    fn evaluate(
        &mut self,
        team: &Team,
        power: f64,
        slots: &[f64],
        cutoff: Cutoff,
    ) -> Result<Evaluation, String> {
        let rewards = if let Some((_, rewards)) = self
            .last
            .as_ref()
            .filter(|(key, _)| *key == Self::key(team, power))
        {
            *rewards
        } else {
            let skill = team
                .members
                .iter()
                .zip(&team.snaps)
                .map(|(&member, &snap)| self.source.objective().skill(member, snap).1)
                .sum();
            let totals = Totals {
                power,
                skill,
                bonus: team
                    .members
                    .iter()
                    .enumerate()
                    .map(|(slot, &member)| {
                        self.member_bonus[self.anchor][member]
                            + self.snap_value(self.anchor, team.snaps[slot])
                    })
                    .sum(),
                ..Totals::default()
            };
            let raw = self.source.objective().bound(totals);
            self.team_upper(team, totals, raw)
        };
        let upper = self.upper(rewards);
        if upper < cutoff.key {
            return Ok(Evaluation::Pruned(upper));
        }
        let floor = self.raw_floor.max(self.primary_floor(cutoff.key));
        match self.source.objective().evaluate(
            team,
            power,
            slots,
            Cutoff {
                key: floor,
                prune_equal: false,
            },
        )? {
            Evaluation::Exact(result) => self.finish(team, result).map(Evaluation::Exact),
            Evaluation::Pruned(raw) => {
                let mut rewards = rewards;
                rewards[if self.anchor == 0 { 0 } else { 2 }] = raw + self.rounding;
                let upper = self.upper(rewards);
                if upper < cutoff.key {
                    Ok(Evaluation::Pruned(upper))
                } else {
                    self.exact(team, power, slots).map(Evaluation::Exact)
                }
            }
        }
    }
}

pub(crate) fn decorate(value: &mut Value, detail: &Value) {
    if detail["rewardRanking"].is_null() {
        return;
    }
    value["rewardRanking"] = detail["rewardRanking"].clone();
    value["event"]["rewardValues"] = detail["rewardValues"].clone();
    value["event"]["rewardPlan"] = detail["rewardPlan"].clone();
    value["event"]["mean"] = detail["planMean"].clone();
}
