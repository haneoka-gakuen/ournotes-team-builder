//! Independent raw-JUST score programs provide a bound before complete replay.
use crate::{
    full::{conditions::Performer, model::Model},
    gekiso::{self, Context},
    master::Master,
    num::f,
};
use rustc_hash::FxHashMap as HashMap;
use std::sync::Arc;

pub(crate) fn slot_dominates(a: &crate::live::Slot, b: &crate::live::Slot) -> bool {
    if a.effects
        .iter()
        .chain(&b.effects)
        .any(|e| e.delta < 0 || e.gate.kind != 0)
        || a.convert.is_some()
        || b.convert.is_some()
    {
        return false;
    }
    let mut times = vec![0];
    for slot in [a, b] {
        times.extend(
            slot.effects
                .iter()
                .map(|e| crate::num::window_ms(e.seconds, slot.extension_ms)),
        );
    }
    times.sort_unstable();
    times.dedup();
    for time in times {
        for judgement in [5, 6] {
            let value = |slot: &crate::live::Slot| {
                slot.effects
                    .iter()
                    .filter(|e| {
                        crate::num::window_ms(e.seconds, slot.extension_ms) > time
                            && (e.kind == 2000 || e.judgements.contains(&judgement))
                    })
                    .map(|e| e.delta as i64)
                    .sum::<i64>()
            };
            if value(a) < value(b) {
                return false;
            }
        }
    }
    true
}

