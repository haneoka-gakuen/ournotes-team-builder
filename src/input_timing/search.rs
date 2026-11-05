//! Finite input words replayed through the native whole-frame selector.
use super::{
    frame::{self, State, Touch, TouchState},
    input_times, Note,
};
use crate::num::f;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use rustc_hash::FxHashSet as HashSet;

pub(super) struct Outcome {
    pub state: State,
    pub inputs: Vec<Touch>,
}

fn adjacent(value: f64, up: bool) -> f64 {
    let value = value as f32;
    if up {
        value.next_up() as f64
    } else {
        value.next_down() as f64
    }
}

fn positions(notes: &[Note]) -> Vec<f64> {
    let mut cuts = vec![0.0, 23.0];
    for note in notes {
        cuts.extend([note.left, note.right, note.body_left, note.body_right]);
    }
    for (i, left) in notes.iter().enumerate() {
        for right in &notes[i + 1..] {
            if left.time != right.time {
                continue;
            }
            let (left, right) = if left.body_right < right.body_left {
                (left, right)
            } else if right.body_right < left.body_left {
                (right, left)
            } else {
                continue;
            };
            for strict in [false, true] {
                let mut low = (left.body_right.max(0.0) as f32).to_bits();
                let mut high = (right.body_left.min(23.0) as f32).to_bits();
                if left.body_right > 23.0 || right.body_left < 0.0 {
                    continue;
                }
                while low < high {
                    let middle = low + (high - low) / 2;
                    let point = f32::from_bits(middle) as f64;
                    let (a, b) = (left.body_distance(point), right.body_distance(point));
                    if a > b || !strict && a == b {
                        high = middle;
                    } else {
                        low = middle + 1;
                    }
                }
                cuts.push(f32::from_bits(low) as f64);
            }
        }
    }
    cuts.retain(|&point| (0.0..=23.0).contains(&point));
    let mut points = cuts.clone();
    for point in cuts {
        points.extend([adjacent(point, false), adjacent(point, true)]);
    }
    points.retain(|&point| (0.0..=23.0).contains(&point));
    points.sort_by(f64::total_cmp);
    points.dedup();
    let middle: Vec<_> = points.windows(2).map(|p| f((p[0] + p[1]) / 2.0)).collect();
    points.extend(middle);
    points.push(-1.0);
    points.sort_by(f64::total_cmp);
    points.dedup();
    points
}

fn atom_key(notes: &[Note], touch: Touch) -> Vec<u8> {
    let mut key = vec![touch.state as u8, u8::from(touch.flick_begin.is_some())];
    for note in notes {
        let current = frame::in_area(note, touch.lane);
        let begin = touch
            .flick_begin
            .is_some_and(|lane| frame::in_area(note, lane));
        key.push(
            u8::from(current)
                | (u8::from(begin) << 1)
                | (u8::from(frame::in_body(note, touch.lane)) << 2),
        );
        key.push(u8::from(frame::permitted_time(note, touch.time)));
        key.push(note.grade_at(touch.time) as u8);
    }
    if touch.state == TouchState::Enter {
        for (i, a) in notes.iter().enumerate() {
            for b in &notes[i + 1..] {
                if a.time == b.time {
                    key.push(
                        a.body_distance(touch.lane)
                            .total_cmp(&b.body_distance(touch.lane)) as i8
                            as u8,
                    );
                }
            }
        }
    }
    key
}

