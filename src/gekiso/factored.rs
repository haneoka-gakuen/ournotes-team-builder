//! Native score replays share judgement, LIFE and Gekiso control streams.
//! Lottery-independent ordinary factor commands are replayed at their delivery
//! frame across all sampled draws. Equivalent input/LIFE streams share bounded
//! sample tapes; each of the 120 scores and ranks is settled separately.
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
    shared_inputs: bool,
}
struct FixedFrame {
    judged: Vec<crate::full::play::JudgedNote>,
    life: [(i32, i32); 2],
}
struct SampleEntry {
    input: Vec<u8>,
    draws: Vec<Arc<ScoreTape>>,
    bytes: usize,
}
#[derive(Default)]
struct SampleCache {
    entries: std::collections::VecDeque<SampleEntry>,
    bytes: usize,
}
impl SampleCache {
    fn get(&mut self, input: &[u8]) -> Option<Vec<Arc<ScoreTape>>> {
        let index = self.entries.iter().position(|entry| entry.input == input)?;
        let entry = self.entries.remove(index).unwrap();
        let draws = entry.draws.clone();
        self.entries.push_back(entry);
        Some(draws)
    }
    fn insert(&mut self, input: Vec<u8>, draws: &[Arc<ScoreTape>]) {
        let mut unique = std::collections::HashSet::new();
        let bytes = input.capacity() + std::mem::size_of_val(draws)
            + draws.iter().filter(|draw| unique.insert(Arc::as_ptr(draw))).map(|draw| draw.retained_bytes()).sum::<usize>() + 128;
        const LIMIT: usize = 4 * 1024 * 1024;
        if bytes > LIMIT { return; }
        while self.bytes + bytes > LIMIT {
            if let Some(entry) = self.entries.pop_front() { self.bytes -= entry.bytes; } else { break; }
        }
        self.bytes += bytes;
        self.entries.push_back(SampleEntry { input, draws: draws.to_vec(), bytes });
    }
}

