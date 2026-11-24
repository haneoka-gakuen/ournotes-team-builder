//! Score-factor frame logging with native binary32 undo and replay.
use super::core::{self, Combo};
use crate::{master::LiveTables, num::f};
use std::{cell::RefCell, collections::BTreeMap, ops::{Deref, DerefMut, Index, IndexMut}, rc::Rc, sync::Arc};
use rustc_hash::FxHashMap;
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
// The frame diff is accumulated and stored in binary32.
#[derive(Clone, Default)]
struct Diff {
    combo: f32,
    note: f32,
    just: f32,
    perfect: f32,
    great: f32,
    good: f32,
    luck: i32,
}
impl Diff {
    fn add(&mut self, c: &FactorCommand) {
        self.luck = self.luck.wrapping_add(c.luck);
        let (fc, fnote, fj) = (mill(c.combo), mill(c.note), mill(c.judge));
        if fc != 0.0 {
            self.combo += fc as f32;
        }
        if fnote != 0.0 {
            self.note += fnote as f32;
        }
        match c.judgement {
            6 => self.just += fj as f32,
            5 => self.perfect += fj as f32,
            4 => self.great += fj as f32,
            3 => self.good += fj as f32,
            _ => {}
        }
    }
    fn undo(&mut self, state: &mut Factors) {
        state.just = f(state.just - self.just as f64);
        state.perfect = f(state.perfect - self.perfect as f64);
        state.combo = f(state.combo - self.combo as f64);
        state.note = f(state.note - self.note as f64);
        state.luck = state.luck.wrapping_sub(self.luck);
        state.great = f(state.great - self.great as f64);
        state.good = f(state.good - self.good as f64);
        *self = Self::default();
    }
}
pub trait GekisoCombo {
    fn combo(&self, time: i32) -> Option<i32>;
}
#[derive(Clone)]
pub struct Observation {
    pub life: i32,
    pub score_type: i32,
    pub note: f64,
    pub gk: f64,
    pub luck: f64,
    pub judge: f64,
}
type NoteCells = Rc<RefCell<Vec<i32>>>;
const MISSING_SCORE: i32 = i32::MIN;
pub(crate) struct NoteMemo {
    bases: Rc<Vec<[Option<f64>; 7]>>,
    base_ids: Vec<[u16; 7]>,
    combo_ids: Vec<usize>,
    combos: Vec<f64>,
    cells: usize,
    rush: i32,
    luck_variants: usize,
    states: RefCell<FxHashMap<[u64; 3], NoteCells>>,
}
impl NoteMemo {
    fn new(calculator: &Calculator<'_>, maximum: usize) -> Self {
        let bases = Rc::clone(calculator.bases.as_ref().unwrap());
        let mut base_ids = vec![[0; 7]; bases.len()];
        let mut ids = FxHashMap::default();
        for (op, row) in bases.iter().enumerate() {
            for (grade, base) in row.iter().enumerate() {
                if let Some(base) = base {
                    let next = ids.len() as u16 + 1;
                    base_ids[op][grade] = *ids.entry(base.to_bits()).or_insert(next);
                }
            }
        }
        let base_count = ids.len();
        let mut combo_ids = Vec::with_capacity(maximum + 1);
        let mut combos = Vec::new();
        let mut ids = FxHashMap::default();
        for count in 0..=maximum {
            let value = f(core::min_nan(calculator.table(0, count as i32), 1.0) + 1.0);
            let next = combos.len();
            let id = *ids.entry(value.to_bits()).or_insert_with(|| { combos.push(value); next });
            combo_ids.push(id);
        }
        let rush = calculator.tables.gekiso.rush_percent.wrapping_add(100).min(200);
        let luck_variants = if rush == 100 { 1 } else { 2 };
        let cells = base_count.checked_mul(combos.len()).and_then(|n| n.checked_mul(2))
            .and_then(|n| n.checked_mul(luck_variants)).unwrap_or(usize::MAX);
        Self { bases, base_ids, combo_ids, combos, cells, rush, luck_variants, states: RefCell::new(FxHashMap::default()) }
    }
    fn state(&self, combo: f64, up: f64, gekiso: u64) -> Option<NoteCells> {
        let key = [combo.to_bits(), up.to_bits(), gekiso];
        let mut states = self.states.borrow_mut();
        if let Some(row) = states.get(&key) { return Some(Rc::clone(row)); }
        let limit = 4 * 1024 * 1024 / std::mem::size_of::<i32>();
        if self.cells == 0 || self.cells > limit / (states.len() + 1) { return None; }
        let row = Rc::new(RefCell::new(vec![MISSING_SCORE; self.cells]));
        states.insert(key, Rc::clone(&row));
        Some(row)
    }
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
    pub(crate) note_combos: Option<Arc<CachedCombos>>,
    pub(crate) combo_input: usize,
    bases: Option<Rc<Vec<[Option<f64>; 7]>>>,
    memo: Option<Rc<NoteMemo>>,
    memo_rows: [Option<NoteCells>; 7],
    memo_ups: [f64; 7],
    memo_grades: u8,
    memo_dirty: bool,
    memo_gekiso: u64,
}
#[derive(PartialEq, Eq, Hash)]
pub(crate) struct CachedCombos {
    pub values: Vec<i32>,
    pub changes: Vec<Vec<(usize, i32)>>,
}
impl<'a> Calculator<'a> {
    fn combo_for(&self, note: &NoteCommand, combo: &Combo) -> i32 {
        let Some(counts) = &self.note_combos else { return combo.at(note.time); };
        let value = counts.values[note.id as usize];
        if value >= 0 { return value; }
        let changes = &counts.changes[(-value - 1) as usize];
        let end = changes.partition_point(|&(input, _)| input <= self.combo_input);
        changes[end - 1].1
    }

