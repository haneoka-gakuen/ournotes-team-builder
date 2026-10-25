//! Gekiso context simulation, nominal lottery expectation and per-order scores.
use crate::{
    chart::Source,
    full::{
        conditions::{Context as CheckContext, Factory, Performer},
        core::{self, Life, Random},
        gekisou,
        model::{LuckRecord, Model, Params},
        play::{self, Accuracy, LiveNote, Setup},
        score::note_factor_mill,
    },
    live::{self, Scores, Slot},
    luck,
    master::Master,
    num::f,
};
use rustc_hash::FxHashMap as HashMap;
use std::{
    borrow::Cow,
    collections::HashSet,
    sync::{Arc, OnceLock},
};
#[derive(Clone)]
pub struct MissRule {
    pub value: i32,
    pub chance: f64,
}
#[derive(Clone)]
pub struct Recorded {
    pub input: luck::Input,
    pub curve: luck::Output,
}
pub struct Chart {
    pub music_type: i32,
    pub full: play::Chart,
    pub setup: Setup,
    pub play: play::Play,
    pub accuracy: Accuracy,
    pub notes: Vec<LiveNote>,
    pub times: Vec<i32>,
    pub note_frac: Vec<f64>,
    pub judgements: Vec<usize>,
    pub range_of: Vec<i32>,
    pub range_percent: Vec<i32>,
    pub luck: bool,
    pub difficulty_factor: f64,
    pub music_length: i32,
    pub combo_factor: Vec<f64>,
    pub judge_frac: [f64; 7],
    pub span: (usize, usize),
    pub(crate) deterministic_ranges: OnceLock<Option<Vec<(i32, i32)>>>,
    pub(crate) factor_operations: OnceLock<usize>,
    curves: HashMap<Vec<u64>, Arc<Recorded>>,
    curve_bytes: usize,
    /// Lottery frame streams by their recorded range states: decks almost always share one.
    frame_streams: HashMap<Vec<u64>, (u64, Arc<Vec<luck::Frame>>)>,
    next_stream: u64,
    pub(crate) performer_ids: HashMap<String, u32>,
    pub(crate) performer_json: Vec<String>,
    pub(crate) seeded: HashMap<String, Vec<crate::search::Hit>>,
    pub(crate) contexts: HashMap<(i32, Vec<u32>), crate::gekiso_objective::Cached>,
    pub(crate) context_order: std::collections::VecDeque<(i32, Vec<u32>)>,
    pub(crate) context_bytes: usize,
    /// Cached contexts per shared lottery record (by address), whose bytes count once.
    pub(crate) context_records: HashMap<usize, usize>,
    /// Note weights by deck key, retained in a bounded second-chance cache.
    pub(crate) weights: HashMap<(i32, Vec<u32>), (Arc<crate::gekiso_objective::Weights>, bool)>,
    pub(crate) weight_order: std::collections::VecDeque<((i32, Vec<u32>), usize)>,
    pub(crate) weight_bytes: usize,
}
#[derive(Clone)]
pub struct Nominal {
    pub probabilities: [Vec<f64>; 4],
    pub note_up: Vec<f64>,
    pub rush_up: f64,
    pub rush_factor: f64,
    pub recorded: Arc<Recorded>,
}
#[derive(Clone)]
pub struct Context {
    pub judgement: Vec<usize>,
    pub gk: Vec<f64>,
    pub up: Vec<f64>,
    pub luck: Vec<f64>,
    pub seeds: usize,
    pub(crate) factor_commands: usize,
    pub(crate) factor_peak: f64,
    pub(crate) rush_changes: usize,
    pub draws: Vec<Arc<Context>>,
    pub nominal: Option<Nominal>,
}
impl Context {
    pub(crate) fn retained_bytes(&self) -> usize {
        let mut bytes = std::mem::size_of::<Self>()
            + self.judgement.capacity() * std::mem::size_of::<usize>()
            + (self.gk.capacity() + self.up.capacity() + self.luck.capacity()) * 8
            + self.draws.capacity() * std::mem::size_of::<Arc<Context>>();
        bytes += self
            .draws
            .iter()
            .map(|draw| draw.retained_bytes())
            .sum::<usize>();
        if let Some(nominal) = &self.nominal {
            bytes += std::mem::size_of::<Nominal>()
                + nominal
                    .probabilities
                    .iter()
                    .map(|p| p.capacity() * 8)
                    .sum::<usize>()
                + nominal.note_up.capacity() * 8;
        }
        bytes
    }
    /// The shared lottery record this context keeps alive: its address and bytes.
    pub(crate) fn shared_record(&self) -> Option<(usize, usize)> {
        self.nominal.as_ref().map(|nominal| {
            let recorded = &nominal.recorded;
            (
                Arc::as_ptr(recorded) as usize,
                recorded.input.retained_bytes()
                    + recorded.curve.curve.len() * 96
                    + recorded.curve.residues.capacity() * 8,
            )
        })
    }
}
pub(crate) fn combo_factors(master: &Master, times: &[i32], judgements: &[usize]) -> Vec<f64> {
    let (mut before, mut pending, mut last) = (0, 0, -1);
    times
        .iter()
        .zip(judgements)
        .map(|(&time, &judgement)| {
            if time != last {
                before = pending;
                last = time;
            }
            let value = master
                .live
                .combo
                .get(&0)
                .into_iter()
                .flatten()
                .take_while(|(required, _)| *required <= before)
                .last()
                .map(|(_, value)| *value)
                .unwrap_or(0.0);
            pending = if judgement <= 2 { 0 } else { pending + 1 };
            f(value.min(1.0) + 1.0)
        })
        .collect()
}

