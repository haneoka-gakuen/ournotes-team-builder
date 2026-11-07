//! Native input feasibility, frame candidates and cached flick-reservation components.
mod frame;
mod optimize;
mod search;
pub(crate) use optimize::FrameObjective;
use crate::{chart::Chart, live, master::Master, num::f};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Enter,
    Hold,
    Exit,
}

#[derive(Clone)]
struct Note {
    time: i32,
    op: i32,
    grade: usize,
    left: f64,
    right: f64,
    body_left: f64,
    body_right: f64,
    before: [i32; 6],
    after: [i32; 6],
    input_first: i64,
    first: i32,
    last: i32,
    last_phase: i32,
    auto_frame: i32,
    forced_frame: i32,
}
impl Note {
    #[inline]
    fn trace(&self) -> bool {
        matches!(self.op, 21 | 60..=63 | 104 | 105 | 120)
    }
    #[inline]
    fn tail(&self) -> bool {
        self.op == 22
    }
    #[inline]
    fn flick(&self) -> bool {
        matches!(self.op, 40..=42 | 102)
    }
    #[inline]
    fn asynchronous(&self) -> bool {
        self.trace() || self.tail()
    }
    #[inline]
    fn body_distance(&self, point: f64) -> f64 {
        f(self.body_left - point)
            .max(f(point - self.body_right))
            .max(0.0)
    }
    #[inline]
    fn grade_at(&self, time: i32) -> usize {
        let diff = time - self.time;
        (1..=5)
            .rev()
            .find(|&grade| {
                diff >= -self.before[grade]
                    && diff <= self.after[grade]
                    && (self.before[grade] != 0 || self.after[grade] != 0 || diff == 0)
            })
            .unwrap_or(1)
    }
    fn hit(&self, frame: i32, time: i32, action: Action) -> Option<usize> {
        if frame < self.first || frame >= self.auto_frame || frame >= self.forced_frame {
            return None;
        }
        if self.trace() {
            if frame >= self.last_phase {
                return None;
            }
            if action == Action::Exit {
                return Some(self.grade_at(time));
            }
            return (frame >= self.time).then_some(5);
        }
        if self.tail() {
            if frame > self.last {
                return None;
            }
            return match action {
                Action::Exit => Some(self.grade_at(time)),
                Action::Hold if frame >= self.time => Some(5),
                _ => None,
            };
        }
        if self.flick() {
            return (action == Action::Exit || action == Action::Hold && frame >= self.time)
                .then(|| self.grade_at(time));
        }
        (action == Action::Enter).then(|| self.grade_at(time))
    }
    fn actions(&self) -> &'static [Action] {
        if self.trace() || self.tail() || self.flick() {
            &[Action::Hold, Action::Exit]
        } else {
            &[Action::Enter]
        }
    }
}

#[derive(Clone)]
pub struct InputTiming {
    pub frames: Vec<Vec<i32>>,
    clock: live::FrameClock,
    notes: Vec<Note>,
    checked: Arc<Mutex<InputChecks>>,
    local_checked: Arc<Mutex<LocalChecks>>,
    atoms: Arc<Mutex<search::AtomCache>>,
    witnesses: Arc<Mutex<Vec<Vec<i32>>>>,
    orderings: Vec<(usize, usize)>,
    contexts: Arc<HashMap<i32, Arc<Vec<usize>>>>,
    components: Arc<Vec<FlickComponent>>,
    component_checked: Arc<Mutex<ComponentChecks>>,
}
struct FlickComponent {
    start: i32,
    end: i32,
    notes: Vec<usize>,
}
#[derive(Default)]
struct ComponentChecks {
    values: HashMap<(usize, Vec<i32>), Option<Conflict>>,
    bytes: usize,
}
type InputChecks = HashMap<Vec<i32>, Result<Option<Conflict>, String>>;
type LocalChecks = HashMap<(i32, i32, Vec<i32>), bool>;

#[derive(Clone)]
pub struct Conflict {
    pub notes: Vec<usize>,
    pub interval: Option<(i32, i32)>,
}

fn input_times(clock: live::FrameClock, frame: i32) -> std::ops::RangeInclusive<i32> {
    let mut first = (frame - clock.advance_ms()).max(0);
    while clock.frame_time(first) < frame {
        first += 1;
    }
    first..=frame
}

