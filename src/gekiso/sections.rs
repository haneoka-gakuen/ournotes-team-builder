//! Compact section observations share the native order-prefix replay tree.
use super::*;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

#[derive(Clone, Copy, Default, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Standing {
    pub metric: i32,
    pub score: i32,
    pub perfect: i32,
}
impl Standing {
    pub fn from_state(state: &gekisou::State, mission: i32) -> Self {
        Self {
            metric: match mission {
                1 => state.max_combo,
                2 => state.luck.total_bonus,
                3 => state.just,
                _ => 0,
            },
            score: state.score(),
            perfect: state.perfect,
        }
    }
    pub fn beats(&self, other: &Self) -> bool {
        (self.metric, self.score, self.perfect) > (other.metric, other.score, other.perfect)
    }

    pub(crate) fn rank_against(&self, range: usize, opponents: &[[Standing; 3]]) -> i32 {
        1 + opponents.iter().filter(|other| other[range].beats(self)).count().min(4) as i32
    }
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Observation {
    pub sections: [Standing; 3],
    pub base_score: f64,
    pub total_score: f64,
    pub bonus: f64,
    pub ranks: [i32; 3],
    pub window: Option<crate::just_window::Window>,
}
impl Observation {
    fn from_model(model: &Model<'_>) -> Self {
        let mut value = Self {
            total_score: model.score.score.score as f64,
            ranks: [1; 3],
            ..Self::default()
        };
        if let Some(g) = &model.gk {
            for (index, state) in g.controller.states.iter().take(3).enumerate() {
                value.sections[index] =
                    Standing::from_state(state, g.controller.ranges[index].mission);
            }
            for bonus in &g.rank_bonus {
                if bonus.range < 3 {
                    value.ranks[bonus.range] = bonus.rank;
                }
                value.bonus += bonus.bonus as f64;
            }
        }
        let bonus = model.gk.as_ref().map_or(0, |g| {
            g.rank_bonus
                .iter()
                .fold(0_i32, |sum, row| sum.wrapping_add(row.bonus))
        });
        value.base_score = model.score.score.score.wrapping_sub(bonus) as f64;
        value.window = model.just_window.as_ref().and_then(|trace| trace.result());
        value
    }
}

pub(crate) type OpponentSamples = Arc<Vec<Vec<[Standing; 3]>>>;
pub(crate) struct ReplayScene<'a> {
    pub opponents: Option<&'a Vec<[Standing; 3]>>,
    pub deliveries: Option<&'a std::collections::HashMap<i32, i32>>,
    pub window: Option<&'a crate::just_window::Trace>,
    pub minimum: Option<crate::just_window::Policy>,
}

pub(crate) fn metrics(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    seed: i32,
) -> Result<[i32; 3], String> {
    let mut model = Model::new(
        master,
        deck,
        &chart.full.notes,
        &chart.full.events,
        chart.params(),
        Some(&chart.setup),
        (seed, false),
    )?;
    chart.fixed_life(&mut model);
    model.score.score.record_only = true;
    let generation = crate::cancellation_generation();
    for (index, frame) in chart.play.frames.iter().enumerate() {
        if generation != crate::cancellation_generation() {
            return Err("cancelled".into());
        }
        model.count_frame(frame.time_ms, &frame.judged, chart.play.dt[index])?;
    }
    let g = model.gk.as_ref().unwrap();
    let result = std::array::from_fn(|range| {
        Standing::from_state(&g.controller.states[range], chart.setup.missions[range]).metric
    });
    #[cfg(debug_assertions)]
    {
        let full = super::replay_order_mode(master, chart, deck, 1, 0, (seed, false))?;
        debug_assert_eq!(result, full.missions);
    }
    Ok(result)
}

