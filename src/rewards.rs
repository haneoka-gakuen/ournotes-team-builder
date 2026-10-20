//! Sampled Gekisou rewards settle every outcome before taking its mean.
use crate::{
    chart::{self, Chart},
    event::{Route, RouteKind},
    full::{conditions::Performer, score::note_factor_mill},
    gekiso::{self, Context},
    live,
    master::{Master, SkillEffect},
    num::{f, window_ms},
    power::{Member, Snap},
    search::{Cutoff, Evaluation, Exact, Objective, Team, Totals},
    skills,
};
use serde_json::json;
use std::{collections::{BTreeMap, BTreeSet, HashMap, VecDeque}, sync::Arc};
const MAX_SAFE: f64 = 9007199254740991.0;
fn safe(n: f64) -> bool {
    n.is_finite() && n.fract() == 0.0 && n.abs() <= MAX_SAFE
}
fn up(n: f64) -> f64 {
    if !n.is_finite() {
        f64::INFINITY
    } else {
        assert!(n >= 0.0, "negative-score-bound");
        f64::from_bits(n.to_bits() + 1)
    }
}
fn add(a: f64, b: f64) -> f64 {
    up(a + b)
}
fn mul(a: f64, b: f64) -> f64 {
    up(a * b)
}
fn range_delta(master: &Master, row: &SkillEffect) -> Option<f64> {
    if ![2000, 2001].contains(&row.kind) || row.trigger_type != 1 || row.reset_group != 0 {
        return None;
    }
    let sets = master.condition_sets.get(&row.trigger_group)?;
    if sets.len() != 1 || sets[0].len() != 1 {
        return None;
    }
    let trigger = master.conditions.get(&sets[0][0])?;
    if trigger.kind != 7010 || !trigger.positive {
        return None;
    }
    let mut value = row.value;
    if row.kind == 2001 {
        if row.cumulative_id == 0 {
            value = 0
        } else {
            let cumulative = master.cumulative.get(&row.cumulative_id)?;
            if !(3000..=3005).contains(&cumulative.kind) || cumulative.cap < 0 {
                return None;
            }
            let count = if cumulative.cap < 1 {
                5
            } else {
                cumulative.cap.min(5)
            };
            for n in 0..=count {
                let mut current = row.value as i64 * n as i64;
                if row.max_value > 0 {
                    current = current.min(row.max_value as i64);
                }
                let factor = f(f(current as f64) / 10000.0);
                if f(f(note_factor_mill(factor) as f64) / 100000.0) != factor {
                    return None;
                }
            }
            let total = value as i64 * count as i64;
            if total >= 2147483648 {
                return None;
            }
            value = total as i32;
            if row.max_value > 0 {
                value = value.min(row.max_value);
            }
        }
    }
    let mill = note_factor_mill(f(f(value as f64) / 10000.0));
    if !(0..i32::MAX).contains(&mill) {
        return None;
    }
    Some(f(f(mill as f64) / 100000.0))
}
#[derive(Default)]
struct Budget {
    positive: f64,
    commands: usize,
}
fn admitted(master: &Master, rows: Option<&Vec<SkillEffect>>, level: i32) -> Option<Budget> {
    let active: Vec<_> = rows
        .into_iter()
        .flatten()
        .filter(|r| r.level == level)
        .collect();
    if active.is_empty() {
        return None;
    }
    let mut budget = Budget::default();
    for row in active {
        if row.value < 0
            || row.value > 1000000
            || !row.seconds.is_finite()
            || row.seconds < 0.0
            || row.max_value < 0
            || row.max_value > 1000000
        {
            return None;
        }
        if [
            11000, 11001, 11002, 11003, 11004, 11005, 12000, 12002, 12003, 12004, 12006, 13000,
            13002, 13003, 13004, 13005,
        ]
        .contains(&row.kind)
        {
            continue;
        }
        budget.positive = add(budget.positive, mul(3.0, range_delta(master, row)?));
        budget.commands += 6;
    }
    Some(budget)
}
fn largest(budgets: &[Budget]) -> f64 {
    let mut rows: Vec<_> = budgets.iter().map(|b| b.positive).collect();
    rows.sort_by(|a, b| b.total_cmp(a));
    rows.into_iter().take(5).fold(0.0, add)
}
fn commands(budgets: &[Budget]) -> usize {
    let mut rows: Vec<_> = budgets.iter().map(|b| b.commands).collect();
    rows.sort_by(|a, b| b.cmp(a));
    rows.into_iter().take(5).sum()
}
/// Infinity disables this proof for unsupported effects; exact simulation is
/// still used. A finite coefficient is outward-rounded at every operation.
pub fn score_coefficient(
    master: &Master,
    chart: &gekiso::Chart,
    members: &[Member],
    snaps: &[Snap],
    bindings: Option<&[Option<Snap>]>,
) -> f64 {
    let make = || -> Option<f64> {
        if bindings.is_some_and(|b| b.len() != members.len() || members.len() > 5) {
            return None;
        }
        let active: Vec<_> = if let Some(b) = bindings {
            b.iter().filter_map(|s| s.as_ref()).collect()
        } else {
            snaps.iter().collect()
        };
        let mut mb = Vec::new();
        let mut sb = Vec::new();
        for m in members {
            if m.card.gekiso_skill_id == 0 {
                continue;
            }
            mb.push(admitted(
                master,
                master
                    .gekiso_skills
                    .get(&m.card.gekiso_skill_id)
                    .map(|s| &s.effects),
                m.growth.gekiso_skill_level,
            )?);
        }
        for s in active {
            let mut total = Budget::default();
            for (i, &id) in s.card.gekiso_support_skills.iter().enumerate() {
                if id == 0 {
                    continue;
                }
                let budget = admitted(
                    master,
                    master.gekiso_support_skills.get(&id).map(|s| &s.effects),
                    s.gekiso_skill_levels[i],
                )?;
                total.positive = add(total.positive, budget.positive);
                total.commands += budget.commands;
            }
            sb.push(total);
        }
        let positive = add(largest(&mb), largest(&sb));
        let note_commands = commands(&mb) + commands(&sb);
        let live = &master.live;
        if !(0.0..=1.0).contains(&live.life_onus)
            || !(1..=1000000000).contains(&chart.full.converted)
            || chart.range_percent.iter().any(|&x| x < 0)
            || chart.judge_frac.iter().any(|x| !x.is_finite() || *x < 0.0)
            || live
                .combo
                .get(&1)
                .into_iter()
                .flatten()
                .any(|r| !r.1.is_finite() || r.1 < 0.0)
            || !(0..=100).contains(&live.gekiso.rush_percent)
        {
            return None;
        }
        let (mut shapes, mut seen, mut max_absolute, mut max_actions) =
            (Vec::new(), BTreeSet::new(), 0.0_f64, 0usize);
        for (index, member) in members.iter().enumerate() {
            let photos: Vec<_> = if let Some(b) = bindings {
                vec![b[index].as_ref()]
            } else {
                std::iter::once(None)
                    .chain(snaps.iter().map(Some))
                    .collect()
            };
            for snap in photos {
                let slot = skills::resolve(master, member, snap, &mut Vec::new());
                let mut shape = Vec::new();
                let (mut absolute, mut actions) = (0.0, 0);
                for e in &slot.effects {
                    let multiplicity = if e.kind == 2000 {
                        1
                    } else {
                        e.judgements.len()
                    };
                    let diff = f(e.delta as f64 / 100000.0);
                    let duration = window_ms(e.seconds, slot.extension_ms);
                    if !diff.is_finite() {
                        return None;
                    }
                    actions += multiplicity;
                    absolute = add(absolute, mul(diff.abs(), multiplicity as f64));
                    if duration > 0 && diff > 0.0 {
                        shape.push((duration, mul(diff, multiplicity as f64)));
                    }
                }
                max_actions = max_actions.max(actions);
                max_absolute = max_absolute.max(absolute);
                let key: Vec<_> = shape.iter().map(|&(t, v)| (t, v.to_bits())).collect();
                if seen.insert(key) {
                    shapes.push(shape);
                }
            }
        }
        let durations: Vec<_> = shapes
            .iter()
            .flatten()
            .map(|r| r.0)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let envelope: Vec<_> = durations
            .iter()
            .map(|&duration| {
                shapes
                    .iter()
                    .map(|shape| {
                        shape
                            .iter()
                            .filter(|r| r.0 >= duration)
                            .map(|r| r.1)
                            .fold(0.0, add)
                    })
                    .fold(0.0, f64::max)
            })
            .collect();
        let events: Vec<_> = chart.full.events.iter().take(5).map(|r| r.1).collect();
        let u = 2.0_f64.powi(-24);
        let eta = 2.0_f64.powi(-149);
        let operations = 3 * (2 * chart.play.frames.len() + 6) * note_commands;
        if operations as f64 * u >= 0.25 {
            return None;
        }
        let error = if note_commands != 0 {
            mul(
                add(
                    mul(operations as f64 * u, add(1.0, mul(2.0, positive))),
                    mul(operations as f64, eta),
                ),
                up(1.0 / (1.0 - operations as f64 * u)),
            )
        } else {
            0.0
        };
        let note_multiplier = add(add(1.0, positive), error);
        let operations = (10 * max_actions + 1) as f64;
        if operations * u > 0.25 {
            return None;
        }
        let residual = add(
            mul(8.0 * operations * u, mul(5.0, max_absolute)),
            4.0 * operations * eta,
        );
        let rho = up(1.0 / (1.0 - u));
        let mut round_factor = 1.0;
        for _ in 0..12 {
            round_factor = mul(round_factor, rho);
        }
        let fraction = chart
            .judge_frac
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        let low = 2.0_f64.powi(-10);
        if [live.adjustment, chart.difficulty_factor, fraction]
            .iter()
            .any(|&v| v < low || v > 1e4 || !v.is_finite())
        {
            return None;
        }
        let mut coefficient = 0.0;
        for (i, note) in chart.notes.iter().enumerate() {
            let frac = chart.note_frac[i];
            let combo = if chart.accuracy.miss_every > 0 { 2.0 } else { chart.combo_factor[i] };
            if frac == 0.0 {
                continue;
            }
            if frac < low || frac > 100.0 || combo < low || combo > 4.0 {
                return None;
            }
            let mut factor = residual;
            for &start in &events {
                let delta = chart.times[i] - start;
                if delta < 0 {
                    continue;
                }
                if let Some(at) = durations.iter().position(|&t| t > delta) {
                    factor = add(factor, envelope[at]);
                }
            }
            if factor > 1e5 {
                return None;
            }
            let gk = if chart
                .setup
                .fevers
                .iter()
                .enumerate()
                .any(|(r, &(start, end))| {
                    chart.setup.missions.get(r) == Some(&1)
                        && start <= note.time_ms
                        && note.time_ms <= end
                }) {
                2.0
            } else {
                1.0
            };
            let luck = if chart.luck { 2.0 } else { 1.0 };
            let mut c = mul(live.adjustment, chart.difficulty_factor);
            for term in [
                frac,
                fraction,
                combo,
                gk,
                luck,
                add(note_multiplier, factor),
                round_factor,
            ] {
                c = mul(c, term);
            }
            c = up(c / chart.full.converted as f64);
            let range = chart.range_of[i];
            if range >= 0 {
                c = mul(
                    c,
                    add(1.0, up(chart.range_percent[range as usize] as f64 / 100.0)),
                );
            }
            coefficient = add(coefficient, c);
        }
        Some(coefficient)
    };
    make().unwrap_or(f64::INFINITY)
}
fn score_bound(coefficient: f64, power: f64) -> f64 {
    if !(1.0..=2147483648.0).contains(&power) {
        return f64::INFINITY;
    }
    let bound = mul(coefficient, power);
    if bound <= MAX_SAFE {
        bound
    } else {
        f64::INFINITY
    }
}
#[derive(Default)]
struct Just {
    eligible: usize,
    baseline: usize,
    effective: f64,
    converted: f64,
}
fn just_summary(chart: &gekiso::Chart, context: &Context) -> Result<Just, String> {
    if context.judgement.len() != chart.notes.len() || chart.judgements.len() != chart.notes.len() {
        return Err("gekiso-just-note-mismatch".into());
    }
    let ids: BTreeSet<_> = chart.play.just_eligible_ids.iter().copied().collect();
    let mut just = Just::default();
    for (i, note) in chart.notes.iter().enumerate() {
        if !ids.contains(&note.id) {
            continue;
        }
        just.eligible += 1;
        let before = chart.judgements[i] == 6;
        let after = context.judgement[i] == 6;
        if before {
            just.baseline += 1;
        }
        if after {
            just.effective += 1.0;
        }
        if after && !before {
            just.converted += 1.0;
        }
    }
    Ok(just)
}
pub struct RewardObjective<'a> {
    master: &'a Master,
    members: &'a [Member],
    snaps: &'a [Snap],
    chart: &'a Chart,
    pub gekiso: gekiso::Chart,
    pub route: Route,
    points: bool,
    samples: i32,
    exchange: Option<(f64, f64)>,
    ranks: Vec<i32>,
    coefficient: f64,
    local: HashMap<Vec<(usize, i32)>, f64>,
    local_order: VecDeque<Vec<(usize, i32)>>,
    reward_target: Option<f64>,
    rank_other: bool,
    other_target: Option<f64>,
    other_members: Vec<f64>,
    other_snaps: Vec<f64>,
    other_top: Vec<f64>,
    member_suffix: Vec<Vec<f64>>,
    other_snap_top: f64,
    combo_bonuses: Option<Vec<i32>>,
    other_suffix: [f64; 6],
    other_ordered: Vec<i32>,
    contexts: HashMap<(i32, Vec<Performer>), Arc<Context>>,
    context_order: VecDeque<((i32, Vec<Performer>), usize)>,
    context_bytes: usize,
    context_attributes: Vec<&'static str>,
}
impl<'a> RewardObjective<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        master: &'a Master,
        members: &'a [Member],
        snaps: &'a [Snap],
        chart: &'a Chart,
        gekiso: gekiso::Chart,
        route: Route,
        points: bool,
        samples: i32,
        exchange: Option<(f64, f64)>,
    ) -> Self {
        let coefficient = score_coefficient(master, &gekiso, members, snaps, None);
        let ranks = std::iter::once(2)
            .chain(
                chart
                    .ranks
                    .iter()
                    .filter(|r| r.battle_required > 0)
                    .map(|r| r.rank),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let other_members: Vec<_> = members.iter().map(|m| crate::power::member_event_percent(&route.effects, m, if points { 1 } else { 0 }) as f64).collect();
        let other_snaps: Vec<_> = snaps.iter().map(|s| crate::power::snap_event_percent(&route.effects, s, if points { 1 } else { 0 }) as f64).collect();
        let mut other_top = other_members.clone(); other_top.sort_by(|a, b| b.total_cmp(a));
        let mut snap_top = other_snaps.clone(); snap_top.sort_by(|a, b| b.total_cmp(a));
        let other_snap_top = snap_top.iter().take(5).sum();
        let combo_bonuses = gekiso.setup.missions.iter().any(|m| *m == 1 || *m == 4).then(|| crate::gekiso_objective::combo_bonuses(master, members, &gekiso.setup.missions));
        Self {
            master,
            members,
            snaps,
            chart,
            gekiso,
            route,
            points,
            samples,
            exchange,
            ranks,
            coefficient,
            local: HashMap::new(),
            local_order: VecDeque::new(),
            reward_target: None,
            rank_other: false,
            other_target: None,
            other_members,
            other_snaps,
            other_top,
            member_suffix: Vec::new(),
            other_snap_top,
            combo_bonuses,
            other_suffix: [0.0; 6],
            other_ordered: Vec::new(),
            contexts: HashMap::new(),
            context_order: VecDeque::new(),
            context_bytes: 0,
            context_attributes: crate::gekiso_objective::read_attributes(master, true),
        }
    }
    pub fn rank_other_at_reward(&mut self, target: f64) {
        self.reward_target = Some(target);
        self.rank_other = true;
    }
    pub fn rank_score_at_reward(&mut self, target: f64, other: f64) {
        self.reward_target = Some(target);
        self.rank_other = false;
        self.other_target = Some(other);
    }
    fn other(&self, bonus: f64, rank: i32) -> f64 {
        if self.points { self.route.items(bonus, rank) } else { self.route.points(bonus, rank) }
    }
    fn other_bonus(&self, team: &Team) -> f64 {
        team.members.iter().map(|&i| self.other_members[i]).sum::<f64>()
            + team.snaps.iter().filter(|&&j| j >= 0).map(|&j| self.other_snaps[j as usize]).sum::<f64>()
    }
    fn other_ceiling(&self, power: f64, bonus: f64, coefficient: f64) -> f64 {
        let upper = score_bound(coefficient, power);
        self.ranks.iter().filter(|&&rank| rank == 2 || self.chart.ranks.iter().any(|r| r.rank == rank && r.battle_required > 0 && r.battle_required as f64 <= upper))
            .map(|&rank| self.other(bonus, rank)).fold(f64::NEG_INFINITY, f64::max)
    }
    fn ranked_bound(&self, power: f64, bonus: f64, coefficient: f64, other: f64) -> f64 {
        let reward = self.ceiling(power, bonus, coefficient);
        match self.reward_target {
            Some(target) if reward < target => -1.0,
            Some(_) => {
                let upper = self.other_ceiling(power, other, coefficient);
                if self.other_target.is_some_and(|target| upper < target) { -1.0 }
                else if self.rank_other { upper } else { score_bound(coefficient, power) }
            },
            None => reward,
        }
    }
    fn ranked(&self, mut exact: Exact) -> Exact {
        if let Some(target) = self.reward_target {
            let other = crate::master::number(&exact.detail["settlement"]["otherMean"], 0.0);
            exact.key = if exact.key == target && self.other_target.is_none_or(|target| other == target) {
                if self.rank_other { other } else { crate::master::number(&exact.detail["scores"]["mean"], 0.0) }
            } else { -1.0 };
        }
        exact
    }
    fn direct(&self, bonus: f64, rank: i32) -> f64 {
        if self.points {
            self.route.points(bonus, rank)
        } else {
            self.route.items(bonus, rank)
        }
    }
    fn value(&self, bonus: f64, rank: i32) -> f64 {
        let reward = self.direct(bonus, rank);
        if let Some((n, d)) = self.exchange {
            reward * d
                + *self.route.challenge_points.get(&rank).unwrap_or(&0) as f64 * self.route.rate * n
        } else {
            reward
        }
    }
    fn team_coefficient(&mut self, team: &Team) -> f64 {
        let mut key: Vec<_> = team
            .members
            .iter()
            .copied()
            .zip(team.snaps.iter().copied())
            .collect();
        key.sort_unstable();
        if let Some(&v) = self.local.get(&key) {
            self.local_order.retain(|k| k != &key);
            self.local_order.push_back(key);
            return v;
        }
        let members: Vec<_> = team
            .members
            .iter()
            .map(|&i| self.members[i].clone())
            .collect();
        let snaps: Vec<_> = team
            .snaps
            .iter()
            .map(|&j| {
                if j < 0 {
                    None
                } else {
                    Some(self.snaps[j as usize].clone())
                }
            })
            .collect();
        let bound = score_coefficient(self.master, &self.gekiso, &members, &[], Some(&snaps));
        if self.local.len() >= 256 {
            if let Some(old) = self.local_order.pop_front() {
                self.local.remove(&old);
            }
        }
        self.local.insert(key.clone(), bound);
        self.local_order.push_back(key);
        bound
    }
    fn ceiling(&self, power: f64, bonus: f64, coefficient: f64) -> f64 {
        let upper = score_bound(coefficient, power);
        self.ranks
            .iter()
            .filter(|&&rank| {
                rank == 2
                    || self.chart.ranks.iter().any(|r| {
                        r.rank == rank && r.battle_required > 0 && r.battle_required as f64 <= upper
                    })
            })
            .map(|&rank| self.value(bonus, rank))
            .fold(f64::NEG_INFINITY, f64::max)
    }
    fn context(&mut self, deck: &[Performer], seed: i32) -> Result<Arc<Context>, String> {
        let runs = |mission: i32| mission == 4 || self.gekiso.setup.missions.contains(&mission);
        let reduced = deck.iter().map(|p| {
            let mut p = p.gekiso_only();
            if let Some((id, _)) = p.gekisou_skill {
                let mission = self.master.gekiso_skills[&id].mission_type;
                if !runs(mission) { p.gekisou_skill = None; p.mission_only = Some(mission); }
            }
            p.gekisou_support_skills.retain(|(id, _)| runs(self.master.gekiso_support_skills[id].mission_type));
            if !self.context_attributes.contains(&"bandId") { p.band_id = 0; }
            if !self.context_attributes.contains(&"cardType") { p.card_type = 0; }
            if !self.context_attributes.contains(&"characterId") { p.character_id = 0; }
            if !self.context_attributes.contains(&"tagIds") { p.tag_ids.clear(); }
            if !self.context_attributes.contains(&"liveSkillCategories") { p.live_skill_categories.clear(); }
            if !self.context_attributes.contains(&"gekisouSkillCategories") { p.gekisou_skill_categories.clear(); }
            if !self.context_attributes.contains(&"gekisouMissionType") { p.gekisou_mission_type = 0; }
            p
        }).collect::<Vec<_>>();
        let key = (seed, reduced);
        if let Some(context) = self.contexts.get(&key) { return Ok(Arc::clone(context)); }
        let context = Arc::new(gekiso::context_fixed(self.master, &self.gekiso, &key.1, &[seed])?);
        let bytes = context.retained_bytes() + key.1.len() * 256 + 128;
        const CAPACITY: usize = 16 * 1024 * 1024;
        if bytes <= CAPACITY {
            while self.context_bytes + bytes > CAPACITY {
                let Some((old, bytes)) = self.context_order.pop_front() else { break; };
                self.contexts.remove(&old); self.context_bytes -= bytes;
            }
            self.context_bytes += bytes;
            self.context_order.push_back((key.clone(), bytes));
            self.contexts.insert(key, Arc::clone(&context));
        }
        Ok(context)
    }
    fn settle(
        &mut self,
        team: &Team,
        power: i32,
        cutoff: Option<Cutoff>,
    ) -> Result<Option<Exact>, String> {
        if self.route.kind != RouteKind::Live {
            return Err("gekiso-reward-route".into());
        }
        let deck: Vec<_> = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &i)| {
                Performer::from_member(
                    self.master,
                    &self.members[i],
                    if team.snaps[slot] < 0 {
                        None
                    } else {
                        Some(&self.snaps[team.snaps[slot] as usize])
                    },
                )
            })
            .collect();
        let slots: Vec<_> = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &i)| {
                skills::resolve(
                    self.master,
                    &self.members[i],
                    if team.snaps[slot] < 0 {
                        None
                    } else {
                        Some(&self.snaps[team.snaps[slot] as usize])
                    },
                    &mut Vec::new(),
                )
            })
            .collect();
        let bonus = team
            .members
            .iter()
            .map(|&i| self.member_bonus(i))
            .sum::<f64>()
            + team
                .snaps
                .iter()
                .filter(|&&j| j >= 0)
                .map(|&j| self.snap_bonus(j as usize))
                .sum::<f64>();
        let other_bonus = self.other_bonus(team);
        let mut other_sum = 0.0;
        let runs = if self.gekiso.luck { self.samples } else { 1 };
        if runs < 1 {
            return Err("gekiso-reward-samples".into());
        }
        let count = runs as usize * 120;
        let ceiling = if cutoff.is_some_and(|c| c.key.is_finite()) {
            let coefficient = self.team_coefficient(team);
            let ceiling = self.ceiling(power as f64, bonus, coefficient);
            if !safe(ceiling) || ceiling < 0.0 || !safe(count as f64 * ceiling) {
                return Err("gekiso-reward-precision".into());
            }
            Some(ceiling)
        } else {
            None
        };
        let mut scores = vec![0.0; 120];
        let (mut min, mut max, mut min_order, mut max_order) =
            (f64::INFINITY, f64::NEG_INFINITY, 0, 0);
        let mut just = Just::default();
        let (
            mut reward_sum,
            mut comparison_sum,
            mut cp_sum,
            mut observations,
            mut direct_min,
            mut direct_max,
        ) = (0.0, 0.0, 0.0, 0usize, f64::INFINITY, f64::NEG_INFINITY);
        let mut ranks = BTreeMap::<i32, usize>::new();
        for seed in 0..runs {
            let context = self.context(&deck, seed)?;
            let current = just_summary(&self.gekiso, &context)?;
            let result =
                gekiso::score_orders(self.master, &self.gekiso, &context, power, &slots, true)?;
            just.eligible = current.eligible;
            just.baseline = current.baseline;
            just.effective += current.effective / runs as f64;
            just.converted += current.converted / runs as f64;
            for (i, &v) in result.scores.iter().enumerate() {
                scores[i] += v / runs as f64;
            }
            if result.min < min {
                min = result.min;
                min_order = result.min_order;
            }
            if result.max > max {
                max = result.max;
                max_order = result.max_order;
            }
            for &score in &result.scores {
                if !score.is_finite() || score < 0.0 {
                    return Err("gekiso-reward-score".into());
                }
                let rank = chart::battle_rank(self.chart, score);
                if if self.points {
                    !self.route.points.contains_key(&rank)
                } else {
                    !self
                        .route
                        .rewards
                        .iter()
                        .any(|r| r.score_rank == rank && r.resource_id == self.route.item_id)
                } {
                    return Err(format!("gekiso-reward-rank:{rank}"));
                }
                if self.exchange.is_some() && !self.route.challenge_points.contains_key(&rank) {
                    return Err(format!("gekiso-reward-cp:{rank}"));
                }
                let cp =
                    *self.route.challenge_points.get(&rank).unwrap_or(&0) as f64 * self.route.rate;
                let reward = self.direct(bonus, rank);
                reward_sum += reward;
                other_sum += self.other(other_bonus, rank);
                cp_sum += cp;
                comparison_sum += if let Some((n, d)) = self.exchange {
                    reward * d + cp * n
                } else {
                    reward
                };
                observations += 1;
                if !safe(reward_sum)
                    || !safe(cp_sum)
                    || !safe(comparison_sum)
                    || comparison_sum > MAX_SAFE / 2.0
                {
                    return Err("gekiso-reward-precision".into());
                }
                direct_min = direct_min.min(reward);
                direct_max = direct_max.max(reward);
                *ranks.entry(rank).or_default() += 1;
            }
            if let (Some(ceiling), Some(cutoff)) = (ceiling, cutoff) {
                if observations < count {
                    let numerator = comparison_sum + (count - observations) as f64 * ceiling;
                    if !safe(numerator) {
                        return Err("gekiso-reward-precision".into());
                    }
                    let upper = numerator / count as f64;
                    if upper < cutoff.key || (cutoff.prune_equal && upper == cutoff.key) {
                        return Ok(None);
                    }
                }
            }
        }
        if observations == 0 {
            return Err("gekiso-reward-empty-sample".into());
        }
        let mean = scores.iter().fold(0.0, |sum, v| sum + v) / 120.0;
        let scores = live::Scores {
            power,
            scores,
            mean,
            min,
            max,
            min_order,
            max_order,
        };
        let settlement = json!({"otherMean":other_sum/observations as f64,"rewardSum":reward_sum,"comparisonSum":comparison_sum,"cpSum":cp_sum,"observations":observations,"directMin":direct_min,"directMax":direct_max,"ranks":ranks,"mean":comparison_sum/observations as f64});
        let baseline_rate = if just.eligible > 0 {
            Some(just.baseline as f64 / just.eligible as f64)
        } else {
            None
        };
        let effective_rate = if just.eligible > 0 {
            Some(just.effective / just.eligible as f64)
        } else {
            None
        };
        Ok(Some(Exact {
            key: comparison_sum / observations as f64,
            detail: json!({"comboBonus":self.combo_bonuses.as_ref().map(|values| 1 + team.members.iter().map(|&i| values[i]).sum::<i32>()),"settlement":settlement,"bonus":bonus,"scores":scores,"just":{"eligible":just.eligible,"baselineHits":just.baseline,"effectiveHits":just.effective,"convertedHits":just.converted,"baselineRate":baseline_rate,"effectiveRate":effective_rate},"luckSamples":runs,"sampled":self.gekiso.luck}),
        }))
    }
}
impl Objective for RewardObjective<'_> {
    fn preserve_snap_options(&self) -> bool {
        self.reward_target.is_some()
    }
    fn skill(&mut self, _m: usize, _s: i32) -> (f64, f64) {
        (0.0, 0.0)
    }
    fn member_bonus(&self, m: usize) -> f64 {
        crate::power::member_event_percent(
            &self.route.effects,
            &self.members[m],
            if self.points { 0 } else { 1 },
        ) as f64
    }
    fn snap_bonus(&self, s: usize) -> f64 {
        crate::power::snap_event_percent(
            &self.route.effects,
            &self.snaps[s],
            if self.points { 0 } else { 1 },
        ) as f64
    }
    fn bound(&mut self, t: Totals) -> f64 {
        self.ranked_bound(t.power, t.bonus, self.coefficient, self.other_top.iter().take(5).sum::<f64>() + self.other_snap_top)
    }
    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        self.member_suffix = crate::search::suffix_tops(&characters.iter().map(|members| members.iter().map(|&i| self.other_members[i]).fold(f64::NEG_INFINITY, f64::max)).collect::<Vec<_>>());
    }
    fn member_bound_at(&mut self, members: &[usize], t: Totals, left: usize, from: usize) -> Option<f64> {
        let other = members.iter().map(|&i| self.other_members[i]).sum::<f64>() + self.member_suffix[from][left] + self.other_snap_top;
        Some(self.ranked_bound(t.power, t.bonus, self.coefficient, other))
    }
    fn prepare_assignment(&mut self, _members: &[usize], options: &[Vec<i32>]) {
        if self.reward_target.is_none() { return; }
        self.other_suffix = [0.0; 6];
        for slot in (0..5).rev() {
            self.other_suffix[slot] = self.other_suffix[slot + 1] + options[slot].iter().map(|&s| if s < 0 { 0.0 } else { self.other_snaps[s as usize] }).fold(f64::NEG_INFINITY, f64::max);
        }
        self.other_ordered = options.iter().flatten().copied().filter(|&s| s >= 0).collect();
        self.other_ordered.sort_unstable(); self.other_ordered.dedup();
        self.other_ordered.sort_by(|&a, &b| self.other_snaps[b as usize].total_cmp(&self.other_snaps[a as usize]));
    }
    fn assignment_bound(&mut self, members: &[usize], t: Totals, assigned: &[i32], from: usize) -> Option<f64> {
        self.reward_target?;
        let distinct = self.other_ordered.iter().filter(|s| !assigned[..from].contains(s)).take(5 - from).map(|&s| self.other_snaps[s as usize]).sum::<f64>();
        let other = members.iter().map(|&i| self.other_members[i]).sum::<f64>()
            + assigned.iter().take(from).filter(|&&s| s >= 0).map(|&s| self.other_snaps[s as usize]).sum::<f64>() + self.other_suffix[from].min(distinct);
        Some(self.ranked_bound(t.power, t.bonus, self.coefficient, other))
    }
    fn leaf_bound(&mut self, team: &Team, t: Totals) -> Option<f64> {
        let coefficient = self.team_coefficient(team);
        Some(self.ranked_bound(t.power, t.bonus, coefficient, self.other_bonus(team)))
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
        let low = self
            .ranks
            .iter()
            .map(|&r| self.value(t.bonus, r))
            .fold(f64::INFINITY, f64::min);
        let coefficient = self.team_coefficient(team);
        let high = self.ceiling(t.power, t.bonus, coefficient);
        if let Some(target) = self.reward_target {
            if low > target || high < target {
                (-1.0, -1.0)
            } else {
                let bonus = self.other_bonus(team);
                let other_low = self.ranks.iter().map(|&r| self.other(bonus, r)).fold(f64::INFINITY, f64::min);
                let other_high = self.other_ceiling(t.power, bonus, coefficient);
                if self.other_target.is_some_and(|target| other_low > target || other_high < target) { (-1.0, -1.0) }
                else if self.rank_other { (-1.0, other_high) } else { (-1.0, score_bound(coefficient, t.power)) }
            }
        } else {
            (low, high)
        }
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        let exact = self
            .settle(team, power as i32, None)?
            .ok_or_else(|| "gekiso-reward-unexpected-cutoff".to_owned())?;
        Ok(self.ranked(exact))
    }
    fn evaluate(
        &mut self,
        team: &Team,
        power: f64,
        _slots: &[f64],
        cutoff: Cutoff,
    ) -> Result<Evaluation, String> {
        // A score cutoff cannot be compared with a reward. The equality pass
        // may only stop settlement when the reward cannot reach its target.
        let reward_cutoff = self
            .reward_target
            .map(|key| Cutoff {
                key,
                prune_equal: false,
            })
            .unwrap_or(cutoff);
        Ok(
            match self.settle(team, power as i32, Some(reward_cutoff))? {
                Some(exact) => Evaluation::Exact(self.ranked(exact)),
                None if self.reward_target.is_some() => Evaluation::Pruned(-1.0),
                None => Evaluation::Pruned(if cutoff.prune_equal {
                    cutoff.key
                } else {
                    cutoff.key.next_down()
                }),
            },
        )
    }
}
