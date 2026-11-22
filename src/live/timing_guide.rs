//! Replay the winning schedule in chart order and explain only score-increasing changes.
use super::*;
use crate::input_timing::{check_deadline, InputTiming};

/// On a non-AP play the all-on-beat delivery can be physically impossible.
/// Prefer the closest individual frames, then restore nominal chords wherever the full replay allows it.
fn reference_frames(
    input: &Input,
    timing: &InputTiming,
    winner: &[i32],
) -> Result<Vec<i32>, String> {
    let nearest: Vec<_> = input
        .times
        .iter()
        .zip(&timing.frames)
        .map(|(&time, domain)| {
            let nominal = input.frame_time(time);
            *domain
                .iter()
                .min_by_key(|&&at| (at.abs_diff(nominal), at))
                .unwrap()
        })
        .collect();
    if timing.conflict(&nearest)?.is_none() {
        return Ok(nearest);
    }
    let mut reference = winner.to_vec();
    let mut start = 0;
    for chord in input.times.chunk_by(|a, b| a == b) {
        let end = start + chord.len();
        if reference[start..end] != nearest[start..end] {
            check_deadline()?;
            reference[start..end].copy_from_slice(&nearest[start..end]);
            if timing.conflict(&reference)?.is_some() {
                reference[start..end].copy_from_slice(&winner[start..end]);
            }
        }
        start = end;
    }
    Ok(reference)
}

fn score_with_counters(input: &Input, order: &[usize; 5], mut counters: Vec<Command>) -> f64 {
    counters.sort_by_key(|c| (c.time, c.owner));
    if let Some(grades) = &input.judgements {
        PlayScorer::new(input, grades).order_with_counters(order, None, Some(counters))
    } else {
        let mut commands = static_commands(input, order);
        commands.extend(counters);
        commands.sort_by_key(|c| (c.time, c.owner));
        ApScorer::new(input).score_commands(input, commands)
    }
}

/// Switch each changed owner's counter program in turn. Static skills, rounding and all
/// other counters stay in the scorer, including when two skills cover the same notes.
fn contributing_windows(
    input: &Input,
    order: &[usize; 5],
    windows: &[TimingWindow],
    before: &[Vec<Command>],
    after: &[Vec<Command>],
    previous_score: f64,
    score: f64,
) -> Result<Vec<usize>, String> {
    let mut owners = Vec::new();
    for (i, window) in windows.iter().enumerate() {
        if before[i] != after[i]
            && owners
                .last()
                .is_none_or(|&j: &usize| windows[j].event != window.event)
        {
            owners.push(i);
        }
    }
    let replay = |use_after: &dyn Fn(usize) -> bool| {
        let commands = windows
            .iter()
            .enumerate()
            .flat_map(|(j, w)| {
                if use_after(w.event) {
                    &after[j]
                } else {
                    &before[j]
                }
            })
            .cloned()
            .collect();
        score_with_counters(input, order, commands)
    };
    let mut previous = previous_score;
    let mut contributors = Vec::new();
    for (position, &i) in owners.iter().enumerate() {
        let event = windows[i].event;
        let value = if position + 1 == owners.len() {
            score
        } else {
            replay(&|e| e <= event)
        };
        // Rounding can make an earlier owner necessary only once the others are active.
        if value > previous || (position + 1 < owners.len() && replay(&|e| e != event) < score) {
            contributors.push(i);
        }
        previous = value;
    }
    if contributors.is_empty() {
        return Err("timing gain without a contributing skill".into());
    }
    Ok(contributors)
}