pub(crate) fn metric_samples(master: &Master, chart: &Chart, deck: &[Performer], seeds: &[i32]) -> Result<Vec<[i32; 3]>, String> {
    let groups = super::factored::metric_order_groups(master, chart, deck);
    let mut aliases = vec![Vec::new(); 120];
    let orders: Vec<_> = groups.iter().map(|group| group[0]).collect();
    for group in groups { let first = group[0]; aliases[first] = group; }
    let events: Vec<Vec<_>> = live::orders().iter().map(|order| chart.full.events.iter()
        .map(|&(event, time)| (order[event], time)).collect()).collect();
    let mut chronological: Vec<_> = (0..chart.full.events.len()).collect();
    chronological.sort_by_key(|&index| (chart.full.events[index].1, index));
    let mut capacity = [0; 5];
    for &order in &orders {
        let mut count = [0; 5];
        for &(actor, _) in &events[order] { count[actor] += 1; }
        for (maximum, count) in capacity.iter_mut().zip(count) { *maximum = (*maximum).max(count); }
    }
    let runs = if super::replay_random(master, chart, deck) { seeds.to_vec() } else { vec![seeds.first().copied().unwrap_or(0)] };
    let batches = crate::parallel::map(&runs, |&seed| {
        let values = Mutex::new(vec![[0; 3]; 120]);
        let record = |model: &Model<'_>, orders: &[usize]| {
            let g = model.gk.as_ref().unwrap();
            let metrics = std::array::from_fn(|range| Standing::from_state(&g.controller.states[range], chart.setup.missions[range]).metric);
            let mut values = values.lock().unwrap();
            for &order in orders { for &alias in &aliases[order] { values[alias] = metrics; } }
        };
        let tree = ReplayTree::<true, true> { chart, events: &events, chronological: chronological.clone(),
            generation: crate::cancellation_generation(), observe: None, upper: &[], multiplicities: &[],
            stopped: AtomicBool::new(false), detail: Some(&record) };
        let mut model = Model::with_live_capacity(master, deck, &chart.full.notes, &events[orders[0]], chart.params(), Some(&chart.setup), (seed, false, Some(&capacity)))?;
        chart.fixed_life(&mut model);
        model.score.score.record_only = true;
        tree.visit(model, &orders, 0, 0, &mut vec![(0.0, [0.0; 3]); 120])?;
        Ok::<_, String>(values.into_inner().unwrap())
    })?;
    let values: Vec<_> = batches.into_iter().flatten().collect();
    #[cfg(debug_assertions)] {
        let full = replay(master, chart, deck, seeds, 1, None, None)?;
        debug_assert_eq!(values, full.iter().map(|row| row.sections.map(|section| section.metric)).collect::<Vec<_>>(), "section metric orders");
    }
    Ok(values)
}

struct ScenarioReplay<'a> {
    opponents: Vec<&'a [[Standing; 3]]>,
    observations: Vec<Observation>,
}

