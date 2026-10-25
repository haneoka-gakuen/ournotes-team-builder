//! Native CB timelines and additive support commands for deterministic bounds.
use crate::{
    full::{
        conditions::Performer,
        core, effects,
        model::{self, ComboSupportFrame, Model},
        score,
    },
    gekiso,
    master::{Condition, Cumulative, GekisoSkill, Master, SkillEffect},
    num::f,
    power::Member,
};
use rustc_hash::FxHashMap as HashMap;
use std::sync::{Arc, Mutex};

pub(crate) struct Scripts {
    members: Vec<Option<Vec<Performer>>>,
}
impl Scripts {
    pub(crate) fn new(master: &Master, members: &[Member]) -> Self {
        let fields = crate::gekiso_objective::read_attributes(master, false);
        Self {
            members: members
                .iter()
                .map(|member| script(master, std::slice::from_ref(member), &fields))
                .collect(),
        }
    }
    pub(crate) fn select(&self, members: &[usize]) -> Option<Vec<Performer>> {
        let mut out = Vec::new();
        for &member in members {
            out.extend(self.members[member].as_ref()?.iter().cloned());
        }
        out.sort_by_cached_key(|p| serde_json::to_string(p).unwrap());
        Some(out)
    }
}
fn script(master: &Master, members: &[Member], fields: &[&str]) -> Option<Vec<Performer>> {
    let mut out = Vec::new();
    for member in members {
        let mut p = Performer::from_member(master, member, None).gekiso_only();
        let Some((id, level)) = p.gekisou_skill else {
            continue;
        };
        let skill = master.gekiso_skills.get(&id)?;
        if ![1, 4].contains(&skill.mission_type) {
            continue;
        }
        let rows: Vec<_> = skill.effects.iter().filter(|r| r.level == level).collect();
        if !rows.iter().any(|r| r.kind == 12000) {
            continue;
        }
        let reads_owner = rows.iter().any(|row| {
            row.cumulative_id != 0
                || [
                    row.trigger_group,
                    row.condition_group,
                    row.release_group,
                    row.reset_group,
                ]
                .iter()
                .any(|group| {
                    master
                        .condition_sets
                        .get(group)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .filter_map(|id| master.conditions.get(id))
                        .any(|c| c.kind == 5000)
                })
        });
        for row in rows {
            if ![12000, 3004, 4004].contains(&row.kind) {
                return None;
            }
            for group in [
                row.trigger_group,
                row.condition_group,
                row.release_group,
                row.reset_group,
            ] {
                for condition in master
                    .condition_sets
                    .get(&group)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|id| master.conditions.get(id))
                {
                    if ![0, 7005, 7020, 7010, 7013, 2000, 2001, 5000].contains(&condition.kind)
                        || condition.kind == 7005 && !condition.positive
                    {
                        return None;
                    }
                }
            }
        }
        if !reads_owner || !fields.contains(&"bandId") {
            p.band_id = 0;
        }
        if !reads_owner || !fields.contains(&"cardType") {
            p.card_type = 0;
        }
        if !reads_owner || !fields.contains(&"characterId") {
            p.character_id = 0;
        }
        if !reads_owner || !fields.contains(&"tagIds") {
            p.tag_ids.clear();
        }
        if !reads_owner || !fields.contains(&"liveSkillCategories") {
            p.live_skill_categories.clear();
        }
        if !reads_owner || !fields.contains(&"gekisouSkillCategories") {
            p.gekisou_skill_categories.clear();
        }
        if !reads_owner || !fields.contains(&"gekisouMissionType") {
            p.gekisou_mission_type = 0;
        }
        out.push(p);
    }
    // CB changes are integer additions; equal-time actors have the same total.
    out.sort_by_cached_key(|p| serde_json::to_string(p).unwrap());
    Some(out)
}

