//! Resolve a common score reference before the section-first formation search.
use super::*;
use crate::{
    full::conditions::Performer,
    gekiso,
    gekiso_objective::{
        self,
        sections::{Plan, Summary},
    },
    search,
};

pub(super) struct Cache {
    key: Value,
    sources: Vec<Source>,
    baseline: Option<Value>,
    references: Vec<Summary>,
    targets: Vec<Option<gekiso::sections::OpponentSamples>>,
    parts: Vec<Value>,
    traces: Vec<Option<crate::just_window::Trace>>,
    plan: Option<Plan>,
    charts: Vec<gekiso::Chart>,
    completed: Option<Value>,
}
pub(super) struct Multiple {
    key: Value,
    sources: Vec<Source>,
    completed: BTreeMap<String, Value>,
}
fn task_key(request: &Value) -> Value {
    let mut key = request.clone();
    for name in [
        "timeLimitMs",
        "deadline",
        "requestId",
        "contextKey",
        "seeds",
        "completed",
        "shard",
        "seedOnly",
        "seeded",
        "noFloor",
        "cooperativeFloor",
        "floors",
    ] {
        key.as_object_mut().unwrap().remove(name);
    }
    key
}
fn pending_reference(mut value: Value, elapsed: f64) -> Value {
    let mark = |hit: &mut Value| {
        hit["key"] = json!(-1);
        hit["sectionReference"] = json!(true);
    };
    for row in value["results"].as_array_mut().into_iter().flatten() {
        row["proven"] = json!(false);
        for hit in row["hits"].as_array_mut().into_iter().flatten() {
            mark(hit);
        }
    }
    for hit in value["overall"].as_array_mut().into_iter().flatten() {
        mark(hit);
    }
    value["elapsedMs"] = json!(elapsed);
    value["stopReason"] = json!("budget");
    value
}

