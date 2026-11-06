//! One bounded raw skill table per native library / WASM instance.
//! Master table identities keep unrelated releases isolated. Weak scoring views
//! share normalized slot programs across charts without retaining old calls.
use super::resolve;
use crate::{
    live::{Input, Slot},
    master::*,
    power::{Member, Snap},
};
use std::{
    collections::BTreeMap,
    mem::size_of,
    sync::{Arc, Mutex, OnceLock, Weak},
};

const CACHE_BYTES: usize = 16 * 1024 * 1024;

struct MemberKey {
    skill: (i32, i32),
    band: i32,
    card_type: i32,
    character: i32,
    tags: Vec<i32>,
    live_categories: Vec<i32>,
    gekiso_categories: Vec<i32>,
    mission: i32,
}
impl MemberKey {
    fn new(m: &Member) -> Self {
        Self {
            skill: (m.card.live_skill_id, m.growth.live_skill_level),
            band: m.card.band_id,
            card_type: m.card.card_type,
            character: m.card.character_id,
            tags: m.card.best_tags.clone(),
            live_categories: m.live_categories.clone(),
            gekiso_categories: m.gekiso_categories.clone(),
            mission: m.mission_type,
        }
    }
    fn matches(&self, m: &Member) -> bool {
        self.skill == (m.card.live_skill_id, m.growth.live_skill_level)
            && self.band == m.card.band_id
            && self.card_type == m.card.card_type
            && self.character == m.card.character_id
            && self.tags == m.card.best_tags
            && self.live_categories == m.live_categories
            && self.gekiso_categories == m.gekiso_categories
            && self.mission == m.mission_type
    }
    fn bytes(&self) -> usize {
        (self.tags.capacity() + self.live_categories.capacity() + self.gekiso_categories.capacity())
            * size_of::<i32>()
    }
}
#[derive(PartialEq)]
struct SnapKey {
    skills: [i32; 2],
    levels: [i32; 2],
}
impl SnapKey {
    fn new(s: &Snap) -> Self {
        Self {
            skills: s.card.support_skills,
            levels: s.skill_levels,
        }
    }
}
struct Key {
    // Weak ownership keeps allocation identities unique without retaining old
    // table contents after a caller replaces or mutates a public Master field.
    live: Weak<BTreeMap<i32, Vec<SkillEffect>>>,
    support: Weak<BTreeMap<i32, Vec<SkillEffect>>>,
    conditions: Weak<BTreeMap<i32, Condition>>,
    sets: Weak<BTreeMap<i32, Vec<Vec<i32>>>>,
    targets: Weak<BTreeMap<i32, Target>>,
    cumulative: Weak<BTreeMap<i32, Cumulative>>,
    members: Vec<MemberKey>,
    snaps: Vec<SnapKey>,
}
impl Key {
    fn new(master: &Master, members: &[Member], snaps: &[Snap]) -> Self {
        Self {
            live: Arc::downgrade(&master.live_skills),
            support: Arc::downgrade(&master.support_skills),
            conditions: Arc::downgrade(&master.conditions),
            sets: Arc::downgrade(&master.condition_sets),
            targets: Arc::downgrade(&master.targets),
            cumulative: Arc::downgrade(&master.cumulative),
            members: members.iter().map(MemberKey::new).collect(),
            snaps: snaps.iter().map(SnapKey::new).collect(),
        }
    }
    fn matches(&self, master: &Master, members: &[Member], snaps: &[Snap]) -> bool {
        self.live.as_ptr() == Arc::as_ptr(&master.live_skills)
            && self.support.as_ptr() == Arc::as_ptr(&master.support_skills)
            && self.conditions.as_ptr() == Arc::as_ptr(&master.conditions)
            && self.sets.as_ptr() == Arc::as_ptr(&master.condition_sets)
            && self.targets.as_ptr() == Arc::as_ptr(&master.targets)
            && self.cumulative.as_ptr() == Arc::as_ptr(&master.cumulative)
            && self.members.len() == members.len()
            && self.snaps.len() == snaps.len()
            && self.members.iter().zip(members).all(|(a, b)| a.matches(b))
            && self
                .snaps
                .iter()
                .zip(snaps)
                .all(|(a, b)| *a == SnapKey::new(b))
    }
    fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.members.capacity() * size_of::<MemberKey>()
            + self.snaps.capacity() * size_of::<SnapKey>()
            + self.members.iter().map(MemberKey::bytes).sum::<usize>()
    }
}

