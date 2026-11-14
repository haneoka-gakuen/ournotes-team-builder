//! Arena Master overrides are scoped to one call; raw chart cache entries never change identity.
use super::*;
use crate::{gekiso, gekiso_objective, search};

fn scoped_input(input: &Value) -> Value {
    Value::Object(input.as_object().unwrap().iter().filter(|(key, _)| key.as_str() != "charts")
        .map(|(key, value)| (key.clone(), value.clone())).collect())
}
impl Engine {
    fn call_view(&self, master: Master) -> Engine {
        let mut scoped = Engine::from_master(master);
        scoped.trusted_scores = self.trusted_scores;
        scoped.section_scenes=self.section_scenes.clone();
        #[cfg(not(target_arch = "wasm32"))]
        {
            scoped.worker_budget = self.worker_budget;
        }
        scoped
    }
    fn inherit_chart(&self, scoped: &mut Engine, key: (i32, i32)) {
        if scoped.charts.contains_key(&key) { return; }
        let mut chart = self.charts[&key].clone();
        chart.song = scoped.master.songs[&key.0].clone();
        scoped.charts.insert(key, chart);
        scoped.sources.insert(key, self.sources[&key].clone());
        scoped.chart_order.push_back(key);
    }

    pub(super) fn arena_view(
        &mut self,
        input: &Value,
    ) -> Result<(Engine, Value, Vec<Value>, Vec<Source>), String> {
        let request = &input["request"];
        let rule = [
            &request["goal"]["arena"],
            &request["gekiso"]["arena"],
            &request["arena"],
        ]
        .into_iter()
        .find(|v| v.is_object())
        .ok_or("arena-rule-missing")?;
        let band = rule["bandId"]
            .as_i64()
            .filter(|v| (1..=5).contains(v))
            .ok_or("arena-band-invalid")? as i32;
        let mut songs: Vec<Value> = master::rows(if request["goal"]["songs"].is_array() {
            &request["goal"]["songs"]
        } else {
            &request["songs"]
        })
        .into_iter()
        .cloned()
        .collect();
        if songs.is_empty() && request["song"].is_object() {
            songs.push(request["song"].clone());
        }
        if songs.is_empty() {
            return Err("arena-songs-missing".into());
        }
        let master = crate::song_rules::view(&self.master, rule)?;
        for song in &songs {
            let id = master::int(&song["songId"]);
            if !master
                .songs
                .get(&id)
                .ok_or("unknown song")?
                .band_ids
                .contains(&band)
            {
                return Err("arena-song-band".into());
            }
        }
        let mut scoped_input = scoped_input(input);
        let preload = songs.len() <= CHART_CACHE_LIMIT;
        let mut scoped = self.call_view(master);
        if rule["objective"]=="sections" {scoped_input["request"]["sectionArena"]=rule.clone();}
        let members: Vec<_> = master::rows(&request["members"])
            .into_iter()
            .filter(|row| {
                self.master
                    .members
                    .get(&master::int(&row["cardId"]))
                    .is_some_and(|m| m.band_id == band)
            })
            .cloned()
            .collect();
        scoped_input["request"]["members"] = json!(members);
        if scoped_input["request"]["goal"].is_object() {
            scoped_input["request"]["goal"]["arena"] = Value::Null;
        }
        if scoped_input["request"]["gekiso"].is_object() {
            scoped_input["request"]["gekiso"]["arena"] = Value::Null;
        }
        scoped_input["request"]["arena"] = Value::Null;
        let mut sources = Vec::with_capacity(songs.len());
        for song in &songs {
            self.load_source(input, song)?;
            if preload {
                self.inherit_chart(&mut scoped, (master::int(&song["songId"]), master::int(&song["difficulty"])));
            }
            sources.push(
                self.sources[&(
                    master::int(&song["songId"]),
                    master::int(&song["difficulty"]),
                )]
                    .clone(),
            );
        }
        scoped_input["charts"] = if preload { json!([]) } else { json!(songs
            .iter()
            .zip(&sources)
            .map(|(song, source)| json!({"song":song,"source":source}))
            .collect::<Vec<_>>()) };
        Ok((scoped, scoped_input, songs, sources))
    }

