//! Finite Live Boost plans settle the complete discrete CP distribution.
use crate::{
    event::{EventObjective, Route},
    search::{Exact, JointTarget, Objective, Team, Totals},
};
use rustc_hash::FxHashMap as HashMap;
use serde_json::json;
use std::{cell::RefCell, rc::Rc, sync::Arc};

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}
fn convolution(a: &[f64], b: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; a.len()];
    for (i, &x) in a.iter().enumerate().filter(|(_, x)| **x != 0.0) {
        for (j, &y) in b.iter().enumerate().filter(|(_, y)| **y != 0.0) {
            out[(i + j) % a.len()] += x * y;
        }
    }
    let sum: f64 = out.iter().sum();
    for value in &mut out {
        *value /= sum;
    }
    out
}
pub(crate) struct Settlement {
    pub lives: u64,
    pub starting: u64,
    pub cost: u64,
    pub points: f64,
    constant: Option<u64>,
    cache: RefCell<HashMap<Vec<(u64, u64)>, f64>>,
}
impl Settlement {
    pub fn new(lives: u64, starting: u64, cost: u64, points: f64, route: &Route) -> Rc<Self> {
        let maximum = route
            .challenge_points
            .keys()
            .map(|&rank| route.cp(rank) as u64)
            .max()
            .unwrap_or(0);
        let minimum = route.cp(2) as u64;
        let constant = if cost == 0 {
            Some(0)
        } else {
            let low = (starting as u128 + lives as u128 * minimum as u128) / cost as u128;
            let high = (starting as u128 + lives as u128 * maximum as u128) / cost as u128;
            (low == high).then_some(low as u64)
        };
        Rc::new(Self {
            lives,
            starting,
            cost,
            points,
            constant,
            cache: RefCell::new(HashMap::default()),
        })
    }
    pub fn weight(&self) -> f64 {
        if self.constant.is_some() || self.points == 0.0 {
            0.0
        } else {
            (self.points / self.cost as f64).next_up()
        }
    }
    fn offset(&self) -> f64 {
        self.constant
            .map_or(self.starting as f64 * self.weight(), |count| {
                count as f64 * self.points
            })
    }
    fn upper(&self, value: f64) -> f64 {
        if self.lives == 0 {
            self.offset()
        } else {
            ((self.lives as f64 * value).next_up() + self.offset().next_up()).next_up()
        }
    }
    fn lower(&self, value: f64) -> f64 {
        if self.lives == 0 {
            return self.offset();
        }
        let loss = if self.constant.is_some() {
            0.0
        } else {
            self.points
        };
        (((self.lives as f64 * value).next_down() + self.offset()).next_down() - loss)
            .next_down()
    }
    fn limit(&self, value: f64) -> f64 {
        if self.lives == 0 {
            return f64::NEG_INFINITY;
        }
        ((value - self.offset().next_up()).next_down() / self.lives as f64).next_down()
    }
    fn expected(&self, law: &[(u64, u64)]) -> f64 {
        if let Some(count) = self.constant {
            return count as f64;
        }
        if let Some(&count) = self.cache.borrow().get(law) {
            return count;
        }
        let count = if law.len() == 1 {
            ((self.starting as u128 + self.lives as u128 * law[0].0 as u128) / self.cost as u128)
                as f64
        } else {
            let divisor = law.iter().fold(self.cost, |d, &(cp, _)| gcd(d, cp));
            let width = (self.cost / divisor) as usize;
            let samples = law.iter().map(|&(_, count)| count).sum::<u64>() as f64;
            let mean = law
                .iter()
                .map(|&(cp, count)| cp as f64 * count as f64 / samples)
                .sum::<f64>();
            let mut power = vec![0.0; width];
            for &(cp, count) in law {
                power[(cp / divisor % width as u64) as usize] += count as f64 / samples;
            }
            let mut total = vec![0.0; width];
            total[(self.starting / divisor % width as u64) as usize] = 1.0;
            let mut lives = self.lives;
            while lives != 0 {
                if lives & 1 != 0 {
                    total = convolution(&total, &power);
                }
                lives >>= 1;
                if lives != 0 {
                    power = convolution(&power, &power);
                }
            }
            let remainder = (self.starting % divisor) as f64
                + divisor as f64
                    * total
                        .iter()
                        .enumerate()
                        .map(|(i, &p)| i as f64 * p)
                        .sum::<f64>();
            let minimum =
                (self.starting as u128 + self.lives as u128 * law[0].0 as u128) / self.cost as u128;
            let maximum = (self.starting as u128
                + self.lives as u128 * law.last().unwrap().0 as u128)
                / self.cost as u128;
            ((self.starting as f64 + self.lives as f64 * mean - remainder) / self.cost as f64)
                .clamp(minimum as f64, maximum as f64)
        };
        let mut cache = self.cache.borrow_mut();
        if cache.len() >= 4096 {
            cache.clear();
        }
        cache.insert(law.to_vec(), count);
        count
    }
    fn apply(&self, exact: &mut Exact, route: &Route) {
        let mut values = std::collections::BTreeMap::<u64, u64>::new();
        for (rank, count) in exact.detail["ranks"].as_object().unwrap() {
            *values
                .entry(route.cp(rank.parse().unwrap()) as u64)
                .or_default() += count.as_u64().unwrap();
        }
        let law: Vec<_> = values.into_iter().collect();
        let challenges = self.expected(&law);
        let cp = crate::master::number(&exact.detail["challengePoints"], 0.0);
        let direct = crate::master::number(&exact.detail["directMean"], 0.0);
        exact.key = self.lives as f64 * direct + challenges * self.points;
        exact.detail["plan"] = json!({"challengeLives":challenges,"challengePointsEarned":self.lives as f64*cp,"leftoverChallengePoints":self.starting as f64+self.lives as f64*cp-challenges*self.cost as f64,"eventPoints":exact.key});
    }
}