struct Effect {
    values: Vec<f64>,
    absolute: f64,
    commands: usize,
}
type Program = Arc<Vec<u64>>;
type ConversionCache = HashMap<Vec<Program>, (usize, Arc<Context>)>;
#[derive(Default)]
pub(crate) struct Compiler {
    conversions: ConversionCache,
    effects: HashMap<(usize, Program), Arc<Effect>>,
    programs: HashMap<Performer, Program>,
    bytes: usize,
    next: usize,
    indices: HashMap<i32, usize>,
    baseline_gains: HashMap<Program, f64>,
    lower_weights: Vec<f64>,
}
impl Compiler {
    pub(crate) fn baseline_gain_floor(
        &mut self,
        master: &Master,
        chart: &gekiso::Chart,
        better: &Performer,
        worse: &Performer,
    ) -> Result<f64, String> {
        if !self.program(master, worse).is_empty() {
            return Ok(0.0);
        }
        for &(id, level) in &better.gekisou_support_skills {
            for row in master.gekiso_support_skills[&id]
                .effects
                .iter()
                .filter(|r| r.level == level && r.kind != 4004)
            {
                if row.kind != 2001
                    || master.cumulative.get(&row.cumulative_id).is_none_or(|c| {
                        c.kind != 1000
                            || c.targets.is_empty()
                            || c.targets.iter().any(|id| master.targets[id].judgement != 6)
                    })
                {
                    return Ok(0.0);
                }
            }
        }
        let program = self.program(master, better);
        if program.is_empty() || program.first() == Some(&u64::MAX) {
            return Ok(0.0);
        }
        if let Some(&gain) = self.baseline_gains.get(&program) {
            return Ok(gain);
        }
        if self.lower_weights.is_empty() {
            self.lower_weights = chart
                .note_frac
                .iter()
                .enumerate()
                .map(|(i, &fraction)| {
                    let mut weight = master.live.adjustment;
                    for value in [
                        chart.difficulty_factor,
                        fraction,
                        chart.combo_factor[i],
                        chart.judge_frac[5].min(chart.judge_frac[6]),
                    ] {
                        weight = (weight * value).next_down().max(0.0);
                    }
                    (weight / chart.full.converted as f64).next_down().max(0.0)
                })
                .collect();
        }
        let mut model = Model::new(
            master,
            std::slice::from_ref(better),
            &chart.full.notes,
            &chart.full.events,
            chart.params(),
            Some(&chart.setup),
            (0, false),
        )?;
        for frame in chart.span.0..chart.span.1 {
            let input = &chart.play.frames[frame];
            model.frame(input.time_ms, &input.judged, chart.play.dt[frame])?;
        }
        let mut commands = model.score.score.note_factor_commands();
        commands.sort_by_key(|c| c.0);
        let (mut at, mut factor, mut gain) = (0, 0.0_f64, 0.0_f64);
        for (i, &time) in chart.times.iter().enumerate() {
            while at < commands.len() && commands[at].0 <= time {
                factor = (factor + f(f(commands[at].1 as f64) / 100000.0)).next_down();
                at += 1;
            }
            let term = (factor.max(0.0) * self.lower_weights[i])
                .next_down()
                .max(0.0);
            gain = (gain + term).next_down().max(0.0);
        }
        self.baseline_gains.insert(program, gain);
        Ok(gain)
    }
    pub(crate) fn score_dominates(
        &mut self,
        master: &Master,
        better: &Performer,
        worse: &Performer,
    ) -> bool {
        let split = |p: &Performer| {
            if p.gekisou_skill.is_some() {
                return None;
            }
            let mut convert = p.clone();
            let mut score = p.clone();
            convert.gekisou_support_skills.clear();
            score.gekisou_support_skills.clear();
            for &(id, level) in &p.gekisou_support_skills {
                let mut sc = false;
                let mut cv = false;
                for row in master.gekiso_support_skills[&id]
                    .effects
                    .iter()
                    .filter(|r| r.level == level)
                {
                    match row.kind {
                        2001 if row.value >= 0 && row.max_value > 0 => sc = true,
                        13005 => cv = true,
                        4004 => {}
                        _ => return None,
                    }
                }
                if sc && cv {
                    return None;
                }
                if sc {
                    score.gekisou_support_skills.push((id, level));
                }
                if cv {
                    convert.gekisou_support_skills.push((id, level));
                }
            }
            Some((convert, score))
        };
        let (Some((ac, ascore)), Some((bc, bscore))) = (split(better), split(worse)) else {
            return false;
        };
        let ac = self.program(master, &ac);
        let bc = self.program(master, &bc);
        if ac.first() == Some(&u64::MAX) || bc.first() == Some(&u64::MAX) || ac != bc {
            return false;
        }
        let a = self.program(master, &ascore);
        let b = self.program(master, &bscore);
        if a.first() == Some(&u64::MAX) || b.first() == Some(&u64::MAX) {
            return false;
        }
        if b.is_empty() {
            return true;
        }
        if a.len() != b.len() {
            return false;
        }
        let mut at = 0;
        while at < b.len() {
            if a[at + 2] != 2001 || b[at + 2] != 2001 {
                return false;
            }
            let end = at + 14 + b[at + 13] as usize;
            if a[at + 6] < b[at + 6] || a[at + 12] < b[at + 12] {
                return false;
            }
            for i in at..end {
                if i != at + 6 && i != at + 12 && a[i] != b[i] {
                    return false;
                }
            }
            at = end;
        }
        true
    }
    fn program(&mut self, master: &Master, p: &Performer) -> Arc<Vec<u64>> {
        if let Some(key) = self.programs.get(p) {
            return Arc::clone(key);
        }
        let mut key = Vec::new();
        let mut row_ids = HashMap::default();
        for &(id, level) in &p.gekisou_support_skills {
            let skill = &master.gekiso_support_skills[&id];
            for row in skill
                .effects
                .iter()
                .filter(|r| r.level == level && r.kind != 4004)
            {
                let static_condition = if row.condition_group == 0 {
                    Some(true)
                } else {
                    master
                        .condition_sets
                        .get(&row.condition_group)
                        .and_then(|sets| {
                            let mut any = false;
                            for set in sets {
                                let mut all = true;
                                for id in set {
                                    let c = &master.conditions[id];
                                    all &= match c.kind {
                                        5000 => {
                                            c.targets.iter().any(|id| {
                                                master.targets.get(id).is_some_and(|t| {
                                                    crate::full::conditions::performer_matches(p, t)
                                                })
                                            }) == c.positive
                                        }
                                        2000 => {
                                            (master.live.life_base <= *c.values.first()?)
                                                == c.positive
                                        }
                                        2001 => {
                                            (master.live.life_base >= *c.values.first()?)
                                                == c.positive
                                        }
                                        _ => return None,
                                    };
                                }
                                any |= all;
                            }
                            Some(any)
                        })
                };
                if static_condition == Some(false) {
                    continue;
                }
                let owner_dynamic = [row.trigger_group, row.release_group, row.reset_group]
                    .iter()
                    .flat_map(|g| master.condition_sets.get(g).into_iter().flatten().flatten())
                    .any(|id| {
                        master
                            .conditions
                            .get(id)
                            .is_some_and(|c| [5000, 2000, 2001].contains(&c.kind))
                    });
                if static_condition.is_none() || owner_dynamic {
                    key = serde_json::to_vec(p)
                        .unwrap()
                        .into_iter()
                        .map(|c| c as u64)
                        .collect();
                    key.insert(0, u64::MAX);
                    let key = Arc::new(key);
                    self.programs.insert(p.clone(), Arc::clone(&key));
                    return key;
                }
                let next = row_ids.len() as u64;
                key.push(*row_ids.entry(row.id).or_insert(next));
                key.extend([
                    skill.mission_type as u64,
                    row.kind as u64,
                    row.trigger_type as u64,
                    row.execute_limit as u64,
                    row.reset_group as u64,
                    row.value as u64,
                    row.seconds.to_bits(),
                    row.trigger_group as u64,
                    row.release_group as u64,
                    row.cumulative_id as u64,
                    row.limit_count as u64,
                    row.max_value as u64,
                    row.targets.len() as u64,
                ]);
                key.extend(row.targets.iter().map(|&v| v as u64));
            }
        }
        let key = Arc::new(key);
        self.programs.insert(p.clone(), Arc::clone(&key));
        key
    }
    pub(crate) fn upper(
        &mut self,
        master: &Master,
        chart: &gekiso::Chart,
        deck: &[Performer],
    ) -> Result<Option<Context>, String> {
        let mut converters = Vec::new();
        let mut modifiers = Vec::new();
        for actor in deck {
            if actor.gekisou_skill.is_some() {
                return Ok(None);
            }
            let mut converter = actor.clone();
            let mut modifier = actor.clone();
            converter.gekisou_support_skills.clear();
            modifier.gekisou_support_skills.clear();
            for &(id, level) in &actor.gekisou_support_skills {
                let skill = &master.gekiso_support_skills[&id];
                let mut score = false;
                let mut convert = false;
                for row in skill.effects.iter().filter(|r| r.level == level) {
                    match row.kind {
                        2001 if row.value >= 0 && row.max_value >= 0 => score = true,
                        13005 => convert = true,
                        4004 => {}
                        _ => return Ok(None),
                    }
                }
                if score && convert {
                    return Ok(None);
                }
                if score {
                    modifier.gekisou_support_skills.push((id, level));
                }
                if convert {
                    converter.gekisou_support_skills.push((id, level));
                }
            }
            if !converter.gekisou_support_skills.is_empty() {
                for &(id, level) in &converter.gekisou_support_skills {
                    for row in master.gekiso_support_skills[&id]
                        .effects
                        .iter()
                        .filter(|r| r.level == level && r.kind == 13005)
                    {
                        if row.cumulative_id != 0
                            || [row.trigger_group, row.release_group, row.reset_group]
                                .iter()
                                .flat_map(|g| {
                                    master.condition_sets.get(g).into_iter().flatten().flatten()
                                })
                                .any(|id| {
                                    ![0, 1030, 7010, 7013, 7020]
                                        .contains(&master.conditions[id].kind)
                                })
                        {
                            return Ok(None);
                        }
                    }
                }
                converters.push(converter);
            }
            modifiers.push(modifier);
        }
        if self.bytes > 16 * 1024 * 1024 {
            self.conversions.clear();
            self.effects.clear();
            self.bytes = 0;
        }
        let conversion_key: Vec<_> = converters.iter().map(|p| self.program(master, p)).collect();
        let (id, converted) = if let Some(value) = self.conversions.get(&conversion_key) {
            (value.0, Arc::clone(&value.1))
        } else {
            let context = Arc::new(gekiso::context_fixed(master, chart, &converters, &[0])?);
            if context.up.iter().any(|&v| v != 1.0)
                || context.gk.iter().any(|&v| v != 1.0)
                || context.luck.iter().any(|&v| v != 1.0)
            {
                return Ok(None);
            }
            self.next += 1;
            self.bytes += context.retained_bytes();
            self.conversions
                .insert(conversion_key, (self.next, Arc::clone(&context)));
            (self.next, context)
        };
        if self.indices.is_empty() {
            self.indices = chart
                .notes
                .iter()
                .enumerate()
                .map(|(i, n)| (n.id, i))
                .collect();
        }
        let mut effects = Vec::new();
        for actor in modifiers {
            if actor.gekisou_support_skills.is_empty() {
                continue;
            }
            let key = (id, self.program(master, &actor));
            let effect = if let Some(effect) = self.effects.get(&key) {
                Arc::clone(effect)
            } else {
                let mut model = Model::new(
                    master,
                    &[actor],
                    &chart.full.notes,
                    &chart.full.events,
                    chart.params(),
                    Some(&chart.setup),
                    (0, false),
                )?;
                for k in chart.span.0..chart.span.1 {
                    let frame = &chart.play.frames[k];
                    let mut judged = frame.judged.clone();
                    for note in &mut judged {
                        let Some(&i) = self.indices.get(&note.note_id) else {
                            return Ok(None);
                        };
                        note.judgement = converted.judgement[i] as i32;
                    }
                    model.frame(frame.time_ms, &judged, chart.play.dt[k])?;
                }
                let mut commands = model.score.score.note_factor_commands();
                commands.sort_by_key(|c| c.0);
                let absolute = commands.iter().fold(0.0_f64, |sum, c| {
                    (sum + f(f(c.1 as f64) / 100000.0).abs()).next_up()
                });
                let (mut at, mut sum) = (0, 0.0_f64);
                let values = chart
                    .times
                    .iter()
                    .map(|&time| {
                        while at < commands.len() && commands[at].0 <= time {
                            sum = (sum + f(f(commands[at].1 as f64) / 100000.0)).next_up();
                            at += 1;
                        }
                        sum
                    })
                    .collect();
                let effect = Arc::new(Effect {
                    values,
                    absolute,
                    commands: commands.len(),
                });
                self.bytes += effect.values.len() * 8 + 128;
                self.effects.insert(key, Arc::clone(&effect));
                effect
            };
            effects.push(effect);
        }
        let Some(ranges) = crate::rewards::deterministic_ranges(master, chart) else {
            return Ok(None);
        };
        let operations = crate::rewards::factor_operations(chart, &ranges) as f64
            * effects.iter().map(|e| e.commands).sum::<usize>() as f64;
        let nu = (operations * 2.0_f64.powi(-24)).next_up();
        if nu >= 0.25 {
            return Ok(None);
        }
        let absolute = effects
            .iter()
            .fold(0.0_f64, |sum, e| (sum + e.absolute).next_up());
        let error = (4.0 * nu * absolute / (1.0 - nu)).next_up();
        let up = (0..chart.notes.len())
            .map(|i| {
                let sum = effects
                    .iter()
                    .fold(1.0_f64, |sum, e| (sum + e.values[i]).next_up());
                (sum + error).next_up().max(0.0)
            })
            .collect();
        Ok(Some(Context {
            judgement: converted.judgement.clone(),
            gk: converted.gk.clone(),
            up,
            luck: converted.luck.clone(),
            seeds: 1,
            draws: Vec::new(),
            nominal: None,
            factor_commands: effects.iter().map(|e| e.commands).sum(),
            factor_peak: (1.0 + absolute + error).next_up(),
            luck_draws: 0,
        }))
    }
}
