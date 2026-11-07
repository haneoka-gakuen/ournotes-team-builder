//! Exact frame-state optimization; equal future states retain the better score prefix.
use super::{frame, search, InputTiming};
use rustc_hash::FxHashMap as HashMap;
use std::{collections::{BTreeMap, BTreeSet}, sync::Arc};

pub(crate) trait FrameObjective {
    type State: Clone;
    fn initial(&self) -> Self::State;
    fn span(&self) -> (i32, i32);
    fn advance(&mut self, state: &Self::State, time: i32, judged: &[usize]) -> Self::State;
    fn key(&self, state: &Self::State) -> [u64; 5];
    fn score(&self, state: &Self::State) -> f64;
    fn dominates(&self, left: &Self::State, right: &Self::State, best: bool) -> bool;
    fn settled(&self, state: &Self::State) -> bool;
}

struct Step {
    previous: Option<Arc<Step>>,
    time: i32,
    notes: Vec<usize>,
    inputs: Arc<[frame::Touch]>,
}

#[derive(Clone)]
struct Node<S> {
    value: S,
    input: Arc<frame::State>,
    history: Option<Arc<Step>>,
}
type Frontier<S> = HashMap<(u128, [u64; 5]), Vec<Node<S>>>;

impl InputTiming {
    fn certificate<O: FrameObjective>(&self, objective: &mut O, best: bool, states: &Frontier<O::State>, context: (&[usize], &[i32], &[i32], bool)) -> Result<Option<(f64, Vec<i32>)>, String> {
        let (region, baseline, frames, constructed) = context;
        let last = *frames.last().unwrap();
        let mut chosen = Vec::new();
        let mut extreme = if best { f64::NEG_INFINITY } else { f64::INFINITY };
        for node in states.values().flatten() {
            let value = objective.advance(&node.value, i32::MAX, &[]);
            let score = objective.score(&value);
            if if best { score > extreme } else { score < extreme } { extreme = score; chosen.clear(); }
            if score == extreme && chosen.len() < 8 { chosen.push(node.history.clone()); }
        }
        for mut history in chosen {
            let mut selected = baseline.to_vec();
            let mut assigned = vec![false; selected.len()];
            let mut inputs = BTreeMap::<i32, Vec<frame::Touch>>::new();
            while let Some(step) = history {
                for &note in &step.notes { selected[note] = step.time; assigned[note] = true; }
                if step.time >= frames[0] { inputs.entry(step.time).or_default().extend_from_slice(&step.inputs); }
                history = step.previous.clone();
            }
            if constructed {
                let notes: Vec<_> = region.iter().map(|&i| self.notes[i].clone()).collect();
                let mut state = frame::State { done: vec![false; notes.len()], flicks: vec![None; notes.len()] };
                for &at in frames {
                    let judged = frame::run(&notes, at, inputs.get(&at).map_or(&[], Vec::as_slice), &mut state);
                    if judged.iter().any(|j| j.grade != notes[j.note].grade || selected[region[j.note]] != at) {
                        return Err("native frame optimization replay mismatch".into());
                    }
                }
                if state.done.iter().any(|&done| !done) { return Err("native frame optimization incomplete replay".into()); }
                self.remember_verified(&selected)?;
                return Ok(Some((extreme, selected)));
            }
            let mut possible = true;
            for &i in region {
                if assigned[i] { continue; }
                if let Some(at) = self.frames[i].iter().copied().filter(|&at| at > last)
                    .min_by_key(|&at| (i64::from(at) - i64::from(baseline[i])).abs()) { selected[i] = at; }
                else { possible = false; break; }
            }
            if !possible { continue; }
            match self.verify_program(&selected, &inputs) {
                Ok(true) => return Ok(Some((extreme, selected))),
                Err(error) if error == "cancelled" => return Err(error),
                _ => continue,
            }
        }
        Ok(None)
    }
    pub(crate) fn optimize<O: FrameObjective>(&self, objective: &mut O, best: bool) -> Result<Option<(f64, Vec<i32>)>, String> {
        if let Some(value) = self.optimize_mode(objective, best, true)? { return Ok(Some(value)); }
        self.optimize_mode(objective, best, false)
    }

