//! Ordered vector dynamic programming for the merged website's Gekiso lottery.
//! Called only inside the compute backend, never from Flutter's UI isolate.
use rustc_hash::FxHashMap as HashMap;
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, collections::BTreeMap, sync::Arc};
const HIGH: u64 = (1 << 18) - 1;
const MISS_USED: u64 = 1 << 19;
const PREV_MISS: u64 = 1 << 20;
const FRAME_LOT: u64 = 1 << 21;
const FRAME_MISS: u64 = 1 << 22;
const BONUS: u64 = 1 << 23;
const SKILL: u64 = 1 << 24;
const SKILL_PREV: u64 = 1 << 25;
const FLAGS: u64 = MISS_USED | PREV_MISS | FRAME_LOT | FRAME_MISS | BONUS | SKILL | SKILL_PREV;
fn lots(s: u64) -> usize {
    ((s >> 8) & 7) as usize
}
fn next(s: u64) -> i32 {
    ((s >> 11) & 7) as i32 - 1
}
fn rush(s: u64) -> usize {
    ((s >> 14) & 7) as usize
}
fn minimum(s: u64) -> i32 {
    ((s >> 17) & 3) as i32
}
fn pack(l: usize, n: i32, r: usize, m: i32, flags: u64) -> u64 {
    ((l.min(7) as u64) << 8)
        + (((n + 1) as u64) << 11)
        + ((r as u64) << 14)
        + ((m as u64) << 17)
        + flags
}
fn floor_i32(x: f32) -> i32 {
    (x.floor() as i64) as i32
}
#[derive(Clone, Deserialize)]
pub struct Input {
    pub(crate) maximum: usize,
    pub(crate) rush_maximum: usize,
    pub(crate) base: Vec<[i32; 4]>,
    pub(crate) lots: Vec<[i32; 3]>,
    pub(crate) ranges: Vec<usize>,
    /// Shared by every deck whose live records the same range states.
    pub(crate) frames: Arc<Vec<Frame>>,
    pub(crate) notes: Vec<Note>,
    pub(crate) actions: Vec<Action>,
    pub(crate) miss: Vec<(i32, f64)>,
    pub(crate) modulus: usize,
    pub(crate) initial: usize,
}
impl Input {
    /// Bytes this input holds apart from its shared frame stream.
    pub(crate) fn retained_bytes(&self) -> usize {
        use std::mem::size_of;
        size_of::<Self>()
            + self.base.capacity() * size_of::<[i32; 4]>()
            + self.lots.capacity() * size_of::<[i32; 3]>()
            + self.ranges.capacity() * size_of::<usize>()
            + self.notes.capacity() * size_of::<Note>()
            + self.actions.capacity() * size_of::<Action>()
            + self.miss.capacity() * size_of::<(i32, f64)>()
    }
}
#[derive(Clone, Deserialize)]
pub(crate) struct Frame {
    pub(crate) time: i32,
    pub(crate) states: Vec<i32>,
    pub(crate) updates: Vec<usize>,
    pub(crate) playing: i32,
    pub(crate) judged: Vec<Judged>,
}
#[derive(Clone, Deserialize)]
pub(crate) struct Judged {
    pub(crate) id: i32,
    pub(crate) time: i32,
    pub(crate) scores: [i32; 4],
}
#[derive(Clone, Deserialize)]
pub(crate) struct Note {
    pub(crate) frame: usize,
    pub(crate) range: usize,
    pub(crate) op: i32,
    pub(crate) judgement: i32,
    pub(crate) time: i32,
    pub(crate) speed: f64,
    pub(crate) buff: i32,
}
#[derive(Clone, Deserialize)]
pub(crate) struct Action {
    pub(crate) frame: usize,
    pub(crate) range: usize,
    pub(crate) kind: i32,
    pub(crate) value: i32,
    pub(crate) chance: f64,
}
#[derive(Clone, Serialize, Default)]
pub struct Output {
    pub(crate) curve: BTreeMap<i32, [f64; 3]>,
    pub(crate) residues: Vec<f64>,
}
struct Row {
    key: u64,
    values: Vec<f64>,
    low: usize,
    high: usize,
}
struct Rows {
    rows: Vec<Row>,
    index: HashMap<u64, usize>,
}
impl Rows {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            index: HashMap::default(),
        }
    }
    fn at(&mut self, key: u64, width: usize, pool: &mut Vec<Vec<f64>>) -> &mut Row {
        let i = if let Some(&i) = self.index.get(&key) {
            i
        } else {
            let i = self.rows.len();
            self.index.insert(key, i);
            let values = pool.pop().unwrap_or_else(|| vec![0.0; width]);
            self.rows.push(Row {
                key,
                values,
                low: width,
                high: 0,
            });
            i
        };
        &mut self.rows[i]
    }
    fn adopt(&mut self, row: Row, pool: &mut Vec<Vec<f64>>) {
        if let Some(&i) = self.index.get(&row.key) {
            let into = &mut self.rows[i];
            for g in row.low..row.high {
                into.values[g] += row.values[g];
            }
            into.low = into.low.min(row.low);
            into.high = into.high.max(row.high);
            release(row, pool);
        } else {
            self.index.insert(row.key, self.rows.len());
            self.rows.push(row);
        }
    }
}
fn release(mut row: Row, pool: &mut Vec<Vec<f64>>) {
    if row.high > row.low {
        row.values[row.low..row.high].fill(0.0);
    }
    pool.push(row.values);
}
thread_local! { static POOL: RefCell<(usize, Vec<Vec<f64>>)> = const { RefCell::new((0, Vec::new())) }; }
/// The rows one consumed lot leads to (by row and buff), stored once and referenced by index.
#[derive(Default)]
struct LotTransitions {
    index: HashMap<(u64, i32), usize>,
    lists: Vec<Vec<(u64, f64, usize)>>,
}
struct Engine<'a> {
    input: &'a Input,
    width: usize,
    rows: Rows,
    spare: Rows,
    pool: Vec<Vec<f64>>,
    draws: HashMap<(usize, i32, i32), Vec<(f64, i32)>>,
    transitions: LotTransitions,
    lot_tables: [Vec<(i32, i32)>; 5],
}
impl<'a> Engine<'a> {
    fn new(input: &'a Input) -> Self {
        let width = input.maximum.max(input.rush_maximum);
        let pool = POOL.with(|p| {
            let mut p = p.borrow_mut();
            if p.0 == width {
                std::mem::take(&mut p.1)
            } else {
                Vec::new()
            }
        });
        let mut lot_tables: [Vec<(i32, i32)>; 5] = std::array::from_fn(|_| Vec::new());
        for &[kind, result, weight] in &input.lots {
            if weight > 0 && (0..5).contains(&kind) {
                lot_tables[kind as usize].push((weight, result));
            }
        }
        for t in &mut lot_tables {
            t.sort_by_key(|a| std::cmp::Reverse(a.1));
        }
        Self {
            input,
            width,
            rows: Rows::new(),
            spare: Rows::new(),
            pool,
            draws: HashMap::default(),
            transitions: LotTransitions::default(),
            lot_tables,
        }
    }
    fn maximum(&self, r: usize) -> usize {
        if r > 0 {
            self.input.rush_maximum
        } else {
            self.input.maximum
        }
    }
    fn pass(&mut self, mut step: impl FnMut(&mut Self, Row)) {
        let mut from = std::mem::replace(
            &mut self.rows,
            std::mem::replace(&mut self.spare, Rows::new()),
        );
        for row in from.rows.drain(..) {
            step(self, row);
        }
        from.index.clear();
        self.spare = from;
    }
    fn unary(&mut self, mut f: impl FnMut(u64) -> u64) {
        // Keys are only looked up in the destination of a pass. Preserve source
        // row order and defer merging, just as the website's pending transforms.
        for row in &mut self.rows.rows {
            row.key = (row.key & !HIGH) + (f((row.key & HIGH) << 8) >> 8);
        }
    }
    fn axpy(&mut self, key: u64, row: &Row, scale: f64, low: usize, high: usize) {
        if scale <= 0.0 {
            return;
        }
        let into = self.rows.at(key, self.width, &mut self.pool);
        for g in low..high {
            into.values[g] += row.values[g] * scale;
        }
        into.low = into.low.min(low);
        into.high = into.high.max(high);
    }
    /// Adds `scale ×` gauge values [a, b) of `row` to row `key` at gauge `g + offset`.
    fn block(&mut self, key: u64, row: &Row, a: usize, b: usize, offset: i64, scale: f64) {
        let into = self.rows.at(key, self.width, &mut self.pool);
        let from = (a as i64 + offset) as usize;
        let to = (b as i64 + offset) as usize;
        for (out, value) in into.values[from..to].iter_mut().zip(&row.values[a..b]) {
            *out += value * scale;
        }
        into.low = into.low.min(from);
        into.high = into.high.max(to);
    }
    /// Gauge addition with overflow into lots at the row's gauge maximum; with `buff`, a row left holding a lot
    /// consumes it at once (a luck note's draw), without an intermediate distribution.
    fn shift(&mut self, key: u64, row: &Row, scale: f64, add: i32, buff: Option<i32>) {
        if scale <= 0.0 {
            return;
        }
        let maximum = self.maximum(rush((key & HIGH) << 8)) as i64;
        let l = key & 7;
        let mut overflow = (row.low as i64 + add as i64) / maximum;
        loop {
            let a = (row.low as i64).max(overflow * maximum - add as i64);
            let b = (row.high as i64).min((overflow + 1) * maximum - add as i64);
            if a >= row.high as i64 {
                break;
            }
            if b > a {
                let to = if overflow == 0 {
                    key
                } else {
                    key - l + (l + overflow as u64).min(7)
                };
                let offset = add as i64 - overflow * maximum;
                match buff {
                    Some(buff) if to & 7 != 0 => {
                        self.consume_block(to, row, a as usize, b as usize, offset, scale, buff)
                    }
                    _ => self.block(to, row, a as usize, b as usize, offset, scale),
                }
            }
            overflow += 1;
        }
    }
    fn draw(&mut self, kind: usize, buff: i32, min: i32) -> Vec<(f64, i32)> {
        if let Some(v) = self.draws.get(&(kind, buff, min)) {
            return v.clone();
        }
        let buff_percent = buff;
        let items = &self.lot_tables[kind];
        let buff = buff as f32 / 100.0;
        let buffed = |w: i32| floor_i32((buff + 1.0) * w as f32);
        let mut total = items
            .iter()
            .fold(0i32, |sum, item| sum.wrapping_add(item.0)) as i64;
        let mut per = 0;
        if min >= 1 {
            let (mut included, mut excluded, mut n) = (0i64, 0i64, 0i64);
            for &(w, r) in items {
                if r >= min {
                    included += buffed(w) as i64;
                    n += 1;
                } else {
                    excluded += buffed(w) as i64;
                }
            }
            per = if n == 0 { 0 } else { excluded / n };
            total = included + per * n;
        }
        let mut out = Vec::new();
        let mut lo = 0i64;
        for &(w, r) in items {
            if min >= 1 && r < min {
                continue;
            }
            let hi = lo + per + buffed(w) as i64;
            let a = lo.max(0).min(total);
            let b = hi.max(0).min(total);
            if b > a {
                merge(&mut out, (b - a) as f64 / total as f64, r);
            }
            lo = hi;
        }
        self.draws.insert((kind, buff_percent, min), out.clone());
        out
    }
    fn lot_rows(&mut self, high: u64, buff: i32) -> usize {
        if let Some(&i) = self.transitions.index.get(&(high, buff)) {
            return i;
        }
        let s = high << 8;
        let l = lots(s) - 1;
        let mut min = minimum(s);
        let first = if next(s) == -1 {
            let v = self.draw(0, buff, min);
            min = 0;
            v
        } else {
            vec![(1.0, next(s))]
        };
        let mut out = Vec::new();
        for (pn, result) in first {
            let mut flags = s & FLAGS;
            let mut r = rush(s);
            if r == 0 || result != 3 {
                if result == 3 {
                    flags |= BONUS;
                } else {
                    flags &= !BONUS;
                }
            }
            let before = self.maximum(r);
            if (0..3).contains(&result) {
                r = 0;
            } else if result == 3 {
                r = (r + 1).min(4);
            }
            let after = self.maximum(r);
            flags |= FRAME_LOT;
            if result == 0 {
                flags |= FRAME_MISS;
            }
            let kind = if r < 4 { [0, 4, 3, 2][r] } else { 1 };
            for (pr, v) in self.draw(kind, buff, min) {
                out.push((
                    pack(l, v, r, 0, flags) >> 8,
                    pn * pr,
                    if before != after && after >= 1 {
                        after
                    } else {
                        0
                    },
                ));
            }
        }
        let i = self.transitions.lists.len();
        self.transitions.lists.push(out);
        self.transitions.index.insert((high, buff), i);
        i
    }
    /// Consumes one lot from row `key`, holding `scale ×` values [a, b) of `row` at gauge `g + offset`.
    #[allow(clippy::too_many_arguments)]
    fn consume_block(
        &mut self,
        key: u64,
        row: &Row,
        a: usize,
        b: usize,
        offset: i64,
        scale: f64,
        buff: i32,
    ) {
        let list = self.lot_rows(key & HIGH, buff);
        for k in 0..self.transitions.lists[list].len() {
            let (next, chance, after) = self.transitions.lists[list][k];
            let to = (key & !HIGH) + next;
            let q = chance * scale;
            if q <= 0.0 {
                continue;
            }
            // A rush start converts the gauge above its new maximum into lots.
            let split = if after == 0 {
                b
            } else {
                ((after as i64 + 1 - offset).max(a as i64) as usize).min(b)
            };
            if split > a {
                self.block(to, row, a, split, offset, q);
            }
            let l = to & 7;
            for g in split..b {
                let value = row.values[g];
                if value == 0.0 {
                    continue;
                }
                let gauge = (g as i64 + offset) as usize;
                let into = self.rows.at(
                    to - l + (l + (gauge / after) as u64).min(7),
                    self.width,
                    &mut self.pool,
                );
                let rem = gauge % after;
                into.values[rem] += value * q;
                into.low = into.low.min(rem);
                into.high = into.high.max(rem + 1);
            }
        }
    }
    fn consume(&mut self, row: &Row, buff: i32) {
        self.consume_block(row.key, row, row.low, row.high, 0, 1.0, buff);
    }
    fn miss_hit(s: u64) -> bool {
        s & (PREV_MISS | MISS_USED) == PREV_MISS
    }
    fn miss(&mut self) {
        if !self
            .rows
            .rows
            .iter()
            .any(|r| Self::miss_hit((r.key & HIGH) << 8))
        {
            return;
        }
        for &(value, chance) in &self.input.miss {
            self.pass(|e, row| {
                if !Self::miss_hit((row.key & HIGH) << 8) {
                    e.rows.adopt(row, &mut e.pool);
                    return;
                }
                let gauge = (e.maximum(rush((row.key & HIGH) << 8)) as i32).wrapping_mul(value);
                e.shift(
                    row.key,
                    &row,
                    chance,
                    floor_i32(gauge as f32 / 10000.0),
                    None,
                );
                e.axpy(row.key, &row, 1.0 - chance, row.low, row.high);
                release(row, &mut e.pool);
            });
        }
        self.unary(|s| if Self::miss_hit(s) { s | MISS_USED } else { s });
    }
    fn pending(&mut self, buff: i32) {
        let ready = |key: u64| {
            let s = (key & HIGH) << 8;
            lots(s) > 0 && s & FRAME_LOT == 0
        };
        if !self.rows.rows.iter().any(|r| ready(r.key)) {
            return;
        }
        self.pass(|e, row| {
            if ready(row.key) {
                e.consume(&row, buff);
                release(row, &mut e.pool);
            } else {
                e.rows.adopt(row, &mut e.pool);
            }
        });
    }
    fn action(&mut self, a: &Action) {
        self.pass(|e, row| {
            let high = row.key & HIGH;
            let s = high << 8;
            if a.kind == 11005 {
                let result = if (0..=2).contains(&(a.value - 2)) {
                    a.value - 1
                } else {
                    0
                };
                let to = (row.key & !HIGH)
                    + (pack(lots(s), next(s), rush(s), minimum(s).max(result), s & FLAGS) >> 8);
                e.axpy(to, &row, a.chance, row.low, row.high);
            } else {
                let gauge = (e.maximum(rush(s)) as i32).wrapping_mul(a.value);
                e.shift(
                    row.key,
                    &row,
                    a.chance,
                    floor_i32(gauge as f32 / 10000.0),
                    None,
                );
            }
            e.axpy(row.key, &row, 1.0 - a.chance, row.low, row.high);
            release(row, &mut e.pool);
        });
    }
    fn note(&mut self, n: &Note, state: i32, points: &[(f64, i32)]) {
        let buff = (state <= 5).then_some(n.buff);
        self.pass(|e, row| {
            for &(p, add) in points {
                e.shift(row.key, &row, p, add, buff);
            }
            release(row, &mut e.pool);
        });
    }
    fn observe(&mut self, j: &Judged, at_frame: bool, out: &mut Output) {
        let bit = if at_frame { SKILL } else { SKILL_PREV };
        if self.input.modulus > 0 {
            self.pass(|e, mut row| {
                let high = row.key & HIGH;
                let s = high << 8;
                let score = j.scores[usize::from(s & BONUS != 0) + 2 * usize::from(s & bit != 0)];
                let residue = ((row.key >> 18) as i64 + score as i64) % e.input.modulus as i64;
                row.key = ((residue as u64) << 18) + high;
                e.rows.adopt(row, &mut e.pool);
            });
        } else {
            let mut value = [0.0; 3];
            for row in &self.rows.rows {
                let s = (row.key & HIGH) << 8;
                let bonus = s & BONUS != 0;
                let skill = s & bit != 0;
                if !bonus && !skill {
                    continue;
                }
                let total = row.values[row.low.min(row.high)..row.high]
                    .iter()
                    .fold(0.0, |a, b| a + b);
                if bonus {
                    value[0] += total;
                }
                if skill {
                    value[1] += total;
                }
                if bonus && skill {
                    value[2] += total;
                }
            }
            let into = out.curve.entry(j.id).or_default();
            for i in 0..3 {
                into[i] += value[i];
            }
        }
    }
    fn reset(&mut self, initial: usize) {
        for row in self.rows.rows.drain(..) {
            release(row, &mut self.pool);
        }
        self.rows.index.clear();
        let start = self
            .rows
            .at((initial as u64) << 18, self.width, &mut self.pool);
        start.values[0] = 1.0;
        start.low = 0;
        start.high = 1;
    }
}
fn merge(out: &mut Vec<(f64, i32)>, p: f64, v: i32) {
    if let Some(at) = out.iter_mut().find(|x| x.1 == v) {
        at.0 += p;
    } else if p > 0.0 {
        out.push((p, v));
    }
}
fn base_points(input: &Input, n: &Note) -> Vec<(f64, i32)> {
    let u = (n.judgement + 1) as u32;
    if [0, 80, 82, 100, 101, 102, 103, 104, 105, 121, 122].contains(&n.op)
        || (u < 9 && ((0x107 >> (u & 31)) & 1) != 0)
    {
        return vec![(1.0, 0)];
    }
    let sub = n.op == 21 || n.op == 120 || (n.op & -4) == 0x3c;
    let j = if sub || ((n.judgement - 5) as u32) < 2 {
        5
    } else {
        n.judgement
    };
    if ![3, 4, 5].contains(&j) {
        return vec![(1.0, 0)];
    }
    let items: Vec<_> = input
        .base
        .iter()
        .filter(|r| r[2] > 0 && r[0] == i32::from(sub) && r[1] == j)
        .collect();
    let total: i64 = items.iter().map(|r| r[2] as i64).sum();
    if total == 0 {
        return vec![(1.0, 0)];
    }
    let mut merged = Vec::new();
    for r in items {
        merge(&mut merged, r[2] as f64 / total as f64, r[3]);
    }
    merged
        .into_iter()
        .map(|(p, v)| (p, floor_i32((n.speed + 1.0) as f32 * v as f32)))
        .collect()
}
pub fn run(input: &Input) -> Result<Output, String> {
    if input.maximum == 0
        || input.rush_maximum == 0
        || input.maximum.max(input.rush_maximum) > 1_000_000
    {
        return Err("invalid gauge width".into());
    }
    let generation = crate::cancellation_generation();
    let mut e = Engine::new(input);
    let mut out = Output {
        residues: vec![0.0; input.modulus],
        ..Output::default()
    };
    let mut notes: HashMap<(usize, usize), Vec<usize>> = HashMap::default();
    let mut actions: HashMap<(usize, usize), Vec<usize>> = HashMap::default();
    let mut points: HashMap<(i32, i32, i64), Vec<(f64, i32)>> = HashMap::default();
    for (i, n) in input.notes.iter().enumerate() {
        notes.entry((n.frame, n.range)).or_default().push(i);
    }
    for (i, a) in input.actions.iter().enumerate() {
        actions.entry((a.frame, a.range)).or_default().push(i);
    }
    for &range in &input.ranges {
        if input.frames.iter().any(|f| range >= f.states.len()) {
            return Err("invalid range".into());
        }
        let Some(first) = input.frames.iter().position(|f| f.states[range] == 3) else {
            continue;
        };
        let last = input
            .frames
            .iter()
            .enumerate()
            .skip(first + 1)
            .find(|(_, f)| f.states[range] == 8)
            .map(|(i, _)| i)
            .unwrap_or(input.frames.len() - 1);
        e.reset(input.initial);
        let mut steady_at = None;
        for k in first..=last {
            if generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
            let frame = &input.frames[k];
            let state = frame.states[range];
            let changed = frame.updates.contains(&range);
            let finishing = changed && state == 8;
            let playing = frame.playing == range as i32;
            if let Some(list) = actions.get(&(k, range)) {
                for &i in list {
                    e.action(&input.actions[i]);
                }
                steady_at = None;
            }
            let empty = Vec::new();
            let list = notes.get(&(k, range)).unwrap_or(&empty);
            if list.is_empty() && frame.judged.is_empty() && !changed {
                let at = (state, playing);
                if steady_at == Some(at) {
                    continue;
                }
                let steady = e.rows.rows.iter().all(|r| {
                    let s = (r.key & HIGH) << 8;
                    let skill = s & SKILL != 0;
                    s & (PREV_MISS | FRAME_MISS | FRAME_LOT) == 0
                        && skill == (s & SKILL_PREV != 0)
                        && (!playing || skill == (rush(s) != 0))
                        && !(state == 4 && lots(s) > 0)
                });
                if steady {
                    steady_at = Some(at);
                    continue;
                }
            }
            steady_at = None;
            if !input.miss.is_empty() && (changed || playing) {
                e.miss();
            }
            e.unary(|s| {
                let mut skill = s & SKILL != 0;
                if playing {
                    skill = rush(s) != 0;
                }
                if changed && (state == 7 || state == 8) {
                    skill = false;
                }
                let mut next = s & !(SKILL | SKILL_PREV);
                if s & SKILL != 0 {
                    next |= SKILL_PREV;
                }
                if skill {
                    next |= SKILL;
                }
                next
            });
            let mut note_step = |e: &mut Engine, n: &Note| {
                let key = (n.op, n.judgement, (n.speed * 10.0 + 0.5).floor() as i64);
                let p = points.entry(key).or_insert_with(|| base_points(input, n));
                e.note(n, state, p);
            };
            let mut times: Vec<i32> = list
                .iter()
                .map(|&i| input.notes[i].time)
                .chain(frame.judged.iter().map(|j| j.time))
                .collect();
            times.sort_unstable();
            times.dedup();
            for at in times {
                if at >= frame.time {
                    continue;
                }
                for &i in list {
                    let n = &input.notes[i];
                    if n.time == at {
                        note_step(&mut e, n);
                    }
                }
                for j in &frame.judged {
                    if j.time == at {
                        e.observe(j, false, &mut out);
                    }
                }
            }
            for &i in list {
                let n = &input.notes[i];
                if n.time >= frame.time {
                    note_step(&mut e, n);
                }
            }
            if state == 4 {
                let buff = list
                    .iter()
                    .map(|&i| &input.notes[i])
                    .find(|n| n.time >= frame.time)
                    .or_else(|| list.last().map(|&i| &input.notes[i]))
                    .map(|n| n.buff)
                    .unwrap_or(0);
                e.pending(buff);
            }
            if finishing {
                e.unary(|s| s & !BONUS);
            }
            for j in &frame.judged {
                if j.time >= frame.time {
                    e.observe(j, true, &mut out);
                }
            }
            e.unary(|s| {
                let mut next = s & !(PREV_MISS | FRAME_MISS | FRAME_LOT);
                if s & FRAME_MISS != 0 {
                    next |= PREV_MISS;
                }
                if changed && state == 7 {
                    next &= !MISS_USED;
                }
                next
            });
        }
        if input.modulus > 0 {
            for r in &e.rows.rows {
                out.residues[(r.key >> 18) as usize] += r.values[r.low.min(r.high)..r.high]
                    .iter()
                    .fold(0.0, |a, b| a + b);
            }
        }
    }
    for row in e.rows.rows.drain(..) {
        release(row, &mut e.pool);
    }
    let limit = 4 * 1024 * 1024 / (e.width * 8);
    e.pool.truncate(limit);
    POOL.with(|p| *p.borrow_mut() = (e.width, std::mem::take(&mut e.pool)));
    Ok(out)
}
