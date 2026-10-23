//! Engine context and platform-neutral team evaluation. Chart loading is owned
//! by the caller; the same release-bound context can serve native and WASM.
use crate::{
    chart::{self, Chart, Source},
    live,
    master::{self, Master},
    power::{self, MemberInput, SnapInput},
    skills,
};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
type CachedScoreCap = (String, f64, Vec<crate::search::Team>);
pub struct Engine {
    pub master: Master,
    pub charts: BTreeMap<(i32, i32), Chart>,
    pub sources: BTreeMap<(i32, i32), Source>,
    chart_order: VecDeque<(i32, i32)>,
    live_cache: RefCell<Option<(String, crate::objectives::LiveCore)>>,
    gekiso_cache: RefCell<Option<(String, crate::gekiso::Chart)>>,
    reward_cache: RefCell<Option<(Value, crate::rewards::SettlementCache)>>,
    score_caps: RefCell<VecDeque<CachedScoreCap>>,
}
impl Engine {
    pub fn compile(data: &Value) -> Result<Self, String> {
        Ok(Self::from_master(master::compile(data)?))
    }
    pub fn from_master(master: Master) -> Self {
        Self {
            master,
            charts: BTreeMap::new(),
            sources: BTreeMap::new(),
            chart_order: VecDeque::new(),
            live_cache: RefCell::new(None),
            gekiso_cache: RefCell::new(None),
            reward_cache: RefCell::new(None),
            score_caps: RefCell::new(VecDeque::new()),
        }
    }
    pub fn insert_chart(
        &mut self,
        song_id: i32,
        difficulty: i32,
        source: Source,
    ) -> Result<(), String> {
        let song = self
            .master
            .songs
            .get(&song_id)
            .ok_or_else(|| format!("unknown-song:{song_id}"))?;
        let level = song
            .difficulties
            .iter()
            .find(|r| r.difficulty == difficulty)
            .ok_or_else(|| format!("unknown-chart:{song_id}:{difficulty}"))?;
        let key = (song_id, difficulty);
        if self.sources.get(&key) == Some(&source) {
            self.chart_order.retain(|&old| old != key);
            self.chart_order.push_back(key);
            return Ok(());
        }
        self.score_caps.borrow_mut().clear();
        self.live_cache.replace(None);
        self.gekiso_cache.replace(None);
        self.reward_cache.replace(None);
        self.chart_order.retain(|&old| old != key);
        if self.charts.len() >= 4 && !self.charts.contains_key(&key) {
            if let Some(key) = self.chart_order.pop_front() {
                self.charts.remove(&key);
                self.sources.remove(&key);
            }
        }
        self.chart_order.push_back(key);
        self.charts.insert(
            (song_id, difficulty),
            chart::compile(&self.master, song, level, &source),
        );
        self.sources.insert(key, source);
        Ok(())
    }
    fn certified_score_cap(
        &self,
        key: String,
        mut input: crate::search::Input<'_, crate::objectives::ScoreCap>,
        generation: u64,
    ) -> Result<(f64, Vec<crate::search::Team>, crate::search::Stats), String> {
        if let Some((_, cap, seeds)) = self.score_caps.borrow().iter().find(|(k, _, _)| k == &key) {
            return Ok((*cap, seeds.clone(), crate::search::Stats::default()));
        }
        input.constraints.min_bonus = None;
        input.constraints.max_bonus = None;
        input.constraints.member_floor = None;
        let result = crate::search::search_at(input, generation)?;
        let best = result.hits.first().map_or(f64::INFINITY, |h| h.key);
        let cap = if result.proven {
            best
        } else {
            best.max(result.bound.unwrap_or(f64::INFINITY))
        };
        let seeds: Vec<_> = result.hits.into_iter().map(|h| h.team).collect();
        if result.proven {
            let mut cache = self.score_caps.borrow_mut();
            if cache.len() >= 4 {
                cache.pop_front();
            }
            cache.push_back((key, cap, seeds.clone()));
        }
        Ok((cap, seeds, result.stats))
    }
    pub fn evaluate(
        &self,
        request: &Value,
        team: &Value,
        song_id: i32,
        difficulty: i32,
    ) -> Result<Value, String> {
        let inputs: Vec<MemberInput> =
            serde_json::from_value(request["members"].clone()).map_err(|e| e.to_string())?;
        let snaps: Vec<SnapInput> =
            serde_json::from_value(request["snaps"].clone()).map_err(|e| e.to_string())?;
        let cards = power::resolve_box(
            &self.master,
            &inputs,
            &snaps,
            request["unknownPolicy"] != "min",
        );
        let keys = team["members"].as_array().ok_or("missing team members")?;
        if keys.len() != 5 {
            return Err("a team requires five members".into());
        }
        let leader = team["leader"]
            .as_str()
            .unwrap_or_else(|| keys[0].as_str().unwrap_or(""));
        let leader_index = keys
            .iter()
            .position(|key| key.as_str() == Some(leader))
            .ok_or("unknown-team-member")?;
        let indices: Vec<_> = std::iter::once(leader_index)
            .chain((0..5).filter(|&i| i != leader_index))
            .collect();
        let members = indices
            .iter()
            .map(|&i| {
                let key = &keys[i];
                cards
                    .members
                    .iter()
                    .find(|m| key.as_str() == Some(m.key.as_str()))
                    .cloned()
                    .ok_or_else(|| format!("unknown member:{key}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let snap_keys = team["snaps"].as_array().ok_or("missing team snaps")?;
        if snap_keys.len() != 5 {
            return Err("a team requires five snap slots".into());
        }
        let snaps = indices
            .iter()
            .map(|&i| {
                let key = &snap_keys[i];
                if key.is_null() {
                    Ok(None)
                } else {
                    cards
                        .snaps
                        .iter()
                        .find(|s| key.as_str() == Some(s.key.as_str()))
                        .cloned()
                        .map(Some)
                        .ok_or_else(|| format!("unknown snap:{key}"))
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        crate::progress::keys(&cards.members, &cards.snaps);
        let player = power::player_state(&self.master, &request["player"]);
        let challenge_event = if request["gekiso"].is_object() {
            None
        } else if request["event"]["route"] == "challenge" {
            request["event"]["eventId"].as_i64()
        } else {
            request["challengeEventId"]
                .as_i64()
                .or_else(|| request["goal"]["challengeEventId"].as_i64())
        }
        .map(|id| id as i32);
        let music = power::music_view(&self.master, song_id, challenge_event)?;
        let effects = challenge_event
            .and_then(|id| self.master.events.get(&id))
            .map(|e| e.effects.as_slice())
            .unwrap_or_default();
        let power = power::team_power(
            &self.master,
            &player,
            &members,
            &snaps,
            0,
            Some(&music),
            effects,
        )?;
        let mut unsupported = Vec::new();
        let slots = members
            .iter()
            .zip(&snaps)
            .map(|(member, snap)| {
                skills::resolve(&self.master, member, snap.as_ref(), &mut unsupported)
            })
            .collect();
        let chart = self
            .charts
            .get(&(song_id, difficulty))
            .ok_or_else(|| format!("missing-chart:{song_id}:{difficulty}"))?;
        let play = if request["play"].is_null() {
            &request["goal"]["play"]
        } else {
            &request["play"]
        };
        let judgements = live::judgements(chart.times.len(), play);
        let is_gekiso = request["gekiso"].is_object();
        let event_route = if request["event"].is_null() {
            None
        } else {
            let input = &request["event"];
            let kind = match input["route"].as_str() {
                Some("challenge") => crate::event::RouteKind::Challenge,
                Some("skip") => crate::event::RouteKind::Skip,
                _ => crate::event::RouteKind::Live,
            };
            Some(crate::event::Route::new(
                &self.master,
                master::int(&input["eventId"]),
                kind,
                master::int(&input["consumption"]),
            )?)
        };
        let mut sampled_settlement = None;
        let mut range_scores = None;
        let scores = if is_gekiso {
            let source = self
                .sources
                .get(&(song_id, difficulty))
                .ok_or("missing-chart")?;
            let samples = master::int(&request["gekiso"]["seeds"]);
            let result = if let Some(route) = event_route.as_ref().filter(|_| samples > 0) {
                use crate::{full::play::Accuracy, search::Objective};
                let accuracy: Accuracy =
                    serde_json::from_value(request["gekiso"]["accuracy"].clone())
                        .map_err(|error| error.to_string())?;
                let mut gekiso = crate::gekiso::Chart::new(
                    &self.master,
                    source,
                    chart.difficulty.play_level,
                    chart.song.missions.clone(),
                    accuracy,
                    gekiso_ranks(&request["gekiso"]),
                )?;
                gekiso.music_type = music.music_type;
                let selected_snaps: Vec<_> = snaps.iter().flatten().cloned().collect();
                let mut index = 0;
                let selected = crate::search::Team {
                    members: (0..5).collect(),
                    snaps: snaps
                        .iter()
                        .map(|snap| {
                            if snap.is_some() {
                                let value = index;
                                index += 1;
                                value
                            } else {
                                -1
                            }
                        })
                        .collect(),
                };
                let mut objective = crate::rewards::RewardObjective::new(
                    &self.master,
                    &members,
                    &selected_snaps,
                    chart,
                    gekiso,
                    route.clone(),
                    true,
                    samples,
                    None,
                );
                let exact = objective.exact(&selected, power.total as f64, &[])?;
                sampled_settlement = Some(exact.detail["settlement"].clone());
                exact.detail["scores"].clone()
            } else {
                let mut gekiso_request = request.clone();
                gekiso_request["goal"] = request["gekiso"].clone();
                let ordered_team = json!({
                    "members": members.iter().map(|member| &member.key).collect::<Vec<_>>(),
                    "snaps": snaps.iter().map(|snap| snap.as_ref().map(|snap| &snap.key)).collect::<Vec<_>>()
                });
                self.evaluate_gekiso(
                    &gekiso_request,
                    &ordered_team,
                    &json!({"songId": song_id, "difficulty": difficulty}),
                    source,
                )?
            };
            range_scores = result.get("rangeScores").cloned();
            serde_json::from_value::<live::Scores>(result).map_err(|e| e.to_string())?
        } else {
            live::score(&chart::prepare(
                &self.master,
                chart,
                power.total as i32,
                slots,
                judgements,
                1.0,
                100.0,
            ))?
        };
        let mut ranks: BTreeMap<i32, i32> = BTreeMap::new();
        for &value in &scores.scores {
            let rank = if is_gekiso {
                chart::battle_rank(chart, value)
            } else {
                chart::score_rank(chart, value)
            };
            *ranks.entry(rank).or_default() += 1;
        }
        let keys_of = |order: usize| live::orders()[order].map(|slot| members[slot].key.clone());
        let mut sorted = scores.scores.clone();
        sorted.sort_by(f64::total_cmp);
        let event = if let Some(route) = &event_route {
            let bonus = |kind| {
                members
                    .iter()
                    .map(|m| power::member_event_percent(&route.effects, m, kind) as f64)
                    .sum::<f64>()
                    + snaps
                        .iter()
                        .flatten()
                        .map(|s| power::snap_event_percent(&route.effects, s, kind) as f64)
                        .sum::<f64>()
            };
            let point_bonus = bonus(0);
            let item_bonus = bonus(1);
            if let Some(settlement) = &sampled_settlement {
                json!({"points":settlement["mean"],"items":settlement["otherMean"],"bonusPercent":point_bonus/100.0,"itemBonusPercent":item_bonus/100.0})
            } else {
                let mut points = 0.0;
                let mut items = 0.0;
                for &score in &scores.scores {
                    let rank = if is_gekiso {
                        chart::battle_rank(chart, score)
                    } else {
                        chart::score_rank(chart, score)
                    };
                    points += route.points(point_bonus, rank);
                    items += route.items(item_bonus, rank);
                }
                json!({"points":points/120.0,"items":items/120.0,"bonusPercent":point_bonus/100.0,"itemBonusPercent":item_bonus/100.0})
            }
        } else {
            Value::Null
        };
        let mut result = json!({"song":{"songId":song_id,"difficulty":difficulty},"power":power.total,"slotPowers":power.slots,"leaderPercents":power.leader_percents,"unknownCards":cards.unknown,"unsupported":unsupported,"skipScore":chart::skip_score(&self.master,chart,power.total as i32),"event":event,"score":{"mean":scores.mean,"min":scores.min,"max":scores.max,"median":(sorted[59]+sorted[60])/2.0,"worstOrder":keys_of(scores.min_order),"bestOrder":keys_of(scores.max_order),"orders":scores.scores.iter().map(|v|crate::num::round(*v)).collect::<Vec<_>>(),"ranks":sampled_settlement.as_ref().map_or_else(||json!(ranks),|value|value["ranks"].clone())}});
        if let Some(ranges) = range_scores {
            result["score"]["rangeScores"] = ranges;
        }
        Ok(result)
    }
}

struct PowerObjective;
impl crate::search::Objective for PowerObjective {
    fn exchange_snap_equivalent(&mut self, _member: usize, _a: i32, _b: i32) -> bool {
        true
    }
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn canonical_leader(&self) -> bool {
        true
    }
    fn linear_assignment(&self) -> Option<(f64, f64, f64)> {
        Some((1.0, 0.0, 0.0))
    }
    fn skill(&mut self, _member: usize, _snap: i32) -> (f64, f64) {
        (0.0, 0.0)
    }
    fn bound(&mut self, totals: crate::search::Totals) -> f64 {
        totals.power
    }
    fn interval(
        &mut self,
        _team: &crate::search::Team,
        totals: crate::search::Totals,
    ) -> (f64, f64) {
        (totals.power, totals.power)
    }
    fn exact(
        &mut self,
        _team: &crate::search::Team,
        power: f64,
        _slots: &[f64],
    ) -> Result<crate::search::Exact, String> {
        Ok(crate::search::Exact {
            key: power,
            detail: Value::Null,
        })
    }
}
fn constraints(
    input: &Value,
    members: &[power::Member],
    snaps: &[power::Snap],
) -> crate::search::Constraints {
    let member_index: BTreeMap<_, _> = members
        .iter()
        .enumerate()
        .map(|(i, m)| (m.key.as_str(), i))
        .collect();
    let snap_index: BTreeMap<_, _> = snaps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.key.as_str(), i))
        .collect();
    let member_ids = |v: &Value| {
        master::rows(v)
            .iter()
            .filter_map(|v| v.as_str().and_then(|key| member_index.get(key).copied()))
            .collect()
    };
    let snap_ids = |v: &Value| {
        master::rows(v)
            .iter()
            .filter_map(|v| v.as_str().and_then(|key| snap_index.get(key).copied()))
            .collect()
    };
    let mut unresolved = master::rows(&input["requiredMembers"]).iter().any(|key| {
        !key.as_str()
            .is_some_and(|key| member_index.contains_key(key))
    }) || master::rows(&input["requiredSnaps"])
        .iter()
        .any(|key| !key.as_str().is_some_and(|key| snap_index.contains_key(key)));
    let mut bindings = std::collections::HashMap::new();
    for binding in master::rows(&input["bindings"]) {
        let Some(member) = binding[0]
            .as_str()
            .and_then(|key| member_index.get(key).copied())
        else {
            unresolved = true;
            continue;
        };
        if binding[1].is_null() {
            bindings.insert(member, -1);
        } else if let Some(snap) = binding[1]
            .as_str()
            .and_then(|key| snap_index.get(key).copied())
        {
            bindings.insert(member, snap as i32);
        } else {
            unresolved = true;
        }
    }
    crate::search::Constraints {
        required_members: member_ids(&input["requiredMembers"]),
        excluded_members: member_ids(&input["excludedMembers"]),
        required_snaps: snap_ids(&input["requiredSnaps"]),
        excluded_snaps: snap_ids(&input["excludedSnaps"]),
        leader: if unresolved {
            Some(-1)
        } else if input["leader"].is_null() {
            None
        } else {
            Some(
                input["leader"]
                    .as_str()
                    .and_then(|key| member_index.get(key).copied())
                    .map(|i| i as i32)
                    .unwrap_or(-1),
            )
        },
        bindings,
        no_snaps: input["noSnaps"] == true,
        min_bonus: input["minBonusPercent"]
            .as_f64()
            .map(|v| crate::num::round(v * 100.0)),
        max_bonus: None,
        member_floor: None,
    }
}
impl Engine {
    pub fn search_power(
        &self,
        request: &Value,
        started: f64,
        now: &dyn Fn() -> f64,
    ) -> Result<Value, String> {
        let member_inputs: Vec<MemberInput> =
            serde_json::from_value(request["members"].clone()).map_err(|e| e.to_string())?;
        let snap_inputs: Vec<SnapInput> =
            serde_json::from_value(request["snaps"].clone()).map_err(|e| e.to_string())?;
        let cards = power::resolve_box(
            &self.master,
            &member_inputs,
            &snap_inputs,
            request["unknownPolicy"] != "min",
        );
        crate::progress::keys(&cards.members, &cards.snaps);
        let player = power::player_state(&self.master, &request["player"]);
        let song = &request["goal"]["song"];
        let challenge = request["goal"]["challengeEventId"]
            .as_i64()
            .map(|id| id as i32);
        let music = if song.is_null() {
            None
        } else {
            Some(power::music_view(
                &self.master,
                master::int(&song["songId"]),
                challenge,
            )?)
        };
        let effects = challenge
            .and_then(|id| self.master.events.get(&id))
            .map(|e| e.effects.as_slice())
            .unwrap_or_default();
        let member_pct = |member: &power::Member| power::member_event_percent(effects, member, 2);
        let snap_pct = |snap: &power::Snap| power::snap_event_percent(effects, snap, 2);
        let constraints = constraints(&request["constraints"], &cards.members, &cards.snaps);
        let k = master::number(&request["k"], 1.0).max(1.0) as usize;
        let deadline = request["deadline"].as_f64().unwrap_or_else(|| {
            request["timeLimitMs"]
                .as_f64()
                .map(|ms| started + ms)
                .unwrap_or(f64::INFINITY)
        });
        let shard = if request["shard"].is_null() {
            None
        } else {
            Some((
                master::int(&request["shard"]["index"]) as usize,
                master::int(&request["shard"]["count"]) as usize,
            ))
        };
        let mut objective: Box<dyn crate::search::Objective> =
            if request["goal"]["kind"] == "potential" {
                Box::new(crate::objectives::Potential::new(
                    &self.master,
                    &cards.members,
                    &cards.snaps,
                    master::number(&request["goal"]["windowSeconds"], 120.0),
                ))
            } else {
                Box::new(PowerObjective)
            };
        let generation = crate::cancellation_generation();
        let output = crate::search::search(crate::search::Input {
            master: &self.master,
            player: &player,
            members: &cards.members,
            snaps: &cards.snaps,
            music: music.as_ref(),
            objective: objective.as_mut(),
            constraints,
            k,
            deadline,
            now,
            seeds: seed_teams(request, song, &cards.members, &cards.snaps),
            shard,
            owns: None,
            floor: f64::NEG_INFINITY,
            floor_exclusive: false,

            streaming: true,
            disable_snap_dominance: false,
            member_power_percent: Some(&member_pct),
            snap_power_percent: Some(&snap_pct),
            on_candidate: Some(&mut crate::progress::emit),
            progress: None,
        })?;
        let hits:Vec<_>=output.hits.iter().map(|hit|json!({"song":song,"members":hit.team.members.iter().map(|&i|cards.members[i].key.clone()).collect::<Vec<_>>(),"snaps":hit.team.snaps.iter().map(|&j|if j<0{None}else{Some(cards.snaps[j as usize].key.clone())}).collect::<Vec<_>>(),"power":hit.power,"slotPowers":hit.slot_powers,"key":hit.key,"potential":if hit.detail["potential"].is_null(){Value::Null}else{json!({"value":hit.detail["potential"],"area":hit.detail["area"]})}})).collect();
        Ok(
            json!({"results":[{"song":song,"hits":hits,"proven":output.proven,"bound":output.bound,"stats":output.stats}],"overall":hits,"unknownCards":cards.unknown,"elapsedMs":now()-started,"stopReason":if output.proven{None}else if generation!=crate::cancellation_generation(){Some("cancelled")}else{Some("budget")}}),
        )
    }
}

impl Engine {
    pub fn search_score(
        &self,
        request: &Value,
        started: f64,
        now: &dyn Fn() -> f64,
    ) -> Result<Value, String> {
        use crate::{
            objectives::{Criterion, LiveCore, LiveObjective},
            search,
        };
        let members: Vec<MemberInput> =
            serde_json::from_value(request["members"].clone()).map_err(|e| e.to_string())?;
        let snaps: Vec<SnapInput> =
            serde_json::from_value(request["snaps"].clone()).map_err(|e| e.to_string())?;
        let cards = power::resolve_box(
            &self.master,
            &members,
            &snaps,
            request["unknownPolicy"] != "min",
        );
        crate::progress::keys(&cards.members, &cards.snaps);
        let player = power::player_state(&self.master, &request["player"]);
        let constraints = constraints(&request["constraints"], &cards.members, &cards.snaps);
        let k = master::number(&request["k"], 1.0).max(1.0) as usize;
        let deadline = request["deadline"].as_f64().unwrap_or_else(|| {
            request["timeLimitMs"]
                .as_f64()
                .map(|ms| started + ms)
                .unwrap_or(f64::INFINITY)
        });
        let generation = crate::cancellation_generation();
        let goal = &request["goal"];
        let songs = master::rows(&goal["songs"]);
        let mut results = Vec::new();
        let mut overall = Vec::new();
        let shard = if request["shard"].is_null() {
            None
        } else {
            Some((
                master::int(&request["shard"]["index"]) as usize,
                master::int(&request["shard"]["count"]) as usize,
            ))
        };
        for (index, song) in songs.iter().enumerate() {
            crate::progress::song(song);
            let done = master::rows(&request["completed"])
                .iter()
                .find(|row| row["proven"] == true && row["song"] == **song)
                .copied();
            if let Some(done) = done {
                results.push(done.clone());
                overall.extend(master::rows(&done["hits"]).iter().map(|v| (*v).clone()));
                continue;
            }
            if now() >= deadline || generation != crate::cancellation_generation() {
                for pending in &songs[index..] {
                    results.push(json!({"song":pending,"hits":[],"proven":false,"bound":null,"stats":search::Stats::default()}));
                }
                break;
            }
            let song_id = master::int(&song["songId"]);
            let difficulty = master::int(&song["difficulty"]);
            let chart = self
                .charts
                .get(&(song_id, difficulty))
                .ok_or_else(|| format!("missing-chart:{song_id}:{difficulty}"))?;
            let challenge = goal["challengeEventId"].as_i64().map(|id| id as i32);
            let music = power::music_view(&self.master, song_id, challenge)?;
            let effects = challenge
                .and_then(|id| self.master.events.get(&id))
                .map(|e| e.effects.as_slice())
                .unwrap_or_default();
            let member_pct = |m: &power::Member| power::member_event_percent(effects, m, 2);
            let snap_pct = |s: &power::Snap| power::snap_event_percent(effects, s, 2);
            let cache_key = json!([
                song,
                request["members"],
                request["snaps"],
                request["unknownPolicy"],
                goal["play"]
            ])
            .to_string();
            let core = self
                .live_cache
                .borrow_mut()
                .take()
                .filter(|(key, _)| key == &cache_key)
                .map(|(_, core)| core)
                .unwrap_or_else(|| {
                    LiveCore::new(
                        &self.master,
                        chart,
                        &cards.members,
                        &cards.snaps,
                        &goal["play"],
                    )
                });
            let mut objective = LiveObjective {
                core,
                criterion: Criterion::from_value(&goal["criterion"]),
                target: crate::objectives::Target::from_value(&goal["target"]),
            };
            let song_deadline =
                (now() + (deadline - now()).max(0.0) / (songs.len() - index) as f64).min(deadline);
            let mut seeds: Vec<_> = master::rows(&request["seeds"])
                .iter()
                .filter(|hit| hit["song"] == **song)
                .filter_map(|hit| {
                    let members = master::rows(&hit["members"])
                        .iter()
                        .map(|v| {
                            cards
                                .members
                                .iter()
                                .position(|m| v.as_str() == Some(&m.key))
                        })
                        .collect::<Option<Vec<_>>>()?;
                    let snaps = master::rows(&hit["snaps"])
                        .iter()
                        .map(|v| {
                            if v.is_null() {
                                Some(-1)
                            } else {
                                cards
                                    .snaps
                                    .iter()
                                    .position(|s| v.as_str() == Some(&s.key))
                                    .map(|i| i as i32)
                            }
                        })
                        .collect::<Option<Vec<_>>>()?;
                    Some(search::Team { members, snaps })
                })
                .collect();
            let mut cap_stats = search::Stats::default();
            objective.core.score_cap = f64::INFINITY;
            let mut saturated = None;
            if let Some(target) = objective.target {
                let criterion = objective.criterion;
                objective.target = None;
                objective.criterion = Criterion::Mean;
                let mut mean = search::search_at(
                    search::Input {
                        master: &self.master,
                        player: &player,
                        members: &cards.members,
                        snaps: &cards.snaps,
                        music: Some(&music),
                        objective: &mut objective,
                        constraints: constraints.clone(),
                        k,
                        deadline: song_deadline,
                        now,
                        seeds: seeds.clone(),
                        shard: None,
                        owns: None,
                        floor: f64::NEG_INFINITY,
                        floor_exclusive: false,
                        streaming: true,
                        disable_snap_dominance: false,
                        member_power_percent: Some(&member_pct),
                        snap_power_percent: Some(&snap_pct),
                        on_candidate: None,
                        progress: None,
                    },
                    generation,
                )?;
                objective.target = Some(target);
                objective.criterion = criterion;
                let complete = mean.proven
                    && mean.hits.len() >= k
                    && mean.hits.iter().all(|hit| {
                        master::number(&hit.detail["scores"]["min"], 0.0) >= target.score
                    });
                let interrupted =
                    generation != crate::cancellation_generation() || now() >= song_deadline;
                if complete || interrupted {
                    if interrupted {
                        mean.proven = false;
                        mean.bound = None;
                    }
                    for hit in &mut mean.hits {
                        let exact = crate::search::Objective::exact(
                            &mut objective,
                            &hit.team,
                            hit.power,
                            &hit.slot_powers,
                        )?;
                        hit.key = exact.key;
                        hit.detail = exact.detail;
                    }
                    saturated = Some(mean);
                } else {
                    seeds.extend(mean.hits.into_iter().map(|hit| hit.team));
                    cap_stats.add(&mean.stats);
                }
            }
            let mut output = if let Some(output) = saturated {
                output
            } else {
                if objective.target.is_some() {
                    let cap_key = json!([
                        song,
                        request["members"],
                        request["snaps"],
                        request["player"],
                        request["unknownPolicy"],
                        request["constraints"],
                        goal["play"],
                        challenge
                    ])
                    .to_string();
                    let mut cap = crate::objectives::ScoreCap {
                        core: LiveCore::new(
                            &self.master,
                            chart,
                            &cards.members,
                            &cards.snaps,
                            &goal["play"],
                        ),
                    };
                    let (value, teams, stats) = self.certified_score_cap(
                        cap_key,
                        search::Input {
                            master: &self.master,
                            player: &player,
                            members: &cards.members,
                            snaps: &cards.snaps,
                            music: Some(&music),
                            objective: &mut cap,
                            constraints: constraints.clone(),
                            k: 1,
                            deadline: song_deadline,
                            now,
                            seeds: seeds.clone(),
                            shard: None,
                            owns: None,
                            floor: f64::NEG_INFINITY,
                            floor_exclusive: false,

                            streaming: true,
                            disable_snap_dominance: false,
                            member_power_percent: Some(&member_pct),
                            snap_power_percent: Some(&snap_pct),
                            on_candidate: None,
                            progress: None,
                        },
                        generation,
                    )?;
                    objective.core.score_cap = value;
                    seeds.extend(teams);
                    cap_stats.add(&stats);
                }
                if objective.core.drains {
                    let mut ap = LiveObjective {
                        core: LiveCore::new(
                            &self.master,
                            chart,
                            &cards.members,
                            &cards.snaps,
                            &Value::Null,
                        ),
                        criterion: objective.criterion,
                        target: objective.target,
                    };
                    let probe = search::search_at(
                        search::Input {
                            master: &self.master,
                            player: &player,
                            members: &cards.members,
                            snaps: &cards.snaps,
                            music: Some(&music),
                            objective: &mut ap,
                            constraints: constraints.clone(),
                            k: 20.max(k * 3),
                            deadline: (now()
                                + 150.0_f64.min((song_deadline - now()).max(0.0) / 4.0))
                            .min(song_deadline),
                            now,
                            seeds: seeds.clone(),
                            shard: None,
                            owns: None,
                            floor: f64::NEG_INFINITY,
                            floor_exclusive: false,

                            streaming: true,
                            disable_snap_dominance: false,
                            member_power_percent: Some(&member_pct),
                            snap_power_percent: Some(&snap_pct),
                            on_candidate: None,
                            progress: None,
                        },
                        generation,
                    )?;
                    seeds.extend(probe.hits.into_iter().map(|hit| hit.team));
                }
                if shard.is_some_and(|(_, count)| count > 1) {
                    let probe = search::search_at(
                        search::Input {
                            master: &self.master,
                            player: &player,
                            members: &cards.members,
                            snaps: &cards.snaps,
                            music: Some(&music),
                            objective: &mut objective,
                            constraints: constraints.clone(),
                            k,
                            deadline: (now() + 300.0).min(song_deadline),
                            now,
                            seeds: seeds.clone(),
                            shard: None,
                            owns: None,
                            floor: f64::NEG_INFINITY,
                            floor_exclusive: false,

                            streaming: true,
                            disable_snap_dominance: false,
                            member_power_percent: Some(&member_pct),
                            snap_power_percent: Some(&snap_pct),
                            on_candidate: None,
                            progress: None,
                        },
                        generation,
                    )?;
                    seeds.extend(probe.hits.into_iter().map(|hit| hit.team));
                }
                search::search_at(
                    search::Input {
                        master: &self.master,
                        player: &player,
                        members: &cards.members,
                        snaps: &cards.snaps,
                        music: Some(&music),
                        objective: &mut objective,
                        constraints: constraints.clone(),
                        k,
                        deadline: song_deadline,
                        now,
                        seeds,
                        shard,
                        owns: None,
                        floor: f64::NEG_INFINITY,
                        floor_exclusive: false,

                        streaming: true,
                        disable_snap_dominance: false,
                        member_power_percent: Some(&member_pct),
                        snap_power_percent: Some(&snap_pct),
                        on_candidate: Some(&mut crate::progress::emit),
                        progress: None,
                    },
                    generation,
                )?
            };
            output.stats.add(&cap_stats);
            let hits:Vec<_>=output.hits.iter().map(|hit|{
                let scores=&hit.detail["scores"];let values:Vec<_>=master::rows(&scores["scores"]).iter().map(|v|master::number(v,0.0)).collect();let mut sorted=values.clone();sorted.sort_by(f64::total_cmp);let mut ranks:BTreeMap<i32,i32>=BTreeMap::new();for &value in &values{*ranks.entry(chart::score_rank(chart,value)).or_default()+=1;}
                let keys=|order:usize|live::orders()[order].map(|slot|cards.members[hit.team.members[slot]].key.clone());
                json!({"song":song,"members":hit.team.members.iter().map(|&i|cards.members[i].key.clone()).collect::<Vec<_>>(),"snaps":hit.team.snaps.iter().map(|&j|if j<0{None}else{Some(cards.snaps[j as usize].key.clone())}).collect::<Vec<_>>(),"power":hit.power,"slotPowers":hit.slot_powers,"key":hit.key,"score":{"mean":scores["mean"],"min":scores["min"],"max":scores["max"],"median":(sorted[59]+sorted[60])/2.0,"worstOrder":keys(master::int(&scores["minOrder"]) as usize),"bestOrder":keys(master::int(&scores["maxOrder"]) as usize),"orders":values.iter().map(|v|crate::num::round(*v)).collect::<Vec<_>>(),"ranks":ranks}})
            }).collect();
            self.live_cache.replace(Some((cache_key, objective.core)));
            overall.extend(hits.iter().cloned());
            results.push(json!({"song":song,"hits":hits,"proven":output.proven,"bound":output.bound,"stats":output.stats}));
        }
        overall.sort_by(compare_hits);
        overall.truncate(k);
        Ok(
            json!({"results":results,"overall":overall,"unknownCards":cards.unknown,"elapsedMs":now()-started,"stopReason":if generation!=crate::cancellation_generation(){Some("cancelled")}else if now()>=deadline{Some("budget")}else{None}}),
        )
    }
}

/// Placement in each of the three ranges: `goal.ranks`, else `goal.rank` everywhere.
fn gekiso_ranks(goal: &Value) -> Vec<i32> {
    let ranks: Vec<i32> = master::rows(&goal["ranks"])
        .iter()
        .map(|v| master::int(v))
        .collect();
    if ranks.is_empty() {
        vec![master::number(&goal["rank"], 1.0) as i32; 3]
    } else {
        ranks
    }
}
/// Opponents' scores per range from `goal.opponents` (one list per range).
fn gekiso_opponents(goal: &Value) -> Vec<Vec<f64>> {
    master::rows(&goal["opponents"])
        .iter()
        .map(|range| {
            master::rows(range)
                .iter()
                .filter_map(|v| v.as_f64())
                .filter(|v| v.is_finite())
                .collect()
        })
        .collect()
}

impl Engine {
    pub fn full_orders(
        &self,
        request: &Value,
        team: &Value,
        song: &Value,
        source: &crate::chart::Source,
    ) -> Result<Value, String> {
        use crate::full::{
            conditions::Performer,
            model::{Model, Params},
            play::{self, Accuracy, Setup},
        };
        let inputs: Vec<MemberInput> =
            serde_json::from_value(request["members"].clone()).map_err(|e| e.to_string())?;
        let snaps: Vec<SnapInput> =
            serde_json::from_value(request["snaps"].clone()).map_err(|e| e.to_string())?;
        let cards = power::resolve_box(
            &self.master,
            &inputs,
            &snaps,
            request["unknownPolicy"] != "min",
        );
        let members = master::rows(&team["members"])
            .iter()
            .map(|key| {
                cards
                    .members
                    .iter()
                    .find(|m| key.as_str() == Some(&m.key))
                    .cloned()
                    .ok_or_else(|| format!("unknown member:{key}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let snaps = master::rows(&team["snaps"])
            .iter()
            .map(|key| {
                if key.is_null() {
                    Ok(None)
                } else {
                    cards
                        .snaps
                        .iter()
                        .find(|s| key.as_str() == Some(&s.key))
                        .cloned()
                        .map(Some)
                        .ok_or_else(|| format!("unknown snap:{key}"))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        if members.len() != 5 || snaps.len() != 5 {
            return Err("a team requires five slots".into());
        }
        let song_id = master::int(&song["songId"]);
        let difficulty = master::int(&song["difficulty"]);
        let item = self.master.songs.get(&song_id).ok_or("unknown song")?;
        let level = item
            .difficulties
            .iter()
            .find(|d| d.difficulty == difficulty)
            .ok_or("unknown difficulty")?
            .play_level;
        crate::progress::keys(&cards.members, &cards.snaps);
        let player = power::player_state(&self.master, &request["player"]);
        let music = power::music_view(&self.master, song_id, None)?;
        let power = power::team_power(
            &self.master,
            &player,
            &members,
            &snaps,
            0,
            Some(&music),
            &[],
        )?;
        let deck: Vec<_> = members
            .iter()
            .zip(&snaps)
            .map(|(m, s)| Performer::from_member(&self.master, m, s.as_ref()))
            .collect();
        let chart = play::compile(&self.master, source, level)?;
        let setup = Setup {
            fevers: chart.fevers.clone(),
            missions: item.missions.clone(),
            ranks: gekiso_ranks(&request["goal"]),
        };
        if setup
            .missions
            .iter()
            .take(setup.fevers.len())
            .any(|&m| m == 2)
        {
            return Err("full Gekisou scores need a lottery-free chart".into());
        }
        let accuracy: Accuracy = serde_json::from_value(request["goal"]["accuracy"].clone())
            .map_err(|e| e.to_string())?;
        let play = play::judgement_stream(&self.master, &chart, Some(&setup), accuracy)?;
        let params = Params {
            music_type: music.music_type,
            power: power.total as i32,
            level,
            converted: chart.converted,
            music_length: chart.last_timing + 1000,
            assist: 1.0,
        };
        let generation = crate::cancellation_generation();
        let master = &self.master;
        let replay = |order: &[usize; 5]| -> Result<(f64, [f64; 3]), String> {
            if generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
            let ordered: Vec<_> = order.iter().map(|&slot| deck[slot].clone()).collect();
            let mut model = Model::new(
                master,
                &ordered,
                &chart.notes,
                &chart.events,
                params.clone(),
                Some(&setup),
                (0, false),
            )?;
            for (i, frame) in play.frames.iter().enumerate() {
                model.frame(frame.time_ms, &frame.judged, play.dt[i])?;
            }
            let mut ranges = [0.0; 3];
            if let Some(g) = &model.gk {
                for (range, state) in ranges.iter_mut().zip(&g.controller.states) {
                    *range = state.score() as f64;
                }
            }
            Ok((model.score.score.score as f64, ranges))
        };
        let replays = crate::parallel::map(live::orders(), replay)?;
        let scores: Vec<f64> = replays.iter().map(|r| r.0).collect();
        let range_scores: Vec<f64> = (0..3)
            .map(|r| replays.iter().map(|x| x.1[r]).sum::<f64>() / replays.len().max(1) as f64)
            .collect();
        let mut sorted = scores.clone();
        sorted.sort_by(f64::total_cmp);
        let total = sorted.iter().fold(0.0, |sum, v| sum + v);
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let (mut min_order, mut max_order) = (0, 0);
        for (i, &value) in scores.iter().enumerate() {
            if value < min {
                min = value;
                min_order = i;
            }
            if value > max {
                max = value;
                max_order = i;
            }
        }
        Ok(
            json!({"power":power.total,"scores":scores,"mean":total/120.0,"min":min,"max":max,"minOrder":min_order,"maxOrder":max_order,"rangeScores":range_scores}),
        )
    }
}

impl Engine {
    pub fn evaluate_gekiso(
        &self,
        request: &Value,
        team: &Value,
        song: &Value,
        source: &crate::chart::Source,
    ) -> Result<Value, String> {
        use crate::{
            full::{conditions::Performer, play::Accuracy},
            gekiso,
        };
        let inputs: Vec<MemberInput> =
            serde_json::from_value(request["members"].clone()).map_err(|e| e.to_string())?;
        let snap_inputs: Vec<SnapInput> =
            serde_json::from_value(request["snaps"].clone()).map_err(|e| e.to_string())?;
        let cards = power::resolve_box(
            &self.master,
            &inputs,
            &snap_inputs,
            request["unknownPolicy"] != "min",
        );
        let members = master::rows(&team["members"])
            .iter()
            .map(|key| {
                cards
                    .members
                    .iter()
                    .find(|m| key.as_str() == Some(&m.key))
                    .cloned()
                    .ok_or_else(|| format!("unknown member:{key}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let snaps = master::rows(&team["snaps"])
            .iter()
            .map(|key| {
                if key.is_null() {
                    Ok(None)
                } else {
                    cards
                        .snaps
                        .iter()
                        .find(|s| key.as_str() == Some(&s.key))
                        .cloned()
                        .map(Some)
                        .ok_or_else(|| format!("unknown snap:{key}"))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        if members.len() != 5 || snaps.len() != 5 {
            return Err("a team requires five slots".into());
        }
        let song_id = master::int(&song["songId"]);
        let difficulty = master::int(&song["difficulty"]);
        let item = self.master.songs.get(&song_id).ok_or("unknown song")?;
        let level = item
            .difficulties
            .iter()
            .find(|d| d.difficulty == difficulty)
            .ok_or("unknown difficulty")?
            .play_level;
        crate::progress::keys(&cards.members, &cards.snaps);
        let player = power::player_state(&self.master, &request["player"]);
        let music = power::music_view(&self.master, song_id, None)?;
        let power = power::team_power(
            &self.master,
            &player,
            &members,
            &snaps,
            0,
            Some(&music),
            &[],
        )?;
        let deck: Vec<_> = members
            .iter()
            .zip(&snaps)
            .map(|(m, s)| Performer::from_member(&self.master, m, s.as_ref()))
            .collect();
        let accuracy: Accuracy = serde_json::from_value(request["goal"]["accuracy"].clone())
            .map_err(|e| e.to_string())?;
        let mut chart = gekiso::Chart::new(
            &self.master,
            source,
            level,
            item.missions.clone(),
            accuracy,
            gekiso_ranks(&request["goal"]),
        )?;
        chart.music_type = music.music_type;
        let count = master::int(&request["goal"]["seeds"]).max(0);
        let seeds = if count > 0 {
            (0..count).collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if !chart.luck || gekiso::uses_full_replay(&self.master, &chart, &seeds) {
            let scores =
                gekiso::replay_orders(&self.master, &chart, &deck, &seeds, power.total as i32)?;
            return serde_json::to_value(scores).map_err(|e| e.to_string());
        }
        let ctx = gekiso::context(&self.master, &mut chart, &deck, &seeds)?;
        let mut unsupported = Vec::new();
        let slots: Vec<_> = members
            .iter()
            .zip(&snaps)
            .map(|(m, s)| skills::resolve(&self.master, m, s.as_ref(), &mut unsupported))
            .collect();
        let scores =
            gekiso::score_orders(&self.master, &chart, &ctx, power.total as i32, &slots, true)?;
        serde_json::to_value(scores).map_err(|e| e.to_string())
    }
}

impl Engine {
    /// With opponents' range scores, the placements are those the recommended team's own average range scores earn:
    /// search at a placement, re-place from the best team's ranges, until the placement holds.
    pub fn search_gekiso(
        &self,
        request: &Value,
        source: &crate::chart::Source,
        started: f64,
        now: &dyn Fn() -> f64,
    ) -> Result<Value, String> {
        let generation = crate::cancellation_generation();
        let opponents = gekiso_opponents(&request["goal"]);
        if opponents.iter().all(Vec::is_empty) {
            return self.search_gekiso_at(request, source, started, now, generation);
        }
        let mut ranks = vec![1; 3];
        let mut tried = Vec::new();
        loop {
            let mut at = request.clone();
            at["goal"]["ranks"] = json!(ranks);
            at["goal"]["opponents"] = Value::Null;
            let mut output = self.search_gekiso_at(&at, source, started, now, generation)?;
            let ranges = output["overall"][0]["score"]["rangeScores"].clone();
            let next: Vec<i32> = (0..3)
                .map(|r| {
                    let own = master::number(&ranges[r], f64::INFINITY);
                    1 + opponents
                        .get(r)
                        .map_or(0, |o| o.iter().filter(|&&s| s > own).count().min(4))
                        as i32
                })
                .collect();
            if ranges.is_null() && !output["overall"][0].is_null() {
                return Err("gekiso-opponents-need-lottery-free-chart".into());
            }
            tried.push(ranks.clone());
            if ranges.is_null() || next == ranks || tried.contains(&next) {
                for result in output["results"].as_array_mut().into_iter().flatten() {
                    result["placements"] = json!(ranks);
                }
                return Ok(output);
            }
            ranks = next;
        }
    }
    fn search_gekiso_at(
        &self,
        request: &Value,
        source: &crate::chart::Source,
        started: f64,
        now: &dyn Fn() -> f64,
        generation: u64,
    ) -> Result<Value, String> {
        use crate::{full::play::Accuracy, gekiso, gekiso_objective, objectives::Criterion};
        let inputs: Vec<MemberInput> =
            serde_json::from_value(request["members"].clone()).map_err(|e| e.to_string())?;
        let snap_inputs: Vec<SnapInput> =
            serde_json::from_value(request["snaps"].clone()).map_err(|e| e.to_string())?;
        let cards = power::resolve_box(
            &self.master,
            &inputs,
            &snap_inputs,
            request["unknownPolicy"] != "min",
        );
        crate::progress::keys(&cards.members, &cards.snaps);
        let player = power::player_state(&self.master, &request["player"]);
        let goal = &request["goal"];
        let song = master::rows(&goal["songs"])
            .first()
            .copied()
            .ok_or("no Gekiso song")?;
        let song_id = master::int(&song["songId"]);
        let difficulty = master::int(&song["difficulty"]);
        let item = self.master.songs.get(&song_id).ok_or("unknown song")?;
        let level = item
            .difficulties
            .iter()
            .find(|d| d.difficulty == difficulty)
            .ok_or("unknown difficulty")?
            .play_level;
        let music = power::music_view(&self.master, song_id, None)?;
        let accuracy: Accuracy =
            serde_json::from_value(goal["accuracy"].clone()).map_err(|e| e.to_string())?;
        let cache_key = json!([
            song,
            request["members"],
            request["snaps"],
            request["player"],
            request["unknownPolicy"],
            request["constraints"],
            goal["accuracy"],
            goal["rank"],
            goal["seeds"],
            goal["ranks"]
        ])
        .to_string();

        let mut chart = if let Some((_, chart)) = self
            .gekiso_cache
            .borrow_mut()
            .take()
            .filter(|(key, _)| key == &cache_key)
        {
            chart
        } else {
            gekiso::Chart::new(
                &self.master,
                source,
                level,
                item.missions.clone(),
                accuracy,
                gekiso_ranks(goal),
            )?
        };
        let seed_count = master::int(&goal["seeds"]).max(0);
        let seeds: Vec<i32> = (0..seed_count).collect();
        let criterion = Criterion::from_value(&goal["criterion"]);
        let constraints = constraints(&request["constraints"], &cards.members, &cards.snaps);
        let k = master::number(&request["k"], 1.0).max(1.0) as usize;
        let deadline = request["deadline"].as_f64().unwrap_or_else(|| {
            request["timeLimitMs"]
                .as_f64()
                .map(|ms| started + ms)
                .unwrap_or(f64::INFINITY)
        });
        let shard = if request["shard"].is_null() {
            None
        } else {
            Some((
                master::int(&request["shard"]["index"]) as usize,
                master::int(&request["shard"]["count"]) as usize,
            ))
        };
        chart.music_type = music.music_type;
        let floor = request["floors"][format!("{song_id}:{difficulty}")]
            .as_f64()
            .unwrap_or(f64::NEG_INFINITY);
        let output = gekiso_objective::search(gekiso_objective::Input {
            env: gekiso_objective::Environment {
                master: &self.master,
                player: &player,
                members: &cards.members,
                snaps: &cards.snaps,
                music: Some(&music),
                constraints: constraints.clone(),
                now,
                generation,
            },
            chart: &mut chart,
            seeds: seeds.clone(),
            criterion,
            k,
            deadline,
            initial: seed_teams(request, song, &cards.members, &cards.snaps),
            seed_only: request["seedOnly"] == true,
            seed_variant: match request["seedVariant"].as_str() {
                Some("sample") => Some(false),
                Some("quiet") => Some(true),
                _ => None,
            },
            defer_scores: request["deferScores"] == true,
            min_combo_bonus: master::int(&goal["minComboBonus"]),
            seeded: request["seeded"] == true,
            shard,
            floor,
            on_candidate: Some(&mut crate::progress::emit),
            progress: None,
        })?;

        let compiled = crate::chart::compile(
            &self.master,
            item,
            item.difficulties
                .iter()
                .find(|d| d.difficulty == difficulty)
                .unwrap(),
            source,
        );
        let hits:Vec<_>=output.hits.iter().map(|hit|{if hit.detail["pendingScores"] == true { return json!({"comboBonus":hit.detail["comboBonus"],"song":song,"members":hit.team.members.iter().map(|&i|cards.members[i].key.clone()).collect::<Vec<_>>(),"snaps":hit.team.snaps.iter().map(|&j|if j<0{None}else{Some(cards.snaps[j as usize].key.clone())}).collect::<Vec<_>>(),"power":hit.power,"slotPowers":hit.slot_powers,"key":hit.key,"score":null,"pendingScores":true}); } let scores=&hit.detail["scores"];let values:Vec<_>=master::rows(&scores["scores"]).iter().map(|v|master::number(v,0.0)).collect();let mut sorted=values.clone();sorted.sort_by(f64::total_cmp);let mut ranks:BTreeMap<i32,i32>=BTreeMap::new();for &value in &values{*ranks.entry(crate::chart::battle_rank(&compiled,value)).or_default()+=1;}let keys=|order:usize|live::orders()[order].map(|slot|cards.members[hit.team.members[slot]].key.clone());json!({"comboBonus":hit.detail["comboBonus"],"song":song,"members":hit.team.members.iter().map(|&i|cards.members[i].key.clone()).collect::<Vec<_>>(),"snaps":hit.team.snaps.iter().map(|&j|if j<0{None}else{Some(cards.snaps[j as usize].key.clone())}).collect::<Vec<_>>(),"power":hit.power,"slotPowers":hit.slot_powers,"key":hit.key,"score":{"mean":scores["mean"],"min":scores["min"],"max":scores["max"],"median":(sorted[59]+sorted[60])/2.0,"worstOrder":keys(master::int(&scores["minOrder"]) as usize),"bestOrder":keys(master::int(&scores["maxOrder"]) as usize),"orders":values.iter().map(|v|crate::num::round(*v)).collect::<Vec<_>>(),"ranks":ranks,"rangeScores":scores["rangeScores"]}})}).collect();
        self.gekiso_cache.replace(Some((cache_key, chart)));
        Ok(
            json!({"results":[{"song":song,"hits":hits,"proven":output.proven,"bound":output.bound,"stats":output.stats}],"overall":hits,"unknownCards":cards.unknown,"elapsedMs":now()-started}),
        )
    }
}

impl Engine {
    /// Ordinary activity rewards. Sampled Gekisou and strict PT validation are
    /// separate protocols and must be validated before entering this route.
    pub fn search_event(
        &self,
        request: &Value,
        started: f64,
        now: &dyn Fn() -> f64,
    ) -> Result<Value, String> {
        use crate::{
            event::{ChallengePoints, EventObjective, Route, RouteKind, ScoreTie, SkipTie},
            objectives::{Criterion, LiveCore, LiveObjective, ScoreCap},
            search::{self, Objective},
        };
        let goal = &request["goal"];
        if !goal["gekiso"].is_null() {
            return Err("event-protocol-not-supported".into());
        }
        let pt = goal["ranking"] == "pt-only";
        if pt {
            let issues = crate::pt::validate(&self.master, request)?;
            if !issues.is_empty() {
                return Err(format!("pt-input:{}", issues.join(",")));
            }
        }
        let inputs: Vec<MemberInput> =
            serde_json::from_value(request["members"].clone()).map_err(|e| e.to_string())?;
        let snap_inputs: Vec<SnapInput> =
            serde_json::from_value(request["snaps"].clone()).map_err(|e| e.to_string())?;
        let cards = power::resolve_box(
            &self.master,
            &inputs,
            &snap_inputs,
            request["unknownPolicy"] != "min",
        );
        crate::progress::keys(&cards.members, &cards.snaps);
        let player = power::player_state(&self.master, &request["player"]);
        let kind = match goal["route"].as_str() {
            Some("challenge") => RouteKind::Challenge,
            Some("skip") => RouteKind::Skip,
            _ => RouteKind::Live,
        };
        let route = Route::new(
            &self.master,
            master::int(&goal["eventId"]),
            kind,
            master::int(&goal["consumption"]),
        )?;
        let songs = if kind == RouteKind::Skip && master::rows(&goal["songs"]).is_empty() {
            vec![Value::Null]
        } else {
            master::rows(&goal["songs"]).into_iter().cloned().collect()
        };
        let k = master::number(&request["k"], 1.0).max(1.0) as usize;
        let deadline = request["deadline"].as_f64().unwrap_or_else(|| {
            request["timeLimitMs"]
                .as_f64()
                .map(|ms| started + ms)
                .unwrap_or(f64::INFINITY)
        });
        let exchange = if goal["cpExchange"].is_null() {
            None
        } else {
            Some((
                master::number(&goal["cpExchange"]["numerator"], 0.0),
                master::number(&goal["cpExchange"]["denominator"], 1.0),
            ))
        };
        if exchange.is_some_and(|(_, d)| d <= 0.0) {
            return Err("invalid-cp-exchange".into());
        }
        let constraints = constraints(&request["constraints"], &cards.members, &cards.snaps);
        let generation = crate::cancellation_generation();
        let mut results = Vec::new();
        let mut overall = Vec::new();
        for song in songs {
            if now() >= deadline || generation != crate::cancellation_generation() {
                results.push(json!({"song":song,"hits":[],"proven":false,"bound":null,"stats":search::Stats::default()}));
                continue;
            }
            let chart = if song.is_null() {
                None
            } else {
                self.charts.get(&(
                    master::int(&song["songId"]),
                    master::int(&song["difficulty"]),
                ))
            };
            if kind != RouteKind::Skip && chart.is_none() {
                return Err("missing-chart".into());
            }
            if pt {
                crate::pt::validate_chart(
                    &self.master,
                    master::int(&song["songId"]),
                    master::int(&song["difficulty"]),
                    self.sources
                        .get(&(
                            master::int(&song["songId"]),
                            master::int(&song["difficulty"]),
                        ))
                        .ok_or("missing-chart")?,
                )?;
            }
            let challenge = if kind == RouteKind::Challenge {
                Some(master::int(&goal["eventId"]))
            } else {
                None
            };
            let music = if song.is_null() {
                None
            } else {
                Some(power::music_view(
                    &self.master,
                    master::int(&song["songId"]),
                    challenge,
                )?)
            };
            let member_pct = |m: &power::Member| {
                if kind == RouteKind::Challenge {
                    power::member_event_percent(&route.effects, m, 2)
                } else {
                    0
                }
            };
            let snap_pct = |s: &power::Snap| {
                if kind == RouteKind::Challenge {
                    power::snap_event_percent(&route.effects, s, 2)
                } else {
                    0
                }
            };
            let (score_cap, cap_seeds, cap_stats) =
                if let Some(chart) = chart.filter(|_| kind != RouteKind::Skip) {
                    let key = json!([
                        song,
                        request["members"],
                        request["snaps"],
                        request["player"],
                        request["unknownPolicy"],
                        request["constraints"],
                        goal["play"],
                        challenge
                    ])
                    .to_string();
                    let mut cap = ScoreCap {
                        core: LiveCore::new(
                            &self.master,
                            chart,
                            &cards.members,
                            &cards.snaps,
                            &goal["play"],
                        ),
                    };
                    self.certified_score_cap(
                        key,
                        search::Input {
                            master: &self.master,
                            player: &player,
                            members: &cards.members,
                            snaps: &cards.snaps,
                            music: music.as_ref(),
                            objective: &mut cap,
                            constraints: constraints.clone(),
                            k: 1,
                            deadline,
                            now,
                            seeds: Vec::new(),
                            shard: None,
                            owns: None,
                            floor: f64::NEG_INFINITY,
                            floor_exclusive: false,

                            streaming: true,
                            disable_snap_dominance: false,
                            member_power_percent: Some(&member_pct),
                            snap_power_percent: Some(&snap_pct),
                            on_candidate: None,
                            progress: None,
                        },
                        generation,
                    )?
                } else {
                    (f64::INFINITY, Vec::new(), search::Stats::default())
                };
            // Weighted rewards: {points, items, challengePoints}, relative to the measure's weight.
            let weights = (!goal["weights"].is_null()).then(|| {
                let w = |key: &str| master::number(&goal["weights"][key], 0.0).max(0.0);
                let (main, other) = if goal["measure"] == "items" {
                    (w("items"), w("points"))
                } else {
                    (w("points"), w("items"))
                };
                let main = if main > 0.0 { main } else { 1.0 };
                (w("challengePoints") / main, other / main)
            });
            let make_event = || {
                let objective = EventObjective::new(
                    &self.master,
                    &cards.members,
                    &cards.snaps,
                    chart,
                    route.clone(),
                    goal["measure"] != "items",
                    &goal["play"],
                    true,
                    0.0,
                    exchange,
                    score_cap,
                );
                match weights {
                    Some((cp, other)) => objective.with_weights(cp, other),
                    None => objective,
                }
            };
            let cp_measure =
                goal["measure"] == "challenge-points" && chart.is_some() && kind != RouteKind::Skip;
            let phase = request["rewardPhase"]["kind"].as_str();
            if phase.is_some_and(|phase| !["primary", "other", "score"].contains(&phase))
                || phase.is_some()
                    && (k != 1
                        || kind == RouteKind::Skip
                        || cp_measure
                        || master::rows(&goal["songs"]).len() != 1)
            {
                return Err("event-reward-phase".into());
            }
            let mut objective: Box<dyn Objective> = if cp_measure {
                Box::new(ChallengePoints::new(
                    LiveObjective {
                        core: LiveCore::new(
                            &self.master,
                            chart.unwrap(),
                            &cards.members,
                            &cards.snaps,
                            &goal["play"],
                        ),
                        criterion: Criterion::Mean,
                        target: None,
                    },
                    chart.unwrap().clone(),
                    route.clone(),
                ))
            } else {
                Box::new(make_event())
            };
            let shard = if request["shard"].is_null() {
                None
            } else {
                Some((
                    master::int(&request["shard"]["index"]) as usize,
                    master::int(&request["shard"]["count"]) as usize,
                ))
            };
            let run = |objective: &mut dyn Objective,
                       search_k,
                       seeds,
                       floor,
                       reward_group: Option<(&[search::Hit], f64)>,
                       notify: bool| {
                let mut publish = |hits: &[search::Hit]| {
                    if let Some((original, key)) = reward_group {
                        crate::progress::emit(&merge_reward_candidates(
                            original, hits, key, k, true,
                        ));
                    } else {
                        crate::progress::emit(hits);
                    }
                };
                search::search(search::Input {
                    master: &self.master,
                    player: &player,
                    members: &cards.members,
                    snaps: &cards.snaps,
                    music: music.as_ref(),
                    objective,
                    constraints: constraints.clone(),
                    k: search_k,
                    deadline,
                    now,
                    seeds,
                    shard,
                    owns: None,
                    floor,
                    floor_exclusive: false,

                    streaming: true,
                    disable_snap_dominance: false,
                    member_power_percent: Some(&member_pct),
                    snap_power_percent: Some(&snap_pct),
                    on_candidate: if notify { Some(&mut publish) } else { None },
                    progress: None,
                })
            };
            let mut seeds = seed_teams(request, &song, &cards.members, &cards.snaps);
            seeds.extend(cap_seeds);
            let mut output = if phase.is_some_and(|phase| phase != "primary") {
                let target = request["rewardPhase"]["reward"]
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or("missing-reward-target")?;
                let mut tie = ScoreTie::new(
                    make_event(),
                    LiveObjective {
                        core: LiveCore::new(
                            &self.master,
                            chart.unwrap(),
                            &cards.members,
                            &cards.snaps,
                            &goal["play"],
                        ),
                        criterion: Criterion::Mean,
                        target: None,
                    },
                    target,
                    &[],
                );
                tie.allow_dominance = true;
                tie.rank_other = phase == Some("other");
                if !tie.rank_other {
                    tie.other_target = Some(
                        request["rewardPhase"]["other"]
                            .as_f64()
                            .filter(|value| value.is_finite())
                            .ok_or("missing-other-reward-target")?,
                    );
                }
                let mut output = run(
                    &mut tie,
                    1,
                    seeds,
                    master::number(&request["rewardPhase"]["floor"], 0.0),
                    Some((&[], target)),
                    true,
                )?;
                output.hits.retain(|hit| hit.key >= 0.0);
                for hit in &mut output.hits {
                    hit.key = target;
                }
                output.bound = (!output.proven).then_some(target);
                output
            } else {
                let floor = if phase == Some("primary") {
                    request["floors"][format!(
                        "{}:{}",
                        master::int(&song["songId"]),
                        master::int(&song["difficulty"])
                    )]
                    .as_f64()
                    .unwrap_or(f64::NEG_INFINITY)
                } else {
                    f64::NEG_INFINITY
                };
                run(objective.as_mut(), k, seeds, floor, None, true)?
            };
            output.stats.add(&cap_stats);
            if let Some(tie_chart) = chart.filter(|_| {
                phase.is_none() && !cp_measure && kind != RouteKind::Skip && !output.hits.is_empty()
            }) {
                let mut keys: Vec<_> = output.hits.iter().map(|h| h.key).collect();
                keys.sort_by(|a, b| b.total_cmp(a));
                keys.dedup();
                let mut ordered = Vec::<search::Hit>::new();
                for key in keys {
                    if ordered.len() >= k {
                        break;
                    }
                    let seeds = output
                        .hits
                        .iter()
                        .filter(|h| h.key == key)
                        .map(|h| h.team.clone())
                        .collect();
                    let excluded: Vec<_> = ordered.iter().map(|h| h.team.clone()).collect();
                    let mut tie = ScoreTie::new(
                        make_event(),
                        LiveObjective {
                            core: LiveCore::new(
                                &self.master,
                                tie_chart,
                                &cards.members,
                                &cards.snaps,
                                &goal["play"],
                            ),
                            criterion: Criterion::Mean,
                            target: None,
                        },
                        key,
                        &excluded,
                    );
                    let baseline: Vec<_> = output.hits.iter().chain(&ordered).cloned().collect();
                    tie.allow_dominance = output.proven;
                    tie.rank_other = true;
                    let reranked = run(
                        &mut tie,
                        k - ordered.len(),
                        seeds,
                        0.0,
                        Some((&baseline, key)),
                        true,
                    )?;
                    output.stats.add(&reranked.stats);
                    if !reranked.proven {
                        output.proven = false;
                        output.bound = Some(output.bound.unwrap_or(f64::NEG_INFINITY).max(key));
                    }
                    let secondary =
                        score_tied_hits(&output.hits, reranked.hits, key, k - ordered.len(), true);
                    let mut other_keys: Vec<_> = secondary
                        .iter()
                        .map(|h| master::number(&h.detail["otherMean"], 0.0))
                        .collect();
                    other_keys.sort_by(|a, b| b.total_cmp(a));
                    other_keys.dedup();
                    for other in other_keys {
                        if ordered.len() >= k {
                            break;
                        }
                        let group: Vec<_> = secondary
                            .iter()
                            .filter(|h| master::number(&h.detail["otherMean"], 0.0) == other)
                            .cloned()
                            .collect();
                        tie.rank_other = false;
                        tie.allow_dominance = output.proven;
                        tie.other_target = Some(other);
                        let reranked = run(
                            &mut tie,
                            k - ordered.len(),
                            group.iter().map(|h| h.team.clone()).collect(),
                            0.0,
                            Some((&baseline, key)),
                            true,
                        )?;
                        output.stats.add(&reranked.stats);
                        if !reranked.proven {
                            output.proven = false;
                            output.bound = Some(output.bound.unwrap_or(f64::NEG_INFINITY).max(key));
                        }
                        ordered.extend(score_tied_hits(
                            &group,
                            reranked.hits,
                            key,
                            k - ordered.len(),
                            true,
                        ));
                    }
                }
                output.hits = ordered;
            }
            if kind == RouteKind::Skip && !output.hits.is_empty() {
                let mut keys: Vec<_> = output.hits.iter().map(|h| h.key).collect();
                keys.sort_by(|a, b| b.total_cmp(a));
                keys.dedup();
                let mut ordered = Vec::new();
                for key in keys {
                    if ordered.len() >= k {
                        break;
                    }
                    let mut tie = SkipTie {
                        event: make_event(),
                        target: key,
                    };
                    let baseline = output.hits.clone();
                    let reranked = run(
                        &mut tie,
                        k - ordered.len(),
                        baseline
                            .iter()
                            .filter(|h| h.key == key)
                            .map(|h| h.team.clone())
                            .collect(),
                        0.0,
                        Some((&baseline, key)),
                        true,
                    )?;
                    output.stats.add(&reranked.stats);
                    if !reranked.proven {
                        output.proven = false;
                        output.bound = Some(output.bound.unwrap_or(f64::NEG_INFINITY).max(key));
                    }
                    ordered.extend(score_tied_hits(
                        &baseline,
                        reranked.hits,
                        key,
                        k - ordered.len(),
                        true,
                    ));
                }
                output.hits = ordered;
            }
            let hits:Vec<_>=output.hits.iter().map(|hit|{
                let d=&hit.detail;let mut value=json!({"song":song,"members":hit.team.members.iter().map(|&i|&cards.members[i].key).collect::<Vec<_>>(),"snaps":hit.team.snaps.iter().map(|&j|if j<0{None}else{Some(&cards.snaps[j as usize].key)}).collect::<Vec<_>>(),"power":hit.power,"slotPowers":hit.slot_powers,"key":hit.key,"event":{"mean":master::number(&d["mean"],0.0)/exchange.map(|(_,d)|d).unwrap_or(1.0),"bonusPercent":master::number(&d["bonus"],0.0)/100.0,"challengePoints":master::number(&d["challengePoints"],0.0),"directMean":d["directMean"],"otherMean":d["otherMean"],"perPlayMin":master::number(&d["directMin"],master::rows(&d["perOrder"]).into_iter().map(|v|master::number(v,0.0)).reduce(f64::min).unwrap_or_else(||master::number(&d["mean"],0.0))),"perPlayMax":master::number(&d["directMax"],master::rows(&d["perOrder"]).into_iter().map(|v|master::number(v,0.0)).reduce(f64::max).unwrap_or_else(||master::number(&d["mean"],0.0)))}});
                if pt {value["event"]["rewardSum"]=d["rewardSum"].clone();value["event"]["comparisonSum"]=d["comparisonSum"].clone();value["event"]["orders"]=json!(120);value["event"]["directMean"]=json!(master::number(&d["rewardSum"],0.0)/120.0);value["event"]["convertedMean"]=json!(exchange.map(|(n,den)|master::number(&d["challengePoints"],0.0)*n/den).unwrap_or(0.0));}
                if let Some(chart)=chart.filter(|_|!d["scores"].is_null()){
                    let scores=&d["scores"];let values:Vec<_>=master::rows(&scores["scores"]).iter().map(|v|master::number(v,0.0)).collect();let mut sorted=values.clone();sorted.sort_by(f64::total_cmp);let mut ranks=BTreeMap::<i32,i32>::new();for &v in &values{*ranks.entry(chart::score_rank(chart,v)).or_default()+=1;}let keys=|order:usize|live::orders()[order].map(|slot|cards.members[hit.team.members[slot]].key.clone());value["score"]=json!({"mean":scores["mean"],"min":scores["min"],"max":scores["max"],"median":(sorted[59]+sorted[60])/2.0,"orders":values,"bestOrder":keys(master::int(&scores["maxOrder"]) as usize),"worstOrder":keys(master::int(&scores["minOrder"]) as usize),"ranks":ranks});
                }value
            }).collect();
            overall.extend(hits.iter().cloned());
            let mut part = json!({"song":song,"hits":hits,"proven":output.proven,"bound":output.bound,"stats":output.stats});
            if let Some(phase) = phase {
                part["rewardPhase"] = json!(phase);
            }
            results.push(part);
        }
        overall.sort_by(compare_hits);
        overall.truncate(k);
        Ok(
            json!({"results":results,"overall":overall,"unknownCards":cards.unknown,"elapsedMs":now()-started,"stopReason":if generation!=crate::cancellation_generation(){Some("cancelled")}else if now()>=deadline{Some("budget")}else{None}}),
        )
    }
}

impl Engine {
    /// Charts enter just before their song is evaluated, keeping a bounded
    /// release-bound cache even for requests containing the entire song list.
    /// The best value of the goal with each owned card raised to full growth, for every card that is not there yet,
    /// most improving first.
    pub fn growth(
        &mut self,
        input: &Value,
        started: f64,
        now: &dyn Fn() -> f64,
    ) -> Result<Value, String> {
        let mut base_input = input.clone();
        base_input["request"]["k"] = json!(1);
        let callback = crate::progress::callback();
        let base = crate::progress::with_callback(None, || self.run(&base_input, started, now))?;
        let best = |out: &Value| {
            out["overall"][0]["key"]
                .as_f64()
                .unwrap_or(f64::NEG_INFINITY)
        };
        let base_key = best(&base);
        let request = &input["request"];
        let maximum = request["unknownPolicy"] != "min";
        let members: Vec<MemberInput> =
            serde_json::from_value(request["members"].clone()).map_err(|e| e.to_string())?;
        let snaps: Vec<SnapInput> =
            serde_json::from_value(request["snaps"].clone()).map_err(|e| e.to_string())?;
        let mut candidates = Vec::new();
        for (i, item) in members.iter().enumerate() {
            let Some(card) = self.master.members.get(&item.card_id) else {
                continue;
            };
            let raised = MemberInput {
                maximize: true,
                ..item.clone()
            };
            if power::member_growth(&self.master, card, item, maximum)
                != power::member_growth(&self.master, card, &raised, maximum)
            {
                candidates.push(("members", i, item.key.clone()));
            }
        }
        for (i, item) in snaps.iter().enumerate() {
            let Some(card) = self.master.snaps.get(&item.card_id) else {
                continue;
            };
            let raised = SnapInput {
                maximize: true,
                ..item.clone()
            };
            if power::snap_growth(&self.master, card, item, maximum)
                != power::snap_growth(&self.master, card, &raised, maximum)
            {
                candidates.push(("snaps", i, item.key.clone()));
            }
        }
        let generation = crate::cancellation_generation();
        let mut rows = Vec::with_capacity(candidates.len());
        base_input["charts"] = json!([]);
        for (done, (kind, index, key)) in candidates.iter().enumerate() {
            if generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
            let mut next = base_input.clone();
            next["request"][*kind][*index]["maximize"] = json!(true);
            let out = crate::progress::with_callback(None, || self.run(&next, now(), now))?;
            let value = best(&out);
            rows.push(json!({"kind":kind,"key":key,"value":value,"gain":value-base_key,"team":out["overall"][0]}));
            crate::progress::send(
                callback,
                &json!({"growth":{"done":done + 1,"total":candidates.len()}}),
            );
        }
        rows.sort_by(|a, b| {
            master::number(&b["gain"], 0.0).total_cmp(&master::number(&a["gain"], 0.0))
        });
        Ok(
            json!({"base":base_key,"baseTeam":base["overall"][0],"upgrades":rows,"elapsedMs":now()-started}),
        )
    }
    /// Per unit of deck power, each requested chart's score without skills and the score a +100 % score skill of
    /// 5 seconds adds at each skill event (live: All Perfect; Gekisou: the goal's accuracy and placements), with the
    /// chart's score rank thresholds. Expected score = power x (base + mean skill value x sum of weights).
    pub fn rates(&mut self, input: &Value) -> Result<Value, String> {
        let request = &input["request"];
        let gekiso_mode = request["goal"]["mode"] == "gekiso";
        let plain = live::Slot {
            effects: vec![live::Effect {
                kind: 2000,
                delta: 100000,
                seconds: 5.0,
                gate: live::Gate {
                    kind: 0,
                    value: 0.0,
                    positive: false,
                },
                judgements: Vec::new(),
            }],
            extension_ms: 0.0,
            recovery: 0,
            convert: None,
        };
        let mut out = Vec::new();
        for song in master::rows(&request["goal"]["songs"]) {
            let key = (
                master::int(&song["songId"]),
                master::int(&song["difficulty"]),
            );
            if let Some(entry) = master::rows(&input["charts"])
                .into_iter()
                .find(|r| r["song"] == *song)
            {
                let source = if entry["source"].is_null() {
                    crate::runtime_chart::convert(&entry["chart"])?
                } else {
                    serde_json::from_value(entry["source"].clone()).map_err(|e| e.to_string())?
                };
                self.insert_chart(key.0, key.1, source)?;
            }
            let chart = self.charts.get(&key).ok_or("missing-chart")?;
            let ranks: Vec<Value> = chart
                .ranks
                .iter()
                .map(|r| json!({"rank":r.rank,"required":r.required,"battleRequired":r.battle_required}))
                .collect();
            let window = crate::num::window_ms(5.0, 0.0);
            // Skill windows to weigh besides the plain five seconds (ms), e.g. every snap extension.
            let lengths: Vec<i32> = master::rows(&request["goal"]["windows"])
                .iter()
                .map(|v| master::int(v))
                .collect();
            let per_window =
                |times: &[i32], events: &[i32], prefix: &[f64], n: f64| -> Vec<Vec<f64>> {
                    lengths
                        .iter()
                        .map(|&length| {
                            events
                                .iter()
                                .map(|&time| {
                                    let a = times.partition_point(|&t| t < time);
                                    let b = times.partition_point(|&t| t < time + length);
                                    (prefix[b] - prefix[a]) / n
                                })
                                .collect()
                        })
                        .collect()
                };
            let (base, weights, general, perfect) = if gekiso_mode {
                let item = self.master.songs.get(&key.0).ok_or("unknown song")?;
                let level = item
                    .difficulties
                    .iter()
                    .find(|d| d.difficulty == key.1)
                    .ok_or("unknown difficulty")?
                    .play_level;
                let accuracy: crate::full::play::Accuracy =
                    serde_json::from_value(request["goal"]["accuracy"].clone())
                        .map_err(|e| e.to_string())?;
                let source = self.sources.get(&key).ok_or("missing-chart")?;
                let mut g = crate::gekiso::Chart::new(
                    &self.master,
                    source,
                    level,
                    item.missions.clone(),
                    accuracy,
                    gekiso_ranks(&request["goal"]),
                )?;
                let ctx = crate::gekiso::context(&self.master, &mut g, &[], &[])?;
                let w = crate::gekiso_objective::note_weights(&self.master, &g, &ctx);
                let n = g.full.converted as f64;
                let events: Vec<i32> = g
                    .full
                    .events
                    .iter()
                    .take(5)
                    .map(|&(_, time)| time)
                    .collect();
                let weights: Vec<f64> = events
                    .iter()
                    .map(|&time| {
                        let a = g.times.partition_point(|&t| t < time);
                        let b = g.times.partition_point(|&t| t < time + window);
                        (w.general[b] - w.general[a]) / n
                    })
                    .collect();
                let perfect_prefix: Vec<f64> = (0..w.general.len())
                    .map(|i| w.judgements[1][i] + w.judgements[2][i])
                    .collect();
                (
                    w.base / n,
                    weights,
                    per_window(&g.times, &events, &w.general, n),
                    per_window(&g.times, &events, &perfect_prefix, n),
                )
            } else {
                let prepared = chart::prepare(&self.master, chart, 0, Vec::new(), None, 1.0, 100.0);
                let linear = crate::model::linear(&prepared);
                let w = crate::model::skill_weights(&linear, &prepared, &plain);
                let n = prepared.converted_count as f64;
                let events: Vec<i32> = prepared.skill_times.iter().take(5).copied().collect();
                let general = per_window(&prepared.times, &events, &linear.prefix, n);
                (
                    linear.total / n,
                    w.per_event[..events.len()].iter().map(|v| v / n).collect(),
                    general.clone(),
                    general,
                )
            };
            let skip = chart::skip_score(&self.master, chart, 1_000_000) as f64 / 1_000_000.0;
            let notes = chart.times.len();
            out.push(json!({
                "song": song, "mode": if gekiso_mode {"gekiso"} else {"live"}, "base": base, "weights": weights,
                "ranks": ranks, "skip": skip, "notes": notes,
                "windows": {"lengths": lengths, "general": general, "perfect": perfect},
            }));
        }
        Ok(json!({"songs":out}))
    }
    pub fn run(
        &mut self,
        input: &Value,
        started: f64,
        now: &dyn Fn() -> f64,
    ) -> Result<Value, String> {
        crate::objectives::reset_slot_table();
        let request = &input["request"];
        let kind = request["goal"]["kind"].as_str().ok_or("missing-goal")?;
        crate::progress::mode(&request["goal"]);
        let primary = kind == "event" && !request["goal"]["ranking"].is_null();
        let pt = kind == "event" && request["goal"]["ranking"] == "pt-only";
        if !request["cutoff"].is_null()
            && (!primary
                || master::number(&request["k"], 1.0) != 1.0
                || request["cutoff"]["contextKey"] != request["contextKey"]
                || !request["cutoff"]["key"]
                    .as_f64()
                    .is_some_and(f64::is_finite))
        {
            return Err("invalid-reward-cutoff".into());
        }
        if !request["rewardCutoff"].is_null()
            && (!pt
                || master::number(&request["k"], 1.0) != 1.0
                || !request["rewardCutoff"].as_f64().is_some_and(f64::is_finite))
        {
            return Err("invalid-reward-cutoff".into());
        }
        if pt {
            let issues = crate::pt::validate(&self.master, request)?;
            if !issues.is_empty() {
                return Err(format!("pt-input:{}", issues.join(",")));
            }
        }
        if kind == "plan" {
            return self.plan(input, started, now);
        }
        let songs = if kind == "power" || kind == "potential" {
            vec![request["goal"]["song"].clone()]
        } else {
            master::rows(&request["goal"]["songs"])
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
        };
        let songs = if songs.is_empty() && kind == "event" && request["goal"]["route"] == "skip" {
            vec![Value::Null]
        } else {
            songs
        };
        if !["power", "potential", "score", "gekiso", "event"].contains(&kind) {
            return Err(format!("unsupported-goal:{kind}"));
        }
        let deadline = request["deadline"].as_f64().unwrap_or_else(|| {
            request["timeLimitMs"]
                .as_f64()
                .map(|ms| started + ms)
                .unwrap_or(f64::INFINITY)
        });
        let generation = crate::cancellation_generation();
        let k = master::number(&request["k"], 1.0).max(1.0) as usize;
        let mut results = Vec::new();
        let mut overall = Vec::new();
        let mut unknown = Value::Null;
        for (index, song) in songs.iter().enumerate() {
            crate::progress::song(song);
            if let Some(done) = master::rows(&request["completed"])
                .into_iter()
                .find(|r| r["song"] == *song && r["proven"] == true)
            {
                results.push(done.clone());
                overall.extend(master::rows(&done["hits"]).into_iter().cloned());
                continue;
            }
            if now() >= deadline || generation != crate::cancellation_generation() {
                results.push(json!({"song":song,"hits":[],"proven":false,"bound":null,"stats":crate::search::Stats::default()}));
                continue;
            }
            let key = (
                master::int(&song["songId"]),
                master::int(&song["difficulty"]),
            );
            if let Some(entry) = master::rows(&input["charts"])
                .into_iter()
                .find(|r| r["song"] == *song)
            {
                let source = if entry["source"].is_null() {
                    crate::runtime_chart::convert(&entry["chart"])?
                } else {
                    serde_json::from_value(entry["source"].clone()).map_err(|e| e.to_string())?
                };
                self.insert_chart(key.0, key.1, source)?;
            }
            if self.charts.contains_key(&key) {
                self.chart_order.retain(|&old| old != key);
                self.chart_order.push_back(key);
            }
            crate::progress::chart(self.charts.get(&key), kind == "gekiso");
            let mut single = request.clone();
            single["goal"]["songs"] = json!([song]);
            let current = now();
            let song_deadline = (current
                + (deadline - current).max(0.0) / (songs.len() - index) as f64)
                .min(deadline);
            if song_deadline.is_finite() {
                single["deadline"] = json!(song_deadline);
            }
            let mut response = match kind {
                "power" | "potential" => self.search_power(&single, started, now)?,
                "score" => self.search_score(&single, started, now)?,
                "event" => {
                    if single["goal"]["gekiso"].is_null() {
                        self.search_event(&single, started, now)?
                    } else {
                        self.search_rewards(&single, started, now)?
                    }
                }
                "gekiso" => self.search_gekiso(
                    &single,
                    self.sources.get(&key).ok_or("missing-chart")?,
                    started,
                    now,
                )?,
                _ => unreachable!(),
            };
            normalize_response_hits(&mut response);
            apply_reward_cutoff(&mut response, request, song);
            unknown = response["unknownCards"].clone();
            results.extend(master::rows(&response["results"]).into_iter().cloned());
            overall.extend(master::rows(&response["overall"]).into_iter().cloned());
        }
        overall.sort_by(compare_hits);
        overall.truncate(k);
        let complete = results.iter().all(|row| {
            row["proven"] == true || !row["exclusion"].is_null() || !row["excludedBelow"].is_null()
        });
        Ok(
            json!({"results":results,"overall":overall,"unknownCards":unknown,"cachedCharts":self.chart_order.iter().map(|(s,d)|format!("{s}:{d}")).collect::<Vec<_>>(),"elapsedMs":now()-started,"stopReason":if complete{Some("complete")}else if generation!=crate::cancellation_generation(){Some("cancelled")}else{Some("budget")}}),
        )
    }
}

fn seed_teams(
    request: &Value,
    song: &Value,
    members: &[power::Member],
    snaps: &[power::Snap],
) -> Vec<crate::search::Team> {
    master::rows(&request["seeds"])
        .into_iter()
        .filter(|h| h["song"] == *song)
        .filter_map(|hit| {
            let members = master::rows(&hit["members"])
                .into_iter()
                .map(|v| members.iter().position(|m| v.as_str() == Some(&m.key)))
                .collect::<Option<Vec<_>>>()?;
            let snaps = master::rows(&hit["snaps"])
                .into_iter()
                .map(|v| {
                    if v.is_null() {
                        Some(-1)
                    } else {
                        snaps
                            .iter()
                            .position(|s| v.as_str() == Some(&s.key))
                            .map(|i| i as i32)
                    }
                })
                .collect::<Option<Vec<_>>>()?;
            if members.len() != 5 || snaps.len() != 5 {
                return None;
            }
            Some(crate::search::Team { members, snaps })
        })
        .collect()
}

impl Engine {
    pub fn search_rewards(
        &self,
        request: &Value,
        started: f64,
        now: &dyn Fn() -> f64,
    ) -> Result<Value, String> {
        use crate::{
            event::{BonusSurrogate, Route, RouteKind},
            full::play::Accuracy,
            rewards::RewardObjective,
            search::{self, Objective},
        };
        let generation = crate::cancellation_generation();
        let goal = &request["goal"];
        if goal["ranking"] != "pt-only" || goal["route"] != "live" {
            return Err("gekiso-reward-mode".into());
        }
        let issues = crate::pt::validate(&self.master, request)?;
        if !issues.is_empty() {
            return Err(format!("pt-input:{}", issues.join(",")));
        }
        let inputs: Vec<MemberInput> =
            serde_json::from_value(request["members"].clone()).map_err(|e| e.to_string())?;
        let snap_inputs: Vec<SnapInput> =
            serde_json::from_value(request["snaps"].clone()).map_err(|e| e.to_string())?;
        let cards = power::resolve_box(
            &self.master,
            &inputs,
            &snap_inputs,
            request["unknownPolicy"] != "min",
        );
        let player = power::player_state(&self.master, &request["player"]);
        let mut constraints = constraints(&request["constraints"], &cards.members, &cards.snaps);
        crate::progress::keys(&cards.members, &cards.snaps);
        let song = master::rows(&goal["songs"])
            .first()
            .copied()
            .ok_or("no Gekiso song")?;
        crate::progress::song(song);
        let id = master::int(&song["songId"]);
        let difficulty = master::int(&song["difficulty"]);
        let source = self.sources.get(&(id, difficulty)).ok_or("missing-chart")?;
        crate::pt::validate_chart(&self.master, id, difficulty, source)?;
        if source
            .enumeration
            .as_ref()
            .is_none_or(|rows| rows.is_empty())
            || source.fever_ms.len() != 3
        {
            return Err(format!("gekiso-chart-unsupported:{id}:{difficulty}"));
        }
        let chart = self.charts.get(&(id, difficulty)).ok_or("missing-chart")?;
        let music = power::music_view(&self.master, id, None)?;
        let route = Route::new(
            &self.master,
            master::int(&goal["eventId"]),
            RouteKind::Live,
            master::int(&goal["consumption"]),
        )?;
        let points = goal["measure"] != "items";
        let bonus_type = if points { 0 } else { 1 };
        let exchange = if goal["cpExchange"].is_null() {
            None
        } else {
            Some((
                master::number(&goal["cpExchange"]["numerator"], 0.0),
                master::number(&goal["cpExchange"]["denominator"], 1.0),
            ))
        };
        let accuracy = Accuracy {
            just: master::number(&goal["gekiso"]["just"], 1.0),
            ..Accuracy::default()
        };
        let mut gekiso = crate::gekiso::Chart::new(
            &self.master,
            source,
            chart.difficulty.play_level,
            chart.song.missions.clone(),
            accuracy,
            vec![1; 3],
        )?;
        gekiso.music_type = music.music_type;
        if chart.song.missions.iter().any(|m| *m == 1 || *m == 4)
            && master::int(&goal["gekiso"]["minComboBonus"]) > 1
        {
            let values: Vec<_> = crate::gekiso_objective::opening_combo_bonuses(
                &self.master,
                &cards.members,
                &chart.song.missions,
            )
            .into_iter()
            .map(|v| v as f64)
            .collect();
            let maximum = values.iter().copied().fold(0.0, f64::max);
            constraints.member_floor = Some(search::MemberFloor {
                values,
                minimum: (master::int(&goal["gekiso"]["minComboBonus"]) - 1) as f64,
                maximum,
            });
        }
        let mut objective = RewardObjective::new(
            &self.master,
            &cards.members,
            &cards.snaps,
            chart,
            gekiso,
            route.clone(),
            points,
            master::int(&goal["gekiso"]["luckSamples"]),
            exchange,
        );
        let deadline = request["deadline"].as_f64().unwrap_or_else(|| {
            request["timeLimitMs"]
                .as_f64()
                .map(|ms| started + ms)
                .unwrap_or(f64::INFINITY)
        });
        let shard = if request["shard"].is_null() {
            None
        } else {
            Some((
                master::int(&request["shard"]["index"]) as usize,
                master::int(&request["shard"]["count"]) as usize,
            ))
        };
        let phase = request["rewardPhase"]["kind"].as_str();
        if phase.is_some_and(|phase| !["primary", "other", "score"].contains(&phase))
            || phase.is_some()
                && (master::int(&request["k"]).max(1) != 1
                    || chart.song.missions.iter().any(|&mission| mission != 1))
        {
            return Err("gekiso-reward-phase".into());
        }
        let cache_key = phase.map(|_| {
            json!([
                request["members"],
                request["snaps"],
                request["player"],
                request["unknownPolicy"],
                goal
            ])
        });
        if let Some(key) = &cache_key {
            let mut cache = self
                .reward_cache
                .borrow_mut()
                .take()
                .filter(|(old, _)| old == key)
                .map(|(_, cache)| cache)
                .unwrap_or_default();
            objective.swap_settlements(&mut cache);
        }
        let response = |output: search::Output, objective: &mut RewardObjective<'_>| {
            if let Some(key) = &cache_key {
                let mut cache = crate::rewards::SettlementCache::default();
                objective.swap_settlements(&mut cache);
                self.reward_cache.replace(Some((key.clone(), cache)));
            }
            let hits:Vec<_>=output.hits.iter().map(|hit|{let d=&hit.detail;let settlement=&d["settlement"];let count=master::number(&settlement["observations"],1.0);let cp=master::number(&settlement["cpSum"],0.0)/count;let mut hit=hit_json(hit,song,&cards.members,&cards.snaps,Some(chart),true);hit["score"]["ranks"]=settlement["ranks"].clone();hit["comboBonus"]=d["comboBonus"].clone();hit["gekiso"]=json!({"just":d["just"],"luckSamples":d["luckSamples"],"sampled":d["sampled"],"rank":1});hit["event"]=json!({"mean":master::number(&settlement["mean"],0.0)/exchange.map(|(_,d)|d).unwrap_or(1.0),"bonusPercent":master::number(&d["bonus"],0.0)/100.0,"challengePoints":cp,"perPlayMin":settlement["directMin"],"perPlayMax":settlement["directMax"],"rewardSum":settlement["rewardSum"],"comparisonSum":settlement["comparisonSum"],"orders":settlement["observations"],"directMean":master::number(&settlement["rewardSum"],0.0)/count,"otherMean":settlement["otherMean"],"convertedMean":exchange.map(|(n,d)|cp*n/d).unwrap_or(0.0)});hit}).collect();
            let mut value = json!({"results":[{"song":song,"hits":hits,"proven":output.proven,"bound":output.bound,"stats":output.stats}],"overall":hits,"unknownCards":cards.unknown,"elapsedMs":now()-started,"stopReason":if generation!=crate::cancellation_generation(){Some("cancelled")}else if now()>=deadline{Some("budget")}else{None}});
            if let Some(phase) = phase {
                value["results"][0]["rewardPhase"] = json!(phase);
            }
            value
        };
        let bonus_probe = |team: &search::Team| -> Result<search::Output, String> {
            let mut probe_constraints = constraints.clone();
            probe_constraints.min_bonus = None;
            probe_constraints.max_bonus = None;
            if points {
                probe_constraints.required_members = team.members.clone();
            } else {
                probe_constraints.required_snaps = team
                    .snaps
                    .iter()
                    .filter(|&&snap| snap >= 0)
                    .map(|&snap| snap as usize)
                    .collect();
            }
            let mut surrogate = BonusSurrogate {
                members: cards
                    .members
                    .iter()
                    .map(|member| {
                        power::member_event_percent(&route.effects, member, 1 - bonus_type) as f64
                    })
                    .collect(),
                snaps: cards
                    .snaps
                    .iter()
                    .map(|snap| {
                        power::snap_event_percent(&route.effects, snap, 1 - bonus_type) as f64
                    })
                    .collect(),
                weight: 16777216.0,
            };
            search::search_at(
                search::Input {
                    master: &self.master,
                    player: &player,
                    members: &cards.members,
                    snaps: &cards.snaps,
                    music: Some(&music),
                    objective: &mut surrogate,
                    constraints: probe_constraints,
                    k: 1,
                    deadline,
                    now,
                    seeds: Vec::new(),
                    shard,
                    owns: None,
                    floor: f64::NEG_INFINITY,
                    floor_exclusive: false,

                    streaming: true,
                    disable_snap_dominance: false,
                    member_power_percent: None,
                    snap_power_percent: None,
                    on_candidate: None,
                    progress: None,
                },
                generation,
            )
        };
        if phase.is_some_and(|phase| phase != "primary") {
            let target = request["rewardPhase"]["reward"]
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or("missing-reward-target")?;
            if phase == Some("other") {
                objective.rank_other_at_reward(target, true);
            } else {
                let other = request["rewardPhase"]["other"]
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or("missing-other-reward-target")?;
                objective.rank_score_at_reward(target, other, true);
            }
            let mut seeds = seed_teams(request, song, &cards.members, &cards.snaps);
            let mut warm_stats = search::Stats::default();
            if phase == Some("other") {
                if let Some(best) = seeds.first() {
                    let probe = bonus_probe(best)?;
                    warm_stats.add(&probe.stats);
                    seeds.extend(probe.hits.into_iter().map(|hit| hit.team));
                }
            }
            let mut publish = |hits: &[search::Hit]| {
                let mut hits: Vec<_> = hits.iter().filter(|hit| hit.key >= 0.0).cloned().collect();
                for hit in &mut hits {
                    hit.key = target;
                }
                crate::progress::emit(&hits);
            };
            let mut output = search::search_at(
                search::Input {
                    master: &self.master,
                    player: &player,
                    members: &cards.members,
                    snaps: &cards.snaps,
                    music: Some(&music),
                    objective: &mut objective,
                    constraints,
                    k: 1,
                    deadline,
                    now,
                    seeds,
                    shard,
                    owns: None,
                    floor: master::number(&request["rewardPhase"]["floor"], 0.0),
                    floor_exclusive: false,
                    streaming: true,
                    disable_snap_dominance: false,
                    member_power_percent: None,
                    snap_power_percent: None,
                    on_candidate: Some(&mut publish),
                    progress: None,
                },
                generation,
            )?;
            output.stats.add(&warm_stats);
            output.hits.retain(|hit| hit.key >= 0.0);
            for hit in &mut output.hits {
                hit.key = target;
            }
            return Ok(response(output, &mut objective));
        }
        let mut surrogate = BonusSurrogate {
            members: cards
                .members
                .iter()
                .map(|m| power::member_event_percent(&route.effects, m, bonus_type) as f64)
                .collect(),
            snaps: cards
                .snaps
                .iter()
                .map(|s| power::snap_event_percent(&route.effects, s, bonus_type) as f64)
                .collect(),
            weight: 16777216.0,
        };
        let mut seeds = seed_teams(request, song, &cards.members, &cards.snaps);
        let mut probe = if request["seeded"] == true && !seeds.is_empty() {
            search::Output {
                hits: Vec::new(),
                proven: false,
                bound: None,
                stats: search::Stats::default(),
            }
        } else {
            search::search_at(
                search::Input {
                    master: &self.master,
                    player: &player,
                    members: &cards.members,
                    snaps: &cards.snaps,
                    music: Some(&music),
                    objective: &mut surrogate,
                    constraints: constraints.clone(),
                    k: 1,
                    deadline,
                    now,
                    seeds: Vec::new(),
                    shard: None,
                    owns: None,
                    floor: f64::NEG_INFINITY,
                    floor_exclusive: false,

                    streaming: true,
                    disable_snap_dominance: false,
                    member_power_percent: None,
                    snap_power_percent: None,
                    on_candidate: None,
                    progress: None,
                },
                generation,
            )?
        };
        seeds.extend(probe.hits.iter().map(|h| h.team.clone()));
        let mut best = f64::NEG_INFINITY;
        for hit in &probe.hits {
            best = best.max(objective.exact(&hit.team, hit.power, &hit.slot_powers)?.key);
        }
        let ceiling = objective.reward_ceiling();
        if best < ceiling {
            if let Some(hit) = probe.hits.first() {
                let mut proposal_constraints = constraints.clone();
                proposal_constraints.min_bonus = None;
                proposal_constraints.max_bonus = None;
                if points {
                    proposal_constraints.required_members = hit.team.members.clone();
                } else {
                    proposal_constraints.required_snaps = hit
                        .team
                        .snaps
                        .iter()
                        .filter(|&&snap| snap >= 0)
                        .map(|&snap| snap as usize)
                        .collect();
                }
                let mut secondary = BonusSurrogate {
                    members: cards
                        .members
                        .iter()
                        .map(|member| {
                            power::member_event_percent(&route.effects, member, 1 - bonus_type)
                                as f64
                        })
                        .collect(),
                    snaps: cards
                        .snaps
                        .iter()
                        .map(|snap| {
                            power::snap_event_percent(&route.effects, snap, 1 - bonus_type) as f64
                        })
                        .collect(),
                    weight: 16777216.0,
                };
                let proposal = search::search_at(
                    search::Input {
                        master: &self.master,
                        player: &player,
                        members: &cards.members,
                        snaps: &cards.snaps,
                        music: Some(&music),
                        objective: &mut secondary,
                        constraints: proposal_constraints,
                        k: 1,
                        deadline,
                        now,
                        seeds: Vec::new(),
                        shard: None,
                        owns: None,
                        floor: f64::NEG_INFINITY,
                        floor_exclusive: false,
                        streaming: true,
                        disable_snap_dominance: false,
                        member_power_percent: None,
                        snap_power_percent: None,
                        on_candidate: None,
                        progress: None,
                    },
                    generation,
                )?;
                probe.stats.add(&proposal.stats);
                for hit in proposal.hits {
                    if seeds.iter().any(|team| {
                        team.members == hit.team.members && team.snaps == hit.team.snaps
                    }) {
                        continue;
                    }
                    let bonus = hit
                        .team
                        .members
                        .iter()
                        .map(|&m| objective.member_bonus(m))
                        .sum::<f64>()
                        + hit
                            .team
                            .snaps
                            .iter()
                            .filter(|&&s| s >= 0)
                            .map(|&s| objective.snap_bonus(s as usize))
                            .sum::<f64>();
                    if constraints
                        .min_bonus
                        .is_some_and(|minimum| bonus < minimum - 1e-6)
                        || constraints
                            .max_bonus
                            .is_some_and(|maximum| bonus > maximum + 1e-6)
                    {
                        continue;
                    }
                    best = best.max(objective.exact(&hit.team, hit.power, &hit.slot_powers)?.key);
                    seeds.push(hit.team);
                }
            }
        }
        if best < ceiling && !probe.hits.is_empty() {
            for weight in [8.0, 16.0, 32.0, 64.0, 128.0] {
                if now() >= deadline || generation != crate::cancellation_generation() {
                    break;
                }
                surrogate.weight = weight;
                let proposal = search::search_at(
                    search::Input {
                        master: &self.master,
                        player: &player,
                        members: &cards.members,
                        snaps: &cards.snaps,
                        music: Some(&music),
                        objective: &mut surrogate,
                        constraints: constraints.clone(),
                        k: 1,
                        deadline,
                        now,
                        seeds: Vec::new(),
                        shard,
                        owns: None,
                        floor: f64::NEG_INFINITY,
                        floor_exclusive: false,

                        streaming: true,
                        disable_snap_dominance: false,
                        member_power_percent: None,
                        snap_power_percent: None,
                        on_candidate: None,
                        progress: None,
                    },
                    generation,
                )?;
                probe.stats.add(&proposal.stats);
                for hit in proposal.hits {
                    if seeds.iter().any(|team| {
                        team.members == hit.team.members && team.snaps == hit.team.snaps
                    }) {
                        continue;
                    }
                    let value = objective.exact(&hit.team, hit.power, &hit.slot_powers)?.key;
                    best = best.max(value);
                    seeds.push(hit.team);
                }
                if best >= ceiling {
                    break;
                }
            }
        }
        let song_key = format!("{id}:{difficulty}");
        let floor = request["cutoff"]["key"].as_f64().unwrap_or_else(|| {
            master::number(&request["floors"][&song_key], f64::NEG_INFINITY)
                .max(master::number(&request["rewardCutoff"], f64::NEG_INFINITY))
        });
        let single_lex = master::int(&request["k"]).max(1) == 1 && objective.enable_lexicographic();
        let mut output = search::search_at(
            search::Input {
                master: &self.master,
                player: &player,
                members: &cards.members,
                snaps: &cards.snaps,
                music: Some(&music),
                objective: &mut objective,
                constraints: constraints.clone(),
                k: 1,
                deadline,
                now,
                seeds,
                shard,
                owns: None,
                floor,
                floor_exclusive: request["cutoff"]["comparison"] == "gt",

                streaming: true,
                disable_snap_dominance: false,
                member_power_percent: None,
                snap_power_percent: None,
                on_candidate: Some(&mut crate::progress::emit),
                progress: None,
            },
            generation,
        )?;
        if let Some(target) = output
            .hits
            .first()
            .map(|hit| hit.key)
            .filter(|_| !single_lex && phase.is_none())
        {
            objective.rank_other_at_reward(target, output.proven);
            let baseline = output.hits.clone();
            let mut publish = |hits: &[search::Hit]| {
                crate::progress::emit(&merge_reward_candidates(&baseline, hits, target, 1, true));
            };
            let mut secondary = if constraints.min_bonus.is_none()
                && constraints.max_bonus.is_none()
                && !chart.song.missions.iter().all(|&mission| mission == 1)
                && !chart.song.missions.iter().all(|&mission| mission == 3)
            {
                let mut chart = crate::gekiso::Chart::new(
                    &self.master,
                    source,
                    chart.difficulty.play_level,
                    chart.song.missions.clone(),
                    Accuracy {
                        just: master::number(&goal["gekiso"]["just"], 1.0),
                        ..Accuracy::default()
                    },
                    vec![1; 3],
                )?;
                chart.music_type = music.music_type;
                let mut secondary = RewardObjective::new(
                    &self.master,
                    &cards.members,
                    &cards.snaps,
                    self.charts.get(&(id, difficulty)).unwrap(),
                    chart,
                    route.clone(),
                    !points,
                    master::int(&goal["gekiso"]["luckSamples"]),
                    None,
                );
                secondary.constrain_other(target, exchange, output.proven);
                Some(secondary)
            } else {
                None
            };
            if let Some(secondary) = &mut secondary {
                objective.swap_context_cache(secondary);
            }
            let mapper = secondary.as_ref().map(RewardObjective::reward_mapper);
            let mut tie_seeds: Vec<_> = output.hits.iter().map(|hit| hit.team.clone()).collect();
            if let Some(best) = output.hits.first() {
                let probe = bonus_probe(&best.team)?;
                output.stats.add(&probe.stats);
                tie_seeds.extend(probe.hits.into_iter().map(|hit| hit.team));
            }
            let mut publish_secondary = |hits: &[search::Hit]| {
                let mut hits = hits.to_vec();
                if let Some(mapper) = &mapper {
                    for hit in &mut hits {
                        if hit.key >= 0.0 {
                            mapper(hit);
                        }
                    }
                }
                crate::progress::emit(&merge_reward_candidates(&baseline, &hits, target, 1, true));
            };
            let mut tie = search::search_at(
                search::Input {
                    master: &self.master,
                    player: &player,
                    members: &cards.members,
                    snaps: &cards.snaps,
                    music: Some(&music),
                    objective: secondary.as_mut().unwrap_or(&mut objective),
                    constraints: constraints.clone(),
                    k: 1,
                    deadline,
                    now,
                    seeds: tie_seeds,
                    shard,
                    owns: None,
                    floor: 0.0,
                    floor_exclusive: false,

                    streaming: true,
                    disable_snap_dominance: false,
                    member_power_percent: None,
                    snap_power_percent: None,
                    on_candidate: Some(&mut publish_secondary),
                    progress: None,
                },
                generation,
            )?;
            if let Some(secondary) = &mut secondary {
                objective.swap_context_cache(secondary);
            }
            if let Some(mapper) = &mapper {
                for hit in &mut tie.hits {
                    if hit.key >= 0.0 {
                        mapper(hit);
                    }
                }
            }
            output.stats.add(&tie.stats);
            if !tie.proven {
                output.proven = false;
                output.bound = Some(output.bound.unwrap_or(f64::NEG_INFINITY).max(target));
            }
            output.hits = score_tied_hits(&output.hits, tie.hits, target, 1, true);
            if let Some(other) = output
                .hits
                .first()
                .map(|h| master::number(&h.detail["settlement"]["otherMean"], 0.0))
            {
                objective.rank_score_at_reward(target, other, output.proven);
                let tie = search::search_at(
                    search::Input {
                        master: &self.master,
                        player: &player,
                        members: &cards.members,
                        snaps: &cards.snaps,
                        music: Some(&music),
                        objective: &mut objective,
                        constraints: constraints.clone(),
                        k: 1,
                        deadline,
                        now,
                        seeds: output.hits.iter().map(|hit| hit.team.clone()).collect(),
                        shard,
                        owns: None,
                        floor: 0.0,
                        floor_exclusive: false,

                        streaming: true,
                        disable_snap_dominance: false,
                        member_power_percent: None,
                        snap_power_percent: None,
                        on_candidate: Some(&mut publish),
                        progress: None,
                    },
                    generation,
                )?;
                output.stats.add(&tie.stats);
                if !tie.proven {
                    output.proven = false;
                    output.bound = Some(output.bound.unwrap_or(f64::NEG_INFINITY).max(target));
                }
                output.hits = score_tied_hits(&output.hits, tie.hits, target, 1, true);
            }
        }
        Ok(response(output, &mut objective))
    }
}
fn hit_json(
    hit: &crate::search::Hit,
    song: &Value,
    members: &[power::Member],
    snaps: &[power::Snap],
    chart: Option<&Chart>,
    battle: bool,
) -> Value {
    let mut value = json!({"song":song,"members":hit.team.members.iter().map(|&i|&members[i].key).collect::<Vec<_>>(),"snaps":hit.team.snaps.iter().map(|&j|if j<0{None}else{Some(&snaps[j as usize].key)}).collect::<Vec<_>>(),"power":hit.power,"slotPowers":hit.slot_powers,"key":hit.key,"score":null,"skipScore":null,"event":null,"potential":null});
    if let Some(chart) = chart.filter(|_| !hit.detail["scores"].is_null()) {
        let scores = &hit.detail["scores"];
        let values: Vec<_> = master::rows(&scores["scores"])
            .into_iter()
            .map(|v| master::number(v, 0.0))
            .collect();
        let mut sorted = values.clone();
        sorted.sort_by(f64::total_cmp);
        let mut ranks = BTreeMap::<i32, i32>::new();
        for &v in &values {
            let rank = if battle {
                chart::battle_rank(chart, v)
            } else {
                chart::score_rank(chart, v)
            };
            *ranks.entry(rank).or_default() += 1;
        }
        let keys = |order: usize| {
            live::orders()[order].map(|slot| members[hit.team.members[slot]].key.clone())
        };
        value["score"] = json!({"mean":scores["mean"],"min":scores["min"],"max":scores["max"],"median":(sorted[59]+sorted[60])/2.0,"orders":values.iter().map(|v|crate::num::round(*v)).collect::<Vec<_>>(),"bestOrder":keys(master::int(&scores["maxOrder"]) as usize),"worstOrder":keys(master::int(&scores["minOrder"]) as usize),"ranks":ranks});
    }
    value
}

impl Engine {
    fn load_source(&mut self, input: &Value, song: &Value) -> Result<(), String> {
        let key = (
            master::int(&song["songId"]),
            master::int(&song["difficulty"]),
        );
        if let Some(entry) = master::rows(&input["charts"])
            .into_iter()
            .find(|r| r["song"] == *song)
        {
            let source = if entry["source"].is_null() {
                crate::runtime_chart::convert(&entry["chart"])?
            } else {
                serde_json::from_value(entry["source"].clone()).map_err(|e| e.to_string())?
            };
            self.insert_chart(key.0, key.1, source)?;
        } else if self.charts.contains_key(&key) {
            self.chart_order.retain(|&old| old != key);
            self.chart_order.push_back(key);
        } else {
            return Err(format!("missing-chart:{}:{}", key.0, key.1));
        }
        Ok(())
    }
    pub fn plan(
        &mut self,
        input: &Value,
        started: f64,
        now: &dyn Fn() -> f64,
    ) -> Result<Value, String> {
        use crate::{
            event::{EventObjective, Route, RouteKind},
            objectives::{Criterion, LiveCore, LiveObjective, ScoreCap},
            plan::{Finite, Settlement, Tie},
            search,
        };
        let request = &input["request"];
        let goal = &request["goal"];
        let deadline = request["deadline"].as_f64().unwrap_or_else(|| {
            request["timeLimitMs"]
                .as_f64()
                .map(|ms| started + ms)
                .unwrap_or(f64::INFINITY)
        });
        let generation = crate::cancellation_generation();
        let k = master::number(&request["k"], 1.0).max(1.0) as usize;
        let normal_songs = master::rows(&goal["normalSongs"]);
        let challenge_songs = master::rows(&goal["challengeSongs"]);
        let mut challenge = json!({"results":[],"overall":[],"unknownCards":[]});
        if goal["challengePointsPerPlay"].is_null() {
            let mut trial = input.clone();
            trial["request"]["goal"] = json!({"kind":"event","measure":"points","route":"challenge","eventId":goal["eventId"],"songs":challenge_songs,"consumption":goal["challengePointsPerLive"],"play":goal["play"]});
            if deadline.is_finite() {
                trial["request"]["deadline"] = json!(deadline);
            }
            challenge = self.run(&trial, started, now)?;
        }
        if master::rows(&challenge["results"])
            .into_iter()
            .any(|r| r["proven"] != true)
        {
            let mut results: Vec<_> = master::rows(&challenge["results"])
                .into_iter()
                .cloned()
                .collect();
            results.extend(normal_songs.iter().map(|song|json!({"song":song,"hits":[],"proven":false,"bound":null,"stats":search::Stats::default()})));
            return Ok(
                json!({"results":results,"overall":challenge["overall"],"unknownCards":challenge["unknownCards"],"elapsedMs":now()-started,"stopReason":if generation!=crate::cancellation_generation(){"cancelled"}else{"budget"},"cachedCharts":self.cached_chart_keys()}),
            );
        }
        let best_challenge = master::rows(&challenge["overall"])
            .first()
            .copied()
            .cloned()
            .unwrap_or(Value::Null);
        let points_per_challenge = master::number(
            &goal["challengePointsPerPlay"],
            master::number(&best_challenge["event"]["mean"], 0.0),
        );
        let cp_cost = master::int(&goal["challengePointsPerLive"]) as i64;
        let members: Vec<MemberInput> =
            serde_json::from_value(request["members"].clone()).map_err(|e| e.to_string())?;
        let snaps: Vec<SnapInput> =
            serde_json::from_value(request["snaps"].clone()).map_err(|e| e.to_string())?;
        let cards = power::resolve_box(
            &self.master,
            &members,
            &snaps,
            request["unknownPolicy"] != "min",
        );
        let player = power::player_state(&self.master, &request["player"]);
        let constraints = constraints(&request["constraints"], &cards.members, &cards.snaps);
        let route = Route::new(
            &self.master,
            master::int(&goal["eventId"]),
            RouteKind::Live,
            master::int(&goal["boostsPerLive"]),
        )?;
        let boosts = master::int(&goal["boostsPerLive"]) as u64;
        let boost_budget = master::int(&goal["boostBudget"]) as u64;
        let lives = boost_budget.checked_div(boosts).unwrap_or(boost_budget);
        let settlement = Settlement::new(
            lives,
            master::int(&goal["startingChallengePoints"]) as u64,
            cp_cost as u64,
            points_per_challenge,
            &route,
        );
        let weight = settlement.weight();
        crate::progress::keys(&cards.members, &cards.snaps);
        let mut normal = Vec::new();
        let mut settlements = BTreeMap::new();
        let mut progress_goal = goal.clone();
        progress_goal["planWeight"] = json!(weight);
        crate::progress::mode(&progress_goal);
        for (index, song) in normal_songs.iter().enumerate() {
            crate::progress::song(song);
            if now() >= deadline || generation != crate::cancellation_generation() {
                normal.extend(normal_songs[index..].iter().map(|song|json!({"song":song,"hits":[],"proven":false,"bound":null,"stats":search::Stats::default()})));
                break;
            }
            self.load_source(input, song)?;
            let id = master::int(&song["songId"]);
            let difficulty = master::int(&song["difficulty"]);
            let chart = self.charts.get(&(id, difficulty)).unwrap();
            let music = power::music_view(&self.master, id, None)?;
            crate::progress::chart(Some(chart), false);
            let budget = (deadline - now()).max(0.0) / (normal_songs.len() - index).max(1) as f64;
            let mut cap = ScoreCap {
                core: LiveCore::new(
                    &self.master,
                    chart,
                    &cards.members,
                    &cards.snaps,
                    &goal["play"],
                ),
            };
            let run = |objective: &mut dyn search::Objective,
                       search_k,
                       limit,
                       notify,
                       seeds,
                       floor,
                       reward_group: Option<(&[search::Hit], f64)>| {
                let mut publish = |hits: &[search::Hit]| {
                    if let Some((original, key)) = reward_group {
                        crate::progress::emit(&merge_reward_candidates(
                            original, hits, key, k, false,
                        ));
                    } else {
                        crate::progress::emit(hits);
                    }
                };
                search::search(search::Input {
                    master: &self.master,
                    player: &player,
                    members: &cards.members,
                    snaps: &cards.snaps,
                    music: Some(&music),
                    objective,
                    constraints: constraints.clone(),
                    k: search_k,
                    deadline: limit,
                    now,
                    seeds,
                    shard: None,
                    owns: None,
                    floor,
                    floor_exclusive: false,

                    streaming: true,
                    disable_snap_dominance: false,
                    member_power_percent: None,
                    snap_power_percent: None,
                    on_candidate: if notify { Some(&mut publish) } else { None },
                    progress: None,
                })
            };
            let best = run(
                &mut cap,
                1,
                (now() + budget / 4.0).min(deadline),
                false,
                Vec::new(),
                f64::NEG_INFINITY,
                None,
            )?;
            let top = best.hits.first().map(|h| h.key).unwrap_or(f64::INFINITY);
            let score_cap = if best.proven {
                top
            } else {
                top.max(best.bound.unwrap_or(f64::INFINITY))
            };
            let make_event = || Finite {
                event: EventObjective::new(
                    &self.master,
                    &cards.members,
                    &cards.snaps,
                    Some(chart),
                    route.clone(),
                    true,
                    &goal["play"],
                    true,
                    weight,
                    None,
                    score_cap,
                ),
                settlement: std::rc::Rc::clone(&settlement),
            };
            let mut objective = make_event();
            let song_deadline = (now() + budget).min(deadline);
            let mut output = run(
                &mut objective,
                k,
                song_deadline,
                true,
                Vec::new(),
                f64::NEG_INFINITY,
                None,
            )?;
            let mut keys: Vec<_> = output.hits.iter().map(|hit| hit.key).collect();
            keys.sort_by(|a, b| b.total_cmp(a));
            keys.dedup();
            let mut ordered: Vec<search::Hit> = Vec::new();
            for key in keys {
                if ordered.len() >= k {
                    break;
                }
                let mut tie = Tie {
                    finite: make_event(),
                    mean: LiveObjective {
                        core: LiveCore::new(
                            &self.master,
                            chart,
                            &cards.members,
                            &cards.snaps,
                            &goal["play"],
                        ),
                        criterion: Criterion::Mean,
                        target: None,
                    },
                    target: key,
                    excluded: ordered.iter().map(|hit| hit.team.members.clone()).collect(),
                    allow_dominance: output.proven,
                };
                let baseline: Vec<_> = output.hits.iter().chain(&ordered).cloned().collect();
                let reranked = run(
                    &mut tie,
                    k - ordered.len(),
                    song_deadline,
                    true,
                    output
                        .hits
                        .iter()
                        .filter(|hit| hit.key == key)
                        .map(|hit| hit.team.clone())
                        .collect(),
                    0.0,
                    Some((&baseline, key)),
                )?;
                output.stats.add(&reranked.stats);
                if !reranked.proven {
                    output.proven = false;
                    output.bound = Some(output.bound.unwrap_or(f64::NEG_INFINITY).max(key));
                }
                ordered.extend(score_tied_hits(
                    &output.hits,
                    reranked.hits,
                    key,
                    k - ordered.len(),
                    false,
                ));
            }
            output.hits = ordered;
            let hits: Vec<_> = output
                .hits
                .iter()
                .map(|hit| {
                    let mut value =
                        hit_json(hit, song, &cards.members, &cards.snaps, Some(chart), false);
                    let detail = &hit.detail;
                    value["event"] = json!({
                        "mean":detail["directMean"],
                        "bonusPercent":master::number(&detail["bonus"], 0.0)/100.0,
                        "challengePoints":detail["challengePoints"],
                        "perPlayMin":detail["directMin"],
                        "perPlayMax":detail["directMax"],
                    });
                    let identity = json!([song, value["members"], value["snaps"]]).to_string();
                    settlements.insert(identity, detail["plan"].clone());
                    value
                })
                .collect();
            normal.push(json!({"song":song,"hits":hits,"proven":output.proven,"bound":output.bound,"stats":output.stats}));
        }
        let best_normal = master::rows(&json!(normal))
            .into_iter()
            .flat_map(|r| master::rows(&r["hits"]))
            .cloned()
            .max_by(|a, b| compare_hits(b, a))
            .unwrap_or(Value::Null);
        let feasible = !best_normal.is_null();
        let identity = json!([
            best_normal["song"],
            best_normal["members"],
            best_normal["snaps"]
        ])
        .to_string();
        let totals = settlements.get(&identity).cloned().unwrap_or(Value::Null);
        let points = master::number(&totals["eventPoints"], 0.0);
        let per_boost = if boost_budget != 0 {
            points
                / (if boosts > 0 {
                    lives * boosts
                } else {
                    boost_budget
                })
                .max(1) as f64
        } else {
            0.0
        };
        let plan = json!({"normal":best_normal,"challenge":best_challenge,"normalLives":lives,"challengeLives":totals["challengeLives"],"challengePointsEarned":totals["challengePointsEarned"],"leftoverChallengePoints":totals["leftoverChallengePoints"],"eventPoints":points,"perBoost":per_boost});
        let mut results: Vec<_> = master::rows(&challenge["results"])
            .into_iter()
            .cloned()
            .collect();
        results.extend(normal);
        let overall: Vec<_> = [best_normal, best_challenge]
            .into_iter()
            .filter(|v| !v.is_null())
            .collect();
        let mut response = json!({"results":results,"overall":overall,"unknownCards":if !goal["challengePointsPerPlay"].is_null(){json!(cards.unknown)}else{challenge["unknownCards"].clone()},"elapsedMs":now()-started,"cachedCharts":self.cached_chart_keys()});
        if feasible {
            response["plan"] = plan;
        }
        Ok(response)
    }
    fn cached_chart_keys(&self) -> Vec<String> {
        self.chart_order
            .iter()
            .map(|(s, d)| format!("{s}:{d}"))
            .collect()
    }
}
fn score_tied_hits(
    original: &[crate::search::Hit],
    reranked: Vec<crate::search::Hit>,
    target: f64,
    k: usize,
    secondary: bool,
) -> Vec<crate::search::Hit> {
    let group: Vec<_> = original
        .iter()
        .filter(|h| h.key == target)
        .cloned()
        .collect();
    merge_reward_candidates(&group, &reranked, target, k, secondary)
}

fn merge_reward_candidates(
    original: &[crate::search::Hit],
    reranked: &[crate::search::Hit],
    target: f64,
    k: usize,
    secondary: bool,
) -> Vec<crate::search::Hit> {
    let mut hits: Vec<_> = original
        .iter()
        .cloned()
        .chain(
            reranked
                .iter()
                .filter(|h| h.key >= 0.0)
                .cloned()
                .map(|mut hit| {
                    hit.key = target;
                    hit
                }),
        )
        .collect();
    hits.sort_by(|a, b| {
        b.key
            .total_cmp(&a.key)
            .then_with(|| {
                if secondary {
                    master::number(
                        &b.detail["otherMean"],
                        master::number(&b.detail["settlement"]["otherMean"], 0.0),
                    )
                    .total_cmp(&master::number(
                        &a.detail["otherMean"],
                        master::number(&a.detail["settlement"]["otherMean"], 0.0),
                    ))
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then_with(|| {
                master::number(&b.detail["scores"]["mean"], 0.0)
                    .total_cmp(&master::number(&a.detail["scores"]["mean"], 0.0))
            })
            .then_with(|| a.team.members.cmp(&b.team.members))
            .then_with(|| a.team.snaps.cmp(&b.team.snaps))
    });
    let mut seen = std::collections::BTreeSet::new();
    hits.into_iter()
        .filter(|hit| {
            let mut key = hit.team.members.clone();
            key.sort_unstable();
            seen.insert(key)
        })
        .take(k)
        .collect()
}

fn compare_hits(a: &Value, b: &Value) -> std::cmp::Ordering {
    master::number(&b["key"], 0.0)
        .total_cmp(&master::number(&a["key"], 0.0))
        .then_with(|| {
            master::number(&b["event"]["otherMean"], 0.0)
                .total_cmp(&master::number(&a["event"]["otherMean"], 0.0))
        })
        .then_with(|| {
            master::number(&b["score"]["mean"], 0.0)
                .total_cmp(&master::number(&a["score"]["mean"], 0.0))
        })
        .then_with(|| master::int(&a["song"]["songId"]).cmp(&master::int(&b["song"]["songId"])))
        .then_with(|| {
            master::int(&a["song"]["difficulty"]).cmp(&master::int(&b["song"]["difficulty"]))
        })
        .then_with(|| a["members"].to_string().cmp(&b["members"].to_string()))
        .then_with(|| a["snaps"].to_string().cmp(&b["snaps"].to_string()))
}

impl Engine {
    pub fn evaluate_all(&mut self, input: &Value) -> Result<Value, String> {
        let request = &input["request"];
        let mut out = Vec::new();
        for song in master::rows(&request["songs"]) {
            self.load_source(input, song)?;
            out.push(self.evaluate(
                request,
                &request["team"],
                master::int(&song["songId"]),
                master::int(&song["difficulty"]),
            )?);
        }
        Ok(json!({"evaluations":out,"cachedCharts":self.cached_chart_keys()}))
    }
    pub fn timeline(&mut self, input: &Value) -> Result<Value, String> {
        let request = &input["request"];
        let song = &request["song"];
        self.load_source(input, song)?;
        let chart = self
            .charts
            .get(&(
                master::int(&song["songId"]),
                master::int(&song["difficulty"]),
            ))
            .unwrap();
        let members: Vec<MemberInput> =
            serde_json::from_value(request["members"].clone()).map_err(|e| e.to_string())?;
        let snaps: Vec<SnapInput> =
            serde_json::from_value(request["snaps"].clone()).map_err(|e| e.to_string())?;
        let box_ = power::resolve_box(
            &self.master,
            &members,
            &snaps,
            request["unknownPolicy"] != "min",
        );
        let members = master::rows(&request["memberKeys"])
            .into_iter()
            .map(|key| {
                box_.members
                    .iter()
                    .find(|m| key.as_str() == Some(&m.key))
                    .ok_or("unknown-team-member")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let snaps: Vec<_> = master::rows(&request["snapKeys"])
            .into_iter()
            .map(|key| box_.snaps.iter().find(|s| key.as_str() == Some(&s.key)))
            .collect();
        let duration = chart.last_note_ms as i64 + 1000;
        if duration < 0 {
            return Err("invalid-chart-duration".into());
        }
        let mut density = vec![0; ((duration + 499) / 500) as usize];
        for &time in &chart.times {
            let bin = time / 500;
            if bin < 0 || bin as usize >= density.len() {
                return Err("invalid-chart-note-time".into());
            }
            density[bin as usize] += 1;
        }
        let mut windows = Vec::new();
        for (event, slot) in master::integers(&request["order"]).into_iter().enumerate() {
            if event >= chart.skill_times.len() {
                continue;
            }
            let member = members.get(slot as usize).ok_or("invalid-skill-order")?;
            let snap = snaps.get(slot as usize).ok_or("invalid-snap-slot")?;
            let skill = skills::resolve(&self.master, member, *snap, &mut Vec::new());
            let (mut percent, mut perfect, mut length) = (0.0, 0.0, 0i32);
            for effect in &skill.effects {
                if effect.gate.blocks(self.master.live.life_base)
                    && ["great", "good", "bad", "miss"]
                        .iter()
                        .all(|key| master::number(&request["play"][key], 0.0) == 0.0)
                {
                    continue;
                }
                if effect.kind == 2000 {
                    percent += effect.delta as f64 / 1000.0;
                } else {
                    perfect += effect.delta as f64 / 1000.0;
                }
                length = length.max(crate::num::window_ms(effect.seconds, skill.extension_ms));
            }
            let start = chart.skill_times[event];
            windows.push(json!({"member":member.key,"event":event,"startMs":start,"endMs":start as i64+length as i64,"percent":percent,"perfectPercent":perfect,"extensionMs":skill.extension_ms}));
        }
        Ok(
            json!({"durationMs":duration,"density":density,"fever":chart.fevers,"windows":windows,"cachedCharts":self.cached_chart_keys()}),
        )
    }
}

fn apply_reward_cutoff(response: &mut Value, request: &Value, song: &Value) {
    let cutoff = &request["cutoff"];
    let reward = request["rewardCutoff"].as_f64();
    if cutoff.is_null() && reward.is_none() {
        return;
    }
    let key = cutoff["key"].as_f64();
    let exclusive = cutoff["comparison"] == "gt";
    if let Some(results) = response["results"].as_array_mut() {
        for row in results {
            if let Some(hits) = row["hits"].as_array_mut() {
                hits.retain(|hit| {
                    let value = master::number(&hit["key"], 0.0);
                    key.is_none_or(|key| if exclusive { value > key } else { value >= key })
                        && reward.is_none_or(|key| value >= key)
                });
            }
            let empty = master::rows(&row["hits"]).is_empty();
            let bound = row["bound"].as_f64();
            if let Some(key) = key {
                if empty
                    && (row["proven"] == true
                        || bound.is_some_and(|b| if exclusive { b <= key } else { b < key }))
                {
                    row["exclusion"] = cutoff.clone();
                    row["proven"] = json!(false);
                    row["floor"] = Value::Null;
                }
            }
            if let Some(key) = reward {
                if empty && row["proven"] == true {
                    let song_key = format!(
                        "{}:{}",
                        master::int(&song["songId"]),
                        master::int(&song["difficulty"])
                    );
                    let excluded = key
                        .max(master::number(
                            &request["floors"][song_key],
                            f64::NEG_INFINITY,
                        ))
                        .max(master::number(&row["floor"], f64::NEG_INFINITY));
                    row["excludedBelow"] = json!(excluded);
                    row["proven"] = json!(false);
                    row["bound"] = json!(excluded);
                    row["floor"] = Value::Null;
                }
            }
        }
    }
    let mut hits: Vec<_> = master::rows(&response["results"])
        .into_iter()
        .flat_map(|r| master::rows(&r["hits"]))
        .cloned()
        .collect();
    hits.sort_by(compare_hits);
    hits.truncate(master::number(&request["k"], 1.0).max(1.0) as usize);
    response["overall"] = json!(hits);
}

fn normalize_response_hits(response: &mut Value) {
    for field in ["results", "overall"] {
        if field == "results" {
            if let Some(rows) = response[field].as_array_mut() {
                for row in rows {
                    if let Some(hits) = row["hits"].as_array_mut() {
                        for hit in hits {
                            normalize_hit(hit);
                        }
                    }
                }
            }
        } else if let Some(hits) = response[field].as_array_mut() {
            for hit in hits {
                normalize_hit(hit);
            }
        }
    }
}
pub(crate) fn normalize_hit(hit: &mut Value) {
    if let Some(object) = hit.as_object_mut() {
        for key in ["score", "skipScore", "event", "potential"] {
            object.entry(key).or_insert(Value::Null);
        }
    }
}
