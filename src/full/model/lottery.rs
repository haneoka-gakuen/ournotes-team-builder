//! Fixed-seed lottery replay using the whole-live condition and effect order.
use super::*;
use crate::gekiso::Chart;
pub(crate) type RushCommands = Vec<(i32, i32)>;
pub(crate) struct RushTrace {
    pub rush: RushCommands,
    pub support: RushCommands,
}
fn support_probe(master: &Master) -> Option<Performer> {
    for (&id, skill) in master
        .gekiso_support_skills
        .iter()
        .filter(|(_, skill)| skill.mission_type == 2)
    {
        let mut levels = std::collections::BTreeSet::new();
        for row in &skill.effects {
            levels.insert(row.level);
        }
        for level in levels {
            let rows = skill
                .effects
                .iter()
                .filter(|row| row.level == level)
                .collect::<Vec<_>>();
            let actor = Performer {
                mission_only: Some(2),
                gekisou_support_skills: vec![(id, level)],
                ..Performer::default()
            };
            let mut active = false;
            if rows.iter().all(|row| {
                if row.kind != 2000
                    || row.value <= 0
                    || row.trigger_type != 2
                    || row.seconds != 0.0
                    || row.cumulative_id != 0
                    || row.execute_limit != 0
                    || row.reset_group != 0
                    || row.release_group != 0
                {
                    return false;
                }
                if master
                    .condition_sets
                    .get(&row.trigger_group)
                    .is_none_or(|sets| {
                        sets.len() != 1
                            || sets[0].len() != 1
                            || master
                                .conditions
                                .get(&sets[0][0])
                                .is_none_or(|c| c.kind != 7021 || !c.positive)
                    })
                {
                    return false;
                }
                if row.condition_group == 0 {
                    active = true;
                    return true;
                }
                let Some(sets) = master.condition_sets.get(&row.condition_group) else {
                    return false;
                };
                let mut matched = false;
                for set in sets {
                    let mut all = true;
                    for id in set {
                        let Some(c) = master.conditions.get(id) else {
                            return false;
                        };
                        if c.kind != 5000 {
                            return false;
                        }
                        all &= c.targets.iter().any(|id| {
                            master.targets.get(id).is_some_and(|target| {
                                crate::full::conditions::performer_matches(&actor, target)
                            })
                        }) == c.positive;
                    }
                    matched |= all;
                }
                active |= matched;
                true
            }) && active
            {
                return Some(actor);
            }
        }
    }
    None
}

pub(crate) fn rush_windows(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
) -> Result<Option<Vec<RushTrace>>, String> {
    if seeds.is_empty() {
        return Ok(None);
    }
    if !chart.luck {
        return Ok(Some(
            seeds
                .iter()
                .map(|_| RushTrace {
                    rush: Vec::new(),
                    support: Vec::new(),
                })
                .collect(),
        ));
    }
    let generation = crate::cancellation_generation();
    let mut reduced: Vec<_> = deck.iter().map(Performer::gekiso_only).collect();
    let Some(probe) = support_probe(master) else {
        return Ok(None);
    };
    reduced.push(probe);
    let mut traces = Vec::with_capacity(seeds.len());
    let mut judged = Vec::new();
    let mut due = Vec::new();
    let mut params = chart.params();
    let length = params.music_length;
    // Score buckets only retain Rush commands; their timestamps stay intact.
    params.music_length = 0;
    let mut model = Model::new(
        master,
        &reduced,
        &chart.full.notes,
        &chart.full.events,
        params,
        Some(&chart.setup),
        (seeds[0], false),
    )?;
    chart.fixed_life(&mut model);
    model.params.music_length = length;
    if model
        .rows
        .iter()
        .any(|row| !(11000..=11005).contains(&row.kind) && ![2000, 3004, 4004].contains(&row.kind))
    {
        return Ok(None);
    }
    let mut initial = Some(model);
    for (index, &seed) in seeds.iter().enumerate() {
        if generation != crate::cancellation_generation() {
            return Err("cancelled".into());
        }
        let mut model = if index + 1 == seeds.len() {
            initial.take().unwrap()
        } else {
            initial.as_ref().unwrap().clone()
        };
        model.random = Random::new(seed);
        for k in chart.span.0..chart.span.1 {
            if k & 63 == 0 && generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
            let input = &chart.play.frames[k];
            frame(
                &mut model,
                input.time_ms,
                &input.judged,
                chart.play.dt[k],
                &mut judged,
                &mut due,
            )?;
            let g = model.gk.as_ref().unwrap();
            if g.controller
                .ranges
                .iter()
                .enumerate()
                .filter(|(_, range)| range.mission == gekisou::MISSION_LUCK)
                .all(|(i, _)| g.controller.states[i].state >= gekisou::FINISH)
            {
                break;
            }
        }
        let mut support = model.score.score.note_factor_commands();
        support.sort_by_key(|command| command.0);
        traces.push(RushTrace {
            rush: model.score.score.rush_factor_commands(),
            support,
        });
    }
    Ok(Some(traces))
}