impl InputTiming {
    pub fn new(master: &Master, chart: &Chart, grades: Option<&[usize]>, clock: live::FrameClock) -> Result<Self, String> {
        let mut notes = Vec::with_capacity(chart.times.len());
        let mut frames: Vec<Vec<i32>> = Vec::with_capacity(chart.times.len());
        let mut line_ends = HashMap::new();
        for source in &chart.notes {
            if !matches!(source.operate_type, 22 | 42 | 62 | 82 | 103 | 105) {
                continue;
            }
            if let Some(input) = &source.input {
                for &line in &input.lines {
                    line_ends.insert(line, source.time_ms);
                }
            }
        }
        for (i, &source_index) in chart.judged_indices.iter().enumerate() {
            let source = &chart.notes[source_index];
            let geometry = source
                .input
                .as_ref()
                .ok_or("missing native input geometry")?;
            let kind = source.judgement_type;
            let mut before = [-1; 6];
            let mut after = [-1; 6];
            for grade in 1..=5 {
                if let (Some(&b), Some(&a)) = (
                    master.live.judgement_before_ms.get(&(kind, grade as i32)),
                    master.live.judgement_after_ms.get(&(kind, grade as i32)),
                ) {
                    before[grade] = b;
                    after[grade] = a;
                }
            }
            let max_before = before.iter().copied().max().unwrap_or(0).max(0);
            let max_after = after.iter().copied().max().unwrap_or(0).max(0);
            let offset = |kind| {
                master
                    .live
                    .judgement_area_offsets
                    .get(&kind)
                    .copied()
                    .ok_or("missing native judgement area offsets")
            };
            let extra = if geometry.judgement_area_offset_type == 1 {
                let setting = |key| {
                    master
                        .live
                        .settings
                        .get(key)
                        .and_then(|value| value.parse::<f64>().ok())
                        .ok_or("missing native slide area settings")
                };
                let minimum = setting("slide_offset_min_note_lane_width")?;
                let maximum = setting("slide_offset_max_note_lane_width")?;
                let factor = if minimum == maximum {
                    0.0
                } else {
                    f(f(geometry.width - minimum) / f(maximum - minimum)).clamp(0.0, 1.0)
                };
                let start = offset(6)?;
                f(start + f(f(offset(7)? - start) * factor))
            } else {
                offset(geometry.judgement_area_offset_type)?
            };
            let end = f(f(geometry.lane + geometry.width) - 1.0);
            let last_phase =
                clock.frame_time(source.time_ms.saturating_add(max_after).saturating_add(1));
            let auto_frame = clock.frame_time(last_phase.saturating_add(1));
            let mut forced_frame = i32::MAX;
            if matches!(source.operate_type, 20 | 41 | 61 | 101 | 102 | 104) {
                for line in &geometry.lines {
                    if let Some(&end) = line_ends.get(line) {
                        forced_frame = forced_frame.min(clock.frame_time(end.saturating_add(1)));
                    }
                }
            }
            let note = Note {
                time: source.time_ms,
                op: source.operate_type,
                grade: grades.map_or(5, |g| g[i]),
                left: f(f(geometry.lane - extra) - 0.5),
                right: f(f(end + extra) + 0.5),
                body_left: f(geometry.lane - 0.5),
                body_right: f(end + 0.5),
                before,
                after,
                input_first: i64::from(source.time_ms) - i64::from(max_before),
                first: clock.frame_time(source.time_ms.saturating_sub(max_before)),
                last: source.time_ms.saturating_add(max_after),
                last_phase,
                auto_frame,
                forced_frame,
            };
            let mut allowed = BTreeSet::new();
            for time in source.time_ms.saturating_sub(max_before).max(0)..=note.last {
                let frame = clock.frame_time(time);
                if note
                    .actions()
                    .iter()
                    .any(|&action| note.hit(frame, time, action) == Some(note.grade))
                {
                    allowed.insert(frame);
                }
            }
            if (note.trace() || note.tail()) && note.grade == 5 {
                for time in source.time_ms.max(0)..=note.last {
                    let frame = clock.frame_time(time);
                    if note.hit(frame, time, Action::Hold) == Some(5) {
                        allowed.insert(frame);
                    }
                }
            }
            if note.grade == 1 {
                allowed.insert(note.auto_frame.min(note.forced_frame));
            }
            if allowed.is_empty() {
                return Err(format!("unreachable native judgement at note {i}"));
            }
            frames.push(allowed.into_iter().collect());
            notes.push(note);
        }
        for (i, note) in notes.iter().enumerate().filter(|(_, note)| note.flick()) {
            let mut allowed: BTreeSet<_> = frames[i].iter().copied().collect();
            for &at in &frames[i] {
                let next = clock.frame_time(at.saturating_add(1));
                if next >= note.auto_frame || next > note.forced_frame || allowed.contains(&next) {
                    continue;
                }
                let reserved = input_times(clock, at).any(|time| {
                    if note.hit(at, time, Action::Exit) != Some(note.grade) {
                        return false;
                    }
                    if at < note.time && next >= note.time {
                        return true;
                    }
                    notes.iter().enumerate().any(|(j, other)| {
                        j != i
                            && other.flick()
                            && note.left <= other.right
                            && other.left <= note.right
                            && (i64::from(at) - i64::from(other.time)).abs()
                                <= (i64::from(at) - i64::from(note.time)).abs()
                            && other.hit(at, time, Action::Exit) == Some(other.grade)
                    })
                });
                if reserved {
                    allowed.insert(next);
                }
            }
            frames[i] = allowed.into_iter().collect();
        }
        let mut traces = HashMap::<(u64, u64, [i32; 6], [i32; 6]), Vec<usize>>::new();
        for (i, note) in notes.iter().enumerate().filter(|(_, note)| note.trace() && note.grade == 5) {
            traces.entry((note.left.to_bits(), note.right.to_bits(), note.before, note.after)).or_default().push(i);
        }
        let mut orderings = Vec::new();
        for group in traces.values_mut() {
            group.sort_by_key(|&i| notes[i].time);
            for pair in group.windows(2) {
                orderings.push((pair[0], pair[1]));
                if notes[pair[0]].time == notes[pair[1]].time { orderings.push((pair[1], pair[0])); }
            }
        }
        let mut contexts = HashMap::<i32, Vec<usize>>::new();
        for (i, note) in notes.iter().enumerate() {
            let last = *frames[i].last().unwrap();
            let mut at = note.first;
            while at <= last {
                contexts.entry(at).or_default().push(i);
                if at == last { break; }
                at = clock.frame_time(at.saturating_add(1));
            }
        }
        // Only flick reservations cross frames. Disjoint potential intervals
        // can be certified independently; other notes only share one update.
        let mut flicks: Vec<_> = notes.iter().enumerate().filter(|(_, note)| note.flick()).map(|(i, _)| i).collect();
        flicks.sort_by_key(|&i| notes[i].first);
        let mut components = Vec::<FlickComponent>::new();
        for i in flicks {
            let start = notes[i].first;
            let end = *frames[i].last().unwrap();
            if let Some(previous) = components.last_mut().filter(|previous| previous.end >= start) { previous.end = previous.end.max(end); }
            else { components.push(FlickComponent { start, end, notes: Vec::new() }); }
        }
        for component in &mut components {
            component.notes = notes.iter().enumerate().filter(|(i, note)| note.first <= component.end && *frames[*i].last().unwrap() >= component.start).map(|(i, _)| i).collect();
        }
        Ok(Self {
            frames,
            clock,
            notes,
            checked: Arc::new(Mutex::new(HashMap::new())),
            local_checked: Arc::new(Mutex::new(HashMap::new())),
            atoms: Arc::new(Mutex::new(search::AtomCache::default())),
            witnesses: Arc::new(Mutex::new(Vec::new())),
            orderings,
            contexts: Arc::new(contexts.into_iter().map(|(at, indices)| (at, Arc::new(indices))).collect()),
            components: Arc::new(components),
            component_checked: Arc::new(Mutex::new(ComponentChecks::default())),
        })
    }