#[derive(PartialEq, Eq, Hash)]
struct ScoreTape {
    bytes: Vec<u8>,
    offsets: Vec<usize>,
    base: i32,
    combos: Option<Arc<[i32]>>,
}
impl ScoreTape {
    fn new(trace: &[ReplayCommand], chart: &Chart, base: i32) -> Self {
        let mut bytes = Vec::new();
        let mut instructions = Vec::new();
        let mut frame = 0;
        let mut offsets = vec![usize::MAX; chart.full.events.len()];
        let mut life = [base; 4];
        let mut combo = core::Combo::default();
        let mut last_note = i32::MIN;
        let mut fixed_combos = true;
        let put = |bytes: &mut Vec<u8>, value: i32| bytes.extend_from_slice(&value.to_le_bytes());
        for command in trace {
            let start = bytes.len();
            match command {
                ReplayCommand::Input(input) => {
                    if !input.is_empty() {
                        for (raw, converted) in chart.play.frames[frame].judged.iter().zip(input) {
                            fixed_combos &= raw.time_ms >= last_note;
                            combo.add(raw.time_ms, converted.judgement);
                        }
                        bytes.push(0);
                        bytes.extend(input.iter().map(|note| note.judgement as u8));
                    }
                }
                ReplayCommand::Note(note) => {
                    last_note = last_note.max(note.time);
                    bytes.push(1);
                    put(&mut bytes, note.id);
                    put(&mut bytes, note.life);
                    bytes.push(note.score_type as u8);
                }
                ReplayCommand::ComboHistory(update) => {
                    bytes.push(8);
                    bytes.push(update.range as u8);
                    put(&mut bytes, update.from as i32);
                    put(&mut bytes, (update.rest.len() + 1) as i32);
                    for (time, combo) in std::iter::once(update.first).chain(update.rest.iter().copied()) {
                        put(&mut bytes, time);
                        put(&mut bytes, combo);
                    }
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
            combos: fixed_combos.then(|| std::iter::once(0).chain(chart.full.notes.iter()
                .map(|note| combo.at(note.time_ms))).collect::<Arc<[i32]>>()),
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
            + self.combos.as_ref().map_or(0, |counts| counts.len() * std::mem::size_of::<i32>())
    }
}
struct Views {
    tables: [usize; 12],
    _master: Master,
    control: Arc<Master>,
    ordinary: Arc<Master>,
}
type DeadHealKey = (Vec<Performer>, Vec<i32>, bool);
struct DeadHealProofs {
    tables: [usize; 12],
    _master: Master,
    results: HashMap<DeadHealKey, bool>,
}
#[derive(Default)]
pub(super) struct Cache {
    entries: std::collections::VecDeque<Entry>,
    bytes: usize,
    views: Option<Views>,
    just: Option<(Arc<crate::master::LiveTables>, Context)>,
    dead_heals: Option<DeadHealProofs>,
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
fn fixed_damage_reduction(master: &Master, row: &SkillEffect) -> bool {
    let condition_is = |group: i32, kind: i32| {
        master.condition_sets.get(&group).is_some_and(|sets| {
            matches!(sets.as_slice(), [set] if matches!(set.as_slice(), [id]
                if master.conditions.get(id).is_some_and(|condition| condition.kind == kind && condition.positive)))
        })
    };
    row.kind == 3004 && row.trigger_type == 2 && row.seconds == 0.0
        && row.cumulative_id == 0 && row.execute_limit == 0 && row.reset_group == 0
        && condition_is(row.trigger_group, 7020)
        && (row.release_group == 0 || condition_is(row.release_group, 7013))
        && (row.condition_group == 0 || master.condition_sets.get(&row.condition_group)
            .is_some_and(|sets| sets.iter().flatten().all(|id| master.conditions[id].kind == 5000)))
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
fn recovery_program(master: &Master, actor: &Performer) -> Option<Vec<i32>> {
    let mut values = Vec::new();
    for &(id, level) in &actor.support_skills {
        let mut rows: Vec<_> = master.support_skills.get(&id)?.iter().filter(|row| row.level == level).collect();
        rows.sort_unstable_by_key(|row| row.id);
        for row in rows {
            if (2000..=2005).contains(&row.kind) || row.kind == 15000 { continue; }
            if row.kind != 3001 || row.value < 0 || row.seconds != 0.0 || row.trigger_type != 1
                || row.cumulative_id != 0 || row.release_group != 0 || row.reset_group != 0 || row.execute_limit != 0 { return None; }
            let trigger = master.condition_sets.get(&row.trigger_group)?;
            if trigger.len() != 1 || trigger[0].len() != 1 { return None; }
            let condition = &master.conditions[&trigger[0][0]];
            if !condition.positive || !matches!(condition.kind, 4010 | 5020) { return None; }
            let mut enabled = row.condition_group == 0;
            if row.condition_group != 0 {
                for set in master.condition_sets.get(&row.condition_group)? {
                    let mut matched = true;
                    for id in set {
                        let condition = &master.conditions[id];
                        if condition.kind != 5000 { return None; }
                        matched &= condition.targets.iter().any(|id|
                            crate::full::conditions::performer_matches(actor, &master.targets[id])) == condition.positive;
                    }
                    enabled |= matched;
                }
            }
            if enabled { values.push(row.value); }
        }
    }
    Some(values)
}
fn control_classes(master: &Master, chart: &Chart, deck: &[Performer], ordinary: &[(bool, bool)]) -> Vec<usize> {
    let mut classes: Vec<_> = ordinary.iter().enumerate().map(|(index, flags)| if flags.1 { index + 1 } else { 0 }).collect();
    let mut frames: Vec<_> = chart.full.events.iter().map(|&(_, time)| chart.play.frames.partition_point(|frame| frame.time_ms < time)).collect();
    frames.sort_unstable();
    if frames.windows(2).any(|pair| pair[0] == pair[1]) { return classes; }
    let mut programs = Vec::new();
    for actor in deck {
        let program = recovery_program(master, actor);
        for &(id, level) in &actor.support_skills {
            for row in master.support_skills[&id].iter().filter(|row| row.level == level) {
                if matches!(row.kind, 3000..=3004) && (row.kind != 3001 || program.is_none()) { return classes; }
            }
        }
        let rows = actor.gekisou_skill.into_iter().filter_map(|(id, level)| master.gekiso_skills.get(&id).map(|skill| (skill, level)))
            .chain(actor.gekisou_support_skills.iter().filter_map(|&(id, level)| master.gekiso_support_skills.get(&id).map(|skill| (skill, level))))
            .filter(|(skill, _)| skill.mission_type == 4 || chart.setup.missions.contains(&skill.mission_type))
            .flat_map(|(skill, level)| skill.effects.iter().filter(move |row| row.level == level));
        if rows.into_iter().any(|row| matches!(row.kind, 3000..=3004)) { return classes; }
        programs.push(program);
    }
    let mut ids = HashMap::default();
    for (index, program) in programs.into_iter().enumerate() {
        if !ordinary[index].1 { continue; }
        if let Some(program) = program {
            if program.is_empty() { classes[index] = 0; } else {
                let next = deck.len() + 1 + ids.len();
                classes[index] = *ids.entry(program).or_insert(next);
            }
        }
    }
    classes
}
fn ignore_dead_heal_order(master: &Master, chart: &Chart, deck: &[Performer], seeds: &[i32], flags: &mut [(bool, bool)]) -> Result<(), String> {
    let gate = |group: i32, kinds: &[i32]| group != 0 && master.condition_sets.get(&group)
        .is_some_and(|sets| sets.iter().all(|set| set.iter().any(|id| master.conditions[id].positive && kinds.contains(&master.conditions[id].kind))));
    let deferred = |row: &SkillEffect| matches!(row.trigger_type, 1 | 2) && gate(row.trigger_group, &[4010, 5020]);
    let healer = |row: &SkillEffect| row.kind == 3001 && deferred(row) && ordinary_row(master, row);
    let normal: Vec<Vec<_>> = deck.iter().map(|actor| actor.support_skills.iter().flat_map(|&(id, level)|
        master.support_skills[&id].iter().filter(move |row| row.level == level))
        .filter(|row| !(2000..=2005).contains(&row.kind) && row.kind != 15000).collect()).collect();
    let candidates: Vec<_> = normal.iter().enumerate().filter(|(index, rows)| flags[*index].1 && !rows.is_empty() && rows.iter().all(|row| healer(row)))
        .map(|(index, _)| index).collect();
    if candidates.is_empty() || normal.iter().flatten().any(|row| matches!(row.kind, 3000..=3004) && !healer(row)) { return Ok(()); }
    let Some(first) = chart.full.events.iter().map(|&(_, time)| time).min() else { return Ok(()); };
    let gk: Vec<_> = deck.iter().flat_map(|actor| actor.gekisou_skill.into_iter().flat_map(|(id, level)| {
        let skill = &master.gekiso_skills[&id];
        skill.effects.iter().filter(move |row| row.level == level && (skill.mission_type == 4 || chart.setup.missions.contains(&skill.mission_type)))
    }).chain(actor.gekisou_support_skills.iter().flat_map(|&(id, level)| {
        let skill = &master.gekiso_support_skills[&id];
        skill.effects.iter().filter(move |row| row.level == level && (skill.mission_type == 4 || chart.setup.missions.contains(&skill.mission_type)))
    }))).collect();
    let fast = chart.setup.fevers.iter().all(|&(start, _)| start >= first)
        && !gk.iter().any(|row| matches!(row.kind, 3000..=3004) && !fixed_damage_reduction(master, row))
        && gk.iter().filter(|row| matches!(row.kind, 12006 | 13005))
            .all(|row| matches!(row.trigger_type, 1 | 2) && gate(row.trigger_group, &[7010, 7013, 7020]))
        && normal.iter().flatten().all(|row| ordinary_row(master, row)
            && (row.cumulative_id == 0 || master.cumulative.get(&row.cumulative_id).is_some_and(|counter| !(3000..=3005).contains(&counter.kind)))
            && [row.trigger_group, row.condition_group, row.release_group, row.reset_group].iter()
                .flat_map(|group| master.condition_sets.get(group).into_iter().flatten().flatten())
                .all(|id| !matches!(master.conditions[id].kind, 3000 | 3001)));
    if !fast && gk.iter().any(|row| matches!(row.kind, 3000..=3004)) { return Ok(()); }
    let projected: Vec<_> = if fast { deck.iter().filter_map(|actor| {
        let mut actor = actor.clone();
        actor.live_skill = None; actor.gekisou_skill = None; actor.gekisou_support_skills.clear();
        actor.support_skills.retain(|&(id, level)| master.support_skills[&id].iter().any(|row|
            row.level == level && row.kind == 12006 && !deferred(row)));
        (!actor.support_skills.is_empty()).then_some(actor)
    }).collect() } else { deck.to_vec() };
    let runs = if fast || !chart.luck { vec![0] } else { seeds.to_vec() };
    let key = (projected.clone(), if fast { Vec::new() } else { runs.clone() }, fast);
    let identity = tables(master);
    let known = {
        let mut cache = chart.factored.lock().unwrap();
        if cache.dead_heals.as_ref().is_none_or(|entry| entry.tables != identity) {
            cache.dead_heals = Some(DeadHealProofs { tables: identity, _master: master.clone(), results: HashMap::default() });
        }
        cache.dead_heals.as_ref().unwrap().results.get(&key).copied()
    };
    let dead = if let Some(dead) = known { dead } else {
        let generation = crate::cancellation_generation();
        let mut template = Model::new(master, &projected, &chart.full.notes, &[], chart.params(), (!fast).then_some(&chart.setup), (0, false))?;
        template.score.score.record_only = true;
        let mut dead = true;
        for seed in runs {
            let mut model = template.clone();model.random = Random::new(seed);
            for (index, frame) in chart.play.frames.iter().enumerate().take_while(|(_, frame)| frame.time_ms < first) {
                if generation != crate::cancellation_generation() { return Err("cancelled".into()); }
                model.count_frame(frame.time_ms, &frame.judged, chart.play.dt[index])?;
            }
            if model.life.at(first.saturating_sub(1)) != 0 { dead = false; break; }
        }
        let mut cache = chart.factored.lock().unwrap();
        if let Some(known) = &mut cache.dead_heals {
            if known.tables == identity {
                let values = &mut known.results;
                if values.len() >= 256 { values.clear(); }
                values.insert(key, dead);
            }
        }
        dead
    };
    if dead { for index in candidates { flags[index].1 = false; } }
    Ok(())
}
fn healthy_controls(master: &Master, chart: &Chart, deck: &[Performer], flags: &mut [(bool, bool)]) {
    if chart.luck { return; }
    let mut ordered = vec![true; deck.len()];
    healthy_metric_controls(master, chart, deck, &mut ordered);
    for (index, actor) in deck.iter().enumerate() {
        let random = actor.support_skills.iter().flat_map(|&(id, level)| master.support_skills[&id].iter().filter(move |row| row.level == level))
            .filter(|row| !(2000..=2005).contains(&row.kind) && row.kind != 15000)
            .flat_map(|row| [row.trigger_group, row.condition_group, row.release_group, row.reset_group])
            .flat_map(|group| master.condition_sets.get(&group).into_iter().flatten().flatten())
            .any(|id| master.conditions[id].kind == 4011);
        if !ordered[index] && !random { flags[index] = (false, false); }
    }
}
fn independent_row(master: &Master, row: &SkillEffect) -> bool {
    row.kind!=15000 && [row.trigger_group,row.condition_group,row.release_group,row.reset_group].iter()
        .flat_map(|group|master.condition_sets.get(group).into_iter().flatten().flatten())
        .all(|id|matches!(master.conditions[id].kind,0|1000..=1031|2000..=2004|4011|5000|7000..=7021))
}
pub(super) fn metric_order_groups(master: &Master, chart: &Chart, deck: &[Performer]) -> Vec<Vec<usize>> {
    let mut ordered: Vec<_> = deck.iter().map(|actor| ordinary(master, actor).is_none_or(|flags| flags.1)).collect();
    healthy_metric_controls(master, chart, deck, &mut ordered);
    for actor in deck {
        let normal = actor.live_skill.into_iter().flat_map(|(id, level)| master.live_skills[&id].iter().filter(move |row| row.level == level))
            .chain(actor.support_skills.iter().flat_map(|&(id, level)| master.support_skills[&id].iter().filter(move |row| row.level == level)));
        let gk: Vec<_> = actor.gekisou_skill.into_iter().flat_map(|(id, level)| {
            let skill = &master.gekiso_skills[&id];
            skill.effects.iter().filter(move |row| row.level == level && (skill.mission_type == 4 || chart.setup.missions.contains(&skill.mission_type)))
        }).chain(actor.gekisou_support_skills.iter().flat_map(|&(id, level)| {
            let skill = &master.gekiso_support_skills[&id];
            skill.effects.iter().filter(move |row| row.level == level && (skill.mission_type == 4 || chart.setup.missions.contains(&skill.mission_type)))
        })).collect();
        if gk.iter().any(|row| !independent_row(master, row)) || normal.chain(gk)
            .flat_map(|row| [row.trigger_group, row.condition_group, row.release_group, row.reset_group])
            .flat_map(|group| master.condition_sets.get(&group).into_iter().flatten().flatten())
            .any(|id| master.conditions[id].kind == 4011) { ordered.fill(true); break; }
    }
    let mut groups = Vec::<Vec<usize>>::new();
    let mut ids = HashMap::default();
    for (index, order) in live::orders().iter().enumerate() {
        let key: Vec<_> = order.iter().map(|&actor| if ordered[actor] { actor } else { usize::MAX }).collect();
        let next = groups.len();
        let group = *ids.entry(key).or_insert(next);
        if group == next { groups.push(Vec::new()); }
        groups[group].push(index);
    }
    groups
}
fn healthy_metric_controls(master: &Master, chart: &Chart, deck: &[Performer], ordered: &mut [bool]) {
    let base = master.live.life_base as i32;
    if base <= 0 || base > i32::MAX / 2 { return; }
    let normal: Vec<Vec<_>> = deck.iter().map(|actor| actor.live_skill.into_iter()
        .flat_map(|(id, level)| master.live_skills[&id].iter().filter(move |row| row.level == level))
        .chain(actor.support_skills.iter().flat_map(|&(id, level)| master.support_skills[&id].iter().filter(move |row| row.level == level))).collect()).collect();
    let all: Vec<_> = normal.iter().flatten().copied().chain(deck.iter().flat_map(|actor| {
        actor.gekisou_skill.into_iter().flat_map(|(id, level)| master.gekiso_skills[&id].effects.iter().filter(move |row| row.level == level))
            .chain(actor.gekisou_support_skills.iter().flat_map(|&(id, level)| master.gekiso_support_skills[&id].effects.iter().filter(move |row| row.level == level)))
    })).collect();
    let mut targets = HashMap::<i32, u8>::default();
    for row in all.iter().filter(|row| matches!(row.kind, 12006 | 13005)) {
        for id in &row.targets { let grade = master.targets[id].judgement; if (0..8).contains(&grade) { *targets.entry(row.id).or_default() |= 1 << grade; } }
    }
    let mut grades = chart.play.frames.iter().flat_map(|frame| &frame.judged).fold(0_u8, |mask, note| mask | (1 << note.judgement));
    loop {
        let previous = grades;
        for row in all.iter().filter(|row| matches!(row.kind, 12006 | 13005)) {
            if targets.get(&row.id).is_some_and(|mask| mask & grades != 0) {
                let grade = if row.kind == 13005 { 6 } else { row.value };
                if !(0..8).contains(&grade) { return; }
                grades |= 1 << grade;
            }
        }
        if grades == previous { break; }
    }
    if (1..=6).any(|grade| grades & (1 << grade) != 0 && master.live.damage[grade] != 0.0) { return; }
    for row in &all {
        if matches!(row.kind, 3000 | 3002) || matches!(row.kind, 3001 | 3004) && row.value < 0
            || row.cumulative_id != 0 && master.cumulative.get(&row.cumulative_id)
            .is_none_or(|counter| matches!(counter.kind, 2000 | 2001)) { return; }
        if row.kind == 3001 && (row.cumulative_id != 0 || row.value > i32::MAX - 2 * base) { return; }
        for id in [row.trigger_group, row.condition_group, row.release_group, row.reset_group].iter()
            .flat_map(|group| master.condition_sets.get(group).into_iter().flatten().flatten()) {
            let condition = &master.conditions[id];
            if matches!(condition.kind, 2000 | 2001) {
                let Some(&value) = condition.values.first() else { return; };
                let test = |life| if condition.kind == 2000 { life <= value } else { life >= value };
                if test(base as f64) != test((2 * base) as f64) { return; }
            } else if (2002..=2004).contains(&condition.kind) { return; }
        }
    }
    for (index, rows) in normal.iter().enumerate() {
        if rows.iter().all(|row| (2000..=2005).contains(&row.kind) || matches!(row.kind, 15000 | 3001 | 3003 | 3004)
            || row.kind == 12006 && (0..8).contains(&row.value) && targets.get(&row.id).is_none_or(|mask| mask & grades & !(1 << row.value) == 0)) {
            ordered[index] = false;
        }
    }
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
    (ordinary, order): (&[(bool, bool)], &[usize; 5]),
    samples: &mut SampleCache,
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
    let combo = chart.setup.missions.contains(&1);
    if combo { model.gk.as_mut().unwrap().controller.record_combo_history(); }
    let runs = if chart.luck {
        seeds.to_vec()
    } else {
        vec![seeds.first().copied().unwrap_or(0)]
    };
    let fixed_inputs = !combo && runs.len() > 1 && reduced.iter().all(|actor| {
        actor.support_skills.iter().all(|&(id, level)| control.support_skills[&id]
            .iter().filter(|row| row.level == level).all(|row|
                (matches!(row.kind, 3000..=3004 | 12006 | 15000)) && ordinary_row(control, row)))
            && actor.gekisou_skill.into_iter().filter_map(|(id, level)|
                control.gekiso_skills.get(&id).map(|skill| (skill, level)))
                .chain(actor.gekisou_support_skills.iter().filter_map(|&(id, level)|
                    control.gekiso_support_skills.get(&id).map(|skill| (skill, level))))
                .filter(|(skill, _)| skill.mission_type == 4 || chart.setup.missions.contains(&skill.mission_type))
                .flat_map(|(skill, level)| skill.effects.iter().filter(move |row| row.level == level))
                .all(|row| (2000..=2005).contains(&row.kind)
                    || (11000..=11005).contains(&row.kind)
                    || fixed_damage_reduction(control, row)
                    || matches!(row.kind, 4004 | 12000 | 12002 | 12003 | 12004 | 13000 | 13002 | 13003 | 13004))
    });
    let mut sampled = if fixed_inputs {
        let mut actors = reduced.clone();
        for actor in &mut actors { actor.support_skills.clear(); }
        let mut model = Model::new(control, &actors, &chart.full.notes, &key.events,
            chart.params(), Some(&chart.setup), (runs[0], false))?;
        model.score.score.replay = Some(Vec::new());
        model.score.score.factored_record = true;
        model.score.score.record_only = true;
        model.score.score.calculator.factors_only = true;
        Some(model)
    } else { None };
    let lottery_record = chart.setup.missions.iter().all(|&mission| mission == 2)
        && sampled.as_mut().is_some_and(Model::prepare_lottery_record);
    let mut prepared: Option<Vec<FixedFrame>> = None;
    let mut prepared_next = Vec::new();
    let mut note_life = if fixed_inputs { vec![0; chart.full.notes.len() + 1] } else { Vec::new() };
    let mut draws = Vec::new();
    let mut unique = HashMap::<Arc<ScoreTape>, ()>::default();
    let mut sample_input = None;
    let generation = crate::cancellation_generation();
    for (run, seed) in runs.iter().copied().enumerate() {
        let mut current = if run > 0 { sampled.as_ref().unwrap_or(&model) } else { &model }.clone();
        current.random = Random::new(seed);
        let sparse = current.sparse_inputs();
        let next = sparse.then(|| if run > 0 && fixed_inputs { prepared_next.as_slice() } else { chart.sparse_inputs() });
        let (mut until, mut quiet) = (0, 0);
        for (index, frame) in chart.play.frames.iter().enumerate() {
            if index % 64 == 0 && generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
            let fixed = prepared.as_ref().filter(|_| run > 0);
            if let Some(prepared) = fixed { current.life.freeze_frame(prepared[index].life); }
            if index < until {
                current.idle_frame(frame.time_ms)?;
            } else {
                if let Some(prepared) = fixed {
                    if lottery_record {
                        current.record_lottery_frame(frame.time_ms, &prepared[index].judged, chart.play.dt[index])?;
                    } else {
                        current.frame(frame.time_ms, &prepared[index].judged, chart.play.dt[index])?;
                    }
                } else {
                    current.frame(frame.time_ms, &frame.judged, chart.play.dt[index])?;
                }
                quiet = if sparse && current.inputs_settled() { quiet + 1 } else { 0 };
                if quiet >= 2 {
                    let wake = current.next_score_frame(frame.time_ms);
                    until = chart.before_wake(next.unwrap()[index + 1], wake);
                }
            }
            current.score.score.calculator.observations.clear();
        }
        let mut trace = current.score.score.replay.take().unwrap();
        if run == 0 && fixed_inputs {
            let mut inputs = Vec::with_capacity(chart.play.frames.len());
            let mut judged = Vec::new();
            for command in &trace {
                match command {
                    ReplayCommand::Input(notes) => judged = notes.clone(),
                    ReplayCommand::Ordinary(_, life) => inputs.push(FixedFrame { judged: std::mem::take(&mut judged), life: *life }),
                    ReplayCommand::Note(note) => note_life[note.id as usize] = note.life,
                    _ => {}
                }
            }
            debug_assert_eq!(inputs.len(), chart.play.frames.len());
            let base = chart.sparse_inputs();
            prepared_next = vec![chart.play.frames.len(); base.len()];
            let mut after = chart.play.frames.len();
            for index in (0..inputs.len()).rev() {
                if base[index] == index || index == 0 || inputs[index].life != inputs[index - 1].life { after = index; }
                prepared_next[index] = after;
            }
            prepared = Some(inputs);
        } else if fixed_inputs {
            for command in &mut trace {
                if let ReplayCommand::Note(note) = command { note.life = note_life[note.id as usize]; }
            }
        }
        let draw = Arc::new(ScoreTape::new(
            &trace,
            chart,
            control.live.life_base as i32,
        ));
        if let Some((stored, _)) = unique.get_key_value(&draw) {
            draws.push(Arc::clone(stored));
        } else {
            unique.insert(Arc::clone(&draw), ());
            draws.push(draw);
        }
        if run == 0 && fixed_inputs {
            let mut input = Vec::with_capacity(chart.play.frames.len() * 16 + chart.full.notes.len() * 5);
            for frame in prepared.as_ref().unwrap() {
                for &(life, maximum) in &frame.life {
                    input.extend_from_slice(&life.to_le_bytes());
                    input.extend_from_slice(&maximum.to_le_bytes());
                }
                input.extend(frame.judged.iter().map(|note| note.judgement as u8));
            }
            for life in &note_life { input.extend_from_slice(&life.to_le_bytes()); }
            if let Some(cached) = samples.get(&input) {
                for draw in cached {
                    if let Some((stored, _)) = unique.get_key_value(&draw) { draws.push(Arc::clone(stored)); }
                    else { unique.insert(Arc::clone(&draw), ()); draws.push(draw); }
                }
                break;
            }
            sample_input = Some(input);
        }
    }
    if let Some(input) = sample_input { samples.insert(input, &draws[1..]); }
    let bytes = unique
        .keys()
        .map(|draw| draw.retained_bytes())
        .sum::<usize>()
        + draws.capacity() * std::mem::size_of::<Arc<ScoreTape>>()
        + std::mem::size_of::<Entry>()
        + key.program.capacity() * 8
        + key.seeds.capacity() * 4
        + key.ranks.capacity() * 4
        + key.events.capacity() * std::mem::size_of::<(usize, i32)>();
    let context = Arc::new(Scene { draws, shared_inputs: fixed_inputs });
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
type OrdinaryTape = Arc<Vec<(usize, FactorCommand)>>;
type OrdinaryOrders = [Option<OrdinaryTape>; 120];
#[derive(Clone)]
struct ComboPlayback<'a> {
    ranges: &'a [(i32, i32)],
    missions: &'a [i32],
    history: [Vec<(i32, i32)>; 3],
}
impl crate::full::score::GekisoCombo for ComboPlayback<'_> {
    fn combo(&self, time: i32) -> Option<i32> {
        let range = self.ranges.iter().zip(self.missions).position(|(&(start, end), &mission)| {
            mission == 1 && start <= time && time <= end
        })?;
        let previous = core::frame(time).wrapping_mul(40).wrapping_sub(40);
        let row = &self.history[range];
        let at = row.partition_point(|&(time, _)| time <= previous);
        Some(at.checked_sub(1).map_or(0, |index| row[index].1))
    }
}

#[derive(Clone)]
struct ScoreState<'a> {
    program: Option<Model<'a>>,
    ordinary_record: Vec<(usize, FactorCommand)>,
    ordinary_replay: Option<OrdinaryTape>,
    ordinary_at: usize,
    score: Incremental<'a>,
    combo: core::Combo,
    gekiso_combo: Option<ComboPlayback<'a>>,
    judged: Vec<crate::full::play::JudgedNote>,
    life: [i32; 4],
    last: [i32; 2],
    ranges: [f64; 3],
    frame: usize,
    sparse: bool,
    next_frame: i32,
    previous_life: [i32; 4],
}
struct ScoreTree<'a, 'o> {
    chart: &'a Chart,
    trace: &'a ScoreTape,
    chronological: Vec<usize>,
    generation: u64,
    ordinary: Option<&'o mut OrdinaryOrders>,
    reuse_ordinary: bool,
}
impl<'a> ScoreTree<'a, '_> {
    fn select_ordinary_tape(&self, state: &mut ScoreState<'a>, order: usize) {
        let tape = self.ordinary.as_ref().unwrap()[order].as_ref().unwrap();
        state.ordinary_at = tape.partition_point(|&(frame, _)| frame < state.frame);
        state.next_frame = tape.get(state.ordinary_at)
            .map_or(i32::MAX, |&(frame, _)| self.chart.play.frames[frame].time_ms);
        state.ordinary_replay = Some(Arc::clone(tape));
    }

    fn calculate(&self, state: &mut ScoreState<'a>, time: i32) -> Result<(), String> {
        state.last[0] = state.last[1];
        let gekiso = state.gekiso_combo.as_ref().map(|combo| combo as &dyn crate::full::score::GekisoCombo);
        state.last[1] = state.score.calculate(time, &state.combo, gekiso)?;
        Ok(())
    }
    fn ordinary(&self, state: &mut ScoreState<'a>) -> Result<(), String> {
        let time = self.chart.play.frames[state.frame].time_ms;
        if let Some(tape) = &state.ordinary_replay {
            while state.ordinary_at < tape.len() && tape[state.ordinary_at].0 == state.frame {
                state.score.add_factor(tape[state.ordinary_at].1.clone());
                state.ordinary_at += 1;
            }
            state.previous_life = state.life;
            state.next_frame = tape.get(state.ordinary_at).map_or(i32::MAX, |&(frame, _)| self.chart.play.frames[frame].time_ms);
        } else {
            let program = state.program.as_mut().unwrap();
            if !state.sparse || !state.judged.is_empty()
                || state.previous_life != state.life || time >= state.next_frame {
                program.life.freeze_frame([
                    (state.life[0], state.life[1]), (state.life[2], state.life[3]),
                ]);
                program.count_frame(time, &state.judged, self.chart.play.dt[state.frame])?;
                state.previous_life = state.life;
                state.next_frame = program.next_score_frame(time);
            }
            for command in program.score.score.replay.as_mut().unwrap().drain(..) {
                if let ReplayCommand::Factor(mut command) = command {
                    command.owner *= 2;
                    if self.ordinary.is_some() { state.ordinary_record.push((state.frame, command.clone())); }
                    state.score.add_factor(command);
                }
            }
        }
        state.judged.clear();
        state.frame += 1;
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
                    if self.reuse_ordinary && self.trace.combos.is_some() {
                        at += self.chart.play.frames[state.frame].judged.len();
                    } else {
                        for raw in &self.chart.play.frames[state.frame].judged {
                            let judgement = i32::from(self.trace.bytes[at]);
                            at += 1;
                            if self.trace.combos.is_none() { state.combo.add(raw.time_ms, judgement); }
                            if !self.reuse_ordinary {
                                state.judged.push(crate::full::play::JudgedNote {
                                    note_id: raw.note_id,
                                    time_ms: raw.time_ms,
                                    judgement,
                                });
                            }
                        }
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
                            self.calculate(state, self.chart.play.frames[next - 1].time_ms)?;
                            state.last[0] = state.last[1];
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
                8 => {
                    let range = self.trace.bytes[at] as usize;
                    at += 1;
                    let from = self.trace.i32(&mut at) as usize;
                    let len = self.trace.i32(&mut at) as usize;
                    let history = &mut state.gekiso_combo.as_mut().unwrap().history[range];
                    history.truncate(from);
                    history.extend((0..len).map(|_| (self.trace.i32(&mut at), self.trace.i32(&mut at))));
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
        &mut self,
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
        if let Some(program) = &mut state.program {
            let events: Vec<_> = self.chart.full.events.iter()
                .map(|&(event, time)| (representative[event], time)).collect();
            program.set_event_members(&events);
        }
        if self.reuse_ordinary {
            self.select_ordinary_tape(&mut state, orders[0]);
        }
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
            let tape = (!self.reuse_ordinary && self.ordinary.is_some())
                .then(|| Arc::new(std::mem::take(&mut state.ordinary_record)));
            for &order in orders {
                values[order] = (state.score.score as f64, state.ranges);
                if let Some(tape) = &tape { self.ordinary.as_mut().unwrap()[order] = Some(Arc::clone(tape)); }
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
    chart: &'a Chart,
    deck: &[Performer],
    draw: &'a ScoreTape,
    power: i32,
    orders: &[usize],
    (ordinary, ordinary_orders, note_memo): (&'a Master, Option<&mut OrdinaryOrders>, &mut Option<std::rc::Rc<crate::full::score::NoteMemo>>),
    (prefix_end, prefix): (usize, &mut Option<ScoreState<'a>>),
) -> Result<Option<Vec<ReplayValue>>, String> {
    let reuse_ordinary = ordinary_orders.as_ref().is_some_and(|rows| orders.iter().all(|&order| rows[order].is_some()));
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
    let program = if reuse_ordinary { None } else {
        let mut program = Model::with_live_capacity(
            ordinary, deck, &chart.full.notes, &events, params.clone(), None,
            (0, false, Some(&capacity)),
        )?;
        program.score.score.replay = Some(Vec::new());
        program.score.score.record_only = true;
        Some(program)
    };
    let sparse = program.as_ref().is_none_or(Model::sparse_score_frames);
    let mut calculator = Calculator::new(
        &master.live,
        power,
        params.level,
        params.converted,
        params.assist,
    );
    calculator.note_combos.clone_from(&draw.combos);
    calculator.share_note_memo(note_memo, chart.full.notes.len());
    let mut state = ScoreState {
        program,
        ordinary_record: Vec::new(),
        ordinary_replay: None,
        ordinary_at: 0,
        score: Incremental::new(calculator, params.music_length),
        combo: core::Combo::default(),
        gekiso_combo: chart.setup.missions.contains(&1).then(|| ComboPlayback {
            ranges: &chart.setup.fevers, missions: &chart.setup.missions,
            history: std::array::from_fn(|_| Vec::new()),
        }),
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
    let mut tree = ScoreTree {
        chart,
        trace,
        chronological,
        generation: crate::cancellation_generation(),
        ordinary: ordinary_orders,
        reuse_ordinary,
    };
    let mut values = vec![(0.0, [0.0; 3]); 120];
    let mut from = 0;
    if reuse_ordinary && prefix_end > 0 {
        if let Some(prior) = prefix.as_ref() {
            state = prior.clone();
        } else {
            tree.select_ordinary_tape(&mut state, orders[0]);
            tree.advance(&mut state, 0, prefix_end)?;
            *prefix = Some(state.clone());
        }
        from = prefix_end;
    }
    tree.visit(state, orders, from, 0, &mut values)?;
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
            .any(|&mission| !(1..=3).contains(&mission))
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
    ignore_dead_heal_order(master, chart, deck, seeds, &mut ordinary)?;
    let (control, ordinary_master) = views(master, chart);
    let classes = control_classes(master, chart, deck, &ordinary);
    let mut groups = Vec::<Vec<usize>>::new();
    let mut ids = HashMap::default();
    for (index, order) in live::orders().iter().enumerate() {
        let key: Vec<_> = order
            .iter()
            .map(|&actor| classes[actor])
            .collect();
        let next = groups.len();
        let group = *ids.entry(key).or_insert(next);
        if group == next {
            groups.push(Vec::new());
        }
        groups[group].push(index);
    }
    // A damaging COMBO play can have a separate controller for every
    // ordinary order. Split out its score timeline only when the orders share it.
    if chart.setup.missions.contains(&1) && uses_full_replay(master, chart, seeds)
        && groups.len() > 1
    {
        return Ok(None);
    }
    let generation = crate::cancellation_generation();
    let mut values = vec![0.0; 120];
    let mut range_values = [[0.0; 3]; 120];
    let mut note_memo = None;
    let mut sample_cache = SampleCache::default();
    for orders in groups {
        let context = context(
            master,
            &control,
            chart,
            deck,
            seeds,
            (&ordinary, &live::orders()[orders[0]]),
            &mut sample_cache,
        )?;
        let draws = &context.draws;
        let mut scored = HashMap::<*const ScoreTape, Vec<ReplayValue>>::default();
        let mut ordinary_orders: OrdinaryOrders = std::array::from_fn(|_| None);
        let reuse_ordinary = context.shared_inputs && draws.iter().skip(1)
            .any(|draw| !Arc::ptr_eq(draw, &draws[0]));
        let mut sample_prefix = None;
        let prefix_end = if reuse_ordinary {
            // Stop before the first ordinary activation. A different control
            // stream or combo table keeps its own scoring prefix.
            let first = &draws[0];
            let end = first.offsets.iter().copied().min().unwrap_or(0).min(first.bytes.len());
            if draws.iter().skip(1).all(|draw| {
                draw.offsets.iter().copied().min().is_some_and(|at| at >= end)
                    && draw.base == first.base && draw.combos == first.combos
                    && draw.bytes.get(..end) == Some(&first.bytes[..end])
            }) { end } else { 0 }
        } else { 0 };
        for draw in draws {
            if generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
            let result = match scored.entry(Arc::as_ptr(draw)) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let Some(result) = score_orders(master, chart, deck, draw, power, &orders,
                        (&ordinary_master, reuse_ordinary.then_some(&mut ordinary_orders), &mut note_memo),
                        (prefix_end, &mut sample_prefix))? else {
                        return Ok(None);
                    };
                    entry.insert(result)
                }
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
