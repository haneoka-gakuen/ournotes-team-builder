//! Score-factor frame logging with native binary32 undo and replay.
use super::core::{self, Combo};
use crate::{master::LiveTables, num::f};
use std::collections::BTreeMap;
#[derive(Clone, Default)]
pub struct FactorCommand {
    pub time: i32,
    pub owner: i32,
    pub note: i32,
    pub combo: i32,
    pub judgement: i32,
    pub judge: i32,
    pub luck: i32,
}
pub fn note_factor_mill(factor: f64) -> i32 {
    core::floor_i32(f(factor * 100000.0))
}
pub fn judgement_factor_mill(factor: f64) -> i32 {
    let x = f(factor * 100000.0);
    if x.is_finite() {
        core::floor_i32_f64(x.round_ties_even())
    } else {
        core::floor_i32(x)
    }
}
fn mill(value: i32) -> f64 {
    if value == 0 {
        0.0
    } else {
        f(f(value as f64) / 100000.0)
    }
}
#[derive(Clone)]
pub struct Factors {
    pub combo: f64,
    pub note: f64,
    pub just: f64,
    pub perfect: f64,
    pub great: f64,
    pub good: f64,
    pub luck: i32,
}
impl Default for Factors {
    fn default() -> Self {
        Self {
            combo: 0.0,
            note: 1.0,
            just: 0.0,
            perfect: 0.0,
            great: 0.0,
            good: 0.0,
            luck: 0,
        }
    }
}
impl Factors {
    pub fn judgement(&self, score_type: i32) -> f64 {
        match score_type {
            1 => self.just,
            2 => self.perfect,
            3 => self.great,
            4 => self.good,
            _ => 0.0,
        }
    }
    pub fn apply(&mut self, c: &FactorCommand) {
        let (fc, fnote, fj) = (mill(c.combo), mill(c.note), mill(c.judge));
        if fc != 0.0 {
            self.combo = f(self.combo + fc);
        }
        if fnote != 0.0 {
            self.note = f(self.note + fnote);
        }
        match c.judgement {
            6 => self.just = f(self.just + fj),
            5 => self.perfect = f(self.perfect + fj),
            4 => self.great = f(self.great + fj),
            3 => self.good = f(self.good + fj),
            _ => {}
        }
        self.luck = self.luck.wrapping_add(c.luck);
    }
}
#[derive(Clone, Default)]
struct Diff {
    combo: f64,
    note: f64,
    just: f64,
    perfect: f64,
    great: f64,
    good: f64,
    luck: i32,
}
impl Diff {
    fn add(&mut self, c: &FactorCommand) {
        self.luck = self.luck.wrapping_add(c.luck);
        let (fc, fnote, fj) = (mill(c.combo), mill(c.note), mill(c.judge));
        if fc != 0.0 {
            self.combo = f(self.combo + fc);
        }
        if fnote != 0.0 {
            self.note = f(self.note + fnote);
        }
        match c.judgement {
            6 => self.just = f(self.just + fj),
            5 => self.perfect = f(self.perfect + fj),
            4 => self.great = f(self.great + fj),
            3 => self.good = f(self.good + fj),
            _ => {}
        }
    }
    fn undo(&mut self, state: &mut Factors) {
        state.just = f(state.just - self.just);
        state.perfect = f(state.perfect - self.perfect);
        state.combo = f(state.combo - self.combo);
        state.note = f(state.note - self.note);
        state.luck = state.luck.wrapping_sub(self.luck);
        state.great = f(state.great - self.great);
        state.good = f(state.good - self.good);
        *self = Self::default();
    }
}
pub trait GekisoCombo {
    fn combo(&self, time: i32) -> Option<i32>;
}
#[derive(Clone)]
pub struct Observation {
    pub score_type: i32,
    pub note: f64,
    pub gk: f64,
    pub luck: f64,
    pub judge: f64,
}
#[derive(Clone)]
pub struct Calculator<'a> {
    pub tables: &'a LiveTables,
    pub power: i32,
    pub converted: i32,
    pub assist: f64,
    pub level_factor: f64,
    pub factors: Factors,
    pub factors_only: bool,
    pub observations: Vec<(i32, Observation)>,
}
impl<'a> Calculator<'a> {
    pub fn new(
        tables: &'a LiveTables,
        power: i32,
        level: f64,
        converted: i32,
        assist: f64,
    ) -> Self {
        Self {
            tables,
            power,
            converted,
            assist,
            level_factor: f(f(f(crate::num::i32(level - 5.0) as f64) * f(0.005)) + 1.0),
            factors: Factors::default(),
            factors_only: false,
            observations: Vec::new(),
        }
    }
    pub fn table(&self, kind: i32, combo: i32) -> f64 {
        let Some(rows) = self.tables.combo.get(&kind) else {
            return 0.0;
        };
        let at = rows.partition_point(|&(required, _)| required as i32 <= combo);
        if at == 0 {
            0.0
        } else {
            rows[at - 1].1
        }
    }
    pub fn gekiso_factor(&self, info: Option<&dyn GekisoCombo>, time: i32) -> f64 {
        info.and_then(|info| info.combo(time))
            .map(|c| f(core::min_nan(self.table(1, c), 1.0) + 1.0))
            .unwrap_or(1.0)
    }
    pub fn note_score(
        &mut self,
        combo: i32,
        note: &NoteCommand,
        info: Option<&dyn GekisoCombo>,
    ) -> Result<i32, String> {
        let (life, time, op, score_type, note_id) =
            (note.life, note.time, note.op, note.score_type, note.id);
        let gk = self.gekiso_factor(info, time);
        let judge = self.factors.judgement(score_type);
        let luck_percent = self.factors.luck.wrapping_add(100).min(200);
        let luck = f(f(luck_percent as f64) / 100.0);
        if self.factors_only {
            self.observations.push((
                note_id,
                Observation {
                    score_type,
                    note: self.factors.note,
                    gk,
                    luck,
                    judge,
                },
            ));
            return Ok(0);
        }
        let cumulative = self.table(0, combo);
        let score_up = f(self.factors.note + judge);
        let combo_factor = f(gk * f(self.factors.combo + f(core::min_nan(cumulative, 1.0) + 1.0)));
        self.core(life, op, score_type, combo_factor, score_up, luck)
    }
    pub fn core(
        &self,
        life: i32,
        op: i32,
        score_type: i32,
        combo_factor: f64,
        score_up: f64,
        luck: f64,
    ) -> Result<i32, String> {
        let note = self
            .tables
            .note_percent
            .get(&op)
            .ok_or_else(|| format!("score percent missing: note {op} score {score_type}"))?;
        let judge = self
            .tables
            .judge_percent
            .get(&score_type)
            .ok_or_else(|| format!("score percent missing: note {op} score {score_type}"))?;
        let life_factor = if life > 0 { 1.0 } else { self.tables.life_onus };
        let t = f(f(self.tables.adjustment * f(self.power as f64)) * self.level_factor);
        let a = f(f(f(*note) / 100.0) * t);
        let b = f(f(f(*judge) / 100.0) * a);
        let c = f(f(b * combo_factor) * score_up);
        let d = f(luck * c);
        let x = f(d / f(self.converted as f64));
        let floor = x.floor();
        let y = if floor == f64::INFINITY {
            -2147483648.0
        } else {
            f(core::sat_i32(floor) as f64)
        };
        let z = f(self.assist * f(life_factor * y));
        Ok(core::floor_i32(z))
    }
}
#[derive(Clone)]
pub struct NoteCommand {
    pub time: i32,
    pub life: i32,
    pub id: i32,
    pub op: i32,
    pub score_type: i32,
    pub added: i32,
}
#[derive(Clone)]
pub struct Incremental<'a> {
    pub calculator: Calculator<'a>,
    pub max_frame: usize,
    pub notes: Vec<Vec<NoteCommand>>,
    factors: Vec<Vec<FactorCommand>>,
    diffs: Vec<Diff>,
    previous: i32,
    added: i32,
    pub score: i32,
    fixed: BTreeMap<i32, i32>,
    pending_fixed: Option<(i32, i32)>,
    pub rank_bonus: i32,
}
impl<'a> Incremental<'a> {
    pub fn new(calculator: Calculator<'a>, music_length: i32) -> Self {
        let max_frame = (core::frame(music_length) + 50).max(1) as usize;
        Self {
            calculator,
            max_frame,
            notes: vec![Vec::new(); max_frame],
            factors: vec![Vec::new(); max_frame],
            diffs: vec![Diff::default(); max_frame],
            previous: -1,
            added: -1,
            score: 0,
            fixed: BTreeMap::new(),
            pending_fixed: None,
            rank_bonus: 0,
        }
    }
    pub(crate) fn note_factor_commands(&self) -> Vec<(i32, i32)> {
        self.factors
            .iter()
            .flatten()
            .filter(|c| c.note != 0)
            .map(|c| (c.time, c.note))
            .collect()
    }
    pub(crate) fn note_factor_commands_with_owner(
        &self,
    ) -> impl Iterator<Item = (i32, i32, i32)> + '_ {
        self.factors
            .iter()
            .flatten()
            .filter(|c| c.note != 0)
            .map(|c| (c.owner, c.time, c.note))
    }
    pub(crate) fn rush_factor_commands(&self) -> Vec<(i32, i32)> {
        let mut commands: Vec<_> = self
            .factors
            .iter()
            .flatten()
            .filter(|command| command.luck != 0)
            .map(|command| (command.time, command.luck))
            .collect();
        commands.sort_by_key(|command| command.0);
        commands
    }
    pub(crate) fn rush_factor_changes(&self) -> usize {
        self.factors
            .iter()
            .flatten()
            .filter(|command| command.luck != 0)
            .count()
    }
    fn file(&mut self, time: i32) -> usize {
        let frame = (core::frame(time) as usize).min(self.max_frame - 1);
        self.added = if self.added < 0 {
            frame as i32
        } else {
            self.added.min(frame as i32)
        };
        frame
    }
    pub fn add_note(&mut self, time: i32, life: i32, id: i32, op: i32, score_type: i32) {
        let frame = self.file(time);
        self.notes[frame].push(NoteCommand {
            time,
            life,
            id,
            op,
            score_type,
            added: 0,
        });
    }
    pub fn add_factor(&mut self, command: FactorCommand) {
        let frame = self.file(command.time);
        self.factors[frame].push(command);
    }
    pub fn add_fixed(&mut self, time: i32, score: i32) {
        self.pending_fixed = Some((time, score));
    }
    pub fn calculate(
        &mut self,
        time: i32,
        combo: &Combo,
        info: Option<&dyn GekisoCombo>,
    ) -> Result<i32, String> {
        let to = (core::frame(time).max(0) as usize).min(self.max_frame - 1) as i32;
        let u = if self.added < 0 {
            to
        } else {
            to.min(self.added - 1)
        };
        let start = if u < self.previous {
            for frame in (u + 1..=self.previous).rev() {
                self.undo(frame as usize);
            }
            u + 1
        } else {
            self.previous + 1
        };
        for frame in start..=to {
            self.execute(frame as usize, combo, info)?;
        }
        if let Some((time, score)) = self.pending_fixed.take() {
            let frame = core::frame(time);
            if self.fixed.contains_key(&frame) {
                return Err("two fixed scores in one frame".into());
            }
            self.fixed.insert(frame, score);
            self.score = self.score.wrapping_add(score);
        }
        self.previous = to;
        self.added = -1;
        Ok(self.score)
    }
    fn undo(&mut self, frame: usize) {
        for note in &self.notes[frame] {
            self.score = self.score.wrapping_sub(note.added);
        }
        if let Some(&value) = self.fixed.get(&(frame as i32)) {
            self.score = self.score.wrapping_sub(value);
        }
        self.diffs[frame].undo(&mut self.calculator.factors);
    }
    fn execute(
        &mut self,
        frame: usize,
        combo: &Combo,
        info: Option<&dyn GekisoCombo>,
    ) -> Result<(), String> {
        let (fl, nl) = (&self.factors[frame], &self.notes[frame]);
        if fl.is_empty() && nl.is_empty() {
            if let Some(&value) = self.fixed.get(&(frame as i32)) {
                self.score = self.score.wrapping_add(value);
                self.rank_bonus = self.rank_bonus.wrapping_add(value);
            }
            return Ok(());
        }
        let mut factor_order: Vec<_> = (0..fl.len()).collect();
        factor_order.sort_by_key(|&i| (fl[i].time, fl[i].owner, i));
        let mut note_order: Vec<_> = (0..nl.len()).collect();
        note_order.sort_by_key(|&i| (nl[i].time, nl[i].id, i));
        let (mut a, mut b) = (0, 0);
        while a < factor_order.len() || b < note_order.len() {
            if a < factor_order.len()
                && (b >= note_order.len()
                    || self.notes[frame][note_order[b]].time
                        >= self.factors[frame][factor_order[a]].time)
            {
                let c = &self.factors[frame][factor_order[a]];
                self.calculator.factors.apply(c);
                self.diffs[frame].add(c);
                a += 1;
            } else {
                let n = &mut self.notes[frame][note_order[b]];
                let value = self.calculator.note_score(combo.at(n.time), n, info)?;
                n.added = value;
                self.score = self.score.wrapping_add(value);
                b += 1;
            }
        }
        if let Some(&value) = self.fixed.get(&(frame as i32)) {
            self.score = self.score.wrapping_add(value);
            self.rank_bonus = self.rank_bonus.wrapping_add(value);
        }
        Ok(())
    }
}