    pub fn conflict(&self, selected: &[i32]) -> Result<Option<Conflict>, String> {
        let cached = self.checked.lock().unwrap().get(selected).cloned();
        let was_cached = cached.is_some();
        let result = cached.unwrap_or_else(|| self.check_inputs(selected));
        if matches!(&result, Ok(None)) {
            self.remember_verified(selected)?;
            return result;
        }
        if was_cached { return result; }
        result.as_ref()?;
        let mut checked = self.checked.lock().unwrap();
        let capacity = (1_048_576 / selected.len().max(1).saturating_mul(16)).clamp(1, 256);
        if checked.len() >= capacity {
            checked.clear();
        }
        checked.insert(selected.to_vec(), result.clone());
        result
    }

    fn remember_verified(&self, selected: &[i32]) -> Result<(), String> {
        let mut checked = self.checked.lock().unwrap();
        if matches!(checked.get(selected), Some(Ok(Some(_)))) { return Err("native input proof contradiction".into()); }
        if !checked.contains_key(selected) {
            let capacity = (1_048_576 / selected.len().max(1).saturating_mul(16)).clamp(1, 256);
            if checked.len() >= capacity { checked.clear(); }
            checked.insert(selected.to_vec(), Ok(None));
        }
        drop(checked);
        let mut witnesses = self.witnesses.lock().unwrap();
        if !witnesses.iter().any(|known| known == selected) {
            if witnesses.len() == 8 { witnesses.remove(0); }
            witnesses.push(selected.to_vec());
        }
        Ok(())
    }

