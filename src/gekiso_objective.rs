//! Gekiso context caches and safe level/support bounds for exact search.
use crate::{
    full::conditions::Performer,
    gekiso::{self, Chart, Context},
    live::Slot,
    master::{GekisoSkill, Master, SkillEffect},
    num::window_ms,
    objectives::Criterion,
    power::{self, Member, Music, Player, Snap},
    search::{self, Exact, Hit, Objective, Team, Totals},
};
use rustc_hash::FxHashMap as HashMap;
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashSet, VecDeque},
    sync::Arc,
};
const REFINE_SLACK: f64 = 1e-4;
const RELATIVE: f64 = 3e-6;
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
    let mut groups = HashSet::new();
    let mut cumulative = HashSet::new();
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
struct Base {
    weights: Arc<Weights>,
    deck: Vec<Performer>,
    gains: HashMap<Performer, f64>,
    lives: HashMap<usize, f64>,
}
type SharedBase = Arc<std::sync::Mutex<Base>>;
type RefineBases = HashMap<(Vec<usize>, Vec<Performer>), SharedBase>;
type Ownership<'a> = &'a (dyn Fn(&[usize]) -> bool + 'a);
type CandidateListener<'a> = &'a mut (dyn FnMut(&[Hit]) + 'a);
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
    support: Vec<Option<Performer>>,
    support_chain: Vec<bool>,
    performer_key_cache: HashMap<Performer, u32>,
    slots: Vec<Slot>,
    pub bonuses: Vec<i32>,
    pub maximum_level: i32,
    grid: Vec<i32>,
    template: Option<SkillEffect>,
    luck_deck: Vec<Performer>,
    luck_members: Vec<Performer>,
    luck_supports: Vec<Performer>,
    cache: HashMap<(i32, Vec<u32>), Cached>,
    cache_order: VecDeque<(i32, Vec<u32>)>,
    cache_bytes: usize,
    cache_records: HashMap<usize, usize>,
    levels: HashMap<i32, Arc<Level>>,
    support_high: HashMap<(String, i32), f64>,
    support_low: HashMap<String, f64>,
    base: Option<Arc<Weights>>,
    member_bases: HashMap<(Vec<Performer>, usize), f64>,
    luck_bases: HashMap<Vec<u32>, Arc<std::sync::Mutex<Base>>>,
    refine_bases: RefineBases,
    rush: HashMap<Performer, Option<f64>>,
    chain: HashMap<Performer, bool>,
    exact_calls: usize,
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
    members.iter().map(|m| {
        if !combo { return 0; }
        let p = Performer::from_member(master, m, None);
        let Some((id, level)) = p.gekisou_skill else { return 0; };
        master.gekiso_skills.get(&id)
            .filter(|s| s.mission_type == 1 || s.mission_type == 4)
            .into_iter().flat_map(|s| &s.effects)
            .filter(|r| r.level == level && r.kind == 12000)
            .map(|r| r.value).max().unwrap_or(0).max(0)
    }).collect()
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
                Performer::from_member(master, m, None)
                    .gekisou_skill
                    .is_some()
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
        let mut support = Vec::with_capacity(members.len() * snaps.len());
        let mut supports: HashMap<String, Performer> = HashMap::default();
        for (i, member) in members.iter().enumerate() {
            for snap in snaps {
                if !has_gk[i] {
                    support.push(None);
                    continue;
                }
                let p = Performer::from_member(master, member, Some(snap));
                let runs = p.gekisou_support_skills.iter().any(|&(id, _)| {
                    let mission = master
                        .gekiso_support_skills
                        .get(&id)
                        .map(|s| s.mission_type)
                        .unwrap_or(0);
                    mission == 4 || missions.contains(&mission) || missions.contains(&4)
                });
                if p.gekisou_support_skills.is_empty() || !runs {
                    support.push(None);
                    continue;
                }
                let band_only = read_all == ["bandId"];
                let favoured = favoured_band(master, &p.gekisou_support_skills);
                let band = if band_only && favoured != 0 {
                    if p.band_id == favoured {
                        favoured
                    } else if favoured == 1 {
                        2
                    } else {
                        1
                    }
                } else {
                    p.band_id
                };
                let key = json!([
                    p.gekisou_support_skills,
                    if band_only {
                        json!(band)
                    } else {
                        json!([p.band_id, p.card_type, p.character_id])
                    }
                ])
                .to_string();
                let value = supports.entry(key).or_insert_with(|| {
                    if band_only {
                        Performer {
                            band_id: band,
                            mission_only: Some(1),
                            gekisou_support_skills: p.gekisou_support_skills.clone(),
                            ..Performer::default()
                        }
                    } else {
                        p.support_only()
                    }
                });
                support.push(Some(value.clone()));
            }
        }
        let bonuses = combo_bonuses(master, members, &chart.setup.missions);
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
        let mut slots = Vec::new();
        let mut unsupported = Vec::new();
        for member in members {
            for snap in std::iter::once(None).chain(snaps.iter().map(Some)) {
                slots.push(crate::skills::resolve(
                    master,
                    member,
                    snap,
                    &mut unsupported,
                ));
            }
        }
        let cache = std::mem::take(&mut chart.contexts);
        let cache_order = std::mem::take(&mut chart.context_order);
        let cache_bytes = std::mem::replace(&mut chart.context_bytes, 0);
        let cache_records = std::mem::take(&mut chart.context_records);
        let support_chain = support
            .iter()
            .map(|p| {
                p.as_ref().is_some_and(|p| {
                    p.gekisou_support_skills.iter().any(|&(id, level)| {
                        master.gekiso_support_skills.get(&id).is_some_and(|skill| {
                            skill
                                .effects
                                .iter()
                                .any(|r| r.level == level && (11000..=11005).contains(&r.kind))
                        })
                    })
                })
            })
            .collect();
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
            support,
            support_chain,
            performer_key_cache: HashMap::default(),
            slots,
            bonuses,
            maximum_level,
            grid,
            template,
            luck_deck: Vec::new(),
            luck_members: Vec::new(),
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
            refine_bases: HashMap::default(),
            rush: HashMap::default(),
            chain: HashMap::default(),
            exact_calls: 0,
        }
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
    /// A deck's note weights. Bounds and gains read only these, so the context behind them is not kept.
    fn weights_of(&mut self, deck: &[Performer], within: Option<&Master>, tag: i32) -> Arc<Weights> {
        let mut keyed: Vec<_> = deck
            .iter()
            .map(|p| (p.clone(), self.performer_key(p)))
            .collect();
        keyed.sort_by(|a, b| {
            self.chart.performer_json[a.1 as usize].cmp(&self.chart.performer_json[b.1 as usize])
        });
        let key = (tag, keyed.iter().map(|p| p.1).collect::<Vec<_>>());
        if let Some(weights) = self.chart.weights.get(&key) {
            return Arc::clone(weights);
        }
        if let Some(value) = self.cache.get(&key) {
            return Arc::clone(&value.weights);
        }
        let context = gekiso::context(
            within.unwrap_or(self.master),
            self.chart,
            &keyed.into_iter().map(|p| p.0).collect::<Vec<_>>(),
            &self.seeds,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let weights = Arc::new(self.weights(&context));
        let bytes = 128 + (weights.general.len() + weights.judgements.iter().map(Vec::len).sum::<usize>()) * 8;
        const WEIGHT_BYTES: usize = 32 * 1024 * 1024;
        while self.chart.weight_bytes + bytes > WEIGHT_BYTES {
            let Some((old, size)) = self.chart.weight_order.pop_front() else {
                break;
            };
            self.chart.weights.remove(&old);
            self.chart.weight_bytes -= size;
        }
        self.chart.weight_bytes += bytes;
        self.chart.weight_order.push_back((key.clone(), bytes));
        self.chart.weights.insert(key, Arc::clone(&weights));
        weights
    }
    fn context_of(
        &mut self,
        deck: &[Performer],
        within: Option<&Master>,
        tag: i32,
    ) -> (Arc<Context>, Arc<Weights>) {
        let mut keyed: Vec<_> = deck
            .iter()
            .map(|p| (p.clone(), self.performer_key(p)))
            .collect();
        keyed.sort_by(|a, b| {
            self.chart.performer_json[a.1 as usize].cmp(&self.chart.performer_json[b.1 as usize])
        });
        let key = (tag, keyed.iter().map(|p| p.1).collect::<Vec<_>>());
        if let Some(value) = self.cache.get(&key) {
            return (Arc::clone(&value.context), Arc::clone(&value.weights));
        }
        let context = Arc::new(
            gekiso::context(
                within.unwrap_or(self.master),
                self.chart,
                &keyed.into_iter().map(|p| p.0).collect::<Vec<_>>(),
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
    /// The sorted deck and its cache key.
    fn deck_key(&mut self, deck: &[Performer], tag: i32) -> (Vec<Performer>, (i32, Vec<u32>)) {
        let mut keyed: Vec<_> = deck.iter().map(|p| (p.clone(), self.performer_key(p))).collect();
        keyed.sort_by(|a, b| self.chart.performer_json[a.1 as usize].cmp(&self.chart.performer_json[b.1 as usize]));
        let key = (tag, keyed.iter().map(|p| p.1).collect::<Vec<_>>());
        (keyed.into_iter().map(|p| p.0).collect(), key)
    }
    /// Computes the support bounds a warm pass at `level` is about to read, several at a time where the target has
    /// threads, and keeps their weights in the cache that `weights_of` reads.
    fn prefetch_supports(&mut self, level: i32, pairs: &[(usize, i32)]) {
        if self.chart.luck {
            return;
        }
        let at = self.grid.iter().find(|&&g| g >= level).copied().unwrap_or(self.maximum_level);
        let data = self.level(at);
        let mut seen = HashSet::new();
        let mut jobs = Vec::new();
        for &(m, s) in pairs {
            let Some(p) = self.support(m, s) else { continue };
            if self.support_high.contains_key(&(json!([p.gekisou_support_skills, p.band_id]).to_string(), at)) {
                continue;
            }
            let combined: Vec<Performer> = data.deck.iter().cloned().chain(std::iter::once(p)).collect();
            let (deck, key) = self.deck_key(&combined, at);
            if self.chart.weights.contains_key(&key) || self.cache.contains_key(&key) || !seen.insert(key.clone()) {
                continue;
            }
            jobs.push((deck, key));
        }
        if jobs.len() < 2 {
            return;
        }
        let (master, chart, seeds) = (&data.within, &*self.chart, &self.seeds);
        let Ok(found) = crate::parallel::map(&jobs, |(deck, _)| {
            gekiso::context_fixed(master, chart, deck, seeds).map(|ctx| note_weights(master, chart, &ctx))
        }) else {
            return;
        };
        for ((_, key), weights) in jobs.into_iter().zip(found) {
            self.store_weights(key, Arc::new(weights));
        }
    }
    fn store_weights(&mut self, key: (i32, Vec<u32>), weights: Arc<Weights>) {
        let bytes = 128 + (weights.general.len() + weights.judgements.iter().map(Vec::len).sum::<usize>()) * 8;
        const WEIGHT_BYTES: usize = 32 * 1024 * 1024;
        while self.chart.weight_bytes + bytes > WEIGHT_BYTES {
            let Some((old, size)) = self.chart.weight_order.pop_front() else {
                break;
            };
            self.chart.weights.remove(&old);
            self.chart.weight_bytes -= size;
        }
        self.chart.weight_bytes += bytes;
        self.chart.weight_order.push_back((key.clone(), bytes));
        self.chart.weights.insert(key, weights);
    }
}
/// A deck's per-note score weights under its context.
pub(crate) fn note_weights(master: &Master, chart: &Chart, ctx: &Context) -> Weights {
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
    fn gain(
        &mut self,
        deck: &[Performer],
        reference: &[Performer],
        within: Option<&Master>,
        tag: i32,
    ) -> f64 {
        let combined: Vec<_> = reference.iter().chain(deck).cloned().collect();
        self.weights_of(&combined, within, tag).base
            - self.weights_of(reference, within, tag).base
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
        let key = shape(p);
        *self.chain.entry(key).or_insert_with(|| {
            p.gekisou_support_skills.iter().any(|&(id, level)| {
                self.master
                    .gekiso_support_skills
                    .get(&id)
                    .into_iter()
                    .flat_map(|s| &s.effects)
                    .any(|r| r.level == level && (11000..=11005).contains(&r.kind))
            })
        })
    }
    fn rush_factor(&mut self, p: &Performer) -> Option<f64> {
        let key = shape(p);
        *self.rush.entry(key).or_insert_with(|| {
            gekiso::rush_only_factor(self.master, p).unwrap_or_else(|e| panic!("{e}"))
        })
    }
    fn deck_of(&self, team: &Team) -> Vec<Performer> {
        team.members
            .iter()
            .enumerate()
            .map(|(slot, &m)| {
                let mut p = self.gk_members[m].clone();
                if let Some(support) = self.support(m, team.snaps[slot]) {
                    p.band_id = support.band_id;
                    p.gekisou_support_skills = support.gekisou_support_skills;
                }
                p
            })
            .collect()
    }
    pub fn exact(&mut self, team: &Team, power: i32, settle: bool) -> Result<Exact, String> {
        let deck = self.deck_of(team);
        let ctx = self.context_of(&deck, None, 0).0;
        let skills: Vec<_> = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &m)| self.slot(m, team.snaps[slot]).clone())
            .collect();
        let scores = gekiso::score_orders(self.master, self.chart, &ctx, power, &skills, settle)?;
        Ok(Exact {
            key: self.criterion.key(&scores),
            detail: json!({"scores":scores,"seeds":ctx.seeds,"comboBonus":if self.chart.setup.missions.iter().any(|m| *m == 1 || *m == 4) { Some(1 + team.members.iter().map(|&i| self.bonuses[i]).sum::<i32>()) } else { None }}),
        })
    }
}
impl Kernel<'_> {
    pub fn calibrate(&mut self, deadline: f64, now: &dyn Fn() -> f64) -> bool {
        if !self.chart.luck {
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
            values.push((i, value));
        }
        let mut member_luck: Vec<_> = values.into_iter().filter(|v| v.1 > 0.0).collect();
        member_luck.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut characters = HashSet::new();
        for &(i, _) in &member_luck {
            let character = self.members[i].card.character_id;
            if characters.contains(&character) || characters.len() == 5 {
                continue;
            }
            characters.insert(character);
            self.luck_deck.push(self.gk_members[i].clone());
        }
        let mut seen = HashSet::new();
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
    fn strongest(&self, variants: Vec<Performer>) -> Vec<Performer> {
        if variants.len() < 2 {
            return variants;
        }
        let first = &variants[0].gekisou_support_skills;
        if variants.iter().any(|p| p.gekisou_support_skills != *first) {
            return variants;
        }
        let favored = favoured_band(self.master, first);
        if let Some(p) = variants.iter().find(|p| p.band_id == favored) {
            if favoured_dominates(self.master, first) {
                return vec![p.clone()];
            }
        }
        variants
    }
    fn luck_base(
        &mut self,
        members: &[Performer],
        chained: &[Performer],
        member_ids: &[u32],
    ) -> Arc<std::sync::Mutex<Base>> {
        // This is an unordered cache identity, not the simulation's performer
        // order. Keep context_of's canonical JSON order for numeric parity.
        let mut key = member_ids.to_vec();
        key.extend(chained.iter().map(|p| self.performer_key(p)));
        key.sort_unstable();
        if let Some(base) = self.luck_bases.get(&key) {
            return Arc::clone(base);
        }
        if self.luck_bases.len() >= 2048 {
            self.luck_bases.clear();
        }
        let deck: Vec<_> = members.iter().chain(chained).cloned().collect();
        let weights = self.weights_of(&deck, None, 0);
        let base = Arc::new(std::sync::Mutex::new(Base {
            weights,
            deck,
            gains: HashMap::default(),
            lives: HashMap::default(),
        }));
        self.luck_bases.insert(key, Arc::clone(&base));
        base
    }
    fn luck_slot(
        &mut self,
        base: &Arc<std::sync::Mutex<Base>>,
        m: usize,
        s: i32,
        pick: Criterion,
    ) -> f64 {
        let index = self.slot_index(m, s) * 3
            + match pick {
                Criterion::Mean => 0,
                Criterion::Max => 1,
                Criterion::Min => 2,
            };
        let mut base = base.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&value) = base.lives.get(&index) {
            return value;
        }
        let mut value = self.live_weight(self.slot(m, s), &base.weights, pick);
        if let Some(p) = self.support(m, s) {
            if !self.support_chain[m * self.snaps.len() + s as usize] {
                let key = shape(&p);
                let gain = if let Some(&value) = base.gains.get(&key) {
                    value
                } else {
                    let value = if let Some(factor) = self.rush_factor(&p) {
                        factor * base.weights.rush
                    } else {
                        self.gain(std::slice::from_ref(&p), &base.deck, None, 0)
                    };
                    base.gains.insert(key, value);
                    value
                };
                value += gain;
            }
        }
        base.lives.insert(index, value);
        value
    }
    fn refine(&mut self, team: &Team, power: f64) -> f64 {
        let supports: Vec<_> = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &m)| self.support(m, team.snaps[slot]))
            .collect();
        let mut chained = Vec::new();
        if self.chart.luck {
            for p in supports.iter().flatten() {
                if self.chain(p) {
                    chained.push(p.clone());
                }
            }
        }
        let mut members = team.members.clone();
        members.sort_unstable();
        let mut ids: Vec<_> = chained.iter().map(shape).collect();
        ids.sort();
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
                .chain(chained)
                .collect();
            let weights = self.weights_of(&deck, None, 0);
            let base = Arc::new(std::sync::Mutex::new(Base {
                weights,
                deck,
                gains: HashMap::default(),
                lives: HashMap::default(),
            }));
            self.refine_bases.insert(key, Arc::clone(&base));
            base
        };
        let mut base = base.lock().unwrap_or_else(|e| e.into_inner());
        let mut total = base.weights.base;
        for (slot, &m) in team.members.iter().enumerate() {
            let snap = team.snaps[slot];
            if let Some(p) = &supports[slot] {
                if !(self.chart.luck && self.chain(p)) {
                    let key = shape(p);
                    let gain = if let Some(&value) = base.gains.get(&key) {
                        value
                    } else {
                        let factor = if self.chart.luck {
                            self.rush_factor(p)
                        } else {
                            None
                        };
                        let value = if let Some(factor) = factor {
                            factor * base.weights.rush
                        } else {
                            self.gain(std::slice::from_ref(p), &base.deck, None, 0)
                        };
                        base.gains.insert(key, value);
                        value
                    };
                    total += gain;
                }
            }
            let index = self.slot_index(m, snap);
            let live = if let Some(&value) = base.lives.get(&index) {
                value
            } else {
                let value = self.live_weight(self.slot(m, snap), &base.weights, self.criterion);
                base.lives.insert(index, value);
                value
            };
            total += live;
        }
        power * total / self.chart.full.converted as f64
    }
}
struct Assignment {
    member_deck: Vec<Performer>,
    member_ids: Vec<u32>,
    options: Vec<Vec<i32>>,
    chain_snaps: Vec<i32>,
}
pub struct Adapter<'k, 'a> {
    kernel: &'k mut Kernel<'a>,
    level: i32,
    surrogate: bool,
    estimate: bool,
    data: Arc<Level>,
    ranges: HashMap<usize, (f64, f64)>,
    assignment: Option<Assignment>,
}
impl<'k, 'a> Adapter<'k, 'a> {
    fn new(kernel: &'k mut Kernel<'a>, level: i32, surrogate: bool, estimate: bool) -> Self {
        let data = kernel.level(level);
        Self {
            kernel,
            level,
            surrogate,
            estimate,
            data,
            ranges: HashMap::default(),
            assignment: None,
        }
    }
    fn high(&self, t: Totals) -> f64 {
        t.power * (self.data.weights.base + t.skill) / self.kernel.chart.full.converted as f64
            * (1.0 + RELATIVE)
            + (self.kernel.chart.notes.len() + 4) as f64
    }
}
impl Objective for Adapter<'_, '_> {
    fn skill(&mut self, m: usize, s: i32) -> (f64, f64) {
        let index = self.kernel.slot_index(m, s);
        if let Some(&value) = self.ranges.get(&index) {
            return value;
        }
        let top = self.kernel.live_weight(
            self.kernel.slot(m, s),
            &self.data.weights,
            if self.kernel.criterion == Criterion::Max {
                Criterion::Max
            } else {
                Criterion::Mean
            },
        ) + self.kernel.support_high(m, s, self.level);
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
    fn member_bonus(&self, m: usize) -> f64 {
        self.kernel.bonuses[m] as f64
    }
    fn bound(&mut self, t: Totals) -> f64 {
        self.high(t)
    }
    fn product_base(&self) -> Option<f64> {
        Some(self.data.weights.base)
    }
    fn leaf_bound(&mut self, team: &Team, t: Totals) -> Option<f64> {
        if self.kernel.criterion != Criterion::Max || self.surrogate {
            return None;
        }
        let rows: Vec<_> = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &m)| {
                self.kernel
                    .event_weights(self.kernel.slot(m, team.snaps[slot]), &self.data.weights)
            })
            .collect();
        let separate = rows.iter().fold(0.0, |sum, row| {
            sum + row.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        });
        let assigned = best_order(&rows);
        Some(self.high(Totals {
            skill: t.skill - separate + assigned,
            ..t
        }))
    }
    fn member_bound(&mut self, members: &[usize], t: Totals, left: usize) -> Option<f64> {
        if !self.kernel.chart.luck || self.surrogate {
            return None;
        }
        if self.kernel.read_all.iter().any(|&field| field != "bandId") {
            return Some(f64::INFINITY);
        }
        let deck: Vec<_> = members
            .iter()
            .map(|&i| self.kernel.gk_members[i].clone())
            .collect();
        let mut keys: Vec<_> = deck.iter().map(shape).collect();
        keys.sort();
        let key = (keys, left);
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
            let mut bound_deck = deck;
            for key in order {
                bound_deck.extend(copies.remove(&key).unwrap());
            }
            bound_deck.extend(self.kernel.luck_supports.iter().cloned());
            let base = self.kernel.weights_of(&bound_deck, None, 0).base;
            self.kernel.member_bases.insert(key, base);
            base
        };
        Some(
            t.power * (base + t.skill) / self.kernel.chart.full.converted as f64 * (1.0 + RELATIVE)
                + (self.kernel.chart.notes.len() + 4) as f64,
        )
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        if self.kernel.chart.luck && !self.surrogate {
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
            self.assignment = Some(Assignment {
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
                chain_snaps,
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
        if !self.kernel.chart.luck || self.surrogate {
            return None;
        }
        let assignment = self.assignment.as_ref()?;
        let options = &assignment.options;
        let member_deck = &assignment.member_deck;
        let taken = &assigned[..from];
        let mut chained = Vec::new();
        for &snap in &assignment.chain_snaps {
            if let Some(slot) = taken.iter().position(|&s| s == snap) {
                if let Some(p) = self.kernel.support(members[slot], snap) {
                    if self.kernel.chain(&p) {
                        chained.push(p);
                    }
                }
                continue;
            }
            let mut variants = Vec::new();
            let mut seen = HashSet::new();
            for slot in from..5 {
                if options[slot].contains(&snap) {
                    if let Some(p) = self.kernel.support(members[slot], snap) {
                        if self.kernel.chain(&p) && seen.insert(shape(&p)) {
                            variants.push(p);
                        }
                    }
                }
            }
            chained.extend(self.kernel.strongest(variants));
        }
        let base = self
            .kernel
            .luck_base(member_deck, &chained, &assignment.member_ids);
        let pick = if self.kernel.criterion == Criterion::Max {
            Criterion::Max
        } else {
            Criterion::Mean
        };
        let mut value = base.lock().unwrap_or_else(|e| e.into_inner()).weights.base;
        for slot in 0..from {
            value += self
                .kernel
                .luck_slot(&base, members[slot], assigned[slot], pick);
        }
        for slot in from..5 {
            let mut top = f64::NEG_INFINITY;
            for &snap in &options[slot] {
                if snap >= 0 && taken.contains(&snap) {
                    continue;
                }
                let chain = snap >= 0
                    && self.kernel.support_chain
                        [members[slot] * self.kernel.snaps.len() + snap as usize];
                top = top.max(self.kernel.luck_slot(
                    &base,
                    members[slot],
                    if chain { -1 } else { snap },
                    pick,
                ));
            }
            value += top;
        }
        Some(
            t.power * value / self.kernel.chart.full.converted as f64 * (1.0 + REFINE_SLACK)
                + (self.kernel.chart.notes.len() + 4) as f64,
        )
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
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
        self.kernel.exact(team, power as i32, true)
    }
}
fn best_order(rows: &[[f64; 5]]) -> f64 {
    let mut states = [f64::NEG_INFINITY; 32];
    states[0] = 0.0;
    for row in rows {
        let previous = states;
        states.fill(f64::NEG_INFINITY);
        for (mask, &value) in previous.iter().enumerate() {
            if value == f64::NEG_INFINITY {
                continue;
            }
            for (event, &weight) in row.iter().enumerate() {
                if mask & (1 << event) == 0 {
                    let next = mask | (1 << event);
                    states[next] = states[next].max(value + weight);
                }
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
        search::search(search::Input {
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
        })
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
pub struct Input<'a> {
    pub env: Environment<'a>,
    pub chart: &'a mut Chart,
    pub seeds: Vec<i32>,
    pub criterion: Criterion,
    pub k: usize,
    pub deadline: f64,
    pub initial: Vec<Team>,
    pub seed_only: bool,
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
    hits.sort_by(|a, b| b.key.total_cmp(&a.key));
    hits.truncate(k);
    hits
}
fn offer(hits: &mut Vec<Hit>, members: &[Member], hit: Hit) {
    let key = set_key(members, &hit.team);
    if let Some(index) = hits
        .iter()
        .position(|old| set_key(members, &old.team) == key)
    {
        if hits[index].key < hit.key {
            hits[index] = hit;
        }
    } else {
        hits.push(hit);
    }
}
fn warm(adapter: &mut Adapter<'_, '_>, env: &Environment<'_>, deadline: f64) -> bool {
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
pub fn search(input: Input<'_>) -> Result<search::Output, String> {
    let Input {
        env,
        chart,
        seeds,
        criterion,
        k,
        deadline,
        initial,
        seed_only,
        min_combo_bonus,
        seeded,
        shard,
        floor,
        mut on_candidate,
        mut progress,
    } = input;
    let k = k.max(1);
    let started = (env.now)();
    let generation = crate::cancellation_generation();
    let mut kernel = Kernel::new(env.master, env.members, env.snaps, chart, seeds, criterion);
    let minimum = if kernel.chart.setup.missions.iter().any(|m| *m == 1 || *m == 4) { (min_combo_bonus - 1).max(0) } else { 0 };
    let levels: Vec<_> = (minimum..=kernel.maximum_level).rev().collect();
    let mut stats = search::Stats::default();
    if levels.is_empty() { return Ok(search::Output { hits: Vec::new(), proven: true, bound: None, stats }); }
    let mut hits = Vec::new();
    let mut proven = true;
    let mut bound: Option<f64> = None;
    let late = || (env.now)() > deadline || generation != crate::cancellation_generation();
    let publish = |hits: &[Hit], callback: &mut Option<CandidateListener<'_>>| {
        if let Some(callback) = callback.as_deref_mut() {
            callback(&ranked(hits, k));
        }
    };
    for team in initial {
        if team.members.iter().map(|&i| kernel.bonuses[i]).sum::<i32>() < minimum { continue; }
        if late() {
            break;
        }
        let power = env.power(&team)?;
        let result = kernel.exact(&team, power.total as i32, true)?;
        kernel.exact_calls += 1;
        offer(
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
        );
        publish(&hits, &mut on_candidate);
    }
    if kernel.chart.luck && hits.is_empty() && (env.now)() < deadline {
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
                constraints: { let mut c = env.constraints.clone(); c.min_bonus = Some(minimum as f64); c },
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
            let result = kernel.exact(&team, power.total as i32, true)?;
            kernel.exact_calls += 1;
            offer(
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
            );
            publish(&hits, &mut on_candidate);
        }
    }
    if (env.now)() >= deadline || !kernel.calibrate(deadline, env.now) {
        stats.exact = kernel.exact_calls;
        stats.elapsed_ms = (env.now)() - started;
        return Ok(search::Output {
            hits: ranked(&hits, k),
            proven: false,
            bound: None,
            stats,
        });
    }
    let seed_key = format!("{}:{minimum}", seed_key(&env, criterion, k, &kernel.seeds));
    let cached = if seeded {
        Some(Vec::new())
    } else {
        kernel.chart.seeded.get(&seed_key).cloned()
    };
    if let Some(cached) = cached {
        for hit in cached {
            offer(&mut hits, env.members, hit);
        }
    } else {
        let mut pool: Vec<(Team, f64)> = Vec::new();
        for level in kernel.grid.clone().into_iter().rev().filter(|level| *level >= minimum) {
            if late() {
                break;
            }
            let maximum = kernel.maximum_level;
            let luck = kernel.chart.luck;
            let constraints = level_constraints(&env.constraints, level, maximum);
            let mut adapter = Adapter::new(&mut kernel, level, true, true);
            if !warm(&mut adapter, &env, deadline) {
                break;
            }
            let end = if luck {
                deadline
            } else {
                ((env.now)() + 400.0).min(deadline)
            };
            let out = env.run(
                &mut adapter,
                Run {
                    constraints,
                    k: if luck { 120.max(k * 12) } else { 16.max(k * 3) },
                    deadline: end,
                    seeds: Vec::new(),
                    shard: None,
                    floor: f64::NEG_INFINITY,
                },
                None,
                None,
            )?;
            for hit in out.hits {
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
        pool.sort_by(|a, b| b.1.total_cmp(&a.1));
        pool.truncate(if kernel.chart.luck {
            120.max(k * 12)
        } else {
            60.max(k * 12)
        });
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
            let result = kernel.exact(&team, power.total as i32, true)?;
            kernel.exact_calls += 1;
            offer(
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
            );
            publish(&hits, &mut on_candidate);
        }
        if !late() {
            if kernel.chart.seeded.len() > 8 {
                kernel.chart.seeded.clear();
            }
            kernel.chart.seeded.insert(seed_key, hits.clone());
        }
    }
    if seed_only {
        stats.exact = kernel.exact_calls;
        stats.elapsed_ms = (env.now)() - started;
        return Ok(search::Output {
            hits: ranked(&hits, k),
            proven: false,
            bound,
            stats,
        });
    }
    for (index, level) in levels.iter().copied().enumerate() {
        if let Some(progress) = progress.as_deref_mut() {
            progress(index, levels.len());
        }
        if late() {
            proven = false;
            break;
        }
        let maximum = kernel.maximum_level;
        let luck = kernel.chart.luck;
        let owned: Option<Vec<String>> = if luck && shard.is_some_and(|(_, count)| count > 1) {
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
            let mut keys: Vec<_> = members.iter().map(|&i| keys[i].as_str()).collect();
            keys.sort();
            let key = keys.join("|");
            let mut hash = 0x811c9dc5u32;
            for code in key.encode_utf16() {
                hash = (hash ^ code as u32).wrapping_mul(0x01000193);
            }
            let (index, count) = shard.unwrap();
            hash as usize % count == index
        };
        let best = ranked(&hits, k);
        let level_floor = floor.max(if best.len() >= k {
            best[k - 1].key
        } else {
            f64::NEG_INFINITY
        });
        let mut adapter = Adapter::new(&mut kernel, level, false, false);
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
                shard: if owned.is_some() { None } else { shard },
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
                offer(&mut hits, env.members, hit);
            }
        }
    }
    stats.exact = kernel.exact_calls;
    stats.elapsed_ms = (env.now)() - started;
    Ok(search::Output {
        hits: ranked(&hits, k),
        proven,
        bound,
        stats,
    })
}