    fn zero_note(&self, op: i32, grade: i32) -> bool {
        !self.factors_only && self.bases.as_ref()
            .and_then(|bases| bases.get(op as usize)).and_then(|row| row.get(grade as usize))
            .is_some_and(|score| *score == Some(0.0))
    }

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
            note_combos: None,
            combo_input: 0,
            bases: None,
            memo: None,
            memo_rows: std::array::from_fn(|_| None),
            memo_ups: [0.0; 7],
            memo_grades: 0,
            memo_dirty: true,
            memo_gekiso: 0,
        }
    }
    pub(crate) fn share_note_memo(&mut self, shared: &mut Option<Rc<NoteMemo>>, maximum: usize) {
        if shared.is_none() {
            self.cache_bases();
            *shared = Some(Rc::new(NoteMemo::new(self, maximum)));
        }
        let memo = Rc::clone(shared.as_ref().unwrap());
        self.bases = Some(Rc::clone(&memo.bases));
        self.memo = Some(memo);
        self.memo_rows = std::array::from_fn(|_| None);
        self.memo_grades = 0;
        self.memo_dirty = true;
    }
    fn memo_score(&mut self, life: i32, op: i32, grade: i32, combo: i32, gekiso: f64) -> Result<Option<i32>, String> {
        let memo = self.memo.as_ref().unwrap();
        let grade_index = grade as usize;
        if grade_index >= self.memo_rows.len() { return Ok(None); }
        if self.memo_dirty || self.memo_gekiso != gekiso.to_bits() {
            self.memo_gekiso = gekiso.to_bits();
            self.memo_grades = 0;
            self.memo_dirty = false;
        }
        let Some(&base) = memo.base_ids.get(op as usize).and_then(|row| row.get(grade_index)) else { return Ok(None); };
        if base == 0 { return Ok(None); }
        let Some(&bucket) = memo.combo_ids.get(combo as usize) else { return Ok(None); };
        let percent = self.factors.luck.wrapping_add(100).min(200);
        let luck = if percent == 100 { 0 } else if percent == memo.rush { 1 } else { return Ok(None); };
        if self.memo_grades & (1 << grade_index) == 0 {
            let up = f(self.factors.note + self.factors.judgement(grade));
            self.memo_ups[grade_index] = up;
            let previous = (0..self.memo_rows.len()).find(|&index| {
                self.memo_grades & (1 << index) != 0 && self.memo_ups[index].to_bits() == up.to_bits()
            });
            self.memo_rows[grade_index] = match previous {
                Some(index) => self.memo_rows[index].clone(),
                None => memo.state(self.factors.combo, up, self.memo_gekiso),
            };
            self.memo_grades |= 1 << grade_index;
        }
        let Some(row) = &self.memo_rows[grade_index] else { return Ok(None); };
        let index = (((base as usize - 1) * memo.combos.len() + bucket) * 2 + usize::from(life <= 0)) * memo.luck_variants + luck;
        let cached = row.borrow()[index];
        if cached != MISSING_SCORE { return Ok(Some(cached)); }
        let combo_factor = f(gekiso * f(self.factors.combo + memo.combos[bucket]));
        let score_up = self.memo_ups[grade_index];
        let luck = f(f(percent as f64) / 100.0);
        let value = self.core(life, op, grade, combo_factor, score_up, luck)?;
        row.borrow_mut()[index] = value;
        Ok(Some(value))
    }
    pub(crate) fn cache_bases(&mut self) {
        let mut bases = vec![[None; 7]; 256];
        for (&op, &note) in &self.tables.note_percent {
            if !(0..256).contains(&op) { continue; }
            for (&grade, &judge) in &self.tables.judge_percent {
                if !(0..7).contains(&grade) { continue; }
                bases[op as usize][grade as usize] = Some(self.base(note, judge));
            }
        }
        self.bases = Some(Rc::new(bases));
    }
    fn base(&self, note: f64, judge: f64) -> f64 {
        let t = f(f(self.tables.adjustment * f(self.power as f64)) * self.level_factor);
        let a = f(f(f(note) / 100.0) * t);
        f(f(f(judge) / 100.0) * a)
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
        if self.memo.is_some() && !self.factors_only {
            if let Some(value) = self.memo_score(life, op, score_type, combo, gk)? { return Ok(value); }
        }
        let judge = self.factors.judgement(score_type);
        let luck_percent = self.factors.luck.wrapping_add(100).min(200);
        let luck = f(f(luck_percent as f64) / 100.0);
        if self.factors_only {
            self.observations.push((
                note_id,
                Observation {
                    life,
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
        let cached = self.bases.as_ref().and_then(|bases| bases.get(op as usize))
            .and_then(|row| row.get(score_type as usize)).copied().flatten();
        let b = if let Some(base) = cached { base } else {
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
        self.base(*note, *judge)
        };
        let life_factor = if life > 0 { 1.0 } else { self.tables.life_onus };
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
}
#[derive(Clone)]
pub(crate) enum ReplayCommand {
    Note(NoteCommand),
    ComboHistory(super::gekisou::ComboHistoryUpdate),
    Factor(FactorCommand),
    Calculate(i32),
    Rush(i32, i32),
    Input(Vec<super::play::JudgedNote>),
    Ordinary(i32, [(i32, i32); 2]),
    IdleFrames(usize),
    Rank(usize, i32, i32),
}

#[derive(Clone, Default)]
enum FrameBucket<T> {
    #[default]
    Empty,
    One(T),
    Many(Vec<T>),
}
impl<T> FrameBucket<T> {
    fn push(&mut self, value: T) {
        match self {
            Self::Empty => *self = Self::One(value),
            Self::One(_) => {
                let Self::One(first) = std::mem::take(self) else { unreachable!(); };
                *self = Self::Many(vec![first, value]);
            }
            Self::Many(values) => values.push(value),
        }
    }
}
impl<T> Deref for FrameBucket<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        match self {
            Self::Empty => &[],
            Self::One(value) => std::slice::from_ref(value),
            Self::Many(values) => values,
        }
    }
}
impl<T> DerefMut for FrameBucket<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        match self {
            Self::Empty => &mut [],
            Self::One(value) => std::slice::from_mut(value),
            Self::Many(values) => values,
        }
    }
}

