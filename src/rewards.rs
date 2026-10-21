//! Sampled Gekisou rewards settle every outcome before taking its mean.
use crate::{
    chart::{self, Chart},
    event::{Route, RouteKind},
    full::{conditions::Performer, score::note_factor_mill},
    gekiso::{self, Context},
    live,
    master::{GekisoSkill, Master, SkillEffect},
    num::{f, window_ms},
    power::{Member, Snap},
    search::{Cutoff, Evaluation, Exact, JointTarget, Objective, Team, Totals},
    skills,
};
use rustc_hash::FxHashMap as HashMap;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, OnceLock},
};
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
    if ![2000, 2001].contains(&row.kind) || row.reset_group != 0 {
        return None;
    }
    let sets = master.condition_sets.get(&row.trigger_group)?;
    if sets.len() != 1 || sets[0].len() != 1 {
        return None;
    }
    let trigger = master.conditions.get(&sets[0][0])?;
    if !trigger.positive
        || !(row.trigger_type == 1 && trigger.kind == 7010
            || row.kind == 2000 && row.trigger_type == 2 && trigger.kind == 7021)
    {
        return None;
    }
    let mut value = row.value;
    if row.kind == 2001 {
        if row.cumulative_id == 0 {
            value = 0
        } else {
            let cumulative = master.cumulative.get(&row.cumulative_id)?;
            if cumulative.cap < 0 {
                return None;
            }
            if !(3000..=3005).contains(&cumulative.kind) {
                if row.max_value <= 0 {
                    return None;
                }
                let factor = f(f(row.max_value as f64) / 10000.0);
                let mill = note_factor_mill(factor);
                if !(0..i32::MAX).contains(&mill) {
                    return None;
                }
                return Some(f(f(mill as f64) / 100000.0));
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
#[derive(Clone, Copy, Default)]
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
            13002, 13003, 13004, 13005, 3004, 4004,
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
#[derive(Clone, Copy, Default)]
struct BudgetShape {
    budget: Budget,
    present: bool,
    raises: bool,
}
fn skill_budget(
    master: &Master,
    chart: &gekiso::Chart,
    skill: Option<&GekisoSkill>,
    level: i32,
) -> Option<BudgetShape> {
    let skill = skill?;
    if skill.mission_type != 4 && !chart.setup.missions.contains(&skill.mission_type) {
        return Some(BudgetShape::default());
    }
    let raises = skill
        .effects
        .iter()
        .any(|row| row.level == level && (row.kind == 13005 || row.kind == 12006 && row.value > 5));
    Some(BudgetShape {
        budget: admitted(master, Some(&skill.effects), level)?,
        present: true,
        raises,
    })
}
fn member_budget(master: &Master, chart: &gekiso::Chart, member: &Member) -> Option<BudgetShape> {
    if member.card.gekiso_skill_id == 0 {
        Some(BudgetShape::default())
    } else {
        skill_budget(
            master,
            chart,
            master.gekiso_skills.get(&member.card.gekiso_skill_id),
            member.growth.gekiso_skill_level,
        )
    }
}
fn snap_budget(master: &Master, chart: &gekiso::Chart, snap: &Snap) -> Option<BudgetShape> {
    let mut total = BudgetShape {
        present: true,
        ..BudgetShape::default()
    };
    for (i, &id) in snap.card.gekiso_support_skills.iter().enumerate() {
        if id == 0 {
            continue;
        }
        let part = skill_budget(
            master,
            chart,
            master.gekiso_support_skills.get(&id),
            snap.gekiso_skill_levels[i],
        )?;
        if !part.present {
            continue;
        }
        total.budget.positive = add(total.budget.positive, part.budget.positive);
        total.budget.commands += part.budget.commands;
        total.raises |= part.raises;
    }
    Some(total)
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
        let mut fixed_judgements =
            !chart.luck && chart.accuracy.great == 0.0 && chart.accuracy.miss_every == 0;
        for member in members {
            let info = member_budget(master, chart, member)?;
            if info.present {
                mb.push(info.budget);
            }
            fixed_judgements &= !info.raises;
        }
        for snap in active {
            let info = snap_budget(master, chart, snap)?;
            sb.push(info.budget);
            fixed_judgements &= !info.raises;
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
                let (actions, absolute) = live::rounding_profile(&slot, None);
                for e in &slot.effects {
                    let diff = f(e.delta as f64 / 100000.0);
                    let duration = window_ms(e.seconds, slot.extension_ms);
                    if !diff.is_finite() {
                        return None;
                    }
                    if duration > 0 && diff > 0.0 {
                        let multiplicity = if e.kind == 2000 {
                            1
                        } else {
                            [3, 4, 5, 6]
                                .into_iter()
                                .map(|j| e.judgements.iter().filter(|&&v| v == j).count())
                                .max()
                                .unwrap()
                        };
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
            let combo = if chart.accuracy.miss_every > 0 {
                2.0
            } else {
                chart.combo_factor[i]
            };
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
                        && start as i64 - 40 <= note.time_ms as i64
                        && note.time_ms as i64 <= end as i64 + 40
                }) {
                2.0
            } else {
                1.0
            };
            let luck = if chart.luck { 2.0 } else { 1.0 };
            let mut c = mul(live.adjustment, chart.difficulty_factor);
            for term in [
                frac,
                if fixed_judgements {
                    chart.judge_frac[chart.judgements[i]]
                } else {
                    fraction
                },
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
// JUST task counters affect the task gauge, while this reward route fixes rank.
// Keep raw-judgement effects and reject conditions that read that gauge, other
// performers or RNG before sharing these scoring-equivalent contexts.
pub(crate) struct JustContext {
    member: BTreeSet<(i32, i32)>,
    support: HashMap<(i32, i32), Option<(i32, i32)>>,
}
impl JustContext {
    pub(crate) fn new(
        master: &Master,
        chart: &gekiso::Chart,
        members: &[Member],
        snaps: &[Snap],
    ) -> Option<Self> {
        if chart.setup.missions.iter().any(|&m| m != 3) {
            return None;
        }
        let safe = |row: &SkillEffect| {
            [
                row.trigger_group,
                row.condition_group,
                row.release_group,
                row.reset_group,
            ]
            .iter()
            .all(|&group| {
                group == 0
                    || master.condition_sets.get(&group).is_some_and(|sets| {
                        sets.iter().flatten().all(|id| {
                            master.conditions.get(id).is_some_and(|c| {
                                [5000, 7010, 7013, 7020, 1030, 2000, 2001].contains(&c.kind)
                            })
                        })
                    })
            }) && (row.cumulative_id == 0
                || master
                    .cumulative
                    .get(&row.cumulative_id)
                    .is_some_and(|c| c.kind == 1000))
        };
        let mut member = BTreeSet::new();
        for m in members {
            let Some(skill) = master.gekiso_skills.get(&m.card.gekiso_skill_id) else {
                continue;
            };
            if skill.mission_type != 3 && skill.mission_type != 4 {
                continue;
            }
            let rows: Vec<_> = skill
                .effects
                .iter()
                .filter(|r| r.level == m.growth.gekiso_skill_level)
                .collect();
            if !rows
                .iter()
                .all(|r| [13000, 13002, 13003, 13004, 4004].contains(&r.kind) && safe(r))
            {
                return None;
            }
            if rows
                .iter()
                .all(|r| [13000, 13002, 13003, 13004, 4004].contains(&r.kind) && safe(r))
            {
                member.insert((m.card.gekiso_skill_id, m.growth.gekiso_skill_level));
            }
        }
        let mut support = HashMap::default();
        let mut shapes = HashMap::<Vec<u64>, (i32, i32)>::default();
        for snap in snaps {
            for (&id, &level) in snap
                .card
                .gekiso_support_skills
                .iter()
                .zip(&snap.gekiso_skill_levels)
            {
                if id == 0 || support.contains_key(&(id, level)) {
                    continue;
                }
                let skill = master.gekiso_support_skills.get(&id)?;
                if skill.mission_type != 3 && skill.mission_type != 4 {
                    continue;
                }
                let mut rows: Vec<_> = skill.effects.iter().filter(|r| r.level == level).collect();
                rows.sort_by_key(|r| r.id);
                if !rows
                    .iter()
                    .all(|r| [2001, 13005, 4004].contains(&r.kind) && safe(r))
                {
                    return None;
                }
                if rows.iter().all(|r| r.kind == 4004) {
                    support.insert((id, level), None);
                    continue;
                }
                let mut key = vec![skill.mission_type as u64];
                for row in rows {
                    key.extend([
                        row.trigger_type as u64,
                        row.execute_limit as u64,
                        row.reset_group as u64,
                        row.kind as u64,
                        row.value as u64,
                        row.seconds.to_bits(),
                        row.condition_group as u64,
                        row.trigger_group as u64,
                        row.release_group as u64,
                        row.cumulative_id as u64,
                        row.limit_count as u64,
                        row.max_value as u64,
                        row.targets.len() as u64,
                    ]);
                    key.extend(row.targets.iter().map(|&t| t as u64));
                }
                let canonical = *shapes.entry(key).or_insert((id, level));
                support.insert((id, level), Some(canonical));
            }
        }
        Some(Self { member, support })
    }
    pub(crate) fn normalize(&self, p: &mut Performer, master: &Master) {
        if p.gekisou_skill
            .is_some_and(|skill| self.member.contains(&skill))
        {
            p.gekisou_skill = None;
            p.mission_only = Some(1);
        }
        p.gekisou_support_skills
            .retain(|skill| self.support.get(skill) != Some(&None));
        // One skill per owner keeps effect keys distinct after aliasing IDs.
        if p.gekisou_support_skills.len() == 1 {
            if let Some(Some(canonical)) = self.support.get(&p.gekisou_support_skills[0]) {
                p.gekisou_support_skills[0] = *canonical;
            }
        }
        if p.band_id != 0
            && !p.gekisou_support_skills.iter().any(|&(id, level)| {
                master.gekiso_support_skills[&id]
                    .effects
                    .iter()
                    .filter(|r| r.level == level)
                    .flat_map(|r| {
                        [
                            r.trigger_group,
                            r.condition_group,
                            r.release_group,
                            r.reset_group,
                        ]
                    })
                    .flat_map(|g| {
                        master
                            .condition_sets
                            .get(&g)
                            .into_iter()
                            .flatten()
                            .flatten()
                    })
                    .filter_map(|id| master.conditions.get(id))
                    .filter(|c| c.kind == 5000)
                    .flat_map(|c| &c.targets)
                    .any(|id| {
                        master
                            .targets
                            .get(id)
                            .is_some_and(|t| t.band_id == p.band_id)
                    })
            })
        {
            p.band_id = 0;
        }
        if p.gekisou_skill.is_none() {
            p.mission_only = p.mission_only.map(|_| 1);
        }
        if p.gekisou_skill.is_none() && p.gekisou_support_skills.is_empty() {
            p.band_id = 0;
            p.character_id = 0;
            p.card_type = 0;
            p.tag_ids.clear();
            p.live_skill_categories.clear();
            p.gekisou_skill_categories.clear();
            p.gekisou_mission_type = 0;
        }
    }
}
pub(crate) fn deterministic_ranges(
    master: &Master,
    chart: &gekiso::Chart,
) -> Option<Vec<(i32, i32)>> {
    chart
        .deterministic_ranges
        .get_or_init(|| {
            let mut model = crate::full::model::Model::new(
                master,
                &[],
                &chart.full.notes,
                &chart.full.events,
                chart.params(),
                Some(&chart.setup),
                (0, false),
            )
            .ok()?;
            let last = chart.times.last().copied().unwrap_or(0).saturating_add(40);
            let mut ends = vec![None; chart.setup.fevers.len()];
            for k in chart.span.0..chart.span.1 {
                let frame = &chart.play.frames[k];
                model
                    .frame(frame.time_ms, &frame.judged, chart.play.dt[k])
                    .ok()?;
                if let Some(gk) = &model.gk {
                    for (i, state) in gk.controller.states.iter().enumerate() {
                        let end = if chart.luck {
                            crate::full::gekisou::COMPLETE
                        } else {
                            crate::full::gekisou::END
                        };
                        if state.state == end && ends[i].is_none() {
                            ends[i] = Some(frame.time_ms.saturating_add(40));
                        }
                    }
                }
            }
            Some(
                chart
                    .setup
                    .fevers
                    .iter()
                    .enumerate()
                    .map(|(i, &(start, _))| (start.saturating_sub(40), ends[i].unwrap_or(last)))
                    .collect(),
            )
        })
        .clone()
}
pub(crate) fn factor_operations(chart: &gekiso::Chart, ranges: &[(i32, i32)]) -> usize {
    *chart.factor_operations.get_or_init(|| {
        let max = (crate::full::core::frame(chart.music_length) + 50).max(1) as usize - 1;
        let mut execute = vec![0usize; max + 1];
        let mut undo = vec![0usize; max + 1];
        let mut previous = -1i32;
        let mut fever = 0;
        for frame in &chart.play.frames {
            let to = crate::full::core::frame(frame.time_ms)
                .max(0)
                .min(max as i32);
            let first = frame
                .judged
                .iter()
                .map(|n| crate::full::core::frame(n.time_ms).min(max as i32))
                .fold(to, i32::min);
            let mut second = to;
            while fever < chart.setup.fevers.len() && chart.setup.fevers[fever].0 <= frame.time_ms {
                second = second
                    .min(crate::full::core::frame(chart.setup.fevers[fever].0).min(max as i32));
                fever += 1;
            }
            for added in [first, second] {
                let upper = to.min(added - 1);
                let start = if upper < previous {
                    for block in upper + 1..=previous {
                        undo[block as usize] += 1;
                    }
                    upper + 1
                } else {
                    previous + 1
                };
                for block in start..=to {
                    execute[block as usize] += 1;
                }
                previous = to;
            }
        }
        // Disjoint ranges add one undo/replay of each affected score block.
        execute
            .iter()
            .zip(undo)
            .enumerate()
            .filter(|(block, _)| {
                ranges.iter().any(|&(start, end)| {
                    crate::full::core::frame(start).max(0) as usize <= *block
                        && *block <= crate::full::core::frame(end).max(0) as usize
                })
            })
            .map(|(_, (&e, u))| 2 * e + u + 3)
            .max()
            .unwrap_or(3)
    })
}
fn static_gk_group(master: &Master, group: i32, p: &Performer) -> Option<bool> {
    if group == 0 {
        return Some(true);
    }
    let mut any = false;
    for set in master.condition_sets.get(&group)? {
        let mut matched = true;
        for id in set {
            let c = master.conditions.get(id)?;
            if c.kind != 5000 {
                return None;
            }
            let found = c.targets.iter().any(|id| {
                master
                    .targets
                    .get(id)
                    .is_some_and(|t| crate::full::conditions::performer_matches(p, t))
            });
            matched &= found == c.positive;
        }
        any |= matched;
    }
    Some(any)
}
struct CounterFrame {
    just: f64,
    combo: f64,
    extras: [f64; 6],
    mission: i32,
}
fn possible_healthy_group(master: &Master, group: i32) -> bool {
    if group == 0 {
        return true;
    }
    master.condition_sets.get(&group).is_none_or(|sets| {
        sets.iter().any(|set| {
            set.iter().all(|id| {
                master.conditions.get(id).is_none_or(|c| {
                    if c.kind != 2001 {
                        return true;
                    }
                    c.values.first().is_none_or(|&value| {
                        (value <= master.live.life_base) == c.positive
                            || (value <= 2.0 * master.live.life_base) == c.positive
                    })
                })
            })
        })
    })
}
fn maximum_combo(master: &Master, members: &[Member], missions: &[i32]) -> f64 {
    let mut bonuses: Vec<_> = members
        .iter()
        .map(|m| {
            master
                .gekiso_skills
                .get(&m.card.gekiso_skill_id)
                .filter(|s| s.mission_type == 4 || missions.contains(&s.mission_type))
                .map_or(0.0, |s| {
                    s.effects
                        .iter()
                        .filter(|r| {
                            r.level == m.growth.gekiso_skill_level
                                && r.kind == 12000
                                && possible_healthy_group(master, r.trigger_group)
                                && possible_healthy_group(master, r.condition_group)
                        })
                        .map(|r| r.value.max(0) as f64)
                        .sum::<f64>()
                })
        })
        .collect();
    bonuses.sort_by(|a, b| b.total_cmp(a));
    1.0 + bonuses.into_iter().take(5).sum::<f64>()
}
struct ComboEnvelope {
    multiplier: f64,
    trace: Option<crate::reward_trace::ComboTrace>,
}
pub(crate) fn range_reset_only(master: &Master, group: i32) -> bool {
    group == 0
        || master.condition_sets.get(&group).is_some_and(|sets| {
            sets.len() == 1
                && sets[0].len() == 1
                && master
                    .conditions
                    .get(&sets[0][0])
                    .is_some_and(|c| c.kind == 7010 && c.positive)
        })
}
fn range_execute_limit(master: &Master, row: &SkillEffect) -> f64 {
    if row.execute_limit > 0 && range_reset_only(master, row.reset_group) {
        row.execute_limit as f64
    } else {
        f64::INFINITY
    }
}
#[derive(Clone, PartialEq, Eq, Hash)]
enum ComboModelKey {
    Multiplier(u64),
    Script(Vec<Performer>),
}
struct CumulativeWindows {
    counts: Vec<Option<CounterFrame>>,
    limits: Vec<(i32, f64, [f64; 6], f64)>,
    trace: Option<crate::reward_trace::ComboTrace>,
    weights: Vec<f64>,
    gains: HashMap<Vec<[i32; 5]>, [f64; 6]>,
}
impl CumulativeWindows {
    fn new(
        master: &Master,
        chart: &gekiso::Chart,
        members: &[Member],
        snaps: &[Snap],
        weights: Vec<f64>,
        ranges: &[(i32, i32)],
        envelope: ComboEnvelope,
    ) -> Option<Self> {
        let combo = envelope.multiplier;
        let mut plans = Vec::<Vec<Vec<(f64, f64, f64, f64)>>>::new();
        for snap in snaps {
            let mut variants = Vec::new();
            for member in members {
                let p = Performer::from_member(master, member, Some(snap));
                if p.gekisou_skill.is_none() {
                    continue;
                }
                let mut plan = Vec::new();
                for &(id, level) in &p.gekisou_support_skills {
                    let skill = master.gekiso_support_skills.get(&id)?;
                    if skill.mission_type != 4
                        && !chart.setup.missions.contains(&skill.mission_type)
                    {
                        continue;
                    }
                    for row in skill
                        .effects
                        .iter()
                        .filter(|r| r.level == level && r.kind == 13005)
                    {
                        if !static_gk_group(master, row.condition_group, &p)? {
                            continue;
                        }
                        let sets = master.condition_sets.get(&row.trigger_group)?;
                        if sets.len() != 1 {
                            return None;
                        }
                        let n = sets[0]
                            .iter()
                            .filter_map(|id| master.conditions.get(id))
                            .filter(|c| c.kind == 1030 && c.positive)
                            .filter_map(|c| c.values.first().copied())
                            .filter(|&n| n > 0.0)
                            .fold(f64::INFINITY, f64::min);
                        if !n.is_finite() || row.limit_count <= 0 {
                            return None;
                        }
                        let guarded = sets[0]
                            .iter()
                            .filter_map(|id| master.conditions.get(id))
                            .any(|c| {
                                c.kind == 7020
                                    && c.positive
                                    && !c.targets.is_empty()
                                    && c.targets.iter().all(|id| {
                                        master
                                            .targets
                                            .get(id)
                                            .is_some_and(|t| [1, 3, 4].contains(&t.mission_type))
                                    })
                            });
                        let range_position = sets[0].iter().position(|id| {
                            let c = &master.conditions[id];
                            c.kind == 7020
                                && c.positive
                                && !c.targets.is_empty()
                                && c.targets.iter().all(|id| {
                                    master
                                        .targets
                                        .get(id)
                                        .is_some_and(|t| [1, 3, 4].contains(&t.mission_type))
                                })
                        });
                        let count_position = sets[0]
                            .iter()
                            .position(|id| master.conditions[id].kind == 1030);
                        if !guarded
                            || !range_position
                                .zip(count_position)
                                .is_some_and(|(range, count)| {
                                    range < count
                                        && sets[0][..range].iter().all(|id| {
                                            [0, 5000].contains(&master.conditions[id].kind)
                                        })
                                })
                        {
                            return None;
                        }
                        plan.push((
                            n,
                            row.limit_count as f64,
                            if guarded { 0.0 } else { 1.0 },
                            range_execute_limit(master, row),
                        ));
                    }
                }
                if !plan.is_empty() && !variants.contains(&plan) {
                    variants.push(plan);
                }
            }
            if !variants.is_empty() {
                plans.push(variants);
            }
        }
        let mut just = Vec::new();
        let mut perfect = Vec::new();
        let compatible: BTreeSet<_> = chart
            .full
            .notes
            .iter()
            .filter(|n| master.live.just_types.contains(&n.judgement_type))
            .map(|n| n.id)
            .collect();
        let mut convertible = Vec::new();
        for frame in &chart.play.frames {
            for note in &frame.judged {
                if note.judgement == 6 {
                    just.push(note.time_ms);
                } else if note.judgement == 5 {
                    perfect.push(note.time_ms);
                    if compatible.contains(&note.note_id) {
                        convertible.push(note.time_ms);
                    }
                }
            }
        }
        just.sort_unstable();
        perfect.sort_unstable();
        convertible.sort_unstable();
        let causal = if plans.iter().flatten().flatten().all(|r| r.2 == 0.0) {
            let mut budgets = Vec::with_capacity(perfect.len() + 1);
            for count in 0..=perfect.len() {
                let mut options: Vec<_> = plans
                    .iter()
                    .map(|variants| {
                        variants
                            .iter()
                            .map(|plan| {
                                plan.iter()
                                    .map(|&(n, limit, _, cap)| {
                                        (count as f64 / n).floor().min(cap) * limit
                                    })
                                    .sum::<f64>()
                            })
                            .fold(0.0, f64::max)
                    })
                    .collect();
                options.sort_by(|a, b| b.total_cmp(a));
                let mut row = [0.0; 6];
                for c in 1..=5 {
                    row[c] = row[c - 1] + options.get(c - 1).copied().unwrap_or(0.0);
                }
                budgets.push(row);
            }
            Some(
                ranges
                    .iter()
                    .enumerate()
                    .map(|(range, &(start, end))| {
                        std::array::from_fn::<_, 6, _>(|c| {
                            let (mut perfect, mut converted, mut issued) = (0usize, 0usize, 0.0);
                            let mut times = Vec::new();
                            for frame in chart.play.frames.iter().filter(|f| {
                                chart.setup.fevers[range].0 <= f.time_ms && f.time_ms <= end
                            }) {
                                for note in &frame.judged {
                                    if note.judgement != 5 || note.time_ms < start {
                                        continue;
                                    }
                                    if compatible.contains(&note.note_id)
                                        && (converted as f64) < issued
                                    {
                                        converted += 1;
                                        times.push(note.time_ms);
                                    } else {
                                        perfect += 1;
                                    }
                                }
                                issued = budgets[perfect][c];
                            }
                            times
                        })
                    })
                    .collect::<Vec<_>>(),
            )
        } else {
            None
        };
        let counts = chart
            .times
            .iter()
            .map(|&time| {
                let (range, &(start, _)) = ranges
                    .iter()
                    .enumerate()
                    .find(|(_, r)| r.0 <= time && time <= r.1)?;
                let end = time.saturating_add(40);
                let j = just.partition_point(|&t| t <= end) - just.partition_point(|&t| t < start);
                let p = perfect.partition_point(|&t| t <= end)
                    - perfect.partition_point(|&t| t < start);
                let available = convertible.partition_point(|&t| t <= end)
                    - convertible.partition_point(|&t| t < start);
                let mut extras: Vec<_> = plans
                    .iter()
                    .map(|variants| {
                        variants
                            .iter()
                            .map(|plan| {
                                plan.iter()
                                    .map(|&(n, limit, initial, cap)| {
                                        ((p as f64 / n).floor() + initial).min(cap) * limit
                                    })
                                    .sum::<f64>()
                            })
                            .fold(0.0, f64::max)
                    })
                    .collect();
                extras.sort_by(|a, b| b.total_cmp(a));
                let mut capacities = [0.0; 6];
                for c in 1..=5 {
                    capacities[c] = (capacities[c - 1] + extras.get(c - 1).copied().unwrap_or(0.0))
                        .min(available as f64);
                }
                if let Some(causal) = &causal {
                    for (c, value) in capacities.iter_mut().enumerate() {
                        *value = causal[range][c].partition_point(|&t| t <= end) as f64;
                    }
                }
                Some(CounterFrame {
                    just: j as f64,
                    combo: (j + p) as f64 * combo,
                    extras: capacities,
                    mission: chart.setup.missions[range],
                })
            })
            .collect();
        let mut limits = Vec::new();
        for (i, &(start, end)) in ranges.iter().enumerate() {
            let to = end.saturating_add(40);
            let j = just.partition_point(|&t| t <= to) - just.partition_point(|&t| t < start);
            let p = perfect.partition_point(|&t| t <= to) - perfect.partition_point(|&t| t < start);
            let available = convertible.partition_point(|&t| t <= to)
                - convertible.partition_point(|&t| t < start);
            let mut extras: Vec<_> = plans
                .iter()
                .map(|variants| {
                    variants
                        .iter()
                        .map(|plan| {
                            plan.iter()
                                .map(|&(n, limit, initial, cap)| {
                                    ((p as f64 / n).floor() + initial).min(cap) * limit
                                })
                                .sum::<f64>()
                        })
                        .fold(0.0, f64::max)
                })
                .collect();
            extras.sort_by(|a, b| b.total_cmp(a));
            let mut capacities = [0.0; 6];
            for c in 1..=5 {
                capacities[c] = (capacities[c - 1] + extras.get(c - 1).copied().unwrap_or(0.0))
                    .min(available as f64);
            }
            if let Some(causal) = &causal {
                for (c, value) in capacities.iter_mut().enumerate() {
                    *value = causal[i][c].partition_point(|&t| t <= to) as f64;
                }
            }
            let maximum = envelope
                .trace
                .as_ref()
                .map_or((j + p) as f64 * combo, |trace| trace.maxima[i]);
            limits.push((chart.setup.missions[i], j as f64, capacities, maximum));
        }
        Some(Self {
            counts,
            limits,
            trace: envelope.trace.or_else(|| {
                (!chart.luck && chart.setup.missions.iter().all(|&m| m == 3))
                    .then(|| crate::reward_trace::ComboTrace::new(master, chart, &[]))
                    .flatten()
            }),
            weights,
            gains: HashMap::default(),
        })
    }
    fn commands(&self, rows: &[[i32; 5]]) -> [usize; 6] {
        let mut out = [0; 6];
        for &[value, maximum, n, kind, mission] in rows {
            let cap = (maximum as usize).div_ceil(value as usize);
            for &(range, just, extras, combo) in &self.limits {
                if mission != 4 && mission != range {
                    continue;
                }
                for (c, count) in out.iter_mut().enumerate() {
                    let raw = if kind == 7001 {
                        combo
                    } else {
                        just + extras[c]
                    };
                    let changes = if n > 0 {
                        (raw / n as f64).ceil().min(cap as f64) as usize
                    } else {
                        cap
                    };
                    if changes > 0 {
                        *count += 2 * changes + 4;
                    }
                }
            }
        }
        out
    }
    fn gain(&mut self, chart: &gekiso::Chart, mut rows: Vec<[i32; 5]>) -> [f64; 6] {
        rows.sort_unstable();
        if let Some(&gain) = self.gains.get(&rows) {
            return gain;
        }
        if rows.is_empty() {
            let gain = [0.0; 6];
            self.gains.insert(rows, gain);
            return gain;
        }
        if rows.iter().all(|r| r[3] == 7001) {
            if let Some(trace) = &self.trace {
                if let Some(values) = trace.support(chart, &rows) {
                    let value = values
                        .into_iter()
                        .zip(&self.weights)
                        .fold(0.0, |sum, (value, &weight)| {
                            add(sum, mul(up(value), weight))
                        });
                    let gain = [value; 6];
                    self.gains.insert(rows, gain);
                    return gain;
                }
            }
        }
        let mut gain = [0.0; 6];
        for (i, count) in self.counts.iter().enumerate() {
            let Some(frame) = count else {
                continue;
            };
            for &[value, maximum, n, kind, mission] in &rows {
                if mission != 4 && mission != frame.mission {
                    continue;
                }
                for (c, total) in gain.iter_mut().enumerate() {
                    let counter = if kind == 7001 {
                        frame.combo
                    } else {
                        frame.just + frame.extras[c]
                    };
                    let count = if n > 0 {
                        (counter / n as f64).ceil()
                    } else {
                        f64::INFINITY
                    };
                    let value = (value as f64 * count).min(maximum as f64);
                    let factor = up(f(
                        f(note_factor_mill(f(f(value) / 10000.0)) as f64) / 100000.0
                    ));
                    *total = add(*total, mul(self.weights[i], factor));
                }
            }
        }
        if chart.setup.missions.iter().all(|&m| m == 3) {
            if let Some(values) = self
                .trace
                .as_ref()
                .and_then(|trace| trace.support(chart, &rows))
            {
                gain[0] = values
                    .into_iter()
                    .zip(&self.weights)
                    .fold(0.0, |sum, (value, &weight)| {
                        add(sum, mul(up(value), weight))
                    });
            }
        }
        self.gains.insert(rows, gain);
        gain
    }
}
#[derive(Clone, Copy)]
struct RewardDual {
    lambda: f64,
    mu: f64,
}
type DualOffers = Vec<Vec<[f64; 5]>>;
type PowerFrontiers = Vec<[Vec<(f64, f64)>; 6]>;
#[derive(Clone)]
struct ConverterOptions {
    rows: [[[u32; 5]; 2]; 6],
    power: [[f64; 2]; 5],
    extras: [[f64; 2]; 5],
}
pub(crate) struct ConverterPartition {
    power: f64,
    ids: [u32; 5],
    deduction: f64,
}
pub(crate) struct DeterministicBound {
    pub(crate) base: f64,
    combo_multiplier: f64,
    capacity: usize,
    coefficient_limit: usize,
    duals: Vec<Vec<RewardDual>>,
    dual_offers: Vec<DualOffers>,
    option_cache: Vec<Option<Vec<i32>>>,
    option_envelopes: Vec<[u32; 6]>,
    option_converters: Vec<bool>,
    score_frontiers: Vec<PowerFrontiers>,
    snapshot_power: Arc<Vec<f64>>,
    snapshot_bonus: Arc<Vec<f64>>,
    converted: f64,
    profiles: Vec<[f64; 5]>,
    baseline_gains: Vec<f64>,
    width: usize,
    ids: Vec<[u32; 6]>,
    converters: Vec<bool>,
    shared: Vec<[f64; 5]>,
    intern: HashMap<[u64; 5], u32>,
    global: Vec<[u32; 6]>,
    suffix: Vec<[u32; 6]>,
    assignment: [[u32; 5]; 6],
    assignment_converters: [bool; 5],
    coefficients: HashMap<[u32; 5], Arc<Vec<f64>>>,
    coefficient_order: VecDeque<[u32; 5]>,
    score_values: HashMap<([u32; 5], u8), f64>,
    score_order: VecDeque<([u32; 5], u8)>,
    joint_scores: Option<(u64, Arc<Vec<f64>>)>,
    conversion_extras: Vec<f64>,
    conversion_extra_cap: f64,
    error_coefficient: f64,
    minimum_coefficient: f64,
    maximum_coefficient: f64,
    absolute_error: f64,
    just_ceiling: usize,
    normal_bounds: Vec<(usize, f64)>,
    normal_maxima: Vec<(usize, f64)>,
    option_normal: Vec<(usize, f64)>,
    normal_maximum: (usize, f64),
    normal_error_floor: f64,
    normal_error_unit: f64,
    assignment_normal_error: f64,
    converter_options: Option<ConverterOptions>,
}
impl DeterministicBound {
    pub(crate) fn new(
        master: &Master,
        chart: &gekiso::Chart,
        members: &[Member],
        snaps: &[Snap],
    ) -> Option<Self> {
        Self::with_combo(
            master,
            chart,
            members,
            snaps,
            maximum_combo(master, members, &chart.setup.missions),
            None,
        )
    }
    pub(crate) fn with_combo(
        master: &Master,
        chart: &gekiso::Chart,
        members: &[Member],
        snaps: &[Snap],
        combo_multiplier: f64,
        script: Option<&[Performer]>,
    ) -> Option<Self> {
        if chart.luck
            || chart.accuracy.great != 0.0
            || chart.accuracy.miss_every != 0
            || chart.setup.missions.iter().any(|&m| m != 1 && m != 3)
        {
            return None;
        }
        for member in members {
            if let Some(skill) = master
                .gekiso_skills
                .get(&member.card.gekiso_skill_id)
                .filter(|s| s.mission_type == 4 || chart.setup.missions.contains(&s.mission_type))
            {
                if skill
                    .effects
                    .iter()
                    .filter(|r| r.level == member.growth.gekiso_skill_level)
                    .any(|r| {
                        ![12000, 13000, 13002, 13003, 13004, 3004, 4004].contains(&r.kind)
                            || r.kind == 12000 && (r.seconds != 0.0 || r.value < 0)
                    })
                {
                    return None;
                }
            }
        }
        if chart
            .play
            .frames
            .windows(2)
            .any(|f| !(10..=40).contains(&(f[1].time_ms - f[0].time_ms)))
            || chart.play.frames.iter().any(|f| {
                f.judged
                    .iter()
                    .any(|j| (j.time_ms as i64 - f.time_ms as i64).abs() > 40)
            })
        {
            return None;
        }
        let ranges = deterministic_ranges(master, chart)?;
        if ranges.windows(2).any(|r| r[0].1 >= r[1].0) {
            return None;
        }
        let mut judged_times: Vec<_> = chart
            .play
            .frames
            .iter()
            .flat_map(|f| f.judged.iter().map(|n| n.time_ms))
            .collect();
        judged_times.sort_unstable();
        let trace = if let Some(actors) = script {
            Some(crate::reward_trace::ComboTrace::new(master, chart, actors)?)
        } else {
            None
        };
        let fraction = chart.judge_frac.iter().copied().fold(0.0, f64::max);
        let mut prefix = vec![0.0];
        let mut all_max = 0.0;
        let mut raw_weights = Vec::new();
        let mut beta_weights = Vec::new();
        for i in 0..chart.notes.len() {
            let range = chart.range_of[i];
            let rank = if range < 0 {
                1.0
            } else {
                add(1.0, up(chart.range_percent[range as usize] as f64 / 100.0))
            };
            let gk = if let Some(trace) = &trace {
                up(trace.gk[i])
            } else if let Some((_, &(start, _))) =
                chart
                    .setup
                    .fevers
                    .iter()
                    .enumerate()
                    .find(|(r, (start, end))| {
                        chart.setup.missions[*r] == 1
                            && *start as i64 - 40 <= chart.times[i] as i64
                            && chart.times[i] as i64 <= *end as i64 + 40
                    })
            {
                let count = judged_times
                    .partition_point(|&t| t <= chart.times[i].saturating_add(40))
                    - judged_times.partition_point(|&t| t < start.saturating_sub(40));
                let combo = count as f64 * combo_multiplier;
                let factor = master
                    .live
                    .combo
                    .get(&1)
                    .into_iter()
                    .flatten()
                    .filter(|&&(n, _)| n as f64 <= combo)
                    .map(|&(_, v)| v)
                    .fold(0.0, f64::max)
                    .min(1.0);
                up(f(factor + 1.0))
            } else {
                1.0
            };
            let raw = [
                chart.difficulty_factor,
                chart.note_frac[i],
                chart.combo_factor[i],
                rank,
                gk,
            ]
            .into_iter()
            .fold(master.live.adjustment, mul);
            all_max = add(all_max, mul(raw, fraction));
            raw_weights.push(raw);
            let beta = mul(raw, chart.judge_frac[chart.judgements[i]]);
            prefix.push(add(*prefix.last().unwrap(), beta));
            beta_weights.push(beta);
        }
        let all = *prefix.last()?;
        let judged_count = chart
            .play
            .frames
            .iter()
            .map(|f| f.judged.len())
            .sum::<usize>();
        let static_group = |group: i32, p: &Performer| static_gk_group(master, group, p);
        let mut windows = CumulativeWindows::new(
            master,
            chart,
            members,
            snaps,
            beta_weights,
            &ranges,
            ComboEnvelope {
                multiplier: combo_multiplier,
                trace,
            },
        )?;
        let mut support_gains = Vec::new();
        let single = |group: i32, kind: i32| {
            master.condition_sets.get(&group).is_some_and(|sets| {
                sets.len() == 1
                    && sets[0].len() == 1
                    && master
                        .conditions
                        .get(&sets[0][0])
                        .is_some_and(|c| c.kind == kind && c.positive)
            })
        };
        let mut profiles = Vec::new();
        let mut positives = Vec::new();
        let mut commands = Vec::new();
        let mut max_actions = 0;
        let mut max_absolute = 0.0_f64;
        let mut conversions = Vec::new();
        let mut conversion_regions = Vec::new();
        let mut ordinary_shapes = Vec::<Vec<(i32, f64)>>::new();
        let mut normal_bounds = Vec::new();
        for member in members {
            let mut member_positive = 0.0_f64;
            let mut member_commands = [0usize; 6];
            for snap in std::iter::once(None).chain(snaps.iter().map(Some)) {
                let p = Performer::from_member(master, member, snap);
                let mut positive = 0.0;
                let mut converts = 0.0;
                let mut regional_converts = Some(vec![0.0_f64; ranges.len()]);
                let mut score_rows = Vec::new();
                // Supports only execute for members with a Gekiso mission.
                if p.gekisou_skill.is_some() {
                    for &(id, level) in &p.gekisou_support_skills {
                        let skill = master.gekiso_support_skills.get(&id)?;
                        if skill.mission_type != 4
                            && !chart.setup.missions.contains(&skill.mission_type)
                        {
                            continue;
                        }
                        for row in skill.effects.iter().filter(|r| r.level == level) {
                            if row.kind == 12006 {
                                if row.value != 5
                                    || row.targets.iter().any(|id| {
                                        master.targets.get(id).is_some_and(|t| t.judgement == 6)
                                    })
                                {
                                    return None;
                                }
                                continue;
                            }
                            if row.kind == 12004 || row.kind == 4004 {
                                continue;
                            }

                            if row.kind == 13005 {
                                if row.limit_count <= 0
                                    || row.trigger_type != 1
                                    || !single(row.release_group, 7013)
                                {
                                    return None;
                                }
                                let groups = master.condition_sets.get(&row.trigger_group)?;
                                if groups.len() != 1 {
                                    return None;
                                }
                                let n = groups[0]
                                    .iter()
                                    .filter_map(|id| master.conditions.get(id))
                                    .filter(|c| c.kind == 1030 && c.positive)
                                    .filter_map(|c| c.values.first().copied())
                                    .filter(|&n| n > 0.0)
                                    .fold(f64::INFINITY, f64::min);
                                if !n.is_finite() {
                                    return None;
                                }
                                if groups[0]
                                    .iter()
                                    .filter_map(|id| master.conditions.get(id))
                                    .filter(|c| c.kind == 1030)
                                    .any(|c| {
                                        c.targets.len() != 1
                                            || master
                                                .targets
                                                .get(&c.targets[0])
                                                .is_none_or(|t| t.judgement != 5)
                                    })
                                {
                                    return None;
                                }
                                if static_group(row.condition_group, &p)? {
                                    let guarded = groups[0]
                                        .iter()
                                        .filter_map(|id| master.conditions.get(id))
                                        .find(|c| {
                                            c.kind == 7020
                                                && c.positive
                                                && !c.targets.is_empty()
                                                && c.targets.iter().all(|id| {
                                                    master.targets.get(id).is_some_and(|t| {
                                                        [1, 3, 4].contains(&t.mission_type)
                                                    })
                                                })
                                        });
                                    if let Some(condition) = guarded {
                                        let mut total = 0.0;
                                        for (range, &(start, end)) in ranges.iter().enumerate() {
                                            if !condition.targets.iter().any(|id| {
                                                let mission = master.targets[id].mission_type;
                                                mission == 4
                                                    || mission == chart.setup.missions[range]
                                            }) {
                                                continue;
                                            }
                                            let count = chart
                                                .play
                                                .frames
                                                .iter()
                                                .flat_map(|f| &f.judged)
                                                .filter(|j| {
                                                    j.judgement == 5
                                                        && start <= j.time_ms
                                                        && j.time_ms <= end
                                                })
                                                .count();
                                            let budget = mul(
                                                (count as f64 / n)
                                                    .floor()
                                                    .min(range_execute_limit(master, row)),
                                                row.limit_count as f64,
                                            );
                                            total = add(total, budget);
                                            if let Some(regions) = &mut regional_converts {
                                                regions[range] = add(regions[range], budget);
                                            }
                                        }
                                        converts = add(converts, total);
                                    } else {
                                        regional_converts = None;
                                        converts = add(
                                            converts,
                                            mul(
                                                (judged_count as f64 / n).floor(),
                                                row.limit_count as f64,
                                            ),
                                        );
                                    }
                                }
                                continue;
                            }
                            if row.kind != 2001 {
                                return None;
                            }
                            if row.value <= 0
                                || row.max_value <= 0
                                || row.seconds != 0.0
                                || row.trigger_type != 1
                                || row.reset_group != 0
                                || !single(row.trigger_group, 7010)
                                || !single(row.release_group, 7013)
                            {
                                return None;
                            }
                            let cumulative = master.cumulative.get(&row.cumulative_id)?;
                            if ![1000, 7001].contains(&cumulative.kind)
                                || cumulative.values.first().is_none_or(|&n| n <= 0.0)
                                || row.value as i64 * cumulative.cap as i64 > i32::MAX as i64
                            {
                                return None;
                            }
                            if static_group(row.condition_group, &p)? {
                                let n = cumulative.values.first().copied().unwrap_or(0.0) as i32;
                                let just = cumulative.kind == 7001
                                    || cumulative.targets.iter().all(|id| {
                                        master.targets.get(id).is_some_and(|t| t.judgement == 6)
                                    });
                                score_rows.push([
                                    row.value,
                                    row.max_value,
                                    if just { n } else { 0 },
                                    cumulative.kind,
                                    skill.mission_type,
                                ]);
                                positive = add(positive, up(f(f(row.max_value as f64) / 10000.0)));
                            }
                        }
                    }
                }
                let mut row = [0.0; 5];
                let count = windows.commands(&score_rows);
                support_gains.push(windows.gain(chart, score_rows));
                let slot = skills::resolve(master, member, snap, &mut Vec::new());
                if slot
                    .convert
                    .as_ref()
                    .is_some_and(|c| c.judgements.contains(&6))
                {
                    return None;
                }
                let (actions, absolute) =
                    live::rounding_profile(&slot, Some(master.live.life_base));
                let mut shape = Vec::new();
                for effect in &slot.effects {
                    if effect.delta < 0
                        || effect.gate.blocks(master.live.life_base)
                            != effect.gate.blocks(2.0 * master.live.life_base)
                    {
                        return None;
                    }
                    if effect.gate.blocks(master.live.life_base) {
                        continue;
                    }
                    let delta = f(effect.delta as f64 / 100000.0);
                    shape.push((window_ms(effect.seconds, slot.extension_ms), delta));
                    if effect.kind != 2000
                        && !effect.judgements.iter().any(|&j| (4..=6).contains(&j))
                    {
                        continue;
                    }
                    for (event, &(_, time)) in chart.full.events.iter().take(5).enumerate() {
                        let start = chart.times.partition_point(|&t| t < time);
                        let end = chart.times.partition_point(|&t| {
                            t < time + window_ms(effect.seconds, slot.extension_ms)
                        });
                        let weight =
                            (prefix[end] - prefix[start]).max(0.0) + 32.0 * f64::EPSILON * all;
                        row[event] = add(row[event], mul(delta, up(weight)));
                    }
                }
                max_actions = max_actions.max(actions);
                max_absolute = max_absolute.max(absolute);
                normal_bounds.push((actions, absolute));
                ordinary_shapes.push(shape);
                member_positive = member_positive.max(positive);
                for c in 0..=5 {
                    member_commands[c] = member_commands[c].max(count[c]);
                }
                profiles.push(row);
                conversions.push(converts);
                conversion_regions.push(regional_converts);
            }
            positives.push(member_positive);
            commands.push(member_commands);
        }
        positives.sort_by(|a, b| b.total_cmp(a));
        let positive = positives.into_iter().take(5).fold(0.0, add);
        let u = 2.0_f64.powi(-24);
        let mut errors = [0.0; 6];
        for c in 0..=5 {
            let mut counts: Vec<_> = commands.iter().map(|v| v[c]).collect();
            counts.sort_by(|a, b| b.cmp(a));
            let operations =
                factor_operations(chart, &ranges) * counts.into_iter().take(5).sum::<usize>();
            let n = operations as f64 * u;
            if n >= 0.25 {
                return None;
            }
            errors[c] = mul(mul(n, add(1.0, mul(2.0, positive))), up(1.0 / (1.0 - n)));
        }
        let residual = mul(
            16.0 * (10 * max_actions + 5) as f64 * u,
            mul(5.0, max_absolute),
        );
        let mut rounding = 1.0;
        for _ in 0..16 {
            rounding = mul(rounding, up(1.0 / (1.0 - u)));
        }
        let maximum_coefficient = up(
            mul(all_max, add(add(1.0, positive), mul(5.0, max_absolute)))
                / chart.full.converted as f64,
        );
        let error_coefficient = up(add(
            mul(
                mul(
                    all_max,
                    add(errors.iter().copied().fold(0.0, f64::max), residual),
                ),
                rounding,
            ),
            mul(
                mul(maximum_coefficient, chart.full.converted as f64),
                (rounding - 1.0).next_up(),
            ),
        ) / chart.full.converted as f64);
        let minimum_judge = chart.judge_frac[5].min(chart.judge_frac[6]);
        let minimum_coefficient = (chart.note_frac.iter().fold(0.0_f64, |sum, &frac| {
            let term = (((master.live.adjustment * chart.difficulty_factor).next_down() * frac)
                .next_down()
                * minimum_judge)
                .next_down();
            (sum + term).next_down()
        }) / chart.full.converted as f64)
            .next_down();
        let absolute_error = add(
            mul(
                chart.notes.len() as f64,
                add(
                    1.0,
                    chart.range_percent.iter().copied().max().unwrap_or(0) as f64 / 100.0,
                ),
            ),
            chart.range_percent.len() as f64,
        );
        let durations: Vec<_> = ordinary_shapes
            .iter()
            .flatten()
            .map(|&(d, _)| d)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let envelope: Vec<_> = durations
            .iter()
            .map(|&duration| {
                ordinary_shapes
                    .iter()
                    .map(|shape| {
                        shape
                            .iter()
                            .filter(|&&(d, _)| d >= duration)
                            .map(|&(_, delta)| delta)
                            .fold(0.0, add)
                    })
                    .fold(0.0, f64::max)
            })
            .collect();
        let eligible: BTreeSet<_> = chart
            .full
            .notes
            .iter()
            .filter(|n| master.live.just_types.contains(&n.judgement_type))
            .map(|n| n.id)
            .collect();
        let mut gains = Vec::new();
        let mut region_gains = vec![Vec::new(); ranges.len()];
        for (i, note) in chart.notes.iter().enumerate() {
            if !eligible.contains(&note.id) || chart.judgements[i] != 5 {
                continue;
            }
            let mut factor = add(1.0, positive);
            for &(_, start) in chart.full.events.iter().take(5) {
                let elapsed = chart.times[i] - start;
                if elapsed >= 0 {
                    if let Some(at) = durations.iter().position(|&d| d > elapsed) {
                        factor = add(factor, envelope[at]);
                    }
                }
            }
            let gain = mul(
                mul(raw_weights[i], up(fraction - chart.judge_frac[5])),
                factor,
            );
            gains.push(gain);
            for (range, &(start, end)) in ranges.iter().enumerate() {
                if start <= note.time_ms && note.time_ms <= end {
                    region_gains[range].push(gain);
                }
            }
        }
        gains.sort_by(|a, b| b.total_cmp(a));
        let mut prefix_gain = vec![0.0];
        for gain in gains {
            prefix_gain.push(add(*prefix_gain.last().unwrap(), gain));
        }
        let regional_prefix: Vec<Vec<f64>> = region_gains
            .into_iter()
            .map(|mut gains| {
                gains.sort_by(|a, b| b.total_cmp(a));
                let mut prefix = vec![0.0];
                for gain in gains {
                    prefix.push(add(*prefix.last().unwrap(), gain));
                }
                prefix
            })
            .collect();
        let width = snaps.len() + 1;
        let capacity = (1..width)
            .filter(|&snap| (0..members.len()).any(|m| conversions[m * width + snap] > 0.0))
            .count()
            .min(5);
        let mut result = Self {
            combo_multiplier,
            capacity,
            coefficient_limit: 4096,
            duals: Vec::new(),
            dual_offers: vec![Vec::new(); members.len()],
            option_cache: vec![None; members.len()],
            option_envelopes: vec![[0; 6]; members.len()],
            option_converters: vec![false; members.len()],
            score_frontiers: vec![Vec::new(); members.len()],
            snapshot_power: Arc::new(Vec::new()),
            snapshot_bonus: Arc::new(Vec::new()),
            base: mul(
                add(add(all, mul(all, errors[0])), mul(all_max, residual)),
                rounding,
            ),
            converted: chart.full.converted as f64,
            profiles: Vec::new(),
            baseline_gains: Vec::new(),
            width: snaps.len() + 1,
            ids: Vec::new(),
            converters: Vec::new(),
            shared: Vec::new(),
            intern: HashMap::default(),
            global: Vec::new(),
            suffix: Vec::new(),
            assignment: [[0; 5]; 6],
            assignment_converters: [false; 5],
            coefficients: HashMap::default(),
            coefficient_order: VecDeque::new(),
            score_values: HashMap::default(),
            score_order: VecDeque::new(),
            joint_scores: None,
            conversion_extras: Vec::new(),
            conversion_extra_cap: mul(*prefix_gain.last().unwrap(), rounding),
            converter_options: None,
            error_coefficient,
            minimum_coefficient,
            maximum_coefficient,
            absolute_error,
            just_ceiling: windows
                .limits
                .iter()
                .map(|(_, just, extras, _)| (just + extras[capacity]).ceil() as usize)
                .max()
                .unwrap_or(0),
            normal_maxima: normal_bounds
                .chunks(width)
                .map(|rows| {
                    rows.iter()
                        .fold((0usize, 0.0_f64), |(a, b), &(c, d)| (a.max(c), b.max(d)))
                })
                .collect(),
            option_normal: vec![(0, 0.0); members.len()],
            normal_bounds,
            normal_maximum: (max_actions, max_absolute),
            normal_error_floor: ((((all_max * residual).next_down() * rounding).next_down()
                / chart.full.converted as f64)
                .next_down())
            .max(0.0),
            normal_error_unit: mul(
                16.0 * u,
                up(mul(all_max, rounding) / chart.full.converted as f64),
            ),
            assignment_normal_error: up(
                mul(mul(all_max, residual), rounding) / chart.full.converted as f64
            ),
        };
        for (((row, gains), converts), regions) in profiles
            .into_iter()
            .zip(support_gains)
            .zip(conversions)
            .zip(conversion_regions)
        {
            result.baseline_gains.push(up(gains[0] / result.converted));
            let extra = regions.map_or_else(
                || prefix_gain[(converts as usize).min(prefix_gain.len() - 1)],
                |regions| {
                    regions
                        .iter()
                        .zip(&regional_prefix)
                        .fold(0.0, |sum, (&count, prefix)| {
                            add(sum, prefix[(count as usize).min(prefix.len() - 1)])
                        })
                },
            );
            result
                .conversion_extras
                .push((extra * rounding).next_down().max(0.0));
            let mut ids = [0; 6];
            for c in 0..=5 {
                let pad = if c == 0 || capacity == 0 {
                    0.0
                } else {
                    up((mul(all_max, errors[c]) - mul(all, errors[0])).max(0.0) / 5.0)
                };
                let profile = std::array::from_fn(|event| {
                    mul(add(add(add(row[event], gains[c]), extra), pad), rounding)
                });
                ids[c] = result.intern(profile);
                if c == 5 {
                    result.profiles.push(profile);
                }
            }
            result.ids.push(ids);
            result.converters.push(converts > 0.0);
        }
        for i in 0..result.profiles.len() {
            result.profiles[i] = result.shared[result.ids[i][capacity] as usize];
        }
        for member in 0..members.len() {
            let mut ids = [0; 6];
            for (c, id) in ids.iter_mut().enumerate() {
                let mut row = [0.0_f64; 5];
                for snap in 0..result.width {
                    for (event, value) in row.iter_mut().enumerate() {
                        *value = value.max(
                            result.shared[result.ids[member * result.width + snap][c] as usize]
                                [event],
                        );
                    }
                }
                *id = result.intern(row);
            }
            result.global.push(ids);
        }
        Some(result)
    }
    fn intern(&mut self, row: [f64; 5]) -> u32 {
        let key = row.map(f64::to_bits);
        let next = self.shared.len() as u32;
        *self.intern.entry(key).or_insert_with(|| {
            self.shared.push(row);
            next
        })
    }
    pub(crate) fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        self.suffix = vec![[0; 6]; characters.len() + 1];
        let mut rows = [[0.0_f64; 5]; 6];
        for (c, &row) in rows.iter().enumerate() {
            let id = self.intern(row);
            self.suffix[characters.len()][c] = id;
        }
        for from in (0..characters.len()).rev() {
            for &m in &characters[from] {
                for (c, row) in rows.iter_mut().enumerate() {
                    for (event, value) in row.iter_mut().enumerate() {
                        *value = value.max(self.shared[self.global[m][c] as usize][event]);
                    }
                }
            }
            for (c, &row) in rows.iter().enumerate() {
                let id = self.intern(row);
                self.suffix[from][c] = id;
            }
        }
    }
    fn prepare_assignment(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
        power: Arc<Vec<f64>>,
        bonus: Arc<Vec<f64>>,
        estimate: f64,
        maximum_bonus: f64,
    ) {
        self.joint_scores = None;
        self.converter_options = None;
        self.snapshot_power = power;
        self.snapshot_bonus = bonus;
        if self.duals.is_empty() {
            let with_bonus = self.snapshot_bonus.iter().any(|&b| b != 0.0);
            for c in 0..=self.capacity {
                let maximum = self
                    .ids
                    .iter()
                    .map(|ids| self.shared[ids[c] as usize].iter().sum::<f64>() / 5.0)
                    .fold(0.0, f64::max);
                let lambda = ((self.base + 5.0 * maximum) / estimate.max(1.0)).sqrt();
                let spread = (lambda * estimate.max(1.0) + (self.base + 5.0 * maximum) / lambda)
                    / maximum_bonus.max(1.0);
                let mut params = Vec::new();
                for scale in [1.0, 0.75, 1.5, 0.9, 1.2, 0.65, 1.8] {
                    for factor in if with_bonus {
                        &[0.0, 1.0, 0.25, 4.0][..]
                    } else {
                        &[0.0][..]
                    } {
                        params.push(RewardDual {
                            lambda: lambda * scale,
                            mu: factor * spread,
                        });
                    }
                }
                self.duals.push(params);
            }
        }
        for (slot, &m) in members.iter().enumerate() {
            if self.option_cache[m].as_ref() != Some(&options[slot]) {
                let mut raw_envelopes = [[0.0; 5]; 6];
                let mut offers = Vec::new();
                for (c, params) in self.duals.iter().enumerate() {
                    let mut envelope = [0.0_f64; 5];
                    let mut cells = vec![[f64::NEG_INFINITY; 5]; params.len()];
                    for &snap in &options[slot] {
                        let row =
                            self.shared[self.ids[m * self.width + (snap + 1) as usize][c] as usize];
                        let power = self.snapshot_power[m * self.width
                            + if snap < 0 {
                                self.width - 1
                            } else {
                                snap as usize
                            }];
                        let bonus = if snap < 0 {
                            0.0
                        } else {
                            self.snapshot_bonus[snap as usize]
                        };
                        for (event, value) in envelope.iter_mut().enumerate() {
                            *value = value.max(row[event]);
                        }
                        for (i, param) in params.iter().enumerate() {
                            let fixed = ((param.lambda * power).next_up()
                                + (param.mu * bonus).next_up())
                            .next_up();
                            for (event, value) in cells[i].iter_mut().enumerate() {
                                *value = value
                                    .max((fixed + (row[event] / param.lambda).next_up()).next_up());
                            }
                        }
                    }
                    raw_envelopes[c] = envelope;
                    offers.push(cells);
                }
                self.option_converters[m] = options[slot]
                    .iter()
                    .any(|&snap| self.converters[m * self.width + (snap + 1) as usize]);
                self.option_envelopes[m] = raw_envelopes.map(|row| self.intern(row));
                self.dual_offers[m] = offers;
                self.option_cache[m] = Some(options[slot].clone());
            }
            self.assignment_converters[slot] = self.option_converters[m];
            for c in 0..=5 {
                self.assignment[c][slot] = self.option_envelopes[m][c];
            }
        }
    }
    fn dual_possible(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        power: f64,
        bonus: f64,
        targets: &[JointTarget],
    ) -> bool {
        if targets.is_empty() {
            return false;
        }
        let c = (0..5)
            .filter(|&slot| {
                if slot < from {
                    self.converters[members[slot] * self.width + (assigned[slot] + 1) as usize]
                } else {
                    self.assignment_converters[slot]
                }
            })
            .count()
            .min(self.capacity);
        let mut possible = vec![true; targets.len()];
        for (index, param) in self.duals[c].iter().enumerate() {
            let mut rows = [[0.0; 5]; 5];
            for slot in 0..5 {
                let m = members[slot];
                if slot < from {
                    let snap = assigned[slot];
                    let values =
                        self.shared[self.ids[m * self.width + (snap + 1) as usize][c] as usize];
                    let p = self.snapshot_power[m * self.width
                        + if snap < 0 {
                            self.width - 1
                        } else {
                            snap as usize
                        }];
                    let b = if snap < 0 {
                        0.0
                    } else {
                        self.snapshot_bonus[snap as usize]
                    };
                    let fixed = ((param.lambda * p).next_up() + (param.mu * b).next_up()).next_up();
                    for (event, value) in rows[slot].iter_mut().enumerate() {
                        *value = (fixed + (values[event] / param.lambda).next_up()).next_up();
                    }
                } else {
                    rows[slot] = self.dual_offers[m][c][index];
                }
            }
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
            let constant = (((param.lambda * power).next_up()
                + (self.base / param.lambda).next_up())
            .next_up()
                + (param.mu * bonus).next_up())
            .next_up();
            let upper = (constant + dp[31]).next_up();
            for (i, target) in targets.iter().enumerate() {
                let root = (2.0 * target.product.max(0.0).sqrt()).next_down();
                let required = (root + (param.mu * target.bonus).next_down()).next_down();
                if upper < required {
                    possible[i] = false;
                }
            }
            if possible.iter().all(|&p| !p) {
                return false;
            }
        }
        true
    }
    pub(crate) fn prepare_score_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.joint_scores = None;
        self.converter_options = None;
        for (slot, &member) in members.iter().enumerate() {
            if self.option_cache[member].as_ref() != Some(&options[slot]) {
                let mut rows = [[0.0_f64; 5]; 6];
                let mut normal = (0usize, 0.0_f64);
                let mut frontiers: PowerFrontiers = (0..=self.capacity)
                    .map(|_| std::array::from_fn(|_| Vec::new()))
                    .collect();
                for &snap in &options[slot] {
                    let bound = self.normal_bounds[member * self.width + (snap + 1) as usize];
                    normal = (normal.0.max(bound.0), normal.1.max(bound.1));
                    for (c, row) in rows.iter_mut().enumerate().take(self.capacity + 1) {
                        let values = self.shared
                            [self.ids[member * self.width + (snap + 1) as usize][c] as usize];
                        for (event, value) in row.iter_mut().enumerate() {
                            *value = value.max(values[event]);
                            let power = self.snapshot_power[member * self.width
                                + if snap < 0 {
                                    self.width - 1
                                } else {
                                    snap as usize
                                }];
                            frontiers[c][event].push((power, values[event]));
                        }
                        let power = self.snapshot_power[member * self.width
                            + if snap < 0 {
                                self.width - 1
                            } else {
                                snap as usize
                            }];
                        frontiers[c][5].push((power, up(values.into_iter().fold(0.0, add) / 5.0)));
                    }
                }
                for events in &mut frontiers {
                    for points in events {
                        points
                            .sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(b.1.total_cmp(&a.1)));
                        let mut high = f64::NEG_INFINITY;
                        points.retain(|&(_, value)| {
                            if value > high {
                                high = value;
                                true
                            } else {
                                false
                            }
                        });
                    }
                }
                self.score_frontiers[member] = frontiers;
                self.option_envelopes[member] = rows.map(|row| self.intern(row));
                self.option_normal[member] = normal;
                self.option_converters[member] = options[slot]
                    .iter()
                    .any(|&snap| self.converters[member * self.width + (snap + 1) as usize]);
                self.option_cache[member] = Some(options[slot].clone());
            }
            self.assignment_converters[slot] = self.option_converters[member];
            for c in 0..=self.capacity {
                self.assignment[c][slot] = self.option_envelopes[member][c];
            }
        }
        let (actions, absolute) = members
            .iter()
            .map(|&m| self.option_normal[m])
            .fold((0usize, 0.0_f64), |(a, b), (c, d)| (a + c, add(b, d)));
        self.assignment_normal_error = mul(
            mul(self.normal_error_unit, (2 * actions + 5) as f64),
            absolute,
        );
    }
    pub(crate) fn joint_score_caps(
        &mut self,
        members: &[usize],
        snapshot_power: &[f64],
        base_power: f64,
    ) -> Arc<Vec<f64>> {
        if let Some((power, caps)) = &self.joint_scores {
            if *power == base_power.to_bits() {
                return Arc::clone(caps);
            }
        }
        let count = self.width - 1;
        let columns = count + 5;
        let bad = -1e30;
        let mut powers = vec![vec![bad; columns]; 5];
        let mut coefficients = vec![vec![bad; columns]; 5];
        let mut blended = vec![vec![bad; columns]; 5];
        let mut allowed = Vec::new();
        let c = self
            .assignment_converters
            .iter()
            .filter(|&&yes| yes)
            .count()
            .min(self.capacity);
        for (slot, &member) in members.iter().enumerate() {
            let mut cells = Vec::new();
            for &snap in self.option_cache[member].as_ref().unwrap() {
                let p = snapshot_power
                    [member * self.width + if snap < 0 { count } else { snap as usize }];
                let row =
                    self.shared[self.ids[member * self.width + (snap + 1) as usize][c] as usize];
                if snap < 0 {
                    for (column, value) in powers[slot].iter_mut().enumerate().skip(count) {
                        *value = p;
                        cells.push((column, p, row));
                    }
                } else {
                    powers[slot][snap as usize] = p;
                    cells.push((snap as usize, p, row));
                }
            }
            allowed.push(cells);
        }
        let mut matching = crate::assignment::Scratch::default();
        let pcap = add(base_power, matching.maximum_upper(&powers, columns));
        let mut caps = Vec::with_capacity(120);
        for order in live::orders() {
            let mut events = [0; 5];
            for (event, &slot) in order.iter().enumerate() {
                events[slot] = event;
            }
            for slot in 0..5 {
                for &(column, _, row) in &allowed[slot] {
                    coefficients[slot][column] = row[events[slot]];
                }
            }
            let ccap = add(self.base, matching.maximum_upper(&coefficients, columns));
            if pcap <= 0.0 || ccap <= 0.0 || !pcap.is_finite() || !ccap.is_finite() {
                caps.push(f64::INFINITY);
                continue;
            }
            let mut upper = up(mul(pcap, ccap) / self.converted);
            let ratio = (ccap / pcap).sqrt();
            for scale in [1.0, 0.85, 1.15] {
                let lambda = ratio * scale;
                for slot in 0..5 {
                    for &(column, p, row) in &allowed[slot] {
                        blended[slot][column] = add(mul(lambda, p), up(row[events[slot]] / lambda));
                    }
                }
                let variable = matching.maximum_upper(&blended, columns);
                if variable < bad / 2.0 {
                    continue;
                }
                let total = add(
                    add(mul(lambda, base_power), up(self.base / lambda)),
                    variable,
                );
                upper = upper.min(up(mul(total, total) / (4.0 * self.converted)));
            }
            caps.push(upper);
        }
        caps.sort_by(f64::total_cmp);
        let caps = Arc::new(caps);
        self.joint_scores = Some((base_power.to_bits(), Arc::clone(&caps)));
        caps
    }
    pub(crate) fn cached_joint_caps(&self) -> Option<Arc<Vec<f64>>> {
        self.joint_scores.as_ref().map(|(_, caps)| Arc::clone(caps))
    }
    pub(crate) fn score_ceiling(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        power: f64,
        criterion: crate::objectives::Criterion,
    ) -> f64 {
        let c = (0..5)
            .filter(|&slot| {
                if slot < from {
                    self.converters[members[slot] * self.width + (assigned[slot] + 1) as usize]
                } else {
                    self.assignment_converters[slot]
                }
            })
            .count()
            .min(self.capacity);
        let ids: [u32; 5] = std::array::from_fn(|slot| {
            if slot < from {
                self.ids[members[slot] * self.width + (assigned[slot] + 1) as usize][c]
            } else {
                self.assignment[c][slot]
            }
        });
        let coefficient = self.score_coefficient(ids, criterion);
        let coefficient = self.with_normal_error(coefficient, self.assignment_normal_error);
        let deduction = self.conversion_overlap(members, assigned, from);
        mul(power, (coefficient - deduction).next_up().max(0.0))
    }
    fn score_coefficient(
        &mut self,
        mut ids: [u32; 5],
        criterion: crate::objectives::Criterion,
    ) -> f64 {
        use crate::objectives::Criterion;
        ids.sort_unstable();
        let tag = match criterion {
            Criterion::Mean => 0,
            Criterion::Min => 1,
            Criterion::Max => 2,
        };
        let key = (ids, tag);
        if let Some(&value) = self.score_values.get(&key) {
            value
        } else {
            let rows = ids.map(|id| self.shared[id as usize]);
            let variable = if criterion == Criterion::Mean {
                rows.iter().fold(0.0, |sum, row| {
                    add(sum, up(row.iter().copied().fold(0.0, add) / 5.0))
                })
            } else {
                let initial = if criterion == Criterion::Max {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                };
                let mut dp = [initial; 32];
                dp[0] = 0.0;
                for mask in 0usize..31 {
                    let row = mask.count_ones() as usize;
                    for (event, &weight) in rows[row].iter().enumerate() {
                        if mask & (1 << event) != 0 {
                            continue;
                        }
                        let next = mask | (1 << event);
                        let value = add(dp[mask], weight);
                        dp[next] = if criterion == Criterion::Max {
                            dp[next].max(value)
                        } else {
                            dp[next].min(value)
                        };
                    }
                }
                dp[31]
            };
            let value = up(add(self.base, variable) / self.converted);
            if self.score_values.len() >= 65536 {
                if let Some(old) = self.score_order.pop_front() {
                    self.score_values.remove(&old);
                }
            }
            self.score_values.insert(key, value);
            self.score_order.push_back(key);
            value
        }
    }
    fn with_normal_error(&self, coefficient: f64, error: f64) -> f64 {
        coefficient.min(add(
            (coefficient - self.normal_error_floor).next_up().max(0.0),
            error,
        ))
    }
    pub(crate) fn joint_score_ceiling(
        &self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        base_power: f64,
        totals: Totals,
        criterion: crate::objectives::Criterion,
    ) -> f64 {
        use crate::objectives::Criterion;
        let count = (0..5)
            .filter(|&slot| {
                if slot < from {
                    self.converters[members[slot] * self.width + (assigned[slot] + 1) as usize]
                } else {
                    self.assignment_converters[slot]
                }
            })
            .count()
            .min(self.capacity);
        let ratio = ((self.base + totals.skill) / totals.power.max(1.0)).sqrt();
        if !ratio.is_finite() || ratio <= 0.0 {
            return f64::INFINITY;
        }
        let mut upper = f64::INFINITY;
        for scale in [1.0, 0.75, 1.5] {
            let lambda = ratio * scale;
            let rows: [[f64; 5]; 5] = std::array::from_fn(|slot| {
                let member = members[slot];
                std::array::from_fn(|event| {
                    let event = if criterion == Criterion::Mean {
                        5
                    } else {
                        event
                    };
                    if slot < from {
                        let snap = assigned[slot];
                        let power = self.snapshot_power[member * self.width
                            + if snap < 0 {
                                self.width - 1
                            } else {
                                snap as usize
                            }];
                        let row = self.shared
                            [self.ids[member * self.width + (snap + 1) as usize][count] as usize];
                        let value = if event == 5 {
                            up(row.into_iter().fold(0.0, add) / 5.0)
                        } else {
                            row[event]
                        };
                        add(mul(lambda, power), up(value / lambda))
                    } else {
                        self.score_frontiers[member][count][event]
                            .iter()
                            .map(|&(power, value)| add(mul(lambda, power), up(value / lambda)))
                            .fold(f64::NEG_INFINITY, f64::max)
                    }
                })
            });
            let variable = if criterion == Criterion::Mean {
                rows.into_iter().fold(0.0, |sum, row| add(sum, row[0]))
            } else {
                let mut dp = [if criterion == Criterion::Min {
                    f64::INFINITY
                } else {
                    f64::NEG_INFINITY
                }; 32];
                dp[0] = 0.0;
                for mask in 0usize..31 {
                    let slot = mask.count_ones() as usize;
                    for (event, &weight) in rows[slot].iter().enumerate() {
                        if mask & (1 << event) == 0 {
                            let next = mask | (1 << event);
                            let value = add(dp[mask], weight);
                            dp[next] = if criterion == Criterion::Min {
                                dp[next].min(value)
                            } else {
                                dp[next].max(value)
                            };
                        }
                    }
                }
                dp[31]
            };
            let total = add(
                add(mul(lambda, base_power), up(self.base / lambda)),
                variable,
            );
            upper = upper.min(up(mul(total, total) / (4.0 * self.converted)));
        }
        upper
    }
    fn order_coefficients(&mut self, mut ids: [u32; 5]) -> Arc<Vec<f64>> {
        ids.sort_unstable();
        if let Some(coefficients) = self.coefficients.get(&ids) {
            return Arc::clone(coefficients);
        }
        let mut rows: Vec<_> = live::orders()
            .iter()
            .map(|order| {
                let sum = order
                    .iter()
                    .enumerate()
                    .fold(self.base, |sum, (event, &slot)| {
                        add(sum, self.shared[ids[slot] as usize][event])
                    });
                up(sum / self.converted)
            })
            .collect();
        rows.sort_by(f64::total_cmp);
        let rows = Arc::new(rows);
        if self.coefficients.len() >= self.coefficient_limit {
            if let Some(key) = self.coefficient_order.pop_front() {
                self.coefficients.remove(&key);
            }
        }
        self.coefficient_order.push_back(ids);
        self.coefficients.insert(ids, Arc::clone(&rows));
        rows
    }
    fn assignment_coefficients(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
    ) -> Arc<Vec<f64>> {
        let c = (0..5)
            .filter(|&slot| {
                if slot < from {
                    self.converters[members[slot] * self.width + (assigned[slot] + 1) as usize]
                } else {
                    self.assignment_converters[slot]
                }
            })
            .count()
            .min(self.capacity);
        let coefficients = self.order_coefficients(std::array::from_fn(|slot| {
            if slot < from {
                self.ids[members[slot] * self.width + (assigned[slot] + 1) as usize][c]
            } else {
                self.assignment[c][slot]
            }
        }));
        let deduction = self.conversion_overlap(members, assigned, from);
        if deduction > 0.0 {
            Arc::new(
                coefficients
                    .iter()
                    .map(|&c| (c - deduction).next_up().max(0.0))
                    .collect(),
            )
        } else {
            coefficients
        }
    }
    fn conversion_overlap(&self, members: &[usize], assigned: &[i32], from: usize) -> f64 {
        let mut minimum = 0.0_f64;
        for (slot, &member) in members.iter().enumerate() {
            let extra = if slot < from {
                self.conversion_extras[member * self.width + (assigned[slot] + 1) as usize]
            } else {
                self.option_cache[member]
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|&snap| self.conversion_extras[member * self.width + (snap + 1) as usize])
                    .fold(f64::INFINITY, f64::min)
            };
            minimum = (minimum + extra).next_down().max(0.0);
        }
        let excess = (minimum - self.conversion_extra_cap).next_down().max(0.0);
        (excess / self.converted).next_down().max(0.0)
    }
    pub(crate) fn converter_partitions(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        base_power: f64,
        power_bound: f64,
    ) -> Option<Vec<ConverterPartition>> {
        if self.capacity == 0 || from == 5 {
            return None;
        }
        if self.converter_options.is_none() {
            let mut rows = [[[[0.0_f64; 5]; 5]; 2]; 6];
            let mut powers = [[f64::NEG_INFINITY; 2]; 5];
            let mut extras = [[f64::INFINITY; 2]; 5];
            for (slot, &member) in members.iter().enumerate() {
                for &snap in self.option_cache[member].as_ref().unwrap() {
                    let index = member * self.width + (snap + 1) as usize;
                    let category = self.converters[index] as usize;
                    let power = self.snapshot_power[member * self.width
                        + if snap < 0 {
                            self.width - 1
                        } else {
                            snap as usize
                        }];
                    powers[slot][category] = powers[slot][category].max(power);
                    extras[slot][category] =
                        extras[slot][category].min(self.conversion_extras[index]);
                    for (c, categories) in rows.iter_mut().enumerate() {
                        let weights = self.shared[self.ids[index][c] as usize];
                        for (event, value) in categories[category][slot].iter_mut().enumerate() {
                            *value = value.max(weights[event]);
                        }
                    }
                }
            }
            let rows =
                rows.map(|categories| categories.map(|slots| slots.map(|row| self.intern(row))));
            self.converter_options = Some(ConverterOptions {
                rows,
                power: powers,
                extras,
            });
        }
        let options = self.converter_options.as_ref().unwrap().clone();
        let fixed = (0..from)
            .filter(|&slot| {
                self.converters[members[slot] * self.width + (assigned[slot] + 1) as usize]
            })
            .count();
        let mut out = Vec::new();
        for mask in 0usize..1 << (5 - from) {
            let c = (fixed + mask.count_ones() as usize).min(self.capacity);
            let mut ids = [0; 5];
            let mut power = base_power;
            let mut minimum = 0.0_f64;
            let mut possible = true;
            for slot in 0..5 {
                let member = members[slot];
                let (p, extra) = if slot < from {
                    let snap = assigned[slot];
                    let index = member * self.width + (snap + 1) as usize;
                    ids[slot] = self.ids[index][c];
                    (
                        self.snapshot_power[member * self.width
                            + if snap < 0 {
                                self.width - 1
                            } else {
                                snap as usize
                            }],
                        self.conversion_extras[index],
                    )
                } else {
                    let category = (mask >> (slot - from)) & 1;
                    ids[slot] = options.rows[c][category][slot];
                    (
                        options.power[slot][category],
                        options.extras[slot][category],
                    )
                };
                if p == f64::NEG_INFINITY {
                    possible = false;
                    break;
                }
                power = add(power, p);
                minimum = (minimum + extra).next_down().max(0.0);
            }
            if !possible {
                continue;
            }
            let excess = (minimum - self.conversion_extra_cap).next_down().max(0.0);
            let deduction = (excess / self.converted).next_down().max(0.0);
            out.push(ConverterPartition {
                power: power.min(power_bound),
                ids,
                deduction,
            });
        }
        Some(out)
    }
    fn partition_coefficients(&mut self, part: &ConverterPartition) -> Arc<Vec<f64>> {
        let values = self.order_coefficients(part.ids);
        if part.deduction > 0.0 {
            Arc::new(
                values
                    .iter()
                    .map(|&v| (v - part.deduction).next_up().max(0.0))
                    .collect(),
            )
        } else {
            values
        }
    }
    pub(crate) fn partition_score(
        &mut self,
        part: &ConverterPartition,
        criterion: crate::objectives::Criterion,
    ) -> f64 {
        let coefficient = (self.score_coefficient(part.ids, criterion) - part.deduction)
            .next_up()
            .max(0.0);
        mul(part.power, coefficient)
    }
    pub(crate) fn snapshot_power(&mut self, power: Arc<Vec<f64>>) {
        self.snapshot_power = power;
    }
    pub(crate) fn just_ceiling(&self) -> usize {
        self.just_ceiling
    }
    pub(crate) fn dominance_margin(&self, power: f64) -> f64 {
        let error = add(mul(power, self.error_coefficient), self.absolute_error);
        if self.minimum_coefficient <= 0.0
            || mul(power, self.maximum_coefficient) + error >= i32::MAX as f64
        {
            return f64::INFINITY;
        }
        up(mul(2.0, error) / self.minimum_coefficient)
    }
    pub(crate) fn dominates_baseline_gain(&self, power_delta: f64, gain: f64) -> bool {
        gain >= mul(2.0, self.error_coefficient)
            && (power_delta * self.minimum_coefficient).next_down() > mul(2.0, self.absolute_error)
    }
    pub(crate) fn can_credit_baseline(&self, member: usize, snap: i32) -> bool {
        self.baseline_gains[member * self.width + (snap + 1) as usize]
            >= mul(2.0, self.error_coefficient)
    }
    pub(crate) fn product_floor(&self, score: f64) -> f64 {
        (score.max(0.0) * self.converted).next_down() * (1.0 - 16.0 * f64::EPSILON)
    }
    pub(crate) fn member_score_ceiling(
        &mut self,
        members: &[usize],
        from: usize,
        power: f64,
        criterion: crate::objectives::Criterion,
    ) -> f64 {
        let ids = std::array::from_fn(|slot| {
            if slot < members.len() {
                self.global[members[slot]][self.capacity]
            } else {
                self.suffix[from][self.capacity]
            }
        });
        let coefficient = self.score_coefficient(ids, criterion);
        let (mut actions, mut absolute) = members
            .iter()
            .map(|&m| self.normal_maxima[m])
            .fold((0usize, 0.0_f64), |(a, b), (c, d)| (a + c, add(b, d)));
        for _ in members.len()..5 {
            actions += self.normal_maximum.0;
            absolute = add(absolute, self.normal_maximum.1);
        }
        let coefficient = self.with_normal_error(
            coefficient,
            mul(
                mul(self.normal_error_unit, (2 * actions + 5) as f64),
                absolute,
            ),
        );
        mul(power, coefficient)
    }
    fn member_coefficients(&mut self, members: &[usize], from: usize) -> Arc<Vec<f64>> {
        self.order_coefficients(std::array::from_fn(|slot| {
            if slot < members.len() {
                self.global[members[slot]][self.capacity]
            } else {
                self.suffix[from][self.capacity]
            }
        }))
    }
    pub(crate) fn weight(&self, member: usize, snap: i32, mean: bool) -> f64 {
        let row = &self.profiles[member * self.width + (snap + 1) as usize];
        if mean {
            row.iter().copied().fold(0.0, add) / 5.0
        } else {
            row.iter().copied().fold(0.0, f64::max)
        }
    }
    pub(crate) fn score(&self, t: Totals) -> f64 {
        up(mul(t.power, add(self.base, t.skill)) / self.converted)
    }
}
#[derive(Clone, Copy)]
struct RushInfo {
    ids: [u32; 5],
    extra: f64,
    rows: usize,
}
type ProjectionKey = (i32, [u32; 5], [u32; 5]);
type SampledProjectionKey = ([u32; 5], [u32; 5], u64, usize);
struct OrderProjection {
    coefficients: Vec<f64>,
    mean: f64,
    rush: f64,
    general: f64,
}
type RewardContextKey = (bool, i32, [u32; 5]);
type SettlementKey = (Vec<usize>, Vec<i32>, i32, Option<(u64, u64)>);
struct RewardContext {
    simulation: Context,
    weights: OnceLock<crate::gekiso_objective::Weights>,
    judgement_mask: u8,
    score_projection: bool,
}
fn bonus_frontier(mut points: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    points.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then_with(|| b.1.total_cmp(&a.1)));
    let mut best = f64::NEG_INFINITY;
    points.retain(|&(_, value)| {
        if value <= best {
            false
        } else {
            best = value;
            true
        }
    });
    points
}
pub struct RewardObjective<'a> {
    exact_cache: HashMap<SettlementKey, (f64, serde_json::Value)>,
    exact_order: VecDeque<SettlementKey>,
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
    local: HashMap<[u32; 5], f64>,
    local_order: VecDeque<[u32; 5]>,
    local_last: Option<([u32; 5], f64)>,
    member_bounds: Vec<Option<BudgetShape>>,
    snap_bounds: Vec<Option<BudgetShape>>,
    bound_profiles: HashMap<Vec<u64>, u32>,
    bound_ids: Vec<u32>,
    stable_life: bool,
    canonical_slots: bool,
    deterministic: Option<DeterministicBound>,
    combo_models: HashMap<ComboModelKey, DeterministicBound>,
    combo_model_order: VecDeque<ComboModelKey>,
    assignment_combo: Option<ComboModelKey>,
    assignment_power: f64,
    assignment_primary: f64,
    power_estimate: f64,
    snapshot_power: Arc<Vec<f64>>,
    primary_snap_bonus: Arc<Vec<f64>>,
    reward_target: Option<f64>,
    rank_other: bool,
    other_target: Option<f64>,
    other_exchange: Option<(f64, f64)>,
    other_floor: f64,
    primary_bonus_ceiling: Option<f64>,
    other_bonus_ceiling: Option<f64>,
    snapshot_bonus_frontier: Vec<Vec<(f64, f64)>>,
    other_members: Vec<f64>,
    other_snaps: Vec<f64>,
    other_top: Vec<f64>,
    member_suffix: Vec<Vec<f64>>,
    other_snap_top: f64,
    bonus_max: f64,
    bonus_floors: HashMap<u64, Option<f64>>,
    targets: HashMap<u64, Vec<JointTarget>>,
    combo_bonuses: Option<Vec<i32>>,
    other_suffix: [f64; 6],
    other_ordered: Vec<i32>,
    contexts: HashMap<RewardContextKey, Arc<RewardContext>>,
    context_order: VecDeque<(RewardContextKey, usize)>,
    rush_aliases: HashMap<u32, (u32, f64, usize)>,
    projections: HashMap<ProjectionKey, Arc<OrderProjection>>,
    projection_order: VecDeque<ProjectionKey>,
    sampled_projections: HashMap<SampledProjectionKey, Arc<OrderProjection>>,
    sampled_projection_order: VecDeque<SampledProjectionKey>,
    sampled_projection_bytes: usize,
    live_shapes: HashMap<Vec<u64>, u32>,
    live_shape_ids: Vec<u32>,
    performer_ids: Vec<u32>,
    performer_index: HashMap<Performer, u32>,
    simple_actors: HashMap<u32, bool>,
    canonical_contexts: HashMap<[u32; 5], [u32; 5]>,
    canonical_context_order: VecDeque<[u32; 5]>,
    performers: Vec<Performer>,
    slots: HashMap<(usize, i32), Arc<live::Slot>>,
    snap_shapes: HashMap<Vec<u64>, usize>,
    allow_dominance: bool,
    context_bytes: usize,
    context_fields: crate::support_aliases::ContextFields,
    just_context: Option<JustContext>,
    just_bounds: crate::just_bound::Compiler,
    just_counters: crate::support_aliases::JustCounters,
    support_aliases: crate::support_aliases::SupportAliases,
    settlement_values: HashMap<(u64, bool), Vec<(i32, f64)>>,
    reward_curves: HashMap<(u64, bool), Option<crate::event::RewardCurve>>,
    conditioned_rewards: HashMap<(u64, u64, u64), f64>,
    conditioned_cuts: HashMap<(u64, u64, u64, u64, bool), bool>,
    member_dominance: HashMap<(usize, usize), bool>,
}
impl<'a> RewardObjective<'a> {
    pub(crate) fn reward_ceiling(&self) -> f64 {
        self.ceiling(f64::INFINITY, self.bonus_max, self.coefficient)
    }
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
                    .filter(|r| r.required >= 0)
                    .map(|r| r.rank),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let other_members: Vec<_> = members
            .iter()
            .map(|m| {
                crate::power::member_event_percent(&route.effects, m, if points { 1 } else { 0 })
                    as f64
            })
            .collect();
        let other_snaps: Vec<_> = snaps
            .iter()
            .map(|s| {
                crate::power::snap_event_percent(&route.effects, s, if points { 1 } else { 0 })
                    as f64
            })
            .collect();
        let mut other_top = other_members.clone();
        other_top.sort_by(|a, b| b.total_cmp(a));
        let mut snap_top = other_snaps.clone();
        snap_top.sort_by(|a, b| b.total_cmp(a));
        let other_snap_top = snap_top.iter().take(5).sum();
        let mut member_bonus: Vec<_> = members
            .iter()
            .map(|m| {
                crate::power::member_event_percent(&route.effects, m, if points { 0 } else { 1 })
                    as f64
            })
            .collect();
        let mut snap_bonus: Vec<_> = snaps
            .iter()
            .map(|s| {
                crate::power::snap_event_percent(&route.effects, s, if points { 0 } else { 1 })
                    as f64
            })
            .collect();
        member_bonus.sort_by(|a, b| b.total_cmp(a));
        snap_bonus.sort_by(|a, b| b.total_cmp(a));
        let bonus_max =
            member_bonus.iter().take(5).sum::<f64>() + snap_bonus.iter().take(5).sum::<f64>();
        let combo_bonuses = gekiso
            .setup
            .missions
            .iter()
            .any(|m| *m == 1 || *m == 4)
            .then(|| {
                crate::gekiso_objective::opening_combo_bonuses(
                    master,
                    members,
                    &gekiso.setup.missions,
                )
            });
        let member_bounds: Vec<_> = members
            .iter()
            .map(|member| member_budget(master, &gekiso, member))
            .collect();
        let snap_bounds: Vec<_> = snaps
            .iter()
            .map(|snap| snap_budget(master, &gekiso, snap))
            .collect();
        let life_changes = |skill: &GekisoSkill, level| {
            (skill.mission_type == 4 || gekiso.setup.missions.contains(&skill.mission_type))
                && skill
                    .effects
                    .iter()
                    .any(|row| row.level == level && (row.kind == 3000 || row.kind == 3002))
        };
        let stable_life = crate::support_aliases::stable_life_conditions(
            master,
            members,
            snaps,
            &gekiso.setup.missions,
        ) && !members.iter().any(|member| {
            master
                .gekiso_skills
                .get(&member.card.gekiso_skill_id)
                .is_some_and(|skill| life_changes(skill, member.growth.gekiso_skill_level))
        }) && !snaps.iter().any(|snap| {
            snap.card
                .gekiso_support_skills
                .iter()
                .enumerate()
                .any(|(i, id)| {
                    master
                        .gekiso_support_skills
                        .get(id)
                        .is_some_and(|skill| life_changes(skill, snap.gekiso_skill_levels[i]))
                })
        });
        let canonical_slots = stable_life
            && gekiso.accuracy.great == 0.0
            && gekiso.accuracy.miss_every == 0
            && members.iter().all(|member| {
                std::iter::once(None)
                    .chain(snaps.iter().map(Some))
                    .all(|snap| {
                        let slot = skills::resolve(master, member, snap, &mut Vec::new());
                        slot.effects.iter().all(|e| {
                            e.gate.blocks(master.live.life_base)
                                == e.gate.blocks(2.0 * master.live.life_base)
                        }) && slot
                            .convert
                            .as_ref()
                            .is_none_or(|c| !c.judgements.contains(&6))
                    })
            });
        let just_context = JustContext::new(master, &gekiso, members, snaps);
        let canonical_slots = canonical_slots
            && master
                .gekiso_skills
                .values()
                .chain(master.gekiso_support_skills.values())
                .all(|s| s.effects.iter().all(|r| !(r.kind == 12006 && r.value < 5)));
        let deterministic = if !gekiso.luck && stable_life {
            DeterministicBound::new(master, &gekiso, members, snaps)
        } else {
            None
        };
        let context_fields = crate::support_aliases::ContextFields::new(master);
        let primary_snap_bonus = Arc::new(
            snaps
                .iter()
                .map(|snap| {
                    crate::power::snap_event_percent(
                        &route.effects,
                        snap,
                        if points { 0 } else { 1 },
                    ) as f64
                })
                .collect(),
        );
        let support_aliases = crate::support_aliases::SupportAliases::new(
            &gekiso.setup.missions,
            snaps,
            canonical_slots,
        );
        let just_counters = crate::support_aliases::JustCounters::new(master, members);
        Self {
            exact_cache: HashMap::default(),
            exact_order: VecDeque::new(),
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
            local: HashMap::default(),
            local_order: VecDeque::new(),
            local_last: None,
            member_bounds,
            snap_bounds,
            stable_life,
            canonical_slots,
            deterministic,
            combo_models: HashMap::default(),
            combo_model_order: VecDeque::new(),
            assignment_combo: None,
            assignment_power: 0.0,
            assignment_primary: 0.0,
            power_estimate: 1.0,
            snapshot_power: Arc::new(Vec::new()),
            primary_snap_bonus,
            bound_profiles: HashMap::default(),
            bound_ids: vec![u32::MAX; members.len() * (snaps.len() + 1)],
            reward_target: None,
            rank_other: false,
            other_target: None,
            other_exchange: None,
            other_floor: 0.0,
            primary_bonus_ceiling: None,
            other_bonus_ceiling: None,
            snapshot_bonus_frontier: Vec::new(),
            other_members,
            other_snaps,
            other_top,
            member_suffix: Vec::new(),
            other_snap_top,
            bonus_max,
            bonus_floors: HashMap::default(),
            targets: HashMap::default(),
            combo_bonuses,
            other_suffix: [0.0; 6],
            other_ordered: Vec::new(),
            contexts: HashMap::default(),
            rush_aliases: HashMap::default(),
            projections: HashMap::default(),
            projection_order: VecDeque::new(),
            sampled_projections: HashMap::default(),
            sampled_projection_order: VecDeque::new(),
            sampled_projection_bytes: 0,
            live_shapes: HashMap::default(),
            live_shape_ids: vec![u32::MAX; members.len() * (snaps.len() + 1)],
            context_order: VecDeque::new(),
            performer_ids: vec![u32::MAX; members.len() * (snaps.len() + 1)],
            performer_index: HashMap::default(),
            simple_actors: HashMap::default(),
            canonical_contexts: HashMap::default(),
            canonical_context_order: VecDeque::new(),
            performers: Vec::new(),
            slots: HashMap::default(),
            snap_shapes: HashMap::default(),
            allow_dominance: true,
            context_bytes: 0,
            context_fields,
            just_context,
            just_bounds: crate::just_bound::Compiler::default(),
            just_counters,
            support_aliases,
            settlement_values: HashMap::default(),
            reward_curves: HashMap::default(),
            conditioned_rewards: HashMap::default(),
            conditioned_cuts: HashMap::default(),
            member_dominance: HashMap::default(),
        }
    }
    pub fn rank_other_at_reward(&mut self, target: f64, proven: bool) {
        self.reward_target = Some(target);
        self.rank_other = true;
        self.allow_dominance = proven;
        self.prepare_bonus_ceilings();
    }
    pub fn constrain_other(&mut self, target: f64, exchange: Option<(f64, f64)>, proven: bool) {
        self.other_target = Some(target);
        self.other_exchange = exchange;
        self.allow_dominance = proven;
        self.reward_curves.clear();
        self.settlement_values.clear();
        self.prepare_bonus_frontier(target);
        self.prepare_bonus_ceilings();
    }
    fn prepare_bonus_ceilings(&mut self) {
        let other_max = self.other_top.iter().take(5).sum::<f64>() + self.other_snap_top;
        let mut possible = self.ranks.clone();
        let observations = 120.0
            * if self.gekiso.luck {
                self.samples.max(1) as f64
            } else {
                1.0
            };
        for (target, secondary, maximum) in [
            (self.reward_target, false, self.bonus_max),
            (self.other_target, true, other_max),
        ] {
            let Some(target) = target else { continue };
            let values: Vec<_> = self
                .ranks
                .iter()
                .map(|&rank| {
                    if secondary {
                        self.other(maximum, rank)
                    } else {
                        self.value(maximum, rank)
                    }
                })
                .collect();
            let top = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            if target == top
                && values.iter().all(|&value| safe(value))
                && 16.0 * f64::EPSILON * observations * observations * top < 1.0
            {
                possible.retain(|rank| {
                    let index = self.ranks.iter().position(|r| r == rank).unwrap();
                    values[index] == top
                });
            }
        }
        let ceiling = |target: Option<f64>, secondary: bool, maximum: f64| {
            let target = target?;
            if possible.len() == self.ranks.len() || possible.is_empty() {
                return None;
            }
            let minimum = |bonus| {
                possible
                    .iter()
                    .map(|&rank| {
                        if secondary {
                            self.other(bonus, rank)
                        } else {
                            self.value(bonus, rank)
                        }
                    })
                    .fold(f64::INFINITY, f64::min)
            };
            let (mut low, mut high) = (0i64, maximum as i64 + 1);
            while low < high {
                let mid = (low + high) >> 1;
                if minimum(mid as f64) > target.next_up() {
                    high = mid;
                } else {
                    low = mid + 1;
                }
            }
            Some((low - 1) as f64)
        };
        let primary_bonus_ceiling = if self
            .members
            .iter()
            .enumerate()
            .all(|(m, _)| self.member_bonus(m) >= 0.0)
            && self.primary_snap_bonus.iter().all(|&bonus| bonus >= 0.0)
        {
            ceiling(self.reward_target, false, self.bonus_max)
        } else {
            None
        };
        let other_bonus_ceiling = if self
            .other_members
            .iter()
            .chain(&self.other_snaps)
            .all(|&bonus| bonus >= 0.0)
        {
            ceiling(self.other_target, true, other_max)
        } else {
            None
        };
        self.primary_bonus_ceiling = primary_bonus_ceiling;
        self.other_bonus_ceiling = other_bonus_ceiling;
    }
    fn bonus_ceilings_possible(&self, members: &[usize], assigned: &[i32]) -> bool {
        let primary = members.iter().map(|&m| self.member_bonus(m)).sum::<f64>()
            + assigned
                .iter()
                .filter(|&&s| s >= 0)
                .map(|&s| self.snap_bonus(s as usize))
                .sum::<f64>();
        let other = members.iter().map(|&m| self.other_members[m]).sum::<f64>()
            + assigned
                .iter()
                .filter(|&&s| s >= 0)
                .map(|&s| self.other_snaps[s as usize])
                .sum::<f64>();
        self.primary_bonus_ceiling
            .is_none_or(|ceiling| primary <= ceiling)
            && self
                .other_bonus_ceiling
                .is_none_or(|ceiling| other <= ceiling)
    }
    fn prepare_bonus_frontier(&mut self, target: f64) {
        let mut choices = vec![Vec::<(f64, f64)>::new(); 6];
        choices[0].push((0.0, 0.0));
        for snap in 0..self.snaps.len() {
            let pair = (self.other_snaps[snap], self.snap_bonus(snap));
            if pair == (0.0, 0.0) {
                continue;
            }
            for count in (1..=5).rev() {
                let extra: Vec<_> = choices[count - 1]
                    .iter()
                    .map(|&(a, b)| (a + pair.0, b + pair.1))
                    .collect();
                choices[count].extend(extra);
                choices[count] = bonus_frontier(std::mem::take(&mut choices[count]));
            }
        }
        let mut available = Vec::new();
        self.snapshot_bonus_frontier = choices
            .into_iter()
            .map(|points| {
                available.extend(points);
                bonus_frontier(available.clone())
            })
            .collect();
        let maximum = self.other_top.iter().take(5).sum::<f64>() + self.other_snap_top;
        let (mut low, mut high) = (0i64, maximum as i64);
        while low < high {
            let mid = (low + high) >> 1;
            let value = self
                .ranks
                .iter()
                .map(|&rank| self.other(mid as f64, rank))
                .fold(f64::NEG_INFINITY, f64::max);
            if value >= target {
                high = mid;
            } else {
                low = mid + 1;
            }
        }
        self.other_floor = low as f64;
    }
    fn bonus_pair_possible(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        limit: f64,
    ) -> bool {
        if !self.bonus_ceilings_possible(members, &assigned[..from]) {
            return false;
        }
        if self.other_target.is_none() || self.snapshot_bonus_frontier.is_empty() {
            return true;
        }
        let required = self.bonus_floor(limit).unwrap_or(f64::NEG_INFINITY)
            - members
                .iter()
                .map(|&member| self.member_bonus(member))
                .sum::<f64>()
            - assigned[..from]
                .iter()
                .filter(|&&s| s >= 0)
                .map(|&s| self.snap_bonus(s as usize))
                .sum::<f64>();
        let other = self.other_floor
            - members
                .iter()
                .map(|&member| self.other_members[member])
                .sum::<f64>()
            - assigned[..from]
                .iter()
                .filter(|&&s| s >= 0)
                .map(|&s| self.other_snaps[s as usize])
                .sum::<f64>();
        let frontier = &self.snapshot_bonus_frontier[5 - from];
        let count = frontier.partition_point(|&(bonus, _)| bonus >= other);
        count > 0 && frontier[count - 1].1 >= required
    }
    pub fn reward_mapper(&self) -> impl Fn(&mut crate::search::Hit) + 'static {
        let route = self.route.clone();
        let points = self.points;
        let exchange = self.other_exchange;
        let members = self.other_members.clone();
        let snaps = self.other_snaps.clone();
        move |hit| {
            let bonus = hit.team.members.iter().map(|&m| members[m]).sum::<f64>()
                + hit
                    .team
                    .snaps
                    .iter()
                    .filter(|&&s| s >= 0)
                    .map(|&s| snaps[s as usize])
                    .sum::<f64>();
            let settlement = &mut hit.detail["settlement"];
            let count = crate::master::number(&settlement["observations"], 1.0);
            let secondary = crate::master::number(&settlement["rewardSum"], 0.0);
            let mut reward = 0.0;
            let mut minimum = f64::INFINITY;
            let mut maximum = f64::NEG_INFINITY;
            for (rank, count) in settlement["ranks"].as_object().unwrap() {
                let rank: i32 = rank.parse().unwrap();
                let value = if points {
                    route.items(bonus, rank)
                } else {
                    route.points(bonus, rank)
                };
                reward += value * count.as_f64().unwrap();
                minimum = minimum.min(value);
                maximum = maximum.max(value);
            }
            let comparison = exchange.map_or(reward, |(n, d)| {
                reward * d + crate::master::number(&settlement["cpSum"], 0.0) * n
            });
            settlement["rewardSum"] = json!(reward);
            settlement["comparisonSum"] = json!(comparison);
            settlement["mean"] = json!(comparison / count);
            settlement["otherMean"] = json!(secondary / count);
            settlement["directMin"] = json!(minimum);
            settlement["directMax"] = json!(maximum);
            hit.detail["bonus"] = json!(bonus);
        }
    }
    pub fn rank_score_at_reward(&mut self, target: f64, other: f64, proven: bool) {
        self.reward_target = Some(target);
        self.rank_other = false;
        self.other_target = Some(other);
        self.allow_dominance = proven;
        self.prepare_bonus_frontier(other);
        self.prepare_bonus_ceilings();
    }
    fn other(&self, bonus: f64, rank: i32) -> f64 {
        let value = if self.points {
            self.route.items(bonus, rank)
        } else {
            self.route.points(bonus, rank)
        };
        self.other_exchange.map_or(value, |(n, d)| {
            value * d
                + *self.route.challenge_points.get(&rank).unwrap_or(&0) as f64 * self.route.rate * n
        })
    }
    fn other_bonus(&self, team: &Team) -> f64 {
        team.members
            .iter()
            .map(|&i| self.other_members[i])
            .sum::<f64>()
            + team
                .snaps
                .iter()
                .filter(|&&j| j >= 0)
                .map(|&j| self.other_snaps[j as usize])
                .sum::<f64>()
    }
    fn other_ceiling(&self, power: f64, bonus: f64, coefficient: f64) -> f64 {
        let upper = score_bound(coefficient, power);
        self.ranks
            .iter()
            .filter(|&&rank| {
                rank == 2
                    || self
                        .chart
                        .ranks
                        .iter()
                        .any(|r| r.rank == rank && r.required >= 0 && r.required as f64 <= upper)
            })
            .map(|&rank| self.other(bonus, rank))
            .fold(f64::NEG_INFINITY, f64::max)
    }
    fn ranked_bound(&self, power: f64, bonus: f64, coefficient: f64, other: f64) -> f64 {
        let reward = self.ceiling(power, bonus, coefficient);
        if self
            .other_target
            .is_some_and(|target| self.other_ceiling(power, other, coefficient) < target)
        {
            return -1.0;
        }
        match self.reward_target {
            Some(target) if reward < target => -1.0,
            Some(_) => {
                let upper = self.other_ceiling(power, other, coefficient);
                if self.other_target.is_some_and(|target| upper < target) {
                    -1.0
                } else if self.rank_other {
                    upper
                } else {
                    score_bound(coefficient, power)
                }
            }
            None => reward,
        }
    }
    fn assignment_model(&mut self) -> Option<&mut DeterministicBound> {
        if let Some(combo) = &self.assignment_combo {
            self.combo_models.get_mut(combo)
        } else {
            self.deterministic.as_mut()
        }
    }
    fn projected_bound(&mut self, t: Totals, other: f64, coefficients: &[f64]) -> f64 {
        let mut values = [f64::INFINITY; 2];
        for (i, &bonus) in [t.bonus, other].iter().enumerate() {
            let secondary = i == 1;
            let key = (bonus.to_bits(), secondary);
            if !self.reward_curves.contains_key(&key) {
                let rows = self
                    .ranks
                    .iter()
                    .map(|&rank| {
                        (
                            if rank == 2 {
                                0
                            } else {
                                self.chart
                                    .ranks
                                    .iter()
                                    .find(|r| r.rank == rank)
                                    .map_or(0, |r| r.required)
                            },
                            if secondary {
                                self.other(bonus, rank)
                            } else {
                                self.value(bonus, rank)
                            },
                        )
                    })
                    .collect();
                let curve = crate::event::RewardCurve::new(rows);
                if self.reward_curves.len() >= 256 {
                    self.reward_curves.clear();
                }
                self.reward_curves.insert(key, curve);
            }
            values[i] = self.reward_curves[&key]
                .as_ref()
                .map_or(f64::INFINITY, |curve| curve.orders(coefficients, t.power));
        }
        if self.other_target.is_some_and(|target| values[1] < target) {
            return -1.0;
        }
        match self.reward_target {
            Some(target)
                if values[0] < target
                    || self.other_target.is_some_and(|target| values[1] < target) =>
            {
                -1.0
            }
            Some(_) if self.rank_other => values[1],
            Some(_) => up(mul(coefficients.iter().copied().fold(0.0, add), t.power) / 120.0),
            None => values[0],
        }
    }
    fn total_bound(&self, t: Totals, other: f64) -> f64 {
        if let Some(projection) = &self.deterministic {
            let score = projection.score(t);
            if self.reward_target.is_some() && !self.rank_other {
                return score;
            }

            let reward = self
                .ranks
                .iter()
                .filter(|&&rank| {
                    rank == 2
                        || self.chart.ranks.iter().any(|r| {
                            r.rank == rank && r.required >= 0 && r.required as f64 <= score
                        })
                })
                .map(|&r| self.value(t.bonus, r))
                .fold(f64::NEG_INFINITY, f64::max);
            let upper = self
                .ranks
                .iter()
                .filter(|&&rank| {
                    rank == 2
                        || self.chart.ranks.iter().any(|r| {
                            r.rank == rank && r.required >= 0 && r.required as f64 <= score
                        })
                })
                .map(|&r| self.other(other, r))
                .fold(f64::NEG_INFINITY, f64::max);
            if self.other_target.is_some_and(|target| upper < target) {
                return -1.0;
            }
            return match self.reward_target {
                Some(target)
                    if reward < target
                        || self.other_target.is_some_and(|target| upper < target) =>
                {
                    -1.0
                }
                Some(_) if self.rank_other => upper,
                Some(_) => score,
                None => reward,
            };
        }
        self.ranked_bound(t.power, t.bonus, self.coefficient, other)
    }
    fn ranked(&self, mut exact: Exact) -> Exact {
        if let Some(target) = self.reward_target {
            let other = crate::master::number(&exact.detail["settlement"]["otherMean"], 0.0);
            exact.key =
                if exact.key == target && self.other_target.is_none_or(|target| other == target) {
                    if self.rank_other {
                        other
                    } else {
                        crate::master::number(&exact.detail["scores"]["mean"], 0.0)
                    }
                } else {
                    -1.0
                };
        } else if self.other_target.is_some_and(|target| {
            crate::master::number(&exact.detail["settlement"]["otherMean"], 0.0) != target
        }) {
            exact.key = -1.0;
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
    fn bound_profile(&mut self, member: usize, snap: i32) -> Option<u32> {
        let index = member * (self.snaps.len() + 1) + (snap + 1) as usize;
        if self.bound_ids[index] != u32::MAX {
            return (self.bound_ids[index] != u32::MAX - 1).then_some(self.bound_ids[index]);
        }
        let make = |this: &mut Self| {
            let member_budget = this.member_bounds[member]?;
            let snap_budget = if snap < 0 {
                BudgetShape::default()
            } else {
                this.snap_bounds[snap as usize]?
            };
            let slot = this.slot(member, snap);
            let mut key = vec![
                member_budget.present as u64,
                member_budget.budget.positive.to_bits(),
                member_budget.budget.commands as u64,
                snap_budget.present as u64,
                snap_budget.budget.positive.to_bits(),
                snap_budget.budget.commands as u64,
                (member_budget.raises || snap_budget.raises) as u64,
            ];
            for effect in &slot.effects {
                key.extend([
                    f(effect.delta as f64 / 100000.0).to_bits(),
                    window_ms(effect.seconds, slot.extension_ms) as u64,
                    if effect.kind == 2000 {
                        1
                    } else {
                        effect.judgements.len() as u64
                    },
                ]);
            }
            let next = this.bound_profiles.len() as u32;
            Some(*this.bound_profiles.entry(key).or_insert(next))
        };
        let id = make(self);
        self.bound_ids[index] = id.unwrap_or(u32::MAX - 1);
        id
    }
    fn team_coefficient(&mut self, team: &Team) -> f64 {
        if self.just_context.is_some() {
            return self.coefficient;
        }
        let mut key = [0; 5];
        for (i, value) in key.iter_mut().enumerate() {
            let Some(id) = self.bound_profile(team.members[i], team.snaps[i]) else {
                return f64::INFINITY;
            };
            *value = id;
        }
        key.sort_unstable();
        if let Some((previous, value)) = self.local_last {
            if key == previous {
                return value;
            }
        }
        if let Some(&value) = self.local.get(&key) {
            self.local_last = Some((key, value));
            return value;
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
        self.local.insert(key, bound);
        self.local_order.push_back(key);
        self.local_last = Some((key, bound));
        bound
    }
    fn ceiling(&self, power: f64, bonus: f64, coefficient: f64) -> f64 {
        let upper = score_bound(coefficient, power);
        self.ranks
            .iter()
            .filter(|&&rank| {
                rank == 2
                    || self
                        .chart
                        .ranks
                        .iter()
                        .any(|r| r.rank == rank && r.required >= 0 && r.required as f64 <= upper)
            })
            .map(|&rank| self.value(bonus, rank))
            .fold(f64::NEG_INFINITY, f64::max)
    }
    fn slot(&mut self, member: usize, snap: i32) -> Arc<live::Slot> {
        Arc::clone(self.slots.entry((member, snap)).or_insert_with(|| {
            let mut slot = skills::resolve(
                self.master,
                &self.members[member],
                (snap >= 0).then(|| &self.snaps[snap as usize]),
                &mut Vec::new(),
            );
            if self.canonical_slots {
                slot.recovery = 0;
                slot.convert = None;
                slot.effects
                    .retain(|e| !e.gate.blocks(self.master.live.life_base));
                for effect in &mut slot.effects {
                    effect.gate = live::Gate {
                        kind: 0,
                        value: 0.0,
                        positive: true,
                    };
                }
                if slot.effects.is_empty() {
                    slot.extension_ms = 0.0;
                }
            }
            Arc::new(slot)
        }))
    }
    fn performer_id(&mut self, member: usize, snap: i32) -> u32 {
        let index = member * (self.snaps.len() + 1) + (snap + 1) as usize;
        if self.performer_ids[index] != u32::MAX {
            return self.performer_ids[index];
        }
        let original = Performer::from_member(
            self.master,
            &self.members[member],
            (snap >= 0).then(|| &self.snaps[snap as usize]),
        );
        let runs = |mission: i32| mission == 4 || self.gekiso.setup.missions.contains(&mission);
        let mut p = original.gekiso_only();
        if let Some((id, _)) = p.gekisou_skill {
            let mission = self.master.gekiso_skills[&id].mission_type;
            if !runs(mission) {
                p.gekisou_skill = None;
                p.mission_only = Some(mission);
            }
        }
        p.gekisou_support_skills
            .retain(|(id, _)| runs(self.master.gekiso_support_skills[id].mission_type));
        self.just_counters.normalize(&mut p);
        if let Some(just) = &self.just_context {
            just.normalize(&mut p, self.master);
        }
        self.support_aliases.apply(self.master, &mut p);
        self.context_fields.normalize(self.master, &mut p);
        let next = self.performers.len() as u32;
        let id = *self.performer_index.entry(p.clone()).or_insert_with(|| {
            self.performers.push(p);
            next
        });
        self.performer_ids[index] = id;
        id
    }
    fn context(&mut self, team: &Team, seed: i32) -> Result<Arc<RewardContext>, String> {
        let ids = std::array::from_fn(|i| self.performer_id(team.members[i], team.snaps[i]));
        self.context_ids(ids, seed)
    }
    fn simple_actor(&mut self, id: u32) -> bool {
        if let Some(&simple) = self.simple_actors.get(&id) {
            return simple;
        }
        let p = &self.performers[id as usize];
        let simple = p.gekisou_support_skills.is_empty()
            && p.gekisou_skill.is_none_or(|(id, level)| {
                let skill = &self.master.gekiso_skills[&id];
                skill.mission_type == 1
                    && skill
                        .effects
                        .iter()
                        .filter(|r| r.level == level)
                        .all(|row| {
                            row.kind == 12000
                                && row.value >= 0
                                && row.cumulative_id == 0
                                && row.reset_group == 0
                                && [row.trigger_group, row.condition_group, row.release_group]
                                    .iter()
                                    .all(|group| {
                                        self.master
                                            .condition_sets
                                            .get(group)
                                            .into_iter()
                                            .flatten()
                                            .flatten()
                                            .filter_map(|id| self.master.conditions.get(id))
                                            .all(|c| match c.kind {
                                                7005 => c.positive,
                                                7010 | 7013 | 7020 | 5000 => true,
                                                2000 | 2001 => {
                                                    c.values.first().is_some_and(|&value| {
                                                        value < self.master.live.life_base
                                                            || value
                                                                > 2.0 * self.master.live.life_base
                                                    })
                                                }
                                                _ => false,
                                            })
                                    })
                        })
            });
        self.simple_actors.insert(id, simple);
        simple
    }
    fn canonical_context_ids(&mut self, ids: [u32; 5]) -> [u32; 5] {
        if !self.canonical_slots || !self.context_fields.owner_local() {
            return ids;
        }
        if let Some(&canonical) = self.canonical_contexts.get(&ids) {
            return canonical;
        }
        let mut canonical = [0; 5];
        let mut others = [0; 5];
        let (mut simple, mut rest) = (0, 0);
        for id in ids {
            if self.simple_actor(id) {
                canonical[simple] = id;
                simple += 1;
            } else {
                others[rest] = id;
                rest += 1;
            }
        }
        canonical[..simple].sort_unstable();
        canonical[simple..].copy_from_slice(&others[..rest]);
        if self.canonical_contexts.len() >= 8192 {
            if let Some(old) = self.canonical_context_order.pop_front() {
                self.canonical_contexts.remove(&old);
            }
        }
        self.canonical_contexts.insert(ids, canonical);
        self.canonical_context_order.push_back(ids);
        canonical
    }
    fn context_ids(&mut self, ids: [u32; 5], seed: i32) -> Result<Arc<RewardContext>, String> {
        let ids = self.canonical_context_ids(ids);
        let key = (false, seed, ids);
        if let Some(context) = self.contexts.get(&key) {
            return Ok(Arc::clone(context));
        }
        let deck: Vec<_> = ids
            .iter()
            .map(|&i| self.performers[i as usize].clone())
            .collect();
        let simulation = gekiso::context_fixed(self.master, &self.gekiso, &deck, &[seed])?;
        Ok(self.cache_context(key, simulation))
    }
    fn cache_context(&mut self, key: RewardContextKey, simulation: Context) -> Arc<RewardContext> {
        let bytes = simulation.retained_bytes() + (self.gekiso.notes.len() + 1) * 32 + 256;
        let judgement_mask = simulation
            .judgement
            .iter()
            .fold(0u8, |mask, &j| mask | (1 << j));
        let score_projection = self.stable_life
            && simulation
                .judgement
                .iter()
                .all(|&j| (4..=6).contains(&j) && self.master.live.damage[j] == 0.0);
        let context = Arc::new(RewardContext {
            simulation,
            weights: OnceLock::new(),
            judgement_mask,
            score_projection,
        });
        const CAPACITY: usize = 16 * 1024 * 1024;
        if bytes <= CAPACITY {
            while self.context_bytes + bytes > CAPACITY {
                let Some((old, bytes)) = self.context_order.pop_front() else {
                    break;
                };
                self.contexts.remove(&old);
                self.context_bytes -= bytes;
            }
            self.context_bytes += bytes;
            self.context_order.push_back((key, bytes));
            self.contexts.insert(key, Arc::clone(&context));
        }
        context
    }
    fn rush_info(&mut self, team: &Team) -> Option<RushInfo> {
        if !self.stable_life
            || !self.context_fields.owner_local()
            || self.master.live.gekiso.rush_percent <= 0
        {
            return None;
        }
        let ids: [u32; 5] =
            std::array::from_fn(|i| self.performer_id(team.members[i], team.snaps[i]));
        let mut stripped = ids;
        let mut extra = 0.0;
        let mut row_count = 0;
        for (slot, &id) in ids.iter().enumerate() {
            let (alias, value, count) = if let Some(&cached) = self.rush_aliases.get(&id) {
                cached
            } else {
                let mut performer = self.performers[id as usize].clone();
                let mut value = 0.0;
                let mut count = 0;
                performer
                    .gekisou_support_skills
                    .retain(|&(skill_id, level)| {
                        let skill = &self.master.gekiso_support_skills[&skill_id];
                        let rows: Vec<_> =
                            skill.effects.iter().filter(|r| r.level == level).collect();
                        let eligible = [2, 4].contains(&skill.mission_type)
                            && !rows.is_empty()
                            && rows.iter().all(|row| {
                                row.kind == 2000
                                    && row.value >= 0
                                    && row.trigger_type == 2
                                    && row.seconds == 0.0
                                    && row.release_group == 0
                                    && row.reset_group == 0
                                    && row.targets.is_empty()
                                    && self
                                        .master
                                        .condition_sets
                                        .get(&row.trigger_group)
                                        .is_some_and(|sets| {
                                            sets.len() == 1
                                                && sets[0].len() == 1
                                                && self
                                                    .master
                                                    .conditions
                                                    .get(&sets[0][0])
                                                    .is_some_and(|c| c.kind == 7021 && c.positive)
                                        })
                                    && static_gk_group(
                                        self.master,
                                        row.condition_group,
                                        &self.performers[id as usize],
                                    )
                                    .is_some()
                            });
                        if eligible {
                            for row in rows {
                                if static_gk_group(
                                    self.master,
                                    row.condition_group,
                                    &self.performers[id as usize],
                                ) == Some(true)
                                {
                                    value = add(
                                        value,
                                        up(f(f(
                                            note_factor_mill(f(f(row.value as f64) / 10000.0))
                                                as f64,
                                        ) / 100000.0)),
                                    );
                                    count += 1;
                                }
                            }
                        }
                        !eligible
                    });
                self.context_fields.normalize(self.master, &mut performer);
                let next = self.performers.len() as u32;
                let alias = *self
                    .performer_index
                    .entry(performer.clone())
                    .or_insert_with(|| {
                        self.performers.push(performer);
                        next
                    });
                self.rush_aliases.insert(id, (alias, value, count));
                (alias, value, count)
            };
            stripped[slot] = alias;
            extra = add(extra, value);
            row_count += count;
        }
        Some(RushInfo {
            ids: self.canonical_context_ids(stripped),
            extra: if extra == 0.0 {
                0.0
            } else {
                (extra as f32).next_up() as f64
            },
            rows: row_count,
        })
    }
    fn live_projection_shapes(&mut self, team: &Team) -> [u32; 5] {
        let mut shapes = [0; 5];
        for (slot, &member) in team.members.iter().enumerate() {
            let snap = team.snaps[slot];
            let index = member * (self.snaps.len() + 1) + (snap + 1) as usize;
            if self.live_shape_ids[index] == u32::MAX {
                let signature = self.slot(member, snap).signature();
                let next = self.live_shapes.len() as u32;
                self.live_shape_ids[index] = *self.live_shapes.entry(signature).or_insert(next);
            }
            shapes[slot] = self.live_shape_ids[index];
        }
        shapes.sort_unstable();
        shapes
    }
    fn record_sampled_projection(
        &mut self,
        key: SampledProjectionKey,
        previous: Option<&OrderProjection>,
        mut coefficients: Vec<f64>,
    ) {
        if coefficients.is_empty() {
            return;
        }
        if let Some(previous) = previous {
            coefficients.extend_from_slice(&previous.coefficients);
        }
        coefficients.sort_unstable_by(f64::total_cmp);
        let projection = Arc::new(OrderProjection {
            mean: up(coefficients.iter().copied().fold(0.0, add) / coefficients.len() as f64),
            coefficients,
            rush: 0.0,
            general: 0.0,
        });
        if let Some(old) = self.sampled_projections.remove(&key) {
            self.sampled_projection_bytes -= old.coefficients.len() * std::mem::size_of::<f64>();
            self.sampled_projection_order.retain(|old| old != &key);
        }
        let bytes = projection.coefficients.len() * std::mem::size_of::<f64>();
        while self.sampled_projection_bytes + bytes > 8 * 1024 * 1024 {
            let Some(old) = self.sampled_projection_order.pop_front() else {
                break;
            };
            if let Some(old) = self.sampled_projections.remove(&old) {
                self.sampled_projection_bytes -=
                    old.coefficients.len() * std::mem::size_of::<f64>();
            }
        }
        self.sampled_projection_bytes += bytes;
        self.sampled_projections.insert(key, projection);
        self.sampled_projection_order.push_back(key);
    }
    fn projected_orders(
        &mut self,
        team: &Team,
        info: RushInfo,
        seed: i32,
    ) -> Result<Option<(Arc<OrderProjection>, f64)>, String> {
        let core = self.context_ids(info.ids, seed)?;

        if !core.score_projection {
            return Ok(None);
        }
        let Some(ranges) = deterministic_ranges(self.master, &self.gekiso) else {
            return Ok(None);
        };
        let commands = 2 * (core.simulation.luck_draws + 6) * info.rows;
        let n = (factor_operations(&self.gekiso, &ranges)
            * (core.simulation.factor_commands + commands)) as f64
            * 2.0_f64.powi(-24);
        if n >= 0.25 {
            return Ok(None);
        }
        let positive = add(core.simulation.factor_peak.max(1.0), info.extra);
        let error = if info.rows == 0 {
            0.0
        } else {
            mul(
                4.0,
                mul(mul(n, add(1.0, mul(2.0, positive))), up(1.0 / (1.0 - n))),
            )
        };
        let shapes = self.live_projection_shapes(team);
        let key = (seed, info.ids, shapes);
        let projection = if let Some(projection) = self.projections.get(&key) {
            Arc::clone(projection)
        } else {
            let slots: [Arc<live::Slot>; 5] =
                std::array::from_fn(|slot| self.slot(team.members[slot], team.snaps[slot]));
            let Some(scores) = self.order_ceilings(&core, &slots, 1) else {
                return Ok(None);
            };
            let absolute = (self.gekiso.notes.len() + 4) as f64;
            let mut coefficients: Vec<_> = scores
                .into_iter()
                .map(|score| up((score - absolute).max(0.0)))
                .collect();
            coefficients.sort_by(f64::total_cmp);
            let weights = core.weights.get().unwrap();
            let rush = (0..self.gekiso.notes.len())
                .filter(|&i| core.simulation.luck[i] > 1.0)
                .map(|i| up(weights.general[i + 1] - weights.general[i]))
                .fold(0.0, add);
            let projection = Arc::new(OrderProjection {
                mean: up(coefficients.iter().copied().fold(0.0, add) / 120.0),
                coefficients,
                rush,
                general: *weights.general.last().unwrap(),
            });
            if self.projections.len() >= 8192 {
                if let Some(old) = self.projection_order.pop_front() {
                    self.projections.remove(&old);
                }
            }
            self.projections.insert(key, Arc::clone(&projection));
            self.projection_order.push_back(key);
            projection
        };
        let rho = up(1.0 / (1.0 - 2.0_f64.powi(-24)));
        let mut rounding = 1.0;
        for _ in 0..12 {
            rounding = mul(rounding, rho);
        }
        let offset = up(mul(
            add(
                mul(info.extra, projection.rush),
                mul(error, projection.general),
            ),
            rounding,
        ) / self.gekiso.full.converted as f64);
        Ok(Some((projection, offset)))
    }
    fn projected_coefficient_sum(
        &mut self,
        bonus: f64,
        secondary: bool,
        projection: &OrderProjection,
        power: f64,
        offset: f64,
    ) -> f64 {
        let key = (bonus.to_bits(), secondary);
        if !self.reward_curves.contains_key(&key) {
            let rows = self
                .ranks
                .iter()
                .map(|&rank| {
                    (
                        if rank == 2 {
                            0
                        } else {
                            self.chart
                                .ranks
                                .iter()
                                .find(|r| r.rank == rank)
                                .map_or(0, |r| r.required)
                        },
                        if secondary {
                            self.other(bonus, rank)
                        } else {
                            self.value(bonus, rank)
                        },
                    )
                })
                .collect();
            if self.reward_curves.len() >= 256 {
                self.reward_curves.clear();
            }
            self.reward_curves
                .insert(key, crate::event::RewardCurve::new(rows));
        }
        self.reward_curves[&key]
            .as_ref()
            .map_or(f64::INFINITY, |curve| {
                curve.shifted_sum(
                    &projection.coefficients,
                    power,
                    offset,
                    (self.gekiso.notes.len() + 4) as f64,
                )
            })
    }
    fn order_ceilings(
        &self,
        context: &RewardContext,
        slots: &[Arc<live::Slot>],
        power: i32,
    ) -> Option<Vec<f64>> {
        let ctx = &context.simulation;
        if !context.score_projection
            || slots.iter().any(|s| {
                s.convert.as_ref().is_some_and(|c| {
                    c.judgements
                        .iter()
                        .any(|&j| j != 5 && context.judgement_mask & (1 << j) != 0)
                }) || s.effects.iter().any(|e| e.delta < 0)
            })
        {
            return None;
        }
        let weights = context
            .weights
            .get_or_init(|| crate::gekiso_objective::note_weights(self.master, &self.gekiso, ctx));
        order_score_coefficients(self.master, &self.gekiso, weights, slots).map(|coefficients| {
            coefficients
                .into_iter()
                .map(|c| add(mul(power as f64, c), (self.gekiso.notes.len() + 4) as f64))
                .collect()
        })
    }
    fn projected_settlement(&mut self, bonus: f64, other: bool, scores: &[f64]) -> f64 {
        self.projected_settlement_sum(bonus, other, scores) / scores.len() as f64
    }
    fn projected_settlement_sum(&mut self, bonus: f64, other: bool, scores: &[f64]) -> f64 {
        let key = (bonus.to_bits(), other);
        if !self.settlement_values.contains_key(&key) {
            let values = self
                .ranks
                .iter()
                .map(|&rank| {
                    (
                        rank,
                        if other {
                            self.other(bonus, rank)
                        } else {
                            self.value(bonus, rank)
                        },
                    )
                })
                .collect();
            self.settlement_values.insert(key, values);
        }
        let values = &self.settlement_values[&key];
        scores
            .iter()
            .map(|&score| {
                let rank = chart::score_rank(self.chart, score);
                values
                    .iter()
                    .find(|&&(r, _)| r == rank)
                    .map_or(f64::INFINITY, |r| r.1)
            })
            .sum::<f64>()
    }
    fn conditioned_reward_cut(&mut self, bonus: f64, other: f64, cutoff: Cutoff) -> bool {
        let Some(target) = self.other_target.filter(|_| self.reward_target.is_none()) else {
            return false;
        };
        let key = (
            bonus.to_bits(),
            other.to_bits(),
            target.to_bits(),
            cutoff.key.to_bits(),
            cutoff.prune_equal,
        );
        if let Some(&value) = self.conditioned_cuts.get(&key) {
            return value;
        }
        let count = 120
            * if self.gekiso.luck {
                self.samples.max(1) as i128
            } else {
                1
            };
        let numerator = |mean: f64| {
            let value = mean * count as f64;
            let error = 8.0 * f64::EPSILON * value.abs();
            (value.is_finite() && error < 0.25 && (value - value.round()).abs() <= error)
                .then_some(value.round() as i128)
        };
        let (Some(target), Some(mut limit)) = (numerator(target), numerator(cutoff.key)) else {
            return false;
        };
        if !cutoff.prune_equal {
            limit -= 1;
        }
        let mut lower: Option<(i128, i128)> = None;
        let mut upper: Option<(i128, i128)> = None;
        let mut possible = true;
        for &rank in &self.ranks {
            let (x, y) = (self.other(other, rank), self.value(bonus, rank));
            if !safe(x) || !safe(y) || x < 0.0 || y < 0.0 {
                return false;
            }
            let (x, y) = (x as i128 * count - target, y as i128 * count - limit);
            if x.abs() > 1i128 << 60 || y.abs() > 1i128 << 60 {
                return false;
            }
            if x == 0 {
                possible &= y <= 0;
            } else if x > 0 {
                if lower.is_none_or(|(a, b)| y * b > a * x) {
                    lower = Some((y, x));
                }
            } else if upper.is_none_or(|(a, b)| -y * b < a * -x) {
                upper = Some((-y, -x));
            }
        }
        possible &= match (lower, upper) {
            (Some((a, b)), Some((c, d))) => a * d <= c * b,
            _ => true,
        };
        if self.conditioned_cuts.len() >= 4096 {
            self.conditioned_cuts.clear();
        }
        self.conditioned_cuts.insert(key, possible);
        possible
    }
    fn conditioned_reward_upper(&mut self, bonus: f64, other: f64) -> f64 {
        let Some(target) = self.other_target else {
            return f64::INFINITY;
        };
        let key = (bonus.to_bits(), other.to_bits(), target.to_bits());
        if let Some(&value) = self.conditioned_rewards.get(&key) {
            return value;
        }
        let rows: Vec<_> = self
            .ranks
            .iter()
            .map(|&rank| (self.other(other, rank), self.value(bonus, rank)))
            .collect();
        if rows.iter().any(|&(x, y)| {
            !x.is_finite()
                || !y.is_finite()
                || !(0.0..=MAX_SAFE).contains(&x)
                || !(0.0..=MAX_SAFE).contains(&y)
        }) {
            return f64::INFINITY;
        }
        let samples = if self.gekiso.luck {
            self.samples.max(1) as f64
        } else {
            1.0
        };
        let accumulation = (120.0 * samples + 4.0) * f64::EPSILON;
        if accumulation >= 0.25 {
            return f64::INFINITY;
        }
        let error = mul(
            rows.iter().map(|r| r.0).fold(0.0, f64::max),
            up(2.0 * accumulation / (1.0 - accumulation)),
        );
        let (low, high) = ((target - error).next_down(), (target + error).next_up());
        let mut upper = -1.0_f64;
        for &(x, y) in &rows {
            if low <= x && x <= high {
                upper = upper.max(y);
            }
            for &(end, value) in &rows {
                if end <= x || end < low || x > high {
                    continue;
                }
                let width = (end - x).next_down();
                if width <= 0.0 {
                    return f64::INFINITY;
                }
                let a = up((end - low.max(x)).next_up() / width).min(1.0);
                let b = up((high.min(end) - x).next_up() / width).min(1.0);
                upper = upper.max(add(mul(a, y), mul(b, value)));
            }
        }
        if upper >= 0.0 {
            upper = mul(upper, up(1.0 + 2.0 * accumulation / (1.0 - accumulation)));
        }
        if self.conditioned_rewards.len() >= 4096 {
            self.conditioned_rewards.clear();
        }
        self.conditioned_rewards.insert(key, upper);
        upper
    }
    fn settle_cached(
        &mut self,
        team: &Team,
        power: i32,
        cutoff: Option<Cutoff>,
        score_cutoff: Option<Cutoff>,
    ) -> Result<Option<Exact>, String> {
        let key = (
            team.members.clone(),
            team.snaps.clone(),
            power,
            self.other_exchange.map(|(n, d)| (n.to_bits(), d.to_bits())),
        );
        if let Some((key, detail)) = self.exact_cache.get(&key) {
            return Ok(Some(Exact {
                key: *key,
                detail: detail.clone(),
            }));
        }
        let result = self.settle(team, power, cutoff, score_cutoff)?;
        if let Some(exact) = &result {
            if self.exact_cache.len() >= 16 {
                if let Some(old) = self.exact_order.pop_front() {
                    self.exact_cache.remove(&old);
                }
            }
            self.exact_cache
                .insert(key.clone(), (exact.key, exact.detail.clone()));
            self.exact_order.push_back(key);
        }
        Ok(result)
    }
    fn settle(
        &mut self,
        team: &Team,
        power: i32,
        cutoff: Option<Cutoff>,
        score_cutoff: Option<Cutoff>,
    ) -> Result<Option<Exact>, String> {
        if self.route.kind != RouteKind::Live {
            return Err("gekiso-reward-route".into());
        }
        let slots: [Arc<live::Slot>; 5] =
            std::array::from_fn(|i| self.slot(team.members[i], team.snaps[i]));
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
        if cutoff.is_some_and(|cutoff| self.conditioned_reward_cut(bonus, other_bonus, cutoff)) {
            return Ok(None);
        }
        let conditioned = self.conditioned_reward_upper(bonus, other_bonus);
        if cutoff.is_some()
            && (self
                .reward_target
                .is_some_and(|target| conditioned < target)
                || self.reward_target.is_none() && cutoff.is_some_and(|c| conditioned < c.key))
        {
            return Ok(None);
        }
        let mut other_sum = 0.0;
        let runs = if self.gekiso.luck { self.samples } else { 1 };
        if runs < 1 {
            return Err("gekiso-reward-samples".into());
        }
        let count = runs as usize * 120;
        if self.just_context.is_some()
            && self.stable_life
            && self.canonical_slots
            && self.context_fields.owner_local()
            && cutoff.is_some_and(|c| c.key.is_finite())
        {
            let deck: Vec<_> = (0..5)
                .map(|i| {
                    let id = self.performer_id(team.members[i], team.snaps[i]);
                    self.performers[id as usize].clone()
                })
                .collect();
            if let Some(upper) = self.just_bounds.upper(self.master, &self.gekiso, &deck)? {
                let weights =
                    crate::gekiso_objective::note_weights(self.master, &self.gekiso, &upper);
                if let Some(coefficients) =
                    order_score_coefficients(self.master, &self.gekiso, &weights, &slots)
                {
                    let scores: Vec<_> = coefficients
                        .iter()
                        .map(|&c| add(mul(power as f64, c), (self.gekiso.notes.len() + 4) as f64))
                        .collect();
                    let reward = self.projected_settlement(bonus, false, &scores);
                    let other = self.projected_settlement(other_bonus, true, &scores);
                    let required = self.reward_target.unwrap_or(cutoff.unwrap().key);
                    if reward < required
                        || self.reward_target.is_none()
                            && cutoff.is_some_and(|c| c.prune_equal && reward == c.key)
                        || self.other_target.is_some_and(|target| other < target)
                    {
                        return Ok(None);
                    }
                    if let Some(cutoff) = score_cutoff {
                        let upper = if self.rank_other {
                            other
                        } else {
                            scores.iter().sum::<f64>() / 120.0
                        };
                        if self.reward_target.is_some()
                            && (upper < cutoff.key || cutoff.prune_equal && upper == cutoff.key)
                        {
                            return Ok(None);
                        }
                    }
                }
            }
        }
        if !self.gekiso.luck && cutoff.is_some_and(|c| c.key.is_finite()) {
            let context = self.context(team, 0)?;
            if let Some(scores) = self.order_ceilings(&context, &slots, power) {
                let reward = self.projected_settlement(bonus, false, &scores);
                let other = self.projected_settlement(other_bonus, true, &scores);
                let required = self.reward_target.unwrap_or(cutoff.unwrap().key);
                if reward < required
                    || self.reward_target.is_none()
                        && cutoff.is_some_and(|c| c.prune_equal && reward == c.key)
                    || self.other_target.is_some_and(|target| other < target)
                {
                    return Ok(None);
                }
                if let Some(cutoff) = score_cutoff {
                    let upper = if self.rank_other {
                        other
                    } else {
                        scores.iter().sum::<f64>() / 120.0
                    };
                    if self.reward_target.is_some()
                        && (upper < cutoff.key || cutoff.prune_equal && upper == cutoff.key)
                    {
                        return Ok(None);
                    }
                }
            }
        }
        let projection_info = if self.gekiso.luck && cutoff.is_some_and(|c| c.key.is_finite()) {
            self.rush_info(team)
        } else {
            None
        };
        let coefficient = if cutoff.is_some_and(|c| c.key.is_finite()) {
            self.team_coefficient(team)
        } else {
            f64::INFINITY
        };

        let ceiling = if cutoff.is_some_and(|c| c.key.is_finite()) {
            let ceiling = self.ceiling(power as f64, bonus, coefficient);
            if !safe(ceiling) || ceiling < 0.0 || !safe(count as f64 * ceiling) {
                return Err("gekiso-reward-precision".into());
            }
            Some(ceiling)
        } else {
            None
        };
        if let Some(info) = projection_info {
            let key = (
                info.ids,
                self.live_projection_shapes(team),
                info.extra.to_bits(),
                info.rows,
            );
            let cached = self.sampled_projections.get(&key).cloned();

            let cached_runs = cached
                .as_ref()
                .map_or(0, |p| p.coefficients.len() as i32 / 120);
            let primary_peak = self.ceiling(power as f64, bonus, coefficient);
            let secondary_peak = self.other_ceiling(power as f64, other_bonus, coefficient);
            let score_peak = score_bound(coefficient, power as f64);
            let mut sums = [0.0; 3];
            let mut appended = Vec::new();
            for completed in cached_runs.max(1)..=runs {
                if completed == cached_runs {
                    let projection = cached.as_ref().unwrap();
                    sums[0] = self
                        .projected_coefficient_sum(bonus, false, projection, power as f64, 0.0)
                        .min(completed as f64 * 120.0 * primary_peak);
                    sums[1] = self
                        .projected_coefficient_sum(other_bonus, true, projection, power as f64, 0.0)
                        .min(completed as f64 * 120.0 * secondary_peak);
                    sums[2] = mul(
                        completed as f64,
                        add(
                            mul(power as f64, projection.mean),
                            (self.gekiso.notes.len() + 4) as f64,
                        ),
                    );
                } else {
                    let Some((projection, offset)) =
                        self.projected_orders(team, info, completed - 1)?
                    else {
                        break;
                    };
                    let primary = self
                        .projected_coefficient_sum(bonus, false, &projection, power as f64, offset)
                        .min(120.0 * primary_peak);
                    let secondary = self
                        .projected_coefficient_sum(
                            other_bonus,
                            true,
                            &projection,
                            power as f64,
                            offset,
                        )
                        .min(120.0 * secondary_peak);
                    if !safe(primary) || !safe(secondary) {
                        break;
                    }
                    sums[0] += primary;
                    sums[1] += secondary;
                    sums[2] = add(
                        sums[2],
                        add(
                            mul(power as f64, add(projection.mean, offset)),
                            (self.gekiso.notes.len() + 4) as f64,
                        ),
                    );
                    appended.extend(
                        projection
                            .coefficients
                            .iter()
                            .map(|&value| add(value, offset)),
                    );
                }
                let remaining = (runs - completed) as f64;
                let primary = sums[0] + remaining * 120.0 * primary_peak;
                let secondary = sums[1] + remaining * 120.0 * secondary_peak;
                if !safe(primary) || !safe(secondary) {
                    break;
                }
                let upper = [
                    primary / count as f64,
                    secondary / count as f64,
                    up(add(sums[2], mul(remaining, score_peak)) / runs as f64),
                ];
                let required = self.reward_target.unwrap_or(cutoff.unwrap().key);
                if upper[0] < required
                    || self.other_target.is_some_and(|target| upper[1] < target)
                    || self.reward_target.is_none()
                        && cutoff.is_some_and(|c| c.prune_equal && upper[0] <= c.key)
                    || self.reward_target.is_some()
                        && score_cutoff.is_some_and(|c| {
                            let value = upper[if self.rank_other { 1 } else { 2 }];
                            value < c.key || c.prune_equal && value == c.key
                        })
                {
                    self.record_sampled_projection(key, cached.as_deref(), appended);
                    return Ok(None);
                }
            }
            self.record_sampled_projection(key, cached.as_deref(), appended);
        }
        let exact_slots: Vec<_> = slots.iter().map(|slot| (**slot).clone()).collect();
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
            let context = self.context(team, seed)?;
            let current = just_summary(&self.gekiso, &context.simulation)?;
            let result = if !self.gekiso.luck {
                let deck: Vec<_> = team
                    .members
                    .iter()
                    .zip(&team.snaps)
                    .map(|(&m, &s)| {
                        Performer::from_member(
                            self.master,
                            &self.members[m],
                            (s >= 0).then(|| &self.snaps[s as usize]),
                        )
                    })
                    .collect();
                gekiso::replay_orders(self.master, &self.gekiso, &deck, &[seed], power)?.scores
            } else {
                gekiso::score_orders(
                    self.master,
                    &self.gekiso,
                    &context.simulation,
                    power,
                    &exact_slots,
                    true,
                )?
            };
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
                let rank = chart::score_rank(self.chart, score);
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
    fn numeric_snap_dominance(
        &mut self,
        member: usize,
        better: i32,
        worse: i32,
        delta: f64,
    ) -> bool {
        if !self.allow_dominance
            || !self.canonical_slots
            || self.just_context.is_none()
            || self.master.live.life_base <= 0.0
        {
            return false;
        }
        let Some(bound) = &self.deterministic else {
            return false;
        };
        if delta <= bound.dominance_margin(self.power_estimate) {
            return false;
        }
        let other = |s: i32| {
            if s < 0 {
                0.0
            } else {
                self.other_snaps[s as usize]
            }
        };
        if other(better) < other(worse) {
            return false;
        }
        let a = self.slot(member, better);
        let b = self.slot(member, worse);
        if !crate::just_bound::slot_dominates(&a, &b) {
            return false;
        }
        let ai = self.performer_id(member, better);
        let bi = self.performer_id(member, worse);
        self.just_bounds.score_dominates(
            self.master,
            &self.performers[ai as usize],
            &self.performers[bi as usize],
        )
    }
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        self.slot(member, a).signature() == self.slot(member, b).signature()
            && self.performer_id(member, a) == self.performer_id(member, b)
    }
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn prepare_power(&mut self, power: Arc<Vec<f64>>, _width: usize) {
        self.snapshot_power = power;
    }
    fn prepare_dominance(&mut self, cap: f64) {
        self.power_estimate = cap;
    }
    fn prepare_assignment_power(&mut self, power: f64) {
        self.assignment_power = power;
    }

    fn needs_power_cap(&self) -> bool {
        self.canonical_slots
    }
    fn numeric_member_dominance(&mut self, better: usize, worse: usize, power_delta: f64) -> bool {
        if !self.canonical_slots
            || power_delta <= 0.0
            || self.other_members[better] < self.other_members[worse]
        {
            return false;
        }
        if let Some(&answer) = self.member_dominance.get(&(better, worse)) {
            return answer;
        }
        let answer = (-1..self.snaps.len() as i32).all(|s| {
            self.slot(better, s).signature() == self.slot(worse, s).signature()
                && self.performer_id(better, s) == self.performer_id(worse, s)
        });
        self.member_dominance.insert((better, worse), answer);
        answer
    }

    fn preserve_snap_options(&self) -> bool {
        !self.allow_dominance
    }
    fn snap_dominance(&mut self, member: usize, snap: i32) -> Vec<f64> {
        let mut key = self.slot(member, snap).signature();
        key.push(self.performer_id(member, snap) as u64);
        let next = self.snap_shapes.len();
        let shape = *self.snap_shapes.entry(key).or_insert(next) as f64;
        vec![
            if snap < 0 {
                0.0
            } else {
                self.other_snaps[snap as usize]
            },
            shape,
            -shape,
        ]
    }
    fn skill(&mut self, m: usize, s: i32) -> (f64, f64) {
        (
            0.0,
            self.deterministic.as_ref().map_or(0.0, |b| {
                b.weight(m, s, self.reward_target.is_some() && !self.rank_other)
            }),
        )
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
        self.total_bound(
            t,
            self.other_top.iter().take(5).sum::<f64>() + self.other_snap_top,
        )
    }
    fn bonus_floor(&mut self, limit: f64) -> Option<f64> {
        let required = self.reward_target.unwrap_or(limit);
        if !required.is_finite() {
            return None;
        }
        if let Some(&floor) = self.bonus_floors.get(&required.to_bits()) {
            return floor;
        }
        let mut low = 0i64;
        let mut high = self.bonus_max as i64;
        let floor = if self.ceiling(f64::INFINITY, high as f64, self.coefficient) < required {
            Some(f64::INFINITY)
        } else {
            while low < high {
                let mid = (low + high) >> 1;
                if self.ceiling(f64::INFINITY, mid as f64, self.coefficient) >= required {
                    high = mid;
                } else {
                    low = mid + 1;
                }
            }
            Some(low as f64)
        };
        if self.bonus_floors.len() >= 64 {
            self.bonus_floors.clear();
        }
        self.bonus_floors.insert(required.to_bits(), floor);
        floor
    }
    fn product_base(&self) -> Option<f64> {
        self.deterministic.as_ref().map(|b| b.base).or_else(|| {
            (self.just_context.is_none() && self.coefficient.is_finite())
                .then_some(self.coefficient)
        })
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<JointTarget>> {
        if let Some(bound) = &self.deterministic {
            if self.reward_target.is_some() && !self.rank_other {
                let converted = bound.converted;
                return Some(vec![JointTarget {
                    bonus: self.bonus_floor(limit).unwrap_or(0.0),
                    product: (limit.max(0.0) * converted).next_down().max(0.0),
                }]);
            }
        }
        if !self.coefficient.is_finite() {
            return None;
        }
        let required = self.reward_target.unwrap_or(limit);
        if !required.is_finite() {
            return None;
        }
        if let Some(targets) = self.targets.get(&required.to_bits()) {
            return Some(targets.clone());
        }
        let mut targets = Vec::new();
        for &rank in &self.ranks {
            if self.value(self.bonus_max, rank) < required {
                continue;
            }
            let (mut low, mut high) = (0i64, self.bonus_max as i64);
            while low < high {
                let mid = (low + high) >> 1;
                if self.value(mid as f64, rank) >= required {
                    high = mid;
                } else {
                    low = mid + 1;
                }
            }
            let score = if rank == 2 {
                0.0
            } else {
                self.chart
                    .ranks
                    .iter()
                    .find(|r| r.rank == rank)
                    .map_or(0.0, |r| r.required as f64)
            };
            targets.push(JointTarget {
                bonus: low as f64,
                product: self
                    .deterministic
                    .as_ref()
                    .map_or(score / (1.0 + crate::model::RELATIVE_ERROR), |b| {
                        (score * b.converted).next_down()
                    }),
            });
        }
        if self.targets.len() >= 64 {
            self.targets.clear();
        }
        self.targets.insert(required.to_bits(), targets.clone());
        Some(targets)
    }
    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        if let Some(bound) = &mut self.deterministic {
            bound.prepare_members(characters);
        }
        self.member_suffix = crate::search::suffix_tops(
            &characters
                .iter()
                .map(|members| {
                    members
                        .iter()
                        .map(|&i| self.other_members[i])
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
        if !self.bonus_ceilings_possible(members, &[]) {
            return Some(f64::NEG_INFINITY);
        }
        let other = members.iter().map(|&i| self.other_members[i]).sum::<f64>()
            + self.member_suffix[from][left]
            + self.other_snap_top;
        let mut upper = self.total_bound(t, other);
        if let Some(bound) = &mut self.deterministic {
            let coefficients = bound.member_coefficients(members, from);
            upper = upper.min(self.projected_bound(t, other, &coefficients));
        }
        Some(upper)
    }
    fn prepare_assignment(&mut self, _members: &[usize], options: &[Vec<i32>]) {
        self.assignment_combo = None;
        if let Some(global) = &self.deterministic {
            let formation: Vec<_> = _members.iter().map(|&m| self.members[m].clone()).collect();
            let combo = maximum_combo(self.master, &formation, &self.gekiso.setup.missions);
            let key = if self.stable_life && self.gekiso.setup.missions.iter().all(|&m| m == 1) {
                crate::reward_trace::script(self.master, &formation).map(ComboModelKey::Script)
            } else if combo != global.combo_multiplier {
                Some(ComboModelKey::Multiplier(combo.to_bits()))
            } else {
                None
            };
            if let Some(key) = key {
                if !self.combo_models.contains_key(&key) {
                    let script = if let ComboModelKey::Script(ref actors) = key {
                        Some(actors.as_slice())
                    } else {
                        None
                    };
                    if let Some(mut model) = DeterministicBound::with_combo(
                        self.master,
                        &self.gekiso,
                        self.members,
                        self.snaps,
                        combo,
                        script,
                    ) {
                        model.coefficient_limit = 256;
                        if self.combo_models.len() >= 32 {
                            if let Some(old) = self.combo_model_order.pop_front() {
                                self.combo_models.remove(&old);
                            }
                        }
                        self.combo_models.insert(key.clone(), model);
                    }
                }
                if self.combo_models.contains_key(&key) {
                    self.combo_model_order.retain(|old| old != &key);
                    self.combo_model_order.push_back(key.clone());
                    self.assignment_combo = Some(key);
                }
            }
        }
        self.assignment_primary = _members.iter().map(|&m| self.member_bonus(m)).sum();
        let power = Arc::clone(&self.snapshot_power);
        let bonus = Arc::clone(&self.primary_snap_bonus);
        let estimate = self.power_estimate;
        let maximum = self.bonus_max;
        if let Some(bound) = self.assignment_model() {
            bound.prepare_assignment(_members, options, power, bonus, estimate, maximum);
        }
        if self.reward_target.is_none() {
            return;
        }
        self.other_suffix = [0.0; 6];
        for slot in (0..5).rev() {
            self.other_suffix[slot] = self.other_suffix[slot + 1]
                + options[slot]
                    .iter()
                    .map(|&s| {
                        if s < 0 {
                            0.0
                        } else {
                            self.other_snaps[s as usize]
                        }
                    })
                    .fold(f64::NEG_INFINITY, f64::max);
        }
        self.other_ordered = options
            .iter()
            .flatten()
            .copied()
            .filter(|&s| s >= 0)
            .collect();
        self.other_ordered.sort_unstable();
        self.other_ordered.dedup();
        self.other_ordered.sort_by(|&a, &b| {
            self.other_snaps[b as usize].total_cmp(&self.other_snaps[a as usize])
        });
    }
    fn assignment_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
    ) -> Option<f64> {
        self.reward_target?;
        let distinct = self
            .other_ordered
            .iter()
            .filter(|s| !assigned[..from].contains(s))
            .take(5 - from)
            .map(|&s| self.other_snaps[s as usize])
            .sum::<f64>();
        let other = members.iter().map(|&i| self.other_members[i]).sum::<f64>()
            + assigned
                .iter()
                .take(from)
                .filter(|&&s| s >= 0)
                .map(|&s| self.other_snaps[s as usize])
                .sum::<f64>()
            + self.other_suffix[from].min(distinct);
        Some(self.total_bound(t, other))
    }
    fn assignment_order_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
        limit: f64,
    ) -> Option<f64> {
        if !self.bonus_pair_possible(members, assigned, from, limit) {
            return Some(f64::NEG_INFINITY);
        }
        let coefficients = self
            .assignment_model()?
            .assignment_coefficients(members, assigned, from);
        let other = if self.reward_target.is_some() {
            let distinct = self
                .other_ordered
                .iter()
                .filter(|s| !assigned[..from].contains(s))
                .take(5 - from)
                .map(|&s| self.other_snaps[s as usize])
                .sum::<f64>();
            members.iter().map(|&m| self.other_members[m]).sum::<f64>()
                + assigned[..from]
                    .iter()
                    .filter(|&&s| s >= 0)
                    .map(|&s| self.other_snaps[s as usize])
                    .sum::<f64>()
                + self.other_suffix[from].min(distinct)
        } else {
            self.other_top.iter().take(5).sum::<f64>() + self.other_snap_top
        };
        let mut upper = self.projected_bound(t, other, &coefficients);
        if upper <= limit {
            return Some(upper);
        }
        let base_power = self.assignment_power;
        let partitions = self
            .assignment_model()?
            .converter_partitions(members, assigned, from, base_power, t.power);
        if let Some(partitions) = partitions {
            let mut partition_upper = f64::NEG_INFINITY;
            for part in partitions {
                let coefficients = self.assignment_model()?.partition_coefficients(&part);
                let value = self.projected_bound(
                    Totals {
                        power: part.power,
                        ..t
                    },
                    other,
                    &coefficients,
                );
                partition_upper = partition_upper.max(value);
                if partition_upper > limit {
                    break;
                }
            }
            if partition_upper <= limit {
                return Some(partition_upper);
            }
        }
        let power = Arc::clone(&self.snapshot_power);
        let base_power = self.assignment_power;
        let scores = if from == 0 {
            Some(
                self.assignment_model()?
                    .joint_score_caps(members, &power, base_power),
            )
        } else {
            self.assignment_model()?.cached_joint_caps()
        };
        if let Some(scores) = scores {
            upper = upper.min(self.projected_bound(Totals { power: 1.0, ..t }, other, &scores));
            if upper <= limit {
                return Some(upper);
            }
        }
        if let Some(targets) = self.joint_targets(limit) {
            let power = self.assignment_power;
            let bonus = self.assignment_primary;
            if !self
                .assignment_model()?
                .dual_possible(members, assigned, from, power, bonus, &targets)
            {
                return Some(f64::NEG_INFINITY);
            }
        }
        Some(upper)
    }
    fn leaf_bound(&mut self, team: &Team, t: Totals) -> Option<f64> {
        if let Some(bound) = self.assignment_model() {
            let coefficients = bound.assignment_coefficients(&team.members, &team.snaps, 5);
            let other = self.other_bonus(team);
            return Some(self.total_bound(t, other).min(self.projected_bound(
                t,
                other,
                &coefficients,
            )));
        }
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
                let other_low = self
                    .ranks
                    .iter()
                    .map(|&r| self.other(bonus, r))
                    .fold(f64::INFINITY, f64::min);
                let other_high = self.other_ceiling(t.power, bonus, coefficient);
                if self
                    .other_target
                    .is_some_and(|target| other_low > target || other_high < target)
                {
                    (-1.0, -1.0)
                } else if self.rank_other {
                    (-1.0, other_high)
                } else {
                    (-1.0, score_bound(coefficient, t.power))
                }
            }
        } else {
            if self.other_target.is_some_and(|target| {
                self.other_ceiling(t.power, self.other_bonus(team), coefficient) < target
            }) {
                (-1.0, -1.0)
            } else if self.other_target.is_some() {
                (-1.0, high)
            } else {
                (low, high)
            }
        }
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        let exact = self
            .settle_cached(team, power as i32, None, None)?
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
            match self.settle_cached(team, power as i32, Some(reward_cutoff), Some(cutoff))? {
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

pub(crate) fn order_score_coefficients(
    master: &Master,
    chart: &gekiso::Chart,
    weights: &crate::gekiso_objective::Weights,
    slots: &[Arc<live::Slot>],
) -> Option<Vec<f64>> {
    let mut events = [[0.0; 5]; 5];
    let (mut absolute, mut actions) = (0.0, 0usize);
    for (slot_index, slot) in slots.iter().enumerate() {
        let (count, bound) = live::rounding_profile(slot, None);
        actions += count;
        absolute = add(absolute, bound);
        for effect in &slot.effects {
            let diff = f(effect.delta as f64 / 100000.0);
            if effect.gate.blocks(master.live.life_base)
                && effect.gate.blocks(2.0 * master.live.life_base)
            {
                continue;
            }
            let duration = window_ms(effect.seconds, slot.extension_ms);
            for (event, &(_, time)) in chart.full.events.iter().take(5).enumerate() {
                let start = chart.times.partition_point(|&t| t < time);
                let end = chart.times.partition_point(|&t| t < time + duration);
                let weight = if effect.kind == 2000 {
                    weights.general[end] - weights.general[start]
                } else {
                    effect
                        .judgements
                        .iter()
                        .filter(|&&j| (4..=6).contains(&j))
                        .map(|&j| weights.judgements[j - 4][end] - weights.judgements[j - 4][start])
                        .sum()
                };
                events[slot_index][event] += diff * weight;
            }
        }
    }
    let u = 2.0_f64.powi(-24);
    let operations = (2 * actions + 5) as f64;
    if operations * u >= 0.25 {
        return None;
    }
    let residual = mul(
        mul(16.0 * operations * u, absolute),
        up(1.0 / (1.0 - operations * u)),
    );
    let residual = mul(weights.general[chart.notes.len()], residual);
    let rho = up(1.0 / (1.0 - u));
    let mut rounding = 1.0;
    for _ in 0..12 {
        rounding = mul(rounding, rho);
    }
    Some(
        live::orders()
            .iter()
            .map(|order| {
                let mut coefficient = add(weights.base, residual);
                for (event, &slot) in order.iter().enumerate() {
                    coefficient = add(coefficient, events[slot][event]);
                }
                coefficient = mul(coefficient, rounding);
                up(coefficient / chart.full.converted as f64)
            })
            .collect(),
    )
}
