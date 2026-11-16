//! All distinct integer-millisecond render grids, with relaxed phase pruning.
use super::{score, static_commands, summarize, timed_scores, ApScorer, FrameClock, Input, Scores};
use crate::{chart::Chart, input_timing::InputTiming, master::Master};
use std::{cell::RefCell, collections::HashMap, sync::Arc};
type FactorKey = (Vec<usize>, i32, i32, i32, i32, u64);

pub struct PhaseTiming {
    base: Arc<InputTiming>,
    cached: RefCell<Vec<(i32, Arc<InputTiming>)>>,
    weights: RefCell<HashMap<FactorKey, [f64; 5]>>,
}
impl PhaseTiming {
    pub fn new(
        master: &Master,
        chart: &Chart,
        grades: Option<&[usize]>,
        clock: FrameClock,
    ) -> Result<Self, String> {
        Ok(Self::from_timing(InputTiming::new(
            master, chart, grades, clock,
        )?))
    }

    pub(crate) fn from_timing(timing: InputTiming) -> Self {
        Self {
            base: Arc::new(timing),
            cached: RefCell::new(Vec::new()),
            weights: RefCell::new(HashMap::new()),
        }
    }

    fn timing(&self, clock: FrameClock, phase: i32) -> Result<Arc<InputTiming>, String> {
        if phase == 0 {
            return Ok(Arc::clone(&self.base));
        }
        let mut cached = self.cached.borrow_mut();
        if let Some(index) = cached.iter().position(|(key, _)| *key == phase) {
            let entry = cached.remove(index);
            let value = Arc::clone(&entry.1);
            cached.push(entry);
            return Ok(value);
        }
        let value = Arc::new(self.base.with_clock(clock)?);
        if cached.len() == 2 {
            cached.remove(0);
        }
        cached.push((phase, Arc::clone(&value)));
        Ok(value)
    }

    pub(crate) fn cumulative_weights(
        &self,
        prefix: &[f64],
        input: &Input,
        factor: &super::Cumulative,
    ) -> [f64; 5] {
        let key = (
            factor.judgements.clone(),
            factor.unit,
            factor.value,
            factor.maximum,
            factor.cap,
            factor.seconds.to_bits(),
        );
        *self.weights.borrow_mut().entry(key).or_insert_with(|| {
            std::array::from_fn(|event| {
                input.skill_times.get(event).map_or(0.0, |&start| {
                    self.base.cumulative_upper(prefix, input, factor, start)
                })
            })
        })
    }

    pub(crate) fn guide(&self, input: &Input, order: usize, before_ms: &[i32]) -> Result<Option<super::Timing>, String> {
        let Some(nominal) = super::timing_with_inputs(input, order, before_ms, Some(&self.base)) else { return Ok(None); };
        if nominal.guide_available && nominal.best > nominal.plan_base_score
            || before_ms.iter().all(|&early| early < input.clock.advance_ms()) { return Ok(Some(nominal)); }
        let mut shifted = input.clone();
        let generation = crate::cancellation_generation();
        for phase in 1..input.clock.phase_count() {
            crate::input_timing::poll(generation)?;
            shifted.clock = input.clock.with_phase(phase);
            let inputs = self.timing(shifted.clock, phase)?;
            if let Some(mut plan) = super::timing_with_inputs(&shifted, order, before_ms, Some(&inputs)) {
                if plan.guide_available && plan.best > plan.plan_base_score && plan.best > nominal.score {
                    plan.score = nominal.score;
                    return Ok(Some(plan));
                }
            }
        }
        Ok(Some(nominal))
    }

    pub fn scores(&self, input: &Input, best: bool) -> Result<Scores, String> {
        Ok(summarize(input.power, self.extremes(input, best, None)?))
    }

    pub(crate) fn order(&self, input: &Input, best: bool, index: usize) -> Result<f64, String> {
        Ok(self.extremes(input, best, Some(index))?[index])
    }

