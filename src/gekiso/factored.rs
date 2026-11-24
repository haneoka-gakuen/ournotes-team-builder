//! Native score replays share judgement, LIFE and Gekiso control streams.
//! Lottery-independent ordinary factor commands are replayed at their delivery
//! frame across all sampled draws. Equivalent input/LIFE streams share bounded
//! sample tapes; each of the 120 scores and ranks is settled separately.
use super::*;
use crate::master::SkillEffect;
pub(super) mod upper;

#[derive(Clone, PartialEq, Eq)]
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
    projection: std::sync::OnceLock<Option<Arc<upper::Projection>>>,
}
struct FixedFrame {
    judged: Vec<crate::full::play::JudgedNote>,
    life: [(i32, i32); 2],
}

struct SamplePrefix<'a> {
    model: Model<'a>,
    at: usize,
    until: usize,
    quiet: usize,
    commands: Vec<ReplayCommand>,
}
struct SampleEntry {
    input: Vec<u8>,
    draws: Vec<Arc<ScoreTape>>,
    bytes: usize,
}
#[derive(Default)]
struct SampleCache {
    entries: std::collections::VecDeque<Arc<SampleEntry>>,
    bytes: usize,
}
impl SampleCache {
    fn get(&mut self, input: &[u8]) -> Option<Arc<SampleEntry>> {
        let index = self.entries.iter().position(|entry| entry.input == input)?;
        let entry = self.entries.remove(index).unwrap();
        self.entries.push_back(Arc::clone(&entry));
        Some(entry)
    }
    fn entry(input: Vec<u8>, draws: &[Arc<ScoreTape>]) -> Arc<SampleEntry> {
        let mut unique = std::collections::HashSet::new();
        let mut combos = HashSet::default();
        let bytes = input.capacity() + std::mem::size_of_val(draws)
            + draws.iter().filter(|draw| unique.insert(Arc::as_ptr(draw))).map(|draw| draw.retained_bytes(&mut combos)).sum::<usize>() + 128;
        Arc::new(SampleEntry { input, draws: draws.to_vec(), bytes })
    }
    fn insert(&mut self, entry: Arc<SampleEntry>) {
        const LIMIT: usize = 4 * 1024 * 1024;
        if entry.bytes > LIMIT { return; }
        while self.bytes + entry.bytes > LIMIT {
            if let Some(entry) = self.entries.pop_front() { self.bytes -= entry.bytes; } else { break; }
        }
        self.bytes += entry.bytes;
        self.entries.push_back(entry);
    }
}

struct SavedSamples {
    key: Key,
    entry: Arc<SampleEntry>,
    _master: Master,
}

#[derive(Default)]
struct SharedSamples {
    entries: std::collections::VecDeque<SavedSamples>,
    bytes: usize,
}
impl SharedSamples {
    fn get(&mut self, key: &Key, input: &[u8]) -> Option<Arc<SampleEntry>> {
        let index = self.entries.iter().position(|saved| saved.key == *key && saved.entry.input == input)?;
        let hit = self.entries.remove(index).unwrap();
        let entry = Arc::clone(&hit.entry);
        self.entries.push_back(hit);
        Some(entry)
    }
    fn insert(&mut self, key: Key, entry: Arc<SampleEntry>, master: &Master) {
        const LIMIT: usize = 2 * 1024 * 1024;
        if entry.bytes > LIMIT { return; }
        while self.bytes + entry.bytes > LIMIT || self.entries.len() >= 8 {
            let Some(old) = self.entries.pop_front() else { break; };
            self.bytes -= old.entry.bytes;
        }
        self.bytes += entry.bytes;
        self.entries.push_back(SavedSamples { key, entry, _master: master.clone() });
    }
}

