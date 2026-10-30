//! Steady JUST input windows after range-opening activation; scoring judgements stay unchanged.
use crate::{
    full::{
        conditions::{performer_matches, Performer},
        convert::Key,
        gekisou,
        model::Model,
    },
    gekiso::Chart,
    live,
    master::{Master, SkillEffect},
    power::{Member, Snap},
    search::Team,
};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

pub(crate) fn target(value: &Value) -> Result<Option<f64>, String> {
    if value.is_null() {
        return Ok(None);
    }
    match value.as_f64() {
        Some(0.0) => Ok(None),
        Some(value) if value.is_finite() && value > 0.0 => Ok(Some(value)),
        _ => Err("just-window-input".into()),
    }
}

pub(crate) fn delta(master: &Master, value: i32) -> i32 {
    let base = master
        .live
        .just_windows
        .values()
        .next()
        .map_or(0, |window| window.0);
    ((value as f32 / 10000.0) * base as f32).round_ties_even() as i32
}
#[derive(Clone)]
pub(crate) struct Trace {
    pub current: HashMap<i32, (i32, i32)>,
    active: HashMap<Key, Vec<(i32, i32, i32)>>,
}
impl Trace {
    pub(crate) fn new(master: &Master) -> Self {
        Self {
            current: master
                .live
                .just_windows
                .iter()
                .map(|(&key, &value)| (key, value))
                .collect(),
            active: HashMap::default(),
        }
    }
    pub(crate) fn enable(&mut self, master: &Master, key: Key, value: i32, targets: &[i32]) {
        if targets.first().copied().unwrap_or(3) > 6 {
            return;
        }
        let percent = value as f32 / 10000.0;
        let mut changed = Vec::new();
        for (&kind, &(base_before, base_after)) in &master.live.just_windows {
            let &(cap_before, cap_after) = master.live.miss_windows.get(&kind).unwrap_or(&(0, 0));
            let current = self.current.get_mut(&kind).unwrap();
            let before = ((base_before as f32 * percent).round_ties_even() as i32)
                .min(cap_before.saturating_sub(current.0))
                .max(0);
            let after = ((base_after as f32 * percent).round_ties_even() as i32)
                .min(cap_after.saturating_sub(current.1))
                .max(0);
            current.0 = current.0.wrapping_add(before);
            current.1 = current.1.wrapping_add(after);
            changed.push((kind, before, after));
        }
        self.active.insert(key, changed);
    }
    pub(crate) fn disable(&mut self, key: Key) {
        if let Some(changed) = self.active.remove(&key) {
            for (kind, before, after) in changed {
                let current = self.current.get_mut(&kind).unwrap();
                current.0 = current.0.wrapping_sub(before);
                current.1 = current.1.wrapping_sub(after);
            }
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Window {
    pub before_ms: i32,
    pub after_ms: i32,
    pub meets_target: bool,
}
pub(crate) fn aggregate<'a>(windows: impl Iterator<Item = &'a Value>) -> Option<Value> {
    let mut result: Option<Window> = None;
    for value in windows.filter(|window| window.is_object()) {
        let before = value["beforeMs"].as_i64().unwrap() as i32;
        let after = value["afterMs"].as_i64().unwrap() as i32;
        let meets = value["meetsTarget"].as_bool().unwrap();
        if let Some(result) = &mut result {
            result.before_ms = result.before_ms.min(before);
            result.after_ms = result.after_ms.min(after);
            result.meets_target &= meets;
        } else {
            result = Some(Window {
                before_ms: before,
                after_ms: after,
                meets_target: meets,
            });
        }
    }
    result.map(|window| serde_json::to_value(window).unwrap())
}

#[derive(Clone, Copy, Default)]
struct Profile {
    full: i32,
    timed: [i32; 4],
    dynamic: bool,
    random: bool,
}
impl Profile {
    fn extra(&self, mask: u8) -> i64 {
        (0..4)
            .filter(|&index| mask & (1 << index) != 0)
            .fold(self.full as i64, |sum, index| {
                sum + self.timed[index] as i64
            })
    }
}
#[derive(Clone, Copy)]
struct Probe {
    frame: usize,
    before: i32,
    after: i32,
    cap_before: i32,
    cap_after: i32,
    kind: i32,
    timed: u8,
    activated: bool,
}

pub(crate) struct Prepared {
    minimum: f64,
    width: usize,
    members: Arc<Vec<Member>>,
    snaps: Arc<Vec<Snap>>,
    profiles: Vec<Profile>,
    probes: Vec<Probe>,
    phases: Vec<Probe>,
    phase_values: Vec<Vec<f64>>,
    assignment: Option<Assignment>,
    seeds: Vec<i32>,
    cache: HashMap<(Vec<usize>, Vec<i32>, i32), Option<Window>>,
    force_model: bool,
    nominal: bool,
    dominance_safe: bool,
}

struct Assignment {
    members: Vec<usize>,
    best: Vec<[Vec<(i32, f64)>; 5]>,
}

fn random_row(master: &Master, row: &SkillEffect) -> bool {
    [
        row.trigger_group,
        row.condition_group,
        row.release_group,
        row.reset_group,
    ]
    .iter()
    .flat_map(|group| master.condition_sets.get(group))
    .flatten()
    .flatten()
    .any(|id| {
        master
            .conditions
            .get(id)
            .is_some_and(|condition| condition.kind == 4011)
    })
}

fn static_condition(master: &Master, group: i32, actor: &Performer) -> Option<bool> {
    if group == 0 {
        return Some(true);
    }
    let sets = master.condition_sets.get(&group)?;
    let mut any = false;
    for set in sets {
        let mut all = true;
        for id in set {
            let condition = master.conditions.get(id)?;
            if condition.kind != 5000 || !condition.values.is_empty() {
                return None;
            }
            let mut hit = false;
            for target in &condition.targets {
                hit |= performer_matches(actor, master.targets.get(target)?);
            }
            all &= hit == condition.positive;
        }
        any |= all;
    }
    Some(any)
}

fn profile_row(
    master: &Master,
    row: &SkillEffect,
    actor: &Performer,
    out: &mut Profile,
) -> Option<()> {
    if master.live.phases.get(&4004) != Some(&2) {
        return None;
    }
    if row.targets.len() != 1
        || master.targets.get(&row.targets[0])?.judgement != 6
        || row.value < 0
    {
        return None;
    }
    let base = *master.live.just_windows.values().next()?;
    if base.0 != base.1
        || master
            .live
            .just_windows
            .values()
            .any(|&window| window != base)
    {
        return None;
    }
    if row.cumulative_id != 0
        || row.limit_count != 0
        || row.execute_limit != 0
        || row.max_value != 0
        || row.reset_group != 0
        || row.release_group != 0
    {
        return None;
    }
    let sets = master.condition_sets.get(&row.trigger_group)?;
    if sets.len() != 1 || sets[0].len() != 1 {
        return None;
    }
    let trigger = master.conditions.get(&sets[0][0])?;
    if !trigger.positive || !trigger.values.is_empty() || trigger.targets.is_empty() {
        return None;
    }
    for target in &trigger.targets {
        if master.targets.get(target)?.mission_type != 3 {
            return None;
        }
    }
    let timed = if row.trigger_type == 1
        && trigger.kind == 7010
        && [2.0, 3.0, 4.0, 5.0].contains(&row.seconds)
    {
        Some(row.seconds as usize - 2)
    } else if row.trigger_type == 2 && trigger.kind == 7020 && row.seconds == 0.0 {
        None
    } else {
        return None;
    };
    if static_condition(master, row.condition_group, actor)? {
        let amount = delta(master, row.value);
        if let Some(index) = timed {
            out.timed[index] = out.timed[index].checked_add(amount)?;
        } else {
            out.full = out.full.checked_add(amount)?;
        }
    }
    Some(())
}

fn profile(master: &Master, chart: &Chart, actor: &Performer) -> Profile {
    let dynamic = actor
        .live_skill
        .into_iter()
        .chain(actor.support_skills.iter().copied())
        .enumerate()
        .any(|(index, (id, level))| {
            let skills = if index == 0 && actor.live_skill.is_some() {
                &master.live_skills
            } else {
                &master.support_skills
            };
            skills.get(&id).is_some_and(|rows| {
                rows.iter()
                    .any(|row| row.level == level && row.kind == 4004)
            })
        });
    let mut out = Profile {
        dynamic,
        ..Profile::default()
    };
    out.random =
        actor
            .live_skill
            .into_iter()
            .filter_map(|(id, level)| master.live_skills.get(&id).map(|rows| (rows, level)))
            .chain(actor.support_skills.iter().filter_map(|&(id, level)| {
                master.support_skills.get(&id).map(|rows| (rows, level))
            }))
            .any(|(rows, level)| {
                rows.iter()
                    .any(|row| row.level == level && random_row(master, row))
            });
    if actor.gekisou_skill.is_none() && actor.mission_only.unwrap_or(0) == 0 {
        return out;
    }
    let rows = actor
        .gekisou_skill
        .into_iter()
        .filter_map(|(id, level)| master.gekiso_skills.get(&id).map(|skill| (skill, level)))
        .chain(
            actor
                .gekisou_support_skills
                .iter()
                .filter_map(|&(id, level)| {
                    master
                        .gekiso_support_skills
                        .get(&id)
                        .map(|skill| (skill, level))
                }),
        );
    for (skill, level) in rows {
        if skill.mission_type != 4 && !chart.setup.missions.contains(&skill.mission_type) {
            continue;
        }
        out.random |= skill
            .effects
            .iter()
            .any(|row| row.level == level && random_row(master, row));
        for row in skill
            .effects
            .iter()
            .filter(|row| row.level == level && row.kind == 4004)
        {
            if profile_row(master, row, actor, &mut out).is_none() {
                out.dynamic = true;
            }
        }
    }
    out
}

impl Prepared {
    pub(crate) fn new(
        master: &Master,
        chart: &Chart,
        members: &[Member],
        snaps: &[Snap],
        minimum: f64,
        seeds: &[i32],
    ) -> Result<Self, String> {
        let width = snaps.len() + 1;
        let mut probes = Vec::new();
        if chart.setup.missions.contains(&3) {
            let mut empty = Model::new(
                master,
                &[],
                &chart.full.notes,
                &[],
                chart.params(),
                Some(&chart.setup),
                (0, false),
            )?;
            let mut starts = [usize::MAX; 3];
            let notes: HashMap<_, _> = chart
                .full
                .notes
                .iter()
                .map(|note| (note.id, note))
                .collect();
            let mut per_range = [Vec::new(), Vec::new(), Vec::new()];
            for (frame_index, frame) in chart.play.frames.iter().enumerate() {
                empty.frame(frame.time_ms, &frame.judged, chart.play.dt[frame_index])?;
                if let Some(gk) = &empty.gk {
                    for (range, state) in gk.controller.states.iter().enumerate().take(3) {
                        if state.state == gekisou::START {
                            starts[range] = starts[range].min(frame_index);
                        }
                    }
                }
                for judged in &frame.judged {
                    let note = notes[&judged.note_id];
                    if note.op != 1 {
                        continue;
                    }
                    for (range, &(start, end)) in chart.setup.fevers.iter().enumerate().take(3) {
                        if chart.setup.missions[range] != 3
                            || frame.time_ms < start
                            || frame.time_ms >= end
                        {
                            continue;
                        }
                        if master.live.just_windows.is_empty()
                            || !master.live.timing_types.contains(&note.judgement_type)
                        {
                            return Err("just-window-timing".into());
                        }
                        if !master.live.just_types.contains(&note.judgement_type) {
                            continue;
                        }
                        let &(before, after) = master
                            .live
                            .just_windows
                            .get(&note.judgement_type)
                            .ok_or("just-window-timing")?;
                        let activated = frame_index > starts[range];
                        let mut timed = 0u8;
                        for index in 0..4 {
                            let finish = start.saturating_add((index + 2) * 1000);
                            let end_frame =
                                chart.play.frames.partition_point(|f| f.time_ms < finish);
                            if frame_index <= end_frame {
                                timed |= 1 << index;
                            }
                        }
                        let (cap_before, cap_after) = master
                            .live
                            .miss_windows
                            .get(&note.judgement_type)
                            .copied()
                            .ok_or("just-window-timing")?;
                        per_range[range].push(Probe {
                            frame: frame_index,
                            before,
                            after,
                            cap_before,
                            cap_after,
                            kind: note.judgement_type,
                            timed,
                            activated,
                        });
                    }
                }
            }
            for rows in per_range {
                if rows.iter().any(|row| row.activated) {
                    probes.extend(rows.into_iter().filter(|row| row.activated));
                } else {
                    probes.extend(rows);
                }
            }
        }
        let mut phases: Vec<Probe> = Vec::new();
        for &probe in &probes {
            if let Some(previous) = phases.iter_mut().find(|p| {
                p.timed == probe.timed
                    && p.activated == probe.activated
                    && p.cap_before == probe.cap_before
                    && p.cap_after == probe.cap_after
                    && p.before == probe.before
                    && p.after == probe.after
            }) {
                previous.before = previous.before.min(probe.before);
                previous.after = previous.after.min(probe.after);
            } else {
                phases.push(probe);
            }
        }
        let mut profiles: Vec<Profile> = members
            .iter()
            .flat_map(|member| {
                snaps
                    .iter()
                    .map(Some)
                    .chain(std::iter::once(None))
                    .map(move |snap| {
                        profile(master, chart, &Performer::from_member(master, member, snap))
                    })
            })
            .collect();
        let maximum = profiles.iter().map(|row| row.extra(15)).max().unwrap_or(0) * 5;
        if phases.iter().any(|phase| {
            [
                (phase.before, phase.cap_before),
                (phase.after, phase.cap_after),
            ]
            .iter()
            .any(|&(base, cap)| cap > base && maximum > (cap - base) as i64)
        }) {
            for row in &mut profiles {
                if row.extra(15) != 0 {
                    row.dynamic = true;
                }
            }
        }
        if profiles.iter().all(|profile| !profile.dynamic) {
            let mut distinct: Vec<Probe> = Vec::new();
            for phase in phases {
                let redundant = distinct.iter().any(|old| {
                    old.before == phase.before
                        && old.after == phase.after
                        && old.cap_before == phase.cap_before
                        && old.cap_after == phase.cap_after
                        && old.activated == phase.activated
                        && profiles
                            .iter()
                            .all(|row| row.extra(old.timed) == row.extra(phase.timed))
                });
                if !redundant {
                    distinct.push(phase);
                }
            }
            phases = distinct;
        }
        let dominance_safe = profiles.iter().all(|profile| !profile.dynamic)
            && phases.iter().all(|phase| {
                let maximum = profiles
                    .iter()
                    .map(|row| {
                        if phase.activated {
                            row.extra(phase.timed)
                        } else {
                            0
                        }
                    })
                    .max()
                    .unwrap_or(0);
                maximum >= 0
                    && maximum.checked_mul(5).is_some_and(|extra| {
                        extra <= i32::MAX as i64
                            && phase.before as i64 + extra <= phase.cap_before as i64
                            && phase.after as i64 + extra <= phase.cap_after as i64
                    })
            });
        let phase_values: Vec<Vec<f64>> = phases
            .iter()
            .map(|phase| {
                profiles
                    .iter()
                    .map(|row| {
                        if row.dynamic {
                            f64::INFINITY
                        } else if !phase.activated {
                            0.0
                        } else {
                            row.extra(phase.timed) as f64
                        }
                    })
                    .collect()
            })
            .collect();
        Ok(Self {
            minimum,
            width,
            members: Arc::new(members.to_vec()),
            snaps: Arc::new(snaps.to_vec()),
            profiles,
            probes,
            phases,
            phase_values,
            assignment: None,
            seeds: if seeds.is_empty() {
                vec![0]
            } else {
                seeds.to_vec()
            },
            cache: HashMap::default(),
            force_model: false,
            nominal: seeds.is_empty(),
            dominance_safe,
        })
    }
    pub(crate) fn measure_full(
        &mut self,
        master: &Master,
        chart: &Chart,
        team: &Team,
        power: i32,
    ) -> Result<Option<Window>, String> {
        self.force_model = true;
        let result = self.measure(master, chart, team, power);
        self.force_model = false;
        result
    }
    pub(crate) fn active(&self) -> bool {
        !self.probes.is_empty()
    }
    pub(crate) fn snap_dominates(&self, member: usize, better: i32, worse: i32) -> bool {
        if better == worse || !self.active() {
            return true;
        }
        if !self.dominance_safe {
            return false;
        }
        let column = |snap: i32| {
            member * self.width
                + if snap < 0 {
                    self.width - 1
                } else {
                    snap as usize
                }
        };
        self.phase_values
            .iter()
            .all(|values| values[column(better)] >= values[column(worse)])
    }
    pub(crate) fn permutation_invariant(&self) -> bool {
        !self.active() || self.dominance_safe
    }
    pub(crate) fn member_dominates(&self, better: usize, worse: usize) -> bool {
        if better == worse || !self.active() {
            return true;
        }
        if !self.dominance_safe
            || (self.members[better].card.gekiso_skill_id == 0)
                != (self.members[worse].card.gekiso_skill_id == 0)
        {
            return false;
        }
        self.phase_values.iter().all(|values| {
            (0..self.width).all(|column| {
                values[better * self.width + column] >= values[worse * self.width + column]
            })
        })
    }
    pub(crate) fn pricing_constraints(&self) -> Option<Vec<(f64, &[f64])>> {
        if !self.dominance_safe || !self.active() {
            return None;
        }
        Some(
            self.phases
                .iter()
                .zip(&self.phase_values)
                .filter_map(|(phase, values)| {
                    let required =
                        (self.minimum - phase.before.min(phase.after) as f64).next_down();
                    (required > 0.0).then_some((required, values.as_slice()))
                })
                .collect(),
        )
    }
    pub(crate) fn impossible(&self) -> bool {
        self.phases
            .iter()
            .zip(&self.phase_values)
            .any(|(phase, values)| {
                let ceiling = values.iter().copied().fold(f64::NEG_INFINITY, f64::max) * 5.0;
                ceiling >= i32::MIN as f64
                    && ceiling <= i32::MAX as f64
                    && (phase.before as f64 + ceiling < self.minimum
                        || phase.after as f64 + ceiling < self.minimum)
            })
    }
    pub(crate) fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.assignment = None;
        if members.len() != 5 || options.len() != 5 || self.phases.is_empty() { return; }
        let best = self.phase_values.iter().map(|values| std::array::from_fn(|slot| {
            let mut row: Vec<_> = options[slot].iter().map(|&snap| {
                let col = if snap < 0 { self.width - 1 } else { snap as usize };
                (snap, values[members[slot] * self.width + col])
            }).collect();
            let compare = |a: &(i32, f64), b: &(i32, f64)| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0));
            if row.len() > 6 { row.select_nth_unstable_by(5, compare); row.truncate(6); }
            row.sort_unstable_by(compare); row
        })).collect();
        self.assignment = Some(Assignment { members: members.to_vec(), best });
    }
    pub(crate) fn possible_prepared(&self, members: &[usize], assigned: &[i32], from: usize, options: &[Vec<i32>]) -> bool {
        let Some(assignment) = self.assignment.as_ref().filter(|a| a.members == members) else {
            return self.possible(members, assigned, from, options);
        };
        for ((phase, values), best) in self.phases.iter().zip(&self.phase_values).zip(&assignment.best) {
            let mut upper = 0.0;
            for (slot, &member) in members.iter().enumerate() {
                upper += if slot < from {
                    let snap = assigned[slot];
                    values[member * self.width + if snap < 0 { self.width - 1 } else { snap as usize }]
                } else {
                    best[slot].iter().find(|(snap, _)| *snap < 0 || !assigned[..from].contains(snap))
                        .map_or(f64::NEG_INFINITY, |(_, value)| *value)
                };
            }
            if upper < i32::MIN as f64 || upper > i32::MAX as f64 { continue; }
            if phase.before as f64 + upper < self.minimum || phase.after as f64 + upper < self.minimum { return false; }
        }
        true
    }
    pub(crate) fn possible(
        &self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        options: &[Vec<i32>],
    ) -> bool {
        if self.phases.is_empty() || options.len() != members.len() {
            return true;
        }
        for (phase, values) in self.phases.iter().zip(&self.phase_values) {
            let mut upper = 0.0;
            for (slot, &member) in members.iter().enumerate() {
                let value = |snap: i32| {
                    values[member * self.width
                        + if snap < 0 {
                            self.width - 1
                        } else {
                            snap as usize
                        }]
                };
                let high = if slot < from {
                    value(assigned[slot])
                } else {
                    options[slot]
                        .iter()
                        .copied()
                        .filter(|snap| *snap < 0 || !assigned[..from].contains(snap))
                        .map(value)
                        .fold(f64::NEG_INFINITY, f64::max)
                };
                upper += high;
            }
            if upper < i32::MIN as f64 || upper > i32::MAX as f64 {
                continue;
            }
            if phase.before as f64 + upper < self.minimum
                || phase.after as f64 + upper < self.minimum
            {
                return false;
            }
        }
        true
    }
    pub(crate) fn allows(
        &mut self,
        master: &Master,
        chart: &Chart,
        team: &Team,
        power: i32,
    ) -> Result<bool, String> {
        Ok(self
            .measure(master, chart, team, power)?
            .is_none_or(|window| window.meets_target))
    }
    pub(crate) fn measure(
        &mut self,
        master: &Master,
        chart: &Chart,
        team: &Team,
        power: i32,
    ) -> Result<Option<Window>, String> {
        if self.probes.is_empty() {
            return Ok(None);
        }
        let mut full = 0i32;
        let mut timed = [0i32; 4];
        let mut dynamic = false;
        let mut random = false;
        for (&member, &snap) in team.members.iter().zip(&team.snaps) {
            let row = self.profiles[member * self.width
                + if snap < 0 {
                    self.width - 1
                } else {
                    snap as usize
                }];
            full = full.wrapping_add(row.full);
            for (sum, value) in timed.iter_mut().zip(row.timed) {
                *sum = sum.wrapping_add(value);
            }
            dynamic |= row.dynamic;
            random |= row.random;
        }
        let (mut before, mut after) = (i32::MAX, i32::MAX);
        if !dynamic && !self.force_model {
            for phase in &self.phases {
                let extra = if !phase.activated {
                    0
                } else {
                    (0..4)
                        .filter(|&index| phase.timed & (1 << index) != 0)
                        .fold(full, |sum, index| sum.wrapping_add(timed[index]))
                };
                before = before.min(
                    phase.before.wrapping_add(
                        extra
                            .min(phase.cap_before.saturating_sub(phase.before))
                            .max(0),
                    ),
                );
                after = after.min(
                    phase.after.wrapping_add(
                        extra
                            .min(phase.cap_after.saturating_sub(phase.after))
                            .max(0),
                    ),
                );
            }
        } else {
            if dynamic && self.nominal && (chart.luck || random) {
                return Err("just-window-random".into());
            }
            let key = (team.members.clone(), team.snaps.clone(), power);
            if let Some(value) = self.cache.get(&key) {
                return Ok(*value);
            }
            let deck: Vec<_> = team
                .members
                .iter()
                .zip(&team.snaps)
                .map(|(&member, &snap)| {
                    Performer::from_member(
                        master,
                        &self.members[member],
                        (snap >= 0).then(|| &self.snaps[snap as usize]),
                    )
                })
                .collect();
            let probe_frames: HashSet<_> = self.probes.iter().map(|probe| probe.frame).collect();
            for &seed in self
                .seeds
                .iter()
                .take(if dynamic { self.seeds.len() } else { 1 })
            {
                for order in live::orders().iter().take(if dynamic { 120 } else { 1 }) {
                    let events: Vec<_> = chart
                        .full
                        .events
                        .iter()
                        .map(|&(event, time)| (order[event], time))
                        .collect();
                    let mut params = chart.params();
                    params.power = power;
                    let mut model = Model::new(
                        master,
                        &deck,
                        &chart.full.notes,
                        &events,
                        params,
                        Some(&chart.setup),
                        (seed, false),
                    )?;
                    model.just_window = Some(Trace::new(master));
                    for (index, frame) in chart.play.frames.iter().enumerate() {
                        if probe_frames.contains(&index) {
                            for probe in self.probes.iter().filter(|probe| probe.frame == index) {
                                let current =
                                    model.just_window.as_ref().unwrap().current[&probe.kind];
                                before = before.min(if probe.activated {
                                    current.0
                                } else {
                                    probe.before
                                });
                                after = after.min(if probe.activated {
                                    current.1
                                } else {
                                    probe.after
                                });
                            }
                        }
                        model.frame(frame.time_ms, &frame.judged, chart.play.dt[index])?;
                    }
                }
            }
            let value = Some(Window {
                before_ms: before,
                after_ms: after,
                meets_target: before as f64 >= self.minimum && after as f64 >= self.minimum,
            });
            if self.cache.len() >= 128 {
                self.cache.clear();
            }
            self.cache.insert(key, value);
            return Ok(value);
        }
        Ok(Some(Window {
            before_ms: before,
            after_ms: after,
            meets_target: before as f64 >= self.minimum && after as f64 >= self.minimum,
        }))
    }
}
