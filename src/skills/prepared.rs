//! One bounded raw skill table per native library / WASM instance.
//! Master table identities keep unrelated releases isolated.
use super::resolve;
use crate::{
    live::Slot,
    master::*,
    power::{Member, Snap},
};
use std::{
    collections::{BTreeMap, HashMap},
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

pub(crate) struct Prepared {
    pub(crate) slots: Vec<Slot>,
    // Gekiso never needs these ids, so do not intern shapes on its cold path.
    shapes: OnceLock<(Vec<usize>, usize)>,
    bytes: usize,
}
impl Prepared {
    pub(crate) fn shapes(&self) -> &(Vec<usize>, usize) {
        self.shapes.get_or_init(|| {
            let mut intern = HashMap::new();
            let mut signature = Vec::new();
            let ids = self
                .slots
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
            (ids, intern.len())
        })
    }
    /// Move an uncached oversized table; shared tables remain immutable.
    pub(crate) fn into_slots(self: Arc<Self>) -> Vec<Slot> {
        match Arc::try_unwrap(self) {
            Ok(table) => table.slots,
            Err(table) => table.slots.clone(),
        }
    }
    /// Upper bound on retained table/key payload, including lazy shape ids.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.bytes
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
                .map(|e| e.judgements.capacity() * size_of::<usize>())
                .sum::<usize>();
            if let Some(convert) = &slot.convert {
                bytes += convert.judgements.capacity() * size_of::<usize>();
            }
            slots.push(slot);
        }
    }
    let table = Arc::new(Prepared {
        slots,
        shapes: OnceLock::new(),
        bytes,
    });
    if table.retained_bytes() <= CACHE_BYTES {
        *cache = Some((key, Arc::clone(&table)));
    }
    table
}