#[derive(PartialEq, Eq, Hash)]
struct ScoreTape {
    bytes: Vec<u8>,
    offsets: Vec<usize>,
    factor_times: Vec<i32>,
    base: i32,
    living_through: i32,
    combos: Arc<crate::full::score::CachedCombos>,
}
impl ScoreTape {
    fn new(trace: &[ReplayCommand], chart: &Chart, base: i32, shared_combos: Option<&Arc<crate::full::score::CachedCombos>>) -> Self {
        let mut bytes = Vec::with_capacity(chart.notes.len().saturating_mul(12));
        let mut factor_times = Vec::new();
        let mut frame = 0;
        let mut living_through = i32::MIN;
        let mut offsets = vec![usize::MAX; chart.full.events.len()];
        let mut events: Vec<_> = chart.full.events.iter().enumerate()
            .map(|(index, &(_, time))| (time, index)).collect();
        events.sort_unstable();
        let mut event = 0;
        let mut life = [base; 4];
        let put = |bytes: &mut Vec<u8>, value: i32| bytes.extend_from_slice(&value.to_le_bytes());
        let commands: Vec<_> = trace.iter().filter(|command| match command {
            ReplayCommand::Rush(..) => false,
            ReplayCommand::Input(notes) => !notes.is_empty(),
            _ => true,
        }).collect();
        let mut at = 0;
        while at < commands.len() {
            let mut idle = 0;
            while idle < u16::MAX as usize {
                let [ReplayCommand::Calculate(before), ReplayCommand::Ordinary(time, values),
                     ReplayCommand::Calculate(after), ..] = &commands[at + idle * 3..] else { break; };
                let current = frame + idle;
                if before != time || after != time
                    || chart.play.frames.get(current).is_none_or(|row| row.time_ms != *time)
                    || chart.play.frames.get(current + 1).is_some_and(|row| row.time_ms == *time)
                    || events.get(event).is_some_and(|&(next, _)| next <= *time)
                    || [values[0].0, values[0].1, values[1].0, values[1].1] != life
                { break; }
                idle += 1;
            }
            if idle > 0 {
                if idle == 1 {
                    bytes.extend_from_slice(&[5, 16, 6]);
                } else {
                    bytes.push(7);
                    bytes.extend_from_slice(&(idle as u16).to_le_bytes());
                }
                at += idle * 3;
                frame += idle;
                continue;
            }
            let command = commands[at];
            at += 1;
            match command {
                ReplayCommand::Input(input) => {
                    if !input.is_empty() {
                        bytes.push(0);
                        bytes.extend(input.iter().map(|note| note.judgement as u8));
                    }
                }
                ReplayCommand::Note(note) => {
                    if note.life > 0 { living_through = living_through.max(note.time); }
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
                    if chart.luck && (c.note != 0 || c.combo != 0 || c.judge != 0) {
                        factor_times.push(c.time);
                    }
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
                ReplayCommand::IdleFrames(mut count) => {
                    debug_assert!(events.get(event).is_none_or(|&(time, _)|
                        time > chart.play.frames[frame + count - 1].time_ms));
                    while count > 0 {
                        let frames = count.min(u16::MAX as usize);
                        bytes.push(7);
                        bytes.extend_from_slice(&(frames as u16).to_le_bytes());
                        frame += frames;
                        count -= frames;
                    }
                }
                ReplayCommand::Ordinary(time, values) => {
                    while let Some(&(_, index)) = events.get(event).filter(|&&(at, _)| at <= *time) {
                        offsets[index] = bytes.len();
                        event += 1;
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
        }
        for offset in &mut offsets {
            if *offset == usize::MAX {
                *offset = bytes.len();
            }
        }
        bytes.shrink_to_fit();
        let combos = if let Some(shared) = shared_combos {
            #[cfg(debug_assertions)]
            debug_assert!(*Self::compile_combos(trace, chart) == **shared, "lottery samples changed the normal combo history");
            Arc::clone(shared)
        } else {
            Self::compile_combos(trace, chart)
        };
        factor_times.sort_unstable();
        factor_times.dedup();
        Self { bytes, offsets, factor_times, base, living_through, combos }
    }
    fn compile_combos(trace: &[ReplayCommand], chart: &Chart) -> Arc<crate::full::score::CachedCombos> {
        let mut delivered = Vec::with_capacity(chart.full.notes.len());
        let mut scored = vec![0; chart.full.notes.len() + 1];
        let mut frame = 0;
        for (index, command) in trace.iter().enumerate() {
            match command {
                ReplayCommand::Input(input) => {
                    for (raw, converted) in chart.play.frames[frame].judged.iter().zip(input) {
                        if !matches!(converted.judgement, 0 | 7) {
                            delivered.push((raw.time_ms, index + 1, converted.judgement));
                        }
                    }
                }
                ReplayCommand::Note(note) => {
                    let seen = &mut scored[note.id as usize];
                    if *seen == 0 { *seen = index + 1; }
                }
                ReplayCommand::Ordinary(..) => frame += 1,
                ReplayCommand::IdleFrames(count) => frame += count,
                _ => {}
            }
        }
        delivered.sort_by_key(|&(time, index, _)| (time, index));
        let mut inputs = delivered.into_iter().peekable();
        let (mut combo, mut required) = (0, 0);
        let mut values = vec![0; scored.len()];
        let mut changes = Vec::new();
        for &(id, time) in chart.prepared_notes().chronological() {
            while inputs.peek().is_some_and(|&(at, _, _)| at < time) {
                let (_, index, grade) = inputs.next().unwrap();
                if matches!(grade, 1 | 2) {
                    combo = 0;
                    required = index;
                } else {
                    combo += 1;
                    required = required.max(index);
                }
            }
            let seen = scored[id as usize];
            if seen != 0 {
                // Inputs before the final break cannot change the count once
                // the break and the following notes have all been delivered.
                values[id as usize] = if required <= seen {
                    combo
                } else {
                    changes.push(Vec::new());
                    -(changes.len() as i32)
                };
            }
        }
        if !changes.is_empty() {
            let mut combo = core::Combo::default();
            let mut observed = std::collections::BTreeMap::<i32, Vec<usize>>::new();
            let (mut frame, mut input_index) = (0, 0);
            for command in trace {
                match command {
                    ReplayCommand::Input(inputs) if !inputs.is_empty() => {
                        input_index += 1;
                        let mut earliest = i32::MAX;
                        for (raw, note) in chart.play.frames[frame].judged.iter().zip(inputs) {
                            if matches!(note.judgement, 0 | 7) { continue; }
                            combo.add(raw.time_ms, note.judgement);
                            earliest = earliest.min(raw.time_ms);
                        }
                        for (&time, rows) in observed.range((std::ops::Bound::Excluded(earliest), std::ops::Bound::Unbounded)) {
                            let count = combo.at(time);
                            for &row in rows {
                                if changes[row].last().is_none_or(|&(_, previous)| previous != count) {
                                    changes[row].push((input_index, count));
                                }
                            }
                        }
                    }
                    ReplayCommand::Note(note) => {
                        let value = values[note.id as usize];
                        if value < 0 {
                            let row = (-value - 1) as usize;
                            if changes[row].is_empty() {
                                observed.entry(note.time).or_default().push(row);
                                changes[row].push((input_index, combo.at(note.time)));
                            }
                        }
                    }
                    ReplayCommand::Ordinary(..) => frame += 1,
                    ReplayCommand::IdleFrames(count) => frame += count,
                    _ => {}
                }
            }
        }
        Arc::new(crate::full::score::CachedCombos { values, changes })
    }
    fn i32(&self, at: &mut usize) -> i32 {
        let value = i32::from_le_bytes(self.bytes[*at..*at + 4].try_into().unwrap());
        *at += 4;
        value
    }
    fn retained_bytes(&self, combos: &mut HashSet<*const crate::full::score::CachedCombos>) -> usize {
        let bytes = std::mem::size_of::<Self>()
            + self.bytes.capacity()
            + self.factor_times.capacity() * std::mem::size_of::<i32>()
            + self.offsets.capacity() * std::mem::size_of::<usize>();
        if !combos.insert(Arc::as_ptr(&self.combos)) { return bytes; }
        bytes + std::mem::size_of::<crate::full::score::CachedCombos>()
            + self.combos.values.capacity() * std::mem::size_of::<i32>()
            + self.combos.changes.iter().map(|row| row.capacity() * std::mem::size_of::<(usize, i32)>()).sum::<usize>()
            + self.combos.changes.capacity() * std::mem::size_of::<Vec<(usize, i32)>>()
    }

    fn ordinary_start(&self) -> usize {
        self.offsets.iter().copied().min().unwrap_or(0).min(self.bytes.len())
    }

    fn shares_prefix(&self, first: &Self, end: usize) -> bool {
        self.ordinary_start() >= end && self.base == first.base && self.combos == first.combos
            && self.bytes.get(..end) == Some(&first.bytes[..end])
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
    all_orders: Option<Arc<OrderBranch>>,
    projection_eligibility: Option<(Arc<crate::master::LiveTables>, bool)>,
    projection_inputs: Option<Arc<upper::ProjectionInputs>>,
    sampled_inputs: SharedSamples,
    measured_ordinary: Option<MeasuredOrdinary>,
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

pub(super) fn sample_ceiling(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
    power: i32,
) -> Result<Option<f64>, String> {
    if seeds.len() < 8 || power <= 0
        || chart.setup.missions.iter().any(|&mission| mission != 2)
        || !(0..=100).contains(&master.live.gekiso.rush_percent)
        || chart.range_percent.iter().any(|&percent| percent < 0)
        || !matching_missions(master, deck)
    { return Ok(None); }
    for actor in deck {
        if ordinary(master, actor).is_none() { return Ok(None); }
        let ordinary_rows = actor.live_skill.into_iter().flat_map(|(id, level)| {
            master.live_skills[&id].iter().filter(move |row| row.level == level)
        }).chain(actor.support_skills.iter().flat_map(|&(id, level)| {
            master.support_skills[&id].iter().filter(move |row| row.level == level)
        }));
        if ordinary_rows.into_iter().any(|row| {
            !(0..=1_000_000).contains(&row.value) || !(0..=1_000_000).contains(&row.max_value)
                || !ordinary_row(master, row)
                || !matches!(row.kind, 2000 | 2001 | 2004 | 3000..=3004 | 12006 | 15000)
        }) { return Ok(None); }
        let active = actor.gekisou_skill.into_iter().filter_map(|(id, level)| {
            master.gekiso_skills.get(&id).map(|skill| (skill, level))
        }).chain(actor.gekisou_support_skills.iter().filter_map(|&(id, level)| {
            master.gekiso_support_skills.get(&id).map(|skill| (skill, level))
        })).filter(|(skill, _)| matches!(skill.mission_type, 2 | 4))
            .flat_map(|(skill, level)| skill.effects.iter().filter(move |row| row.level == level));
        if active.into_iter().any(|row| {
            let disabled = crate::rewards::static_gk_group(master, row.condition_group, actor) == Some(false)
                || crate::rewards::static_gk_group(master, row.trigger_group, actor) == Some(false);
            !disabled && !(11000..=11005).contains(&row.kind) && !fixed_damage_reduction(master, row)
        }) {
            return Ok(None);
        }
    }
    let Some(ranges) = crate::rewards::deterministic_ranges(master, chart) else { return Ok(None); };
    if ranges.windows(2).any(|range| range[0].1 >= range[1].0) { return Ok(None); }
    let Some(first) = scores(master, chart, deck, &seeds[..1], power)? else { return Ok(None); };
    let ratio = (f(f(100.0 + master.live.gekiso.rush_percent as f64) / 100.0)
        * (1.0 + 16.0 * f32::EPSILON as f64)).next_up();
    // All seeds use the same ordinary score and LIFE. Rush is the only
    // additional factor, followed by integer note and rank rounding.
    let rank = chart.range_percent.iter().copied().max().unwrap_or(0) as f64;
    let rounding = (chart.notes.len() + chart.range_percent.len()) as f64 * (1.0 + rank / 100.0) * 8.0;
    let other = ((first.scores.mean * ratio).next_up() + rounding).next_up();
    let mean = ((first.scores.mean + (other * (seeds.len() - 1) as f64).next_up()).next_up()
        / seeds.len() as f64).next_up();
    Ok(Some(mean))
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
            if crate::rewards::static_gk_group(master, row.condition_group, actor)? {
                values.push(row.value);
            }
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
        .is_some_and(|sets| !sets.is_empty() && sets.iter().all(|set| set.iter().any(|id|
            master.conditions[id].positive && kinds.contains(&master.conditions[id].kind))));
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
        let mut template = Model::with_prepared_notes(
            master, &projected, chart.prepared_notes(), &[], chart.params(), (!fast).then_some(&chart.setup), (0, false, None),
        )?;
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
    if dead {
        for index in candidates {
            flags[index] = (false, false);
        }
    }
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
        let support_key = (supports.len() == actor.gekisou_support_skills.len())
            .then(|| crate::gekiso_objective::replay_gekiso_support_key(master, actor))
            .flatten();
        let recovery_key = recovery_program(master, actor);
        let rows: Vec<_> = member
            .into_iter()
            .flat_map(|(id, level)| {
                master.gekiso_skills[&id]
                    .effects
                    .iter()
                    .filter(move |row| row.level == level)
                    .map(|row| (row, false))
            })
            .chain(supports.iter().flat_map(|&(id, level)| {
                master.gekiso_support_skills[&id]
                    .effects
                    .iter()
                    .filter(move |row| row.level == level)
                    .map(|row| (row, support_key.is_some()))
            }))
            .chain(ordinary.iter().flat_map(|&(id, level)| {
                master.support_skills[&id]
                    .iter()
                    .filter(move |row| row.level == level && row.kind != 15000)
                    .map(|row| (row, recovery_key.is_some()))
            }))
            .collect();
        let mut owner = Vec::new();
        for (row, static_owner) in rows {
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
                    if condition.kind == 5000 && !static_owner {
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
        ];
        if let Some(key) = support_key {
            code.extend([1, key.len() as u64]);
            code.extend(key);
        } else {
            code.extend([0, supports.len() as u64]);
            for (id, level) in supports {
                code.extend([id as u64, level as u64]);
            }
        }
        if let Some(key) = recovery_key {
            code.extend([1, key.len() as u64]);
            code.extend(key.into_iter().map(|value| value as u64));
        } else {
            code.extend([0, ordinary.len() as u64]);
            for (id, level) in ordinary {
                code.extend([id as u64, level as u64]);
            }
        }
        code.extend(owner);
        result.push(code.len() as u64);
        result.extend(code);
    }
    result
}

type DrawObserver<'a> = dyn FnMut(&Arc<ScoreTape>, bool) -> Result<bool, String> + 'a;

fn context(
    master: &Master,
    control: &Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
    (ordinary, order): (&[(bool, bool)], &[usize; 5]),
    samples: &mut SampleCache,
    mut observe: Option<&mut DrawObserver<'_>>,
) -> Result<Option<Arc<Scene>>, String> {
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
    let cached = {
        let mut cache = chart.factored.lock().unwrap();
        cache.entries.iter().position(|entry| entry.key == key).map(|index| {
            let entry = cache.entries.remove(index).unwrap();
            let context = Arc::clone(&entry.context);
            cache.entries.push_back(entry);
            context
        })
    };
    if let Some(cached) = cached {
        if let Some(observe) = &mut observe {
            for draw in &cached.draws {
                if !observe(draw, cached.shared_inputs)? { return Ok(None); }
            }
        }
        return Ok(Some(cached));
    }
    let mut model = Model::with_prepared_notes(
        control,
        &reduced,
        chart.prepared_notes(),
        &key.events,
        chart.params(),
        Some(&chart.setup),
        (seeds.first().copied().unwrap_or(0), false, None),
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
    let sampled_actors = fixed_inputs.then(|| {
        let mut actors = reduced.clone();
        for actor in &mut actors { actor.support_skills.clear(); }
        actors
    });
    let shared_key = sampled_actors.as_ref().filter(|_| chart.luck && runs.len() >= 8).map(|actors| Key {
        program: control_program(control, chart, actors),
        ..key.clone()
    });
    let mut sampled = None;
    let mut lottery_record = false;
    let mut prepared: Option<Vec<FixedFrame>> = None;
    let mut prepared_next = Vec::new();
    let mut note_life = if fixed_inputs { vec![0; chart.full.notes.len() + 1] } else { Vec::new() };
    let mut draws: Vec<Arc<ScoreTape>> = Vec::new();
    let mut unique = HashMap::<Arc<ScoreTape>, ()>::default();
    let mut sample_input = None;
    let mut capture = Vec::new();
    let shared_prefix_at = chart.setup.fevers.iter().zip(&chart.setup.missions)
        .filter_map(|(&(start, _), &mission)| (mission == 2).then_some(start))
        .min().map(|time| chart.play.frames.partition_point(|frame| frame.time_ms < time))
        .filter(|&index| fixed_inputs && runs.len() >= 8 && index >= 120);
    let mut shared_prefix: Option<SamplePrefix<'_>> = None;
    let generation = crate::cancellation_generation();
    for (run, seed) in runs.iter().copied().enumerate() {
        if run == 1 {
            if let Some(actors) = &sampled_actors {
                let mut replay = Model::with_prepared_notes(control, actors, chart.prepared_notes(), &key.events,
                    chart.params(), Some(&chart.setup), (runs[0], false, None))?;
                replay.score.score.replay = Some(Vec::new());
                replay.score.score.factored_record = true;
                replay.score.score.record_only = true;
                replay.score.score.calculator.factors_only = true;
                lottery_record = chart.setup.missions.iter().all(|&mission| mission == 2)
                    && replay.prepare_lottery_record();
                sampled = Some(replay);
            }
        }
        let (mut current, mut index, mut until, mut quiet) = if let Some(prefix) = &shared_prefix {
            capture.extend_from_slice(&prefix.commands);
            (prefix.model.clone(), prefix.at, prefix.until, prefix.quiet)
        } else {
            let initial = if run > 0 { sampled.as_ref().unwrap_or(&model) } else { &model };
            (initial.clone(), 0, 0, 0)
        };
        current.random = Random::new(seed);
        current.score.score.replay = Some(std::mem::take(&mut capture));
        let sparse = current.sparse_inputs();
        let next = sparse.then(|| if run > 0 && fixed_inputs { prepared_next.as_slice() } else { chart.sparse_inputs() });
        while index < chart.play.frames.len() {
            let frame = &chart.play.frames[index];
            if index % 64 == 0 && generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
            // The remaining seeds use this same fixed-input model. Replacing
            // its random streams is exact before the first random draw.
            if run == 1 && shared_prefix.is_none() && current.random.draws == 0
                && shared_prefix_at.is_some_and(|at| index >= at)
            {
                let commands = current.score.score.replay.take().unwrap();
                shared_prefix = Some(SamplePrefix {
                    model: current.clone(), at: index, until, quiet, commands: commands.clone(),
                });
                current.score.score.replay = Some(commands);
            }
            let fixed = prepared.as_ref().filter(|_| run > 0);
            if let Some(prepared) = fixed { current.life.freeze_frame(prepared[index].life); }
            if index < until {
                // The precomputed wake table stops at both skill events and
                // LIFE changes. The score tape can keep the idle run compact.
                let end = until.min(chart.play.frames.len());
                if end > index + 1 && index > 0
                    && fixed.is_some_and(|prepared| prepared[index - 1].life == prepared[index].life)
                    && current.record_idle_run(chart.play.frames[end - 1].time_ms, end - index)
                {
                    index = end;
                    continue;
                }
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
                    until = chart.idle_until(&current, index, next.unwrap()[index + 1]);
                }
            }
            current.score.score.calculator.observations.clear();
            index += 1;
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
        let shared_combos = if fixed_inputs { draws.first().map(|first| &first.combos) } else { None };
        let draw = Arc::new(ScoreTape::new(&trace, chart, control.live.life_base as i32, shared_combos));
        trace.clear();
        capture = trace;
        if let Some((stored, _)) = unique.get_key_value(&draw) {
            draws.push(Arc::clone(stored));
        } else {
            unique.insert(Arc::clone(&draw), ());
            draws.push(draw);
        }
        if let Some(observe) = &mut observe {
            if !observe(draws.last().unwrap(), fixed_inputs)? { return Ok(None); }
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
            let cached = samples.get(&input).or_else(|| {
                let key = shared_key.as_ref()?;
                let entry = chart.factored.lock().unwrap().sampled_inputs.get(key, &input)?;
                samples.insert(Arc::clone(&entry));
                Some(entry)
            });
            if let Some(cached) = cached {
                for draw in &cached.draws {
                    if let Some((stored, _)) = unique.get_key_value(draw) { draws.push(Arc::clone(stored)); }
                    else { unique.insert(Arc::clone(draw), ()); draws.push(Arc::clone(draw)); }
                    if let Some(observe) = &mut observe {
                        if !observe(draws.last().unwrap(), fixed_inputs)? { return Ok(None); }
                    }
                }
                break;
            }
            sample_input = Some(input);
        }
    }
    if let Some(input) = sample_input {
        let entry = SampleCache::entry(input, &draws[1..]);
        samples.insert(Arc::clone(&entry));
        if let Some(key) = shared_key {
            chart.factored.lock().unwrap().sampled_inputs.insert(key, entry, master);
        }
    }
    let mut combos = HashSet::default();
    let projection_bytes = if uses_full_replay(master, chart, seeds) {
        (chart.notes.len() + 1) * std::mem::size_of::<[[f64; 2]; 5]>()
            + if chart.luck {
                (chart.play.frames.len() + (core::frame(chart.music_length) + 50).max(1) as usize + 2)
                    * std::mem::size_of::<usize>()
            } else { 0 }
    } else { 0 };
    let bytes = unique
        .keys()
        .map(|draw| draw.retained_bytes(&mut combos))
        .sum::<usize>()
        + draws.capacity() * std::mem::size_of::<Arc<ScoreTape>>()
        + std::mem::size_of::<Entry>()
        + key.program.capacity() * 8
        + key.seeds.capacity() * 4
        + key.ranks.capacity() * 4
        + key.events.capacity() * std::mem::size_of::<(usize, i32)>()
        + projection_bytes;
    let context = Arc::new(Scene { draws, shared_inputs: fixed_inputs, projection: std::sync::OnceLock::new() });
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
    Ok(Some(context))
}
type OrdinaryTape = Arc<Vec<(usize, FactorCommand)>>;
type OrdinaryOrders = [Option<OrdinaryTape>; 120];
struct MeasuredOrdinary {
    input: Arc<ScoreTape>,
    deck: Vec<Performer>,
    orders: Vec<usize>,
    factors: OrdinaryOrders,
}

fn take_measured_ordinary(chart: &Chart, deck: &[Performer], orders: &[usize], input: &Arc<ScoreTape>) -> OrdinaryOrders {
    let mut cache = chart.factored.lock().unwrap();
    if cache.measured_ordinary.as_ref().is_some_and(|saved|
        Arc::ptr_eq(&saved.input, input) && saved.deck == deck && saved.orders == orders
    ) {
        return cache.measured_ordinary.take().unwrap().factors;
    }
    std::array::from_fn(|_| None)
}
struct OrdinaryClasses {
    representatives: Vec<usize>,
    sources: [usize; 120],
    branch: OrderBranch,
}
impl OrdinaryClasses {
    fn new(chart: &Chart, draws: &[Arc<ScoreTape>], orders: &[usize], ordinary: &OrdinaryOrders) -> Result<Option<Self>, String> {
        let control_times: HashSet<_> = draws.iter().flat_map(|draw| draw.factor_times.iter().copied()).collect();
        let mut sources = std::array::from_fn(|index| index);
        let mut known = HashMap::default();
        let mut representatives = Vec::new();
        for &order in orders {
            let tape = ordinary[order].as_ref().unwrap();
            let mut seen = HashSet::new();
            let repeated: HashSet<_> = tape.iter().filter_map(|(_, command)| {
                (!seen.insert(command.time)).then_some(command.time)
            }).collect();
            let key: Vec<_> = tape.iter().map(|(frame, command)| {
                // The owner determines floating-point addition order only
                // when more than one factor has the same timestamp.
                let owner = if repeated.contains(&command.time) || control_times.contains(&command.time) {
                    command.owner
                } else { 0 };
                (*frame, [command.time, owner, command.note, command.combo,
                    command.judgement, command.judge, command.luck])
            }).collect();
            let source = *known.entry(key).or_insert(order);
            sources[order] = source;
            if source == order { representatives.push(order); }
        }
        if representatives.len() == orders.len() { return Ok(None); }
        let branch = OrderBranch::new(&representatives, &chart.full.events)?;
        Ok(Some(Self { representatives, sources, branch }))
    }

    fn layout<'a>(&'a self) -> (&'a [usize], &'a OrderBranch) {
        (&self.representatives, &self.branch)
    }

    fn expand(&self, orders: &[usize], values: &mut [ReplayValue]) {
        for &order in orders { values[order] = values[self.sources[order]]; }
    }
}
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
struct OrderBranch {
    representative: usize,
    target: Option<usize>,
    children: Vec<OrderBranch>,
    orders: Vec<usize>,
}
impl OrderBranch {
    fn for_chart(chart: &Chart, orders: &[usize]) -> Result<Arc<Self>, String> {
        if orders.len() != 120 {
            return Self::new(orders, &chart.full.events).map(Arc::new);
        }
        let mut cache = chart.factored.lock().unwrap();
        if let Some(branch) = &cache.all_orders { return Ok(Arc::clone(branch)); }
        let branch = Arc::new(Self::new(orders, &chart.full.events)?);
        cache.all_orders = Some(Arc::clone(&branch));
        Ok(branch)
    }

    fn new(orders: &[usize], events: &[(usize, i32)]) -> Result<Self, String> {
        if let Some(&(event, _)) = events.iter().find(|&&(event, _)| event >= 5) {
            return Err(format!("ordinary skill event {event} out of range"));
        }
        let mut chronological: Vec<_> = (0..events.len()).collect();
        chronological.sort_by_key(|&index| (events[index].1, index));
        Ok(Self::partition(orders, events, &chronological))
    }

    fn partition(orders: &[usize], events: &[(usize, i32)], chronological: &[usize]) -> Self {
        let first = orders[0];
        let representative = &live::orders()[first];
        for (index, &target) in chronological.iter().enumerate() {
            let event = events[target].0;
            if orders.iter().all(|&order| live::orders()[order][event] == representative[event]) {
                continue;
            }
            let mut groups: [Vec<usize>; 5] = std::array::from_fn(|_| Vec::new());
            for &order in orders {
                groups[live::orders()[order][event]].push(order);
            }
            let children = groups.iter().filter(|group| !group.is_empty())
                .map(|group| Self::partition(group, events, &chronological[index + 1..]))
                .collect();
            return Self { representative: first, target: Some(target), children, orders: Vec::new() };
        }
        Self { representative: first, target: None, children: Vec::new(), orders: orders.to_vec() }
    }
}

struct ScoreTree<'a, 'o> {
    chart: &'a Chart,
    trace: &'o ScoreTape,
    generation: u64,
    ordinary: Option<&'o mut OrdinaryOrders>,
    reuse_ordinary: bool,
    observe: Option<&'o dyn Fn(usize, f64) -> bool>,
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
        state.last[1] = match state.score.settled_score(time) {
            Some(score) => score,
            None => {
                let gekiso = state.gekiso_combo.as_ref().map(|combo| combo as &dyn crate::full::score::GekisoCombo);
                state.score.calculate(time, &core::Combo::default(), gekiso)?
            }
        };
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
                    state.score.calculator.combo_input += 1;
                    if self.reuse_ordinary {
                        at += self.chart.play.frames[state.frame].judged.len();
                    } else {
                        for raw in &self.chart.play.frames[state.frame].judged {
                            let judgement = i32::from(self.trace.bytes[at]);
                            at += 1;
                            state.judged.push(crate::full::play::JudgedNote {
                                note_id: raw.note_id,
                                time_ms: raw.time_ms,
                                judgement,
                            });
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
        branch: &OrderBranch,
        from: usize,
        values: &mut [ReplayValue],
    ) -> Result<bool, String> {
        if self.generation != crate::cancellation_generation() {
            return Err("cancelled".into());
        }
        let representative = &live::orders()[branch.representative];
        if let Some(program) = &mut state.program {
            let events: Vec<_> = self.chart.full.events.iter()
                .map(|&(event, time)| (representative[event], time)).collect();
            program.set_event_members(&events);
        }
        if self.reuse_ordinary {
            self.select_ordinary_tape(&mut state, branch.representative);
        }
        let until = branch.target.map_or(self.trace.bytes.len(), |at| self.trace.offsets[at]);
        self.advance(&mut state, from, until)?;
        if until == self.trace.bytes.len() {
            let tape = (!self.reuse_ordinary && self.ordinary.is_some())
                .then(|| Arc::new(std::mem::take(&mut state.ordinary_record)));
            return Ok(self.settle_branch(branch, (state.score.score as f64, state.ranges), tape.as_ref(), values));
        }
        let (last, children) = branch.children.split_last().unwrap();
        for child in children {
            if !self.visit(state.clone(), child, until, values)? { return Ok(false); }
        }
        self.visit(state, last, until, values)
    }

    fn settle_branch(&mut self, branch: &OrderBranch, score: ReplayValue, tape: Option<&OrdinaryTape>, values: &mut [ReplayValue]) -> bool {
        if branch.children.is_empty() {
            for &order in &branch.orders {
                values[order] = score;
                if self.observe.is_some_and(|observe| !observe(order, score.0)) { return false; }
                if let Some(tape) = tape { self.ordinary.as_mut().unwrap()[order] = Some(Arc::clone(tape)); }
            }
            return true;
        }
        branch.children.iter().all(|child| self.settle_branch(child, score, tape, values))
    }
}
fn score_orders<'a, const SETTLE: bool>(
    master: &'a Master,
    chart: &'a Chart,
    deck: &[Performer],
    draw: &ScoreTape,
    power: i32,
    (orders, branch): (&[usize], &OrderBranch),
    (ordinary, ordinary_orders, note_memo): (&'a Master, Option<&mut OrdinaryOrders>, &mut Option<std::rc::Rc<crate::full::score::NoteMemo>>),
    (prefix_end, prefix): (usize, &mut Option<ScoreState<'a>>),
    observe: Option<&dyn Fn(usize, f64) -> bool>,
) -> Result<Option<Vec<ReplayValue>>, String> {
    let reuse_ordinary = ordinary_orders.as_ref().is_some_and(|rows| orders.iter().all(|&order| rows[order].is_some()));
    let mut tree = ScoreTree {
        chart,
        trace: draw,
        generation: crate::cancellation_generation(),
        ordinary: ordinary_orders,
        reuse_ordinary,
        observe,
    };
    let from = if reuse_ordinary { prefix_end } else { 0 };
    let state = if let Some(prior) = prefix.as_ref().filter(|_| from > 0) {
        prior.clone()
    } else {
        let params = chart.params();
        let program = if reuse_ordinary {
            None
        } else {
            let order = &live::orders()[orders[0]];
            let events: Vec<_> = chart.full.events.iter().map(|&(event, time)| {
                order.get(event).copied().map(|actor| (actor, time))
                    .ok_or_else(|| format!("ordinary skill event {event} out of range"))
            }).collect::<Result<_, _>>()?;
            let mut capacity = [0; 5];
            for &(member, _) in &events {
                capacity[member] += 1;
            }
            let capacity = [capacity.into_iter().max().unwrap_or(0); 5];
            let mut program = Model::with_prepared_notes(
                ordinary, deck, chart.prepared_notes(), &events, params.clone(), None,
                (0, false, Some(&capacity)),
            )?;
            program.score.score.replay = Some(Vec::new());
            program.score.score.record_only = true;
            Some(program)
        };
        let sparse = program.as_ref().is_none_or(Model::sparse_score_frames);
        let mut calculator = Calculator::new(
            &master.live, power, params.level, params.converted, params.assist,
        );
        calculator.note_combos = Some(Arc::clone(&draw.combos));
        calculator.share_note_memo(note_memo, chart.full.notes.len());
        let mut score = Incremental::new(calculator, params.music_length);
        score.record_only = !SETTLE;
        let mut initial = ScoreState {
            program,
            ordinary_record: Vec::new(),
            ordinary_replay: None,
            ordinary_at: 0,
            score,
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
        if from > 0 {
            tree.select_ordinary_tape(&mut initial, orders[0]);
            tree.advance(&mut initial, 0, from)?;
            *prefix = Some(initial.clone());
        }
        initial
    };
    let mut values = vec![(0.0, [0.0; 3]); 120];
    Ok(tree.visit(state, branch, from, &mut values)?.then_some(values))
}
pub(super) fn scores(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
    power: i32,
) -> Result<Option<ReplayedScores>, String> {
    scores_observed(master, chart, deck, seeds, power, None)
}

pub(super) fn scores_observed(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
    power: i32,
    observe: Option<&dyn Fn(usize, f64) -> bool>,
) -> Result<Option<ReplayedScores>, String> {
    let Some(ordinary) = scoring_profiles(master, chart, deck, seeds)? else { return Ok(None); };
    let classes = control_classes(master, chart, deck, &ordinary);
    let (control, ordinary_master) = views(master, chart);
    settle_profiles(master, chart, deck, seeds, power, observe, None, ordinary, classes, control, ordinary_master)
}

pub(super) fn score_distribution(
    master: &Master, chart: &Chart, deck: &[Performer], seeds: &[i32], power: i32, keep_ranges: bool,
) -> Result<Option<std::collections::BTreeMap<[i32; 4], usize>>, String> {
    let Some(ordinary) = scoring_profiles(master, chart, deck, seeds)? else { return Ok(None); };
    let classes = control_classes(master, chart, deck, &ordinary);
    let (control, ordinary_master) = views(master, chart);
    let mut counts = std::collections::BTreeMap::new();
    Ok(settle_profiles(master, chart, deck, seeds, power, None, Some((&mut counts, keep_ranges)),
        ordinary, classes, control, ordinary_master)?.map(|_| counts))
}

fn scoring_profiles(master: &Master, chart: &Chart, deck: &[Performer], seeds: &[i32]) -> Result<Option<Vec<(bool, bool)>>, String> {
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
    Ok(Some(ordinary))
}

fn settle_profiles(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
    power: i32,
    observe: Option<&dyn Fn(usize, f64) -> bool>,
    mut score_distribution: Option<(&mut std::collections::BTreeMap<[i32; 4], usize>, bool)>,
    ordinary: Vec<(bool, bool)>,
    classes: Vec<usize>,
    control: Arc<Master>,
    ordinary_master: Arc<Master>,
) -> Result<Option<ReplayedScores>, String> {
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
    // Amortize the control pass over the orders in each small group. A
    // sampled COMBO chart can need a different input tape for every draw.
    let max_combo_groups = if chart.luck { 1 } else { 5 };
    if chart.setup.missions.contains(&1) && uses_full_replay(master, chart, seeds)
        && groups.len() > max_combo_groups
    {
        return Ok(None);
    }
    let generation = crate::cancellation_generation();
    let mut values = vec![0.0; 120];
    let mut range_values = [[0.0; 3]; 120];
    let mut note_memo = None;
    let mut sample_cache = SampleCache::default();
    for orders in groups {
        let branch = OrderBranch::for_chart(chart, &orders)?;
        let ordered = (orders.as_slice(), branch.as_ref());
        if let Some(observe) = observe.filter(|_| chart.luck) {
            let mut scored = HashMap::<*const ScoreTape, Vec<ReplayValue>>::default();
            let mut ordinary_orders: OrdinaryOrders = std::array::from_fn(|_| None);
            let mut prefix = None;
            let mut first: Option<Arc<ScoreTape>> = None;
            let mut stream = |draw: &Arc<ScoreTape>, shared_inputs: bool| {
                if generation != crate::cancellation_generation() { return Err("cancelled".into()); }
                let (result, reused) = match scored.entry(Arc::as_ptr(draw)) {
                    std::collections::hash_map::Entry::Occupied(entry) => (entry.into_mut(), true),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let prefix_end = first.as_ref().map_or(0, |first| {
                            let end = first.ordinary_start();
                            if draw.shares_prefix(first, end) { end } else { 0 }
                        });
                        if first.is_none() && shared_inputs {
                            ordinary_orders = take_measured_ordinary(chart, deck, &orders, draw);
                            first = Some(Arc::clone(draw));
                        }
                        let Some(result) = score_orders::<true>(master, chart, deck, draw, power, ordered,
                            (&ordinary_master, shared_inputs.then_some(&mut ordinary_orders), &mut note_memo),
                            (prefix_end, &mut prefix), Some(observe))? else { return Ok(false); };
                        (entry.insert(result), false)
                    }
                };
                for &order in &orders {
                    if reused && !observe(order, result[order].0) { return Ok(false); }
                    values[order] += result[order].0;
                    for (sum, value) in range_values[order].iter_mut().zip(result[order].1) { *sum += value; }
                }
                Ok(true)
            };
            let Some(scene) = context(master, &control, chart, deck, seeds,
                (&ordinary, &live::orders()[orders[0]]), &mut sample_cache, Some(&mut stream))?
            else { return Ok(None); };
            for order in orders {
                values[order] /= scene.draws.len() as f64;
                for value in &mut range_values[order] { *value /= scene.draws.len() as f64; }
            }
            continue;
        }
        let context = context(
            master,
            &control,
            chart,
            deck,
            seeds,
            (&ordinary, &live::orders()[orders[0]]),
            &mut sample_cache,
            None,
        )?.unwrap();
        let draws = &context.draws;
        let mut scored = HashMap::<*const ScoreTape, Vec<ReplayValue>>::default();
        let reuse_ordinary = context.shared_inputs && draws.iter().skip(1)
            .any(|draw| !Arc::ptr_eq(draw, &draws[0]));
        let mut ordinary_orders = if reuse_ordinary {
            take_measured_ordinary(chart, deck, &orders, &draws[0])
        } else {
            std::array::from_fn(|_| None)
        };
        let mut sample_prefix = None;
        let prefix_end = if reuse_ordinary {
            // Stop before the first ordinary activation. A different control
            // stream or combo table keeps its own scoring prefix.
            let first = &draws[0];
            let end = first.ordinary_start();
            if draws.iter().skip(1).all(|draw| draw.shares_prefix(first, end)) { end } else { 0 }
        } else { 0 };
        let classes = if observe.is_none() && reuse_ordinary && draws.len() >= 8 {
            let Some(first) = score_orders::<true>(master, chart, deck, &draws[0], power, ordered,
                (&ordinary_master, Some(&mut ordinary_orders), &mut note_memo), (0, &mut sample_prefix), None)?
            else { return Ok(None); };
            scored.insert(Arc::as_ptr(&draws[0]), first);
            OrdinaryClasses::new(chart, draws, &orders, &ordinary_orders)?
        } else { None };
        let tail_layout = classes.as_ref().map_or(ordered, OrdinaryClasses::layout);
        #[cfg(not(target_arch = "wasm32"))]
        if observe.is_none() && reuse_ordinary && draws.len() >= 16 && crate::parallel::workers(draws.len()) > 1 {
            let mut seen = HashSet::new();
            let unique: Vec<_> = draws.iter().filter(|draw| seen.insert(Arc::as_ptr(draw))).collect();
            if unique.len() >= 8 {
                let tail = &unique[1..];
                let workers = crate::parallel::workers(tail.len());
                let jobs: Vec<_> = tail.chunks(tail.len().div_ceil(workers)).collect();
                let results: Vec<_> = crate::parallel::map(&jobs, |batch| {
                    let mut ordinary = ordinary_orders.clone();
                    let mut memo = None;
                    let mut prefix = None;
                    batch.iter().map(|draw| {
                        score_orders::<true>(master, chart, deck, draw, power, tail_layout,
                            (&ordinary_master, Some(&mut ordinary), &mut memo), (prefix_end, &mut prefix), None)
                    }).collect::<Result<Vec<_>, String>>()
                })?.into_iter().flatten().collect();
                for (draw, result) in tail.iter().zip(results) {
                    let Some(mut result) = result else { return Ok(None); };
                    if let Some(classes) = &classes { classes.expand(&orders, &mut result); }
                    scored.insert(Arc::as_ptr(draw), result);
                }
            }
        }
        for draw in draws {
            if generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
            let (result, reused) = match scored.entry(Arc::as_ptr(draw)) {
                std::collections::hash_map::Entry::Occupied(entry) => (entry.into_mut(), true),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let Some(mut result) = score_orders::<true>(master, chart, deck, draw, power, tail_layout,
                        (&ordinary_master, reuse_ordinary.then_some(&mut ordinary_orders), &mut note_memo),
                        (prefix_end, &mut sample_prefix), observe)? else {
                        return Ok(None);
                    };
                    if let Some(classes) = &classes { classes.expand(&orders, &mut result); }
                    (entry.insert(result), false)
                }
            };
            for &order in &orders {
                if let Some((counts, keep_ranges)) = &mut score_distribution {
                    let (total, stages) = result[order];
                    let [a, b, c] = if *keep_ranges { stages.map(|score| score as i32) } else { [0; 3] };
                    *counts.entry([total as i32, a, b, c]).or_default() += 1;
                }
                if reused && observe.is_some_and(|observe| !observe(order, result[order].0)) {
                    return Ok(None);
                }
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