pub(crate) struct Finite {
    pub event: EventObjective,
    pub settlement: Rc<Settlement>,
}
impl Finite {
    fn conservative(&self) -> bool {
        self.event.nonmonotone() || self.settlement.points < 0.0
    }
    fn conservative_upper(&self) -> f64 {
        let cp = self.event.route.challenge_points.keys()
            .map(|&rank| self.event.route.cp(rank) as u128).max().unwrap_or(0);
        let plays = if self.settlement.cost == 0 { 0 } else {
            (self.settlement.starting as u128 + self.settlement.lives as u128 * cp)
                / self.settlement.cost as u128
        };
        // The wide settlement caps the final per-live reward, not its product.
        ((self.settlement.lives as f64 * i32::MAX as f64).next_up()
            + (plays as f64 * self.settlement.points.max(0.0)).next_up()).next_up()
    }

    fn projected(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        totals: Totals,
    ) -> Option<f64> {
        let scores = self
            .event
            .score_upper_orders(members, assigned, from, totals.power)?;
        let chart = self.event.chart.as_ref().unwrap();
        let mut cp_counts = std::collections::BTreeMap::<u64, u64>::new();
        let mut direct = 0.0;
        for &score in &scores {
            let rank = crate::chart::score_rank(chart, score);
            direct += self.event.route.points(totals.bonus, rank);
            *cp_counts
                .entry(self.event.route.cp(rank) as u64)
                .or_default() += 1;
        }
        let law: Vec<_> = cp_counts.into_iter().collect();
        let challenges = self.settlement.expected(&law);
        let value =
            self.settlement.lives as f64 * (direct / 120.0) + challenges * self.settlement.points;
        if law.len() == 1 || self.settlement.constant.is_some() {
            return Some(value);
        }
        // Two normalized convolution evaluations can differ by their accumulated rounding error.
        let divisor = law
            .iter()
            .fold(self.settlement.cost.max(1), |d, &(cp, _)| gcd(d, cp));
        let width = (self.settlement.cost.max(1) / divisor) as f64;
        let nu = 16.0 * width * width * (self.settlement.lives as f64 + 1.0) * f64::EPSILON;
        if nu >= 0.125 {
            return None;
        }
        let mean_cp = law
            .iter()
            .map(|&(cp, count)| cp as f64 * count as f64 / 120.0)
            .sum::<f64>();
        let error = (8.0 * nu / (1.0 - nu)
            + 64.0
                * f64::EPSILON
                * (self.settlement.starting as f64 + self.settlement.lives as f64 * mean_cp)
                / self.settlement.cost.max(1) as f64)
            * self.settlement.points;
        Some((value + error).next_up())
    }
    fn cap(&self, totals: Totals) -> f64 {
        if self.conservative() {
            return self.conservative_upper();
        }
        let chart = self.event.chart.as_ref().unwrap();
        let score = self
            .event
            .core
            .as_ref()
            .unwrap()
            .score_high(totals)
            .min(self.event.score_cap);
        let rank = crate::chart::score_rank(chart, score);
        let direct = self.event.route.points(totals.bonus, rank);
        let cp = self.event.route.cp(rank) as u128;
        let challenges = if self.settlement.cost == 0 {
            0
        } else {
            (self.settlement.starting as u128 + self.settlement.lives as u128 * cp)
                / self.settlement.cost as u128
        };
        self.settlement.lives as f64 * direct + challenges as f64 * self.settlement.points
    }
}
impl Objective for Finite {
    fn preserve_snap_options(&self) -> bool {
        self.conservative()
    }
    fn conditional_snap_dominance(&self) -> bool {
        !self.conservative() && self.event.conditional_snap_dominance()
    }
    fn exchange_snap_equivalent(&mut self, m: usize, a: i32, b: i32) -> bool {
        !self.conservative() && self.event.exchange_snap_equivalent(m, a, b)
    }
    fn canonical_leader(&self) -> bool {
        self.event.canonical_leader()
    }
    fn needs_power_cap(&self) -> bool {
        self.event.needs_power_cap()
    }
    fn prepare_dominance(&mut self, power: f64) {
        self.event.prepare_dominance(power);
    }
    fn numeric_member_dominance(&mut self, a: usize, b: usize, p: f64) -> bool {
        !self.conservative() && self.event.numeric_member_dominance(a, b, p)
    }
    fn numeric_snap_dominance(&mut self, m: usize, a: i32, b: i32, p: f64) -> bool {
        !self.conservative() && self.event.numeric_snap_dominance(m, a, b, p)
    }
    fn skill(&mut self, m: usize, s: i32) -> (f64, f64) {
        self.event.skill(m, s)
    }
    fn member_bonus(&self, m: usize) -> f64 {
        self.event.member_bonus(m)
    }
    fn snap_bonus(&self, s: usize) -> f64 {
        self.event.snap_bonus(s)
    }
    fn snap_dominance(&mut self, m: usize, s: i32) -> Vec<f64> {
        self.event.snap_dominance(m, s)
    }
    fn prepare_power(&mut self, power: Arc<Vec<f64>>, width: usize) {
        self.event.prepare_power(power, width);
    }
    fn prepare_assignment_power(&mut self, power: f64) {
        self.event.prepare_assignment_power(power);
    }
    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        self.event.prepare_members(characters);
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.event.prepare_assignment(members, options);
    }

    fn bound(&mut self, t: Totals) -> f64 {
        if self.conservative() {
            return self.conservative_upper();
        }
        self.settlement.upper(self.event.bound(t)).min(self.cap(t))
    }
    fn member_bound_at(
        &mut self,
        m: &[usize],
        t: Totals,
        left: usize,
        from: usize,
        limit: f64,
    ) -> Option<f64> {
        if self.conservative() {
            return None;
        }
        self.event
            .member_bound_at(m, t, left, from, self.settlement.limit(limit))
            .map(|v| self.settlement.upper(v).min(self.cap(t)))
    }
    fn assignment_bound(&mut self, m: &[usize], t: Totals, a: &[i32], from: usize) -> Option<f64> {
        if self.conservative() {
            return None;
        }
        self.event
            .assignment_bound(m, t, a, from)
            .map(|v| self.settlement.upper(v).min(self.cap(t)))
    }
    fn assignment_order_bound(
        &mut self,
        m: &[usize],
        t: Totals,
        a: &[i32],
        from: usize,
        limit: f64,
    ) -> Option<f64> {
        if self.conservative() {
            return None;
        }
        let upper = self
            .event
            .assignment_order_bound(m, t, a, from, self.settlement.limit(limit))
            .map(|v| self.settlement.upper(v).min(self.cap(t)));
        if upper.is_some_and(|value| value <= limit) {
            return upper;
        }
        self.projected(m, a, from, t)
            .map(|value| value.min(upper.unwrap_or(f64::INFINITY)))
            .or(upper)
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<JointTarget>> {
        if self.conservative() {
            return None;
        }
        self.event.joint_targets(self.settlement.limit(limit))
    }
    fn bonus_floor(&mut self, limit: f64) -> Option<f64> {
        if self.conservative() {
            return None;
        }
        self.event.bonus_floor(self.settlement.limit(limit))
    }
    fn leaf_bound(&mut self, team: &Team, t: Totals) -> Option<f64> {
        if self.conservative() {
            return None;
        }
        let upper = self
            .event
            .leaf_bound(team, t)
            .map(|v| self.settlement.upper(v).min(self.cap(t)));
        self.projected(&team.members, &team.snaps, 5, t)
            .map(|value| value.min(upper.unwrap_or(f64::INFINITY)))
            .or(upper)
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
        if self.conservative() {
            return (f64::NEG_INFINITY, self.conservative_upper());
        }
        let (a, b) = self.event.interval(team, t);
        (
            self.settlement.lower(a),
            self.settlement.upper(b).min(self.cap(t)),
        )
    }
    fn exact(&mut self, team: &Team, power: f64, slots: &[f64]) -> Result<Exact, String> {
        let mut exact = self.event.exact(team, power, slots)?;
        self.settlement.apply(&mut exact, &self.event.route);
        Ok(exact)
    }
}