impl Chart {
    pub(crate) fn fixed_life(&self, model: &mut Model<'_>) {
        if self
            .play
            .frames
            .iter()
            .flat_map(|frame| &frame.judged)
            .all(|note| model.master.live.damage.get(note.judgement as usize) == Some(&0.0))
        {
            model.fixed_life();
        }
    }
    pub(crate) fn combo<'a>(&'a self, master: &Master, judgements: &[usize]) -> Cow<'a, [f64]> {
        if self
            .judgements
            .iter()
            .zip(judgements)
            .all(|(&a, &b)| (a <= 2) == (b <= 2))
        {
            Cow::Borrowed(&self.combo_factor)
        } else {
            Cow::Owned(combo_factors(master, &self.times, judgements))
        }
    }

    pub fn new(
        master: &Master,
        source: &Source,
        level: f64,
        missions: Vec<i32>,
        accuracy: Accuracy,
        ranks: Vec<i32>,
    ) -> Result<Self, String> {
        let full = play::compile(master, source, level)?;
        Self::from_full(master, full, missions, accuracy, ranks)
    }
    pub(crate) fn all_just(&self, master: &Master) -> Result<Self, String> {
        let mut chart = Self::from_full(
            master,
            self.full.clone(),
            self.setup.missions.clone(),
            Accuracy {
                just: 1.0,
                ..Accuracy::default()
            },
            self.setup.ranks.clone(),
        )?;
        chart.music_type = self.music_type;
        Ok(chart)
    }
    fn from_full(
        master: &Master,
        full: play::Chart,
        missions: Vec<i32>,
        accuracy: Accuracy,
        ranks: Vec<i32>,
    ) -> Result<Self, String> {
        let setup = Setup {
            fevers: full.fevers.clone(),
            missions,
            ranks: ranks.clone(),
        };
        let play = play::judgement_stream(master, &full, Some(&setup), accuracy)?;
        let judgements_by_id: HashMap<_, _> = play
            .frames
            .iter()
            .flat_map(|frame| frame.judged.iter().map(|j| (j.note_id, j.judgement)))
            .collect();
        let mut notes: Vec<_> = full
            .notes
            .iter()
            .filter(|note| {
                play::is_judged(note.op) && master.live.note_percent.contains_key(&note.op)
            })
            .cloned()
            .collect();
        notes.sort_by_key(|n| (n.time_ms, n.id));
        let n = notes.len();
        let times: Vec<_> = notes.iter().map(|n| n.time_ms).collect();
        let note_frac = notes
            .iter()
            .map(|n| f(master.live.note_percent.get(&n.op).copied().unwrap_or(0.0) / 100.0))
            .collect();
        let judgements: Vec<_> = notes
            .iter()
            .map(|n| judgements_by_id.get(&n.id).copied().unwrap_or(5) as usize)
            .collect();
        let mission = |i| setup.missions.get(i).copied().unwrap_or(0);
        let factors = gekisou::ranking_factors(
            &master.live.gekiso,
            gekisou::mission_pattern(mission(0), mission(1), mission(2)),
        );
        let ranges: Vec<_> = full.fevers.iter().take(3).copied().collect();
        let range_percent = (0..ranges.len())
            .map(|r| {
                let rank = ranks.get(r).copied().unwrap_or(1);
                if (1..=5).contains(&rank) {
                    factors[r][rank as usize - 1]
                } else {
                    0
                }
            })
            .collect();
        let mut range_of = vec![-1; n];
        for (i, note) in notes.iter().enumerate() {
            let frame = core::frame(note.time_ms);
            for (r, &(start, end)) in ranges.iter().enumerate() {
                if core::frame(start) < frame && frame <= core::frame(end) && range_of[i] == -1 {
                    range_of[i] = r as i32;
                }
            }
        }
        let combo_factor = combo_factors(master, &times, &judgements);
        let mut judge_frac = [0.0; 7];
        for kind in 1..=6 {
            judge_frac[7 - kind] = f(master
                .live
                .judge_percent
                .get(&(kind as i32))
                .copied()
                .unwrap_or(0.0)
                / 100.0);
        }
        let mut span = (0, 0);
        if !ranges.is_empty() {
            let first = ranges.iter().map(|r| r.0).min().unwrap();
            let last = ranges.iter().map(|r| r.1).max().unwrap();
            let end = play
                .frames
                .iter()
                .position(|frame| {
                    frame.time_ms as f64 >= last as f64 + master.live.after_ms + 1200.0
                })
                .unwrap_or(play.frames.len());
            let start = play
                .frames
                .iter()
                .position(|frame| frame.time_ms >= first - 4500)
                .map(|i| i.saturating_sub(1))
                .unwrap_or(0);
            span = (start, end);
        }
        let luck = setup.missions.iter().take(ranges.len()).any(|&m| m == 2);
        let difficulty_factor = f(f(f(crate::num::i32(full.level - 5.0) as f64) * f(0.005)) + 1.0);
        let music_length = full.last_timing + 1000;
        Ok(Self {
            music_type: 0,
            full,
            setup,
            play,
            accuracy,
            notes,
            times,
            note_frac,
            judgements,
            range_of,
            range_percent,
            luck,
            difficulty_factor,
            music_length,
            combo_factor,
            judge_frac,
            span,
            deterministic_ranges: OnceLock::new(),
            factor_operations: OnceLock::new(),
            curves: HashMap::default(),
            curve_bytes: 0,
            frame_streams: HashMap::default(),
            next_stream: 0,
            performer_ids: HashMap::default(),
            performer_json: Vec::new(),
            seeded: HashMap::default(),
            contexts: HashMap::default(),
            context_order: std::collections::VecDeque::new(),
            context_bytes: 0,
            context_records: HashMap::default(),
            weights: HashMap::default(),
            weight_order: std::collections::VecDeque::new(),
            weight_bytes: 0,
        })
    }
    pub(crate) fn converted_judgements(&self, judgement: &[usize]) -> Option<Self> {
        let indices: HashMap<_, _> = self
            .notes
            .iter()
            .enumerate()
            .map(|(i, note)| (note.id, i))
            .collect();
        let mut play = self.play.clone();
        for frame in &mut play.frames {
            for note in &mut frame.judged {
                note.judgement = judgement[*indices.get(&note.note_id)?] as i32;
            }
        }
        Some(Self {
            music_type: self.music_type,
            full: self.full.clone(),
            setup: self.setup.clone(),
            play,
            accuracy: self.accuracy,
            notes: self.notes.clone(),
            times: self.times.clone(),
            note_frac: self.note_frac.clone(),
            judgements: judgement.to_vec(),
            range_of: self.range_of.clone(),
            range_percent: self.range_percent.clone(),
            luck: self.luck,
            difficulty_factor: self.difficulty_factor,
            music_length: self.music_length,
            combo_factor: self.combo_factor.clone(),
            judge_frac: self.judge_frac,
            span: self.span,
            deterministic_ranges: self.deterministic_ranges.clone(),
            factor_operations: self.factor_operations.clone(),
            curves: HashMap::default(),
            curve_bytes: 0,
            frame_streams: HashMap::default(),
            next_stream: 0,
            performer_ids: HashMap::default(),
            performer_json: Vec::new(),
            seeded: HashMap::default(),
            contexts: HashMap::default(),
            context_order: Default::default(),
            context_bytes: 0,
            context_records: HashMap::default(),
            weights: HashMap::default(),
            weight_order: Default::default(),
            weight_bytes: 0,
        })
    }
    pub(crate) fn params(&self) -> Params {
        Params {
            music_type: self.music_type,
            power: 1,
            level: self.full.level,
            converted: self.full.converted,
            music_length: self.music_length,
            assist: 1.0,
        }
    }
}
fn readers(
    master: &Master,
    deck: &[Performer],
    music_type: i32,
) -> Result<(f64, Vec<MissRule>), String> {
    let initial = master.live.life_base as i32;
    let factory = Factory {
        master,
        deck,
        initial_life: initial,
        music_type,
        initial_time: 0,
    };
    let mut life = Life::new(initial, master.live.damage, 1000);
    let mut random = Random::new(0);
    let mut ctx = CheckContext {
        previous_rank: None,
        life: &mut life,
        random: &mut random,
        time: 0,
        combo: 0,
        judged: &[],
        events: &[],
        gk: None,
        record: true,
    };
    let reads = |group: i32, kind: i32, value: Option<f64>| {
        master
            .condition_sets
            .get(&group)
            .into_iter()
            .flatten()
            .any(|set| {
                set.iter().any(|id| {
                    master.conditions.get(id).is_some_and(|c| {
                        c.kind == kind && value.is_none_or(|v| c.values.first() == Some(&v))
                    })
                })
            })
    };
    let mut rush = 0.0;
    let mut misses = Vec::new();
    for (k, p) in deck.iter().enumerate() {
        if p.gekisou_skill.is_none() && p.mission_only.unwrap_or(0) == 0 {
            continue;
        }
        for &(id, level) in &p.gekisou_support_skills {
            let Some(skill) = master.gekiso_support_skills.get(&id) else {
                continue;
            };
            if skill.mission_type != 2 && skill.mission_type != 4 {
                continue;
            }
            for row in &skill.effects {
                if row.level != level {
                    continue;
                }
                let mut chance = || {
                    factory
                        .group(row.condition_group, k)?
                        .map(|mut checker| checker.chance(&mut ctx))
                        .unwrap_or(Ok(1.0))
                };
                if row.kind == 2000 && reads(row.trigger_group, 7021, None) {
                    if chance()? > 0.5 {
                        rush = f(rush
                            + f(note_factor_mill(f(f(row.value as f64) / 10000.0)) as f64
                                / 100000.0));
                    }
                } else if row.kind == 11003 && reads(row.trigger_group, 7000, Some(0.0)) {
                    let chance = chance()?;
                    if chance > 0.0 {
                        misses.push(MissRule {
                            value: row.value,
                            chance,
                        });
                    }
                }
            }
        }
    }
    Ok((rush, misses))
}
fn frame_stream(record: &LuckRecord, frames: &[play::Frame]) -> Vec<luck::Frame> {
    record
        .frames
        .iter()
        .enumerate()
        .map(|(i, frame)| luck::Frame {
            time: frames[i].time_ms,
            states: frame.states.to_vec(),
            updates: frame.updates.clone(),
            playing: frame.playing,
            judged: frames[i]
                .judged
                .iter()
                .map(|j| luck::Judged {
                    id: j.note_id,
                    time: j.time_ms,
                    scores: [0; 4],
                })
                .collect(),
        })
        .collect()
}
fn lottery_input(
    master: &Master,
    record: &LuckRecord,
    frames: Arc<Vec<luck::Frame>>,
    ranges: Vec<usize>,
    misses: &[MissRule],
) -> luck::Input {
    luck::Input {
        maximum: master.live.gekiso.gauge_max as usize,
        rush_maximum: master.live.gekiso.gauge_max_rush as usize,
        base: master.live.gekiso.base.clone(),
        lots: master.live.gekiso.lots.clone(),
        ranges,
        frames,
        notes: record
            .notes
            .iter()
            .map(|n| luck::Note {
                frame: n.frame,
                range: n.range,
                op: n.op,
                judgement: n.judgement,
                time: n.time,
                speed: n.speed,
                buff: n.buff,
            })
            .collect(),
        actions: record
            .actions
            .iter()
            .map(|a| luck::Action {
                frame: a.frame,
                range: a.range as usize,
                kind: a.kind,
                value: a.value,
                chance: a.chance,
            })
            .collect(),
        miss: misses.iter().map(|m| (m.value, m.chance)).collect(),
        modulus: 0,
        initial: 0,
    }
}
pub fn context(
    master: &Master,
    chart: &mut Chart,
    deck: &[Performer],
    seeds: &[i32],
) -> Result<Context, String> {
    if chart.luck && seeds.is_empty() {
        return expected(master, chart, deck);
    }
    context_fixed(master, chart, deck, seeds)
}
/// A context that needs no lottery expectation (a lottery-free chart, or fixed seeds): it only reads the chart.
pub fn context_fixed(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
) -> Result<Context, String> {
    let n = chart.notes.len();
    if chart.luck && seeds.len() > 1 {
        let reduced: Vec<_> = deck.iter().map(Performer::gekiso_only).collect();
        let jobs: Vec<_> = seeds
            .chunks(seeds.len().div_ceil(crate::parallel::preparation_workers()))
            .collect();
        let draws: Vec<_> = crate::parallel::map(&jobs, |batch| {
            let mut initial = Model::new(
                master,
                &reduced,
                &chart.full.notes,
                &chart.full.events,
                chart.params(),
                Some(&chart.setup),
                (seeds[0], false),
            )?;
            chart.fixed_life(&mut initial);

            batch
                .iter()
                .map(|&seed| {
                    context_fixed_observed(master, chart, deck, &[seed], false, Some(&initial))
                        .map(|(context, _)| Arc::new(context))
                })
                .collect::<Result<Vec<_>, String>>()
        })?
        .into_iter()
        .flatten()
        .collect();
        let last = draws.last().unwrap();
        let average = |select: fn(&Context) -> &[f64]| {
            (0..n)
                .map(|i| draws.iter().map(|draw| select(draw)[i]).sum::<f64>() / draws.len() as f64)
                .collect()
        };
        return Ok(Context {
            judgement: last.judgement.clone(),
            gk: last.gk.clone(),
            up: average(|draw| &draw.up),
            luck: average(|draw| &draw.luck),
            factor_commands: draws
                .iter()
                .map(|draw| draw.factor_commands)
                .max()
                .unwrap_or(0),
            factor_peak: draws
                .iter()
                .map(|draw| draw.factor_peak)
                .fold(0.0, f64::max),
            rush_changes: draws.iter().map(|draw| draw.rush_changes).sum(),
            seeds: draws.len(),
            draws,
            nominal: None,
        });
    }
    context_fixed_observed(master, chart, deck, seeds, false, None).map(|(context, _)| context)
}
pub(crate) fn context_fixed_rush(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    seed: i32,
) -> Result<(Context, Vec<bool>), String> {
    context_fixed_observed(master, chart, deck, &[seed], true, None)
}
fn context_fixed_observed<'a>(
    master: &'a Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
    observe_rush: bool,
    template: Option<&Model<'a>>,
) -> Result<(Context, Vec<bool>), String> {
    let n = chart.notes.len();
    let mut rush_support = vec![false; if observe_rush { n } else { 0 }];
    let mut judgement = chart.judgements.clone();
    let mut gk = vec![1.0; n];
    let mut up = vec![0.0; n];
    let mut luck = vec![0.0; n];
    let runs = if chart.luck {
        seeds.to_vec()
    } else {
        vec![seeds.first().copied().unwrap_or(0)]
    };
    let reduced: Vec<_> = deck.iter().map(Performer::gekiso_only).collect();
    let index: HashMap<_, _> = chart
        .notes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, i))
        .collect();
    let mut factor_commands = 0;
    let mut factor_peak = 0.0_f64;
    let mut rush_changes = 0;
    let make_model = |seed| -> Result<Model<'a>, String> {
        let mut model = Model::new(
            master,
            &reduced,
            &chart.full.notes,
            &chart.full.events,
            chart.params(),
            Some(&chart.setup),
            (seed, false),
        )?;
        if observe_rush {
            model.observe_rush_support()?;
        }
        chart.fixed_life(&mut model);
        Ok(model)
    };
    let initial = if template.is_none() && runs.len() > 1 {
        Some(make_model(runs[0])?)
    } else {
        None
    };
    let template = template.or(initial.as_ref());
    for &seed in &runs {
        let mut model = if let Some(template) = template {
            let mut model = template.clone();
            model.random = Random::new(seed);
            model
        } else {
            make_model(seed)?
        };
        model.score.score.calculator.factors_only = true;
        for k in chart.span.0..chart.span.1 {
            let frame = &chart.play.frames[k];
            model.frame(frame.time_ms, &frame.judged, chart.play.dt[k])?;
        }
        if observe_rush {
            model.rush_support_commands.sort_by_key(|&(time, _)| time);
            let mut active = 0;
            let states: Vec<_> = model
                .rush_support_commands
                .iter()
                .map(|&(time, delta)| {
                    active += delta;
                    (time, active)
                })
                .collect();
            for (i, note) in chart.notes.iter().enumerate() {
                let at = states.partition_point(|&(time, _)| time <= note.time_ms);
                rush_support[i] |= at > 0 && states[at - 1].1 > 0;
            }
        }
        rush_changes += model.score.score.rush_factor_changes();
        let commands = model.score.score.note_factor_commands();
        factor_commands = factor_commands.max(commands.len());
        let mut prefix = 1.0;
        for &(_, mill) in &commands {
            prefix += f(f(mill as f64) / 100000.0);
            factor_peak = factor_peak.max(prefix.abs());
        }
        let mut note_up = vec![1.0; n];
        let mut note_luck = vec![1.0; n];
        for (id, o) in &model.score.score.calculator.observations {
            if let Some(&i) = index.get(id) {
                judgement[i] = (7 - o.score_type) as usize;
                gk[i] = o.gk;
                note_up[i] = o.note;
                note_luck[i] = o.luck;
            }
        }
        for i in 0..n {
            up[i] += note_luck[i] * note_up[i];
            luck[i] += note_luck[i];
        }
    }
    for i in 0..n {
        up[i] /= runs.len() as f64;
        luck[i] /= runs.len() as f64;
    }
    Ok((
        Context {
            judgement,
            gk,
            up,
            luck,
            factor_commands,
            factor_peak,
            rush_changes,
            seeds: runs.len(),
            draws: Vec::new(),
            nominal: None,
        },
        rush_support,
    ))
}
fn expected(master: &Master, chart: &mut Chart, deck: &[Performer]) -> Result<Context, String> {
    let n = chart.notes.len();
    let index: HashMap<_, _> = chart
        .notes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, i))
        .collect();
    let mut judgement = chart.judgements.clone();
    let mut gk = vec![1.0; n];
    let mut note_up = vec![1.0; n];
    let reduced: Vec<_> = deck.iter().map(Performer::gekiso_only).collect();
    let mut model = Model::new(
        master,
        &reduced,
        &chart.full.notes,
        &chart.full.events,
        chart.params(),
        Some(&chart.setup),
        (0, true),
    )?;
    model.score.score.calculator.factors_only = true;
    let simulated = &chart.play.frames[chart.span.0..chart.span.1];
    for (k, frame) in simulated.iter().enumerate() {
        model.frame(
            frame.time_ms,
            &frame.judged,
            chart.play.dt[chart.span.0 + k],
        )?;
    }
    for (id, o) in &model.score.score.calculator.observations {
        if let Some(&i) = index.get(id) {
            judgement[i] = (7 - o.score_type) as usize;
            gk[i] = o.gk;
            note_up[i] = o.note;
        }
    }
    let record = model.record.take().unwrap();
    let (rush, misses) = readers(master, &reduced, chart.music_type)?;
    let ranges = chart
        .setup
        .missions
        .iter()
        .enumerate()
        .take(chart.full.fevers.len().min(3))
        .filter(|(_, m)| **m == 2)
        .map(|(i, _)| i)
        .collect();
    let mut signature = Vec::new();
    for a in &record.actions {
        signature.extend([
            a.frame as u64,
            a.range as u64,
            a.kind as u64,
            a.value as u64,
            a.limit as u64,
            a.chance.to_bits(),
        ]);
    }
    signature.push(u64::MAX);
    for m in &misses {
        signature.extend([m.value as u64, m.chance.to_bits()]);
    }
    signature.push(u64::MAX);
    for note in &record.notes {
        signature.extend([
            note.frame as u64,
            note.judgement as u64,
            note.speed.to_bits(),
            note.buff as u64,
        ]);
    }
    signature.push(u64::MAX);
    let mut stream_key = Vec::with_capacity(record.frames.len() * 6);
    for frame in &record.frames {
        stream_key.push(frame.states.len() as u64);
        stream_key.extend(frame.states.iter().map(|&s| s as u64));
        stream_key.push(frame.updates.len() as u64);
        stream_key.extend(frame.updates.iter().map(|&i| i as u64));
        stream_key.push(frame.playing as u64);
    }
    let (stream_id, stream) = if let Some((id, stream)) = chart.frame_streams.get(&stream_key) {
        (*id, Arc::clone(stream))
    } else {
        let stream = Arc::new(frame_stream(&record, simulated));
        if chart.frame_streams.len() >= 16 {
            chart.frame_streams.clear();
        }
        let id = chart.next_stream;
        chart.next_stream += 1;
        chart
            .frame_streams
            .insert(stream_key, (id, Arc::clone(&stream)));
        (id, stream)
    };
    signature.push(stream_id);
    let recorded = if let Some(record) = chart.curves.get(&signature) {
        Arc::clone(record)
    } else {
        let input = lottery_input(master, &record, stream, ranges, &misses);
        let curve = luck::run(&input)?;
        // Frame streams are shared; each entry keeps its notes and curve. A Luck
        // search over every card builds a few hundred.
        let bytes = input.retained_bytes()
            + signature.capacity() * 8
            + curve.curve.len() * 96
            + curve.residues.capacity() * 8
            + std::mem::size_of::<Recorded>();
        let record = Arc::new(Recorded { input, curve });
        const CACHE_BYTES: usize = 32 * 1024 * 1024;
        if bytes <= CACHE_BYTES {
            if chart.curve_bytes + bytes > CACHE_BYTES {
                chart.curves.clear();
                chart.curve_bytes = 0;
            }
            chart.curve_bytes += bytes;
            chart.curves.insert(signature, Arc::clone(&record));
        }
        record
    };
    let bonus = (100 + master.live.gekiso.rush_percent).min(200) as f64 / 100.0 - 1.0;
    let mut up = vec![0.0; n];
    let mut luck = vec![0.0; n];
    let mut probabilities: [Vec<f64>; 4] = std::array::from_fn(|_| vec![0.0; n]);
    for i in 0..n {
        let [pr, ps, both] = recorded
            .curve
            .curve
            .get(&chart.notes[i].id)
            .copied()
            .unwrap_or([0.0; 3]);
        luck[i] = 1.0 + bonus * pr;
        up[i] = note_up[i] * luck[i] + rush * (ps + bonus * both);
        probabilities[0][i] = (1.0 - pr - ps + both).max(0.0);
        probabilities[1][i] = (pr - both).max(0.0);
        probabilities[2][i] = (ps - both).max(0.0);
        probabilities[3][i] = both.max(0.0);
    }
    Ok(Context {
        judgement,
        gk,
        up,
        luck,
        seeds: 0,
        factor_commands: 0,
        factor_peak: 0.0,
        rush_changes: 0,
        draws: Vec::new(),
        nominal: Some(Nominal {
            probabilities,
            note_up,
            rush_up: rush,
            rush_factor: f(1.0 + bonus),
            recorded,
        }),
    })
}
pub fn rush_only_factor(master: &Master, performer: &Performer) -> Result<Option<f64>, String> {
    rush_factor_for_music(master, performer, 0)
}
pub(crate) fn rush_factor_for_music(
    master: &Master,
    performer: &Performer,
    music_type: i32,
) -> Result<Option<f64>, String> {
    for &(id, level) in &performer.gekisou_support_skills {
        let Some(skill) = master.gekiso_support_skills.get(&id) else {
            continue;
        };
        if skill.mission_type != 2 && skill.mission_type != 4 {
            return Ok(None);
        }
        if skill
            .effects
            .iter()
            .any(|r| r.level == level && r.kind != 2000)
        {
            return Ok(None);
        }
    }
    let mut performer = performer.clone();
    if performer.mission_only.is_none_or(|m| m == 0) {
        performer.mission_only = Some(1);
    }
    let (rush, misses) = readers(master, &[performer], music_type)?;
    Ok(if misses.is_empty() { Some(rush) } else { None })
}
pub fn settle_slack(chart: &Chart) -> usize {
    chart
        .range_percent
        .iter()
        .enumerate()
        .filter(|(i, _)| chart.setup.missions[*i] == 2)
        .count()
}
fn rank_remainder(
    chart: &Chart,
    input: &mut luck::Input,
    range: usize,
    outcomes: &[f64],
    cache: &mut HashMap<Vec<i32>, f64>,
    index: &HashMap<i32, usize>,
) -> Result<f64, String> {
    let percent = chart.range_percent[range];
    let (mut divisor, mut numerator) = (100, percent);
    while numerator != 0 {
        let next = divisor % numerator;
        divisor = numerator;
        numerator = next;
    }
    let modulus = (100 / divisor) as usize;
    if modulus == 1 {
        return Ok(0.0);
    }
    let first = input
        .frames
        .iter()
        .position(|frame| frame.states[range] == 3)
        .ok_or("luck range without start")?;
    let last = input
        .frames
        .iter()
        .enumerate()
        .skip(first + 1)
        .find(|(_, frame)| frame.states[range] == 8)
        .map(|(i, _)| i)
        .unwrap_or(input.frames.len() - 1);
    let observed: HashSet<_> = input.frames[first..=last]
        .iter()
        .flat_map(|frame| frame.judged.iter().map(|note| note.id))
        .collect();
    let mut initial = 0;
    let mut signature = vec![range as i32];
    for (i, note) in chart.notes.iter().enumerate() {
        if chart.range_of[i] != range as i32 {
            continue;
        }
        if !observed.contains(&note.id) {
            initial = (initial + outcomes[4 * i] as usize) % modulus;
        }
        for mask in 0..4 {
            signature.push((outcomes[4 * i + mask] as i32) % (modulus as i32));
        }
    }
    if let Some(&value) = cache.get(&signature) {
        return Ok(value);
    }
    input.ranges.clear();
    input.ranges.push(range);
    input.modulus = modulus;
    input.initial = initial;
    for frame in Arc::make_mut(&mut input.frames) {
        for note in &mut frame.judged {
            note.scores = if let Some(&i) = index
                .get(&note.id)
                .filter(|&&i| chart.range_of[i] == range as i32)
            {
                std::array::from_fn(|mask| outcomes[4 * i + mask] as i32)
            } else {
                [0; 4]
            };
        }
    }
    let result = luck::run(input)?;
    let mut value = 0.0;
    for (residue, &probability) in result.residues.iter().enumerate() {
        value += probability * ((residue as i32 * percent) % 100) as f64 / 100.0;
    }
    cache.insert(signature, value);
    Ok(value)
}
struct Active {
    end: i32,
    channel: usize,
    delta: i32,
}
struct Factors {
    channel: [f64; 7],
    active: Vec<Active>,
    factor: f64,
    convert_end: Option<i32>,
    convert_judgements: Vec<usize>,
    convert_left: f64,
}
impl Factors {
    fn expire(&mut self, time: i64) {
        for i in (0..self.active.len()).rev() {
            if self.active[i].end as i64 <= time {
                let active = self.active.remove(i);
                let diff = f(active.delta as f64 / 100000.0);
                if active.channel == 0 {
                    self.factor = f(self.factor - diff);
                } else {
                    self.channel[active.channel] = f(self.channel[active.channel] - diff);
                }
            }
        }
        if self.convert_end.is_some_and(|end| end as i64 <= time) {
            self.convert_end = None;
        }
    }
}
type ScoreRuns = Vec<(usize, usize, usize)>;
pub(crate) struct NoteScorePlan {
    plan: Arc<live::OrderPlan>,
    states: Vec<[f64; 2]>,
    segments: Vec<(usize, ScoreRuns)>,
    regions: Vec<(usize, usize, usize)>,
}
impl NoteScorePlan {
    pub(crate) fn new(chart: &Chart, slots: &[Slot]) -> Option<Self> {
        if slots.iter().any(|slot| {
            slot.convert.is_some()
                || slot
                    .effects
                    .iter()
                    .any(|effect| effect.gate.kind != 0 || effect.delta < 0)
        }) {
            return None;
        }
        let n = chart.notes.len();
        let plan = live::plan(slots);
        let event_times: Vec<_> = chart.full.events.iter().map(|event| event.1).collect();
        let mut firing: Vec<_> = (0..event_times.len().min(5)).collect();
        firing.sort_by_key(|&index| (event_times[index], index));
        let mut keys = HashMap::default();
        let mut states = Vec::new();
        let mut segments = Vec::new();
        for &index in &plan.representatives {
            let order = &live::orders()[index];
            let mut factors = Factors {
                channel: [0.0; 7],
                active: Vec::new(),
                factor: f(0.0),
                convert_end: None,
                convert_judgements: Vec::new(),
                convert_left: 0.0,
            };
            let (mut cursor, mut at) = (0, 0);
            let mut runs = Vec::new();
            while at < n {
                let time = chart.times[at] as i64;
                while cursor < firing.len() && event_times[firing[cursor]] as i64 <= time {
                    let event = firing[cursor];
                    let start = event_times[event];
                    factors.expire(start as i64);
                    let slot = &slots[order[event]];
                    for effect in &slot.effects {
                        let end = start + crate::num::window_ms(effect.seconds, slot.extension_ms);
                        let diff = f(effect.delta as f64 / 100000.0);
                        let channels = if effect.kind == 2000 {
                            vec![0]
                        } else {
                            effect.judgements.clone()
                        };
                        for channel in channels {
                            if channel == 0 {
                                factors.factor = f(factors.factor + diff);
                            } else {
                                factors.channel[channel] = f(factors.channel[channel] + diff);
                            }
                            factors.active.push(Active {
                                end,
                                channel,
                                delta: effect.delta,
                            });
                        }
                    }
                    cursor += 1;
                }
                factors.expire(time);
                let values = [
                    f(factors.factor + factors.channel[5]),
                    f(factors.factor + factors.channel[6]),
                ];
                let next = states.len();
                let state = *keys.entry(values.map(f64::to_bits)).or_insert_with(|| {
                    states.push(values);
                    next
                });
                // Expire at the next actual note, preserving reverse removal order.
                let boundary = firing
                    .get(cursor)
                    .map(|&event| event_times[event])
                    .into_iter()
                    .chain(factors.active.iter().map(|active| active.end))
                    .min();
                let end = boundary
                    .map_or(n, |time| chart.times.partition_point(|&note| note < time))
                    .max(at + 1);
                runs.push((at, end, state));
                at = end;
            }
            segments.push((index, runs));
        }
        if states.len() >= plan.representatives.len() {
            return None;
        }
        let mut regions = Vec::new();
        let mut at = 0;
        while at < n {
            let range = chart.range_of[at];
            let mut end = at + 1;
            while end < n && chart.range_of[end] == range {
                end += 1;
            }
            if range >= 0 {
                regions.push((at, end, range as usize));
            }
            at = end;
        }
        Some(Self {
            plan,
            states,
            segments,
            regions,
        })
    }
    pub(crate) fn score(
        &self,
        master: &Master,
        chart: &Chart,
        ctx: &Context,
        power: i32,
    ) -> Result<Option<Scores>, String> {
        if ctx.seeds != 1
            || ctx.nominal.is_some()
            || !ctx.draws.is_empty()
            || master.live.life_base <= 0.0
            || ctx
                .judgement
                .iter()
                .any(|&j| ![5, 6].contains(&j) || master.live.damage[j] != 0.0)
            || ctx
                .luck
                .iter()
                .any(|&value| !value.is_finite() || value <= 0.0)
        {
            return Ok(None);
        }
        let n = chart.notes.len();
        let generation = crate::cancellation_generation();
        let t = f(f(master.live.adjustment * power as f64) * chart.difficulty_factor);
        let combo = chart.combo(master, &ctx.judgement);
        let base: Vec<_> = (0..n)
            .map(|i| {
                let a = f(chart.note_frac[i] * t);
                let combo = f(ctx.gk[i] * combo[i]);
                f(f(chart.judge_frac[ctx.judgement[i]] * a) * combo)
            })
            .collect();
        let count = chart.full.converted as f64;
        let mut prefixes = Vec::new();
        for state in &self.states {
            let mut prefix = Vec::with_capacity(n + 1);
            let mut sum = 0.0;
            prefix.push(0.0);
            for i in 0..n {
                let live = state[ctx.judgement[i] - 5];
                let up = f(f(ctx.up[i] / ctx.luck[i]) + live);
                let value = f(f(f(ctx.luck[i]) * f(base[i] * up)) / count).floor();
                sum += value;
                if !value.is_finite() || value < 0.0 || sum > 9007199254740991.0 {
                    return Ok(None);
                }
                prefix.push(sum);
            }
            prefixes.push(prefix);
        }
        let mut scores = vec![0.0; 120];
        for (index, runs) in &self.segments {
            if generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
            let mut score = 0.0;
            let mut range_score = vec![0.0; chart.range_percent.len()];
            for &(start, end, state) in runs {
                let prefix = &prefixes[state];
                score += prefix[end] - prefix[start];
                if score > 9007199254740991.0 {
                    return Ok(None);
                }
                for &(left, right, range) in &self.regions {
                    let (left, right) = (left.max(start), right.min(end));
                    if left < right {
                        range_score[range] += prefix[right] - prefix[left];
                    }
                }
            }
            for (range, value) in range_score.into_iter().enumerate() {
                score += (value * chart.range_percent[range] as f64 / 100.0).floor();
                if !score.is_finite() || score.abs() > 9007199254740991.0 {
                    return Ok(None);
                }
            }
            if score > 9007199254740991.0 {
                return Ok(None);
            }
            scores[*index] = score;
        }
        for (index, &source) in self.plan.sources.iter().enumerate() {
            scores[index] = scores[source];
        }
        Ok(Some(live::summarize(power, scores)))
    }
}
pub fn score_orders(
    master: &Master,
    chart: &Chart,
    ctx: &Context,
    power: i32,
    slots: &[Slot],
    settle: bool,
) -> Result<Scores, String> {
    score_orders_selected(master, chart, ctx, power, slots, settle, None)
}
#[allow(clippy::too_many_arguments)]
fn score_orders_selected(
    master: &Master,
    chart: &Chart,
    ctx: &Context,
    power: i32,
    slots: &[Slot],
    settle: bool,
    selected: Option<&[usize]>,
) -> Result<Scores, String> {
    if !ctx.draws.is_empty() {
        let plan = selected
            .is_none()
            .then(|| NoteScorePlan::new(chart, slots))
            .flatten();
        let mut scores = vec![0.0; 120];
        for draw in &ctx.draws {
            let planned = plan
                .as_ref()
                .map(|plan| plan.score(master, chart, draw, power))
                .transpose()?
                .flatten();
            let result = if let Some(scores) = planned {
                scores
            } else {
                score_orders_selected(master, chart, draw, power, slots, settle, selected)?
            };
            for (order, &value) in result.scores.iter().enumerate() {
                scores[order] += value / ctx.seeds as f64;
            }
        }
        return Ok(live::summarize(power, scores));
    }
    if ctx.seeds != 1 && ctx.nominal.is_none() {
        return Err(
            "Gekisou expectation requires outcome probabilities or individual draws".into(),
        );
    }
    let generation = crate::cancellation_generation();
    let n = chart.notes.len();
    let t = f(f(master.live.adjustment * power as f64) * chart.difficulty_factor);
    let count = chart.full.converted as f64;
    let onus = master.live.life_onus;
    let damage = &master.live.damage;
    let event_times: Vec<_> = chart.full.events.iter().map(|e| e.1).collect();
    let mut firing: Vec<_> = (0..event_times.len().min(5)).collect();
    firing.sort_by_key(|&i| (event_times[i], i));
    let note_a: Vec<_> = chart.note_frac.iter().map(|&frac| f(frac * t)).collect();
    let normal_combo = chart.combo(master, &ctx.judgement);
    let combo: Vec<_> = ctx
        .gk
        .iter()
        .zip(normal_combo.iter())
        .map(|(&g, &c)| f(g * c))
        .collect();
    let mut outcomes = vec![0.0; n * 4];
    let mut remainders = HashMap::default();
    let ranges = chart.range_percent.len();
    let mut scores = vec![0.0; 120];
    let mut range_score = vec![0.0; ranges];
    let plan = live::plan(slots);
    let mut settlement = ctx
        .nominal
        .as_ref()
        .map(|nominal| nominal.recorded.input.clone());
    let note_index: HashMap<_, _> = chart
        .notes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, i))
        .collect();
    let needs_life = ctx.judgement.iter().any(|&j| damage[j] != 0.0)
        || slots
            .iter()
            .flat_map(|s| &s.effects)
            .any(|e| e.gate.kind != 0);
    let mut life_cache = HashMap::<[i32; 5], (Vec<i32>, Vec<i32>)>::default();
    for &order_index in &plan.representatives {
        if selected.is_some_and(|orders| !orders.contains(&order_index)) {
            continue;
        }
        if generation != crate::cancellation_generation() {
            return Err("cancelled".into());
        }
        let order = &live::orders()[order_index];
        let recoveries = order.map(|slot| slots.get(slot).map_or(0, |s| s.recovery));
        let lives = if needs_life {
            Some(&*life_cache.entry(recoveries).or_insert_with(|| {
                let mut life = Life::new(master.live.life_base as i32, *damage, chart.music_length);
                let mut note = vec![0; n];
                let mut gate = vec![0; event_times.len()];
                let (mut i, mut event) = (0, 0);
                while i < n || event < firing.len() {
                    let frame = (if i < n {
                        core::frame(chart.times[i])
                    } else {
                        i32::MAX
                    })
                    .min(if event < firing.len() {
                        core::frame(event_times[firing[event]])
                    } else {
                        i32::MAX
                    });
                    while i < n && core::frame(chart.times[i]) == frame {
                        life.note_damage(chart.times[i], ctx.judgement[i]);
                        note[i] = life.at(chart.times[i]);
                        i += 1;
                    }
                    let start = event;
                    while event < firing.len() && core::frame(event_times[firing[event]]) == frame {
                        let key = firing[event];
                        life.recovery(event_times[key], slots[order[key]].recovery, true);
                        event += 1;
                    }
                    for &key in &firing[start..event] {
                        gate[key] = life.at(live::frame_time(event_times[key]));
                    }
                }
                (note, gate)
            }))
        } else {
            None
        };
        let mut factors = Factors {
            channel: [0.0; 7],
            active: Vec::new(),
            factor: f(0.0),
            convert_end: None,
            convert_judgements: Vec::new(),
            convert_left: 0.0,
        };
        let mut cursor = 0;
        let mut score = 0.0;
        range_score.fill(0.0);
        for i in 0..=n {
            let time = if i < n {
                chart.times[i] as i64
            } else {
                1 << 53
            };
            while cursor < firing.len() && event_times[firing[cursor]] as i64 <= time {
                let event = firing[cursor];
                let start = event_times[event];
                factors.expire(start as i64);
                if let Some(slot) = slots.get(order[event]) {
                    for effect in &slot.effects {
                        if effect
                            .gate
                            .blocks(lives.map_or(master.live.life_base, |l| l.1[event] as f64))
                        {
                            continue;
                        }
                        let end = start + crate::num::window_ms(effect.seconds, slot.extension_ms);
                        let diff = f(effect.delta as f64 / 100000.0);
                        let channels = if effect.kind == 2000 {
                            vec![0]
                        } else {
                            effect.judgements.clone()
                        };
                        for channel in channels {
                            if channel == 0 {
                                factors.factor = f(factors.factor + diff);
                            } else {
                                factors.channel[channel] = f(factors.channel[channel] + diff);
                            }
                            factors.active.push(Active {
                                end,
                                channel,
                                delta: effect.delta,
                            });
                        }
                    }
                    if let Some(convert) = &slot.convert {
                        factors.convert_end = Some(
                            start
                                + crate::num::window_ms(
                                    slot.effects.first().map(|e| e.seconds).unwrap_or(5.0),
                                    slot.extension_ms,
                                ),
                        );
                        factors.convert_judgements = convert.judgements.clone();
                        factors.convert_left = convert.limit.unwrap_or(f64::INFINITY);
                    }
                }
                cursor += 1;
            }
            if i == n {
                break;
            }
            factors.expire(time);
            let mut judgement = ctx.judgement[i];
            if factors.convert_end.is_some()
                && factors.convert_left > 0.0
                && factors.convert_judgements.contains(&judgement)
            {
                judgement = 5;
                factors.convert_left -= 1.0;
            }
            let c = f(f(chart.judge_frac[judgement] * note_a[i]) * combo[i]);
            let live = f(factors.factor + factors.channel[judgement]);
            let mut x = 0.0;
            if let Some(nominal) = &ctx.nominal {
                for mask in 0..4 {
                    let up = f(f(
                        nominal.note_up[i] + if mask & 2 != 0 { nominal.rush_up } else { 0.0 }
                    ) + live);
                    let mut value = f(f(f(if mask & 1 != 0 {
                        nominal.rush_factor
                    } else {
                        1.0
                    }) * f(c * up))
                        / count)
                    .floor();
                    if lives.map_or(master.live.life_base as i32, |l| l.0[i]) <= 0 {
                        value = f(onus * value).floor();
                    }
                    outcomes[4 * i + mask] = value;
                    x += nominal.probabilities[mask][i] * value;
                }
            } else {
                let up = f(f(ctx.up[i] / ctx.luck[i]) + live);
                x = f(f(f(ctx.luck[i]) * f(c * up)) / count).floor();
                if lives.map_or(master.live.life_base as i32, |l| l.0[i]) <= 0 {
                    x = f(onus * x).floor();
                }
            }
            score += x;
            let range = chart.range_of[i];
            if range >= 0 {
                range_score[range as usize] += x;
            }
        }
        for (range, &value) in range_score.iter().enumerate() {
            if ctx.nominal.is_some() && chart.setup.missions[range] == 2 {
                let remainder = if settle {
                    rank_remainder(
                        chart,
                        settlement.as_mut().unwrap(),
                        range,
                        &outcomes,
                        &mut remainders,
                        &note_index,
                    )?
                } else {
                    0.0
                };
                score += value * chart.range_percent[range] as f64 / 100.0 - remainder;
            } else {
                score += (value * chart.range_percent[range] as f64 / 100.0).floor();
            }
        }
        scores[order_index] = score;
    }
    for (index, &source) in plan.sources.iter().enumerate() {
        scores[index] = scores[source];
    }
    Ok(live::summarize(power, scores))
}