    pub fn witnesses(&self) -> Vec<Vec<i32>> {
        self.witnesses.lock().unwrap().clone()
    }

    pub(crate) fn ordering_pairs(&self) -> &[(usize, usize)] { &self.orderings }

    fn check_inputs(&self, selected: &[i32]) -> Result<Option<Conflict>, String> {
        for &(first, second) in &self.orderings {
            if selected[first] > selected[second] {
                return Ok(Some(Conflict { notes: vec![first, second], interval: None }));
            }
        }
        for (i, &at) in selected.iter().enumerate() {
            if !self.frames[i].contains(&at) {
                return Ok(Some(Conflict {
                    notes: vec![i],
                    interval: None,
                }));
            }
        }
        for i in 0..self.components.len() {
            if let Some(conflict) = self.component_conflict(i, selected)? { return Ok(Some(conflict)); }
        }
        let mut frames = BTreeSet::new();
        for (i, &at) in selected.iter().enumerate() {
            if self.notes[i].flick() { continue; }
            let component = self.components.partition_point(|component| component.end < at);
            if self.components.get(component).is_none_or(|component| component.start > at) { frames.insert(at); }
        }
        for at in frames {
            if let Some(conflict) = self.local_conflict(selected, at, at)? {
                return Ok(Some(conflict));
            }
        }
        Ok(None)
    }