pub(super) struct Scene {
    key: Value,
    source: Source,
    targets: gekiso::sections::OpponentSamples,
}
impl Engine {
    pub fn section_metrics(&mut self, input: &Value) -> Result<Value, String> {
        if input["request"]["songRules"].is_object() {
            let (mut scoped, scoped_input) = self.song_rule_view(input)?;
            return scoped.section_metrics(&scoped_input);
        }
        if input["request"]["gekiso"]["arena"].is_object() {
            let (mut scoped, scoped_input, _, _) = self.arena_view(input)?;
            return scoped.section_metrics(&scoped_input);
        }
        let request = &input["request"];
        validate_gekiso_counts(&request["gekiso"])?;
        let options = &request["gekiso"];
        let count = master::int(&options["seeds"]);
        let seeds: Vec<_> = (0..if count > 0 { count } else { 32 }).collect();
        let mut values = Vec::new();
        for song in master::rows(&request["songs"]) {
            self.load_source(input, song)?;
            let id = master::int(&song["songId"]);
            let difficulty = master::int(&song["difficulty"]);
            let resolved = self.resolve_team(request, &request["team"], id)?;
            let compiled = &self.charts[&(id, difficulty)];
            let source = &self.sources[&(id, difficulty)];
            let mut chart = gekiso::Chart::new(&self.master, source, compiled.difficulty.play_level,
                compiled.song.missions.clone(), serde_json::from_value(options["accuracy"].clone()).map_err(|e| e.to_string())?, gekiso_ranks(options))?;
            chart.music_type = resolved.music.music_type;
            let deck: Vec<_> = resolved.members.iter().zip(&resolved.snaps)
                .map(|(member, snap)| Performer::from_member(&self.master, member, snap.as_ref())).collect();
            let rows = gekiso::sections::metric_samples(&self.master, &chart, &deck, &seeds)?;
            let distributions: Vec<_> = (0..3).map(|range| {
                let mut counts = BTreeMap::<i32, usize>::new();
                for row in &rows { *counts.entry(row[range]).or_default() += 1; }
                counts.into_iter().map(|(points, count)| json!({"points":points,"count":count})).collect::<Vec<_>>()
            }).collect();
            values.push(json!({"song":song,"missions":chart.setup.missions,"samples":rows.len(),"distributions":distributions}));
        }
        Ok(json!({"metrics":values,"cachedCharts":self.cached_chart_keys()}))
    }
    pub(super) fn evaluate_section_team(
        &self,
        request: &Value,
        team: &Value,
        song: &Value,
        source: &Source,
    ) -> Result<Value, String> {
        let id = master::int(&song["songId"]);
        let difficulty = master::int(&song["difficulty"]);
        let resolved = self.resolve_team(request, team, id)?;
        let compiled = self.charts.get(&(id, difficulty)).ok_or("missing-chart")?;
        let setup = &request["goal"];
        let mut chart = gekiso::Chart::new(
            &self.master,
            source,
            compiled.difficulty.play_level,
            compiled.song.missions.clone(),
            serde_json::from_value(setup["accuracy"].clone()).map_err(|e| e.to_string())?,
            gekiso_ranks(setup),
        )?;
        chart.music_type = resolved.music.music_type;
        let count = master::int(&setup["seeds"]);
        let seeds: Vec<_> = (0..if count > 0 { count } else { 32 }).collect();
        let opponents =
            self.section_opponents(&request["sectionArena"], &chart, source, id, &seeds)?;
        let minimum = crate::just_window::Policy::read(setup)?;
        let selected_snaps: Vec<_> = resolved.snaps.iter().flatten().cloned().collect();
        let trace = minimum
            .map(|minimum| {
                crate::just_window::Prepared::new(
                    &self.master,
                    &chart,
                    &resolved.members,
                    &selected_snaps,
                    minimum,
                    &seeds,
                )
                .map(|prepared| prepared.capture_trace(&self.master,&chart))
            })
            .transpose()?;
        let deck: Vec<_> = resolved
            .members
            .iter()
            .zip(&resolved.snaps)
            .map(|(m, s)| Performer::from_member(&self.master, m, s.as_ref()))
            .collect();
        let rows = gekiso::sections::replay(
            &self.master,
            &chart,
            &deck,
            &seeds,
            resolved.power.total as i32,
            opponents.as_ref(),
            trace.as_ref(),
        )?;
        let summary = Summary::of(
            &rows,
            opponents.is_some(),
            gekiso::replay_random(&self.master, &chart, &deck),
        );
        let values = (0..120)
            .map(|order| {
                rows.iter()
                    .skip(order)
                    .step_by(120)
                    .map(|row| row.total_score)
                    .sum::<f64>()
                    / (rows.len() / 120) as f64
            })
            .collect();
        let mut result = json!(live::summarize(resolved.power.total as i32, values));
        result["rangeScores"] = json!(summary.range_scores);
        result["sectionSummary"] = json!(summary);
        if let Some(window) = summary.window {
            result["justWindow"] = json!(window);
        }
        Ok(result)
    }
    pub(super) fn section_opponents(
        &self,
        options: &Value,
        chart: &gekiso::Chart,
        source: &Source,
        id: i32,
        seeds: &[i32],
    ) -> Result<Option<gekiso::sections::OpponentSamples>, String> {
        if master::rows(&options["opponents"]).is_empty() {
            return Ok(None);
        }
        let key = json!([
            id,
            chart.music_type,
            chart.full.level,
            chart.setup.missions,
            chart.setup.ranks,
            chart.accuracy,
            seeds.len(),
            options["opponents"],
            self.master.live.gekiso.ranks
        ]);
        if let Some(found) = self
            .section_scenes
            .lock()
            .unwrap()
            .iter()
            .find(|entry| entry.key == key && &entry.source == source)
        {
            return Ok(Some(found.targets.clone()));
        }
        let samples = seeds.len();
        let mut scenarios = vec![Vec::new(); samples];
        for (opponent_index, opponent) in
            master::rows(&options["opponents"]).into_iter().enumerate()
        {
            let resolved = self.resolve_team(opponent, &opponent["team"], id)?;
            if resolved
                .members
                .iter()
                .any(|m| m.card.band_id != master::int(&options["bandId"]))
            {
                return Err("section-opponent-band".into());
            }
            let deck: Vec<_> = resolved
                .members
                .iter()
                .zip(&resolved.snaps)
                .map(|(m, s)| Performer::from_member(&self.master, m, s.as_ref()))
                .collect();
            let mut opponent_chart = chart.converted_judgements(&chart.judgements).unwrap();
            if !opponent["accuracy"].is_null() {
                let item = &self.master.songs[&id];
                opponent_chart = gekiso::Chart::new(
                    &self.master,
                    source,
                    chart.full.level,
                    item.missions.clone(),
                    serde_json::from_value(opponent["accuracy"].clone())
                        .map_err(|e| e.to_string())?,
                    chart.setup.ranks.clone(),
                )?;
                opponent_chart.music_type = chart.music_type;
            }
            if opponent["ranks"].is_array() {
                opponent_chart.setup.ranks = gekiso_ranks(opponent);
            }
            let salt = 104729 * (opponent_index as i32 + 1);
            let independent: Vec<_> = seeds.iter().map(|seed| seed.wrapping_add(salt)).collect();
            let deterministic = !gekiso::replay_random(&self.master, &opponent_chart, &deck);
            let mut orders = [None; 120];
            for (sample, into) in scenarios.iter_mut().enumerate() {
                let mut random = crate::full::core::Random::new(
                    (sample as i32).wrapping_mul(17).wrapping_add(salt),
                );
                let order = random.integer(0).unsigned_abs() as usize % 120;
                let standing = if let Some(value) = orders[order].filter(|_| deterministic) {
                    value
                } else {
                    let value = gekiso::replay_order_mode(
                        &self.master,
                        &opponent_chart,
                        &deck,
                        resolved.power.total as i32,
                        order,
                        (independent[sample], false),
                    )?;
                    let standing = std::array::from_fn(|range| gekiso::sections::Standing {
                        metric: value.missions[range],
                        score: value.ranges[range] as i32,
                        perfect: value.perfects[range],
                    });
                    if deterministic {
                        orders[order] = Some(standing);
                    }
                    standing
                };
                into.push(standing);
            }
        }
        let scenarios = Arc::new(scenarios);
        let mut cache = self.section_scenes.lock().unwrap();
        if samples > 4096 {
            return Ok(Some(scenarios));
        }
        if cache.len() >= 32 {
            cache.pop_front();
        }
        cache.push_back(Scene {
            key,
            source: source.clone(),
            targets: scenarios.clone(),
        });
        Ok(Some(scenarios))
    }
}