fn frame(
    model: &mut Model<'_>,
    time: i32,
    input: &[JudgedNote],
    dt: f64,
    judged: &mut Vec<gekisou::GkNote>,
    due: &mut Vec<usize>,
) -> Result<(), String> {
    model.frame_time = time;
    model.frame_rank = model.previous_rank.take();
    let g = model.gk.as_mut().unwrap();
    g.fever_updates = g.fever.update(time);
    g.controller.before_update(
        dt,
        time,
        &g.fever_updates,
        0,
        &mut Environment {
            random: &mut model.random,
            handle: &mut model.score,
        },
    )?;
    due.clear();
    while model.fired < model.events.len() && model.events[model.by_time[model.fired]].1 <= time {
        due.push(model.by_time[model.fired]);
        model.fired += 1;
    }
    due.sort_unstable();
    model.frame_events.clear();
    model
        .frame_events
        .extend(due.iter().map(|&i| model.events[i]));
    model.judged.clear();
    judged.clear();
    let mut broken = false;
    for j in input {
        let n = model
            .notes
            .get(&j.note_id)
            .ok_or_else(|| format!("unknown note {}", j.note_id))?;
        let converted = model
            .conversion
            .convert(j.judgement, n.judgement_type, j.time_ms)?;
        model.combo.add(n.time_ms, converted);
        model.judged.push((n.id, converted, n.time_ms));
        judged.push((n.id, n.op, n.time_ms, converted));
        broken |= converted == 1 || converted == 2;
    }
    let combo = model.combo.at(time);
    let added = (combo - model.previous_combo).max(0);
    model.previous_combo = combo;
    if broken {
        model.current_combo = 0;
    }
    model.current_combo = model.current_combo.wrapping_add(added);
    for &(_, _, note_time, converted) in judged.iter() {
        model.life.note_damage(note_time, converted as usize);
        model.life.at(note_time);
    }
    for condition in &mut model.condition {
        condition.updater.begin_frame();
    }
    let mut listed = std::mem::take(&mut model.listed);
    let mut updated = std::mem::take(&mut model.updated);
    let input = FrameInput {
        time,
        music_length: model.params.music_length,
        finished: false,
    };
    for phase in 1..=2 {
        listed.clear();
        let mut ctx = Context {
            previous_rank: model.frame_rank,
            life: &mut model.life,
            random: &mut model.random,
            time,
            combo: model.current_combo,
            judged: &model.judged,
            events: &model.frame_events,
            gk: model.gk.as_ref().map(|g| GkView {
                controller: &g.controller,
                previous_lots: &g.previous_lots,
                previous_lot_ms: g.previous_lot_ms,
            }),
            record: false,
        };
        for (i, condition) in model.condition.iter_mut().enumerate() {
            updated.clear();
            condition
                .updater
                .update(phase, &input, &mut ctx, &mut updated)?;
            for &u in &updated {
                let updater = &condition.updater.updaters[u];
                let effect = &condition.updater.effects[updater.effect];
                if model.rows[effect.row].applies(updater.state.state) {
                    listed.push(Listed::Condition(i, u));
                }
            }
        }
        for &item in &listed {
            model.apply(item)?;
        }
    }
    model.listed = listed;
    model.updated = updated;
    model.life.sync(time);
    let g = model.gk.as_mut().unwrap();
    g.controller.update(
        time,
        judged,
        &g.fever_updates,
        0,
        &mut Environment {
            random: &mut model.random,
            handle: &mut model.score,
        },
    )?;
    g.previous_lots.clone_from(&g.controller.lot_results);
    g.previous_lot_ms = if g.previous_lots.is_empty() { 0 } else { time };
    if let Some(&i) = g
        .controller
        .updates
        .iter()
        .find(|&&i| g.controller.states[i].state == gekisou::COMPLETE)
    {
        if i < g.factors.len() {
            model.previous_rank = Some(g.ranks.get(i).copied().unwrap_or(1));
        }
    }
    Ok(())
}