    fn component_conflict(&self, index: usize, selected: &[i32]) -> Result<Option<Conflict>, String> {
        let component = &self.components[index];
        let key: Vec<_> = component.notes.iter().map(|&i| if selected[i] < component.start { -1 } else if selected[i] > component.end { -2 } else { selected[i] }).collect();
        if let Some(value) = self.component_checked.lock().unwrap().values.get(&(index, key.clone())).cloned() { return Ok(value); }
        // A late Perfect that needs a saved flick must also survive the
        // preceding update. Check that short necessary condition first.
        for &i in &component.notes {
            let note = &self.notes[i];
            let at = selected[i];
            if !note.flick() || note.grade != 5 || at < component.start || at > component.end
                || *input_times(self.clock,at).start() <= note.time.saturating_add(note.after[5]) { continue; }
            let start = self.clock.frame_time(at.saturating_sub(self.clock.advance_ms()));
            match self.local_conflict(selected,start,at) {
                Ok(Some(conflict)) => return Ok(Some(conflict)),
                Err(error) if error == "cancelled" => return Err(error),
                _ => {}
            }
        }
        let notes: Vec<_> = component.notes.iter().map(|&i| self.notes[i].clone()).collect();
        let targets: Vec<_> = component.notes.iter().map(|&i| selected[i]).collect();
        let initial = frame::State { done: targets.iter().map(|&at| at < component.start).collect(), flicks: vec![None; notes.len()] };
        let mut frames: BTreeSet<_> = targets.iter().copied().filter(|&at| component.start <= at && at <= component.end).collect();
        for (i, note) in notes.iter().enumerate() {
            for at in [note.auto_frame, note.forced_frame] {
                if component.start <= at && at <= component.end && at <= targets[i] { frames.insert(at); }
            }
            if note.flick() {
                let mut at = note.first;
                while at <= targets[i] {
                    frames.insert(at);
                    if at == targets[i] { break; }
                    at = self.clock.frame_time(at.saturating_add(1));
                }
            }
        }
        let checked = replay(self.clock, &notes, &frames.into_iter().collect::<Vec<_>>(), &targets, &initial, &self.atoms, None);
        let mut tightened = None;
        if !matches!(checked, Ok(None)) {
            if matches!(&checked, Err(error) if error == "cancelled") { return Err("cancelled".into()); }
            let mut ends: Vec<_> = targets.iter().copied().filter(|&at| component.start <= at && at <= component.end).collect();
            ends.sort_unstable(); ends.dedup();
            for at in ends.into_iter().rev() {
                let start = self.clock.frame_time(at.saturating_sub(self.clock.advance_ms()));
                match self.local_conflict(selected,start,at) {
                    Ok(Some(conflict)) => { tightened = Some(conflict); break; },
                    Err(error) if error == "cancelled" => return Err(error),
                    _ => {}
                }
            }
        }
        let value = if tightened.is_some() { tightened } else { checked?.map(|(start, end)| Conflict {
            notes: component.notes.iter().copied().filter(|&i| self.notes[i].first <= end && *self.frames[i].last().unwrap() >= start).collect(),
            interval: Some((start, end)),
        }) };
        let bytes = key.len() * 4 + value.as_ref().map_or(0, |conflict| conflict.notes.len() * std::mem::size_of::<usize>()) + 64;
        let mut checked = self.component_checked.lock().unwrap();
        if checked.bytes + bytes > 4 * 1_048_576 { checked.values.clear(); checked.bytes = 0; }
        checked.values.insert((index, key), value.clone());
        checked.bytes += bytes;
        Ok(value)
    }

    fn replay_frames(&self, selected: &[i32]) -> BTreeSet<i32> {
        let mut frames: BTreeSet<_> = selected.iter().copied().collect();
        for (i, note) in self.notes.iter().enumerate() {
            for at in [note.auto_frame, note.forced_frame] {
                if at <= selected[i] {
                    frames.insert(at);
                }
            }
            if note.flick() {
                let mut at = note.first;
                while at <= selected[i] {
                    frames.insert(at);
                    at = self.clock.frame_time(at.saturating_add(1));
                }
            }
        }
        frames
    }

    fn verify_program(&self, selected: &[i32], inputs: &BTreeMap<i32, Vec<frame::Touch>>) -> Result<bool, String> {
        if selected.iter().enumerate().any(|(i, at)| !self.frames[i].contains(at)) { return Ok(false); }
        let mut frames = self.replay_frames(selected);
        frames.extend(inputs.keys().copied());
        let initial = frame::State { done: vec![false; self.notes.len()], flicks: vec![None; self.notes.len()] };
        if replay(self.clock, &self.notes, &frames.into_iter().collect::<Vec<_>>(), selected, &initial, &self.atoms, Some(inputs))?.is_none() {
            self.remember_verified(selected)?;
            Ok(true)
        } else { Ok(false) }
    }