    pub(super) fn song_rule_view(&mut self, input: &Value) -> Result<(Engine, Value), String> {
        let request = &input["request"];
        let master = crate::song_rules::view(&self.master, &request["songRules"])?;
        let mut scoped = scoped_input(input);
        scoped["request"]["songRules"] = Value::Null;
        let mut songs: Vec<Value> = [
            &request["goal"]["songs"],
            &request["goal"]["normalSongs"],
            &request["goal"]["challengeSongs"],
            &request["goal"]["rewardPlan"]["autoCp"]["songs"],
            &request["songs"],
        ]
        .into_iter()
        .flat_map(master::rows)
        .cloned()
        .collect();
        for song in [&request["goal"]["song"], &request["song"]] {
            if song.is_object() {
                songs.push(song.clone());
            }
        }
        let preload = songs.len() <= CHART_CACHE_LIMIT;
        let mut engine = self.call_view(master);
        let mut charts = Vec::new();
        for song in songs {
            self.load_source(input, &song)?;
            if preload {
                self.inherit_chart(&mut engine, (master::int(&song["songId"]), master::int(&song["difficulty"])));
                continue;
            }
            let source = self
                .sources
                .get(&(
                    master::int(&song["songId"]),
                    master::int(&song["difficulty"]),
                ))
                .unwrap()
                .clone();
            charts.push(json!({"song":song,"source":source}));
        }
        scoped["charts"] = json!(charts);
        Ok((engine, scoped))
    }

    pub(super) fn run_arena(
        &mut self,
        input: &Value,
        started: f64,
        now: &dyn Fn() -> f64,
    ) -> Result<Value, String> {
        if input["request"]["goal"]["arena"]["objective"] == "sections" {
            return self.run_arena_sections(input, started, now);
        }
        let arena = input["request"]["goal"]["arena"].is_object();
        let (mut scoped, scoped_input, songs, sources) = if arena {
            self.arena_view(input)?
        } else {
            let songs: Vec<_> = master::rows(&input["request"]["goal"]["songs"])
                .into_iter()
                .cloned()
                .collect();
            let mut sources = Vec::with_capacity(songs.len());
            for song in &songs {
                self.load_source(input, song)?;
                sources.push(
                    self.sources[&(
                        master::int(&song["songId"]),
                        master::int(&song["difficulty"]),
                    )]
                        .clone(),
                );
            }
            (
                self.call_view(self.master.clone()),
                input.clone(),
                songs,
                sources,
            )
        };
        if arena && input["request"]["goal"]["arena"]["mode"] != "shared" {
            let mut result = scoped.run(&scoped_input, started, now)?;
            result["cachedCharts"] = json!(self.cached_chart_keys());
            return Ok(result);
        }
        let _slots = crate::objectives::slot_table_scope();
        let request = &scoped_input["request"];
        let goal = &request["goal"];
        if goal["criterion"] != "mean" || goal["opponents"].is_array() {
            return Err("arena-shared-mean-required".into());
        }
        let cards = search_box(&scoped.master, request)?;
        crate::progress::keys(&cards.members, &cards.snaps);
        let player = power::player_state(&scoped.master, &request["player"]);
        let accuracy =
            serde_json::from_value(goal["accuracy"].clone()).map_err(|e| e.to_string())?;
        let mut charts = Vec::with_capacity(songs.len());
        let mut music = Vec::with_capacity(songs.len());
        let mut compiled = Vec::with_capacity(songs.len());
        for (song, source) in songs.iter().zip(&sources) {
            let id = master::int(&song["songId"]);
            let difficulty = master::int(&song["difficulty"]);
            let item = scoped.master.songs.get(&id).ok_or("unknown song")?;
            let level = item
                .difficulties
                .iter()
                .find(|row| row.difficulty == difficulty)
                .ok_or("unknown difficulty")?;
            let view = power::music_view(&scoped.master, id, None)?;
            let mut chart = gekiso::Chart::new(
                &scoped.master,
                source,
                level.play_level,
                item.missions.clone(),
                accuracy,
                gekiso_ranks(goal),
            )?;
            chart.music_type = view.music_type;
            charts.push(chart);
            music.push(view);
            compiled.push(chart::compile(&scoped.master, item, level, source));
        }
        let deadline = request["deadline"].as_f64().unwrap_or_else(|| {
            request["timeLimitMs"]
                .as_f64()
                .map(|ms| started + ms)
                .unwrap_or(f64::INFINITY)
        });
        let initial = seed_teams(request, &Value::Null, &cards.members, &cards.snaps);
        let shard = request["shard"].is_object().then(|| {
            (
                master::int(&request["shard"]["index"]) as usize,
                master::int(&request["shard"]["count"]) as usize,
            )
        });
        let mut publish = |hits: &[search::Hit]| {
            crate::progress::emit_document(
                &json!({"song":null,"hits":hits.iter().map(|hit|arena_hit(hit, &songs, &compiled, &cards.members, &cards.snaps)).collect::<Vec<_>>()}),
            );
        };
        let minimum_just_window = crate::just_window::Policy::read(goal)?;
        let floor = if self.trusted_scores && minimum_just_window.is_none() {
            request["floors"][format!(
                "{}:{}",
                master::int(&songs[0]["songId"]),
                master::int(&songs[0]["difficulty"])
            )]
            .as_f64()
            .unwrap_or(f64::NEG_INFINITY)
        } else {
            f64::NEG_INFINITY
        };
        let seeded = self.trusted_scores
            && minimum_just_window.is_none()
            && request["seeded"] == true
            && master::number(&request["k"], 1.0) == 1.0;
        if seeded && (!floor.is_finite() || initial.is_empty()) {
            return Err("shared-seed-invalid".into());
        }
        let output = gekiso_objective::search_shared(gekiso_objective::SharedInput {
            env: gekiso_objective::Environment {
                master: &scoped.master,
                player: &player,
                members: &cards.members,
                snaps: &cards.snaps,
                music: Some(&music[0]),
                constraints: constraints(&self.master, &request["constraints"], &cards.members, &cards.snaps),
                now,
                generation: crate::cancellation_generation(),
                window_policy: minimum_just_window,
            },
            charts: &mut charts,
            music: &music,
            seeds: (0..master::int(&goal["seeds"]).max(0)).collect(),
            k: master::number(&request["k"], 1.0).max(1.0) as usize,
            deadline,
            initial,
            shard,
            min_combo_bonus: master::int(&goal["minComboBonus"]),
            seeded,
            cooperative_floor: self.trusted_scores
                && request["cooperativeFloor"] == true
                && request["noFloor"] != true
                && minimum_just_window.is_none_or(|policy| arena && !policy.partial)
                && master::number(&request["k"], 1.0) == 1.0,
            floor,
            on_candidate: Some(&mut publish),
            sections: None,
        })?;
        let hits: Vec<_> = output
            .hits
            .iter()
            .map(|hit| arena_hit(hit, &songs, &compiled, &cards.members, &cards.snaps))
            .collect();
        let mut result = json!({"results":[{"song":null,"hits":hits,"proven":output.proven,"bound":output.bound,"stats":output.stats}],"overall":hits,"unknownCards":cards.unknown,"cachedCharts":self.cached_chart_keys(),"elapsedMs":now()-started,"stopReason":if output.proven {"complete"} else if now() >= deadline {"budget"} else {"cancelled"}});
        if seeded {
            result["results"][0]["floor"] = json!(floor);
        }
        Ok(result)
    }
}

