//! Section-first ranking keeps a base-score floor and complete physical formations.
use super::*;
use crate::gekiso::sections::{Observation, OpponentSamples};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;

#[derive(Default)]
pub(crate) struct Reuse {
    actor_ids: HashMap<Performer, u32>,
    actors: HashMap<(usize, i32), Performer>,
    metrics: Vec<HashMap<[u32; 5], [f64; 3]>>,
    ceilings: HashMap<Vec<usize>, [f64; 2]>,
    scores: HashMap<(Vec<usize>, Vec<i32>), Exact>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Summary {
    pub metrics: [f64; 3],
    pub minimum: [f64; 3],
    pub maximum: [f64; 3],
    pub lower_decile: [f64; 3],
    pub range_scores: [f64; 3],
    pub base_mean: f64,
    pub bonus_mean: f64,
    pub total_mean: f64,
    pub wins: Option<[f64; 3]>,
    pub all_win: Option<f64>,
    pub samples: usize,
    pub sampled: bool,
    pub window: Option<crate::just_window::Window>,
}
impl Summary {
    pub fn of(rows: &[Observation], opponents: bool, random: bool) -> Self {
        let n = rows.len() as f64;
        let mut result = Self {
            samples: rows.len(),
            sampled: opponents || random,
            ..Self::default()
        };
        for range in 0..3 {
            let mut values: Vec<_> = rows
                .iter()
                .map(|row| row.sections[range].metric as f64)
                .collect();
            values.sort_unstable_by(f64::total_cmp);
            result.metrics[range] = values.iter().sum::<f64>() / n;
            result.minimum[range] = values[0];
            result.maximum[range] = values[values.len() - 1];
            result.lower_decile[range] = values[(values.len() - 1) / 10];
            result.range_scores[range] = rows
                .iter()
                .map(|row| row.sections[range].score as f64)
                .sum::<f64>()
                / n;
        }
        result.base_mean = rows.iter().map(|row| row.base_score).sum::<f64>() / n;
        result.bonus_mean = rows.iter().map(|row| row.bonus).sum::<f64>() / n;
        result.total_mean = rows.iter().map(|row| row.total_score).sum::<f64>() / n;
        result.window = rows
            .iter()
            .filter_map(|row| row.window)
            .reduce(|mut a, b| {a.merge(b);a});
        if opponents {
            result.wins = Some(std::array::from_fn(|range| {
                rows.iter().filter(|row| row.ranks[range] == 1).count() as f64 / n
            }));
            result.all_win = Some(
                rows.iter()
                    .filter(|row| row.ranks.iter().all(|&rank| rank == 1))
                    .count() as f64
                    / n,
            );
        }
        result
    }
}

pub(crate) struct Plan {
    pub reuse: RefCell<Option<Reuse>>,
    pub incumbents: RefCell<Vec<Hit>>,
    pub full_master: Master,
    pub references: Vec<Summary>,
    pub opponents: Vec<Option<OpponentSamples>>,
    pub seeds: Vec<i32>,
    pub max_base_loss: f64,
    pub base_bounds: bool,
    pub reference_hit: Option<Hit>,
    pub score_ceiling: Option<f64>,
    pub minimum_window: Option<crate::just_window::Policy>,
    pub window_traces: Vec<Option<crate::just_window::Trace>>,
}
impl Plan {
    fn floor(&self) -> f64 {
        self.references.iter().map(|row| row.base_mean).sum::<f64>() / self.references.len() as f64
            * (1.0 - self.max_base_loss)
    }
    pub fn rank(&self, summaries: &[Summary], details: &[Value]) -> Exact {
        if self.minimum_window.is_some_and(|minimum| {
            summaries
                .iter()
                .filter_map(|row| row.window)
                .any(|window| !minimum.allows(&window))
        }) {
            return Exact {
                key: f64::NEG_INFINITY,
                detail: Value::Null,
            };
        }
        let count = summaries.len() as f64;
        let base = summaries.iter().map(|row| row.base_mean).sum::<f64>() / count;
        if base + 1e-8 < self.floor() {
            return Exact {
                key: f64::NEG_INFINITY,
                detail: Value::Null,
            };
        }
        let total = summaries.iter().map(|row| row.total_mean).sum::<f64>() / count;
        let ranges: [f64; 3] = std::array::from_fn(|range| {
            summaries
                .iter()
                .zip(&self.references)
                .map(|(row, baseline)| {
                    row.wins.map_or(
                        (row.metrics[range] + 1.0) / (baseline.metrics[range] + 1.0),
                        |wins| wins[range],
                    )
                })
                .sum::<f64>()
                / count
        });
        let weakest = ranges.into_iter().fold(f64::INFINITY, f64::min);
        let sum = ranges.iter().sum::<f64>();
        let versus = self.opponents.iter().any(Option::is_some);
        let key = if versus { sum } else { weakest };
        let secondary = if versus { weakest } else { sum };
        let performance = json!({"versus":versus,"sampled":summaries.iter().any(|row|row.sampled),"ranges":ranges,"baseMean":base,"referenceBaseMean":self.references.iter().map(|r|r.base_mean).sum::<f64>()/count,
            "bonusMean":summaries.iter().map(|r|r.bonus_mean).sum::<f64>()/count,"totalMean":total,"maxBaseLoss":self.max_base_loss,
            "baseline":self.references,"songs":summaries,"allWin":if versus {Some(summaries.iter().map(|r|r.all_win.unwrap_or(0.0)).sum::<f64>()/count)}else{None}});
        let mut exact = Exact {
            key,
            detail: json!({"songs":details,"sectionPerformance":performance,"sectionTie":[secondary,total],"scores":{"mean":total}}),
        };
        if let Some(window) = summaries
            .iter()
            .filter_map(|row| row.window)
            .reduce(|mut a, b| {a.merge(b);a})
        {
            exact.detail["justWindow"] = json!(window);
        }
        exact
    }
}
pub(super) fn tie(a: &Value, b: &Value) -> std::cmp::Ordering {
    a["sectionTie"][0]
        .as_f64()
        .unwrap_or(0.0)
        .total_cmp(&b["sectionTie"][0].as_f64().unwrap_or(0.0))
        .then_with(|| {
            a["sectionTie"][1]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&b["sectionTie"][1].as_f64().unwrap_or(0.0))
        })
}

