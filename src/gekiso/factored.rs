//! Native score replays share the judgement, LIFE and Gekiso control stream.
use super::*;
use crate::master::SkillEffect;

#[derive(PartialEq, Eq)]
struct Key {
    tables: [usize; 12],
    program: Vec<u64>,
    seeds: Vec<i32>,
    ranks: Vec<i32>,
    music_type: i32,
    events: Vec<(usize, i32)>,
}
struct Entry {
    key: Key,
    context: Arc<Scene>,
    bytes: usize,
    _master: Master,
}
struct Scene {
    draws: Vec<Arc<ScoreTape>>,
}
struct ScoreTape {
    bytes: Vec<u8>,
    offsets: Vec<usize>,
    base: i32,
}
impl ScoreTape {
    fn new(trace: &[ReplayCommand], chart: &Chart, base: i32) -> Self {
        let mut bytes = Vec::new();
        let mut instructions = Vec::new();
        let mut frame = 0;
        let mut offsets = vec![usize::MAX; chart.full.events.len()];
        let mut life = [base; 4];
        let put = |bytes: &mut Vec<u8>, value: i32| bytes.extend_from_slice(&value.to_le_bytes());
        for command in trace {
            let start = bytes.len();
            match command {
                ReplayCommand::Input(input) => {
                    if !input.is_empty() {
                        bytes.push(0);
                        bytes.extend(input.iter().map(|note| note.judgement as u8));
                    }
                }
                ReplayCommand::Note(note) => {
                    bytes.push(1);
                    put(&mut bytes, note.id);
                    put(&mut bytes, note.life);
                    bytes.push(note.score_type as u8);
                }
                ReplayCommand::Factor(c) => {
                    bytes.push(2);
                    for value in [
                        c.time,
                        c.owner,
                        c.note,
                        c.combo,
                        c.judgement,
                        c.judge,
                        c.luck,
                    ] {
                        put(&mut bytes, value);
                    }
                }
                ReplayCommand::Calculate(time) => {
                    if chart.play.frames.get(frame).is_some_and(|row| row.time_ms == *time) {
                        bytes.push(5);
                    } else if frame > 0 && chart.play.frames[frame - 1].time_ms == *time {
                        bytes.push(6);
                    } else {
                        bytes.push(3);
                        put(&mut bytes, *time);
                    }
                }
                ReplayCommand::Rush(_, _) => {}
                ReplayCommand::Rank(range, end, percent) => {
                    bytes.push(4);
                    bytes.push(*range as u8);
                    put(&mut bytes, *end);
                    put(&mut bytes, *percent);
                }
                ReplayCommand::Ordinary(time, values) => {
                    for (index, &(_, at)) in chart.full.events.iter().enumerate() {
                        if offsets[index] == usize::MAX && at <= *time {
                            offsets[index] = bytes.len();
                        }
                    }
                    let values = [values[0].0, values[0].1, values[1].0, values[1].1];
                    let mut flags = 0u8;
                    for (index, &value) in values.iter().enumerate() {
                        if value != life[index] {
                            flags |= 1 << index;
                        }
                    }
                    bytes.push(16 | flags);
                    for (index, &value) in values.iter().enumerate() {
                        if flags & (1 << index) != 0 {
                            put(&mut bytes, value);
                        }
                    }
                    life = values;
                    frame += 1;
                }
            }
            if bytes.len() != start { instructions.push((start, bytes.len() - start)); }
        }
        for offset in &mut offsets {
            if *offset == usize::MAX {
                *offset = bytes.len();
            }
        }
        let mut packed = Vec::new();
        let mut positions = vec![usize::MAX; offsets.len()];
        let mut index = 0;
        while index < instructions.len() {
            let mut count = 0;
            while index + count * 3 + 2 < instructions.len() && count < u16::MAX as usize {
                let next = &instructions[index + count * 3..][..3];
                if next.iter().zip([5, 16, 6]).all(|(&(at, len), op)| len == 1 && bytes[at] == op)
                    && !offsets.contains(&next[1].0) { count += 1; } else { break; }
            }
            if count >= 2 {
                packed.push(7);
                packed.extend_from_slice(&(count as u16).to_le_bytes());
                index += count * 3;
            } else {
                let (at, len) = instructions[index];
                if bytes[at] >= 16 {
                    for (index, &offset) in offsets.iter().enumerate() {
                        if offset == at { positions[index] = packed.len(); }
                    }
                }
                packed.extend_from_slice(&bytes[at..at + len]);
                index += 1;
            }
        }
        for (offset, position) in offsets.iter_mut().zip(positions) {
            *offset = if position == usize::MAX { packed.len() } else { position };
        }
        packed.shrink_to_fit();
        Self {
            bytes: packed,
            offsets,
            base,
        }
    }
    fn i32(&self, at: &mut usize) -> i32 {
        let value = i32::from_le_bytes(self.bytes[*at..*at + 4].try_into().unwrap());
        *at += 4;
        value
    }
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.bytes.capacity()
            + self.offsets.capacity() * std::mem::size_of::<usize>()
    }
}
struct Views {
    tables: [usize; 12],
    _master: Master,
    control: Arc<Master>,
    ordinary: Arc<Master>,
}
#[derive(Default)]
pub(super) struct Cache {
    entries: std::collections::VecDeque<Entry>,
    bytes: usize,
    views: Option<Views>,
    just: Option<(Arc<crate::master::LiveTables>, Context)>,
}
pub(super) fn just_context(master: &Master, chart: &Chart, deck: &[Performer]) -> Option<Context> {
    if chart.setup.missions.iter().any(|&mission| mission != 3)
        || !matching_missions(master, deck) {
        return None;
    }
    for actor in deck {
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
            )
            .filter(|(skill, _)| skill.mission_type == 4 || skill.mission_type == 3)
            .flat_map(|(skill, level)| skill.effects.iter().filter(move |row| row.level == level));
        if rows
            .into_iter()
            .any(|row| !matches!(row.kind, 13000 | 13002 | 13003 | 13004 | 4004))
        {
            return None;
        }
    }
    if replay_random_skills(master, chart, deck) {
        return None;
    }
    let mut cache = chart.factored.lock().unwrap();
    if let Some((live, context)) = &cache.just {
        if Arc::ptr_eq(live, &master.live) {
            return Some(context.clone());
        }
    }
    let n = chart.notes.len();
    let mut life = crate::full::core::Life::new(
        master.live.life_base as i32,
        master.live.damage,
        chart.params().music_length,
    );
    let notes: HashMap<_, _> = chart
        .full
        .notes
        .iter()
        .map(|note| (note.id, note))
        .collect();
    let scored: HashMap<_, _> = chart
        .notes
        .iter()
        .enumerate()
        .map(|(index, note)| (note.id, index))
        .collect();
    let mut judgement = chart.judgements.clone();
    let mut lives = vec![master.live.life_base as i32; n];
    let mut event_life = vec![master.live.life_base as i32; chart.full.events.len()];
    let mut firing: Vec<_> = (0..chart.full.events.len()).collect();
    firing.sort_by_key(|&index| (chart.full.events[index].1, index));
    let mut event = 0;
    for frame in &chart.play.frames {
        for judged in &frame.judged {
            let note = notes[&judged.note_id];
            life.note_damage(note.time_ms, judged.judgement as usize);
            if let Some(&index) = scored.get(&note.id) {
                lives[index] = life.at(note.time_ms);
                judgement[index] = judged.judgement as usize;
            }
        }
        while event < firing.len() && chart.full.events[firing[event]].1 <= frame.time_ms {
            event_life[firing[event]] = life.at(frame.time_ms);
            event += 1;
        }
        life.sync(frame.time_ms);
    }
    let context = Context {
        life: Some(Arc::new(lives)),
        live_life: Some(Arc::new(event_life)),
        judgement,
        gk: vec![1.0; n],
        up: vec![1.0; n],
        luck: vec![1.0; n],
        seeds: 1,
        factor_commands: 0,
        factor_peak: 0.0,
        rush_changes: 0,
        rush_trace: None,
        draws: Vec::new(),
        nominal: None,
    };
    if context.retained_bytes() <= 2 * 1024 * 1024 {
        cache.just = Some((Arc::clone(&master.live), context.clone()));
    }
    Some(context)
}