#[derive(PartialEq, Eq)]
struct PlayKey {
    ap: bool,
    grades: u8,
    life: u64,
    damage: [u64; 7],
    total_damage: u64,
    short_windows: [u64; 7],
    short_assist: u64,
}
pub(crate) struct Played {
    pub(crate) slots: Arc<Vec<Slot>>,
    pub(crate) ids: Vec<usize>,
    pub(crate) shapes: usize,
    pub(crate) healthy: bool,
    pub(crate) converters: Vec<usize>,
}
pub(crate) struct Prepared {
    pub(crate) slots: Arc<Vec<Slot>>,
    // Ordinary scoring prepares projections lazily; Gekiso keeps the raw programs.
    played: Mutex<Vec<(PlayKey, Weak<Played>)>>,
    gekiso: Mutex<Vec<(u64, Weak<Vec<Slot>>)>>,
    converter_limits: OnceLock<([Vec<f64>; 7], Vec<f64>)>,
    bytes: usize,
}
impl Prepared {
    pub(crate) fn for_play(&self, input: &Input) -> Arc<Played> {
        let ap = input.judgements.is_none();
        let perfect;
        let grades = if let Some(grades) = &input.judgements {
            grades.as_slice()
        } else {
            perfect = vec![5; input.times.len()];
            &perfect
        };
        let mask = grades.iter().fold(0u8, |mask, &grade| mask | (1 << grade));
        let total_damage = if ap {
            0.0
        } else {
            grades.iter().map(|&grade| input.damage[grade]).sum::<f64>()
        };
        let (window_limits, assist_limits) = self.converter_limits.get_or_init(|| {
            let mut limits: [Vec<f64>; 7] = std::array::from_fn(|_| Vec::new());
            let mut assists = Vec::new();
            for slot in self.slots.iter() {
                if let Some(convert) = &slot.convert {
                    let Some(limit) = convert
                        .limit
                        .filter(|limit| limit.is_finite() && *limit > 0.0)
                    else {
                        continue;
                    };
                    if !convert.at_start
                        && convert
                            .seconds
                            .is_some_and(|seconds| crate::num::window_ms(seconds, 0.0) == 5000)
                    {
                        limits[convert.judgement].push(limit);
                    } else if convert.at_start
                        && convert.judgement >= 3
                        && convert.judgements.iter().all(|&grade| grade <= 2)
                    {
                        assists.push(limit);
                    }
                }
            }
            for values in limits.iter_mut().chain(std::iter::once(&mut assists)) {
                values.sort_by(f64::total_cmp);
                values.dedup();
            }
            (limits, assists)
        });
        let mut maximum = [0usize; 7];
        for &start in input.skill_times.iter().take(5) {
            let from = input.clock.frame_time(start);
            let until = input.clock.frame_time(from.saturating_add(5000));
            let first = input.times.partition_point(|&time| time <= from);
            let after = input.times.partition_point(|&time| time <= until);
            let mut counts = [0; 7];
            for &grade in &grades[first..after] {
                counts[grade] += 1;
            }
            for grade in 1..7 {
                maximum[grade] = maximum[grade].max(after - first - counts[grade]);
            }
        }
        // Counters that cover every possible window are interchangeable.
        let short_windows = if ap {
            [0; 7]
        } else {
            std::array::from_fn(|grade| {
                window_limits[grade]
                    .iter()
                    .copied()
                    .find(|&limit| limit >= maximum[grade] as f64)
                    .map_or(0, f64::to_bits)
            })
        };
        let failures = grades.iter().filter(|&&grade| grade <= 2).count();
        let short_assist = if ap {
            0
        } else {
            assist_limits
                .iter()
                .copied()
                .find(|&limit| limit >= failures as f64)
                .map_or(0, f64::to_bits)
        };
        let key = PlayKey {
            ap,
            grades: mask,
            life: input.life_base.to_bits(),
            damage: input.damage.map(f64::to_bits),
            total_damage: if total_damage < input.life_base {
                total_damage.to_bits()
            } else {
                u64::MAX
            },
            short_windows,
            short_assist,
        };
        let mut cache = self.played.lock().expect("prepared-play-cache-poisoned");
        cache.retain(|(_, profile)| profile.strong_count() > 0);
        if let Some(profile) = cache
            .iter()
            .find_map(|(prior, profile)| (prior == &key).then(|| profile.upgrade()).flatten())
        {
            return profile;
        }
        let mut slots = self.slots.as_ref().clone();
        for slot in &mut slots {
            if slot.convert.as_ref().is_some_and(|convert| {
                !convert
                    .judgements
                    .iter()
                    .any(|&grade| grade != convert.judgement && mask & (1 << grade) != 0)
            }) {
                slot.convert = None;
            }
            if let Some(convert) = &mut slot.convert {
                let cap = if !convert.at_start
                    && short_windows[convert.judgement] != 0
                    && convert
                        .seconds
                        .is_some_and(|seconds| crate::num::window_ms(seconds, 0.0) == 5000)
                {
                    Some(f64::from_bits(short_windows[convert.judgement]))
                } else if convert.at_start
                    && short_assist != 0
                    && convert.judgement >= 3
                    && convert.judgements.iter().all(|&grade| grade <= 2)
                {
                    Some(f64::from_bits(short_assist))
                } else {
                    None
                };
                if let Some(cap) = cap {
                    if convert.limit.is_none_or(|limit| limit >= cap) {
                        convert.limit = Some(cap);
                    }
                }
            }
        }
        if ap {
            for slot in &mut slots {
                slot.recovery = 0;
                slot.convert = None;
                slot.cumulative
                    .retain(|factor| factor.value != 0 && factor.judgements.contains(&5));
                slot.effects
                    .retain(|effect| !effect.gate.blocks(input.life_base));
                for effect in &mut slot.effects {
                    effect.gate = crate::live::Gate {
                        kind: 0,
                        value: 0.0,
                        positive: true,
                    };
                }
                if slot.effects.is_empty() && slot.cumulative.is_empty() {
                    slot.extension_ms = 0.0;
                }
            }
        }
        let healthy = input.life_base.is_finite()
            && input.life_base >= 0.0
            && (0..7)
                .filter(|grade| mask & (1 << grade) != 0)
                .all(|grade| (3..=5).contains(&grade) && input.damage[grade] == 0.0)
            && slots.iter().all(|slot| {
                slot.recovery >= 0
                    && slot.convert.as_ref().is_none_or(|convert| {
                        convert.judgement >= 3 && input.damage[convert.judgement] == 0.0
                    })
            });
        if healthy {
            for slot in &mut slots {
                slot.effects.retain(|effect| {
                    !(effect.gate.blocks(input.life_base)
                        && effect.gate.blocks(input.life_base * 2.0))
                });
                for effect in &mut slot.effects {
                    if !effect.gate.blocks(input.life_base)
                        && !effect.gate.blocks(input.life_base * 2.0)
                    {
                        effect.gate = crate::live::Gate {
                            kind: 0,
                            value: 0.0,
                            positive: true,
                        };
                    }
                }
            }
            if slots
                .iter()
                .all(|slot| slot.effects.iter().all(|effect| effect.gate.kind == 0))
            {
                for slot in &mut slots {
                    slot.recovery = 0;
                }
            }
            for slot in &mut slots {
                if slot.effects.is_empty() && slot.cumulative.is_empty() && slot.convert.is_none() {
                    slot.extension_ms = 0.0;
                }
            }
        }
        let survives = input.life_base.is_finite()
            && input.life_base >= 0.0
            && input.life_base.fract() == 0.0
            && total_damage >= 0.0
            && total_damage < input.life_base
            && total_damage <= i32::MAX as f64
            && input
                .damage
                .iter()
                .all(|damage| damage.is_finite() && *damage >= 0.0 && damage.fract() == 0.0)
            && input.life_base * 2.0 + total_damage < (1u64 << 52) as f64
            && slots.iter().all(|slot| {
                slot.recovery >= 0
                    && slot
                        .convert
                        .as_ref()
                        .is_none_or(|convert| input.damage[convert.judgement] == 0.0)
            });
        if survives {
            let minimum = input.life_base - total_damage;
            for slot in &mut slots {
                slot.effects.retain(|effect| {
                    !(effect.gate.blocks(minimum) && effect.gate.blocks(input.life_base * 2.0))
                });
                for effect in &mut slot.effects {
                    if !effect.gate.blocks(minimum) && !effect.gate.blocks(input.life_base * 2.0) {
                        effect.gate = crate::live::Gate {
                            kind: 0,
                            value: 0.0,
                            positive: true,
                        };
                    }
                }
            }
            if slots.iter().all(|slot| {
                slot.effects
                    .iter()
                    .all(|effect| effect.gate.kind == 0 || effect.gate.value == input.life_base)
            }) {
                // With no death and only the base-LIFE gate, covering the full
                // damage budget makes larger recovery amounts unobservable.
                let observes_life = slots
                    .iter()
                    .any(|slot| slot.effects.iter().any(|effect| effect.gate.kind != 0));
                for slot in &mut slots {
                    slot.recovery = if observes_life {
                        slot.recovery.min(total_damage as i32)
                    } else {
                        0
                    };
                }
            }
        }
        let mut intern = rustc_hash::FxHashMap::default();
        let mut signature = Vec::new();
        let ids = slots
            .iter()
            .map(|slot| {
                slot.write_signature(&mut signature);
                if let Some(&id) = intern.get(signature.as_slice()) {
                    id
                } else {
                    let id = intern.len();
                    intern.insert(std::mem::take(&mut signature), id);
                    id
                }
            })
            .collect();
        let mut programs = rustc_hash::FxHashMap::default();
        let converters = slots
            .iter()
            .map(|slot| {
                slot.conversion_signature().map_or(0, |signature| {
                    let next = programs.len() + 1;
                    *programs.entry(signature).or_insert(next)
                })
            })
            .collect();
        let profile = Arc::new(Played {
            slots: Arc::new(slots),
            ids,
            shapes: intern.len(),
            healthy,
            converters,
        });
        if cache.len() >= 8 {
            cache.remove(0);
        }
        cache.push((key, Arc::downgrade(&profile)));
        profile
    }
    pub(crate) fn for_gekiso(self: Arc<Self>, life: f64) -> Arc<Vec<Slot>> {
        match Arc::try_unwrap(self) {
            Ok(table) => {
                let mut slots = table.slots;
                normalize_gekiso(Arc::make_mut(&mut slots).as_mut_slice(), life);
                slots
            }
            Err(table) => {
                let mut cache = table.gekiso.lock().expect("prepared-gekiso-cache-poisoned");
                cache.retain(|(_, slots)| slots.strong_count() > 0);
                if let Some(slots) = cache.iter().find_map(|(key, slots)| {
                    (*key == life.to_bits()).then(|| slots.upgrade()).flatten()
                }) {
                    return slots;
                }
                let mut slots = table.slots.as_ref().clone();
                normalize_gekiso(&mut slots, life);
                let slots = Arc::new(slots);
                if cache.len() >= 4 { cache.remove(0); }
                cache.push((life.to_bits(), Arc::downgrade(&slots)));
                slots
            }
        }
    }
    /// Upper bound on retained table/key payload, including scoring-view bookkeeping.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.bytes
    }
}
fn normalize_gekiso(slots: &mut [Slot], life: f64) {
    for slot in slots {
        slot.recovery = 0;
        slot.convert = None;
        slot.effects.retain(|effect| !effect.gate.blocks(life));
        for effect in &mut slot.effects {
            effect.gate = crate::live::Gate { kind: 0, value: 0.0, positive: true };
        }
        if slot.effects.is_empty() && slot.cumulative.is_empty() { slot.extension_ms = 0.0; }
    }
}