pub(super) struct Wrapper<'a, 'b> {
    pub base: &'b mut dyn Objective,
    pub env: &'b Environment<'a>,
    pub charts: &'b [Chart],
    pub music: &'b [Music],
    pub plan: &'b Plan,
    programs: HashMap<Performer, usize>,
    ids: HashMap<(usize, i32), usize>,
    fields: crate::support_aliases::ContextFields,
    actors: HashMap<(usize, i32), Performer>,
    metrics: Vec<HashMap<[u32; 5], [f64; 3]>>,
    actor_ids: HashMap<Performer, u32>,
    scores: HashMap<(Vec<usize>, Vec<i32>), Exact>,
    metric_master: Master,
    floor_rank: Option<[f64; 3]>,
    inert_snapshots: bool,
    domain_rank: Option<[f64; 3]>,
    just_ceiling: Option<Vec<Chart>>,
    just_ceil_cache: HashMap<Vec<usize>, [f64; 2]>,
}
impl<'a, 'b> Wrapper<'a, 'b> {
    pub fn new(
        base: &'b mut dyn Objective,
        env: &'b Environment<'a>,
        charts: &'b [Chart],
        music: &'b [Music],
        plan: &'b Plan,
        floor_rank: Option<[f64; 3]>,
    ) -> Self {
        let mut metric_master = env.master.clone();
        let mut reused = plan.reuse.borrow_mut().take().unwrap_or_default();
        if reused.metrics.len() != charts.len() {
            reused.metrics = (0..charts.len()).map(|_| HashMap::default()).collect();
        }
        let retain = |skills: &std::collections::BTreeMap<i32, crate::master::GekisoSkill>| {
            skills
                .iter()
                .map(|(&id, skill)| {
                    let mut skill = skill.clone();
                    skill
                        .effects
                        .retain(|row| !(2000..=2005).contains(&row.kind) && row.kind != 4004);
                    (id, skill)
                })
                .collect()
        };
        metric_master.gekiso_skills = Arc::new(retain(&env.master.gekiso_skills));
        metric_master.gekiso_support_skills = Arc::new(retain(&env.master.gekiso_support_skills));
        let inert_chart = charts.iter().all(|chart| {
            chart
                .setup
                .missions
                .iter()
                .all(|&mission| mission == 1 || mission == 3)
                && chart.judgements.iter().all(|&grade| grade >= 5)
                && chart
                    .notes
                    .iter()
                    .zip(&chart.judgements)
                    .all(|(note, &grade)| {
                        !env.master.live.just_types.contains(&note.judgement_type)
                            || !chart.setup.fevers.iter().zip(&chart.setup.missions).any(
                                |(&(start, end), &mission)| {
                                    mission == 3 && start <= note.time_ms && note.time_ms <= end
                                },
                            )
                            || grade == 6
                    })
        });
        let ap_snapshots = env.snaps.iter().all(|snap| {
            let normal = snap
                .card
                .support_skills
                .into_iter()
                .zip(snap.skill_levels)
                .filter(|&(id, _)| id != 0)
                .flat_map(|(id, level)| {
                    env.master
                        .support_skills
                        .get(&id)
                        .into_iter()
                        .flatten()
                        .filter(move |row| row.level == level)
                });
            let inert = |row: &crate::master::SkillEffect| {
                (2000..=2005).contains(&row.kind)
                    || [15000, 3004, 12004, 4004].contains(&row.kind)
                    || row.kind == 3001 && row.value >= 0 && env.master.live.life_base == 1000.0
                    || row.kind == 12006
                        && row.targets.iter().all(|id| {
                            env.master
                                .targets
                                .get(id)
                                .is_some_and(|target| target.judgement < 5)
                        })
                    || row.kind == 13005
            };
            normal.clone().all(inert)
                && snap
                    .card
                    .gekiso_support_skills
                    .into_iter()
                    .zip(snap.gekiso_skill_levels)
                    .filter(|&(id, _)| id != 0)
                    .filter_map(|(id, level)| {
                        env.master
                            .gekiso_support_skills
                            .get(&id)
                            .map(|skill| (skill, level))
                    })
                    .filter(|(skill, _)| {
                        skill.mission_type == 4
                            || charts
                                .iter()
                                .any(|chart| chart.setup.missions.contains(&skill.mission_type))
                    })
                    .all(|(skill, level)| {
                        skill
                            .effects
                            .iter()
                            .filter(|row| row.level == level)
                            .all(|row| {
                                inert(row)
                                    && !(row.kind == 13005
                                        && skill.mission_type == 4
                                        && charts
                                            .iter()
                                            .any(|chart| chart.setup.missions.contains(&1)))
                            })
                    })
        });
        let inert_snapshots = inert_chart && ap_snapshots;
        let just_ceiling = if ap_snapshots
            && charts.iter().all(|chart| {
                chart.setup.missions.iter().all(|&mission| mission == 3)
                    && chart.judgements.iter().all(|&grade| grade >= 5)
            }) {
            Some(
                charts
                    .iter()
                    .map(|chart| {
                        let grades = chart
                            .notes
                            .iter()
                            .zip(&chart.judgements)
                            .map(|(note, &grade)| {
                                if env.master.live.just_types.contains(&note.judgement_type) {
                                    6
                                } else {
                                    grade
                                }
                            })
                            .collect::<Vec<_>>();
                        chart.converted_judgements(&grades).unwrap()
                    })
                    .collect(),
            )
        } else {
            None
        };
        Self {
            base,
            env,
            charts,
            music,
            plan,
            floor_rank,
            programs: HashMap::default(),
            ids: HashMap::default(),
            fields: crate::support_aliases::ContextFields::new(&metric_master),
            metric_master,
            inert_snapshots,
            domain_rank: None,
            just_ceiling,
            just_ceil_cache: reused.ceilings,
            actors: reused.actors,
            metrics: reused.metrics,
            actor_ids: reused.actor_ids,
            scores: reused.scores,
        }
    }
    fn bound_key(&self, score: f64) -> f64 {
        if self.plan.base_bounds && score < self.plan.floor() {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        }
    }
    fn program(&mut self, member: usize, snap: i32) -> usize {
        if let Some(&id) = self.ids.get(&(member, snap)) {
            return id;
        }
        let actor = Performer::from_member(
            self.env.master,
            &self.env.members[member],
            (snap >= 0).then(|| &self.env.snaps[snap as usize]),
        );
        let next = self.programs.len();
        let id = *self.programs.entry(actor).or_insert(next);
        self.ids.insert((member, snap), id);
        id
    }
    fn metric_actor(&mut self, member: usize, snap: i32) -> Option<Performer> {
        if !self.actors.contains_key(&(member, snap)) {
            let full = Performer::from_member(
                self.env.master,
                &self.env.members[member],
                (snap >= 0).then(|| &self.env.snaps[snap as usize]),
            );
            if self.charts.iter().any(|chart| {
                gekiso::replay_random_skills(self.env.master, chart, std::slice::from_ref(&full))
            }) {
                return None;
            }
            let normal = full
                .live_skill
                .into_iter()
                .flat_map(|(id, level)| {
                    self.env
                        .master
                        .live_skills
                        .get(&id)
                        .into_iter()
                        .flatten()
                        .filter(move |row| row.level == level)
                })
                .chain(full.support_skills.iter().flat_map(|&(id, level)| {
                    self.env
                        .master
                        .support_skills
                        .get(&id)
                        .into_iter()
                        .flatten()
                        .filter(move |row| row.level == level)
                }));
            let perfect = self
                .charts
                .iter()
                .all(|chart| chart.judgements.iter().all(|&grade| grade >= 5));
            if normal.clone().any(|row| {
                !(2000..=2005).contains(&row.kind)
                    && row.kind != 15000
                    && !(row.kind == 3001
                        && row.value >= 0
                        && self.env.master.live.life_base == 1000.0)
                    && !(row.kind == 12006
                        && perfect
                        && row.targets.iter().all(|id| {
                            self.env
                                .master
                                .targets
                                .get(id)
                                .is_some_and(|target| target.judgement < 5)
                        }))
            }) {
                return None;
            }
            let mut actor = full.gekiso_only();
            actor.gekisou_support_skills.retain(|&(id, level)| {
                self.metric_master
                    .gekiso_support_skills
                    .get(&id)
                    .is_some_and(|skill| {
                        (skill.mission_type == 4
                            || self
                                .charts
                                .iter()
                                .any(|chart| chart.setup.missions.contains(&skill.mission_type)))
                            && skill.effects.iter().any(|row| row.level == level)
                    })
            });
            let rows: Vec<_> = actor
                .gekisou_skill
                .into_iter()
                .filter_map(|(id, level)| {
                    self.metric_master
                        .gekiso_skills
                        .get(&id)
                        .map(|skill| (skill, level))
                })
                .chain(
                    actor
                        .gekisou_support_skills
                        .iter()
                        .filter_map(|&(id, level)| {
                            self.metric_master
                                .gekiso_support_skills
                                .get(&id)
                                .map(|skill| (skill, level))
                        }),
                )
                .flat_map(|(skill, level)| {
                    skill.effects.iter().filter(move |row| row.level == level)
                })
                .collect();
            if rows.iter().any(|row| {
                !((11000..=11005).contains(&row.kind)
                    || (12000..=12006).contains(&row.kind)
                    || (13000..=13005).contains(&row.kind)
                    || [3004, 4004].contains(&row.kind))
            }) {
                return None;
            }
            if rows.iter().any(|row| row.kind == 12006 && row.value < 5) {
                return None;
            }
            if rows
                .into_iter()
                .flat_map(|row| {
                    [
                        row.trigger_group,
                        row.condition_group,
                        row.release_group,
                        row.reset_group,
                    ]
                })
                .flat_map(|group| {
                    self.metric_master
                        .condition_sets
                        .get(&group)
                        .into_iter()
                        .flatten()
                        .flatten()
                })
                .any(|id| {
                    self.metric_master
                        .conditions
                        .get(id)
                        .is_some_and(|condition| condition.kind == 4011)
                })
            {
                return None;
            }
            self.fields.normalize(&self.metric_master, &mut actor);
            self.actors.insert((member, snap), actor);
        }
        Some(self.actors[&(member, snap)].clone())
    }
    fn metric_rank(&mut self, team: &Team) -> Option<[f64; 2]> {
        if self.plan.opponents.iter().any(Option::is_some)
            || !self.fields.owner_local()
            || self.charts.iter().any(|chart| {
                crate::gekiso::uses_full_replay(self.env.master, chart, &self.plan.seeds)
            })
        {
            return None;
        }
        let deck = team
            .members
            .iter()
            .zip(&team.snaps)
            .map(|(&member, &snap)| self.metric_actor(member, snap))
            .collect::<Option<Vec<_>>>()?;
        let key = std::array::from_fn(|index| {
            let next = self.actor_ids.len() as u32;
            *self.actor_ids.entry(deck[index].clone()).or_insert(next)
        });
        let mut ranges = [0.0; 3];
        for (index, chart) in self.charts.iter().enumerate() {
            let cached = if let Some(value) = self.metrics[index].get(&key) {
                *value
            } else {
                let runs = if chart.luck {
                    self.plan.seeds.as_slice()
                } else {
                    &self.plan.seeds[..1]
                };
                let mut totals = [0_i64; 3];
                for &seed in runs {
                    let observed =
                        gekiso::sections::metrics(&self.metric_master, chart, &deck, seed).ok()?;
                    for (total, value) in totals.iter_mut().zip(observed) {
                        *total += value as i64;
                    }
                }
                let value = totals.map(|sum| sum as f64 / runs.len() as f64);
                let limit = (65536 / self.charts.len()).clamp(128, 32768);
                if self.metrics[index].len() >= limit {
                    let first = self.metrics[index].keys().next().cloned().unwrap();
                    self.metrics[index].remove(&first);
                }
                self.metrics[index].insert(key, value);
                value
            };
            for range in 0..3 {
                ranges[range] +=
                    (cached[range] + 1.0) / (self.plan.references[index].metrics[range] + 1.0);
            }
        }
        for value in &mut ranges {
            *value /= self.charts.len() as f64;
        }
        Some([
            ranges.into_iter().fold(f64::INFINITY, f64::min),
            ranges.iter().sum(),
        ])
    }
    fn just_roster_upper(&mut self, members: &[usize]) -> Option<[f64; 2]> {
        if self.plan.opponents.iter().any(Option::is_some) || !self.fields.owner_local() {
            return None;
        }
        if let Some(value) = self.just_ceil_cache.get(members) {
            return Some(*value);
        }
        let charts = self.just_ceiling.as_ref()?;
        let length = charts.iter().map(|chart| chart.music_length).max()? as f64;
        let group_all = |group: i32, allowed: &dyn Fn(&crate::master::Condition) -> bool| {
            group == 0
                || self
                    .metric_master
                    .condition_sets
                    .get(&group)
                    .is_some_and(|sets| {
                        sets.iter()
                            .flatten()
                            .all(|id| self.metric_master.conditions.get(id).is_some_and(allowed))
                    })
        };
        for &member in members {
            let actor = Performer::from_member(self.env.master, &self.env.members[member], None);
            if let Some((id, level)) = actor.gekisou_skill {
                let skill = self.metric_master.gekiso_skills.get(&id)?;
                if ![3, 4].contains(&skill.mission_type) {
                    continue;
                }
                for row in skill.effects.iter().filter(|row| row.level == level) {
                    if row.kind == 3004 {
                        continue;
                    }
                    if ![13000, 13002].contains(&row.kind)
                        || row.value < 0
                        || row.limit_count != 0
                        || !(row.seconds == 0.0 && row.trigger_type == 2
                            || row.seconds * 1000.0 >= length)
                        || !group_all(row.condition_group, &|condition| {
                            [2000, 2001].contains(&condition.kind)
                        })
                        || !group_all(row.trigger_group, &|condition| {
                            condition.positive && [7010, 7020].contains(&condition.kind)
                        })
                        || !group_all(row.release_group, &|condition| {
                            condition.positive && condition.kind == 7013
                        })
                        || !group_all(row.reset_group, &|condition| {
                            condition.positive && condition.kind == 7010
                        })
                    {
                        return None;
                    }
                    if row.kind == 13000 && row.cumulative_id != 0 {
                        return None;
                    }
                    if row.kind == 13002 {
                        let cumulative = self.metric_master.cumulative.get(&row.cumulative_id)?;
                        if cumulative.kind != 1000
                            || cumulative.cap < 0
                            || cumulative.values.first().is_none_or(|value| *value <= 0.0)
                            || cumulative.targets.iter().any(|id| {
                                self.metric_master
                                    .targets
                                    .get(id)
                                    .is_none_or(|target| target.judgement != 6)
                            })
                        {
                            return None;
                        }
                    }
                }
            }
        }
        let deck = members
            .iter()
            .map(|&member| self.metric_actor(member, -1))
            .collect::<Option<Vec<_>>>()?;
        let mut ranges = [0.0; 3];
        for (index, chart) in self.just_ceiling.as_ref()?.iter().enumerate() {
            let observed =
                crate::gekiso::sections::metrics(&self.metric_master, chart, &deck, 0).ok()?;
            for range in 0..3 {
                ranges[range] += (observed[range] as f64 + 1.0)
                    / (self.plan.references[index].metrics[range] + 1.0);
            }
        }
        for value in &mut ranges {
            *value /= self.charts.len() as f64;
        }
        let rank = [
            ranges.into_iter().fold(f64::INFINITY, f64::min),
            ranges.iter().sum(),
        ];
        if self.just_ceil_cache.len() >= 256 {
            self.just_ceil_cache.clear();
        }
        self.just_ceil_cache.insert(members.to_vec(), rank);
        Some(rank)
    }
}
impl Objective for Wrapper<'_, '_> {
    fn has_team_filter(&self) -> bool {
        true
    }
    fn filter_member_dominates(&self, _better: usize, _worse: usize) -> bool {
        false
    }
    fn filter_snap_dominates(&self, _member: usize, _better: i32, _worse: i32) -> bool {
        true
    }
    fn filter_permutation_invariant(&self) -> bool {
        false
    }
    fn feasible_team(&mut self, team: &Team, power: f64) -> Result<bool, String> {
        self.base.feasible_team(team, power)
    }
    fn feasible_assignment(
        &self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        options: &[Vec<i32>],
    ) -> bool {
        self.base
            .feasible_assignment(members, assigned, from, options)
    }
    fn score_ties(&self) -> bool {
        true
    }
    fn ranked_bounds(&self) -> bool {
        true
    }
    fn rank_floor(&self) -> Option<[f64; 3]> {
        self.floor_rank
    }
    fn leaf_rank_ties(&self) -> bool {
        true
    }
    fn seed_ready(&self, detail: &Value) -> bool {
        detail["sectionPerformance"].is_object()
    }
    fn tie_rank(&self, hit: &Hit) -> [f64; 2] {
        [
            hit.detail["sectionTie"][0].as_f64().unwrap_or(0.0),
            hit.detail["sectionTie"][1].as_f64().unwrap_or(0.0),
        ]
    }
    fn skill(&mut self, member: usize, snap: i32) -> (f64, f64) {
        self.base.skill(member, snap)
    }
    fn member_bonus(&self, member: usize) -> f64 {
        self.base.member_bonus(member)
    }
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn snap_dominance(&mut self, member: usize, snap: i32) -> Vec<f64> {
        let id = self.program(member, snap) as f64;
        let mut shape = self.base.snap_dominance(member, snap);
        shape.extend([id, -id]);
        shape
    }
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        self.program(member, a) == self.program(member, b)
            && self.base.exchange_snap_equivalent(member, a, b)
    }
    fn bound(&mut self, t: Totals) -> f64 {
        let upper = self.base.bound(t);
        self.bound_key(upper)
    }
    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        self.base.prepare_members(characters);
    }
    fn prepare_leader(&mut self, leader: usize) {
        self.base.prepare_leader(leader);
    }
    fn prepare_member_domains_at(&mut self, leader: usize, power: &[f64], options: &[Vec<i32>]) {
        self.base.prepare_member_domains_at(leader, power, options);
    }
    fn member_bound_at(
        &mut self,
        members: &[usize],
        t: Totals,
        left: usize,
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        let upper = self
            .base
            .member_bound_at(members, t, left, from, self.plan.floor())?;
        Some(self.bound_key(upper))
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.domain_rank = None;
        self.base.prepare_assignment(members, options);
    }
    fn partial_assignment_domains(&self) -> bool {
        false
    }
    fn assignment_domains(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
    ) -> Option<Vec<Vec<Vec<i32>>>> {
        if self.plan.opponents.iter().any(Option::is_some)
            || !self.fields.owner_local()
            || self.charts.iter().any(|chart| {
                crate::gekiso::uses_full_replay(self.env.master, chart, &self.plan.seeds)
            })
        {
            return None;
        }
        let mut domains = Vec::new();
        for (&member, choices) in members.iter().zip(options) {
            let mut groups: Vec<(Option<Performer>, Vec<i32>)> = Vec::new();
            for &snap in choices {
                let actor = self.metric_actor(member, snap);
                if let Some((_, group)) = groups
                    .iter_mut()
                    .find(|(key, _)| actor.is_some() && key == &actor)
                {
                    group.push(snap);
                } else {
                    groups.push((actor, vec![snap]));
                }
            }
            domains.push(groups.into_iter().map(|(_, group)| group).collect());
        }
        Some(domains)
    }
    fn prepare_assignment_domain(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
        complete: bool,
    ) {
        self.domain_rank = None;
        if complete {
            let team = Team {
                members: members.to_vec(),
                snaps: options.iter().map(|row| row[0]).collect(),
            };
            self.domain_rank = self.metric_rank(&team).map(|rank| {
                [
                    rank[0],
                    rank[1],
                    self.plan.score_ceiling.unwrap_or(f64::INFINITY),
                ]
            });
            if let (Some(upper), Some(floor)) = (self.domain_rank, self.floor_rank) {
                if upper[0]
                    .total_cmp(&floor[0])
                    .then_with(|| upper[1].total_cmp(&floor[1]))
                    .then_with(|| upper[2].total_cmp(&floor[2]))
                    .is_le()
                {
                    return;
                }
            }
        }
        self.base.prepare_assignment(members, options);
    }
    fn domain_rank_upper(&mut self) -> Option<[f64; 3]> {
        self.domain_rank
    }
    fn prepare_assignment_power(&mut self, power: f64) {
        self.base.prepare_assignment_power(power);
    }
    fn cheap_assignment_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
    ) -> f64 {
        let upper = self.base.cheap_assignment_bound(members, t, assigned, from);
        self.bound_key(upper)
    }
    fn assignment_order_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        let upper =
            self.base
                .assignment_order_bound(members, t, assigned, from, self.plan.floor())?;
        Some(self.bound_key(upper))
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
        let upper = self.base.interval(team, t).1;
        (f64::NEG_INFINITY, self.bound_key(upper))
    }
    fn leaf_rank_upper(&mut self, team: &Team, t: Totals) -> Option<[f64; 3]> {
        let metric = self.metric_rank(team)?;
        let score = self.plan.score_ceiling.unwrap_or(f64::INFINITY);
        let _ = t;
        Some([metric[0], metric[1], score])
    }
    fn roster_rank_upper(&mut self, members: &[usize]) -> Option<[f64; 3]> {
        let metric = if self.inert_snapshots {
            self.metric_rank(&Team {
                members: members.to_vec(),
                snaps: vec![-1; members.len()],
            })?
        } else {
            self.just_roster_upper(members)?
        };
        Some([
            metric[0],
            metric[1],
            self.plan.score_ceiling.unwrap_or(f64::INFINITY),
        ])
    }
    fn exact(&mut self, team: &Team, _power: f64, _slots: &[f64]) -> Result<Exact, String> {
        let key = (team.members.clone(), team.snaps.clone());
        if let Some(known) = self.scores.get(&key) {
            return Ok(known.clone());
        }
        if let Some(hit) = self
            .plan
            .reference_hit
            .as_ref()
            .filter(|hit| hit.team.members == team.members && hit.team.snaps == team.snaps)
        {
            return Ok(Exact {
                key: hit.key,
                detail: hit.detail.clone(),
            });
        }
        let members: Vec<_> = team
            .members
            .iter()
            .map(|&i| self.env.members[i].clone())
            .collect();
        let snaps: Vec<_> = team
            .snaps
            .iter()
            .map(|&i| (i >= 0).then(|| self.env.snaps[i as usize].clone()))
            .collect();
        let deck: Vec<_> = members
            .iter()
            .zip(&snaps)
            .map(|(member, snap)| Performer::from_member(self.env.master, member, snap.as_ref()))
            .collect();
        let mut details = Vec::new();
        let mut summaries = Vec::new();
        for (index, (chart, music)) in self.charts.iter().zip(self.music).enumerate() {
            let power = power::team_power(
                self.env.master,
                self.env.player,
                &members,
                &snaps,
                0,
                Some(music),
                &[],
            )?;
            let rows = crate::gekiso::sections::replay(
                &self.plan.full_master,
                chart,
                &deck,
                &self.plan.seeds,
                power.total as i32,
                self.plan.opponents[index].as_ref(),
                self.plan.window_traces[index].as_ref(),
            )?;
            let summary = Summary::of(
                &rows,
                self.plan.opponents[index].is_some(),
                gekiso::replay_random(&self.plan.full_master, chart, &deck),
            );
            let scores: Vec<_> = (0..120)
                .map(|order| {
                    rows.iter()
                        .skip(order)
                        .step_by(120)
                        .map(|row| row.total_score)
                        .sum::<f64>()
                        / (rows.len() / 120) as f64
                })
                .collect();
            details.push(json!({"power":power.total,"slotPowers":power.slots,"detail":{"scores":live::summarize(power.total as i32, scores),"sectionPerformance":summary,
                "justWindow":summary.window}}));
            summaries.push(summary);
        }
        let exact = self.plan.rank(&summaries, &details);
        if self.scores.len() >= (2048 / self.charts.len()).clamp(8, 128) {
            let first = self.scores.keys().next().cloned().unwrap();
            self.scores.remove(&first);
        }
        self.scores.insert(key, exact.clone());
        Ok(exact)
    }
}
impl Drop for Wrapper<'_, '_> {
    fn drop(&mut self) {
        self.plan.reuse.replace(Some(Reuse {
            actor_ids: std::mem::take(&mut self.actor_ids),
            actors: std::mem::take(&mut self.actors),
            metrics: std::mem::take(&mut self.metrics),
            ceilings: std::mem::take(&mut self.just_ceil_cache),
            scores: std::mem::take(&mut self.scores),
        }));
    }
}