pub(super) fn arena_hit(
    hit: &search::Hit,
    songs: &[Value],
    compiled: &[Chart],
    members: &[power::Member],
    snaps: &[power::Snap],
) -> Value {
    let parts: Vec<_> = master::rows(&hit.detail["songs"])
        .into_iter()
        .enumerate()
        .map(|(i, part)| {
            let child = search::Hit {
                team: hit.team.clone(),
                power: master::number(&part["power"], 0.0),
                slot_powers: master::rows(&part["slotPowers"])
                    .iter()
                    .map(|v| master::number(v, 0.0))
                    .collect(),
                key: master::number(&part["detail"]["scores"]["mean"], 0.0),
                detail: part["detail"].clone(),
                totals: search::Totals::default(),
            };
            let mut row = hit_json(&child, &songs[i], members, snaps, Some(&compiled[i]), true);
            row["comboBonus"] = child.detail["comboBonus"].clone();
            row["score"]["rangeScores"] = child.detail["scores"]["rangeScores"].clone();
            if !child.detail["sectionPerformance"].is_null() {
                row["sectionSummary"] = child.detail["sectionPerformance"].clone();
                row["score"]["rangeScores"] = child.detail["sectionPerformance"]["rangeScores"].clone();
            }
            row
        })
        .collect();
    let count = parts.len() as f64;
    let avg = |key: &str| {
        parts
            .iter()
            .map(|p| master::number(&p["score"][key], 0.0))
            .sum::<f64>()
            / count
    };
    let mut row = hit_json(hit, &Value::Null, members, snaps, None, true);
    row["power"] = json!(
        parts
            .iter()
            .map(|p| master::number(&p["power"], 0.0))
            .sum::<f64>()
            / count
    );
    row["slotPowers"] = json!((0..5)
        .map(|i| parts
            .iter()
            .map(|p| master::number(&p["slotPowers"][i], 0.0))
            .sum::<f64>()
            / count)
        .collect::<Vec<_>>());
    row["score"] = json!({"mean":avg("mean"),"min":avg("min"),"max":avg("max"),"median":avg("median"),"orders":[],"ranks":{},"bestOrder":[],"worstOrder":[]});
    if !hit.detail["sectionPerformance"].is_null() {
        row["sectionPerformance"] = hit.detail["sectionPerformance"].clone();
        row["sectionRanking"] = hit.detail["sectionTie"].clone();
    }
    row["aggregate"] = json!({"kind":"mean","songs":parts});
    row
}