    fn local_conflict(
        &self,
        selected: &[i32],
        start: i32,
        end: i32,
    ) -> Result<Option<Conflict>, String> {
        use frame::{ReservedFlick, State};
        let context = if start == end { self.contexts.get(&start).cloned().unwrap_or_else(|| Arc::new(Vec::new())) }
            else { Arc::new(self.notes.iter().enumerate().filter(|(i, note)| note.first <= end && self.frames[*i].last().is_some_and(|&last| start <= last)).map(|(i, _)| i).collect()) };
        let key: Vec<_> = context
            .iter()
            .map(|&i| {
                if selected[i] < start {
                    -1
                } else if selected[i] > end {
                    -2
                } else {
                    selected[i]
                }
            })
            .collect();
        let conflict = || Conflict {
            notes: context.as_ref().clone(),
            interval: Some((start, end)),
        };
        if let Some(&possible) = self
            .local_checked
            .lock()
            .unwrap()
            .get(&(start, end, key.clone()))
        {
            return Ok((!possible).then(conflict));
        }
        let indices: Vec<_> = context
            .iter()
            .copied()
            .filter(|&i| selected[i] >= start)
            .collect();
        let notes: Vec<_> = indices.iter().map(|&i| self.notes[i].clone()).collect();
        let selected: Vec<_> = indices.iter().map(|&i| selected[i]).collect();
        let initial = State {
            done: vec![false; notes.len()],
            flicks: vec![None; notes.len()],
        };
        let mut choices = Vec::new();
        for (i, note) in notes.iter().enumerate() {
            if !note.flick() {
                continue;
            }
            let mut options = vec![
                None,
                Some(ReservedFlick {
                    time: note.time,
                    begin: note.left - 1.0,
                }),
            ];
            for grade in 1..=5 {
                if start >= note.time && (selected[i] != start || grade != note.grade) {
                    continue;
                }
                let time = (note
                    .time
                    .saturating_sub(note.before.iter().copied().max().unwrap_or(0).max(0))
                    ..start)
                    .find(|&time| {
                        frame::permitted_time(note, time) && note.grade_at(time) == grade
                    });
                if let Some(time) = time {
                    options.push(Some(ReservedFlick {
                        time,
                        begin: note.body_left.max(0.0),
                    }));
                }
            }
            options.sort_by_key(|saved| saved.map_or(3, |saved| {
                if !frame::in_area(note, saved.begin) { return 1; }
                if selected[i] == start && note.grade_at(saved.time) == note.grade { 0 } else { 2 }
            }));
            let mut classes = HashSet::new();
            options.retain(|&saved| classes.insert(search::reserved_class(note, saved)));
            choices.push((i, options));
        }
        fn possible(
            clock: live::FrameClock,
            notes: &[Note],
            frames: &[i32],
            selected: &[i32],
            choices: &[(usize, Vec<Option<ReservedFlick>>)],
            state: &mut State,
            atoms: &Arc<Mutex<search::AtomCache>>,
        ) -> Result<bool, String> {
            let Some(((index, options), rest)) = choices.split_first() else {
                return replay(clock, notes, frames, selected, state, atoms, None).map(|conflict| conflict.is_none());
            };
            let mut unresolved = None;
            for &option in options {
                state.flicks[*index] = option;
                match possible(clock, notes, frames, selected, rest, state, atoms) {
                    Ok(true) => return Ok(true),
                    Ok(false) => {}
                    Err(error) if error == "cancelled" => return Err(error),
                    Err(error) => unresolved = Some(error),
                }
            }
            if let Some(error) = unresolved {
                Err(error)
            } else {
                Ok(false)
            }
        }
        let mut frames = vec![start];
        while *frames.last().unwrap() < end {
            frames.push(self.clock.frame_time(frames.last().unwrap().saturating_add(1)));
        }
        let possible = possible(
            self.clock,
            &notes,
            &frames,
            &selected,
            &choices,
            &mut initial.clone(),
            &self.atoms,
        )?;
        let mut cache = self.local_checked.lock().unwrap();
        if cache.len() >= 8192 {
            cache.clear();
        }
        cache.insert((start, end, key), possible);
        Ok((!possible).then(conflict))
    }
}