type Cache = Mutex<Option<(Key, Arc<Prepared>)>>;
static CACHE: OnceLock<Cache> = OnceLock::new();

pub(crate) fn prepare(master: &Master, members: &[Member], snaps: &[Snap]) -> Arc<Prepared> {
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("prepared-skills-cache-poisoned");
    if let Some((key, table)) = cache.as_ref() {
        if key.matches(master, members, snaps) {
            return Arc::clone(table);
        }
    }
    // Drop the previous inventory before allocating its replacement.
    cache.take();
    let key = Key::new(master, members, snaps);
    let mut slots = Vec::with_capacity(members.len() * (snaps.len() + 1));
    let mut unsupported = Vec::new();
    let mut bytes = size_of::<Prepared>()
        + key.bytes()
        + slots.capacity() * (size_of::<Slot>() + size_of::<usize>());
    for member in members {
        for snap in std::iter::once(None).chain(snaps.iter().map(Some)) {
            let slot = resolve(master, member, snap, &mut unsupported);
            bytes += slot.effects.capacity() * size_of::<crate::live::Effect>();
            bytes += slot
                .effects
                .iter()
                .map(|e| {
                    (e.judgements.capacity() + e.release_judgements.capacity()) * size_of::<usize>()
                })
                .sum::<usize>();
            if let Some(convert) = &slot.convert {
                bytes += convert.judgements.capacity() * size_of::<usize>();
            }
            slots.push(slot);
        }
    }
    let table = Arc::new(Prepared {
        slots: Arc::new(slots),
        played: Mutex::new(Vec::new()),
        gekiso: Mutex::new(Vec::new()),
        converter_limits: OnceLock::new(),
        bytes,
    });
    if table.retained_bytes() <= CACHE_BYTES {
        *cache = Some((key, Arc::clone(&table)));
    }
    table
}