fn tables(master: &Master) -> [usize; 12] {
    [
        Arc::as_ptr(&master.live) as usize,
        Arc::as_ptr(&master.params) as usize,
        Arc::as_ptr(&master.live_skills) as usize,
        Arc::as_ptr(&master.support_skills) as usize,
        Arc::as_ptr(&master.gekiso_skills) as usize,
        Arc::as_ptr(&master.gekiso_support_skills) as usize,
        Arc::as_ptr(&master.conditions) as usize,
        Arc::as_ptr(&master.condition_sets) as usize,
        Arc::as_ptr(&master.targets) as usize,
        Arc::as_ptr(&master.cumulative) as usize,
        Arc::as_ptr(&master.live_categories) as usize,
        Arc::as_ptr(&master.gekiso_categories) as usize,
    ]
}
fn views(master: &Master, chart: &Chart) -> (Arc<Master>, Arc<Master>) {
    let mut cache = chart.factored.lock().unwrap();
    let tables = tables(master);
    if cache
        .views
        .as_ref()
        .is_none_or(|known| known.tables != tables)
    {
        let mut control = master.clone();
        let mut ordinary = master.clone();
        control.support_skills = Arc::new(
            master
                .support_skills
                .iter()
                .map(|(&id, rows)| {
                    (
                        id,
                        rows.iter()
                            .filter(|row| !(2000..=2005).contains(&row.kind))
                            .cloned()
                            .collect(),
                    )
                })
                .collect(),
        );
        ordinary.support_skills = Arc::new(
            master
                .support_skills
                .iter()
                .map(|(&id, rows)| {
                    (
                        id,
                        rows.iter()
                            .filter(|row| (2000..=2005).contains(&row.kind) || row.kind == 15000)
                            .cloned()
                            .collect(),
                    )
                })
                .collect(),
        );
        cache.views = Some(Views {
            tables,
            _master: master.clone(),
            control: Arc::new(control),
            ordinary: Arc::new(ordinary),
        });
    }
    let views = cache.views.as_ref().unwrap();
    (Arc::clone(&views.control), Arc::clone(&views.ordinary))
}
fn ordinary_row(master: &Master, row: &SkillEffect) -> bool {
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
    .all(|id| {
        let kind = master.conditions[id].kind;
        kind != 4011 && !(7000..=7021).contains(&kind)
    }) && (row.cumulative_id == 0
        || master
            .cumulative
            .get(&row.cumulative_id)
            .is_some_and(|counter| !(7000..=7002).contains(&counter.kind)))
}
fn ordinary(master: &Master, actor: &Performer) -> Option<(bool, bool)> {
    if let Some((id, level)) = actor.live_skill {
        if master
            .live_skills
            .get(&id)?
            .iter()
            .filter(|row| row.level == level)
            .any(|row| {
                (!(2000..=2005).contains(&row.kind) && row.kind != 15000)
                    || !ordinary_row(master, row)
            })
        {
            return None;
        }
    }
    let mut retained = false;
    let mut ordered = false;
    for &(id, level) in &actor.support_skills {
        for row in master
            .support_skills
            .get(&id)?
            .iter()
            .filter(|row| row.level == level)
        {
            if (2000..=2005).contains(&row.kind) || row.kind == 15000 {
                if !ordinary_row(master, row) {
                    return None;
                }
            } else {
                retained = true;
                ordered |= [
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
                .any(|id| matches!(master.conditions[id].kind, 4010 | 5020));
            }
        }
    }
    Some((retained, ordered))
}
fn healthy_controls(master: &Master, chart: &Chart, deck: &[Performer], flags: &mut [(bool, bool)]) {
    if chart.luck { return; }
    let mut grades = [false; 7];
    for note in chart.play.frames.iter().flat_map(|frame| &frame.judged) {
        grades[note.judgement as usize] = true;
    }
    let normal: Vec<Vec<_>> = deck.iter().map(|actor| actor.support_skills.iter()
        .flat_map(|&(id, level)| master.support_skills[&id].iter().filter(move |row| row.level == level))
        .filter(|row| !(2000..=2005).contains(&row.kind) && row.kind != 15000).collect()).collect();
    let rows: Vec<_> = normal.iter().flatten().copied().chain(deck.iter().flat_map(|actor| {
        actor.gekisou_skill.into_iter().flat_map(|(id, level)| {
            let skill = &master.gekiso_skills[&id];
            skill.effects.iter().filter(move |row| row.level == level
                && (skill.mission_type == 4 || chart.setup.missions.contains(&skill.mission_type)))
        }).chain(actor.gekisou_support_skills.iter().flat_map(|&(id, level)| {
            let skill = &master.gekiso_support_skills[&id];
            skill.effects.iter().filter(move |row| row.level == level
                && (skill.mission_type == 4 || chart.setup.missions.contains(&skill.mission_type)))
        }))
    })).collect();
    let mut targets = HashMap::<i32, [bool; 7]>::default();
    for row in rows.iter().filter(|row| matches!(row.kind, 12006 | 13005)) {
        let mask = targets.entry(row.id).or_default();
        for id in &row.targets {
            let grade = master.targets[id].judgement;
            if (1..=6).contains(&grade) { mask[grade as usize] = true; }
        }
    }
    let reachable = |id: i32, grades: &[bool; 7]| targets[&id].iter().zip(grades)
        .any(|(&target, &possible)| target && possible);
    loop {
        let before = grades;
        for row in &rows {
            if matches!(row.kind, 12006 | 13005)
                && reachable(row.id, &grades) {
                let to = if row.kind == 13005 { 6 } else { row.value };
                if (1..=6).contains(&to) { grades[to as usize] = true; }
            }
        }
        if before == grades { break; }
    }
    if grades.iter().enumerate().any(|(grade, &possible)| possible && master.live.damage[grade] != 0.0)
        || rows.iter().any(|row| matches!(row.kind, 3000 | 3002)
            || matches!(row.kind, 3001 | 3004) && row.value < 0) { return; }
    for (index, rows) in normal.iter().enumerate() {
        let active: Vec<_> = rows.iter().filter(|row| match row.kind {
            3001 | 3003 | 3004 => false,
            12006 => reachable(row.id, &grades),
            _ => true,
        }).collect();
        flags[index].0 = !active.is_empty();
        flags[index].1 = active.iter().any(|row| [row.trigger_group, row.condition_group,
            row.release_group, row.reset_group].iter().flat_map(|group| {
                master.condition_sets.get(group).into_iter().flatten().flatten()
            }).any(|id| matches!(master.conditions[id].kind, 4010 | 5020)));
    }
}
fn independent_row(master: &Master, row: &SkillEffect) -> bool {
    row.kind!=15000 && [row.trigger_group,row.condition_group,row.release_group,row.reset_group].iter()
        .flat_map(|group|master.condition_sets.get(group).into_iter().flatten().flatten())
        .all(|id|matches!(master.conditions[id].kind,0|1000..=1031|2000..=2004|4011|5000|7000..=7021))
}
fn matching_missions(master: &Master, deck: &[Performer]) -> bool {
    deck.iter().all(|actor| actor.mission_only.filter(|&mission| mission != 0).is_none_or(|mission| {
        actor.gekisou_skill.is_some_and(|(id, _)| master.gekiso_skills[&id].mission_type == mission)
    }))
}
pub(super) fn seed_actor(master: &Master, chart: &Chart, actor: &Performer) -> bool {
    if !matching_missions(master, std::slice::from_ref(actor))
        || ordinary(master, actor).is_none_or(|row| row.0) {
        return false;
    }
    actor
        .gekisou_skill
        .into_iter()
        .filter_map(|(id, level)| master.gekiso_skills.get(&id).map(|skill| (skill, level)))
        .filter(|(skill, _)| {
            skill.mission_type == 4 || chart.setup.missions.contains(&skill.mission_type)
        })
        .flat_map(|(skill, level)| skill.effects.iter().filter(move |row| row.level == level))
        .all(|row| independent_row(master, row))
}
fn score_phase_safe(master: &Master, chart: &Chart, deck: &[Performer]) -> bool {
    let channel = |kind| match kind {
        2000 | 2001 | 2005 => Some(0),
        2002 | 2003 => Some(1),
        2004 => Some(2),
        _ => None,
    };
    for actor in deck {
        let normal: Vec<_> = actor
            .live_skill
            .into_iter()
            .flat_map(|(id, level)| {
                master.live_skills[&id]
                    .iter()
                    .filter(move |row| row.level == level)
                    .map(|row| (1, row))
            })
            .chain(actor.support_skills.iter().flat_map(|&(id, level)| {
                master.support_skills[&id]
                    .iter()
                    .filter(move |row| row.level == level)
                    .map(|row| (2, row))
            }))
            .collect();
        let gk: Vec<_> = actor
            .gekisou_skill
            .into_iter()
            .filter(|&(id, _)| {
                let kind = master.gekiso_skills[&id].mission_type;
                kind == 4 || chart.setup.missions.contains(&kind)
            })
            .flat_map(|(id, level)| {
                master.gekiso_skills[&id]
                    .effects
                    .iter()
                    .filter(move |row| row.level == level)
                    .map(|row| (1, row))
            })
            .chain(
                actor
                    .gekisou_support_skills
                    .iter()
                    .filter(|&&(id, _)| {
                        let kind = master.gekiso_support_skills[&id].mission_type;
                        kind == 4 || chart.setup.missions.contains(&kind)
                    })
                    .flat_map(|&(id, level)| {
                        master.gekiso_support_skills[&id]
                            .effects
                            .iter()
                            .filter(move |row| row.level == level)
                            .map(|row| (2, row))
                    }),
            )
            .collect();
        for (owner, row) in normal {
            if let Some(channel) = channel(row.kind) {
                if gk.iter().any(|&(other, effect)| {
                    owner == other
                        && Some(channel)
                            == (match effect.kind {
                                2000 | 2001 | 2005 => Some(0),
                                2002 | 2003 => Some(1),
                                2004 => Some(2),
                                _ => None,
                            })
                        && master.live.phases.get(&row.kind).copied().unwrap_or(1)
                            != master.live.phases.get(&effect.kind).copied().unwrap_or(1)
                }) {
                    return false;
                }
            }
        }
    }
    true
}
fn omit_unused_just_points(master: &Master, chart: &Chart, deck: &mut [Performer]) {
    if chart.setup.missions.iter().any(|&mission| mission != 3) {
        return;
    }
    let removable: Vec<_> = deck
        .iter()
        .map(|actor| {
            actor.gekisou_skill.is_some_and(|(id, level)| {
                let rows: Vec<_> = master.gekiso_skills[&id]
                    .effects
                    .iter()
                    .filter(|row| row.level == level)
                    .collect();
                !rows.is_empty()
                    && rows
                        .iter()
                        .all(|row| matches!(row.kind, 13000 | 13002 | 13003 | 13004 | 4004))
            })
        })
        .collect();
    for (index, actor) in deck.iter().enumerate() {
        let rows = actor
            .gekisou_skill
            .into_iter()
            .filter(|_| !removable[index])
            .flat_map(|(id, level)| {
                master.gekiso_skills[&id]
                    .effects
                    .iter()
                    .filter(move |row| row.level == level)
            })
            .chain(
                actor
                    .gekisou_support_skills
                    .iter()
                    .flat_map(|&(id, level)| {
                        master.gekiso_support_skills[&id]
                            .effects
                            .iter()
                            .filter(move |row| row.level == level)
                    }),
            )
            .chain(actor.support_skills.iter().flat_map(|&(id, level)| {
                master.support_skills[&id]
                    .iter()
                    .filter(move |row| row.level == level)
            }));
        for row in rows {
            if master
                .cumulative
                .get(&row.cumulative_id)
                .is_some_and(|counter| counter.kind == 7000)
                || [
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
                .any(|id| matches!(master.conditions[id].kind, 7002 | 7003 | 7006))
            {
                return;
            }
        }
    }
    for (actor, removable) in deck.iter_mut().zip(removable) {
        if removable {
            let (id, _) = actor.gekisou_skill.take().unwrap();
            if actor.mission_only.unwrap_or(0) == 0 {
                actor.mission_only = Some(master.gekiso_skills[&id].mission_type);
            }
        }
    }
}
fn control_program(master: &Master, chart: &Chart, deck: &[Performer]) -> Vec<u64> {
    let mut result = vec![0];
    for actor in deck {
        let enabled =
            actor.gekisou_skill.is_some() || actor.mission_only.is_some_and(|kind| kind != 0);
        let member = actor.gekisou_skill.filter(|&(id, _)| {
            let kind = actor
                .mission_only
                .filter(|&kind| kind != 0)
                .unwrap_or(master.gekiso_skills[&id].mission_type);
            kind == 4 || chart.setup.missions.contains(&kind)
        });
        let supports: Vec<_> = actor
            .gekisou_support_skills
            .iter()
            .copied()
            .filter(|&(id, _)| {
                enabled
                    && (master.gekiso_support_skills[&id].mission_type == 4
                        || chart
                            .setup
                            .missions
                            .contains(&master.gekiso_support_skills[&id].mission_type))
            })
            .collect();
        let ordinary: Vec<_> = actor
            .support_skills
            .iter()
            .copied()
            .filter(|&(id, level)| {
                master.support_skills[&id]
                    .iter()
                    .any(|row| row.level == level && row.kind != 15000)
            })
            .collect();
        let rows: Vec<_> = member
            .into_iter()
            .flat_map(|(id, level)| {
                master.gekiso_skills[&id]
                    .effects
                    .iter()
                    .filter(move |row| row.level == level)
            })
            .chain(supports.iter().flat_map(|&(id, level)| {
                master.gekiso_support_skills[&id]
                    .effects
                    .iter()
                    .filter(move |row| row.level == level)
            }))
            .chain(ordinary.iter().flat_map(|&(id, level)| {
                master.support_skills[&id]
                    .iter()
                    .filter(move |row| row.level == level && row.kind != 15000)
            }))
            .collect();
        let mut owner = Vec::new();
        for row in rows {
            if master
                .cumulative
                .get(&row.cumulative_id)
                .is_some_and(|counter| (3000..=3005).contains(&counter.kind))
            {
                return std::iter::once(1)
                    .chain(serde_json::to_vec(deck).unwrap().into_iter().map(u64::from))
                    .collect();
            }
            for group in [
                row.trigger_group,
                row.condition_group,
                row.release_group,
                row.reset_group,
            ] {
                for id in master
                    .condition_sets
                    .get(&group)
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    let condition = &master.conditions[id];
                    if (3000..=3005).contains(&condition.kind) {
                        return std::iter::once(1)
                            .chain(serde_json::to_vec(deck).unwrap().into_iter().map(u64::from))
                            .collect();
                    }
                    if condition.kind == 5000 {
                        owner.push(u64::from(
                            condition.targets.iter().any(|id| {
                                crate::full::conditions::performer_matches(
                                    actor,
                                    &master.targets[id],
                                )
                            }) == condition.positive,
                        ));
                    }
                }
            }
        }
        let mut code = vec![
            u64::from(enabled),
            member.map_or(0, |_| actor.mission_only.unwrap_or(0)) as u64,
            member.map_or(0, |pair| pair.0 as u64),
            member.map_or(0, |pair| pair.1 as u64),
            supports.len() as u64,
        ];
        for (id, level) in supports {
            code.extend([id as u64, level as u64]);
        }
        code.push(ordinary.len() as u64);
        for (id, level) in ordinary {
            code.extend([id as u64, level as u64]);
        }
        code.extend(owner);
        result.push(code.len() as u64);
        result.extend(code);
    }
    result
}
fn context(
    master: &Master,
    control: &Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
    ordinary: &[(bool, bool)],
    order: &[usize; 5],
) -> Result<Arc<Scene>, String> {
    let mut reduced: Vec<_> = deck.iter().map(Performer::gekiso_only).collect();
    for (i, actor) in reduced.iter_mut().enumerate() {
        if ordinary[i].0 {
            actor.support_skills = deck[i].support_skills.clone();
        }
        actor.gekisou_support_skills.retain(|&(id, _)| {
            master.gekiso_support_skills.get(&id).is_some_and(|skill| {
                skill.mission_type == 4 || chart.setup.missions.contains(&skill.mission_type)
            })
        });
    }
    omit_unused_just_points(control, chart, &mut reduced);
    let key = Key {
        tables: tables(master),
        program: control_program(control, chart, &reduced),
        seeds: seeds.to_vec(),
        ranks: chart.setup.ranks.clone(),
        music_type: chart.music_type,
        events: chart
            .full
            .events
            .iter()
            .map(|&(event, time)| {
                order
                    .get(event)
                    .copied()
                    .map(|actor| (actor, time))
                    .ok_or_else(|| format!("ordinary skill event {event} out of range"))
            })
            .collect::<Result<_, _>>()?,
    };
    {
        let mut cache = chart.factored.lock().unwrap();
        if let Some(index) = cache.entries.iter().position(|entry| entry.key == key) {
            let entry = cache.entries.remove(index).unwrap();
            let context = Arc::clone(&entry.context);
            cache.entries.push_back(entry);
            return Ok(context);
        }
    }
    let mut model = Model::new(
        control,
        &reduced,
        &chart.full.notes,
        &key.events,
        chart.params(),
        Some(&chart.setup),
        (seeds.first().copied().unwrap_or(0), false),
    )?;
    model.score.score.replay = Some(Vec::new());
    model.score.score.factored_record = true;
    model.score.score.record_only = true;
    model.score.score.calculator.factors_only = true;
    let runs = if chart.luck {
        seeds.to_vec()
    } else {
        vec![seeds.first().copied().unwrap_or(0)]
    };
    let mut draws = Vec::new();
    let generation = crate::cancellation_generation();
    for seed in runs {
        let mut current = model.clone();
        current.random = Random::new(seed);
        for (index, frame) in chart.play.frames.iter().enumerate() {
            if index % 64 == 0 && generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
            current.frame(frame.time_ms, &frame.judged, chart.play.dt[index])?;
            current.score.score.calculator.observations.clear();
        }
        let trace = current.score.score.replay.take().unwrap();
        draws.push(Arc::new(ScoreTape::new(
            &trace,
            chart,
            control.live.life_base as i32,
        )));
    }
    let bytes = draws
        .iter()
        .map(|draw| draw.retained_bytes())
        .sum::<usize>()
        + draws.capacity() * std::mem::size_of::<Arc<ScoreTape>>()
        + std::mem::size_of::<Entry>()
        + key.program.capacity() * 8
        + key.seeds.capacity() * 4
        + key.ranks.capacity() * 4
        + key.events.capacity() * std::mem::size_of::<(usize, i32)>();
    let context = Arc::new(Scene { draws });
    const LIMIT: usize = 2 * 1024 * 1024;
    if bytes <= LIMIT {
        let mut cache = chart.factored.lock().unwrap();
        while cache.bytes + bytes > LIMIT || cache.entries.len() >= 64 {
            if let Some(entry) = cache.entries.pop_front() {
                cache.bytes -= entry.bytes;
            } else {
                break;
            }
        }
        cache.bytes += bytes;
        cache.entries.push_back(Entry {
            key,
            context: Arc::clone(&context),
            bytes,
            _master: master.clone(),
        });
    }
    Ok(context)
}
#[derive(Clone)]
struct ScoreState<'a> {
    program: Model<'a>,
    score: Incremental<'a>,
    combo: core::Combo,
    judged: Vec<crate::full::play::JudgedNote>,
    life: [i32; 4],
    last: [i32; 2],
    ranges: [f64; 3],
    frame: usize,
    sparse: bool,
    next_frame: i32,
    previous_life: [i32; 4],
}
struct ScoreTree<'a> {
    chart: &'a Chart,
    trace: &'a ScoreTape,
    chronological: Vec<usize>,
    generation: u64,
}
impl<'a> ScoreTree<'a> {
    fn calculate(&self, state: &mut ScoreState<'a>, time: i32) -> Result<(), String> {
        state.last[0] = state.last[1];
        state.last[1] = state.score.calculate(time, &state.combo, None)?;
        Ok(())
    }
    fn ordinary(&self, state: &mut ScoreState<'a>) -> Result<(), String> {
        let time = self.chart.play.frames[state.frame].time_ms;
        if !state.sparse || !state.judged.is_empty()
            || state.previous_life != state.life || time >= state.next_frame {
            state.program.life.freeze_frame([
                (state.life[0], state.life[1]), (state.life[2], state.life[3]),
            ]);
            state.program.count_frame(time, &state.judged, self.chart.play.dt[state.frame])?;
            state.previous_life = state.life;
            state.next_frame = state.program.next_score_frame(time);
        }
        state.judged.clear();
        state.frame += 1;
        for command in state.program.score.score.replay.as_mut().unwrap().drain(..) {
            if let ReplayCommand::Factor(mut command) = command {
                command.owner *= 2;
                state.score.add_factor(command);
            }
        }
        Ok(())
    }
    fn advance(&self, state: &mut ScoreState<'a>, from: usize, until: usize) -> Result<(), String> {
        let mut at = from;
        while at < until {
            let op = self.trace.bytes[at];
            at += 1;
            match op {
                0 => {
                    state.judged.clear();
                    for raw in &self.chart.play.frames[state.frame].judged {
                        let judgement = i32::from(self.trace.bytes[at]);
                        at += 1;
                        state.combo.add(raw.time_ms, judgement);
                        state.judged.push(crate::full::play::JudgedNote {
                            note_id: raw.note_id,
                            time_ms: raw.time_ms,
                            judgement,
                        });
                    }
                }
                1 => {
                    let id = self.trace.i32(&mut at);
                    let life = self.trace.i32(&mut at);
                    let score_type = i32::from(self.trace.bytes[at]);
                    at += 1;
                    let note = &self.chart.full.notes[(id - 1) as usize];
                    state
                        .score
                        .add_note(note.time_ms, life, id, note.op, score_type);
                }
                2 => {
                    let c = FactorCommand {
                        time: self.trace.i32(&mut at),
                        owner: self.trace.i32(&mut at) * 2 + 1,
                        note: self.trace.i32(&mut at),
                        combo: self.trace.i32(&mut at),
                        judgement: self.trace.i32(&mut at),
                        judge: self.trace.i32(&mut at),
                        luck: self.trace.i32(&mut at),
                    };
                    state.score.add_factor(c);
                }
                3 => {
                    let time = self.trace.i32(&mut at);
                    self.calculate(state, time)?;
                }
                5 | 6 => {
                    let frame = state.frame - usize::from(op == 6);
                    self.calculate(state, self.chart.play.frames[frame].time_ms)?;
                }
                7 => {
                    let count = u16::from_le_bytes(self.trace.bytes[at..at + 2].try_into().unwrap()) as usize;
                    at += 2;
                    let end = state.frame + count;
                    while state.frame < end {
                        let time = self.chart.play.frames[state.frame].time_ms;
                        if state.sparse && state.judged.is_empty() && state.life == state.previous_life
                            && time < state.next_frame {
                            let next = self.chart.play.frames[state.frame..end]
                                .partition_point(|frame| frame.time_ms < state.next_frame) + state.frame;
                            let score = state.score.calculate(self.chart.play.frames[next - 1].time_ms, &state.combo, None)?;
                            state.last = [score; 2];
                            state.frame = next;
                        } else {
                            self.calculate(state, time)?;
                            self.ordinary(state)?;
                            self.calculate(state, time)?;
                        }
                    }
                }
                4 => {
                    let range = self.trace.bytes[at] as usize;
                    at += 1;
                    let end = self.trace.i32(&mut at);
                    let percent = self.trace.i32(&mut at);
                    let own = state.last[1].wrapping_sub(state.last[0]);
                    state.ranges[range] = own as f64;
                    let bonus =
                        crate::num::i32((i64::from(own) * i64::from(percent)) as f64 / 100.0);
                    state.score.add_fixed(end, bonus);
                }
                16..=31 => {
                    for (field, life) in state.life.iter_mut().enumerate() {
                        if op & (1 << field) != 0 {
                            *life = self.trace.i32(&mut at);
                        }
                    }
                    self.ordinary(state)?;
                }
                _ => unreachable!("control replay opcode"),
            }
        }
        Ok(())
    }
    fn visit(
        &self,
        mut state: ScoreState<'a>,
        orders: &[usize],
        from: usize,
        fixed: usize,
        values: &mut [ReplayValue],
    ) -> Result<(), String> {
        if self.generation != crate::cancellation_generation() {
            return Err("cancelled".into());
        }
        let representative = &live::orders()[orders[0]];
        let events: Vec<_> = self
            .chart
            .full
            .events
            .iter()
            .map(|&(event, time)| (representative[event], time))
            .collect();
        state.program.set_event_members(&events);
        let split = (fixed..self.chronological.len()).find(|&at| {
            let event = self.chart.full.events[self.chronological[at]].0;
            orders
                .iter()
                .any(|&order| live::orders()[order][event] != representative[event])
        });
        let until = split.map_or(self.trace.bytes.len(), |at| {
            self.trace.offsets[self.chronological[at]]
        });
        self.advance(&mut state, from, until)?;
        if until == self.trace.bytes.len() {
            for &order in orders {
                values[order] = (state.score.score as f64, state.ranges);
            }
            return Ok(());
        }
        let at = split.unwrap();
        let event = self.chart.full.events[self.chronological[at]].0;
        let mut groups: [Vec<usize>; 5] = std::array::from_fn(|_| Vec::new());
        for &order in orders {
            groups[live::orders()[order][event]].push(order);
        }
        let mut pending = groups
            .into_iter()
            .filter(|group| !group.is_empty())
            .peekable();
        while let Some(group) = pending.next() {
            if pending.peek().is_none() {
                return self.visit(state, &group, until, at + 1, values);
            }
            self.visit(state.clone(), &group, until, at + 1, values)?;
        }
        Ok(())
    }
}
fn score_orders<'a>(
    master: &'a Master,
    ordinary: &'a Master,
    chart: &'a Chart,
    deck: &[Performer],
    draw: &'a ScoreTape,
    power: i32,
    orders: &[usize],
) -> Result<Option<Vec<ReplayValue>>, String> {
    let trace = draw;
    let order = &live::orders()[orders[0]];
    let events: Vec<_> = chart
        .full
        .events
        .iter()
        .map(|&(event, time)| {
            order
                .get(event)
                .copied()
                .map(|actor| (actor, time))
                .ok_or_else(|| format!("ordinary skill event {event} out of range"))
        })
        .collect::<Result<_, _>>()?;
    let mut capacity = [0; 5];
    for &(member, _) in &events {
        capacity[member] += 1;
    }
    let capacity = [capacity.into_iter().max().unwrap_or(0); 5];
    let params = chart.params();
    let mut program = Model::with_live_capacity(
        ordinary,
        deck,
        &chart.full.notes,
        &events,
        params.clone(),
        None,
        (0, false, Some(&capacity)),
    )?;
    program.score.score.replay = Some(Vec::new());
    program.score.score.record_only = true;
    let sparse = program.sparse_score_frames();
    let calculator = Calculator::new(
        &master.live,
        power,
        params.level,
        params.converted,
        params.assist,
    );
    let state = ScoreState {
        program,
        score: Incremental::new(calculator, params.music_length),
        combo: core::Combo::default(),
        judged: Vec::new(),
        life: [draw.base; 4],
        last: [0; 2],
        ranges: [0.0; 3],
        frame: 0,
        sparse,
        next_frame: i32::MIN,
        previous_life: [draw.base; 4],
    };
    let mut chronological: Vec<_> = (0..events.len()).collect();
    chronological.sort_by_key(|&event| (events[event].1, event));
    let tree = ScoreTree {
        chart,
        trace,
        chronological,
        generation: crate::cancellation_generation(),
    };
    let mut values = vec![(0.0, [0.0; 3]); 120];
    tree.visit(state, orders, 0, 0, &mut values)?;
    Ok(Some(values))
}
pub(super) fn scores(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
    power: i32,
) -> Result<Option<ReplayedScores>, String> {
    if deck.len() != 5
        || !matching_missions(master, deck)
        || chart
            .setup
            .missions
            .iter()
            .any(|&mission| mission != 2 && mission != 3)
        || chart.luck && seeds.is_empty()
        || !chart.luck && replay_random_skills(master, chart, deck)
    {
        return Ok(None);
    }
    for actor in deck {
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
            )
            .filter(|(skill, _)| {
                skill.mission_type == 4 || chart.setup.missions.contains(&skill.mission_type)
            })
            .flat_map(|(skill, level)| skill.effects.iter().filter(move |row| row.level == level));
        if rows.into_iter().any(|row| !independent_row(master, row)) {
            return Ok(None);
        }
    }
    if !score_phase_safe(master, chart, deck) {
        return Ok(None);
    }
    let Some(mut ordinary) = deck
        .iter()
        .map(|actor| ordinary(master, actor))
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(None);
    };
    healthy_controls(master, chart, deck, &mut ordinary);
    let (control, ordinary_master) = views(master, chart);
    let mut groups = Vec::<Vec<usize>>::new();
    let mut ids = HashMap::default();
    for (index, order) in live::orders().iter().enumerate() {
        let key: Vec<_> = order
            .iter()
            .map(|&actor| if ordinary[actor].1 { actor } else { usize::MAX })
            .collect();
        let next = groups.len();
        let group = *ids.entry(key).or_insert(next);
        if group == next {
            groups.push(Vec::new());
        }
        groups[group].push(index);
    }
    let generation = crate::cancellation_generation();
    let mut values = vec![0.0; 120];
    let mut range_values = [[0.0; 3]; 120];
    for orders in groups {
        let context = context(
            master,
            &control,
            chart,
            deck,
            seeds,
            &ordinary,
            &live::orders()[orders[0]],
        )?;
        let draws = &context.draws;
        for draw in draws {
            if generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
            let Some(result) =
                score_orders(master, &ordinary_master, chart, deck, draw, power, &orders)?
            else {
                return Ok(None);
            };
            for &order in &orders {
                values[order] += result[order].0;
                for (sum, value) in range_values[order].iter_mut().zip(result[order].1) {
                    *sum += value;
                }
            }
        }
        for order in orders {
            values[order] /= draws.len() as f64;
            for value in &mut range_values[order] {
                *value /= draws.len() as f64;
            }
        }
    }

    let scores = ReplayedScores {
        scores: live::summarize(power, values),
        range_scores: std::array::from_fn(|range| {
            range_values.iter().map(|row| row[range]).sum::<f64>() / 120.0
        }),
    };
    #[cfg(debug_assertions)]
    {
        let full = replay_orders_original(master, chart, deck, seeds, power, None)?.unwrap();
        debug_assert_eq!(
            scores.scores.scores, full.scores.scores,
            "factored ordinary orders"
        );
        debug_assert_eq!(
            scores.range_scores, full.range_scores,
            "factored section scores"
        );
    }
    Ok(Some(scores))
}
