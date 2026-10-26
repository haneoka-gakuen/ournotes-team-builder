//! Gekiso context caches and safe level/support bounds for exact search.
use crate::{
    full::conditions::Performer,
    gekiso::{self, Chart, Context},
    live::{self, Slot},
    master::{GekisoSkill, Master, SkillEffect},
    num::window_ms,
    objectives::Criterion,
    power::{self, Member, Music, Player, Snap},
    search::{self, Exact, Hit, Objective, Team, Totals},
};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, VecDeque},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, OnceLock,
    },
};
const REFINE_SLACK: f64 = 1e-4;
const RELATIVE: f64 = 3e-6;
fn luck_envelope_limit(notes: usize) -> usize {
    let bytes = notes
        .saturating_mul(2 + 3 * std::mem::size_of::<usize>())
        .saturating_add(4096);
    (8 * 1024 * 1024 / bytes).clamp(1, 256)
}
#[cfg(not(target_arch = "wasm32"))]
type SharedLuckKey = (String, usize);
#[cfg(not(target_arch = "wasm32"))]
type SharedLuckCell = Arc<OnceLock<Option<Arc<crate::rewards::LuckEnvelope>>>>;
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct SharedLuckEntries {
    values: HashMap<SharedLuckKey, SharedLuckCell>,
    order: VecDeque<SharedLuckKey>,
}
#[cfg(not(target_arch = "wasm32"))]
/// Level workers of one request have the same chart, seeds and support domain.
struct SharedLuckCache {
    entries: std::sync::Mutex<SharedLuckEntries>,
    limit: usize,
}
#[cfg(not(target_arch = "wasm32"))]
impl SharedLuckCache {
    fn new(notes: usize) -> Self {
        Self {
            entries: Default::default(),
            limit: luck_envelope_limit(notes),
        }
    }
    fn cell(&self, key: &SharedLuckKey) -> SharedLuckCell {
        let mut entries = self.entries.lock().unwrap();
        if let Some(value) = entries.values.get(key) {
            return Arc::clone(value);
        }
        if entries.values.len() >= self.limit {
            if let Some(old) = entries.order.pop_front() {
                entries.values.remove(&old);
            }
        }
        let value = Arc::new(OnceLock::new());
        entries.values.insert(key.clone(), Arc::clone(&value));
        entries.order.push_back(key.clone());
        value
    }
}
fn favoured_band(master: &Master, skills: &[(i32, i32)]) -> i32 {
    for &(id, level) in skills {
        for row in master
            .gekiso_support_skills
            .get(&id)
            .into_iter()
            .flat_map(|s| &s.effects)
        {
            if row.level != level {
                continue;
            }
            for set in master
                .condition_sets
                .get(&row.condition_group)
                .into_iter()
                .flatten()
            {
                for id in set {
                    let Some(c) = master
                        .conditions
                        .get(id)
                        .filter(|c| c.kind == 5000 && c.positive)
                    else {
                        continue;
                    };
                    for id in &c.targets {
                        let band = master.targets.get(id).map(|t| t.band_id).unwrap_or(0);
                        if band != 0 {
                            return band;
                        }
                    }
                }
            }
        }
    }
    0
}
fn favoured_dominates(master: &Master, skills: &[(i32, i32)]) -> bool {
    let shape = |group: i32, flip: bool| {
        json!(master
            .condition_sets
            .get(&group)
            .into_iter()
            .flatten()
            .map(|set| set
                .iter()
                .map(
                    |id| if let Some(c) = master.conditions.get(id).filter(|c| c.kind == 5000) {
                        json!([5000, flip || c.positive, c.targets])
                    } else {
                        json!(id)
                    }
                )
                .collect::<Vec<_>>())
            .collect::<Vec<_>>())
    };
    for &(id, level) in skills {
        let rows: Vec<_> = master
            .gekiso_support_skills
            .get(&id)
            .into_iter()
            .flat_map(|s| &s.effects)
            .filter(|r| r.level == level)
            .collect();
        for row in &rows {
            if [row.trigger_group, row.release_group, row.reset_group]
                .iter()
                .any(|g| {
                    master
                        .condition_sets
                        .get(g)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .any(|id| master.conditions.get(id).is_some_and(|c| c.kind == 5000))
                })
            {
                return false;
            }
            let negative = master
                .condition_sets
                .get(&row.condition_group)
                .into_iter()
                .flatten()
                .flatten()
                .any(|id| {
                    master
                        .conditions
                        .get(id)
                        .is_some_and(|c| c.kind == 5000 && !c.positive)
                });
            if !negative {
                continue;
            }
            let wanted = shape(row.condition_group, true);
            if !rows.iter().any(|other| {
                !std::ptr::eq(*other, *row)
                    && other.kind == row.kind
                    && other.trigger_type == row.trigger_type
                    && other.execute_limit == row.execute_limit
                    && other.seconds == row.seconds
                    && other.limit_count == row.limit_count
                    && other.max_value == row.max_value
                    && other.targets == row.targets
                    && other.trigger_group == row.trigger_group
                    && other.release_group == row.release_group
                    && other.reset_group == row.reset_group
                    && other.value >= row.value
                    && shape(other.condition_group, false) == wanted
            }) {
                return false;
            }
        }
    }
    true
}
pub(crate) fn read_attributes(master: &Master, all: bool) -> Vec<&'static str> {
    let mut out = BTreeSet::new();
    let mut groups = HashSet::default();
    let mut cumulative = HashSet::default();
    for table in std::iter::once(&master.gekiso_skills).chain(if all {
        Some(&master.gekiso_support_skills)
    } else {
        None
    }) {
        for skill in table.values() {
            for row in &skill.effects {
                for group in [
                    row.trigger_group,
                    row.condition_group,
                    row.release_group,
                    row.reset_group,
                ] {
                    if group != 0 {
                        groups.insert(group);
                    }
                }
                if row.cumulative_id != 0 {
                    cumulative.insert(row.cumulative_id);
                }
            }
        }
    }
    let mut note = |ids: &[i32]| {
        for id in ids {
            let Some(t) = master.targets.get(id) else {
                continue;
            };
            if t.band_id != 0 {
                out.insert("bandId");
            }
            if t.card_type != 0 {
                out.insert("cardType");
            }
            if t.character_id != 0 {
                out.insert("characterId");
            }
            if t.tag_id != 0 {
                out.insert("tagIds");
            }
            if !t.live_categories.is_empty() {
                out.insert("liveSkillCategories");
            }
            if !t.gekiso_categories.is_empty() {
                out.insert("gekisouSkillCategories");
            }
            if t.mission_type != 0 {
                out.insert("gekisouMissionType");
            }
        }
    };
    for group in groups {
        for id in master
            .condition_sets
            .get(&group)
            .into_iter()
            .flatten()
            .flatten()
        {
            if let Some(c) = master
                .conditions
                .get(id)
                .filter(|c| c.kind == 5000 || c.kind == 3000 || c.kind == 3001)
            {
                note(&c.targets);
            }
        }
    }
    for id in &cumulative {
        if let Some(c) = master
            .cumulative
            .get(id)
            .filter(|c| c.kind == 3000 || c.kind == 3001)
        {
            note(&c.targets);
        }
    }
    for id in cumulative {
        let Some(c) = master.cumulative.get(&id) else {
            continue;
        };
        if [3002, 3003, 3004].contains(&c.kind) {
            out.insert("bandId");
        }
        if c.kind == 3005 {
            out.insert("cardType");
        }
    }
    out.into_iter().collect()
}
fn field(p: &Performer, field: &str) -> Value {
    match field {
        "bandId" => json!(p.band_id),
        "cardType" => json!(p.card_type),
        "characterId" => json!(p.character_id),
        "tagIds" => json!(p.tag_ids),
        "liveSkillCategories" => json!(p.live_skill_categories),
        "gekisouSkillCategories" => json!(p.gekisou_skill_categories),
        "gekisouMissionType" => json!(p.gekisou_mission_type),
        _ => json!(0),
    }
}
fn combo_template(master: &Master) -> Option<SkillEffect> {
    for skill in master.gekiso_skills.values() {
        if skill.mission_type != 1 {
            continue;
        }
        for row in &skill.effects {
            if row.kind != 12000 || row.trigger_type != 2 || row.condition_group != 0 {
                continue;
            }
            let Some(sets) = master.condition_sets.get(&row.trigger_group) else {
                continue;
            };
            if sets.len() == 1
                && sets[0].len() == 1
                && master
                    .conditions
                    .get(&sets[0][0])
                    .is_some_and(|c| c.kind == 7020)
            {
                return Some(row.clone());
            }
        }
    }
    None
}
#[derive(Clone)]
pub(crate) struct Weights {
    pub(crate) base: f64,
    rush: f64,
    pub(crate) general: Vec<f64>,
    pub(crate) judgements: [Vec<f64>; 3],
}
pub(crate) struct Cached {
    context: Arc<Context>,
    weights: Arc<Weights>,
    /// Own bytes, without the shared lottery record.
    bytes: usize,
    record: Option<(usize, usize)>,
}
struct Level {
    within: Master,
    deck: Vec<Performer>,
    weights: Arc<Weights>,
}
#[derive(Clone, Copy)]
struct LuckChoice {
    snap: i32,
    power: f64,
    weights: [f64; 5],
    chain: bool,
}
#[derive(Clone, Copy)]
struct PricedChoice {
    snap: i32,
    value: f64,
}
struct PricedChoices {
    events: [[Vec<PricedChoice>; 2]; 5],
}
struct LuckChoices {
    priced: OnceLock<(u64, PricedChoices)>,
    nonnegative: bool,
    rows: Vec<LuckChoice>,
    highest: Vec<(i32, f64)>,
    frontiers: [Vec<usize>; 5],
}
enum PartitionRow {
    Fixed(LuckChoice),
    Options(Arc<LuckChoices>),
}
impl PartitionRow {
    fn nonnegative(&self) -> bool {
        match self {
            Self::Fixed(value) => {
                value.power >= 0.0 && value.weights.iter().all(|&weight| weight >= 0.0)
            }
            Self::Options(values) => values.nonnegative,
        }
    }
    fn offers(&self, event: usize) -> PartitionOffers<'_> {
        match self {
            Self::Fixed(value) => PartitionOffers::Fixed(std::slice::from_ref(value).iter()),
            Self::Options(values) => PartitionOffers::Options {
                indices: values.frontiers[event].iter(),
                rows: &values.rows,
            },
        }
    }
}
enum PartitionOffers<'a> {
    Fixed(std::slice::Iter<'a, LuckChoice>),
    Options {
        indices: std::slice::Iter<'a, usize>,
        rows: &'a [LuckChoice],
    },
}
impl<'a> Iterator for PartitionOffers<'a> {
    type Item = &'a LuckChoice;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Fixed(values) => values.next(),
            Self::Options { indices, rows } => indices.next().map(|&index| &rows[index]),
        }
    }
}
struct Base {
    choice_id: u64,
    pricing_lambda: Option<f64>,
    weights: Arc<Weights>,
    deck: Vec<Performer>,
    gains: HashMap<Performer, f64>,
    numeric_gains: Option<Arc<std::sync::Mutex<HashMap<u32, f64>>>>,
    slot_gains: HashMap<usize, f64>,
    lives: HashMap<usize, f64>,
    events: HashMap<usize, [f64; 5]>,
}
type ChoiceKey = (u64, usize, u32, bool);
type SharedBase = Arc<std::sync::Mutex<Base>>;
type RefineBases = HashMap<([usize; 5], [usize; 5]), SharedBase>;
type LuckBaseCache = HashMap<(usize, [i32; 5]), (SharedBase, SharedBase)>;
type Ownership<'a> = &'a (dyn Fn(&[usize]) -> bool + 'a);
type CandidateListener<'a> = &'a mut (dyn FnMut(&[Hit]) + 'a);
type AttributeDependencies = HashMap<Vec<(bool, i32)>, (Vec<&'static str>, bool)>;
type OrderScoreKey = (Vec<usize>, Vec<i32>, i32);
type PartialOrderScores = Vec<Option<(f64, [f64; 3])>>;
type ComboBound = Arc<std::sync::Mutex<crate::rewards::DeterministicBound>>;
type RegionalBound = Arc<std::sync::Mutex<crate::rewards::FixedRegionBound>>;
#[derive(Clone, PartialEq, Eq, Hash)]
enum FixedRegionKey {
    Outside,
    Combo(String),
    Luck(Arc<crate::rewards::LuckEnvelope>),
    Just(usize),
    Guarded(Box<FixedRegionKey>, usize),
}
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum FixedConversionBound {
    Trace(usize),
    Hull(usize),
}
#[derive(Clone, PartialEq, Eq, Hash)]
struct FixedRegionalKey {
    conversion: FixedConversionBound,
    combo: String,
    luck: Arc<crate::rewards::LuckEnvelope>,
}
#[derive(Clone, Copy)]
struct FixedClassMode {
    supports: Option<bool>,
    hull: bool,
}
fn changes_context(master: &Master, p: &Performer) -> bool {
    p.gekisou_support_skills.iter().any(|&(id, level)| {
        master.gekiso_support_skills.get(&id).is_some_and(|skill| {
            skill.effects.iter().any(|row| {
                row.level == level
                    && ((11000..=11005).contains(&row.kind)
                        || row.kind == 13005
                        || row.kind == 12006)
            })
        })
    })
}
#[derive(Clone, PartialEq, Eq, Hash)]
struct FixedLuckKey {
    members: String,
    combo: String,
    supports: bool,
    level: i32,
}
fn condition_rows<'a>(
    master: &'a Master,
    row: &SkillEffect,
) -> impl Iterator<Item = &'a crate::master::Condition> {
    [
        row.trigger_group,
        row.condition_group,
        row.release_group,
        row.reset_group,
    ]
    .into_iter()
    .flat_map(|group| {
        master
            .condition_sets
            .get(&group)
            .into_iter()
            .flatten()
            .flatten()
    })
    .filter_map(|id| master.conditions.get(id))
}
fn lottery_support_program(master: &Master, actor: &Performer) -> Option<(bool, String)> {
    if actor.gekisou_support_skills.len() != 1 {
        return None;
    }
    let mut kind = None;
    let group = |id: i32| -> Option<Value> {
        if id == 0 {
            return Some(Value::Null);
        }
        Some(Value::Array(
            master
                .condition_sets
                .get(&id)?
                .iter()
                .map(|set| {
                    Some(Value::Array(
                        set.iter()
                            .map(|id| {
                                let c = master.conditions.get(id)?;
                                if ![4011, 5000, 7000, 7010, 7013, 7020].contains(&c.kind) {
                                    return None;
                                }
                                Some(if c.kind == 5000 {
                                    json!([
                                        0,
                                        c.targets.iter().any(|id| master
                                            .targets
                                            .get(id)
                                            .is_some_and(|target| {
                                                crate::full::conditions::performer_matches(
                                                    actor, target,
                                                )
                                            }))
                                            == c.positive
                                    ])
                                } else {
                                    json!([c.kind, c.positive, c.values, c.targets])
                                })
                            })
                            .collect::<Option<Vec<_>>>()?,
                    ))
                })
                .collect::<Option<Vec<_>>>()?,
        ))
    };
    let mut program = Vec::new();
    for &(id, level) in &actor.gekisou_support_skills {
        let skill = master.gekiso_support_skills.get(&id)?;
        if skill.mission_type != 2 {
            return None;
        }
        let mut rows: Vec<_> = skill.effects.iter().filter(|r| r.level == level).collect();
        rows.sort_by_key(|r| r.id);
        for row in rows {
            if ![11003, 11005].contains(&row.kind) || row.cumulative_id != 0 {
                return None;
            }
            let minimum = row.kind == 11005;
            if kind.is_some_and(|old| old != minimum) {
                return None;
            }
            kind = Some(minimum);
            if !minimum
                && (row.trigger_type != 1
                    || row.seconds != 0.0
                    || row.value < 0
                    || condition_rows(master, row).any(|c| c.kind == 4011)
                    || master
                        .condition_sets
                        .get(&row.trigger_group)
                        .is_none_or(|sets| {
                            sets.len() != 1
                                || sets[0].len() != 1
                                || master.conditions.get(&sets[0][0]).is_none_or(|c| {
                                    c.kind != 7000 || !c.positive || c.values.as_slice() != [0.0]
                                })
                        }))
            {
                return None;
            }
            program.push(json!([
                level,
                row.kind,
                row.value,
                row.trigger_type,
                row.execute_limit,
                row.seconds,
                row.limit_count,
                row.max_value,
                row.targets,
                group(row.trigger_group)?,
                group(row.condition_group)?,
                group(row.release_group)?,
                group(row.reset_group)?
            ]));
        }
    }
    Some((kind?, serde_json::to_string(&program).ok()?))
}
fn compact_lottery_supports(
    master: &Master,
    groups: &[Vec<Performer>],
) -> Option<Vec<Vec<Performer>>> {
    let mut minima = Vec::<(Vec<String>, Vec<Performer>, usize)>::new();
    let mut misses = Vec::new();
    for group in groups {
        let mut keyed = group
            .iter()
            .map(|actor| {
                lottery_support_program(master, actor).map(|(kind, key)| (kind, key, actor.clone()))
            })
            .collect::<Option<Vec<_>>>()?;
        keyed.sort_by(|a, b| a.1.cmp(&b.1));
        let minimum = keyed.first()?.0;
        if keyed.iter().any(|item| item.0 != minimum) {
            return None;
        }
        let signature = keyed.iter().map(|item| item.1.clone()).collect::<Vec<_>>();
        let actors = keyed.into_iter().map(|item| item.2).collect::<Vec<_>>();
        if minimum {
            if let Some(old) = minima.iter_mut().find(|old| old.0 == signature) {
                old.2 += 1;
            } else {
                minima.push((signature, actors, 1));
            }
        } else {
            misses.push(actors);
        }
    }
    fn min_words(
        groups: &[(Vec<String>, Vec<Performer>, usize)],
        used: &mut [usize],
        at: &mut Vec<Performer>,
        out: &mut Vec<Vec<Performer>>,
    ) {
        out.push(at.clone());
        if at.len() == 5 {
            return;
        }
        for (i, (_, actors, capacity)) in groups.iter().enumerate() {
            if used[i] == *capacity {
                continue;
            }
            used[i] += 1;
            for actor in actors {
                at.push(actor.clone());
                min_words(groups, used, at, out);
                at.pop();
            }
            used[i] -= 1;
        }
    }
    let mut words = Vec::new();
    min_words(
        &minima,
        &mut vec![0; minima.len()],
        &mut Vec::new(),
        &mut words,
    );
    let mut subsets = vec![Vec::new()];
    for group in misses {
        let old = subsets.clone();
        for prefix in old {
            if prefix.len() == 5 {
                continue;
            }
            for actor in &group {
                let mut next = prefix.clone();
                next.push(actor.clone());
                subsets.push(next);
            }
        }
    }
    let mut out = Vec::new();
    for prefix in words {
        for suffix in &subsets {
            if prefix.len() + suffix.len() <= 5 {
                let mut deck = prefix.clone();
                deck.extend_from_slice(suffix);
                out.push(deck);
            }
        }
    }
    Some(out)
}
fn lottery_points_only(master: &Master, actor: &Performer) -> bool {
    let Some((id, level)) = actor.gekisou_skill else {
        return false;
    };
    let Some(skill) = master.gekiso_skills.get(&id) else {
        return false;
    };
    let rows = skill
        .effects
        .iter()
        .filter(|row| row.level == level)
        .collect::<Vec<_>>();
    rows.iter().all(|row| {
        [11002, 11004, 3004, 4004].contains(&row.kind)
            && condition_rows(master, row).all(|condition| condition.kind != 4011)
    })
}
pub(crate) fn lottery_gauge_row<'a>(
    master: &'a Master,
    actor: &Performer,
) -> Option<&'a SkillEffect> {
    let (id, level) = actor.gekisou_skill?;
    let skill = master.gekiso_skills.get(&id)?;
    let rows = skill
        .effects
        .iter()
        .filter(|row| row.level == level)
        .collect::<Vec<_>>();
    if rows
        .iter()
        .any(|row| condition_rows(master, row).any(|condition| condition.kind == 4011))
    {
        return None;
    }
    let mut relevant = rows
        .into_iter()
        .filter(|row| ![3004, 4004].contains(&row.kind));
    let row = relevant.next()?;
    let single = |group: i32, kind: i32| {
        master.condition_sets.get(&group).is_some_and(|sets| {
            sets.len() == 1
                && sets[0].len() == 1
                && master
                    .conditions
                    .get(&sets[0][0])
                    .is_some_and(|condition| condition.kind == kind && condition.positive)
        })
    };
    (relevant.next().is_none()
        && row.kind == 11001
        && row.value >= 0
        && row.value % 10000 == 0
        && crate::num::f(crate::num::f(row.value as f64) / 10000.0) == (row.value / 10000) as f64
        && row.cumulative_id == 0
        && row.seconds == 0.0
        && row.trigger_type == 2
        && row.execute_limit == 0
        && row.condition_group == 0
        && row.reset_group == 0
        && row.limit_count == 0
        && row.max_value == 0
        && row.targets.is_empty()
        && single(row.trigger_group, 7020)
        && (row.release_group == 0 || single(row.release_group, 7013)))
    .then_some(row)
}
fn lottery_member_key(master: &Master, members: &[Performer]) -> String {
    let canonical = (|| -> Option<String> {
        let mut gauges = std::collections::BTreeMap::<String, i64>::new();
        let mut other = Vec::new();
        let mut total = 0i64;
        for actor in members {
            if lottery_points_only(master, actor) {
                continue;
            }
            let Some(row) = lottery_gauge_row(master, actor) else {
                if actor.gekisou_skill.is_some_and(|(id, level)| {
                    master.gekiso_skills[&id]
                        .effects
                        .iter()
                        .any(|row| row.level == level && row.kind == 11001)
                }) {
                    return None;
                }
                other.push(actor);
                continue;
            };
            let shape = serde_json::to_string(&json!([
                row.trigger_type,
                row.execute_limit,
                row.trigger_group,
                row.condition_group,
                row.release_group,
                row.reset_group,
                row.targets,
                row.limit_count,
                row.max_value
            ]))
            .ok()?;
            let value = (row.value / 10000) as i64;
            total += value;
            if 2 * total + 1 >= 1 << 24 {
                return None;
            }
            *gauges.entry(shape).or_default() += value;
        }
        serde_json::to_string(&(gauges, other)).ok()
    })();
    canonical.map_or_else(
        || format!("raw:{}", serde_json::to_string(members).unwrap()),
        |key| format!("sum:{key}"),
    )
}
fn lottery_member_orders(master: &Master, members: &[Performer]) -> Vec<Vec<Performer>> {
    let mut gauges = Vec::new();
    let mut other = Vec::new();
    let mut sum = 0i64;
    for actor in members {
        if lottery_points_only(master, actor) {
            continue;
        }
        if let Some(row) = lottery_gauge_row(master, actor) {
            sum += (row.value / 10000) as i64;
            gauges.push(actor.clone());
        } else {
            if actor.gekisou_skill.is_some_and(|(id, level)| {
                master.gekiso_skills[&id]
                    .effects
                    .iter()
                    .any(|row| row.level == level && row.kind == 11001)
            }) {
                return permutations(members);
            }
            other.push(actor.clone());
        }
    }
    if 2 * sum + 1 >= 1 << 24 {
        return permutations(members);
    }
    permutations(&other)
        .into_iter()
        .map(|order| {
            let mut deck = gauges.clone();
            deck.extend(order);
            deck
        })
        .collect()
}
fn permutations(values: &[Performer]) -> Vec<Vec<Performer>> {
    fn visit(
        values: &[Performer],
        used: &mut [bool],
        at: &mut Vec<Performer>,
        out: &mut Vec<Vec<Performer>>,
    ) {
        if at.len() == values.len() {
            out.push(at.clone());
            return;
        }
        for i in 0..values.len() {
            if used[i] || i > 0 && values[i] == values[i - 1] && !used[i - 1] {
                continue;
            }
            used[i] = true;
            at.push(values[i].clone());
            visit(values, used, at, out);
            at.pop();
            used[i] = false;
        }
    }
    let mut sorted = values.to_vec();
    sorted.sort();
    let mut out = Vec::new();
    visit(
        &sorted,
        &mut vec![false; sorted.len()],
        &mut Vec::new(),
        &mut out,
    );
    out
}
#[derive(Clone)]
struct FixedChoice {
    order: u32,
    converter: Option<Arc<crate::just_bound::Converter>>,
    lottery: bool,
    supported: bool,
}
pub struct Kernel<'a> {
    master: &'a Master,
    members: &'a [Member],
    snaps: &'a [Snap],
    chart: &'a mut Chart,
    seeds: Vec<i32>,
    criterion: Criterion,
    read_all: Vec<&'static str>,
    read_members: Vec<&'static str>,
    pub gk_members: Vec<Performer>,
    member_bound_ids: Vec<u32>,
    member_bound_decks: Vec<Option<Performer>>,
    just_counters: crate::support_aliases::JustCounters,
    support: Vec<Option<Performer>>,
    support_chain: Vec<bool>,
    performer_key_cache: HashMap<Performer, u32>,
    context_keys: HashMap<Performer, u32>,
    normalized_performers: HashMap<u32, (Performer, u32)>,
    slots: Vec<Slot>,
    shapes: HashMap<(Vec<u64>, Performer), u32>,
    shape_ids: Vec<u32>,
    bound_shape_ids: Vec<u32>,
    bound_actors: Vec<Option<Performer>>,
    bound_converters: Vec<bool>,
    raw_actor_keys: Vec<u32>,
    pub bonuses: Vec<i32>,
    opening_bonuses: Vec<i32>,
    pub maximum_level: i32,
    grid: Vec<i32>,
    template: Option<SkillEffect>,
    luck_deck: Vec<Performer>,
    luck_members: Vec<Performer>,
    member_gain: Vec<f64>,
    calibrated: bool,
    luck_supports: Vec<Performer>,
    cache: HashMap<(i32, Vec<u32>), Cached>,
    cache_order: VecDeque<(i32, Vec<u32>)>,
    cache_bytes: usize,
    cache_records: HashMap<usize, usize>,
    levels: HashMap<i32, Arc<Level>>,
    support_high: HashMap<(String, i32), f64>,
    support_low: HashMap<String, f64>,
    base: Option<Arc<Weights>>,
    member_bases: HashMap<(i32, [u32; 5], usize), f64>,
    luck_bases: HashMap<Vec<u32>, (SharedBase, bool)>,
    luck_base_order: VecDeque<Vec<u32>>,
    luck_aliases: HashMap<Vec<u32>, Vec<u32>>,
    luck_alias_order: VecDeque<Vec<u32>>,
    base_key: Vec<u32>,
    refine_bases: RefineBases,
    refined: HashMap<[usize; 5], f64>,
    refined_order: VecDeque<[usize; 5]>,
    rush: HashMap<Performer, Option<f64>>,
    luck_gain_slots: Vec<Option<u32>>,
    luck_gain_ids: HashMap<Performer, u32>,
    luck_gain_actors: Vec<Performer>,
    luck_gain_factors: Vec<Option<f64>>,
    numeric_gain_profiles:
        HashMap<Vec<Performer>, std::sync::Weak<std::sync::Mutex<HashMap<u32, f64>>>>,
    numeric_gain_order: VecDeque<Vec<Performer>>,
    exact_calls: usize,
    partial_orders: HashMap<OrderScoreKey, PartialOrderScores>,
    combo_bounds: HashMap<Vec<Performer>, ComboBound>,
    combo_order: VecDeque<Vec<Performer>>,
    fixed_luck_supports: Option<Option<Vec<Vec<Performer>>>>,
    fixed_luck_bounds: HashMap<FixedLuckKey, ComboBound>,
    fixed_luck_order: VecDeque<FixedLuckKey>,
    global_fixed_luck: Option<Option<Arc<crate::rewards::LuckEnvelope>>>,
    #[cfg(not(target_arch = "wasm32"))]
    shared_fixed_luck: Option<Arc<SharedLuckCache>>,
    fixed_forced_members: Option<Vec<usize>>,
    converted_master: Option<Master>,
    fixed_choices: Vec<Option<FixedChoice>>,
    fixed_combo_traces: HashMap<String, Arc<crate::reward_trace::ComboTrace>>,
    combo_scripts: Option<crate::reward_trace::Scripts>,
    last_combo_key: Option<(Vec<Performer>, String)>,
    fixed_preparations: HashMap<Option<usize>, Arc<crate::rewards::BoundPreparation>>,
    global_fixed_preparation: Option<Arc<crate::rewards::BoundPreparation>>,
    fixed_combo_trace_order: VecDeque<String>,
    fixed_luck_counts: HashMap<(String, usize), Option<Arc<crate::rewards::LuckEnvelope>>>,
    fixed_luck_count_order: VecDeque<(String, usize)>,
    fixed_region_domains: Option<Option<Vec<i32>>>,
    fixed_components: HashMap<FixedRegionKey, Arc<crate::rewards::FixedComponent>>,
    fixed_just_hull: Option<Option<Vec<Arc<crate::rewards::FixedComponent>>>>,
    fixed_component_order: VecDeque<FixedRegionKey>,
    fixed_component_bytes: usize,
    fixed_region_rows: HashMap<FixedRegionalKey, Arc<crate::rewards::FixedRegionRows>>,
    fixed_region_row_order: VecDeque<FixedRegionalKey>,
    fixed_region_allocated: Arc<AtomicUsize>,
    fixed_region_limit: usize,
    unsettled: HashMap<OrderScoreKey, Arc<live::Scores>>,
    context_fields: AttributeDependencies,
    just_context: Option<crate::rewards::JustContext>,
    support_aliases: crate::support_aliases::SupportAliases,
    favoured: HashMap<Vec<(i32, i32)>, (i32, bool)>,
    chain_actor_ids: Vec<Option<u32>>,
    chain_choices: HashMap<(i32, [u32; 5], u8), Arc<ChainChoice>>,
    chain_choice_order: VecDeque<(i32, [u32; 5], u8)>,
    healthy: bool,
    owner_local: bool,
    regional_gains: HashMap<(i32, Vec<u32>, u32), f64>,
    regional_gain_order: VecDeque<(i32, Vec<u32>, u32)>,
    gain_probe: Option<Performer>,
    luck_envelopes: HashMap<Vec<u32>, Arc<Weights>>,
    order_bounds: HashMap<[u32; 5], f64>,
    order_estimates: HashMap<([u32; 5], [u32; 5]), f64>,
    choice_domains: HashMap<Vec<i32>, u32>,
    next_choice_base: u64,
    choice_base_ids: HashMap<Vec<u32>, u64>,
    choice_base_order: VecDeque<(Vec<u32>, usize)>,
    choice_base_bytes: usize,
    choice_profiles: HashMap<ChoiceKey, (Arc<LuckChoices>, bool)>,
    choice_order: VecDeque<(ChoiceKey, usize)>,
    choice_bytes: usize,
    context_matching: crate::assignment::Scratch,
    context_matrix: Vec<Vec<f64>>,
    just_bounds: crate::just_bound::Compiler,
    pattern_domains: HashMap<Vec<usize>, usize>,
    next_pattern_domain: usize,
    patterns: PatternCache,
    pattern_order: VecDeque<(PatternKey, usize)>,
    pattern_bytes: usize,
    numeric_hull: Option<Arc<Vec<Arc<crate::just_bound::Upper>>>>,
    defer_scores: bool,
    seed_preview: bool,
    live_floor: bool,
    numeric_hull_ready: bool,
    numeric_root_work: usize,
}
impl Drop for Kernel<'_> {
    fn drop(&mut self) {
        // The chart's API key includes the complete box, player, constraints,
        // accuracy, rank and seed mode. Reuse bounded traces across shards.
        self.chart.contexts = std::mem::take(&mut self.cache);
        self.chart.context_order = std::mem::take(&mut self.cache_order);
        self.chart.context_bytes = self.cache_bytes;
        self.chart.context_records = std::mem::take(&mut self.cache_records);
    }
}
fn shape(p: &Performer) -> Performer {
    p.clone()
}
pub fn combo_bonuses(master: &Master, members: &[Member], missions: &[i32]) -> Vec<i32> {
    let combo = missions.iter().any(|m| *m == 1 || *m == 4);
    members
        .iter()
        .map(|m| {
            if !combo {
                return 0;
            }
            let p = Performer::from_member(master, m, None);
            let Some((id, level)) = p.gekisou_skill else {
                return 0;
            };
            master
                .gekiso_skills
                .get(&id)
                .filter(|s| s.mission_type == 1 || s.mission_type == 4)
                .into_iter()
                .flat_map(|s| &s.effects)
                .filter(|r| r.level == level && r.kind == 12000)
                .map(|r| r.value)
                .max()
                .unwrap_or(0)
                .max(0)
        })
        .collect()
}
pub(crate) fn opening_combo_bonuses(
    master: &Master,
    members: &[Member],
    missions: &[i32],
) -> Vec<i32> {
    let combo = missions.iter().any(|m| *m == 1 || *m == 4);
    members
        .iter()
        .map(|member| {
            if !combo {
                return 0;
            }
            let p = Performer::from_member(master, member, None);
            let Some((id, level)) = p.gekisou_skill else {
                return 0;
            };
            let Some(skill) = master
                .gekiso_skills
                .get(&id)
                .filter(|s| s.mission_type == 1 || s.mission_type == 4)
            else {
                return 0;
            };
            let group = |group: i32| {
                group == 0
                    || master.condition_sets.get(&group).is_some_and(|sets| {
                        sets.iter().any(|set| {
                            set.iter().all(|id| {
                                let c = &master.conditions[id];
                                match c.kind {
                                    0 => true,
                                    5000 => {
                                        c.targets.iter().any(|id| {
                                            crate::full::conditions::performer_matches(
                                                &p,
                                                &master.targets[id],
                                            )
                                        }) == c.positive
                                    }
                                    2000 => c.values.first().is_some_and(|&value| {
                                        (master.live.life_base > value) == c.positive
                                    }),
                                    2001 => c.values.first().is_some_and(|&value| {
                                        (master.live.life_base >= value) == c.positive
                                    }),
                                    7005 => {
                                        c.positive
                                            && c.values.first().is_some_and(|&value| value <= 1.0)
                                    }
                                    7020 => {
                                        let targets: Vec<_> = c
                                            .targets
                                            .iter()
                                            .map(|id| &master.targets[id])
                                            .filter(|t| {
                                                t.skill_target_type == 5 && t.mission_type != 0
                                            })
                                            .collect();
                                        c.positive
                                            && (targets.is_empty()
                                                || targets.iter().any(|target| {
                                                    [1, 4].contains(&target.mission_type)
                                                        && missions.contains(&target.mission_type)
                                                }))
                                    }
                                    _ => false,
                                }
                            })
                        })
                    })
            };
            skill
                .effects
                .iter()
                .filter(|row| {
                    row.level == level
                        && row.kind == 12000
                        && row.trigger_type == 2
                        && row.seconds == 0.0
                        && row.cumulative_id == 0
                        && row.reset_group == 0
                        && row.release_group == 0
                        && group(row.trigger_group)
                        && group(row.condition_group)
                })
                .map(|row| row.value)
                .sum::<i32>()
                .max(0)
        })
        .collect()
}

fn collect_luck_envelope(
    master: &Master,
    chart: &Chart,
    seeds: &[i32],
    orders: Vec<Vec<Performer>>,
    supports: &[Vec<Performer>],
) -> Option<Arc<crate::rewards::LuckEnvelope>> {
    let generation = crate::cancellation_generation();
    let aggregate = std::sync::Mutex::new((
        crate::rewards::LuckEnvelope {
            fingerprint: OnceLock::new(),
            moments: None,
            changes: 0,
            rush: vec![false; chart.notes.len()],
            support: vec![false; chart.notes.len()],
        },
        crate::rewards::LuckMoments {
            samples: seeds.len(),
            rush: vec![0; chart.notes.len()],
            support: vec![0; chart.notes.len()],
            joint: vec![0; chart.notes.len()],
        },
    ));
    let jobs = orders.len().checked_mul(supports.len())?;
    let workers = crate::parallel::preparation_workers().min(jobs.max(1));
    let tasks: Vec<_> = (0..workers).collect();
    crate::parallel::map(&tasks, |&worker| -> Result<(), ()> {
        for index in (worker..jobs).step_by(workers) {
            if crate::cancellation_generation() != generation {
                return Err(());
            }
            let mut deck = orders[index / supports.len()].clone();
            deck.extend_from_slice(&supports[index % supports.len()]);
            let traces = crate::full::model::lottery::rush_windows(master, chart, &deck, seeds)
                .ok()
                .flatten()
                .ok_or(())?;
            let mut changes = 0;
            let mut rush = vec![0i32; chart.times.len() + 1];
            let mut support = vec![0i32; chart.times.len() + 1];
            let mut joint = vec![0i32; chart.times.len() + 1];
            for trace in traces {
                changes = changes.max(trace.rush.len());
                let (mut rush_level, mut support_level, mut r, mut s) = (0, 0, 0, 0);
                while r < trace.rush.len() || s < trace.support.len() {
                    let time = trace
                        .rush
                        .get(r)
                        .map_or(i32::MAX, |v| v.0)
                        .min(trace.support.get(s).map_or(i32::MAX, |v| v.0));
                    let previous = (rush_level > 0, support_level > 0);
                    while r < trace.rush.len() && trace.rush[r].0 == time {
                        rush_level += trace.rush[r].1;
                        r += 1;
                    }
                    while s < trace.support.len() && trace.support[s].0 == time {
                        support_level += trace.support[s].1;
                        s += 1;
                    }
                    let current = (rush_level > 0, support_level > 0);
                    let at = chart.times.partition_point(|&note| note < time);
                    rush[at] += i32::from(current.0) - i32::from(previous.0);
                    support[at] += i32::from(current.1) - i32::from(previous.1);
                    joint[at] +=
                        i32::from(current.0 && current.1) - i32::from(previous.0 && previous.1);
                }
            }
            let mut result = aggregate.lock().unwrap();
            let (value, moments) = &mut *result;
            value.changes = value.changes.max(changes);
            let (mut r, mut s, mut j) = (0, 0, 0);
            for at in 0..chart.times.len() {
                r += rush[at];
                s += support[at];
                j += joint[at];
                value.rush[at] |= r > 0;
                value.support[at] |= s > 0;
                moments.rush[at] = moments.rush[at].max(r as usize);
                moments.support[at] = moments.support[at].max(s as usize);
                moments.joint[at] = moments.joint[at].max(j as usize);
            }
        }
        Ok(())
    })
    .ok()?;
    let (mut value, moments) = aggregate.into_inner().unwrap();
    value.moments = Some(Arc::new(moments));
    Some(Arc::new(value))
}

type RewardEnvelopeKey = (String, Vec<String>, i32);

pub(crate) struct EmptyLuckPrograms {
    pub(crate) rng_snaps: Vec<bool>,
    members: Vec<Option<Vec<Performer>>>,
    supports: Vec<Vec<Option<(String, Performer)>>>,
    envelopes: HashMap<RewardEnvelopeKey, (Arc<crate::rewards::LuckEnvelope>, usize)>,
    envelope_order: VecDeque<RewardEnvelopeKey>,
    envelope_bytes: usize,
}
impl EmptyLuckPrograms {
    pub(crate) fn new(
        master: &Master,
        members: &[Member],
        snaps: &[Snap],
        chart: &Chart,
    ) -> Option<Self> {
        if !chart.luck || !chart.setup.missions.iter().all(|&mission| mission == 2) {
            return None;
        }
        let mut fresh = chart.converted_judgements(&chart.judgements)?;
        let mut kernel = Kernel::new(master, members, snaps, &mut fresh, vec![0], Criterion::Mean);
        kernel.fixed_luck_supports()?;
        let programs = (0..members.len())
            .map(|member| kernel.fixed_luck_members(&[member]))
            .collect();
        let rng_snaps: Vec<bool> = snaps
            .iter()
            .map(|snap| {
                snap.card
                    .gekiso_support_skills
                    .iter()
                    .enumerate()
                    .any(|(slot, id)| {
                        master.gekiso_support_skills.get(id).is_some_and(|skill| {
                            skill
                                .effects
                                .iter()
                                .filter(|row| row.level == snap.gekiso_skill_levels[slot])
                                .any(|row| {
                                    (11000..=11005).contains(&row.kind)
                                        || condition_rows(master, row)
                                            .any(|condition| condition.kind == 4011)
                                })
                        })
                    })
            })
            .collect();
        let supports = members
            .iter()
            .map(|member| {
                snaps
                    .iter()
                    .enumerate()
                    .map(|(snap_index, snap)| {
                        if !rng_snaps[snap_index] {
                            return None;
                        }
                        let mut actor =
                            Performer::from_member(master, member, Some(snap)).gekiso_only();
                        actor.gekisou_skill = None;
                        actor.mission_only = Some(2);
                        actor.gekisou_support_skills.retain(|&(id, level)| {
                            master.gekiso_support_skills.get(&id).is_some_and(|skill| {
                                skill.mission_type == 2
                                    && skill.effects.iter().filter(|row| row.level == level).any(
                                        |row| {
                                            (11000..=11005).contains(&row.kind)
                                                || condition_rows(master, row)
                                                    .any(|condition| condition.kind == 4011)
                                        },
                                    )
                            })
                        });
                        let key = lottery_support_program(master, &actor)
                            .map(|(kind, key)| format!("{kind}:{key}"))
                            .unwrap_or_else(|| serde_json::to_string(&actor).unwrap());
                        Some((key, actor))
                    })
                    .collect()
            })
            .collect();
        Some(Self {
            rng_snaps,
            members: programs,
            supports,
            envelopes: HashMap::default(),
            envelope_order: VecDeque::new(),
            envelope_bytes: 0,
        })
    }
    pub(crate) fn support_key(&self, member: usize, snap: i32) -> Option<&str> {
        self.supports[member][snap as usize]
            .as_ref()
            .map(|(key, _)| key.as_str())
    }
    pub(crate) fn envelope(
        &mut self,
        master: &Master,
        chart: &Chart,
        selected: &[usize],
        seed: i32,
        support: &[(usize, i32)],
    ) -> Option<Arc<crate::rewards::LuckEnvelope>> {
        let mut actors = Vec::new();
        for &member in selected {
            actors.extend_from_slice(self.members[member].as_ref()?);
        }
        actors.sort();
        let supports = support
            .iter()
            .map(|&(position, snap)| {
                self.supports[selected[position]][snap as usize]
                    .as_ref()
                    .map(|(_, actor)| actor.clone())
            })
            .collect::<Option<Vec<_>>>()?;
        let support_keys = support
            .iter()
            .map(|&(position, snap)| {
                self.supports[selected[position]][snap as usize]
                    .as_ref()
                    .map(|(key, _)| key.clone())
            })
            .collect::<Option<Vec<_>>>()?;
        let key = (lottery_member_key(master, &actors), support_keys, seed);
        if let Some((value, _)) = self.envelopes.get(&key) {
            let value = Arc::clone(value);
            self.envelope_order.retain(|old| old != &key);
            self.envelope_order.push_back(key);
            return Some(value);
        }
        let value = collect_luck_envelope(
            master,
            chart,
            &[seed],
            lottery_member_orders(master, &actors),
            &[supports],
        )?;
        let key_bytes = key.0.capacity()
            + key.1.capacity() * std::mem::size_of::<String>()
            + key.1.iter().map(String::capacity).sum::<usize>();
        let bytes = 512
            + 2 * key_bytes
            + value.rush.capacity()
            + value.support.capacity()
            + value.moments.as_ref().map_or(0, |moments| {
                (moments.rush.capacity() + moments.support.capacity() + moments.joint.capacity())
                    * std::mem::size_of::<usize>()
            });
        const CAPACITY: usize = 2 * 1024 * 1024;
        if bytes <= CAPACITY {
            while self.envelope_bytes + bytes > CAPACITY {
                let Some(old) = self.envelope_order.pop_front() else {
                    break;
                };
                if let Some((_, size)) = self.envelopes.remove(&old) {
                    self.envelope_bytes -= size;
                }
            }
            self.envelope_bytes += bytes;
            self.envelopes
                .insert(key.clone(), (Arc::clone(&value), bytes));
            self.envelope_order.push_back(key);
        }
        Some(value)
    }
}

impl<'a> Kernel<'a> {
    pub fn new(
        master: &'a Master,
        members: &'a [Member],
        snaps: &'a [Snap],
        chart: &'a mut Chart,
        seeds: Vec<i32>,
        criterion: Criterion,
    ) -> Self {
        let read_all = read_attributes(master, true);
        let read_members = read_attributes(master, false);
        let missions: HashSet<_> = chart
            .setup
            .missions
            .iter()
            .take(chart.full.fevers.len())
            .copied()
            .collect();
        let combo_ranges = missions.contains(&1) || missions.contains(&4);
        let has_gk: Vec<_> = members
            .iter()
            .map(|m| {
                m.card.gekiso_skill_id != 0
                    && master.gekiso_skills.contains_key(&m.card.gekiso_skill_id)
            })
            .collect();
        let gk_members: Vec<_> = members
            .iter()
            .map(|m| {
                let p = Performer::from_member(master, m, None).gekiso_only();
                let Some((id, _)) = p.gekisou_skill else {
                    return p;
                };
                let mission = master
                    .gekiso_skills
                    .get(&id)
                    .map(|s| s.mission_type)
                    .unwrap_or(0);
                if mission == 4 || missions.contains(&mission) {
                    p
                } else {
                    Performer {
                        mission_only: Some(if mission != 0 { mission } else { 1 }),
                        ..Performer::default()
                    }
                }
            })
            .collect();
        let band_only = read_all == ["bandId"];
        let snap_supports: Vec<_> = snaps
            .iter()
            .map(|snap| {
                let skills: Vec<_> = snap
                    .card
                    .gekiso_support_skills
                    .into_iter()
                    .zip(snap.gekiso_skill_levels)
                    .filter(|(id, _)| *id != 0)
                    .collect();
                let runs = skills.iter().any(|&(id, _)| {
                    let mission = master
                        .gekiso_support_skills
                        .get(&id)
                        .map(|s| s.mission_type)
                        .unwrap_or(0);
                    mission == 4 || missions.contains(&mission) || missions.contains(&4)
                });
                if skills.is_empty() || !runs {
                    return None;
                }
                let favoured = if band_only {
                    favoured_band(master, &skills)
                } else {
                    0
                };
                Some((skills, favoured))
            })
            .collect();
        let mut support = Vec::with_capacity(members.len() * snaps.len());
        let mut supports: HashMap<_, Performer> = HashMap::default();
        for (i, member) in members.iter().enumerate() {
            for snap in &snap_supports {
                let Some((skills, favoured)) = snap.as_ref().filter(|_| has_gk[i]) else {
                    support.push(None);
                    continue;
                };
                let band = if band_only && *favoured != 0 {
                    if member.card.band_id == *favoured {
                        *favoured
                    } else if *favoured == 1 {
                        2
                    } else {
                        1
                    }
                } else {
                    member.card.band_id
                };
                let fields = if band_only {
                    [band, 0, 0]
                } else {
                    [band, member.card.card_type, member.card.character_id]
                };
                let value = supports.entry((skills.as_slice(), fields)).or_insert_with(|| {
                    if band_only {
                        Performer {
                            band_id: band,
                            mission_only: Some(1),
                            gekisou_support_skills: skills.clone(),
                            ..Performer::default()
                        }
                    } else {
                        let mut p = Performer::from_member(master, member, None);
                        p.live_skill = None;
                        p.gekisou_skill = None;
                        p.gekisou_support_skills = skills.clone();
                        p.mission_only = Some(1);
                        p
                    }
                });
                support.push(Some(value.clone()));
            }
        }
        // Release constructor-only groups before preparing the slot table and contexts.
        drop(supports);
        drop(snap_supports);
        let bonuses = combo_bonuses(master, members, &chart.setup.missions);
        let opening_bonuses = opening_combo_bonuses(master, members, &chart.setup.missions);
        let template = if combo_ranges {
            combo_template(master)
        } else {
            None
        };
        let mut best: HashMap<i32, i32> = HashMap::default();
        for (i, m) in members.iter().enumerate() {
            best.entry(m.card.character_id)
                .and_modify(|v| *v = (*v).max(bonuses[i]))
                .or_insert(bonuses[i]);
        }
        let mut values: Vec<_> = best.into_values().collect();
        values.sort_by(|a, b| b.cmp(a));
        let maximum_level = if template.is_some() {
            values.iter().take(5).sum()
        } else {
            0
        };
        let grid = if maximum_level > 0 {
            [
                0,
                ((maximum_level as f64) / 4.0).ceil() as i32,
                ((maximum_level as f64) / 2.0).ceil() as i32,
                ((3 * maximum_level) as f64 / 4.0).ceil() as i32,
                maximum_level,
            ]
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
        } else {
            vec![0]
        };
        let mut slots = crate::skills::prepare(master, members, snaps).into_slots();
        let healthy = chart.accuracy.great == 0.0
            && chart.accuracy.miss_every == 0
            && crate::support_aliases::stable_life_conditions(
                master,
                members,
                snaps,
                &chart.setup.missions,
            )
            && master
                .gekiso_skills
                .values()
                .chain(master.gekiso_support_skills.values())
                .all(|s| {
                    s.effects.iter().all(|r| {
                        r.kind != 3000 && r.kind != 3002 && !(r.kind == 12006 && r.value < 5)
                    })
                })
            && slots.iter().all(|s| {
                s.effects.iter().all(|e| {
                    e.gate.blocks(master.live.life_base)
                        == e.gate.blocks(2.0 * master.live.life_base)
                }) && s
                    .convert
                    .as_ref()
                    .is_none_or(|c| !c.judgements.contains(&6))
            });
        if healthy {
            for slot in &mut slots {
                slot.recovery = 0;
                slot.convert = None;
                slot.effects
                    .retain(|e| !e.gate.blocks(master.live.life_base));
                for effect in &mut slot.effects {
                    effect.gate = crate::live::Gate {
                        kind: 0,
                        value: 0.0,
                        positive: true,
                    };
                }
                if slot.effects.is_empty() {
                    slot.extension_ms = 0.0;
                }
            }
        }
        let cache = std::mem::take(&mut chart.contexts);
        let cache_order = std::mem::take(&mut chart.context_order);
        let cache_bytes = std::mem::replace(&mut chart.context_bytes, 0);
        let cache_records = std::mem::take(&mut chart.context_records);
        let support_chain = support
            .iter()
            .map(|p| p.as_ref().is_some_and(|p| changes_context(master, p)))
            .collect();
        let just_context = crate::rewards::JustContext::new(master, chart, members, snaps);
        let support_aliases =
            crate::support_aliases::SupportAliases::new(&chart.setup.missions, snaps, healthy);
        let mut member_fields = crate::support_aliases::ContextFields::new(master);
        let just_counters = crate::support_aliases::JustCounters::new(master, members);
        let member_bound_decks: Vec<_> = gk_members
            .iter()
            .cloned()
            .map(|mut performer| {
                just_counters.normalize(&mut performer);
                if maximum_level > 0 {
                    if let Some((id, level)) = performer.gekisou_skill {
                        let skill = &master.gekiso_skills[&id];
                        if skill.mission_type == 1 {
                            if skill
                                .effects
                                .iter()
                                .filter(|row| row.level == level)
                                .any(|row| row.kind != 12000 || row.value < 0)
                            {
                                return None;
                            }
                            performer.gekisou_skill = None;
                            performer.mission_only = Some(1);
                        }
                    }
                }
                Some(performer)
            })
            .collect();
        let mut member_shapes = HashMap::default();
        let member_bound_ids = gk_members
            .iter()
            .cloned()
            .zip(&member_bound_decks)
            .map(|(mut performer, deck)| {
                if deck.is_none() {
                    return u32::MAX;
                }
                just_counters.normalize(&mut performer);
                if member_fields.owner_local() && performer.gekisou_skill.is_none() {
                    return 0;
                }
                member_fields.normalize(master, &mut performer);
                let mut key = shape(&performer);
                if maximum_level > 0
                    && key
                        .gekisou_skill
                        .is_some_and(|(id, _)| master.gekiso_skills[&id].mission_type == 1)
                {
                    key.gekisou_skill = None;
                    key.mission_only = Some(1);
                }
                let next = member_shapes.len() as u32 + 1;
                *member_shapes.entry(key).or_insert(next)
            })
            .collect();
        let gain_probe = support
            .iter()
            .flatten()
            .find(|p| {
                p.gekisou_support_skills.iter().all(|&(id, level)| {
                    let skill = &master.gekiso_support_skills[&id];
                    [1, 3].contains(&skill.mission_type)
                        && skill
                            .effects
                            .iter()
                            .filter(|r| r.level == level)
                            .all(|r| r.kind == 2001)
                })
            })
            .cloned();
        Self {
            master,
            members,
            snaps,
            chart,
            seeds,
            criterion,
            read_all,
            read_members,
            gk_members,
            member_bound_ids,
            member_bound_decks,
            just_counters,
            support,
            support_chain,
            performer_key_cache: HashMap::default(),
            context_keys: HashMap::default(),
            normalized_performers: HashMap::default(),
            slots,
            shapes: HashMap::default(),
            shape_ids: vec![u32::MAX; members.len() * (snaps.len() + 1)],
            bound_shape_ids: vec![u32::MAX; members.len() * (snaps.len() + 1)],
            bound_actors: vec![None; members.len() * (snaps.len() + 1)],
            bound_converters: vec![false; members.len() * (snaps.len() + 1)],
            raw_actor_keys: vec![u32::MAX; members.len() * (snaps.len() + 1)],
            bonuses,
            opening_bonuses,
            maximum_level,
            grid,
            template,
            luck_deck: Vec::new(),
            luck_members: Vec::new(),
            member_gain: vec![0.0; members.len()],
            calibrated: false,
            luck_supports: Vec::new(),
            cache,
            cache_order,
            cache_bytes,
            cache_records,
            levels: HashMap::default(),
            support_high: HashMap::default(),
            support_low: HashMap::default(),
            base: None,
            member_bases: HashMap::default(),
            luck_bases: HashMap::default(),
            luck_base_order: VecDeque::new(),
            luck_aliases: HashMap::default(),
            luck_alias_order: VecDeque::new(),
            base_key: Vec::new(),
            refine_bases: HashMap::default(),
            refined: HashMap::default(),
            refined_order: VecDeque::new(),
            rush: HashMap::default(),
            luck_gain_slots: vec![None; members.len() * (snaps.len() + 1)],
            luck_gain_ids: HashMap::default(),
            luck_gain_actors: Vec::new(),
            luck_gain_factors: Vec::new(),
            numeric_gain_profiles: HashMap::default(),
            numeric_gain_order: VecDeque::new(),
            exact_calls: 0,
            partial_orders: HashMap::default(),
            combo_bounds: HashMap::default(),
            combo_order: VecDeque::new(),
            fixed_luck_supports: None,
            fixed_luck_bounds: HashMap::default(),
            fixed_luck_order: VecDeque::new(),
            global_fixed_luck: None,
            #[cfg(not(target_arch = "wasm32"))]
            shared_fixed_luck: None,
            fixed_forced_members: None,
            converted_master: None,
            fixed_choices: vec![None; members.len() * (snaps.len() + 1)],
            fixed_combo_traces: HashMap::default(),
            combo_scripts: None,
            last_combo_key: None,
            fixed_preparations: HashMap::default(),
            global_fixed_preparation: None,
            fixed_combo_trace_order: VecDeque::new(),
            fixed_luck_counts: HashMap::default(),
            fixed_luck_count_order: VecDeque::new(),
            fixed_region_domains: None,
            fixed_components: HashMap::default(),
            fixed_just_hull: None,
            fixed_component_order: VecDeque::new(),
            fixed_component_bytes: 0,
            fixed_region_rows: HashMap::default(),
            fixed_region_row_order: VecDeque::new(),
            fixed_region_allocated: Arc::new(AtomicUsize::new(0)),
            fixed_region_limit: 8 * 1024 * 1024,
            unsettled: HashMap::default(),
            context_fields: HashMap::default(),
            just_context,
            support_aliases,
            favoured: HashMap::default(),
            chain_actor_ids: vec![None; members.len() * snaps.len()],
            chain_choices: HashMap::default(),
            chain_choice_order: VecDeque::new(),
            healthy,
            owner_local: member_fields.owner_local(),
            regional_gains: HashMap::default(),
            regional_gain_order: VecDeque::new(),
            gain_probe,
            luck_envelopes: HashMap::default(),
            order_bounds: HashMap::default(),
            order_estimates: HashMap::default(),
            choice_domains: HashMap::default(),
            next_choice_base: 0,
            choice_base_ids: HashMap::default(),
            choice_base_order: VecDeque::new(),
            choice_base_bytes: 0,
            choice_profiles: HashMap::default(),
            choice_order: VecDeque::new(),
            choice_bytes: 0,
            context_matching: crate::assignment::Scratch::default(),
            context_matrix: Vec::new(),
            just_bounds: crate::just_bound::Compiler::default(),
            pattern_domains: HashMap::default(),
            next_pattern_domain: 0,
            patterns: HashMap::default(),
            pattern_order: VecDeque::new(),
            pattern_bytes: 0,
            numeric_hull: None,
            defer_scores: false,
            seed_preview: false,
            live_floor: false,
            numeric_hull_ready: false,
            numeric_root_work: 0,
        }
    }
    fn fixed_luck_supports(&mut self) -> Option<&[Vec<Performer>]> {
        if self.fixed_luck_supports.is_none() {
            self.fixed_luck_supports = Some(self.build_fixed_luck_supports());
        }
        self.fixed_luck_supports.as_ref()?.as_deref()
    }
    fn build_fixed_luck_supports(&self) -> Option<Vec<Vec<Performer>>> {
        if !self.healthy || !self.owner_local || self.seeds.is_empty() {
            return None;
        }
        if self
            .chart
            .setup
            .fevers
            .windows(2)
            .any(|pair| pair[1].0 <= pair[0].1.saturating_add(1))
        {
            return None;
        }
        let mut after_luck = false;
        for &mission in &self.chart.setup.missions {
            if mission == 2 && after_luck {
                return None;
            }
            after_luck |= mission != 2;
        }
        let has_probability =
            |row: &SkillEffect| condition_rows(self.master, row).any(|c| c.kind == 4011);
        if self.members.iter().any(|member| {
            self.master
                .live_skills
                .get(&member.card.live_skill_id)
                .into_iter()
                .flatten()
                .any(has_probability)
        }) || self.snaps.iter().any(|snap| {
            snap.card.support_skills.iter().any(|id| {
                self.master
                    .support_skills
                    .get(id)
                    .into_iter()
                    .flatten()
                    .any(has_probability)
            })
        }) {
            return None;
        }
        let mut groups = Vec::new();
        for snap in self.snaps {
            let mut variants = HashMap::default();
            for member in self.members {
                let mut actor =
                    Performer::from_member(self.master, member, Some(snap)).gekiso_only();
                if actor.gekisou_skill.is_none() {
                    continue;
                }
                let mut signature = Vec::new();
                let mut skills = Vec::new();
                for &(id, level) in &actor.gekisou_support_skills {
                    let skill = self.master.gekiso_support_skills.get(&id)?;
                    let rows: Vec<_> = skill.effects.iter().filter(|r| r.level == level).collect();
                    if skill.mission_type != 2 {
                        if rows.iter().any(|row| has_probability(row)) {
                            return None;
                        }
                        continue;
                    }
                    if !rows
                        .iter()
                        .any(|r| (11000..=11005).contains(&r.kind) || has_probability(r))
                    {
                        continue;
                    }
                    skills.push((id, level));
                    for row in rows {
                        for condition in condition_rows(self.master, row).filter(|c| c.kind == 5000)
                        {
                            signature.push(
                                condition.targets.iter().any(|id| {
                                    self.master.targets.get(id).is_some_and(|target| {
                                        crate::full::conditions::performer_matches(&actor, target)
                                    })
                                }) == condition.positive,
                            );
                        }
                    }
                }
                if skills.is_empty() {
                    continue;
                }
                actor.gekisou_skill = None;
                actor.gekisou_support_skills = skills.clone();
                actor.mission_only = Some(2);
                variants.entry((skills, signature)).or_insert(actor);
            }
            if !variants.is_empty() {
                groups.push(variants.into_values().collect::<Vec<_>>());
            }
        }
        fn visit(
            groups: &[Vec<Performer>],
            used: &mut [bool],
            deck: &mut Vec<Performer>,
            out: &mut Vec<Vec<Performer>>,
        ) {
            out.push(deck.clone());
            if deck.len() == 5 {
                return;
            }
            for i in 0..groups.len() {
                if used[i] {
                    continue;
                }
                used[i] = true;
                for actor in &groups[i] {
                    deck.push(actor.clone());
                    visit(groups, used, deck, out);
                    deck.pop();
                }
                used[i] = false;
            }
        }
        if let Some(families) = compact_lottery_supports(self.master, &groups) {
            return Some(families);
        }
        let mut families = Vec::new();
        visit(
            &groups,
            &mut vec![false; groups.len()],
            &mut Vec::new(),
            &mut families,
        );
        Some(families)
    }
    fn fixed_luck_members(&self, members: &[usize]) -> Option<Vec<Performer>> {
        let mut out = Vec::new();
        for &index in members {
            let member = &self.members[index];
            let actor = Performer::from_member(self.master, member, None).gekiso_only();
            let Some((id, level)) = actor.gekisou_skill else {
                continue;
            };
            let skill = self.master.gekiso_skills.get(&id)?;
            if skill.mission_type == 4 {
                return None;
            }
            if skill.mission_type != 2 {
                continue;
            }
            if skill
                .effects
                .iter()
                .filter(|r| r.level == level)
                .any(|row| condition_rows(self.master, row).any(|c| c.kind == 5000))
            {
                return None;
            }
            out.push(Performer {
                gekisou_skill: Some((id, level)),
                ..Performer::default()
            });
        }
        out.sort();
        Some(out)
    }
    fn fixed_luck_changes(
        &mut self,
        members: &[Performer],
        with_supports: bool,
    ) -> Option<Arc<crate::rewards::LuckEnvelope>> {
        self.fixed_luck_changes_limit(members, if with_supports { 5 } else { 0 })
    }
    fn fixed_luck_changes_limit(
        &mut self,
        members: &[Performer],
        support_limit: usize,
    ) -> Option<Arc<crate::rewards::LuckEnvelope>> {
        let key = (lottery_member_key(self.master, members), support_limit);
        if let Some(value) = self.fixed_luck_counts.get(&key) {
            self.fixed_luck_count_order.retain(|old| old != &key);
            self.fixed_luck_count_order.push_back(key);
            return value.clone();
        }
        #[cfg(not(target_arch = "wasm32"))]
        let shared = self.shared_fixed_luck.clone();
        let mut compute = || {
            let previous = (0..support_limit).rev().find_map(|count| {
                self.fixed_luck_counts
                    .get(&(key.0.clone(), count))
                    .and_then(|value| value.as_ref())
                    .map(|value| (count, Arc::clone(value)))
            });
            let supports = self.fixed_luck_supports()?;
            let supports = supports
                .iter()
                .filter(|family| {
                    family.len() <= support_limit
                        && previous
                            .as_ref()
                            .is_none_or(|(count, _)| family.len() > *count)
                })
                .cloned()
                .collect::<Vec<_>>();
            let value = self
                .collect_fixed_luck(lottery_member_orders(self.master, members), &supports)
                .map(|current| {
                    let Some((_, previous)) = previous else {
                        return current;
                    };
                    let (a, b) = (
                        previous.moments.as_ref().unwrap(),
                        current.moments.as_ref().unwrap(),
                    );
                    let maxima = |a: &[usize], b: &[usize]| {
                        a.iter().zip(b).map(|(&a, &b)| a.max(b)).collect()
                    };
                    Arc::new(crate::rewards::LuckEnvelope {
                        fingerprint: OnceLock::new(),
                        changes: previous.changes.max(current.changes),
                        rush: previous
                            .rush
                            .iter()
                            .zip(&current.rush)
                            .map(|(&a, &b)| a || b)
                            .collect(),
                        support: previous
                            .support
                            .iter()
                            .zip(&current.support)
                            .map(|(&a, &b)| a || b)
                            .collect(),
                        moments: Some(Arc::new(crate::rewards::LuckMoments {
                            samples: a.samples,
                            rush: maxima(&a.rush, &b.rush),
                            support: maxima(&a.support, &b.support),
                            joint: maxima(&a.joint, &b.joint),
                        })),
                    })
                });
            value
        };
        #[cfg(not(target_arch = "wasm32"))]
        let value = match shared {
            Some(shared) => shared.cell(&key).get_or_init(compute).clone(),
            None => compute(),
        };
        #[cfg(target_arch = "wasm32")]
        let value = compute();

        let limit = if cfg!(target_arch = "wasm32") {
            luck_envelope_limit(self.chart.notes.len()).min(128)
        } else {
            32
        };
        if self.fixed_luck_counts.len() >= limit {
            if let Some(old) = self.fixed_luck_count_order.pop_front() {
                self.fixed_luck_counts.remove(&old);
            }
        }
        self.fixed_luck_counts.insert(key.clone(), value.clone());
        self.fixed_luck_count_order.push_back(key);
        value
    }
    fn collect_fixed_luck(
        &self,
        orders: Vec<Vec<Performer>>,
        supports: &[Vec<Performer>],
    ) -> Option<Arc<crate::rewards::LuckEnvelope>> {
        collect_luck_envelope(self.master, self.chart, &self.seeds, orders, supports)
    }
    fn analytic_fixed_luck(&mut self) -> Option<Arc<crate::rewards::LuckEnvelope>> {
        let families = self.fixed_luck_supports()?.to_vec();
        let ranges = crate::rewards::deterministic_ranges(self.master, self.chart)?.to_vec();
        if ranges.windows(2).any(|pair| pair[0].1 >= pair[1].0) {
            return None;
        }

        let tables = &self.master.live.gekiso;
        let minimum = tables.gauge_max.min(tables.gauge_max_rush);
        let maximum = tables.gauge_max.max(tables.gauge_max_rush);
        if minimum < 1 {
            return None;
        }
        let single = |group: i32, kind: i32| {
            self.master.condition_sets.get(&group).is_some_and(|sets| {
                sets.len() == 1
                    && sets[0].len() == 1
                    && self
                        .master
                        .conditions
                        .get(&sets[0][0])
                        .is_some_and(|c| c.kind == kind && c.positive)
            })
        };
        let burst = |row: &SkillEffect, executions: u64| -> Option<u64> {
            if row.kind != 11003
                || row.value < 0
                || row.cumulative_id != 0
                || row.seconds != 0.0
                || row.limit_count != 0
                || row.max_value != 0
            {
                return None;
            }
            let mut high = 0;
            for gauge in [tables.gauge_max, tables.gauge_max_rush] {
                let product = gauge.checked_mul(row.value)?;
                let value = crate::full::core::floor_i32(crate::num::f(
                    crate::num::f(product as f64) / 10000.0,
                ));
                if value < 0 {
                    return None;
                }
                high = high.max(value as u64);
            }
            high.checked_mul(executions)
        };
        if tables.base.iter().any(|row| row[2] > 0 && row[3] < 0) {
            return None;
        }
        let point = tables
            .base
            .iter()
            .filter(|row| row[2] > 0)
            .map(|row| row[3] as u64)
            .max()
            .unwrap_or(0);
        let mut params = self.chart.params();
        params.music_length = 0;
        let model = crate::full::model::Model::new(
            self.master,
            &[],
            &self.chart.full.notes,
            &self.chart.full.events,
            params,
            Some(&self.chart.setup),
            (0, false),
        )
        .ok()?;
        let controller = &model.gk.as_ref()?.controller;
        let luck_ranges = controller
            .ranges
            .iter()
            .filter(|range| range.mission == 2)
            .count() as u64;
        let events = self
            .chart
            .play
            .frames
            .iter()
            .flat_map(|frame| &frame.judged)
            .map(|note| {
                controller
                    .ranges
                    .iter()
                    .filter(|range| range.mission == 2 && range.targets.contains(&note.note_id))
                    .count() as u64
            })
            .sum::<u64>();
        let base = events.checked_mul(point)?;
        let mut rates = Vec::new();
        let mut member_gauge = Vec::new();
        for member in self.members {
            let mut rate = 0u64;
            let mut gauge = 0u64;
            if let Some(skill) = self.master.gekiso_skills.get(&member.card.gekiso_skill_id) {
                if skill.mission_type == 4 {
                    return None;
                }
                for row in skill
                    .effects
                    .iter()
                    .filter(|row| row.level == member.growth.gekiso_skill_level)
                {
                    if skill.mission_type != 2 {
                        if (11000..=11005).contains(&row.kind) {
                            return None;
                        }
                        continue;
                    }
                    match row.kind {
                        11001 => {
                            let actor =
                                Performer::from_member(self.master, member, None).gekiso_only();
                            let checked = lottery_gauge_row(self.master, &actor)?;
                            if checked.id != row.id {
                                return None;
                            }
                            rate = rate.checked_add((row.value / 10000) as u64)?;
                        }
                        11003 => {
                            if row.trigger_type != 1
                                || !single(row.trigger_group, 7010)
                                || row.reset_group != 0
                            {
                                return None;
                            }
                            gauge = gauge.checked_add(burst(row, luck_ranges)?)?;
                        }
                        11000 | 11002 | 11004 | 11005 | 3004 | 4004 => {}
                        _ => return None,
                    }
                }
            }
            rates.push(rate);
            member_gauge.push(base.checked_mul(rate)?.checked_add(gauge)?);
        }
        rates.sort_unstable_by(|a, b| b.cmp(a));
        let speed = rates
            .into_iter()
            .take(5)
            .try_fold(0u64, |sum, value| sum.checked_add(value))?;
        if speed.checked_add(1)?.checked_mul(point)? >= 1 << 24
            || speed.checked_mul(2 * (self.chart.setup.missions.len() as u64 + 1))? >= 1 << 24
        {
            return None;
        }
        member_gauge.sort_unstable_by(|a, b| b.cmp(a));
        let member_gauge = member_gauge
            .into_iter()
            .take(5)
            .try_fold(0u64, |sum, value| sum.checked_add(value))?;
        let mut support_gauge = 0u64;
        for family in families {
            let mut gauge = 0u64;
            for actor in family {
                for (id, level) in actor.gekisou_support_skills {
                    let skill = self.master.gekiso_support_skills.get(&id)?;
                    for row in skill.effects.iter().filter(|row| row.level == level) {
                        match row.kind {
                            11003 => {
                                if row.trigger_type != 1
                                    || row.execute_limit != 1
                                    || !(row.reset_group == 0 || single(row.reset_group, 7013))
                                {
                                    return None;
                                }
                                let executions = if row.reset_group == 0 {
                                    1
                                } else {
                                    1 + self.chart.setup.missions.len() as u64
                                };
                                gauge = gauge.checked_add(burst(row, executions)?)?;
                            }
                            11001 => return None,
                            11000 | 11002 | 11004 | 11005 | 3004 | 4004 | 2000 => {}
                            _ => return None,
                        }
                    }
                }
            }
            support_gauge = support_gauge.max(gauge);
        }
        let input = base.checked_add(member_gauge)?.checked_add(support_gauge)?;
        if input >= i32::MAX as u64 || maximum < 1 {
            return None;
        }
        let changes = usize::try_from((input / minimum as u64).checked_mul(2)?).ok()?;
        if changes > i32::MAX as usize {
            return None;
        }

        let mask = self
            .chart
            .times
            .iter()
            .map(|&time| {
                ranges.iter().enumerate().any(|(index, &(start, end))| {
                    self.chart.setup.missions[index] == 2 && start <= time && time <= end
                })
            })
            .collect::<Vec<_>>();
        let value = Arc::new(crate::rewards::LuckEnvelope {
            fingerprint: OnceLock::new(),
            moments: None,
            changes,
            rush: mask.clone(),
            support: mask,
        });
        let admitted = crate::rewards::DeterministicBound::with_luck_cap(
            self.master,
            self.chart,
            self.members,
            self.snaps,
            1.0 + self.maximum_level as f64,
            None,
            Some(&value),
        )
        .is_some();
        admitted.then_some(value)
    }
    fn global_fixed_luck(&mut self) -> Option<Arc<crate::rewards::LuckEnvelope>> {
        if let Some(value) = &self.global_fixed_luck {
            return value.clone();
        }
        let value = if let Some(members) = self.fixed_forced_members.clone() {
            self.fixed_luck_members(&members)
                .and_then(|programs| self.fixed_luck_changes(&programs, true))
        } else {
            self.analytic_fixed_luck()
                .or_else(|| self.build_global_fixed_luck())
        };
        self.global_fixed_luck = Some(value.clone());
        value
    }
    fn build_global_fixed_luck(&mut self) -> Option<Arc<crate::rewards::LuckEnvelope>> {
        let supports = self.fixed_luck_supports()?.to_vec();
        let mut gauges = HashMap::default();
        let mut bursts = Vec::new();
        let mut gauge_shape = None;
        for member in self.members {
            let (id, level) = (
                member.card.gekiso_skill_id,
                member.growth.gekiso_skill_level,
            );
            let Some(skill) = self.master.gekiso_skills.get(&id) else {
                continue;
            };
            if skill.mission_type == 4 {
                return None;
            }
            if skill.mission_type != 2 {
                continue;
            }
            let rows: Vec<_> = skill.effects.iter().filter(|r| r.level == level).collect();
            if rows
                .iter()
                .any(|r| condition_rows(self.master, r).any(|c| c.kind == 5000))
            {
                return None;
            }
            let actor = Performer {
                gekisou_skill: Some((id, level)),
                ..Performer::default()
            };
            let relevant: Vec<_> = rows
                .iter()
                .filter(|r| ![3004, 4004].contains(&r.kind))
                .collect();
            if relevant.iter().all(|r| [11002, 11004].contains(&r.kind))
                && rows
                    .iter()
                    .all(|r| condition_rows(self.master, r).all(|c| c.kind != 4011))
            {
                continue;
            }
            if relevant.len() == 1 && relevant[0].kind == 11001 {
                let r = relevant[0];
                if r.value < 0
                    || r.value % 10000 != 0
                    || r.cumulative_id != 0
                    || r.condition_group != 0
                    || r.trigger_type != 2
                    || r.seconds != 0.0
                    || r.execute_limit != 0
                    || r.reset_group != 0
                    || r.release_group != 0
                        && self
                            .master
                            .condition_sets
                            .get(&r.release_group)
                            .is_none_or(|sets| {
                                sets.len() != 1
                                    || sets[0].len() != 1
                                    || self
                                        .master
                                        .conditions
                                        .get(&sets[0][0])
                                        .is_none_or(|c| c.kind != 7013 || !c.positive)
                            })
                    || r.limit_count != 0
                    || r.max_value != 0
                    || rows
                        .iter()
                        .any(|row| condition_rows(self.master, row).any(|c| c.kind == 4011))
                    || self
                        .master
                        .condition_sets
                        .get(&r.trigger_group)
                        .is_none_or(|sets| {
                            sets.len() != 1
                                || sets[0].len() != 1
                                || self
                                    .master
                                    .conditions
                                    .get(&sets[0][0])
                                    .is_none_or(|condition| {
                                        condition.kind != 7020 || !condition.positive
                                    })
                        })
                {
                    return None;
                }
                let shape = json!([
                    r.trigger_type,
                    r.execute_limit,
                    r.reset_group,
                    r.seconds,
                    r.condition_group,
                    r.trigger_group,
                    0,
                    r.cumulative_id,
                    r.targets,
                    r.limit_count,
                    r.max_value
                ]);
                if gauge_shape.as_ref().is_some_and(|old| old != &shape) {
                    return None;
                }
                gauge_shape = Some(shape);
                gauges.entry(r.value / 10000).or_insert(actor);
            } else {
                bursts.push(actor);
            }
        }
        bursts.sort();
        let mut choices = vec![Vec::new()];
        fn burst_choices(
            all: &[Performer],
            used: &mut [bool],
            at: &mut Vec<Performer>,
            out: &mut Vec<Vec<Performer>>,
        ) {
            if at.len() == 5 {
                return;
            }
            for i in 0..all.len() {
                if used[i] || i > 0 && all[i] == all[i - 1] && !used[i - 1] {
                    continue;
                }
                used[i] = true;
                at.push(all[i].clone());
                out.push(at.clone());
                burst_choices(all, used, at, out);
                at.pop();
                used[i] = false;
            }
        }
        burst_choices(
            &bursts,
            &mut vec![false; bursts.len()],
            &mut Vec::new(),
            &mut choices,
        );
        let mut totals = vec![HashMap::default(); 6];
        totals[0].insert(0, Vec::<Performer>::new());
        for count in 0..5 {
            for (sum, actors) in totals[count].clone() {
                for (&value, actor) in &gauges {
                    let next = sum + value;
                    if next >= 1 << 24 {
                        return None;
                    }
                    let mut actors = actors.clone();
                    actors.push(actor.clone());
                    totals[count + 1].entry(next).or_insert(actors);
                }
            }
        }
        let mut profiles = Vec::new();
        for burst in choices {
            let mut seen = HashSet::default();
            for table in totals.iter().take(6 - burst.len()) {
                for (&sum, gauge) in table {
                    if seen.insert(sum) {
                        let mut profile = gauge.clone();
                        profile.extend_from_slice(&burst);
                        profiles.push(profile);
                    }
                }
            }
        }
        self.collect_fixed_luck(profiles, &supports)
    }
    fn fixed_region_domains(&mut self) -> Option<&[i32]> {
        if self.fixed_region_domains.is_none() {
            let value = (|| {
                if self.members.iter().any(|member| {
                    self.master
                        .gekiso_skills
                        .get(&member.card.gekiso_skill_id)
                        .is_some_and(|skill| {
                            skill.mission_type != 2
                                && skill
                                    .effects
                                    .iter()
                                    .filter(|row| row.level == member.growth.gekiso_skill_level)
                                    .any(|row| {
                                        condition_rows(self.master, row)
                                            .any(|condition| condition.kind == 4011)
                                    })
                        })
                }) {
                    return None;
                }
                let ranges = crate::rewards::deterministic_ranges(self.master, self.chart)?;
                if ranges.windows(2).any(|pair| pair[0].1 >= pair[1].0) {
                    return None;
                }
                Some(
                    self.chart
                        .times
                        .iter()
                        .map(|&time| {
                            ranges
                                .iter()
                                .enumerate()
                                .find(|(_, range)| range.0 <= time && time <= range.1)
                                .map_or(0, |(index, _)| self.chart.setup.missions[index])
                        })
                        .collect(),
                )
            })();
            self.fixed_region_domains = Some(value);
        }
        self.fixed_region_domains.as_ref()?.as_deref()
    }
    fn fixed_just_hull(&mut self, count: usize) -> Option<Arc<crate::rewards::FixedComponent>> {
        if self.fixed_just_hull.is_none() {
            let value = self.build_fixed_just_hulls();
            self.fixed_just_hull = Some(value);
        }
        self.fixed_just_hull
            .as_ref()?
            .as_ref()
            .map(|hulls| Arc::clone(&hulls[count]))
    }
    fn build_fixed_just_hulls(&mut self) -> Option<Vec<Arc<crate::rewards::FixedComponent>>> {
        let domains = self.fixed_region_domains()?.to_vec();
        if !domains.contains(&3) {
            let zero = Arc::new(crate::rewards::FixedComponent {
                base: 0.0,
                converted: self.chart.full.converted as f64,
                width: self.snaps.len() + 1,
                rows: vec![[0.0; 5]; self.members.len() * (self.snaps.len() + 1)],
            });
            return Some(vec![zero; 6]);
        }
        let mut programs = HashMap::default();
        let mut snapshots = vec![Vec::new(); self.snaps.len()];
        for member in 0..self.members.len() {
            for (snap, choices) in snapshots.iter_mut().enumerate() {
                let choice = self.fixed_choice(member, snap as i32);
                if !choice.supported {
                    return None;
                }
                if let Some(converter) = choice.converter.as_ref() {
                    choices.push(converter.program);
                    programs
                        .entry(converter.program)
                        .or_insert_with(|| Arc::clone(converter));
                }
            }
        }
        let mut groups = HashMap::<Vec<usize>, usize>::default();
        let mut physical = 0usize;
        for mut choices in snapshots {
            if choices.is_empty() {
                continue;
            }
            physical += 1;
            choices.sort_unstable();
            choices.dedup();
            *groups.entry(choices).or_default() += 1;
        }
        let groups = groups
            .into_iter()
            .map(|(choices, count)| (choices, count.min(5)))
            .collect::<Vec<_>>();
        let mut completions = 1usize;
        let mut layer = 1usize;
        for depth in 1..=physical.min(5) {
            layer = layer
                .saturating_mul(6 - depth)
                .saturating_mul(physical + 1 - depth)
                / depth;
            completions = completions.saturating_add(layer);
        }
        let mut words = 1usize;
        let mut layer = 1usize;
        for _ in 0..5 {
            layer = layer.saturating_mul(programs.len());
            words = words.saturating_add(layer);
        }
        if words > completions {
            return None;
        }
        let mut states = HashSet::default();
        let mut sequences = HashSet::default();
        states.insert(([usize::MAX; 5], [0usize; 5]));
        sequences.insert([0usize; 5]);
        for depth in 0..5 {
            let mut next = HashSet::default();
            for (selected, sequence) in states {
                for (group, (choices, capacity)) in groups.iter().enumerate() {
                    if selected[..depth]
                        .iter()
                        .filter(|&&old| old == group)
                        .count()
                        >= *capacity
                    {
                        continue;
                    }
                    for &program in choices {
                        let mut selected = selected;
                        let mut sequence = sequence;
                        selected[depth] = group;
                        selected[..=depth].sort_unstable();
                        sequence[depth] = program;
                        next.insert((selected, sequence));
                        sequences.insert(sequence);
                    }
                }
            }
            states = next;
        }
        let mut traces: HashMap<usize, (usize, Arc<Context>)> = HashMap::default();
        for sequence in sequences {
            let actors = sequence
                .iter()
                .take_while(|&&program| program != 0)
                .map(|program| Arc::clone(&programs[program]))
                .collect::<Vec<_>>();
            let depth = actors.len();
            let (id, trace) = self
                .just_bounds
                .converter_trace(self.master, self.chart, &actors)
                .ok()??;
            if trace
                .judgement
                .iter()
                .enumerate()
                .any(|(index, &judgement)| {
                    ![5, 6].contains(&judgement)
                        || judgement != self.chart.judgements[index] && domains[index] != 3
                })
            {
                return None;
            }
            traces
                .entry(id)
                .and_modify(|old| old.0 = old.0.min(depth))
                .or_insert((depth, trace));
        }
        let mut layers = vec![Vec::new(); 6];
        for (id, (depth, trace)) in traces {
            layers[depth].push(self.fixed_component(FixedRegionKey::Just(id), &trace, &[])?);
        }
        let mut hulls: Vec<Arc<crate::rewards::FixedComponent>> = Vec::new();
        for components in layers {
            let mut current = hulls.last().map(|old| crate::rewards::FixedComponent {
                base: old.base,
                converted: old.converted,
                width: old.width,
                rows: old.rows.clone(),
            });
            for component in components {
                if let Some(hull) = &mut current {
                    hull.base = hull.base.max(component.base);
                    for (row, source) in hull.rows.iter_mut().zip(&component.rows) {
                        for (value, &high) in row.iter_mut().zip(source) {
                            *value = value.max(high);
                        }
                    }
                } else {
                    current = Some(crate::rewards::FixedComponent {
                        base: component.base,
                        converted: component.converted,
                        width: component.width,
                        rows: component.rows.clone(),
                    });
                }
            }
            hulls.push(Arc::new(current?));
        }
        Some(hulls)
    }
    fn fixed_component(
        &mut self,
        key: FixedRegionKey,
        trace: &Context,
        script: &[Performer],
    ) -> Option<Arc<crate::rewards::FixedComponent>> {
        if let Some(value) = self.fixed_components.get(&key) {
            self.fixed_component_order.retain(|old| old != &key);
            self.fixed_component_order.push_back(key);
            return Some(Arc::clone(value));
        }
        let (source, command_cap) = match &key {
            FixedRegionKey::Guarded(source, cap) => (source.as_ref(), Some(*cap)),
            _ => (&key, None),
        };
        let kind = match source {
            FixedRegionKey::Outside => 0,
            FixedRegionKey::Combo(_) => 1,
            FixedRegionKey::Luck(_) => 2,
            FixedRegionKey::Just(_) => 3,
            FixedRegionKey::Guarded(..) => unreachable!(),
        };
        let domains = self.fixed_region_domains()?.to_vec();
        let judgement = if kind == 3 {
            &trace.judgement
        } else {
            &self.chart.judgements
        };
        let mut chart = self.chart.converted_judgements(judgement)?;
        for (index, weight) in chart.note_frac.iter_mut().enumerate() {
            if domains[index] != kind {
                *weight = 0.0;
            }
        }
        let luck = match source {
            FixedRegionKey::Luck(luck) => Arc::clone(luck),
            _ => {
                let global = self.global_fixed_luck()?;
                if let Some(changes) = command_cap {
                    Arc::new(crate::rewards::LuckEnvelope {
                        fingerprint: OnceLock::new(),
                        moments: None,
                        changes,
                        rush: global.rush.clone(),
                        support: global.support.clone(),
                    })
                } else {
                    global
                }
            }
        };
        let combo = if kind == 1 {
            Some(self.fixed_combo_trace(script)?)
        } else {
            None
        };
        if self.converted_master.is_none() {
            let mut master = self.master.clone();
            let mut skills = (*master.gekiso_support_skills).clone();
            for skill in skills.values_mut() {
                skill.effects.retain(|row| row.kind != 13005);
            }
            master.gekiso_support_skills = Arc::new(skills);
            self.converted_master = Some(master);
        }
        let master = self.converted_master.as_ref().unwrap();
        let preparation_key = match source {
            FixedRegionKey::Just(id) => Some(*id),
            _ => None,
        };
        let mut preparation = self.fixed_preparations.get(&preparation_key).cloned();
        let model = crate::rewards::DeterministicBound::with_luck_preparation(
            master,
            &chart,
            self.members,
            self.snaps,
            1.0 + self.maximum_level as f64,
            (combo, Some(&luck)),
            &mut preparation,
        )?;
        if let Some(preparation) = preparation {
            if self.fixed_preparations.len() >= 16
                && !self.fixed_preparations.contains_key(&preparation_key)
            {
                self.fixed_preparations.clear();
            }
            self.fixed_preparations.insert(preparation_key, preparation);
        };
        let value = Arc::new(model.fixed_component()?);
        let bytes = value.rows.capacity() * std::mem::size_of::<[f64; 5]>() + 128;
        const CACHE_BYTES: usize = 16 * 1024 * 1024;
        if bytes <= CACHE_BYTES {
            while self.fixed_component_bytes + bytes > CACHE_BYTES {
                let Some(old) = self.fixed_component_order.pop_front() else {
                    break;
                };
                if let Some(value) = self.fixed_components.remove(&old) {
                    self.fixed_component_bytes -=
                        value.rows.capacity() * std::mem::size_of::<[f64; 5]>() + 128;
                }
            }
            self.fixed_component_bytes += bytes;
            self.fixed_components
                .insert(key.clone(), Arc::clone(&value));
            self.fixed_component_order.push_back(key);
        }
        Some(value)
    }
    fn trim_fixed_region_rows(&mut self) {
        while self.fixed_region_allocated.load(Ordering::Relaxed) > self.fixed_region_limit {
            let Some(key) = self.fixed_region_row_order.pop_front() else {
                break;
            };
            if let Some(rows) = self.fixed_region_rows.get(&key) {
                if rows.take_reference() {
                    self.fixed_region_row_order.push_back(key);
                    continue;
                }
            }
            self.fixed_region_rows.remove(&key);
        }
    }
    fn combined_fixed_rows(
        &mut self,
        key: FixedRegionalKey,
        components: &[Arc<crate::rewards::FixedComponent>],
    ) -> Option<Arc<crate::rewards::FixedRegionRows>> {
        if let Some(value) = self.fixed_region_rows.get(&key) {
            return Some(Arc::clone(value));
        }
        let value = Arc::new(crate::rewards::FixedRegionRows::new(
            components,
            Arc::clone(&self.fixed_region_allocated),
            2 * (key.combo.capacity() + std::mem::size_of::<FixedRegionalKey>()),
        )?);
        self.fixed_region_row_order.push_back(key.clone());
        self.fixed_region_rows.insert(key, Arc::clone(&value));
        Some(value)
    }
    fn combo_script(&mut self, members: &[usize]) -> Option<Vec<Performer>> {
        self.combo_scripts
            .get_or_insert_with(|| crate::reward_trace::Scripts::new(self.master, self.members))
            .select(members)
    }
    fn combo_key(&mut self, script: &[Performer]) -> String {
        if let Some((old, key)) = &self.last_combo_key {
            if old == script {
                return key.clone();
            }
        }
        let key = crate::reward_trace::combo_key(self.master, script);
        self.last_combo_key = Some((script.to_vec(), key.clone()));
        key
    }
    fn fixed_combo_trace(
        &mut self,
        script: &[Performer],
    ) -> Option<Arc<crate::reward_trace::ComboTrace>> {
        let key = self.combo_key(script);
        if !self.fixed_combo_traces.contains_key(&key) {
            let mut trace = crate::reward_trace::ComboTrace::new(self.master, self.chart, script)?;
            trace.cache_combo_supports();
            if self.fixed_combo_traces.len() >= 32 {
                if let Some(old) = self.fixed_combo_trace_order.pop_front() {
                    self.fixed_combo_traces.remove(&old);
                }
            }
            self.fixed_combo_traces.insert(key.clone(), Arc::new(trace));
        }
        self.fixed_combo_trace_order.retain(|old| old != &key);
        self.fixed_combo_trace_order.push_back(key.clone());
        self.fixed_combo_traces.get(&key).map(Arc::clone)
    }
    fn fixed_choice(&mut self, member: usize, snap: i32) -> &FixedChoice {
        let index = self.slot_index(member, snap);
        if self.fixed_choices[index].is_none() {
            let mut actor = self.actor_of(member, snap);
            let order = self.performer_key(&actor);
            let active = actor.gekisou_skill.is_some()
                || actor.mission_only.is_some_and(|mission| mission != 0);
            let lottery = active
                && actor.gekisou_support_skills.iter().any(|&(id, level)| {
                    self.master.gekiso_support_skills[&id]
                        .effects
                        .iter()
                        .any(|row| {
                            row.level == level
                                && ((11000..=11005).contains(&row.kind)
                                    || condition_rows(self.master, row).any(|c| c.kind == 4011))
                        })
                });
            actor.gekisou_support_skills.retain(|&(id, level)| {
                active
                    && self.master.gekiso_support_skills[&id]
                        .effects
                        .iter()
                        .any(|row| row.level == level && row.kind == 13005)
            });
            let mut supported = actor.gekisou_support_skills.iter().all(|&(id, level)| {
                self.master.gekiso_support_skills[&id]
                    .effects
                    .iter()
                    .filter(|row| row.level == level && row.kind == 13005)
                    .all(|row| {
                        condition_rows(self.master, row).all(|condition| {
                            [0, 1030, 1040, 2000, 2001, 5000, 7010, 7013, 7020]
                                .contains(&condition.kind)
                        }) && self
                            .master
                            .condition_sets
                            .get(&row.condition_group)
                            .into_iter()
                            .flatten()
                            .flatten()
                            .all(|id| {
                                self.master
                                    .conditions
                                    .get(id)
                                    .is_some_and(|c| [0, 2000, 2001, 5000].contains(&c.kind))
                            })
                    })
            });
            actor.gekisou_skill = None;
            actor.mission_only = Some(1);
            let converter = if actor.gekisou_support_skills.is_empty() {
                None
            } else {
                let converter = self.just_bounds.converter(self.master, &actor);
                supported &= converter.is_some();
                converter
            };
            let choice = FixedChoice {
                order,
                converter,
                lottery,
                supported,
            };
            self.fixed_choices[index] = Some(choice);
        }
        self.fixed_choices[index].as_ref().unwrap()
    }
    fn fixed_lottery(&mut self, member: usize, snap: i32) -> bool {
        let index = self.slot_index(member, snap);
        if let Some(choice) = &self.fixed_choices[index] {
            choice.lottery
        } else {
            self.fixed_choice(member, snap).lottery
        }
    }
    fn shape_id(&mut self, member: usize, snap: i32) -> u32 {
        let index = self.slot_index(member, snap);
        if self.shape_ids[index] == u32::MAX {
            let performer = if self.healthy && self.owner_local && self.just_context.is_some() {
                let mut actor = Performer::from_member(
                    self.master,
                    &self.members[member],
                    (snap >= 0).then(|| &self.snaps[snap as usize]),
                )
                .gekiso_only();
                self.support_aliases.retain_active(self.master, &mut actor);
                let original = actor.clone();
                self.support_aliases.apply(self.master, &mut actor);
                if original.gekisou_support_skills != actor.gekisou_support_skills {
                    let converter = original.gekisou_support_skills.iter().any(|&(id, level)| {
                        self.master.gekiso_support_skills[&id]
                            .effects
                            .iter()
                            .any(|r| r.level == level && r.kind == 13005)
                    });
                    let mut protected = Vec::new();
                    if let Some((id, level)) = original.gekisou_skill {
                        protected.extend(
                            self.master.gekiso_skills[&id]
                                .effects
                                .iter()
                                .filter(|r| r.level == level)
                                .map(|r| r.id),
                        );
                    }
                    if snap >= 0 {
                        let snapshot = &self.snaps[snap as usize];
                        for (id, level) in snapshot
                            .card
                            .support_skills
                            .into_iter()
                            .zip(snapshot.skill_levels)
                        {
                            protected.extend(
                                self.master
                                    .support_skills
                                    .get(&id)
                                    .into_iter()
                                    .flatten()
                                    .filter(|r| {
                                        r.level == level
                                            && [2000, 2001, 2002, 2003].contains(&r.kind)
                                    })
                                    .map(|r| r.id),
                            );
                        }
                    }
                    let collision = original
                        .gekisou_support_skills
                        .iter()
                        .chain(&actor.gekisou_support_skills)
                        .any(|&(id, level)| {
                            self.master.gekiso_support_skills[&id]
                                .effects
                                .iter()
                                .any(|r| r.level == level && protected.contains(&r.id))
                        });
                    if converter || collision {
                        actor = original;
                    }
                }
                actor
            } else {
                self.actor_of(member, snap)
            };
            let shape = self.slot(member, snap).signature();
            let next = self.shapes.len() as u32;
            self.shape_ids[index] = *self.shapes.entry((shape, performer)).or_insert(next);
        }
        self.shape_ids[index]
    }
    fn bound_shape_id(&mut self, member: usize, snap: i32) -> u32 {
        let index = self.slot_index(member, snap);
        if self.bound_shape_ids[index] == u32::MAX {
            let performer = self.bound_actor_of(member, snap);
            let shape = self.slot(member, snap).signature();
            let next = self.shapes.len() as u32;
            self.bound_shape_ids[index] = *self.shapes.entry((shape, performer)).or_insert(next);
        }
        self.bound_shape_ids[index]
    }
    fn bound_actor_of(&mut self, member: usize, snap: i32) -> Performer {
        let index = self.slot_index(member, snap);
        if let Some(actor) = &self.bound_actors[index] {
            return actor.clone();
        }
        let actor = self.actor_of(member, snap);
        let actor = self
            .deck_key(&[actor], 0)
            .0
            .into_iter()
            .next()
            .unwrap_or_default();
        self.bound_converters[index] = actor.gekisou_support_skills.iter().any(|&(id, level)| {
            self.master.gekiso_support_skills[&id]
                .effects
                .iter()
                .any(|r| r.level == level && r.kind == 13005)
        });
        self.bound_actors[index] = Some(actor.clone());
        actor
    }
    fn prepare_numeric_hull(&mut self) -> Option<Arc<Vec<Arc<crate::just_bound::Upper>>>> {
        if self.numeric_hull_ready {
            return self.numeric_hull.clone();
        }
        self.numeric_hull_ready = true;
        let result = (|| {
            let mut candidates = vec![Vec::new(); self.snaps.len()];
            let mut programs = HashMap::default();
            for member in 0..self.members.len() {
                for (snap, choices) in candidates.iter_mut().enumerate() {
                    if !self.support_chain[member * self.snaps.len() + snap] {
                        continue;
                    }
                    let mut actor = self.bound_actor_of(member, snap as i32);
                    let index = self.slot_index(member, snap as i32);
                    if !self.bound_converters[index] {
                        continue;
                    }
                    actor.gekisou_support_skills.retain(|&(id, level)| {
                        self.master.gekiso_support_skills[&id]
                            .effects
                            .iter()
                            .any(|r| r.level == level && r.kind == 13005)
                    });
                    let prepared = self.just_bounds.converter(self.master, &actor)?;
                    choices.push(prepared.program);
                    programs.entry(prepared.program).or_insert(prepared);
                }
            }
            let mut domains = HashMap::<Vec<usize>, usize>::default();
            let mut converter_count = 0;
            for mut choices in candidates {
                if choices.is_empty() {
                    continue;
                }
                choices.sort_unstable();
                choices.dedup();
                converter_count += 1;
                *domains.entry(choices).or_default() += 1;
            }
            let domains: Vec<_> = domains
                .into_iter()
                .map(|(choices, count)| (choices, count.min(5)))
                .collect();
            let mut completions = 1_usize;
            let mut layer = 1_usize;
            for count in 1..=converter_count.min(5) {
                layer = layer
                    .saturating_mul(6 - count)
                    .saturating_mul(converter_count + 1 - count)
                    / count;
                completions = completions.saturating_add(layer);
            }
            let mut words = 1_usize;
            let mut layer = 1_usize;
            for _ in 0..5 {
                layer = layer.saturating_mul(programs.len());
                words = words.saturating_add(layer);
            }
            if words > completions {
                return None;
            }
            let mut states = HashSet::default();
            let mut sequences = HashSet::default();
            states.insert(([usize::MAX; 5], [0usize; 5]));
            sequences.insert([0usize; 5]);
            // Every legal ordered converter list is a word using at most each domain's physical SNP capacity.
            for count in 0..5 {
                let mut next = HashSet::default();
                for (selected, sequence) in states {
                    for (domain, (choices, capacity)) in domains.iter().enumerate() {
                        if selected[..count].iter().filter(|&&id| id == domain).count() >= *capacity
                        {
                            continue;
                        }
                        let mut selected = selected;
                        selected[count] = domain;
                        selected[..=count].sort_unstable();
                        for &program in choices {
                            let mut sequence = sequence;
                            sequence[count] = program;
                            sequences.insert(sequence);
                            next.insert((selected, sequence));
                        }
                    }
                }
                states = next;
            }
            let mut hull = Vec::new();
            let mut traces = HashSet::default();
            for sequence in sequences {
                let converters: Vec<_> = sequence
                    .iter()
                    .take_while(|&&id| id != 0)
                    .map(|id| Arc::clone(&programs[id]))
                    .collect();
                let upper = self
                    .just_bounds
                    .converter_upper(self.master, self.chart, &converters)
                    .ok()
                    .flatten()?;
                if traces.insert(upper.id) {
                    hull.push(upper);
                }
            }
            hull.sort_unstable_by(|a, b| b.base.total_cmp(&a.base));
            Some(Arc::new(hull))
        })();
        self.numeric_hull = result.clone();
        result
    }
    fn choice_domain(&mut self, choices: &[i32]) -> u32 {
        if let Some(&id) = self.choice_domains.get(choices) {
            return id;
        }
        let id = self.choice_domains.len() as u32;
        self.choice_domains.insert(choices.to_vec(), id);
        id
    }
    fn slot_index(&self, m: usize, s: i32) -> usize {
        m * (self.snaps.len() + 1) + (s + 1) as usize
    }
    fn slot(&self, m: usize, s: i32) -> &Slot {
        &self.slots[self.slot_index(m, s)]
    }
    fn support(&self, m: usize, s: i32) -> Option<Performer> {
        if s < 0 {
            None
        } else {
            self.support[m * self.snaps.len() + s as usize].clone()
        }
    }
    fn performer_key(&mut self, p: &Performer) -> u32 {
        if let Some(&id) = self.performer_key_cache.get(p) {
            return id;
        }
        let attrs = if p.gekisou_support_skills.is_empty() {
            &self.read_members
        } else {
            &self.read_all
        };
        // A mission without skills only gates the performer's own supports: without them it simulates as nothing.
        let mission = if p.gekisou_skill.is_some() || !p.gekisou_support_skills.is_empty() {
            p.mission_only.unwrap_or(0)
        } else {
            0
        };
        let mut values = vec![
            json!(p.gekisou_skill),
            json!(mission),
            json!(p.gekisou_support_skills),
        ];
        values.extend(attrs.iter().map(|name| field(p, name)));
        let json = Value::Array(values).to_string();
        let next = self.chart.performer_json.len() as u32;
        let id = *self
            .chart
            .performer_ids
            .entry(json.clone())
            .or_insert_with(|| {
                self.chart.performer_json.push(json);
                next
            });
        self.performer_key_cache.insert(p.clone(), id);
        id
    }
    fn context_performer_key(&mut self, p: &Performer) -> u32 {
        if let Some(&id) = self.context_keys.get(p) {
            return id;
        }
        let mission = if p.gekisou_skill.is_some() || !p.gekisou_support_skills.is_empty() {
            p.mission_only.unwrap_or(0)
        } else {
            0
        };
        let mut values = vec![
            json!(p.gekisou_skill),
            json!(mission),
            json!(p.gekisou_support_skills),
        ];
        values.extend(self.read_all.iter().map(|name| field(p, name)));
        let text = Value::Array(values).to_string();
        let next = self.chart.performer_json.len() as u32;
        let id = *self
            .chart
            .performer_ids
            .entry(text.clone())
            .or_insert_with(|| {
                self.chart.performer_json.push(text);
                next
            });
        self.context_keys.insert(p.clone(), id);
        id
    }
    /// A deck's note weights. Bounds and gains read only these, so the context behind them is not kept.
    fn weights_of(
        &mut self,
        deck: &[Performer],
        within: Option<&Master>,
        tag: i32,
    ) -> Arc<Weights> {
        let (deck, key) = self.deck_key(deck, tag);
        if let Some((weights, recent)) = self.chart.weights.get_mut(&key) {
            *recent = true;
            return Arc::clone(weights);
        }
        if let Some(value) = self.cache.get(&key) {
            return Arc::clone(&value.weights);
        }
        let context = gekiso::context(
            within.unwrap_or(self.master),
            self.chart,
            &deck,
            &self.seeds,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let weights = Arc::new(self.weights(&context));
        self.store_weights(key, Arc::clone(&weights));
        weights
    }
    fn context_of(
        &mut self,
        deck: &[Performer],
        within: Option<&Master>,
        tag: i32,
    ) -> (Arc<Context>, Arc<Weights>) {
        let (deck, key) = self.deck_key(deck, tag);
        if let Some(value) = self.cache.get(&key) {
            return (Arc::clone(&value.context), Arc::clone(&value.weights));
        }
        let context = Arc::new(
            gekiso::context(
                within.unwrap_or(self.master),
                self.chart,
                &deck,
                &self.seeds,
            )
            .unwrap_or_else(|e| panic!("{e}")),
        );
        let weights = Arc::new(self.weights(&context));
        let bytes = 128
            + context.retained_bytes()
            + weights.general.len() * 8
            + weights
                .judgements
                .iter()
                .map(|r| r.len() * 8)
                .sum::<usize>();
        if bytes > 16 * 1024 * 1024 {
            return (context, weights);
        }
        // A lottery record shared by several contexts counts once, while any of them holds it.
        let record = context.shared_record();
        self.cache_bytes += bytes;
        if let Some((address, size)) = record {
            let count = self.cache_records.entry(address).or_insert(0);
            if *count == 0 {
                self.cache_bytes += size;
            }
            *count += 1;
        }
        self.cache_order.push_back(key.clone());
        self.cache.insert(
            key.clone(),
            Cached {
                context: Arc::clone(&context),
                weights: Arc::clone(&weights),
                bytes,
                record,
            },
        );
        if self.cache_bytes > 16 * 1024 * 1024 {
            while self.cache_bytes > 12 * 1024 * 1024 {
                let Some(old) = self.cache_order.pop_front() else {
                    break;
                };
                if old == key {
                    self.cache_order.push_back(old);
                    break;
                }
                if let Some(value) = self.cache.remove(&old) {
                    self.cache_bytes -= value.bytes;
                    if let Some((address, size)) = value.record {
                        if let Some(count) = self.cache_records.get_mut(&address) {
                            *count -= 1;
                            if *count == 0 {
                                self.cache_records.remove(&address);
                                self.cache_bytes -= size;
                            }
                        }
                    }
                }
            }
        }
        (context, weights)
    }
    fn weights(&self, ctx: &Context) -> Weights {
        note_weights(self.master, self.chart, ctx)
    }
    fn fields_for(&mut self, mut skills: Vec<(bool, i32)>) -> (Vec<&'static str>, bool) {
        skills.sort_unstable();
        skills.dedup();
        if let Some(fields) = self.context_fields.get(&skills) {
            return fields.clone();
        }
        let mut active = self.master.clone();
        active.gekiso_skills = Arc::new(
            skills
                .iter()
                .filter(|&&(support, _)| !support)
                .filter_map(|&(_, id)| self.master.gekiso_skills.get(&id).map(|s| (id, s.clone())))
                .collect(),
        );
        active.gekiso_support_skills = Arc::new(
            skills
                .iter()
                .filter(|&&(support, _)| support)
                .filter_map(|&(_, id)| {
                    self.master
                        .gekiso_support_skills
                        .get(&id)
                        .map(|s| (id, s.clone()))
                })
                .collect(),
        );
        let local = active
            .gekiso_skills
            .values()
            .chain(active.gekiso_support_skills.values())
            .flat_map(|s| &s.effects)
            .all(|row| {
                if row.cumulative_id != 0
                    && self
                        .master
                        .cumulative
                        .get(&row.cumulative_id)
                        .is_some_and(|c| (3000..=3005).contains(&c.kind))
                {
                    return false;
                }
                [
                    row.trigger_group,
                    row.condition_group,
                    row.release_group,
                    row.reset_group,
                ]
                .into_iter()
                .all(|group| {
                    self.master
                        .condition_sets
                        .get(&group)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .filter_map(|id| self.master.conditions.get(id))
                        .all(|c| {
                            [
                                1030, 2000, 2001, 4011, 5000, 7000, 7001, 7002, 7003, 7004, 7005,
                                7006, 7007, 7010, 7011, 7012, 7013, 7020, 7021,
                            ]
                            .contains(&c.kind)
                        })
                })
            });
        let fields = (read_attributes(&active, true), local);
        if self.context_fields.len() >= 1024 {
            self.context_fields.clear();
        }
        self.context_fields.insert(skills, fields.clone());
        fields
    }
    /// The sorted deck and its cache key.
    fn deck_key(&mut self, deck: &[Performer], tag: i32) -> (Vec<Performer>, (i32, Vec<u32>)) {
        let mut keyed: Vec<_> = deck
            .iter()
            .map(|p| {
                let raw = self.performer_key(p);
                if tag == 0 && self.owner_local {
                    if let Some((normalized, id)) = self.normalized_performers.get(&raw) {
                        return (normalized.clone(), raw, Some(*id));
                    }
                }
                (p.clone(), raw, None)
            })
            .collect();
        keyed.sort_by(|a, b| {
            self.chart.performer_json[a.1 as usize].cmp(&self.chart.performer_json[b.1 as usize])
        });
        if tag == 0 {
            for (p, _, cached) in &mut keyed {
                if cached.is_some() {
                    continue;
                }
                self.just_counters.normalize(p);
                self.support_aliases.apply(self.master, p);
            }
            let skill_ids = |p: &Performer| {
                p.gekisou_skill
                    .map(|(id, _)| (false, id))
                    .into_iter()
                    .chain(p.gekisou_support_skills.iter().map(|&(id, _)| (true, id)))
                    .collect::<Vec<_>>()
            };
            let (all_fields, local) = if self.owner_local {
                (Vec::new(), true)
            } else {
                self.fields_for(keyed.iter().flat_map(|(p, _, _)| skill_ids(p)).collect())
            };
            for (p, raw, cached) in &mut keyed {
                if cached.is_some() {
                    continue;
                }
                let owned = skill_ids(p);
                let fields = if local {
                    self.fields_for(owned.clone()).0
                } else {
                    all_fields.clone()
                };
                if p.gekisou_skill.is_none() {
                    p.mission_only = p.mission_only.map(|_| 1);
                }

                if local && p.band_id != 0 {
                    let referenced = owned
                        .iter()
                        .filter_map(|&(support, id)| {
                            if support {
                                self.master.gekiso_support_skills.get(&id)
                            } else {
                                self.master.gekiso_skills.get(&id)
                            }
                        })
                        .flat_map(|s| &s.effects)
                        .flat_map(|r| {
                            [
                                r.trigger_group,
                                r.condition_group,
                                r.release_group,
                                r.reset_group,
                            ]
                        })
                        .flat_map(|group| {
                            self.master
                                .condition_sets
                                .get(&group)
                                .into_iter()
                                .flatten()
                                .flatten()
                        })
                        .filter_map(|id| self.master.conditions.get(id))
                        .filter(|c| c.kind == 5000)
                        .flat_map(|c| &c.targets)
                        .any(|id| {
                            self.master
                                .targets
                                .get(id)
                                .is_some_and(|t| t.band_id == p.band_id)
                        });
                    if !referenced {
                        p.band_id = 0;
                    }
                }
                if !fields.contains(&"bandId") {
                    p.band_id = 0;
                }
                if !fields.contains(&"cardType") {
                    p.card_type = 0;
                }
                if !fields.contains(&"characterId") {
                    p.character_id = 0;
                }
                if !fields.contains(&"tagIds") {
                    p.tag_ids.clear();
                }
                if !fields.contains(&"liveSkillCategories") {
                    p.live_skill_categories.clear();
                }
                if !fields.contains(&"gekisouSkillCategories") {
                    p.gekisou_skill_categories.clear();
                }
                if !fields.contains(&"gekisouMissionType") {
                    p.gekisou_mission_type = 0;
                }
                if let Some(just) = &self.just_context {
                    just.normalize(p, self.master);
                }
                if self.owner_local {
                    let id = self.context_performer_key(p);
                    self.normalized_performers.insert(*raw, (p.clone(), id));
                    *cached = Some(id);
                }
            }
            if local {
                keyed.retain(|(p, _, _)| {
                    p.gekisou_skill.is_some() || !p.gekisou_support_skills.is_empty()
                });
            }
        }
        let mut deck = Vec::with_capacity(keyed.len());
        let mut ids = Vec::with_capacity(keyed.len());
        for (p, _, cached) in keyed {
            ids.push(cached.unwrap_or_else(|| self.context_performer_key(&p)));
            deck.push(p);
        }
        (deck, (tag, ids))
    }
    /// Computes the support bounds a warm pass at `level` is about to read, several at a time where the target has
    /// threads, and keeps their weights in the cache that `weights_of` reads.
    fn prefetch_supports(&mut self, level: i32, pairs: &[(usize, i32)]) {
        if self.chart.luck {
            return;
        }
        let at = self
            .grid
            .iter()
            .find(|&&g| g >= level)
            .copied()
            .unwrap_or(self.maximum_level);
        let data = self.level(at);
        let mut seen = HashSet::default();
        let mut jobs = Vec::new();
        for &(m, s) in pairs {
            let Some(p) = self.support(m, s) else {
                continue;
            };
            if self
                .support_high
                .contains_key(&(json!([p.gekisou_support_skills, p.band_id]).to_string(), at))
            {
                continue;
            }
            let combined: Vec<Performer> = data
                .deck
                .iter()
                .cloned()
                .chain(std::iter::once(p))
                .collect();
            let (deck, key) = self.deck_key(&combined, at);
            if self.chart.weights.contains_key(&key)
                || self.cache.contains_key(&key)
                || !seen.insert(key.clone())
            {
                continue;
            }
            jobs.push((deck, key));
        }
        if jobs.len() < 2 {
            return;
        }
        let (master, chart, seeds) = (&data.within, &*self.chart, &self.seeds);
        let Ok(found) = crate::parallel::map(&jobs, |(deck, _)| {
            gekiso::context_fixed(master, chart, deck, seeds)
                .map(|ctx| note_weights(master, chart, &ctx))
        }) else {
            return;
        };
        for ((_, key), weights) in jobs.into_iter().zip(found) {
            self.store_weights(key, Arc::new(weights));
        }
    }
    fn store_weights(&mut self, key: (i32, Vec<u32>), weights: Arc<Weights>) {
        let bytes = 128
            + (weights.general.len() + weights.judgements.iter().map(Vec::len).sum::<usize>()) * 8;
        const WEIGHT_BYTES: usize = 32 * 1024 * 1024;
        while self.chart.weight_bytes + bytes > WEIGHT_BYTES {
            let Some((old, size)) = self.chart.weight_order.pop_front() else {
                break;
            };
            if let Some((_, recent)) = self.chart.weights.get_mut(&old) {
                if *recent {
                    *recent = false;
                    self.chart.weight_order.push_back((old, size));
                    continue;
                }
            }
            self.chart.weights.remove(&old);
            self.chart.weight_bytes -= size;
        }
        self.chart.weight_bytes += bytes;
        self.chart.weight_order.push_back((key.clone(), bytes));
        self.chart.weights.insert(key, (weights, true));
    }
}
/// A deck's per-note score weights under its context.
pub(crate) fn note_weights(master: &Master, chart: &Chart, ctx: &Context) -> Weights {
    if !ctx.draws.is_empty() {
        let count = ctx.draws.len() as f64;
        let mut result = Weights {
            base: 0.0,
            rush: 0.0,
            general: vec![0.0; chart.notes.len() + 1],
            judgements: std::array::from_fn(|_| vec![0.0; chart.notes.len() + 1]),
        };
        for draw in &ctx.draws {
            let weights = note_weights(master, chart, draw);
            result.base += weights.base / count;
            result.rush += weights.rush / count;
            for (sum, value) in result.general.iter_mut().zip(weights.general) {
                *sum += value / count;
            }
            for (sums, values) in result.judgements.iter_mut().zip(weights.judgements) {
                for (sum, value) in sums.iter_mut().zip(values) {
                    *sum += value / count;
                }
            }
        }
        return result;
    }
    let combo = chart.combo(master, &ctx.judgement);
    {
        let n = chart.notes.len();
        let unit = master.live.adjustment * chart.difficulty_factor;
        let mut general = vec![0.0; n + 1];
        let mut judgements: [Vec<f64>; 3] = std::array::from_fn(|_| vec![0.0; n + 1]);
        let (mut base, mut rush) = (0.0, 0.0);
        let bonus = ctx
            .nominal
            .as_ref()
            .map(|n| n.rush_factor - 1.0)
            .unwrap_or(0.0);
        for i in 0..n {
            let range = chart.range_of[i];
            let range_factor = if range >= 0 {
                1.0 + chart.range_percent[range as usize] as f64 / 100.0
            } else {
                1.0
            };
            let beta = range_factor
                * unit
                * chart.note_frac[i]
                * chart.judge_frac[ctx.judgement[i]]
                * combo[i]
                * ctx.gk[i];
            base += beta * ctx.up[i];
            if let Some(nominal) = &ctx.nominal {
                rush += beta
                    * (nominal.probabilities[2][i]
                        + nominal.probabilities[3][i]
                        + bonus * nominal.probabilities[3][i]);
            }
            let live = beta * ctx.luck[i];
            general[i + 1] = general[i] + live;
            for (j, row) in judgements.iter_mut().enumerate() {
                row[i + 1] = row[i] + if ctx.judgement[i] == j + 4 { live } else { 0.0 };
            }
        }
        Weights {
            base,
            rush,
            general,
            judgements,
        }
    }
}
impl Kernel<'_> {
    fn regional_gain_reference(
        &mut self,
        support: &Performer,
        reference: &[Performer],
        scoped: bool,
    ) -> Option<Vec<Performer>> {
        if !self.healthy || !self.chart.luck || support.gekisou_support_skills.is_empty() {
            return None;
        }
        let mut support = support.clone();
        self.support_aliases.apply(self.master, &mut support);
        let mission = scoped
            .then(|| {
                let missions: BTreeSet<_> = support
                    .gekisou_support_skills
                    .iter()
                    .map(|(id, _)| self.master.gekiso_support_skills[id].mission_type)
                    .collect();
                (missions.len() == 1).then(|| *missions.first().unwrap())
            })
            .flatten();
        let group = |group: i32, kinds: &[i32]| {
            self.master
                .condition_sets
                .get(&group)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|id| self.master.conditions.get(id))
                .all(|c| kinds.contains(&c.kind))
        };
        for &(id, level) in &support.gekisou_support_skills {
            let skill = self.master.gekiso_support_skills.get(&id)?;
            if ![1, 3].contains(&skill.mission_type) {
                return None;
            }
            for row in skill.effects.iter().filter(|r| r.level == level) {
                if row.kind != 2001
                    || row.value < 0
                    || row.seconds != 0.0
                    || row.trigger_type != 1
                    || row.trigger_group == 0
                    || row.release_group == 0
                    || row.reset_group != 0
                    || !group(row.trigger_group, &[7010])
                    || !group(row.release_group, &[7013])
                    || [row.trigger_group, row.release_group].iter().any(|g| {
                        self.master
                            .condition_sets
                            .get(g)
                            .into_iter()
                            .flatten()
                            .flatten()
                            .any(|id| self.master.conditions.get(id).is_some_and(|c| !c.positive))
                    })
                    || !group(row.condition_group, &[5000, 2000, 2001])
                    || self
                        .master
                        .cumulative
                        .get(&row.cumulative_id)
                        .is_none_or(|c| {
                            if skill.mission_type == 1 {
                                c.kind != 7001
                            } else {
                                !(1000..=1002).contains(&c.kind)
                            }
                        })
                {
                    return None;
                }
            }
        }
        let mut combo = Vec::new();
        for original in reference {
            let mut p = original.clone();
            self.just_counters.normalize(&mut p);
            self.support_aliases.apply(self.master, &mut p);
            let mut retained = Vec::new();
            for &(id, level) in &p.gekisou_support_skills {
                let skill = self.master.gekiso_support_skills.get(&id)?;
                for row in skill.effects.iter().filter(|r| r.level == level) {
                    let known = if skill.mission_type == 2 {
                        [11000, 11001, 11002, 11003, 11004, 11005, 2000, 2001, 4004]
                            .contains(&row.kind)
                    } else {
                        [13005, 12006, 4004].contains(&row.kind)
                            && [1, 3].contains(&skill.mission_type)
                            && (row.cumulative_id == 0
                                || self
                                    .master
                                    .cumulative
                                    .get(&row.cumulative_id)
                                    .is_some_and(|c| (1000..=1002).contains(&c.kind)))
                    };
                    if !known
                        || [
                            row.trigger_group,
                            row.condition_group,
                            row.release_group,
                            row.reset_group,
                        ]
                        .iter()
                        .any(|&g| {
                            !group(
                                g,
                                &[
                                    1030, 2000, 2001, 4011, 5000, 7000, 7001, 7002, 7003, 7004,
                                    7005, 7006, 7007, 7010, 7013, 7020, 7021,
                                ],
                            )
                        })
                    {
                        return None;
                    }
                    if skill.mission_type != 2
                        && [
                            row.trigger_group,
                            row.condition_group,
                            row.release_group,
                            row.reset_group,
                        ]
                        .iter()
                        .any(|&g| {
                            !group(
                                g,
                                &[
                                    1030, 2000, 2001, 5000, 7001, 7002, 7003, 7004, 7005, 7006,
                                    7007, 7010, 7013, 7020,
                                ],
                            )
                        })
                    {
                        return None;
                    }
                }
                if skill.mission_type != 2 {
                    retained.push((id, level));
                }
            }
            p.gekisou_support_skills = retained;
            let Some((id, level)) = p.gekisou_skill else {
                if !p.gekisou_support_skills.is_empty() {
                    combo.push(p);
                }
                continue;
            };
            let skill = self.master.gekiso_skills.get(&id)?;
            for row in skill.effects.iter().filter(|r| r.level == level) {
                let supported = if [1, 4].contains(&skill.mission_type) {
                    [12000, 3004, 4004].contains(&row.kind) && row.cumulative_id == 0
                } else {
                    [
                        11000, 11001, 11002, 11003, 11004, 11005, 13000, 13002, 13003, 13004, 3004,
                        4004,
                    ]
                    .contains(&row.kind)
                };
                if !supported
                    || [
                        row.trigger_group,
                        row.condition_group,
                        row.release_group,
                        row.reset_group,
                    ]
                    .iter()
                    .any(|&g| {
                        !group(
                            g,
                            &[
                                1030,
                                2000,
                                2001,
                                5000,
                                7000,
                                7001,
                                7002,
                                7003,
                                7004,
                                7005,
                                7006,
                                7007,
                                7010,
                                7013,
                                7020,
                                7021,
                                if skill.mission_type == 2 { 4011 } else { -1 },
                            ],
                        )
                    })
                {
                    return None;
                }
            }
            if skill.mission_type == 2 {
                p.gekisou_skill = None;
                p.mission_only = Some(1);
            }
            if p.gekisou_skill.is_some() || !p.gekisou_support_skills.is_empty() {
                combo.push(p);
            }
        }
        if crate::rewards::deterministic_ranges(self.master, self.chart)?
            .windows(2)
            .any(|ranges| ranges[0].1 >= ranges[1].0)
        {
            return None;
        }
        if let Some(mission) = mission {
            for p in &mut combo {
                if mission == 3
                    && p.gekisou_skill
                        .is_some_and(|(id, _)| self.master.gekiso_skills[&id].mission_type == 1)
                {
                    p.gekisou_skill = None;
                    p.mission_only = Some(1);
                }
                let mut retained = Vec::new();
                for &(id, level) in &p.gekisou_support_skills {
                    let skill = &self.master.gekiso_support_skills[&id];
                    if skill.mission_type == mission {
                        retained.push((id, level));
                        continue;
                    }
                    let scoped = skill
                        .effects
                        .iter()
                        .filter(|r| r.level == level)
                        .all(|row| {
                            row.kind == 4004
                                || (row.release_group != 0
                                    && group(row.release_group, &[7013])
                                    && self.master.condition_sets[&row.release_group]
                                        .iter()
                                        .flatten()
                                        .all(|id| self.master.conditions[id].positive))
                                || (row.trigger_type == 2
                                    && row.seconds == 0.0
                                    && self
                                        .master
                                        .condition_sets
                                        .get(&row.trigger_group)
                                        .is_some_and(|sets| {
                                            sets.iter().all(|set| {
                                                set.iter().any(|id| {
                                                    self.master.conditions.get(id).is_some_and(
                                                        |c| c.kind == 7020 && c.positive,
                                                    )
                                                })
                                            })
                                        }))
                        });
                    if !scoped {
                        return None;
                    }
                }
                p.gekisou_support_skills = retained;
                if mission == 3 {
                    let skills = p
                        .gekisou_skill
                        .into_iter()
                        .map(|(id, level)| (false, id, level))
                        .chain(
                            p.gekisou_support_skills
                                .iter()
                                .map(|&(id, level)| (true, id, level)),
                        );
                    for (support, id, level) in skills {
                        let skill = if support {
                            &self.master.gekiso_support_skills[&id]
                        } else {
                            &self.master.gekiso_skills[&id]
                        };
                        if skill.effects.iter().filter(|r| r.level == level).any(|r| {
                            self.master
                                .cumulative
                                .get(&r.cumulative_id)
                                .is_some_and(|c| c.kind == 7001)
                                || [
                                    r.trigger_group,
                                    r.condition_group,
                                    r.release_group,
                                    r.reset_group,
                                ]
                                .iter()
                                .any(|g| {
                                    self.master
                                        .condition_sets
                                        .get(g)
                                        .into_iter()
                                        .flatten()
                                        .flatten()
                                        .any(|id| {
                                            self.master
                                                .conditions
                                                .get(id)
                                                .is_some_and(|c| [7001, 7005].contains(&c.kind))
                                        })
                                })
                        }) {
                            return None;
                        }
                    }
                }
            }
            combo.retain(|p| p.gekisou_skill.is_some() || !p.gekisou_support_skills.is_empty());
        }
        Some(combo)
    }
    fn gain(
        &mut self,
        deck: &[Performer],
        reference: &[Performer],
        within: Option<&Master>,
        tag: i32,
    ) -> f64 {
        if self.owner_local && deck.len() == 1 && deck[0].gekisou_skill.is_none() {
            let mut support = deck[0].clone();
            self.support_aliases.apply(self.master, &mut support);
            if support.gekisou_support_skills.is_empty() {
                return 0.0;
            }
        }
        if within.is_none() && tag == 0 && deck.len() == 1 {
            if let Some(scoped) = self.regional_gain_reference(&deck[0], reference, true) {
                let reference = scoped;
                let (reference, (_, ids)) = self.deck_key(&reference, 0);
                let support = self.performer_key(&deck[0]);
                let mut plain = 0i32;
                let mut distinct = Vec::new();
                for (actor, id) in reference.iter().zip(ids) {
                    let value = actor.gekisou_skill.and_then(|(id, level)| {
                        let skill = &self.master.gekiso_skills[&id];
                        let rows: Vec<_> =
                            skill.effects.iter().filter(|r| r.level == level).collect();
                        if skill.mission_type != 1
                            || !actor.gekisou_support_skills.is_empty()
                            || rows.len() != 1
                        {
                            return None;
                        }
                        let row = rows[0];
                        (row.kind == 12000
                            && row.trigger_type == 2
                            && row.condition_group == 0
                            && row.release_group == 0
                            && row.reset_group == 0
                            && row.cumulative_id == 0
                            && row.execute_limit == 0
                            && row.seconds == 0.0
                            && row.value >= 0
                            && self
                                .master
                                .condition_sets
                                .get(&row.trigger_group)
                                .is_some_and(|sets| {
                                    !sets.is_empty()
                                        && sets.iter().all(|set| !set.is_empty())
                                        && sets.iter().flatten().all(|id| {
                                            self.master.conditions.get(id).is_some_and(|c| {
                                                c.kind == 7020
                                                    && c.positive
                                                    && !c.targets.is_empty()
                                                    && c.targets.iter().all(|id| {
                                                        self.master
                                                            .targets
                                                            .get(id)
                                                            .is_some_and(|t| t.mission_type == 1)
                                                    })
                                            })
                                        })
                                }))
                        .then_some(row.value)
                    });
                    if let Some(value) = value {
                        if let Some(total) = plain.checked_add(value) {
                            plain = total;
                        } else {
                            return self
                                .weights_of(
                                    &reference.iter().chain(deck).cloned().collect::<Vec<_>>(),
                                    None,
                                    0,
                                )
                                .base
                                - self.weights_of(&reference, None, 0).base;
                        }
                    } else {
                        distinct.push(id);
                    }
                }
                distinct.sort_unstable();
                let key = (plain, distinct, support);
                if let Some(&gain) = self.regional_gains.get(&key) {
                    return gain;
                }

                let combined: Vec<_> = reference.iter().chain(deck).cloned().collect();
                let gain = self.weights_of(&combined, None, 0).base
                    - self.weights_of(&reference, None, 0).base;
                if self.regional_gains.len() >= 8192 {
                    if let Some(old) = self.regional_gain_order.pop_front() {
                        self.regional_gains.remove(&old);
                    }
                }
                self.regional_gain_order.push_back(key.clone());
                self.regional_gains.insert(key, gain);
                return gain;
            }
        }
        let combined: Vec<_> = reference.iter().chain(deck).cloned().collect();
        self.weights_of(&combined, within, tag).base - self.weights_of(reference, within, tag).base
    }
    fn event_weights(&self, slot: &Slot, w: &Weights) -> [f64; 5] {
        let mut per_event = [0.0; 5];
        for (event, &(_, time)) in self.chart.full.events.iter().enumerate().take(5) {
            for effect in &slot.effects {
                if effect.gate.blocks(self.master.live.life_base) {
                    continue;
                }
                let start = self.chart.times.partition_point(|&t| t < time);
                let end = self
                    .chart
                    .times
                    .partition_point(|&t| t < time + window_ms(effect.seconds, slot.extension_ms));
                let factor = effect.delta as f64 / 100000.0;
                if effect.kind == 2000 {
                    per_event[event] += factor * (w.general[end] - w.general[start]);
                } else {
                    for &j in &effect.judgements {
                        if (4..=6).contains(&j) {
                            per_event[event] +=
                                factor * (w.judgements[j - 4][end] - w.judgements[j - 4][start]);
                        }
                    }
                }
            }
        }
        per_event
    }
    fn live_weight(&self, slot: &Slot, w: &Weights, pick: Criterion) -> f64 {
        let weights = self.event_weights(slot, w);
        match pick {
            Criterion::Max => weights.into_iter().fold(f64::NEG_INFINITY, f64::max),
            Criterion::Min => weights.into_iter().fold(f64::INFINITY, f64::min),
            Criterion::Mean => weights.iter().fold(0.0, |sum, v| sum + v) / 5.0,
        }
    }
    fn low_of(&mut self, m: usize, s: i32) -> f64 {
        let mut support = 0.0;
        if let Some(p) = self.support(m, s) {
            let key = json!([p.gekisou_support_skills, p.band_id]).to_string();
            support = if let Some(&v) = self.support_low.get(&key) {
                v
            } else {
                let value = self.gain(&[p], &[], None, 0).max(0.0);
                self.support_low.insert(key, value);
                value
            };
        }
        let base = if let Some(base) = &self.base {
            Arc::clone(base)
        } else {
            let base = self.weights_of(&[], None, 0);
            self.base = Some(Arc::clone(&base));
            base
        };
        self.live_weight(
            self.slot(m, s),
            &base,
            if self.criterion == Criterion::Min {
                Criterion::Min
            } else {
                Criterion::Mean
            },
        ) + support
    }
    fn chain(&mut self, p: &Performer) -> bool {
        changes_context(self.master, p)
    }
    fn rush_factor(&mut self, p: &Performer) -> Option<f64> {
        if !self.seeds.is_empty() {
            return None;
        }
        let key = shape(p);
        *self.rush.entry(key).or_insert_with(|| {
            gekiso::rush_factor_for_music(self.master, p, self.chart.music_type)
                .unwrap_or_else(|e| panic!("{e}"))
        })
    }
    fn actor_of(&self, member: usize, snap: i32) -> Performer {
        let mut p = self.gk_members[member].clone();
        if let Some(support) = self.support(member, snap) {
            p.band_id = support.band_id;
            p.gekisou_support_skills = support.gekisou_support_skills;
        }
        p
    }
    fn deck_of(&self, team: &Team) -> Vec<Performer> {
        team.members
            .iter()
            .enumerate()
            .map(|(slot, &m)| self.actor_of(m, team.snaps[slot]))
            .collect()
    }
    fn complete_score_tie(&mut self, hit: &mut Hit) -> Result<(), String> {
        if hit.detail["pendingScores"] == true {
            let exact = self.exact(&hit.team, hit.power as i32, true)?;
            self.exact_calls += 1;
            hit.detail = exact.detail;
        }
        Ok(())
    }
    fn complete_hits(&mut self, hits: &[Hit], k: usize) -> Result<Vec<Hit>, String> {
        let mut tied = hits.to_vec();
        let primary = ranked(&tied, k);
        if self.criterion != Criterion::Mean {
            let threshold = primary.last().map_or(f64::NEG_INFINITY, |hit| hit.key);
            for i in 0..tied.len() {
                if tied[i].key >= threshold
                    && tied
                        .iter()
                        .enumerate()
                        .any(|(j, h)| j != i && h.key == tied[i].key)
                {
                    self.complete_score_tie(&mut tied[i])?;
                }
            }
        }
        let mut result = ranked(&tied, k);
        for (index, hit) in result.iter_mut().enumerate() {
            if hit.detail["pendingScores"] == true
                && self.defer_scores
                && !(self.seed_preview && index == 0)
            {
                hit.detail["comboBonus"] = if self
                    .chart
                    .setup
                    .missions
                    .iter()
                    .any(|mission| *mission == 1 || *mission == 4)
                {
                    json!(
                        1 + hit
                            .team
                            .members
                            .iter()
                            .map(|&member| self.opening_bonuses[member])
                            .sum::<i32>()
                    )
                } else {
                    Value::Null
                };
                continue;
            }
            if hit.detail["pendingScores"] == true {
                let exact = self.exact(&hit.team, hit.power as i32, true)?;
                self.exact_calls += 1;
                hit.key = exact.key;
                hit.detail = exact.detail;
            }
        }
        Ok(result)
    }
    fn unsettled_scores(&mut self, team: &Team, power: i32) -> Result<Arc<live::Scores>, String> {
        let key = (team.members.clone(), team.snaps.clone(), power);
        if let Some(scores) = self.unsettled.get(&key) {
            return Ok(Arc::clone(scores));
        }
        let deck = self.deck_of(team);
        let ctx = self.context_of(&deck, None, 0).0;
        let slots: Vec<_> = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &m)| self.slot(m, team.snaps[slot]).clone())
            .collect();
        let scores = Arc::new(gekiso::score_orders(
            self.master,
            self.chart,
            &ctx,
            power,
            &slots,
            false,
        )?);
        if self.unsettled.len() >= 2048 {
            self.unsettled.clear();
        }
        self.unsettled.insert(key, Arc::clone(&scores));
        Ok(scores)
    }
    fn original_deck(&self, team: &Team) -> Vec<Performer> {
        team.members
            .iter()
            .zip(&team.snaps)
            .map(|(&member, &snap)| {
                Performer::from_member(
                    self.master,
                    &self.members[member],
                    (snap >= 0).then(|| &self.snaps[snap as usize]),
                )
            })
            .collect()
    }
    fn complete_orders(
        &self,
        team: &Team,
        power: i32,
        mut rows: PartialOrderScores,
    ) -> Result<Exact, String> {
        let deck = self.original_deck(team);
        let missing: Vec<_> = rows
            .iter()
            .enumerate()
            .filter_map(|(order, row)| row.is_none().then_some(order))
            .collect();
        // Amortize replay-tree and native worker setup over larger completions.
        if missing.len() >= 30 {
            let values =
                gekiso::replay_order_subset(self.master, self.chart, &deck, power, &missing)?;
            for (order, value) in missing.into_iter().zip(values) {
                rows[order] = Some(value);
            }
        } else {
            for order in missing {
                rows[order] = Some(gekiso::replay_order(
                    self.master,
                    self.chart,
                    &deck,
                    power,
                    order,
                )?);
            }
        }
        let scores = gekiso::ReplayedScores {
            scores: live::summarize(power, rows.iter().map(|row| row.unwrap().0).collect()),
            range_scores: std::array::from_fn(|range| {
                rows.iter().map(|row| row.unwrap().1[range]).sum::<f64>() / 120.0
            }),
        };
        Ok(Exact {
            key: self.criterion.key(&scores.scores),
            detail: json!({"scores":scores,"seeds":1,"comboBonus":if self.chart.setup.missions.iter().any(|m| *m==1 || *m==4) {Some(1+team.members.iter().map(|&i|self.opening_bonuses[i]).sum::<i32>())} else {None}}),
        })
    }
    fn deterministic_upper(&mut self, team: &Team) -> Option<Arc<crate::just_bound::Upper>> {
        if self.chart.luck || !self.healthy || !self.owner_local || self.just_context.is_none() {
            return None;
        }
        let deck: Vec<_> = team
            .members
            .iter()
            .zip(&team.snaps)
            .map(|(&member, &snap)| self.bound_actor_of(member, snap))
            .collect();
        self.just_bounds
            .upper(self.master, self.chart, &deck)
            .ok()
            .flatten()
    }
    fn has_converter(&mut self, team: &Team) -> bool {
        team.members
            .iter()
            .zip(&team.snaps)
            .any(|(&member, &snap)| {
                self.bound_actor_of(member, snap);
                self.bound_converters[self.slot_index(member, snap)]
            })
    }
    fn max_order_bounds(&mut self, team: &Team, power: i32) -> Option<Vec<f64>> {
        if !self.healthy || !self.owner_local {
            return None;
        }
        let compiled = self.deterministic_upper(team);
        let weights = if let Some(upper) = &compiled {
            Arc::clone(&upper.weights)
        } else {
            if self.has_converter(team) {
                return None;
            }
            let deck = self.deck_of(team);
            self.weights_of(&deck, None, 0)
        };
        let slots: Vec<_> = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &member)| Arc::new(self.slot(member, team.snaps[slot]).clone()))
            .collect();
        if slots
            .iter()
            .any(|slot| slot.effects.iter().any(|effect| effect.delta < 0))
        {
            return None;
        }
        let values = crate::rewards::order_score_coefficients_with_base(
            self.master,
            self.chart,
            &weights,
            &slots,
            compiled.as_ref().map_or(weights.base, |upper| upper.base),
        )?;
        let values: Vec<_> = values
            .into_iter()
            .map(|value| {
                ((value * power as f64).next_up() + (self.chart.notes.len() + 4) as f64).next_up()
            })
            .collect();
        values
            .iter()
            .all(|value| value.is_finite())
            .then_some(values)
    }
    fn search_exact(&mut self, team: &Team, power: i32, settle: bool) -> Result<Exact, String> {
        if !self.chart.luck && self.criterion == Criterion::Max && self.seeds.is_empty() && settle {
            if let Some(bounds) = self.max_order_bounds(team, power) {
                let deck = self.original_deck(team);
                let mut order: Vec<_> = (0..120).collect();
                order.sort_unstable_by(|&a, &b| bounds[b].total_cmp(&bounds[a]).then(a.cmp(&b)));
                let mut scores = vec![None; 120];
                let mut best = f64::NEG_INFINITY;
                let mut count = 0;
                for index in order {
                    if bounds[index] <= best {
                        break;
                    }
                    let result =
                        gekiso::replay_order(self.master, self.chart, &deck, power, index)?;
                    best = best.max(result.0);
                    scores[index] = Some(result);
                    count += 1;
                }
                if count == 120 {
                    return self.complete_orders(team, power, scores);
                }
                if self.partial_orders.len() >= 128 {
                    self.partial_orders.clear();
                }
                self.partial_orders
                    .insert((team.members.clone(), team.snaps.clone(), power), scores);
                return Ok(Exact {
                    key: best,
                    detail: json!({"pendingScores":true}),
                });
            }
        }
        if self.criterion == Criterion::Mean
            || !self.chart.luck
            || !self.seeds.is_empty()
            || !settle
        {
            return self.exact(team, power, settle);
        }
        let upper = self.unsettled_scores(team, power)?;
        let deck = self.deck_of(team);
        let ctx = self.context_of(&deck, None, 0).0;
        let slots: Vec<_> = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &m)| self.slot(m, team.snaps[slot]).clone())
            .collect();
        let key = gekiso::settle_extreme(
            self.master,
            self.chart,
            &ctx,
            power,
            &slots,
            &upper,
            self.criterion == Criterion::Max,
        )?;
        Ok(Exact {
            key,
            detail: json!({"pendingScores":true}),
        })
    }
    fn deterministic_coefficient(&mut self, team: &Team) -> Option<f64> {
        if self.chart.luck || !self.healthy || !self.owner_local || self.just_context.is_none() {
            return None;
        }
        let mut estimate_key =
            std::array::from_fn(|slot| self.bound_shape_id(team.members[slot], team.snaps[slot]));
        let mut conversion_order = [u32::MAX; 5];
        let mut count = 0;
        for (slot, &shape) in estimate_key.iter().enumerate() {
            let index = self.slot_index(team.members[slot], team.snaps[slot]);
            if self.bound_converters[index] {
                conversion_order[count] = shape;
                count += 1;
            }
        }
        // The additive score bound is unordered; judgement conversion is not.
        estimate_key.sort_unstable();
        let estimate_key = (estimate_key, conversion_order);
        let coefficient = if let Some(&value) = self.order_estimates.get(&estimate_key) {
            Some(value)
        } else {
            let upper = self.deterministic_upper(team);
            upper.and_then(|upper| {
                let slots: Vec<_> = team
                    .members
                    .iter()
                    .enumerate()
                    .map(|(slot, &m)| Arc::new(self.slot(m, team.snaps[slot]).clone()))
                    .collect();
                if slots.iter().any(|s| s.effects.iter().any(|e| e.delta < 0)) {
                    return None;
                }
                let weights = &upper.weights;
                let scores = crate::rewards::order_score_coefficients_with_base(
                    self.master,
                    self.chart,
                    weights,
                    &slots,
                    upper.base,
                )?;
                let value = match self.criterion {
                    Criterion::Mean => (scores.iter().fold(0.0_f64, |sum, &v| (sum + v).next_up())
                        / 120.0)
                        .next_up(),
                    Criterion::Min => scores.iter().copied().fold(f64::INFINITY, f64::min),
                    Criterion::Max => scores.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                };
                if self.order_estimates.len() >= 65536 {
                    self.order_estimates.clear();
                }
                self.order_estimates.insert(estimate_key, value);
                Some(value)
            })
        };
        coefficient
    }
    fn deterministic_order_bound(&mut self, team: &Team, power: f64, limit: f64) -> Option<f64> {
        if self.chart.luck || !self.healthy {
            return None;
        }
        let key = std::array::from_fn(|slot| self.shape_id(team.members[slot], team.snaps[slot]));
        if let Some(coefficient) = self.deterministic_coefficient(team) {
            let upper = (power * coefficient).next_up() + (self.chart.notes.len() + 4) as f64;
            if upper <= limit {
                return Some(upper);
            }
        }
        if self.has_converter(team) {
            return None;
        }
        let coefficient = if let Some(&value) = self.order_bounds.get(&key) {
            value
        } else {
            let deck = self.deck_of(team);
            let weights = self.weights_of(&deck, None, 0);
            let slots: Vec<_> = team
                .members
                .iter()
                .enumerate()
                .map(|(slot, &m)| Arc::new(self.slot(m, team.snaps[slot]).clone()))
                .collect();
            if slots
                .iter()
                .any(|slot| slot.effects.iter().any(|e| e.delta < 0))
            {
                return None;
            }
            let scores = crate::rewards::order_score_coefficients(
                self.master,
                self.chart,
                &weights,
                &slots,
            )?;
            let value = match self.criterion {
                Criterion::Mean => {
                    (scores.iter().fold(0.0_f64, |sum, &v| (sum + v).next_up()) / 120.0).next_up()
                }
                Criterion::Min => scores.iter().copied().fold(f64::INFINITY, f64::min),
                Criterion::Max => scores.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            };
            if self.order_bounds.len() >= 4096 {
                self.order_bounds.clear();
            }
            self.order_bounds.insert(key, value);
            value
        };
        Some((power * coefficient).next_up() + (self.chart.notes.len() + 4) as f64)
    }
    pub fn exact(&mut self, team: &Team, power: i32, settle: bool) -> Result<Exact, String> {
        if !self.chart.luck && self.seeds.is_empty() {
            if let Some(scores) =
                self.partial_orders
                    .remove(&(team.members.clone(), team.snaps.clone(), power))
            {
                return self.complete_orders(team, power, scores);
            }
        }
        if !self.chart.luck || gekiso::uses_full_replay(self.master, self.chart, &self.seeds) {
            let deck: Vec<_> = team
                .members
                .iter()
                .zip(&team.snaps)
                .map(|(&m, &s)| {
                    Performer::from_member(
                        self.master,
                        &self.members[m],
                        (s >= 0).then(|| &self.snaps[s as usize]),
                    )
                })
                .collect();
            let scores = gekiso::replay_orders(self.master, self.chart, &deck, &self.seeds, power)?;
            return Ok(Exact {
                key: self.criterion.key(&scores.scores),
                detail: json!({"scores":scores,"seeds":self.seeds.len().max(1),"comboBonus":if self.chart.setup.missions.iter().any(|m| *m==1 || *m==4) {Some(1+team.members.iter().map(|&i|self.opening_bonuses[i]).sum::<i32>())} else {None}}),
            });
        }
        let ctx = if self.seeds.is_empty() {
            let deck = self.deck_of(team);
            self.context_of(&deck, None, 0).0
        } else {
            let deck = team
                .members
                .iter()
                .zip(&team.snaps)
                .map(|(&member, &snap)| {
                    Performer::from_member(
                        self.master,
                        &self.members[member],
                        (snap >= 0).then(|| &self.snaps[snap as usize]),
                    )
                })
                .collect::<Vec<_>>();
            Arc::new(gekiso::context(
                self.master,
                self.chart,
                &deck,
                &self.seeds,
            )?)
        };
        let skills: Vec<_> = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &m)| self.slot(m, team.snaps[slot]).clone())
            .collect();
        let scores = gekiso::score_orders(self.master, self.chart, &ctx, power, &skills, settle)?;
        Ok(Exact {
            key: self.criterion.key(&scores),
            detail: json!({"scores":scores,"seeds":ctx.seeds,"comboBonus":if self.chart.setup.missions.iter().any(|m| *m == 1 || *m == 4) { Some(1 + team.members.iter().map(|&i| self.opening_bonuses[i]).sum::<i32>()) } else { None }}),
        })
    }
}
impl Kernel<'_> {
    pub fn calibrate(&mut self, deadline: f64, now: &dyn Fn() -> f64) -> bool {
        if self.calibrated || !self.chart.luck {
            return true;
        }
        let mut values = Vec::new();
        for i in 0..self.members.len() {
            if now() >= deadline {
                return false;
            }
            let p = self.gk_members[i].clone();
            let value = if p.gekisou_skill.is_some() && self.bonuses[i] == 0 {
                self.gain(&[p], &[], None, 0)
            } else {
                0.0
            };
            self.member_gain[i] = value.max(0.0);
            values.push((i, value));
        }
        let mut member_luck: Vec<_> = values.into_iter().filter(|v| v.1 > 0.0).collect();
        member_luck.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut characters = HashSet::default();
        for &(i, _) in &member_luck {
            let character = self.members[i].card.character_id;
            if characters.contains(&character) || characters.len() == 5 {
                continue;
            }
            characters.insert(character);
            self.luck_deck.push(self.gk_members[i].clone());
        }
        let mut seen = HashSet::default();
        let mut supports = Vec::new();
        for i in 0..self.snaps.len() {
            if now() >= deadline {
                return false;
            }
            let snap = &self.snaps[i];
            let skills: Vec<_> = snap
                .card
                .gekiso_support_skills
                .into_iter()
                .zip(snap.gekiso_skill_levels)
                .filter(|pair| pair.0 != 0)
                .collect();
            if skills.is_empty() || !seen.insert(skills.clone()) {
                continue;
            }
            let p = Performer {
                band_id: favoured_band(self.master, &skills),
                mission_only: Some(1),
                gekisou_support_skills: skills,
                ..Performer::default()
            };
            let value = self.gain(std::slice::from_ref(&p), &[], None, 0);
            supports.push((p, value));
        }
        let mut ranked = supports.clone();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        self.luck_deck
            .extend(ranked.into_iter().take(5).map(|item| item.0));
        self.luck_members.extend(
            member_luck
                .into_iter()
                .map(|item| self.gk_members[item.0].clone()),
        );
        for (p, _) in supports {
            if !self.chain(&p) {
                continue;
            }
            self.luck_supports.push(p.clone());
            if p.band_id != 0 && !favoured_dominates(self.master, &p.gekisou_support_skills) {
                let mut other = p.clone();
                other.band_id = if p.band_id == 1 { 2 } else { 1 };
                self.luck_supports.push(other);
            }
        }
        self.calibrated = true;
        true
    }
    fn level(&mut self, level: i32) -> Arc<Level> {
        if let Some(data) = self.levels.get(&level) {
            return Arc::clone(data);
        }
        let mut within = self.master.clone();
        let mut deck = self.luck_deck.clone();
        let mut tag = 0;
        if level > 0 {
            if let Some(template) = &self.template {
                let mut row = template.clone();
                row.id = 2_000_000_000 + level;
                row.level = 1;
                row.value = level;
                let mut skills = (*within.gekiso_skills).clone();
                skills.insert(
                    -1,
                    GekisoSkill {
                        mission_type: 1,
                        effects: vec![row],
                    },
                );
                within.gekiso_skills = Arc::new(skills);
                tag = level;
                deck.push(Performer {
                    gekisou_skill: Some((-1, 1)),
                    gekisou_mission_type: 1,
                    ..Performer::default()
                });
            }
        }
        let weights = self.weights_of(&deck, Some(&within), tag);
        let data = Arc::new(Level {
            within,
            deck,
            weights,
        });
        self.levels.insert(level, Arc::clone(&data));
        data
    }
    fn support_high(&mut self, m: usize, s: i32, level: i32) -> f64 {
        let Some(p) = self.support(m, s) else {
            return 0.0;
        };
        let at = self
            .grid
            .iter()
            .find(|&&g| g >= level)
            .copied()
            .unwrap_or(self.maximum_level);
        let key = (json!([p.gekisou_support_skills, p.band_id]).to_string(), at);
        if let Some(&value) = self.support_high.get(&key) {
            return value;
        }
        let data = self.level(at);
        let value = self.gain(&[p], &data.deck, Some(&data.within), at).max(0.0);
        self.support_high.insert(key, value);
        value
    }
    fn strongest(&mut self, variants: Vec<Performer>) -> Vec<Performer> {
        if variants.len() < 2 {
            return variants;
        }
        let first = &variants[0].gekisou_support_skills;
        if variants.iter().any(|p| p.gekisou_support_skills != *first) {
            return variants;
        }
        let (favored, dominates) = *self.favoured.entry(first.clone()).or_insert_with(|| {
            (
                favoured_band(self.master, first),
                favoured_dominates(self.master, first),
            )
        });
        if let Some(p) = variants.iter().find(|p| p.band_id == favored) {
            if dominates {
                return vec![p.clone()];
            }
        }
        variants
    }
    fn partition_weights(
        &mut self,
        deck: &[Performer],
        within: Option<&Master>,
        tag: i32,
    ) -> Option<(Arc<Weights>, Vec<Performer>)> {
        if !self.seeds.is_empty() || self.chart.setup.missions.contains(&4) {
            return None;
        }
        let master = within.unwrap_or(self.master);
        for p in deck {
            let rows = p
                .gekisou_skill
                .into_iter()
                .map(|(id, level)| (false, id, level))
                .chain(
                    p.gekisou_support_skills
                        .iter()
                        .map(|&(id, level)| (true, id, level)),
                );
            for (support, id, level) in rows {
                let skill = if support {
                    &master.gekiso_support_skills[&id]
                } else {
                    &master.gekiso_skills[&id]
                };
                if skill.mission_type == 2
                    && skill.effects.iter().filter(|r| r.level == level).any(|r| {
                        ![11000, 11001, 11002, 11003, 11004, 11005, 3004, 4004].contains(&r.kind)
                    })
                {
                    return None;
                }
            }
        }
        let probe = self.gain_probe.clone()?;
        let native: Vec<_> = deck
            .iter()
            .filter(|p| !p.gekisou_skill.is_some_and(|(id, _)| id == -1))
            .cloned()
            .collect();
        let mut stripped = self.regional_gain_reference(&probe, &native, false)?;
        if tag > 0 {
            stripped.extend(
                deck.iter()
                    .filter(|p| p.gekisou_skill.is_some_and(|(id, _)| id == -1))
                    .cloned(),
            );
        }

        for p in &stripped {
            let rows = p
                .gekisou_skill
                .into_iter()
                .map(|(id, level)| (false, id, level))
                .chain(
                    p.gekisou_support_skills
                        .iter()
                        .map(|&(id, level)| (true, id, level)),
                );
            for (support, id, level) in rows {
                let skill = if support {
                    &master.gekiso_support_skills[&id]
                } else {
                    &master.gekiso_skills[&id]
                };
                if skill.effects.iter().filter(|r| r.level == level).any(|r| {
                    [
                        r.trigger_group,
                        r.condition_group,
                        r.release_group,
                        r.reset_group,
                    ]
                    .iter()
                    .any(|g| {
                        self.master
                            .condition_sets
                            .get(g)
                            .into_iter()
                            .flatten()
                            .flatten()
                            .any(|id| {
                                self.master
                                    .conditions
                                    .get(id)
                                    .is_some_and(|c| [7000, 7021, 4011].contains(&c.kind))
                            })
                    })
                }) {
                    return None;
                }
            }
        }
        let luck: Vec<_> = deck
            .iter()
            .filter_map(|p| {
                let mut p = p.clone();
                if p.gekisou_skill
                    .is_some_and(|(id, _)| master.gekiso_skills[&id].mission_type != 2)
                {
                    p.gekisou_skill = None;
                    p.mission_only = Some(1);
                }
                p.gekisou_support_skills
                    .retain(|(id, _)| master.gekiso_support_skills[id].mission_type == 2);
                (p.gekisou_skill.is_some() || !p.gekisou_support_skills.is_empty()).then_some(p)
            })
            .collect();
        let (_, (_, mut key)) = self.deck_key(&stripped, tag);
        key.insert(0, tag as u32);
        key.push(u32::MAX);
        key.extend(self.deck_key(&luck, 0).1 .1);
        if let Some(weights) = self.luck_envelopes.get(&key) {
            return Some((Arc::clone(weights), stripped));
        }
        let other = self.weights_of(&stripped, within, tag);
        let lottery = self.weights_of(&luck, None, 0);
        let empty = self.weights_of(&[], None, 0);
        let combine = |a: f64, b: f64, empty: f64| ((a + b).next_up() - empty).next_up();
        let general = other
            .general
            .iter()
            .zip(&lottery.general)
            .zip(&empty.general)
            .map(|((&a, &b), &e)| combine(a, b, e))
            .collect();
        let judgements = std::array::from_fn(|j| {
            other.judgements[j]
                .iter()
                .zip(&lottery.judgements[j])
                .zip(&empty.judgements[j])
                .map(|((&a, &b), &e)| combine(a, b, e))
                .collect()
        });
        let weights = Arc::new(Weights {
            base: combine(other.base, lottery.base, empty.base),
            rush: lottery.rush,
            general,
            judgements,
        });
        if self.luck_envelopes.len() >= 256 {
            self.luck_envelopes.clear();
        }
        self.luck_envelopes.insert(key, Arc::clone(&weights));
        Some((weights, stripped))
    }
    fn choice_base_id(&mut self, key: &[u32]) -> u64 {
        if let Some(&id) = self.choice_base_ids.get(key) {
            return id;
        }
        let bytes = 128 + key.len() * 2 * std::mem::size_of::<u32>();
        while self.choice_base_bytes + bytes > 8 * 1024 * 1024 {
            let Some((old, size)) = self.choice_base_order.pop_front() else {
                break;
            };
            self.choice_base_ids.remove(&old);
            self.choice_base_bytes -= size;
        }
        self.next_choice_base += 1;
        let id = self.next_choice_base;
        self.choice_base_ids.insert(key.to_vec(), id);
        self.choice_base_order.push_back((key.to_vec(), bytes));
        self.choice_base_bytes += bytes;
        id
    }
    fn luck_base(
        &mut self,
        members: &[Performer],
        chained: &[&ChainActor],
        member_ids: &[u32],
    ) -> Arc<std::sync::Mutex<Base>> {
        // This is an unordered cache identity, not the simulation's performer
        // order. Keep context_of's canonical JSON order for numeric parity.
        let mut key = std::mem::take(&mut self.base_key);
        key.clear();
        key.extend_from_slice(member_ids);
        key.extend(chained.iter().map(|p| p.1));
        key.sort_unstable();
        let canonical = if self.healthy && self.owner_local {
            self.luck_aliases.get(&key)
        } else {
            Some(&key)
        };
        if let Some((base, used)) = canonical.and_then(|key| self.luck_bases.get_mut(key)) {
            *used = true;
            let base = Arc::clone(base);
            self.base_key = key;
            return base;
        }
        let mut deck: Vec<_> = members
            .iter()
            .cloned()
            .chain(chained.iter().map(|p| p.0.clone()))
            .collect();
        if self.healthy && self.owner_local {
            let canonical = self.deck_key(&deck, 0).1 .1;
            if self
                .luck_aliases
                .insert(key.clone(), canonical.clone())
                .is_none()
            {
                self.luck_alias_order.push_back(key.clone());
            }
            while self.luck_aliases.len() > 8192 {
                let old = self.luck_alias_order.pop_front().unwrap();
                self.luck_aliases.remove(&old);
            }
            if let Some((base, used)) = self.luck_bases.get_mut(&canonical) {
                *used = true;
                let base = Arc::clone(base);
                self.base_key = key;
                return base;
            }
            key = canonical;
        }
        let weights = if let Some((weights, stripped)) = self.partition_weights(&deck, None, 0) {
            deck = stripped;
            weights
        } else {
            self.weights_of(&deck, None, 0)
        };
        // The existing Base key preserves normalized skill levels, owner traits and replay order.
        // Chart, accuracy and seeds stay fixed within this Kernel.
        let choice_id = self.choice_base_id(&key);
        let base = Arc::new(std::sync::Mutex::new(Base {
            choice_id,
            pricing_lambda: None,
            weights,
            deck,
            gains: HashMap::default(),
            numeric_gains: None,
            slot_gains: HashMap::default(),
            lives: HashMap::default(),
            events: HashMap::default(),
        }));
        while self.luck_bases.len() >= 2048 {
            let old = self.luck_base_order.pop_front().unwrap();
            let (_, used) = self.luck_bases.get_mut(&old).unwrap();
            if std::mem::replace(used, false) {
                self.luck_base_order.push_back(old);
            } else {
                self.luck_bases.remove(&old);
            }
        }
        self.luck_base_order.push_back(key.clone());
        self.luck_bases
            .insert(key.clone(), (Arc::clone(&base), false));
        self.base_key = key;
        base
    }
    fn numeric_gain_profile(
        &mut self,
        reference: &[Performer],
    ) -> Arc<std::sync::Mutex<HashMap<u32, f64>>> {
        if let Some(profile) = self
            .numeric_gain_profiles
            .get(reference)
            .and_then(std::sync::Weak::upgrade)
        {
            profile
        } else {
            let profile = Arc::new(std::sync::Mutex::new(HashMap::default()));
            if self
                .numeric_gain_profiles
                .insert(reference.to_vec(), Arc::downgrade(&profile))
                .is_none()
            {
                self.numeric_gain_order.push_back(reference.to_vec());
            }
            while self.numeric_gain_profiles.len() > 2048 {
                let old = self.numeric_gain_order.pop_front().unwrap();
                self.numeric_gain_profiles.remove(&old);
            }
            profile
        }
    }
    fn luck_gain(&mut self, base: &mut Base, m: usize, s: i32) -> f64 {
        if s < 0 || self.support_chain[m * self.snaps.len() + s as usize] {
            return 0.0;
        }
        let slot = self.slot_index(m, s);
        if let Some(&gain) = base.slot_gains.get(&slot) {
            return gain;
        }
        let id = if let Some(id) = self.luck_gain_slots[slot] {
            id
        } else {
            let Some(actor) = self.support(m, s) else {
                return 0.0;
            };
            let id = if let Some(&id) = self.luck_gain_ids.get(&actor) {
                id
            } else {
                let id = self.luck_gain_actors.len() as u32;
                let factor = self.rush_factor(&actor);
                self.luck_gain_actors.push(actor.clone());
                self.luck_gain_factors.push(factor);
                self.luck_gain_ids.insert(actor, id);
                id
            };
            self.luck_gain_slots[slot] = Some(id);
            id
        };
        let gain = if let Some(factor) = self.luck_gain_factors[id as usize] {
            factor * base.weights.rush
        } else {
            if base.numeric_gains.is_none() {
                base.numeric_gains = Some(self.numeric_gain_profile(&base.deck));
            }
            let mut gains = base
                .numeric_gains
                .as_ref()
                .unwrap()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(&gain) = gains.get(&id) {
                gain
            } else {
                let actor = self.luck_gain_actors[id as usize].clone();
                let gain = self.gain(std::slice::from_ref(&actor), &base.deck, None, 0);
                gains.insert(id, gain);
                gain
            }
        };
        base.slot_gains.insert(slot, gain);
        gain
    }
    fn luck_slot_locked(&mut self, base: &mut Base, m: usize, s: i32, pick: Criterion) -> f64 {
        let index = self.slot_index(m, s) * 3
            + match pick {
                Criterion::Mean => 0,
                Criterion::Max => 1,
                Criterion::Min => 2,
            };
        if let Some(&value) = base.lives.get(&index) {
            return value;
        }
        let value =
            self.live_weight(self.slot(m, s), &base.weights, pick) + self.luck_gain(base, m, s);
        base.lives.insert(index, value);
        value
    }
    fn luck_events(&mut self, base: &mut Base, m: usize, s: i32) -> [f64; 5] {
        let index = self.slot_index(m, s);
        if let Some(&row) = base.events.get(&index) {
            return row;
        }
        let mut row = self.event_weights(self.slot(m, s), &base.weights);
        if s >= 0
            && !self.support_chain[m * self.snaps.len() + s as usize]
            && self.support[m * self.snaps.len() + s as usize].is_some()
        {
            let gain = self.luck_gain(base, m, s);
            row = row.map(|value| (value + gain).next_up());
        }
        base.events.insert(index, row);
        row
    }
    fn refine(&mut self, team: &Team, power: f64) -> f64 {
        let slots: [usize; 5] =
            std::array::from_fn(|slot| self.slot_index(team.members[slot], team.snaps[slot]));
        if let Some(&total) = self.refined.get(&slots) {
            return power * total / self.chart.full.converted as f64;
        }
        let mut members: [usize; 5] = std::array::from_fn(|slot| team.members[slot]);
        members.sort_unstable();
        let mut ids = [usize::MAX; 5];
        let mut count = 0;
        for (slot, &m) in team.members.iter().enumerate() {
            let snap = team.snaps[slot];
            if snap >= 0 && self.support_chain[m * self.snaps.len() + snap as usize] {
                ids[count] = slots[slot];
                count += 1;
            }
        }
        ids[..count].sort_unstable();
        let key = (members, ids);
        let base = if let Some(base) = self.refine_bases.get(&key) {
            Arc::clone(base)
        } else {
            if self.refine_bases.len() >= 2048 {
                self.refine_bases.clear();
            }
            let deck: Vec<_> = team
                .members
                .iter()
                .map(|&i| self.gk_members[i].clone())
                .chain(team.members.iter().enumerate().filter_map(|(slot, &m)| {
                    let snap = team.snaps[slot];
                    (snap >= 0 && self.support_chain[m * self.snaps.len() + snap as usize])
                        .then(|| self.support(m, snap).unwrap())
                }))
                .collect();
            let weights = self.weights_of(&deck, None, 0);
            self.next_choice_base += 1;
            let base = Arc::new(std::sync::Mutex::new(Base {
                choice_id: self.next_choice_base,
                pricing_lambda: None,
                weights,
                deck,
                gains: HashMap::default(),
                numeric_gains: None,
                slot_gains: HashMap::default(),
                lives: HashMap::default(),
                events: HashMap::default(),
            }));
            self.refine_bases.insert(key, Arc::clone(&base));
            base
        };
        let mut base = base.lock().unwrap_or_else(|e| e.into_inner());
        let mut total = base.weights.base;
        let mut event_rows = [[0.0; 5]; 5];
        for (slot, &m) in team.members.iter().enumerate() {
            let snap = team.snaps[slot];
            if snap >= 0
                && self.support[m * self.snaps.len() + snap as usize].is_some()
                && !self.support_chain[m * self.snaps.len() + snap as usize]
            {
                let index = slots[slot];
                let gain = if let Some(&gain) = base.slot_gains.get(&index) {
                    gain
                } else {
                    let p = self.support(m, snap).unwrap();
                    let key = shape(&p);
                    let gain = if let Some(&value) = base.gains.get(&key) {
                        value
                    } else {
                        let factor = if self.chart.luck {
                            self.rush_factor(&p)
                        } else {
                            None
                        };
                        let value = if let Some(factor) = factor {
                            factor * base.weights.rush
                        } else {
                            self.gain(std::slice::from_ref(&p), &base.deck, None, 0)
                        };
                        base.gains.insert(key, value);
                        value
                    };
                    base.slot_gains.insert(index, gain);
                    gain
                };
                total += gain;
            }
            let index = self.slot_index(m, snap);
            if self.criterion != Criterion::Mean {
                let row = if let Some(&row) = base.events.get(&index) {
                    row
                } else {
                    let row = self.event_weights(self.slot(m, snap), &base.weights);
                    base.events.insert(index, row);
                    row
                };
                event_rows[slot] = row;
            } else {
                let live = if let Some(&value) = base.lives.get(&index) {
                    value
                } else {
                    let value = self.live_weight(self.slot(m, snap), &base.weights, self.criterion);
                    base.lives.insert(index, value);
                    value
                };
                total += live;
            }
        }
        if self.criterion != Criterion::Mean {
            total += if self.criterion == Criterion::Max {
                best_order(&event_rows)
            } else {
                -best_order(&event_rows.map(|row| row.map(|value| -value)))
            };
        }
        if self.refined.len() >= 65536 {
            if let Some(old) = self.refined_order.pop_front() {
                self.refined.remove(&old);
            }
        }
        self.refined.insert(slots, total);
        self.refined_order.push_back(slots);
        power * total / self.chart.full.converted as f64
    }
}
type ChainActor = (Performer, u32);
struct ChainChoice {
    snap: i32,
    owned: [Option<ChainActor>; 5],
    future: [Vec<ChainActor>; 6],
}
struct SnapUnion {
    options: [Vec<i32>; 5],
    seen: Vec<u8>,
    plain: [bool; 5],
}
impl SnapUnion {
    fn new(snaps: usize) -> Self {
        Self {
            options: std::array::from_fn(|_| Vec::new()),
            seen: vec![0; snaps + 1],
            plain: [false; 5],
        }
    }
    fn add(&mut self, slot: usize, snap: i32) {
        let index = (snap + 1) as usize;
        let bit = 1 << slot;
        if self.seen[index] & bit == 0 {
            self.seen[index] |= bit;
            self.options[slot].push(snap);
        }
    }
    fn extend(&mut self, slot: usize, snap: i32, plain: &[i32]) {
        if snap >= 0 {
            self.add(slot, snap);
        } else if !self.plain[slot] {
            self.plain[slot] = true;
            for &choice in plain {
                self.add(slot, choice);
            }
        }
    }
}
struct CohortShape {
    base: Arc<crate::just_bound::Upper>,
    options: [Vec<i32>; 5],
    plain: [bool; 5],
}
struct PatternSet {
    fine: Vec<CohortShape>,
    coarse: Vec<CohortShape>,
}
type PatternKey = (usize, usize, [i32; 5]);
type PatternCache = HashMap<PatternKey, Option<Arc<PatternSet>>>;
struct LuckCohort {
    base: SharedBase,
    options: [Vec<i32>; 5],
    domains: [u32; 5],
    selected: u8,
}
struct Cohort {
    base: Arc<crate::just_bound::Upper>,
    options: [Vec<i32>; 5],
    domains: [u32; 5],
}
type CohortCache = HashMap<(usize, [i32; 5], bool), Option<Arc<Vec<Cohort>>>>;
struct ChainOption {
    snap: i32,
    actor: ChainActor,
    domain: u32,
}
struct NumericConverter {
    snap: i32,
    prepared: Arc<crate::just_bound::Converter>,
    order: usize,
}
struct Assignment {
    numeric_converters: Vec<Vec<NumericConverter>>,
    numeric_supported: bool,
    member_deck: Vec<Performer>,
    member_ids: Vec<u32>,
    options: Vec<Vec<i32>>,
    option_ids: Vec<u32>,
    nonchain: Vec<Vec<i32>>,
    nonchain_ids: Vec<u32>,
    chain_options: Vec<Vec<ChainOption>>,
    chains: Vec<Arc<ChainChoice>>,
}
struct AssignmentPrefix<'a> {
    members: &'a [usize],
    assigned: &'a [i32],
    options: &'a [&'a [i32]],
    option_ids: &'a [u32],
    selected_slots: u8,
    from: usize,
    base_power: f64,
    power_bound: f64,
    snapshot_power: &'a [f64],
}
impl Kernel<'_> {
    fn chain_choice(
        &mut self,
        snap: i32,
        members: &[usize],
        options: &[Vec<i32>],
    ) -> Arc<ChainChoice> {
        let ids = std::array::from_fn(|slot| {
            let index = members[slot] * self.snaps.len() + snap as usize;
            if !self.support_chain[index] {
                return u32::MAX;
            }
            if let Some(id) = self.chain_actor_ids[index] {
                return id;
            }
            let p = self.support(members[slot], snap).unwrap();
            let id = self.performer_key(&p);
            self.chain_actor_ids[index] = Some(id);
            id
        });
        let available = options
            .iter()
            .enumerate()
            .fold(0u8, |bits, (slot, choices)| {
                bits | ((choices.contains(&snap) as u8) << slot)
            });
        let key = (snap, ids, available);
        if let Some(choice) = self.chain_choices.get(&key) {
            return Arc::clone(choice);
        }
        let owned: [Option<ChainActor>; 5] = std::array::from_fn(|slot| {
            (ids[slot] != u32::MAX).then(|| (self.support(members[slot], snap).unwrap(), ids[slot]))
        });
        let future = std::array::from_fn(|from| {
            let mut seen = HashSet::default();
            let variants = (from..5)
                .filter(|&slot| available & (1 << slot) != 0)
                .filter_map(|slot| owned[slot].as_ref())
                .filter(|p| seen.insert(p.1))
                .map(|p| p.0.clone())
                .collect();
            self.strongest(variants)
                .into_iter()
                .map(|p| {
                    let id = self.performer_key(&p);
                    (p, id)
                })
                .collect()
        });
        let choice = Arc::new(ChainChoice {
            snap,
            owned,
            future,
        });
        if self.chain_choices.len() >= 2048 {
            if let Some(old) = self.chain_choice_order.pop_front() {
                self.chain_choices.remove(&old);
            }
        }
        self.chain_choices.insert(key, Arc::clone(&choice));
        self.chain_choice_order.push_back(key);
        choice
    }
    fn luck_choices(
        &mut self,
        data: &mut Base,
        member: usize,
        domain: u32,
        options: &[i32],
        snapshot_power: &[f64],
    ) -> Arc<LuckChoices> {
        let maximum = self.criterion == Criterion::Max;
        let key = (data.choice_id, member, domain, maximum);
        if let Some((choices, used)) = self.choice_profiles.get_mut(&key) {
            *used = true;
            return Arc::clone(choices);
        }
        let width = self.snaps.len() + 1;
        let rows: Vec<LuckChoice> = options
            .iter()
            .map(|&snap| LuckChoice {
                snap,
                power: snapshot_power
                    [member * width + if snap < 0 { width - 1 } else { snap as usize }],
                weights: if maximum {
                    self.luck_events(data, member, snap)
                } else {
                    [self.luck_slot_locked(data, member, snap, Criterion::Mean); 5]
                },
                chain: snap >= 0 && self.support_chain[member * self.snaps.len() + snap as usize],
            })
            .collect();
        let mut highest: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.snap,
                    row.weights
                        .iter()
                        .copied()
                        .fold(f64::NEG_INFINITY, f64::max),
                )
            })
            .collect();
        highest.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        let frontiers = std::array::from_fn(|event| {
            if !maximum && event > 0 {
                return Vec::new();
            }
            let mut frontier = Vec::new();
            for chain in [false, true] {
                let mut indices: Vec<_> = (0..rows.len())
                    .filter(|&index| rows[index].chain == chain)
                    .collect();
                indices.sort_unstable_by(|&a, &b| {
                    rows[b]
                        .power
                        .total_cmp(&rows[a].power)
                        .then(rows[b].weights[event].total_cmp(&rows[a].weights[event]))
                        .then(rows[a].snap.cmp(&rows[b].snap))
                });
                let mut witnesses = [f64::NEG_INFINITY; 5];
                indices.retain(|&index| {
                    let value = rows[index].weights[event];
                    if value <= witnesses[4] {
                        return false;
                    }
                    let mut at = 4;
                    while at > 0 && witnesses[at - 1] < value {
                        witnesses[at] = witnesses[at - 1];
                        at -= 1;
                    }
                    witnesses[at] = value;
                    true
                });
                frontier.extend(indices);
            }
            frontier
        });
        let nonnegative = rows
            .iter()
            .all(|row| row.power >= 0.0 && row.weights.iter().all(|&weight| weight >= 0.0));
        let choices = Arc::new(LuckChoices {
            priced: OnceLock::new(),
            nonnegative,
            rows,
            highest,
            frontiers,
        });
        let bytes = 128
            + (if maximum { 50 } else { 10 }) * std::mem::size_of::<PricedChoice>()
            + std::mem::size_of::<LuckChoices>()
            + choices.rows.capacity() * std::mem::size_of::<LuckChoice>()
            + choices.highest.capacity() * std::mem::size_of::<(i32, f64)>()
            + choices.frontiers.iter().map(Vec::capacity).sum::<usize>()
                * std::mem::size_of::<usize>();
        if bytes <= 16 * 1024 * 1024 {
            while self.choice_bytes + bytes > 16 * 1024 * 1024 {
                let Some((old, size)) = self.choice_order.pop_front() else {
                    break;
                };
                let (_, used) = self.choice_profiles.get_mut(&old).unwrap();
                if std::mem::replace(used, false) {
                    self.choice_order.push_back((old, size));
                } else {
                    self.choice_profiles.remove(&old);
                    self.choice_bytes -= size;
                }
            }
            self.choice_bytes += bytes;
            self.choice_profiles
                .insert(key, (Arc::clone(&choices), false));
            self.choice_order.push_back((key, bytes));
        }
        choices
    }
    fn priced_order(offers: &[[f64; 5]; 5]) -> (f64, [usize; 5]) {
        let mut dp = [f64::NEG_INFINITY; 32];
        let mut previous = [0usize; 32];
        dp[0] = 0.0;
        for mask in 0usize..31 {
            if dp[mask] == f64::NEG_INFINITY {
                continue;
            }
            let slot = mask.count_ones() as usize;
            for (event, &offer) in offers[slot].iter().enumerate() {
                if mask & (1 << event) == 0 {
                    let next = mask | (1 << event);
                    let value = (dp[mask] + offer).next_up();
                    if value > dp[next] {
                        dp[next] = value;
                        previous[next] = event;
                    }
                }
            }
        }
        let mut order = [0usize; 5];
        let mut mask = 31;
        for slot in (0..5).rev() {
            order[slot] = previous[mask];
            mask &= !(1 << order[slot]);
        }
        (dp[31], order)
    }
    fn balanced_partition_lambda(
        rows: &[PartitionRow; 5],
        prefix: &AssignmentPrefix<'_>,
        future_chain: bool,
        maximum: bool,
        constant: f64,
        lambda: f64,
    ) -> f64 {
        let mut offers = [[f64::NEG_INFINITY; 5]; 5];
        let mut picked = [[None; 5]; 5];
        for (slot, row) in rows.iter().enumerate() {
            for event in 0..if maximum { 5 } else { 1 } {
                for choice in row.offers(event) {
                    if slot >= prefix.from
                        && (choice.snap >= 0
                            && prefix.assigned[..prefix.from].contains(&choice.snap)
                            || !future_chain
                                && choice.chain
                                && prefix.selected_slots & (1 << slot) == 0)
                    {
                        continue;
                    }
                    let value = lambda * choice.power + choice.weights[event] / lambda;
                    if value > offers[slot][event] {
                        offers[slot][event] = value;
                        picked[slot][event] = Some(*choice);
                    }
                }
            }
        }
        let order = if maximum {
            Self::priced_order(&offers).1
        } else {
            [0; 5]
        };
        let mut power = prefix.base_power;
        let mut weight = constant;
        for slot in 0..5 {
            if let Some(choice) = picked[slot][order[slot]] {
                power += choice.power;
                weight += choice.weights[order[slot]];
            }
        }
        let balanced = (weight / power).sqrt();
        if balanced > 0.0 && balanced.is_finite() {
            balanced
        } else {
            lambda
        }
    }
    fn fixed_partition_bound(
        &self,
        rows: &[PartitionRow; 5],
        prefix: &AssignmentPrefix<'_>,
        future_chain: bool,
        constant: f64,
        lambda: f64,
    ) -> f64 {
        let maximum = self.criterion == Criterion::Max;
        let inverse = 1.0 / lambda;
        let high = inverse.next_up();
        let low = inverse.next_down();
        let price = |power: f64, weight: f64| {
            ((lambda * power).next_up()
                + (weight * if weight >= 0.0 { high } else { low }).next_up())
            .next_up()
        };
        let sum = |a: f64, b: f64| {
            if a == f64::NEG_INFINITY || b == f64::NEG_INFINITY {
                f64::NEG_INFINITY
            } else {
                (a + b).next_up()
            }
        };
        let mut offers = [[[f64::NEG_INFINITY; 5]; 2]; 5];
        for (slot, (row, slot_offers)) in rows.iter().zip(&mut offers).enumerate() {
            let [plain, chained] = slot_offers;
            let mut local = None;
            for (event, (plain, chained)) in plain
                .iter_mut()
                .zip(chained)
                .enumerate()
                .take(if maximum { 5 } else { 1 })
            {
                match row {
                    PartitionRow::Fixed(choice) => {
                        *plain = price(choice.power, choice.weights[event])
                    }
                    PartitionRow::Options(choices) => {
                        let build = || {
                            let events = std::array::from_fn(|event| {
                                std::array::from_fn(|chain| {
                                    if !maximum && event > 0 {
                                        return Vec::new();
                                    }
                                    let mut points: Vec<_> = choices
                                        .rows
                                        .iter()
                                        .filter(|choice| choice.chain as usize == chain)
                                        .map(|choice| PricedChoice {
                                            snap: choice.snap,
                                            value: price(choice.power, choice.weights[event]),
                                        })
                                        .collect();
                                    points.sort_unstable_by(|a, b| b.value.total_cmp(&a.value));
                                    points.truncate(5);
                                    points.shrink_to_fit();
                                    points
                                })
                            });
                            PricedChoices { events }
                        };
                        let cached = choices.priced.get_or_init(|| (lambda.to_bits(), build()));
                        let priced = if cached.0 == lambda.to_bits() {
                            &cached.1
                        } else {
                            local.get_or_insert_with(build)
                        };
                        for chain in 0..2 {
                            let future = chain == 1 && prefix.selected_slots & (1 << slot) == 0;
                            if future && !future_chain {
                                continue;
                            }
                            if let Some(point) = priced.events[event][chain].iter().find(|point| {
                                point.snap < 0
                                    || !prefix.assigned[..prefix.from].contains(&point.snap)
                            }) {
                                let target = if future { &mut *chained } else { &mut *plain };
                                *target = target.max(point.value);
                            }
                        }
                    }
                }
            }
        }
        let variable = if !maximum {
            let mut dp = [0.0, f64::NEG_INFINITY];
            for row in &offers {
                dp = [
                    sum(dp[0], row[0][0]),
                    sum(dp[1], row[0][0])
                        .max(sum(dp[0], row[1][0]))
                        .max(sum(dp[1], row[1][0])),
                ];
            }
            dp[future_chain as usize]
        } else if !future_chain {
            Self::priced_order(&std::array::from_fn(|slot| offers[slot][0])).0
        } else {
            let mut dp = [[f64::NEG_INFINITY; 2]; 32];
            dp[0][0] = 0.0;
            for mask in 0usize..31 {
                let slot = mask.count_ones() as usize;
                for (event, (&plain, &chained)) in
                    offers[slot][0].iter().zip(&offers[slot][1]).enumerate()
                {
                    if mask & (1 << event) == 0 {
                        let next = mask | (1 << event);
                        dp[next][0] = dp[next][0].max(sum(dp[mask][0], plain));
                        dp[next][1] = dp[next][1]
                            .max(sum(dp[mask][1], plain))
                            .max(sum(dp[mask][0], chained))
                            .max(sum(dp[mask][1], chained));
                    }
                }
            }
            dp[31][1]
        };
        if variable == f64::NEG_INFINITY {
            return variable;
        }
        let total = (((lambda * prefix.base_power).next_up() + price(0.0, constant)).next_up()
            + variable)
            .next_up();
        (((total * total).next_up() / 4.0).next_up() / self.chart.full.converted as f64).next_up()
            * (1.0 + REFINE_SLACK)
            + (self.chart.notes.len() + 4) as f64
    }
    fn minimum_context_plane(
        &mut self,
        data: &mut Base,
        prefix: &AssignmentPrefix<'_>,
        require_future_chain: bool,
        lambda: f64,
    ) -> f64 {
        let inverse = 1.0 / lambda;
        let high = inverse.next_up();
        let low = inverse.next_down();
        let divide = |weight: f64| (weight * if weight >= 0.0 { high } else { low }).next_up();
        let width = self.snaps.len() + 1;
        let mut offers = [[f64::NEG_INFINITY; 5]; 5];
        for (slot, row) in offers.iter_mut().enumerate() {
            let member = prefix.members[slot];
            let fixed = [if slot < prefix.from {
                prefix.assigned[slot]
            } else {
                -1
            }];
            let options = if slot < prefix.from {
                &fixed[..]
            } else {
                prefix.options[slot]
            };
            for &snap in options {
                if slot >= prefix.from
                    && (snap >= 0 && prefix.assigned[..prefix.from].contains(&snap)
                        || !require_future_chain
                            && snap >= 0
                            && self.support_chain[member * self.snaps.len() + snap as usize]
                            && prefix.selected_slots & (1 << slot) == 0)
                {
                    continue;
                }
                let power = prefix.snapshot_power
                    [member * width + if snap < 0 { width - 1 } else { snap as usize }];
                let weights = self.luck_events(data, member, snap);
                for (event, &weight) in weights.iter().enumerate() {
                    let value = ((lambda * power).next_up() + divide(weight)).next_up();
                    row[event] = row[event].max(value);
                }
            }
            if row[0] == f64::NEG_INFINITY {
                return f64::NEG_INFINITY;
            }
        }
        let mut dp = [f64::INFINITY; 32];
        dp[0] = 0.0;
        for mask in 0usize..31 {
            let slot = mask.count_ones() as usize;
            for (event, &value) in offers[slot].iter().enumerate() {
                if mask & (1 << event) == 0 {
                    let next = mask | (1 << event);
                    dp[next] = dp[next].min((dp[mask] + value).next_up());
                }
            }
        }
        let total = (((lambda * prefix.base_power).next_up() + divide(data.weights.base))
            .next_up()
            + dp[31])
            .next_up();
        if total < 0.0 {
            return f64::INFINITY;
        }
        (((total * total).next_up() / 4.0).next_up() / self.chart.full.converted as f64).next_up()
            * (1.0 + REFINE_SLACK)
            + (self.chart.notes.len() + 4) as f64
    }
    fn context_partition_bound(
        &mut self,
        base: &SharedBase,
        prefix: &AssignmentPrefix<'_>,
        require_future_chain: bool,
        cutoff: f64,
    ) -> f64 {
        let maximum = self.criterion == Criterion::Max;
        let width = self.snaps.len() + 1;
        let mut data = base.lock().unwrap_or_else(|e| e.into_inner());
        let constant = data.weights.base;
        let mut coefficient = constant;
        let chain = |slot: usize, choice: &LuckChoice| {
            slot >= prefix.from && choice.chain && prefix.selected_slots & (1 << slot) == 0
        };
        let available = |slot: usize, choice: &LuckChoice| {
            !(slot >= prefix.from
                && choice.snap >= 0
                && prefix.assigned[..prefix.from].contains(&choice.snap)
                || !require_future_chain && chain(slot, choice))
        };
        let mut rows: [PartitionRow; 5] = std::array::from_fn(|_| {
            PartitionRow::Fixed(LuckChoice {
                snap: -1,
                power: 0.0,
                weights: [0.0; 5],
                chain: false,
            })
        });
        for (slot, row) in rows.iter_mut().enumerate() {
            let member = prefix.members[slot];
            *row = if slot < prefix.from {
                let snap = prefix.assigned[slot];
                let weights = if maximum {
                    self.luck_events(&mut data, member, snap)
                } else {
                    [self.luck_slot_locked(&mut data, member, snap, Criterion::Mean); 5]
                };
                let power = prefix.snapshot_power
                    [member * width + if snap < 0 { width - 1 } else { snap as usize }];
                PartitionRow::Fixed(LuckChoice {
                    snap,
                    power,
                    weights,
                    chain: false,
                })
            } else {
                PartitionRow::Options(self.luck_choices(
                    &mut data,
                    member,
                    prefix.option_ids[slot],
                    prefix.options[slot],
                    prefix.snapshot_power,
                ))
            };
            let high = match row {
                PartitionRow::Fixed(choice) => Some(
                    choice
                        .weights
                        .iter()
                        .copied()
                        .fold(f64::NEG_INFINITY, f64::max),
                ),
                PartitionRow::Options(choices) => {
                    choices.highest.iter().find_map(|&(snap, weight)| {
                        let chain = snap >= 0
                            && self.support_chain[member * self.snaps.len() + snap as usize]
                            && prefix.selected_slots & (1 << slot) == 0;
                        (!(snap >= 0 && prefix.assigned[..prefix.from].contains(&snap)
                            || !require_future_chain && chain))
                            .then_some(weight)
                    })
                }
            };
            let Some(high) = high else {
                return f64::NEG_INFINITY;
            };
            coefficient += high.max(0.0);
        }
        let lambda = (coefficient / prefix.power_bound.max(1.0)).sqrt();
        if !lambda.is_finite() || lambda <= 0.0 {
            return f64::INFINITY;
        }
        let fixed_lambda = if cutoff.is_finite() {
            let fixed = data.pricing_lambda.unwrap_or_else(|| {
                Self::balanced_partition_lambda(
                    &rows,
                    prefix,
                    require_future_chain,
                    maximum,
                    constant,
                    lambda,
                )
            });
            data.pricing_lambda = Some(fixed);
            Some(fixed)
        } else {
            None
        };
        drop(data);
        if let Some(fixed) = fixed_lambda {
            let upper =
                self.fixed_partition_bound(&rows, prefix, require_future_chain, constant, fixed);
            if upper < cutoff {
                return upper;
            }
        }
        if self.criterion == Criterion::Min
            && self.healthy
            && self.owner_local
            && self.seeds.is_empty()
            && prefix.from <= 2
        {
            let mut data = base.lock().unwrap_or_else(|e| e.into_inner());
            let upper = self.minimum_context_plane(
                &mut data,
                prefix,
                require_future_chain,
                fixed_lambda.unwrap_or(lambda),
            );
            if upper < cutoff {
                return upper;
            }
        }
        let inverse = 1.0 / lambda;
        let high = inverse.next_up();
        let low = inverse.next_down();
        let divide = |value: f64| (value * if value >= 0.0 { high } else { low }).next_up();
        let nonnegative = high.is_finite() && rows.iter().all(PartitionRow::nonnegative);
        let price = |power: f64, weight: f64| {
            if nonnegative {
                lambda * power + high * weight
            } else {
                ((lambda * power).next_up() + divide(weight)).next_up()
            }
        };
        let sum = |a: f64, b: f64| {
            if a == f64::NEG_INFINITY || b == f64::NEG_INFINITY {
                f64::NEG_INFINITY
            } else {
                (a + b).next_up()
            }
        };
        let mut event_offers = [[f64::NEG_INFINITY; 5]; 5];
        let variable = if !maximum {
            let mut dp = [0.0, f64::NEG_INFINITY];
            for (slot, row) in rows.iter().enumerate() {
                let mut offers = [f64::NEG_INFINITY; 2];
                for choice in row.offers(0) {
                    if !available(slot, choice) {
                        continue;
                    }
                    let (power, weights, chain) =
                        (choice.power, choice.weights, chain(slot, choice));
                    let value = price(power, weights[0]);
                    offers[chain as usize] = offers[chain as usize].max(value);
                }
                dp = [
                    sum(dp[0], offers[0]),
                    sum(dp[1], offers[0])
                        .max(sum(dp[0], offers[1]))
                        .max(sum(dp[1], offers[1])),
                ];
            }
            dp[require_future_chain as usize]
        } else {
            let mut all_offers = [[[f64::NEG_INFINITY; 5]; 2]; 5];
            for (slot, (row, offers)) in rows.iter().zip(&mut all_offers).enumerate() {
                let [plain, chained] = offers;
                for (event, (plain, chained)) in plain.iter_mut().zip(chained).enumerate() {
                    for choice in row.offers(event) {
                        if !available(slot, choice) {
                            continue;
                        }
                        let value = price(choice.power, choice.weights[event]);
                        let target = if chain(slot, choice) {
                            &mut *chained
                        } else {
                            &mut *plain
                        };
                        *target = target.max(value);
                    }
                }
            }
            for slot in 0..5 {
                for event in 0..5 {
                    event_offers[slot][event] =
                        all_offers[slot][0][event].max(all_offers[slot][1][event]);
                }
            }
            if !require_future_chain || prefix.from == 4 {
                let mut dp = [f64::NEG_INFINITY; 32];
                dp[0] = 0.0;
                for mask in 0usize..31 {
                    let slot = mask.count_ones() as usize;
                    let category = (require_future_chain && slot == 4) as usize;
                    for (event, &weight) in all_offers[slot][category].iter().enumerate() {
                        if mask & (1 << event) == 0 {
                            let next = mask | (1 << event);
                            dp[next] = dp[next].max(sum(dp[mask], weight));
                        }
                    }
                }
                dp[31]
            } else {
                let mut dp = [[f64::NEG_INFINITY; 2]; 32];
                dp[0][0] = 0.0;
                for (slot, offers) in all_offers.iter().enumerate() {
                    for mask in 0usize..32 {
                        if mask.count_ones() as usize != slot {
                            continue;
                        }
                        for (event, _) in offers[0].iter().enumerate() {
                            if mask & (1 << event) != 0 {
                                continue;
                            }
                            let next = mask | (1 << event);
                            dp[next][0] = dp[next][0].max(sum(dp[mask][0], offers[0][event]));
                            if slot >= prefix.from {
                                dp[next][1] = dp[next][1]
                                    .max(sum(dp[mask][1], offers[0][event]))
                                    .max(sum(dp[mask][0], offers[1][event]))
                                    .max(sum(dp[mask][1], offers[1][event]));
                            }
                        }
                    }
                }
                dp[31][1]
            }
        };
        if variable == f64::NEG_INFINITY {
            return variable;
        }
        // Nonnegative two-product prices have gamma(3) relative error; outward DP sums also cover subnormal products.
        let variable = if nonnegative {
            (variable * (1.0 + 8.0 * f64::EPSILON)).next_up()
        } else {
            variable
        };
        let total = (((lambda * prefix.base_power).next_up() + divide(constant)).next_up()
            + variable)
            .next_up();
        let mut upper = (((total * total).next_up() / 4.0).next_up()
            / self.chart.full.converted as f64)
            .next_up()
            * (1.0 + REFINE_SLACK)
            + (self.chart.notes.len() + 4) as f64;
        if upper >= cutoff && (!maximum || prefix.from < 4) {
            let columns = self.snaps.len() + 5;
            let bad = -1e100;
            if self.context_matrix.is_empty() {
                self.context_matrix = vec![vec![bad; columns]; 5];
            }
            for row in &mut self.context_matrix {
                row.fill(bad);
            }
            for (slot, row) in rows.iter().enumerate() {
                for event in 0..if maximum { 5 } else { 1 } {
                    for choice in row.offers(event) {
                        if !available(slot, choice) {
                            continue;
                        }
                        let value = ((lambda * choice.power).next_up()
                            + divide(choice.weights[event]))
                        .next_up();
                        if choice.snap < 0 {
                            for cell in &mut self.context_matrix[slot][self.snaps.len()..] {
                                *cell = cell.max(value);
                            }
                        } else {
                            let cell = &mut self.context_matrix[slot][choice.snap as usize];
                            *cell = cell.max(value);
                        }
                    }
                }
            }
            let matched = self
                .context_matching
                .maximum_upper(&self.context_matrix, columns);
            if matched < bad / 2.0 {
                return f64::NEG_INFINITY;
            }
            let total = (((lambda * prefix.base_power).next_up() + divide(constant)).next_up()
                + matched)
                .next_up();
            let bound = (((total * total).next_up() / 4.0).next_up()
                / self.chart.full.converted as f64)
                .next_up()
                * (1.0 + REFINE_SLACK)
                + (self.chart.notes.len() + 4) as f64;
            upper = upper.min(bound);
            if maximum && prefix.from <= 2 && upper >= cutoff {
                let prices = crate::assignment::event_prices(&event_offers);
                for row in &mut self.context_matrix {
                    row.fill(bad);
                }
                for (slot, row) in rows.iter().enumerate() {
                    for (event, &price) in prices.iter().enumerate() {
                        for choice in row.offers(event) {
                            if !available(slot, choice) {
                                continue;
                            }
                            let value = (((lambda * choice.power).next_up()
                                + divide(choice.weights[event]))
                            .next_up()
                                - price)
                                .next_up();
                            if choice.snap < 0 {
                                for cell in &mut self.context_matrix[slot][self.snaps.len()..] {
                                    *cell = cell.max(value);
                                }
                            } else {
                                let cell = &mut self.context_matrix[slot][choice.snap as usize];
                                *cell = cell.max(value);
                            }
                        }
                    }
                }
                let matched = self
                    .context_matching
                    .maximum_upper(&self.context_matrix, columns);
                let prices = prices
                    .into_iter()
                    .fold(0.0, |sum, value| (sum + value).next_up());
                let total = sum(
                    sum((lambda * prefix.base_power).next_up(), divide(constant)),
                    sum(matched, prices),
                );
                let bound = (((total * total).next_up() / 4.0).next_up()
                    / self.chart.full.converted as f64)
                    .next_up()
                    * (1.0 + REFINE_SLACK)
                    + (self.chart.notes.len() + 4) as f64;
                upper = upper.min(bound);
            }
        }
        upper
    }
}
struct RosterPriceEntry {
    power: Vec<f64>,
    options: Vec<Vec<i32>>,
    pricing: Arc<crate::rewards::RosterPricing>,
}
pub struct Adapter<'k, 'a> {
    kernel: &'k mut Kernel<'a>,
    level: i32,
    surrogate: bool,
    estimate: bool,
    data: Option<Arc<Level>>,
    ranges: HashMap<usize, (f64, f64)>,
    assignment: Option<Assignment>,
    event_rows: HashMap<usize, [f64; 5]>,
    global_rows: HashMap<usize, [f64; 5]>,
    suffix_rows: Vec<[f64; 5]>,
    max_choices: Vec<Vec<i32>>,
    max_frontiers: Vec<[Vec<(f64, f64)>; 5]>,
    snapshot_power: Arc<Vec<f64>>,
    power_width: usize,
    base_power: f64,
    nominal_base: Option<f64>,
    member_dominance: HashMap<(usize, usize), bool>,
    deterministic: Option<crate::rewards::DeterministicBound>,
    roster_price: Option<Arc<crate::rewards::RosterPricing>>,
    roster_prices: VecDeque<RosterPriceEntry>,
    roster_suffix: Option<crate::rewards::RosterSuffix>,
    roster_extra: Option<crate::rewards::RosterSuffix>,
    roster_extra_ready: bool,
    roster_leader: Option<usize>,
    roster_last: Option<([usize; 5], usize, f64)>,
    dominance_margin: f64,
    refine_cutoff: f64,
    numeric_cohorts: CohortCache,
    luck_cohorts: HashMap<(usize, [i32; 5]), Arc<Vec<LuckCohort>>>,
    luck_assignment_bases: LuckBaseCache,
    plain_second_bounds: HashMap<i32, f64>,
    plain_pair_bound: Option<f64>,
    numeric_domain: usize,
    seed_scores: Vec<Hit>,
    combo_bound: Option<ComboBound>,
    combo_attempted: bool,
    combo_options: Vec<Vec<i32>>,
    fixed_no_rng_bound: Option<ComboBound>,
    fixed_no_rng_attempted: bool,
    fixed_region_models: HashMap<(FixedConversionBound, Option<usize>), RegionalBound>,
    fixed_region_order: VecDeque<(FixedConversionBound, Option<usize>)>,
    fixed_leaf_models: HashMap<(usize, bool), ComboBound>,
    fixed_leaf_order: VecDeque<(usize, bool)>,
    fixed_local_power: Arc<Vec<f64>>,
    fixed_box: bool,
    fixed_box_complete: bool,
    fixed_root_checked: bool,
    reuse_assignment: bool,
}
impl<'k, 'a> Adapter<'k, 'a> {
    fn roster_upper(&mut self, members: &[usize], from: usize, limit: f64) -> Option<f64> {
        let mut key = [usize::MAX; 5];
        key[..members.len()].copy_from_slice(members);
        if let Some((old, index, upper)) = self.roster_last {
            if old == key && index == from {
                return Some(upper);
            }
        }
        let mut upper = self.roster_suffix.as_mut()?.upper(members, from, limit);
        if upper >= limit {
            if !self.roster_extra_ready {
                self.roster_extra_ready = true;
                if let (Some(leader), Some(base), Some(suffix), Some(bound)) = (
                    self.roster_leader,
                    &self.roster_price,
                    &self.roster_suffix,
                    &self.deterministic,
                ) {
                    if let Some(entry) = self
                        .roster_prices
                        .iter()
                        .find(|entry| Arc::ptr_eq(&entry.pricing, base))
                    {
                        self.roster_extra = bound
                            .roster_pricing(
                                &entry.power,
                                &entry.options,
                                self.kernel.members,
                                Some((leader, &self.kernel.bonuses, self.level)),
                            )
                            .map(|extra| suffix.repriced(Arc::new(extra)));
                    }
                }
            }
            if let Some(extra) = &mut self.roster_extra {
                upper = upper.min(extra.upper(members, from, limit));
            }
        }
        self.roster_last = Some((key, from, upper));
        Some(upper)
    }
    fn new(
        kernel: &'k mut Kernel<'a>,
        level: i32,
        surrogate: bool,
        estimate: bool,
        deadline: f64,
        now: &dyn Fn() -> f64,
    ) -> Option<Self> {
        let n = kernel.members.len();
        let global_luck =
            (!surrogate && !estimate && kernel.chart.luck && !kernel.seeds.is_empty())
                .then(|| kernel.global_fixed_luck())
                .flatten();
        let deterministic =
            if !surrogate && !estimate && kernel.chart.luck && !kernel.seeds.is_empty() {
                crate::rewards::DeterministicBound::with_luck_preparation(
                    kernel.master,
                    kernel.chart,
                    kernel.members,
                    kernel.snaps,
                    1.0 + level as f64,
                    (None, global_luck.as_deref()),
                    &mut kernel.global_fixed_preparation,
                )
            } else if !surrogate && !estimate && !kernel.chart.luck {
                if kernel.maximum_level > 0 {
                    crate::rewards::DeterministicBound::with_combo(
                        kernel.master,
                        kernel.chart,
                        kernel.members,
                        kernel.snaps,
                        1.0 + level as f64,
                        None,
                    )
                } else {
                    crate::rewards::DeterministicBound::new(
                        kernel.master,
                        kernel.chart,
                        kernel.members,
                        kernel.snaps,
                    )
                }
            } else {
                None
            };
        let data = if deterministic.is_none() {
            if !kernel.calibrate(deadline, now) {
                return None;
            }
            Some(kernel.level(level))
        } else {
            None
        };
        if kernel.healthy && kernel.owner_local && kernel.just_context.is_some() {
            if let Some(bound) = &deterministic {
                kernel
                    .support_aliases
                    .set_just_ceiling(bound.just_ceiling());
                kernel.shape_ids.fill(u32::MAX);
                kernel.bound_shape_ids.fill(u32::MAX);
                kernel.bound_actors.fill(None);
            }
        }
        Some(Self {
            kernel,
            level,
            surrogate,
            estimate,
            data,
            ranges: HashMap::default(),
            assignment: None,
            event_rows: HashMap::default(),
            global_rows: HashMap::default(),
            suffix_rows: Vec::new(),
            max_choices: vec![Vec::new(); n],
            max_frontiers: (0..n)
                .map(|_| std::array::from_fn(|_| Vec::new()))
                .collect(),
            snapshot_power: Arc::new(Vec::new()),
            power_width: 0,
            base_power: 0.0,
            nominal_base: None,
            member_dominance: HashMap::default(),
            deterministic,
            roster_price: None,
            roster_prices: VecDeque::new(),
            roster_suffix: None,
            roster_extra: None,
            roster_extra_ready: false,
            roster_leader: None,
            roster_last: None,
            dominance_margin: f64::INFINITY,
            refine_cutoff: f64::NEG_INFINITY,
            numeric_cohorts: HashMap::default(),
            luck_cohorts: HashMap::default(),
            luck_assignment_bases: HashMap::default(),
            plain_second_bounds: HashMap::default(),
            plain_pair_bound: None,
            numeric_domain: 0,
            seed_scores: Vec::new(),
            combo_bound: None,
            combo_attempted: false,
            combo_options: Vec::new(),
            fixed_no_rng_bound: None,
            fixed_no_rng_attempted: false,
            fixed_region_models: HashMap::default(),
            fixed_region_order: VecDeque::new(),
            fixed_leaf_models: HashMap::default(),
            fixed_leaf_order: VecDeque::new(),
            fixed_local_power: Arc::new(Vec::new()),
            fixed_box: false,
            fixed_box_complete: false,
            fixed_root_checked: false,
            reuse_assignment: false,
        })
    }
    fn scripted_combo_bound(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        totals: Totals,
        limit: f64,
    ) -> Option<f64> {
        if !self.combo_attempted {
            self.combo_attempted = true;
            if !self.kernel.healthy {
                return None;
            }
            let script = self.kernel.combo_script(members)?;
            let fixed_luck = self.kernel.chart.luck && !self.kernel.seeds.is_empty();
            let bound = if fixed_luck {
                let programs = self.kernel.fixed_luck_members(members)?;
                let supports = self
                    .combo_options
                    .iter()
                    .enumerate()
                    .any(|(slot, choices)| {
                        choices.iter().any(|&snap| {
                            self.kernel
                                .support(members[slot], snap)
                                .is_some_and(|actor| {
                                    actor.gekisou_support_skills.iter().any(|&(id, level)| {
                                        self.kernel.master.gekiso_support_skills[&id]
                                            .effects
                                            .iter()
                                            .any(|row| {
                                                row.level == level
                                                    && (11000..=11005).contains(&row.kind)
                                            })
                                    })
                                })
                        })
                    });
                let key = FixedLuckKey {
                    members: lottery_member_key(self.kernel.master, &programs),
                    combo: self.kernel.combo_key(&script),
                    supports,
                    level: self.level,
                };
                if !self.kernel.fixed_luck_bounds.contains_key(&key) {
                    let changes = self.kernel.fixed_luck_changes(&programs, supports)?;
                    let trace = self.kernel.fixed_combo_trace(&script)?;
                    let mut model = crate::rewards::DeterministicBound::with_luck_trace(
                        self.kernel.master,
                        self.kernel.chart,
                        self.kernel.members,
                        self.kernel.snaps,
                        1.0 + self.level as f64,
                        trace,
                        &changes,
                    )?;
                    model.coefficient_limit = 256;
                    model.snapshot_power(Arc::clone(&self.snapshot_power));
                    if self.kernel.fixed_luck_bounds.len() >= 32 {
                        if let Some(old) = self.kernel.fixed_luck_order.pop_front() {
                            self.kernel.fixed_luck_bounds.remove(&old);
                        }
                    }
                    self.kernel
                        .fixed_luck_bounds
                        .insert(key.clone(), Arc::new(std::sync::Mutex::new(model)));
                }
                self.kernel.fixed_luck_order.retain(|old| old != &key);
                self.kernel.fixed_luck_order.push_back(key.clone());
                Arc::clone(self.kernel.fixed_luck_bounds.get(&key)?)
            } else {
                if !self
                    .kernel
                    .chart
                    .setup
                    .missions
                    .iter()
                    .all(|&mission| mission == 1)
                {
                    return None;
                }
                if !self.kernel.combo_bounds.contains_key(&script) {
                    let mut model = crate::rewards::DeterministicBound::with_combo(
                        self.kernel.master,
                        self.kernel.chart,
                        self.kernel.members,
                        self.kernel.snaps,
                        1.0 + self.level as f64,
                        Some(&script),
                    )?;
                    model.coefficient_limit = 256;
                    model.snapshot_power(Arc::clone(&self.snapshot_power));
                    if self.kernel.combo_bounds.len() >= 32 {
                        if let Some(old) = self.kernel.combo_order.pop_front() {
                            self.kernel.combo_bounds.remove(&old);
                        }
                    }
                    self.kernel
                        .combo_bounds
                        .insert(script.clone(), Arc::new(std::sync::Mutex::new(model)));
                }
                self.kernel.combo_order.retain(|old| old != &script);
                self.kernel.combo_order.push_back(script.clone());
                Arc::clone(self.kernel.combo_bounds.get(&script)?)
            };
            bound
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .prepare_score_assignment(members, &self.combo_options);
            self.combo_bound = Some(bound);
        }
        let mut bound = self
            .combo_bound
            .as_ref()?
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut upper =
            bound.score_ceiling(members, assigned, from, totals.power, self.kernel.criterion);
        if upper > limit {
            upper = upper.min(bound.joint_score_ceiling(
                members,
                assigned,
                from,
                self.base_power,
                (totals, limit),
                self.kernel.criterion,
            ));
        }
        Some(upper)
    }
    fn fixed_conversion_trace(&mut self, team: &Team) -> Option<(usize, Arc<Context>, bool)> {
        if !self.kernel.chart.luck
            || self.kernel.seeds.is_empty()
            || !self.kernel.healthy
            || !self.kernel.owner_local
        {
            return None;
        }
        let choices = team
            .members
            .iter()
            .zip(&team.snaps)
            .map(|(&member, &snap)| self.kernel.fixed_choice(member, snap).clone())
            .collect::<Vec<_>>();
        if choices.iter().any(|choice| !choice.supported) {
            return None;
        }
        let supports = choices.iter().any(|choice| choice.lottery);
        let converters = choices
            .into_iter()
            .filter_map(|choice| choice.converter)
            .collect::<Vec<_>>();
        let (id, trace) = self
            .kernel
            .just_bounds
            .converter_trace(self.kernel.master, self.kernel.chart, &converters)
            .ok()??;
        if trace
            .judgement
            .iter()
            .any(|&judgement| judgement != 5 && judgement != 6)
        {
            return None;
        }
        Some((id, trace, supports))
    }
    fn fixed_region_model(&mut self, team: &Team, supports: bool) -> Option<RegionalBound> {
        self.fixed_region_bound(team, Some(supports), false)
    }
    fn fixed_region_bound(
        &mut self,
        team: &Team,
        supports: Option<bool>,
        hull: bool,
    ) -> Option<RegionalBound> {
        self.fixed_region_bound_limit(team, supports.map(|yes| if yes { 5 } else { 0 }), hull)
    }
    fn fixed_region_bound_limit(
        &mut self,
        team: &Team,
        supports: Option<usize>,
        hull: bool,
    ) -> Option<RegionalBound> {
        let (conversion, trace) = if hull {
            let mut count = 0;
            for (slot, choices) in self.combo_options.iter().enumerate() {
                if choices.iter().any(|&snap| {
                    self.kernel
                        .fixed_choice(team.members[slot], snap)
                        .converter
                        .is_some()
                }) {
                    count += 1;
                }
            }
            self.kernel.fixed_just_hull(count)?;
            let (_, trace) = self
                .kernel
                .just_bounds
                .converter_trace(self.kernel.master, self.kernel.chart, &[])
                .ok()??;
            (FixedConversionBound::Hull(count), trace)
        } else {
            let (id, trace, _) = self.fixed_conversion_trace(team)?;
            (FixedConversionBound::Trace(id), trace)
        };
        let key = (conversion, supports);
        if !self.fixed_region_models.contains_key(&key) {
            let domains = self.kernel.fixed_region_domains()?.to_vec();
            if trace
                .judgement
                .iter()
                .enumerate()
                .any(|(index, &judgement)| {
                    judgement != self.kernel.chart.judgements[index] && domains[index] != 3
                })
            {
                return None;
            }
            let script = self.kernel.combo_script(&team.members)?;
            let luck = if let Some(supports) = supports {
                let programs = self.kernel.fixed_luck_members(&team.members)?;
                self.kernel.fixed_luck_changes_limit(&programs, supports)?
            } else {
                self.kernel.global_fixed_luck()?
            };
            let combo_key = self.kernel.combo_key(&script);
            let shared_key = FixedRegionalKey {
                conversion,
                combo: combo_key.clone(),
                luck: Arc::clone(&luck),
            };
            self.kernel.trim_fixed_region_rows();
            let rows = if let Some(rows) = self.kernel.fixed_region_rows.get(&shared_key) {
                rows.touch();
                Arc::clone(rows)
            } else {
                let mut components = Vec::new();
                for kind in 0..=3 {
                    if !domains.contains(&kind) {
                        continue;
                    }
                    if kind == 3 {
                        if let FixedConversionBound::Hull(count) = conversion {
                            components.push(self.kernel.fixed_just_hull(count)?);
                            continue;
                        }
                    }
                    let component_key = match kind {
                        0 => FixedRegionKey::Outside,
                        1 => FixedRegionKey::Combo(combo_key.clone()),
                        2 => FixedRegionKey::Luck(Arc::clone(&luck)),
                        _ => match conversion {
                            FixedConversionBound::Trace(id) => FixedRegionKey::Just(id),
                            _ => unreachable!(),
                        },
                    };
                    let component_key = if supports.is_some() && kind != 2 {
                        FixedRegionKey::Guarded(Box::new(component_key), luck.changes)
                    } else {
                        component_key
                    };
                    components.push(
                        self.kernel
                            .fixed_component(component_key, &trace, &script)?,
                    );
                }
                self.kernel.combined_fixed_rows(shared_key, &components)?
            };
            let model = crate::rewards::FixedRegionBound::new(
                rows,
                &team.members,
                Arc::clone(&self.fixed_local_power),
            )?;
            if self.fixed_region_models.len() >= 32 {
                if let Some(old) = self.fixed_region_order.pop_front() {
                    self.fixed_region_models.remove(&old);
                }
            }
            self.fixed_region_models
                .insert(key, Arc::new(std::sync::Mutex::new(model)));
        }
        self.fixed_region_order.retain(|old| old != &key);
        self.fixed_region_order.push_back(key);
        self.fixed_region_models.get(&key).map(Arc::clone)
    }
    fn fixed_converted_model(
        &mut self,
        team: &Team,
        with_supports: Option<bool>,
    ) -> Option<ComboBound> {
        let (id, trace, actual_supports) = self.fixed_conversion_trace(team)?;
        let supports = with_supports.unwrap_or(actual_supports);
        let key = (id, supports);
        if !self.fixed_leaf_models.contains_key(&key) {
            let programs = self.kernel.fixed_luck_members(&team.members)?;
            let luck = self.kernel.fixed_luck_changes(&programs, supports)?;
            let formation = team
                .members
                .iter()
                .map(|&m| self.kernel.members[m].clone())
                .collect::<Vec<_>>();
            let script = self.kernel.combo_script(&team.members)?;
            let chart = self.kernel.chart.converted_judgements(&trace.judgement)?;
            if self.kernel.converted_master.is_none() {
                let mut master = self.kernel.master.clone();
                let mut skills = (*master.gekiso_support_skills).clone();
                for skill in skills.values_mut() {
                    skill.effects.retain(|row| row.kind != 13005);
                }
                master.gekiso_support_skills = Arc::new(skills);
                self.kernel.converted_master = Some(master);
            }
            let combo = self.kernel.fixed_combo_trace(&script)?;
            let mut model = crate::rewards::DeterministicBound::with_luck_trace(
                self.kernel.converted_master.as_ref().unwrap(),
                &chart,
                &formation,
                self.kernel.snaps,
                1.0 + self.level as f64,
                combo,
                &luck,
            )?;
            model.coefficient_limit = 256;
            model.snapshot_power(Arc::clone(&self.fixed_local_power));
            model.prepare_fixed_score_assignment(&[0, 1, 2, 3, 4], &self.combo_options);
            if self.fixed_leaf_models.len() >= 32 {
                if let Some(old) = self.fixed_leaf_order.pop_front() {
                    self.fixed_leaf_models.remove(&old);
                }
            }
            self.fixed_leaf_models
                .insert(key, Arc::new(std::sync::Mutex::new(model)));
        }
        self.fixed_leaf_order.retain(|old| old != &key);
        self.fixed_leaf_order.push_back(key);
        Some(Arc::clone(self.fixed_leaf_models.get(&key)?))
    }
    fn fixed_converted_score_ceiling(&mut self, team: &Team, power: f64) -> Option<f64> {
        let (_, _, supports) = self.fixed_conversion_trace(team)?;
        if let Some(model) = self.fixed_region_model(team, supports) {
            let mut model = model.lock().unwrap_or_else(|error| error.into_inner());
            model.prepare(&self.combo_options);
            return Some(model.score_ceiling(&team.snaps, 5, power, self.kernel.criterion));
        }
        let model = self.fixed_converted_model(team, None)?;
        let mut model = model.lock().unwrap_or_else(|error| error.into_inner());
        model.prepare_fixed_score_assignment(&[0, 1, 2, 3, 4], &self.combo_options);
        Some(model.score_ceiling(
            &[0, 1, 2, 3, 4],
            &team.snaps,
            5,
            power,
            self.kernel.criterion,
        ))
    }
    fn fixed_class_score_ceiling(
        &mut self,
        team: &Team,
        options: &[Vec<i32>],
        mode: FixedClassMode,
        totals: Totals,
        limit: f64,
    ) -> Option<f64> {
        let FixedClassMode { supports, hull } = mode;
        let required = (supports == Some(true)).then(|| {
            options
                .iter()
                .enumerate()
                .map(|(slot, choices)| {
                    let mut flags = vec![false; self.power_width - 1 + 5];
                    for &snap in choices {
                        let column = if snap < 0 {
                            self.power_width - 1 + slot
                        } else {
                            snap as usize
                        };
                        flags[column] = self.kernel.fixed_lottery(team.members[slot], snap);
                    }
                    flags
                })
                .collect::<Vec<_>>()
        });
        let power = options
            .iter()
            .enumerate()
            .fold(self.base_power, |sum, (slot, choices)| {
                let power = choices
                    .iter()
                    .map(|&snap| {
                        self.snapshot_power[team.members[slot] * self.power_width
                            + if snap < 0 {
                                self.power_width - 1
                            } else {
                                snap as usize
                            }]
                    })
                    .fold(f64::NEG_INFINITY, f64::max);
                (sum + power).next_up()
            })
            .min(totals.power);
        if supports == Some(true) {
            let flags = required.as_deref()?;
            let capacity = crate::assignment::flagged_capacity(flags);
            let coarse = self.fixed_region_bound(team, None, hull)?;
            let mut upper = f64::NEG_INFINITY;
            for count in 1..=capacity {
                let value = {
                    let mut model = coarse.lock().unwrap_or_else(|error| error.into_inner());
                    model.prepare(options);
                    model.physical_score_ceiling_count(
                        self.base_power,
                        Totals { power, ..totals },
                        self.kernel.criterion,
                        Some((flags, count, count)),
                        limit,
                    )
                };
                if value < limit {
                    upper = upper.max(value);
                    continue;
                }
                let model = self.fixed_region_bound_limit(team, Some(count), hull)?;
                let mut model = model.lock().unwrap_or_else(|error| error.into_inner());
                model.prepare(options);
                let mut value = model.score_ceiling(&team.snaps, 0, power, self.kernel.criterion);
                if value >= limit {
                    value = value.min(model.physical_score_ceiling_count(
                        self.base_power,
                        Totals { power, ..totals },
                        self.kernel.criterion,
                        Some((flags, count, count)),
                        limit,
                    ));
                }
                if value >= limit {
                    return Some(f64::INFINITY);
                }
                upper = upper.max(value);
            }
            return Some(upper);
        }
        if let Some(model) = self.fixed_region_bound(team, supports, hull) {
            let mut model = model.lock().unwrap_or_else(|error| error.into_inner());
            model.prepare(options);
            let mut value = model.score_ceiling(&team.snaps, 0, power, self.kernel.criterion);
            if value >= limit {
                value = value.min(model.physical_score_ceiling(
                    self.base_power,
                    Totals { power, ..totals },
                    self.kernel.criterion,
                    required.as_deref(),
                    limit,
                ));
            }
            return Some(value);
        }
        if hull || supports.is_none() {
            return None;
        }
        let model = self.fixed_converted_model(team, supports)?;
        let mut model = model.lock().unwrap_or_else(|error| error.into_inner());
        model.prepare_fixed_score_assignment(&[0, 1, 2, 3, 4], options);
        let mut value = model.score_ceiling(
            &[0, 1, 2, 3, 4],
            &team.snaps,
            0,
            power,
            self.kernel.criterion,
        );
        if value >= limit {
            value = value.min(model.required_physical_score_ceiling(
                &[0, 1, 2, 3, 4],
                self.base_power,
                Totals { power, ..totals },
                self.kernel.criterion,
                required.as_deref(),
            ));
        }
        Some(value)
    }
    fn fixed_box_score_ceiling(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        totals: Totals,
        limit: f64,
    ) -> Option<f64> {
        let mut team = Team {
            members: members.to_vec(),
            snaps: assigned.to_vec(),
        };
        let mut options = self.combo_options.clone();
        for slot in 0..5 {
            if slot < from {
                options[slot] = vec![assigned[slot]];
            } else if self
                .kernel
                .fixed_choice(members[slot], options[slot][0])
                .converter
                .is_some()
            {
                team.snaps[slot] = options[slot][0];
            } else {
                team.snaps[slot] = -1;
            }
        }
        if let Some(upper) = self.fixed_class_score_ceiling(
            &team,
            &options,
            FixedClassMode {
                supports: None,
                hull: !self.fixed_box_complete,
            },
            totals,
            limit,
        ) {
            if upper < limit {
                return Some(upper);
            }
        }
        let with_supports = options.iter().enumerate().any(|(slot, choices)| {
            choices
                .iter()
                .any(|&snap| self.kernel.fixed_lottery(members[slot], snap))
        });
        let mut upper = f64::NEG_INFINITY;
        for supports in [false, true] {
            if supports && !with_supports {
                continue;
            }
            let mut allowed = options.clone();
            if !supports {
                for (slot, choices) in allowed.iter_mut().enumerate() {
                    choices.retain(|&snap| !self.kernel.fixed_lottery(members[slot], snap));
                }
            }
            if allowed.iter().any(Vec::is_empty) {
                continue;
            }
            let value = self.fixed_class_score_ceiling(
                &team,
                &allowed,
                FixedClassMode {
                    supports: Some(supports),
                    hull: !self.fixed_box_complete,
                },
                totals,
                limit,
            )?;
            if value >= limit {
                return None;
            }
            upper = upper.max(value);
        }
        Some(upper)
    }
    fn fixed_tail_score_ceiling(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        totals: Totals,
        limit: f64,
    ) -> Option<f64> {
        if !self.kernel.chart.luck || self.kernel.seeds.is_empty() || !(2..=4).contains(&from) {
            return None;
        }
        let mut plain = self.combo_options.clone();
        let mut classes = Vec::new();
        let mut lottery = [false; 5];
        for slot in from..5 {
            let mut choices = Vec::new();
            let mut nonconverter = Vec::new();
            for &snap in &self.combo_options[slot] {
                let choice = self.kernel.fixed_choice(members[slot], snap);
                if !choice.supported {
                    return None;
                }
                if choice.converter.is_some() {
                    if snap >= 0 && assigned[..from].contains(&snap) {
                        continue;
                    }
                    choices.push(Some(snap));
                } else {
                    lottery[slot] |= choice.lottery;
                    nonconverter.push(snap);
                }
            }
            plain[slot] = nonconverter;
            if !plain[slot].is_empty() {
                choices.insert(0, None);
            }
            if choices.is_empty() {
                return Some(f64::NEG_INFINITY);
            }
            classes.push(choices);
        }
        let mut cursor = vec![0; 5 - from];
        let mut upper = f64::NEG_INFINITY;
        loop {
            let mut team = Team {
                members: members.to_vec(),
                snaps: assigned.to_vec(),
            };
            let mut options = plain.clone();
            let mut valid = true;
            let mut with_supports = false;
            for slot in 0..from {
                options[slot] = vec![assigned[slot]];
                with_supports |= self
                    .kernel
                    .fixed_choice(members[slot], assigned[slot])
                    .lottery;
            }
            for slot in from..5 {
                match classes[slot - from][cursor[slot - from]] {
                    Some(snap) => {
                        if team.snaps[..slot].contains(&snap) {
                            valid = false;
                            break;
                        }
                        team.snaps[slot] = snap;
                        options[slot] = vec![snap];
                        with_supports |= self.kernel.fixed_lottery(members[slot], snap);
                    }
                    None => {
                        team.snaps[slot] = -1;
                        with_supports |= lottery[slot];
                    }
                }
            }
            if valid {
                for supports in [false, true] {
                    if supports && !with_supports {
                        continue;
                    }
                    let mut allowed = options.clone();
                    if !supports {
                        for (slot, choices) in allowed.iter_mut().enumerate() {
                            choices.retain(|&snap| !self.kernel.fixed_lottery(members[slot], snap));
                        }
                    }
                    if allowed.iter().any(Vec::is_empty) {
                        continue;
                    }
                    let value = self.fixed_class_score_ceiling(
                        &team,
                        &allowed,
                        FixedClassMode {
                            supports: Some(supports),
                            hull: false,
                        },
                        totals,
                        limit,
                    )?;
                    if value >= limit {
                        return None;
                    }
                    upper = upper.max(value);
                }
            }
            let mut at = cursor.len();
            while at > 0 {
                at -= 1;
                cursor[at] += 1;
                if cursor[at] < classes[at].len() {
                    break;
                }
                cursor[at] = 0;
            }
            if at == 0 && cursor[0] == 0 {
                break;
            }
        }
        Some(upper)
    }
    fn fixed_no_rng_score_ceiling(&mut self, team: &Team, power: f64) -> Option<f64> {
        if !self.kernel.chart.luck
            || self.kernel.seeds.is_empty()
            || team
                .members
                .iter()
                .zip(&team.snaps)
                .any(|(&member, &snap)| {
                    self.kernel.support(member, snap).is_some_and(|actor| {
                        actor.gekisou_support_skills.iter().any(|&(id, level)| {
                            self.kernel.master.gekiso_support_skills[&id]
                                .effects
                                .iter()
                                .any(|row| {
                                    row.level == level
                                        && ((11000..=11005).contains(&row.kind)
                                            || condition_rows(self.kernel.master, row)
                                                .any(|c| c.kind == 4011))
                                })
                        })
                    })
                })
        {
            return None;
        }
        if !self.fixed_no_rng_attempted {
            self.fixed_no_rng_attempted = true;
            let programs = self.kernel.fixed_luck_members(&team.members)?;
            let script = self.kernel.combo_script(&team.members)?;
            let key = FixedLuckKey {
                members: lottery_member_key(self.kernel.master, &programs),
                combo: self.kernel.combo_key(&script),
                supports: false,
                level: self.level,
            };
            if !self.kernel.fixed_luck_bounds.contains_key(&key) {
                let luck = self.kernel.fixed_luck_changes(&programs, false)?;
                let combo = self.kernel.fixed_combo_trace(&script)?;
                let mut model = crate::rewards::DeterministicBound::with_luck_trace(
                    self.kernel.master,
                    self.kernel.chart,
                    self.kernel.members,
                    self.kernel.snaps,
                    1.0 + self.level as f64,
                    combo,
                    &luck,
                )?;
                model.coefficient_limit = 256;
                model.snapshot_power(Arc::clone(&self.snapshot_power));
                if self.kernel.fixed_luck_bounds.len() >= 32 {
                    if let Some(old) = self.kernel.fixed_luck_order.pop_front() {
                        self.kernel.fixed_luck_bounds.remove(&old);
                    }
                }
                self.kernel
                    .fixed_luck_bounds
                    .insert(key.clone(), Arc::new(std::sync::Mutex::new(model)));
            }
            self.kernel.fixed_luck_order.retain(|old| old != &key);
            self.kernel.fixed_luck_order.push_back(key.clone());
            let bound = Arc::clone(self.kernel.fixed_luck_bounds.get(&key)?);
            bound
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .prepare_score_assignment(&team.members, &self.combo_options);
            self.fixed_no_rng_bound = Some(bound);
        }
        Some(
            self.fixed_no_rng_bound
                .as_ref()?
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .score_ceiling(&team.members, &team.snaps, 5, power, self.kernel.criterion),
        )
    }
    fn fixed_luck_score_ceiling(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        power: f64,
    ) -> Option<f64> {
        if !self.kernel.chart.luck || self.kernel.seeds.is_empty() {
            return None;
        }
        Some(
            self.combo_bound
                .as_ref()?
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .score_ceiling(members, assigned, from, power, self.kernel.criterion),
        )
    }
    fn shape_id(&mut self, member: usize, snap: i32) -> u32 {
        self.kernel.shape_id(member, snap)
    }
    fn prepare_numeric_patterns(
        &mut self,
        _members: &[usize],
        prefix: [i32; 5],
        from: usize,
    ) -> Option<Arc<PatternSet>> {
        let key = (self.numeric_domain, from, prefix);
        if let Some(cached) = self.kernel.patterns.get(&key) {
            return cached.clone();
        }
        let result = (|| {
            let assignment = self.assignment.as_ref()?;
            if !assignment.numeric_supported {
                return None;
            }
            fn expand(
                at: usize,
                choices: &[Vec<i32>; 5],
                snaps: &mut [i32; 5],
                result: &mut Vec<[i32; 5]>,
            ) {
                if at == 5 {
                    result.push(*snaps);
                    return;
                }
                for &snap in &choices[at] {
                    if snap >= 0 && snaps[..at].contains(&snap) {
                        continue;
                    }
                    snaps[at] = snap;
                    expand(at + 1, choices, snaps, result);
                }
            }
            let choices = std::array::from_fn(|slot| {
                assignment.chain_options[slot]
                    .iter()
                    .map(|choice| choice.snap)
                    .chain(std::iter::once(-1))
                    .collect()
            });
            let mut completions = Vec::new();
            let mut snaps = prefix;
            expand(from, &choices, &mut snaps, &mut completions);
            if from == 0 {
                self.kernel.numeric_root_work = self
                    .kernel
                    .numeric_root_work
                    .saturating_add(completions.len());
            }
            let mut sources = Vec::<Vec<Arc<crate::just_bound::Converter>>>::new();
            let mut masks = Vec::new();
            let mut unions = Vec::new();
            let mut by_program = HashMap::default();
            let mut baseline = 0;
            for snaps in completions {
                let mut selected = [None; 5];
                let mut count = 0;
                let mut mask = 0u8;
                for (slot, &snap) in snaps.iter().enumerate() {
                    if let Some(choice) = assignment.numeric_converters[slot]
                        .iter()
                        .find(|choice| choice.snap == snap)
                    {
                        selected[count] = Some(choice);
                        count += 1;
                        mask |= 1 << slot;
                    }
                }
                selected[..count].sort_unstable_by_key(|choice| choice.unwrap().order);
                let mut programs = [0usize; 5];
                for (at, choice) in selected[..count].iter().enumerate() {
                    programs[at] = choice.unwrap().prepared.program;
                }
                let next = sources.len();
                let index = *by_program.entry((programs, mask)).or_insert_with(|| {
                    sources.push(
                        selected[..count]
                            .iter()
                            .map(|choice| Arc::clone(&choice.unwrap().prepared))
                            .collect(),
                    );
                    masks.push(mask);
                    unions.push(SnapUnion::new(self.kernel.snaps.len()));
                    next
                });
                if snaps[from..].iter().all(|&snap| snap < 0) {
                    baseline = index;
                }
                for (slot, &snap) in snaps.iter().enumerate().skip(from) {
                    unions[index].extend(slot, snap, &[]);
                }
            }
            let mut compiled = Vec::with_capacity(sources.len());
            for converters in sources {
                compiled.push(
                    self.kernel
                        .just_bounds
                        .converter_upper(self.kernel.master, self.kernel.chart, &converters)
                        .ok()
                        .flatten()?,
                );
            }
            let baseline_id = compiled[baseline].id;
            let mut fine = Vec::new();
            let mut merged = Vec::new();
            let mut by_trace = HashMap::default();
            for ((base, mask), union) in compiled.into_iter().zip(masks).zip(unions) {
                let mask = if base.id == baseline_id {
                    0
                } else {
                    mask & !((1u8 << from) - 1)
                };
                let next = fine.len();
                let index = *by_trace.entry((base.id, mask)).or_insert_with(|| {
                    fine.push(CohortShape {
                        base,
                        options: std::array::from_fn(|_| Vec::new()),
                        plain: [false; 5],
                    });
                    merged.push(SnapUnion::new(self.kernel.snaps.len()));
                    next
                });
                for slot in from..5 {
                    for &snap in &union.options[slot] {
                        merged[index].add(slot, snap);
                    }
                    if union.plain[slot] {
                        merged[index].extend(slot, -1, &[]);
                    }
                }
            }
            for (shape, union) in fine.iter_mut().zip(merged) {
                shape.options = union.options;
                shape.plain = union.plain;
            }
            let mut coarse = Vec::new();
            if from < 2 {
                let mut by_trace = HashMap::default();
                let mut unions = Vec::new();
                for shape in &fine {
                    let next = coarse.len();
                    let index = *by_trace.entry(shape.base.id).or_insert_with(|| {
                        coarse.push(CohortShape {
                            base: Arc::clone(&shape.base),
                            options: std::array::from_fn(|_| Vec::new()),
                            plain: [false; 5],
                        });
                        unions.push(SnapUnion::new(self.kernel.snaps.len()));
                        next
                    });
                    for slot in from..5 {
                        for &snap in &shape.options[slot] {
                            unions[index].add(slot, snap);
                        }
                        if shape.plain[slot] {
                            unions[index].extend(slot, -1, &[]);
                        }
                    }
                }
                for (shape, union) in coarse.iter_mut().zip(unions) {
                    shape.options = union.options;
                    shape.plain = union.plain;
                }
            }
            Some(Arc::new(PatternSet { fine, coarse }))
        })();
        let bytes = 128
            + result.as_ref().map_or(0, |patterns| {
                (patterns.fine.capacity() + patterns.coarse.capacity())
                    * std::mem::size_of::<CohortShape>()
                    + patterns
                        .fine
                        .iter()
                        .chain(&patterns.coarse)
                        .flat_map(|shape| &shape.options)
                        .map(|row| row.capacity() * 4)
                        .sum::<usize>()
            });
        if bytes <= 16 * 1024 * 1024 {
            while self.kernel.pattern_bytes + bytes > 16 * 1024 * 1024 {
                let Some((old, size)) = self.kernel.pattern_order.pop_front() else {
                    break;
                };
                self.kernel.patterns.remove(&old);
                self.kernel.pattern_bytes -= size;
            }
            self.kernel.patterns.insert(key, result.clone());
            self.kernel.pattern_order.push_back((key, bytes));
            self.kernel.pattern_bytes += bytes;
        }
        result
    }
    fn prepare_numeric_cohorts(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        coarse: bool,
    ) -> Option<Arc<Vec<Cohort>>> {
        let prefix = std::array::from_fn(|slot| {
            if slot < from
                && self.kernel.bound_converters
                    [self.kernel.slot_index(members[slot], assigned[slot])]
            {
                assigned[slot]
            } else {
                -1
            }
        });
        let key = (from, prefix, coarse);
        if let Some(cached) = self.numeric_cohorts.get(&key) {
            return cached.clone();
        }
        let result = (|| {
            let patterns = self.prepare_numeric_patterns(members, prefix, from)?;
            let shapes = if coarse {
                &patterns.coarse
            } else {
                &patterns.fine
            };
            let mut cohorts = Vec::with_capacity(shapes.len());
            for shape in shapes {
                let mut options = shape.options.clone();
                let mut domains = [0; 5];
                for slot in from..5 {
                    if shape.plain[slot] {
                        options[slot].extend_from_slice(&self.assignment.as_ref()?.nonchain[slot]);
                    }
                    options[slot].sort_unstable();
                    options[slot].dedup();
                    domains[slot] = self.kernel.choice_domain(&options[slot]);
                }
                cohorts.push(Cohort {
                    base: Arc::clone(&shape.base),
                    options,
                    domains,
                });
            }
            Some(Arc::new(cohorts))
        })();
        self.numeric_cohorts.insert(key, result.clone());
        result
    }
    fn prepare_luck_cohorts(
        kernel: &mut Kernel<'_>,
        cache: &mut HashMap<(usize, [i32; 5]), Arc<Vec<LuckCohort>>>,
        assignment: &Assignment,
        members: &[usize],
        assigned: &[i32],
        from: usize,
    ) -> Arc<Vec<LuckCohort>> {
        let prefix: [i32; 5] = std::array::from_fn(|slot| {
            let snap = assigned[slot];
            if slot < from
                && snap >= 0
                && kernel.support_chain[members[slot] * kernel.snaps.len() + snap as usize]
            {
                snap
            } else {
                -1
            }
        });
        let key = (from, prefix);
        if let Some(cohorts) = cache.get(&key) {
            return Arc::clone(cohorts);
        }
        let mut groups = Vec::<LuckCohort>::new();
        let mut unions = Vec::new();
        let mut by_base = HashMap::default();
        let mut positions = [0usize; 5];
        loop {
            let choices: [Option<&ChainOption>; 5] = std::array::from_fn(|slot| {
                if slot < from {
                    assignment.chain_options[slot]
                        .iter()
                        .find(|choice| choice.snap == prefix[slot])
                } else {
                    assignment.chain_options[slot].get(positions[slot])
                }
            });
            let selected: [i32; 5] =
                std::array::from_fn(|slot| choices[slot].map_or(-1, |choice| choice.snap));
            let mut digit = 4;
            let finished = loop {
                if positions[digit] < assignment.chain_options[digit].len() {
                    positions[digit] += 1;
                    break false;
                }
                positions[digit] = 0;
                if digit == from {
                    break true;
                }
                digit -= 1;
            };
            if selected[from..].iter().all(|&snap| snap < 0)
                || (from..5)
                    .any(|slot| selected[slot] >= 0 && selected[..slot].contains(&selected[slot]))
            {
                if finished {
                    break;
                }
                continue;
            }
            let actors: Vec<_> = choices
                .iter()
                .flatten()
                .map(|choice| &choice.actor)
                .collect();
            let base = kernel.luck_base(&assignment.member_deck, &actors, &assignment.member_ids);
            let id = base.lock().unwrap_or_else(|e| e.into_inner()).choice_id;
            let mask = (from..5).fold(0u8, |mask, slot| {
                mask | (((selected[slot] >= 0) as u8) << slot)
            });
            let next = groups.len();
            let index = *by_base.entry((id, mask)).or_insert_with(|| {
                unions.push(SnapUnion::new(kernel.snaps.len()));
                groups.push(LuckCohort {
                    base,
                    options: std::array::from_fn(|_| Vec::new()),
                    domains: [0; 5],
                    selected: mask,
                });
                next
            });
            for (slot, &snap) in selected.iter().enumerate().skip(from) {
                unions[index].extend(slot, snap, &assignment.nonchain[slot]);
            }
            if finished {
                break;
            }
        }
        for (group, union) in groups.iter_mut().zip(unions) {
            group.options = union.options;
            for slot in from..5 {
                group.options[slot].sort_unstable();
                group.options[slot].dedup();
                group.domains[slot] = kernel.choice_domain(&group.options[slot]);
            }
        }
        let groups = Arc::new(groups);
        if cache.len() >= 128 {
            cache.clear();
        }
        cache.insert(key, Arc::clone(&groups));
        groups
    }
    fn prepare_luck_mission_cohorts(
        kernel: &mut Kernel<'_>,
        assignment: &Assignment,
        members: &[usize],
    ) -> Option<Vec<LuckCohort>> {
        if !kernel.healthy || !kernel.owner_local {
            return None;
        }
        let mut classes = HashMap::<Vec<i32>, usize>::default();
        let mut snap_classes = HashMap::default();
        for chain in &assignment.chains {
            let Some(actor) = chain.future[2].first() else {
                continue;
            };
            let mut missions: Vec<_> = actor
                .0
                .gekisou_support_skills
                .iter()
                .filter_map(|&(id, level)| {
                    let skill = &kernel.master.gekiso_support_skills[&id];
                    skill
                        .effects
                        .iter()
                        .any(|r| {
                            r.level == level
                                && ((11000..=11005).contains(&r.kind)
                                    || r.kind == 12006
                                    || r.kind == 13005)
                        })
                        .then_some(skill.mission_type)
                })
                .collect();
            missions.sort_unstable();
            missions.dedup();
            if missions.is_empty() {
                return None;
            }
            let next = classes.len() + 1;
            let class = *classes.entry(missions).or_insert(next);
            snap_classes.insert(chain.snap, class);
        }
        let labels: [Vec<usize>; 3] = std::array::from_fn(|at| {
            let slot = at + 2;
            let mut values: Vec<_> = assignment.chain_options[slot]
                .iter()
                .filter_map(|choice| snap_classes.get(&choice.snap).copied())
                .collect();
            if !assignment.nonchain[slot].is_empty() {
                values.push(0);
            }
            values.sort_unstable();
            values.dedup();
            values
        });
        let coarse = labels.iter().fold(1usize, |n, v| n.saturating_mul(v.len()));
        let exact = (2..5).fold(1usize, |n, slot| {
            n.saturating_mul(
                assignment.chain_options[slot].len()
                    + usize::from(!assignment.nonchain[slot].is_empty()),
            )
        });
        if coarse >= exact {
            return None;
        }
        let mut groups = Vec::new();
        for &a in &labels[0] {
            for &b in &labels[1] {
                for &c in &labels[2] {
                    let labels = [a, b, c];
                    if labels == [0; 3] {
                        continue;
                    }
                    let options: [Vec<i32>; 5] = std::array::from_fn(|slot| {
                        if slot < 2 {
                            Vec::new()
                        } else if labels[slot - 2] == 0 {
                            assignment.nonchain[slot].clone()
                        } else {
                            assignment.chain_options[slot]
                                .iter()
                                .filter(|choice| snap_classes[&choice.snap] == labels[slot - 2])
                                .map(|choice| choice.snap)
                                .collect()
                        }
                    });
                    let choices: Vec<_> = assignment
                        .chains
                        .iter()
                        .filter(|chain| {
                            snap_classes
                                .get(&chain.snap)
                                .is_some_and(|class| labels.contains(class))
                        })
                        .map(|chain| kernel.chain_choice(chain.snap, members, &options))
                        .collect();
                    let actors: Vec<_> = choices
                        .iter()
                        .flat_map(|chain| chain.future[2].iter())
                        .collect();
                    let base =
                        kernel.luck_base(&assignment.member_deck, &actors, &assignment.member_ids);
                    let mut domains = [0; 5];
                    for (domain, options) in domains[2..].iter_mut().zip(&options[2..]) {
                        *domain = kernel.choice_domain(options);
                    }
                    let selected = (2..5).fold(0u8, |mask, slot| {
                        mask | (u8::from(labels[slot - 2] != 0) << slot)
                    });
                    groups.push(LuckCohort {
                        base,
                        options,
                        domains,
                        selected,
                    });
                }
            }
        }
        Some(groups)
    }
    fn plain_region_upper(
        kernel: &mut Kernel<'_>,
        cohorts: &[LuckCohort],
        assignment: &Assignment,
        prefix: &AssignmentPrefix<'_>,
        start: usize,
        limits: (f64, f64),
    ) -> f64 {
        let (cutoff, mut upper) = limits;
        for cohort in cohorts {
            let rows: [&[i32]; 5] = std::array::from_fn(|slot| {
                if slot >= start && slot < 2 {
                    &assignment.nonchain[slot]
                } else if slot >= 2 {
                    &cohort.options[slot]
                } else {
                    prefix.options[slot]
                }
            });
            let mut domains = cohort.domains;
            domains[start..2].copy_from_slice(&assignment.nonchain_ids[start..2]);
            let part = AssignmentPrefix {
                options: &rows,
                option_ids: &domains,
                selected_slots: cohort.selected,
                ..*prefix
            };
            upper = upper.max(kernel.context_partition_bound(&cohort.base, &part, false, cutoff));
            if upper >= cutoff {
                break;
            }
        }
        upper
    }
    fn numeric_assignment_bound(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        limit: f64,
    ) -> Option<f64> {
        if self.kernel.chart.luck {
            return None;
        }
        let mut used = vec![false; self.kernel.snaps.len()];
        for &snap in &assigned[..from] {
            if snap >= 0 {
                used[snap as usize] = true;
            }
        }
        let global_work = self
            .kernel
            .chart
            .notes
            .len()
            .saturating_mul(self.kernel.members.len())
            .saturating_mul(self.kernel.snaps.len().max(1));
        // Compare local converter planning with the chart-wide note/profile grid before preparing Max's envelope.
        if from == 0
            && (self.kernel.criterion != Criterion::Max
                || self.kernel.numeric_root_work >= global_work)
        {
            if let Some(hull) = self.kernel.prepare_numeric_hull() {
                let assignment = self.assignment.as_ref()?;
                let rows = std::array::from_fn(|slot| crate::just_bound::SourceRow {
                    member: members[slot],
                    snaps: &assignment.options[slot],
                    domain: (assignment.option_ids[slot] as u64) << 1,
                    fixed: false,
                });
                let input = crate::just_bound::Assignment {
                    actors: &self.kernel.bound_actors,
                    slots: &self.kernel.slots,
                    powers: &self.snapshot_power,
                    width: self.power_width,
                    rows: &rows,
                    used: &used,
                    fixed_power: self.base_power,
                    criterion: self.kernel.criterion,
                    cutoff: limit,
                    base: &hull[0],
                };
                let upper = self
                    .kernel
                    .just_bounds
                    .assignment_hull_upper(self.kernel.master, self.kernel.chart, &hull, &input)
                    .ok()
                    .flatten()?;
                if upper < limit {
                    return Some(upper);
                }
            }
        }
        for coarse in [true, false] {
            if coarse && from >= 2 {
                continue;
            }
            let cohorts = self.prepare_numeric_cohorts(members, assigned, from, coarse)?;
            let mut complete = true;
            let mut upper = f64::NEG_INFINITY;
            for cohort in cohorts.iter() {
                let rows = std::array::from_fn(|slot| {
                    if slot < from {
                        crate::just_bound::SourceRow {
                            member: members[slot],
                            snaps: &assigned[slot..slot + 1],
                            domain: (((assigned[slot] + 1) as u64) << 1) | 1,
                            fixed: true,
                        }
                    } else {
                        crate::just_bound::SourceRow {
                            member: members[slot],
                            snaps: &cohort.options[slot],
                            domain: (cohort.domains[slot] as u64) << 1,
                            fixed: false,
                        }
                    }
                });
                let input = crate::just_bound::Assignment {
                    actors: &self.kernel.bound_actors,
                    slots: &self.kernel.slots,
                    powers: &self.snapshot_power,
                    width: self.power_width,
                    rows: &rows,
                    used: &used,
                    fixed_power: self.base_power,
                    criterion: self.kernel.criterion,
                    cutoff: limit,
                    base: &cohort.base,
                };
                let value = self
                    .kernel
                    .just_bounds
                    .assignment_upper(self.kernel.master, self.kernel.chart, &input)
                    .ok()
                    .flatten()?;
                if value >= limit {
                    if !coarse {
                        return None;
                    }
                    complete = false;
                    break;
                }
                upper = upper.max(value);
            }
            if complete {
                return Some(upper);
            }
        }
        None
    }
    fn member_constant(&mut self, members: &[usize], left: usize) -> Option<f64> {
        if !self.kernel.chart.luck || self.surrogate {
            return None;
        }
        if self.kernel.read_all.iter().any(|&field| field != "bandId") {
            return Some(f64::INFINITY);
        }
        let level = self.level;
        let mut keys = [0; 5];
        for (slot, &member) in members.iter().enumerate() {
            let key = self.kernel.member_bound_ids[member];
            if key == u32::MAX {
                return None;
            }
            keys[slot] = key;
        }
        keys.sort_unstable();
        let key = (level, keys, left);
        let base = if let Some(&base) = self.kernel.member_bases.get(&key) {
            base
        } else {
            let mut copies: HashMap<Performer, Vec<Performer>> = HashMap::default();
            let mut order = Vec::new();
            for p in &self.kernel.luck_members {
                let key = shape(p);
                if !copies.contains_key(&key) {
                    order.push(key.clone());
                }
                let list = copies.entry(key).or_default();
                if list.len() < left {
                    list.push(p.clone());
                }
            }
            let mut bound_deck: Vec<_> = members
                .iter()
                .map(|&member| {
                    self.kernel.member_bound_decks[member]
                        .as_ref()
                        .unwrap()
                        .clone()
                })
                .collect();
            for key in order {
                bound_deck.extend(copies.remove(&key).unwrap());
            }
            bound_deck.extend(self.kernel.luck_supports.iter().cloned());
            if level > 0 && self.kernel.maximum_level > 0 {
                bound_deck.push(Performer {
                    gekisou_skill: Some((-1, 1)),
                    gekisou_mission_type: 1,
                    ..Performer::default()
                });
            }
            let level_data = self.kernel.level(level);
            let within = &level_data.within;
            let base = self
                .kernel
                .partition_weights(&bound_deck, Some(within), level)
                .map(|(weights, _)| weights.base)
                .unwrap_or_else(|| {
                    self.kernel
                        .weights_of(&bound_deck, Some(within), level)
                        .base
                });
            self.kernel.member_bases.insert(key, base);
            base
        };
        Some(base)
    }
    fn bounded_minimum(&self) -> bool {
        self.kernel.criterion == Criterion::Min
            && self.kernel.chart.luck
            && self.kernel.seeds.is_empty()
            && !self.surrogate
            && !self.estimate
    }
    fn global_row(&mut self, member: usize) -> [f64; 5] {
        if let Some(&row) = self.global_rows.get(&member) {
            return row;
        }
        let mut row = [0.0_f64; 5];
        for snap in -1..self.kernel.snaps.len() as i32 {
            if let Some(weights) = self.event_rows.get(&self.kernel.slot_index(member, snap)) {
                for event in 0..5 {
                    row[event] = row[event].max(weights[event]);
                }
            }
        }
        self.global_rows.insert(member, row);
        row
    }
    fn max_score(&self, power: f64, weight: f64) -> f64 {
        power * (self.data.as_ref().unwrap().weights.base + weight)
            / self.kernel.chart.full.converted as f64
            * (1.0 + RELATIVE)
            + (self.kernel.chart.notes.len() + 4) as f64
    }
    fn order_upper(&self, team: &Team, power: f64) -> f64 {
        if let Some(bound) = &self.deterministic {
            return bound.score(Totals {
                power,
                skill: team
                    .members
                    .iter()
                    .enumerate()
                    .map(|(slot, &member)| bound.weight(member, team.snaps[slot], false))
                    .sum(),
                ..Totals::default()
            });
        }
        let rows = std::array::from_fn(|slot| {
            self.event_rows[&self.kernel.slot_index(team.members[slot], team.snaps[slot])]
        });
        self.max_score(power, best_order(&rows))
    }
    fn high(&self, t: Totals) -> f64 {
        if let Some(bound) = &self.deterministic {
            return bound.score(t);
        }
        if (!self.kernel.chart.luck || !self.kernel.seeds.is_empty())
            && !self.surrogate
            && !self.estimate
        {
            return f64::INFINITY;
        }
        t.power * (self.data.as_ref().unwrap().weights.base + t.skill)
            / self.kernel.chart.full.converted as f64
            * (1.0 + RELATIVE)
            + (self.kernel.chart.notes.len() + 4) as f64
    }
}
impl Objective for Adapter<'_, '_> {
    fn live_floor(&self) -> bool {
        self.kernel.live_floor && !self.surrogate && !self.estimate
    }
    fn score_ties(&self) -> bool {
        !self.estimate && !self.surrogate && self.kernel.criterion != Criterion::Mean
    }
    fn complete_score_tie(&mut self, hit: &mut Hit) -> Result<(), String> {
        self.kernel.complete_score_tie(hit)
    }

    fn preserve_snap_options(&self) -> bool {
        (!self.kernel.chart.luck || !self.kernel.seeds.is_empty())
            && !self.surrogate
            && !self.estimate
            && self.deterministic.is_none()
    }
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn snap_dominance(&mut self, member: usize, snap: i32) -> Vec<f64> {
        let shape = self.shape_id(member, snap) as f64;
        vec![shape, -shape]
    }
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        self.shape_id(member, a) == self.shape_id(member, b)
    }
    fn canonical_leader(&self) -> bool {
        !self.kernel.chart.luck || self.kernel.seeds.is_empty()
    }
    fn needs_power_cap(&self) -> bool {
        !self.surrogate && !self.estimate
    }
    fn prepare_dominance(&mut self, cap: f64) {
        self.dominance_margin = self
            .deterministic
            .as_ref()
            .map_or(f64::INFINITY, |b| b.dominance_margin(cap));
    }
    fn numeric_snap_dominance(
        &mut self,
        member: usize,
        better: i32,
        worse: i32,
        delta: f64,
    ) -> bool {
        if delta <= 0.0 || !self.dominance_margin.is_finite() || !self.kernel.healthy {
            return false;
        }
        if self.kernel.just_context.is_none() || !self.kernel.owner_local {
            return false;
        }
        if !crate::just_bound::slot_dominates(
            self.kernel.slot(member, better),
            self.kernel.slot(member, worse),
        ) {
            return false;
        }
        let a = self.kernel.bound_actor_of(member, better);
        let b = self.kernel.bound_actor_of(member, worse);
        if !self
            .kernel
            .just_bounds
            .score_dominates(self.kernel.master, &a, &b)
        {
            return false;
        }
        if delta > self.dominance_margin {
            return true;
        }
        if !self
            .deterministic
            .as_ref()
            .unwrap()
            .can_credit_baseline(member, better)
        {
            return false;
        }
        let gain = self
            .kernel
            .just_bounds
            .baseline_gain_floor(self.kernel.master, self.kernel.chart, &a, &b)
            .unwrap_or(0.0);
        self.deterministic
            .as_ref()
            .unwrap()
            .dominates_baseline_gain(delta, gain)
    }
    fn numeric_member_dominance(&mut self, better: usize, worse: usize, power_delta: f64) -> bool {
        if power_delta <= 0.0 {
            return false;
        }
        if let Some(&answer) = self.member_dominance.get(&(better, worse)) {
            return answer;
        }
        if self.kernel.healthy
            && self.kernel.owner_local
            && self.kernel.just_context.is_some()
            && power_delta > self.dominance_margin
        {
            let answer = (-1..self.kernel.snaps.len() as i32).all(|snap| {
                if !crate::just_bound::slot_dominates(
                    self.kernel.slot(better, snap),
                    self.kernel.slot(worse, snap),
                ) {
                    return false;
                }
                let a = self.kernel.bound_actor_of(better, snap);
                let b = self.kernel.bound_actor_of(worse, snap);
                self.kernel
                    .just_bounds
                    .score_dominates(self.kernel.master, &a, &b)
            });
            if answer {
                return true;
            }
        }
        let answer = (-1..self.kernel.snaps.len() as i32).all(|snap| {
            if self.kernel.slot(better, snap).signature()
                != self.kernel.slot(worse, snap).signature()
            {
                return false;
            }
            let p = self.kernel.actor_of(better, snap);
            let q = self.kernel.actor_of(worse, snap);
            p.gekisou_skill == q.gekisou_skill
                && p.mission_only == q.mission_only
                && p.gekisou_support_skills == q.gekisou_support_skills
                && self
                    .kernel
                    .read_all
                    .iter()
                    .all(|name| field(&p, name) == field(&q, name))
        });
        self.member_dominance.insert((better, worse), answer);
        answer
    }

    fn skill(&mut self, m: usize, s: i32) -> (f64, f64) {
        if let Some(bound) = &self.deterministic {
            return (
                0.0,
                bound.weight(m, s, self.kernel.criterion != Criterion::Max),
            );
        }
        if self.preserve_snap_options() {
            return (0.0, 0.0);
        }
        let index = self.kernel.slot_index(m, s);
        if let Some(&value) = self.ranges.get(&index) {
            return value;
        }
        let gain = self.kernel.support_high(m, s, self.level)
            + if self.surrogate {
                self.kernel.member_gain[m]
            } else {
                0.0
            };
        let maximum = self.kernel.criterion == Criterion::Max;
        let top = if maximum || self.bounded_minimum() {
            let mut row = self
                .kernel
                .event_weights(self.kernel.slot(m, s), &self.data.as_ref().unwrap().weights);
            let mean = if maximum {
                0.0
            } else {
                row.iter().fold(0.0, |sum, value| sum + value) / 5.0 + gain
            };
            for value in &mut row {
                *value = (*value + gain).next_up();
            }
            let top = if maximum {
                row.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            } else {
                mean
            };
            self.event_rows.insert(index, row);
            top
        } else {
            self.kernel.live_weight(
                self.kernel.slot(m, s),
                &self.data.as_ref().unwrap().weights,
                Criterion::Mean,
            ) + gain
        };
        let value = (
            if self.surrogate {
                top
            } else {
                self.kernel.low_of(m, s)
            },
            top,
        );
        self.ranges.insert(index, value);
        value
    }
    fn prepare_power(&mut self, power: Arc<Vec<f64>>, width: usize) {
        if let Some(bound) = &mut self.deterministic {
            bound.snapshot_power(Arc::clone(&power));
        }
        self.snapshot_power = power;
        self.power_width = width;
    }
    fn prepare_member_domains(&mut self, power: &[f64], options: &[Vec<i32>]) {
        self.roster_last = None;
        self.roster_price = None;
        if self.generic_assignment_product() {
            return;
        }
        if let Some(entry) = self
            .roster_prices
            .iter()
            .find(|entry| entry.power == power && entry.options == options)
        {
            self.roster_price = Some(Arc::clone(&entry.pricing));
            return;
        }
        if let Some(pricing) = self
            .deterministic
            .as_ref()
            .and_then(|bound| bound.roster_pricing(power, options, self.kernel.members, None))
        {
            let pricing = Arc::new(pricing);
            self.roster_price = Some(Arc::clone(&pricing));
            if self.roster_prices.len() == 8 {
                self.roster_prices.pop_front();
            }
            self.roster_prices.push_back(RosterPriceEntry {
                power: power.to_vec(),
                options: options.to_vec(),
                pricing,
            });
        }
    }
    fn prepare_member_domains_at(&mut self, leader: usize, power: &[f64], options: &[Vec<i32>]) {
        self.roster_leader = Some(leader);
        self.roster_extra_ready = false;
        self.roster_extra = None;
        self.prepare_member_domains(power, options);
    }
    fn prepare_assignment_power(&mut self, power: f64) {
        self.plain_second_bounds.clear();
        self.plain_pair_bound = None;
        self.base_power = power;
    }
    fn assignment_options(
        &self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        limit: f64,
    ) -> Option<Vec<i32>> {
        if from != 1
            || !self.kernel.chart.luck
            || self.surrogate
            || self.estimate
            || !self.kernel.seeds.is_empty()
        {
            return None;
        }
        let plain = assigned[0] < 0
            || !self.kernel.support_chain
                [members[0] * self.kernel.snaps.len() + assigned[0] as usize];
        let pair = if plain { self.plain_pair_bound } else { None };
        let upper = self
            .plain_second_bounds
            .get(&assigned[0])
            .copied()
            .into_iter()
            .chain(pair)
            .reduce(f64::min);
        if upper.is_some_and(|upper| upper < limit) {
            return Some(
                self.assignment.as_ref()?.chain_options[1]
                    .iter()
                    .map(|choice| choice.snap)
                    .collect(),
            );
        }
        None
    }
    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        self.roster_last = None;
        if let Some(bound) = &mut self.deterministic {
            bound.prepare_members(characters);
            self.roster_suffix = self.roster_price.as_ref().map(|pricing| {
                crate::rewards::RosterSuffix::new(
                    Arc::clone(pricing),
                    characters,
                    &self.kernel.bonuses,
                    self.level,
                )
            });
            return;
        }
        if self.preserve_snap_options() {
            return;
        }
        if self.kernel.criterion != Criterion::Max && !self.bounded_minimum() {
            return;
        }
        self.suffix_rows = vec![[0.0; 5]; characters.len() + 1];
        for from in (0..characters.len()).rev() {
            let mut row = self.suffix_rows[from + 1];
            for &m in &characters[from] {
                let weights = self.global_row(m);
                for event in 0..5 {
                    row[event] = row[event].max(weights[event]);
                }
            }
            self.suffix_rows[from] = row;
        }
    }
    fn early_member_bound(&mut self, members: &[usize], from: usize, limit: f64) -> Option<f64> {
        self.roster_upper(members, from, limit)
    }
    fn member_bound_at(
        &mut self,
        members: &[usize],
        t: Totals,
        left: usize,
        from: usize,
        limit: f64,
    ) -> Option<f64> {
        if let Some(bound) = &mut self.deterministic {
            let mut upper =
                bound.member_score_ceiling(members, from, t.power, self.kernel.criterion);
            if upper >= limit {
                if let Some(roster) = self.roster_upper(members, from, limit) {
                    upper = upper.min(roster);
                }
            }
            return Some(upper);
        }
        if self.preserve_snap_options() {
            return Some(f64::INFINITY);
        }
        if self.kernel.criterion != Criterion::Max && !self.bounded_minimum() {
            return self.member_bound(members, t, left);
        }
        let rows = std::array::from_fn(|slot| {
            if slot < members.len() {
                self.global_row(members[slot])
            } else {
                self.suffix_rows[from]
            }
        });
        let weight = if self.bounded_minimum() {
            minimum_order_upper(&rows)
        } else {
            best_order(&rows)
        };
        let upper = self.max_score(t.power, weight);
        if upper < limit {
            return Some(upper);
        }
        Some(
            self.member_bound(
                members,
                Totals {
                    skill: if self.kernel.seeds.is_empty() {
                        t.skill.min(weight)
                    } else {
                        t.skill
                    },
                    ..t
                },
                left,
            )
            .map_or(upper, |bound| upper.min(bound)),
        )
    }
    fn assignment_order_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        self.refine_cutoff = _limit;
        if self.fixed_box
            && self.kernel.seeds.is_empty()
            && self.kernel.criterion != Criterion::Min
            && from == 0
        {
            if self.fixed_root_checked {
                return None;
            }
            self.fixed_root_checked = true;
            return self.assignment_bound(members, t, assigned, from);
        }
        if self.fixed_box && !self.kernel.seeds.is_empty() {
            if from == 0 && self.fixed_root_checked {
                return None;
            }
            let result = self.fixed_box_score_ceiling(members, assigned, from, t, _limit);
            if from == 0 {
                self.fixed_root_checked = true;
            }
            return result;
        }
        if from == 0
            && self.kernel.chart.luck
            && !self.kernel.seeds.is_empty()
            && self.kernel.healthy
            && self.kernel.owner_local
        {
            let mut known = true;
            let mut supported = true;
            for (slot, choices) in self.combo_options.iter().enumerate() {
                let mut converters = false;
                for &snap in choices {
                    let choice = self.kernel.fixed_choice(members[slot], snap);
                    supported &= choice.supported;
                    converters |= choice.converter.is_some();
                }
                known &= choices.len() == 1 || !converters;
            }
            if supported && (known || self.kernel.fixed_just_hull(5).is_some()) {
                self.fixed_box_complete = known;
                return self.fixed_box_score_ceiling(members, assigned, from, t, _limit);
            }
        }
        let deterministic = self.deterministic.as_mut().map(|bound| {
            bound.score_ceiling(members, assigned, from, t.power, self.kernel.criterion)
        });
        if let Some(mut upper) = deterministic {
            if upper > _limit {
                upper = upper.min(self.deterministic.as_ref().unwrap().joint_score_ceiling(
                    members,
                    assigned,
                    from,
                    self.base_power,
                    (t, _limit),
                    self.kernel.criterion,
                ));
            }
            if upper > _limit {
                let bound = self.deterministic.as_mut().unwrap();
                if let Some(partitions) =
                    bound.converter_partitions(members, assigned, from, self.base_power, t.power)
                {
                    let mut partition_upper = f64::NEG_INFINITY;
                    for part in partitions {
                        partition_upper = partition_upper
                            .max(bound.partition_score(&part, self.kernel.criterion));
                        if partition_upper > _limit {
                            break;
                        }
                    }
                    if partition_upper <= _limit {
                        return Some(partition_upper);
                    }
                }
            }
            if upper > _limit {
                if let Some(value) = self.scripted_combo_bound(members, assigned, from, t, _limit) {
                    upper = upper.min(value);
                }
            }
            if upper > _limit && (2..=4).contains(&from) {
                if let Some(value) =
                    self.fixed_tail_score_ceiling(members, assigned, from, t, _limit)
                {
                    upper = upper.min(value);
                }
            }
            if upper > _limit && from < 5 {
                if let Some(value) = self.numeric_assignment_bound(members, assigned, from, _limit)
                {
                    upper = upper.min(value);
                }
            }
            return Some(upper);
        }
        if self.preserve_snap_options() {
            return None;
        }
        if deterministic.is_some_and(|upper| upper < _limit) {
            return deterministic;
        }
        if self.kernel.criterion != Criterion::Max && !self.bounded_minimum() {
            return deterministic;
        }
        let base = self
            .data
            .as_ref()
            .unwrap()
            .weights
            .base
            .min(self.nominal_base.unwrap_or(f64::INFINITY));
        let lambda = ((base + t.skill) / t.power.max(1.0)).sqrt();
        if !lambda.is_finite() || lambda <= 0.0 {
            return None;
        }
        let scales: &[f64] = if self.bounded_minimum() && from <= 2 {
            &[1.0, 0.9, 1.1]
        } else {
            &[1.0]
        };
        let mut upper = f64::INFINITY;
        for &scale in scales {
            let lambda = lambda * scale;
            let mut rows = [[0.0; 5]; 5];
            for slot in 0..5 {
                let m = members[slot];
                if slot < from {
                    let s = assigned[slot];
                    let power = self.snapshot_power[m * self.power_width
                        + if s < 0 {
                            self.power_width - 1
                        } else {
                            s as usize
                        }];
                    let weights = self.event_rows[&self.kernel.slot_index(m, s)];
                    for (event, &weight) in weights.iter().enumerate() {
                        rows[slot][event] =
                            ((lambda * power).next_up() + (weight / lambda).next_up()).next_up();
                    }
                } else {
                    for (event, value) in rows[slot].iter_mut().enumerate() {
                        *value = self.max_frontiers[m][event]
                            .iter()
                            .map(|&(power, weight)| {
                                ((lambda * power).next_up() + (weight / lambda).next_up()).next_up()
                            })
                            .fold(0.0, f64::max);
                    }
                }
            }
            let order = if self.bounded_minimum() {
                minimum_order_upper(&rows)
            } else {
                best_order(&rows)
            };
            let sum = ((lambda * self.base_power).next_up() + (base / lambda).next_up() + order)
                .next_up();
            let candidate =
                ((sum * sum).next_up() / (4.0 * self.kernel.chart.full.converted as f64)).next_up()
                    * (1.0 + RELATIVE)
                    + (self.kernel.chart.notes.len() + 4) as f64;
            upper = upper.min(candidate);
            if from <= 2 && upper >= _limit {
                let minimum = self.bounded_minimum();
                let events = if minimum {
                    crate::assignment::minimum_order(&rows)
                } else {
                    [0; 5]
                };
                let prices = if minimum {
                    [0.0; 5]
                } else {
                    crate::assignment::event_prices(&rows)
                };
                let columns = self.power_width - 1 + 5;
                let bad = -1e100;
                if self.kernel.context_matrix.is_empty() {
                    self.kernel.context_matrix = vec![vec![bad; columns]; 5];
                }
                for (slot, &member) in members.iter().enumerate() {
                    self.kernel.context_matrix[slot].fill(bad);
                    let choices = if slot < from {
                        std::slice::from_ref(&assigned[slot])
                    } else {
                        &self.max_choices[member]
                    };
                    for &snap in choices {
                        let power = self.snapshot_power[member * self.power_width
                            + if snap < 0 {
                                self.power_width - 1
                            } else {
                                snap as usize
                            }];
                        let weights = self.event_rows[&self.kernel.slot_index(member, snap)];
                        let price = |event: usize| {
                            (((lambda * power).next_up() + (weights[event] / lambda).next_up())
                                .next_up()
                                - prices[event])
                                .next_up()
                        };
                        let value = if minimum {
                            price(events[slot])
                        } else {
                            (0..5).map(price).fold(f64::NEG_INFINITY, f64::max)
                        };
                        let column = if snap < 0 {
                            self.power_width - 1 + slot
                        } else {
                            snap as usize
                        };
                        self.kernel.context_matrix[slot][column] = value;
                    }
                }
                let variable = self
                    .kernel
                    .context_matching
                    .maximum_upper(&self.kernel.context_matrix, columns);
                if variable < bad / 2.0 {
                    return Some(f64::NEG_INFINITY);
                }
                let prices = prices.into_iter().fold(0.0, |a, b| (a + b).next_up());
                let sum = (((lambda * self.base_power).next_up() + (base / lambda).next_up())
                    .next_up()
                    + (variable + prices).next_up())
                .next_up();
                let matched = ((sum * sum).next_up()
                    / (4.0 * self.kernel.chart.full.converted as f64))
                    .next_up()
                    * (1.0 + RELATIVE)
                    + (self.kernel.chart.notes.len() + 4) as f64;
                upper = upper.min(matched);
            }
            if upper < _limit {
                break;
            }
        }
        Some(deterministic.map_or(upper, |bound| upper.min(bound)))
    }
    fn roster_bound(&mut self, members: &[usize], totals: Totals, limit: f64) -> Option<f64> {
        if let Some(bound) = &mut self.deterministic {
            let upper = bound.member_score_ceiling(members, 0, totals.power, self.kernel.criterion);
            return Some(
                upper.min(
                    self.roster_price
                        .as_ref()
                        .map_or(f64::INFINITY, |pricing| pricing.upper(members, limit)),
                ),
            );
        }
        if !self.kernel.chart.luck
            || self.kernel.criterion != Criterion::Max && !self.bounded_minimum()
            || !self.kernel.seeds.is_empty()
            || self.surrogate
            || self.estimate
        {
            return None;
        }
        let rows = std::array::from_fn(|slot| self.global_row(members[slot]));
        let skill = if self.bounded_minimum() {
            minimum_order_upper(&rows)
        } else {
            best_order(&rows)
        };
        let upper = self.max_score(totals.power, skill);
        if upper < limit {
            return Some(upper);
        }
        let base = self.member_constant(members, 0)?;
        Some(upper.min(
            totals.power * (base + skill) / self.kernel.chart.full.converted as f64
                * (1.0 + RELATIVE)
                + (self.kernel.chart.notes.len() + 4) as f64,
        ))
    }
    fn member_bonus(&self, m: usize) -> f64 {
        self.kernel.bonuses[m] as f64
    }
    fn bound(&mut self, t: Totals) -> f64 {
        self.high(t)
    }
    fn cheap_assignment_bound(
        &mut self,
        members: &[usize],
        totals: Totals,
        assigned: &[i32],
        from: usize,
    ) -> f64 {
        if let Some(bound) = &mut self.deterministic {
            let mut upper =
                bound.score_ceiling(members, assigned, from, totals.power, self.kernel.criterion);
            if from == 0 && self.kernel.chart.luck && !self.kernel.seeds.is_empty() {
                upper = upper.min(bound.physical_score_ceiling(
                    members,
                    self.base_power,
                    totals,
                    self.kernel.criterion,
                ));
            }
            self.fixed_luck_score_ceiling(members, assigned, from, totals.power)
                .map_or(upper, |value| upper.min(value))
        } else if let Some(base) = self.nominal_base {
            let base = base.min(self.data.as_ref().unwrap().weights.base);
            totals.power * (base + totals.skill) / self.kernel.chart.full.converted as f64
                * (1.0 + RELATIVE)
                + (self.kernel.chart.notes.len() + 4) as f64
        } else {
            self.high(totals)
        }
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<crate::search::JointTarget>> {
        if !limit.is_finite() {
            return None;
        }
        if let Some(bound) = &self.deterministic {
            return Some(vec![crate::search::JointTarget {
                bonus: 0.0,
                product: bound.product_floor(limit),
            }]);
        }
        if self.preserve_snap_options() {
            return None;
        }
        Some(vec![crate::search::JointTarget {
            bonus: 0.0,
            product: ((limit - (self.kernel.chart.notes.len() + 4) as f64).max(0.0)
                * self.kernel.chart.full.converted as f64
                / (1.0 + RELATIVE))
                .next_down()
                .max(0.0),
        }])
    }
    fn member_snapshot_prices(&self) -> bool {
        self.kernel.chart.luck
            && !self.kernel.seeds.is_empty()
            && self.deterministic.is_some()
            && !self.surrogate
            && !self.estimate
    }
    fn generic_assignment_product(&self) -> bool {
        !(self.kernel.chart.luck
            && self.kernel.seeds.len() >= 16
            && self.kernel.criterion == Criterion::Max
            && self.deterministic.is_some()
            && !self.surrogate
            && !self.estimate)
    }
    fn product_base(&self) -> Option<f64> {
        if let Some(bound) = &self.deterministic {
            return Some(bound.base);
        }
        if self.preserve_snap_options() {
            return None;
        }
        Some(self.data.as_ref().unwrap().weights.base)
    }
    fn leaf_bound(&mut self, team: &Team, t: Totals) -> Option<f64> {
        if let Some(bound) = &mut self.deterministic {
            let upper = bound.score_ceiling(
                &team.members,
                &team.snaps,
                5,
                t.power,
                self.kernel.criterion,
            );
            let tight = self
                .fixed_luck_score_ceiling(&team.members, &team.snaps, 5, t.power)
                .map_or(upper, |value| upper.min(value));
            let converted = self.fixed_converted_score_ceiling(team, t.power);
            let no_rng = if converted.is_none() {
                self.fixed_no_rng_score_ceiling(team, t.power)
            } else {
                None
            };
            let tight = no_rng.map_or(tight, |value| tight.min(value));
            let tight = converted.map_or(tight, |value| tight.min(value));
            return Some(tight);
        }
        if self.preserve_snap_options() {
            return None;
        }
        if self.kernel.criterion != Criterion::Max || self.surrogate {
            return None;
        }
        let rows = std::array::from_fn(|slot| {
            self.kernel.event_weights(
                self.kernel.slot(team.members[slot], team.snaps[slot]),
                &self.data.as_ref().unwrap().weights,
            )
        });
        let separate = rows.iter().fold(0.0, |sum, row| {
            sum + row.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        });
        let assigned = best_order(&rows);
        let slack = if self.kernel.chart.accuracy.great > 0.0 {
            10.0 * REFINE_SLACK
        } else {
            REFINE_SLACK
        };
        let refined = self.kernel.refine(team, t.power) * (1.0 + slack)
            + (self.kernel.chart.notes.len() + 4) as f64;
        Some(
            self.high(Totals {
                skill: t.skill - separate + assigned,
                ..t
            })
            .min(refined),
        )
    }
    fn member_bound(&mut self, members: &[usize], t: Totals, left: usize) -> Option<f64> {
        let base = self.member_constant(members, left)?;
        Some(
            t.power * (base + t.skill) / self.kernel.chart.full.converted as f64 * (1.0 + RELATIVE)
                + (self.kernel.chart.notes.len() + 4) as f64,
        )
    }
    fn assignment_domains(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
    ) -> Option<Vec<Vec<Vec<i32>>>> {
        if self.kernel.chart.luck
            && self.kernel.seeds.is_empty()
            && self.kernel.criterion != Criterion::Min
            && self.kernel.healthy
            && self.kernel.owner_local
            && !self.surrogate
            && !self.estimate
            && !self.preserve_snap_options()
        {
            let domains = members
                .iter()
                .zip(options)
                .map(|(&member, choices)| {
                    let mut groups = [Vec::new(), Vec::new()];
                    for &snap in choices {
                        let chain = snap >= 0
                            && self.kernel.support_chain
                                [member * self.kernel.snaps.len() + snap as usize];
                        groups[usize::from(chain)].push(snap);
                    }
                    groups
                        .into_iter()
                        .filter(|group| !group.is_empty())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            return domains
                .iter()
                .any(|groups| groups.len() > 1)
                .then_some(domains);
        }
        if !self.kernel.chart.luck
            || self.kernel.seeds.is_empty()
            || !self.kernel.healthy
            || !self.kernel.owner_local
        {
            return None;
        }
        let mut plain = vec![Vec::new(); members.len()];
        let mut candidates = Vec::new();
        for (slot, choices) in options.iter().enumerate() {
            for &snap in choices {
                let choice = self.kernel.fixed_choice(members[slot], snap);
                if !choice.supported {
                    return None;
                }
                if let Some(converter) = choice.converter.as_ref() {
                    candidates.push((choice.order, converter.program, slot, snap));
                } else {
                    plain[slot].push(snap);
                }
            }
        }
        candidates.sort_unstable_by(|a, b| {
            self.kernel.chart.performer_json[a.0 as usize]
                .cmp(&self.kernel.chart.performer_json[b.0 as usize])
                .then(a.2.cmp(&b.2))
                .then(a.3.cmp(&b.3))
        });
        let mut groups = (0..members.len())
            .map(|_| std::collections::BTreeMap::<usize, Vec<i32>>::new())
            .collect::<Vec<_>>();
        let mut last = None;
        let mut run = 0;
        for (_, program, slot, snap) in candidates {
            if last.is_some_and(|old| old != program) {
                run += 1;
            }
            last = Some(program);
            groups[slot].entry(run).or_default().push(snap);
        }
        Some(
            plain
                .into_iter()
                .zip(groups)
                .map(|(plain, groups)| {
                    let mut domains = Vec::new();
                    if !plain.is_empty() {
                        domains.push(plain);
                    }
                    domains.extend(groups.into_values());
                    domains
                })
                .collect(),
        )
    }
    fn prepare_assignment_domain(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
        complete: bool,
    ) {
        self.reuse_assignment = true;
        self.prepare_assignment(members, options);
        self.reuse_assignment = false;
        self.fixed_box = true;
        self.fixed_box_complete = complete;
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.nominal_base = if self.kernel.chart.luck
            && self.kernel.seeds.is_empty()
            && self.kernel.criterion != Criterion::Mean
            && !self.surrogate
            && !self.estimate
        {
            self.member_constant(members, 0)
        } else {
            None
        };
        self.combo_bound = None;
        self.combo_attempted = false;
        self.fixed_no_rng_bound = None;
        self.fixed_no_rng_attempted = false;
        self.fixed_box = false;
        self.fixed_root_checked = false;
        if !self.reuse_assignment {
            self.fixed_region_models.clear();
            self.fixed_region_order.clear();
            self.fixed_leaf_models.clear();
            self.fixed_leaf_order.clear();
        }
        if !self.reuse_assignment && self.kernel.chart.luck && !self.kernel.seeds.is_empty() {
            let mut powers = Vec::with_capacity(5 * self.power_width);
            for &member in members {
                powers.extend_from_slice(
                    &self.snapshot_power
                        [member * self.power_width..(member + 1) * self.power_width],
                );
            }
            self.fixed_local_power = Arc::new(powers);
        }
        if self.kernel.healthy
            && (self.kernel.chart.luck && !self.kernel.seeds.is_empty()
                || self
                    .kernel
                    .chart
                    .setup
                    .missions
                    .iter()
                    .all(|&mission| mission == 1))
        {
            self.combo_options.resize_with(options.len(), Vec::new);
            for (stored, source) in self.combo_options.iter_mut().zip(options) {
                stored.clone_from(source);
            }
        }
        self.numeric_cohorts.clear();
        self.luck_cohorts.clear();
        self.luck_assignment_bases.clear();
        self.plain_second_bounds.clear();
        self.plain_pair_bound = None;
        let compiled = self.deterministic.is_some()
            && self.kernel.healthy
            && self.kernel.owner_local
            && self.kernel.just_context.is_some();
        if let Some(bound) = &mut self.deterministic {
            if self.reuse_assignment {
                bound.prepare_fixed_score_assignment(members, options);
            } else {
                bound.prepare_score_assignment(members, options);
            }
            if !compiled {
                return;
            }
        }
        if self.preserve_snap_options() {
            return;
        }
        if (self.kernel.criterion == Criterion::Max || self.bounded_minimum())
            && self.deterministic.is_none()
        {
            for (slot, &m) in members.iter().enumerate() {
                if self.max_choices[m] == options[slot] {
                    continue;
                }
                let mut frontiers: [Vec<(f64, f64)>; 5] = std::array::from_fn(|_| Vec::new());
                for &s in &options[slot] {
                    let power = self.snapshot_power[m * self.power_width
                        + if s < 0 {
                            self.power_width - 1
                        } else {
                            s as usize
                        }];
                    let weights = self.event_rows[&self.kernel.slot_index(m, s)];
                    for event in 0..5 {
                        frontiers[event].push((power, weights[event]));
                    }
                }
                for frontier in &mut frontiers {
                    frontier.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.total_cmp(&a.1)));
                    let mut high = f64::NEG_INFINITY;
                    frontier.retain(|&(_, value)| {
                        if value > high {
                            high = value;
                            true
                        } else {
                            false
                        }
                    });
                }
                self.max_choices[m] = options[slot].clone();
                self.max_frontiers[m] = frontiers;
            }
        }
        if !self.surrogate && (self.kernel.chart.luck || compiled) {
            if compiled {
                for (slot, &member) in members.iter().enumerate() {
                    for &snap in &options[slot] {
                        self.kernel.bound_shape_id(member, snap);
                        let index = self.kernel.slot_index(member, snap);
                        if self.kernel.bound_converters[index]
                            && self.kernel.raw_actor_keys[index] == u32::MAX
                        {
                            let actor = self.kernel.actor_of(member, snap);
                            self.kernel.raw_actor_keys[index] = self.kernel.performer_key(&actor);
                        }
                    }
                }
            }
            let mut chain_snaps = Vec::new();
            for (slot, list) in options.iter().enumerate() {
                for &snap in list {
                    if snap >= 0
                        && self.kernel.support_chain
                            [members[slot] * self.kernel.snaps.len() + snap as usize]
                        && !chain_snaps.contains(&snap)
                    {
                        chain_snaps.push(snap);
                    }
                }
            }
            let chains = chain_snaps
                .into_iter()
                .map(|snap| self.kernel.chain_choice(snap, members, options))
                .collect();
            let mut nonchain = Vec::new();
            let mut nonchain_ids = Vec::new();
            let mut chain_options = Vec::new();
            for (slot, &member) in members.iter().enumerate() {
                let mut plain = Vec::new();
                let mut active = Vec::new();
                for &snap in &options[slot] {
                    let active_context = if compiled {
                        self.kernel.bound_converters[self.kernel.slot_index(member, snap)]
                    } else {
                        snap >= 0
                            && self.kernel.support_chain
                                [member * self.kernel.snaps.len() + snap as usize]
                    };
                    if active_context {
                        let actor = self.kernel.support(member, snap).unwrap();
                        let id = self.kernel.performer_key(&actor);
                        let domain = self.kernel.choice_domain(&[snap]);
                        active.push(ChainOption {
                            snap,
                            actor: (actor, id),
                            domain,
                        });
                    } else {
                        plain.push(snap);
                    }
                }
                nonchain_ids.push(self.kernel.choice_domain(&plain));
                nonchain.push(plain);
                chain_options.push(active);
            }
            let mut numeric_converters: Vec<Vec<NumericConverter>> =
                (0..5).map(|_| Vec::new()).collect();
            let mut numeric_supported = compiled;
            if compiled {
                let mut candidates = Vec::new();
                for (slot, choices) in chain_options.iter().enumerate() {
                    for choice in choices {
                        let index = self.kernel.slot_index(members[slot], choice.snap);
                        let mut actor = self.kernel.bound_actors[index].as_ref().unwrap().clone();
                        actor.gekisou_support_skills.retain(|&(id, level)| {
                            self.kernel.master.gekiso_support_skills[&id]
                                .effects
                                .iter()
                                .any(|row| row.level == level && row.kind == 13005)
                        });
                        let Some(prepared) = self
                            .kernel
                            .just_bounds
                            .converter(self.kernel.master, &actor)
                        else {
                            numeric_supported = false;
                            continue;
                        };
                        candidates.push((
                            slot,
                            choice.snap,
                            prepared.program,
                            self.kernel.raw_actor_keys[index],
                            prepared,
                        ));
                    }
                }
                let mut order: Vec<_> = (0..candidates.len()).collect();
                order.sort_by(|&a, &b| {
                    self.kernel.chart.performer_json[candidates[a].3 as usize]
                        .cmp(&self.kernel.chart.performer_json[candidates[b].3 as usize])
                        .then(candidates[a].0.cmp(&candidates[b].0))
                        .then(candidates[a].1.cmp(&candidates[b].1))
                });
                let mut ranks = vec![0; candidates.len()];
                let mut rank = 0;
                let mut previous = None;
                for index in order {
                    let program = candidates[index].2;
                    if previous != Some(program) {
                        rank += 1;
                    }
                    ranks[index] = rank;
                    previous = Some(program);
                }
                let mut signature = Vec::new();
                for (slot, prepared_converters) in numeric_converters.iter_mut().enumerate() {
                    let mut owned: Vec<_> = candidates
                        .iter()
                        .enumerate()
                        .filter(|(_, entry)| entry.0 == slot)
                        .collect();
                    owned.sort_by_key(|(_, entry)| entry.1);
                    signature.push(owned.len());
                    for (_, entry) in owned {
                        signature.extend([(entry.1 + 1) as usize, entry.2]);
                        prepared_converters.push(NumericConverter {
                            snap: entry.1,
                            prepared: Arc::clone(&entry.4),
                            order: slot,
                        });
                    }
                }
                let mut canonical: Vec<_> = (0..candidates.len()).collect();
                canonical.sort_unstable_by_key(|&index| (candidates[index].0, candidates[index].1));
                for (at, &first) in canonical.iter().enumerate() {
                    for &second in &canonical[at + 1..] {
                        if candidates[first].0 != candidates[second].0
                            && candidates[first].2 != candidates[second].2
                        {
                            signature.push((ranks[first] < ranks[second]) as usize);
                        }
                    }
                }
                if self.kernel.pattern_domains.len() >= 2048 {
                    self.kernel.pattern_domains.clear();
                    self.kernel.patterns.clear();
                    self.kernel.pattern_order.clear();
                    self.kernel.pattern_bytes = 0;
                }
                self.numeric_domain =
                    if let Some(&domain) = self.kernel.pattern_domains.get(&signature) {
                        domain
                    } else {
                        self.kernel.next_pattern_domain += 1;
                        let domain = self.kernel.next_pattern_domain;
                        self.kernel.pattern_domains.insert(signature, domain);
                        domain
                    };
            }
            self.assignment = Some(Assignment {
                numeric_converters,
                numeric_supported,
                nonchain,
                nonchain_ids,
                chain_options,
                member_deck: members
                    .iter()
                    .map(|&i| self.kernel.gk_members[i].clone())
                    .collect(),
                member_ids: members
                    .iter()
                    .map(|&i| {
                        let p = self.kernel.gk_members[i].clone();
                        self.kernel.performer_key(&p)
                    })
                    .collect(),
                options: options.to_vec(),
                option_ids: options
                    .iter()
                    .map(|choices| self.kernel.choice_domain(choices))
                    .collect(),
                chains,
            });
        }
    }
    fn assignment_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
    ) -> Option<f64> {
        if !self.kernel.chart.luck
            || self.surrogate
            || self.estimate
            || !self.kernel.seeds.is_empty()
        {
            return None;
        }
        if from == 2
            && (assigned[1] < 0
                || !self.kernel.support_chain
                    [members[1] * self.kernel.snaps.len() + assigned[1] as usize])
        {
            let plain = assigned[0] < 0
                || !self.kernel.support_chain
                    [members[0] * self.kernel.snaps.len() + assigned[0] as usize];
            let pair = if plain { self.plain_pair_bound } else { None };
            if let Some(upper) = self
                .plain_second_bounds
                .get(&assigned[0])
                .copied()
                .into_iter()
                .chain(pair)
                .reduce(f64::min)
            {
                return Some(upper);
            }
        }
        let assignment = self.assignment.as_ref()?;
        let options = &assignment.options;
        let member_deck = &assignment.member_deck;
        let taken = &assigned[..from];
        let key = (
            from,
            std::array::from_fn(|slot| {
                if slot < from
                    && assignment
                        .chains
                        .iter()
                        .any(|chain| chain.snap == assigned[slot])
                {
                    assigned[slot]
                } else {
                    -1
                }
            }),
        );
        let (base, no_future) = if let Some((base, selected)) = self.luck_assignment_bases.get(&key)
        {
            (Arc::clone(base), Arc::clone(selected))
        } else {
            let mut chained = Vec::new();
            let mut selected = Vec::new();
            for chain in &assignment.chains {
                if let Some(slot) = taken.iter().position(|&snap| snap == chain.snap) {
                    if let Some(actor) = &chain.owned[slot] {
                        chained.push(actor);
                        selected.push(actor);
                    }
                } else {
                    chained.extend(chain.future[from].iter());
                }
            }
            let base = self
                .kernel
                .luck_base(member_deck, &chained, &assignment.member_ids);
            let no_future = self
                .kernel
                .luck_base(member_deck, &selected, &assignment.member_ids);
            if self.luck_assignment_bases.len() >= 512 {
                self.luck_assignment_bases.clear();
            }
            self.luck_assignment_bases
                .insert(key, (Arc::clone(&base), Arc::clone(&no_future)));
            (base, no_future)
        };
        let pick = if self.kernel.criterion == Criterion::Max {
            Criterion::Max
        } else {
            Criterion::Mean
        };
        let mut data = base.lock().unwrap_or_else(|e| e.into_inner());
        let mut value = data.weights.base;
        for slot in 0..from {
            value += self
                .kernel
                .luck_slot_locked(&mut data, members[slot], assigned[slot], pick);
        }
        for slot in from..5 {
            let choices = self.kernel.luck_choices(
                &mut data,
                members[slot],
                assignment.option_ids[slot],
                &options[slot],
                &self.snapshot_power,
            );
            value += choices
                .highest
                .iter()
                .find(|&&(snap, _)| snap < 0 || !taken.contains(&snap))
                .map_or(f64::NEG_INFINITY, |&(_, weight)| weight);
        }
        drop(data);
        let original = t.power * value / self.kernel.chart.full.converted as f64
            * (1.0 + REFINE_SLACK)
            + (self.kernel.chart.notes.len() + 4) as f64;
        if original < self.refine_cutoff {
            return Some(original);
        }
        let option_rows: [&[i32]; 5] = std::array::from_fn(|slot| options[slot].as_slice());
        let prefix = AssignmentPrefix {
            members,
            assigned,
            options: &option_rows,
            option_ids: &assignment.option_ids,
            selected_slots: 0,
            from,
            base_power: self.base_power,
            power_bound: t.power,
            snapshot_power: &self.snapshot_power,
        };
        let no_chain =
            self.kernel
                .context_partition_bound(&no_future, &prefix, false, self.refine_cutoff);
        let mut with_chain =
            self.kernel
                .context_partition_bound(&base, &prefix, true, self.refine_cutoff);
        let plain_first = from > 0
            && (assigned[0] < 0
                || !self.kernel.support_chain
                    [members[0] * self.kernel.snaps.len() + assigned[0] as usize]);
        let pair_covered = plain_first
            && self
                .plain_pair_bound
                .is_some_and(|upper| upper < self.refine_cutoff);
        let region = if from == 0
            && self.plain_pair_bound.is_none()
            && !assignment.nonchain[0].is_empty()
        {
            Some(0)
        } else if from == 1 && !pair_covered && !self.plain_second_bounds.contains_key(&assigned[0])
        {
            Some(1)
        } else {
            None
        };
        if let Some(start) = region.filter(|_| {
            self.kernel.seeds.is_empty()
                && !assignment.nonchain[1].is_empty()
                && no_chain < self.refine_cutoff
                && with_chain >= self.refine_cutoff
        }) {
            let coarse = if start == 0 {
                Self::prepare_luck_mission_cohorts(self.kernel, assignment, members)
            } else {
                None
            };
            let coarse_upper = coarse.as_ref().map(|cohorts| {
                Self::plain_region_upper(
                    self.kernel,
                    cohorts,
                    assignment,
                    &prefix,
                    start,
                    (self.refine_cutoff, no_chain),
                )
            });
            let upper =
                if let Some(upper) = coarse_upper.filter(|&upper| upper < self.refine_cutoff) {
                    upper
                } else {
                    let mut ordinary = assigned.to_vec();
                    ordinary[start..2].fill(-1);
                    let cohorts = Self::prepare_luck_cohorts(
                        self.kernel,
                        &mut self.luck_cohorts,
                        assignment,
                        members,
                        &ordinary,
                        2,
                    );
                    Self::plain_region_upper(
                        self.kernel,
                        &cohorts,
                        assignment,
                        &prefix,
                        start,
                        (self.refine_cutoff, no_chain),
                    )
                };
            if upper < self.refine_cutoff {
                if start == 0 {
                    self.plain_pair_bound = Some(upper);
                } else {
                    self.plain_second_bounds.insert(assigned[0], upper);
                }
                if assignment.chain_options[start..2].iter().all(Vec::is_empty) {
                    return Some(upper);
                }
            }
        }
        if from == 2 && no_chain < self.refine_cutoff && with_chain >= self.refine_cutoff {
            let cohorts = Self::prepare_luck_cohorts(
                self.kernel,
                &mut self.luck_cohorts,
                assignment,
                members,
                assigned,
                from,
            );
            let mut upper = f64::NEG_INFINITY;
            for cohort in cohorts.iter() {
                let rows: [&[i32]; 5] = std::array::from_fn(|slot| {
                    if slot < from {
                        option_rows[slot]
                    } else {
                        &cohort.options[slot]
                    }
                });
                let part = AssignmentPrefix {
                    options: &rows,
                    option_ids: &cohort.domains,
                    selected_slots: cohort.selected,
                    ..prefix
                };
                upper = upper.max(self.kernel.context_partition_bound(
                    &cohort.base,
                    &part,
                    false,
                    self.refine_cutoff,
                ));
                if upper >= self.refine_cutoff {
                    break;
                }
            }
            if upper < self.refine_cutoff {
                with_chain = with_chain.min(upper);
            }
        }
        if from == 3 && no_chain < self.refine_cutoff && with_chain >= self.refine_cutoff {
            let selected: Vec<_> = assignment
                .chains
                .iter()
                .filter_map(|chain| {
                    taken
                        .iter()
                        .position(|&snap| snap == chain.snap)
                        .and_then(|slot| chain.owned[slot].as_ref())
                })
                .collect();
            let mut upper = f64::NEG_INFINITY;
            let mut exact = selected.clone();
            exact.reserve(2);
            let mut team = Team {
                members: members.to_vec(),
                snaps: assigned.to_vec(),
            };
            let mut power = self.base_power;
            for slot in 0..3 {
                let snap = assigned[slot];
                power += self.snapshot_power[members[slot] * self.power_width
                    + if snap < 0 {
                        self.power_width - 1
                    } else {
                        snap as usize
                    }];
            }
            let slack = if self.kernel.chart.accuracy.great > 0.0 {
                10.0 * REFINE_SLACK
            } else {
                REFINE_SLACK
            };
            'patterns: for first in assignment.chain_options[3]
                .iter()
                .map(Some)
                .chain(std::iter::once(None))
            {
                for last in assignment.chain_options[4]
                    .iter()
                    .map(Some)
                    .chain(std::iter::once(None))
                {
                    if first.is_none() && last.is_none()
                        || first.is_some_and(|x| taken.contains(&x.snap))
                        || last.is_some_and(|x| taken.contains(&x.snap))
                        || first.zip(last).is_some_and(|(a, b)| a.snap == b.snap)
                    {
                        continue;
                    }
                    let value = if let (Some(a), Some(b)) = (first, last) {
                        team.snaps[3] = a.snap;
                        team.snaps[4] = b.snap;
                        let total = power
                            + self.snapshot_power[members[3] * self.power_width + a.snap as usize]
                            + self.snapshot_power[members[4] * self.power_width + b.snap as usize];
                        self.kernel.refine(&team, total) * (1.0 + slack)
                            + (self.kernel.chart.notes.len() + 4) as f64
                    } else {
                        exact.truncate(selected.len());
                        let mut rows = option_rows;
                        let mut ids: [u32; 5] =
                            std::array::from_fn(|slot| assignment.option_ids[slot]);
                        let mut selected_slots = 0;
                        for (slot, choice) in [(3, first), (4, last)] {
                            if let Some(choice) = choice {
                                exact.push(&choice.actor);
                                rows[slot] = std::slice::from_ref(&choice.snap);
                                ids[slot] = choice.domain;
                                selected_slots |= 1 << slot;
                            } else {
                                rows[slot] = &assignment.nonchain[slot];
                                ids[slot] = assignment.nonchain_ids[slot];
                            }
                        }
                        let base =
                            self.kernel
                                .luck_base(member_deck, &exact, &assignment.member_ids);
                        let part = AssignmentPrefix {
                            options: &rows,
                            option_ids: &ids,
                            selected_slots,
                            ..prefix
                        };
                        self.kernel
                            .context_partition_bound(&base, &part, false, self.refine_cutoff)
                    };
                    upper = upper.max(value);
                    if upper >= self.refine_cutoff {
                        break 'patterns;
                    }
                }
            }
            if upper < self.refine_cutoff {
                with_chain = with_chain.min(upper);
            }
        }
        if from == 4 && no_chain < self.refine_cutoff && with_chain >= self.refine_cutoff {
            let width = self.power_width;
            let mut power = self.base_power;
            for slot in 0..4 {
                let snap = assigned[slot];
                power += self.snapshot_power
                    [members[slot] * width + if snap < 0 { width - 1 } else { snap as usize }];
            }
            let mut team = Team {
                members: members.to_vec(),
                snaps: assigned.to_vec(),
            };
            let slack = if self.kernel.chart.accuracy.great > 0.0 {
                10.0 * REFINE_SLACK
            } else {
                REFINE_SLACK
            };
            let mut upper = f64::NEG_INFINITY;
            for &snap in &options[4] {
                if snap < 0
                    || taken.contains(&snap)
                    || !self.kernel.support_chain
                        [members[4] * self.kernel.snaps.len() + snap as usize]
                {
                    continue;
                }
                team.snaps[4] = snap;
                let total = power + self.snapshot_power[members[4] * width + snap as usize];
                upper = upper.max(
                    self.kernel.refine(&team, total) * (1.0 + slack)
                        + (self.kernel.chart.notes.len() + 4) as f64,
                );
                // This hint controls refinement cost; a stopped refinement keeps the original full bound.
                if upper >= self.refine_cutoff {
                    break;
                }
            }
            if upper < self.refine_cutoff {
                with_chain = with_chain.min(upper);
            }
        }
        Some(original.min(no_chain.max(with_chain)))
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
        if self.deterministic.is_some() {
            return (0.0, self.leaf_bound(team, t).unwrap());
        }
        if self.preserve_snap_options() {
            return (0.0, f64::INFINITY);
        }
        if self.kernel.criterion == Criterion::Max {
            let upper = self.order_upper(team, t.power);
            return if self.surrogate {
                (upper, upper)
            } else {
                let slack = if self.kernel.chart.accuracy.great > 0.0 {
                    10.0 * REFINE_SLACK
                } else {
                    REFINE_SLACK
                };
                let refined = self.kernel.refine(team, t.power) * (1.0 + slack)
                    + (self.kernel.chart.notes.len() + 4) as f64;
                (0.0, upper.min(refined))
            };
        }
        if self.surrogate {
            return (self.high(t), self.high(t));
        }
        let slack = if self.kernel.chart.accuracy.great > 0.0 {
            10.0 * REFINE_SLACK
        } else {
            REFINE_SLACK
        };
        let value = self.kernel.refine(team, t.power);
        let absolute = (self.kernel.chart.notes.len() + 4) as f64;
        (
            (value * (1.0 - slack) - absolute).max(0.0),
            self.high(t)
                .min(value * (1.0 + slack) + absolute)
                .max(value * (1.0 + slack)),
        )
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        if self.kernel.criterion == Criterion::Max && (self.estimate || self.surrogate) {
            return Ok(Exact {
                key: self.order_upper(team, power),
                detail: Value::Null,
            });
        }
        if self.estimate {
            let mut skill = 0.0;
            for (slot, &m) in team.members.iter().enumerate() {
                skill += self.skill(m, team.snaps[slot]).1;
            }
            return Ok(Exact {
                key: self.high(Totals {
                    power,
                    skill,
                    ..Totals::default()
                }),
                detail: Value::Null,
            });
        }
        if self.surrogate {
            return Ok(Exact {
                key: self.high(Totals {
                    power,
                    ..Totals::default()
                }),
                detail: Value::Null,
            });
        }
        self.kernel.exact_calls += 1;
        if let Some(hit) = self.seed_scores.iter().find(|hit| {
            hit.power == power && hit.team.members == team.members && hit.team.snaps == team.snaps
        }) {
            return Ok(Exact {
                key: hit.key,
                detail: hit.detail.clone(),
            });
        }
        self.kernel.search_exact(team, power as i32, true)
    }
    fn evaluate(
        &mut self,
        team: &Team,
        power: f64,
        slots: &[f64],
        cutoff: search::Cutoff,
    ) -> Result<search::Evaluation, String> {
        if !self.estimate && !self.surrogate && cutoff.key.is_finite() {
            if let Some(upper) = self
                .kernel
                .deterministic_order_bound(team, power, cutoff.key)
            {
                if upper < cutoff.key || cutoff.prune_equal && upper == cutoff.key {
                    return Ok(search::Evaluation::Pruned(upper));
                }
            }
        }
        self.exact(team, power, slots)
            .map(search::Evaluation::Exact)
    }
}
struct WarmProjection<'p, 'k, 'a> {
    adapter: &'p mut Adapter<'k, 'a>,
    lambda: f64,
    mean: bool,
}
impl Objective for WarmProjection<'_, '_, '_> {
    fn canonical_leader(&self) -> bool {
        true
    }
    fn skill(&mut self, member: usize, snap: i32) -> (f64, f64) {
        let value = self.adapter.skill(member, snap);
        if self.mean {
            let row = self.adapter.event_rows[&self.adapter.kernel.slot_index(member, snap)];
            let value = row.iter().sum::<f64>() / 5.0;
            (value, value)
        } else {
            value
        }
    }
    fn member_bonus(&self, member: usize) -> f64 {
        self.adapter.member_bonus(member)
    }
    fn snap_dominance(&mut self, member: usize, snap: i32) -> Vec<f64> {
        vec![self.skill(member, snap).1]
    }
    fn linear_assignment(&self) -> Option<(f64, f64, f64)> {
        Some((self.lambda, 1.0 / self.lambda, 0.0))
    }
    fn bound(&mut self, totals: Totals) -> f64 {
        self.lambda * totals.power + totals.skill / self.lambda
    }
    fn interval(&mut self, _team: &Team, totals: Totals) -> (f64, f64) {
        let value = self.bound(totals);
        (value, value)
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        let skill = team
            .members
            .iter()
            .zip(&team.snaps)
            .map(|(&m, &s)| self.skill(m, s).1)
            .sum();
        Ok(Exact {
            key: self.bound(Totals {
                power,
                skill,
                ..Totals::default()
            }),
            detail: Value::Null,
        })
    }
}
fn minimum_order_upper(rows: &[[f64; 5]; 5]) -> f64 {
    // max(snapshot) min(order) <= min(order) max(snapshot).
    let mut states = [f64::INFINITY; 32];
    states[0] = 0.0;
    for mask in 0usize..31 {
        let row = mask.count_ones() as usize;
        for (event, &weight) in rows[row].iter().enumerate() {
            if mask & (1 << event) == 0 {
                let next = mask | (1 << event);
                states[next] = states[next].min((states[mask] + weight).next_up());
            }
        }
    }
    states[31]
}
fn best_order(rows: &[[f64; 5]; 5]) -> f64 {
    let mut states = [f64::NEG_INFINITY; 32];
    states[0] = 0.0;
    for mask in 0usize..31 {
        let value = states[mask];
        if value == f64::NEG_INFINITY {
            continue;
        }
        let row = mask.count_ones() as usize;
        for (event, &weight) in rows[row].iter().enumerate() {
            if mask & (1 << event) == 0 {
                let next = mask | (1 << event);
                states[next] = states[next].max(value + weight);
            }
        }
    }
    states[31]
}

pub struct Environment<'a> {
    pub master: &'a Master,
    pub player: &'a Player,
    pub members: &'a [Member],
    pub snaps: &'a [Snap],
    pub music: Option<&'a Music>,
    pub constraints: search::Constraints,
    pub now: &'a dyn Fn() -> f64,
    pub generation: u64,
}
struct Run {
    constraints: search::Constraints,
    k: usize,
    deadline: f64,
    seeds: Vec<Team>,
    shard: Option<(usize, usize)>,
    floor: f64,
}
impl<'a> Environment<'a> {
    fn run<'b, O: Objective>(
        &'b self,
        objective: &'b mut O,
        run: Run,
        owns: Option<Ownership<'b>>,
        callback: Option<CandidateListener<'b>>,
    ) -> Result<search::Output, String>
    where
        'a: 'b,
    {
        search::search_at(
            search::Input {
                master: self.master,
                player: self.player,
                members: self.members,
                snaps: self.snaps,
                music: self.music,
                objective,
                constraints: run.constraints,
                k: run.k,
                deadline: run.deadline,
                now: self.now,
                seeds: run.seeds,
                shard: run.shard,
                owns,
                floor: run.floor,
                floor_exclusive: false,

                streaming: true,
                disable_snap_dominance: false,
                member_power_percent: None,
                snap_power_percent: None,
                on_candidate: callback,
                progress: None,
            },
            self.generation,
        )
    }
    fn power(&self, team: &Team) -> Result<power::TeamPower, String> {
        let members: Vec<_> = team
            .members
            .iter()
            .map(|&i| self.members[i].clone())
            .collect();
        let snaps: Vec<_> = team
            .snaps
            .iter()
            .map(|&i| {
                if i < 0 {
                    None
                } else {
                    Some(self.snaps[i as usize].clone())
                }
            })
            .collect();
        power::team_power(
            self.master,
            self.player,
            &members,
            &snaps,
            0,
            self.music,
            &[],
        )
    }
}
fn refine_seed(
    env: &Environment<'_>,
    kernel: &mut Kernel<'_>,
    mut best: Hit,
    excluded: &[Vec<usize>],
    late: &dyn Fn() -> bool,
    mut evaluated: Option<(&mut Vec<Hit>, usize)>,
) -> Result<Hit, String> {
    let mut proposal_chart = if kernel.chart.luck && kernel.seeds.len() > 1 {
        kernel.chart.converted_judgements(&kernel.chart.judgements)
    } else {
        None
    };
    let mut proposal_kernel = proposal_chart.as_mut().map(|chart| {
        Kernel::new(
            kernel.master,
            kernel.members,
            kernel.snaps,
            chart,
            vec![kernel.seeds[0]],
            kernel.criterion,
        )
    });
    let slack = if kernel.chart.accuracy.great > 0.0 {
        10.0 * REFINE_SLACK
    } else {
        REFINE_SLACK
    };
    let absolute = (kernel.chart.notes.len() + 4) as f64;
    loop {
        let mut changed = false;
        let mut proposal: Option<(Team, power::TeamPower, f64)> = None;
        {
            let mut consider = |team: Team,
                                best: &mut Hit,
                                leader_only: bool|
             -> Result<bool, String> {
                if late() || !search::valid_seed(env.members, env.snaps, &env.constraints, &team) {
                    return Ok(false);
                }
                if !excluded.is_empty() {
                    let mut members = team.members.clone();
                    members.sort_unstable();
                    if excluded.contains(&members) {
                        return Ok(false);
                    }
                }
                let power = env.power(&team)?;
                if leader_only && power.total as f64 <= best.power {
                    return Ok(false);
                }
                let upper = if kernel.chart.luck {
                    let value = if let Some(proposal) = &mut proposal_kernel {
                        proposal.refine(&team, power.total as f64)
                    } else {
                        kernel.refine(&team, power.total as f64)
                    };
                    value * (1.0 + slack) + absolute
                } else if let Some(coefficient) = kernel.deterministic_coefficient(&team) {
                    (power.total as f64 * coefficient).next_up() + absolute
                } else {
                    return Ok(false);
                };
                let floor = evaluated.as_ref().map_or(best.key, |(hits, k)| {
                    let mut keys = hits.iter().map(|hit| hit.key).collect::<Vec<_>>();
                    if keys.len() < *k {
                        return f64::NEG_INFINITY;
                    }
                    keys.select_nth_unstable_by(k - 1, |a, b| b.total_cmp(a));
                    keys[k - 1]
                });
                if upper <= floor {
                    return Ok(false);
                }
                if !kernel.chart.luck {
                    if proposal.as_ref().is_none_or(|(_, old, value)| {
                        upper > *value || upper == *value && power.total > old.total
                    }) {
                        proposal = Some((team, power, upper));
                    }
                    return Ok(false);
                }
                let exact = kernel.search_exact(&team, power.total as i32, true)?;
                kernel.exact_calls += 1;
                if let Some((evaluated, _)) = evaluated.as_mut() {
                    offer_scored(
                        kernel,
                        evaluated,
                        env.members,
                        Hit {
                            team: team.clone(),
                            power: power.total as f64,
                            slot_powers: power.slots.iter().map(|&v| v as f64).collect(),
                            totals: Totals {
                                power: power.total as f64,
                                ..Totals::default()
                            },
                            key: exact.key,
                            detail: exact.detail.clone(),
                        },
                    )?;
                }
                if exact.key <= best.key {
                    return Ok(false);
                }
                *best = Hit {
                    team,
                    power: power.total as f64,
                    slot_powers: power.slots.iter().map(|&v| v as f64).collect(),
                    totals: Totals {
                        power: power.total as f64,
                        ..Totals::default()
                    },
                    key: exact.key,
                    detail: exact.detail,
                };
                Ok(true)
            };
            for slot in 0..5 {
                let member = best.team.members[slot];
                if env.constraints.required_members.contains(&member)
                    || env.constraints.bindings.contains_key(&member)
                    || slot == 0 && env.constraints.leader.is_some()
                {
                    continue;
                }
                for member in 0..env.members.len() {
                    if late() {
                        return Ok(best);
                    }
                    if member == best.team.members[slot] {
                        continue;
                    }
                    let mut team = best.team.clone();
                    team.members[slot] = member;
                    if consider(team, &mut best, false)? {
                        changed = true;
                    }
                }
            }
            for slot in 0..5 {
                let snap = best.team.snaps[slot];
                if env
                    .constraints
                    .bindings
                    .contains_key(&best.team.members[slot])
                    || snap >= 0 && env.constraints.required_snaps.contains(&(snap as usize))
                {
                    continue;
                }
                let end = if env.constraints.no_snaps {
                    0
                } else {
                    env.snaps.len() as i32
                };
                for snap in -1..end {
                    if late() {
                        return Ok(best);
                    }
                    if snap == best.team.snaps[slot] {
                        continue;
                    }
                    let mut team = best.team.clone();
                    team.snaps[slot] = snap;
                    if consider(team, &mut best, false)? {
                        changed = true;
                    }
                }
            }
            for a in 0..4 {
                for b in a + 1..5 {
                    if best.team.snaps[a] == best.team.snaps[b] {
                        continue;
                    }
                    let mut team = best.team.clone();
                    team.snaps.swap(a, b);
                    if consider(team, &mut best, false)? {
                        changed = true;
                    }
                }
            }
            if env.constraints.leader.is_none() {
                for slot in 1..5 {
                    let mut team = best.team.clone();
                    team.members.swap(0, slot);
                    team.snaps.swap(0, slot);
                    if consider(team, &mut best, true)? {
                        changed = true;
                    }
                }
            }
        }
        if let Some((team, power, _)) = proposal {
            if late() {
                return Ok(best);
            }
            let exact = kernel.search_exact(&team, power.total as i32, true)?;
            kernel.exact_calls += 1;
            if exact.key > best.key {
                best = Hit {
                    team,
                    power: power.total as f64,
                    slot_powers: power.slots.iter().map(|&v| v as f64).collect(),
                    totals: Totals {
                        power: power.total as f64,
                        ..Totals::default()
                    },
                    key: exact.key,
                    detail: exact.detail,
                };
                changed = true;
            }
        }
        if !changed || late() {
            return Ok(best);
        }
    }
}
pub struct Input<'a> {
    pub env: Environment<'a>,
    pub chart: &'a mut Chart,
    pub seeds: Vec<i32>,
    pub criterion: Criterion,
    pub k: usize,
    pub deadline: f64,
    pub initial: Vec<Team>,
    pub seed_only: bool,
    pub seed_variant: Option<bool>,
    pub defer_scores: bool,
    pub cooperative_floor: bool,
    pub min_combo_bonus: i32,
    pub seeded: bool,
    pub shard: Option<(usize, usize)>,
    pub floor: f64,
    pub on_candidate: Option<CandidateListener<'a>>,
    pub progress: Option<&'a mut dyn FnMut(usize, usize)>,
}
struct Feasible {
    bonuses: Vec<i32>,
    preference: Vec<f64>,
}
impl Objective for Feasible {
    fn skill(&mut self, _m: usize, _s: i32) -> (f64, f64) {
        (0.0, 0.0)
    }
    fn member_bonus(&self, m: usize) -> f64 {
        self.bonuses[m] as f64
    }
    fn bound(&mut self, _t: Totals) -> f64 {
        0.0
    }
    fn interval(&mut self, _team: &Team, _t: Totals) -> (f64, f64) {
        (0.0, 0.0)
    }
    fn exact(&mut self, _team: &Team, _power: f64, _slots: &[f64]) -> Result<Exact, String> {
        Ok(Exact {
            key: 0.0,
            detail: Value::Null,
        })
    }
    fn prefer_snap(&self, _m: usize, s: i32) -> f64 {
        if s < 0 {
            1.0
        } else {
            0.0
        }
    }
    fn prefer_member(&self, m: usize) -> f64 {
        self.preference[m]
    }
}
fn set_key(members: &[Member], team: &Team) -> String {
    let mut keys: Vec<_> = team
        .members
        .iter()
        .map(|&i| members[i].key.as_str())
        .collect();
    keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    keys.join("|")
}
fn ranked(hits: &[Hit], k: usize) -> Vec<Hit> {
    let mut hits = hits.to_vec();
    hits.sort_by(|a, b| compare_score_hits(b, a));
    hits.truncate(k);
    hits
}
pub(crate) fn compare_score_hits(a: &Hit, b: &Hit) -> std::cmp::Ordering {
    a.key.total_cmp(&b.key).then_with(|| {
        a.detail["scores"]["mean"]
            .as_f64()
            .unwrap_or(f64::NEG_INFINITY)
            .total_cmp(
                &b.detail["scores"]["mean"]
                    .as_f64()
                    .unwrap_or(f64::NEG_INFINITY),
            )
    })
}
fn offer_scored(
    kernel: &mut Kernel<'_>,
    hits: &mut Vec<Hit>,
    members: &[Member],
    mut hit: Hit,
) -> Result<(), String> {
    if kernel.criterion != Criterion::Mean {
        let key = hit.key;
        for old in hits.iter_mut().filter(|old| old.key == key) {
            if old.power == hit.power
                && old.team.members == hit.team.members
                && old.team.snaps == hit.team.snaps
            {
                continue;
            }
            kernel.complete_score_tie(old)?;
            kernel.complete_score_tie(&mut hit)?;
        }
    }
    offer(hits, members, hit);
    Ok(())
}
fn offer(hits: &mut Vec<Hit>, members: &[Member], hit: Hit) {
    let key = set_key(members, &hit.team);
    if let Some(index) = hits
        .iter()
        .position(|old| set_key(members, &old.team) == key)
    {
        if compare_score_hits(&hit, &hits[index]).is_gt() {
            hits[index] = hit;
        }
    } else {
        hits.push(hit);
    }
}
fn warm(adapter: &mut Adapter<'_, '_>, env: &Environment<'_>, deadline: f64) -> bool {
    if adapter.deterministic.is_some() || adapter.preserve_snap_options() {
        return true;
    }
    if !env.constraints.no_snaps {
        let pairs: Vec<(usize, i32)> = (0..env.members.len())
            .filter(|m| !env.constraints.excluded_members.contains(m))
            .flat_map(|m| {
                (0..env.snaps.len())
                    .filter(|s| !env.constraints.excluded_snaps.contains(s))
                    .map(move |s| (m, s as i32))
            })
            .collect();
        let level = adapter.level;
        adapter.kernel.prefetch_supports(level, &pairs);
    }
    for m in 0..env.members.len() {
        if (env.now)() >= deadline {
            return false;
        }
        if env.constraints.excluded_members.contains(&m) {
            continue;
        }
        adapter.skill(m, -1);
        if !env.constraints.no_snaps {
            for s in 0..env.snaps.len() {
                if (env.now)() >= deadline {
                    return false;
                }
                if !env.constraints.excluded_snaps.contains(&s) {
                    adapter.skill(m, s as i32);
                }
            }
        }
    }
    true
}
fn level_constraints(
    constraints: &search::Constraints,
    level: i32,
    maximum: i32,
) -> search::Constraints {
    let mut c = constraints.clone();
    if maximum > 0 {
        c.min_bonus = Some(level as f64);
        c.max_bonus = Some(level as f64);
    }
    c
}
fn reachable_levels(
    env: &Environment<'_>,
    bonuses: &[i32],
    minimum: i32,
    maximum: i32,
) -> Vec<i32> {
    if maximum == 0 {
        return if minimum == 0 { vec![0] } else { Vec::new() };
    }
    let constraints = &env.constraints;
    let mut required: HashSet<usize> = constraints
        .required_members
        .iter()
        .copied()
        .chain(constraints.bindings.keys().copied())
        .collect();
    if let Some(leader) = constraints.leader {
        if leader < 0 || leader as usize >= env.members.len() {
            return Vec::new();
        }
        required.insert(leader as usize);
    }
    if required.len() > 5
        || required
            .iter()
            .any(|index| constraints.excluded_members.contains(index))
    {
        return Vec::new();
    }
    let characters: HashSet<_> = required
        .iter()
        .map(|&index| env.members[index].card.character_id)
        .collect();
    if characters.len() != required.len() {
        return Vec::new();
    }
    let start = required.iter().map(|&index| bonuses[index]).sum::<i32>();
    if start > maximum {
        return Vec::new();
    }
    let mut groups = std::collections::BTreeMap::<i32, BTreeSet<i32>>::new();
    for (index, member) in env.members.iter().enumerate() {
        if !characters.contains(&member.card.character_id)
            && !constraints.excluded_members.contains(&index)
        {
            groups
                .entry(member.card.character_id)
                .or_default()
                .insert(bonuses[index]);
        }
    }
    let mut states = vec![vec![false; maximum as usize + 1]; 6];
    states[required.len()][start as usize] = true;
    for values in groups.values() {
        let mut next = states.clone();
        for count in 0..5 {
            for (sum, &possible) in states[count].iter().enumerate() {
                if !possible {
                    continue;
                }
                for &value in values {
                    let sum = sum + value as usize;
                    if sum <= maximum as usize {
                        next[count + 1][sum] = true;
                    }
                }
            }
        }
        states = next;
    }
    (minimum..=maximum)
        .rev()
        .filter(|&level| states[5][level as usize])
        .collect()
}
fn seed_key(env: &Environment<'_>, criterion: Criterion, k: usize, seeds: &[i32]) -> String {
    let c = &env.constraints;
    let mut bindings: Vec<_> = c.bindings.iter().map(|(&m, &s)| (m, s)).collect();
    bindings.sort_unstable();
    json!([
        env.members.iter().map(|m| &m.key).collect::<Vec<_>>(),
        env.snaps.iter().map(|s| &s.key).collect::<Vec<_>>(),
        match criterion {
            Criterion::Mean => "mean",
            Criterion::Min => "min",
            Criterion::Max => "max",
        },
        k,
        c.required_members,
        c.excluded_members,
        c.required_snaps,
        c.excluded_snaps,
        c.leader,
        bindings,
        c.no_snaps,
        seeds
    ])
    .to_string()
}
fn replay_bound(
    master: &Master,
    chart: &Chart,
    members: &[Member],
    snaps: &[Snap],
) -> Result<Option<crate::rewards::DeterministicBound>, String> {
    if chart.luck {
        return Ok(None);
    }
    // Remove LIFE gates only in the upper model; exact settlement keeps them.
    let mut relaxed = master.clone();
    relaxed.condition_sets = Arc::new(
        master
            .condition_sets
            .iter()
            .map(|(&id, sets)| {
                (
                    id,
                    sets.iter()
                        .map(|set| {
                            set.iter()
                                .copied()
                                .filter(|id| {
                                    master
                                        .conditions
                                        .get(id)
                                        .is_none_or(|c| c.kind != 2000 && c.kind != 2001)
                                })
                                .collect()
                        })
                        .collect(),
                )
            })
            .collect(),
    );
    let optimistic = chart.all_just(&relaxed)?;
    Ok(crate::rewards::DeterministicBound::new(
        &relaxed,
        &optimistic,
        members,
        snaps,
    ))
}
struct ReplayObjective<'a> {
    master: &'a Master,
    members: &'a [Member],
    snaps: &'a [Snap],
    chart: &'a Chart,
    seeds: &'a [i32],
    criterion: Criterion,
    bonuses: Vec<i32>,
    bound: Option<crate::rewards::DeterministicBound>,
    coefficient: f64,
    shapes: HashMap<Performer, u32>,
}
impl Objective for ReplayObjective<'_> {
    fn score_ties(&self) -> bool {
        self.criterion != Criterion::Mean
    }

    fn skill(&mut self, member: usize, snap: i32) -> (f64, f64) {
        (
            0.0,
            self.bound
                .as_ref()
                .map_or(0.0, |b| b.weight(member, snap, false)),
        )
    }
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn snap_dominance(&mut self, member: usize, snap: i32) -> Vec<f64> {
        let p = Performer::from_member(
            self.master,
            &self.members[member],
            (snap >= 0).then(|| &self.snaps[snap as usize]),
        );
        let next = self.shapes.len() as u32;
        let id = *self.shapes.entry(p).or_insert(next) as f64;
        vec![id, -id]
    }
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        self.snap_dominance(member, a) == self.snap_dominance(member, b)
    }
    fn bound(&mut self, totals: Totals) -> f64 {
        self.bound.as_ref().map_or_else(
            || (totals.power * self.coefficient).next_up(),
            |b| b.score(totals),
        )
    }
    fn interval(&mut self, team: &Team, totals: Totals) -> (f64, f64) {
        let upper = self
            .bound
            .as_mut()
            .map(|b| b.score_ceiling(&team.members, &team.snaps, 5, totals.power, self.criterion));
        (0.0, upper.unwrap_or_else(|| self.bound(totals)))
    }
    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        if let Some(b) = &mut self.bound {
            b.prepare_members(characters);
        }
    }
    fn member_bound_at(
        &mut self,
        members: &[usize],
        t: Totals,
        _left: usize,
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        self.bound
            .as_mut()
            .map(|b| b.member_score_ceiling(members, from, t.power, self.criterion))
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        if let Some(b) = &mut self.bound {
            b.prepare_score_assignment(members, options);
        }
    }
    fn assignment_order_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        self.bound
            .as_mut()
            .map(|b| b.score_ceiling(members, assigned, from, t.power, self.criterion))
    }
    fn product_base(&self) -> Option<f64> {
        self.bound.as_ref().map(|b| b.base)
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<search::JointTarget>> {
        self.bound.as_ref().map(|b| {
            vec![search::JointTarget {
                bonus: 0.0,
                product: b.product_floor(limit),
            }]
        })
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        let deck: Vec<_> = team
            .members
            .iter()
            .zip(&team.snaps)
            .map(|(&m, &s)| {
                Performer::from_member(
                    self.master,
                    &self.members[m],
                    if s < 0 {
                        None
                    } else {
                        Some(&self.snaps[s as usize])
                    },
                )
            })
            .collect();
        let scores =
            gekiso::replay_orders(self.master, self.chart, &deck, self.seeds, power as i32)?;
        Ok(Exact {
            key: self.criterion.key(&scores.scores),
            detail: json!({"scores":scores,"seeds":self.seeds.len().max(1),"comboBonus":if self.chart.setup.missions.iter().any(|m| *m == 1 || *m == 4) { Some(1 + team.members.iter().map(|&i| self.bonuses[i]).sum::<i32>()) } else { None }}),
        })
    }
}

fn warm_pool(
    env: &Environment<'_>,
    kernel: &mut Kernel<'_>,
    minimum: i32,
    candidates: usize,
    deadline: f64,
    reference_power: f64,
    late: &dyn Fn() -> bool,
) -> Result<Vec<(Team, f64)>, String> {
    let criterion = kernel.criterion;
    let mut pool: Vec<(Team, f64)> = Vec::new();
    for level in kernel
        .grid
        .clone()
        .into_iter()
        .rev()
        .filter(|level| *level >= minimum)
    {
        if late() {
            break;
        }
        let maximum = kernel.maximum_level;
        let luck = kernel.chart.luck;
        let constraints = level_constraints(&env.constraints, level, maximum);
        let Some(mut adapter) = Adapter::new(kernel, level, true, true, deadline, env.now) else {
            break;
        };
        if !warm(&mut adapter, env, deadline) {
            break;
        }
        let end = if luck {
            deadline
        } else {
            ((env.now)() + 400.0).min(deadline)
        };
        let run = Run {
            constraints,
            k: candidates,
            deadline: end,
            seeds: Vec::new(),
            shard: None,
            floor: f64::NEG_INFINITY,
        };
        let mut outputs = Vec::new();
        if luck {
            let variants: &[bool] = if criterion == Criterion::Max {
                &[false, true]
            } else {
                &[false]
            };
            for &mean in variants {
                let power = reference_power.max(1.0);
                let skill = if mean {
                    adapter
                        .event_rows
                        .values()
                        .map(|row| row.iter().sum::<f64>() / 5.0)
                        .fold(0.0, f64::max)
                } else {
                    adapter
                        .ranges
                        .values()
                        .map(|&(_, high)| high)
                        .fold(0.0, f64::max)
                };
                let coefficient = adapter.data.as_ref().unwrap().weights.base + 5.0 * skill;
                let lambda = (coefficient.max(1.0) / power).sqrt();
                let mut projection = WarmProjection {
                    adapter: &mut adapter,
                    lambda,
                    mean,
                };
                outputs.push(env.run(
                    &mut projection,
                    Run {
                        constraints: run.constraints.clone(),
                        k: run.k,
                        deadline: run.deadline,
                        seeds: Vec::new(),
                        shard: None,
                        floor: run.floor,
                    },
                    None,
                    None,
                )?);
            }
        } else {
            outputs.push(env.run(&mut adapter, run, None, None)?);
        }
        for hit in outputs.into_iter().flat_map(|out| out.hits) {
            let key = (set_key(env.members, &hit.team), hit.team.snaps.clone());
            if let Some(index) = pool
                .iter()
                .position(|old| set_key(env.members, &old.0) == key.0 && old.0.snaps == key.1)
            {
                pool[index] = (hit.team, hit.key);
            } else {
                pool.push((hit.team, hit.key));
            }
        }
    }
    Ok(pool)
}

struct SharedLevelBounds {
    preparation: Option<Arc<crate::rewards::BoundPreparation>>,
    luck: Option<Option<Arc<crate::rewards::LuckEnvelope>>>,
    #[cfg(not(target_arch = "wasm32"))]
    luck_cache: Arc<SharedLuckCache>,
}
struct LevelSchedule {
    levels: Vec<i32>,
    queue: Option<Arc<std::sync::Mutex<VecDeque<i32>>>>,
}

#[cfg(not(target_arch = "wasm32"))]
struct NativeLevelSeed {
    seeds: Vec<i32>,
    k: usize,
    deadline: f64,
    min_combo_bonus: i32,
    floor: f64,
    hits: Vec<Hit>,
    bounds: SharedLevelBounds,
    queue: Arc<std::sync::Mutex<VecDeque<i32>>>,
    cooperative_floor: bool,
    split_leaders: bool,
}

#[cfg(not(target_arch = "wasm32"))]
enum NativeLevelUpdate {
    Candidates(Vec<Hit>),
    Progress(usize, usize),
}

#[cfg(not(target_arch = "wasm32"))]
fn native_levels(
    env: &Environment<'_>,
    jobs: Vec<(Vec<i32>, Chart)>,
    seed: NativeLevelSeed,
    mut update: impl FnMut(NativeLevelUpdate) -> Result<(), String>,
) -> Result<Vec<search::Output>, String> {
    use std::{
        sync::{atomic::AtomicU64, mpsc},
        time::{Duration, Instant},
    };
    enum Message {
        Update(NativeLevelUpdate),
        Done(usize, usize, Result<search::Output, String>),
    }
    let total = jobs.len();
    let (send, receive) = mpsc::sync_channel(32);
    let origin = Instant::now();
    let clock = Arc::new(AtomicU64::new(
        ((env.now)() - origin.elapsed().as_secs_f64() * 1000.0).to_bits(),
    ));
    let cancellation = crate::local_cancellation();
    let shared_floor = crate::local_search_floor();
    let (master, player, members, snaps, music, generation) = (
        env.master,
        env.player,
        env.members,
        env.snaps,
        env.music,
        env.generation,
    );
    std::thread::scope(|scope| {
        for (worker, (levels, mut chart)) in jobs.into_iter().enumerate() {
            let send = send.clone();
            let clock = Arc::clone(&clock);
            let constraints = env.constraints.clone();
            let cancellation = cancellation.clone();
            let shared_floor = shared_floor.clone();
            let seed = &seed;
            scope.spawn(move || {
                let level_count = levels.len();
                let _floor_scope = shared_floor.map(crate::enter_search_floor);
                let mut completed = 0;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let run = || {
                        crate::parallel::serial(|| {
                            let now = || {
                                f64::from_bits(clock.load(Ordering::Relaxed))
                                    + origin.elapsed().as_secs_f64() * 1000.0
                            };
                            if now() >= seed.deadline
                                || generation != crate::cancellation_generation()
                            {
                                return Ok(search::Output {
                                    hits: seed.hits.clone(),
                                    proven: false,
                                    bound: None,
                                    stats: search::Stats::default(),
                                });
                            }
                            let mut candidate = |hits: &[Hit]| {
                                let _ = send.send(Message::Update(NativeLevelUpdate::Candidates(
                                    hits.to_vec(),
                                )));
                            };
                            let mut progress = |done, _total| {
                                completed = done;
                                let _ = send.send(Message::Update(NativeLevelUpdate::Progress(
                                    worker, done,
                                )));
                            };
                            search_levels(
                                Input {
                                    env: Environment {
                                        master,
                                        player,
                                        members,
                                        snaps,
                                        music,
                                        constraints,
                                        now: &now,
                                        generation,
                                    },
                                    chart: &mut chart,
                                    seeds: seed.seeds.clone(),
                                    criterion: Criterion::Max,
                                    k: seed.k,
                                    deadline: seed.deadline,
                                    initial: Vec::new(),
                                    seed_only: false,
                                    seed_variant: None,
                                    defer_scores: false,
                                    cooperative_floor: seed.cooperative_floor,
                                    min_combo_bonus: seed.min_combo_bonus,
                                    seeded: true,
                                    shard: seed.split_leaders.then_some((worker, total)),
                                    floor: seed.floor,
                                    on_candidate: Some(&mut candidate),
                                    progress: Some(&mut progress),
                                },
                                Some(LevelSchedule {
                                    levels,
                                    queue: (!seed.split_leaders).then(|| Arc::clone(&seed.queue)),
                                }),
                                &seed.hits,
                                Some(&seed.bounds),
                            )
                        })
                    };
                    if let Some(cancellation) = cancellation {
                        crate::with_cancellation(cancellation, run)
                    } else {
                        run()
                    }
                }))
                .unwrap_or_else(|_| Err("native-level-worker-panic".to_owned()));
                if seed.split_leaders && result.as_ref().is_ok_and(|output| output.proven) {
                    completed = level_count;
                    let _ = send.send(Message::Update(NativeLevelUpdate::Progress(
                        worker, completed,
                    )));
                }
                if result.is_err() {
                    seed.queue.lock().unwrap().clear();
                }
                let _ = send.send(Message::Done(worker, completed, result));
            });
        }
        drop(send);
        let mut outputs = Vec::with_capacity(total);
        let mut finished = 0;
        let mut error = None;
        while finished < total {
            clock.store(
                ((env.now)() - origin.elapsed().as_secs_f64() * 1000.0).to_bits(),
                Ordering::Relaxed,
            );
            match receive.recv_timeout(Duration::from_millis(10)) {
                Ok(Message::Update(value)) => {
                    if error.is_none() {
                        if let Err(problem) = update(value) {
                            error = Some(problem);
                        }
                    }
                }
                Ok(Message::Done(worker, count, result)) => {
                    finished += 1;
                    match result {
                        Ok(output) => {
                            if output.proven && error.is_none() {
                                if let Err(problem) =
                                    update(NativeLevelUpdate::Progress(worker, count))
                                {
                                    error = Some(problem);
                                }
                            }
                            outputs.push(output);
                        }
                        Err(problem) => {
                            if error.is_none() {
                                error = Some(problem);
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    error = Some("native-level-worker-disconnected".to_owned());
                    break;
                }
            }
        }
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(outputs)
        }
    })
}

fn level_order(env: &Environment<'_>, bonuses: &[i32], levels: &[i32]) -> Vec<i32> {
    let maximum = levels.iter().copied().max().unwrap_or(0) as usize;
    let mut order = levels.to_vec();
    if maximum > 128 {
        return order;
    }
    let mut groups = std::collections::BTreeMap::<i32, Vec<usize>>::new();
    for (member, &bonus) in env.members.iter().zip(bonuses) {
        if bonus >= 0 && bonus as usize <= maximum {
            groups
                .entry(member.card.character_id)
                .or_default()
                .push(bonus as usize);
        }
    }
    let mut counts = vec![vec![0u64; maximum + 1]; 6];
    counts[0][0] = 1;
    for group in groups.values() {
        for count in (0..5).rev() {
            for sum in 0..=maximum {
                let previous = counts[count][sum];
                if previous == 0 {
                    continue;
                }
                for &bonus in group {
                    if sum + bonus <= maximum {
                        counts[count + 1][sum + bonus] =
                            counts[count + 1][sum + bonus].saturating_add(previous);
                    }
                }
            }
        }
    }
    order.sort_by(|&a, &b| {
        counts[5][b as usize]
            .cmp(&counts[5][a as usize])
            .then(b.cmp(&a))
    });
    order
}

pub fn search(input: Input<'_>) -> Result<search::Output, String> {
    search_levels(input, None, &[], None)
}
pub(crate) fn search_scored(input: Input<'_>, scores: &[Hit]) -> Result<search::Output, String> {
    search_levels(input, None, scores, None)
}

fn search_levels(
    input: Input<'_>,
    selected_levels: Option<LevelSchedule>,
    initial_scores: &[Hit],
    shared_bounds: Option<&SharedLevelBounds>,
) -> Result<search::Output, String> {
    #[cfg(not(target_arch = "wasm32"))]
    let whole = selected_levels.is_none();
    let Input {
        mut env,
        chart,
        seeds,
        criterion,
        k,
        deadline,
        initial,
        seed_only,
        seed_variant,
        defer_scores,
        cooperative_floor,
        min_combo_bonus,
        seeded,
        shard,
        floor,
        mut on_candidate,
        mut progress,
    } = input;
    if !seed_only && gekiso::uses_full_replay(env.master, chart, &seeds) {
        let bonuses = opening_combo_bonuses(env.master, env.members, &chart.setup.missions);
        let mut constraints = env.constraints.clone();
        if chart.setup.missions.iter().any(|m| *m == 1 || *m == 4) && min_combo_bonus > 1 {
            let values: Vec<_> = bonuses.iter().map(|&v| v as f64).collect();
            let maximum = values.iter().copied().fold(0.0, f64::max);
            constraints.member_floor = Some(search::MemberFloor {
                values,
                minimum: (min_combo_bonus - 1) as f64,
                maximum,
            });
        }
        let mut objective = ReplayObjective {
            master: env.master,
            members: env.members,
            snaps: env.snaps,
            chart,
            seeds: &seeds,
            criterion,
            bonuses,
            coefficient: crate::rewards::score_coefficient(
                env.master,
                chart,
                env.members,
                env.snaps,
                None,
            ),
            bound: replay_bound(env.master, chart, env.members, env.snaps)?,
            shapes: HashMap::default(),
        };
        let mut candidate = |hits: &[Hit]| {
            if let Some(callback) = on_candidate.as_deref_mut() {
                callback(hits);
            }
        };
        let mut update = |done, total| {
            if let Some(callback) = progress.as_deref_mut() {
                callback(done, total);
            }
        };
        let output = search::search_at(
            search::Input {
                master: env.master,
                player: env.player,
                members: env.members,
                snaps: env.snaps,
                music: env.music,
                objective: &mut objective,
                constraints,
                k,
                deadline,
                now: env.now,
                seeds: initial,
                shard,
                owns: None,
                floor,
                floor_exclusive: false,

                streaming: true,
                disable_snap_dominance: false,
                member_power_percent: None,
                snap_power_percent: None,
                on_candidate: Some(&mut candidate),
                progress: Some(&mut update),
            },
            env.generation,
        )?;
        return Ok(output);
    }
    let k = k.max(1);
    let started = (env.now)();
    let generation = env.generation;
    let mut kernel = Kernel::new(env.master, env.members, env.snaps, chart, seeds, criterion);
    if let Some(bounds) = shared_bounds {
        kernel.global_fixed_preparation = bounds.preparation.clone();
        kernel.global_fixed_luck = bounds.luck.clone();
        #[cfg(not(target_arch = "wasm32"))]
        {
            kernel.shared_fixed_luck = Some(Arc::clone(&bounds.luck_cache));
        }
    }
    kernel.defer_scores = defer_scores
        && k > 1
        && !kernel.chart.luck
        && kernel.seeds.is_empty()
        && criterion == Criterion::Max;
    kernel.seed_preview = seed_only;
    kernel.live_floor = cooperative_floor
        && k == 1
        && criterion == Criterion::Max
        && kernel.chart.luck
        && kernel.seeds.len() >= 16;
    if kernel.chart.luck && !kernel.seeds.is_empty() {
        let forced = env
            .constraints
            .required_members
            .iter()
            .copied()
            .chain(env.constraints.bindings.keys().copied())
            .chain(
                env.constraints
                    .leader
                    .filter(|&leader| leader >= 0)
                    .map(|leader| leader as usize),
            )
            .collect::<BTreeSet<_>>();
        if forced.len() == 5 {
            kernel.fixed_forced_members = Some(forced.into_iter().collect());
        }
    }
    let minimum = if kernel
        .chart
        .setup
        .missions
        .iter()
        .any(|m| *m == 1 || *m == 4)
    {
        (min_combo_bonus - 1).max(0)
    } else {
        0
    };
    if minimum > 0 {
        let values: Vec<_> = kernel
            .opening_bonuses
            .iter()
            .map(|&value| value as f64)
            .collect();
        let maximum = values.iter().copied().fold(0.0, f64::max);
        env.constraints.member_floor = Some(search::MemberFloor {
            values,
            minimum: minimum as f64,
            maximum,
        });
    }
    let (mut levels, work_queue) = if let Some(schedule) = selected_levels {
        (schedule.levels, schedule.queue)
    } else {
        (
            reachable_levels(&env, &kernel.bonuses, minimum, kernel.maximum_level),
            None,
        )
    };
    let level_shards = !seed_only
        && k == 1
        && criterion == Criterion::Max
        && kernel.chart.luck
        && kernel.seeds.len() >= 16
        && env.members.len() >= 20
        && env.constraints.leader.is_none()
        && levels.len() >= 8
        && shard.is_some_and(|(_, count)| count > 1);
    if level_shards {
        let (index, count) = shard.unwrap();
        let owned: HashSet<_> = level_order(&env, &kernel.bonuses, &levels)
            .into_iter()
            .enumerate()
            .filter_map(|(slot, level)| (slot % count == index).then_some(level))
            .collect();
        levels.retain(|level| owned.contains(level));
    }
    let mut stats = search::Stats::default();
    if levels.is_empty() {
        return Ok(search::Output {
            hits: Vec::new(),
            proven: true,
            bound: None,
            stats,
        });
    }
    let mut hits = initial_scores.to_vec();
    let mut proven = true;
    let mut bound: Option<f64> = None;
    let late = || (env.now)() > deadline || generation != crate::cancellation_generation();
    let publish = |hits: &[Hit], callback: &mut Option<CandidateListener<'_>>| {
        if let Some(callback) = callback.as_deref_mut() {
            let ranked = ranked(hits, k);
            if ranked.iter().all(|hit| hit.detail["pendingScores"] != true) {
                callback(&ranked);
            }
        }
    };
    let owned: Option<Vec<String>> = if kernel.chart.luck
        && kernel.seeds.is_empty()
        && shard.is_some_and(|(_, count)| count > 1)
    {
        Some(
            kernel
                .gk_members
                .clone()
                .iter()
                .map(|p| {
                    let id = kernel.performer_key(p);
                    kernel.chart.performer_json[id as usize].clone()
                })
                .collect(),
        )
    } else {
        None
    };
    let owns = |members: &[usize]| {
        let Some(keys) = &owned else {
            return true;
        };
        let mut keys: [&str; 5] = std::array::from_fn(|slot| keys[members[slot]].as_str());
        keys.sort_unstable();
        let mut hash = 0x811c9dc5u32;
        for (index, key) in keys.iter().enumerate() {
            if index > 0 {
                hash = (hash ^ u32::from('|')).wrapping_mul(0x01000193);
            }
            for code in key.encode_utf16() {
                hash = (hash ^ code as u32).wrapping_mul(0x01000193);
            }
        }
        let (index, count) = shard.unwrap();
        hash as usize % count == index
    };
    let owned_seed_floor = kernel.chart.luck
        && kernel.seeds.is_empty()
        && seeded
        && !seed_only
        && floor.is_finite()
        && owned.is_some();
    for team in initial {
        if !search::valid_seed(env.members, env.snaps, &env.constraints, &team) {
            continue;
        }
        if team.members.iter().map(|&i| kernel.bonuses[i]).sum::<i32>() < minimum {
            continue;
        }
        if owned_seed_floor && !owns(&team.members) {
            continue;
        }
        if late() {
            break;
        }
        if hits
            .iter()
            .any(|hit| hit.team.members == team.members && hit.team.snaps == team.snaps)
        {
            continue;
        }
        let power = env.power(&team)?;
        let result = kernel.search_exact(&team, power.total as i32, true)?;
        kernel.exact_calls += 1;
        offer_scored(
            &mut kernel,
            &mut hits,
            env.members,
            Hit {
                team,
                power: power.total as f64,
                slot_powers: power.slots.iter().map(|&v| v as f64).collect(),
                totals: Totals {
                    power: power.total as f64,
                    ..Totals::default()
                },
                key: result.key,
                detail: result.detail,
            },
        )?;
        publish(&hits, &mut on_candidate);
    }
    let fixed_assignment = kernel.fixed_forced_members.as_ref().is_some_and(|members| {
        env.constraints.no_snaps
            || members
                .iter()
                .all(|member| env.constraints.bindings.contains_key(member))
    });
    let mut proposal_power = None;
    let projected_warm = seed_only
        && k == 1
        && criterion == Criterion::Max
        && kernel.seeds.len() >= 16
        && !seeded
        && !fixed_assignment;
    if kernel.chart.luck && hits.is_empty() && !owned_seed_floor && (env.now)() < deadline {
        let preference = kernel
            .gk_members
            .iter()
            .map(|p| {
                p.gekisou_skill
                    .map(|(id, level)| {
                        -(env
                            .master
                            .gekiso_skills
                            .get(&id)
                            .into_iter()
                            .flat_map(|s| &s.effects)
                            .filter(|r| r.level == level && (11000..=11005).contains(&r.kind))
                            .count() as f64)
                    })
                    .unwrap_or(0.0)
            })
            .collect();
        let mut feasible = Feasible {
            bonuses: kernel.bonuses.clone(),
            preference,
        };
        let out = env.run(
            &mut feasible,
            Run {
                constraints: {
                    let mut c = env.constraints.clone();
                    c.min_bonus = Some(minimum as f64);
                    c
                },
                k: 1,
                deadline: ((env.now)() + 100.0).min(deadline),
                seeds: Vec::new(),
                shard: None,
                floor: f64::NEG_INFINITY,
            },
            None,
            None,
        )?;
        if let Some(hit) = out.hits.first().filter(|_| (env.now)() < deadline) {
            let team = hit.team.clone();
            let power = env.power(&team)?;
            if projected_warm {
                proposal_power = Some(power.total as f64);
            } else {
                let result = kernel.search_exact(&team, power.total as i32, true)?;
                kernel.exact_calls += 1;
                offer_scored(
                    &mut kernel,
                    &mut hits,
                    env.members,
                    Hit {
                        team,
                        power: power.total as f64,
                        slot_powers: power.slots.iter().map(|&v| v as f64).collect(),
                        totals: Totals {
                            power: power.total as f64,
                            ..Totals::default()
                        },
                        key: result.key,
                        detail: result.detail,
                    },
                )?;
                publish(&hits, &mut on_candidate);
            }
        }
    }
    if (env.now)() >= deadline {
        let final_hits = kernel.complete_hits(&hits, k)?;
        stats.exact = kernel.exact_calls;
        stats.elapsed_ms = (env.now)() - started;
        return Ok(search::Output {
            hits: final_hits,
            proven: false,
            bound: None,
            stats,
        });
    }
    let seed_key = format!(
        "{}:{minimum}:{seed_variant:?}",
        seed_key(&env, criterion, k, &kernel.seeds)
    );
    let cached = if seeded || fixed_assignment {
        Some(Vec::new())
    } else {
        kernel.chart.seeded.get(&seed_key).cloned()
    };
    if let Some(cached) = cached {
        for hit in cached {
            if search::valid_seed(env.members, env.snaps, &env.constraints, &hit.team) {
                offer_scored(&mut kernel, &mut hits, env.members, hit)?;
            }
        }
    } else {
        let reference_power = hits
            .first()
            .map_or(proposal_power.unwrap_or(1.0), |hit| hit.power);
        let warm_k = if kernel.chart.luck && criterion == Criterion::Max {
            k.max(5)
        } else {
            k
        };
        let mut pool = if kernel.chart.luck && kernel.seeds.len() > 1 {
            let make_chart = || kernel.chart.converted_judgements(&kernel.chart.judgements);
            let mut single_chart = make_chart();
            let mut quiet_chart = make_chart();
            if let (Some(single), Some(quiet)) = (&mut single_chart, &mut quiet_chart) {
                let mut pool = Vec::new();
                if seed_variant != Some(true) {
                    let mut proposal = Kernel::new(
                        kernel.master,
                        kernel.members,
                        kernel.snaps,
                        single,
                        vec![kernel.seeds[0]],
                        criterion,
                    );
                    proposal.calibrate(deadline, env.now);
                    let mut sample = warm_pool(
                        &env,
                        &mut proposal,
                        minimum,
                        k,
                        deadline,
                        reference_power,
                        &late,
                    )?;
                    sample.sort_by(|a, b| b.1.total_cmp(&a.1));
                    sample.truncate(k);
                    pool.extend(sample);
                }
                if seed_variant != Some(false) {
                    let mut master = kernel.master.clone();
                    Arc::make_mut(&mut master.live).gekiso.rush_percent = 0;
                    let mut skills = (*master.gekiso_support_skills).clone();
                    for skill in skills.values_mut().filter(|skill| skill.mission_type == 2) {
                        skill.effects.retain(|row| row.kind != 2000);
                    }
                    master.gekiso_support_skills = Arc::new(skills);
                    let mut stable = Kernel::new(
                        &master,
                        kernel.members,
                        kernel.snaps,
                        quiet,
                        vec![kernel.seeds[0]],
                        criterion,
                    );
                    stable.calibrate(deadline, env.now);
                    let mut quiet = warm_pool(
                        &env,
                        &mut stable,
                        minimum,
                        k,
                        deadline,
                        reference_power,
                        &late,
                    )?;
                    quiet.sort_by(|a, b| b.1.total_cmp(&a.1));
                    quiet.truncate(k);
                    pool.extend(quiet);
                }
                pool
            } else {
                warm_pool(
                    &env,
                    &mut kernel,
                    minimum,
                    warm_k,
                    deadline,
                    reference_power,
                    &late,
                )?
            }
        } else {
            warm_pool(
                &env,
                &mut kernel,
                minimum,
                warm_k,
                deadline,
                reference_power,
                &late,
            )?
        };
        pool.sort_by(|a, b| b.1.total_cmp(&a.1));

        let slack = kernel.chart.luck && gekiso::settle_slack(kernel.chart) > 0;
        let mut ranked_pool = Vec::new();
        if slack {
            for (team, _) in pool {
                if late() {
                    break;
                }
                let power = env.power(&team)?;
                let upper = kernel.exact(&team, power.total as i32, false)?.key;
                ranked_pool.push((team, upper));
            }
            ranked_pool.sort_by(|a, b| b.1.total_cmp(&a.1));
        } else {
            ranked_pool = pool;
        }
        for (team, upper) in ranked_pool {
            if late() {
                break;
            }
            let best = ranked(&hits, k);
            let kth = if best.len() >= k {
                best[k - 1].key
            } else {
                f64::NEG_INFINITY
            };
            if slack && upper <= kth {
                break;
            }
            let power = env.power(&team)?;
            let result = kernel.search_exact(&team, power.total as i32, true)?;
            kernel.exact_calls += 1;
            offer_scored(
                &mut kernel,
                &mut hits,
                env.members,
                Hit {
                    team,
                    power: power.total as f64,
                    slot_powers: power.slots.iter().map(|&v| v as f64).collect(),
                    totals: Totals {
                        power: power.total as f64,
                        ..Totals::default()
                    },
                    key: result.key,
                    detail: result.detail,
                },
            )?;
            publish(&hits, &mut on_candidate);
        }
        if !late()
            && (kernel.chart.luck && (criterion == Criterion::Max || !kernel.seeds.is_empty())
                || !kernel.chart.luck
                    && kernel.healthy
                    && kernel.owner_local
                    && kernel.just_context.is_some())
        {
            if let Some(best) = ranked(&hits, 1).into_iter().next() {
                let collect = kernel.chart.luck
                    && kernel.seeds.is_empty()
                    && criterion == Criterion::Max
                    && k > 1;
                let improved = refine_seed(
                    &env,
                    &mut kernel,
                    best,
                    &[],
                    &late,
                    collect.then_some((&mut hits, k)),
                )?;
                offer_scored(&mut kernel, &mut hits, env.members, improved)?;
                publish(&hits, &mut on_candidate);
            }
            if !kernel.chart.luck {
                for rank in 1..k {
                    if late() {
                        break;
                    }
                    let current = ranked(&hits, k);
                    let Some(first) = current.first() else {
                        break;
                    };
                    let excluded: Vec<_> = current
                        .iter()
                        .take(rank)
                        .map(|hit| {
                            let mut members = hit.team.members.clone();
                            members.sort_unstable();
                            members
                        })
                        .collect();
                    let seed = current.get(rank).cloned().unwrap_or_else(|| {
                        let mut seed = first.clone();
                        seed.key = f64::NEG_INFINITY;
                        seed
                    });
                    let improved = refine_seed(&env, &mut kernel, seed, &excluded, &late, None)?;
                    if improved.key.is_finite() {
                        offer_scored(&mut kernel, &mut hits, env.members, improved)?;
                        publish(&hits, &mut on_candidate);
                    }
                }
            }
        }
        if !late() {
            if kernel.chart.seeded.len() > 8 {
                kernel.chart.seeded.clear();
            }
            kernel.chart.seeded.insert(seed_key, hits.clone());
        }
    }
    if seed_only {
        let final_hits = kernel.complete_hits(&hits, k)?;
        stats.exact = kernel.exact_calls;
        stats.elapsed_ms = (env.now)() - started;
        return Ok(search::Output {
            hits: final_hits,
            proven: false,
            bound,
            stats,
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    if whole
        && shard.is_none()
        && k == 1
        && criterion == Criterion::Max
        && kernel.chart.luck
        && kernel.seeds.len() >= 16
        && (env.members.len() >= 20 || kernel.live_floor)
        && env.constraints.leader.is_none()
        && !late()
    {
        let workers = crate::parallel::preparation_workers();
        if workers > 1 {
            let split_leaders = levels.len() < 8;
            let progress_total = levels.len() * if split_leaders { workers } else { 1 };
            let queue = Arc::new(std::sync::Mutex::new(
                level_order(&env, &kernel.bonuses, &levels)
                    .into_iter()
                    .collect(),
            ));
            let jobs = (0..workers)
                .map(|_| {
                    let levels = levels.clone();
                    kernel
                        .chart
                        .converted_judgements(&kernel.chart.judgements)
                        .map(|chart| (levels, chart))
                })
                .collect::<Option<Vec<_>>>();
            if let Some(jobs) = jobs {
                hits = kernel.complete_hits(&hits, k)?;
                let inherited = hits.clone();
                let _ = Adapter::new(&mut kernel, levels[0], false, false, deadline, env.now);
                let seed = NativeLevelSeed {
                    seeds: kernel.seeds.clone(),
                    cooperative_floor: kernel.live_floor,
                    split_leaders,
                    k,
                    deadline,
                    min_combo_bonus,
                    floor,
                    hits: hits.clone(),
                    queue,
                    bounds: SharedLevelBounds {
                        preparation: kernel.global_fixed_preparation.clone(),
                        luck: kernel.global_fixed_luck.clone(),
                        luck_cache: Arc::clone(kernel.shared_fixed_luck.get_or_insert_with(|| {
                            Arc::new(SharedLuckCache::new(kernel.chart.notes.len()))
                        })),
                    },
                };
                let mut finished = vec![0; jobs.len()];
                let outputs = native_levels(&env, jobs, seed, |update| {
                    match update {
                        NativeLevelUpdate::Candidates(candidates) => {
                            for hit in candidates {
                                offer_scored(&mut kernel, &mut hits, env.members, hit)?;
                            }
                            publish(&hits, &mut on_candidate);
                        }
                        NativeLevelUpdate::Progress(worker, done) => {
                            finished[worker] = done;
                            if let Some(progress) = progress.as_deref_mut() {
                                progress(finished.iter().sum(), progress_total);
                            }
                        }
                    }
                    Ok(())
                })?;
                let mut candidates = Vec::new();
                for output in outputs {
                    stats.add(&output.stats);
                    if !output.proven {
                        proven = false;
                        bound = Some(
                            bound
                                .unwrap_or(f64::NEG_INFINITY)
                                .max(output.bound.unwrap_or(f64::NEG_INFINITY)),
                        );
                    }
                    candidates.extend(output.hits);
                }
                candidates.sort_by_key(|hit| {
                    std::cmp::Reverse(
                        hit.team
                            .members
                            .iter()
                            .map(|&member| kernel.bonuses[member])
                            .sum::<i32>(),
                    )
                });
                hits = inherited;
                for hit in candidates {
                    offer_scored(&mut kernel, &mut hits, env.members, hit)?;
                }
                stats.exact += kernel.exact_calls;
                stats.elapsed_ms = (env.now)() - started;
                return Ok(search::Output {
                    hits: kernel.complete_hits(&hits, k)?,
                    proven,
                    bound,
                    stats,
                });
            }
        }
    }
    let mut fixed = levels.iter().copied();
    let mut completed_levels = 0;
    let work = std::iter::from_fn(|| {
        if let Some(queue) = &work_queue {
            queue.lock().unwrap().pop_front()
        } else {
            fixed.next()
        }
    });
    for (index, level) in work.enumerate() {
        if let Some(progress) = progress.as_deref_mut() {
            progress(
                if work_queue.is_some() {
                    completed_levels
                } else {
                    index
                },
                levels.len(),
            );
        }
        if late() {
            proven = false;
            break;
        }
        let maximum = kernel.maximum_level;
        let luck = kernel.chart.luck;
        let best = ranked(&hits, k);
        let level_floor = floor.max(if best.len() >= k {
            best[k - 1].key
        } else {
            f64::NEG_INFINITY
        });
        let Some(mut adapter) = Adapter::new(&mut kernel, level, false, false, deadline, env.now)
        else {
            proven = false;
            break;
        };
        if !luck {
            adapter.seed_scores = best.clone();
        }
        if !warm(&mut adapter, &env, deadline) {
            proven = false;
            break;
        }
        let mut candidate = |candidates: &[Hit]| {
            for hit in candidates {
                if !hit.detail.is_null() {
                    offer(&mut hits, env.members, hit.clone());
                }
            }
            publish(&hits, &mut on_candidate);
        };
        let out = env.run(
            &mut adapter,
            Run {
                constraints: level_constraints(&env.constraints, level, maximum),
                k,
                deadline,
                seeds: best.into_iter().map(|hit| hit.team).collect(),
                shard: if owned.is_some() || level_shards {
                    None
                } else {
                    shard
                },
                floor: level_floor,
            },
            if owned.is_some() { Some(&owns) } else { None },
            Some(&mut candidate),
        )?;
        stats.leaders += out.stats.leaders;
        stats.member_nodes += out.stats.member_nodes;
        stats.snap_nodes += out.stats.snap_nodes;
        stats.candidates += out.stats.candidates;
        if !out.proven {
            proven = false;
            bound = Some(
                bound
                    .unwrap_or(f64::NEG_INFINITY)
                    .max(out.bound.unwrap_or(f64::NEG_INFINITY)),
            );
        }
        for hit in out.hits {
            if !hit.detail.is_null() {
                offer_scored(&mut kernel, &mut hits, env.members, hit)?;
            }
        }
        if out.proven {
            completed_levels += 1;
        }
        if work_queue.is_some() {
            if let Some(progress) = progress.as_deref_mut() {
                progress(completed_levels, levels.len());
            }
        }
    }
    if work_queue.is_some() {
        hits.sort_by_key(|hit| {
            let initial = initial_scores.iter().any(|seed| {
                seed.team.members == hit.team.members && seed.team.snaps == hit.team.snaps
            });
            (
                std::cmp::Reverse(initial),
                std::cmp::Reverse(
                    hit.team
                        .members
                        .iter()
                        .map(|&member| kernel.bonuses[member])
                        .sum::<i32>(),
                ),
            )
        });
    }
    let final_hits = kernel.complete_hits(&hits, k)?;
    stats.exact = kernel.exact_calls;
    stats.elapsed_ms = (env.now)() - started;
    Ok(search::Output {
        hits: final_hits,
        proven,
        bound,
        stats,
    })
}