pub(super) fn build(
    input: &Input,
    index: usize,
    timing: &InputTiming,
    winner: &[i32],
    best: f64,
    score: f64,
) -> Result<Timing, String> {
    let order = &orders()[index];
    let reference = reference_frames(input, timing, winner)?;
    let plan_base_score = selected_score(input, index, &reference);
    let mut frames = winner.to_vec();
    // Two linked notes can be dispensable together even if neither can move back alone.
    let mut start = 0;
    for chord in input.times.chunk_by(|a, b| a == b) {
        let end = start + chord.len();
        if frames[start..end] != reference[start..end]
            && (chord.len() > 1 || reference[start] != input.frame_time(input.times[start]))
        {
            check_deadline()?;
            frames[start..end].copy_from_slice(&reference[start..end]);
            if selected_score(input, index, &frames) != best || timing.conflict(&frames)?.is_some()
            {
                frames[start..end].copy_from_slice(&winner[start..end]);
            }
        }
        start = end;
    }
    let final_inputs = timing.plan(&frames)?;
    if selected_score(input, index, &frames) != best {
        return Err("optimal timing score mismatch".into());
    }
    let base_inputs = if reference == frames {
        final_inputs.clone()
    } else {
        timing.plan(&reference)?
    };
    let mut windows = Vec::new();
    let mut factors = Vec::new();
    for (event, &start) in input.skill_times.iter().take(5).enumerate() {
        for factor in &input.slots[order[event]].cumulative {
            let (start_ms, end_ms) = factor.window(input.clock, start);
            windows.push(TimingWindow {
                event,
                start_ms,
                end_ms,
                unit: factor.unit,
                notes: input
                    .times
                    .iter()
                    .zip(&frames)
                    .filter(|(_, at)| (start_ms..=end_ms).contains(at))
                    .map(|(&time, _)| time)
                    .collect(),
                steps: Vec::new(),
            });
            factors.push((event, factor));
        }
    }
    let grades = input
        .judgements
        .as_ref()
        .map(|grades| converted_judgements(input, order, grades));
    let programs = |at: &[i32]| {
        let delivery = Delivery::from_frames(at);
        factors
            .iter()
            .map(|&(event, factor)| {
                slot_cumulative_commands(
                    input,
                    std::slice::from_ref(factor),
                    event,
                    grades.as_deref(),
                    Some(&delivery),
                )
            })
            .collect::<Vec<_>>()
    };
    let mut current = reference.clone();
    let mut previous_score = plan_base_score;
    let mut previous_programs = programs(&current);
    let changed: Vec<_> = (0..frames.len())
        .filter(|&i| frames[i] != reference[i] || base_inputs[i] != final_inputs[i])
        .collect();
    let mut pending = Vec::new();
    for chord in changed.chunk_by(|&a, &b| input.times[a] == input.times[b]) {
        if previous_score == best {
            break;
        }
        check_deadline()?;
        pending.extend_from_slice(chord);
        for &i in chord {
            current[i] = frames[i];
        }
        let value = selected_score(input, index, &current);
        if value <= previous_score {
            continue;
        }
        let split = chord.last().unwrap() + 1;
        // A cut is playable only if its prefix and suffix use the same actual inputs as
        // the two endpoints. Keep prerequisites with the next gain when they cannot stand alone.
        if current != frames {
            if timing.conflict(&current)?.is_some() {
                continue;
            }
            let intermediate = timing.plan(&current)?;
            if intermediate[..split] != final_inputs[..split]
                || intermediate[split..] != base_inputs[split..]
            {
                continue;
            }
        } else if final_inputs[split..] != base_inputs[split..] {
            continue;
        }
        let after = programs(&current);
        let sources = contributing_windows(
            input,
            order,
            &windows,
            &previous_programs,
            &after,
            previous_score,
            value,
        )?;
        let events = sources.iter().map(|&i| windows[i].event).collect();
        let window = &mut windows[sources[0]];
        let step = TimingStep {
            step: window.steps.len() as i32 + 1,
            events,
            time_ms: input.times[pending[0]],
            notes: pending.len(),
            note_times_ms: pending.iter().map(|&i| input.times[i]).collect(),
            input_times_ms: pending.iter().map(|&i| final_inputs[i].time).collect(),
            input_kinds: pending.iter().map(|&i| final_inputs[i].kind).collect(),
            delivery_times_ms: pending.iter().map(|&i| frames[i]).collect(),
            note_indices: std::mem::take(&mut pending),
            gain: value - previous_score,
        };
        window.steps.push(step);
        previous_programs = after;
        previous_score = value;
    }
    if previous_score != best {
        return Err("timing guide gains do not reach the verified score".into());
    }
    Ok(Timing {
        score,
        plan_base_score,
        frame_origin_ms: input.clock.origin_ms(),
        best,
        guide_available: true,
        plan_is_optimal: true,
        advance_ms: input.clock.advance_ms(),
        windows,
    })
}