impl ReplayTree<'_, true> {
    fn visit_scenarios(
        &self,
        mut model: Model<'_>,
        orders: &[usize],
        from: usize,
        fixed: usize,
        samples: &[usize],
        replay: &mut ScenarioReplay<'_>,
    ) -> Result<(), String> {
        let split = self.order_split(orders, fixed);
        let until = split.as_ref().map_or(self.chart.play.frames.len(), |split| split.frame);
        for index in from..until {
            if self.generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
            if let Some(trace) = &mut model.just_window {
                trace.record_probe(index);
            }
            let frame = &self.chart.play.frames[index];
            let Some((range, own)) = model.frame_to_section_rank(
                frame.time_ms,
                &frame.judged,
                self.chart.play.dt[index],
            )? else {
                continue;
            };
            let rank = |sample: usize| own.rank_against(range, replay.opponents[sample]);
            let first = rank(samples[0]);
            if samples[1..].iter().all(|&sample| rank(sample) == first) {
                model.finish_section_frame(range, first);
                continue;
            }
            // A different rank changes the fixed bonus and becomes skill input on the next frame.
            let mut by_rank = [const { Vec::new() }; 5];
            for &sample in samples {
                by_rank[rank(sample) as usize - 1].push(sample);
            }
            let mut branches = by_rank.into_iter().enumerate().filter(|(_, samples)| !samples.is_empty()).peekable();
            while let Some((rank, samples)) = branches.next() {
                let rank = rank as i32 + 1;
                if branches.peek().is_none() {
                    model.finish_section_frame(range, rank);
                    return self.visit_scenarios(model, orders, index + 1, fixed, &samples, replay);
                }
                let mut branch = model.clone();
                branch.finish_section_frame(range, rank);
                self.visit_scenarios(branch, orders, index + 1, fixed, &samples, replay)?;
            }
        }
        if let Some(split) = split {
            let mut groups = self.order_groups(orders, &split).peekable();
            while let Some(orders) = groups.next() {
                if groups.peek().is_none() {
                    model.set_event_members(&self.events[orders[0]]);
                    return self.visit_scenarios(model, &orders, until, split.end, samples, replay);
                }
                let mut branch = model.clone();
                branch.set_event_members(&self.events[orders[0]]);
                self.visit_scenarios(branch, &orders, until, split.end, samples, replay)?;
            }
        } else {
            let row = Observation::from_model(&model);
            for &sample in samples {
                for &order in orders {
                    replay.observations[sample * 120 + order] = row;
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn replay(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
    power: i32,
    opponents: Option<&OpponentSamples>,
    window: Option<&crate::just_window::Trace>,
) -> Result<Vec<Observation>, String> {
    let random = crate::gekiso::replay_random(master, chart, deck);
    let runs = if random || opponents.is_some() {
        seeds.to_vec()
    } else {
        vec![seeds.first().copied().unwrap_or(0)]
    };
    if runs.is_empty() {
        return Err("section-luck-samples-required".into());
    }
    let events: Vec<Vec<_>> = live::orders()
        .iter()
        .map(|order| {
            chart
                .full
                .events
                .iter()
                .map(|&(event, time)| (order[event], time))
                .collect()
        })
        .collect();
    let mut chronological: Vec<_> = (0..chart.full.events.len()).collect();
    chronological.sort_by_key(|&event| (chart.full.events[event].1, event));
    let mut capacity = [0; 5];
    for row in &events {
        let mut counts = [0; 5];
        for &(member, _) in row {
            counts[member] += 1;
        }
        for (maximum, count) in capacity.iter_mut().zip(counts) {
            *maximum = (*maximum).max(count);
        }
    }
    let orders: Vec<_> = (0..120).collect();
    let make_model = |seed| {
        let mut params = chart.params();
        params.power = power;
        params.music_type = chart.music_type;
        let mut model = Model::with_live_capacity(
            master,
            deck,
            &chart.full.notes,
            &events[0],
            params,
            Some(&chart.setup),
            (seed, false, Some(&capacity)),
        )?;
        chart.fixed_life(&mut model);
        model.just_window = window.cloned();
        Ok::<_, String>(model)
    };
    if let Some(opponents) = opponents.filter(|_| !random && runs.len() > 1) {
        let mut replay = ScenarioReplay {
            opponents: runs.iter().map(|&seed| opponents[seed as usize % opponents.len()].as_slice()).collect(),
            observations: vec![Observation::default(); 120 * runs.len()],
        };
        let samples: Vec<_> = (0..runs.len()).collect();
        let tree = ReplayTree::<true> {
            chart,
            events: &events,
            chronological,
            generation: crate::cancellation_generation(),
            observe: None,
            upper: &[],
            multiplicities: &[],
            stopped: AtomicBool::new(false),
            detail: None,
        };
        tree.visit_scenarios(make_model(runs[0])?, &orders, 0, 0, &samples, &mut replay)?;
        return Ok(replay.observations);
    }
    let batches = crate::parallel::map(&runs, |&seed| {
        let observations = Mutex::new(vec![Observation::default(); 120]);
        let record = |model: &Model<'_>, orders: &[usize]| {
            let observation = Observation::from_model(model);
            let mut values = observations.lock().unwrap();
            for &order in orders {
                values[order] = observation;
            }
        };
        let tree = ReplayTree::<true> {
            chart,
            events: &events,
            chronological: chronological.clone(),
            generation: crate::cancellation_generation(),
            observe: None,
            upper: &[],
            multiplicities: &[],
            stopped: AtomicBool::new(false),
            detail: Some(&record),
        };
        let mut model = make_model(seed)?;
        if let Some(samples) = opponents {
            model.gk.as_mut().unwrap().section_opponents =
                Some(Arc::new(samples[seed as usize % samples.len()].clone()));
        }
        let mut unused = vec![(0.0, [0.0; 3]); 120];
        tree.visit(model, &orders, 0, 0, &mut unused)?;
        Ok::<_, String>(observations.into_inner().unwrap())
    })?;
    Ok(batches.into_iter().flatten().collect())
}
