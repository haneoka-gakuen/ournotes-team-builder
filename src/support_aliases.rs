//! Scoring-equivalent Gekiso supports, resolved against their owner's traits.
use crate::{
    full::conditions::{performer_matches, Performer},
    master::{Master, SkillEffect},
    power::Snap,
};
use rustc_hash::FxHashMap as HashMap;

pub(crate) fn stable_life_conditions(
    master: &Master,
    members: &[crate::power::Member],
    snaps: &[Snap],
    missions: &[i32],
) -> bool {
    let skills = members
        .iter()
        .map(|m| (false, m.card.gekiso_skill_id, m.growth.gekiso_skill_level))
        .chain(snaps.iter().flat_map(|s| {
            s.card
                .gekiso_support_skills
                .into_iter()
                .zip(s.gekiso_skill_levels)
                .map(|(id, level)| (true, id, level))
        }));
    for (support, id, level) in skills {
        if id == 0 {
            continue;
        }
        let Some(skill) = (if support {
            master.gekiso_support_skills.get(&id)
        } else {
            master.gekiso_skills.get(&id)
        }) else {
            return false;
        };
        if skill.mission_type != 4 && !missions.contains(&skill.mission_type) {
            continue;
        }
        for row in skill.effects.iter().filter(|r| r.level == level) {
            for id in [
                row.trigger_group,
                row.condition_group,
                row.release_group,
                row.reset_group,
            ]
            .iter()
            .flat_map(|g| master.condition_sets.get(g).into_iter().flatten().flatten())
            {
                let c = &master.conditions[id];
                if [2000, 2001].contains(&c.kind) {
                    let Some(&value) = c.values.first() else {
                        return false;
                    };
                    let test = |life| {
                        if c.kind == 2000 {
                            life <= value
                        } else {
                            life >= value
                        }
                    };
                    if test(master.live.life_base) != test(2.0 * master.live.life_base) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

pub(crate) struct JustCounters {
    members: std::collections::BTreeSet<(i32, i32)>,
}
impl JustCounters {
    pub fn new(master: &Master, members: &[crate::power::Member]) -> Self {
        let reads_counter = master
            .gekiso_skills
            .values()
            .chain(master.gekiso_support_skills.values())
            .flat_map(|s| &s.effects)
            .any(|row| {
                master
                    .cumulative
                    .get(&row.cumulative_id)
                    .is_some_and(|c| c.kind == 7000)
                    || [
                        row.trigger_group,
                        row.condition_group,
                        row.release_group,
                        row.reset_group,
                    ]
                    .iter()
                    .any(|group| {
                        master
                            .condition_sets
                            .get(group)
                            .into_iter()
                            .flatten()
                            .flatten()
                            .filter_map(|id| master.conditions.get(id))
                            .any(|c| [7002, 7003, 7006, 7011, 7012].contains(&c.kind))
                    })
            });
        let mut silent = std::collections::BTreeSet::new();
        if !reads_counter {
            for member in members {
                let id = member.card.gekiso_skill_id;
                let level = member.growth.gekiso_skill_level;
                let Some(skill) = master
                    .gekiso_skills
                    .get(&id)
                    .filter(|s| s.mission_type == 3)
                else {
                    continue;
                };
                if skill
                    .effects
                    .iter()
                    .filter(|r| r.level == level)
                    .all(|row| {
                        [13000, 13002, 13003, 13004, 4004].contains(&row.kind)
                            && [
                                row.trigger_group,
                                row.condition_group,
                                row.release_group,
                                row.reset_group,
                            ]
                            .iter()
                            .all(|group| {
                                master
                                    .condition_sets
                                    .get(group)
                                    .into_iter()
                                    .flatten()
                                    .flatten()
                                    .filter_map(|id| master.conditions.get(id))
                                    .all(|c| {
                                        [5000, 7010, 7013, 7020, 1030, 2000, 2001].contains(&c.kind)
                                    })
                            })
                    })
                {
                    silent.insert((id, level));
                }
            }
        }
        Self { members: silent }
    }
    pub fn normalize(&self, performer: &mut Performer) {
        if performer
            .gekisou_skill
            .is_some_and(|skill| self.members.contains(&skill))
        {
            performer.gekisou_skill = None;
            performer.mission_only = Some(1);
        }
    }
}

pub(crate) struct SupportAliases {
    missions: Vec<i32>,
    healthy: bool,
    candidates: Vec<(i32, i32)>,
    cache: HashMap<Performer, Vec<(i32, i32)>>,
    just_ceiling: Option<usize>,
}
impl SupportAliases {
    pub fn set_just_ceiling(&mut self, count: usize) {
        self.just_ceiling = Some(count);
        self.cache.clear();
    }
    pub fn new(missions: &[i32], snaps: &[Snap], healthy: bool) -> Self {
        let mut candidates: Vec<_> = snaps
            .iter()
            .flat_map(|s| {
                s.card
                    .gekiso_support_skills
                    .into_iter()
                    .zip(s.gekiso_skill_levels)
            })
            .filter(|&(id, _)| id != 0)
            .collect();
        candidates.sort_unstable();
        candidates.dedup();
        Self {
            missions: missions.to_vec(),
            healthy,
            candidates,
            cache: HashMap::default(),
            just_ceiling: None,
        }
    }
    fn runs(&self, mission: i32) -> bool {
        mission == 4 || self.missions.contains(&mission)
    }
    fn static_group(master: &Master, group: i32, p: &Performer) -> Option<bool> {
        if group == 0 {
            return Some(true);
        }
        let mut any = false;
        for set in master.condition_sets.get(&group)? {
            let mut all = true;
            for id in set {
                let c = master.conditions.get(id)?;
                if c.kind != 5000 {
                    return None;
                }
                all &= c.targets.iter().any(|id| {
                    master
                        .targets
                        .get(id)
                        .is_some_and(|t| performer_matches(p, t))
                }) == c.positive;
            }
            any |= all;
        }
        Some(any)
    }
    fn no_effect(&self, master: &Master, row: &SkillEffect) -> bool {
        row.kind == 4004
            || self.healthy
                && (row.kind == 12004 || row.kind == 12006 && row.value == 5)
                && row.targets.iter().all(|id| {
                    master
                        .targets
                        .get(id)
                        .is_some_and(|t| t.judgement != 5 && t.judgement != 6)
                })
    }
    fn signature(&self, master: &Master, pair: (i32, i32), p: &Performer) -> Option<Vec<u64>> {
        let skill = master.gekiso_support_skills.get(&pair.0)?;
        if !self.runs(skill.mission_type) {
            return Some(Vec::new());
        }
        let mut rows: Vec<_> = skill.effects.iter().filter(|r| r.level == pair.1).collect();
        rows.sort_by_key(|r| r.id);
        for row in &rows {
            if ![2000, 2001, 4004, 12004, 12006, 13005, 11003, 11005].contains(&row.kind) {
                return None;
            }
            for group in [
                row.trigger_group,
                row.condition_group,
                row.release_group,
                row.reset_group,
            ] {
                if master
                    .condition_sets
                    .get(&group)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|id| master.conditions.get(id))
                    .any(|c| {
                        ![5000, 7000, 7005, 7010, 7013, 7020, 7021, 1030, 2000, 2001]
                            .contains(&c.kind)
                    })
                {
                    return None;
                }
            }
            if row.cumulative_id != 0
                && master
                    .cumulative
                    .get(&row.cumulative_id)
                    .is_none_or(|c| ![1000, 7001].contains(&c.kind))
            {
                return None;
            }
        }
        let mut signature = vec![skill.mission_type as u64];
        for row in rows {
            if self.no_effect(master, row) || !Self::static_group(master, row.condition_group, p)? {
                continue;
            }
            let mut maximum = row.max_value;
            if let Some(count) = self.just_ceiling {
                if row.kind == 2001 && row.value > 0 && row.max_value > 0 {
                    if let Some(c) = master.cumulative.get(&row.cumulative_id).filter(|c| {
                        c.kind == 1000
                            && !c.targets.is_empty()
                            && c.targets.iter().all(|id| master.targets[id].judgement == 6)
                    }) {
                        if let Some(&n) = c.values.first().filter(|&&n| n >= 1.0) {
                            let units = crate::full::core::floor_i32(crate::num::f(
                                crate::num::f(count as f64) / crate::num::f(n as i32 as f64),
                            ))
                            .min(if c.cap > 0 {
                                c.cap
                            } else {
                                i32::MAX
                            });
                            if let Some(cap) = row.value.checked_mul(units) {
                                maximum = maximum.min(cap);
                            }
                        }
                    }
                }
            }
            signature.extend([
                row.kind as u64,
                row.value as u64,
                row.seconds.to_bits(),
                row.trigger_type as u64,
                row.execute_limit as u64,
                row.reset_group as u64,
                row.trigger_group as u64,
                row.release_group as u64,
                row.cumulative_id as u64,
                row.limit_count as u64,
                maximum as u64,
                row.targets.len() as u64,
            ]);
            signature.extend(row.targets.iter().map(|&id| id as u64));
        }
        Some(if signature.len() == 1 {
            Vec::new()
        } else {
            signature
        })
    }
    pub fn apply(&mut self, master: &Master, p: &mut Performer) {
        if let Some(skills) = self.cache.get(p) {
            p.gekisou_support_skills = skills.clone();
            return;
        }
        let original = p.clone();
        let mut skills: Vec<_> = p
            .gekisou_support_skills
            .iter()
            .copied()
            .filter(|&pair| {
                self.signature(master, pair, p)
                    .is_none_or(|s| !s.is_empty())
            })
            .collect();
        // Two supports with one canonical row ID would share an applier key.
        if skills.len() == 1 {
            if let Some(signature) = self.signature(master, skills[0], p) {
                if let Some(&canonical) = self
                    .candidates
                    .iter()
                    .find(|&&pair| self.signature(master, pair, p).as_ref() == Some(&signature))
                {
                    skills[0] = canonical;
                }
            }
        }
        if self.cache.len() >= 8192 {
            self.cache.clear();
        }
        self.cache.insert(original, skills.clone());
        p.gekisou_support_skills = skills;
    }
    pub fn retain_active(&self, master: &Master, p: &mut Performer) {
        p.gekisou_support_skills = p
            .gekisou_support_skills
            .iter()
            .copied()
            .filter(|&pair| {
                self.signature(master, pair, p)
                    .is_none_or(|s| !s.is_empty())
            })
            .collect();
    }
}

pub(crate) struct ContextFields {
    local: bool,
    global: Vec<&'static str>,
    cache: HashMap<Vec<(bool, i32)>, Vec<&'static str>>,
}
impl ContextFields {
    pub fn new(master: &Master) -> Self {
        let local = master
            .gekiso_skills
            .values()
            .chain(master.gekiso_support_skills.values())
            .flat_map(|skill| &skill.effects)
            .all(|row| {
                !(row.cumulative_id != 0
                    && master
                        .cumulative
                        .get(&row.cumulative_id)
                        .is_some_and(|c| (3000..=3005).contains(&c.kind)))
                    && [
                        row.trigger_group,
                        row.condition_group,
                        row.release_group,
                        row.reset_group,
                    ]
                    .iter()
                    .all(|group| {
                        master
                            .condition_sets
                            .get(group)
                            .into_iter()
                            .flatten()
                            .flatten()
                            .filter_map(|id| master.conditions.get(id))
                            .all(|c| {
                                [
                                    1030, 2000, 2001, 4011, 5000, 7000, 7001, 7002, 7003, 7004,
                                    7005, 7006, 7007, 7010, 7011, 7012, 7013, 7020, 7021,
                                ]
                                .contains(&c.kind)
                            })
                    })
            });
        Self {
            local,
            global: crate::gekiso_objective::read_attributes(master, true),
            cache: HashMap::default(),
        }
    }
    pub fn owner_local(&self) -> bool {
        self.local
    }
    pub fn normalize(&mut self, master: &Master, p: &mut Performer) {
        let mut skills: Vec<_> = p
            .gekisou_skill
            .map(|(id, _)| (false, id))
            .into_iter()
            .chain(p.gekisou_support_skills.iter().map(|&(id, _)| (true, id)))
            .collect();
        skills.sort_unstable();
        skills.dedup();
        let fields = if self.local {
            self.cache.entry(skills.clone()).or_insert_with(|| {
                let mut active = master.clone();
                active.gekiso_skills = std::sync::Arc::new(
                    skills
                        .iter()
                        .filter(|&&(support, _)| !support)
                        .filter_map(|&(_, id)| {
                            master.gekiso_skills.get(&id).map(|s| (id, s.clone()))
                        })
                        .collect(),
                );
                active.gekiso_support_skills = std::sync::Arc::new(
                    skills
                        .iter()
                        .filter(|&&(support, _)| support)
                        .filter_map(|&(_, id)| {
                            master
                                .gekiso_support_skills
                                .get(&id)
                                .map(|s| (id, s.clone()))
                        })
                        .collect(),
                );
                crate::gekiso_objective::read_attributes(&active, true)
            })
        } else {
            &self.global
        };
        if self.local && p.band_id != 0 {
            let referenced = skills
                .iter()
                .filter_map(|&(support, id)| {
                    if support {
                        master.gekiso_support_skills.get(&id)
                    } else {
                        master.gekiso_skills.get(&id)
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
                    master
                        .condition_sets
                        .get(&group)
                        .into_iter()
                        .flatten()
                        .flatten()
                })
                .filter_map(|id| master.conditions.get(id))
                .filter(|c| c.kind == 5000)
                .flat_map(|c| &c.targets)
                .any(|id| {
                    master
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
        if self.local && p.gekisou_skill.is_none() {
            p.mission_only = if p.gekisou_support_skills.is_empty() {
                None
            } else {
                p.mission_only.map(|_| 1)
            };
        }
    }
}