pub(crate) struct Tie {
    pub finite: Finite,
    pub mean: crate::objectives::LiveObjective,
    pub target: f64,
    pub excluded: Vec<Vec<usize>>,
    pub allow_dominance: bool,
}
impl Tie {
    fn eligible(&self, members: &[usize]) -> bool {
        !self
            .excluded
            .iter()
            .any(|set| set.len() == members.len() && members.iter().all(|m| set.contains(m)))
    }
    fn mean_bound(&self, t: Totals) -> f64 {
        self.mean.core.score_high(t)
    }
    fn event_totals(&mut self, team: Option<&Team>, totals: Totals) -> Totals {
        let (mut low, mut high) = (f64::NEG_INFINITY, f64::INFINITY);
        if let Some(team) = team {
            (low, high) = (0.0, 0.0);
            for (slot, &member) in team.members.iter().enumerate() {
                let (a, b) = self.finite.skill(member, team.snaps[slot]);
                low += a;
                high += b;
            }
        }
        Totals {
            skill: high,
            skill_low: low,
            ..totals
        }
    }
}
impl Objective for Tie {
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn canonical_leader(&self) -> bool {
        self.finite.canonical_leader()
    }
    fn preserve_snap_options(&self) -> bool {
        !self.allow_dominance || self.finite.conservative()
    }
    fn exchange_snap_equivalent(&mut self, m: usize, a: i32, b: i32) -> bool {
        self.finite.exchange_snap_equivalent(m, a, b)
    }
    fn needs_power_cap(&self) -> bool {
        self.finite.needs_power_cap()
    }
    fn prepare_dominance(&mut self, p: f64) {
        self.finite.prepare_dominance(p);
    }
    fn numeric_member_dominance(&mut self, a: usize, b: usize, p: f64) -> bool {
        self.excluded.is_empty() && self.finite.numeric_member_dominance(a, b, p)
    }
    fn numeric_snap_dominance(&mut self, m: usize, a: i32, b: i32, p: f64) -> bool {
        self.finite.numeric_snap_dominance(m, a, b, p)
    }
    fn skill(&mut self, m: usize, s: i32) -> (f64, f64) {
        self.mean.skill(m, s)
    }
    fn member_bonus(&self, m: usize) -> f64 {
        self.finite.member_bonus(m)
    }
    fn snap_bonus(&self, s: usize) -> f64 {
        self.finite.snap_bonus(s)
    }
    fn snap_dominance(&mut self, m: usize, s: i32) -> Vec<f64> {
        self.finite.snap_dominance(m, s)
    }
    fn prepare_power(&mut self, p: Arc<Vec<f64>>, width: usize) {
        self.mean.prepare_power(Arc::clone(&p), width);
        self.finite.prepare_power(p, width);
    }
    fn prepare_assignment_power(&mut self, p: f64) {
        self.mean.prepare_assignment_power(p);
        self.finite.prepare_assignment_power(p);
    }
    fn prepare_members(&mut self, c: &[Vec<usize>]) {
        self.finite.prepare_members(c);
    }
    fn prepare_assignment(&mut self, m: &[usize], o: &[Vec<i32>]) {
        self.mean.prepare_assignment(m, o);
        self.finite.prepare_assignment(m, o);
    }
    fn bound(&mut self, t: Totals) -> f64 {
        let totals = self.event_totals(None, t);
        if self.finite.bound(totals) < self.target {
            -1.0
        } else {
            self.mean_bound(t)
        }
    }
    fn member_bound_at(
        &mut self,
        m: &[usize],
        t: Totals,
        left: usize,
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        let totals = self.event_totals(None, t);
        self.finite
            .member_bound_at(m, totals, left, from, self.target)
            .map(|v| {
                if v < self.target {
                    -1.0
                } else {
                    self.mean_bound(t)
                }
            })
    }
    fn assignment_bound(&mut self, m: &[usize], t: Totals, a: &[i32], from: usize) -> Option<f64> {
        if !self.eligible(m) {
            return Some(-1.0);
        }
        let totals = self.event_totals(None, t);
        self.finite.assignment_bound(m, totals, a, from).map(|v| {
            if v < self.target {
                -1.0
            } else {
                self.mean_bound(t)
            }
        })
    }
    fn assignment_order_bound(
        &mut self,
        m: &[usize],
        t: Totals,
        a: &[i32],
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        let totals = self.event_totals(None, t);
        self.finite
            .assignment_order_bound(m, totals, a, from, self.target)
            .map(|v| {
                if v < self.target {
                    -1.0
                } else {
                    self.mean_bound(t)
                }
            })
    }
    fn product_base(&self) -> Option<f64> {
        self.mean.product_base()
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<JointTarget>> {
        self.mean.joint_targets(limit)
    }
    fn bonus_floor(&mut self, _limit: f64) -> Option<f64> {
        self.finite.bonus_floor(self.target)
    }
    fn leaf_bound(&mut self, team: &Team, t: Totals) -> Option<f64> {
        if !self.eligible(&team.members) {
            return Some(-1.0);
        }
        let totals = self.event_totals(Some(team), t);
        self.finite.leaf_bound(team, totals).map(|v| {
            if v < self.target {
                -1.0
            } else {
                self.mean_bound(t)
            }
        })
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
        if !self.eligible(&team.members) {
            return (-1.0, -1.0);
        }
        let totals = self.event_totals(Some(team), t);
        let (low, high) = self.finite.interval(team, totals);
        if high < self.target || low > self.target {
            (-1.0, -1.0)
        } else {
            (-1.0, self.mean_bound(t))
        }
    }
    fn exact(&mut self, team: &Team, power: f64, slots: &[f64]) -> Result<Exact, String> {
        let mut exact = self.finite.exact(team, power, slots)?;
        exact.key = if self.eligible(&team.members) && exact.key == self.target {
            crate::master::number(&exact.detail["scores"]["mean"], 0.0)
        } else {
            -1.0
        };
        Ok(exact)
    }
}
