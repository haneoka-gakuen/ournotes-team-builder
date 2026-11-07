//! Whole-frame input selection used by the original FTLiveSimulator.
use super::Note;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum TouchState {
    None,
    Enter,
    Hold,
    Exit,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct Touch {
    pub state: TouchState,
    pub lane: f64,
    pub time: i32,
    pub flick_begin: Option<f64>,
}

#[derive(Clone, Copy)]
pub(super) struct ReservedFlick {
    pub time: i32,
    pub begin: f64,
}

pub(super) struct State {
    pub done: Vec<bool>,
    pub flicks: Vec<Option<ReservedFlick>>,
}
impl Clone for State {
    fn clone(&self) -> Self {
        Self { done: self.done.clone(), flicks: self.flicks.clone() }
    }
    fn clone_from(&mut self, source: &Self) {
        self.done.clone_from(&source.done);
        self.flicks.clone_from(&source.flicks);
    }
}

#[derive(Clone, Copy)]
pub(super) struct Judgement {
    pub note: usize,
    pub grade: usize,
}

#[derive(Clone, Copy)]
struct Candidate {
    note: usize,
    time: i32,
}

struct Claim {
    note: usize,
    input: Option<usize>,
}
#[derive(Default)]
pub(super) struct Scratch {
    results: Vec<Judgement>,
    synchronous: Vec<Vec<Candidate>>,
    asynchronous: Vec<Vec<Candidate>>,
    claims: Vec<Claim>,
    claimed: Vec<Option<usize>>,
}

#[inline]
pub(super) fn in_area(note: &Note, lane: f64) -> bool {
    note.left <= lane && lane <= note.right
}

#[inline]
pub(super) fn in_body(note: &Note, lane: f64) -> bool {
    note.body_left <= lane && lane <= note.body_right
}

/// GetUseSimulateInputUnitPair and GetHigherPriorityInputUnit.
pub(super) fn input_for(note: &Note, inputs: &[Touch]) -> Option<usize> {
    let mut selected: Option<usize> = None;
    for (index, input) in inputs.iter().enumerate() {
        if prefers_input(note, selected.map(|previous| inputs[previous]), *input) {
            selected = Some(index);
        }
    }
    selected
}

#[inline]
pub(super) fn prefers_input(note: &Note, previous: Option<Touch>, input: Touch) -> bool {
    if !in_area(note, input.lane) && !(note.flick() && input.flick_begin.is_some_and(|lane| in_area(note, lane))) { return false; }
    let Some(old) = previous else { return true; };
    if note.flick() { old.flick_begin.is_none() && input.flick_begin.is_some() }
    else if note.asynchronous() { input.state == TouchState::Hold }
    else if input.state == TouchState::Enter { old.state != TouchState::Enter || !in_body(note, old.lane) || in_body(note, input.lane) }
    else { false }
}

/// ShouldPreferCandidateNote uses frame offsets when no press position exists.
fn prefer(notes: &[Note], old: usize, new: usize, frame: i32, lane: Option<f64>) -> usize {
    let previous = &notes[old];
    let next = &notes[new];
    let choose_new = match lane {
        Some(lane) => {
            next.time < previous.time
                || next.time == previous.time
                    && next.body_distance(lane) < previous.body_distance(lane)
        }
        None => {
            (i64::from(frame) - i64::from(next.time)).abs()
                < (i64::from(frame) - i64::from(previous.time)).abs()
        }
    };
    if choose_new {
        new
    } else {
        old
    }
}

#[inline]
pub(super) fn permitted_time(note: &Note, time: i32) -> bool {
    i64::from(time) >= note.input_first && time <= note.last
}

fn judge_input(
    note: &Note,
    frame: i32,
    input: Touch,
    reserved: &mut Option<ReservedFlick>,
) -> Option<i32> {
    if note.flick() {
        if let Some(begin) = input.flick_begin {
            *reserved = Some(ReservedFlick {
                time: input.time,
                begin,
            });
        }
        let saved = (*reserved)?;
        return (in_area(note, saved.begin)
            && (input.state == TouchState::Exit
                || input.state == TouchState::Hold && frame >= note.time))
            .then_some(saved.time);
    }
    if note.trace() {
        return match input.state {
            TouchState::Exit if frame < note.last_phase => Some(input.time),
            TouchState::Enter | TouchState::Hold
                if frame >= note.time && frame < note.last_phase =>
            {
                Some(note.time)
            }
            _ => None,
        };
    }
    if note.tail() {
        return match input.state {
            TouchState::Exit if frame <= note.last => Some(input.time),
            TouchState::Hold if frame >= note.time && frame <= note.last => Some(note.time),
            _ => None,
        };
    }
    (input.state == TouchState::Enter).then_some(input.time)
}
pub(super) fn may_receive_grade(note: &Note, frame: i32, clock: crate::live::FrameClock, saved: Option<ReservedFlick>) -> bool {
    // Relax touch position and competition; they can only remove choices.
    for time in super::input_times(clock, frame) {
        if !permitted_time(note, time) { continue; }
        for state in [TouchState::Enter, TouchState::Hold, TouchState::Exit] {
            for flick_begin in [None, Some(note.left)] {
                if flick_begin.is_some() && !note.flick() { continue; }
                let mut reserved = saved;
                let input = Touch { state, lane: note.left, time, flick_begin };
                if judge_input(note, frame, input, &mut reserved).is_some_and(|time| note.grade_at(time) == note.grade) {
                    return true;
                }
            }
        }
    }
    false
}

pub(super) fn run(
    notes: &[Note],
    frame: i32,
    inputs: &[Touch],
    state: &mut State,
) -> Vec<Judgement> {
    run_cached(notes, frame, inputs, state, &mut Scratch::default()).to_vec()
}
pub(super) fn run_cached<'a>(
    notes: &[Note], frame: i32, inputs: &[Touch], state: &mut State, scratch: &'a mut Scratch,
) -> &'a [Judgement] {
    run_selection(notes, frame, inputs, None, state, scratch)
}