/// Damage-bearing fixed-seed plays need per-order LIFE and skill replay.
/// Zero-seed Luck expectations retain their separate probability model.
pub fn uses_full_replay(master: &Master, chart: &Chart, seeds: &[i32]) -> bool {
    (!chart.luck || !seeds.is_empty())
        && (master.live.life_base <= 0.0
            || chart
                .judgements
                .iter()
                .any(|&j| master.live.damage[j] > 0.0))
}

/// Complete replay scores and mean range scores over the same orders and seeds.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayedScores {
    #[serde(flatten)]
    pub scores: Scores,
    pub range_scores: [f64; 3],
}

pub(crate) fn replay_order(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    power: i32,
    order_index: usize,
) -> Result<(f64, [f64; 3]), String> {
    let order = &live::orders()[order_index];
    let events = chart
        .full
        .events
        .iter()
        .map(|&(event, time)| {
            order
                .get(event)
                .copied()
                .map(|member| (member, time))
                .ok_or_else(|| format!("ordinary skill event {event} out of range"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut params = chart.params();
    params.power = power;
    params.music_type = chart.music_type;
    let mut model = Model::new(
        master,
        deck,
        &chart.full.notes,
        &events,
        params,
        Some(&chart.setup),
        (0, false),
    )?;
    chart.fixed_life(&mut model);
    let generation = crate::cancellation_generation();
    for (k, frame) in chart.play.frames.iter().enumerate() {
        if generation != crate::cancellation_generation() {
            return Err("cancelled".into());
        }
        model.frame(frame.time_ms, &frame.judged, chart.play.dt[k])?;
    }
    let mut ranges = [0.0; 3];
    if let Some(g) = &model.gk {
        for (score, state) in ranges.iter_mut().zip(&g.controller.states) {
            *score = state.score() as f64;
        }
    }
    Ok((model.score.score.score as f64, ranges))
}
type ReplayValue = (f64, [f64; 3]);
struct ReplayTree<'a> {
    chart: &'a Chart,
    events: &'a [Vec<(usize, i32)>],
    chronological: Vec<usize>,
    generation: u64,
}
impl ReplayTree<'_> {
    fn advance(&self, model: &mut Model<'_>, from: usize, to: usize) -> Result<(), String> {
        for k in from..to {
            if self.generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
            let frame = &self.chart.play.frames[k];
            model.frame(frame.time_ms, &frame.judged, self.chart.play.dt[k])?;
        }
        Ok(())
    }
    fn visit(
        &self,
        mut model: Model<'_>,
        orders: &[usize],
        from: usize,
        fixed: usize,
        values: &mut [ReplayValue],
    ) -> Result<(), String> {
        let representative = &self.events[orders[0]];
        let split = (fixed..self.chronological.len()).find(|&at| {
            let event = self.chronological[at];
            orders
                .iter()
                .any(|&order| self.events[order][event].0 != representative[event].0)
        });
        let until = split.map_or(self.chart.play.frames.len(), |at| {
            let time = representative[self.chronological[at]].1;
            self.chart
                .play
                .frames
                .partition_point(|frame| frame.time_ms < time)
        });
        self.advance(&mut model, from, until)?;
        if until == self.chart.play.frames.len() {
            let mut ranges = [0.0; 3];
            if let Some(g) = &model.gk {
                for (score, state) in ranges.iter_mut().zip(&g.controller.states) {
                    *score = state.score() as f64;
                }
            }
            for &order in orders {
                values[order] = (model.score.score.score as f64, ranges);
            }
            return Ok(());
        }
        let at = split.unwrap();
        let time = self.chart.play.frames[until].time_ms;
        let end = self
            .chronological
            .partition_point(|&event| representative[event].1 <= time);
        let mut groups = std::collections::BTreeMap::<Vec<usize>, Vec<usize>>::new();
        for &order in orders {
            let key = self.chronological[at..end]
                .iter()
                .map(|&event| self.events[order][event].0)
                .collect();
            groups.entry(key).or_default().push(order);
        }
        let mut groups = groups.into_values().peekable();
        while let Some(orders) = groups.next() {
            if groups.peek().is_none() {
                model.set_event_members(&self.events[orders[0]]);
                return self.visit(model, &orders, until, end, values);
            }
            let mut branch = model.clone();
            branch.set_event_members(&self.events[orders[0]]);
            self.visit(branch, &orders, until, end, values)?;
        }
        Ok(())
    }
}
/// Replay complete skill, conversion and LIFE state for each ordinary order
/// and requested lottery seed, retaining each note's original LIFE snapshot.
pub fn replay_orders(
    master: &Master,
    chart: &Chart,
    deck: &[Performer],
    seeds: &[i32],
    power: i32,
) -> Result<ReplayedScores, String> {
    if deck.len() != 5 {
        return Err("a team requires five performers".into());
    }
    if chart.luck && seeds.is_empty() {
        return Err("full Gekisou replay requires explicit lottery seeds".into());
    }
    let random_row = |rows: &[crate::master::SkillEffect], level: i32| {
        rows.iter().filter(|r| r.level == level).any(|row| {
            [
                row.trigger_group,
                row.condition_group,
                row.release_group,
                row.reset_group,
            ]
            .iter()
            .flat_map(|g| master.condition_sets.get(g).into_iter().flatten().flatten())
            .any(|id| master.conditions[id].kind == 4011)
        })
    };
    let random = chart.luck
        || deck.iter().any(|p| {
            p.live_skill.is_some_and(|(id, level)| {
                master
                    .live_skills
                    .get(&id)
                    .is_some_and(|rows| random_row(rows, level))
            }) || p.support_skills.iter().any(|&(id, level)| {
                master
                    .support_skills
                    .get(&id)
                    .is_some_and(|rows| random_row(rows, level))
            }) || p.gekisou_skill.is_some_and(|(id, level)| {
                master
                    .gekiso_skills
                    .get(&id)
                    .filter(|s| {
                        s.mission_type == 4 || chart.setup.missions.contains(&s.mission_type)
                    })
                    .is_some_and(|s| random_row(&s.effects, level))
            }) || p.gekisou_support_skills.iter().any(|&(id, level)| {
                master
                    .gekiso_support_skills
                    .get(&id)
                    .filter(|s| {
                        s.mission_type == 4 || chart.setup.missions.contains(&s.mission_type)
                    })
                    .is_some_and(|s| random_row(&s.effects, level))
            })
        });
    let runs = if seeds.is_empty() || !random {
        vec![seeds.first().copied().unwrap_or(0)]
    } else {
        seeds.to_vec()
    };
    let generation = crate::cancellation_generation();
    let mut event_orders = Vec::new();
    let mut indices = Vec::with_capacity(120);
    let mut cache = HashMap::default();
    for order in live::orders() {
        let events = chart
            .full
            .events
            .iter()
            .map(|&(event, time)| {
                order
                    .get(event)
                    .copied()
                    .map(|member| (member, time))
                    .ok_or_else(|| format!("ordinary skill event {event} out of range"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let index = *cache.entry(events.clone()).or_insert_with(|| {
            let index = event_orders.len();
            event_orders.push(events);
            index
        });
        indices.push(index);
    }
    let mut chronological: Vec<_> = (0..chart.full.events.len()).collect();
    chronological.sort_by_key(|&event| (chart.full.events[event].1, event));
    let tree = ReplayTree {
        chart,
        events: &event_orders,
        chronological,
        generation,
    };
    #[cfg(target_arch = "wasm32")]
    let jobs = vec![(0..event_orders.len()).collect::<Vec<_>>()];
    #[cfg(not(target_arch = "wasm32"))]
    let jobs = {
        let mut jobs = std::collections::BTreeMap::<usize, Vec<usize>>::new();
        for (order, events) in event_orders.iter().enumerate() {
            let member = tree
                .chronological
                .first()
                .map_or(0, |&event| events[event].0);
            jobs.entry(member).or_default().push(order);
        }
        jobs.into_values().collect::<Vec<_>>()
    };
    let batches = crate::parallel::map(&jobs, |orders| -> Result<_, String> {
        let mut totals = vec![(0.0, [0.0; 3]); event_orders.len()];
        for &seed in &runs {
            let mut params = chart.params();
            params.power = power;
            params.music_type = chart.music_type;
            let mut model = Model::new(
                master,
                deck,
                &chart.full.notes,
                &event_orders[orders[0]],
                params,
                Some(&chart.setup),
                (seed, false),
            )?;
            chart.fixed_life(&mut model);
            let mut values = vec![(0.0, [0.0; 3]); event_orders.len()];
            tree.visit(model, orders, 0, 0, &mut values)?;
            for &order in orders {
                totals[order].0 += values[order].0;
                for (total, value) in totals[order].1.iter_mut().zip(values[order].1) {
                    *total += value;
                }
            }
        }
        Ok(orders
            .iter()
            .map(|&order| {
                let (score, ranges) = totals[order];
                (
                    order,
                    (
                        score / runs.len() as f64,
                        ranges.map(|v| v / runs.len() as f64),
                    ),
                )
            })
            .collect::<Vec<_>>())
    })?;
    let mut replays = vec![(0.0, [0.0; 3]); event_orders.len()];
    for batch in batches {
        for (order, value) in batch {
            replays[order] = value;
        }
    }
    let scores = indices.iter().map(|&i| replays[i].0).collect();
    let range_scores = std::array::from_fn(|range| {
        indices.iter().map(|&i| replays[i].1[range]).sum::<f64>() / indices.len() as f64
    });
    Ok(ReplayedScores {
        scores: live::summarize(power, scores),
        range_scores,
    })
}

pub(crate) fn settle_extreme(
    master: &Master,
    chart: &Chart,
    ctx: &Context,
    power: i32,
    slots: &[Slot],
    upper: &Scores,
    maximum: bool,
) -> Result<f64, String> {
    let plan = live::plan(slots);
    let mut choices = plan.representatives.clone();
    choices.sort_unstable_by(|&a, &b| {
        if maximum {
            upper.scores[b].total_cmp(&upper.scores[a])
        } else {
            upper.scores[a].total_cmp(&upper.scores[b])
        }
    });
    let mut best = if maximum {
        f64::NEG_INFINITY
    } else {
        f64::INFINITY
    };
    let remainder = settle_slack(chart) as f64;
    for order in choices {
        if maximum && upper.scores[order] < best
            || !maximum && upper.scores[order] - remainder > best
        {
            break;
        }
        let exact = score_orders_selected(master, chart, ctx, power, slots, true, Some(&[order]))?;
        best = if maximum {
            best.max(exact.scores[order])
        } else {
            best.min(exact.scores[order])
        };
    }
    Ok(best)
}