fn replay(
    clock: live::FrameClock,
    notes: &[Note],
    frames: &[i32],
    selected: &[i32],
    initial: &frame::State,
    atoms: &Arc<Mutex<search::AtomCache>>,
    inputs: Option<&BTreeMap<i32, Vec<frame::Touch>>>,
) -> Result<Option<(i32, i32)>, String> {
    use frame::ReservedFlick;
    type Saved = Vec<(usize, ReservedFlick)>;
    struct Step {
        index: usize,
        notes: Vec<usize>,
        incoming: Saved,
        key: Vec<u8>,
        search: Choices,
        complete: bool,
    }
    struct Choices {
        search: Option<search::Search>,
        forced: Option<search::Outcome>,
    }
    impl Choices {
        fn next(&mut self) -> Result<Option<search::Outcome>, String> {
            if let Some(search) = &mut self.search { search.next() } else { Ok(self.forced.take()) }
        }
    }
    let mut saved: Saved = initial
        .flicks
        .iter()
        .enumerate()
        .filter_map(|(i, value)| value.map(|value| (i, value)))
        .collect();
    let mut stack = Vec::<Step>::new();
    let mut failed = HashSet::<(usize, Vec<u8>)>::new();
    let mut step = Some(0usize);
    let mut witness = Vec::<(i32, Vec<frame::Touch>)>::new();
    let generation = crate::cancellation_generation();
    let mut iterations = 0usize;
    let mut block_start = frames.first().copied().unwrap_or(0);
    let mut block_end = block_start;
    loop {
        iterations += 1;
        if iterations.is_multiple_of(128) {
            crate::clock::milliseconds();
            if generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
        }
        if let Some(index) = step.take() {
            if index == frames.len() {
                return Ok(None);
            }
            let at = frames[index];
            block_end = block_end.max(at);
            let indices: Vec<_> = notes
                .iter()
                .enumerate()
                .filter(|(i, note)| !initial.done[*i] && selected[*i] >= at && note.first <= at)
                .map(|(i, _)| i)
                .collect();
            if indices.iter().any(|&i| notes[i].flick() && selected[i] > at) {
                block_end = block_end.max(frames.get(index + 1).copied().unwrap_or(at));
            }
            let key: Vec<_> = indices
                .iter()
                .map(|&i| {
                    saved
                        .iter()
                        .find(|&&(j, _)| j == i)
                        .map_or(0, |&(_, value)| search::reserved_class(&notes[i], Some(value)))
                })
                .collect();
            if !failed.contains(&(index, key.clone())) {
                let local_notes: Vec<_> = indices.iter().map(|&i| notes[i].clone()).collect();
                let targets: Vec<_> = indices.iter().map(|&i| selected[i]).collect();
                let local = frame::State {
                    done: vec![false; indices.len()],
                    flicks: indices
                        .iter()
                        .map(|&i| {
                            saved
                                .iter()
                                .find(|&&(j, _)| j == i)
                                .map(|&(_, value)| value)
                        })
                        .collect(),
                };
                let complete = indices
                    .iter()
                    .all(|&i| !notes[i].flick() || selected[i] == at);
                let choices = if let Some(inputs) = inputs.and_then(|inputs| inputs.get(&at)) {
                    let mut state = local.clone();
                    let judged = frame::run(&local_notes, at, inputs, &mut state);
                    let valid = judged.len() == targets.iter().filter(|&&target| target == at).count()
                        && judged.iter().all(|j| targets[j.note] == at && j.grade == local_notes[j.note].grade);
                    Choices { search: None, forced: valid.then(|| search::Outcome { state, inputs: inputs.clone() }) }
                } else {
                    Choices { search: Some(search::Search::new(local_notes, at, targets, local, clock, Arc::clone(atoms), frames.get(index + 1).copied())), forced: None }
                };
                stack.push(Step {
                    index,
                    notes: indices,
                    incoming: saved.clone(),
                    key,
                    search: choices,
                    complete,
                });
            }
        }
        let Some(current) = stack.last_mut() else {
            return Ok(Some((block_start, block_end)));
        };
        if let Some(outcome) = current.search.next()? {
            let index = current.index;
            saved = current.incoming.clone();
            for (j, &i) in current.notes.iter().enumerate() {
                saved.retain(|&(old, _)| old != i);
                if !outcome.state.done[j] {
                    if let Some(value) = outcome.state.flicks[j] {
                        saved.push((i, value));
                    }
                }
            }
            witness.truncate(index);
            witness.push((frames[index], outcome.inputs));
            if current.complete {
                stack.clear();
                failed.clear();
                witness.clear();
                block_start = frames.get(index + 1).copied().unwrap_or(frames[index]);
                block_end = block_start;
            }
            step = Some(index + 1);
        } else {
            let previous = stack.pop().unwrap();
            failed.insert((previous.index, previous.key));
            saved = previous.incoming;
        }
    }
}
