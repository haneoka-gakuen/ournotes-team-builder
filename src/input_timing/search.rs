//! Finite input words replayed through the native whole-frame selector.
use super::{
    frame::{self, State, Touch, TouchState},
    input_times, Action, Note,
};
use crate::num::f;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::sync::{Arc, Mutex};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

type InputPrograms = Vec<(Vec<usize>, Vec<u8>)>;

#[derive(Default)]
pub(super) struct AtomCache {
    entries: HashMap<Vec<u64>, Arc<Vec<Touch>>>,
    bytes: usize,
    solutions: HashMap<Vec<u64>, (Vec<Outcome>, bool)>,
    solution_bytes: usize,
    languages: HashMap<Vec<u64>, InputPrograms>,
    language_bytes: usize,
}

fn solution_key(notes: &[Note], at: i32, clock: crate::live::FrameClock, initial: &State, due: &[bool], early_auto: &[bool], auto_grade: &[Option<usize>]) -> Vec<u64> {
    let mut key = vec![0, clock.advance_ms() as u64, (i64::from(*input_times(clock, at).start()) - i64::from(at)) as u64, notes.len() as u64];
    for (i, note) in notes.iter().enumerate() {
        key.extend([note.op as u64, note.grade as u64, note.left.to_bits(), note.right.to_bits(), note.body_left.to_bits(), note.body_right.to_bits(),
            u64::from(due[i]) | u64::from(early_auto[i]) << 1 | u64::from(initial.done[i]) << 2 | (auto_grade[i].unwrap_or(0) as u64) << 3]);
        key.extend([(i64::from(note.time) - i64::from(at)) as u64,
            u64::from(at >= note.first) | u64::from(at <= note.last) << 1 | u64::from(at < note.last_phase) << 2
                | u64::from(at < note.auto_frame) << 3 | u64::from(at < note.forced_frame) << 4]);
        key.extend(note.before.iter().chain(&note.after).map(|&time| time as u64));
        key.push(reserved_class(note, initial.flicks[i]) as u64);
    }
    key
}

fn shift_outcome(outcome: &Outcome, shift: i32) -> Outcome {
    let mut out = outcome.clone();
    for saved in out.state.flicks.iter_mut().flatten() { saved.time += shift; }
    for input in &mut out.inputs { input.time += shift; }
    out
}

fn save_solutions(cache: &Mutex<AtomCache>, key: &[u64], outcomes: &[Outcome], complete: bool, at: i32) {
    let outcomes: Vec<_> = outcomes.iter().map(|outcome| shift_outcome(outcome, -at)).collect();
    let bytes = key.len() * 8 + outcomes.iter().map(|outcome| outcome.inputs.capacity() * std::mem::size_of::<Touch>()
        + outcome.state.done.capacity() + outcome.state.flicks.capacity() * std::mem::size_of::<Option<frame::ReservedFlick>>()).sum::<usize>();
    const LIMIT: usize = 4 * 1_048_576;
    if bytes > LIMIT { return; }
    let mut cache = cache.lock().unwrap();
    if let Some((old, _)) = cache.solutions.remove(key) {
        cache.solution_bytes -= key.len()*8 + old.iter().map(|outcome| outcome.inputs.capacity()*std::mem::size_of::<Touch>()
            + outcome.state.done.capacity() + outcome.state.flicks.capacity()*std::mem::size_of::<Option<frame::ReservedFlick>>()).sum::<usize>();
    }
    if cache.solution_bytes + bytes > LIMIT { cache.solutions.clear(); cache.solution_bytes = 0; }
    cache.solutions.insert(key.to_vec(), (outcomes, complete));
    cache.solution_bytes += bytes;
}

fn cached_atoms(notes: &[Note], at: i32, clock: crate::live::FrameClock, flicks_only: bool, cache: &Mutex<AtomCache>) -> Arc<Vec<Touch>> {
    let mut key = vec![clock.advance_ms() as u64, (i64::from(*input_times(clock, at).start()) - i64::from(at)) as u64, u64::from(flicks_only)];
    for note in notes {
        key.extend([(i64::from(note.time) - i64::from(at)) as u64, note.op as u64, note.grade as u64, note.left.to_bits(), note.right.to_bits(),
            note.body_left.to_bits(), note.body_right.to_bits()]);
        key.extend(note.before.iter().chain(&note.after).map(|&time| time as u64));
    }
    if let Some(value) = cache.lock().unwrap().entries.get(&key) {
        return Arc::new(value.iter().copied().map(|mut touch| { touch.time += at; touch }).collect());
    }
    let value = Arc::new(atoms(notes, at, clock, flicks_only));
    let bytes = key.capacity() * std::mem::size_of::<u64>() + value.capacity() * std::mem::size_of::<Touch>();
    const LIMIT: usize = 4 * 1_048_576;
    if bytes <= LIMIT {
        let mut cache = cache.lock().unwrap();
        if cache.bytes + bytes > LIMIT { cache.entries.clear(); cache.bytes = 0; }
        cache.entries.insert(key, Arc::new(value.iter().copied().map(|mut touch| { touch.time -= at; touch }).collect()));
        cache.bytes += bytes;
    }
    value
}

#[derive(Clone)]
pub(super) struct Outcome {
    pub state: State,
    pub inputs: Vec<Touch>,
}

fn verified_outcome(notes: &[Note], at: i32, initial: &State, inputs: &[Touch], expected: &State) -> Result<Outcome, String> {
    let mut state = initial.clone();
    let judged = frame::run(notes, at, inputs, &mut state);
    if judged.iter().any(|j| j.grade != notes[j.note].grade) || state.done != expected.done
        || notes.iter().enumerate().any(|(i, note)| !state.done[i] && reserved_class(note, state.flicks[i]) != reserved_class(note, expected.flicks[i])) {
        return Err("native input selection projection mismatch".into());
    }
    Ok(Outcome { state, inputs: inputs.to_vec() })
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
    cuts.retain(|point| point.is_finite());
    let mut points = cuts.clone();
    for point in cuts {
        points.extend([adjacent(point, false), adjacent(point, true)]);
    }
    points.retain(|point| point.is_finite());
    points.sort_by(f64::total_cmp);
    points.dedup();
    let middle: Vec<_> = points.windows(2).map(|p| f((p[0] + p[1]) / 2.0)).collect();
    points.extend(middle);
    points.push(-1.0);
    if let Some(left) = notes.iter().map(|note| note.left).min_by(f64::total_cmp) {
        points.push(adjacent(left, false));
    }
    if let Some(right) = notes.iter().map(|note| note.right).max_by(f64::total_cmp) {
        points.push(adjacent(right, true));
    }
    points.retain(|point| point.is_finite());
    points.sort_by(f64::total_cmp);
    points.dedup();
    points
}