const FRAMES_PER_CHUNK: usize = 32;
const CHUNKS_PER_GROUP: usize = 8;
#[derive(Clone)]
struct Chunks<T> {
    groups: Vec<Rc<[Rc<[T; FRAMES_PER_CHUNK]>; CHUNKS_PER_GROUP]>>,
    len: usize,
}
impl<T: Default> Chunks<T> {
    fn new(frames: usize) -> Self {
        let chunk = Rc::new(std::array::from_fn(|_| T::default()));
        let group = Rc::new(std::array::from_fn(|_| Rc::clone(&chunk)));
        let len = frames.div_ceil(FRAMES_PER_CHUNK);
        Self { groups: vec![group; len.div_ceil(CHUNKS_PER_GROUP)], len }
    }
}
impl<T> Chunks<T> {
    fn iter(&self) -> impl Iterator<Item = &Rc<[T; FRAMES_PER_CHUNK]>> {
        self.groups.iter().flat_map(|group| group.iter()).take(self.len)
    }
}
impl<T> Index<usize> for Chunks<T> {
    type Output = Rc<[T; FRAMES_PER_CHUNK]>;
    fn index(&self, index: usize) -> &Self::Output {
        &self.groups[index / CHUNKS_PER_GROUP][index % CHUNKS_PER_GROUP]
    }
}
impl<T> IndexMut<usize> for Chunks<T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut Rc::make_mut(&mut self.groups[index / CHUNKS_PER_GROUP])[index % CHUNKS_PER_GROUP]
    }
}
#[derive(Clone)]
pub struct Incremental<'a> {
    pub calculator: Calculator<'a>,
    pub max_frame: usize,
    notes: Chunks<FrameBucket<NoteCommand>>,
    note_scores: Chunks<i32>,
    factors: Chunks<FrameBucket<FactorCommand>>,
    factor_order: Vec<usize>,
    pub(crate) replay: Option<Vec<ReplayCommand>>,
    pub(crate) factored_record: bool,
    /// Capture control commands without settling them in the control model.
    pub(crate) record_only: bool,
    note_order: Vec<usize>,
    diffs: Chunks<Diff>,
    occupied: Vec<u32>,
    previous: i32,
    added: i32,
    last_command: i32,
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
            notes: Chunks::new(max_frame),
            note_scores: Chunks::new(max_frame),
            factors: Chunks::new(max_frame),
            factor_order: Vec::new(),
            replay: None,
            factored_record: false,
            record_only: false,
            note_order: Vec::new(),
            diffs: Chunks::new(max_frame),
            occupied: vec![0; max_frame.div_ceil(FRAMES_PER_CHUNK)],
            previous: -1,
            added: -1,
            last_command: -1,
            score: 0,
            fixed: BTreeMap::new(),
            pending_fixed: None,
            rank_bonus: 0,
        }
    }
    pub(crate) fn note_factor_commands(&self) -> Vec<(i32, i32)> {
        self.factors
            .iter()
            .flat_map(|chunk| chunk.iter())
            .flat_map(|commands| commands.iter())
            .filter(|c| c.note != 0)
            .map(|c| (c.time, c.note))
            .collect()
    }
    pub(crate) fn note_factor_commands_with_owner(
        &self,
    ) -> impl Iterator<Item = (i32, i32, i32)> + '_ {
        self.factors
            .iter()
            .flat_map(|chunk| chunk.iter())
            .flat_map(|commands| commands.iter())
            .filter(|c| c.note != 0)
            .map(|c| (c.owner, c.time, c.note))
    }
    pub(crate) fn rush_factor_commands(&self) -> Vec<(i32, i32)> {
        let mut commands: Vec<_> = self
            .factors
            .iter()
            .flat_map(|chunk| chunk.iter())
            .flat_map(|commands| commands.iter())
            .filter(|command| command.luck != 0)
            .map(|command| (command.time, command.luck))
            .collect();
        commands.sort_by_key(|command| command.0);
        commands
    }
    pub(crate) fn rush_factor_changes(&self) -> usize {
        self.factors
            .iter()
            .flat_map(|chunk| chunk.iter())
            .flat_map(|commands| commands.iter())
            .filter(|command| command.luck != 0)
            .count()
    }
    fn file(&mut self, time: i32) -> usize {
        let frame = (core::frame(time) as usize).min(self.max_frame - 1);
        self.last_command = self.last_command.max(frame as i32);
        self.added = if self.added < 0 {
            frame as i32
        } else {
            self.added.min(frame as i32)
        };
        frame
    }
    fn mark(&mut self, frame: usize) {
        self.occupied[frame / FRAMES_PER_CHUNK] |= 1 << (frame % FRAMES_PER_CHUNK);
    }

    fn frames_between(&self, chunk: usize, from: usize, through: usize) -> u32 {
        let start = chunk * FRAMES_PER_CHUNK;
        let first = from.saturating_sub(start);
        let last = through.saturating_sub(start).min(FRAMES_PER_CHUNK - 1);
        self.occupied[chunk] & (u32::MAX << first) & (u32::MAX >> (FRAMES_PER_CHUNK - 1 - last))
    }
    pub fn add_note(&mut self, time: i32, life: i32, id: i32, op: i32, score_type: i32) {
        let note = NoteCommand {
            time,
            life,
            id,
            op,
            score_type,
        };
        if let Some(replay) = &mut self.replay {
            replay.push(ReplayCommand::Note(note.clone()));
        }
        if !self.record_only {
            let frame = self.file(time);
            if !self.calculator.zero_note(op, score_type) {
                self.mark(frame);
                Rc::make_mut(&mut self.notes[frame / FRAMES_PER_CHUNK])[frame % FRAMES_PER_CHUNK].push(note);
            }
        }
    }
    pub fn add_factor(&mut self, command: FactorCommand) {
        if let Some(replay) = &mut self.replay {
            replay.push(ReplayCommand::Factor(command.clone()));
        }
        if !self.record_only {
            let frame = self.file(command.time);
            self.mark(frame);
            Rc::make_mut(&mut self.factors[frame / FRAMES_PER_CHUNK])[frame % FRAMES_PER_CHUNK].push(command);
        }
    }
    pub fn add_fixed(&mut self, time: i32, score: i32) {
        self.pending_fixed = Some((time, score));
    }
    pub(crate) fn record_idle_run(&mut self, count: usize) -> bool {
        if self.pending_fixed.is_some() || !self.record_only || !self.factored_record {
            return false;
        }
        let Some(replay) = &mut self.replay else { return false; };
        replay.push(ReplayCommand::IdleFrames(count));
        true
    }

    pub(crate) fn idle_ready(&self) -> bool {
        self.replay.is_none() && self.pending_fixed.is_none()
            && self.added < 0 && self.previous >= self.last_command
    }

    #[inline]
    pub(crate) fn settled_score(&self, time: i32) -> Option<i32> {
        // Idle frames contain no commands to apply or undo. Leaving the cursor
        // at the last calculation also keeps the next rewind over real work.
        (self.idle_ready() && self.previous <= core::frame(time).max(0)).then_some(self.score)
    }

    pub fn calculate(
        &mut self,
        time: i32,
        combo: &Combo,
        info: Option<&dyn GekisoCombo>,
    ) -> Result<i32, String> {
        if let Some(replay) = &mut self.replay {
            replay.push(ReplayCommand::Calculate(time));
        }
        let to = if self.record_only { self.previous } else {
            (core::frame(time).max(0) as usize).min(self.max_frame - 1) as i32
        };
        if !self.record_only && (self.added >= 0 || to < self.previous || self.previous < self.last_command) {
            let u = if self.added < 0 { to } else { to.min(self.added - 1) };
            let start = if u < self.previous {
                let from = (u + 1) as usize;
                let through = self.previous as usize;
                for chunk in (from / FRAMES_PER_CHUNK..=through / FRAMES_PER_CHUNK).rev() {
                    let mut frames = self.frames_between(chunk, from, through);
                    while frames != 0 {
                        let last = u32::BITS - 1 - frames.leading_zeros();
                        self.undo(chunk * FRAMES_PER_CHUNK + last as usize);
                        frames &= !(1 << last);
                    }
                }
                u + 1
            } else {
                self.previous + 1
            };
            if start <= to {
                let from = start as usize;
                let through = to as usize;
                for chunk in from / FRAMES_PER_CHUNK..=through / FRAMES_PER_CHUNK {
                    let mut frames = self.frames_between(chunk, from, through);
                    while frames != 0 {
                        let first = frames.trailing_zeros();
                        self.execute(chunk * FRAMES_PER_CHUNK + first as usize, combo, info)?;
                        frames &= frames - 1;
                    }
                }
            }
        }
        if let Some((time, score)) = self.pending_fixed.take() {
            let frame = core::frame(time);
            if self.fixed.contains_key(&frame) {
                return Err("two fixed scores in one frame".into());
            }
            self.fixed.insert(frame, score);
            if (0..self.max_frame as i32).contains(&frame) { self.mark(frame as usize); }
            self.last_command = self.last_command.max(frame);
            self.score = self.score.wrapping_add(score);
        }
        self.previous = to;
        self.added = -1;
        Ok(self.score)
    }
    fn undo(&mut self, frame: usize) {
        let chunk = frame / FRAMES_PER_CHUNK;
        let at = frame % FRAMES_PER_CHUNK;
        self.score = self.score.wrapping_sub(self.note_scores[chunk][at]);
        if let Some(&value) = self.fixed.get(&(frame as i32)) {
            self.score = self.score.wrapping_sub(value);
        }
        if !self.factors[frame / FRAMES_PER_CHUNK][frame % FRAMES_PER_CHUNK].is_empty() {
            Rc::make_mut(&mut self.diffs[frame / FRAMES_PER_CHUNK])[frame % FRAMES_PER_CHUNK].undo(&mut self.calculator.factors);
            self.calculator.memo_dirty = true;
        }
    }
    fn execute(
        &mut self,
        frame: usize,
        combo: &Combo,
        info: Option<&dyn GekisoCombo>,
    ) -> Result<(), String> {
        let chunk = frame / FRAMES_PER_CHUNK;
        let at = frame % FRAMES_PER_CHUNK;
        let (fl, nl) = (&self.factors[chunk][at], &self.notes[chunk][at]);
        if fl.is_empty() && nl.len() <= 1 {
            if let Some(note) = nl.first() {
                let count = if self.calculator.factors_only { 0 } else { self.calculator.combo_for(note, combo) };
                let value = self.calculator.note_score(count, note, info)?;
                if value != 0 || self.note_scores[chunk][at] != 0 {
                    Rc::make_mut(&mut self.note_scores[chunk])[at] = value;
                    self.score = self.score.wrapping_add(value);
                }
            }
            if let Some(&value) = self.fixed.get(&(frame as i32)) {
                self.score = self.score.wrapping_add(value);
                self.rank_bonus = self.rank_bonus.wrapping_add(value);
            }
            return Ok(());
        }
        let mut factor_stack = [0usize, 1, 2, 3, 4, 5, 6, 7];
        let factor_order = if fl.len() <= factor_stack.len() {
            &mut factor_stack[..fl.len()]
        } else {
            self.factor_order.clear();
            self.factor_order.extend(0..fl.len());
            self.factor_order.as_mut_slice()
        };
        factor_order.sort_unstable_by_key(|&i| (fl[i].time, fl[i].owner, i));
        let mut note_stack = [0usize, 1, 2, 3, 4, 5, 6, 7];
        let note_order = if nl.len() <= note_stack.len() {
            &mut note_stack[..nl.len()]
        } else {
            self.note_order.clear();
            self.note_order.extend(0..nl.len());
            self.note_order.as_mut_slice()
        };
        note_order.sort_unstable_by_key(|&i| (nl[i].time, nl[i].id, i));
        let (mut a, mut b) = (0, 0);
        let mut subtotal = 0_i32;
        while a < factor_order.len() || b < note_order.len() {
            if a < factor_order.len()
                && (b >= note_order.len() || nl[note_order[b]].time >= fl[factor_order[a]].time)
            {
                let c = &fl[factor_order[a]];
                self.calculator.factors.apply(c);
                if c.note != 0 || c.combo != 0 || c.judgement != 0 { self.calculator.memo_dirty = true; }
                Rc::make_mut(&mut self.diffs[frame / FRAMES_PER_CHUNK])[frame % FRAMES_PER_CHUNK].add(c);
                a += 1;
            } else {
                let n = &nl[note_order[b]];
                let count = if self.calculator.factors_only { 0 } else { self.calculator.combo_for(n, combo) };
                let value = self.calculator.note_score(count, n, info)?;
                subtotal = subtotal.wrapping_add(value);
                b += 1;
            }
        }
        if !nl.is_empty() && (subtotal != 0 || self.note_scores[chunk][at] != 0) {
            Rc::make_mut(&mut self.note_scores[chunk])[at] = subtotal;
            self.score = self.score.wrapping_add(subtotal);
        }
        if let Some(&value) = self.fixed.get(&(frame as i32)) {
            self.score = self.score.wrapping_add(value);
            self.rank_bonus = self.rank_bonus.wrapping_add(value);
        }
        Ok(())
    }
}