type ComboSupportCache = Mutex<HashMap<Vec<[i32; 5]>, Vec<f64>>>;
pub(crate) fn combo_key(master: &Master, script: &[Performer]) -> String {
    let canonical = (|| -> Option<String> {
        let mut groups = std::collections::BTreeMap::<String, i32>::new();
        let mut total = 0i64;
        for actor in script {
            let group = |id: i32| -> Option<serde_json::Value> {
                if id == 0 {
                    return Some(serde_json::json!(["absent"]));
                }
                let sets = master.condition_sets.get(&id)?;
                if sets.len() != 1 {
                    return None;
                }
                let mut out = Vec::new();
                for id in &sets[0] {
                    let c = master.conditions.get(id)?;
                    let fixed = match c.kind {
                        0 => Some(true),
                        5000 => Some(
                            c.targets.iter().any(|id| {
                                master.targets.get(id).is_some_and(|target| {
                                    crate::full::conditions::performer_matches(actor, target)
                                })
                            }) == c.positive,
                        ),
                        2000 | 2001 => {
                            let value = *c.values.first()?;
                            let test = |life: f64| {
                                if c.kind == 2000 {
                                    life > value
                                } else {
                                    life >= value
                                }
                            };
                            if test(master.live.life_base) != test(2.0 * master.live.life_base) {
                                return None;
                            }
                            Some(test(master.live.life_base) == c.positive)
                        }
                        7005 | 7010 | 7013 | 7020 => None,
                        _ => return None,
                    };
                    match fixed {
                        Some(false) => return Some(serde_json::json!(["false"])),
                        Some(true) => {}
                        None => {
                            out.push(serde_json::json!([c.kind, c.positive, c.values, c.targets]))
                        }
                    }
                }
                Some(serde_json::json!(["and", out]))
            };
            let (id, level) = actor.gekisou_skill?;
            let skill = master.gekiso_skills.get(&id)?;
            if skill.mission_type != 1 {
                return None;
            }
            for row in skill.effects.iter().filter(|row| row.level == level) {
                if [3004, 4004].contains(&row.kind) {
                    continue;
                }
                if row.kind != 12000
                    || row.value < 0
                    || row.cumulative_id != 0
                    || row.seconds != 0.0
                {
                    return None;
                }
                let trigger = group(row.trigger_group)?;
                let condition = group(row.condition_group)?;
                if trigger == serde_json::json!(["false"])
                    || condition == serde_json::json!(["false"])
                {
                    continue;
                }
                let key = serde_json::to_string(&serde_json::json!([
                    row.trigger_type,
                    row.execute_limit,
                    trigger,
                    condition,
                    group(row.release_group)?,
                    group(row.reset_group)?,
                    row.targets,
                    row.limit_count,
                    row.max_value
                ]))
                .ok()?;
                total += row.value as i64;
                if total + 1 >= 1 << 24 {
                    return None;
                }
                let value = groups.entry(key).or_default();
                *value = (*value as i64 + row.value as i64) as i32;
            }
        }
        serde_json::to_string(&groups).ok()
    })();
    canonical.map_or_else(
        || format!("raw:{}", serde_json::to_string(script).unwrap()),
        |key| format!("sum:{key}"),
    )
}

