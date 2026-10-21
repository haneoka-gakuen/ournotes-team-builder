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
struct Base {
    weights: Arc<Weights>,
    deck: Vec<Performer>,
    gains: HashMap<Performer, f64>,
    lives: HashMap<usize, f64>,
    events: HashMap<usize, [f64; 5]>,
}
type SharedBase = Arc<std::sync::Mutex<Base>>;
type RefineBases = HashMap<(Vec<usize>, Vec<Performer>), SharedBase>;
type Ownership<'a> = &'a (dyn Fn(&[usize]) -> bool + 'a);
type CandidateListener<'a> = &'a mut (dyn FnMut(&[Hit]) + 'a);
type AttributeDependencies = HashMap<Vec<(bool, i32)>, (Vec<&'static str>, bool)>;
type OrderScoreKey = (Vec<usize>, Vec<i32>, i32);
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
    member_bound_keys: Vec<Option<Performer>>,
    just_counters: crate::support_aliases::JustCounters,
    support: Vec<Option<Performer>>,
    support_chain: Vec<bool>,
    performer_key_cache: HashMap<Performer, u32>,
    context_keys: HashMap<Performer, u32>,
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
    luck_supports: Vec<Performer>,
    cache: HashMap<(i32, Vec<u32>), Cached>,
    cache_order: VecDeque<(i32, Vec<u32>)>,
    cache_bytes: usize,
    cache_records: HashMap<usize, usize>,
    levels: HashMap<i32, Arc<Level>>,
    support_high: HashMap<(String, i32), f64>,
    support_low: HashMap<String, f64>,
    base: Option<Arc<Weights>>,
    member_bases: HashMap<(i32, Vec<Performer>, usize), f64>,
    luck_bases: HashMap<Vec<u32>, Arc<std::sync::Mutex<Base>>>,
    luck_aliases: HashMap<Vec<u32>, Vec<u32>>,
    base_key: Vec<u32>,
    refine_bases: RefineBases,
    rush: HashMap<Performer, Option<f64>>,
    exact_calls: usize,
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
    just_bounds: crate::just_bound::Compiler,
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
                                        (master.live.life_base <= value) == c.positive
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
        let member_bound_keys = gk_members
            .iter()
            .cloned()
            .map(|mut performer| {
                just_counters.normalize(&mut performer);
                if member_fields.owner_local() && performer.gekisou_skill.is_none() {
                    return None;
                }
                member_fields.normalize(master, &mut performer);
                Some(shape(&performer))
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
            member_bound_keys,
            just_counters,
            support,
            support_chain,
            performer_key_cache: HashMap::default(),
            context_keys: HashMap::default(),
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
            luck_aliases: HashMap::default(),
            base_key: Vec::new(),
            refine_bases: HashMap::default(),
            rush: HashMap::default(),
            exact_calls: 0,
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
            just_bounds: crate::just_bound::Compiler::default(),
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
        if let Some(weights) = self.chart.weights.get(&key) {
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
        let bytes = 128
            + (weights.general.len() + weights.judgements.iter().map(Vec::len).sum::<usize>()) * 8;
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
            .map(|p| (p.clone(), self.performer_key(p)))
            .collect();
        keyed.sort_by(|a, b| {
            self.chart.performer_json[a.1 as usize].cmp(&self.chart.performer_json[b.1 as usize])
        });
        let mut deck: Vec<_> = keyed.into_iter().map(|p| p.0).collect();
        if tag == 0 {
            for p in &mut deck {
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
            let (all_fields, local) = self.fields_for(deck.iter().flat_map(skill_ids).collect());
            for p in &mut deck {
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
            }
            if local {
                deck.retain(|p| p.gekisou_skill.is_some() || !p.gekisou_support_skills.is_empty());
            }
        }
        let key = (
            tag,
            deck.iter().map(|p| self.context_performer_key(p)).collect(),
        );
        (deck, key)
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
    fn complete_hits(&mut self, hits: &[Hit], k: usize) -> Result<Vec<Hit>, String> {
        let mut result = ranked(hits, k);
        for hit in &mut result {
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
    fn search_exact(&mut self, team: &Team, power: i32, settle: bool) -> Result<Exact, String> {
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
    fn deterministic_order_bound(&mut self, team: &Team, power: f64, limit: f64) -> Option<f64> {
        if self.chart.luck || !self.healthy {
            return None;
        }
        let key = std::array::from_fn(|slot| self.shape_id(team.members[slot], team.snaps[slot]));
        if self.just_context.is_some() && self.owner_local {
            let mut estimate_key = std::array::from_fn(|slot| {
                self.bound_shape_id(team.members[slot], team.snaps[slot])
            });
            let mut converters = [(u32::MAX, u32::MAX); 5];
            let mut count = 0;
            for (slot, &shape) in estimate_key.iter().enumerate() {
                let index = self.slot_index(team.members[slot], team.snaps[slot]);
                if !self.bound_converters[index] {
                    continue;
                }
                if self.raw_actor_keys[index] == u32::MAX {
                    let actor = self.actor_of(team.members[slot], team.snaps[slot]);
                    self.raw_actor_keys[index] = self.performer_key(&actor);
                }
                converters[count] = (self.raw_actor_keys[index], shape);
                count += 1;
            }
            converters[..count].sort_by(|a, b| {
                self.chart.performer_json[a.0 as usize]
                    .cmp(&self.chart.performer_json[b.0 as usize])
            });
            let mut conversion_order = [u32::MAX; 5];
            for (slot, &(_, id)) in converters[..count].iter().enumerate() {
                conversion_order[slot] = id;
            }
            // The additive score bound is unordered; judgement conversion is not.
            estimate_key.sort_unstable();
            let estimate_key = (estimate_key, conversion_order);
            let coefficient = if let Some(&value) = self.order_estimates.get(&estimate_key) {
                Some(value)
            } else {
                let raw = self.deck_of(team);
                let deck = self.deck_key(&raw, 0).0;
                let upper = self
                    .just_bounds
                    .upper(self.master, self.chart, &deck)
                    .ok()
                    .flatten();
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
                    let weights = note_weights(self.master, self.chart, &upper);
                    let scores = crate::rewards::order_score_coefficients(
                        self.master,
                        self.chart,
                        &weights,
                        &slots,
                    )?;
                    let value = match self.criterion {
                        Criterion::Mean => {
                            (scores.iter().fold(0.0_f64, |sum, &v| (sum + v).next_up()) / 120.0)
                                .next_up()
                        }
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
            if let Some(coefficient) = coefficient {
                let upper = (power * coefficient).next_up() + (self.chart.notes.len() + 4) as f64;
                if upper <= limit {
                    return Some(upper);
                }
            }
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
            detail: json!({"scores":scores,"seeds":ctx.seeds,"comboBonus":if self.chart.setup.missions.iter().any(|m| *m == 1 || *m == 4) { Some(1 + team.members.iter().map(|&i| self.opening_bonuses[i]).sum::<i32>()) } else { None }}),
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
        if let Some(base) = canonical.and_then(|key| self.luck_bases.get(key)) {
            let base = Arc::clone(base);
            self.base_key = key;
            return base;
        }
        if self.luck_bases.len() >= 2048 {
            self.luck_bases.clear();
        }
        let mut deck: Vec<_> = members
            .iter()
            .cloned()
            .chain(chained.iter().map(|p| p.0.clone()))
            .collect();
        if self.healthy && self.owner_local {
            let canonical = self.deck_key(&deck, 0).1 .1;
            if self.luck_aliases.len() >= 8192 {
                self.luck_aliases.clear();
            }
            self.luck_aliases.insert(key.clone(), canonical.clone());
            if let Some(base) = self.luck_bases.get(&canonical) {
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
        let base = Arc::new(std::sync::Mutex::new(Base {
            weights,
            deck,
            gains: HashMap::default(),
            lives: HashMap::default(),
            events: HashMap::default(),
        }));
        self.luck_bases.insert(key.clone(), Arc::clone(&base));
        self.base_key = key;
        base
    }
    fn luck_slot(
        &mut self,
        base: &Arc<std::sync::Mutex<Base>>,
        m: usize,
        s: i32,
        pick: Criterion,
    ) -> f64 {
        let mut data = base.lock().unwrap_or_else(|e| e.into_inner());
        self.luck_slot_locked(&mut data, m, s, pick)
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
    fn luck_events(&mut self, base: &mut Base, m: usize, s: i32) -> [f64; 5] {
        let index = self.slot_index(m, s);
        if let Some(&row) = base.events.get(&index) {
            return row;
        }
        let mut row = self.event_weights(self.slot(m, s), &base.weights);
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
                row = row.map(|value| (value + gain).next_up());
            }
        }
        base.events.insert(index, row);
        row
    }
    fn refine(&mut self, team: &Team, power: f64) -> f64 {
        let supports: Vec<_> = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &m)| self.support(m, team.snaps[slot]))
            .collect();
        let mut chained = Vec::new();
        for p in supports.iter().flatten() {
            if self.chain(p) {
                chained.push(p.clone());
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
            if let Some(p) = &supports[slot] {
                if !self.chain(p) {
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
        power * total / self.chart.full.converted as f64
    }
}
type ChainActor = (Performer, u32);
struct ChainChoice {
    snap: i32,
    owned: [Option<ChainActor>; 5],
    future: [Vec<ChainActor>; 6],
}
struct Assignment {
    member_deck: Vec<Performer>,
    member_ids: Vec<u32>,
    options: Vec<Vec<i32>>,
    chains: Vec<Arc<ChainChoice>>,
}
struct AssignmentPrefix<'a> {
    members: &'a [usize],
    assigned: &'a [i32],
    options: &'a [Vec<i32>],
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
    fn context_partition_bound(
        &mut self,
        base: &SharedBase,
        prefix: &AssignmentPrefix<'_>,
        require_future_chain: bool,
    ) -> f64 {
        let maximum = self.criterion == Criterion::Max;
        let width = self.snaps.len() + 1;
        let mut data = base.lock().unwrap_or_else(|e| e.into_inner());
        let constant = data.weights.base;
        let mut coefficient = constant;
        let mut rows = Vec::with_capacity(5);
        for slot in 0..5 {
            let member = prefix.members[slot];
            let choices = if slot < prefix.from {
                &prefix.assigned[slot..slot + 1]
            } else {
                &prefix.options[slot]
            };
            let mut row = Vec::new();
            let mut high = f64::NEG_INFINITY;
            for &snap in choices {
                if slot >= prefix.from
                    && snap >= 0
                    && prefix.assigned[..prefix.from].contains(&snap)
                {
                    continue;
                }
                let chain = slot >= prefix.from
                    && snap >= 0
                    && self.support_chain[member * self.snaps.len() + snap as usize];
                if !require_future_chain && chain {
                    continue;
                }
                let weights = if maximum {
                    self.luck_events(&mut data, member, snap)
                } else {
                    [self.luck_slot_locked(&mut data, member, snap, Criterion::Mean); 5]
                };
                let power = prefix.snapshot_power
                    [member * width + if snap < 0 { width - 1 } else { snap as usize }];
                high = high.max(weights.iter().copied().fold(0.0, f64::max));
                row.push((power, weights, chain));
            }
            if row.is_empty() {
                return f64::NEG_INFINITY;
            }
            coefficient += high;
            rows.push(row);
        }
        drop(data);
        let lambda = (coefficient / prefix.power_bound.max(1.0)).sqrt();
        if !lambda.is_finite() || lambda <= 0.0 {
            return f64::INFINITY;
        }
        let sum = |a: f64, b: f64| {
            if a == f64::NEG_INFINITY || b == f64::NEG_INFINITY {
                f64::NEG_INFINITY
            } else {
                (a + b).next_up()
            }
        };
        let variable = if !maximum {
            let mut dp = [0.0, f64::NEG_INFINITY];
            for row in rows {
                let mut offers = [f64::NEG_INFINITY; 2];
                for (power, weights, chain) in row {
                    let value =
                        ((lambda * power).next_up() + (weights[0] / lambda).next_up()).next_up();
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
            let mut dp = [[f64::NEG_INFINITY; 2]; 32];
            dp[0][0] = 0.0;
            for (slot, row) in rows.into_iter().enumerate() {
                let mut offers = [[f64::NEG_INFINITY; 5]; 2];
                for (power, weights, chain) in row {
                    for (event, weight) in weights.into_iter().enumerate() {
                        let value =
                            ((lambda * power).next_up() + (weight / lambda).next_up()).next_up();
                        offers[chain as usize][event] = offers[chain as usize][event].max(value);
                    }
                }
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
                        dp[next][1] = dp[next][1]
                            .max(sum(dp[mask][1], offers[0][event]))
                            .max(sum(dp[mask][0], offers[1][event]))
                            .max(sum(dp[mask][1], offers[1][event]));
                    }
                }
            }
            dp[31][require_future_chain as usize]
        };
        if variable == f64::NEG_INFINITY {
            return variable;
        }
        let total = (((lambda * prefix.base_power).next_up() + (constant / lambda).next_up())
            .next_up()
            + variable)
            .next_up();
        (((total * total).next_up() / 4.0).next_up() / self.chart.full.converted as f64).next_up()
            * (1.0 + REFINE_SLACK)
            + (self.chart.notes.len() + 4) as f64
    }
}
pub struct Adapter<'k, 'a> {
    kernel: &'k mut Kernel<'a>,
    level: i32,
    surrogate: bool,
    estimate: bool,
    data: Arc<Level>,
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
    member_dominance: HashMap<(usize, usize), bool>,
    deterministic: Option<crate::rewards::DeterministicBound>,
    dominance_margin: f64,
}
impl<'k, 'a> Adapter<'k, 'a> {
    fn new(kernel: &'k mut Kernel<'a>, level: i32, surrogate: bool, estimate: bool) -> Self {
        let data = kernel.level(level);
        let n = kernel.members.len();
        let deterministic = if !surrogate && !estimate && !kernel.chart.luck {
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
        Self {
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
            member_dominance: HashMap::default(),
            deterministic,
            dominance_margin: f64::INFINITY,
        }
    }
    fn shape_id(&mut self, member: usize, snap: i32) -> u32 {
        self.kernel.shape_id(member, snap)
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
        power * (self.data.weights.base + weight) / self.kernel.chart.full.converted as f64
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
        let rows: Vec<_> = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &m)| self.event_rows[&self.kernel.slot_index(m, team.snaps[slot])])
            .collect();
        self.max_score(power, best_order(&rows))
    }
    fn high(&self, t: Totals) -> f64 {
        if let Some(bound) = &self.deterministic {
            return bound.score(t);
        }
        if !self.kernel.chart.luck && !self.surrogate && !self.estimate {
            return f64::INFINITY;
        }
        t.power * (self.data.weights.base + t.skill) / self.kernel.chart.full.converted as f64
            * (1.0 + RELATIVE)
            + (self.kernel.chart.notes.len() + 4) as f64
    }
}
impl Objective for Adapter<'_, '_> {
    fn preserve_snap_options(&self) -> bool {
        !self.kernel.chart.luck && !self.surrogate && !self.estimate && self.deterministic.is_none()
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
        true
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
        let top = if self.kernel.criterion == Criterion::Max {
            let mut row = self
                .kernel
                .event_weights(self.kernel.slot(m, s), &self.data.weights);
            for value in &mut row {
                *value = (*value + gain).next_up();
            }
            let top = row.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            self.event_rows.insert(index, row);
            top
        } else {
            self.kernel
                .live_weight(self.kernel.slot(m, s), &self.data.weights, Criterion::Mean)
                + gain
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
    fn prepare_assignment_power(&mut self, power: f64) {
        self.base_power = power;
    }
    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        if let Some(bound) = &mut self.deterministic {
            bound.prepare_members(characters);
            return;
        }
        if self.preserve_snap_options() {
            return;
        }
        if self.kernel.criterion != Criterion::Max {
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
    fn member_bound_at(
        &mut self,
        members: &[usize],
        t: Totals,
        left: usize,
        from: usize,
        limit: f64,
    ) -> Option<f64> {
        if let Some(bound) = &mut self.deterministic {
            return Some(bound.member_score_ceiling(members, from, t.power, self.kernel.criterion));
        }
        if self.preserve_snap_options() {
            return Some(f64::INFINITY);
        }
        if self.kernel.criterion != Criterion::Max {
            return self.member_bound(members, t, left);
        }
        let rows: Vec<_> = (0..5)
            .map(|slot| {
                if slot < members.len() {
                    self.global_row(members[slot])
                } else {
                    self.suffix_rows[from]
                }
            })
            .collect();
        let upper = self.max_score(t.power, best_order(&rows));
        if upper < limit {
            return Some(upper);
        }
        Some(
            self.member_bound(members, t, left)
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
                    t,
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
            return Some(upper);
        }
        if self.preserve_snap_options() {
            return None;
        }
        if deterministic.is_some_and(|upper| upper < _limit) {
            return deterministic;
        }
        if self.kernel.criterion != Criterion::Max {
            return deterministic;
        }
        let base = self.data.weights.base;
        let lambda = ((base + t.skill) / t.power.max(1.0)).sqrt();
        if !lambda.is_finite() || lambda <= 0.0 {
            return None;
        }
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
        let sum =
            ((lambda * self.base_power).next_up() + (base / lambda).next_up() + best_order(&rows))
                .next_up();
        let upper = ((sum * sum).next_up() / (4.0 * self.kernel.chart.full.converted as f64))
            .next_up()
            * (1.0 + RELATIVE)
            + (self.kernel.chart.notes.len() + 4) as f64;
        Some(deterministic.map_or(upper, |bound| upper.min(bound)))
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
            bound.score_ceiling(members, assigned, from, totals.power, self.kernel.criterion)
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
    fn product_base(&self) -> Option<f64> {
        if let Some(bound) = &self.deterministic {
            return Some(bound.base);
        }
        if self.preserve_snap_options() {
            return None;
        }
        Some(self.data.weights.base)
    }
    fn leaf_bound(&mut self, team: &Team, t: Totals) -> Option<f64> {
        if let Some(bound) = &mut self.deterministic {
            return Some(bound.score_ceiling(
                &team.members,
                &team.snaps,
                5,
                t.power,
                self.kernel.criterion,
            ));
        }
        if self.preserve_snap_options() {
            return None;
        }
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
        if !self.kernel.chart.luck || self.surrogate {
            return None;
        }
        if self.kernel.read_all.iter().any(|&field| field != "bandId") {
            return Some(f64::INFINITY);
        }
        let level = self.level;
        let mut deck: Vec<_> = members
            .iter()
            .map(|&i| self.kernel.gk_members[i].clone())
            .collect();
        for p in &mut deck {
            self.kernel.just_counters.normalize(p);
        }
        if self.kernel.maximum_level > 0 {
            for p in &mut deck {
                if let Some((id, level)) = p.gekisou_skill {
                    let skill = &self.kernel.master.gekiso_skills[&id];
                    if skill.mission_type == 1 {
                        if skill
                            .effects
                            .iter()
                            .filter(|r| r.level == level)
                            .any(|r| r.kind != 12000 || r.value < 0)
                        {
                            return None;
                        }
                        p.gekisou_skill = None;
                        p.mission_only = Some(1);
                    }
                }
            }
        }
        let mut keys: Vec<_> = members
            .iter()
            .filter_map(|&m| self.kernel.member_bound_keys[m].clone())
            .collect();
        if self.kernel.maximum_level > 0 {
            for p in &mut keys {
                if p.gekisou_skill
                    .is_some_and(|(id, _)| self.kernel.master.gekiso_skills[&id].mission_type == 1)
                {
                    p.gekisou_skill = None;
                    p.mission_only = Some(1);
                }
            }
        }
        keys.sort();
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
            let mut bound_deck = deck;
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
        Some(
            t.power * (base + t.skill) / self.kernel.chart.full.converted as f64 * (1.0 + RELATIVE)
                + (self.kernel.chart.notes.len() + 4) as f64,
        )
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        if let Some(bound) = &mut self.deterministic {
            bound.prepare_score_assignment(members, options);
            return;
        }
        if self.preserve_snap_options() {
            return;
        }
        if self.kernel.criterion == Criterion::Max {
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
            let chains = chain_snaps
                .into_iter()
                .map(|snap| self.kernel.chain_choice(snap, members, options))
                .collect();
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
        if !self.kernel.chart.luck || self.surrogate {
            return None;
        }
        let assignment = self.assignment.as_ref()?;
        let options = &assignment.options;
        let member_deck = &assignment.member_deck;
        let taken = &assigned[..from];
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
                top = top.max(self.kernel.luck_slot(&base, members[slot], snap, pick));
            }
            value += top;
        }
        let original = t.power * value / self.kernel.chart.full.converted as f64
            * (1.0 + REFINE_SLACK)
            + (self.kernel.chart.notes.len() + 4) as f64;
        let no_future = self
            .kernel
            .luck_base(member_deck, &selected, &assignment.member_ids);
        let prefix = AssignmentPrefix {
            members,
            assigned,
            options,
            from,
            base_power: self.base_power,
            power_bound: t.power,
            snapshot_power: &self.snapshot_power,
        };
        let no_chain = self
            .kernel
            .context_partition_bound(&no_future, &prefix, false);
        let with_chain = self.kernel.context_partition_bound(&base, &prefix, true);
        Some(original.min(no_chain.max(with_chain)))
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
        if let Some(bound) = &mut self.deterministic {
            return (
                0.0,
                bound.score_ceiling(
                    &team.members,
                    &team.snaps,
                    5,
                    t.power,
                    self.kernel.criterion,
                ),
            );
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

pub fn search(input: Input<'_>) -> Result<search::Output, String> {
    let Input {
        mut env,
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
        let output = search::search(search::Input {
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
        })?;
        return Ok(output);
    }
    let k = k.max(1);
    let started = (env.now)();
    let generation = crate::cancellation_generation();
    let mut kernel = Kernel::new(env.master, env.members, env.snaps, chart, seeds, criterion);
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
    let levels: Vec<_> = (minimum..=kernel.maximum_level).rev().collect();
    let mut stats = search::Stats::default();
    if levels.is_empty() {
        return Ok(search::Output {
            hits: Vec::new(),
            proven: true,
            bound: None,
            stats,
        });
    }
    let mut hits = Vec::new();
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
    for team in initial {
        if !search::valid_seed(env.members, env.snaps, &env.constraints, &team) {
            continue;
        }
        if team.members.iter().map(|&i| kernel.bonuses[i]).sum::<i32>() < minimum {
            continue;
        }
        if late() {
            break;
        }
        let power = env.power(&team)?;
        let result = kernel.search_exact(&team, power.total as i32, true)?;
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
            let result = kernel.search_exact(&team, power.total as i32, true)?;
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
    let seed_key = format!("{}:{minimum}", seed_key(&env, criterion, k, &kernel.seeds));
    let cached = if seeded {
        Some(Vec::new())
    } else {
        kernel.chart.seeded.get(&seed_key).cloned()
    };
    if let Some(cached) = cached {
        for hit in cached {
            if search::valid_seed(env.members, env.snaps, &env.constraints, &hit.team) {
                offer(&mut hits, env.members, hit);
            }
        }
    } else {
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
                    k: if luck && criterion == Criterion::Max {
                        5
                    } else {
                        1
                    },
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
