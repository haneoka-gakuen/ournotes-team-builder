//! Mission ranges, combo/Just replay, luck lotteries and native stopwatches.
use super::{
    core::{self, Random, STREAM_LUCK},
    score::GekisoCombo,
};
use crate::{master::GekisoTables, num::f};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::collections::BTreeMap;
pub const WAIT: i32 = 1;
pub const STANDBY: i32 = 2;
pub const START: i32 = 3;
pub const PLAYING: i32 = 4;
pub const END: i32 = 5;
pub const DELAY: i32 = 6;
pub const COMPLETE: i32 = 7;
pub const FINISH: i32 = 8;
pub const MISSION_COMBO: i32 = 1;
pub const MISSION_LUCK: i32 = 2;
pub const MISSION_ALL: i32 = 4;
pub const FEVER_WAIT: i32 = 1;
pub const FEVER: i32 = 2;
pub const FEVER_END: i32 = 3;
fn sub_note(op: i32) -> bool {
    op == 21 || op == 120 || (op as u32 & 0xfffffffc) == 0x3c
}
fn buffed(buff: f64, weight: i32) -> i32 {
    core::floor_i32(f(f(buff + 1.0) * f(weight as f64)))
}
type Item = (i32, i32);
fn lottery_table(items: &[Item], random: i32) -> Result<Option<i32>, String> {
    if items.is_empty() {
        return Ok(None);
    }
    let total = items.iter().fold(0i32, |sum, &(w, _)| sum.wrapping_add(w));
    if total < 1 {
        return Err("lottery table with a total weight below 1".into());
    }
    let value = core::srem(random.wrapping_abs(), total);
    let mut cumulative = 0i32;
    for &(weight, point) in items {
        cumulative = cumulative.wrapping_add(weight);
        if value < cumulative {
            return Ok(Some(point));
        }
    }
    Ok(None)
}
#[derive(Clone)]
struct LuckTable {
    items: Vec<Item>,
    total: i32,
    buff: f64,
}
impl LuckTable {
    fn new(mut items: Vec<Item>) -> Result<Self, String> {
        items.sort_by_key(|item| std::cmp::Reverse(item.1));
        let total = items.iter().fold(0i32, |sum, &(w, _)| sum.wrapping_add(w));
        if !items.is_empty() && total < 1 {
            return Err("luck bonus table with a total weight below 1".into());
        }
        Ok(Self {
            items,
            total,
            buff: 0.0,
        })
    }
    fn lottery(&self, random: i32, minimum: i32) -> Option<i32> {
        if self.items.is_empty() {
            return None;
        }
        if minimum < 1 {
            let value = core::srem(random.wrapping_abs(), self.total);
            let mut cumulative = 0i32;
            for &(w, r) in &self.items {
                cumulative = cumulative.wrapping_add(buffed(self.buff, w));
                if value < cumulative {
                    return Some(r);
                }
            }
            return None;
        }
        let (mut included, mut excluded, mut count) = (0i32, 0i32, 0i32);
        for &(w, r) in &self.items {
            let weight = buffed(self.buff, w);
            if minimum <= r {
                count += 1;
                included = included.wrapping_add(weight);
            } else {
                excluded = excluded.wrapping_add(weight);
            }
        }
        if count == 0 {
            return None;
        }
        let per = core::sdiv(excluded, count);
        let total = included.wrapping_add(per.wrapping_mul(count));
        let value = core::srem(random.wrapping_abs(), total);
        let mut cumulative = 0i32;
        for &(w, r) in &self.items {
            if minimum <= r {
                cumulative = cumulative
                    .wrapping_add(per)
                    .wrapping_add(buffed(self.buff, w));
                if value < cumulative {
                    return Some(r);
                }
            }
        }
        None
    }
}
#[derive(Clone)]
pub struct Lottery {
    good: Option<Vec<Item>>,
    great: Option<Vec<Item>>,
    perfect: Option<Vec<Item>>,
    hold: Vec<Item>,
    tables: Vec<LuckTable>,
    minimum: Vec<[i32; 3]>,
    minimum_counter: i32,
    consumed: HashMap<i32, i32>,
}
impl Lottery {
    pub fn new(tables: &GekisoTables) -> Result<Self, String> {
        let (mut good, mut great, mut perfect) = (None, None, None);
        let mut hold = Vec::new();
        for &[category, judgement, weight, point] in &tables.base {
            if weight <= 0 {
                continue;
            }
            let item = (weight, point);
            if category == 0 {
                match judgement {
                    3 => good.get_or_insert_with(Vec::new).push(item),
                    4 => great.get_or_insert_with(Vec::new).push(item),
                    5 => perfect.get_or_insert_with(Vec::new).push(item),
                    _ => {}
                }
            } else if category == 1 && judgement == 5 {
                hold.push(item);
            }
        }
        let mut bonus = vec![Vec::new(); 5];
        for &[kind, result, weight] in &tables.lots {
            if weight > 0 && (0..5).contains(&kind) {
                bonus[kind as usize].push((weight, result));
            }
        }
        Ok(Self {
            good,
            great,
            perfect,
            hold,
            tables: bonus
                .into_iter()
                .map(LuckTable::new)
                .collect::<Result<_, _>>()?,
            minimum: Vec::new(),
            minimum_counter: 0,
            consumed: HashMap::default(),
        })
    }
    pub fn set_buff(&mut self, percent: i32) {
        let buff = f(f(percent as f64) / 100.0);
        for table in &mut self.tables {
            table.buff = buff;
        }
    }
    pub fn enable_minimum(&mut self, result: i32, limit: i32) -> i32 {
        self.minimum_counter = self.minimum_counter.wrapping_add(1);
        self.minimum.push([
            self.minimum_counter,
            result,
            if limit >= 1 { limit } else { -1 },
        ]);
        self.minimum_counter
    }
    pub fn disable_minimum(&mut self, id: i32) {
        self.minimum.retain(|entry| entry[0] != id);
    }
    pub fn minimum_active(&self, id: i32) -> bool {
        self.minimum.iter().any(|entry| entry[0] == id)
    }
    pub fn take_consumed(&mut self, id: i32) -> i32 {
        self.consumed.remove(&id).unwrap_or(-1)
    }
    pub fn bonus(&mut self, kind: i32, time: i32, random: &mut Random) -> Result<i32, String> {
        let sample = random.integer(STREAM_LUCK);
        let applied = self.minimum.iter().fold(0, |m, e| m.max(e[1]));
        let table = self
            .tables
            .get(kind as usize)
            .ok_or("lot type out of range")?;
        let value = table
            .lottery(sample, applied)
            .ok_or("no luck lottery item")?;
        if applied >= 1 {
            self.minimum.retain_mut(|entry| {
                if entry[1] > applied || entry[2] < 0 {
                    return true;
                }
                entry[2] = entry[2].wrapping_sub(1);
                if entry[2] != 0 {
                    return true;
                }
                self.consumed.insert(entry[0], time);
                false
            });
        }
        Ok(value)
    }
    pub fn point(&self, op: i32, judgement: i32, random: &mut Random) -> Result<i32, String> {
        if [0, 80, 82, 100, 101, 102, 103, 104, 105, 121, 122].contains(&op) {
            return Ok(0);
        }
        let u = judgement.wrapping_add(1) as u32;
        if u < 9 && ((0x107 >> (u & 31)) & 1) != 0 {
            return Ok(0);
        }
        let sample = random.integer(STREAM_LUCK);
        let items = if sub_note(op) {
            Some(&self.hold)
        } else if (judgement.wrapping_sub(5) as u32) < 2 {
            self.perfect.as_ref()
        } else if judgement == 4 {
            self.great.as_ref()
        } else if judgement == 3 {
            self.good.as_ref()
        } else {
            return Ok(0);
        };
        let items = items.ok_or("no base point table")?;
        lottery_table(items, sample)?.ok_or_else(|| "empty base point table".into())
    }
}
#[derive(Clone)]
pub struct LuckScore {
    pub total_bonus: i32,
    pub gauge: i32,
    pub lots: i32,
    pub rush: i32,
    pub results: [i32; 4],
    pub next: i32,
    pub gauge_max: i32,
    default_max: i32,
    rush_max: i32,
}
impl Default for LuckScore {
    fn default() -> Self {
        Self {
            total_bonus: 0,
            gauge: 0,
            lots: 0,
            rush: 0,
            results: [0; 4],
            next: -1,
            gauge_max: 100,
            default_max: 100,
            rush_max: 50,
        }
    }
}
impl LuckScore {
    pub fn add_gauge(&mut self, value: i32) -> Result<(), String> {
        let gauge = self.gauge.wrapping_add(value);
        let maximum = self.gauge_max;
        if maximum <= gauge {
            if maximum <= 0 {
                return Err("luck gauge maximum below 1".into());
            }
            self.lots = self.lots.wrapping_add(core::sdiv(gauge, maximum));
            self.gauge = core::srem(gauge, maximum);
        } else {
            self.gauge = gauge;
        }
        Ok(())
    }
    fn change_maximum(&mut self, maximum: i32) {
        if maximum < 1 || self.gauge_max == maximum {
            return;
        }
        self.gauge_max = maximum;
        if self.gauge > maximum {
            self.lots = self.lots.wrapping_add(core::sdiv(self.gauge, maximum));
            self.gauge = core::srem(self.gauge, maximum);
        }
    }
    pub fn initialize(&mut self, maximum: i32, rush: i32) {
        if maximum > 0 && rush > 0 {
            self.default_max = maximum;
            self.rush_max = rush;
            self.change_maximum(maximum);
        }
    }
    pub fn lot_type(&self) -> i32 {
        if (self.rush as u32) < 4 {
            [0, 4, 3, 2][self.rush as usize]
        } else {
            1
        }
    }
    fn add_score(&mut self, result: i32) {
        let i = result.wrapping_sub(1);
        if (i as u32) < 3 {
            self.total_bonus = self.total_bonus.wrapping_add([5, 10, 10][i as usize]);
        }
        if (0..=3).contains(&result) {
            self.results[result as usize] += 1;
        }
        if (result as u32) < 3 {
            self.rush = 0;
            self.change_maximum(self.default_max);
        } else if result == 3 {
            self.rush = self.rush.wrapping_add(1);
            self.change_maximum(self.rush_max);
        }
    }
}
#[derive(Clone, Default)]
struct Storage {
    lot: Vec<(i32, f64)>,
    gauge: Vec<(i32, f64)>,
    lot_ids: HashMap<i32, f64>,
    gauge_ids: HashMap<i32, f64>,
    current: i32,
}
impl Storage {
    fn at(commands: &[(i32, f64)], time: i32) -> f64 {
        commands
            .iter()
            .filter(|c| c.0 <= time)
            .fold(0.0, |sum, c| f(sum + c.1))
    }
    fn add(&mut self, gauge: bool, time: i32, value: f64) -> i32 {
        self.current = self.current.wrapping_add(1);
        if gauge {
            self.gauge_ids.insert(self.current, value);
            self.gauge.push((time, value));
        } else {
            self.lot_ids.insert(self.current, value);
            self.lot.push((time, value));
        }
        self.current
    }
    fn subtract(&mut self, gauge: bool, time: i32, id: i32) -> Result<(), String> {
        let value = if gauge {
            self.gauge_ids.remove(&id)
        } else {
            self.lot_ids.remove(&id)
        }
        .ok_or("luck factor id not found")?;
        if gauge {
            self.gauge.push((time, -value));
        } else {
            self.lot.push((time, -value));
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct Range {
    pub start: i32,
    pub end: i32,
    pub mission: i32,
    pub targets: HashSet<i32>,
}
#[derive(Clone)]
pub struct State {
    pub state: i32,
    pub time_to: i32,
    pub combo: i32,
    pub max_combo: i32,
    pub just: i32,
    pub raw_just: i32,
    pub cumulative: i32,
    pub start_score: i32,
    pub end_score: i32,
    pub last_combo_ms: i32,
    pub last_just_ms: i32,
    pub luck: LuckScore,
    elapsed: f64,
    stopped: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            state: WAIT,
            time_to: 0,
            combo: 0,
            max_combo: 0,
            just: 0,
            raw_just: 0,
            cumulative: 0,
            start_score: 0,
            end_score: 0,
            last_combo_ms: -1,
            last_just_ms: -1,
            luck: LuckScore::default(),
            elapsed: 0.0,
            stopped: true,
        }
    }
}
impl State {
    pub fn score(&self) -> i32 {
        self.end_score.wrapping_sub(self.start_score)
    }
}
#[derive(Default, Clone, Copy)]
struct Resume {
    processed: usize,
    combo: i32,
    max_combo: i32,
    just: i32,
    raw_just: i32,
    cumulative: i32,
    combo_bonus: f64,
    just_bonus: f64,
    i_cb: usize,
    i_jb: usize,
    i_just: usize,
    i_combo: usize,
}
#[derive(Clone)]
struct Rule {
    range: i32,
    unit: i32,
    effect: i32,
    max_cumulative: i32,
    max_effect: i32,
}
fn cumulative_bonus(rules: &BTreeMap<i32, Rule>, range: usize, x: i32) -> i32 {
    let mut total = 0i32;
    for r in rules.values() {
        if r.range != range as i32 || r.unit <= 0 {
            continue;
        }
        let mut steps = core::sdiv(x, r.unit);
        if !(steps <= r.max_cumulative || r.max_cumulative <= 0) {
            steps = r.max_cumulative;
        }
        let mut value = steps.wrapping_mul(r.effect);
        if r.max_effect > 0 && r.max_effect <= value {
            value = r.max_effect;
        }
        total = total.wrapping_add(value);
    }
    total
}
pub trait LuckHandle {
    fn add(&mut self, time: i32, percent: i32) -> Result<i32, String>;
    fn disable(&mut self, time: i32, id: i32) -> Result<(), String>;
}
pub struct Environment<'a> {
    pub random: &'a mut Random,
    pub handle: &'a mut dyn LuckHandle,
}
pub type GkNote = (i32, i32, i32, i32);
#[derive(Clone)]
pub struct Controller {
    pub ranges: Vec<Range>,
    pub states: Vec<State>,
    pub machine: Lottery,
    rush_percent: i32,
    last_note_delay: i32,
    luck_gauge_max: i32,
    storage: Storage,
    rush_id: i32,
    playing: Vec<usize>,
    combo_bonus_ids: HashMap<i32, i32>,
    just_bonus_ids: HashMap<i32, i32>,
    rules: BTreeMap<i32, Rule>,
    handle_id: i32,
    needs_recalculation: bool,
    full_recalculation: bool,
    sequence: i32,
    history: Vec<Vec<(i32, i32, i32)>>,
    snapshot: Vec<Vec<i32>>,
    resume: Vec<Resume>,
    combo_stack: Vec<(i32, f64, i32)>,
    just_stack: Vec<(i32, f64, i32)>,
    add_just_stack: Vec<(i32, f64, i32)>,
    add_combo_stack: Vec<(i32, f64, i32)>,
    protection: Vec<(i32, i32, i32, i32)>,
    unlimited: Vec<(i32, i32)>,
    limited: Vec<(i32, i32, i32)>,
    limited_ids: HashSet<i32>,
    pending: Vec<Vec<(i32, i32, i32)>>,
    lot_result: i32,
    pub lot_results: Vec<i32>,
    pub updates: Vec<usize>,
    pub current_playing: i32,
    pub weighted: bool,
}
impl Controller {
    pub fn new(
        ranges: &[(i32, i32, i32)],
        notes: &[(i32, i32)],
        tables: &GekisoTables,
        rush_percent: i32,
        last_note_delay: i32,
    ) -> Result<Self, String> {
        let ranges: Vec<_> = ranges
            .iter()
            .map(|&(start, end, mission)| Range {
                start,
                end,
                mission,
                targets: notes
                    .iter()
                    .filter(|&&(_, time)| start <= time && time <= end)
                    .map(|&(id, _)| id)
                    .collect(),
            })
            .collect();
        let states = ranges
            .iter()
            .map(|r| {
                let mut s = State::default();
                s.luck.initialize(
                    if r.mission == MISSION_LUCK {
                        tables.gauge_max
                    } else {
                        0
                    },
                    if r.mission == MISSION_LUCK {
                        tables.gauge_max_rush
                    } else {
                        0
                    },
                );
                s
            })
            .collect();
        let n = ranges.len();
        Ok(Self {
            ranges,
            states,
            machine: Lottery::new(tables)?,
            rush_percent,
            last_note_delay,
            luck_gauge_max: tables.gauge_max,
            storage: Storage::default(),
            rush_id: 0,
            playing: Vec::new(),
            combo_bonus_ids: HashMap::default(),
            just_bonus_ids: HashMap::default(),
            rules: BTreeMap::new(),
            handle_id: 0,
            needs_recalculation: false,
            full_recalculation: false,
            sequence: 0,
            history: vec![Vec::new(); n],
            snapshot: vec![Vec::new(); n],
            resume: vec![Resume::default(); n],
            combo_stack: Vec::new(),
            just_stack: Vec::new(),
            add_just_stack: Vec::new(),
            add_combo_stack: Vec::new(),
            protection: Vec::new(),
            unlimited: Vec::new(),
            limited: Vec::new(),
            limited_ids: HashSet::default(),
            pending: vec![Vec::new(); n],
            lot_result: -1,
            lot_results: Vec::new(),
            updates: Vec::new(),
            current_playing: -1,
            weighted: false,
        })
    }
    fn next_id(&mut self) -> i32 {
        self.handle_id = self.handle_id.wrapping_add(1);
        self.handle_id
    }
    fn flag(&mut self) {
        self.needs_recalculation = true;
        self.full_recalculation = true;
    }
    pub fn playing_range(&self) -> i32 {
        self.playing.last().map(|&i| i as i32).unwrap_or(-1)
    }
    pub fn add_just(&mut self, time: i32, count: f64) {
        self.add_just_stack
            .push((time, f(count), self.playing_range()));
        self.flag();
    }
    pub fn add_combo(&mut self, time: i32, count: f64) {
        self.add_combo_stack
            .push((time, f(count), self.playing_range()));
        self.flag();
    }
    fn fixed_count(&mut self, index: usize, delta: f64, range: i32, just: bool) {
        if range != -1 && range != index as i32 {
            return;
        }
        let n = crate::num::i32(delta.trunc());
        let state = &mut self.states[index];
        if just {
            state.just = state.just.wrapping_add(n);
            state.cumulative = state.cumulative.wrapping_add(n);
        } else {
            state.combo = state.combo.wrapping_add(n);
            state.max_combo = state.max_combo.max(state.combo);
        }
    }
    pub fn add_luck_point(&mut self, point: i32) {
        let i = self.playing_range();
        if i >= 0 {
            self.states[i as usize].luck.total_bonus =
                self.states[i as usize].luck.total_bonus.wrapping_add(point);
        }
    }
    pub fn add_gauge(&mut self, gauge: i32) -> Result<(), String> {
        let i = self.playing_range();
        if i >= 0 {
            self.states[i as usize].luck.add_gauge(gauge)?;
        }
        Ok(())
    }
    pub fn gauge_maximum(&self) -> i32 {
        let i = self.playing_range();
        if i >= 0 {
            self.states[i as usize].luck.gauge_max
        } else {
            self.luck_gauge_max
        }
    }
    pub fn add_combo_bonus(&mut self, time: i32, bonus: i32) -> i32 {
        let id = self.next_id();
        self.combo_bonus_ids.insert(id, bonus);
        self.combo_stack
            .push((time, bonus as f64, self.sequence.wrapping_sub(1)));
        self.flag();
        id
    }
    pub fn subtract_combo_bonus(&mut self, time: i32, id: i32) -> Result<(), String> {
        let bonus = self
            .combo_bonus_ids
            .remove(&id)
            .ok_or("combo bonus id not found")?;
        self.combo_stack
            .push((time, -(bonus as f64), self.sequence.wrapping_sub(1)));
        self.flag();
        Ok(())
    }
    pub fn add_just_bonus(&mut self, time: i32, bonus: i32) -> i32 {
        let id = self.next_id();
        self.just_bonus_ids.insert(id, bonus);
        self.just_stack
            .push((time, bonus as f64, self.sequence.wrapping_sub(1)));
        self.flag();
        id
    }
    pub fn subtract_just_bonus(&mut self, time: i32, id: i32) -> Result<(), String> {
        let bonus = self
            .just_bonus_ids
            .remove(&id)
            .ok_or("Just bonus id not found")?;
        self.just_stack
            .push((time, -(bonus as f64), self.sequence.wrapping_sub(1)));
        self.flag();
        Ok(())
    }
    pub fn add_cumulative(
        &mut self,
        unit: i32,
        effect: i32,
        max_cumulative: i32,
        max_effect: i32,
    ) -> i32 {
        let id = self.next_id();
        self.rules.insert(
            id,
            Rule {
                range: self.playing_range(),
                unit,
                effect,
                max_cumulative,
                max_effect,
            },
        );
        self.flag();
        id
    }
    pub fn remove_cumulative(&mut self, id: i32) {
        if self.rules.remove(&id).is_some() {
            self.flag();
        }
    }
    pub fn enable_protection(&mut self, time: i32, limit: i32, mask: i32) -> i32 {
        let id = self.next_id();
        let value = if limit < 1 {
            1
        } else {
            self.limited_ids.insert(id);
            limit.wrapping_add(1)
        };
        self.protection.push((time, value, id, mask & 255));
        self.flag();
        id
    }
    pub fn disable_protection(&mut self, time: i32, id: i32) {
        let d = if self.limited_ids.remove(&id) { -2 } else { -1 };
        self.protection.push((time, d, id, 0));
        self.flag();
    }
    pub fn add_lot_factor(&mut self, time: i32, value: f64) -> i32 {
        self.storage.add(false, time, value)
    }
    pub fn add_gauge_factor(&mut self, time: i32, value: f64) -> i32 {
        self.storage.add(true, time, value)
    }
    pub fn subtract_lot_factor(&mut self, time: i32, id: i32) -> Result<(), String> {
        self.storage.subtract(false, time, id)
    }
    pub fn subtract_gauge_factor(&mut self, time: i32, id: i32) -> Result<(), String> {
        self.storage.subtract(true, time, id)
    }
    pub fn gauge_speed(&self, time: i32) -> f64 {
        Storage::at(&self.storage.gauge, time)
    }
    pub fn lot_buff(&self, time: i32) -> i32 {
        core::floor_i32(f(Storage::at(&self.storage.lot, time) * 100.0))
    }
    fn recalculate(&mut self) {
        for index in 0..self.states.len() {
            if self.states[index].state >= DELAY || self.history[index].is_empty() {
                continue;
            }
            let r = self.resume[index];
            let (mut cb, mut jb, mut i_cb, mut i_jb, mut i_pr, mut k, mut i_just, mut i_combo);
            if !self.full_recalculation
                && self.protection.is_empty()
                && r.processed >= 1
                && r.processed <= self.history[index].len()
            {
                let s = &mut self.states[index];
                s.combo = r.combo;
                s.max_combo = r.max_combo;
                s.just = r.just;
                s.raw_just = r.raw_just;
                s.cumulative = r.cumulative;
                cb = r.combo_bonus;
                jb = r.just_bonus;
                i_cb = r.i_cb;
                i_jb = r.i_jb;
                i_pr = 0;
                k = r.processed;
                i_just = r.i_just;
                i_combo = r.i_combo;
            } else {
                let s = &mut self.states[index];
                s.combo = 0;
                s.max_combo = 0;
                s.just = 0;
                s.raw_just = 0;
                s.cumulative = 0;
                self.snapshot[index].clear();
                self.unlimited.clear();
                self.limited.clear();
                cb = 1.0;
                jb = 1.0;
                i_cb = 0;
                i_jb = 0;
                i_pr = 0;
                k = 0;
                i_just = 0;
                i_combo = 0;
            }
            while k < self.history[index].len() {
                let (time, judgement, sequence) = self.history[index][k];
                while i_cb < self.combo_stack.len() {
                    let (ct, delta, cs) = self.combo_stack[i_cb];
                    if time <= ct && (time < ct || sequence < cs) {
                        break;
                    }
                    cb = f(cb + delta);
                    i_cb += 1;
                }
                while i_jb < self.just_stack.len() {
                    let (ct, delta, cs) = self.just_stack[i_jb];
                    if time <= ct && (time < ct || sequence < cs) {
                        break;
                    }
                    jb = f(jb + delta);
                    i_jb += 1;
                }
                while i_pr < self.protection.len() && time >= self.protection[i_pr].0 {
                    let (_, d, id, mask) = self.protection[i_pr];
                    if d < 2 {
                        if d == 1 {
                            self.unlimited.push((id, mask));
                        } else if d < -1 {
                            if let Some(i) = self.limited.iter().position(|entry| entry.0 == id) {
                                self.limited.remove(i);
                            }
                        } else if let Some(i) =
                            self.unlimited.iter().position(|entry| entry.0 == id)
                        {
                            self.unlimited.remove(i);
                        }
                    } else {
                        self.limited.push((id, d.wrapping_sub(1), mask));
                    }
                    i_pr += 1;
                }
                while i_just < self.add_just_stack.len() && self.add_just_stack[i_just].0 <= time {
                    let (_, delta, range) = self.add_just_stack[i_just];
                    self.fixed_count(index, delta, range, true);
                    i_just += 1;
                }
                while i_combo < self.add_combo_stack.len()
                    && self.add_combo_stack[i_combo].0 <= time
                {
                    let (_, delta, range) = self.add_combo_stack[i_combo];
                    self.fixed_count(index, delta, range, false);
                    i_combo += 1;
                }
                self.entry(index, judgement, core::floor_i32(cb), core::floor_i32(jb));
                self.snapshot[index].push(self.states[index].combo);
                k += 1;
            }
            let s = &self.states[index];
            self.resume[index] = Resume {
                processed: self.history[index].len(),
                combo: s.combo,
                max_combo: s.max_combo,
                just: s.just,
                raw_just: s.raw_just,
                cumulative: s.cumulative,
                combo_bonus: cb,
                just_bonus: jb,
                i_cb,
                i_jb,
                i_just,
                i_combo,
            };
            for i in i_just..self.add_just_stack.len() {
                let (_, delta, range) = self.add_just_stack[i];
                self.fixed_count(index, delta, range, true);
            }
            for i in i_combo..self.add_combo_stack.len() {
                let (_, delta, range) = self.add_combo_stack[i];
                self.fixed_count(index, delta, range, false);
            }
        }
        self.full_recalculation = false;
    }
    fn entry(&mut self, index: usize, judgement: i32, combo_delta: i32, just_delta: i32) {
        if (judgement.wrapping_sub(3) as u32) < 0xfffffffe {
            if (judgement.wrapping_sub(7) as u32) > 0xfffffffb {
                let s = &mut self.states[index];
                s.combo = s.combo.wrapping_add(combo_delta);
                s.max_combo = s.max_combo.max(s.combo);
            } else {
                return;
            }
        } else {
            let bit = 1i32.wrapping_shl((judgement & 31) as u32);
            let mask = |m: i32| if m != 0 { m } else { 6 };
            let mut protected = self.unlimited.iter().any(|entry| mask(entry.1) & bit != 0);
            for i in (0..self.limited.len()).rev() {
                let (id, remaining, m) = self.limited[i];
                if mask(m) & bit != 0 {
                    let left = remaining.wrapping_sub(1);
                    if left < 1 {
                        self.limited.remove(i);
                    } else {
                        self.limited[i] = (id, left, m);
                    }
                    protected = true;
                }
            }
            if !protected {
                let s = &mut self.states[index];
                s.max_combo = s.max_combo.max(s.combo);
                s.combo = 0;
            }
        }
        if judgement == 6 {
            let s = &mut self.states[index];
            s.raw_just = s.raw_just.wrapping_add(1);
            s.cumulative = s.cumulative.wrapping_add(just_delta);
            let bonus = cumulative_bonus(&self.rules, index, s.cumulative);
            s.just = bonus.wrapping_add(just_delta).wrapping_add(s.just);
        }
    }
    fn combo_in_range(&self, index: usize, time: i32) -> i32 {
        let history = &self.history[index];
        let snapshot = &self.snapshot[index];
        let n = history.len().min(snapshot.len());
        if n == 0 {
            return 0;
        }
        let frame = if time > 0 {
            f(f(time as f64) / 40.0).ceil() as i32
        } else {
            0
        };
        let previous = frame.wrapping_mul(40).wrapping_sub(40);
        if previous < history[0].0 {
            return 0;
        }
        let i = history[..n].partition_point(|entry| entry.0 <= previous);
        snapshot[i.saturating_sub(1)]
    }
    fn update_luck(
        &mut self,
        index: usize,
        op: i32,
        time: i32,
        judgement: i32,
        env: &mut Environment<'_>,
    ) -> Result<(), String> {
        if self.weighted {
            return Ok(());
        }
        let buff = self.lot_buff(time);
        self.machine.set_buff(buff);
        let speed = self.gauge_speed(time);
        let base = self.machine.point(op, judgement, env.random)?;
        let add = core::floor_i32(f(f(speed + 1.0) * f(base as f64)));
        self.states[index].luck.add_gauge(add)?;
        self.consume(index, time, env)
    }
    fn disable_rush(&mut self, time: i32, env: &mut Environment<'_>) -> Result<(), String> {
        if self.rush_id != 0 {
            env.handle.disable(time, self.rush_id)?;
            self.rush_id = 0;
        }
        Ok(())
    }
    fn consume(
        &mut self,
        index: usize,
        time: i32,
        env: &mut Environment<'_>,
    ) -> Result<(), String> {
        if self.states[index].state > END || self.states[index].luck.lots < 1 {
            return Ok(());
        }
        self.states[index].luck.lots -= 1;
        if self.states[index].luck.next == -1 {
            self.states[index].luck.next = self.machine.bonus(0, time, env.random)?;
        }
        let next = self.states[index].luck.next;
        let rush = self.states[index].luck.rush;
        if rush == 0 {
            if next == 3 {
                self.rush_id = env.handle.add(time, self.rush_percent)?;
            } else {
                self.disable_rush(time, env)?;
            }
        } else if next != 3 {
            self.disable_rush(time, env)?;
        }
        self.states[index].luck.add_score(next);
        if next != -1 {
            self.lot_result = next;
            self.lot_results.push(next);
        }
        self.states[index].luck.next =
            self.machine
                .bonus(self.states[index].luck.lot_type(), time, env.random)?;
        Ok(())
    }
    fn pending_lots(&mut self, time: i32, env: &mut Environment<'_>) -> Result<(), String> {
        if self.weighted {
            return Ok(());
        }
        for position in 0..self.playing.len() {
            let index = self.playing[position];
            if self.ranges[index].mission == MISSION_LUCK
                && self.states[index].state == PLAYING
                && self.states[index].luck.lots != 0
                && self.lot_result == -1
            {
                self.machine.set_buff(self.lot_buff(time));
                self.consume(index, time, env)?;
            }
        }
        Ok(())
    }
    fn judge(
        &mut self,
        index: usize,
        op: i32,
        time: i32,
        judgement: i32,
        env: &mut Environment<'_>,
    ) -> Result<(), String> {
        if judgement == 0 || judgement == 7 {
            return Ok(());
        }
        self.history[index].push((time, judgement, self.sequence));
        self.sequence = self.sequence.wrapping_add(1);
        self.needs_recalculation = true;
        if (judgement.wrapping_sub(3) as u32) < 4 {
            self.states[index].last_combo_ms = time;
        }
        if judgement == 6 {
            self.states[index].last_just_ms = time;
        }
        if self.ranges[index].mission == MISSION_LUCK {
            if self.states[index].state < START {
                self.pending[index].push((op, time, judgement));
            } else {
                self.update_luck(index, op, time, judgement, env)?;
            }
        }
        Ok(())
    }
    pub fn update(
        &mut self,
        time: i32,
        judged: &[GkNote],
        fever_updates: &[(usize, i32)],
        score: i32,
        env: &mut Environment<'_>,
    ) -> Result<(), String> {
        for index in 0..self.states.len() {
            if self.states[index].state > STANDBY && !self.pending[index].is_empty() {
                let pending = std::mem::take(&mut self.pending[index]);
                for (op, time, judgement) in pending {
                    self.update_luck(index, op, time, judgement, env)?;
                }
            }
        }
        for index in 0..self.states.len() {
            for &(id, op, time, judgement) in judged {
                if self.ranges[index].targets.contains(&id) {
                    self.judge(index, op, time, judgement, env)?;
                }
            }
        }
        if self.needs_recalculation {
            self.recalculate();
            self.needs_recalculation = false;
        }
        for &(index, state) in fever_updates {
            if index >= self.states.len() {
                return Err("IndexOutOfRangeException: fever without a Gekisou range".into());
            }
            if state == FEVER_END {
                self.states[index].end_score = score;
            }
        }
        self.pending_lots(time, env)
    }
    fn state_update(&mut self, index: usize) {
        if !self.updates.contains(&index) {
            self.updates.push(index);
        }
    }
    fn elapsed(state: &State) -> i64 {
        let value = f(state.elapsed * 1000.0);
        if value == f64::INFINITY {
            -9007199254740991
        } else {
            value.trunc() as i64
        }
    }
    pub fn before_update(
        &mut self,
        dt: f64,
        time: i32,
        fever_updates: &[(usize, i32)],
        score: i32,
        env: &mut Environment<'_>,
    ) -> Result<(), String> {
        self.updates.clear();
        self.lot_result = -1;
        self.lot_results.clear();
        self.current_playing = -1;
        for (i, state) in self.states.iter_mut().enumerate() {
            state.time_to = self.ranges[i].start.wrapping_sub(time);
        }
        let mut remove = Vec::new();
        for position in 0..self.playing.len() {
            let index = self.playing[position];
            let state = self.states[index].state;
            if state < END {
                if state == START {
                    self.states[index].state = PLAYING;
                    self.state_update(index);
                    self.current_playing = index as i32;
                } else if state == PLAYING {
                    self.current_playing = index as i32;
                }
            } else if state == END {
                self.current_playing = index as i32;
                if !self.states[index].stopped {
                    self.states[index].elapsed = f(self.states[index].elapsed + dt);
                }
                if self.last_note_delay as i64 <= Self::elapsed(&self.states[index]) {
                    self.states[index].elapsed = 0.0;
                    self.states[index].stopped = false;
                    self.states[index].state = DELAY;
                    self.state_update(index);
                }
            } else if state == DELAY {
                if !self.states[index].stopped {
                    self.states[index].elapsed = f(self.states[index].elapsed + dt);
                }
                if 500 <= Self::elapsed(&self.states[index]) {
                    self.states[index].state = COMPLETE;
                    self.states[index].stopped = true;
                    self.state_update(index);
                }
            } else if state == COMPLETE {
                self.states[index].state = FINISH;
                self.state_update(index);
                remove.push(index);
                self.disable_rush(time, env)?;
            }
        }
        self.playing.retain(|index| !remove.contains(index));
        for &(index, fever) in fever_updates {
            if index >= self.states.len() {
                return Err("IndexOutOfRangeException: fever without a Gekisou range".into());
            }
            if fever == FEVER_END {
                self.states[index].state = END;
                self.states[index].elapsed = 0.0;
                self.states[index].stopped = false;
                self.state_update(index);
            } else if fever == FEVER {
                self.states[index].state = START;
                self.states[index].start_score = score;
                self.playing.push(index);
                self.state_update(index);
                self.current_playing = index as i32;
            }
        }
        for index in 0..self.states.len() {
            if self.states[index].state == WAIT && self.states[index].time_to < 4001 {
                self.states[index].state = STANDBY;
                self.state_update(index);
            }
        }
        Ok(())
    }
}
impl GekisoCombo for Controller {
    fn combo(&self, time: i32) -> Option<i32> {
        self.ranges
            .iter()
            .position(|range| {
                range.mission == MISSION_COMBO && range.start <= time && time <= range.end
            })
            .map(|i| self.combo_in_range(i, time))
    }
}
#[derive(Clone)]
pub struct FeverUpdater {
    fevers: Vec<(i32, i32)>,
    states: Vec<i32>,
}
impl FeverUpdater {
    pub fn new(fevers: Vec<(i32, i32)>) -> Self {
        let n = fevers.len();
        Self {
            fevers,
            states: vec![FEVER_WAIT; n],
        }
    }
    pub fn update(&mut self, time: i32) -> Vec<(usize, i32)> {
        let mut out = Vec::new();
        for (i, &(start, end)) in self.fevers.iter().enumerate() {
            let state = self.states[i];
            let next = if state == FEVER_WAIT && time >= start {
                FEVER
            } else if state == FEVER && time >= end {
                FEVER_END
            } else {
                0
            };
            if next != 0 {
                self.states[i] = next;
                out.push((i, next));
            }
        }
        out
    }
}
pub fn mission_pattern(t0: i32, t1: i32, t2: i32) -> i32 {
    if t0 == 0 || t1 == 0 || t2 == 0 {
        0
    } else if t0 == t1 {
        if t0 == t2 {
            1
        } else {
            3
        }
    } else if t1 != t2 && t0 != t2 {
        2
    } else {
        3
    }
}
pub fn ranking_factors(tables: &GekisoTables, pattern: i32) -> [[i32; 5]; 3] {
    let mut out = [[0; 5]; 3];
    for &[p, count, rank, percent] in &tables.ranks {
        let c = count.wrapping_sub(1);
        let r = rank.wrapping_sub(1);
        if p == pattern && (c as u32) < 3 && (r as u32) <= 4 {
            out[c as usize][r as usize] = percent;
        }
    }
    out
}
