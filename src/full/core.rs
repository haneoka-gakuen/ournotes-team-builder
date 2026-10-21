//! Native numeric conversions, .NET random streams, combo and life replay.
use crate::num::f;
use std::collections::BTreeMap;
pub const I32_MIN: i32 = i32::MIN;
pub fn sat_i32(x: f64) -> i32 {
    x as i32
}
pub fn floor_i32(x: f64) -> i32 {
    if x == f64::INFINITY {
        return I32_MIN;
    }
    let t = sat_i32(x);
    if t != I32_MIN && f(t as f64) > x {
        t - 1
    } else {
        t
    }
}
pub fn floor_i32_f64(x: f64) -> i32 {
    if x == f64::INFINITY {
        return I32_MIN;
    }
    let t = sat_i32(x);
    if t != I32_MIN && (t as f64) > x {
        t - 1
    } else {
        t
    }
}
pub fn ceil_i32(x: f64) -> i32 {
    if x.is_infinite() {
        return I32_MIN;
    }
    let t = sat_i32(x);
    if t != i32::MAX && f(t as f64) < x {
        t + 1
    } else {
        t
    }
}
pub fn min_nan(a: f64, b: f64) -> f64 {
    if a.is_nan() {
        b
    } else if b.is_nan() {
        a
    } else if a == b {
        if a == 0.0 && a.is_sign_negative() {
            a
        } else {
            b
        }
    } else if a < b {
        a
    } else {
        b
    }
}
pub fn sdiv(a: i32, b: i32) -> i32 {
    if b == 0 {
        0
    } else {
        a.wrapping_div(b)
    }
}
pub fn srem(a: i32, b: i32) -> i32 {
    a.wrapping_sub(sdiv(a, b).wrapping_mul(b))
}
pub fn frame(ms: i32) -> i32 {
    if ms < 0 {
        0
    } else {
        ceil_i32(f(f(ms as f64) / 40.0))
    }
}
const MBIG: i32 = 2_147_483_647;
const MSEED: i32 = 161_803_398;
pub const STREAM_SKILL: usize = 0;
pub const STREAM_LUCK: usize = 1;
#[derive(Clone)]
struct NetRandom {
    seeds: [i32; 56],
    next: usize,
    next_p: usize,
}
impl NetRandom {
    fn new(seed: i32) -> Self {
        let mut seeds = [0; 56];
        let subtraction = if seed == i32::MIN { MBIG } else { seed.abs() };
        let mut mj = MSEED.wrapping_sub(subtraction);
        seeds[55] = mj;
        let (mut mk, mut ii) = (1i32, 0);
        for _ in 1..55 {
            ii += 21;
            if ii >= 55 {
                ii -= 55;
            }
            seeds[ii] = mk;
            mk = mj.wrapping_sub(mk);
            if mk < 0 {
                mk = mk.wrapping_add(MBIG);
            }
            mj = seeds[ii];
        }
        for _ in 1..5 {
            for i in 1..56 {
                let mut n = i + 30;
                if n >= 55 {
                    n -= 55;
                }
                seeds[i] = seeds[i].wrapping_sub(seeds[1 + n]);
                if seeds[i] < 0 {
                    seeds[i] = seeds[i].wrapping_add(MBIG);
                }
            }
        }
        Self {
            seeds,
            next: 0,
            next_p: 21,
        }
    }
    fn internal(&mut self) -> i32 {
        let (mut a, mut b) = (self.next + 1, self.next_p + 1);
        if a >= 56 {
            a = 1;
        }
        if b >= 56 {
            b = 1;
        }
        let mut r = self.seeds[a].wrapping_sub(self.seeds[b]);
        if r == MBIG {
            r -= 1;
        }
        if r < 0 {
            r = r.wrapping_add(MBIG);
        }
        self.seeds[a] = r;
        self.next = a;
        self.next_p = b;
        r
    }
    fn double(&mut self) -> f64 {
        self.internal() as f64 * (1.0 / MBIG as f64)
    }
    fn integer(&mut self) -> i32 {
        let mut result = self.internal();
        if self.internal() % 2 == 0 {
            result = -result;
        }
        let mut d = result as f64 + (MBIG - 1) as f64;
        d /= (2 * MBIG as i64 - 1) as f64;
        crate::num::i32((d * 4294967295.0).trunc() - 2147483648.0)
    }
}
#[derive(Clone)]
pub struct Random {
    pub base_seed: i32,
    streams: [NetRandom; 4],
    pub draws: usize,
}
impl Random {
    pub fn new(base_seed: i32) -> Self {
        Self {
            base_seed,
            streams: std::array::from_fn(|i| {
                NetRandom::new((i as i32).wrapping_mul(-0x61c88647) ^ base_seed)
            }),
            draws: 0,
        }
    }
    pub fn value(&mut self, stream: usize) -> f64 {
        self.draws += 1;
        f(self.streams[stream].double())
    }
    pub fn integer(&mut self, stream: usize) -> i32 {
        self.draws += 1;
        self.streams[stream].integer()
    }
}
#[derive(Default, Clone)]
pub struct Combo {
    times: Vec<i32>,
    judgements: Vec<i32>,
    combos: Vec<i32>,
}
impl Combo {
    pub fn add(&mut self, time: i32, judgement: i32) {
        if judgement == 0 || judgement == 7 {
            return;
        }
        let at = self.times.partition_point(|&t| t <= time);
        self.times.insert(at, time);
        self.judgements.insert(at, judgement);
        self.combos.insert(at, 0);
        let mut combo = if at == 0 { 0 } else { self.combos[at - 1] };
        for i in at..self.times.len() {
            combo = if self.judgements[i] == 1 || self.judgements[i] == 2 {
                0
            } else {
                combo + 1
            };
            self.combos[i] = combo;
        }
    }
    pub fn at(&self, time: i32) -> i32 {
        let at = self.times.partition_point(|&t| t < time);
        if at == 0 {
            0
        } else {
            self.combos[at - 1]
        }
    }
}
#[derive(Clone, Copy)]
enum Kind {
    NoteDamage,
    SkillDamage,
    Recovery,
    GuardStart,
    GuardEnd,
    ReductionStart,
    ReductionEnd,
}
#[derive(Clone)]
struct Command {
    time: i32,
    kind: Kind,
    value: i32,
    over_heal: bool,
    safety: bool,
}
fn reduce(damage: i32, bp: i32) -> i32 {
    if bp < 1 {
        damage
    } else if (bp as u32) >> 4 > 0x270 {
        0
    } else {
        sdiv((10000_i32.wrapping_sub(bp)).wrapping_mul(damage), 10000).max(0)
    }
}
#[derive(Clone)]
pub struct Life {
    initial: i32,
    damage: [f64; 7],
    maximum: i32,
    pub current: i32,
    guards: Vec<i32>,
    guard_counter: i32,
    reductions: BTreeMap<i32, i32>,
    reduction_counter: i32,
    limits: BTreeMap<i32, i32>,
    limit_counter: i32,
    frames: usize,
    commands: BTreeMap<usize, Vec<Command>>,
    cached_frame: i32,
    cached: [i32; 3],
}
impl Life {
    pub fn new(initial: i32, damage: [f64; 7], music_length: i32) -> Self {
        let length = frame(music_length).wrapping_add(2).max(1) as usize;
        Self {
            initial,
            damage,
            maximum: initial,
            current: initial,
            guards: Vec::new(),
            guard_counter: 0,
            reductions: BTreeMap::new(),
            reduction_counter: 0,
            limits: BTreeMap::new(),
            limit_counter: 0,
            frames: length,
            commands: BTreeMap::new(),
            cached_frame: -1,
            cached: [0; 3],
        }
    }
    pub fn maximum(&self) -> i32 {
        self.maximum
    }
    fn frame_of(&self, time: i32) -> usize {
        (frame(time) as usize).min(self.frames - 1)
    }
    fn add(&mut self, command: Command) {
        let frame = self.frame_of(command.time);
        let commands = self.commands.entry(frame).or_default();
        let at = commands.partition_point(|c| c.time <= command.time);
        commands.insert(at, command);
        if frame as i32 <= self.cached_frame {
            self.cached_frame = -1;
            self.cached = [0; 3];
        }
    }
    pub fn note_damage(&mut self, time: i32, judgement: usize) {
        if judgement == 0 || judgement == 7 {
            return;
        }
        let damage = self.damage.get(judgement).copied().unwrap_or(0.0) as i32;
        if damage > 0 {
            self.add(Command {
                time,
                kind: Kind::NoteDamage,
                value: damage,
                over_heal: false,
                safety: false,
            });
        }
    }
    pub fn recovery(&mut self, time: i32, amount: i32, over_heal: bool) {
        if amount > 0 {
            self.add(Command {
                time,
                kind: Kind::Recovery,
                value: amount,
                over_heal,
                safety: false,
            });
        }
    }
    pub fn skill_damage(&mut self, time: i32, amount: i32, safety: bool) {
        if amount > 0 {
            self.add(Command {
                time,
                kind: Kind::SkillDamage,
                value: amount,
                over_heal: false,
                safety,
            });
        }
    }
    pub fn enable_reduction(&mut self, time: i32, bp: i32) -> i32 {
        self.reduction_counter += 1;
        let id = self.reduction_counter;
        self.reductions.insert(id, bp);
        self.add(Command {
            time,
            kind: Kind::ReductionStart,
            value: bp,
            over_heal: false,
            safety: false,
        });
        id
    }
    pub fn disable_reduction(&mut self, time: i32, id: i32) {
        if let Some(value) = self.reductions.remove(&id) {
            self.add(Command {
                time,
                kind: Kind::ReductionEnd,
                value,
                over_heal: false,
                safety: false,
            });
        }
    }
    pub fn add_limit(&mut self, amount: i32) -> i32 {
        self.limit_counter += 1;
        let id = self.limit_counter;
        self.limits.insert(id, amount);
        self.update_maximum();
        id
    }
    pub fn subtract_limit(&mut self, id: i32) {
        if self.limits.remove(&id).is_some() {
            self.update_maximum();
        }
    }
    fn update_maximum(&mut self) {
        let sum = self
            .limits
            .values()
            .fold(0i32, |sum, &v| sum.wrapping_add(v));
        self.maximum = self.maximum.wrapping_add(sum);
    }
    pub fn enable_guard(&mut self, time: i32) -> i32 {
        self.guard_counter += 1;
        let id = self.guard_counter;
        self.guards.push(id);
        self.add(Command {
            time,
            kind: Kind::GuardStart,
            value: 0,
            over_heal: false,
            safety: false,
        });
        id
    }
    pub fn disable_guard(&mut self, time: i32, id: i32) {
        if let Some(i) = self.guards.iter().position(|&v| v == id) {
            self.guards.remove(i);
            self.add(Command {
                time,
                kind: Kind::GuardEnd,
                value: 0,
                over_heal: false,
                safety: false,
            });
        }
    }
    fn step(&self, state: &mut [i32; 3], command: &Command) {
        let [mut life, mut guard, mut reduction] = *state;
        match command.kind {
            Kind::NoteDamage => {
                if guard < 1 {
                    life = life.wrapping_sub(reduce(command.value, reduction)).max(0);
                }
            }
            Kind::SkillDamage => {
                if guard < 1 && (life > 0 || !command.safety) {
                    life = life.wrapping_sub(reduce(command.value, reduction));
                    life = life.max(i32::from(command.safety));
                }
            }
            Kind::Recovery => {
                if life > 0 {
                    let cap = if command.over_heal {
                        self.maximum.wrapping_shl(1)
                    } else {
                        self.maximum
                    };
                    life = command.value.wrapping_add(life).min(cap);
                }
            }
            Kind::GuardStart => guard = guard.wrapping_add(1),
            Kind::GuardEnd => guard = guard.wrapping_sub(1).max(0),
            Kind::ReductionStart => reduction = reduction.wrapping_add(command.value),
            Kind::ReductionEnd => reduction = reduction.wrapping_sub(command.value).max(0),
        }
        *state = [life, guard, reduction];
    }
    pub fn at(&mut self, time: i32) -> i32 {
        let frame = self.frame_of(time);
        let c = self.cached_frame;
        let full = c >= 0 && frame as i32 <= c;
        let (mut state, start) = if c < 0 || frame as i32 <= c {
            ([self.initial, 0, 0], 0)
        } else {
            (self.cached, (c + 1) as usize)
        };
        for commands in self.commands.range(start..frame.max(start)).map(|(_, v)| v) {
            for command in commands {
                self.step(&mut state, command);
            }
        }
        if !full {
            self.cached_frame = frame as i32 - 1;
            self.cached = state;
        }
        for command in self.commands.get(&frame).into_iter().flatten() {
            if command.time > time {
                break;
            }
            self.step(&mut state, command);
        }
        state[0]
    }
    pub fn sync(&mut self, time: i32) {
        self.current = self.at(time);
    }
}
