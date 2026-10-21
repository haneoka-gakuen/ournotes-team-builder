//! Native CB timelines and additive support commands for deterministic bounds.
use crate::{
    full::{conditions::Performer, model::Model},
    gekiso,
    master::{Condition, Cumulative, GekisoSkill, Master, SkillEffect},
    num::f,
    power::Member,
};
use std::sync::Arc;

pub(crate) fn script(master: &Master, members: &[Member]) -> Option<Vec<Performer>> {
    let fields = crate::gekiso_objective::read_attributes(master, false);
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

pub(crate) struct ComboTrace {
    master: Master,
    actors: Vec<Performer>,
    pub gk: Vec<f64>,
    pub maxima: Vec<f64>,
}
impl ComboTrace {
    pub fn new(master: &Master, chart: &gekiso::Chart, actors: &[Performer]) -> Option<Self> {
        let context = gekiso::context_fixed(master, chart, actors, &[0]).ok()?;
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
        Some(Self {
            master: master.clone(),
            actors: actors.to_vec(),
            gk: context.gk,
            maxima,
        })
    }
    pub fn support(&self, chart: &gekiso::Chart, rows: &[[i32; 5]]) -> Option<Vec<f64>> {
        let mission = if chart.setup.missions.iter().all(|&m| m == 3) {
            3
        } else {
            1
        };
        if rows.iter().any(|r| r[4] != 4 && r[4] != mission) {
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
        let mut effects = Vec::new();
        for (index, &[value, maximum, n, kind, _]) in rows.iter().enumerate() {
            let id = -2000 - index as i32;
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
        master.cumulative = Arc::new(cumulative);
        let mut skills = (*master.gekiso_support_skills).clone();
        skills.insert(
            -1,
            GekisoSkill {
                mission_type: mission,
                effects,
            },
        );
        master.gekiso_support_skills = Arc::new(skills);
        let mut deck = self.actors.clone();
        deck.push(Performer {
            mission_only: Some(mission),
            gekisou_support_skills: vec![(-1, 1)],
            ..Performer::default()
        });
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
        for k in chart.span.0..chart.span.1 {
            let frame = &chart.play.frames[k];
            model
                .frame(frame.time_ms, &frame.judged, chart.play.dt[k])
                .ok()?;
        }
        let mut commands = model.score.score.note_factor_commands();
        commands.sort_by_key(|c| c.0);
        let (mut at, mut current) = (0, 0.0);
        Some(
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
                .collect(),
        )
    }
}