fn asynchronous_grade(note: &Note, at: i32, input: Touch) -> Option<usize> {
    if !frame::permitted_time(note, input.time) { return None; }
    let action = match input.state {
        TouchState::Enter => Action::Enter,
        TouchState::Hold => Action::Hold,
        TouchState::Exit => Action::Exit,
        TouchState::None => return None,
    };
    note.hit(at, input.time, action)
}

fn grade_class(note: &Note, grade: Option<usize>) -> usize {
    grade.map_or(0, |grade| if grade == note.grade { 2 } else { 1 })
}

pub(super) fn reserved_class(note: &Note, saved: Option<frame::ReservedFlick>) -> u8 {
    saved.map_or(0, |saved| if !frame::in_area(note, saved.begin) { 1 }
        else if note.grade_at(saved.time) == note.grade { 2 } else { 3 })
}

fn atom_key(notes: &[Note], touch: Touch, at: i32) -> Vec<u8> {
    let taps = notes.iter().any(|note| !note.flick() && !note.asynchronous());
    let mut key = vec![
        u8::from(touch.state == TouchState::Enter && taps),
        u8::from(touch.state == TouchState::Hold && notes.iter().any(Note::asynchronous)),
        u8::from(touch.flick_begin.is_some()),
    ];
    for note in notes {
        let current = frame::in_area(note, touch.lane);
        if note.asynchronous() {
            key.extend([u8::from(current), if current { grade_class(note, asynchronous_grade(note, at, touch)) as u8 } else { 0 }, 0]);
            continue;
        }
        if !note.flick() && touch.state != TouchState::Enter { key.extend([0; 3]); continue; }
        let begin = touch
            .flick_begin
            .is_some_and(|lane| frame::in_area(note, lane));
        if note.flick() {
            let permitted = frame::permitted_time(note, touch.time);
            if !current && !begin { key.extend([0; 4]); continue; }
            let gesture = touch.flick_begin.is_some();
            let update = if gesture && permitted { if !begin { 1 } else if note.grade_at(touch.time) == note.grade { 2 } else { 3 } } else { 0 };
            let release = touch.state == TouchState::Exit || touch.state == TouchState::Hold && at >= note.time;
            let judgement = if permitted && release { if !gesture { 4 } else if begin { grade_class(note, Some(note.grade_at(touch.time))) as u8 } else { 0 } } else { 0 };
            key.extend([1, update, judgement, u8::from(touch.state == TouchState::Enter && current && permitted)]);
            continue;
        }
        if !current { key.extend([0; 3]); continue; }
        key.push(
            u8::from(current || begin)
                | (u8::from(begin) << 1)
                | (u8::from(!note.flick() && frame::in_body(note, touch.lane)) << 2),
        );
        key.push(u8::from(frame::permitted_time(note, touch.time)));
        key.push(if frame::permitted_time(note, touch.time) && (!note.flick() || begin) { grade_class(note, Some(note.grade_at(touch.time))) as u8 } else { 0 });
    }
    if touch.state == TouchState::Enter && taps {
        for (i, a) in notes.iter().enumerate() {
            if a.asynchronous() { continue; }
            for b in &notes[i + 1..] {
                if !b.asynchronous() && a.time == b.time {
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

fn atoms(notes: &[Note], at: i32, clock: crate::live::FrameClock, flicks_only: bool) -> Vec<Touch> {
    let points = positions(notes);
    let geometry = |state| {
        let mut seen = HashSet::default();
        points.iter().copied().filter(|&lane| seen.insert(atom_key(notes, Touch {
            state, lane, time: *input_times(clock, at).start(), flick_begin: None,
        }, at))).collect::<Vec<_>>()
    };
    let times = |state, begin| {
        let mut seen = HashSet::default();
        input_times(clock, at).filter(|&time| {
            let mut key = Vec::with_capacity(notes.len() * 2);
            for note in notes {
                if note.asynchronous() { key.extend([grade_class(note, asynchronous_grade(note, at, Touch { state, lane: 0.0, time, flick_begin: None })) as u8, 0]); }
                else if note.flick() || state == TouchState::Enter {
                    let permitted = frame::permitted_time(note, time);
                    key.extend([u8::from(permitted), if permitted && (!note.flick() || begin) { grade_class(note, Some(note.grade_at(time))) as u8 } else { 0 }]);
                } else { key.extend([0, 0]); }
            }
            seen.insert(key)
        }).collect::<Vec<_>>()
    };
    let mut seen = HashSet::default();
    let mut out = Vec::new();
    let mut add = |touch| {
        if seen.insert(atom_key(notes, touch, at)) {
            out.push(touch);
        }
    };
    for state in [TouchState::Enter, TouchState::Hold, TouchState::Exit].into_iter().filter(|&state| !flicks_only || state != TouchState::Enter) {
        let current = geometry(state);
        for time in times(state, false) {
            for &lane in &current {
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
            let current = geometry(state);
            for time in times(state, true) {
                for &lane in &current {
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
    key: Vec<u8>,
    reservations: Vec<(usize,u8)>,
}
fn compact(notes: &[Note], automatic: &State, claims: &[bool], atoms: &[Touch], original: &[usize], mode: (i32, &[Option<usize>]), scratch: &mut WordScratch) {
    let (at, base) = mode;
    let last = original.len() - 1;
    let select = |i: usize, note: &Note, inputs: &[Touch]| {
        if automatic.done[i] { return None; }
        let chosen = if frame::prefers_input(note, base[i].map(|unit| inputs[unit]), inputs[last]) { Some(last) } else { base[i] };
        chosen.filter(|&index| if note.flick() { inputs[index].flick_begin.is_some()
            || automatic.flicks[i].is_some_and(|saved| frame::in_area(note, saved.begin)) }
            else { note.asynchronous() || inputs[index].state == TouchState::Enter })
    };
    let competes = |i: usize, note: &Note, input: Touch| {
        if note.asynchronous() || !frame::permitted_time(note, input.time) { return false; }
        if !note.flick() { return input.state == TouchState::Enter; }
        let valid = input.flick_begin.map_or_else(|| automatic.flicks[i].is_some_and(|saved| frame::in_area(note, saved.begin)), |lane| frame::in_area(note, lane));
        valid && (input.state == TouchState::Exit || input.state == TouchState::Hold && at >= note.time)
    };
    let WordScratch { inputs, selected, used, word, units, key, reservations } = scratch;
    reservations.clear();
    inputs.clear();
    inputs.extend(original.iter().map(|&index| atoms[index]));
    selected.clear();
    selected.extend(notes
        .iter()
        .enumerate().map(|(i, note)| select(i, note, inputs)));
    used.clear();
    used.extend(selected.iter().copied().flatten());
    used.sort_unstable();
    used.dedup();
    word.clear();
    word.extend(used.iter().map(|&index| original[index]));
    inputs.clear();
    inputs.extend(word.iter().map(|&index| atoms[index]));
    for selected in selected.iter_mut().flatten() { *selected = used.binary_search(selected).unwrap(); }
    units.resize(inputs.len(), usize::MAX);
    units.fill(usize::MAX);
    let mut next = 0;
    key.clear();
    for (i, chosen) in selected.iter().enumerate() {
        let Some(index) = *chosen else { key.extend([u8::MAX; 5]); continue; };
        let input = inputs[index];
        let note = &notes[i];
        if note.asynchronous() {
            key.extend([usize::MAX, 0, grade_class(note, asynchronous_grade(note, at, input)), 0, 0].map(|value| value as u8));
            continue;
        }
        let unit = if competes(i, note, input) {
            if units[index] == usize::MAX { units[index] = next; next += 1; }
            units[index]
        } else { usize::MAX };
        let permitted = frame::permitted_time(note, input.time);
        if note.flick() {
            let saved = if permitted { input.flick_begin.map(|begin| frame::ReservedFlick { time: input.time, begin }).or(automatic.flicks[i]) } else { automatic.flicks[i] };
            let state = reserved_class(note, saved);
            if input.flick_begin.is_some() && state >= 2 {reservations.push((i,state));}
            let judgement = if unit == usize::MAX { 0 } else if state == 2 { 2 } else { 1 };
            key.extend([unit as u8, u8::from(input.flick_begin.is_some()), state, judgement, 0]);
            continue;
        }
        key.extend([unit as u8, u8::from(frame::in_body(note, input.lane)), u8::from(permitted),
            if permitted { grade_class(note, Some(note.grade_at(input.time))) as u8 } else { 0 }, 0]);
    }
    key.push(next as u8);
    // Appended inputs can replace a note's selected unit, but cannot restore a
    // discarded unit. Keep selected-note behaviour and every possible flick
    // claim; differences on other notes do not distinguish continuations.
    for canonical in 0..next {
        let unit = units.iter().position(|&value| value == canonical).unwrap();
        let input = inputs[unit];
        key.extend([u8::from(input.state == TouchState::Enter), u8::from(input.flick_begin.is_some())]);
        if input.state != TouchState::Enter { continue; }
        for (i, note) in notes.iter().enumerate() {
            if claims[i] { key.push(u8::from(frame::in_area(note, input.lane) && frame::permitted_time(note, input.time))); }
        }
        for (i, a) in notes.iter().enumerate() {
            if a.asynchronous() { continue; }
            if !claims[i] && selected[i] != Some(unit) { continue; }
            for (j, b) in notes.iter().enumerate().skip(i + 1) {
                if !b.asynchronous() && a.time == b.time && (claims[j] || selected[j] == Some(unit)) {
                    key.push(a.body_distance(input.lane).total_cmp(&b.body_distance(input.lane)) as i8 as u8);
                }
            }
        }
    }
}

fn reservation_key(notes: &[Note], state: &State, free: bool) -> Vec<u8> {
    notes
        .iter()
        .zip(&state.flicks)
        .enumerate()
        .map(|(i, (note, saved))| {
            if state.done[i] {
                return if free { 8 } else { 0 };
            }
            reserved_class(note, *saved)
        })
        .collect()
}

fn reservation_mask(note: &Note, at: i32, clock: crate::live::FrameClock, saved: Option<frame::ReservedFlick>, early: bool, grade: Option<usize>) -> u8 {
    let state = reserved_class(note, saved);
    let invalid = saved.is_some_and(|saved| frame::in_area(note, saved.begin)
        && (early || grade.is_some_and(|required| note.grade_at(saved.time) != required)));
    let mut states = if invalid { 0 } else { 1u8 << state };
    states |= 1 << 1;
    if !early {
        for time in input_times(clock, at).filter(|&time| frame::permitted_time(note, time)) {
            let candidate = note.grade_at(time);
            if grade.is_none_or(|required| candidate == required) { states |= 1 << if candidate == note.grade { 2 } else { 3 }; }
        }
    }
    states
}

struct FlickSignatureBound<'a> {
    notes: &'a [Note],
    flicks: &'a [usize],
    masks: &'a [u8],
    times: &'a [[u64; 2]],
    suppressors: &'a [[u16; 2]],
    done: u16,
    at: i32,
}
type SharedFlickInput = (usize, f64, f64, u64);
impl FlickSignatureBound<'_> {
    fn assign(&self, needed: &[(usize, usize)], position: usize, groups: &mut [SharedFlickInput]) -> bool {
        let Some(&(note, class)) = needed.get(position) else { return true; };
        let geometry = &self.notes[self.flicks[note]];
        for winner in 0..groups.len() {
            let old = groups[winner];
            if self.suppressors[note][class] & (1 << old.0) == 0 { continue; }
            let left = old.1.max(geometry.left);
            let right = old.2.min(geometry.right);
            let times = old.3 & self.times[note][class];
            if left > right || times == 0 { continue; }
            groups[winner] = (old.0, left, right, times);
            let possible = self.assign(needed, position + 1, groups);
            groups[winner] = old;
            if possible { return true; }
        }
        false
    }

    fn count(&self, position: usize, needed: &mut Vec<(usize, usize)>) -> usize {
        if position == self.flicks.len() {
            if needed.is_empty() { return 1; }
            let mut required = needed.clone();
            required.sort_by_key(|&(note, class)| (self.suppressors[note][class] & self.done).count_ones());
            let mut groups: Vec<_> = self.flicks.iter().enumerate().filter(|(winner, _)| self.done & (1 << winner) != 0)
                .map(|(winner, &note)| (winner, self.notes[note].left, self.notes[note].right, self.times[winner][0])).collect();
            return usize::from(self.assign(&required, 0, &mut groups));
        }
        if self.done & (1 << position) != 0 { return self.count(position + 1, needed); }
        let mut total = ((self.masks[position] & 3).count_ones() as usize).saturating_mul(self.count(position + 1, needed));
        for class in 0..2 {
            if self.masks[position] & (1 << (class + 2)) == 0 { continue; }
            let past = self.at >= self.notes[self.flicks[position]].time;
            if past { needed.push((position, class)); }
            total = total.saturating_add(self.count(position + 1, needed));
            if past { needed.pop(); }
        }
        total
    }
}

fn irreversible(
    notes: &[Note],
    at: i32,
    desired: (&[bool], bool, &[bool]),
    automatic: &State,
    inputs: &[Touch],
    judged: &[frame::Judgement],
    clock: crate::live::FrameClock,
) -> bool {
    let (due, free, allowed) = desired;
    for (i, note) in notes.iter().enumerate() {
        if automatic.done[i] {
            continue;
        }
        let actual = judged.iter().find(|j| j.note == i).map(|j| j.grade);
        if actual == Some(note.grade) && if free { allowed[i] } else { due[i] }
            || actual.is_none() && !due[i] {
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
                && (!free && !due[i] || note.grade != 5)
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
    free: bool,
    allowed: Vec<bool>,
    early_auto: Vec<bool>,
    auto_grade: Vec<Option<usize>>,
    initial: State,
    automatic: State,
    claims: Vec<bool>,
    atoms: Option<Arc<Vec<Touch>>>,
    atom_cache: Arc<Mutex<AtomCache>>,
    solution_key: Vec<u64>,
    language_key: Option<Vec<u64>>,
    solutions: Vec<Outcome>,
    warm: VecDeque<Outcome>,
    queue: BinaryHeap<Word>,
    seen: HashSet<Vec<u8>>,
    found: HashSet<Vec<u8>>,
    pending: Option<Vec<usize>>,
    generation: u64,
    iterations: usize,
    serial: usize,
    exhausted: bool,
    final_state: bool,
    reservation_limit: usize,
    limit_ready: bool,
    flicks_only: bool,
    async_checked: bool,
    candidate: Vec<usize>,
    compact: WordScratch,
    base_selected: Vec<Option<usize>>,
    inputs: Vec<Touch>,
    trial: State,
    frame_scratch: frame::Scratch,
}

impl Search {
    pub fn new(notes: Vec<Note>, at: i32, targets: Vec<i32>, initial: State, clock: crate::live::FrameClock, atom_cache: Arc<Mutex<AtomCache>>, next_frame: Option<i32>) -> Self {
        let due: Vec<_> = targets.iter().map(|&target| target == at).collect();
        let early_auto: Vec<_> = notes.iter().zip(&targets).map(|(note, &target)| note.flick()
            && next_frame.is_some_and(|next| note.time <= next && next < target)).collect();
        let auto_grade: Vec<_> = notes.iter().zip(&targets).map(|(note, &target)| next_frame
            .filter(|&next| note.flick() && note.time <= next && next == target && next < note.auto_frame)
            .map(|_| note.grade)).collect();
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
        let flicks_only = notes.iter().enumerate().all(|(i, note)| initial.done[i] || note.flick());
        let solution_key = solution_key(&notes, at, clock, &initial, &due, &early_auto, &auto_grade);
        let (solutions, complete) = atom_cache.lock().unwrap().solutions.get(&solution_key)
            .map(|(outcomes, complete)| (outcomes.iter().map(|outcome| shift_outcome(outcome, at)).collect::<Vec<_>>(), *complete))
            .unwrap_or_default();
        let found = solutions.iter().map(|outcome| reservation_key(&notes, &outcome.state, false)).collect();
        let warm = solutions.clone().into();
        let trial = initial.clone();
        Self {
            notes,
            at,
            clock,
            allowed: due.clone(),
            due,
            free: false,
            early_auto,
            auto_grade,
            initial,
            automatic,
            claims,
            atoms: None,
            atom_cache,
            solution_key,
            language_key: None,
            solutions,
            warm,
            queue: BinaryHeap::from([(0, Reverse(0), Reverse(0), Vec::new())]),
            seen: HashSet::default(),
            found,
            pending: None,
            generation: crate::cancellation_generation(),
            iterations: 0,
            serial: 0,
            exhausted: invalid || complete,
            final_state,
            reservation_limit: usize::MAX,
            limit_ready: false,
            flicks_only,
            async_checked: false,
            candidate: Vec::new(),
            compact: WordScratch::default(),
            base_selected: Vec::new(),
            inputs: Vec::new(),
            trial,
            frame_scratch: frame::Scratch::default(),
        }
    }

    pub fn flexible(notes: Vec<Note>, at: i32, masks: (Vec<bool>, Vec<bool>), initial: State, clock: crate::live::FrameClock, atom_cache: Arc<Mutex<AtomCache>>, next_frame: Option<i32>) -> Self {
        let (required, allowed) = masks;
        let targets = required.iter().map(|&due| if due { at } else { i32::MAX }).collect();
        let mut out = Self::new(notes, at, targets, initial, clock, atom_cache, None);
        out.free = true;
        out.allowed = allowed;
        out.final_state = false;
        out.early_auto.fill(false);
        out.auto_grade = out.notes.iter().map(|note| next_frame
            .filter(|&next| note.flick() && note.time <= next && next < note.auto_frame).map(|_| note.grade)).collect();
        let mut automatic = out.initial.clone();
        out.exhausted = frame::run(&out.notes, at, &[], &mut automatic).iter()
            .any(|j| !out.allowed[j.note] || j.grade != out.notes[j.note].grade)
            || out.notes.iter().enumerate().any(|(i, note)| out.due[i] && !automatic.done[i]
                && !frame::may_receive_grade(note, at, clock, out.initial.flicks[i]));
        out.automatic = automatic;
        out.limit_ready = false;
        out.solution_key = solution_key(&out.notes, at, clock, &out.initial, &out.due, &out.early_auto, &out.auto_grade);
        out.solution_key[0] = 1;
        out.solution_key.extend(out.allowed.iter().map(|&allowed| u64::from(allowed)));
        let (solutions, complete) = out.atom_cache.lock().unwrap().solutions.get(&out.solution_key)
            .map(|(outcomes, complete)| (outcomes.iter().map(|outcome| shift_outcome(outcome, at)).collect::<Vec<_>>(), *complete))
            .unwrap_or_default();
        out.found = solutions.iter().map(|outcome| reservation_key(&out.notes, &outcome.state, true)).collect();
        out.warm = solutions.clone().into();
        out.solutions = solutions;
        out.exhausted |= complete;
        out
    }

    // Atomic observations and both native note priorities define the frame language.
    fn language(&mut self) -> Result<bool, String> {
        let active: Vec<_> = self.notes.iter().enumerate().filter(|(i,_)| !self.automatic.done[*i]).map(|(_,note)| note.clone()).collect();
        let atoms = self.atoms.get_or_insert_with(|| cached_atoms(&active, self.at, self.clock, self.flicks_only, &self.atom_cache));
        let mut key = vec![u64::from(self.free),self.notes.len() as u64,atoms.len() as u64];
        for (i,note) in self.notes.iter().enumerate() {
            key.extend([note.op as u64,note.grade as u64,note.left.to_bits(),note.right.to_bits(),note.body_left.to_bits(),note.body_right.to_bits(),
                u64::from(self.initial.done[i]) | u64::from(self.automatic.done[i]) << 1 | u64::from(self.due[i]) << 2 | u64::from(self.allowed[i]) << 3
                    | u64::from(self.early_auto[i]) << 4 | u64::from(self.auto_grade[i].is_some()) << 5,
                reserved_class(note,self.initial.flicks[i]) as u64,
                u64::from(self.at >= note.first) | u64::from(self.at >= note.time) << 1 | u64::from(self.at < note.last_phase) << 2
                    | u64::from(self.at <= note.last) << 3 | u64::from(self.at >= note.auto_frame) << 4 | u64::from(self.at >= note.forced_frame) << 5]);
            for other in &self.notes[..i] {
                key.extend([note.time.cmp(&other.time) as i8 as u64,
                    (i64::from(self.at)-i64::from(note.time)).abs().cmp(&(i64::from(self.at)-i64::from(other.time)).abs()) as i8 as u64]);
            }
        }
        for &input in atoms.iter() {
            let effect = atom_key(&self.notes,input,self.at);
            key.push(effect.len() as u64);
            key.extend(effect.into_iter().map(u64::from));
        }
        let cached = self.atom_cache.lock().unwrap().languages.get(&key).cloned();
        self.language_key = Some(key);
        if let Some(programs) = cached {
            let mut outcomes = Vec::new();
            for (word,signature) in programs {
                let inputs: Vec<_> = word.into_iter().map(|index| atoms[index]).collect();
                let mut state = self.initial.clone();
                let judged = frame::run(&self.notes,self.at,&inputs,&mut state);
                if judged.iter().any(|j| !self.allowed[j.note] || j.grade != self.notes[j.note].grade)
                    || self.due.iter().enumerate().any(|(i,&due)| due && !state.done[i])
                    || reservation_key(&self.notes,&state,self.free) != signature {
                    return Err("native input language projection mismatch".into());
                }
                outcomes.push(Outcome { state,inputs });
            }
            self.warm = outcomes.iter().filter(|outcome| self.found.insert(reservation_key(&self.notes,&outcome.state,self.free))).cloned().collect();
            self.solutions = outcomes;
            self.exhausted = true;
            return Ok(true);
        }
        Ok(false)
    }

    fn remember_language(&self) {
        let Some(key) = &self.language_key else { return; };
        if self.found.len() != self.solutions.len() { return; }
        let atoms = self.atoms.as_ref().unwrap();
        let mut programs = Vec::new();
        for outcome in &self.solutions {
            let mut word = Vec::new();
            for input in &outcome.inputs {
                let Some(index) = atoms.iter().position(|atom| atom == input) else { return; };
                word.push(index);
            }
            programs.push((word,reservation_key(&self.notes,&outcome.state,self.free)));
        }
        let bytes = key.len()*8 + programs.iter().map(|(word,signature)|word.len()*8+signature.len()+48).sum::<usize>();
        const LIMIT: usize = 4*1_048_576;
        if bytes > LIMIT { return; }
        let mut cache = self.atom_cache.lock().unwrap();
        if cache.language_bytes+bytes > LIMIT { cache.languages.clear();cache.language_bytes=0; }
        cache.languages.insert(key.clone(),programs);
        cache.language_bytes+=bytes;
    }

    fn output_limit(&self) -> usize {
        let flicks: Vec<_> = self.notes.iter().enumerate().filter(|(i, note)| note.flick() && !self.automatic.done[*i]).map(|(i, _)| i).collect();
        let masks: Vec<_> = flicks.iter().map(|&i| reservation_mask(&self.notes[i], self.at, self.clock, self.initial.flicks[i], self.early_auto[i], self.auto_grade[i])).collect();
        let scalar = self.notes.iter().enumerate().filter(|(i, note)| !note.flick() && !self.automatic.done[*i] && !self.due[*i] && self.free && self.allowed[*i])
            .fold(1usize, |limit, _| limit.saturating_mul(2));
        if flicks.len() > 8 {
            return flicks.iter().enumerate().fold(scalar, |limit, (position, &i)| {
                let states = if self.due[i] { 1 } else { masks[position].count_ones() as usize + usize::from(self.free && self.allowed[i]) };
                limit.saturating_mul(states)
            });
        }
        let mut suppressors = vec![[0u16; 2]; flicks.len()];
        let times: Vec<_> = flicks.iter().map(|&i| {
            let mut masks = [0u64; 2];
            for (position, time) in input_times(self.clock, self.at).enumerate() {
                if frame::permitted_time(&self.notes[i], time) {
                    masks[usize::from(self.notes[i].grade_at(time) != self.notes[i].grade)] |= 1 << position;
                }
            }
            masks
        }).collect();
        for (position, &i) in flicks.iter().enumerate().filter(|(_, i)| self.at >= self.notes[**i].time) {
            for (winner, &j) in flicks.iter().enumerate() {
                if !self.allowed[j] || i == j { continue; }
                let a = ((i64::from(self.at) - i64::from(self.notes[j].time)).abs(), j);
                let b = ((i64::from(self.at) - i64::from(self.notes[i].time)).abs(), i);
                if a >= b || self.notes[i].left > self.notes[j].right || self.notes[j].left > self.notes[i].right { continue; }
                for (class, &possible) in times[position].iter().enumerate() {
                    if possible & times[winner][0] != 0 { suppressors[position][class] |= 1 << winner; }
                }
            }
        }
        let mut total = 0usize;
        for done in 0..(1u16 << flicks.len()) {
            if flicks.iter().enumerate().any(|(position, &i)| {
                let graded = done & (1 << position) != 0;
                self.due[i] && !graded || graded && !self.allowed[i] || !self.free && graded != self.due[i]
            }) { continue; }
            let mut count = scalar;
            let mut possible_masks = masks.clone();
            for (position, &i) in flicks.iter().enumerate() {
                if done & (1 << position) != 0 { continue; }
                let mut mask = masks[position];
                // A post-time reservation can survive only as a losing flick
                // candidate; its higher-priority winner is graded in this frame.
                if self.at >= self.notes[i].time {
                    for (class, &winners) in suppressors[position].iter().enumerate() {
                        if winners & done == 0 { mask &= !(1 << (class + 2)); }
                    }
                }
                possible_masks[position] = mask;
                count = count.saturating_mul(mask.count_ones() as usize);
            }
            if flicks.len() <= 6 {
                let bound = FlickSignatureBound { notes: &self.notes, flicks: &flicks, masks: &possible_masks, times: &times,
                    suppressors: &suppressors, done, at: self.at };
                count = scalar.saturating_mul(bound.count(0, &mut Vec::new()));
            }
            total = total.saturating_add(count);
        }
        total
    }

    fn reservation_outcomes(&self) -> Result<Option<Vec<Outcome>>, String> {
        let indices: Vec<_> = self.notes.iter().enumerate().filter(|(i, _)| !self.automatic.done[*i]).map(|(i, _)| i).collect();
        if indices.len() > 20 || indices.iter().any(|&i| !self.notes[i].flick()
            || self.due[i] || self.free && self.allowed[i]) { return Ok(None); }
        let active: Vec<_> = indices.iter().map(|&i| self.notes[i].clone()).collect();
        let atoms = cached_atoms(&active, self.at, self.clock, true, &self.atom_cache);
        let mut unique = HashSet::default();
        let transitions: Vec<_> = atoms.iter().copied().filter_map(|input| {
            if input.state != TouchState::Hold || input.flick_begin.is_none() { return None; }
            let mut mask = 0u64;
            let mut value = 0u64;
            for (position, &i) in indices.iter().enumerate() {
                let note = &self.notes[i];
                if !frame::in_area(note, input.lane) && !frame::in_area(note, input.flick_begin.unwrap()) { continue; }
                let saved = if frame::permitted_time(note, input.time) {
                    Some(frame::ReservedFlick { time: input.time, begin: input.flick_begin.unwrap() })
                } else { None };
                mask |= 7 << (position * 3);
                value |= u64::from(4 | reserved_class(note, saved)) << (position * 3);
            }
            unique.insert((mask, value)).then_some((mask, value, input))
        }).collect();
        let initial = 0;
        let empty = State { done: vec![false; active.len()], flicks: vec![None; active.len()] };
        let mut cache_key = solution_key(&active, self.at, self.clock, &empty, &vec![false; active.len()], &vec![false; active.len()], &vec![None; active.len()]);
        cache_key[0] = 2;
        let cached = self.atom_cache.lock().unwrap().solutions.get(&cache_key).map(|(outcomes, complete)| {
            debug_assert!(*complete);
            outcomes.iter().map(|outcome| shift_outcome(outcome, self.at)).collect::<Vec<_>>()
        });
        let apply = |programs: Vec<Outcome>| -> Result<Vec<Outcome>, String> {
            let mut seen = HashSet::default();
            let mut outcomes = Vec::new();
            for program in programs {
                let mut state = self.initial.clone();
                let judged = frame::run(&self.notes, self.at, &program.inputs, &mut state);
                if judged.iter().any(|j| !self.allowed[j.note] || j.grade != self.notes[j.note].grade) {
                    return Err("native flick reservation projection mismatch".into());
                }
                if self.notes.iter().enumerate().any(|(i,note)| state.flicks[i].is_some_and(|saved| frame::in_area(note,saved.begin)
                    && (self.early_auto[i] || self.auto_grade[i].is_some_and(|grade| note.grade_at(saved.time) != grade)))) { continue; }
                if seen.insert(reservation_key(&self.notes,&state,self.free)) { outcomes.push(Outcome { state, inputs: program.inputs }); }
            }
            Ok(outcomes)
        };
        if let Some(cached) = cached { return apply(cached).map(Some); }
        let mut programs = vec![(initial, usize::MAX, usize::MAX, 0u8)];
        let mut seen = HashSet::default();
        seen.insert(initial);
        let mut found = HashSet::default();
        let mut outcomes = Vec::new();
        let mut cursor = 0;
        while cursor < programs.len() {
            if cursor.is_multiple_of(1024) {
                crate::clock::milliseconds();
                if crate::cancellation_generation() != self.generation { return Err("cancelled".into()); }
            }
            let (key, _, _, depth) = programs[cursor];
            let signature = (0..indices.len()).fold(0u64, |signature, position| signature
                | ((key >> (position * 3) & 3) << (position * 2)));
            if found.insert(signature) {
                let mut word = Vec::new();
                let mut step = cursor;
                while programs[step].1 != usize::MAX {
                    word.push(transitions[programs[step].2].2);
                    step = programs[step].1;
                }
                word.reverse();
                let mut state = empty.clone();
                let judged = frame::run(&active, self.at, &word, &mut state);
                if !judged.is_empty() || active.iter().enumerate().any(|(position, note)|
                    u64::from(reserved_class(note, state.flicks[position])) != (key >> (position * 3) & 3)) {
                    return Err("native flick reservation projection mismatch".into());
                }
                outcomes.push(Outcome { state, inputs: word });
            }
            if depth < 24 {
                let locks = (0..indices.len()).fold(0u64, |locks, position| if key & (4 << (position * 3)) == 0 {
                    locks | (7 << (position * 3))
                } else { locks });
                for (index, &(mask, value, _)) in transitions.iter().enumerate() {
                    let changed = mask & locks;
                    let next = (key & !changed) | (value & changed);
                    if indices.iter().enumerate().any(|(position, &i)| self.at >= self.notes[i].time && (next >> (position * 3) & 3) >= 2) { continue; }
                    if seen.insert(next) {
                        programs.push((next, cursor, index, depth + 1));
                        if programs.len() > 65_536 { return Ok(None); }
                    }
                }
            }
            cursor += 1;
        }
        save_solutions(&self.atom_cache, &cache_key, &outcomes, true, self.at);
        apply(outcomes).map(Some)
    }

    // Without flick judgements, Hold gestures can precede Enter units: flicks
    // retain their first gesture, and taps select Enter units. Reservation
    // classes then determine which flicks can claim those Enter units.
    fn tap_flick_outcomes(&self) -> Result<Option<Vec<Outcome>>, String> {
        let active: Vec<_> = self.notes.iter().enumerate().filter(|(i,_)| !self.automatic.done[*i]).map(|(i,_)| i).collect();
        let flicks: Vec<_> = active.iter().copied().filter(|&i| self.notes[i].flick()).collect();
        if flicks.is_empty() || flicks.len() == active.len() || active.iter().any(|&i| self.notes[i].asynchronous())
            || flicks.iter().any(|&i| self.due[i] || self.free && self.allowed[i]) { return Ok(None); }
        let notes: Vec<_> = flicks.iter().map(|&i| self.notes[i].clone()).collect();
        let initial = State { done: vec![false;flicks.len()],flicks:flicks.iter().map(|&i|self.initial.flicks[i]).collect() };
        let mut reservations = Self::new(notes,self.at,vec![i32::MAX;flicks.len()],initial,self.clock,Arc::clone(&self.atom_cache),None);
        reservations.early_auto = flicks.iter().map(|&i|self.early_auto[i]).collect();
        reservations.auto_grade = flicks.iter().map(|&i|self.auto_grade[i]).collect();
        let Some(reservations) = reservations.reservation_outcomes()? else { return Ok(None); };
        let notes: Vec<_> = active.iter().map(|&i|self.notes[i].clone()).collect();
        let enters: Arc<Vec<_>> = Arc::new(cached_atoms(&notes,self.at,self.clock,false,&self.atom_cache).iter().copied().filter(|input| input.state == TouchState::Enter).collect());
        let mut found = HashSet::default();
        let mut outcomes = Vec::new();
        for reservation in reservations {
            let mut initial = self.initial.clone();
            for (position,&i) in flicks.iter().enumerate() { initial.flicks[i] = reservation.state.flicks[position]; }
            let mut taps = Self::new(self.notes.clone(),self.at,self.due.iter().map(|&due|if due {self.at}else{i32::MAX}).collect(),initial,self.clock,Arc::clone(&self.atom_cache),None);
            taps.free = self.free;
            taps.allowed = self.allowed.clone();
            taps.early_auto = self.early_auto.clone();
            taps.auto_grade = self.auto_grade.clone();
            taps.solution_key = solution_key(&taps.notes,taps.at,taps.clock,&taps.initial,&taps.due,&taps.early_auto,&taps.auto_grade);
            taps.solution_key[0] = 3;
            taps.solution_key.push(u64::from(taps.free));
            taps.solution_key.extend(taps.allowed.iter().map(|&allowed|u64::from(allowed)));
            taps.warm.clear();taps.solutions.clear();taps.found.clear();
            taps.exhausted = false;
            taps.async_checked = true;
            taps.atoms = Some(Arc::clone(&enters));
            taps.reservation_limit = if self.free { active.iter().filter(|&&i| !self.notes[i].flick() && self.allowed[i] && !self.due[i]).fold(1usize,|value,_|value.saturating_mul(2)) } else { 1 };
            taps.limit_ready = true;
            taps.final_state = !self.free;
            loop {
                let outcome = match taps.next() {
                    Ok(Some(outcome)) => outcome,
                    Ok(None) => break,
                    Err(error) if error.starts_with("native input word search budget exceeded") => return Ok(None),
                    Err(error) => return Err(error),
                };
                let mut inputs = reservation.inputs.clone();
                inputs.extend(outcome.inputs);
                if inputs.len() > 24 { return Ok(None); }
                let actual = verified_outcome(&self.notes,self.at,&self.initial,&inputs,&outcome.state)?;
                if found.insert(reservation_key(&self.notes,&actual.state,self.free)) { outcomes.push(actual); }
            }
        }
        Ok(Some(outcomes))
    }

    fn asynchronous_outcomes(&self) -> Result<Option<Vec<Outcome>>, String> {
        let indices: Vec<_> = self.notes.iter().enumerate().filter(|(i, _)| !self.automatic.done[*i]).map(|(i, _)| i).collect();
        if indices.len() > 12 || indices.iter().any(|&i| !self.notes[i].asynchronous()) { return Ok(None); }
        let active: Vec<_> = indices.iter().map(|&i| self.notes[i].clone()).collect();
        let atoms = cached_atoms(&active, self.at, self.clock, false, &self.atom_cache);
        let mut seen_atoms = HashSet::default();
        let transitions: Vec<_> = atoms.iter().copied().filter_map(|input| {
            let hold = input.state == TouchState::Hold;
            let values: Vec<_> = active.iter().map(|note| if frame::in_area(note, input.lane) {
                match asynchronous_grade(note, self.at, input) { None => 1, Some(grade) if grade == note.grade => 2, Some(_) => 3 }
            } else { 0 }).collect();
            let mut key = vec![u8::from(hold)];
            key.extend_from_slice(&values);
            seen_atoms.insert(key).then_some((hold, values, input))
        }).collect();
        let maximum = if self.free { 1usize << indices.iter().filter(|&&i| self.allowed[i] && !self.due[i]).count() } else { 1 };
        let mut programs = vec![(0u64, usize::MAX, usize::MAX, 0u8)];
        let mut seen = HashSet::default();
        seen.insert(0u64);
        let mut produced = HashSet::default();
        let mut outcomes = Vec::new();
        let mut cursor = 0;
        while cursor < programs.len() {
            if cursor % 1024 == 0 {
                crate::clock::milliseconds();
                if crate::cancellation_generation() != self.generation { return Err("cancelled".into()); }
            }
            let (state, _, _, depth) = programs[cursor];
            let mut mask = 0u16;
            let valid = indices.iter().enumerate().all(|(position, &i)| {
                let value = (state >> (position * 2)) & 3;
                let done = value == 2;
                if done { mask |= 1 << position; }
                value != 3 && (!done || self.allowed[i]) && (!self.due[i] || done)
            });
            if valid && produced.insert(mask) {
                let mut word = Vec::new();
                let mut parent = cursor;
                while programs[parent].1 != usize::MAX {
                    word.push(transitions[programs[parent].2].2);
                    parent = programs[parent].1;
                }
                word.reverse();
                let mut trial = self.initial.clone();
                let judged = frame::run(&self.notes, self.at, &word, &mut trial);
                if judged.iter().any(|j| !self.allowed[j.note] || j.grade != self.notes[j.note].grade)
                    || self.due.iter().enumerate().any(|(i, &due)| due && !trial.done[i])
                    || indices.iter().enumerate().any(|(position, &i)| trial.done[i] != (mask & (1 << position) != 0)) {
                    return Err("native asynchronous input projection mismatch".into());
                }
                outcomes.push(Outcome { state: trial, inputs: word });
                if outcomes.len() == maximum { return Ok(Some(outcomes)); }
            }
            if depth < 24 {
                for (atom, (hold, values, _)) in transitions.iter().enumerate() {
                    let mut next = state;
                    for (position, &value) in values.iter().enumerate() {
                        if value != 0 && (*hold || (state >> (position * 2)) & 3 == 0) {
                            next = (next & !(3 << (position * 2))) | ((value as u64) << (position * 2));
                        }
                    }
                    if seen.insert(next) { programs.push((next, cursor, atom, depth + 1)); }
                }
            }
            if programs.len() > 65_536 { return Ok(None); }
            cursor += 1;
        }
        Ok(Some(outcomes))
    }

    fn expand(&mut self, word: &[usize]) -> Result<Option<Outcome>, String> {
        if word.len() == 24 {
            return Ok(None);
        }
        let atoms = self
            .atoms
            .get_or_insert_with(|| {
                let active: Vec<_> = self.notes.iter().enumerate().filter(|(i, _)| !self.automatic.done[*i]).map(|(_, note)| note.clone()).collect();
                cached_atoms(&active, self.at, self.clock, self.flicks_only, &self.atom_cache)
            });
        self.inputs.clear();
        self.inputs.extend(word.iter().map(|&index| atoms[index]));
        self.base_selected.clear();
        self.base_selected.extend(self.notes.iter().enumerate().map(|(i, note)| {
            if self.automatic.done[i] { None } else { frame::input_for(note, &self.inputs) }
        }));
        for index in 0..atoms.len() {
            self.candidate.clear();
            self.candidate.extend_from_slice(word);
            self.candidate.push(index);
            compact(&self.notes, &self.automatic, &self.claims, atoms, &self.candidate, (self.at, &self.base_selected), &mut self.compact);
            if self.seen.contains(self.compact.key.as_slice()) {
                continue;
            }
            // Reservation updates depend on the selected gesture, before any
            // candidate competition. Check the next automatic judgement first.
            if self.compact.reservations.iter().any(|&(i,state)| self.early_auto[i] || self.auto_grade[i].is_some() && state == 3) { continue; }
            std::mem::swap(&mut self.inputs, &mut self.compact.inputs);
            self.trial.clone_from(&self.initial);
            let judged = frame::run_selected(&self.notes, self.at, &self.inputs, &self.compact.selected, &mut self.trial, &mut self.frame_scratch);
            if irreversible(
                &self.notes,
                self.at,
                (&self.due, self.free, &self.allowed),
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
                let outcome = verified_outcome(&self.notes, self.at, &self.initial, &self.inputs, &self.trial)?;
                self.solutions.push(outcome.clone());
                self.remember_language();
                save_solutions(&self.atom_cache, &self.solution_key, &self.solutions, true, self.at);
                return Ok(Some(outcome));
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
                .filter(|j| (if self.free { self.allowed[j.note] } else { self.due[j.note] }) && j.grade == self.notes[j.note].grade)
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
        if let Some(outcome) = self.warm.pop_front() { return Ok(Some(outcome)); }
        if self.exhausted {
            return Ok(None);
        }
        if !self.async_checked {
            self.async_checked = true;
            if let Some(outcomes) = self.reservation_outcomes()?.or(self.asynchronous_outcomes()?).or(self.tap_flick_outcomes()?) {
                save_solutions(&self.atom_cache, &self.solution_key, &outcomes, true, self.at);
                self.warm = outcomes.into_iter().filter(|outcome| self.found.insert(reservation_key(&self.notes, &outcome.state, self.free))).collect();
                self.exhausted = true;
                return Ok(self.warm.pop_front());
            }
        }
        if self.language_key.is_none() && self.language()? { return Ok(self.warm.pop_front()); }
        if !self.limit_ready {
            self.reservation_limit = self.output_limit();
            self.limit_ready = true;
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
            if (self.free || judged.len() == self.due.iter().filter(|&&due| due).count())
                && judged
                    .iter()
                    .all(|j| (if self.free { self.allowed[j.note] } else { self.due[j.note] }) && j.grade == self.notes[j.note].grade)
                && (!self.free || self.due.iter().enumerate().all(|(i, &required)| !required || self.trial.done[i]))
                && !self.notes.iter().enumerate().any(|(i, note)| self.trial.flicks[i].is_some_and(|saved| frame::in_area(note, saved.begin)
                    && (self.early_auto[i] || self.auto_grade[i].is_some_and(|grade| note.grade_at(saved.time) != grade))))
                && self.found.insert(reservation_key(&self.notes, &self.trial, self.free))
            {
                if self.found.len() == self.reservation_limit {
                    self.exhausted = true;
                } else {
                    self.pending = Some(word);
                }
                let outcome = verified_outcome(&self.notes, self.at, &self.initial, &self.inputs, &self.trial)?;
                self.solutions.push(outcome.clone());
                if self.exhausted { self.remember_language(); }
                if self.exhausted || self.solutions.len() <= 8 { save_solutions(&self.atom_cache, &self.solution_key, &self.solutions, self.exhausted && self.found.len() == self.solutions.len(), self.at); }
                return Ok(Some(outcome));
            }
            if let Some(outcome) = self.expand(&word)? { return Ok(Some(outcome)); }
        }
        self.exhausted = true;
        self.remember_language();
        save_solutions(&self.atom_cache, &self.solution_key, &self.solutions, self.found.len() == self.solutions.len(), self.at);
        Ok(None)
    }
}