pub(crate) struct ComboTrace {
    master: Master,
    actors: Vec<Performer>,
    pub gk: Vec<f64>,
    pub maxima: Vec<f64>,
    support_cache: Option<ComboSupportCache>,
    support_frames: Option<Vec<ComboSupportFrame>>,
}
impl ComboTrace {
    pub fn new(master: &Master, chart: &gekiso::Chart, actors: &[Performer]) -> Option<Self> {
        let mut model = Model::new(
            master,
            actors,
            &chart.full.notes,
            &chart.full.events,
            chart.params(),
            Some(&chart.setup),
            (0, false),
        )
        .ok()?;
        model.score.score.calculator.factors_only = true;
        chart.fixed_life(&mut model);
        if chart.setup.missions.iter().all(|&mission| mission == 1)
            && actors.iter().all(|actor| {
                actor.live_skill.is_none()
                    && actor.support_skills.is_empty()
                    && actor.gekisou_support_skills.is_empty()
            })
            && combo_key(master, actors).starts_with("sum:")
        {
            model.observe_combo_support().ok()?;
        }
        let mut maxima = vec![0.0_f64; chart.setup.fevers.len()];
        for k in chart.span.0..chart.span.1 {
            let frame = &chart.play.frames[k];
            model
                .frame(frame.time_ms, &frame.judged, chart.play.dt[k])
                .ok()?;
            if let Some(gk) = &model.gk {
                for (i, state) in gk.controller.states.iter().enumerate() {
                    maxima[i] = maxima[i].max(state.combo as f64);
                }
            }
        }
        let index: HashMap<_, _> = chart
            .notes
            .iter()
            .enumerate()
            .map(|(i, note)| (note.id, i))
            .collect();
        let mut gk = vec![1.0; chart.notes.len()];
        for (id, observation) in &model.score.score.calculator.observations {
            if let Some(&index) = index.get(id) {
                gk[index] = observation.gk;
            }
        }
        Some(Self {
            master: master.clone(),
            actors: actors.to_vec(),
            gk,
            maxima,
            support_cache: None,
            support_frames: model.take_combo_support(),
        })
    }
    pub(crate) fn cache_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.actors.capacity() * std::mem::size_of::<Performer>()
            + self
                .actors
                .iter()
                .map(|actor| {
                    (actor.support_skills.capacity() + actor.gekisou_support_skills.capacity())
                        * std::mem::size_of::<(i32, i32)>()
                        + (actor.tag_ids.capacity()
                            + actor.live_skill_categories.capacity()
                            + actor.gekisou_skill_categories.capacity())
                            * std::mem::size_of::<i32>()
                })
                .sum::<usize>()
            + (self.gk.capacity() + self.maxima.capacity()) * std::mem::size_of::<f64>()
            + self.support_frames.as_ref().map_or(0, |frames| {
                frames.capacity() * std::mem::size_of::<ComboSupportFrame>()
                    + frames
                        .iter()
                        .map(|frame| frame.states.capacity() * std::mem::size_of::<[i32; 4]>())
                        .sum::<usize>()
            })
            + self.support_cache.as_ref().map_or(0, |cache| {
                let cache = cache.lock().unwrap_or_else(|error| error.into_inner());
                cache.capacity() * (std::mem::size_of::<(Vec<[i32; 5]>, Vec<f64>)>() + 1)
                    + cache
                        .iter()
                        .map(|(key, value)| {
                            key.capacity() * std::mem::size_of::<[i32; 5]>()
                                + value.capacity() * std::mem::size_of::<f64>()
                        })
                        .sum::<usize>()
            })
    }
    pub(crate) fn cache_combo_supports(&mut self) {
        self.support_cache = Some(Mutex::new(HashMap::default()));
    }
    pub(crate) fn prepare_supports(&self, chart: &gekiso::Chart, programs: &[&[[i32; 5]]]) {
        let Some(cache) = &self.support_cache else {
            return;
        };
        let mission = if chart.setup.missions.iter().all(|&m| m == 3) {
            3
        } else {
            1
        };
        let mut wanted: Vec<_> = programs
            .iter()
            .copied()
            .filter(|rows| {
                !rows.is_empty()
                    && rows
                        .iter()
                        .all(|row| row[3] == 7001 && (row[4] == 4 || row[4] == mission))
            })
            .collect();
        wanted.sort_unstable();
        wanted.dedup();
        if wanted.len() > 64 {
            return;
        }
        let missing: Vec<_> = {
            let cache = cache.lock().unwrap_or_else(|error| error.into_inner());
            wanted
                .iter()
                .copied()
                .filter(|rows| !cache.contains_key(*rows))
                .collect()
        };
        if missing.len() < 2 {
            return;
        }
        let Some(values) = self.support_programs(chart, &missing) else {
            return;
        };
        let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
        if cache.len() + missing.len() > 64 {
            cache.retain(|key, _| wanted.binary_search(&key.as_slice()).is_ok());
        }
        for (rows, value) in missing.into_iter().zip(values) {
            cache.insert(rows.to_vec(), value);
        }
    }
    pub fn support(&self, chart: &gekiso::Chart, rows: &[[i32; 5]]) -> Option<Vec<f64>> {
        let cache = self
            .support_cache
            .as_ref()
            .filter(|_| rows.iter().all(|row| row[3] == 7001));
        if let Some(cache) = cache {
            if let Some(value) = cache
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(rows)
            {
                return Some(value.clone());
            }
        }
        let value = self.support_uncached(chart, rows)?;
        if let Some(cache) = cache {
            let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
            if cache.len() >= 64 {
                cache.clear();
            }
            cache.insert(rows.to_vec(), value.clone());
        }
        Some(value)
    }
    fn support_uncached(&self, chart: &gekiso::Chart, rows: &[[i32; 5]]) -> Option<Vec<f64>> {
        self.support_programs(chart, &[rows])?.pop()
    }
    fn support_programs(
        &self,
        chart: &gekiso::Chart,
        programs: &[&[[i32; 5]]],
    ) -> Option<Vec<Vec<f64>>> {
        let Some(frames) = self.support_frames.as_ref().filter(|_| {
            programs
                .iter()
                .flat_map(|rows| rows.iter())
                .all(|row| row[3] == 7001 && (row[4] == 1 || row[4] == 4))
        }) else {
            return self.support_programs_native(chart, programs);
        };
        Self::recorded_support(chart, frames, programs)
            .or_else(|| self.support_programs_native(chart, programs))
    }
    fn recorded_support(
        chart: &gekiso::Chart,
        frames: &[ComboSupportFrame],
        programs: &[&[[i32; 5]]],
    ) -> Option<Vec<Vec<f64>>> {
        let mut commands = Vec::with_capacity(programs.len());
        for rows in programs {
            let mut values = Vec::new();
            let mut counts = vec![[0; 5]; rows.len()];
            let mut active = vec![[None; 5]; rows.len()];
            for frame in frames {
                for &[index, state, execute, finish] in &frame.states {
                    let index = index as usize;
                    for (row, &[value, maximum, unit, _, _]) in rows.iter().enumerate() {
                        if state == effects::END {
                            let mill: i32 = active[row][index].take()?;
                            if mill != 0 {
                                values.push((finish, mill.wrapping_neg()));
                            }
                            continue;
                        }
                        if state != effects::EXECUTE && (state != effects::EXECUTING || unit <= 0) {
                            continue;
                        }
                        let value = if unit > 0 {
                            let count = core::floor_i32(f(f(frame.combo as f64) / f(unit as f64)));
                            if state == effects::EXECUTING && counts[row][index] == count {
                                continue;
                            }
                            counts[row][index] = count;
                            let value = value.wrapping_mul(count);
                            if maximum > 0 {
                                value.min(maximum)
                            } else {
                                value
                            }
                        } else {
                            maximum
                        };
                        let factor = f(f(value as f64) / 10000.0);
                        let mill = score::note_factor_mill(factor);
                        if state == effects::EXECUTE {
                            active[row][index] = Some(mill);
                            if mill != 0 {
                                values.push((execute, mill));
                            }
                        } else {
                            let old = active[row][index]?;
                            let previous = if old == 0 {
                                0.0
                            } else {
                                f(f(old as f64) / 100000.0)
                            };
                            if !model::approximate(previous, factor) {
                                if old != 0 {
                                    values.push((frame.time, old.wrapping_neg()));
                                }
                                if mill != 0 {
                                    values.push((frame.time, mill));
                                }
                                active[row][index] = Some(mill);
                            }
                        }
                    }
                }
            }
            commands.push(values);
        }
        Some(Self::support_values(chart, commands))
    }
    fn support_programs_native(
        &self,
        chart: &gekiso::Chart,
        programs: &[&[[i32; 5]]],
    ) -> Option<Vec<Vec<f64>>> {
        let mission = if chart.setup.missions.iter().all(|&m| m == 3) {
            3
        } else {
            1
        };
        if programs
            .iter()
            .flat_map(|rows| rows.iter())
            .any(|r| r[4] != 4 && r[4] != mission)
        {
            return None;
        }
        let mut master = self.master.clone();
        let mut conditions = (*master.conditions).clone();
        let mut sets = (*master.condition_sets).clone();
        for (id, kind) in [(-1001, 7010), (-1002, 7013)] {
            conditions.insert(
                id,
                Condition {
                    id,
                    kind,
                    targets: Vec::new(),
                    values: Vec::new(),
                    positive: true,
                },
            );
            sets.insert(id, vec![vec![id]]);
        }
        master.conditions = Arc::new(conditions);
        master.condition_sets = Arc::new(sets);
        let mut cumulative = (*master.cumulative).clone();
        let mut skills = (*master.gekiso_support_skills).clone();
        let mut deck = self.actors.clone();
        let mut next = -2000;
        for (program, rows) in programs.iter().enumerate() {
            let mut effects = Vec::new();
            for (index, &[value, maximum, n, kind, _]) in rows.iter().enumerate() {
                let id = next;
                next -= 1;
                cumulative.insert(
                    id,
                    Cumulative {
                        id,
                        kind,
                        targets: if kind == 1000 {
                            vec![master.targets.values().find(|t| t.judgement == 6)?.id]
                        } else {
                            Vec::new()
                        },
                        values: vec![n as f64],
                        cap: 0,
                    },
                );
                effects.push(SkillEffect {
                    id: index as i32 + 1,
                    level: 1,
                    trigger_type: 1,
                    execute_limit: 0,
                    reset_group: 0,
                    kind: if n > 0 { 2001 } else { 2000 },
                    value: if n > 0 { value } else { maximum },
                    seconds: 0.0,
                    condition_group: 0,
                    trigger_group: -1001,
                    release_group: -1002,
                    cumulative_id: if n > 0 { id } else { 0 },
                    targets: Vec::new(),
                    limit_count: 0,
                    max_value: maximum,
                });
            }
            let skill = -1 - program as i32;
            skills.insert(
                skill,
                GekisoSkill {
                    mission_type: mission,
                    effects,
                },
            );
            deck.push(Performer {
                mission_only: Some(mission),
                gekisou_support_skills: vec![(skill, 1)],
                ..Performer::default()
            });
        }
        master.cumulative = Arc::new(cumulative);
        master.gekiso_support_skills = Arc::new(skills);
        let mut model = Model::new(
            &master,
            &deck,
            &chart.full.notes,
            &chart.full.events,
            chart.params(),
            Some(&chart.setup),
            (0, false),
        )
        .ok()?;
        model.score.score.calculator.factors_only = true;
        chart.fixed_life(&mut model);
        for k in chart.span.0..chart.span.1 {
            let frame = &chart.play.frames[k];
            model
                .frame(frame.time_ms, &frame.judged, chart.play.dt[k])
                .ok()?;
        }
        let mut commands = vec![Vec::new(); programs.len()];
        for (owner, time, value) in model.score.score.note_factor_commands_with_owner() {
            let member = owner / 100;
            if member >= self.actors.len() as i32 {
                commands[member as usize - self.actors.len()].push((time, value));
            }
        }
        Some(Self::support_values(chart, commands))
    }
    fn support_values(chart: &gekiso::Chart, commands: Vec<Vec<(i32, i32)>>) -> Vec<Vec<f64>> {
        commands
            .into_iter()
            .map(|mut commands| {
                commands.sort_by_key(|c| c.0);
                let (mut at, mut current) = (0, 0.0);
                chart
                    .times
                    .iter()
                    .map(|&time| {
                        while at < commands.len() && commands[at].0 <= time {
                            current += f(f(commands[at].1 as f64) / 100000.0);
                            at += 1;
                        }
                        current.max(0.0)
                    })
                    .collect()
            })
            .collect()
    }
}
