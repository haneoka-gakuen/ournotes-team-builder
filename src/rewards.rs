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
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
};
const MAX_SAFE: f64 = 9007199254740991.0;
fn safe(n: f64) -> bool {
    n.is_finite() && n.fract() == 0.0 && n.abs() <= MAX_SAFE
}
fn bonus_totals(members: &[f64], snaps: &[f64]) -> Vec<f64> {
    fn totals(values: &[f64]) -> Option<Vec<BTreeSet<i64>>> {
        let mut sums = vec![BTreeSet::new(); 6];
        sums[0].insert(0);
        for &value in values {
            if !safe(value) || !(0.0..=1e8).contains(&value) {
                return None;
            }
            for count in (1..=5).rev() {
                let previous: Vec<_> = sums[count - 1].iter().copied().collect();
                for sum in previous {
                    sums[count].insert(sum + value as i64);
                }
                if sums[count].len() > 4096 {
                    return None;
                }
            }
        }
        Some(sums)
    }
    let (Some(members), Some(snaps)) = (totals(members), totals(snaps)) else {
        return Vec::new();
    };
    let snapshots: BTreeSet<_> = snaps.into_iter().flatten().collect();
    let mut combined = BTreeSet::new();
    for &member in &members[5] {
        for &snap in &snapshots {
            combined.insert(member + snap);
            if combined.len() > 4096 {
                return Vec::new();
            }
        }
    }
    combined.into_iter().map(|value| value as f64).collect()
}
struct SnapshotRegion {
    snaps: [i32; 5],
    count: usize,
    bonus: f64,
}
#[derive(Clone)]
struct RegionRequirement {
    mean: f64,
    floors: Vec<(Arc<crate::event::RewardCurve>, f64)>,
}
fn snapshot_regions(bonus: &[f64], minimum: f64) -> Option<Vec<SnapshotRegion>> {
    if bonus.iter().any(|&value| value < 0.0) {
        return None;
    }
    let mut positive: Vec<_> = bonus
        .iter()
        .enumerate()
        .filter(|&(_, &value)| value > 0.0)
        .map(|(index, _)| index)
        .collect();
    positive.sort_by(|&a, &b| bonus[b].total_cmp(&bonus[a]));
    if minimum
        <= positive
            .iter()
            .take(3)
            .map(|&snap| bonus[snap])
            .sum::<f64>()
    {
        return None;
    }
    fn visit(
        bonus: &[f64],
        positive: &[usize],
        minimum: f64,
        from: usize,
        chosen: &mut Vec<usize>,
        sum: f64,
        regions: &mut Vec<SnapshotRegion>,
    ) -> bool {
        if sum
            + positive[from..]
                .iter()
                .take(5 - chosen.len())
                .map(|&snap| bonus[snap])
                .sum::<f64>()
            < minimum
        {
            return true;
        }
        if chosen.len() >= 4 && sum >= minimum {
            let mut snaps = [-1; 5];
            for (slot, &snap) in chosen.iter().enumerate() {
                snaps[slot] = snap as i32;
            }
            regions.push(SnapshotRegion {
                snaps,
                count: chosen.len(),
                bonus: sum,
            });
            if regions.len() > 4096 {
                return false;
            }
        }
        if chosen.len() == 5 {
            return true;
        }
        for index in from..positive.len() {
            let snap = positive[index];
            chosen.push(snap);
            if !visit(
                bonus,
                positive,
                minimum,
                index + 1,
                chosen,
                sum + bonus[snap],
                regions,
            ) {
                return false;
            }
            chosen.pop();
        }
        true
    }
    let mut regions = Vec::new();
    visit(
        bonus,
        &positive,
        minimum,
        0,
        &mut Vec::new(),
        0.0,
        &mut regions,
    )
    .then_some(regions)
}
fn up(n: f64) -> f64 {
    let magnitude = n.to_bits() & 0x7fff_ffff_ffff_ffff;
    f64::from_bits(magnitude.min(f64::INFINITY.to_bits() - 1) + 1)
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
        let prepared = bindings
            .is_none()
            .then(|| skills::prepare(master, members, snaps));
        for (index, member) in members.iter().enumerate() {
            let photos: Vec<_> = if let Some(b) = bindings {
                vec![b[index].as_ref()]
            } else {
                std::iter::once(None)
                    .chain(snaps.iter().map(Some))
                    .collect()
            };
            for (photo_index, snap) in photos.into_iter().enumerate() {
                let resolved;
                let slot = if let Some(table) = &prepared {
                    &table.slots[index * (snaps.len() + 1) + photo_index]
                } else {
                    resolved = skills::resolve(master, member, snap, &mut Vec::new());
                    &resolved
                };
                let mut shape = Vec::new();
                let (actions, absolute) = live::rounding_profile(slot, None);
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
pub(crate) fn deterministic_ranges<'a>(
    master: &Master,
    chart: &'a gekiso::Chart,
) -> Option<&'a [(i32, i32)]> {
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
        .as_deref()
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
#[derive(PartialEq, Eq, Hash)]
pub(crate) struct LuckMoments {
    pub samples: usize,
    pub rush: Vec<usize>,
    pub support: Vec<usize>,
    pub joint: Vec<usize>,
}
pub(crate) struct LuckEnvelope {
    pub(crate) fingerprint: OnceLock<u64>,
    pub moments: Option<Arc<LuckMoments>>,
    pub changes: usize,
    pub rush: Vec<bool>,
    pub support: Vec<bool>,
}
impl PartialEq for LuckEnvelope {
    fn eq(&self, other: &Self) -> bool {
        self.changes == other.changes
            && self.moments == other.moments
            && self.rush == other.rush
            && self.support == other.support
    }
}
impl Eq for LuckEnvelope {}
impl std::hash::Hash for LuckEnvelope {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u64(*self.fingerprint.get_or_init(|| {
            use std::hash::Hasher;
            let mut hasher = rustc_hash::FxHasher::default();
            self.changes.hash(&mut hasher);
            self.moments.hash(&mut hasher);
            self.rush.hash(&mut hasher);
            self.support.hash(&mut hasher);
            hasher.finish()
        }));
    }
}
pub(crate) struct FixedComponent {
    pub(crate) base: f64,
    pub(crate) converted: f64,
    pub(crate) width: usize,
    pub(crate) rows: Vec<[f64; 5]>,
}
pub(crate) struct FixedRegionRows {
    components: Vec<Arc<FixedComponent>>,
    values: Vec<OnceLock<FixedMemberRows>>,
    allocated: Arc<AtomicUsize>,
    bytes: AtomicUsize,
    referenced: AtomicBool,
    base: f64,
    converted: f64,
    width: usize,
}
struct FixedMemberRows {
    rows: Vec<[f64; 5]>,
    envelopes: Mutex<VecDeque<(u128, [f64; 5])>>,
}
impl FixedRegionRows {
    pub(crate) fn new(
        components: &[Arc<FixedComponent>],
        allocated: Arc<AtomicUsize>,
        extra: usize,
    ) -> Option<Self> {
        let first = components.first()?;
        let values = (0..first.rows.len() / first.width)
            .map(|_| OnceLock::new())
            .collect::<Vec<_>>();
        let bytes =
            values.capacity() * std::mem::size_of::<OnceLock<FixedMemberRows>>() + extra + 128;
        allocated.fetch_add(bytes, Ordering::Relaxed);
        Some(Self {
            values,
            allocated,
            bytes: AtomicUsize::new(bytes),
            referenced: AtomicBool::new(true),
            components: components.to_vec(),
            base: components
                .iter()
                .fold(0.0, |sum, component| add(sum, component.base)),
            converted: first.converted,
            width: first.width,
        })
    }
    pub(crate) fn touch(&self) {
        self.referenced.store(true, Ordering::Relaxed);
    }
    pub(crate) fn take_reference(&self) -> bool {
        self.referenced.swap(false, Ordering::Relaxed)
    }
    fn member(&self, member: usize) -> &FixedMemberRows {
        self.values[member].get_or_init(|| {
            let start = member * self.width;
            let rows = (start..start + self.width)
                .map(|index| {
                    std::array::from_fn(|event| {
                        self.components
                            .iter()
                            .fold(0.0, |sum, part| add(sum, part.rows[index][event]))
                    })
                })
                .collect::<Vec<_>>();
            let bytes = rows.capacity() * std::mem::size_of::<[f64; 5]>();
            self.bytes.fetch_add(bytes, Ordering::Relaxed);
            self.allocated.fetch_add(bytes, Ordering::Relaxed);
            FixedMemberRows {
                rows,
                envelopes: Mutex::new(VecDeque::new()),
            }
        })
    }
    fn member_rows(&self, member: usize) -> &[[f64; 5]] {
        &self.member(member).rows
    }
    fn member_envelope(&self, member: usize, choices: &[i32]) -> [f64; 5] {
        let member = self.member(member);
        let calculate = || {
            let mut row = [0.0_f64; 5];
            for &snap in choices {
                for (value, high) in row.iter_mut().zip(member.rows[(snap + 1) as usize]) {
                    *value = value.max(high);
                }
            }
            row
        };
        if self.width > 128 {
            return calculate();
        }
        let key = choices
            .iter()
            .fold(0u128, |mask, &snap| mask | (1 << (snap + 1)));
        let mut envelopes = member.envelopes.lock().unwrap();
        if let Some((_, row)) = envelopes.iter().find(|(mask, _)| *mask == key) {
            return *row;
        }
        let row = calculate();
        let capacity = envelopes.capacity();
        if envelopes.len() == 8 {
            envelopes.pop_front();
        }
        envelopes.push_back((key, row));
        let bytes = (envelopes.capacity() - capacity) * std::mem::size_of::<(u128, [f64; 5])>();
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        self.allocated.fetch_add(bytes, Ordering::Relaxed);
        row
    }
}
impl Drop for FixedRegionRows {
    fn drop(&mut self) {
        self.allocated
            .fetch_sub(self.bytes.load(Ordering::Relaxed), Ordering::Relaxed);
    }
}
pub(crate) struct FixedRegionBound {
    base: f64,
    converted: f64,
    width: usize,
    rows: Arc<FixedRegionRows>,
    members: [usize; 5],
    powers: Arc<Vec<f64>>,
    options: Vec<Vec<i32>>,
    envelopes: [[f64; 5]; 5],
    matching: crate::assignment::Scratch,
    matrix: Vec<Vec<f64>>,
    event_pricing: Option<FixedEventPricing>,
}
struct FixedEventPricing {
    base: f64,
    coefficient: f64,
    rows: Vec<Vec<(i32, f64)>>,
    cached: Option<(u64, u64, f64)>,
}
impl FixedRegionBound {
    pub(crate) fn new(
        rows: Arc<FixedRegionRows>,
        members: &[usize],
        powers: Arc<Vec<f64>>,
    ) -> Option<Self> {
        Some(Self {
            base: rows.base,
            converted: rows.converted,
            width: rows.width,
            rows,
            members: members.try_into().ok()?,
            powers,
            options: vec![Vec::new(); 5],
            envelopes: [[0.0; 5]; 5],
            matching: crate::assignment::Scratch::default(),
            matrix: Vec::new(),
            event_pricing: None,
        })
    }
    fn row(&self, slot: usize, snap: i32) -> [f64; 5] {
        self.rows.member_rows(self.members[slot])[(snap + 1) as usize]
    }
    pub(crate) fn prepare(&mut self, options: &[Vec<i32>]) {
        for (slot, choices) in options.iter().enumerate() {
            if self.options[slot] == *choices {
                continue;
            }
            self.event_pricing = None;
            self.envelopes[slot] = self.rows.member_envelope(self.members[slot], choices);
            self.options[slot].clone_from(choices);
        }
    }
    pub(crate) fn score_ceiling(
        &self,
        assigned: &[i32],
        from: usize,
        power: f64,
        criterion: crate::objectives::Criterion,
    ) -> f64 {
        use crate::objectives::Criterion;
        let rows: [[f64; 5]; 5] = std::array::from_fn(|slot| {
            if slot < from {
                self.row(slot, assigned[slot])
            } else {
                self.envelopes[slot]
            }
        });
        let variable = if criterion == Criterion::Mean {
            rows.iter().fold(0.0, |sum, row| {
                add(sum, up(row.iter().copied().fold(0.0, add) / 5.0))
            })
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
                    if mask & (1 << event) != 0 {
                        continue;
                    }
                    let next = mask | (1 << event);
                    let value = add(dp[mask], weight);
                    dp[next] = if criterion == Criterion::Min {
                        dp[next].min(value)
                    } else {
                        dp[next].max(value)
                    };
                }
            }
            dp[31]
        };
        mul(power, up(add(self.base, variable) / self.converted))
    }
    pub(crate) fn physical_score_ceiling(
        &mut self,
        base_power: f64,
        totals: Totals,
        criterion: crate::objectives::Criterion,
        required: Option<&[Vec<bool>]>,
        cutoff: f64,
    ) -> f64 {
        self.physical_score_ceiling_count(
            base_power,
            totals,
            criterion,
            required.map(|flags| (flags, 1, 5)),
            cutoff,
        )
    }
    fn joint_score_ceiling(&mut self, base_power: f64, power: f64, cutoff: f64) -> f64 {
        let columns = self.width - 1 + 5;
        self.matrix.resize_with(5, Vec::new);
        for row in &mut self.matrix {
            row.resize(columns, -1e100);
        }
        if self.event_pricing.is_none() {
            for (row, envelope) in self.matrix.iter_mut().zip(self.envelopes) {
                row[..5].copy_from_slice(&envelope);
            }
            self.matching.maximum(&self.matrix, 5, Some(&mut [0; 5]));
            let mut prices = self.matching.column_prices(5);
            let minimum = prices.iter().copied().fold(f64::INFINITY, f64::min);
            for price in &mut prices {
                *price = (*price - minimum).max(0.0);
            }
            let rows: Vec<Vec<_>> = self
                .options
                .iter()
                .enumerate()
                .map(|(slot, choices)| {
                    let profiles = self.rows.member_rows(self.members[slot]);
                    choices
                        .iter()
                        .map(|&snap| {
                            let coefficient = profiles[(snap + 1) as usize]
                                .iter()
                                .zip(&prices)
                                .map(|(&value, &price)| (value - price).next_up())
                                .fold(0.0, f64::max);
                            (snap, coefficient)
                        })
                        .collect()
                })
                .collect();
            let base = add(self.base, prices.into_iter().fold(0.0, add));
            let coefficient = rows.iter().fold(base, |sum, row| {
                add(sum, row.iter().map(|entry| entry.1).fold(0.0, f64::max))
            });
            self.event_pricing = Some(FixedEventPricing {
                base,
                coefficient,
                rows,
                cached: None,
            });
        }
        let pricing = self.event_pricing.as_ref().unwrap();
        if let Some((base, total, upper)) = pricing.cached {
            if base == base_power.to_bits() && total == power.to_bits() {
                return upper;
            }
        }
        let ratio = (pricing.coefficient / power.max(1.0)).sqrt();
        if !ratio.is_finite() || ratio <= 0.0 {
            return f64::INFINITY;
        }
        let mut upper = f64::INFINITY;
        for scale in [1.0, 0.75, 1.5] {
            let lambda = ratio * scale;
            for (slot, row) in self.matrix.iter_mut().enumerate() {
                row.fill(-1e100);
                for &(snap, coefficient) in &pricing.rows[slot] {
                    let column = if snap < 0 {
                        self.width - 1 + slot
                    } else {
                        snap as usize
                    };
                    let power = self.powers[slot * self.width
                        + if snap < 0 {
                            self.width - 1
                        } else {
                            snap as usize
                        }];
                    row[column] = add(mul(lambda, power), up(coefficient / lambda));
                }
            }
            let variable = self.matching.maximum_upper(&self.matrix, columns);
            if variable < -1e50 {
                return f64::NEG_INFINITY;
            }
            let total = add(
                add(mul(lambda, base_power), up(pricing.base / lambda)),
                variable,
            );
            upper = upper.min(up(mul(total, total) / (4.0 * self.converted)));
            if upper < cutoff {
                break;
            }
        }
        self.event_pricing.as_mut().unwrap().cached =
            Some((base_power.to_bits(), power.to_bits(), upper));
        upper
    }
    pub(crate) fn physical_score_ceiling_count(
        &mut self,
        base_power: f64,
        totals: Totals,
        criterion: crate::objectives::Criterion,
        required: Option<(&[Vec<bool>], usize, usize)>,
        cutoff: f64,
    ) -> f64 {
        let joint = if criterion == crate::objectives::Criterion::Max {
            let upper = self.joint_score_ceiling(base_power, totals.power, cutoff);
            if upper < cutoff {
                return upper;
            }
            upper
        } else {
            f64::INFINITY
        };
        let initial = if criterion == crate::objectives::Criterion::Min {
            let mean = self.physical_score_ceiling_count(
                base_power,
                totals,
                crate::objectives::Criterion::Mean,
                required,
                cutoff,
            );
            if mean < cutoff {
                return mean;
            }
            mean
        } else {
            joint
        };
        let order = if criterion == crate::objectives::Criterion::Min {
            Some(crate::assignment::minimum_order(&self.envelopes))
        } else {
            None
        };
        let value = |slot: usize, row: [f64; 5]| {
            if let Some(order) = order {
                row[order[slot]]
            } else if criterion == crate::objectives::Criterion::Max {
                row.into_iter().fold(f64::NEG_INFINITY, f64::max)
            } else {
                up(row.into_iter().fold(0.0, add) / 5.0)
            }
        };
        let coefficient = self
            .envelopes
            .iter()
            .enumerate()
            .fold(self.base, |sum, (slot, &row)| add(sum, value(slot, row)));
        let ratio = (coefficient / totals.power.max(1.0)).sqrt();
        if !ratio.is_finite() || ratio <= 0.0 {
            return f64::INFINITY;
        }
        let columns = self.width - 1 + 5;
        self.matrix.resize_with(5, Vec::new);
        for row in &mut self.matrix {
            row.resize(columns, -1e100);
        }
        let mut upper = initial;
        for scale in [1.0, 0.75, 1.5] {
            let lambda = ratio * scale;
            let mut event_rows = [[[f64::NEG_INFINITY; 2]; 5]; 5];
            for (slot, row) in self.matrix.iter_mut().enumerate() {
                row.fill(-1e100);
                let profiles = self.rows.member_rows(self.members[slot]);
                for &snap in &self.options[slot] {
                    let power = self.powers[slot * self.width
                        + if snap < 0 {
                            self.width - 1
                        } else {
                            snap as usize
                        }];
                    let coefficients = profiles[(snap + 1) as usize];
                    if criterion == crate::objectives::Criterion::Max {
                        let column = if snap < 0 {
                            self.width - 1 + slot
                        } else {
                            snap as usize
                        };
                        let class =
                            usize::from(required.is_some_and(|(flags, _, _)| flags[slot][column]));
                        for (event, coefficient) in coefficients.iter().enumerate() {
                            event_rows[slot][event][class] = event_rows[slot][event][class]
                                .max(add(mul(lambda, power), up(coefficient / lambda)));
                        }
                    }
                    let weight = value(slot, coefficients);
                    row[if snap < 0 {
                        self.width - 1 + slot
                    } else {
                        snap as usize
                    }] = add(mul(lambda, power), up(weight / lambda));
                }
            }
            let highs: [f64; 5] = std::array::from_fn(|slot| {
                self.matrix[slot]
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max)
            });
            let mut variable = highs.into_iter().fold(0.0, add);
            if let Some((required, minimum, maximum)) = required {
                let mut dp = [f64::NEG_INFINITY; 6];
                dp[0] = 0.0;
                for (slot, flags) in required.iter().enumerate() {
                    let mut classes = [f64::NEG_INFINITY; 2];
                    for (&value, &yes) in self.matrix[slot].iter().zip(flags) {
                        classes[usize::from(yes)] = classes[usize::from(yes)].max(value);
                    }
                    let mut next = [f64::NEG_INFINITY; 6];
                    for count in 0..=slot {
                        if dp[count] < -1e50 {
                            continue;
                        }
                        for (extra, &price) in classes.iter().enumerate() {
                            if price < -1e50 {
                                continue;
                            }
                            next[count + extra] = next[count + extra].max(add(dp[count], price));
                        }
                    }
                    dp = next;
                }
                let forced = dp[minimum..=maximum]
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max);
                variable = variable.min(forced);
            }
            if criterion == crate::objectives::Criterion::Max {
                let mut dp = [[f64::NEG_INFINITY; 6]; 32];
                dp[0][0] = 0.0;
                for mask in 0usize..31 {
                    let slot = mask.count_ones() as usize;
                    for count in 0..=slot {
                        if dp[mask][count] < -1e50 {
                            continue;
                        }
                        for (event, prices) in event_rows[slot].iter().enumerate() {
                            if mask & (1 << event) != 0 {
                                continue;
                            }
                            for (extra, &price) in prices.iter().enumerate() {
                                if price < -1e50 {
                                    continue;
                                }
                                let next = mask | (1 << event);
                                dp[next][count + extra] =
                                    dp[next][count + extra].max(add(dp[mask][count], price));
                            }
                        }
                    }
                }
                let (minimum, maximum) = required.map_or((0, 5), |(_, low, high)| (low, high));
                let price = dp[31][minimum..=maximum]
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max);
                variable = variable.min(price);
            }
            if variable < -1e50 {
                return f64::NEG_INFINITY;
            }
            let constant = add(mul(lambda, base_power), up(self.base / lambda));
            let total = add(constant, variable);
            upper = upper.min(up(mul(total, total) / (4.0 * self.converted)));
            if upper < cutoff {
                return upper;
            }
            variable = variable.min(self.matching.maximum_upper(&self.matrix, columns));
            if variable < -1e50 {
                return f64::NEG_INFINITY;
            }
            let total = add(constant, variable);
            upper = upper.min(up(mul(total, total) / (4.0 * self.converted)));
            if upper < cutoff {
                return upper;
            }
        }
        upper
    }
}
pub(crate) struct BoundPreparation {
    layout: Arc<CumulativeLayout>,
    slots: Vec<PreparedSlot>,
    row_ids: Vec<u32>,
    member_rows: Vec<Vec<u32>>,
    capacity: usize,
}
struct PreparedSlot {
    positive: f64,
    converts: f64,
    regional_converts: Option<Vec<f64>>,
    score_rows: Vec<[i32; 5]>,
    normal: (usize, f64),
    shape: Vec<(i32, f64)>,
    windows: Vec<(f64, Vec<(usize, usize)>)>,
}
impl PreparedSlot {
    fn template_key(&self) -> Vec<u64> {
        let mut key = vec![
            self.positive.to_bits(),
            self.converts.to_bits(),
            self.normal.0 as u64,
            self.normal.1.to_bits(),
        ];
        match &self.regional_converts {
            None => key.push(0),
            Some(regions) => {
                key.extend([1, regions.len() as u64]);
                key.extend(regions.iter().map(|x| x.to_bits()));
            }
        }
        key.push(self.score_rows.len() as u64);
        key.extend(self.score_rows.iter().flatten().map(|&x| x as u64));
        key.push(self.shape.len() as u64);
        for &(duration, delta) in &self.shape {
            key.extend([duration as u64, delta.to_bits()]);
        }
        key.push(self.windows.len() as u64);
        for (delta, intervals) in &self.windows {
            key.extend([delta.to_bits(), intervals.len() as u64]);
            for &(start, end) in intervals {
                key.extend([start as u64, end as u64]);
            }
        }
        key
    }
}
impl BoundPreparation {
    fn new(
        master: &Master,
        chart: &gekiso::Chart,
        members: &[Member],
        snaps: &[Snap],
        ranges: &[(i32, i32)],
        layout: Arc<CumulativeLayout>,
    ) -> Option<Self> {
        let luck_cap = chart.luck;
        let judged_count = chart
            .play
            .frames
            .iter()
            .map(|f| f.judged.len())
            .sum::<usize>();
        let static_group = |group: i32, p: &Performer| static_gk_group(master, group, p);
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
        let mut slots = Vec::new();
        let mut row_ids = Vec::new();
        let mut templates = HashMap::<Vec<u64>, u32>::default();
        let mut prepared_skills = None;
        for (member_index, member) in members.iter().enumerate() {
            for (snap_index, snap) in std::iter::once(None)
                .chain(snaps.iter().map(Some))
                .enumerate()
            {
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
                            if luck_cap && skill.mission_type == 2 {
                                if (11000..=11005).contains(&row.kind)
                                    || [3004, 4004].contains(&row.kind)
                                {
                                    continue;
                                }
                                if row.kind == 2000 {
                                    if row.value < 0
                                        || row.seconds != 0.0
                                        || row.trigger_type != 2
                                        || row.cumulative_id != 0
                                        || row.reset_group != 0
                                        || row.release_group != 0
                                        || !single(row.trigger_group, 7021)
                                    {
                                        return None;
                                    }
                                    if row.value > 0 && static_group(row.condition_group, &p)? {
                                        score_rows.push([row.value, row.value, -1, 1000, 2]);
                                        positive =
                                            add(positive, up(f(f(row.value as f64) / 10000.0)));
                                    }
                                    continue;
                                }
                            }
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
                let raw =
                    prepared_skills.get_or_insert_with(|| skills::prepare(master, members, snaps));
                let slot = &raw.slots[member_index * (snaps.len() + 1) + snap_index];
                if slot
                    .convert
                    .as_ref()
                    .is_some_and(|c| c.judgements.contains(&6))
                {
                    return None;
                }
                let (actions, absolute) = live::rounding_profile(slot, Some(master.live.life_base));
                let mut shape = Vec::new();
                let mut normal_windows = Vec::new();
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
                    let intervals = chart
                        .full
                        .events
                        .iter()
                        .take(5)
                        .map(|&(_, time)| {
                            let start = chart.times.partition_point(|&t| t < time);
                            let end = chart.times.partition_point(|&t| {
                                t < time + window_ms(effect.seconds, slot.extension_ms)
                            });
                            (start, end)
                        })
                        .collect();
                    normal_windows.push((delta, intervals));
                }
                score_rows.sort_unstable();
                let slot = PreparedSlot {
                    positive,
                    converts,
                    regional_converts,
                    score_rows,
                    normal: (actions, absolute),
                    shape,
                    windows: normal_windows,
                };
                let key = slot.template_key();
                let next = slots.len() as u32;
                let id = *templates.entry(key).or_insert_with(|| {
                    slots.push(slot);
                    next
                });
                row_ids.push(id);
            }
        }
        let width = snaps.len() + 1;
        let member_rows = row_ids
            .chunks(width)
            .map(|rows| {
                let mut unique = Vec::new();
                for &id in rows {
                    if !unique.contains(&id) {
                        unique.push(id);
                    }
                }
                unique
            })
            .collect();
        let capacity = (1..width)
            .filter(|&snap| {
                (0..members.len()).any(|m| slots[row_ids[m * width + snap] as usize].converts > 0.0)
            })
            .count()
            .min(5);
        Some(Self {
            layout,
            slots,
            row_ids,
            member_rows,
            capacity,
        })
    }
}
enum ComboSource<'a> {
    None,
    Actors(&'a [Performer]),
    Trace(Arc<crate::reward_trace::ComboTrace>),
    Prepared(
        Option<Arc<crate::reward_trace::ComboTrace>>,
        Arc<BoundPreparation>,
    ),
}
struct ComboEnvelope {
    multiplier: f64,
    trace: Option<Arc<crate::reward_trace::ComboTrace>>,
    rush: Option<Vec<bool>>,
    rush_weights: Option<Vec<f64>>,
    prepared: Option<Arc<BoundPreparation>>,
    rush_commands: usize,
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
    Script(String, u64),
}
struct CumulativeLayout {
    counts: Vec<Option<CounterFrame>>,
    limits: Vec<(i32, f64, [f64; 6], f64)>,
}
impl CumulativeLayout {
    fn new(
        master: &Master,
        chart: &gekiso::Chart,
        members: &[Member],
        snaps: &[Snap],
        ranges: &[(i32, i32)],
    ) -> Option<Self> {
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
                    combo: (j + p) as f64,
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
            let maximum = (j + p) as f64;
            limits.push((chart.setup.missions[i], j as f64, capacities, maximum));
        }
        Some(Self { counts, limits })
    }
}
struct CumulativeWindows {
    layout: Arc<CumulativeLayout>,
    limits: Vec<(i32, f64, [f64; 6], f64)>,
    multiplier: f64,
    trace: Option<Arc<crate::reward_trace::ComboTrace>>,
    rush: Option<Vec<bool>>,
    rush_weights: Option<Vec<f64>>,
    weights: Vec<f64>,
    gains: HashMap<Vec<[i32; 5]>, [f64; 6]>,
    rush_commands: usize,
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
        let layout = match &envelope.prepared {
            Some(prepared) => Arc::clone(&prepared.layout),
            None => Arc::new(CumulativeLayout::new(
                master, chart, members, snaps, ranges,
            )?),
        };
        let limits = layout
            .limits
            .iter()
            .enumerate()
            .map(|(i, &(mission, just, extras, base_combo))| {
                let maximum = envelope
                    .trace
                    .as_ref()
                    .map_or(base_combo * envelope.multiplier, |trace| trace.maxima[i]);
                (mission, just, extras, maximum)
            })
            .collect();
        Some(Self {
            layout,
            limits,
            multiplier: envelope.multiplier,
            trace: envelope.trace.or_else(|| {
                (!chart.luck && chart.setup.missions.iter().all(|&m| m == 3))
                    .then(|| crate::reward_trace::ComboTrace::new(master, chart, &[]))
                    .flatten()
                    .map(Arc::new)
            }),
            rush: envelope.rush,
            rush_weights: envelope.rush_weights,
            weights,
            gains: HashMap::default(),
            rush_commands: envelope.rush_commands,
        })
    }
    fn commands(&self, rows: &[[i32; 5]]) -> [usize; 6] {
        let mut out = [0usize; 6];
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
                    } else if n < 0 {
                        self.rush_commands
                    } else {
                        cap
                    };
                    if changes > 0 {
                        *count = count.saturating_add(changes.saturating_mul(2).saturating_add(4));
                    }
                }
            }
        }
        out
    }
    fn gain(&mut self, chart: &gekiso::Chart, rows: &[[i32; 5]]) -> [f64; 6] {
        if let Some(&gain) = self.gains.get(rows) {
            return gain;
        }
        if rows.is_empty() {
            let gain = [0.0; 6];
            self.gains.insert(rows.to_vec(), gain);
            return gain;
        }
        if rows.iter().all(|r| r[3] == 7001) {
            if let Some(trace) = &self.trace {
                if let Some(values) = trace.support(chart, rows) {
                    let value = values
                        .into_iter()
                        .zip(&self.weights)
                        .fold(0.0, |sum, (value, &weight)| {
                            add(sum, mul(up(value), weight))
                        });
                    let gain = [value; 6];
                    self.gains.insert(rows.to_vec(), gain);
                    return gain;
                }
            }
        }
        let mut gain = [0.0; 6];
        for (i, count) in self.layout.counts.iter().enumerate() {
            let Some(frame) = count else {
                continue;
            };
            for &[value, maximum, n, kind, mission] in rows {
                if n < 0 && self.rush.as_ref().is_some_and(|rush| !rush[i]) {
                    continue;
                }
                if mission != 4 && mission != frame.mission {
                    continue;
                }
                for (c, total) in gain.iter_mut().enumerate() {
                    let counter = if kind == 7001 {
                        frame.combo * self.multiplier
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
                    let weight = if n < 0 {
                        self.rush_weights
                            .as_ref()
                            .map_or(self.weights[i], |weights| weights[i])
                    } else {
                        self.weights[i]
                    };
                    *total = add(*total, mul(weight, factor));
                }
            }
        }
        if chart.setup.missions.iter().all(|&m| m == 3) {
            if let Some(values) = self
                .trace
                .as_ref()
                .and_then(|trace| trace.support(chart, rows))
            {
                gain[0] = values
                    .into_iter()
                    .zip(&self.weights)
                    .fold(0.0, |sum, (value, &weight)| {
                        add(sum, mul(up(value), weight))
                    });
            }
        }
        self.gains.insert(rows.to_vec(), gain);
        gain
    }
}
#[derive(Clone, Copy)]
struct RewardDual {
    lambda: f64,
    mu: f64,
}
type DualOffers = Vec<Vec<Option<[f64; 5]>>>;
type PowerFrontiers = Vec<[Vec<(f64, f64)>; 6]>;
type PricingFrontiers = Vec<Vec<Option<[Vec<i32>; 5]>>>;
type JointRewardOffers = [[Vec<(usize, f64, f64, f64)>; 5]; 5];
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
#[derive(Clone)]
struct FixedScoreOptions {
    options: Vec<i32>,
    envelopes: [u32; 6],
    normal: (usize, f64),
    converter: bool,
    minimum_conversion: f64,
}
pub(crate) struct DeterministicBound {
    pub(crate) base: f64,
    combo_multiplier: f64,
    capacity: usize,
    pub(crate) coefficient_limit: usize,
    duals: Vec<Vec<RewardDual>>,
    dual_witnesses: [usize; 6],
    dual_paths: Vec<Vec<[u8; 5]>>,
    dual_offers: Vec<DualOffers>,
    option_cache: Vec<Option<Vec<i32>>>,
    fixed_score_options: Vec<Option<FixedScoreOptions>>,
    option_minimum_conversion: Vec<f64>,
    option_envelopes: Vec<[u32; 6]>,
    option_converters: Vec<bool>,
    score_frontiers: Vec<PowerFrontiers>,
    pricing_frontiers: PricingFrontiers,
    sparse_matrix: [Vec<(usize, f64)>; 5],
    snapshot_power: Arc<Vec<f64>>,
    snapshot_bonus: Arc<Vec<f64>>,
    converted: f64,
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
    joint_rewards: Option<Arc<JointRewardPricing>>,
    joint_mean: Option<Arc<JointRewardPricing>>,
    prefix_mean: Option<PrefixMeanPricing>,
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
    physical_matching: crate::assignment::Scratch,
    physical_matrix: Vec<Vec<f64>>,
    preparation: Option<Arc<BoundPreparation>>,
    member_pricing: Option<Arc<MemberRewardPricing>>,
}
struct MemberRewardPlane {
    constant: f64,
    mu: f64,
    members: Vec<[f64; 5]>,
    suffix: Vec<[f64; 32]>,
    mean: Vec<f64>,
    mean_suffix: Vec<[f64; 6]>,
}
struct JointRewardRow {
    ceiling: f64,
    prices: Vec<(f64, f64)>,
}
struct PrefixMeanPlane {
    constant: f64,
    mu: f64,
    values: Vec<Vec<f64>>,
    top: Vec<Vec<(i32, f64)>>,
}
struct PrefixMeanPricing {
    planes: Vec<PrefixMeanPlane>,
    converted: f64,
}
impl PrefixMeanPricing {
    fn sums(&self, assigned: &[i32], from: usize) -> Vec<f64> {
        self.planes
            .iter()
            .map(|plane| {
                (0..5).fold(plane.constant, |sum, slot| {
                    let value = if slot < from {
                        plane.values[slot][(assigned[slot] + 1) as usize]
                    } else {
                        plane.top[slot]
                            .iter()
                            .find(|&&(snap, _)| snap < 0 || !assigned[..from].contains(&snap))
                            .map_or(f64::INFINITY, |cell| cell.1)
                    };
                    (sum + value).next_up()
                })
            })
            .collect()
    }
    fn mean(&self, sums: &[f64], bonus: f64) -> f64 {
        let minimum = self
            .planes
            .iter()
            .zip(sums)
            .fold(f64::INFINITY, |minimum, (plane, &sum)| {
                let total = (sum - (plane.mu * bonus).next_down()).next_up().max(0.0);
                minimum.min(total)
            });
        up(mul(minimum, minimum) / (4.0 * self.converted))
    }
}
impl JointRewardRow {
    fn upper(&self, bonus: f64, converted: f64) -> f64 {
        let mut minimum = f64::INFINITY;
        for &(constant, mu) in &self.prices {
            let total = (constant - (mu * bonus).next_down()).next_up().max(0.0);
            minimum = minimum.min(total);
        }
        self.ceiling
            .min(up(mul(minimum, minimum) / (4.0 * converted)))
    }
}
struct JointRewardPricing {
    rows: Vec<JointRewardRow>,
    converted: f64,
}
impl JointRewardPricing {
    fn scores(&self, bonus: f64) -> Vec<f64> {
        let mut scores: Vec<_> = self
            .rows
            .iter()
            .map(|row| row.upper(bonus, self.converted))
            .collect();
        scores.resize(120, f64::INFINITY);
        scores.sort_by(f64::total_cmp);
        scores
    }
}
struct MemberRewardPricing {
    planes: Vec<MemberRewardPlane>,
    converted: f64,
    prefixes: Mutex<[Option<MemberPricePrefix>; 6]>,
}
struct MemberPricePrefix {
    members: Vec<usize>,
    sums: Vec<f64>,
}
pub(crate) struct RosterPricing {
    planes: Vec<(f64, Vec<[f64; 5]>)>,
    converted: f64,
}
pub(crate) struct RosterSuffix {
    pricing: Arc<RosterPricing>,
    tables: Vec<Option<Vec<[f64; 32]>>>,
    characters: Vec<Vec<usize>>,
    prefixes: [Option<RosterPrefix>; 6],
    bonuses: Vec<usize>,
    target: Option<usize>,
}
struct RosterPrefix {
    members: Vec<usize>,
    values: Vec<Option<[f64; 32]>>,
}
impl RosterSuffix {
    pub(crate) fn repriced(&self, pricing: Arc<RosterPricing>) -> Self {
        Self {
            tables: vec![None; pricing.planes.len()],
            pricing,
            characters: self.characters.clone(),
            prefixes: std::array::from_fn(|_| None),
            bonuses: self.bonuses.clone(),
            target: self.target,
        }
    }
    pub(crate) fn new(
        pricing: Arc<RosterPricing>,
        characters: &[Vec<usize>],
        bonuses: &[i32],
        level: i32,
    ) -> Self {
        let target = ((0..=64).contains(&level)
            && bonuses.iter().all(|&value| value >= 0)
            && bonuses.iter().any(|&value| value > 0))
        .then_some(level as usize);
        Self {
            tables: vec![None; pricing.planes.len()],
            pricing,
            characters: characters.to_vec(),
            prefixes: std::array::from_fn(|_| None),
            bonuses: bonuses.iter().map(|&value| value.max(0) as usize).collect(),
            target,
        }
    }
    fn table(&self, plane: usize) -> Vec<[f64; 32]> {
        let rows = &self.pricing.planes[plane].1;
        let target = self.target;
        let width = target.map_or(1, |target| target + 1);
        let mut table = vec![[f64::NEG_INFINITY; 32]; (self.characters.len() + 1) * width];
        table[self.characters.len() * width][0] = 0.0;
        for from in (0..self.characters.len()).rev() {
            if let Some(target) = target {
                table.copy_within((from + 1) * width..(from + 2) * width, from * width);
                let mut groups: Vec<(usize, [f64; 5])> = Vec::new();
                for &member in &self.characters[from] {
                    let bonus = self.bonuses[member];
                    if bonus > target {
                        continue;
                    }
                    if let Some((_, row)) = groups.iter_mut().find(|(old, _)| *old == bonus) {
                        for (value, &candidate) in row.iter_mut().zip(&rows[member]) {
                            *value = value.max(candidate);
                        }
                    } else {
                        groups.push((bonus, rows[member]));
                    }
                }
                for (bonus, row) in groups {
                    for total in bonus..=target {
                        for mask in 0usize..31 {
                            let previous = table[(from + 1) * width + total - bonus][mask];
                            if previous == f64::NEG_INFINITY {
                                continue;
                            }
                            let mut available = 31 ^ mask;
                            while available != 0 {
                                let event = available.trailing_zeros() as usize;
                                available &= available - 1;
                                if row[event] == f64::NEG_INFINITY {
                                    continue;
                                }
                                let cell = &mut table[from * width + total][mask | (1 << event)];
                                *cell = cell.max((previous + row[event]).next_up());
                            }
                        }
                    }
                }
                continue;
            }
            table[from] = table[from + 1];
            let group: [f64; 5] = std::array::from_fn(|event| {
                self.characters[from]
                    .iter()
                    .map(|&member| rows[member][event])
                    .fold(f64::NEG_INFINITY, f64::max)
            });
            for mask in 1usize..32 {
                for (event, &value) in group.iter().enumerate() {
                    let previous = table[from + 1][mask ^ (1 << event)];
                    if mask & (1 << event) != 0
                        && previous != f64::NEG_INFINITY
                        && value != f64::NEG_INFINITY
                    {
                        table[from][mask] = table[from][mask].max((previous + value).next_up());
                    }
                }
            }
        }
        table
    }
    pub(crate) fn upper(&mut self, members: &[usize], from: usize, limit: f64) -> f64 {
        let count = members.len();
        let at = if let Some(target) = self.target {
            let used = members
                .iter()
                .map(|&member| self.bonuses[member])
                .sum::<usize>();
            let Some(left) = target.checked_sub(used) else {
                return f64::NEG_INFINITY;
            };
            from * (target + 1) + left
        } else {
            from
        };
        if self.prefixes[count]
            .as_ref()
            .is_none_or(|prefix| prefix.members != members)
        {
            let mut prefix = self.prefixes[count].take().unwrap_or_else(|| RosterPrefix {
                members: Vec::with_capacity(count),
                values: Vec::with_capacity(self.pricing.planes.len()),
            });
            prefix.members.clear();
            prefix.members.extend_from_slice(members);
            prefix.values.clear();
            prefix.values.resize(self.pricing.planes.len(), None);
            self.prefixes[count] = Some(prefix);
        }
        let mut upper = f64::INFINITY;
        for plane in 0..self.pricing.planes.len() {
            if self.prefixes[count].as_ref().unwrap().values[plane].is_none() {
                let parent = count
                    .checked_sub(1)
                    .and_then(|index| self.prefixes[index].as_ref())
                    .filter(|parent| parent.members == members[..count - 1])
                    .and_then(|parent| parent.values[plane]);
                let mut dp = parent.unwrap_or([f64::NEG_INFINITY; 32]);
                dp[0] = 0.0;
                let rows = &self.pricing.planes[plane].1;
                for (slot, &member) in members
                    .iter()
                    .enumerate()
                    .skip(parent.map_or(0, |_| count - 1))
                {
                    for &mask in crate::assignment::SLOT_MASKS[slot] {
                        if dp[mask] == f64::NEG_INFINITY {
                            continue;
                        }
                        for (event, &value) in rows[member].iter().enumerate() {
                            if mask & (1 << event) == 0 && value != f64::NEG_INFINITY {
                                let next = mask | (1 << event);
                                dp[next] = dp[next].max((dp[mask] + value).next_up());
                            }
                        }
                    }
                }
                self.prefixes[count].as_mut().unwrap().values[plane] = Some(dp);
            }
            let dp = self.prefixes[count].as_ref().unwrap().values[plane]
                .as_ref()
                .unwrap();
            if self.tables[plane].is_none() {
                self.tables[plane] = Some(self.table(plane));
            }
            let table = self.tables[plane].as_ref().unwrap();
            let mut variable = f64::NEG_INFINITY;
            for &mask in crate::assignment::SLOT_MASKS[count] {
                if dp[mask] != f64::NEG_INFINITY && table[at][31 ^ mask] != f64::NEG_INFINITY {
                    variable = variable.max((dp[mask] + table[at][31 ^ mask]).next_up());
                }
            }
            if variable == f64::NEG_INFINITY {
                return f64::NEG_INFINITY;
            }
            let sum = (self.pricing.planes[plane].0 + variable).next_up();
            if sum.is_finite() && sum >= 0.0 {
                upper = upper.min(up(mul(sum, sum) / (4.0 * self.pricing.converted)));
                if upper < limit {
                    break;
                }
            }
        }
        upper
    }
}
impl RosterPricing {
    pub(crate) fn upper(&self, members: &[usize], limit: f64) -> f64 {
        let mut upper = f64::INFINITY;
        for (constant, rows) in &self.planes {
            let mut dp = [f64::NEG_INFINITY; 32];
            dp[0] = 0.0;
            for mask in 0usize..31 {
                let slot = mask.count_ones() as usize;
                for (event, &value) in rows[members[slot]].iter().enumerate() {
                    if mask & (1 << event) == 0 {
                        let next = mask | (1 << event);
                        dp[next] = dp[next].max((dp[mask] + value).next_up());
                    }
                }
            }
            let sum = (constant + dp[31]).next_up();
            if sum.is_finite() && sum >= 0.0 {
                upper = upper.min(up(mul(sum, sum) / (4.0 * self.converted)));
                if upper < limit {
                    break;
                }
            }
        }
        upper
    }
}
fn member_orders(count: usize) -> &'static [[i8; 5]] {
    static ORDERS: OnceLock<[Vec<[i8; 5]>; 6]> = OnceLock::new();
    &ORDERS.get_or_init(|| {
        std::array::from_fn(|count| {
            let mut rows = Vec::new();
            for order in live::orders() {
                let row = order.map(|slot| if slot < count { slot as i8 } else { -1 });
                if !rows.contains(&row) {
                    rows.push(row);
                }
            }
            rows
        })
    })[count]
}
fn member_order_links(count: usize) -> &'static [(usize, usize, usize)] {
    static LINKS: OnceLock<[Vec<(usize, usize, usize)>; 6]> = OnceLock::new();
    &LINKS.get_or_init(|| {
        std::array::from_fn(|count| {
            member_orders(count)
                .iter()
                .map(|order| {
                    let mask = order
                        .iter()
                        .enumerate()
                        .fold(0usize, |mask, (event, &slot)| {
                            mask | (usize::from(slot < 0) << event)
                        });
                    if count == 0 {
                        return (0, 0, mask);
                    }
                    let event = order
                        .iter()
                        .position(|&slot| slot == count as i8 - 1)
                        .unwrap();
                    let mut previous = *order;
                    previous[event] = -1;
                    let source = member_orders(count - 1)
                        .iter()
                        .position(|order| *order == previous)
                        .unwrap();
                    (source, event, mask)
                })
                .collect()
        })
    })[count]
}
struct MemberPricingDomain<'a> {
    power: &'a [f64],
    snapshot_power: &'a [f64],
    options: &'a [Vec<i32>],
    member_bonus: &'a [f64],
    snap_bonus: &'a [f64],
}
impl MemberRewardPricing {
    fn mean(&self, members: &[usize], from: usize, floor: f64) -> f64 {
        let mut minimum = f64::INFINITY;
        for plane in &self.planes {
            let mut sum = plane.constant;
            for &member in members {
                sum = (sum + plane.mean[member]).next_up();
            }
            sum = (sum + plane.mean_suffix[from][5 - members.len()]).next_up();
            sum = (sum - (plane.mu * floor).next_down()).next_up();
            if sum.is_finite() {
                minimum = minimum.min(sum.max(0.0));
            }
        }
        up(mul(minimum, minimum) / (4.0 * self.converted))
    }
    fn sums(&self, members: &[usize], from: usize) -> Vec<f64> {
        let mut prefixes = self.prefixes.lock().unwrap();
        for count in 0..=members.len() {
            if prefixes[count]
                .as_ref()
                .is_some_and(|prefix| prefix.members == members[..count])
            {
                continue;
            }
            let mut prefix = prefixes[count].take().unwrap_or_else(|| MemberPricePrefix {
                members: Vec::with_capacity(count),
                sums: Vec::new(),
            });
            prefix.members.clear();
            prefix.members.extend_from_slice(&members[..count]);
            prefix.sums.clear();
            if count == 0 {
                prefix
                    .sums
                    .extend(self.planes.iter().map(|plane| plane.constant));
            } else {
                let parent = &prefixes[count - 1].as_ref().unwrap().sums;
                // Upward additions preserve an upper bound in the cached member order.
                for &(source, event, _) in member_order_links(count) {
                    for (plane, row) in self.planes.iter().enumerate() {
                        prefix.sums.push(
                            (parent[source * self.planes.len() + plane]
                                + row.members[members[count - 1]][event])
                                .next_up(),
                        );
                    }
                }
            }
            prefixes[count] = Some(prefix);
        }
        let prefix = &prefixes[members.len()].as_ref().unwrap().sums;
        let mut sums = Vec::with_capacity(prefix.len());
        for (order, &(_, _, mask)) in member_order_links(members.len()).iter().enumerate() {
            for (plane, row) in self.planes.iter().enumerate() {
                sums.push(
                    (prefix[order * self.planes.len() + plane] + row.suffix[from][mask]).next_up(),
                );
            }
        }
        sums
    }

