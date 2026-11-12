//! One input-checked chronological plan for COMBO edges and cumulative skills.
use crate::{
    chart, full::conditions::Performer, gekiso, input_timing::InputTiming, live, master::Master,
};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Action {
    kind: &'static str,
    event: Option<usize>,
    range: Option<usize>,
    time_ms: i32,
    note_indices: Vec<usize>,
    note_times_ms: Vec<i32>,
    input_times_ms: Vec<i32>,
    included: bool,
    gain: Option<f64>,
    combo_gain: Option<f64>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Guide {
    frame_rate: i32,
    score: Option<f64>,
    best: Option<f64>,
    actions: Vec<Action>,
}
#[derive(Serialize)]
#[serde(rename_all="camelCase")]
pub(crate) struct Analysis {pub guide:Option<Guide>,pub window_profile:Option<crate::just_window::WindowProfile>}
#[derive(Clone)]
struct Outcome {
    valid: bool,
    base: f64,
    protect_base: bool,
    wins: [f64; 3],
    all_win: f64,
    score: f64,
    combos: [f64; 3],
    missions: [f64; 3],
    combo_frames: Vec<i32>,
}
impl Outcome {
    fn improves(&self, previous: &Self) -> bool {
        self.valid
            && previous.valid
            && (!self.protect_base || self.base >= previous.base)
            && self.all_win >= previous.all_win
            && self.wins.iter().zip(previous.wins).all(|(&a, b)| a >= b)
            && self.score >= previous.score
            && self
                .missions
                .iter()
                .zip(previous.missions)
                .all(|(&a, b)| a >= b)
            && (self.score > previous.score
                || self
                    .missions
                    .iter()
                    .zip(previous.missions)
                    .any(|(&a, b)| a > b))
    }
}
struct Candidate {
    action: Action,
    targets: Vec<i32>,
}
pub(crate) struct Context<'a> {
    pub master: &'a Master,
    pub compiled: &'a chart::Chart,
    pub chart: gekiso::Chart,
    pub deck: Vec<Performer>,
    pub input: live::Input,
    pub power: i32,
    pub order: usize,
    pub seeds: Vec<i32>,
    pub opponents: Option<gekiso::sections::OpponentSamples>,
    pub window: Option<crate::just_window::Trace>,
    pub minimum: Option<crate::just_window::Policy>,
    pub preserve_base: bool,
}
impl Context<'_> {
    fn deliver(&mut self, selected: &[i32]) {
        let positions: HashMap<_, _> = self
            .chart
            .notes
            .iter()
            .enumerate()
            .map(|(index, note)| (note.id, index))
            .collect();
        let mut delivered = Vec::with_capacity(selected.len());
        for frame in &mut self.chart.play.frames {
            delivered.append(&mut frame.judged);
        }
        for note in delivered {
            let target = positions
                .get(&note.note_id)
                .map_or(note.time_ms, |&index| selected[index]);
            let frame = self
                .chart
                .play
                .frames
                .partition_point(|frame| frame.time_ms < target);
            self.chart.play.frames[frame].judged.push(note);
        }
        for frame in &mut self.chart.play.frames {
            frame
                .judged
                .sort_by_key(|note| (note.time_ms, note.note_id));
        }
    }
    fn score(&mut self, selected: &[i32], trace_combo: bool) -> Result<Outcome, String> {
        self.deliver(selected);
        let mut total = 0.0;
        let mut base = 0.0;
        let mut wins = [0.0; 3];
        let mut all_win = 0.0;
        let mut combos = [0.0; 3];
        let mut missions = [0.0; 3];
        let mut combo_frames = Vec::new();
        let deliveries = self
            .chart
            .notes
            .iter()
            .enumerate()
            .map(|(index, note)| (note.id, selected[index]))
            .collect();
        let window=self.window.as_ref().map(|trace|{let mut trace=trace.clone();trace.relocate(&self.chart.play.frames);trace});
        for &seed in &self.seeds {
            let scene = gekiso::sections::ReplayScene {
                opponents: self
                    .opponents
                    .as_ref()
                    .map(|samples| &samples[seed as usize % samples.len()]),
                deliveries: Some(&deliveries),
                window: window.as_ref(),
                minimum: self.minimum,
            };
            let value = gekiso::replay_order_scene(
                self.master,
                &self.chart,
                &self.deck,
                self.power,
                self.order,
                (seed, trace_combo),
                Some(&scene),
            )?;
            if !value.valid {
                return Ok(Outcome {
                    valid: false,
                    base: 0.0,
                    protect_base: self.preserve_base,
                    wins: [0.0; 3],
                    all_win: 0.0,
                    score: 0.0,
                    combos: [0.0; 3],
                    missions: [0.0; 3],
                    combo_frames: Vec::new(),
                });
            }
            base += value.base_score;
            for (win, rank) in wins.iter_mut().zip(value.ranks) {
                if rank == 1 {
                    *win += 1.0;
                }
            }
            if value.ranks.iter().all(|&rank| rank == 1) {
                all_win += 1.0;
            }
            total += value.score;
            for (target, value) in missions.iter_mut().zip(value.missions) {
                *target += value as f64;
            }
            combo_frames.extend(value.combo_frames);
            for (target, combo) in combos.iter_mut().zip(value.combos) {
                *target += combo as f64;
            }
        }
        for target in &mut combos {
            *target /= self.seeds.len() as f64;
        }
        for target in &mut missions {
            *target /= self.seeds.len() as f64;
        }
        combo_frames.sort_unstable();
        combo_frames.dedup();
        Ok(Outcome {
            valid: true,
            base: base / self.seeds.len() as f64,
            protect_base: self.preserve_base,
            wins: wins.map(|win| win / self.seeds.len() as f64),
            all_win: all_win / self.seeds.len() as f64,
            score: total / self.seeds.len() as f64,
            combos,
            missions,
            combo_frames,
        })
    }
}
pub(crate) fn build(mut context: Context<'_>) -> Result<Analysis, String> {
    let profile=|context:&Context<'_>|crate::just_window::describe(crate::just_window::ProfileInput{
        master:context.master,chart:&context.chart,deck:&context.deck,power:context.power,order:context.order,
        seeds:&context.seeds,opponents:context.opponents.as_ref(),minimum:context.minimum,
    });
    let nominal_profile=profile(&context)?;
    let compiled = context.compiled;
    if context.chart.notes.len() != compiled.times.len()
        || context
            .chart
            .notes
            .iter()
            .enumerate()
            .any(|(i, note)| note.time_ms != compiled.times[i] || note.op != compiled.note_types[i])
    {
        return Ok(Analysis{guide:None,window_profile:nominal_profile});
    }
    if !gekiso::replay_random(context.master, &context.chart, &context.deck)
        && context.opponents.is_none()
    {
        context.seeds.clear();
    }
    let report_scores = !context.chart.luck || !context.seeds.is_empty();
    if context.seeds.is_empty() {
        context.seeds.push(0);
    }
    let clock = live::FrameClock::default();
    let grades: Vec<_> = context
        .chart
        .judgements
        .iter()
        .map(|&grade| grade.min(5))
        .collect();
    let inputs = InputTiming::new(context.master, compiled, Some(&grades), clock)?;
    let delivery: HashMap<_, _> = context
        .chart
        .play
        .frames
        .iter()
        .flat_map(|frame| {
            frame
                .judged
                .iter()
                .map(move |note| (note.note_id, frame.time_ms))
        })
        .collect();
    let mut selected: Vec<_> = context
        .chart
        .notes
        .iter()
        .map(|note| delivery[&note.id])
        .collect();
    if !matches!(inputs.conflict(&selected), Ok(None)) {
        return Ok(Analysis{guide:None,window_profile:nominal_profile});
    }
    let score = context.score(&selected, true)?;
    if !score.valid {
        return Ok(Analysis{guide:None,window_profile:nominal_profile});
    }
    let mut best = score.clone();
    let mut actions = Vec::new();
    let mut candidates = Vec::new();
    let fevers = context.chart.setup.fevers.clone();
    for (range, &(start, end)) in fevers.iter().enumerate() {
        if context.chart.setup.missions.get(range) != Some(&1) {
            continue;
        }
        let first = compiled.times.partition_point(|&time| time < start);
        if first < selected.len() && compiled.times[first] <= end {
            let time = compiled.times[first];
            let after = compiled.times.partition_point(|&value| value <= time);
            let indices: Vec<_> = (first..after).collect();
            if indices.len() > 1
                && indices
                    .iter()
                    .skip(1)
                    .any(|&i| selected[i] > clock.frame_time(time))
            {
                let mut simultaneous = selected.clone();
                for &i in &indices {
                    simultaneous[i] = clock.frame_time(time);
                }
                let check = inputs.conflict(&simultaneous);
                let without = context.score(&simultaneous, false)?;
                if matches!(check, Ok(None)) && score.improves(&without) {
                    actions.push(Action {
                        kind: "combo-opening",
                        event: None,
                        range: Some(range),
                        time_ms: time,
                        note_times_ms: indices.iter().map(|&i| compiled.times[i]).collect(),
                        input_times_ms: indices.iter().map(|&i| selected[i]).collect(),
                        note_indices: indices,
                        included: true,
                        gain: None,
                        combo_gain: Some(score.combos[range] - without.combos[range]),
                    });
                }
            }
        }
        let indices: Vec<_> = compiled
            .times
            .iter()
            .enumerate()
            .filter_map(|(i, &time)| {
                (time == end && matches!(compiled.note_types[i], 1 | 40) && grades[i] == 5)
                    .then_some(i)
            })
            .collect();
        if !indices.is_empty() {
            let target = indices
                .iter()
                .map(|&i| inputs.frames[i].first().copied().unwrap_or(end))
                .max()
                .unwrap()
                .max(start);
            candidates.push(Candidate {
                targets: vec![target; indices.len()],
                action: Action {
                    kind: "combo-ending",
                    event: None,
                    range: Some(range),
                    time_ms: end,
                    note_times_ms: indices.iter().map(|&i| compiled.times[i]).collect(),
                    note_indices: indices,
                    input_times_ms: Vec::new(),
                    included: false,
                    gain: None,
                    combo_gain: None,
                },
            });
        }
    }
    for &frame in &score.combo_frames {
        let first = compiled
            .times
            .partition_point(|&time| clock.frame_time(time) < frame);
        let after = compiled
            .times
            .partition_point(|&time| clock.frame_time(time) <= frame);
        if after - first < 2 {
            continue;
        }
        let time = compiled.times[first];
        let Some(range) = fevers
            .iter()
            .enumerate()
            .position(|(range, &(start, end))| {
                context.chart.setup.missions.get(range) == Some(&1) && start <= time && time < end
            })
        else {
            continue;
        };
        if (first..after).any(|i| {
            compiled.times[i] != time || !matches!(compiled.note_types[i], 1 | 40) || grades[i] != 5
        }) {
            continue;
        }
        let indices: Vec<_> = (first..after).collect();
        let next = context
            .chart
            .play
            .frames
            .partition_point(|row| row.time_ms <= frame);
        let Some(next) = context
            .chart
            .play
            .frames
            .get(next)
            .filter(|row| row.time_ms < fevers[range].1)
        else {
            continue;
        };
        let mut targets = vec![next.time_ms; indices.len()];
        targets[0] = frame;
        candidates.push(Candidate {
            targets,
            action: Action {
                kind: "combo-chord",
                event: None,
                range: Some(range),
                time_ms: time,
                note_times_ms: indices.iter().map(|&i| compiled.times[i]).collect(),
                note_indices: indices,
                input_times_ms: Vec::new(),
                included: false,
                gain: None,
                combo_gain: None,
            },
        });
    }
    let before: Vec<_> = compiled
        .judgement_types
        .iter()
        .enumerate()
        .map(|(i, &kind)| {
            if context.chart.judgements[i] > 5 || !matches!(compiled.note_types[i], 1 | 40) {
                return 0;
            }
            if context
                .master
                .live
                .judgement_before_ms
                .get(&(kind, grades[i] as i32))
                .is_some_and(|&before| before >= clock.advance_ms())
            {
                clock.advance_ms()
            } else {
                0
            }
        })
        .collect();
    if let Some(timing) = live::timing(&context.input, context.order, &before) {
        for window in timing.windows {
            for step in window.steps {
                let targets: Vec<i32> = step
                    .note_indices
                    .iter()
                    .map(|&i| clock.frame_time(compiled.times[i] - clock.advance_ms()))
                    .collect();
                if !report_scores && targets.iter().any(|&frame| frame < window.start_ms) {
                    continue;
                }
                candidates.push(Candidate {
                    targets,
                    action: Action {
                        kind: "skill",
                        event: Some(window.event),
                        range: None,
                        time_ms: step.time_ms,
                        note_indices: step.note_indices,
                        input_times_ms: Vec::new(),
                        included: false,
                        gain: None,
                        combo_gain: None,
                        note_times_ms: step.note_times_ms,
                    },
                });
            }
        }
    }
    candidates.sort_by_key(|candidate| candidate.action.time_ms);
    let mut fixed: Vec<_> = selected
        .iter()
        .enumerate()
        .map(|(i, &frame)| frame != clock.frame_time(compiled.times[i]))
        .collect();
    for mut candidate in candidates {
        if !report_scores && candidate.action.kind != "skill" {
            continue;
        }
        let indices = &candidate.action.note_indices;
        if indices.iter().zip(&candidate.targets).any(|(&i, &frame)| {
            (fixed[i] && selected[i] != frame) || !inputs.frames[i].contains(&frame)
        }) {
            continue;
        }
        let previous: Vec<_> = indices.iter().map(|&i| selected[i]).collect();
        for (&i, &frame) in indices.iter().zip(&candidate.targets) {
            selected[i] = frame;
        }
        if matches!(inputs.conflict(&selected), Ok(None)) {
            let value = context.score(&selected, false)?;
            if value.improves(&best) {
                candidate.action.input_times_ms = candidate.targets;
                candidate.action.gain = report_scores.then_some(value.score - best.score);
                candidate.action.combo_gain = candidate
                    .action
                    .range
                    .map(|range| value.combos[range] - best.combos[range]);
                for &i in indices {
                    fixed[i] = true;
                }
                best = value;
                actions.push(candidate.action);
                continue;
            }
        }
        for (&i, frame) in indices.iter().zip(previous) {
            selected[i] = frame;
        }
    }
    actions.sort_by_key(|action| action.time_ms);
    let window_profile=if !actions.is_empty()&&nominal_profile.is_some() {context.deliver(&selected);profile(&context)?}else{nominal_profile};
    Ok(Analysis{window_profile,guide:Some(Guide {
        frame_rate: 60,
        score: report_scores.then_some(score.score),
        best: report_scores.then_some(best.score),
        actions,
    })})
}
