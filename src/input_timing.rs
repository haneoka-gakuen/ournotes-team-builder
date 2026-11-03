//! Native input kinds and shared spatial candidates for cumulative delivery.
mod frame;
mod search;
use crate::{chart::Chart, live, master::Master, num::f};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
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
    first: i32,
    last: i32,
    last_phase: i32,
    auto_frame: i32,
    forced_frame: i32,
}
impl Note {
    fn trace(&self) -> bool {
        matches!(self.op, 21 | 60..=63 | 104 | 105 | 120)
    }
    fn tail(&self) -> bool {
        self.op == 22
    }
    fn flick(&self) -> bool {
        matches!(self.op, 40..=42 | 102)
    }
    fn asynchronous(&self) -> bool {
        self.trace() || self.tail()
    }
    fn body_distance(&self, point: f64) -> f64 {
        f(self.body_left - point)
            .max(f(point - self.body_right))
            .max(0.0)
    }
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
        Ok(Self {
            frames,
            clock,
            notes,
            checked: Arc::new(Mutex::new(HashMap::new())),
            local_checked: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn conflict(&self, selected: &[i32]) -> Result<Option<Conflict>, String> {
        if let Some(result) = self.checked.lock().unwrap().get(selected) {
            return result.clone();
        }
        let result = self.check_inputs(selected);
        let mut checked = self.checked.lock().unwrap();
        let capacity = (1_048_576 / selected.len().max(1).saturating_mul(16)).clamp(1, 256);
        if checked.len() >= capacity {
            checked.clear();
        }
        checked.insert(selected.to_vec(), result.clone());
        result
    }

    fn check_inputs(&self, selected: &[i32]) -> Result<Option<Conflict>, String> {
        use frame::State;
        let clock = self.clock;
        for (i, &at) in selected.iter().enumerate() {
            if !self.frames[i].contains(&at) {
                return Ok(Some(Conflict {
                    notes: vec![i],
                    interval: None,
                }));
            }
        }
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
                    at = clock.frame_time(at.saturating_add(1));
                }
            }
        }
        let frames: Vec<_> = frames.into_iter().collect();
        for &at in &frames {
            if let Some(conflict) = self.local_conflict(selected, at, at)? {
                return Ok(Some(conflict));
            }
        }
        let initial = State {
            done: vec![false; self.notes.len()],
            flicks: vec![None; self.notes.len()],
        };
        for (i, note) in self.notes.iter().enumerate() {
            if note.flick() && selected[i] > note.time {
                let at = selected[i];
                let before = *input_times(clock, at).start() - 1;
                if let Some(conflict) = self.local_conflict(selected, before, at)? {
                    return Ok(Some(conflict));
                }
            }
        }
        if replay(clock, &self.notes, &frames, selected, &initial)? {
            Ok(None)
        } else {
            Ok(Some(Conflict {
                notes: (0..selected.len()).collect(),
                interval: None,
            }))
        }
    }

    fn local_conflict(
        &self,
        selected: &[i32],
        start: i32,
        end: i32,
    ) -> Result<Option<Conflict>, String> {
        use frame::{ReservedFlick, State};
        let context: Vec<_> = self
            .notes
            .iter()
            .enumerate()
            .filter(|(i, note)| {
                note.first <= end && self.frames[*i].last().is_some_and(|&last| start <= last)
            })
            .map(|(i, _)| i)
            .collect();
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
            notes: context.clone(),
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
            choices.push((i, options));
        }
        fn possible(
            clock: live::FrameClock,
            notes: &[Note],
            frames: &[i32],
            selected: &[i32],
            choices: &[(usize, Vec<Option<ReservedFlick>>)],
            step: usize,
            state: &mut State,
        ) -> Result<bool, String> {
            let Some((index, options)) = choices.get(step) else {
                return replay(clock, notes, frames, selected, state);
            };
            let mut unresolved = None;
            for &option in options {
                state.flicks[*index] = option;
                match possible(clock, notes, frames, selected, choices, step + 1, state) {
                    Ok(true) => return Ok(true),
                    Ok(false) => {}
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
            0,
            &mut initial.clone(),
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
) -> Result<bool, String> {
    use frame::ReservedFlick;
    type Saved = Vec<(usize, ReservedFlick)>;
    struct Step {
        index: usize,
        notes: Vec<usize>,
        incoming: Saved,
        key: Vec<u8>,
        search: search::Search,
        complete: bool,
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
                return Ok(true);
            }
            let at = frames[index];
            let indices: Vec<_> = notes
                .iter()
                .enumerate()
                .filter(|(i, note)| !initial.done[*i] && selected[*i] >= at && note.first <= at)
                .map(|(i, _)| i)
                .collect();
            let key: Vec<_> = indices
                .iter()
                .map(|&i| {
                    saved
                        .iter()
                        .find(|&&(j, _)| j == i)
                        .map_or(0, |&(_, value)| {
                            if frame::in_area(&notes[i], value.begin) {
                                2 + notes[i].grade_at(value.time) as u8
                            } else {
                                1
                            }
                        })
                })
                .collect();
            if !failed.contains(&(index, key.clone())) {
                let local_notes: Vec<_> = indices.iter().map(|&i| notes[i].clone()).collect();
                let due: Vec<_> = indices.iter().map(|&i| selected[i] == at).collect();
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
                stack.push(Step {
                    index,
                    notes: indices,
                    incoming: saved.clone(),
                    key,
                    search: search::Search::new(local_notes, at, due, local, clock),
                    complete,
                });
            }
        }
        let Some(current) = stack.last_mut() else {
            return Ok(false);
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
            }
            step = Some(index + 1);
        } else {
            let previous = stack.pop().unwrap();
            failed.insert((previous.index, previous.key));
            saved = previous.incoming;
        }
    }
}