    fn scores(&self, sums: &[f64], floor: f64) -> Vec<f64> {
        let count = sums.len() / self.planes.len();
        let deductions: Vec<_> = self
            .planes
            .iter()
            .map(|plane| (plane.mu * floor).next_down())
            .collect();
        let mut scores = Vec::with_capacity(count);
        for row in sums.chunks_exact(self.planes.len()) {
            let mut minimum = f64::INFINITY;
            for (&deduction, &sum) in deductions.iter().zip(row) {
                let sum = (sum - deduction).next_up();
                if sum.is_finite() {
                    minimum = minimum.min(sum.max(0.0));
                }
            }
            scores.push(up(mul(minimum, minimum) / (4.0 * self.converted)));
        }
        scores
    }
}
fn priced_roster(
    members: &[Member],
    bonuses: &[i32],
    leader: usize,
    level: i32,
    values: &[[f64; 5]],
) -> Option<Vec<usize>> {
    if !(0..=64).contains(&level) || bonuses.iter().any(|&bonus| bonus < 0) {
        return None;
    }
    let target = usize::try_from(level.checked_sub(bonuses[leader])?).ok()?;
    let mut groups = BTreeMap::<i32, Vec<usize>>::new();
    for (index, member) in members.iter().enumerate() {
        if member.card.character_id != members[leader].card.character_id
            && values[index].iter().any(|value| value.is_finite())
        {
            groups
                .entry(member.card.character_id)
                .or_default()
                .push(index);
        }
    }
    let mut table = vec![vec![(f64::NEG_INFINITY, [usize::MAX; 5]); target + 1]; 32];
    for event in 0..5 {
        let mut selected = [usize::MAX; 5];
        selected[event] = leader;
        table[1 << event][0] = (values[leader][event], selected);
    }
    for group in groups.values() {
        for mask in (0usize..31).rev() {
            for &member in group {
                let bonus = bonuses[member] as usize;
                if bonus > target {
                    continue;
                }
                for total in bonus..=target {
                    let (previous, selected) = table[mask][total - bonus];
                    if !previous.is_finite() {
                        continue;
                    }
                    let mut available = 31 ^ mask;
                    while available != 0 {
                        let event = available.trailing_zeros() as usize;
                        available &= available - 1;
                        let value = previous + values[member][event];
                        let next = mask | (1 << event);
                        if value > table[next][total].0 {
                            let mut selected = selected;
                            selected[event] = member;
                            table[next][total] = (value, selected);
                        }
                    }
                }
            }
        }
    }
    let (value, selected) = table[31][target];
    value.is_finite().then(|| selected.to_vec())
}
impl DeterministicBound {
    pub(crate) fn roster_pricing(
        &self,
        power: &[f64],
        options: &[Vec<i32>],
        members: &[Member],
        selected: Option<(usize, &[i32], i32)>,
    ) -> Option<RosterPricing> {
        let count = self.width - 1;
        let columns = count + 5;
        let mut caps: Vec<_> = power
            .iter()
            .enumerate()
            .map(|(member, &base)| {
                base + options[member]
                    .iter()
                    .map(|&snap| {
                        self.snapshot_power
                            [member * self.width + if snap < 0 { count } else { snap as usize }]
                    })
                    .fold(f64::NEG_INFINITY, f64::max)
            })
            .collect();
        caps.sort_by(|a, b| b.total_cmp(a));
        let power_cap = caps.iter().take(5).sum::<f64>();
        let coefficient =
            self.base + 5.0 * self.shared.iter().flatten().copied().fold(0.0, f64::max);
        let ratio = (coefficient / power_cap).sqrt();
        if !ratio.is_finite() || ratio <= 0.0 {
            return None;
        }
        let mut planes = Vec::new();
        let scales: &[f64] = if selected.is_some() {
            &[1.0]
        } else {
            &[1.0, 0.87, 0.75, 1.5]
        };
        for &scale in scales {
            let lambda = ratio * scale;
            let cells: Vec<Vec<_>> = power
                .iter()
                .enumerate()
                .map(|(member, &base)| {
                    options[member]
                        .iter()
                        .map(|&snap| {
                            let power = (base
                                + self.snapshot_power[member * self.width
                                    + if snap < 0 { count } else { snap as usize }])
                            .next_up();
                            let row = self.shared[self.ids
                                [member * self.width + (snap + 1) as usize][self.capacity]
                                as usize];
                            (
                                snap,
                                row.map(|value| {
                                    ((lambda * power).next_up() + (value / lambda).next_up())
                                        .next_up()
                                }),
                            )
                        })
                        .collect()
                })
                .collect();
            let mut ranked: Vec<_> = (0..power.len()).collect();
            let maxima: Vec<_> = cells
                .iter()
                .map(|choices| {
                    choices
                        .iter()
                        .flat_map(|choice| choice.1)
                        .fold(f64::NEG_INFINITY, f64::max)
                })
                .collect();
            ranked.sort_by(|&a, &b| maxima[b].total_cmp(&maxima[a]));
            let mut seen = BTreeSet::new();
            ranked.retain(|&member| {
                maxima[member].is_finite() && seen.insert(members[member].card.character_id)
            });
            ranked.truncate(5);
            if let Some((leader, bonuses, level)) = selected {
                let events: Vec<[f64; 5]> = cells
                    .iter()
                    .map(|choices| {
                        std::array::from_fn(|event| {
                            choices
                                .iter()
                                .map(|choice| choice.1[event])
                                .fold(f64::NEG_INFINITY, f64::max)
                        })
                    })
                    .collect();
                ranked = priced_roster(members, bonuses, leader, level, &events)?;
            }
            if ranked.len() != 5 {
                return None;
            }
            let matrix: Vec<_> = ranked
                .iter()
                .enumerate()
                .map(|(event, &member)| {
                    let mut row = vec![-1e100; columns];
                    for &(snap, values) in &cells[member] {
                        let value = if selected.is_some() {
                            values[event]
                        } else {
                            values.into_iter().fold(f64::NEG_INFINITY, f64::max)
                        };
                        if snap < 0 {
                            row[count..].fill(value);
                        } else {
                            row[snap as usize] = value;
                        }
                    }
                    row
                })
                .collect();
            let mut matching = crate::assignment::Scratch::default();
            matching.maximum(&matrix, columns, Some(&mut [0; 5]));
            let mut prices = matching.column_prices(columns);
            prices[count..].fill(0.0);
            let constant = add(
                up(self.base / lambda),
                prices.iter().copied().fold(0.0, add),
            );
            let rows = cells
                .iter()
                .map(|choices| {
                    let mut row = [f64::NEG_INFINITY; 5];
                    for &(snap, values) in choices {
                        let price = if snap < 0 { 0.0 } else { prices[snap as usize] };
                        for event in 0..5 {
                            row[event] = row[event].max((values[event] - price).next_up());
                        }
                    }
                    row
                })
                .collect();
            planes.push((constant, rows));
        }
        Some(RosterPricing {
            planes,
            converted: self.converted,
        })
    }
    fn prepare_prefix_mean(
        &mut self,
        members: &[usize],
        base_power: f64,
        power: f64,
        minimum: f64,
    ) {
        if self.prefix_mean.is_some() || !minimum.is_finite() {
            return;
        }
        let count = self
            .assignment_converters
            .iter()
            .filter(|&&yes| yes)
            .count()
            .min(self.capacity);
        let coefficient = members.iter().fold(self.base, |sum, &member| {
            let best = self.option_cache[member]
                .as_ref()
                .unwrap()
                .iter()
                .map(|&snap| {
                    let row = self.shared
                        [self.ids[member * self.width + (snap + 1) as usize][count] as usize];
                    up(row.into_iter().fold(0.0, add) / 5.0)
                })
                .fold(0.0_f64, f64::max);
            add(sum, best)
        });
        let ratio = (coefficient / power.max(1.0)).sqrt();
        if !ratio.is_finite() || ratio <= 0.0 {
            return;
        }
        let columns = self.width - 1 + 5;
        let mut planes = Vec::new();
        for scale in [1.0, 0.85, 1.15] {
            let lambda = ratio * scale;
            let spread = (lambda * power + coefficient / lambda) / minimum.abs().max(1.0);
            for factor in [0.0, 0.3, 1.0] {
                let mu = factor * spread;
                let mut values = vec![vec![f64::NEG_INFINITY; self.width]; 5];
                let mut matrix = vec![vec![-1e100; columns]; 5];
                for (slot, &member) in members.iter().enumerate() {
                    for &snap in self.option_cache[member].as_ref().unwrap() {
                        let column = if snap < 0 {
                            self.width - 1
                        } else {
                            snap as usize
                        };
                        let p = self.snapshot_power[member * self.width + column];
                        let coefficient = self.shared
                            [self.ids[member * self.width + (snap + 1) as usize][count] as usize];
                        let coefficient = up(coefficient.into_iter().fold(0.0, add) / 5.0);
                        let bonus = if snap < 0 {
                            0.0
                        } else {
                            self.snapshot_bonus[snap as usize]
                        };
                        let value = add(
                            add(mul(lambda, p), up(coefficient / lambda)),
                            mul(mu, bonus),
                        );
                        values[slot][(snap + 1) as usize] = value;
                        if snap < 0 {
                            matrix[slot][self.width - 1..].fill(value);
                        } else {
                            matrix[slot][snap as usize] = value;
                        }
                    }
                }
                let mut matching = crate::assignment::Scratch::default();
                matching.maximum(&matrix, columns, Some(&mut [0; 5]));
                let prices = matching.column_prices(columns);
                let price_sum = prices[..self.width - 1]
                    .iter()
                    .fold(0.0, |sum, &value| add(sum, value));
                let top = members
                    .iter()
                    .enumerate()
                    .map(|(slot, &member)| {
                        let mut choices: Vec<_> = self.option_cache[member]
                            .as_ref()
                            .unwrap()
                            .iter()
                            .map(|&snap| {
                                let price = if snap < 0 { 0.0 } else { prices[snap as usize] };
                                let value = (values[slot][(snap + 1) as usize] - price).next_up();
                                values[slot][(snap + 1) as usize] = value;
                                (snap, value)
                            })
                            .collect();
                        choices.sort_by(|a, b| b.1.total_cmp(&a.1));
                        choices.truncate(5);
                        choices
                    })
                    .collect();
                planes.push(PrefixMeanPlane {
                    constant: add(
                        add(mul(lambda, base_power), up(self.base / lambda)),
                        price_sum,
                    ),
                    mu,
                    values,
                    top,
                });
            }
        }
        self.prefix_mean = Some(PrefixMeanPricing {
            planes,
            converted: self.converted,
        });
    }
    fn snapshot_regions_possible(
        &mut self,
        members: &[usize],
        base_power: f64,
        power: f64,
        regions: &[SnapshotRegion],
        thresholds: &[RegionRequirement],
    ) -> bool {
        let count = self
            .assignment_converters
            .iter()
            .filter(|&&yes| yes)
            .count()
            .min(self.capacity);
        let coefficient = members.iter().fold(self.base, |sum, &member| {
            let best = self.option_cache[member]
                .as_ref()
                .unwrap()
                .iter()
                .map(|&snap| {
                    let row = self.shared
                        [self.ids[member * self.width + (snap + 1) as usize][count] as usize];
                    up(row.into_iter().fold(0.0, add) / 5.0)
                })
                .fold(0.0_f64, f64::max);
            add(sum, best)
        });
        let ratio = (coefficient / power.max(1.0)).sqrt();
        if !ratio.is_finite() || ratio <= 0.0 {
            return true;
        }
        let mut planes = Vec::new();
        let mut event_planes = None;
        for scale in [1.0, 0.85, 1.15] {
            let lambda = ratio * scale;
            let rows: Vec<_> = members
                .iter()
                .map(|&member| {
                    let mut row = vec![-1e100; self.width];
                    let mut zero = -1e100_f64;
                    for &snap in self.option_cache[member].as_ref().unwrap() {
                        let column = if snap < 0 {
                            self.width - 1
                        } else {
                            snap as usize
                        };
                        let p = self.snapshot_power[member * self.width + column];
                        let coefficient = self.shared
                            [self.ids[member * self.width + (snap + 1) as usize][count] as usize];
                        let coefficient = up(coefficient.into_iter().fold(0.0, add) / 5.0);
                        let value = add(mul(lambda, p), up(coefficient / lambda));
                        row[column] = value;
                        if snap < 0 || self.snapshot_bonus[snap as usize] == 0.0 {
                            zero = zero.max(value);
                        }
                    }
                    row[self.width - 1] = zero;
                    row
                })
                .collect();
            planes.push((add(mul(lambda, base_power), up(self.base / lambda)), rows));
        }
        let mut matrix = vec![vec![0.0; 5]; 5];
        let mut matching = crate::assignment::Scratch::default();
        for (region, requirement) in regions.iter().zip(thresholds) {
            let threshold = requirement.mean;
            if threshold <= 0.0 {
                return true;
            }
            let mut rejected = false;
            for (constant, rows) in &planes {
                for slot in 0..5 {
                    for column in 0..5 {
                        matrix[slot][column] = if column < region.count {
                            rows[slot][region.snaps[column] as usize]
                        } else {
                            rows[slot][self.width - 1]
                        };
                    }
                }
                let variable = matching.maximum_upper(&matrix, 5);
                if variable < -1e50 {
                    rejected = true;
                    break;
                }
                let total = add(*constant, variable);
                let upper = up(mul(total, total) / (4.0 * self.converted));
                if upper < threshold {
                    rejected = true;
                    break;
                }
            }
            if rejected {
                continue;
            }
            if requirement.floors.is_empty() {
                return true;
            }
            let event_planes = event_planes.get_or_insert_with(|| {
                let mut out = Vec::new();
                for scale in [1.0, 0.85, 1.15] {
                    let lambda = ratio * scale;
                    let rows: Vec<_> = members
                        .iter()
                        .map(|&member| {
                            let mut rows = vec![[-1e100; 5]; self.width];
                            let mut zero = [-1e100_f64; 5];
                            for &snap in self.option_cache[member].as_ref().unwrap() {
                                let column = if snap < 0 {
                                    self.width - 1
                                } else {
                                    snap as usize
                                };
                                let p = self.snapshot_power[member * self.width + column];
                                let coefficient = self.shared[self.ids
                                    [member * self.width + (snap + 1) as usize][count]
                                    as usize];
                                rows[column] = coefficient.map(|coefficient| {
                                    add(mul(lambda, p), up(coefficient / lambda))
                                });
                                if snap < 0 || self.snapshot_bonus[snap as usize] == 0.0 {
                                    for event in 0..5 {
                                        zero[event] = zero[event].max(rows[column][event]);
                                    }
                                }
                            }
                            rows[self.width - 1] = zero;
                            rows
                        })
                        .collect();
                    out.push((add(mul(lambda, base_power), up(self.base / lambda)), rows));
                }
                out
            });
            let mut orders: Vec<_> = (0..120).collect();
            orders.sort_by_cached_key(|&index| {
                let mut total = 0.0;
                for (event, &slot) in live::orders()[index].iter().enumerate() {
                    let row = &event_planes[0].1[slot];
                    let best = region.snaps[..region.count]
                        .iter()
                        .map(|&snap| row[snap as usize][event])
                        .chain((region.count < 5).then_some(row[self.width - 1][event]))
                        .fold(f64::NEG_INFINITY, f64::max);
                    total += best;
                }
                total.to_bits()
            });
            let mut scores = Vec::with_capacity(120);
            for index in orders {
                let mut events = [0; 5];
                for (event, &slot) in live::orders()[index].iter().enumerate() {
                    events[slot] = event;
                }
                let mut upper = f64::INFINITY;
                for (constant, rows) in event_planes.iter() {
                    for slot in 0..5 {
                        for column in 0..5 {
                            matrix[slot][column] = if column < region.count {
                                rows[slot][region.snaps[column] as usize][events[slot]]
                            } else {
                                rows[slot][self.width - 1][events[slot]]
                            };
                        }
                    }
                    let variable = matching.maximum_upper(&matrix, 5);
                    if variable < -1e50 {
                        upper = 0.0;
                        break;
                    }
                    let total = add(*constant, variable);
                    upper = upper.min(up(mul(total, total) / (4.0 * self.converted)));
                }
                scores.push(upper);
                if (scores.len() <= 5 || scores.len().is_multiple_of(5))
                    && requirement
                        .floors
                        .iter()
                        .any(|(curve, floor)| curve.partial_orders(&scores, 1.0, 120) < *floor)
                {
                    rejected = true;
                    break;
                }
            }
            if !rejected {
                return true;
            }
        }
        false
    }
    fn prepare_member_pricing(
        &mut self,
        leader: usize,
        characters: &[Vec<usize>],
        domain: MemberPricingDomain<'_>,
        floor: f64,
        power_cap: f64,
    ) {
        let MemberPricingDomain {
            power,
            snapshot_power,
            options,
            member_bonus,
            snap_bonus,
        } = domain;
        if self.member_pricing.is_some() {
            return;
        }
        let cap = self.base
            + 5.0
                * self
                    .shared
                    .iter()
                    .flatten()
                    .copied()
                    .fold(0.0_f64, f64::max);
        let lambda = (cap / power_cap.max(1.0)).sqrt();
        if !lambda.is_finite() || lambda <= 0.0 || !floor.is_finite() {
            return;
        }
        let spread = (lambda * power_cap + cap / lambda) / floor.abs().max(1.0);
        let count = self.width - 1;
        let columns = count + 5;
        let mut planes = Vec::new();
        for factor in [0.0, 0.2, 0.6, 1.2] {
            let mu = factor * spread;
            let mut cells = vec![Vec::<(i32, [f64; 5])>::new(); power.len()];
            let mut maxima = vec![f64::NEG_INFINITY; power.len()];
            for member in 0..power.len() {
                for &snap in &options[member] {
                    let p = add(
                        power[member],
                        snapshot_power
                            [member * self.width + if snap < 0 { count } else { snap as usize }],
                    );
                    let row = self.shared[self.ids[member * self.width + (snap + 1) as usize]
                        [self.capacity] as usize];
                    let bonus = member_bonus[member]
                        + if snap < 0 {
                            0.0
                        } else {
                            snap_bonus[snap as usize]
                        };
                    let values = row
                        .map(|value| add(add(mul(lambda, p), up(value / lambda)), mul(mu, bonus)));
                    let mean = values.iter().sum::<f64>() / 5.0;
                    maxima[member] = maxima[member].max(mean);
                    cells[member].push((snap, values));
                }
            }
            let mut representatives: Vec<_> = characters
                .iter()
                .filter_map(|group| {
                    group
                        .iter()
                        .copied()
                        .max_by(|&a, &b| maxima[a].total_cmp(&maxima[b]))
                })
                .collect();
            representatives.sort_by(|&a, &b| maxima[b].total_cmp(&maxima[a]));
            representatives.truncate(4);
            representatives.push(leader);
            let matrix: Vec<_> = representatives
                .iter()
                .map(|&member| {
                    let mut row = vec![-1e100; columns];
                    for &(snap, values) in &cells[member] {
                        let mean = values.iter().sum::<f64>() / 5.0;
                        if snap < 0 {
                            row[count..].fill(mean);
                        } else {
                            row[snap as usize] = mean;
                        }
                    }
                    row
                })
                .collect();
            let mut matching = crate::assignment::Scratch::default();
            let mut assignment = [0; 5];
            matching.maximum(&matrix, columns, Some(&mut assignment));
            let mut prices = matching.column_prices(columns);
            prices[count..].fill(0.0);
            let price_sum = prices.iter().fold(0.0, |sum, &price| add(sum, price));
            let rows: Vec<[f64; 5]> = cells
                .iter()
                .map(|choices| {
                    let mut row = [f64::NEG_INFINITY; 5];
                    for &(snap, values) in choices {
                        let price = if snap < 0 { 0.0 } else { prices[snap as usize] };
                        for event in 0..5 {
                            row[event] = row[event].max((values[event] - price).next_up());
                        }
                    }
                    row
                })
                .collect();
            let mean: Vec<_> = cells
                .iter()
                .map(|choices| {
                    choices
                        .iter()
                        .map(|&(snap, values)| {
                            let price = if snap < 0 { 0.0 } else { prices[snap as usize] };
                            (up(values.into_iter().fold(0.0, add) / 5.0) - price).next_up()
                        })
                        .fold(f64::NEG_INFINITY, f64::max)
                })
                .collect();
            let mut mean_suffix = vec![[f64::NEG_INFINITY; 6]; characters.len() + 1];
            mean_suffix[characters.len()][0] = 0.0;
            for from in (0..characters.len()).rev() {
                mean_suffix[from] = mean_suffix[from + 1];
                let value = characters[from]
                    .iter()
                    .map(|&member| mean[member])
                    .fold(f64::NEG_INFINITY, f64::max);
                for count in 1..=5 {
                    let previous = mean_suffix[from + 1][count - 1];
                    if previous.is_finite() {
                        mean_suffix[from][count] =
                            mean_suffix[from][count].max((previous + value).next_up());
                    }
                }
            }
            let mut suffix = vec![[f64::NEG_INFINITY; 32]; characters.len() + 1];
            suffix[characters.len()][0] = 0.0;
            for from in (0..characters.len()).rev() {
                suffix[from] = suffix[from + 1];
                let group: [f64; 5] = std::array::from_fn(|event| {
                    characters[from]
                        .iter()
                        .map(|&member| rows[member][event])
                        .fold(f64::NEG_INFINITY, f64::max)
                });
                for mask in 1usize..32 {
                    for (event, &value) in group.iter().enumerate() {
                        if mask & (1 << event) != 0 {
                            let previous = suffix[from + 1][mask ^ (1 << event)];
                            if previous.is_finite() {
                                suffix[from][mask] =
                                    suffix[from][mask].max((previous + value).next_up());
                            }
                        }
                    }
                }
            }
            planes.push(MemberRewardPlane {
                constant: add(up(self.base / lambda), price_sum),
                mu,
                members: rows,
                suffix,
                mean,
                mean_suffix,
            });
        }
        self.member_pricing = Some(Arc::new(MemberRewardPricing {
            planes,
            converted: self.converted,
            prefixes: Mutex::new(std::array::from_fn(|_| None)),
        }));
    }
    pub(crate) fn fixed_component(&self) -> Option<FixedComponent> {
        (self.capacity == 0).then(|| FixedComponent {
            base: self.base,
            converted: self.converted,
            width: self.width,
            rows: self
                .ids
                .iter()
                .map(|ids| self.shared[ids[0] as usize])
                .collect(),
        })
    }
    pub(crate) fn new(
        master: &Master,
        chart: &gekiso::Chart,
        members: &[Member],
        snaps: &[Snap],
    ) -> Option<Self> {
        if chart.luck {
            return None;
        }
        Self::with_envelope(
            master,
            chart,
            members,
            snaps,
            maximum_combo(master, members, &chart.setup.missions),
            ComboSource::None,
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
        if chart.luck {
            return None;
        }
        Self::with_envelope(
            master,
            chart,
            members,
            snaps,
            combo_multiplier,
            script.map_or(ComboSource::None, ComboSource::Actors),
            None,
        )
        .map(|mut bound| {
            bound.preparation = None;
            bound
        })
    }
    pub(crate) fn with_combo_trace(
        master: &Master,
        chart: &gekiso::Chart,
        members: &[Member],
        snaps: &[Snap],
        combo_multiplier: f64,
        trace: Arc<crate::reward_trace::ComboTrace>,
        prepared: Arc<BoundPreparation>,
    ) -> Option<Self> {
        if chart.luck {
            return None;
        }
        Self::with_envelope(
            master,
            chart,
            members,
            snaps,
            combo_multiplier,
            ComboSource::Prepared(Some(trace), prepared),
            None,
        )
        .map(|mut bound| {
            bound.preparation = None;
            bound
        })
    }
    fn with_preparation(
        master: &Master,
        chart: &gekiso::Chart,
        members: &[Member],
        snaps: &[Snap],
        combo: f64,
        prepared: Arc<BoundPreparation>,
    ) -> Option<Self> {
        if chart.luck {
            return None;
        }
        Self::with_envelope(
            master,
            chart,
            members,
            snaps,
            combo,
            ComboSource::Prepared(None, prepared),
            None,
        )
        .map(|mut bound| {
            bound.preparation = None;
            bound
        })
    }
    pub(crate) fn with_luck_cap(
        master: &Master,
        chart: &gekiso::Chart,
        members: &[Member],
        snaps: &[Snap],
        combo_multiplier: f64,
        script: Option<&[Performer]>,
        luck: Option<&LuckEnvelope>,
    ) -> Option<Self> {
        Self::with_envelope(
            master,
            chart,
            members,
            snaps,
            combo_multiplier,
            script.map_or(ComboSource::None, ComboSource::Actors),
            luck,
        )
        .map(|mut bound| {
            bound.preparation = None;
            bound
        })
    }
    pub(crate) fn with_luck_trace(
        master: &Master,
        chart: &gekiso::Chart,
        members: &[Member],
        snaps: &[Snap],
        combo_multiplier: f64,
        trace: Arc<crate::reward_trace::ComboTrace>,
        luck: &LuckEnvelope,
    ) -> Option<Self> {
        Self::with_envelope(
            master,
            chart,
            members,
            snaps,
            combo_multiplier,
            ComboSource::Trace(trace),
            Some(luck),
        )
        .map(|mut bound| {
            bound.preparation = None;
            bound
        })
    }
    pub(crate) fn with_luck_preparation(
        master: &Master,
        chart: &gekiso::Chart,
        members: &[Member],
        snaps: &[Snap],
        combo_multiplier: f64,
        source: (
            Option<Arc<crate::reward_trace::ComboTrace>>,
            Option<&LuckEnvelope>,
        ),
        prepared: &mut Option<Arc<BoundPreparation>>,
    ) -> Option<Self> {
        let script = match prepared.as_ref() {
            Some(prepared) => ComboSource::Prepared(source.0, Arc::clone(prepared)),
            None => source.0.map_or(ComboSource::None, ComboSource::Trace),
        };
        let mut bound = Self::with_envelope(
            master,
            chart,
            members,
            snaps,
            combo_multiplier,
            script,
            source.1,
        )?;
        *prepared = bound.preparation.take();
        Some(bound)
    }
    fn with_envelope(
        master: &Master,
        chart: &gekiso::Chart,
        members: &[Member],
        snaps: &[Snap],
        combo_multiplier: f64,
        script: ComboSource<'_>,
        luck: Option<&LuckEnvelope>,
    ) -> Option<Self> {
        let luck_cap = chart.luck;
        let luck_commands = luck.map_or(chart.play.frames.len(), |trace| trace.changes);
        if luck.is_some_and(|luck| luck.moments.is_some()) {
            let reads_luck = |row: &SkillEffect| {
                [
                    row.trigger_group,
                    row.condition_group,
                    row.release_group,
                    row.reset_group,
                ]
                .iter()
                .flat_map(|group| {
                    master
                        .condition_sets
                        .get(group)
                        .into_iter()
                        .flatten()
                        .flatten()
                })
                .filter_map(|id| master.conditions.get(id))
                .any(|condition| [7000, 7021, 4011].contains(&condition.kind))
            };
            for member in members {
                if master
                    .live_skills
                    .get(&member.card.live_skill_id)
                    .into_iter()
                    .flatten()
                    .filter(|row| row.level == member.growth.live_skill_level)
                    .any(reads_luck)
                {
                    return None;
                }
                if let Some(skill) = master.gekiso_skills.get(&member.card.gekiso_skill_id) {
                    if skill.mission_type != 2
                        && skill
                            .effects
                            .iter()
                            .filter(|row| row.level == member.growth.gekiso_skill_level)
                            .any(reads_luck)
                    {
                        return None;
                    }
                }
            }
            for snap in snaps {
                for (index, &id) in snap.card.support_skills.iter().enumerate() {
                    if master
                        .support_skills
                        .get(&id)
                        .into_iter()
                        .flatten()
                        .filter(|row| row.level == snap.skill_levels[index])
                        .any(reads_luck)
                    {
                        return None;
                    }
                }
                for (index, &id) in snap.card.gekiso_support_skills.iter().enumerate() {
                    if let Some(skill) = master.gekiso_support_skills.get(&id) {
                        if skill
                            .effects
                            .iter()
                            .filter(|row| row.level == snap.gekiso_skill_levels[index])
                            .any(|row| {
                                reads_luck(row)
                                    && !(skill.mission_type == 2
                                        && (row.kind == 2000
                                            || (11000..=11005).contains(&row.kind)
                                            || [3004, 4004].contains(&row.kind)))
                            })
                        {
                            return None;
                        }
                    }
                }
            }
        }

        if chart.accuracy.great != 0.0
            || chart.accuracy.miss_every != 0
            || chart
                .setup
                .missions
                .iter()
                .any(|&m| m != 1 && m != 3 && !(luck_cap && m == 2))
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
                        (![12000, 13000, 13002, 13003, 13004, 3004, 4004].contains(&r.kind)
                            && !(luck_cap
                                && skill.mission_type == 2
                                && (11000..=11005).contains(&r.kind)))
                            || r.kind == 12000 && r.value < 0
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
        let (trace, shared_preparation) = match script {
            ComboSource::None => (None, None),
            ComboSource::Actors(actors) => (
                Some(Arc::new(crate::reward_trace::ComboTrace::new(
                    master, chart, actors,
                )?)),
                None,
            ),
            ComboSource::Trace(trace) => (Some(trace), None),
            ComboSource::Prepared(trace, prepared) => (trace, Some(prepared)),
        };
        let fraction = chart.judge_frac.iter().copied().fold(0.0, f64::max);
        let mut prefix = vec![0.0];
        let mut all_max = 0.0;
        let mut raw_weights = Vec::new();
        let mut beta_weights = Vec::new();
        let mut rush_weights = luck
            .and_then(|luck| luck.moments.as_ref())
            .map(|_| Vec::with_capacity(chart.notes.len()));

        for i in 0..chart.notes.len() {
            let range = chart.range_of[i];
            let rank = if range < 0 {
                1.0
            } else {
                add(1.0, up(chart.range_percent[range as usize] as f64 / 100.0))
            };
            let gk = if luck_cap
                && ranges.iter().enumerate().any(|(range, &(start, end))| {
                    chart.setup.missions[range] == 2
                        && start <= chart.times[i]
                        && chart.times[i] <= end
                }) {
                if luck.is_some_and(|trace| !trace.rush[i]) {
                    1.0
                } else {
                    up(1.0 + master.live.gekiso.rush_percent.max(0) as f64 / 100.0)
                }
            } else if let Some(trace) = &trace {
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
            let without_gk = [
                chart.difficulty_factor,
                chart.note_frac[i],
                chart.combo_factor[i],
                rank,
            ]
            .into_iter()
            .fold(master.live.adjustment, mul);
            all_max = add(all_max, mul(mul(without_gk, gk), fraction));
            let mut mean_gk = gk;
            if let Some(moments) = luck.and_then(|luck| luck.moments.as_ref()) {
                let probability = |count: usize| {
                    if count == 0 {
                        0.0
                    } else {
                        up(count as f64 / moments.samples as f64)
                    }
                };
                let is_luck = ranges.iter().enumerate().any(|(range, &(start, end))| {
                    chart.setup.missions[range] == 2
                        && start <= chart.times[i]
                        && chart.times[i] <= end
                });
                let factor = up(master.live.gekiso.rush_percent.max(0) as f64 / 100.0);
                if is_luck {
                    mean_gk = add(1.0, mul(factor, probability(moments.rush[i])));
                }
                let joint = add(
                    probability(moments.support[i]),
                    mul(factor, probability(moments.joint[i])),
                );
                rush_weights.as_mut().unwrap().push(mul(
                    mul(without_gk, joint),
                    chart.judge_frac[chart.judgements[i]],
                ));
            }
            let raw = mul(without_gk, mean_gk);
            raw_weights.push(raw);
            let beta = mul(raw, chart.judge_frac[chart.judgements[i]]);
            prefix.push(add(*prefix.last().unwrap(), beta));
            beta_weights.push(beta);
        }
        let all = *prefix.last()?;
        let mut windows = CumulativeWindows::new(
            master,
            chart,
            members,
            snaps,
            beta_weights,
            ranges,
            ComboEnvelope {
                multiplier: combo_multiplier,
                trace,
                rush: luck.map(|trace| trace.support.clone()),
                rush_weights,
                prepared: shared_preparation.as_ref().map(Arc::clone),
                rush_commands: luck_commands.max(1),
            },
        )?;
        let mut support_gains = Vec::new();
        let prepared = match shared_preparation {
            Some(prepared) => prepared,
            None => Arc::new(BoundPreparation::new(
                master,
                chart,
                members,
                snaps,
                ranges,
                Arc::clone(&windows.layout),
            )?),
        };
        let mut profiles = Vec::new();
        if let Some(trace) = &windows.trace {
            let programs: Vec<_> = prepared
                .slots
                .iter()
                .map(|slot| slot.score_rows.as_slice())
                .collect();
            trace.prepare_supports(chart, &programs);
        }
        let mut template_commands = Vec::new();
        let mut max_actions = 0;
        let mut max_absolute = 0.0_f64;
        let mut conversions = Vec::new();
        let mut conversion_regions = Vec::new();
        let mut ordinary_shapes = Vec::new();
        for slot in &prepared.slots {
            template_commands.push(windows.commands(&slot.score_rows));
            support_gains.push(windows.gain(chart, &slot.score_rows));
            let mut row = [0.0; 5];
            for &(delta, ref intervals) in &slot.windows {
                for (event, &(start, end)) in intervals.iter().enumerate() {
                    let weight = (prefix[end] - prefix[start]).max(0.0) + 32.0 * f64::EPSILON * all;
                    row[event] = add(row[event], mul(delta, up(weight)));
                }
            }
            let (actions, absolute) = slot.normal;
            max_actions = max_actions.max(actions);
            max_absolute = max_absolute.max(absolute);
            ordinary_shapes.push(&slot.shape);
            profiles.push(row);
            conversions.push(slot.converts);
            conversion_regions.push(&slot.regional_converts);
        }
        let normal_bounds: Vec<_> = prepared
            .row_ids
            .iter()
            .map(|&id| prepared.slots[id as usize].normal)
            .collect();
        let mut positives = Vec::new();
        let mut commands = Vec::new();
        for rows in prepared.row_ids.chunks(snaps.len() + 1) {
            let mut member_positive = 0.0_f64;
            let mut member_commands = [0usize; 6];
            for &id in rows {
                member_positive = member_positive.max(prepared.slots[id as usize].positive);
                for (c, value) in member_commands.iter_mut().enumerate() {
                    *value = (*value).max(template_commands[id as usize][c]);
                }
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
            let count = counts
                .into_iter()
                .take(5)
                .try_fold(0usize, |sum, value| sum.checked_add(value))?;
            let operations = factor_operations(chart, ranges).checked_mul(count)?;
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
        let width = snaps.len() + 1;
        let capacity = prepared.capacity;
        let (prefix_gain, regional_prefix) = if capacity == 0 {
            (vec![0.0], vec![vec![0.0]; ranges.len()])
        } else {
            let durations: Vec<_> = ordinary_shapes
                .iter()
                .flat_map(|shape| shape.iter())
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
            (prefix_gain, regional_prefix)
        };
        let mut result = Self {
            combo_multiplier,
            capacity,
            coefficient_limit: 4096,
            duals: Vec::new(),
            dual_witnesses: [0; 6],
            dual_paths: Vec::new(),
            dual_offers: vec![Vec::new(); members.len()],
            option_cache: vec![None; members.len()],
            fixed_score_options: vec![None; members.len()],
            option_minimum_conversion: vec![f64::INFINITY; members.len()],
            option_envelopes: vec![[0; 6]; members.len()],
            option_converters: vec![false; members.len()],
            score_frontiers: vec![Vec::new(); members.len()],
            pricing_frontiers: vec![vec![None; capacity + 1]; members.len()],
            sparse_matrix: std::array::from_fn(|_| Vec::new()),
            snapshot_power: Arc::new(Vec::new()),
            snapshot_bonus: Arc::new(Vec::new()),
            base: mul(
                add(add(all, mul(all, errors[0])), mul(all_max, residual)),
                rounding,
            ),
            converted: chart.full.converted as f64,
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
            joint_rewards: None,
            joint_mean: None,
            prefix_mean: None,
            conversion_extras: Vec::new(),
            conversion_extra_cap: mul(*prefix_gain.last().unwrap(), rounding),
            converter_options: None,
            physical_matching: crate::assignment::Scratch::default(),
            physical_matrix: Vec::new(),
            preparation: Some(Arc::clone(&prepared)),
            member_pricing: None,
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
            let extra = regions.as_ref().map_or_else(
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
            }
            result.ids.push(ids);
            result.converters.push(converts > 0.0);
        }
        let template_ids = std::mem::take(&mut result.ids);
        let template_converters = std::mem::take(&mut result.converters);
        let template_gains = std::mem::take(&mut result.baseline_gains);
        let template_extras = std::mem::take(&mut result.conversion_extras);
        let physical_rows = prepared.row_ids.len();
        result.ids.reserve(physical_rows);
        result.converters.reserve(physical_rows);
        result.baseline_gains.reserve(physical_rows);
        result.conversion_extras.reserve(physical_rows);
        for &id in &prepared.row_ids {
            let id = id as usize;
            let rows = template_ids[id];
            result.ids.push(rows);
            result.converters.push(template_converters[id]);
            result.baseline_gains.push(template_gains[id]);
            result.conversion_extras.push(template_extras[id]);
        }
        for templates in &prepared.member_rows {
            let mut ids = [0; 6];
            for (c, id) in ids.iter_mut().enumerate() {
                let mut row = [0.0_f64; 5];
                for &template in templates {
                    let values = result.shared[template_ids[template as usize][c] as usize];
                    for (event, value) in row.iter_mut().enumerate() {
                        *value = value.max(values[event]);
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
    fn member_snapshots(&mut self, allowed: &[bool]) {
        for member in 0..self.global.len() {
            let mut ids = [0; 6];
            for (c, id) in ids.iter_mut().enumerate() {
                let mut row = [0.0_f64; 5];
                for (snap, &yes) in allowed.iter().enumerate() {
                    if !yes {
                        continue;
                    }
                    let values = self.shared[self.ids[member * self.width + snap][c] as usize];
                    for (event, value) in row.iter_mut().enumerate() {
                        *value = value.max(values[event]);
                    }
                }
                *id = self.intern(row);
            }
            self.global[member] = ids;
        }
    }
    pub(crate) fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        self.member_pricing = None;
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
        self.joint_rewards = None;
        self.joint_mean = None;
        self.prefix_mean = None;
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
                self.dual_paths.push(vec![[0, 1, 2, 3, 4]; params.len()]);
                self.duals.push(params);
            }
        }
        for (slot, &m) in members.iter().enumerate() {
            if self.option_cache[m].as_ref() != Some(&options[slot]) {
                self.pricing_frontiers[m].fill(None);
                let mut raw_envelopes = [[0.0; 5]; 6];
                let mut offers = Vec::new();
                for (c, params) in self.duals.iter().enumerate() {
                    let mut envelope = [0.0_f64; 5];
                    let cells = vec![None; params.len()];
                    for &snap in &options[slot] {
                        let row =
                            self.shared[self.ids[m * self.width + (snap + 1) as usize][c] as usize];
                        for (event, value) in envelope.iter_mut().enumerate() {
                            *value = value.max(row[event]);
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
                self.option_minimum_conversion[m] = options[slot]
                    .iter()
                    .map(|&snap| self.conversion_extras[m * self.width + (snap + 1) as usize])
                    .fold(f64::INFINITY, f64::min);
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
        let mut possible: Vec<_> = targets
            .iter()
            .map(|target| (true, (2.0 * target.product.max(0.0).sqrt()).next_down()))
            .collect();
        let witness = self.dual_witnesses[c];
        for step in 0..self.duals[c].len() {
            let index = if step == 0 {
                witness
            } else if step <= witness {
                step - 1
            } else {
                step
            };
            let param = self.duals[c][index];
            for &member in &members[from..] {
                if self.dual_offers[member][c][index].is_some() {
                    continue;
                }
                let mut envelope = [f64::NEG_INFINITY; 5];
                for &snap in self.option_cache[member].as_ref().unwrap() {
                    let row = self.shared
                        [self.ids[member * self.width + (snap + 1) as usize][c] as usize];
                    let column = if snap < 0 {
                        self.width - 1
                    } else {
                        snap as usize
                    };
                    let power = self.snapshot_power[member * self.width + column];
                    let bonus = if snap < 0 {
                        0.0
                    } else {
                        self.snapshot_bonus[snap as usize]
                    };
                    let fixed =
                        ((param.lambda * power).next_up() + (param.mu * bonus).next_up()).next_up();
                    for (event, value) in envelope.iter_mut().enumerate() {
                        *value =
                            value.max((fixed + (row[event] / param.lambda).next_up()).next_up());
                    }
                }
                self.dual_offers[member][c][index] = Some(envelope);
            }
            let constant = (((param.lambda * power).next_up()
                + (self.base / param.lambda).next_up())
            .next_up()
                + (param.mu * bonus).next_up())
            .next_up();
            let path = self.dual_paths[c][index];
            let witness = path.iter().enumerate().fold(0.0, |sum, (slot, &event)| {
                let member = members[slot];
                let value = if slot < from {
                    let snap = assigned[slot];
                    let row = self.shared
                        [self.ids[member * self.width + (snap + 1) as usize][c] as usize];
                    let power = self.snapshot_power[member * self.width
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
                    let fixed =
                        ((param.lambda * power).next_up() + (param.mu * bonus).next_up()).next_up();
                    (fixed + (row[event as usize] / param.lambda).next_up()).next_up()
                } else {
                    self.dual_offers[member][c][index].unwrap()[event as usize]
                };
                (sum + value).next_up()
            });
            let witness = (constant + witness).next_up();
            if targets
                .iter()
                .zip(&possible)
                .all(|(target, &(possible, root))| {
                    let required = (root + (param.mu * target.bonus).next_down()).next_down();
                    !possible || witness >= required
                })
            {
                continue;
            }
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
                    rows[slot] = self.dual_offers[m][c][index].unwrap();
                }
            }
            let mut dp = [f64::NEG_INFINITY; 32];
            let mut choices = [0u8; 32];
            dp[0] = 0.0;
            for mask in 1usize..32 {
                let slot = mask.count_ones() as usize - 1;
                choices[mask] = mask.trailing_zeros() as u8;
                for event in 0..5 {
                    if mask & (1 << event) != 0 {
                        let value = (dp[mask ^ (1 << event)] + rows[slot][event]).next_up();
                        if value > dp[mask] {
                            choices[mask] = event as u8;
                        }
                        dp[mask] = dp[mask].max(value);
                    }
                }
            }
            let mut mask = 31usize;
            for slot in (0..5).rev() {
                let event = choices[mask];
                self.dual_paths[c][index][slot] = event;
                mask ^= 1 << event;
            }
            let upper = (constant + dp[31]).next_up();
            for (target, (possible, root)) in targets.iter().zip(&mut possible) {
                let required = (*root + (param.mu * target.bonus).next_down()).next_down();
                if upper < required {
                    *possible = false;
                }
            }
            if possible.iter().all(|&(possible, _)| !possible) {
                self.dual_witnesses[c] = index;
                return false;
            }
        }
        true
    }
    pub(crate) fn prepare_score_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.prepare_score_rows(members, options, true);
    }
    pub(crate) fn prepare_fixed_score_assignment(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
    ) {
        self.prepare_score_rows(members, options, false);
    }
    fn prepare_score_rows(&mut self, members: &[usize], options: &[Vec<i32>], price: bool) {
        self.joint_scores = None;
        self.joint_rewards = None;
        self.joint_mean = None;
        self.prefix_mean = None;
        self.converter_options = None;
        for (slot, &member) in members.iter().enumerate() {
            self.pricing_frontiers[member].fill(None);
            if !price
                && self.fixed_score_options[member]
                    .as_ref()
                    .is_some_and(|row| row.options == options[slot])
            {
                let row = self.fixed_score_options[member].as_ref().unwrap();
                self.option_envelopes[member] = row.envelopes;
                self.option_normal[member] = row.normal;
                self.option_converters[member] = row.converter;
                self.option_minimum_conversion[member] = row.minimum_conversion;
                self.option_cache[member] = Some(row.options.clone());
                self.score_frontiers[member].clear();
            } else if self.option_cache[member].as_ref() != Some(&options[slot])
                || price && self.score_frontiers[member].is_empty()
            {
                let mut rows = [[0.0_f64; 5]; 6];
                let mut normal = (0usize, 0.0_f64);
                let mut frontiers: PowerFrontiers = if price {
                    (0..=self.capacity)
                        .map(|_| std::array::from_fn(|_| Vec::new()))
                        .collect()
                } else {
                    Vec::new()
                };
                for &snap in &options[slot] {
                    let bound = self.normal_bounds[member * self.width + (snap + 1) as usize];
                    normal = (normal.0.max(bound.0), normal.1.max(bound.1));
                    for (c, row) in rows.iter_mut().enumerate().take(self.capacity + 1) {
                        let values = self.shared
                            [self.ids[member * self.width + (snap + 1) as usize][c] as usize];
                        for (event, value) in row.iter_mut().enumerate() {
                            *value = value.max(values[event]);
                            if price {
                                let power = self.snapshot_power[member * self.width
                                    + if snap < 0 {
                                        self.width - 1
                                    } else {
                                        snap as usize
                                    }];
                                frontiers[c][event].push((power, values[event]));
                            }
                        }
                        if price {
                            let power = self.snapshot_power[member * self.width
                                + if snap < 0 {
                                    self.width - 1
                                } else {
                                    snap as usize
                                }];
                            frontiers[c][5]
                                .push((power, up(values.into_iter().fold(0.0, add) / 5.0)));
                        }
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
                if price {
                    self.score_frontiers[member] = frontiers;
                } else {
                    self.score_frontiers[member].clear();
                }
                self.option_envelopes[member] = rows.map(|row| self.intern(row));
                self.option_normal[member] = normal;
                self.option_converters[member] = options[slot]
                    .iter()
                    .any(|&snap| self.converters[member * self.width + (snap + 1) as usize]);
                self.option_minimum_conversion[member] = options[slot]
                    .iter()
                    .map(|&snap| self.conversion_extras[member * self.width + (snap + 1) as usize])
                    .fold(f64::INFINITY, f64::min);
                self.option_cache[member] = Some(options[slot].clone());
                if !price && options[slot].len() > 1 {
                    self.fixed_score_options[member] = Some(FixedScoreOptions {
                        options: options[slot].clone(),
                        envelopes: self.option_envelopes[member],
                        normal: self.option_normal[member],
                        converter: self.option_converters[member],
                        minimum_conversion: self.option_minimum_conversion[member],
                    });
                }
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
    fn prepare_pricing_frontiers(&mut self, members: &[usize], count: usize) {
        for &member in members {
            if self.pricing_frontiers[member][count].is_some() {
                continue;
            }
            let cells: Vec<_> = self.option_cache[member]
                .as_ref()
                .unwrap()
                .iter()
                .map(|&snap| {
                    let power = self.snapshot_power[member * self.width
                        + if snap < 0 {
                            self.width - 1
                        } else {
                            snap as usize
                        }];
                    let row = self.shared
                        [self.ids[member * self.width + (snap + 1) as usize][count] as usize];
                    let bonus = if snap < 0 {
                        0.0
                    } else {
                        self.snapshot_bonus[snap as usize]
                    };
                    (snap, power, row, bonus)
                })
                .collect();
            let frontiers = std::array::from_fn(|event| {
                let mut order: Vec<_> = (0..cells.len()).collect();
                order.sort_unstable_by(|&a, &b| {
                    cells[b]
                        .1
                        .total_cmp(&cells[a].1)
                        .then_with(|| cells[b].2[event].total_cmp(&cells[a].2[event]))
                        .then_with(|| cells[b].3.total_cmp(&cells[a].3))
                        .then_with(|| cells[a].0.cmp(&cells[b].0))
                });
                let mut kept = Vec::<usize>::new();
                for index in order {
                    let own = cells[index];
                    let witnesses = kept
                        .iter()
                        .filter(|&&other| {
                            let other = cells[other];
                            other.0 >= 0
                                && other.1 >= own.1
                                && other.2[event] >= own.2[event]
                                && other.3 >= own.3
                        })
                        .take(5)
                        .count();
                    // Four other slots can occupy at most four of these
                    // retained physical columns; the fifth remains available.
                    if witnesses < 5 {
                        kept.push(index);
                    }
                }
                kept.into_iter().map(|index| cells[index].0).collect()
            });
            self.pricing_frontiers[member][count] = Some(frontiers);
        }
    }
    pub(crate) fn joint_score_caps(
        &mut self,
        members: &[usize],
        snapshot_power: &[f64],
        base_power: f64,
        minimum_bonus: f64,
        floors: &[(crate::event::RewardCurve, f64)],
    ) -> Arc<Vec<f64>> {
        self.joint_reward_caps(
            members,
            snapshot_power,
            base_power,
            minimum_bonus,
            floors,
            false,
        )
    }
    fn joint_reward_caps(
        &mut self,
        members: &[usize],
        snapshot_power: &[f64],
        base_power: f64,
        minimum_bonus: f64,
        floors: &[(crate::event::RewardCurve, f64)],
        mean: bool,
    ) -> Arc<Vec<f64>> {
        if !mean {
            if let Some((power, caps)) = &self.joint_scores {
                if *power == base_power.to_bits() {
                    return Arc::clone(caps);
                }
            }
        }
        let count = self.width - 1;
        let columns = count + 5;
        let c = self
            .assignment_converters
            .iter()
            .filter(|&&yes| yes)
            .count()
            .min(self.capacity);
        self.prepare_pricing_frontiers(members, c);
        let mut matching = std::mem::take(&mut self.physical_matching);
        let mut weights = std::mem::take(&mut self.sparse_matrix);
        let offers: JointRewardOffers = std::array::from_fn(|slot| {
            let member = members[slot];
            std::array::from_fn(|event| {
                let choices = if mean {
                    self.option_cache[member].as_ref().unwrap()
                } else {
                    &self.pricing_frontiers[member][c].as_ref().unwrap()[event]
                };
                let mut offers = Vec::with_capacity(choices.len() + 4);
                for &snap in choices {
                    let power = snapshot_power
                        [member * self.width + if snap < 0 { count } else { snap as usize }];
                    let profile = self.shared
                        [self.ids[member * self.width + (snap + 1) as usize][c] as usize];
                    let coefficient = if mean {
                        up(profile.into_iter().fold(0.0, add) / 5.0)
                    } else {
                        profile[event]
                    };
                    let bonus = if snap < 0 {
                        0.0
                    } else {
                        self.snapshot_bonus[snap as usize]
                    };
                    if snap < 0 {
                        offers.extend(
                            (count..columns).map(|column| (column, power, coefficient, bonus)),
                        );
                    } else {
                        offers.push((snap as usize, power, coefficient, bonus));
                    }
                }
                offers
            })
        });
        let fill = |weights: &mut [Vec<(usize, f64)>; 5],
                    events: &[usize; 5],
                    mode: u8,
                    lambda: f64,
                    mu: f64| {
            let inverse = up(1.0 / lambda);
            for (slot, row) in weights.iter_mut().enumerate() {
                row.clear();
                let offers = &offers[slot][events[slot]];
                match mode {
                    0 => row.extend(offers.iter().map(|&(column, power, _, _)| (column, power))),
                    1 => row.extend(
                        offers
                            .iter()
                            .map(|&(column, _, coefficient, _)| (column, coefficient)),
                    ),
                    _ if mu == 0.0 => {
                        row.extend(offers.iter().map(|&(column, power, coefficient, _)| {
                            (column, add(mul(lambda, power), mul(coefficient, inverse)))
                        }))
                    }
                    _ => row.extend(offers.iter().map(|&(column, power, coefficient, bonus)| {
                        (
                            column,
                            add(
                                add(mul(lambda, power), mul(coefficient, inverse)),
                                mul(mu, bonus),
                            ),
                        )
                    })),
                }
            }
        };
        fill(&mut weights, &[0; 5], 0, 1.0, 0.0);
        let pcap = add(base_power, matching.sparse_upper(&weights, columns));
        let mut caps = Vec::with_capacity(120);
        let mut reward_rows = Vec::with_capacity(120);
        let mut orders = std::borrow::Cow::Borrowed(if mean {
            &live::orders()[..1]
        } else {
            live::orders()
        });
        if !floors.is_empty() {
            orders.to_mut().sort_by_cached_key(|order| {
                order
                    .iter()
                    .enumerate()
                    .fold(self.base, |sum, (event, &slot)| {
                        add(sum, self.shared[self.assignment[c][slot] as usize][event])
                    })
                    .to_bits()
            });
        }
        for order in orders.iter() {
            let mut events = [0; 5];
            for (event, &slot) in order.iter().enumerate() {
                events[slot] = event;
            }
            fill(&mut weights, &events, 1, 1.0, 0.0);
            let ccap = add(self.base, matching.sparse_upper(&weights, columns));
            if pcap <= 0.0 || ccap <= 0.0 || !pcap.is_finite() || !ccap.is_finite() {
                caps.push(f64::INFINITY);
                reward_rows.push(JointRewardRow {
                    ceiling: f64::INFINITY,
                    prices: Vec::new(),
                });
                continue;
            }
            let mut upper = up(mul(pcap, ccap) / self.converted);
            let mut ceiling = upper;
            let mut prices = Vec::new();
            let ratio = (ccap / pcap).sqrt();
            for scale in [1.0, 0.85, 1.15] {
                let lambda = ratio * scale;
                fill(&mut weights, &events, 2, lambda, 0.0);
                let variable = matching.sparse_upper(&weights, columns);
                let total = add(
                    add(mul(lambda, base_power), up(self.base / lambda)),
                    variable,
                );
                let cap = up(mul(total, total) / (4.0 * self.converted));
                ceiling = ceiling.min(cap);
                upper = upper.min(cap);
                if scale == 1.0 && minimum_bonus.is_finite() && minimum_bonus > 0.0 {
                    let spread = total / minimum_bonus;
                    for factor in [0.1, 0.4] {
                        let mu = factor * spread;
                        fill(&mut weights, &events, 2, lambda, mu);
                        let variable = matching.sparse_upper(&weights, columns);
                        let constant = add(
                            add(mul(lambda, base_power), up(self.base / lambda)),
                            variable,
                        );
                        prices.push((constant, mu));
                        let total = (add(
                            add(mul(lambda, base_power), up(self.base / lambda)),
                            variable,
                        ) - (mu * minimum_bonus).next_down())
                        .next_up()
                        .max(0.0);
                        upper = upper.min(up(mul(total, total) / (4.0 * self.converted)));
                    }
                }
            }
            caps.push(upper);
            reward_rows.push(JointRewardRow { ceiling, prices });
            if !floors.is_empty()
                && (caps.len() <= 5 || caps.len().is_multiple_of(5))
                && floors
                    .iter()
                    .any(|(curve, floor)| curve.partial_orders(&caps, 1.0, 120) < *floor)
            {
                caps.resize(120, f64::INFINITY);
                break;
            }
        }
        caps.sort_by(f64::total_cmp);
        self.physical_matching = matching;
        self.sparse_matrix = weights;
        let caps = Arc::new(caps);
        if mean {
            self.joint_mean = Some(Arc::new(JointRewardPricing {
                rows: reward_rows,
                converted: self.converted,
            }));
        } else {
            self.joint_scores = Some((base_power.to_bits(), Arc::clone(&caps)));
            self.joint_rewards = Some(Arc::new(JointRewardPricing {
                rows: reward_rows,
                converted: self.converted,
            }));
        }
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
    fn repeated_order_floor(&mut self, mut ids: [u32; 5]) -> Option<(f64, usize)> {
        ids.sort_unstable();
        let mut count = 1;
        let mut from = 0;
        while from < 5 {
            let mut end = from + 1;
            while end < 5 && ids[end] == ids[from] {
                end += 1;
            }
            count *= [1, 1, 2, 6, 24, 120][end - from];
            from = end;
        }
        (count > 1).then(|| {
            (
                self.score_coefficient(ids, crate::objectives::Criterion::Min),
                count,
            )
        })
    }
    fn assignment_order_floor(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
    ) -> Option<(f64, usize)> {
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
        let ids = std::array::from_fn(|slot| {
            if slot < from {
                self.ids[members[slot] * self.width + (assigned[slot] + 1) as usize][c]
            } else {
                self.assignment[c][slot]
            }
        });
        self.repeated_order_floor(ids)
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
        bounds: (Totals, f64),
        criterion: crate::objectives::Criterion,
    ) -> f64 {
        use crate::objectives::Criterion;
        let (totals, cutoff) = bounds;
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
            if upper < cutoff {
                break;
            }
        }
        upper
    }
    pub(crate) fn physical_score_ceiling(
        &mut self,
        members: &[usize],
        base_power: f64,
        totals: Totals,
        criterion: crate::objectives::Criterion,
    ) -> f64 {
        self.required_physical_score_ceiling(members, base_power, totals, criterion, None)
    }
    pub(crate) fn required_physical_score_ceiling(
        &mut self,
        members: &[usize],
        base_power: f64,
        totals: Totals,
        criterion: crate::objectives::Criterion,
        required: Option<&[Vec<bool>]>,
    ) -> f64 {
        let count = self
            .assignment_converters
            .iter()
            .filter(|&&value| value)
            .count()
            .min(self.capacity);
        let ratio = ((self.base + totals.skill) / totals.power.max(1.0)).sqrt();
        if !ratio.is_finite() || ratio <= 0.0 {
            return f64::INFINITY;
        }
        let columns = self.width - 1 + 5;
        self.physical_matrix.resize_with(5, Vec::new);
        for row in &mut self.physical_matrix {
            row.resize(columns, -1e100);
        }
        let mut upper = f64::INFINITY;
        for scale in [1.0, 0.75, 1.5] {
            let lambda = ratio * scale;
            for (slot, &member) in members.iter().enumerate() {
                let row = &mut self.physical_matrix[slot];
                row.fill(-1e100);
                for &snap in self.option_cache[member].as_ref().unwrap() {
                    let power = self.snapshot_power[member * self.width
                        + if snap < 0 {
                            self.width - 1
                        } else {
                            snap as usize
                        }];
                    let profile = self.shared
                        [self.ids[member * self.width + (snap + 1) as usize][count] as usize];
                    let value = if criterion == crate::objectives::Criterion::Max {
                        profile.into_iter().fold(f64::NEG_INFINITY, f64::max)
                    } else {
                        up(profile.into_iter().fold(0.0, add) / 5.0)
                    };
                    let column = if snap < 0 {
                        self.width - 1 + slot
                    } else {
                        snap as usize
                    };
                    row[column] = add(mul(lambda, power), up(value / lambda));
                }
            }
            let mut variable = self
                .physical_matching
                .maximum_upper(&self.physical_matrix, columns);
            if let Some(required) = required {
                let highs: [f64; 5] = std::array::from_fn(|slot| {
                    self.physical_matrix[slot]
                        .iter()
                        .copied()
                        .fold(f64::NEG_INFINITY, f64::max)
                });
                let mut forced = f64::NEG_INFINITY;
                for (slot, flags) in required.iter().enumerate() {
                    let mut value = self.physical_matrix[slot]
                        .iter()
                        .zip(flags)
                        .filter_map(|(&value, &yes)| yes.then_some(value))
                        .fold(f64::NEG_INFINITY, f64::max);
                    if value < -1e50 {
                        continue;
                    }
                    for (other, &high) in highs.iter().enumerate() {
                        if other != slot {
                            value = add(value, high);
                        }
                    }
                    forced = forced.max(value);
                }
                variable = variable.min(forced);
            }
            if variable < -1e50 {
                return f64::NEG_INFINITY;
            }
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
        if self.capacity == 0 {
            return 0.0;
        }
        let mut minimum = 0.0_f64;
        for (slot, &member) in members.iter().enumerate() {
            let extra = if slot < from {
                self.conversion_extras[member * self.width + (assigned[slot] + 1) as usize]
            } else {
                self.option_minimum_conversion[member]
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
        let id = self.ids[member * self.width + (snap + 1) as usize][self.capacity];
        let row = &self.shared[id as usize];
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
type IntegerGaugeKey = ([(i32, i32, i64); 5], [u32; 5], usize, usize);
#[derive(Clone, PartialEq, Eq, Hash)]
enum GaugeCoreKey {
    Integer(IntegerGaugeKey),
    Phases(String),
}
type SettlementKey = (Vec<usize>, Vec<i32>, i32, Option<(u64, u64)>);
type AssignmentCertificate = ([usize; 5], [u32; 5]);
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type SharedRewardKey = ([usize; 5], [u128; 5]);
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
pub(crate) struct SharedRewardProofs {
    values: HashMap<SharedRewardKey, (f64, f64)>,
    order: VecDeque<SharedRewardKey>,
}
#[cfg(not(target_arch = "wasm32"))]
impl SharedRewardProofs {
    pub(crate) fn get(&self, key: &SharedRewardKey) -> Option<(f64, f64)> {
        self.values.get(key).copied()
    }
    pub(crate) fn record(&mut self, key: SharedRewardKey, power: f64, limit: f64) {
        if let Some(old) = self.values.get_mut(&key) {
            if power > old.0 || power == old.0 && limit < old.1 {
                *old = (power, limit);
            }
            return;
        }
        if self.values.len() >= 8192 {
            if let Some(old) = self.order.pop_front() {
                self.values.remove(&old);
            }
        }
        self.order.push_back(key);
        self.values.insert(key, (power, limit));
    }
}
#[derive(Default)]
pub(crate) struct SettlementCache {
    values: HashMap<SettlementKey, (f64, serde_json::Value)>,
    order: VecDeque<SettlementKey>,
}

struct RewardContext {
    simulation: Context,
    projection_totals: OnceLock<(f64, f64)>,
    judgement_mask: u8,
    score_projection: bool,
    rush_support: Vec<bool>,
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
type JustPatternKey = (usize, usize, [i32; 5]);
struct JustRewardOffers {
    offers: [Vec<crate::just_bound::Offer>; 5],
    powers: Vec<(i32, f64)>,
    bonuses: Vec<(i32, f64)>,
    other_bonuses: Vec<(i32, f64)>,
}
type JustWeightKey = (u64, u64, [[u64; 5]; 5]);
struct JustRewardTables {
    weight_orders: HashMap<JustWeightKey, Arc<Vec<f64>>>,
    weight_order: VecDeque<(JustWeightKey, usize)>,
    weight_bytes: usize,
    master: Master,
    converters: Vec<Option<Arc<crate::just_bound::Converter>>>,
    components: HashMap<usize, Arc<FixedComponent>>,
    domains: HashMap<Vec<i32>, usize>,
    offers: HashMap<(usize, usize, usize), Arc<JustRewardOffers>>,
    order: VecDeque<((usize, usize, usize), usize)>,
    bytes: usize,
}
type EmptyLuckKey = (Vec<usize>, Vec<Vec<i32>>, i32, u64);
type WitnessTeamKey = (Vec<usize>, Vec<i32>);

#[derive(Clone, Copy)]
enum MemberDominance {
    Different,
    Equivalent,
    Stronger,
}
struct RewardGrid {
    maximum: i64,
    minimum: i64,
    divisor: i64,
    losses: Vec<usize>,
    coins: Vec<u8>,
    reachable: Vec<u32>,
}
pub struct RewardObjective<'a> {
    pub(crate) live_floor: bool,
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
    raw_slots: Option<Arc<skills::Prepared>>,
    deterministic: Option<DeterministicBound>,
    empty_luck: Option<crate::gekiso_objective::EmptyLuckPrograms>,
    empty_options: Option<Vec<Vec<i32>>>,
    empty_scores: HashMap<EmptyLuckKey, Option<Vec<f64>>>,
    empty_score_order: VecDeque<EmptyLuckKey>,
    witness_candidates: HashMap<WitnessTeamKey, Vec<(i32, f64)>>,
    witness_gaps: BTreeMap<i32, f64>,
    empty_root_bonus: Option<f64>,
    empty_upper: Option<f64>,
    lottery_support: Vec<(usize, i32)>,
    witness_candidate_order: VecDeque<WitnessTeamKey>,
    witness_seeds: Vec<i32>,
    combo_models: HashMap<ComboModelKey, DeterministicBound>,
    combo_scripts: Option<crate::reward_trace::Scripts>,
    combo_traces: HashMap<String, Arc<crate::reward_trace::ComboTrace>>,
    combo_trace_order: VecDeque<String>,
    combo_model_order: VecDeque<ComboModelKey>,
    assignment_combo: Option<ComboModelKey>,
    assignment_power: f64,
    assignment_certificate: Option<AssignmentCertificate>,
    #[cfg(not(target_arch = "wasm32"))]
    assignment_shared_certificate: Option<SharedRewardKey>,
    certificate_domains: HashMap<Vec<i32>, u32>,
    assignment_certificates: HashMap<AssignmentCertificate, (f64, f64)>,
    certificate_order: VecDeque<AssignmentCertificate>,
    assignment_primary: f64,
    power_estimate: f64,
    snapshot_power: Arc<Vec<f64>>,
    primary_snap_bonus: Arc<Vec<f64>>,
    primary_member_bonus: Vec<f64>,
    primary_bonus_totals: Vec<f64>,
    reward_grids: HashMap<u64, Option<RewardGrid>>,
    reward_grid_cells: usize,
    reward_grid_targets: HashMap<(u64, u64), (f64, f64)>,
    combo_regions: Option<(u64, Option<Arc<Vec<SnapshotRegion>>>)>,
    target_bonus_possible: HashMap<(u64, u64), bool>,
    prefix_mean_targets: HashMap<(u64, u64, u64), f64>,
    reward_target: Option<f64>,
    single_lex: bool,
    lex_incumbent: Option<[f64; 3]>,
    rank_other: bool,
    other_target: Option<f64>,
    other_exchange: Option<(f64, f64)>,
    other_floor: f64,
    primary_bonus_ceiling: Option<f64>,
    other_bonus_ceiling: Option<f64>,
    snapshot_bonus_frontier: Vec<Vec<(f64, f64)>>,
    other_members: Vec<f64>,
    other_snaps: Vec<f64>,
    other_member_top: f64,
    member_suffix: Vec<Vec<f64>>,
    member_combo_caps: Option<Vec<f64>>,
    combo_characters: Vec<Vec<usize>>,
    member_power: Vec<f64>,
    member_options: Vec<Vec<i32>>,
    combo_suffix: Vec<Vec<f64>>,
    combo_bonus_suffix: Vec<Vec<Vec<[f64; 2]>>>,
    combo_snap_bonus: [f64; 2],
    combo_member_floor: Option<(u64, Option<u64>, bool)>,
    combo_discarded_upper: f64,
    assignment_filtered_empty: bool,
    combo_member_snaps: Vec<bool>,
    other_snap_top: f64,
    bonus_max: f64,
    bonus_floors: HashMap<u64, Option<f64>>,
    other_bonus_floors: HashMap<u64, f64>,
    targets: HashMap<u64, Vec<JointTarget>>,
    combo_bonuses: Option<Vec<i32>>,
    other_suffix: [f64; 6],
    other_ordered: Vec<i32>,
    contexts: HashMap<RewardContextKey, (Arc<RewardContext>, bool)>,
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
    gauge_actors: HashMap<u32, Option<(i32, i32, i64, bool)>>,
    gauge_contexts: HashMap<[u32; 5], [u32; 5]>,
    gauge_context_order: VecDeque<[u32; 5]>,
    gauge_cores: HashMap<GaugeCoreKey, [u32; 5]>,
    gauge_core_order: VecDeque<(GaugeCoreKey, usize)>,
    gauge_core_bytes: usize,
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
    just_tables_ready: bool,
    just_tables: Option<JustRewardTables>,
    just_assignment_ready: bool,
    just_options: [Vec<i32>; 5],
    just_converters: [Vec<crate::just_bound::NumericConverter>; 5],
    just_nonchain: [Vec<i32>; 5],
    just_domains: HashMap<Vec<usize>, usize>,
    just_domain: usize,
    just_patterns: HashMap<JustPatternKey, Option<Arc<crate::just_bound::PatternSet>>>,
    just_pattern_order: VecDeque<(JustPatternKey, usize)>,
    just_pattern_bytes: usize,
    just_root_caps: HashMap<usize, f64>,
    just_counters: crate::support_aliases::JustCounters,
    support_aliases: crate::support_aliases::SupportAliases,
    settlement_values: HashMap<(u64, bool), Vec<(i32, f64)>>,
    reward_curves: HashMap<(u64, bool), Option<crate::event::RewardCurve>>,
    conditioned_rewards: HashMap<(u64, u64, u64), f64>,
    conditioned_cuts: HashMap<(u64, u64, u64, u64, bool), bool>,
    member_dominance: HashMap<(usize, usize), MemberDominance>,
}
impl<'a> RewardObjective<'a> {
    pub(crate) fn score_upper(&self, totals: Totals) -> f64 {
        up(totals.power * self.coefficient)
    }
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
                    .filter(|r| r.battle_required > 0)
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
        let other_member_top = crate::event::top_character_bonus(members, &other_members);
        let mut snap_top = other_snaps.clone();
        snap_top.sort_by(|a, b| b.total_cmp(a));
        let other_snap_top = snap_top.iter().take(5).sum();
        let primary_member_bonus: Vec<_> = members
            .iter()
            .map(|m| {
                crate::power::member_event_percent(&route.effects, m, if points { 0 } else { 1 })
                    as f64
            })
            .collect();
        let primary_snap_bonus: Vec<_> = snaps
            .iter()
            .map(|s| {
                crate::power::snap_event_percent(&route.effects, s, if points { 0 } else { 1 })
                    as f64
            })
            .collect();
        let mut snap_bonus = primary_snap_bonus.clone();
        snap_bonus.sort_by(|a, b| b.total_cmp(a));
        let bonus_max = crate::event::top_character_bonus(members, &primary_member_bonus)
            + snap_bonus.iter().take(5).sum::<f64>();
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
        let raw_slots =
            (stable_life && gekiso.accuracy.great == 0.0 && gekiso.accuracy.miss_every == 0)
                .then(|| skills::prepare(master, members, snaps));
        let canonical_slots = raw_slots.as_ref().is_some_and(|table| {
            table.slots.iter().all(|slot| {
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
        let empty_luck = (stable_life && samples > 0)
            .then(|| {
                crate::gekiso_objective::EmptyLuckPrograms::new(master, members, snaps, &gekiso)
            })
            .flatten();
        let member_combo_caps = (deterministic.is_some()
            && stable_life
            && !gekiso.luck
            && gekiso.setup.missions.iter().all(|&mission| mission == 1))
        .then(|| {
            members
                .iter()
                .map(|member| {
                    maximum_combo(master, std::slice::from_ref(member), &gekiso.setup.missions)
                        - 1.0
                })
                .collect()
        });
        let context_fields = crate::support_aliases::ContextFields::new(master);
        let primary_snap_bonus = Arc::new(primary_snap_bonus);
        let support_aliases = crate::support_aliases::SupportAliases::new(
            &gekiso.setup.missions,
            snaps,
            canonical_slots,
        );
        let just_counters = crate::support_aliases::JustCounters::new(master, members);
        let primary_bonus_totals = if member_combo_caps.is_some() {
            bonus_totals(&primary_member_bonus, &primary_snap_bonus)
        } else {
            Vec::new()
        };
        Self {
            exact_cache: HashMap::default(),
            live_floor: false,
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
            raw_slots,
            deterministic,
            empty_luck,
            empty_options: None,
            empty_scores: HashMap::default(),
            empty_score_order: VecDeque::new(),
            witness_candidates: HashMap::default(),
            witness_gaps: BTreeMap::new(),
            empty_root_bonus: None,
            empty_upper: None,
            lottery_support: Vec::new(),
            witness_candidate_order: VecDeque::new(),
            witness_seeds: Vec::new(),
            combo_models: HashMap::default(),
            combo_scripts: None,
            combo_traces: HashMap::default(),
            combo_trace_order: VecDeque::new(),
            combo_model_order: VecDeque::new(),
            assignment_combo: None,
            assignment_power: 0.0,
            assignment_certificate: None,
            #[cfg(not(target_arch = "wasm32"))]
            assignment_shared_certificate: None,
            certificate_domains: HashMap::default(),
            assignment_certificates: HashMap::default(),
            certificate_order: VecDeque::new(),
            assignment_primary: 0.0,
            power_estimate: 1.0,
            snapshot_power: Arc::new(Vec::new()),
            primary_snap_bonus,
            primary_member_bonus,
            primary_bonus_totals,
            reward_grids: HashMap::default(),
            reward_grid_cells: 0,
            reward_grid_targets: HashMap::default(),
            combo_regions: None,
            target_bonus_possible: HashMap::default(),
            prefix_mean_targets: HashMap::default(),
            bound_profiles: HashMap::default(),
            bound_ids: vec![u32::MAX; members.len() * (snaps.len() + 1)],
            reward_target: None,
            single_lex: false,
            lex_incumbent: None,
            rank_other: false,
            other_target: None,
            other_exchange: None,
            other_floor: 0.0,
            primary_bonus_ceiling: None,
            other_bonus_ceiling: None,
            snapshot_bonus_frontier: Vec::new(),
            other_members,
            other_snaps,
            other_member_top,
            member_suffix: Vec::new(),
            member_combo_caps,
            combo_characters: Vec::new(),
            member_power: Vec::new(),
            member_options: Vec::new(),
            combo_suffix: Vec::new(),
            combo_bonus_suffix: Vec::new(),
            combo_snap_bonus: [snap_bonus.iter().take(5).sum(), other_snap_top],
            combo_member_floor: None,
            combo_discarded_upper: f64::NEG_INFINITY,
            assignment_filtered_empty: false,
            combo_member_snaps: Vec::new(),
            other_snap_top,
            bonus_max,
            bonus_floors: HashMap::default(),
            other_bonus_floors: HashMap::default(),
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
            gauge_actors: HashMap::default(),
            gauge_contexts: HashMap::default(),
            gauge_context_order: VecDeque::new(),
            gauge_cores: HashMap::default(),
            gauge_core_order: VecDeque::new(),
            gauge_core_bytes: 0,
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
            just_tables_ready: false,
            just_tables: None,
            just_assignment_ready: false,
            just_options: std::array::from_fn(|_| Vec::new()),
            just_converters: std::array::from_fn(|_| Vec::new()),
            just_nonchain: std::array::from_fn(|_| Vec::new()),
            just_domains: HashMap::default(),
            just_domain: 0,
            just_patterns: HashMap::default(),
            just_pattern_order: VecDeque::new(),
            just_pattern_bytes: 0,
            just_root_caps: HashMap::default(),
            just_counters,
            support_aliases,
            settlement_values: HashMap::default(),
            reward_curves: HashMap::default(),
            conditioned_rewards: HashMap::default(),
            conditioned_cuts: HashMap::default(),
            member_dominance: HashMap::default(),
        }
    }
    pub(crate) fn swap_settlements(&mut self, cache: &mut SettlementCache) {
        std::mem::swap(&mut self.exact_cache, &mut cache.values);
        std::mem::swap(&mut self.exact_order, &mut cache.order);
    }
    pub(crate) fn score_proposal(
        &mut self,
        team: &Team,
        power: f64,
        slots: &[f64],
        cutoff: f64,
    ) -> Result<Option<Exact>, String> {
        let bonus = team
            .members
            .iter()
            .map(|&m| self.member_bonus(m))
            .sum::<f64>()
            + team
                .snaps
                .iter()
                .filter(|&&snap| snap >= 0)
                .map(|&snap| self.snap_bonus(snap as usize))
                .sum::<f64>();
        let rewards: Vec<_> = self
            .ranks
            .iter()
            .map(|&rank| self.value(bonus, rank))
            .collect();
        let peak = rewards.iter().copied().fold(0.0, f64::max);
        let count = 120.0
            * if self.gekiso.luck {
                self.samples as f64
            } else {
                1.0
            };
        if !cutoff.is_finite()
            || rewards.iter().any(|&reward| !safe(reward) || reward < 0.0)
            || !safe(peak * count)
        {
            return self.exact(team, power, slots).map(Some);
        }
        self.evaluate(
            team,
            power,
            slots,
            Cutoff {
                key: cutoff,
                prune_equal: false,
            },
        )
        .map(|value| match value {
            Evaluation::Exact(exact) => Some(exact),
            Evaluation::Pruned(_) => None,
        })
    }
    pub(crate) fn install_scores(&mut self, hits: &[crate::search::Hit]) {
        for hit in hits {
            let Some(mean) = hit.detail["settlement"]["mean"].as_f64() else {
                continue;
            };
            let key = (
                hit.team.members.clone(),
                hit.team.snaps.clone(),
                hit.power as i32,
                self.other_exchange.map(|(n, d)| (n.to_bits(), d.to_bits())),
            );
            if !self.exact_cache.contains_key(&key) {
                self.exact_cache
                    .insert(key.clone(), (mean, hit.detail.clone()));
                self.exact_order.push_back(key);
            }
        }
    }
    /// Shares replay state between reward passes with the same inventory and chart.
    pub(crate) fn swap_context_cache(&mut self, other: &mut Self) {
        std::mem::swap(&mut self.contexts, &mut other.contexts);
        std::mem::swap(&mut self.context_order, &mut other.context_order);
        std::mem::swap(&mut self.context_bytes, &mut other.context_bytes);
        std::mem::swap(&mut self.performer_ids, &mut other.performer_ids);
        std::mem::swap(&mut self.performer_index, &mut other.performer_index);
        std::mem::swap(&mut self.performers, &mut other.performers);
        std::mem::swap(&mut self.simple_actors, &mut other.simple_actors);
        std::mem::swap(&mut self.gauge_actors, &mut other.gauge_actors);
        std::mem::swap(&mut self.gauge_contexts, &mut other.gauge_contexts);
        std::mem::swap(
            &mut self.gauge_context_order,
            &mut other.gauge_context_order,
        );
        std::mem::swap(&mut self.gauge_cores, &mut other.gauge_cores);
        std::mem::swap(&mut self.gauge_core_order, &mut other.gauge_core_order);
        std::mem::swap(&mut self.gauge_core_bytes, &mut other.gauge_core_bytes);
        std::mem::swap(&mut self.canonical_contexts, &mut other.canonical_contexts);
        std::mem::swap(
            &mut self.canonical_context_order,
            &mut other.canonical_context_order,
        );
        std::mem::swap(&mut self.rush_aliases, &mut other.rush_aliases);
    }
    pub fn rank_other_at_reward(&mut self, target: f64, proven: bool) {
        self.clear_assignment_certificates();
        self.reward_target = Some(target);
        self.rank_other = true;
        self.allow_dominance = proven;
        self.prepare_bonus_ceilings();
        self.combo_member_floor = None;
        self.member_snapshot_floor(f64::NEG_INFINITY);
    }
    pub fn constrain_other(&mut self, target: f64, exchange: Option<(f64, f64)>, proven: bool) {
        self.clear_assignment_certificates();
        self.other_target = Some(target);
        self.other_exchange = exchange;
        self.other_bonus_floors.clear();
        self.allow_dominance = proven;
        self.reward_curves.clear();
        self.settlement_values.clear();
        self.prepare_bonus_frontier(target);
        self.prepare_bonus_ceilings();
    }
    fn prepare_bonus_ceilings(&mut self) {
        let other_max = self.other_member_top + self.other_snap_top;
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
        let maximum = self.other_member_top + self.other_snap_top;
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
    fn other_bonus_floor(&mut self, required: f64) -> f64 {
        if let Some(&floor) = self.other_bonus_floors.get(&required.to_bits()) {
            return floor;
        }
        let maximum = self.other_member_top + self.other_snap_top;
        let value = |bonus| {
            self.ranks
                .iter()
                .map(|&rank| self.other(bonus, rank))
                .fold(f64::NEG_INFINITY, f64::max)
        };
        let floor = if !required.is_finite() {
            if required < 0.0 {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            }
        } else if value(maximum) < required {
            f64::INFINITY
        } else {
            let (mut low, mut high) = (0i64, maximum as i64);
            while low < high {
                let mid = (low + high) >> 1;
                if value(mid as f64) >= required {
                    high = mid;
                } else {
                    low = mid + 1;
                }
            }
            low as f64
        };
        if self.other_bonus_floors.len() >= 64 {
            self.other_bonus_floors.clear();
        }
        self.other_bonus_floors.insert(required.to_bits(), floor);
        floor
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
        self.clear_assignment_certificates();
        self.reward_target = Some(target);
        self.rank_other = false;
        self.other_target = Some(other);
        self.allow_dominance = proven;
        self.prepare_bonus_frontier(other);
        self.prepare_bonus_ceilings();
        self.combo_member_floor = None;
        self.member_snapshot_floor(f64::NEG_INFINITY);
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
                    || self.chart.ranks.iter().any(|r| {
                        r.rank == rank && r.battle_required > 0 && r.battle_required as f64 <= upper
                    })
            })
            .map(|&rank| self.other(bonus, rank))
            .fold(f64::NEG_INFINITY, f64::max)
    }
    pub(crate) fn enable_lexicographic(&mut self) -> bool {
        self.single_lex = self.just_reward_enabled();
        self.single_lex
    }
    fn below_limit(&self, upper: f64, limit: f64) -> bool {
        upper < limit || !self.single_lex && upper == limit
    }
    fn lex_bound(&self, primary: f64, other: f64, mean: f64) -> f64 {
        if let Some(best) = self.lex_incumbent {
            if primary == best[0] && (other < best[1] || other == best[1] && mean <= best[2]) {
                return primary.next_down();
            }
        }
        primary
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
            None if self.lex_incumbent.is_some_and(|best| reward == best[0]) => self.lex_bound(
                reward,
                self.other_ceiling(power, other, coefficient),
                score_bound(coefficient, power),
            ),
            None => reward,
        }
    }
    fn combo_trace(
        &mut self,
        key: &str,
        script: &[Performer],
    ) -> Option<Arc<crate::reward_trace::ComboTrace>> {
        if !self.combo_traces.contains_key(key) {
            let mut trace =
                crate::reward_trace::ComboTrace::new(self.master, &self.gekiso, script)?;
            trace.cache_combo_supports();
            self.combo_traces.insert(key.to_owned(), Arc::new(trace));
        }
        self.combo_trace_order.retain(|old| old != key);
        self.combo_trace_order.push_back(key.to_owned());
        self.combo_traces.get(key).map(Arc::clone)
    }
    fn trim_combo_traces(&mut self) {
        let bytes = |cache: &HashMap<String, Arc<crate::reward_trace::ComboTrace>>| {
            cache
                .iter()
                .map(|(key, value)| key.capacity() + value.cache_bytes())
                .sum::<usize>()
        };
        let mut total = bytes(&self.combo_traces);
        while total > 8 * 1024 * 1024 {
            let Some(key) = self.combo_trace_order.pop_front() else {
                break;
            };
            if let Some((key, value)) = self.combo_traces.remove_entry(&key) {
                total -= key.capacity() + value.cache_bytes();
            }
        }
    }
    fn assignment_model(&mut self) -> Option<&mut DeterministicBound> {
        if let Some(combo) = &self.assignment_combo {
            self.combo_models.get_mut(combo)
        } else {
            self.deterministic.as_mut()
        }
    }
    fn clear_assignment_certificates(&mut self) {
        self.assignment_certificate = None;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.assignment_shared_certificate = None;
        }
        self.assignment_certificates.clear();
        self.certificate_order.clear();
        self.prefix_mean_targets.clear();
    }
    fn prepare_assignment_certificate(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.assignment_certificate = None;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.assignment_shared_certificate = None;
        }
        if self.member_combo_caps.is_none() || self.single_lex {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.live_floor && self.snaps.len() < 128 {
                let mut slots = [(0usize, 0u128); 5];
                for (slot, &member) in members.iter().enumerate() {
                    let mask = options[slot]
                        .iter()
                        .fold(0u128, |mask, &snap| mask | (1u128 << (snap + 1) as usize));
                    slots[slot] = (member, mask);
                }
                slots.sort_unstable();
                self.assignment_shared_certificate =
                    Some((slots.map(|slot| slot.0), slots.map(|slot| slot.1)));
            }
        }
        let mut slots = [(0usize, 0u32); 5];
        for (slot, &member) in members.iter().enumerate() {
            let domain = if let Some(&id) = self.certificate_domains.get(options[slot].as_slice()) {
                id
            } else {
                if self.certificate_domains.len() >= 4096 {
                    return;
                }
                let id = self.certificate_domains.len() as u32;
                self.certificate_domains.insert(options[slot].clone(), id);
                id
            };
            slots[slot] = (member, domain);
        }
        slots.sort_unstable();
        self.assignment_certificate = Some((slots.map(|slot| slot.0), slots.map(|slot| slot.1)));
    }
    fn remember_assignment_certificate(
        &mut self,
        key: Option<AssignmentCertificate>,
        limit: f64,
        bound: f64,
    ) {
        let Some(key) = key.filter(|_| self.below_limit(bound, limit) && limit.is_finite()) else {
            return;
        };
        let power = self.assignment_power;
        #[cfg(not(target_arch = "wasm32"))]
        if self.live_floor {
            if let Some(key) = self.assignment_shared_certificate {
                crate::publish_reward_proof(key, power, bound);
            }
        }

        if let Some(old) = self.assignment_certificates.get_mut(&key) {
            if power > old.0 || power == old.0 && bound < old.1 {
                *old = (power, bound);
            }
            return;
        }
        if self.assignment_certificates.len() >= 32768 {
            if let Some(old) = self.certificate_order.pop_front() {
                self.assignment_certificates.remove(&old);
            }
        }
        self.assignment_certificates.insert(key, (power, bound));
        self.certificate_order.push_back(key);
    }
    fn certified_assignment_bound(&self, power: f64, limit: f64) -> Option<f64> {
        // COMBO certificates cover skill and accumulation orders. Lowering
        // power preserves their rejection and the native rounding envelope.
        let check = |(maximum, proved): (f64, f64)| {
            if power <= maximum {
                let bound = proved.max(self.combo_discarded_upper);
                if self.below_limit(bound, limit) {
                    return Some(bound);
                }
            }
            None
        };
        if let Some(bound) = self
            .assignment_certificate
            .and_then(|key| self.assignment_certificates.get(&key).copied())
            .and_then(check)
        {
            return Some(bound);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.live_floor {
            return self
                .assignment_shared_certificate
                .and_then(|key| crate::shared_reward_proof(&key))
                .and_then(check);
        }
        None
    }
    fn member_snapshot_floor(&mut self, floor: f64) {
        let identity = (
            floor.to_bits(),
            self.reward_target.map(f64::to_bits),
            self.rank_other,
        );
        if self.combo_member_floor == Some(identity)
            || self.combo_snap_bonus[0] <= 0.0
            || self.primary_snap_bonus.iter().any(|&bonus| bonus < 0.0)
        {
            return;
        }
        self.combo_member_floor = Some(identity);
        let required = (floor - (self.bonus_max - self.combo_snap_bonus[0])).next_down();
        let mut order: Vec<_> = (0..self.snaps.len()).collect();
        order.sort_by(|&a, &b| self.primary_snap_bonus[b].total_cmp(&self.primary_snap_bonus[a]));
        let allowed: Vec<_> = (-1..self.snaps.len() as i32)
            .map(|snap| {
                let own = if snap < 0 {
                    0.0
                } else {
                    self.primary_snap_bonus[snap as usize]
                };
                own + order
                    .iter()
                    .filter(|&&s| s as i32 != snap)
                    .take(4)
                    .map(|&s| self.primary_snap_bonus[s])
                    .sum::<f64>()
                    >= required
            })
            .collect();
        let member_upper = self.bonus_max - self.combo_snap_bonus[0];
        let discarded_bonus = allowed
            .iter()
            .enumerate()
            .filter(|(_, yes)| !**yes)
            .map(|(column, _)| {
                let snap = column as i32 - 1;
                let own = if snap < 0 {
                    0.0
                } else {
                    self.primary_snap_bonus[snap as usize]
                };
                member_upper
                    + own
                    + order
                        .iter()
                        .filter(|&&other| other as i32 != snap)
                        .take(4)
                        .map(|&other| self.primary_snap_bonus[other])
                        .sum::<f64>()
            })
            .fold(f64::NEG_INFINITY, f64::max);
        self.combo_discarded_upper = if discarded_bonus == f64::NEG_INFINITY {
            f64::NEG_INFINITY
        } else {
            let reward = self
                .ranks
                .iter()
                .map(|&rank| self.value(discarded_bonus, rank))
                .fold(f64::NEG_INFINITY, f64::max);
            if let Some(target) = self.reward_target {
                if reward < target {
                    -1.0
                } else {
                    f64::INFINITY
                }
            } else {
                reward
            }
        };
        if self.combo_member_snaps == allowed {
            return;
        }
        self.combo_member_snaps = allowed;
        if let Some(model) = &mut self.deterministic {
            model.member_snapshots(&self.combo_member_snaps);
            model.prepare_members(&self.combo_characters);
        }
        for model in self.combo_models.values_mut() {
            model.member_snapshots(&self.combo_member_snaps);
            model.prepare_members(&self.combo_characters);
        }
    }
    fn reward_curve_key(&mut self, bonus: f64, secondary: bool) -> (u64, bool) {
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
                                .map_or(0, |r| r.battle_required)
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
        key
    }
    fn projected_bound(&mut self, t: Totals, other: f64, coefficients: &[f64]) -> f64 {
        self.projected_from(t, other, coefficients, false)
    }
    fn projected_pricing_bound(&mut self, t: Totals, other: f64, coefficients: &[f64]) -> f64 {
        self.projected_from(t, other, coefficients, true)
    }
    fn projected_from(
        &mut self,
        t: Totals,
        other: f64,
        coefficients: &[f64],
        unordered: bool,
    ) -> f64 {
        let mut values = [f64::INFINITY; 2];
        for (i, &bonus) in [t.bonus, other].iter().enumerate() {
            let secondary = i == 1;
            if secondary
                && self.reward_target.is_none()
                && self.other_target.is_none()
                && self.lex_incumbent.is_none()
            {
                continue;
            }
            let key = self.reward_curve_key(bonus, secondary);
            values[i] = self.reward_curves[&key]
                .as_ref()
                .map_or(f64::INFINITY, |curve| {
                    if unordered {
                        curve.unordered_orders(coefficients, t.power)
                    } else {
                        curve.orders(coefficients, t.power)
                    }
                });
        }
        self.projected_values(values, || {
            up(mul(coefficients.iter().copied().fold(0.0, add), t.power)
                / coefficients.len() as f64)
        })
    }
    fn projected_limited(&mut self, t: Totals, other: f64, coefficient: f64, count: usize) -> f64 {
        let mut values = [f64::INFINITY; 2];
        for (i, &bonus) in [t.bonus, other].iter().enumerate() {
            if i == 1
                && self.reward_target.is_none()
                && self.other_target.is_none()
                && self.lex_incumbent.is_none()
            {
                continue;
            }
            let key = self.reward_curve_key(bonus, i == 1);
            values[i] = self.reward_curves[&key]
                .as_ref()
                .map_or(f64::INFINITY, |curve| {
                    curve.limited_orders(coefficient, t.power, count)
                });
        }
        self.projected_values(values, || {
            if count == 120 {
                mul(t.power, coefficient)
            } else {
                f64::INFINITY
            }
        })
    }
    fn projected_values(&self, values: [f64; 2], mean: impl FnOnce() -> f64) -> f64 {
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
            Some(_) => mean(),
            None => {
                let mean = if self
                    .lex_incumbent
                    .is_some_and(|best| values[0] == best[0] && values[1] == best[1])
                {
                    mean()
                } else {
                    f64::INFINITY
                };
                self.lex_bound(values[0], values[1], mean)
            }
        }
    }
    fn bonus_target_possible(&mut self, bonus: f64) -> bool {
        let Some(target) = self.reward_target else {
            return true;
        };
        let key = (target.to_bits(), bonus.to_bits());
        if let Some(&possible) = self.target_bonus_possible.get(&key) {
            return possible;
        }
        let values: Vec<_> = self
            .ranks
            .iter()
            .map(|&rank| self.value(bonus, rank))
            .collect();
        if values
            .iter()
            .any(|&value| !safe(value) || !(0.0..=1e10).contains(&value))
        {
            return true;
        }
        let total = (target * 120.0).round();
        if !safe(total) || total / 120.0 != target {
            return true;
        }
        let maximum = values.iter().copied().fold(0.0_f64, f64::max) as i64;
        let minimum = values.iter().copied().fold(f64::INFINITY, f64::min) as i64;
        let deficit = 120 * maximum - total as i64;
        let mut losses: Vec<_> = values
            .iter()
            .map(|&value| maximum - value as i64)
            .filter(|&loss| loss > 0)
            .collect();
        losses.sort_unstable();
        losses.dedup();
        let mut divisor = 0i64;
        for &loss in &losses {
            let mut a = divisor;
            let mut b = loss;
            while b != 0 {
                let rest = a % b;
                a = b;
                b = rest;
            }
            divisor = a;
        }
        let possible = if deficit < 0 || total < 120.0 * minimum as f64 {
            false
        } else if deficit == 0 {
            true
        } else if divisor == 0 || deficit % divisor != 0 {
            false
        } else {
            let deficit = (deficit / divisor) as usize;
            if deficit > 2_000_000 {
                return true;
            }
            let losses: Vec<_> = losses
                .into_iter()
                .map(|loss| (loss / divisor) as usize)
                .filter(|&loss| loss <= deficit)
                .collect();
            let mut coins = vec![121u8; deficit + 1];
            coins[0] = 0;
            for value in 1..=deficit {
                for &loss in &losses {
                    if loss > value {
                        break;
                    }
                    coins[value] = coins[value].min(coins[value - loss].saturating_add(1));
                }
            }
            coins[deficit] <= 120
        };
        self.target_bonus_possible.insert(key, possible);
        possible
    }
    fn discrete_primary(&self) -> bool {
        self.reward_target.is_none()
            && !self.single_lex
            && self.exchange.is_none()
            && self.member_combo_caps.is_some()
    }
    fn reward_floor(&mut self, bonus: f64, limit: f64) -> f64 {
        if !limit.is_finite() || !self.discrete_primary() {
            return limit;
        }
        let key = bonus.to_bits();
        let target_key = (key, limit.to_bits());
        if let Some(&(target, _)) = self.reward_grid_targets.get(&target_key) {
            return target;
        }
        if self.reward_grid_targets.len() >= 16384 {
            self.reward_grid_targets.clear();
        }
        if !self.reward_grids.contains_key(&key) {
            let rewards: Vec<_> = self
                .ranks
                .iter()
                .map(|&rank| self.value(bonus, rank))
                .collect();
            if rewards
                .iter()
                .any(|&value| !safe(value) || !(0.0..=MAX_SAFE / 120.0).contains(&value))
            {
                self.reward_grids.insert(key, None);
                return limit;
            }
            let values: Vec<_> = rewards.into_iter().map(|value| value as i64).collect();
            let maximum = values.iter().copied().max().unwrap_or(0);
            let minimum = values.iter().copied().min().unwrap_or(0);
            let mut divisor = 0;
            for &value in &values {
                let (mut a, mut b) = (divisor, maximum - value);
                while b != 0 {
                    let remainder = a % b;
                    a = b;
                    b = remainder;
                }
                divisor = a;
            }
            let mut losses: Vec<_> = values
                .into_iter()
                .filter(|&value| value < maximum)
                .map(|value| ((maximum - value) / divisor) as usize)
                .collect();
            losses.sort_unstable();
            losses.dedup();
            self.reward_grids.insert(
                key,
                Some(RewardGrid {
                    maximum,
                    minimum,
                    divisor,
                    losses,
                    coins: Vec::new(),
                    reachable: Vec::new(),
                }),
            );
        }
        let Some(grid) = self.reward_grids.get_mut(&key).and_then(Option::as_mut) else {
            return limit;
        };
        if (grid.maximum as f64) < limit {
            self.reward_grid_targets
                .insert(target_key, (f64::INFINITY, grid.maximum as f64));
            return f64::INFINITY;
        }
        if grid.divisor == 0 {
            self.reward_grid_targets
                .insert(target_key, (grid.maximum as f64, f64::NEG_INFINITY));
            return grid.maximum as f64;
        }
        let total = 120 * grid.maximum;
        let (mut low, mut high) = (0, 120 * (grid.maximum - grid.minimum) / grid.divisor);
        while low < high {
            let middle = low + (high - low + 1) / 2;
            if (total - grid.divisor * middle) as f64 / 120.0 >= limit {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let predecessor = (total - grid.divisor * (low + 1)) as f64 / 120.0;
        let length = low as usize + 1;
        if length <= 262_144
            && self.reward_grid_cells + length.saturating_sub(grid.coins.len()) <= 1_048_576
        {
            let start = grid.coins.len().max(1);
            let previous = grid.coins.capacity();
            grid.coins
                .reserve_exact(length.saturating_sub(grid.coins.len()));
            self.reward_grid_cells += grid.coins.capacity() - previous;
            grid.coins.resize(grid.coins.len().max(length), 121);
            grid.coins[0] = 0;
            if grid.reachable.is_empty() {
                grid.reachable.push(0);
            }
            for value in start..length {
                for &loss in &grid.losses {
                    if loss > value {
                        break;
                    }
                    grid.coins[value] =
                        grid.coins[value].min(grid.coins[value - loss].saturating_add(1));
                }
                if grid.coins[value] <= 120 {
                    grid.reachable.push(value as u32);
                }
            }
        }
        if grid.coins.len() >= length {
            let end = grid
                .reachable
                .partition_point(|&value| value < length as u32);
            low = grid.reachable[end - 1] as i64;
        }
        let target = (total - grid.divisor * low) as f64 / 120.0;
        self.reward_grid_targets
            .insert(target_key, (target, predecessor));
        target
    }
    fn projected_mean_bound(&mut self, bonus: f64, other: f64, mean: f64, limit: f64) -> f64 {
        let required = self.reward_floor(bonus, limit);
        let primary_key = self.reward_curve_key(bonus, false);
        let primary = self.reward_curves[&primary_key]
            .as_ref()
            .map_or(f64::INFINITY, |curve| curve.upper(mean));
        if self.reward_target.is_some_and(|target| primary < target) {
            return -1.0;
        }
        if self.reward_target.is_none() {
            if primary < required && self.discrete_primary() {
                return self
                    .reward_grid_targets
                    .get(&(bonus.to_bits(), limit.to_bits()))
                    .map_or(primary, |&(_, predecessor)| primary.min(predecessor));
            }
            return primary;
        }
        let secondary_key = self.reward_curve_key(other, true);
        let secondary = self.reward_curves[&secondary_key]
            .as_ref()
            .map_or(f64::INFINITY, |curve| curve.upper(mean));
        if self.other_target.is_some_and(|target| secondary < target) {
            return -1.0;
        }
        if self.rank_other {
            secondary
        } else {
            mean
        }
    }
    fn mean_requirement(&mut self, bonus: f64, other: f64, limit: f64) -> f64 {
        let key = (bonus.to_bits(), other.to_bits(), limit.to_bits());
        if let Some(&value) = self.prefix_mean_targets.get(&key) {
            return value;
        }
        let required = if !self.bonus_target_possible(bonus) {
            f64::INFINITY
        } else {
            let target = self.reward_floor(bonus, self.reward_target.unwrap_or(limit));
            let curve_key = self.reward_curve_key(bonus, false);
            let mut required = self.reward_curves[&curve_key]
                .as_ref()
                .map_or(0.0, |curve| curve.required(target));
            if let Some(target) = if self.rank_other {
                Some(limit)
            } else {
                self.other_target
            } {
                let curve_key = self.reward_curve_key(other, true);
                required = required.max(
                    self.reward_curves[&curve_key]
                        .as_ref()
                        .map_or(0.0, |curve| curve.required(target)),
                );
            }
            if self.reward_target.is_some() && !self.rank_other {
                required = required.max(limit);
            }
            required
        };
        if self.prefix_mean_targets.len() >= 16384 {
            self.prefix_mean_targets.clear();
        }
        self.prefix_mean_targets.insert(key, required);
        required
    }
    fn bonus_levels(&self, minimum: f64, maximum: f64) -> &[f64] {
        let start = self
            .primary_bonus_totals
            .partition_point(|&bonus| bonus < minimum);
        let end = self
            .primary_bonus_totals
            .partition_point(|&bonus| bonus <= maximum);
        &self.primary_bonus_totals[start..end.max(start)]
    }
    fn prefix_mean_possible(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        t: Totals,
        other: f64,
        limit: f64,
    ) -> bool {
        if self.primary_bonus_totals.is_empty() {
            return true;
        }
        let floor = self.bonus_floor(limit).unwrap_or(f64::NEG_INFINITY);
        let selected = self.assignment_primary
            + assigned[..from]
                .iter()
                .filter(|&&snap| snap >= 0)
                .map(|&snap| self.primary_snap_bonus[snap as usize])
                .sum::<f64>();
        let levels = self.bonus_levels(floor.max(selected), t.bonus).to_vec();
        if levels.len() > 64 {
            return true;
        }
        let targets: Vec<_> = levels
            .iter()
            .map(|&bonus| self.mean_requirement(bonus, other, limit))
            .collect();
        let power = self.assignment_power;
        let primary = self.assignment_primary;
        let Some(model) = self.assignment_model() else {
            return true;
        };
        model.prepare_prefix_mean(members, power, t.power, (floor - primary).next_down());
        let Some(pricing) = &model.prefix_mean else {
            return true;
        };
        let sums = pricing.sums(assigned, from);
        levels
            .iter()
            .zip(targets)
            .any(|(&bonus, target)| pricing.mean(&sums, (bonus - primary).next_down()) >= target)
    }
    fn member_pricing_bound(
        &mut self,
        pricing: &MemberRewardPricing,
        members: &[usize],
        from: usize,
        t: Totals,
        other: f64,
        limit: f64,
    ) -> f64 {
        let floor = self.bonus_floor(limit).unwrap_or(f64::NEG_INFINITY);
        let mut sums = None;
        if self.primary_bonus_totals.is_empty() {
            let sums = pricing.sums(members, from);
            let scores = pricing.scores(&sums, floor);
            return self.projected_pricing_bound(Totals { power: 1.0, ..t }, other, &scores);
        }
        let levels = self.bonus_levels(floor, t.bonus).to_vec();
        if levels.len() > 64 {
            let sums = pricing.sums(members, from);
            let scores = pricing.scores(&sums, floor);
            return self.projected_pricing_bound(Totals { power: 1.0, ..t }, other, &scores);
        }
        let mut upper = f64::NEG_INFINITY;
        for bonus in levels.into_iter().rev() {
            if !self.bonus_target_possible(bonus) {
                continue;
            }
            let mean = pricing.mean(members, from, bonus);
            let mean_value = self.projected_mean_bound(bonus, other, mean, limit);
            if mean_value < limit {
                upper = upper.max(mean_value);
                continue;
            }
            let sums = sums.get_or_insert_with(|| pricing.sums(members, from));
            let scores = pricing.scores(sums, bonus);
            let value = self.projected_pricing_bound(
                Totals {
                    power: 1.0,
                    bonus,
                    ..t
                },
                other,
                &scores,
            );
            upper = upper.max(value.min(mean_value));
            if upper >= limit {
                return upper;
            }
        }
        upper
    }
    fn joint_bonus_bound(
        &mut self,
        pricing: &JointRewardPricing,
        t: Totals,
        other: f64,
        limit: f64,
    ) -> f64 {
        if self.primary_bonus_totals.is_empty() {
            return f64::INFINITY;
        }
        let floor = self.bonus_floor(limit).unwrap_or(f64::NEG_INFINITY);
        let levels = self.bonus_levels(floor, t.bonus).to_vec();
        if levels.len() > 64 {
            return f64::INFINITY;
        }
        let mut upper = f64::NEG_INFINITY;
        for bonus in levels.into_iter().rev() {
            if !self.bonus_target_possible(bonus) {
                continue;
            }
            let scores = pricing.scores((bonus - self.assignment_primary).next_down());
            let value = self.projected_bound(
                Totals {
                    power: 1.0,
                    bonus,
                    ..t
                },
                other,
                &scores,
            );
            upper = upper.max(value);
            if upper >= limit {
                return upper;
            }
        }
        upper
    }
    fn joint_mean_bound(
        &mut self,
        pricing: &JointRewardPricing,
        t: Totals,
        other: f64,
        limit: f64,
    ) -> f64 {
        let floor = self.bonus_floor(limit).unwrap_or(f64::NEG_INFINITY);
        let levels = self.bonus_levels(floor, t.bonus).to_vec();
        let regions: Vec<_> = if self.primary_bonus_totals.is_empty() || levels.len() > 64 {
            vec![(floor, t.bonus)]
        } else {
            levels.into_iter().map(|bonus| (bonus, bonus)).collect()
        };
        let mut upper = f64::NEG_INFINITY;
        for (minimum, bonus) in regions.into_iter().rev() {
            if minimum == bonus && !self.bonus_target_possible(bonus) {
                continue;
            }
            let mean = pricing.rows[0].upper(
                (minimum - self.assignment_primary).next_down(),
                pricing.converted,
            );
            let value = self.projected_mean_bound(bonus, other, mean, limit);
            upper = upper.max(value);
            if upper >= limit {
                return upper;
            }
        }
        upper
    }
    fn snapshot_region_requirements(
        &mut self,
        members: &[usize],
        other: f64,
        limit: f64,
    ) -> Option<(Arc<Vec<SnapshotRegion>>, Vec<RegionRequirement>)> {
        let floor = self.bonus_floor(limit)?;
        let member_upper = self.bonus_max - self.combo_snap_bonus[0];
        let minimum = (floor - member_upper).next_down();
        if self
            .combo_regions
            .as_ref()
            .is_none_or(|(key, _)| *key != minimum.to_bits())
        {
            let regions = snapshot_regions(&self.primary_snap_bonus, minimum).map(Arc::new);
            self.combo_regions = Some((minimum.to_bits(), regions));
        }
        let regions = Arc::clone(self.combo_regions.as_ref()?.1.as_ref()?);
        let member_other = members
            .iter()
            .map(|&member| self.other_members[member])
            .sum::<f64>();
        let zero_other = self
            .other_snaps
            .iter()
            .enumerate()
            .filter(|&(snap, _)| self.primary_snap_bonus[snap] == 0.0)
            .map(|(_, &bonus)| bonus)
            .fold(0.0_f64, f64::max);
        let mut cache = HashMap::<(u64, u64), RegionRequirement>::default();
        let thresholds = regions
            .iter()
            .map(|region| {
                let bonus = self.assignment_primary + region.bonus;
                let other = other.min(
                    member_other
                        + region.snaps[..region.count]
                            .iter()
                            .map(|&snap| self.other_snaps[snap as usize])
                            .sum::<f64>()
                        + if region.count < 5 { zero_other } else { 0.0 },
                );
                let cache_key = (bonus.to_bits(), other.to_bits());
                if let Some(value) = cache.get(&cache_key) {
                    return value.clone();
                }
                let mut floors = Vec::new();
                let mean = if !self.bonus_target_possible(bonus) {
                    f64::INFINITY
                } else {
                    let key = self.reward_curve_key(bonus, false);
                    let mut required = 0.0_f64;
                    if let Some(curve) = &self.reward_curves[&key] {
                        let target = self.reward_target.unwrap_or(limit);
                        required = curve.required(target);
                        floors.push((Arc::new(curve.clone()), target));
                    }
                    let other_target = if self.rank_other {
                        Some(limit)
                    } else {
                        self.other_target
                    };
                    if let Some(target) = other_target {
                        let key = self.reward_curve_key(other, true);
                        if let Some(curve) = &self.reward_curves[&key] {
                            required = required.max(curve.required(target));
                            floors.push((Arc::new(curve.clone()), target));
                        }
                    }
                    if self.reward_target.is_some() && !self.rank_other {
                        required = required.max(limit);
                    }
                    required
                };
                let requirement = RegionRequirement { mean, floors };
                cache.insert(cache_key, requirement.clone());
                requirement
            })
            .collect();
        Some((regions, thresholds))
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
                            r.rank == rank
                                && r.battle_required > 0
                                && r.battle_required as f64 <= score
                        })
                })
                .map(|&r| self.value(t.bonus, r))
                .fold(f64::NEG_INFINITY, f64::max);
            if self.reward_target.is_none()
                && self.other_target.is_none()
                && self.lex_incumbent.is_none()
            {
                return reward;
            }
            let upper = self
                .ranks
                .iter()
                .filter(|&&rank| {
                    rank == 2
                        || self.chart.ranks.iter().any(|r| {
                            r.rank == rank
                                && r.battle_required > 0
                                && r.battle_required as f64 <= score
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
                None => self.lex_bound(reward, upper, score),
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
        if self.gekiso.luck && (self.reward_target.is_none() || self.rank_other) {
            return self.coefficient;
        }
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
                    || self.chart.ranks.iter().any(|r| {
                        r.rank == rank && r.battle_required > 0 && r.battle_required as f64 <= upper
                    })
            })
            .map(|&rank| self.value(bonus, rank))
            .fold(f64::NEG_INFINITY, f64::max)
    }
    fn just_reward_enabled(&self) -> bool {
        !self.gekiso.luck
            && self
                .gekiso
                .setup
                .missions
                .iter()
                .all(|&mission| mission == 3)
            && self.stable_life
            && self.canonical_slots
            && self.just_context.is_some()
            && self.context_fields.owner_local()
    }
    fn prepare_just_reward(&mut self, members: &[usize]) -> Option<()> {
        if !self.just_tables_ready {
            self.just_tables_ready = true;
            let mut converters = Vec::with_capacity(self.members.len() * (self.snaps.len() + 1));
            for member in 0..self.members.len() {
                for snap in -1..self.snaps.len() as i32 {
                    let id = self.performer_id(member, snap);
                    let mut actor = self.performers[id as usize].clone();
                    actor.gekisou_support_skills.retain(|&(id, level)| {
                        self.master.gekiso_support_skills[&id]
                            .effects
                            .iter()
                            .any(|row| row.level == level && row.kind == 13005)
                    });
                    converters.push(if actor.gekisou_support_skills.is_empty() {
                        None
                    } else {
                        Some(self.just_bounds.converter(self.master, &actor)?)
                    });
                }
            }
            let mut master = self.master.clone();
            let mut skills = (*master.gekiso_support_skills).clone();
            for skill in skills.values_mut() {
                skill.effects.retain(|row| row.kind != 13005);
            }
            master.gekiso_support_skills = Arc::new(skills);
            self.just_tables = Some(JustRewardTables {
                weight_orders: HashMap::default(),
                weight_order: VecDeque::new(),
                weight_bytes: 0,
                master,
                converters,
                components: HashMap::default(),
                domains: HashMap::default(),
                offers: HashMap::default(),
                order: VecDeque::new(),
                bytes: 0,
            });
        }
        if self.just_assignment_ready {
            return Some(());
        }
        let tables = self.just_tables.as_ref()?;
        let width = self.snaps.len() + 1;
        let mut signature = Vec::new();
        for (slot, &member) in members.iter().enumerate() {
            self.just_converters[slot].clear();
            self.just_nonchain[slot].clear();
            for &snap in &self.just_options[slot] {
                if let Some(prepared) = &tables.converters[member * width + (snap + 1) as usize] {
                    self.just_converters[slot].push(crate::just_bound::NumericConverter {
                        snap,
                        prepared: Arc::clone(prepared),
                        order: slot,
                    });
                } else {
                    self.just_nonchain[slot].push(snap);
                }
            }
            self.just_converters[slot].sort_unstable_by_key(|converter| converter.snap);
            signature.push(self.just_converters[slot].len());
            for converter in &self.just_converters[slot] {
                signature.extend([(converter.snap + 1) as usize, converter.prepared.program]);
            }
        }
        let next = self.just_domains.len();
        self.just_domain = *self.just_domains.entry(signature).or_insert(next);
        self.just_assignment_ready = true;
        Some(())
    }
    fn just_reward_bound(
        &mut self,
        members: &[usize],
        totals: Totals,
        assigned: &[i32],
        from: usize,
        limit: f64,
    ) -> Option<f64> {
        if !self.just_reward_enabled() {
            return None;
        }
        self.prepare_just_reward(members)?;
        let width = self.snaps.len() + 1;
        let prefix = std::array::from_fn(|slot| {
            if slot < from
                && self.just_tables.as_ref().unwrap().converters
                    [members[slot] * width + (assigned[slot] + 1) as usize]
                    .is_some()
            {
                assigned[slot]
            } else {
                -1
            }
        });
        let key = (self.just_domain, from, prefix);
        let patterns = if let Some(value) = self.just_patterns.get(&key) {
            Arc::clone(value.as_ref()?)
        } else {
            let value = self
                .just_bounds
                .converter_patterns(
                    self.master,
                    &self.gekiso,
                    &self.just_converters,
                    prefix,
                    from,
                    self.snaps.len(),
                )
                .map(|(patterns, _)| patterns);
            let bytes = 128
                + value.as_ref().map_or(0, |patterns| {
                    (patterns.fine.capacity() + patterns.coarse.capacity())
                        * std::mem::size_of::<crate::just_bound::CohortShape>()
                        + patterns
                            .fine
                            .iter()
                            .chain(&patterns.coarse)
                            .flat_map(|shape| &shape.options)
                            .map(|row| row.capacity() * 4)
                            .sum::<usize>()
                });
            if bytes <= 16 * 1024 * 1024 {
                while self.just_pattern_bytes + bytes > 16 * 1024 * 1024 {
                    let Some((old, size)) = self.just_pattern_order.pop_front() else {
                        break;
                    };
                    self.just_patterns.remove(&old);
                    self.just_pattern_bytes -= size;
                }
                self.just_patterns.insert(key, value.clone());
                self.just_pattern_order.push_back((key, bytes));
                self.just_pattern_bytes += bytes;
            }
            value?
        };
        let fixed_bonus = self.assignment_primary
            + assigned[..from]
                .iter()
                .filter(|&&snap| snap >= 0)
                .map(|&snap| self.primary_snap_bonus[snap as usize])
                .sum::<f64>();
        let fixed_other = members
            .iter()
            .map(|&member| self.other_members[member])
            .sum::<f64>()
            + assigned[..from]
                .iter()
                .filter(|&&snap| snap >= 0)
                .map(|&snap| self.other_snaps[snap as usize])
                .sum::<f64>();
        for coarse in [true, false] {
            if coarse && from >= 2 {
                continue;
            }
            let shapes = if coarse {
                &patterns.coarse
            } else {
                &patterns.fine
            };
            let mut upper = f64::NEG_INFINITY;
            let mut complete = true;
            for shape in shapes {
                if let Some(&cap) = self.just_root_caps.get(&shape.base.id) {
                    if self.below_limit(cap, limit) || from == 0 && coarse {
                        upper = upper.max(cap);
                        if !self.below_limit(cap, limit) {
                            complete = false;
                        }
                        continue;
                    }
                }

                if !self
                    .just_tables
                    .as_ref()
                    .unwrap()
                    .components
                    .contains_key(&shape.base.id)
                {
                    let chart = self
                        .gekiso
                        .converted_judgements(&shape.base.context.judgement)?;
                    let master = &self.just_tables.as_ref().unwrap().master;
                    let component = Arc::new(
                        DeterministicBound::with_luck_cap(
                            master,
                            &chart,
                            self.members,
                            self.snaps,
                            1.0,
                            None,
                            None,
                        )?
                        .fixed_component()?,
                    );
                    self.just_tables
                        .as_mut()
                        .unwrap()
                        .components
                        .insert(shape.base.id, component);
                }
                let component =
                    Arc::clone(&self.just_tables.as_ref().unwrap().components[&shape.base.id]);
                let width = component.width;
                let mut groups = Vec::with_capacity(5);
                let mut maximum_bonus = fixed_bonus;
                let mut maximum_other = fixed_other;
                let mut power_cap = self.assignment_power;
                let mut weight_cap = component.base;
                let available = |snap: i32| snap < 0 || !assigned[..from].contains(&snap);
                let mut feasible = true;
                for slot in 0..5 {
                    let member = members[slot];
                    if slot < from {
                        let snap = assigned[slot];
                        power_cap += self.snapshot_power
                            [member * width + if snap < 0 { width - 1 } else { snap as usize }];
                        weight_cap = add(
                            weight_cap,
                            component.rows[member * width + (snap + 1) as usize]
                                .into_iter()
                                .fold(0.0_f64, f64::max),
                        );
                        groups.push(None);
                        continue;
                    }
                    let mut choices = shape.options[slot].clone();
                    if shape.plain[slot] {
                        choices.extend_from_slice(&self.just_nonchain[slot]);
                    }
                    choices.sort_unstable();
                    choices.dedup();
                    let tables = self.just_tables.as_mut().unwrap();
                    let next = tables.domains.len();
                    let domain = *tables.domains.entry(choices.clone()).or_insert(next);
                    let key = (shape.base.id, member, domain);
                    let group = if let Some(group) = tables.offers.get(&key) {
                        Arc::clone(group)
                    } else {
                        let mut offers: [Vec<crate::just_bound::Offer>; 5] =
                            std::array::from_fn(|_| Vec::new());
                        let mut powers = Vec::new();
                        let mut bonuses = Vec::new();
                        let mut other_bonuses = Vec::new();
                        for &snap in &choices {
                            let power = self.snapshot_power
                                [member * width + if snap < 0 { width - 1 } else { snap as usize }];
                            powers.push((snap, power));
                            bonuses.push((
                                snap,
                                if snap < 0 {
                                    0.0
                                } else {
                                    self.primary_snap_bonus[snap as usize]
                                },
                            ));
                            other_bonuses.push((
                                snap,
                                if snap < 0 {
                                    0.0
                                } else {
                                    self.other_snaps[snap as usize]
                                },
                            ));
                            for (event, offers) in offers.iter_mut().enumerate() {
                                offers.push(crate::just_bound::Offer {
                                    snap,
                                    power,
                                    weight: component.rows[member * width + (snap + 1) as usize]
                                        [event],
                                });
                            }
                        }
                        powers.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
                        powers.truncate(5);
                        powers.shrink_to_fit();
                        bonuses.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
                        bonuses.truncate(5);
                        bonuses.shrink_to_fit();

                        other_bonuses.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
                        other_bonuses.truncate(5);
                        other_bonuses.shrink_to_fit();
                        let offers = offers.map(crate::just_bound::witnesses);

                        let bytes = 256
                            + offers
                                .iter()
                                .map(|values| {
                                    values.capacity()
                                        * std::mem::size_of::<crate::just_bound::Offer>()
                                })
                                .sum::<usize>()
                            + (powers.capacity() + bonuses.capacity() + other_bonuses.capacity())
                                * std::mem::size_of::<(i32, f64)>();
                        let group = Arc::new(JustRewardOffers {
                            offers,
                            powers,
                            bonuses,
                            other_bonuses,
                        });
                        while tables.bytes + bytes > 16 * 1024 * 1024 {
                            let Some((old, size)) = tables.order.pop_front() else {
                                break;
                            };
                            tables.offers.remove(&old);
                            tables.bytes -= size;
                        }
                        tables.offers.insert(key, Arc::clone(&group));
                        tables.order.push_back((key, bytes));
                        tables.bytes += bytes;
                        group
                    };
                    let Some(&(_, power)) = group.powers.iter().find(|&&(snap, _)| available(snap))
                    else {
                        feasible = false;
                        break;
                    };
                    let Some(&(_, bonus)) =
                        group.bonuses.iter().find(|&&(snap, _)| available(snap))
                    else {
                        feasible = false;
                        break;
                    };
                    let Some(&(_, other)) = group
                        .other_bonuses
                        .iter()
                        .find(|&&(snap, _)| available(snap))
                    else {
                        feasible = false;
                        break;
                    };
                    maximum_other += other;
                    power_cap += power;
                    maximum_bonus += bonus;
                    weight_cap = add(
                        weight_cap,
                        group
                            .offers
                            .iter()
                            .flatten()
                            .filter(|point| available(point.snap))
                            .map(|point| point.weight)
                            .fold(0.0_f64, f64::max),
                    );
                    groups.push(Some(group));
                }
                if !feasible {
                    continue;
                }
                let weights: [[f64; 5]; 5] = std::array::from_fn(|slot| {
                    if slot < from {
                        let snap = assigned[slot];
                        component.rows[members[slot] * width + (snap + 1) as usize]
                    } else {
                        std::array::from_fn(|event| {
                            groups[slot].as_ref().unwrap().offers[event]
                                .iter()
                                .filter(|point| available(point.snap))
                                .map(|point| point.weight)
                                .fold(f64::NEG_INFINITY, f64::max)
                        })
                    }
                });
                let mut matrix = weights.map(|row| row.map(f64::to_bits));
                matrix.sort_unstable();
                let key = (
                    component.base.to_bits(),
                    component.converted.to_bits(),
                    matrix,
                );
                let tables = self.just_tables.as_mut().unwrap();
                let coefficients = if let Some(values) = tables.weight_orders.get(&key) {
                    Arc::clone(values)
                } else {
                    let weights = matrix.map(|row| row.map(f64::from_bits));
                    let mut values: Vec<_> = live::orders()
                        .iter()
                        .map(|order| {
                            let weight = order
                                .iter()
                                .enumerate()
                                .fold(component.base, |sum, (event, &slot)| {
                                    add(sum, weights[slot][event])
                                });
                            up(up(weight / component.converted))
                        })
                        .collect();
                    values.sort_unstable_by(f64::total_cmp);
                    let values = Arc::new(values);
                    let bytes = 320 + values.capacity() * 8;
                    while tables.weight_bytes + bytes > 8 * 1024 * 1024 {
                        let Some((old, bytes)) = tables.weight_order.pop_front() else {
                            break;
                        };
                        tables.weight_orders.remove(&old);
                        tables.weight_bytes -= bytes;
                    }
                    tables.weight_orders.insert(key, Arc::clone(&values));
                    tables.weight_order.push_back((key, bytes));
                    tables.weight_bytes += bytes;
                    values
                };
                let coarse_upper = self.projected_bound(
                    Totals {
                        bonus: totals.bonus.min(maximum_bonus),
                        ..totals
                    },
                    maximum_other,
                    &coefficients,
                );
                if self.below_limit(coarse_upper, limit) {
                    if from == 0 && coarse {
                        self.just_root_caps.insert(shape.base.id, coarse_upper);
                    }
                    upper = upper.max(coarse_upper);
                    continue;
                }
                let lambda = (weight_cap / power_cap.min(totals.power).max(1.0)).sqrt();
                if !lambda.is_finite() || lambda <= 0.0 {
                    return None;
                }
                let inverse = up(1.0 / lambda);
                let rows: [[f64; 5]; 5] = std::array::from_fn(|slot| {
                    std::array::from_fn(|event| {
                        if slot < from {
                            let snap = assigned[slot];
                            let power = self.snapshot_power[members[slot] * width
                                + if snap < 0 { width - 1 } else { snap as usize }];
                            return add(
                                mul(lambda, power),
                                mul(
                                    inverse,
                                    component.rows[members[slot] * width + (snap + 1) as usize]
                                        [event],
                                ),
                            );
                        }
                        groups[slot].as_ref().unwrap().offers[event]
                            .iter()
                            .filter(|point| available(point.snap))
                            .map(|point| add(mul(lambda, point.power), mul(inverse, point.weight)))
                            .fold(f64::NEG_INFINITY, f64::max)
                    })
                });
                let constant = add(
                    mul(lambda, self.assignment_power),
                    mul(inverse, component.base),
                );
                let mut scores: Vec<_> = live::orders()
                    .iter()
                    .map(|order| {
                        let price = order
                            .iter()
                            .enumerate()
                            .fold(constant, |sum, (event, &slot)| add(sum, rows[slot][event]));
                        up(mul(price, price) / (4.0 * component.converted))
                    })
                    .collect();
                scores.sort_unstable_by(f64::total_cmp);
                let value = self.projected_bound(
                    Totals {
                        power: 1.0,
                        skill: 0.0,
                        skill_low: 0.0,
                        bonus: totals.bonus.min(maximum_bonus),
                    },
                    maximum_other,
                    &scores,
                );
                if from == 0 && coarse {
                    self.just_root_caps.insert(shape.base.id, value);
                }
                upper = upper.max(value);
                if !self.below_limit(upper, limit) {
                    complete = false;
                    if from != 0 || !coarse {
                        break;
                    }
                }
            }
            if complete {
                return Some(upper);
            }
        }
        None
    }
    fn slot(&mut self, member: usize, snap: i32) -> Arc<live::Slot> {
        Arc::clone(self.slots.entry((member, snap)).or_insert_with(|| {
            let mut slot = if let Some(raw) = &self.raw_slots {
                raw.slots[member * (self.snaps.len() + 1) + (snap + 1) as usize].clone()
            } else {
                skills::resolve(
                    self.master,
                    &self.members[member],
                    (snap >= 0).then(|| &self.snaps[snap as usize]),
                    &mut Vec::new(),
                )
            };
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
    #[cfg(not(target_arch = "wasm32"))]
    fn prefetch_contexts(&mut self, team: &Team, runs: i32) -> Option<Vec<Arc<RewardContext>>> {
        if !(8..=32).contains(&runs) || self.gekiso.notes.len() > 2000 {
            return None;
        }
        let ids = std::array::from_fn(|i| self.performer_id(team.members[i], team.snaps[i]));
        self.prefetch_ids(ids, 0, runs)
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn prefetch_ids(
        &mut self,
        ids: [u32; 5],
        begin: i32,
        end: i32,
    ) -> Option<Vec<Arc<RewardContext>>> {
        if crate::parallel::workers(4) <= 1 {
            return None;
        }
        let ids = self.canonical_context_ids(ids);
        let mut contexts = vec![None; (end - begin) as usize];
        let mut missing = Vec::new();
        for seed in begin..end {
            if let Some((context, used)) = self.contexts.get_mut(&(false, seed, ids)) {
                *used = true;
                contexts[(seed - begin) as usize] = Some(Arc::clone(context));
            } else {
                missing.push(seed);
            }
        }
        if missing.len() < 2 {
            return None;
        }
        let jobs: Vec<_> = missing
            .chunks(missing.len().div_ceil(crate::parallel::workers(4)))
            .collect();
        let deck: Vec<_> = ids
            .iter()
            .map(|&id| self.performers[id as usize].clone())
            .collect();
        let master = self.master;
        let chart = &self.gekiso;
        let batches = crate::parallel::map(&jobs, |batch| {
            Ok::<_, ()>(
                batch
                    .iter()
                    .map(|&seed| (seed, gekiso::context_fixed_rush(master, chart, &deck, seed)))
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap();
        for (seed, result) in batches.into_iter().flatten() {
            if let Ok((context, rush)) = result {
                contexts[(seed - begin) as usize] =
                    Some(self.cache_context((false, seed, ids), context, rush));
            }
        }
        contexts.into_iter().collect()
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
    fn gauge_core_ids(&mut self, ids: [u32; 5]) -> [u32; 5] {
        if self.empty_luck.is_none() {
            return self.canonical_context_ids(ids);
        }
        if let Some(&canonical) = self.gauge_contexts.get(&ids) {
            return canonical;
        }
        let key = if let Some(signature) = self.phase_signature(ids) {
            GaugeCoreKey::Phases(signature)
        } else {
            let mut gauges = [(0, 0, 0i64); 5];
            let mut other = [0; 5];
            let (mut count, mut rest, mut speed) = (0, 0, 0i64);
            for id in ids {
                let state = if let Some(&state) = self.gauge_actors.get(&id) {
                    state
                } else {
                    let actor = &self.performers[id as usize];
                    let member_gauge = actor.gekisou_skill.is_some_and(|(id, level)| {
                        self.master.gekiso_skills[&id]
                            .effects
                            .iter()
                            .any(|row| row.level == level && row.kind == 11001)
                    });
                    let support_gauge = actor.gekisou_support_skills.iter().any(|&(id, level)| {
                        self.master.gekiso_support_skills[&id]
                            .effects
                            .iter()
                            .any(|row| row.level == level && row.kind == 11001)
                    });
                    let state = if support_gauge {
                        None
                    } else if let Some(row) =
                        crate::gekiso_objective::lottery_gauge_row(self.master, actor)
                    {
                        Some((
                            row.trigger_group,
                            row.release_group,
                            (row.value / 10000) as i64,
                            actor.gekisou_support_skills.is_empty(),
                        ))
                    } else if member_gauge {
                        None
                    } else {
                        Some((0, 0, 0, false))
                    };
                    self.gauge_actors.insert(id, state);
                    state
                };
                let Some((trigger, release, value, movable)) = state else {
                    return self.canonical_context_ids(ids);
                };
                speed += value;
                if 2 * speed + 1 >= 1 << 24 {
                    return self.canonical_context_ids(ids);
                }
                if movable {
                    if let Some(row) = gauges[..count]
                        .iter_mut()
                        .find(|row| row.0 == trigger && row.1 == release)
                    {
                        row.2 += value;
                    } else {
                        gauges[count] = (trigger, release, value);
                        count += 1;
                    }
                } else {
                    other[rest] = id;
                    rest += 1;
                }
            }
            gauges[..count].sort_unstable();
            GaugeCoreKey::Integer((gauges, other, count, rest))
        };
        let canonical = if let Some(&canonical) = self.gauge_cores.get(&key) {
            canonical
        } else {
            let canonical = self.canonical_context_ids(ids);
            let bytes = 2 * std::mem::size_of::<GaugeCoreKey>()
                + 128
                + 2 * match &key {
                    GaugeCoreKey::Phases(s) => s.capacity(),
                    _ => 0,
                };
            const CAPACITY: usize = 2 * 1024 * 1024;
            if bytes <= CAPACITY {
                while self.gauge_core_bytes + bytes > CAPACITY {
                    let Some((old, size)) = self.gauge_core_order.pop_front() else {
                        break;
                    };
                    self.gauge_cores.remove(&old);
                    self.gauge_core_bytes -= size;
                }
                self.gauge_cores.insert(key.clone(), canonical);
                self.gauge_core_order.push_back((key, bytes));
                self.gauge_core_bytes += bytes;
            }
            canonical
        };
        if self.gauge_contexts.len() >= 8192 {
            if let Some(old) = self.gauge_context_order.pop_front() {
                self.gauge_contexts.remove(&old);
            }
        }
        self.gauge_contexts.insert(ids, canonical);
        self.gauge_context_order.push_back(ids);
        canonical
    }
    // Native registration runs every member programme before every support programme.
    fn phase_signature(&self, ids: [u32; 5]) -> Option<String> {
        if self.empty_luck.is_none() || !self.context_fields.owner_local() || !self.stable_life {
            return None;
        }
        let mut gauges = BTreeMap::<(i32, i32), i64>::new();
        let mut members = Vec::new();
        let mut supports = Vec::new();
        let mut total = 0;
        for id in ids {
            let p = &self.performers[id as usize];
            for &(id, level) in &p.gekisou_support_skills {
                if self.master.gekiso_support_skills[&id]
                    .effects
                    .iter()
                    .any(|r| r.level == level && (r.kind == 11001 || r.cumulative_id != 0))
                {
                    return None;
                }
            }
            if p.gekisou_skill.is_some_and(|(id, level)| {
                self.master.gekiso_skills[&id].effects.iter().any(|row| {
                    row.level == level
                        && !((11000..=11005).contains(&row.kind)
                            || [3004, 4004].contains(&row.kind))
                })
            }) {
                return None;
            }
            if let Some(row) = crate::gekiso_objective::lottery_gauge_row(self.master, p) {
                let value = (row.value / 10000) as i64;
                total += value;
                if 2 * total + 1 >= 1 << 24 {
                    return None;
                }
                *gauges
                    .entry((row.trigger_group, row.release_group))
                    .or_default() += value;
            } else if let Some((skill, level)) = p.gekisou_skill {
                if self.master.gekiso_skills[&skill]
                    .effects
                    .iter()
                    .any(|r| r.level == level && r.kind == 11001)
                {
                    return None;
                }
                let mut member = p.clone();
                member.gekisou_support_skills.clear();
                members.push(member);
            }
            if !p.gekisou_support_skills.is_empty() {
                let mut support = p.clone();
                let mission = p.mission_only.filter(|&m| m != 0).or_else(|| {
                    p.gekisou_skill
                        .map(|(id, _)| self.master.gekiso_skills[&id].mission_type)
                });
                support.gekisou_skill = None;
                support.mission_only = mission;
                supports.push(support);
            }
        }
        if gauges.is_empty() {
            return None;
        }
        serde_json::to_string(&(gauges.into_iter().collect::<Vec<_>>(), members, supports)).ok()
    }
    fn context_ids(&mut self, ids: [u32; 5], seed: i32) -> Result<Arc<RewardContext>, String> {
        let ids = self.canonical_context_ids(ids);
        let key = (false, seed, ids);
        if let Some((context, used)) = self.contexts.get_mut(&key) {
            *used = true;
            return Ok(Arc::clone(context));
        }
        let deck: Vec<_> = ids
            .iter()
            .map(|&i| self.performers[i as usize].clone())
            .collect();
        let (simulation, rush_support) = if self.gekiso.luck {
            gekiso::context_fixed_rush(self.master, &self.gekiso, &deck, seed)?
        } else {
            (
                gekiso::context_fixed(self.master, &self.gekiso, &deck, &[seed])?,
                Vec::new(),
            )
        };
        Ok(self.cache_context(key, simulation, rush_support))
    }
    fn cache_context(
        &mut self,
        key: RewardContextKey,
        simulation: Context,
        rush_support: Vec<bool>,
    ) -> Arc<RewardContext> {
        let bytes = simulation.retained_bytes() + rush_support.capacity() + 256;
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
            projection_totals: OnceLock::new(),
            judgement_mask,
            score_projection,
            rush_support,
        });
        const CAPACITY: usize = 16 * 1024 * 1024;
        if bytes <= CAPACITY {
            while self.context_bytes + bytes > CAPACITY {
                let Some((old, bytes)) = self.context_order.pop_front() else {
                    break;
                };
                if let Some((_, used)) = self.contexts.get_mut(&old) {
                    if *used {
                        *used = false;
                        self.context_order.push_back((old, bytes));
                        continue;
                    }
                }
                self.contexts.remove(&old);
                self.context_bytes -= bytes;
            }
            self.context_bytes += bytes;
            self.context_order.push_back((key, bytes));
            self.contexts.insert(key, (Arc::clone(&context), false));
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
                                    && row.cumulative_id == 0
                                    && row.execute_limit == 0
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
            ids: self.gauge_core_ids(stripped),
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
        const CAPACITY: usize = 8 * 1024 * 1024;
        if (coefficients.len() + previous.map_or(0, |p| p.coefficients.len()))
            * std::mem::size_of::<f64>()
            > CAPACITY
        {
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
        let bytes = projection.coefficients.capacity() * std::mem::size_of::<f64>();
        if bytes > CAPACITY {
            return;
        }
        if let Some(old) = self.sampled_projections.remove(&key) {
            self.sampled_projection_bytes -=
                old.coefficients.capacity() * std::mem::size_of::<f64>();
            self.sampled_projection_order.retain(|old| old != &key);
        }
        while self.sampled_projection_bytes + bytes > CAPACITY {
            let Some(old) = self.sampled_projection_order.pop_front() else {
                break;
            };
            if let Some(old) = self.sampled_projections.remove(&old) {
                self.sampled_projection_bytes -=
                    old.coefficients.capacity() * std::mem::size_of::<f64>();
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
        // Each sustained Rush row adds/removes once per transition. Range
        // boundaries can also reset its remembered Rush state.
        let changes = core.simulation.rush_changes + 2 * self.gekiso.setup.fevers.len();
        let commands = changes * info.rows;
        let n = (factor_operations(&self.gekiso, ranges)
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
            let &(general, rush) = core.projection_totals.get().unwrap();
            let projection = Arc::new(OrderProjection {
                mean: up(coefficients.iter().copied().fold(0.0, add) / 120.0),
                coefficients,
                rush,
                general,
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
                                .map_or(0, |r| r.battle_required)
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
        let weights = crate::gekiso_objective::note_weights(self.master, &self.gekiso, ctx);
        context.projection_totals.get_or_init(|| {
            let rush = context
                .rush_support
                .iter()
                .enumerate()
                .filter(|(_, active)| **active)
                .map(|(i, _)| up(weights.general[i + 1] - weights.general[i]))
                .fold(0.0, add);
            (*weights.general.last().unwrap(), rush)
        });
        order_score_coefficients(self.master, &self.gekiso, &weights, slots).map(|coefficients| {
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
                let rank = chart::battle_rank(self.chart, score);
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
            if self.exact_cache.len() >= 128 {
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
                let weights = &upper.weights;
                if let Some(coefficients) = order_score_coefficients_with_base(
                    self.master,
                    &self.gekiso,
                    weights,
                    &slots,
                    upper.base,
                ) {
                    let scores: Vec<_> = coefficients
                        .iter()
                        .map(|&c| add(mul(power as f64, c), (self.gekiso.notes.len() + 4) as f64))
                        .collect();
                    let reward = self.projected_settlement(bonus, false, &scores);
                    let other = self.projected_settlement(other_bonus, true, &scores);
                    let required = self.reward_target.unwrap_or(cutoff.unwrap().key);
                    if self.lex_bound(
                        reward,
                        other,
                        up(scores.iter().copied().fold(0.0, add) / 120.0),
                    ) < required
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
                if self.lex_bound(
                    reward,
                    other,
                    up(scores.iter().copied().fold(0.0, add) / 120.0),
                ) < required
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
            let needs_secondary =
                self.reward_target.is_some() || self.other_target.is_some() || self.single_lex;
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
                    sums[1] = if needs_secondary {
                        self.projected_coefficient_sum(
                            other_bonus,
                            true,
                            projection,
                            power as f64,
                            0.0,
                        )
                        .min(completed as f64 * 120.0 * secondary_peak)
                    } else {
                        0.0
                    };
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
                    let secondary = if needs_secondary {
                        self.projected_coefficient_sum(
                            other_bonus,
                            true,
                            &projection,
                            power as f64,
                            offset,
                        )
                        .min(120.0 * secondary_peak)
                    } else {
                        0.0
                    };
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
        let prefix_plan = if self.gekiso.luck {
            gekiso::NoteScorePlan::new(&self.gekiso, &exact_slots)
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
        let mut witness_costs = Vec::new();
        #[cfg(not(target_arch = "wasm32"))]
        let prefetched = if self.gekiso.luck && cutoff.is_none() {
            self.prefetch_contexts(team, runs)
        } else {
            None
        };
        for seed in 0..runs {
            let before = comparison_sum;
            #[cfg(target_arch = "wasm32")]
            let context = self.context(team, seed)?;
            #[cfg(not(target_arch = "wasm32"))]
            let context = if let Some(contexts) = &prefetched {
                Arc::clone(&contexts[seed as usize])
            } else {
                self.context(team, seed)?
            };
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
                let bounded = cutoff.filter(|_| self.deterministic.is_some());
                let rewards: Vec<_> = self
                    .ranks
                    .iter()
                    .map(|&rank| (rank, self.value(bonus, rank)))
                    .collect();
                let peak = rewards.iter().map(|row| row.1).fold(0.0, f64::max);
                let bounded = bounded.filter(|_| {
                    rewards.iter().all(|row| safe(row.1) && row.1 >= 0.0) && safe(120.0 * peak)
                });
                if let Some(cutoff) = bounded {
                    let observed = Mutex::new((0.0, 0usize, true));
                    let observe = |score: f64, count: usize| {
                        let mut observed = observed.lock().unwrap();
                        let rank = chart::battle_rank(self.chart, score);
                        let reward = rewards.iter().find(|row| row.0 == rank).map(|row| row.1);
                        if !score.is_finite() || score < 0.0 || reward.is_none() {
                            observed.2 = false;
                        }
                        if !observed.2 {
                            return true;
                        }
                        observed.0 += reward.unwrap() * count as f64;
                        observed.1 += count;
                        let upper = (observed.0 + (120 - observed.1) as f64 * peak) / 120.0;
                        !(upper < cutoff.key || cutoff.prune_equal && upper == cutoff.key)
                    };
                    let Some(replayed) = gekiso::replay_orders_with_cutoff(
                        self.master,
                        &self.gekiso,
                        &deck,
                        &[seed],
                        power,
                        &observe,
                    )?
                    else {
                        return Ok(None);
                    };
                    replayed.scores
                } else {
                    gekiso::replay_orders(self.master, &self.gekiso, &deck, &[seed], power)?.scores
                }
            } else {
                let prepared = if let Some(plan) = &prefix_plan {
                    plan.score(self.master, &self.gekiso, &context.simulation, power)?
                } else {
                    None
                };
                if let Some(scores) = prepared {
                    scores
                } else {
                    gekiso::score_orders(
                        self.master,
                        &self.gekiso,
                        &context.simulation,
                        power,
                        &exact_slots,
                        true,
                    )?
                }
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
            witness_costs.push((seed, comparison_sum - before));
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
        if self.empty_luck.is_some() && self.reward_target.is_none() && !self.rank_other {
            let peak = self
                .ranks
                .iter()
                .map(|&rank| self.value(bonus, rank))
                .fold(0.0, f64::max)
                * 120.0;
            witness_costs.retain(|&(_, cost)| cost < peak);
            witness_costs.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
            let key = (team.members.clone(), team.snaps.clone());
            if !self.witness_candidates.contains_key(&key) {
                if self.witness_candidates.len() >= 64 {
                    if let Some(old) = self.witness_candidate_order.pop_front() {
                        self.witness_candidates.remove(&old);
                    }
                }
                self.witness_candidate_order.push_back(key.clone());
            }
            self.witness_candidates.insert(
                key,
                witness_costs
                    .into_iter()
                    .map(|(seed, cost)| (seed, peak - cost))
                    .collect(),
            );
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
    fn live_floor(&self) -> bool {
        self.live_floor
    }
    fn generic_assignment_product(&self) -> bool {
        self.member_combo_caps.is_none()
    }
    fn score_ties(&self) -> bool {
        self.single_lex
    }
    fn tie_rank(&self, hit: &crate::search::Hit) -> [f64; 2] {
        [
            crate::master::number(&hit.detail["settlement"]["otherMean"], 0.0),
            crate::master::number(&hit.detail["scores"]["mean"], 0.0),
        ]
    }
    fn observe_hit(&mut self, hit: &crate::search::Hit) {
        if self.reward_target.is_none() && !self.rank_other {
            if let Some(seeds) = self
                .witness_candidates
                .get(&(hit.team.members.clone(), hit.team.snaps.clone()))
            {
                for &(seed, gap) in seeds {
                    let entry = self.witness_gaps.entry(seed).or_insert(0.0);
                    *entry = entry.max(gap);
                }
                let mut ordered: Vec<_> = self
                    .witness_gaps
                    .iter()
                    .map(|(&seed, &gap)| (seed, gap))
                    .collect();
                ordered.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
                let seeds: Vec<_> = ordered.into_iter().map(|row| row.0).collect();
                if self.witness_seeds != seeds {
                    self.witness_seeds = seeds;
                    self.empty_upper = None;
                }
            }
        }
        if self.single_lex {
            let rank = self.tie_rank(hit);
            self.lex_incumbent = Some([hit.key, rank[0], rank[1]]);
        }
    }
    fn numeric_snap_dominance(
        &mut self,
        member: usize,
        better: i32,
        worse: i32,
        delta: f64,
    ) -> bool {
        if !self.allow_dominance || !self.canonical_slots || self.master.live.life_base <= 0.0 {
            return false;
        }
        let combo = self
            .gekiso
            .setup
            .missions
            .iter()
            .all(|&mission| mission == 1);
        if self.just_context.is_none() && !combo {
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
        if combo {
            return ai == bi;
        }
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
    fn reused_assignment_bound(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
        power: f64,
        limit: f64,
    ) -> Option<f64> {
        if self.member_combo_caps.is_none() || self.single_lex {
            return None;
        }
        let filtered = self.combo_member_snaps.iter().any(|&yes| !yes).then(|| {
            options
                .iter()
                .map(|row| {
                    row.iter()
                        .copied()
                        .filter(|&snap| self.combo_member_snaps[(snap + 1) as usize])
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        });
        self.prepare_assignment_certificate(members, filtered.as_deref().unwrap_or(options));
        self.certified_assignment_bound(power, limit)
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
        let just = self.just_reward_enabled();
        let ordered = just
            || self
                .gekiso
                .setup
                .missions
                .iter()
                .all(|&mission| mission == 1);
        let class = if let Some(&class) = self.member_dominance.get(&(better, worse)) {
            class
        } else {
            let mut exact = true;
            let mut dominates = true;
            for snap in -1..self.snaps.len() as i32 {
                if self.performer_id(better, snap) != self.performer_id(worse, snap) {
                    dominates = false;
                    exact = false;
                    break;
                }
                let a = self.slot(better, snap);
                let b = self.slot(worse, snap);
                if a.signature() != b.signature() {
                    exact = false;
                    if !ordered || !crate::just_bound::slot_dominates(&a, &b) {
                        dominates = false;
                        break;
                    }
                }
            }
            let class = if exact {
                MemberDominance::Equivalent
            } else if dominates {
                MemberDominance::Stronger
            } else {
                MemberDominance::Different
            };
            self.member_dominance.insert((better, worse), class);
            class
        };
        match class {
            MemberDominance::Equivalent => true,
            MemberDominance::Stronger => {
                ordered
                    && self.deterministic.as_ref().is_some_and(|bound| {
                        power_delta > bound.dominance_margin(self.power_estimate)
                    })
            }
            MemberDominance::Different => false,
        }
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
        self.primary_member_bonus[m]
    }
    fn snap_bonus(&self, s: usize) -> f64 {
        self.primary_snap_bonus[s]
    }
    fn bound(&mut self, t: Totals) -> f64 {
        self.total_bound(t, self.other_member_top + self.other_snap_top)
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
    fn member_snapshot_prices(&self) -> bool {
        self.member_combo_caps.is_some()
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
                    .map_or(0.0, |r| r.battle_required as f64)
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
        if self.member_combo_caps.is_some() || self.just_reward_enabled() {
            self.combo_characters = characters.to_vec();
        }
        if let Some(caps) = &self.member_combo_caps {
            self.combo_suffix = crate::search::suffix_tops(
                &characters
                    .iter()
                    .map(|group| {
                        group
                            .iter()
                            .map(|&member| caps[member])
                            .fold(f64::NEG_INFINITY, f64::max)
                    })
                    .collect::<Vec<_>>(),
            );
            self.combo_bonus_suffix.clear();
            let maximum = caps.iter().copied().fold(0.0_f64, f64::max);
            if maximum <= 64.0 && caps.iter().all(|&cap| cap >= 0.0 && cap.fract() == 0.0) {
                let width = 4 * maximum as usize + 1;
                self.combo_bonus_suffix =
                    vec![vec![vec![[f64::NEG_INFINITY; 2]; width]; 5]; characters.len() + 1];
                self.combo_bonus_suffix[characters.len()][0][0] = [0.0; 2];
                for from in (0..characters.len()).rev() {
                    self.combo_bonus_suffix[from] = self.combo_bonus_suffix[from + 1].clone();
                    for left in 1..=4 {
                        for &member in &characters[from] {
                            let cap = caps[member] as usize;
                            let bonus = [self.member_bonus(member), self.other_members[member]];
                            for sum in cap..width {
                                for (kind, &bonus) in bonus.iter().enumerate() {
                                    let previous = self.combo_bonus_suffix[from + 1][left - 1]
                                        [sum - cap][kind];
                                    if previous.is_finite() {
                                        let value = (previous + bonus).next_up();
                                        let best =
                                            &mut self.combo_bonus_suffix[from][left][sum][kind];
                                        *best = best.max(value);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            for model in self.combo_models.values_mut() {
                model.prepare_members(characters);
            }
        }
    }
    fn prepare_member_domains(&mut self, power: &[f64], options: &[Vec<i32>]) {
        if self.member_combo_caps.is_some() {
            self.member_power = power.to_vec();
            self.member_options = options.to_vec();
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
        if !self.bonus_ceilings_possible(members, &[]) {
            return Some(self.combo_discarded_upper);
        }
        let bonus_floor = if self.member_combo_caps.is_some()
            || (!self.points && self.reward_target.is_none() && self.just_reward_enabled())
        {
            let floor = self.bonus_floor(_limit).unwrap_or(f64::NEG_INFINITY);
            self.member_snapshot_floor(floor);
            floor
        } else {
            f64::NEG_INFINITY
        };
        let other = members.iter().map(|&i| self.other_members[i]).sum::<f64>()
            + self.member_suffix[from][left]
            + self.other_snap_top;
        let mut upper = self.total_bound(t, other);
        if let Some(bound) = self
            .deterministic
            .as_mut()
            .filter(|_| self.member_combo_caps.is_none())
        {
            let coefficients = bound.member_coefficients(members, from);
            upper = upper.min(self.projected_bound(t, other, &coefficients));
        }
        if upper < _limit {
            return Some(upper.max(self.combo_discarded_upper));
        }
        let other_floor = if self.rank_other && self.member_combo_caps.is_some() {
            self.other_bonus_floor(_limit)
        } else if self.other_target.is_some() {
            self.other_floor
        } else {
            f64::NEG_INFINITY
        };
        let mut pricing_model = None;
        if let Some(caps) = &self.member_combo_caps {
            let selected = members.iter().map(|&member| caps[member]).sum::<f64>();
            let mut remaining = self.combo_suffix[from][left];
            if !self.combo_bonus_suffix.is_empty() {
                let selected_bonus = [
                    members.iter().map(|&m| self.member_bonus(m)).sum::<f64>(),
                    members.iter().map(|&m| self.other_members[m]).sum::<f64>(),
                ];
                let floors = [bonus_floor, other_floor];
                for (kind, &floor) in floors.iter().enumerate() {
                    if !floor.is_finite() {
                        continue;
                    }
                    let required =
                        (floor - selected_bonus[kind] - self.combo_snap_bonus[kind]).next_down();
                    let possible = self.combo_bonus_suffix[from][left]
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(_, bonus)| bonus[kind] >= required)
                        .map(|(sum, _)| sum as f64);
                    let Some(possible) = possible else {
                        return Some(self.combo_discarded_upper);
                    };
                    remaining = remaining.min(possible);
                }
            }
            let combo = 1.0 + selected + remaining;
            if self
                .deterministic
                .as_ref()
                .is_some_and(|global| combo < global.combo_multiplier)
            {
                let key = ComboModelKey::Multiplier(combo.to_bits());
                pricing_model = Some(key.clone());
                if !self.combo_models.contains_key(&key) {
                    let prepared = self
                        .deterministic
                        .as_ref()
                        .and_then(|bound| bound.preparation.as_ref())
                        .map(Arc::clone);
                    if let Some(mut model) = prepared.and_then(|prepared| {
                        DeterministicBound::with_preparation(
                            self.master,
                            &self.gekiso,
                            self.members,
                            self.snaps,
                            combo,
                            prepared,
                        )
                    }) {
                        model.coefficient_limit = 256;
                        if !self.combo_member_snaps.is_empty() {
                            model.member_snapshots(&self.combo_member_snaps);
                        }
                        model.prepare_members(&self.combo_characters);
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
                    self.combo_model_order.push_back(key);
                }
            }
        }
        if self.member_combo_caps.is_some() && upper >= _limit && !self.member_power.is_empty() {
            let model = if let Some(key) = pricing_model.as_ref() {
                self.combo_models.get_mut(key)
            } else {
                self.deterministic.as_mut()
            };
            if let Some(model) = model {
                model.prepare_member_pricing(
                    members[0],
                    &self.combo_characters,
                    MemberPricingDomain {
                        power: &self.member_power,
                        snapshot_power: &self.snapshot_power,
                        options: &self.member_options,
                        member_bonus: &self.primary_member_bonus,
                        snap_bonus: &self.primary_snap_bonus,
                    },
                    bonus_floor,
                    t.power,
                );
                if let Some(pricing) = model.member_pricing.clone() {
                    upper = upper
                        .min(self.member_pricing_bound(&pricing, members, from, t, other, _limit));
                }
            }
        }
        if self.member_combo_caps.is_some() && upper >= _limit {
            let model = if let Some(key) = pricing_model.as_ref() {
                self.combo_models.get_mut(key)
            } else {
                self.deterministic.as_mut()
            };
            if let Some(model) = model {
                let coefficients = model.member_coefficients(members, from);
                upper = upper.min(self.projected_bound(t, other, &coefficients));
            }
        }
        Some(upper.max(self.combo_discarded_upper))
    }
    fn roster_bound(&mut self, members: &[usize], mut totals: Totals, limit: f64) -> Option<f64> {
        self.member_combo_caps.as_ref()?;
        let bonus = members
            .iter()
            .map(|&member| self.member_bonus(member))
            .sum::<f64>()
            + self.combo_snap_bonus[0];
        totals.bonus = totals.bonus.min(bonus);
        self.member_bound_at(members, totals, 0, self.combo_characters.len(), limit)
    }
    fn assignment_domains(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
    ) -> Option<Vec<Vec<Vec<i32>>>> {
        if self.reward_target.is_some() || self.rank_other || self.witness_seeds.is_empty() {
            return None;
        }
        let programs = self.empty_luck.as_ref()?;
        options
            .iter()
            .enumerate()
            .map(|(position, choices)| {
                let mut plain = Vec::new();
                let mut groups = BTreeMap::<String, Vec<i32>>::new();
                for &snap in choices {
                    if snap < 0 || !programs.rng_snaps[snap as usize] {
                        plain.push(snap);
                    } else {
                        groups
                            .entry(programs.support_key(members[position], snap)?.to_owned())
                            .or_default()
                            .push(snap);
                    }
                }
                let mut domains = Vec::new();
                if !plain.is_empty() {
                    domains.push(plain);
                }
                domains.extend(groups.into_values());
                Some(domains)
            })
            .collect()
    }
    fn prepare_assignment_domain(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
        complete: bool,
    ) {
        self.prepare_assignment(members, options);
        if complete {
            if let Some(programs) = &self.empty_luck {
                let supports: Vec<_> = options
                    .iter()
                    .enumerate()
                    .filter_map(|(position, row)| {
                        let snap = row[0];
                        (snap >= 0 && programs.rng_snaps[snap as usize]).then_some((position, snap))
                    })
                    .collect();
                if supports.len() <= 2 {
                    self.empty_options = Some(options.to_vec());
                    self.lottery_support = supports;
                }
            }
        }
    }
    fn prepare_assignment(&mut self, _members: &[usize], options: &[Vec<i32>]) {
        self.empty_options = None;
        self.lottery_support.clear();
        self.empty_root_bonus = None;
        self.empty_upper = None;
        self.assignment_combo = None;
        let filtered = self.combo_member_snaps.iter().any(|&yes| !yes).then(|| {
            options
                .iter()
                .map(|row| {
                    row.iter()
                        .copied()
                        .filter(|&snap| self.combo_member_snaps[(snap + 1) as usize])
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        });
        let numeric_options = filtered.as_deref().unwrap_or(options);
        self.prepare_assignment_certificate(_members, numeric_options);
        self.assignment_filtered_empty = numeric_options.iter().any(Vec::is_empty);
        if self.just_reward_enabled() {
            self.just_root_caps.clear();
            self.just_assignment_ready = false;
            for (slot, choices) in options.iter().enumerate() {
                self.just_options[slot].clone_from(choices);
            }
        }

        if let Some(global) = &self.deterministic {
            let shared_preparation = global.preparation.as_ref().map(Arc::clone);
            let scripted = self.stable_life && self.gekiso.setup.missions.iter().all(|&m| m == 1);
            let formation = (scripted || global.combo_multiplier != 1.0).then(|| {
                _members
                    .iter()
                    .map(|&m| self.members[m].clone())
                    .collect::<Vec<_>>()
            });
            let combo = formation.as_ref().map_or(1.0, |formation| {
                maximum_combo(self.master, formation, &self.gekiso.setup.missions)
            });
            let script = if scripted {
                self.combo_scripts
                    .get_or_insert_with(|| {
                        crate::reward_trace::Scripts::new(self.master, self.members)
                    })
                    .select(_members)
            } else {
                None
            };
            let key = if let Some(actors) = &script {
                Some(ComboModelKey::Script(
                    crate::reward_trace::combo_key(self.master, actors),
                    combo.to_bits(),
                ))
            } else if combo != global.combo_multiplier {
                Some(ComboModelKey::Multiplier(combo.to_bits()))
            } else {
                None
            };
            if let Some(key) = key {
                if !self.combo_models.contains_key(&key) {
                    let model =
                        if let (ComboModelKey::Script(key, _), Some(actors)) = (&key, &script) {
                            self.combo_trace(key, actors).and_then(|trace| {
                                DeterministicBound::with_combo_trace(
                                    self.master,
                                    &self.gekiso,
                                    self.members,
                                    self.snaps,
                                    combo,
                                    trace,
                                    shared_preparation.as_ref().map(Arc::clone)?,
                                )
                            })
                        } else {
                            shared_preparation
                                .as_ref()
                                .map(Arc::clone)
                                .and_then(|prepared| {
                                    DeterministicBound::with_preparation(
                                        self.master,
                                        &self.gekiso,
                                        self.members,
                                        self.snaps,
                                        combo,
                                        prepared,
                                    )
                                })
                        };
                    self.trim_combo_traces();
                    if let Some(mut model) = model {
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
            bound.prepare_assignment(_members, numeric_options, power, bonus, estimate, maximum);
        }
        if self.reward_target.is_none() && !self.single_lex {
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
        if from == 5 && self.member_combo_caps.is_some() {
            let mean = self.assignment_model()?.score_ceiling(
                members,
                assigned,
                5,
                t.power,
                crate::objectives::Criterion::Mean,
            );
            let other = members
                .iter()
                .map(|&member| self.other_members[member])
                .sum::<f64>()
                + assigned
                    .iter()
                    .filter(|&&snap| snap >= 0)
                    .map(|&snap| self.other_snaps[snap as usize])
                    .sum::<f64>();
            return Some(self.projected_mean_bound(t.bonus, other, mean, f64::NEG_INFINITY));
        }
        if self.reward_target.is_none() && !self.single_lex {
            return None;
        }
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
        let certificate = (from == 0).then_some(self.assignment_certificate).flatten();
        if from == 0 {
            if let Some(bound) = self.certified_assignment_bound(self.assignment_power, limit) {
                return Some(bound);
            }
        }
        if self.reward_target.is_none() && !self.rank_other {
            if let Some(upper) = self.empty_upper {
                if upper < limit {
                    return Some(upper);
                }
            } else if let Some(options) = self.empty_options.clone() {
                let bonus = if let Some(bonus) = self.empty_root_bonus {
                    bonus
                } else if from == 0 {
                    self.empty_root_bonus = Some(t.bonus);
                    t.bonus
                } else {
                    return None;
                };
                let values: Vec<_> = self
                    .ranks
                    .iter()
                    .map(|&rank| (rank, self.value(bonus, rank)))
                    .collect();
                let peak = values.iter().map(|row| row.1).fold(0.0, f64::max);
                let count = 120 * self.samples as usize;
                if values
                    .iter()
                    .all(|&(_, v)| safe(v) && v >= 0.0 && v.fract() == 0.0)
                    && safe(peak * count as f64)
                    && peak * count as f64 <= MAX_SAFE / 2.0
                {
                    let mut total = peak * count as f64;
                    for seed in self.witness_seeds.clone() {
                        let key = (
                            members.to_vec(),
                            options.clone(),
                            seed,
                            self.assignment_power.to_bits(),
                        );
                        if !self.empty_scores.contains_key(&key) {
                            let scores = self
                                .empty_luck
                                .as_mut()
                                .and_then(|programs| {
                                    programs.envelope(
                                        self.master,
                                        &self.gekiso,
                                        members,
                                        seed,
                                        &self.lottery_support,
                                    )
                                })
                                .and_then(|envelope| {
                                    let cards: Vec<_> = members
                                        .iter()
                                        .map(|&member| self.members[member].clone())
                                        .collect();
                                    let mut bound = DeterministicBound::with_luck_cap(
                                        self.master,
                                        &self.gekiso,
                                        &cards,
                                        self.snaps,
                                        1.0,
                                        None,
                                        Some(&envelope),
                                    )?;
                                    let width = self.snaps.len() + 1;
                                    let projected: Arc<Vec<f64>> = Arc::new(
                                        members
                                            .iter()
                                            .flat_map(|&member| {
                                                self.snapshot_power
                                                    [member * width..(member + 1) * width]
                                                    .iter()
                                                    .copied()
                                            })
                                            .collect(),
                                    );
                                    let indices: Vec<_> = (0..members.len()).collect();
                                    bound.prepare_assignment(
                                        &indices,
                                        &options,
                                        Arc::clone(&projected),
                                        Arc::clone(&self.primary_snap_bonus),
                                        self.power_estimate,
                                        self.bonus_max,
                                    );
                                    Some(
                                        bound
                                            .joint_score_caps(
                                                &indices,
                                                &projected,
                                                self.assignment_power,
                                                0.0,
                                                &[],
                                            )
                                            .to_vec(),
                                    )
                                });
                            if self.empty_scores.len() >= 32 {
                                if let Some(old) = self.empty_score_order.pop_front() {
                                    self.empty_scores.remove(&old);
                                }
                            }
                            self.empty_scores.insert(key.clone(), scores);
                            self.empty_score_order.push_back(key.clone());
                        }
                        if let Some(scores) = self.empty_scores[&key].as_ref() {
                            let reward = scores
                                .iter()
                                .map(|&score| {
                                    if !score.is_finite() {
                                        return peak;
                                    }
                                    values
                                        .iter()
                                        .filter(|&&(rank, _)| {
                                            rank == 2
                                                || self.chart.ranks.iter().any(|row| {
                                                    row.rank == rank
                                                        && row.battle_required as f64 <= score
                                                })
                                        })
                                        .map(|row| row.1)
                                        .fold(0.0, f64::max)
                                })
                                .sum::<f64>();
                            total -= peak * 120.0 - reward;
                        }
                        if total / (count as f64) < limit {
                            self.empty_upper = Some(total / count as f64);
                            return self.empty_upper;
                        }
                    }
                    self.empty_upper = Some(total / count as f64);
                }
            }
        }
        if self.assignment_filtered_empty
            || assigned[..from].iter().any(|&snap| {
                !self.combo_member_snaps.is_empty() && !self.combo_member_snaps[(snap + 1) as usize]
            })
        {
            return Some(self.combo_discarded_upper);
        }

        if !self.bonus_pair_possible(members, assigned, from, limit) {
            self.remember_assignment_certificate(certificate, limit, self.combo_discarded_upper);
            return Some(self.combo_discarded_upper);
        }
        let other = if self.reward_target.is_some() || self.single_lex {
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
            self.other_member_top + self.other_snap_top
        };
        if from > 0
            && self.member_combo_caps.is_some()
            && !self.prefix_mean_possible(members, assigned, from, t, other, limit)
        {
            return Some(self.combo_discarded_upper);
        }
        if self.member_combo_caps.is_some() {
            if let Some((coefficient, count)) = self
                .assignment_model()?
                .assignment_order_floor(members, assigned, from)
            {
                let upper = self.projected_limited(t, other, coefficient, count);
                if self.below_limit(upper, limit) {
                    let upper = upper.max(self.combo_discarded_upper);
                    self.remember_assignment_certificate(certificate, limit, upper);
                    return Some(upper);
                }
            }
        }
        let coefficients = self
            .assignment_model()?
            .assignment_coefficients(members, assigned, from);
        let mut upper = self.projected_bound(t, other, &coefficients);
        if self.below_limit(upper, limit) {
            let upper = upper.max(self.combo_discarded_upper);
            self.remember_assignment_certificate(certificate, limit, upper);
            return Some(upper);
        }
        if let Some(value) = self.just_reward_bound(members, t, assigned, from, limit) {
            upper = upper.min(value);
            if self.below_limit(upper, limit) {
                return Some(upper.max(self.combo_discarded_upper));
            }
        }
        if from == 0 && self.member_combo_caps.is_some() {
            let minimum = (self.bonus_floor(limit).unwrap_or(f64::NEG_INFINITY)
                - self.assignment_primary)
                .next_down();
            let power = Arc::clone(&self.snapshot_power);
            let base_power = self.assignment_power;
            let model = self.assignment_model()?;
            if model.joint_mean.is_none() {
                model.joint_reward_caps(members, &power, base_power, minimum, &[], true);
            }
            if let Some(pricing) = model.joint_mean.clone() {
                upper = upper.min(self.joint_mean_bound(&pricing, t, other, limit));
                if self.below_limit(upper, limit) {
                    let upper = upper.max(self.combo_discarded_upper);
                    self.remember_assignment_certificate(certificate, limit, upper);
                    return Some(upper);
                }
            }
            if let Some((regions, thresholds)) =
                self.snapshot_region_requirements(members, other, limit)
            {
                if !self.assignment_model()?.snapshot_regions_possible(
                    members,
                    base_power,
                    t.power,
                    &regions,
                    &thresholds,
                ) {
                    self.remember_assignment_certificate(
                        certificate,
                        limit,
                        self.combo_discarded_upper,
                    );
                    return Some(self.combo_discarded_upper);
                }
            }
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
                if !self.below_limit(partition_upper, limit) {
                    break;
                }
            }
            if self.below_limit(partition_upper, limit) {
                return Some(partition_upper.max(self.combo_discarded_upper));
            }
        }
        let root_dual = from == 0 && self.member_combo_caps.is_some();
        if root_dual {
            if let Some(targets) = self.joint_targets(limit) {
                let power = self.assignment_power;
                let bonus = self.assignment_primary;
                if !self
                    .assignment_model()?
                    .dual_possible(members, assigned, from, power, bonus, &targets)
                {
                    self.remember_assignment_certificate(
                        certificate,
                        limit,
                        self.combo_discarded_upper,
                    );
                    return Some(self.combo_discarded_upper);
                }
            }
        }
        let power = Arc::clone(&self.snapshot_power);
        let base_power = self.assignment_power;
        let scores = if from == 0 {
            let minimum_bonus = if self.member_combo_caps.is_some() && !self.single_lex {
                (self.bonus_floor(limit).unwrap_or(f64::NEG_INFINITY) - self.assignment_primary)
                    .next_down()
            } else {
                f64::NEG_INFINITY
            };
            let floors = if self.member_combo_caps.is_some() && !self.single_lex {
                [
                    (t.bonus, false, self.reward_target.unwrap_or(limit)),
                    (
                        other,
                        true,
                        if self.rank_other {
                            limit
                        } else {
                            self.other_target.unwrap_or(f64::NEG_INFINITY)
                        },
                    ),
                ]
                .into_iter()
                .filter(|(_, _, floor)| floor.is_finite())
                .filter_map(|(bonus, secondary, floor)| {
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
                                        .find(|row| row.rank == rank)
                                        .map_or(0, |row| row.battle_required)
                                },
                                if secondary {
                                    self.other(bonus, rank)
                                } else {
                                    self.value(bonus, rank)
                                },
                            )
                        })
                        .collect();
                    crate::event::RewardCurve::new(rows).map(|curve| (curve, floor))
                })
                .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            Some(self.assignment_model()?.joint_reward_caps(
                members,
                &power,
                base_power,
                minimum_bonus,
                &floors,
                false,
            ))
        } else {
            self.assignment_model()?.cached_joint_caps()
        };
        if let Some(scores) = scores {
            upper = upper.min(self.projected_bound(Totals { power: 1.0, ..t }, other, &scores));
            if self.below_limit(upper, limit) {
                let upper = upper.max(self.combo_discarded_upper);
                self.remember_assignment_certificate(certificate, limit, upper);
                return Some(upper);
            }
        }
        if self.member_combo_caps.is_some() && from == 0 {
            if let Some(pricing) = self.assignment_model()?.joint_rewards.clone() {
                upper = upper.min(self.joint_bonus_bound(&pricing, t, other, limit));
                if self.below_limit(upper, limit) {
                    let upper = upper.max(self.combo_discarded_upper);
                    self.remember_assignment_certificate(certificate, limit, upper);
                    return Some(upper);
                }
            }
        }
        if !root_dual {
            if let Some(targets) = self.joint_targets(limit) {
                let power = self.assignment_power;
                let bonus = self.assignment_primary;
                if !self
                    .assignment_model()?
                    .dual_possible(members, assigned, from, power, bonus, &targets)
                {
                    return Some(self.combo_discarded_upper);
                }
            }
        }
        Some(upper.max(self.combo_discarded_upper))
    }
    fn leaf_bound(&mut self, team: &Team, t: Totals) -> Option<f64> {
        if team.snaps.iter().any(|&snap| {
            !self.combo_member_snaps.is_empty() && !self.combo_member_snaps[(snap + 1) as usize]
        }) {
            return Some(self.combo_discarded_upper);
        }
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
    order_score_coefficients_with_base(master, chart, weights, slots, weights.base)
}
pub(crate) fn order_score_coefficients_with_base(
    master: &Master,
    chart: &gekiso::Chart,
    weights: &crate::gekiso_objective::Weights,
    slots: &[Arc<live::Slot>],
    base: f64,
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
                let mut coefficient = add(base, residual);
                for (event, &slot) in order.iter().enumerate() {
                    coefficient = add(coefficient, events[slot][event]);
                }
                coefficient = mul(coefficient, rounding);
                up(coefficient / chart.full.converted as f64)
            })
            .collect(),
    )
}