    fn optimize_mode<O: FrameObjective>(&self, objective: &mut O, best: bool, relaxed: bool) -> Result<Option<(f64, Vec<i32>)>, String> {
        let baseline = self.witnesses().pop();
        let mut region: Vec<_> = (0..self.notes.len()).collect();
        let (window_start, window_end) = objective.span();
        let padding = self.notes.iter().enumerate().map(|(i, note)| self.frames[i].last().unwrap().saturating_sub(note.first)).max().unwrap_or(0);
        let cut = self.clock.frame_time(window_start.saturating_sub(padding.saturating_mul(2)));
        if relaxed {
            region.retain(|&i| self.notes[i].first <= window_end && *self.frames[i].last().unwrap() >= cut);
        } else if baseline.is_some() {
            let mut sorted = region.clone();
            sorted.sort_by_key(|&i| self.notes[i].first);
            let (start, end) = objective.span();
            region.clear();
            let mut group = Vec::new();
            let mut group_start = 0;
            let mut group_end = -1;
            for i in sorted.into_iter().chain(std::iter::once(usize::MAX)) {
                if i == usize::MAX || self.notes[i].first > group_end {
                    if group_start <= end && group_end >= start { region.append(&mut group); }
                    group.clear();
                    if i == usize::MAX { break; }
                    group_start = self.notes[i].first;
                    group_end = -1;
                }
                group_end = group_end.max(*self.frames[i].last().unwrap());
                group.push(i);
            }
        }
        region.sort_unstable();
        let selected_base = baseline.unwrap_or_else(|| self.notes.iter().map(|note| self.clock.frame_time(note.time)).collect::<Vec<_>>());
        if region.is_empty() {
            let value = objective.advance(&objective.initial(), i32::MAX, &[]);
            return Ok(Some((objective.score(&value), selected_base)));
        }
        let mut frames = BTreeSet::new();
        for &i in &region {
            frames.extend(self.frames[i].iter().copied());
            if self.notes[i].flick() {
                let mut at = self.notes[i].first;
                while at <= *self.frames[i].last().unwrap() {
                    frames.insert(at);
                    at = self.clock.frame_time(at.saturating_add(1));
                }
            }
        }
        if relaxed {
            frames.retain(|&at| cut <= at && at <= window_end);
            frames.insert(cut);
        }
        let frames: Vec<_> = frames.into_iter().collect();
        let value = objective.advance(&objective.initial(), (frames[0] - 1).min(window_start - 1), &[]);
        let initial = Node { value, input: Arc::new(frame::State { done: Vec::new(), flicks: Vec::new() }), history: None };
        let mut previous = if relaxed { region.iter().copied().filter(|&i| self.notes[i].first <= cut && cut <= *self.frames[i].last().unwrap()).collect() } else { Vec::<usize>::new() };
        let mut roots = vec![initial];
        for &i in &previous {
            let note = &self.notes[i];
            let mut options = vec![(false, None)];
            if note.flick() {
                options.push((false, Some(frame::ReservedFlick { time: note.time, begin: note.left - 1.0 })));
                options.push((false, Some(frame::ReservedFlick { time: note.time, begin: note.left })));
            }
            let before = self.frames[i].iter().copied().filter(|&at| at < cut)
                .min_by_key(|&at| (i64::from(at) - i64::from(selected_base[i])).abs());
            if before.is_some() { options.push((true, None)); }
            let mut next = Vec::new();
            for root in roots {
                for &(done, saved) in &options {
                    let mut node = root.clone();
                    Arc::make_mut(&mut node.input).done.push(done);
                    Arc::make_mut(&mut node.input).flicks.push(saved);
                    if done { node.history = Some(Arc::new(Step { previous: node.history, time: before.unwrap(), notes: vec![i], inputs: Arc::from([]) })); }
                    next.push(node);
                }
            }
            if next.len() > 32_768 { return Ok(None); }
            roots = next;
        }
        let mut states = HashMap::default();
        for root in roots {
            if previous.len() > 32 { return Ok(None); }
            let physical = previous.iter().enumerate().fold(0u128, |key, (position, &i)| key
                | (u128::from(if root.input.done[position] { 8 } else { search::reserved_class(&self.notes[i], root.input.flicks[position]) }) << (position * 4)));
            states.entry((physical, objective.key(&root.value))).or_insert_with(Vec::new).push(root);
        }
        let mut old_positions = vec![usize::MAX; self.notes.len()];
        let generation = crate::cancellation_generation();
        let mut tried_settled = false;
        for (step, &at) in frames.iter().enumerate() {
            crate::clock::milliseconds();
            if generation != crate::cancellation_generation() { return Err("cancelled".into()); }
            let indices: Vec<_> = region.iter().copied().filter(|&i| self.notes[i].first <= at && at <= *self.frames[i].last().unwrap()).collect();
            if indices.len() > 32 { return Ok(None); }
            for (position, &i) in previous.iter().enumerate() { old_positions[i] = position; }
            let allowed: Vec<_> = indices.iter().map(|&i| self.frames[i].binary_search(&at).is_ok()).collect();
            let required: Vec<_> = indices.iter().map(|&i| self.frames[i].last() == Some(&at)).collect();
            let mut next = HashMap::default();
            let mut next_count = 0usize;
            let mut physical_groups = HashMap::<u128, Vec<Node<O::State>>>::default();
            for ((physical, _), nodes) in states.drain() { physical_groups.entry(physical).or_default().extend(nodes); }
            for nodes in physical_groups.into_values() {
                let node = &nodes[0];
                let input = frame::State {
                    done: indices.iter().map(|&i| old_positions[i] != usize::MAX && node.input.done[old_positions[i]]).collect(),
                    flicks: indices.iter().map(|&i| if old_positions[i] == usize::MAX { None } else { node.input.flicks[old_positions[i]] }).collect(),
                };
                let mut search = search::Search::flexible(indices.iter().map(|&i| self.notes[i].clone()).collect(), at,
                    (required.clone(), allowed.clone()), input.clone(), self.clock, Arc::clone(&self.atoms), frames.get(step + 1).copied());
                loop {
                    let outcome = match search.next() {
                        Ok(Some(outcome)) => outcome,
                        Ok(None) => break,
                        Err(error) if error == "cancelled" => return Err(error),
                        Err(error) if error.starts_with("native input word search budget exceeded") => return Ok(None),
                        Err(error) => return Err(error),
                    };
                    let judged: Vec<_> = indices.iter().enumerate().filter(|(position, _)| !input.done[*position] && outcome.state.done[*position]).map(|(_, &i)| i).collect();
                    let physical = indices.iter().enumerate().fold(0u128, |key, (position, &i)| key
                        | (u128::from(if outcome.state.done[position] { 8 } else { search::reserved_class(&self.notes[i], outcome.state.flicks[position]) }) << (position * 4)));
                    let input = Arc::new(outcome.state);
                    let inputs: Arc<[frame::Touch]> = outcome.inputs.into();
                    for node in &nodes {
                        let value = objective.advance(&node.value, at, &judged);
                        let key = (physical, objective.key(&value));
                        let group = next.entry(key).or_insert_with(Vec::<Node<O::State>>::new);
                        if !group.iter().any(|old| objective.dominates(&old.value, &value, best)) {
                            let before = group.len();
                            group.retain(|old| !objective.dominates(&value, &old.value, best));
                            next_count -= before - group.len();
                            let history = if judged.is_empty() && inputs.is_empty() { node.history.clone() }
                                else { Some(Arc::new(Step { previous: node.history.clone(), time: at, notes: judged.clone(), inputs: Arc::clone(&inputs) })) };
                            group.push(Node { value, input: Arc::clone(&input), history });
                            next_count += 1;
                        }
                        if next_count > 32_768 { return Ok(None); }
                    }
                }
            }
            for &i in &previous { old_positions[i] = usize::MAX; }
            if next.is_empty() { return if relaxed { Ok(None) } else { Err("unreachable native input sequence".into()) }; }
            if !tried_settled && next.values().flatten().all(|node| objective.settled(&node.value)) {
                tried_settled = true;
                if let Some(value) = self.certificate(objective, best, &next, (&region, &selected_base, &frames[..=step], false))? { return Ok(Some(value)); }
            }
            states = next;
            previous = indices;
        }
        self.certificate(objective, best, &states, (&region, &selected_base, &frames, !relaxed))
    }
}
