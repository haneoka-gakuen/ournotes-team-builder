//! Equal-weight objectives retain one formation across every selected chart.
use super::*;
use crate::{
    event::{EventObjective, Route, RouteKind},
    objectives::{Criterion, LiveCore, LiveObjective},
    reward_plan::{Context, Fraction, Metric, Plan, Ranked},
    search::{self, Exact, Objective, Team, Totals},
};

struct SongObjective<'a> {
    objective: Box<dyn Objective + 'a>,
    music: power::Music,
    bases: Vec<power::SlotBase>,
    snap_power: Arc<Vec<f64>>,
    skills: Vec<(f64, f64)>,
    cap: f64,
    linear: Option<(f64, f64)>,
    unbounded: bool,
    slots: Vec<Vec<f64>>,
    opening: Vec<i32>,
}
struct Combined<'a> {
    master: &'a Master,
    player: &'a power::Player,
    members: &'a [power::Member],
    snaps: &'a [power::Snap],
    effects: &'a [master::EventEffect],
    songs: Vec<SongObjective<'a>>,
    options: Vec<Vec<i32>>,
    min_combo: i32,
    event: bool,
    plan: Option<Plan>,
    affine: Option<f64>,
}
impl Combined<'_> {
    fn eligible(&self, team: &Team) -> bool {
        self.songs.iter().all(|song| {
            song.opening.is_empty()
                || 1 + team.members.iter().map(|&m| song.opening[m]).sum::<i32>() >= self.min_combo
        })
    }
    fn upper(&mut self, members: &[usize], assigned: &[i32], from: usize) -> f64 {
        let width = self.snaps.len() + 1;
        let mut sum = 0.0_f64;
        for song in &mut self.songs {
            let mut t = Totals::default();
            for (slot, &m) in members.iter().enumerate() {
                let choices: Vec<_> = if slot < from {
                    vec![assigned[slot]]
                } else {
                    self.options[slot]
                        .iter()
                        .copied()
                        .filter(|&s| s < 0 || !assigned[..from].contains(&s))
                        .collect()
                };
                let col = |s: i32| if s < 0 { width - 1 } else { s as usize };
                t.power = (t.power
                    + choices
                        .iter()
                        .map(|&s| song.slots[slot][col(s)])
                        .fold(f64::NEG_INFINITY, f64::max))
                .next_up();
                t.skill = (t.skill
                    + choices
                        .iter()
                        .map(|&s| song.skills[m * width + col(s)].1)
                        .fold(f64::NEG_INFINITY, f64::max))
                .next_up();
                t.skill_low += choices
                    .iter()
                    .map(|&s| song.skills[m * width + col(s)].0)
                    .fold(f64::INFINITY, f64::min);
                t.bonus += song.objective.member_bonus(m)
                    + choices
                        .iter()
                        .map(|&s| {
                            if s < 0 {
                                0.0
                            } else {
                                song.objective.snap_bonus(s as usize)
                            }
                        })
                        .fold(0.0, f64::max);
            }
            let mut bound = song.objective.bound(t).min(
                song.objective
                    .cheap_assignment_bound(members, t, assigned, from),
            );
            if let Some(value) = song.objective.assignment_bound(members, t, assigned, from) {
                bound = bound.min(value);
            }
            sum = (sum + bound).next_up();
        }
        (sum / self.songs.len() as f64).next_up()
    }
}
impl Objective for Combined<'_> {
    fn seed_objective(&self) -> Option<Box<dyn Objective>> {
        Some(Box::new(super::PowerObjective))
    }
    fn seed_ready(&self, detail: &Value) -> bool {
        detail["songs"].is_array()
    }
    fn canonical_leader(&self) -> bool {
        self.songs.iter().all(|s| s.objective.canonical_leader())
    }
    fn preserve_snap_options(&self) -> bool {
        self.event || self.songs.iter().any(|song| song.unbounded)
    }
    fn conditional_snap_dominance(&self) -> bool {
        !self.event && !self.songs.iter().any(|song| song.unbounded)
    }
    fn snap_dominance(&mut self, member: usize, snap: i32) -> Vec<f64> {
        let width = self.snaps.len() + 1;
        self.songs
            .iter_mut()
            .flat_map(|song| {
                let mut values = song.objective.snap_dominance(member, snap);
                values.push(
                    song.snap_power
                        [member * width + if snap < 0 { width - 1 } else { snap as usize }],
                );
                values.push(
                    song.skills[member * width + if snap < 0 { width - 1 } else { snap as usize }]
                        .1,
                );
                values
            })
            .collect()
    }
    fn score_ties(&self) -> bool {
        self.event
    }
    fn tie_rank(&self, hit: &search::Hit) -> [f64; 2] {
        [
            master::number(&hit.detail["otherMean"], 0.0),
            master::number(&hit.detail["scores"]["mean"], 0.0),
        ]
    }
    fn compare_rank(&self, a: &search::Hit, b: &search::Hit) -> Option<std::cmp::Ordering> {
        crate::reward_plan::compare(&a.detail["rewardRanking"], &b.detail["rewardRanking"])
    }
    fn skill(&mut self, m: usize, s: i32) -> (f64, f64) {
        let col = if s < 0 { self.snaps.len() } else { s as usize };
        if self.affine.is_some() {
            let high = self.songs.iter().fold(0.0_f64, |sum, song| {
                (sum + (song.skills[m * (self.snaps.len() + 1) + col].1.max(0.0)
                    * song.linear.unwrap().1)
                    .next_up())
                .next_up()
            });
            return (0.0, (high / self.songs.len() as f64).next_up());
        }
        (
            0.0,
            self.songs
                .iter()
                .map(|song| song.skills[m * (self.snaps.len() + 1) + col].1)
                .fold(0.0, f64::max),
        )
    }
    fn member_bonus(&self, m: usize) -> f64 {
        self.songs
            .iter()
            .map(|s| s.objective.member_bonus(m))
            .fold(0.0, f64::max)
    }
    fn snap_bonus(&self, s: usize) -> f64 {
        self.songs
            .iter()
            .map(|song| song.objective.snap_bonus(s))
            .fold(0.0, f64::max)
    }
    fn product_base(&self) -> Option<f64> {
        self.affine
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<search::JointTarget>> {
        self.affine?;
        Some(vec![search::JointTarget {
            bonus: 0.0,
            product: (limit.next_down() / (1.0 + 3e-12)).next_down(),
        }])
    }
    fn bound(&mut self, t: Totals) -> f64 {
        if let Some(base) = self.affine {
            return ((t.power * (base + t.skill).next_up()).next_up() * (1.0 + 3e-12)).next_up();
        }
        let sum = self.songs.iter_mut().fold(0.0_f64, |sum, song| {
            (sum + song.objective.bound(Totals {
                power: song.cap,
                ..t
            }))
            .next_up()
        });
        (sum / self.songs.len() as f64).next_up()
    }
    fn prepare_members(&mut self, groups: &[Vec<usize>]) {
        for song in &mut self.songs {
            song.objective.prepare_members(groups);
        }
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.options = options.to_vec();
        let roster: Vec<_> = members.iter().map(|&m| self.members[m].clone()).collect();
        let profile = power::leader_profile(self.master, &roster[0]);
        let width = self.snaps.len() + 1;
        for song in &mut self.songs {
            let percents =
                power::leader_percents(self.master, &profile, &roster, 0, Some(&song.music));
            let bases: Vec<_> = members
                .iter()
                .enumerate()
                .map(|(slot, &m)| {
                    (song.bases[m].fixed_points
                        + power::leader_points(&song.bases[m], &percents[slot]))
                        as f64
                })
                .collect();
            song.slots = members
                .iter()
                .enumerate()
                .map(|(slot, &m)| {
                    (0..width)
                        .map(|s| bases[slot] + song.snap_power[m * width + s])
                        .collect()
                })
                .collect();
            song.objective
                .prepare_power(Arc::clone(&song.snap_power), width);
            song.objective.prepare_assignment_power(bases.iter().sum());
            song.objective.prepare_assignment(members, options);
        }
    }
    fn cheap_assignment_bound(
        &mut self,
        members: &[usize],
        _t: Totals,
        assigned: &[i32],
        from: usize,
    ) -> f64 {
        self.upper(members, assigned, from)
    }
    fn interval(&mut self, _team: &Team, _t: Totals) -> (f64, f64) {
        (if self.songs.iter().any(|song| song.unbounded) { f64::NEG_INFINITY } else { 0.0 }, f64::INFINITY)
    }
    fn exact(&mut self, team: &Team, _power: f64, _slots: &[f64]) -> Result<Exact, String> {
        if !self.eligible(team) {
            return Ok(Exact {
                key: f64::NEG_INFINITY,
                detail: Value::Null,
            });
        }
        let members: Vec<_> = team
            .members
            .iter()
            .map(|&m| self.members[m].clone())
            .collect();
        let snaps: Vec<_> = team
            .snaps
            .iter()
            .map(|&s| (s >= 0).then(|| self.snaps[s as usize].clone()))
            .collect();
        let mut parts = Vec::new();
        for song in &mut self.songs {
            let p = power::team_power(
                self.master,
                self.player,
                &members,
                &snaps,
                0,
                Some(&song.music),
                self.effects,
            )?;
            let mut exact = song.objective.exact(
                team,
                p.total as f64,
                &p.slots.iter().map(|&v| v as f64).collect::<Vec<_>>(),
            )?;
            if exact.detail["settlement"].is_object() {
                exact.detail["otherMean"] = exact.detail["settlement"]["otherMean"].clone();
            }
            parts.push(
                json!({"key":exact.key,"detail":exact.detail,"power":p.total,"slotPowers":p.slots}),
            );
        }
        let count = parts.len() as f64;
        let mean = |path: &[&str]| {
            parts
                .iter()
                .map(|part| {
                    let mut value = part;
                    for key in path {
                        value = &value[*key];
                    }
                    master::number(value, 0.0)
                })
                .sum::<f64>()
                / count
        };
        let mut detail = json!({"songs":parts,"otherMean":mean(&["detail","otherMean"]),"scores":{"mean":mean(&["detail","scores","mean"])}});
        let mut key = mean(&["key"]);
        if let Some(plan) = &self.plan {
            let ranks: Vec<_> = (0..plan.levels.len())
                .map(|i| {
                    Fraction::mean(
                        &parts
                            .iter()
                            .map(|p| Fraction::from_json(&p["detail"]["rewardRanking"][i]).unwrap())
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            key = ranks[0].lower();
            detail["rewardRanking"] = json!(ranks.iter().map(Fraction::json).collect::<Vec<_>>());
            detail["rewardPlan"] = json!(plan);
            detail["planMean"] = json!(ranks[0].divided(plan.unit).value());
            detail["rewardValues"] = json!({});
            for metric in ["points", "challenge-points", "items"] {
                detail["rewardValues"][metric] = Fraction::mean(
                    &parts
                        .iter()
                        .map(|p| Fraction::from_json(&p["detail"]["rewardValues"][metric]).unwrap())
                        .collect::<Vec<_>>(),
                )
                .json();
            }
        }
        Ok(Exact { key, detail })
    }
}

impl Engine {
    pub(super) fn run_shared(
        &mut self,
        input: &Value,
        started: f64,
        now: &dyn Fn() -> f64,
    ) -> Result<Value, String> {
        let request = &input["request"];
        let goal = &request["goal"];
        let songs: Vec<_> = master::rows(&goal["songs"]).into_iter().cloned().collect();
        if songs.is_empty() {
            return Err("shared-songs-missing".into());
        }
        if goal["kind"] == "gekiso" {
            return self.run_arena(input, started, now);
        }
        let event = goal["kind"] == "event";
        if !event && goal["kind"] != "score" || goal["route"] == "skip" {
            return Err("shared-goal-unsupported".into());
        }
        if !event && (goal["criterion"] != "mean" || goal["target"].is_object()) {
            return Err("shared-mean-required".into());
        }
        if event && goal["ranking"] == "pt-only" {
            let issues = crate::pt::validate(&self.master, request)?;
            if !issues.is_empty() {
                return Err(format!("pt-input:{}", issues.join(",")));
            }
        }
        let _scope = crate::objectives::slot_table_scope();
        let mut sources = Vec::new();
        let mut charts = Vec::new();
        for song in &songs {
            self.load_source(input, song)?;
            let key = (
                master::int(&song["songId"]),
                master::int(&song["difficulty"]),
            );
            if goal["ranking"] == "pt-only" {
                crate::pt::validate_chart(&self.master, key.0, key.1, &self.sources[&key])?;
            }
            sources.push(self.sources[&key].clone());
            charts.push(self.charts[&key].clone());
        }
        let cards = search_box(&self.master, request)?;
        crate::progress::keys(&cards.members, &cards.snaps);
        let player = power::player_state(&self.master, &request["player"]);
        let route_kind = if goal["route"] == "challenge" {
            RouteKind::Challenge
        } else {
            RouteKind::Live
        };
        let route = event
            .then(|| {
                Route::for_goal(
                    &self.master,
                    goal,
                    route_kind,
                    master::int(&goal["consumption"]),
                )
            })
            .transpose()?;
        let challenge = if route_kind == RouteKind::Challenge {
            Some(master::int(&goal["eventId"]))
        } else {
            goal["challengeEventId"].as_i64().map(|v| v as i32)
        };
        let effects = if let Some(id) = challenge {
            self.master
                .events
                .get(&id)
                .map(|e| e.effects.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let plan = (!goal["rewardPlan"].is_null())
            .then(|| Plan::parse(&goal["rewardPlan"]))
            .transpose()?;
        let k = master::number(&request["k"], 1.0).max(1.0) as usize;
        let battle = goal["gekiso"].is_object();
        let mut objectives = Vec::new();
        for ((song, chart), source) in songs.iter().zip(&charts).zip(&sources) {
            let music = power::music_view(&self.master, master::int(&song["songId"]), challenge)?;
            let anchor = plan.as_ref().map_or(
                if goal["measure"] == "items" {
                    Metric::Items
                } else {
                    Metric::Points
                },
                Plan::anchor,
            );
            let exchange = (plan.is_none() && goal["cpExchange"].is_object()).then(|| {
                (
                    master::number(&goal["cpExchange"]["numerator"], 0.0),
                    master::number(&goal["cpExchange"]["denominator"], 1.0),
                )
            });
            let mut linear = None;
            let unbounded;
            let mut objective: Box<dyn Objective> = if let Some(route) = &route {
                let raw = if battle {
                    let mut gk = crate::gekiso::Chart::new(
                        &self.master,
                        source,
                        chart.difficulty.play_level,
                        chart.song.missions.clone(),
                        crate::full::play::Accuracy {
                            just: master::number(&goal["gekiso"]["just"], 1.0),
                            ..Default::default()
                        },
                        vec![1; 3],
                    )?;
                    gk.music_type = music.music_type;
                    crate::reward_plan::Source::Gekiso(Box::new(
                        crate::rewards::RewardObjective::new(
                            &self.master,
                            &cards.members,
                            &cards.snaps,
                            chart,
                            gk,
                            route.clone(),
                            anchor == Metric::Points,
                            master::int(&goal["gekiso"]["luckSamples"]),
                            exchange,
                        ),
                    ))
                } else {
                    crate::reward_plan::Source::Ordinary(Box::new(EventObjective::new(
                        &self.master,
                        &cards.members,
                        &cards.snaps,
                        Some(chart),
                        route.clone(),
                        anchor == Metric::Points,
                        &goal["play"],
                        true,
                        0.0,
                        exchange,
                        f64::INFINITY,
                    )))
                };
                unbounded = match &raw {
                    crate::reward_plan::Source::Ordinary(event) => event.core.as_ref().is_some_and(|core| core.unbounded),
                    crate::reward_plan::Source::Gekiso(_) => false,
                };
                if let Some(plan) = &plan {
                    Box::new(Ranked::new(
                        raw,
                        Context {
                            plan: plan.clone(),
                            source_anchor: anchor,
                            route: route.clone(),
                            chart: Some(chart),
                            members: &cards.members,
                            snaps: &cards.snaps,
                            k,
                            floor: f64::NEG_INFINITY,
                            skip_rank: self.master.params.skip_rank,
                            observations: 120
                                * if battle {
                                    master::int(&goal["gekiso"]["luckSamples"]).max(1) as usize
                                } else {
                                    1
                                },
                        },
                    ))
                } else if goal["measure"] == "challenge-points" {
                    Box::new(crate::event::ChallengePoints::new(
                        LiveObjective {
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
                        chart.clone(),
                        route.clone(),
                    ))
                } else {
                    match raw {
                        crate::reward_plan::Source::Ordinary(v) => v,
                        crate::reward_plan::Source::Gekiso(v) => v,
                    }
                }
            } else {
                let core = LiveCore::new(
                    &self.master,
                    chart,
                    &cards.members,
                    &cards.snaps,
                    &goal["play"],
                );
                let scale = ((1.0 + crate::model::RELATIVE_ERROR)
                    / core.input.converted_count as f64)
                    .next_up();
                unbounded = core.unbounded;
                linear = (!unbounded).then(|| ((core.high.total * scale).next_up(), scale));
                Box::new(LiveObjective {
                    core,
                    criterion: Criterion::Mean,
                    target: None,
                })
            };
            let width = cards.snaps.len() + 1;
            let bases: Vec<_> = cards
                .members
                .iter()
                .map(|m| {
                    power::slot_base(
                        &self.master,
                        &player,
                        m,
                        Some(&music),
                        power::member_event_percent(&effects, m, 2),
                    )
                })
                .collect();
            let mut snap_power = Vec::new();
            let mut skills = Vec::new();
            for (m, member) in cards.members.iter().enumerate() {
                for (s, snap) in cards
                    .snaps
                    .iter()
                    .map(Some)
                    .chain(std::iter::once(None))
                    .enumerate()
                {
                    snap_power.push(power::snap_points(
                        &self.master,
                        &bases[m],
                        member,
                        snap,
                        snap.map_or(0, |s| power::snap_event_percent(&effects, s, 2)),
                    ) as f64);
                    skills.push(objective.skill(m, if s == width - 1 { -1 } else { s as i32 }));
                }
            }
            let profiles: Vec<_> = cards
                .members
                .iter()
                .map(|m| power::leader_profile(&self.master, m))
                .collect();
            let mut caps: Vec<_> = cards
                .members
                .iter()
                .enumerate()
                .map(|(m, member)| {
                    (bases[m].fixed_points
                        + profiles
                            .iter()
                            .map(|p| {
                                power::leader_points(
                                    &bases[m],
                                    &power::leader_percent_bound(&self.master, p, member),
                                )
                            })
                            .max()
                            .unwrap_or(0)) as f64
                        + snap_power[m * width..(m + 1) * width]
                            .iter()
                            .copied()
                            .fold(0.0, f64::max)
                })
                .collect();
            caps.sort_by(|a, b| b.total_cmp(a));
            let cap = caps
                .iter()
                .take(5)
                .fold(0.0_f64, |sum, v| (sum + v).next_up());
            let snap_power = Arc::new(snap_power);
            objective.prepare_power(Arc::clone(&snap_power), width);
            let opening = if battle && chart.song.missions.iter().any(|m| *m == 1 || *m == 4) {
                crate::gekiso_objective::opening_combo_bonuses(
                    &self.master,
                    &cards.members,
                    &chart.song.missions,
                )
            } else {
                Vec::new()
            };
            objectives.push(SongObjective {
                objective,
                music,
                bases,
                snap_power,
                skills,
                cap,
                linear,
                unbounded,
                slots: Vec::new(),
                opening,
            });
        }
        let affine = (effects.is_empty()
            && objectives.iter().all(|song| {
                song.linear.is_some()
                    && song.music.music_type == objectives[0].music.music_type
                    && song
                        .bases
                        .iter()
                        .zip(&objectives[0].bases)
                        .all(|(a, b)| a.common == b.common && a.fixed_points == b.fixed_points)
            }))
        .then(|| {
            (objectives.iter().fold(0.0_f64, |sum, song| {
                (sum + song.linear.unwrap().0).next_up()
            }) / objectives.len() as f64)
                .next_up()
        });
        let music = objectives[0].music.clone();
        let mut combined = Combined {
            master: &self.master,
            player: &player,
            members: &cards.members,
            snaps: &cards.snaps,
            effects: &effects,
            songs: objectives,
            options: Vec::new(),
            min_combo: master::int(&goal["gekiso"]["minComboBonus"]).max(1),
            event,
            plan,
            affine,
        };
        let deadline = request["deadline"].as_f64().unwrap_or_else(|| {
            request["timeLimitMs"]
                .as_f64()
                .map_or(f64::INFINITY, |v| started + v)
        });
        let mut publish = |hits: &[search::Hit]| {
            crate::progress::emit_document(
                &json!({"song":null,"hits":hits.iter().map(|h|shared_hit(h,&songs,&charts,&cards.members,&cards.snaps,battle,event,goal)).collect::<Vec<_>>()}),
            )
        };
        let output = search::search(search::Input {
            master: &self.master,
            player: &player,
            members: &cards.members,
            snaps: &cards.snaps,
            music: Some(&music),
            objective: &mut combined,
            constraints: constraints(&request["constraints"], &cards.members, &cards.snaps),
            k,
            deadline,
            now,
            seeds: seed_teams(request, &Value::Null, &cards.members, &cards.snaps),
            shard: request["shard"].is_object().then(|| {
                (
                    master::int(&request["shard"]["index"]) as usize,
                    master::int(&request["shard"]["count"]) as usize,
                )
            }),
            owns: None,
            floor: f64::NEG_INFINITY,
            floor_exclusive: false,
            streaming: true,
            disable_snap_dominance: event,
            member_power_percent: None,
            snap_power_percent: None,
            on_candidate: Some(&mut publish),
            progress: None,
        })?;
        let hits: Vec<_> = output
            .hits
            .iter()
            .map(|h| {
                shared_hit(
                    h,
                    &songs,
                    &charts,
                    &cards.members,
                    &cards.snaps,
                    battle,
                    event,
                    goal,
                )
            })
            .collect();
        Ok(
            json!({"results":[{"song":null,"hits":hits,"proven":output.proven,"bound":output.bound,"stats":output.stats}],"overall":hits,"unknownCards":cards.unknown,"cachedCharts":self.cached_chart_keys(),"elapsedMs":now()-started,"stopReason":if output.proven {"complete"} else if now()>=deadline {"budget"} else {"cancelled"}}),
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn shared_hit(
    hit: &search::Hit,
    songs: &[Value],
    charts: &[Chart],
    members: &[power::Member],
    snaps: &[power::Snap],
    battle: bool,
    event: bool,
    goal: &Value,
) -> Value {
    let parts: Vec<_> = master::rows(&hit.detail["songs"]).into_iter().enumerate().map(|(i,p)| {
        let child = search::Hit {team:hit.team.clone(),power:master::number(&p["power"],0.0),slot_powers:master::rows(&p["slotPowers"]).iter().map(|v|master::number(v,0.0)).collect(),key:master::number(&p["key"],0.0),detail:p["detail"].clone(),totals:Totals::default()};
        let mut row=hit_json(&child,&songs[i],members,snaps,Some(&charts[i]),battle);
        let d=&child.detail;
        row["comboBonus"]=d["comboBonus"].clone();
        if event {
            let settlement=&d["settlement"];
            let count=master::number(&settlement["observations"],120.0);
            let direct=if battle {master::number(&settlement["rewardSum"],0.0)/count} else {master::number(&d["directMean"],0.0)};
            let cp=if battle {master::number(&settlement["cpSum"],0.0)/count} else {master::number(&d["challengePoints"],0.0)};
            let mean=if goal["rewardPlan"].is_object() {0.0} else if battle {master::number(&settlement["comparisonSum"],child.key*count)/count/master::number(&goal["cpExchange"]["denominator"],1.0)} else {master::number(&d["mean"],child.key)/master::number(&goal["cpExchange"]["denominator"],1.0)};
            row["event"]=json!({"mean":mean,"bonusPercent":master::number(&d["bonus"],0.0)/100.0,"challengePoints":cp,"directMean":direct,"otherMean":if battle {&settlement["otherMean"]} else {&d["otherMean"]},"perPlayMin":if battle {&settlement["directMin"]} else {&d["directMin"]},"perPlayMax":if battle {&settlement["directMax"]} else {&d["directMax"]}});
            if battle { row["score"]["ranks"]=settlement["ranks"].clone(); }
            crate::reward_plan::decorate(&mut row,d);
        }
        row
    }).collect();
    let count = parts.len() as f64;
    let avg = |path: &[&str]| {
        parts
            .iter()
            .map(|p| {
                let mut v = p;
                for key in path {
                    v = &v[*key];
                }
                master::number(v, 0.0)
            })
            .sum::<f64>()
            / count
    };
    let mut row = hit_json(hit, &Value::Null, members, snaps, None, battle);
    row["power"] = json!(avg(&["power"]));
    row["slotPowers"] = json!((0..5)
        .map(|i| parts
            .iter()
            .map(|p| master::number(&p["slotPowers"][i], 0.0))
            .sum::<f64>()
            / count)
        .collect::<Vec<_>>());
    row["score"] = json!({"mean":avg(&["score","mean"]),"min":avg(&["score","min"]),"max":avg(&["score","max"]),"median":avg(&["score","median"]),"orders":[],"ranks":{},"bestOrder":[],"worstOrder":[]});
    if event {
        row["event"] = json!({});
        for key in [
            "mean",
            "bonusPercent",
            "challengePoints",
            "directMean",
            "otherMean",
            "perPlayMin",
            "perPlayMax",
        ] {
            row["event"][key] = json!(avg(&["event", key]));
        }
        crate::reward_plan::decorate(&mut row, &hit.detail);
    }
    row["aggregate"] = json!({"kind":"mean","songs":parts});
    row
}