    fn extremes(&self, input: &Input, best: bool, only: Option<usize>) -> Result<Vec<f64>, String> {
        super::validate(input)?;
        if input.slots.iter().all(|slot| slot.cumulative.is_empty()) {
            return Ok(score(input)?.scores);
        }
        let spans: Vec<_> = input
            .skill_times
            .iter()
            .flat_map(|&start| {
                input.slots.iter().flat_map(move |slot| {
                    slot.cumulative.iter().map(move |factor| {
                        (
                            start,
                            start
                                .saturating_add(factor.duration_ms())
                                .saturating_add(input.clock.advance_ms()),
                        )
                    })
                })
            })
            .collect();
        let mut shifted = input.clone();
        let mut ap = input.judgements.is_none().then(|| ApScorer::new(input));
        let generation = crate::cancellation_generation();
        let mut landmarks = Vec::new();
        for event in 0..input.skill_times.len().min(5) {
            for slot in 0..input.slots.len() {
                let mut order = [usize::MAX; 5];
                order[event] = slot;
                landmarks.extend(
                    static_commands(input, &order)
                        .iter()
                        .map(|command| (command.time, command.owner)),
                );
            }
        }
        landmarks.sort_unstable();
        landmarks.dedup();
        let mut programs = HashMap::<(usize, Vec<i64>), Vec<f64>>::new();
        let mut phases = Vec::new();
        for phase in 0..input.clock.phase_count() {
            crate::input_timing::poll(generation)?;
            shifted.clock = input.clock.with_phase(phase);
            let shapes = relaxed_programs(&shifted, &self.base, &landmarks, best);
            let mut bounds = vec![0.0; 120];
            let mut needed = [true; 120];
            let mut missing = Vec::new();
            if let Some((slot, keys)) = shapes {
                for (event, key) in keys.into_iter().enumerate() {
                    if let Some(cached) = programs.get(&(event, key.clone())) {
                        for (index, _) in super::orders()
                            .iter()
                            .enumerate()
                            .filter(|(_, order)| order[event] == slot)
                        {
                            bounds[index] = cached[index];
                            needed[index] = false;
                        }
                    } else {
                        missing.push((event, key));
                    }
                }
            }
            if needed.iter().any(|&needed| needed) {
                let frames = self.base.relaxed_frames(shifted.clock, &spans);
                if frames.iter().any(Vec::is_empty) {
                    continue;
                }
                let scores = timed_scores(
                    &shifted,
                    &frames,
                    best,
                    None,
                    (only, Some(&needed)),
                    None,
                    ap.as_mut(),
                )?;
                for (index, &needed) in needed.iter().enumerate() {
                    if needed {
                        bounds[index] = scores[index];
                    }
                }
                for (event, key) in missing {
                    programs.insert((event, key), bounds.clone());
                }
            }
            let priority = only.map_or_else(|| bounds.iter().sum::<f64>(), |index| bounds[index]);
            phases.push((phase, priority, bounds));
        }
        if phases.is_empty() {
            return Err("unreachable native judgement timing".into());
        }
        phases.sort_by(|a, b| {
            if best {
                b.1.total_cmp(&a.1)
            } else {
                a.1.total_cmp(&b.1)
            }
        });
        let mut values = vec![
            if best {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            };
            120
        ];
        for (phase, _, bounds) in &phases {
            let improves = |index: usize| {
                if best {
                    bounds[index] > values[index]
                } else {
                    bounds[index] < values[index]
                }
            };
            if !only.map_or_else(|| (0..120).any(improves), improves) {
                continue;
            }
            crate::input_timing::check_deadline()?;
            shifted.clock = input.clock.with_phase(*phase);
            let timing = self.timing(shifted.clock, *phase)?;
            let scores = timed_scores(
                &shifted,
                &timing.frames,
                best,
                Some(&timing),
                (only, None),
                Some((&values, bounds)),
                ap.as_mut(),
            )?;
            for (value, score) in values.iter_mut().zip(scores) {
                *value = if best {
                    value.max(score)
                } else {
                    value.min(score)
                };
            }
        }
        Ok(values)
    }
}

// Equal ledger positions and static-command interleavings preserve every binary32
// operation for all orders. Physical feasibility remains specific to each origin.
fn relaxed_programs(
    input: &Input,
    timing: &InputTiming,
    landmarks: &[(i32, usize)],
    best: bool,
) -> Option<(usize, Vec<Vec<i64>>)> {
    if input.judgements.is_some()
        || input.slots.iter().any(|slot| {
            slot.effects
                .iter()
                .any(|effect| effect.release_judgements.contains(&5))
        })
    {
        return None;
    }
    let mut factors = input
        .slots
        .iter()
        .enumerate()
        .flat_map(|(slot, value)| value.cumulative.iter().map(move |factor| (slot, factor)));
    let (slot, factor) = factors.next()?;
    if factors.next().is_some() || factor.requires_unbounded_bounds(input.times.len()) {
        return None;
    }
    if !factor.judgements.contains(&5) || factor.value == 0 {
        return Some((slot, vec![Vec::new(); 5]));
    }
    let mut keys = Vec::new();
    for (event, &start) in input.skill_times.iter().take(5).enumerate() {
        let (activation, end) = factor.window(input.clock, start);
        let mut counted = timing.relaxed_counter_frames(input.clock, activation, end, best);
        counted.sort_unstable();
        let mut commands = Vec::new();
        let mut append = |time: i32, delta: i32| {
            commands.extend([
                input.times.partition_point(|&at| at < time) as i64,
                landmarks.partition_point(|&at| at <= (time, event)) as i64,
                delta as i64,
            ]);
        };
        let (mut first, mut count, mut applied) = (0, 0, 0i32);
        while first < counted.len() {
            let at = counted[first];
            let after = first + counted[first..].partition_point(|&time| time == at);
            count += (after - first) as i32;
            let mill = factor.mill(count);
            if mill != applied {
                append(at, applied.wrapping_neg());
                append(at, mill);
                applied = mill;
            }
            first = after;
        }
        if applied != 0 {
            append(end, applied.wrapping_neg());
        }
        keys.push(commands);
    }
    keys.resize_with(5, Vec::new);
    Some((slot, keys))
}