impl Engine {
    pub(super) fn run_arena_sections(
        &mut self,
        input: &Value,
        started: f64,
        now: &dyn Fn() -> f64,
    ) -> Result<Value, String> {
        let options = &input["request"]["goal"]["arena"];
        let loss = options["maxBaseLoss"].as_f64().unwrap_or(0.03);
        if !loss.is_finite() || !(0.0..=1.0).contains(&loss) {
            return Err("section-loss-invalid".into());
        }
        if master::rows(&options["opponents"]).len() > 4 {
            return Err("section-opponents-limit".into());
        }
        let (mut scoped, mut scoped_input, songs, sources) =
            if input["request"]["songRules"].is_object() {
                let (mut view, projected) = self.song_rule_view(input)?;
                view.arena_view(&projected)?
            } else {
                self.arena_view(input)?
            };
        if options["mode"] == "individual" && songs.len() > 1 {
            let key = task_key(&input["request"]);
            let mut multiple = self
                .section_multiple
                .take()
                .filter(|cache| cache.key == key && cache.sources == sources)
                .unwrap_or_else(|| Multiple {
                    key,
                    sources: sources.clone(),
                    completed: BTreeMap::new(),
                });
            let response = (|| -> Result<Value, String> {
                let mut results = Vec::new();
                for song in &songs {
                    let key = song.to_string();
                    if let Some(completed) = multiple.completed.get(&key) {
                        results.extend(master::rows(&completed["results"]).into_iter().cloned());
                        continue;
                    }
                    if now()
                        >= input["request"]["deadline"]
                            .as_f64()
                            .unwrap_or(f64::INFINITY)
                    {
                        break;
                    }
                    let mut single = input.clone();
                    single["request"]["goal"]["songs"] = json!([song]);
                    let response = self.run_arena_sections(&single, started, now)?;
                    results.extend(master::rows(&response["results"]).into_iter().cloned());
                    if response["stopReason"] == "complete" {
                        multiple.completed.insert(key, response);
                    } else {
                        break;
                    }
                }
                let complete =
                    results.len() == songs.len() && results.iter().all(|row| row["proven"] == true);
                let mut hits: Vec<_> = results
                    .iter()
                    .flat_map(|row| master::rows(&row["hits"]).into_iter().cloned())
                    .collect();
                hits.sort_by(compare_hits);
                hits.truncate(master::number(&input["request"]["k"], 1.0).max(1.0) as usize);
                Ok(
                    json!({"results":results,"overall":hits,"unknownCards":[],"cachedCharts":self.cached_chart_keys(),"elapsedMs":now()-started,"stopReason":if complete {"complete"} else {"budget"}}),
                )
            })();
            self.section_multiple = Some(multiple);
            return response;
        }
        let key = task_key(&input["request"]);
        let mut cache = self
            .section_cache
            .take()
            .filter(|cache| cache.key == key && cache.sources == sources)
            .unwrap_or_else(|| {
                Box::new(Cache {
                    key,
                    sources: sources.clone(),
                    baseline: None,
                    references: Vec::new(),
                    targets: Vec::new(),
                    parts: Vec::new(),
                    traces: Vec::new(),
                    plan: None,
                    charts: Vec::new(),
                    completed: None,
                })
            });
        let result = (|| -> Result<Value, String> {
            if let Some(completed) = &cache.completed {
                let mut value = completed.clone();
                value["elapsedMs"] = json!(now() - started);
                for row in value["results"].as_array_mut().into_iter().flatten() {
                    row["stats"] = json!(search::Stats::default());
                }
                return Ok(value);
            }
            let shared = options["mode"] == "shared" && songs.len() > 1;
            scoped_input["request"]["aggregation"] =
                if shared { json!("mean") } else { Value::Null };
            scoped_input["request"]["goal"]["criterion"] = json!("mean");
            scoped_input["request"]["goal"]["opponents"] = Value::Null;
            scoped_input["request"]["sectionArena"] = Value::Null;
            if !master::rows(&options["opponents"]).is_empty() {
                scoped_input["request"]["goal"]["minJustWindowMs"] = Value::Null;
                scoped_input["request"]["goal"]["justWindow"] = Value::Null;
            }
            scoped_input["request"]["cooperativeFloor"] = json!(false);
            for key in [
                "shard",
                "floors",
                "completed",
                "seeded",
                "seedOnly",
                "seeds",
            ] {
                scoped_input["request"][key] = Value::Null;
            }
            let deadline = input["request"]["deadline"].as_f64().unwrap_or_else(|| {
                input["request"]["timeLimitMs"]
                    .as_f64()
                    .map_or(f64::INFINITY, |ms| started + ms)
            });
            let mut charts = Vec::new();
            let mut compiled = Vec::new();
            let mut music = Vec::new();
            for (song, source) in songs.iter().zip(&sources) {
                let item = &scoped.master.songs[&master::int(&song["songId"])];
                let level = item
                    .difficulties
                    .iter()
                    .find(|row| row.difficulty == master::int(&song["difficulty"]))
                    .ok_or("unknown difficulty")?;
                let view = power::music_view(&scoped.master, item.id, None)?;
                let mut chart = gekiso::Chart::new(
                    &scoped.master,
                    source,
                    level.play_level,
                    item.missions.clone(),
                    serde_json::from_value(input["request"]["goal"]["accuracy"].clone())
                        .map_err(|e| e.to_string())?,
                    gekiso_ranks(&input["request"]["goal"]),
                )?;
                chart.music_type = view.music_type;
                charts.push(chart);
                compiled.push(chart::compile(&scoped.master, item, level, source));
                music.push(view);
            }
            let versus = !master::rows(&options["opponents"]).is_empty();
            let requested = master::int(&input["request"]["goal"]["seeds"]);
            let samples = if requested > 0 { requested } else { 32 };
            scoped_input["request"]["goal"]["seeds"] = json!(samples);
            let seeds: Vec<_> = (0..samples).collect();
            let cards = search_box(&scoped.master, &scoped_input["request"])?;
            let mut possible: Vec<_> = cards
                .members
                .iter()
                .map(|member| Performer::from_member(&scoped.master, member, None))
                .collect();
            possible.extend(cards.snaps.iter().map(|snap| {
                Performer {
                    support_skills: snap
                        .card
                        .support_skills
                        .into_iter()
                        .zip(snap.skill_levels)
                        .filter(|&(id, _)| id != 0)
                        .collect(),
                    gekisou_support_skills: snap
                        .card
                        .gekiso_support_skills
                        .into_iter()
                        .zip(snap.gekiso_skill_levels)
                        .filter(|&(id, _)| id != 0)
                        .collect(),
                    ..Performer::default()
                }
            }));
            let random = charts
                .iter()
                .any(|chart| gekiso::replay_random(&scoped.master, chart, &possible));
            scoped_input["request"]["goal"]["seeds"] = json!(if random { samples } else { 0 });
            let baseline = if let Some(stored) = cache
                .baseline
                .as_ref()
                .filter(|stored| stored["stopReason"] == "complete")
            {
                let mut value = stored.clone();
                for row in value["results"].as_array_mut().into_iter().flatten() {
                    row["stats"] = json!(search::Stats::default());
                }
                value
            } else {
                if let Some(previous) = &cache.baseline {
                    scoped_input["request"]["seeds"] = previous["overall"].clone();
                }
                let value =
                    crate::progress::section_reference(|| scoped.run(&scoped_input, started, now))?;
                cache.baseline = Some(value.clone());
                value
            };
            let reference = &baseline["overall"][0];
            if reference.is_null() {
                return Ok(baseline);
            }
            if baseline["stopReason"] != "complete" || now() >= deadline {
                return Ok(pending_reference(baseline, now() - started));
            }
            let reference_team = json!({"members":reference["members"],"snaps":reference["snaps"],"leader":reference["members"][0]});
            let mut targets = cache.targets.clone();
            let mut references = cache.references.clone();
            let mut reference_parts = cache.parts.clone();
            let request = &scoped_input["request"];

            let minimum_window =
                crate::just_window::Policy::read(&input["request"]["goal"])?;
            let mut window_traces = cache.traces.clone();
            for (index, chart) in charts.iter().enumerate().skip(references.len()) {
                if now() >= deadline {
                    return Ok(pending_reference(baseline, now() - started));
                }
                let trace = minimum_window
                    .map(|minimum| {
                        crate::just_window::Prepared::new(
                            &scoped.master,
                            chart,
                            &cards.members,
                            &cards.snaps,
                            minimum,
                            &seeds,
                        )
                        .map(|prepared| prepared.capture_trace(&scoped.master,chart))
                    })
                    .transpose()?;
                let id = master::int(&songs[index]["songId"]);
                let opponents =
                    scoped.section_opponents(options, chart, &sources[index], id, &seeds)?;
                let resolved =
                    scoped.resolve_team(&scoped_input["request"], &reference_team, id)?;
                let deck: Vec<_> = resolved
                    .members
                    .iter()
                    .zip(&resolved.snaps)
                    .map(|(m, s)| Performer::from_member(&scoped.master, m, s.as_ref()))
                    .collect();
                let rows = gekiso::sections::replay(
                    &scoped.master,
                    chart,
                    &deck,
                    &seeds,
                    resolved.power.total as i32,
                    opponents.as_ref(),
                    trace.as_ref(),
                )?;
                references.push(Summary::of(
                    &rows,
                    versus,
                    gekiso::replay_random(&scoped.master, chart, &deck),
                ));
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
                reference_parts.push(json!({"power":resolved.power.total,"slotPowers":resolved.power.slots,"detail":{"scores":live::summarize(resolved.power.total as i32,scores),"sectionPerformance":references.last().unwrap(),
                "justWindow":references.last().unwrap().window}}));
                targets.push(opponents);
                window_traces.push(trace);
                cache.references = references.clone();
                cache.targets = targets.clone();
                cache.parts = reference_parts.clone();
                cache.traces = window_traces.clone();
            }
            let mut plan = cache.plan.take().unwrap_or_else(|| Plan {
                reuse: RefCell::new(None),
                incumbents: RefCell::new(Vec::new()),
                full_master: scoped.master.clone(),
                references,
                opponents: targets,
                seeds,
                max_base_loss: loss,
                base_bounds: !versus || !charts.iter().any(|chart| {
                    gekiso::replay_condition(&scoped.master, chart, &possible, 7012)
                }),
                reference_hit: None,
                minimum_window,
                window_traces,
                score_ceiling: (!versus
                    && charts.iter().all(|chart| !chart.luck)
                    && baseline["stopReason"] == "complete")
                    .then(|| master::number(&reference["score"]["mean"], f64::INFINITY)),
            });
            crate::progress::keys(&cards.members, &cards.snaps);
            let initial = seed_teams(
                &json!({"seeds":[reference]}),
                &reference["song"],
                &cards.members,
                &cards.snaps,
            );
            let scored = plan.rank(&plan.references, &reference_parts);
            if plan.reference_hit.is_none() {
                plan.reference_hit = scored.key.is_finite().then(|| search::Hit {
                    team: initial[0].clone(),
                    power: master::number(&reference["power"], 0.0),
                    slot_powers: master::rows(&reference["slotPowers"])
                        .iter()
                        .map(|v| master::number(v, 0.0))
                        .collect(),
                    key: scored.key,
                    detail: scored.detail,
                    totals: search::Totals::default(),
                });
            }
            let player = power::player_state(&scoped.master, &request["player"]);
            let k = master::number(&request["k"], 1.0).max(1.0) as usize;
            let mut publish = |hits: &[search::Hit]| {
                crate::progress::emit_document(
                    &json!({"song":if shared {Value::Null}else{songs[0].clone()},"hits":hits.iter().map(|hit|section_hit(hit,&songs,&compiled,&cards.members,&cards.snaps,reference,shared)).collect::<Vec<_>>()}),
                )
            };
            let search_seeds = if random {
                plan.seeds.clone()
            } else {
                Vec::new()
            };
            let mut bound_master = scoped.master.clone();
            for row in &mut Arc::make_mut(&mut bound_master.live).gekiso.ranks {
                row[3] = 0;
            }
            if cache.charts.is_empty() {
                for chart in &mut charts {
                    chart.range_percent.fill(0);
                }
                cache.charts = charts;
            }
            let output = gekiso_objective::search_shared(gekiso_objective::SharedInput {
                env: gekiso_objective::Environment {
                    master: &bound_master,
                    player: &player,
                    members: &cards.members,
                    snaps: &cards.snaps,
                    music: Some(&music[0]),
                    constraints: constraints(
                        &scoped.master,
                        &request["constraints"],
                        &cards.members,
                        &cards.snaps,
                    ),
                    now,
                    generation: crate::cancellation_generation(),
                    window_policy: crate::just_window::Policy::read(&request["goal"])?,
                },
                charts: &mut cache.charts,
                music: &music,
                seeds: search_seeds,
                k,
                deadline,
                initial,
                shard: None,
                min_combo_bonus: master::int(&request["goal"]["minComboBonus"]),
                seeded: false,
                cooperative_floor: false,
                floor: f64::NEG_INFINITY,
                on_candidate: Some(&mut publish),
                sections: Some(&plan),
            });
            if let Ok(output) = &output {
                *plan.incumbents.borrow_mut() = output.hits.clone();
            }
            cache.plan = Some(plan);
            let output = output?;
            let proven = output.proven && baseline["stopReason"] == "complete";
            let mut stats = output.stats;
            for result in master::rows(&baseline["results"]) {
                if let Ok(reference) =
                    serde_json::from_value::<search::Stats>(result["stats"].clone())
                {
                    stats.add(&reference);
                }
            }
            stats.elapsed_ms = now() - started;
            let hits: Vec<_> = output
                .hits
                .iter()
                .map(|hit| {
                    section_hit(
                        hit,
                        &songs,
                        &compiled,
                        &cards.members,
                        &cards.snaps,
                        reference,
                        shared,
                    )
                })
                .collect();
            Ok(
                json!({"results":[{"song":if shared {Value::Null}else{songs[0].clone()},"hits":hits,"proven":proven,"bound":output.bound,"stats":stats}],"overall":hits,"unknownCards":cards.unknown,"cachedCharts":self.cached_chart_keys(),"elapsedMs":now()-started,
            "stopReason":if proven {"complete"}else if now()>=deadline {"budget"}else{"cancelled"}}),
            )
        })();
        if let Ok(value) = &result {
            if value["stopReason"] == "complete" {
                cache.completed = Some(value.clone());
                cache.plan = None;
                cache.charts.clear();
                cache.references.clear();
                cache.targets.clear();
                cache.parts.clear();
                cache.traces.clear();
            }
        }
        self.section_cache = Some(cache);
        result
    }
}

fn section_hit(
    hit: &search::Hit,
    songs: &[Value],
    compiled: &[Chart],
    members: &[power::Member],
    snaps: &[power::Snap],
    reference: &Value,
    shared: bool,
) -> Value {
    let mut row = super::arena::arena_hit(hit, songs, compiled, members, snaps);
    row["sectionPerformance"]["reference"] = reference.clone();
    if !shared {
        row["song"] = songs[0].clone();
        row["score"] = row["aggregate"]["songs"][0]["score"].clone();
        row["aggregate"] = Value::Null;
    }
    row
}
