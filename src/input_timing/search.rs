//! Finite input words replayed through the native whole-frame selector.
use super::{
    frame::{self, State, Touch, TouchState},
    input_times, Note,
};
use crate::num::f;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};

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

fn atoms(notes: &[Note], at: i32) -> Vec<Touch> {
    let points = positions(notes);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut add = |touch| {
        if seen.insert(atom_key(notes, touch)) {
            out.push(touch);
        }
    };
    for state in [TouchState::Enter, TouchState::Hold, TouchState::Exit] {
        for time in input_times(at) {
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
        let mut seen_points = HashSet::new();
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
            for time in input_times(at) {
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

fn compact(notes: &[Note], atoms: &[Touch], word: &[usize]) -> (Vec<usize>, Vec<usize>) {
    let inputs: Vec<_> = word.iter().map(|&index| atoms[index]).collect();
    let selected: Vec<_> = notes
        .iter()
        .map(|note| frame::input_for(note, &inputs))
        .collect();
    let mut used: Vec<_> = selected.iter().copied().flatten().collect();
    used.sort_unstable();
    used.dedup();
    let word: Vec<_> = used.iter().map(|&index| word[index]).collect();
    let key = selected
        .iter()
        .flat_map(|selected| {
            selected.map_or([usize::MAX; 2], |index| {
                [
                    used.binary_search(&index).unwrap(),
                    inputs[index].state as usize,
                ]
            })
        })
        .chain(word.iter().copied())
        .collect();
    (word, key)
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
            let times = input_times(at);
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
    due: Vec<bool>,
    initial: State,
    automatic: State,
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
}

impl Search {
    pub fn new(notes: Vec<Note>, at: i32, due: Vec<bool>, initial: State) -> Self {
        let mut automatic = initial.clone();
        let invalid = frame::run(&notes, at, &[], &mut automatic)
            .iter()
            .any(|j| !due[j.note] || j.grade != notes[j.note].grade);
        let final_state = notes
            .iter()
            .enumerate()
            .all(|(i, note)| !note.flick() || initial.done[i] || due[i]);
        Self {
            notes,
            at,
            due,
            initial,
            automatic,
            atoms: None,
            queue: BinaryHeap::from([(0, Reverse(0), Reverse(0), Vec::new())]),
            seen: HashSet::new(),
            found: HashSet::new(),
            pending: None,
            generation: crate::cancellation_generation(),
            iterations: 0,
            serial: 0,
            exhausted: invalid,
            final_state,
        }
    }

    fn expand(&mut self, word: &[usize]) -> Result<(), String> {
        if word.len() == 24 {
            return Ok(());
        }
        let atoms = self
            .atoms
            .get_or_insert_with(|| atoms(&self.notes, self.at));
        for index in 0..atoms.len() {
            let mut next = word.to_vec();
            next.push(index);
            let (next, key) = compact(&self.notes, atoms, &next);
            if self.seen.contains(&key) {
                continue;
            }
            let inputs: Vec<_> = next.iter().map(|&index| atoms[index]).collect();
            let mut result = self.initial.clone();
            let judged = frame::run(&self.notes, self.at, &inputs, &mut result);
            if irreversible(
                &self.notes,
                self.at,
                &self.due,
                &self.automatic,
                &inputs,
                &judged,
            ) {
                continue;
            }
            self.seen.insert(key);
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
                Reverse(next.len()),
                Reverse(self.serial),
                next,
            ));
        }
        Ok(())
    }

    pub fn next(&mut self) -> Result<Option<Outcome>, String> {
        if self.exhausted {
            return Ok(None);
        }
        if let Some(word) = self.pending.take() {
            self.expand(&word)?;
        }
        while let Some((_, _, _, word)) = self.queue.pop() {
            self.iterations += 1;
            if self.iterations.is_multiple_of(1024) {
                crate::clock::milliseconds();
                if crate::cancellation_generation() != self.generation {
                    return Err("cancelled".into());
                }
            }
            let inputs: Vec<_> = if word.is_empty() {
                Vec::new()
            } else {
                word.iter()
                    .map(|&index| self.atoms.as_ref().unwrap()[index])
                    .collect()
            };
            let mut state = self.initial.clone();
            let judged = frame::run(&self.notes, self.at, &inputs, &mut state);
            if judged.len() == self.due.iter().filter(|&&due| due).count()
                && judged
                    .iter()
                    .all(|j| self.due[j.note] && j.grade == self.notes[j.note].grade)
                && self.found.insert(reservation_key(&self.notes, &state))
            {
                if self.final_state {
                    self.exhausted = true;
                } else {
                    self.pending = Some(word);
                }
                return Ok(Some(Outcome { state, inputs }));
            }
            self.expand(&word)?;
        }
        self.exhausted = true;
        Ok(None)
    }
}