fn atoms(notes: &[Note], at: i32, clock: crate::live::FrameClock) -> Vec<Touch> {
    let points = positions(notes);
    let mut seen = HashSet::default();
    let mut out = Vec::new();
    let mut add = |touch| {
        if seen.insert(atom_key(notes, touch)) {
            out.push(touch);
        }
    };
    for state in [TouchState::Enter, TouchState::Hold, TouchState::Exit] {
        for time in input_times(clock, at) {
            for &lane in &points {
                add(Touch {
                    state,
                    lane,
                    time,
                    flick_begin: None,
                });
            }
        }
    }
    if notes.iter().any(Note::flick) {
        let mut seen_points = HashSet::default();
        let begins: Vec<_> = points
            .iter()
            .copied()
            .filter(|&point| {
                seen_points.insert(
                    notes
                        .iter()
                        .filter(|note| note.flick())
                        .map(|note| frame::in_area(note, point))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        for state in [TouchState::Hold, TouchState::Exit] {
            for time in input_times(clock, at) {
                for &lane in &points {
                    for &begin in &begins {
                        add(Touch {
                            state,
                            lane,
                            time,
                            flick_begin: Some(begin),
                        });
                    }
                }
            }
        }
    }
    out
}

#[derive(Default)]
struct WordScratch {
    inputs: Vec<Touch>,
    selected: Vec<Option<usize>>,
    used: Vec<usize>,
    word: Vec<usize>,
    units: Vec<usize>,
    key: Vec<usize>,
}
fn compact(notes: &[Note], done: &[bool], claims: &[bool], atoms: &[Touch], original: &[usize], scratch: &mut WordScratch) {
    let WordScratch { inputs, selected, used, word, units, key } = scratch;
    inputs.clear();
    inputs.extend(original.iter().map(|&index| atoms[index]));
    selected.clear();
    selected.extend(notes
        .iter()
        .enumerate().map(|(i, note)| if done[i] { None } else { frame::input_for(note, inputs) }));
    used.clear();
    used.extend(selected.iter().copied().flatten());
    used.sort_unstable();
    used.dedup();
    word.clear();
    word.extend(used.iter().map(|&index| original[index]));
    inputs.clear();
    inputs.extend(word.iter().map(|&index| atoms[index]));
    selected.clear();
    selected.extend(notes.iter().enumerate().map(|(i, note)| if done[i] { None } else { frame::input_for(note, inputs) }));
    units.resize(inputs.len(), usize::MAX);
    units.fill(usize::MAX);
    let mut next = 0;
    key.clear();
    for (i, chosen) in selected.iter().enumerate() {
        let Some(index) = *chosen else { key.extend([usize::MAX; 5]); continue; };
        if units[index] == usize::MAX { units[index] = next; next += 1; }
        let input = inputs[index];
        let note = &notes[i];
        let geometry = usize::from(frame::in_area(note, input.lane))
            | (usize::from(input.flick_begin.is_some_and(|lane| frame::in_area(note, lane))) << 1)
            | (usize::from(frame::in_body(note, input.lane)) << 2);
        key.extend([units[index], input.state as usize | (usize::from(input.flick_begin.is_some()) << 2),
            geometry, usize::from(frame::permitted_time(note, input.time)), note.grade_at(input.time)]);
    }
    for unit in units.iter_mut() {
        if *unit == usize::MAX { *unit = next; next += 1; }
    }
    key.push(next);
    // Appended inputs can replace a note's selected unit, but cannot restore a
    // discarded unit. Keep selected-note behaviour and every possible flick
    // claim; differences on other notes do not distinguish continuations.
    for canonical in 0..next {
        let unit = units.iter().position(|&value| value == canonical).unwrap();
        let input = inputs[unit];
        key.extend([input.state as usize, usize::from(input.flick_begin.is_some())]);
        if input.state != TouchState::Enter { continue; }
        for (i, note) in notes.iter().enumerate() {
            if claims[i] { key.push(usize::from(frame::in_area(note, input.lane) && frame::permitted_time(note, input.time))); }
        }
        for (i, a) in notes.iter().enumerate() {
            if !claims[i] && selected[i] != Some(unit) { continue; }
            for (j, b) in notes.iter().enumerate().skip(i + 1) {
                if a.time == b.time && (claims[j] || selected[j] == Some(unit)) {
                    key.push(a.body_distance(input.lane).total_cmp(&b.body_distance(input.lane)) as i8 as u8 as usize);
                }
            }
        }
    }
}

fn reservation_key(notes: &[Note], state: &State) -> Vec<u8> {
    notes
        .iter()
        .zip(&state.flicks)
        .enumerate()
        .map(|(i, (note, saved))| {
            if state.done[i] {
                return 0;
            }
            saved.map_or(0, |saved| {
                if frame::in_area(note, saved.begin) {
                    2 + note.grade_at(saved.time) as u8
                } else {
                    1
                }
            })
        })
        .collect()
}

fn irreversible(
    notes: &[Note],
    at: i32,
    due: &[bool],
    automatic: &State,
    inputs: &[Touch],
    judged: &[frame::Judgement],
    clock: crate::live::FrameClock,
) -> bool {
    for (i, note) in notes.iter().enumerate() {
        if automatic.done[i] {
            continue;
        }
        let actual = judged.iter().find(|j| j.note == i).map(|j| j.grade);
        if actual == due[i].then_some(note.grade) {
            continue;
        }
        let Some(unit) = frame::input_for(note, inputs) else {
            continue;
        };
        let input = inputs[unit];
        if note.flick() && input.flick_begin.is_some() {
            return true;
        }
        if note.asynchronous() {
            if due[i] && input.state == TouchState::Hold && at < note.time {
                return true;
            }
            let times = input_times(clock, at);
            if actual.is_some()
                && at >= note.time
                && frame::permitted_time(note, *times.start())
                && frame::permitted_time(note, *times.end())
                && (!due[i] || note.grade != 5)
            {
                return true;
            }
        }
    }
    false
}

type Word = (isize, Reverse<usize>, Reverse<usize>, Vec<usize>);

pub(super) struct Search {
    notes: Vec<Note>,
    at: i32,
    clock: crate::live::FrameClock,
    due: Vec<bool>,
    initial: State,
    automatic: State,
    claims: Vec<bool>,
    atoms: Option<Vec<Touch>>,
    queue: BinaryHeap<Word>,
    seen: HashSet<Vec<usize>>,
    found: HashSet<Vec<u8>>,
    pending: Option<Vec<usize>>,
    generation: u64,
    iterations: usize,
    serial: usize,
    exhausted: bool,
    final_state: bool,
    candidate: Vec<usize>,
    compact: WordScratch,
    inputs: Vec<Touch>,
    trial: State,
    frame_scratch: frame::Scratch,
}

impl Search {
    pub fn new(notes: Vec<Note>, at: i32, due: Vec<bool>, initial: State, clock: crate::live::FrameClock) -> Self {
        let mut automatic = initial.clone();
        let invalid = frame::run(&notes, at, &[], &mut automatic)
            .iter()
            .any(|j| !due[j.note] || j.grade != notes[j.note].grade)
            || notes.iter().enumerate().any(|(i, note)| due[i] && !automatic.done[i]
                && !frame::may_receive_grade(note, at, clock, initial.flicks[i]))
;
        let final_state = notes
            .iter()
            .enumerate()
            .all(|(i, note)| !note.flick() || initial.done[i] || due[i]);
        let claims = notes.iter().enumerate().map(|(i, note)| !automatic.done[i]
            && matches!(note.op, 40 | 41 | 102) && initial.flicks[i].is_none()).collect();
        let trial = initial.clone();
        Self {
            notes,
            at,
            clock,
            due,
            initial,
            automatic,
            claims,
            atoms: None,
            queue: BinaryHeap::from([(0, Reverse(0), Reverse(0), Vec::new())]),
            seen: HashSet::default(),
            found: HashSet::default(),
            pending: None,
            generation: crate::cancellation_generation(),
            iterations: 0,
            serial: 0,
            exhausted: invalid,
            final_state,
            candidate: Vec::new(),
            compact: WordScratch::default(),
            inputs: Vec::new(),
            trial,
            frame_scratch: frame::Scratch::default(),
        }
    }

    fn expand(&mut self, word: &[usize]) -> Result<Option<Outcome>, String> {
        if word.len() == 24 {
            return Ok(None);
        }
        let atoms = self
            .atoms
            .get_or_insert_with(|| {
                let active: Vec<_> = self.notes.iter().enumerate().filter(|(i, _)| !self.automatic.done[*i]).map(|(_, note)| note.clone()).collect();
                atoms(&active, self.at, self.clock)
            });
        for index in 0..atoms.len() {
            self.candidate.clear();
            self.candidate.extend_from_slice(word);
            self.candidate.push(index);
            compact(&self.notes, &self.automatic.done, &self.claims, atoms, &self.candidate, &mut self.compact);
            if self.seen.contains(self.compact.key.as_slice()) {
                continue;
            }
            self.inputs.clear();
            self.inputs.extend(self.compact.word.iter().map(|&index| atoms[index]));
            self.trial.clone_from(&self.initial);
            let judged = frame::run_cached(&self.notes, self.at, &self.inputs, &mut self.trial, &mut self.frame_scratch);
            if irreversible(
                &self.notes,
                self.at,
                &self.due,
                &self.automatic,
                &self.inputs,
                judged,
                self.clock,
            ) {
                continue;
            }
            if self.final_state && judged.len() == self.due.iter().filter(|&&due| due).count()
                && judged.iter().all(|j| self.due[j.note] && j.grade == self.notes[j.note].grade) {
                self.exhausted = true;
                return Ok(Some(Outcome { state: self.trial.clone(), inputs: self.inputs.clone() }));
            }
            self.seen.insert(self.compact.key.clone());
            if self.seen.len() > 65536 {
                return Err(format!(
                    "native input word search budget exceeded at {}",
                    self.at
                ));
            }
            let correct = judged
                .iter()
                .filter(|j| self.due[j.note] && j.grade == self.notes[j.note].grade)
                .count();
            let incorrect = judged.len() - correct;
            self.serial += 1;
            self.queue.push((
                correct as isize - 2 * incorrect as isize,
                Reverse(self.compact.word.len()),
                Reverse(self.serial),
                self.compact.word.clone(),
            ));
        }
        Ok(None)
    }

    pub fn next(&mut self) -> Result<Option<Outcome>, String> {
        if self.exhausted {
            return Ok(None);
        }
        if let Some(word) = self.pending.take() {
            if let Some(outcome) = self.expand(&word)? { return Ok(Some(outcome)); }
        }
        while let Some((_, _, _, word)) = self.queue.pop() {
            self.iterations += 1;
            if self.iterations.is_multiple_of(1024) {
                crate::clock::milliseconds();
                if crate::cancellation_generation() != self.generation {
                    return Err("cancelled".into());
                }
            }
            self.inputs.clear();
            if !word.is_empty() {
                self.inputs.extend(word.iter()
                    .map(|&index| self.atoms.as_ref().unwrap()[index])
                );
            }
            self.trial.clone_from(&self.initial);
            let judged = frame::run_cached(&self.notes, self.at, &self.inputs, &mut self.trial, &mut self.frame_scratch);
            if judged.len() == self.due.iter().filter(|&&due| due).count()
                && judged
                    .iter()
                    .all(|j| self.due[j.note] && j.grade == self.notes[j.note].grade)
                && self.found.insert(reservation_key(&self.notes, &self.trial))
            {
                if self.final_state {
                    self.exhausted = true;
                } else {
                    self.pending = Some(word);
                }
                return Ok(Some(Outcome { state: self.trial.clone(), inputs: self.inputs.clone() }));
            }
            if let Some(outcome) = self.expand(&word)? { return Ok(Some(outcome)); }
        }
        self.exhausted = true;
        Ok(None)
    }
}
