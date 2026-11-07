//! Single-counter AP timing over native frame states and the original score ledger.
use super::{commands, f, orders, ApScorer, Command, Cumulative, Delivery, Input};
use crate::input_timing::{FrameObjective, InputTiming};

#[derive(Clone)]
struct State {
    general: f64,
    perfect: f64,
    count: i32,
    applied: i32,
    ended: bool,
    note: usize,
    command: usize,
    score: f64,
}

struct Ledger<'a> {
    input: &'a Input,
    scorer: ApScorer<'a>,
    commands: Vec<Command>,
    factor: Cumulative,
    owner: usize,
    start: i32,
    activation: i32,
    end: i32,
    limit: i32,
}

impl Ledger<'_> {
    fn notes_before(&mut self, state: &mut State, time: i32, inclusive: bool) {
        let end = self.input.times.partition_point(|&at| at < time || inclusive && at == time);
        if end > state.note {
            state.score += self.scorer.sum(state.note, end, f(state.general + state.perfect));
            state.note = end;
        }
    }

    fn delta(state: &mut State, channel: usize, delta: i32) {
        let delta = f(delta as f64 / 100000.0);
        if channel == 0 { state.general = f(state.general + delta); }
        else if channel == 5 { state.perfect = f(state.perfect + delta); }
    }

    fn statics_before(&mut self, state: &mut State, time: i32, owner: usize) {
        while let Some(command) = self.commands.get(state.command) {
            if (command.time, command.owner) > (time, owner) { break; }
            let command = command.clone();
            self.notes_before(state, command.time, false);
            Self::delta(state, command.channel, command.delta);
            state.command += 1;
        }
    }

    fn counter(&mut self, state: &mut State, time: i32, next: Option<i32>) {
        self.statics_before(state, time, self.owner);
        self.notes_before(state, time, false);
        Self::delta(state, 0, state.applied.wrapping_neg());
        if let Some(next) = next { Self::delta(state, 0, next); }
        state.applied = next.unwrap_or(0);
    }
}

impl FrameObjective for Ledger<'_> {
    type State = State;

    fn initial(&self) -> State {
        State { general: f(1.0), perfect: f(0.0), count: 0, applied: 0, ended: false, note: 0, command: 0, score: 0.0 }
    }

    fn span(&self) -> (i32, i32) { (self.start, self.end) }

    fn advance(&mut self, previous: &State, time: i32, judged: &[usize]) -> State {
        let mut state = previous.clone();
        if !state.ended && time >= self.activation && time <= self.end {
            state.count = (state.count + judged.len() as i32).min(self.limit);
            let next = self.factor.mill(state.count);
            if next != state.applied {
                self.counter(&mut state, if time == self.activation { self.start } else { time }, Some(next));
            }
        }
        if !state.ended && time >= self.end {
            if state.applied != 0 { self.counter(&mut state, self.end, None); }
            state.ended = true;
            state.count = 0;
        }
        self.statics_before(&mut state, time, usize::MAX);
        self.notes_before(&mut state, time, true);
        state
    }

    fn key(&self, state: &State) -> [u64; 5] {
        [state.count as u64, state.applied as u64, u64::from(state.ended), state.note as u64, state.command as u64]
    }

    fn score(&self, state: &State) -> f64 { state.score }

    fn dominates(&self, left: &State, right: &State, best: bool) -> bool {
        if best { left.score >= right.score && left.general >= right.general && left.perfect >= right.perfect }
        else { left.score <= right.score && left.general <= right.general && left.perfect <= right.perfect }
    }

    fn settled(&self, state: &State) -> bool { state.ended || state.count == self.limit }
}

pub(super) fn score(input: &Input, timing: &InputTiming, order_index: usize, best: bool) -> Result<Option<f64>, String> {
    if input.judgements.is_some() || input.times.len() > 4096 { return Ok(None); }
    let factors: Vec<_> = input.slots.iter().enumerate().flat_map(|(slot, value)| value.cumulative.iter().map(move |factor| (slot, factor))).collect();
    if factors.len() != 1 { return Ok(None); }
    let (slot, factor) = factors[0];
    let Some(limit) = factor.cap.checked_mul(factor.unit).filter(|&limit| (1..=64).contains(&limit)) else { return Ok(None); };
    if !factor.judgements.contains(&5) { return Ok(None); }
    let order = &orders()[order_index];
    let Some(owner) = order.iter().position(|&position| position == slot).filter(|&event| event < input.skill_times.len()) else { return Ok(None); };
    let start = input.skill_times[owner];
    let Some(end) = start.checked_add(factor.duration_ms()) else { return Ok(None); };
    let mut plain = input.clone();
    plain.slots[slot].cumulative.clear();
    let commands = commands(&plain, order, None);
    let scorer = ApScorer::new(input);
    if scorer.assist < 0.0 || scorer.luck < 0.0 || scorer.base.iter().any(|&base| base < 0.0) { return Ok(None); }
    let cumulative = (0..=limit).map(|count| (factor.mill(count) as f64).abs()).fold(0.0, f64::max);
    let magnitude = 2.0 * (1.0 + (commands.iter().map(|command| (command.delta as f64).abs()).sum::<f64>()
        + (2.0 * limit as f64 + 1.0) * cumulative) / 100000.0);
    if !magnitude.is_finite() || scorer.integer_bound(magnitude) >= (1u64 << 50) as f64 { return Ok(None); }
    let mut ledger = Ledger { input, scorer, commands, factor: factor.clone(), owner, start, activation: input.frame_time(start), end, limit };
    let Some((value, frames)) = timing.optimize(&mut ledger, best)? else { return Ok(None); };
    if timing.conflict(&frames)?.is_some() { return Err("native frame optimization input mismatch".into()); }
    if ApScorer::new(input).order(order, Some(&Delivery::from_frames(&frames))) != value {
        return Err("native frame optimization score mismatch".into());
    }
    Ok(Some(value))
}