pub(super) fn run_selected<'a>(notes: &[Note], frame: i32, inputs: &[Touch], selected: &[Option<usize>], state: &mut State, scratch: &'a mut Scratch) -> &'a [Judgement] {
    run_selection(notes, frame, inputs, Some(selected), state, scratch)
}

fn run_selection<'a>(notes: &[Note], frame: i32, inputs: &[Touch], selected: Option<&[Option<usize>]>, state: &mut State, scratch: &'a mut Scratch) -> &'a [Judgement] {
    let Scratch { results, synchronous, asynchronous, claims, claimed } = scratch;
    results.clear();
    if synchronous.len() < inputs.len() { synchronous.resize_with(inputs.len(), Vec::new); }
    if asynchronous.len() < inputs.len() { asynchronous.resize_with(inputs.len(), Vec::new); }
    for row in synchronous.iter_mut().chain(asynchronous.iter_mut()) { row.clear(); }
    claims.clear();
    claimed.resize(inputs.len(), None);
    claimed.fill(None);
    for (index, note) in notes.iter().enumerate() {
        if state.done[index] || frame < note.first {
            continue;
        }
        if frame >= note.auto_frame {
            state.done[index] = true;
            results.push(Judgement {
                note: index,
                grade: 1,
            });
            continue;
        }
        if frame >= note.time {
            if let Some(saved) = state.flicks[index].filter(|saved| in_area(note, saved.begin)) {
                state.done[index] = true;
                results.push(Judgement {
                    note: index,
                    grade: note.grade_at(saved.time),
                });
                continue;
            }
        }
        if frame >= note.forced_frame {
            state.done[index] = true;
            results.push(Judgement {
                note: index,
                grade: 1,
            });
        }
    }

    for (index, note) in notes.iter().enumerate() {
        if state.done[index] || frame < note.first {
            continue;
        }
        let claim_before = matches!(note.op, 40 | 41 | 102) && state.flicks[index].is_none();
        let selected = selected.map_or_else(|| input_for(note, inputs), |selected| selected[index]);
        let mut judged = None;
        if let Some(unit) = selected {
            let input = inputs[unit];
            if permitted_time(note, input.time) {
                judged = judge_input(note, frame, input, &mut state.flicks[index]);
            }
        }
        if let (Some(unit), Some(time)) = (selected, judged) {
            let candidate = Candidate { note: index, time };
            if note.asynchronous() {
                asynchronous[unit].push(candidate);
            } else {
                synchronous[unit].push(candidate);
            }
        } else if claim_before {
            claims.push(Claim {
                note: index,
                input: selected.filter(|&unit| {
                    permitted_time(note, inputs[unit].time) && state.flicks[index].is_some()
                }),
            });
        }
    }

    for claim in claims.iter() {
        for (unit, input) in inputs.iter().enumerate() {
            if claim.input.is_some_and(|index| index != unit)
                || input.state != TouchState::Enter
                || !in_area(&notes[claim.note], input.lane)
                || !permitted_time(&notes[claim.note], input.time)
            {
                continue;
            }
            claimed[unit] = Some(claimed[unit].map_or(claim.note, |old| {
                prefer(notes, old, claim.note, frame, Some(input.lane))
            }));
        }
    }

    for (unit, input) in inputs.iter().enumerate() {
        if input.state == TouchState::None {
            continue;
        }
        let position = (input.state == TouchState::Enter).then_some(input.lane);
        let mut winner: Option<Candidate> = None;
        for candidate in &synchronous[unit] {
            if claimed[unit]
                .is_some_and(|claim| prefer(notes, claim, candidate.note, frame, position) == claim)
            {
                continue;
            }
            if winner.is_none_or(|old| {
                prefer(notes, old.note, candidate.note, frame, position) == candidate.note
            }) {
                winner = Some(*candidate);
            }
        }
        for candidate in winner.iter().chain(asynchronous[unit].iter()) {
            if !state.done[candidate.note] {
                state.done[candidate.note] = true;
                results.push(Judgement {
                    note: candidate.note,
                    grade: notes[candidate.note].grade_at(candidate.time),
                });
            }
        }
    }
    results
}
