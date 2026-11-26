mod sampling;
use super::{
    room_search::{
        fixed_candidate,
        solver::{Draw, Objective, Search},
        Cursor, Visit,
    },
    room_ss_reference::{parse_accuracy, SongKey},
    Engine,
};
use crate::{
    clock,
    full::play::Accuracy,
    gekiso::{ss_reference::ss_room_threshold, Chart},
    master::Master,
};
use sampling::Sampling;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

pub(super) struct Run {
    id: u64,
    conditions: Value,
    validation: bool,
    ss: Result<i64, String>,
    search: Search,
    inventories: Vec<Option<Cursor>>,
    discovered: Vec<bool>,
    next_seat: usize,
    collecting: bool,
    failure: Option<String>,
}

struct Budget {
    ms: f64,
    work: u64,
}

impl Budget {
    fn read(input: &Value) -> Result<Self, String> {
        let ms = input
            .get("budgetMs")
            .map(|v| {
                v.as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .ok_or("budgetMs must be nonnegative")
            })
            .transpose()?
            .unwrap_or(16.0);
        let work = input
            .get("maxWork")
            .map(|v| v.as_u64().ok_or("maxWork must be a nonnegative integer"))
            .transpose()?
            .unwrap_or(u64::MAX);
        Ok(Self { ms, work })
    }
}

fn checked_accuracy(raw: Option<&Value>) -> Result<Option<Accuracy>, String> {
    let parsed = parse_accuracy(raw)?;
    if parsed.is_some_and(|a| {
        !(0.0..=1.0).contains(&a.just) || !(0.0..=1.0).contains(&a.great) || a.miss_every < 0
    }) {
        return Err("invalid room accuracy".into());
    }
    Ok(parsed)
}

struct SeatConditions {
    difficulty: i32,
    profiles: Vec<Accuracy>,
}

impl SeatConditions {
    fn read(
        player: &Value,
        fallback: i32,
        accuracy: Accuracy,
        sampled: bool,
    ) -> Result<Self, String> {
        let difficulty = player
            .get("difficulty")
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()
            .map_err(|e| format!("difficulty: {e}"))?
            .unwrap_or(fallback);
        if let Some(allowed) = player.get("allowedDifficulties") {
            let allowed: Vec<i32> =
                serde_json::from_value(allowed.clone()).map_err(|e| e.to_string())?;
            if !allowed.contains(&difficulty) {
                return Err("cannot play the selected difficulty".into());
            }
        }
        let profiles = match player.get("accuracyProfiles") {
            Some(v) => {
                if !sampled || player.get("accuracy").is_some() {
                    return Err(
                        "accuracyProfiles requires sampling and replaces seat accuracy".into(),
                    );
                }
                let rows = v
                    .as_array()
                    .filter(|v| (1..=16).contains(&v.len()))
                    .ok_or("supply 1 to 16 equally weighted accuracyProfiles")?;
                rows.iter()
                    .map(|row| checked_accuracy(Some(row)).map(|a| a.unwrap()))
                    .collect::<Result<Vec<_>, _>>()?
            }
            None => vec![checked_accuracy(player.get("accuracy"))?.unwrap_or(accuracy)],
        };
        Ok(Self {
            difficulty,
            profiles,
        })
    }
}

fn seat_values<T: Copy + Default + PartialEq + DeserializeOwned>(
    input: &Value,
    players: &[Value],
    list: &str,
    field: &str,
) -> Result<Vec<T>, String> {
    let values: Option<Vec<T>> = input
        .get(list)
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()
        .map_err(|e| format!("{list}: {e}"))?;
    if values.as_ref().is_some_and(|v| v.len() != players.len()) {
        return Err(format!("{list} must have one entry per seat"));
    }
    players
        .iter()
        .enumerate()
        .map(|(i, player)| {
            let own = player
                .get(field)
                .map(|v| serde_json::from_value::<T>(v.clone()))
                .transpose()
                .map_err(|e| format!("seat {} {field}: {e}", i + 1))?;
            let listed = values.as_ref().map(|v| v[i]);
            if matches!((own,listed),(Some(a),Some(b)) if a!=b) {
                return Err(format!("seat {} {field} conflicts with {list}", i + 1));
            }
            Ok(own.or(listed).unwrap_or_default())
        })
        .collect()
}

impl Engine {
    pub(crate) fn room_search_start(
        &mut self,
        input: &Value,
        started: f64,
        generation: u64,
    ) -> Result<Value, String> {
        let budget = Budget::read(input)?;
        if let Some(field) = input.as_object().and_then(|obj| {
            obj.keys().find(|k| {
                !matches!(
                    k.as_str(),
                    "op" | "handle"
                        | "song"
                        | "players"
                        | "accuracy"
                        | "orderIndices"
                        | "seeds"
                        | "budgetMs"
                        | "maxWork"
                        | "charts"
                        | "sampling"
                        | "includeSamples"
                        | "objective"
                )
            })
        }) {
            return Err(format!("unsupported roomSearchStart field: {field}"));
        }
        let key: SongKey =
            serde_json::from_value(input["song"].clone()).map_err(|e| e.to_string())?;
        let players = input["players"]
            .as_array()
            .filter(|rows| (2..=5).contains(&rows.len()))
            .ok_or("a cooperative room requires two to five players")?;
        let accuracy = checked_accuracy(input.get("accuracy"))?.unwrap_or_default();
        let sampling: Option<Sampling> = input
            .get("sampling")
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()
            .map_err(|e| format!("sampling: {e}"))?;
        if sampling.is_some_and(|s| !(1..=4096).contains(&s.count)) {
            return Err("sampling.count must be between 1 and 4096".into());
        }
        let validation = sampling.is_some_and(Sampling::validation);
        if validation && players.iter().any(|p| p["lockedTeam"].is_null()) {
            return Err("independent validation requires every team to be locked".into());
        }
        let include_samples = input
            .get("includeSamples")
            .map(|v| v.as_bool().ok_or("includeSamples must be boolean"))
            .transpose()?
            .unwrap_or(false);
        let objective_name = input
            .get("objective")
            .map(|v| v.as_str().ok_or("invalid room objective"))
            .transpose()?
            .unwrap_or("meanScore");
        let objective = match objective_name {
            "meanScore" => Objective::MeanScore,
            "ssRate" => Objective::SsRate,
            _ => return Err("objective must be meanScore or ssRate".into()),
        };
        let orders = seat_values::<usize>(input, players, "orderIndices", "orderIndex")?;
        let seeds = seat_values::<i32>(input, players, "seeds", "seed")?;
        let mut conditions = Vec::with_capacity(players.len());
        for (i, player) in players.iter().enumerate() {
            let fields = player.as_object().ok_or("expected a room seat object")?;
            if let Some(field) = fields.keys().find(|k| {
                !matches!(
                    k.as_str(),
                    "members"
                        | "snaps"
                        | "player"
                        | "constraints"
                        | "unknownPolicy"
                        | "lockedTeam"
                        | "orderIndex"
                        | "seed"
                        | "allowedDifficulties"
                        | "difficulty"
                        | "accuracyProfiles"
                        | "accuracy"
                )
            }) {
                return Err(format!("unsupported room seat {} field: {field}", i + 1));
            }
            conditions.push(
                SeatConditions::read(player, key.difficulty, accuracy, sampling.is_some())
                    .map_err(|e| format!("seat {} {e}", i + 1))?,
            );
            if orders[i] >= crate::live::orders().len() {
                return Err(format!("seat {} skill order out of range", i + 1));
            }
        }
        let mut loaded = std::collections::BTreeSet::new();
        for c in &conditions {
            if loaded.insert(c.difficulty) {
                let song = json!({"songId":key.song_id,"difficulty":c.difficulty});
                self.load_source(input, &song)?;
            }
        }
        let cached = &self.charts[&(key.song_id, conditions[0].difficulty)];
        let &[a, b, c] = cached.song.missions.as_slice() else {
            return Err("a cooperative room requires three section missions".into());
        };
        if ![a, b, c].iter().all(|m| (1..=3).contains(m)) {
            return Err("unknown cooperative room mission".into());
        }
        let pattern = crate::full::gekisou::mission_pattern(a, b, c);
        for section in 0..3 {
            for rank in 1..=players.len() {
                crate::gekiso::ss_reference::require_bonus(
                    &self.master,
                    pattern,
                    section,
                    rank as i32,
                )?;
            }
        }
        let ss = ss_room_threshold(&self.master, &cached.song, players.len());
        let mut chart_keys = Vec::new();
        let mut charts = Vec::new();
        let mut seat_charts = Vec::new();
        for seat in &conditions {
            let cache_key = (key.song_id, seat.difficulty);
            let cached = &self.charts[&cache_key];
            let mut indices = Vec::new();
            for &profile in &seat.profiles {
                let profile_key = (seat.difficulty, profile);
                let index = match chart_keys.iter().position(|k| *k == profile_key) {
                    Some(index) => index,
                    None => {
                        let mut chart = Chart::new(
                            &self.master,
                            &self.sources[&cache_key],
                            cached.difficulty.play_level,
                            cached.song.missions.clone(),
                            profile,
                            vec![1; 3],
                        )?;
                        chart.music_type = cached.song.music_type;
                        chart_keys.push(profile_key);
                        charts.push(chart);
                        charts.len() - 1
                    }
                };
                indices.push(index);
            }
            seat_charts.push(indices);
        }
        let samples = match sampling {
            Some(plan) => plan.draws(&seat_charts, &orders, &seeds),
            None => vec![(0..players.len())
                .map(|i| Draw {
                    chart: seat_charts[i][0],
                    order: orders[i],
                    seed: seeds[i],
                })
                .collect()],
        };
        let sample_draws = include_samples.then(|| samples.iter().map(|sample| {
            sample.iter().map(|draw| {
                let (difficulty, accuracy) = chart_keys[draw.chart];
                json!({"difficulty":difficulty,"accuracy":accuracy,"orderIndex":draw.order,"seed":draw.seed})
            }).collect::<Vec<_>>()
        }).collect::<Vec<_>>());
        let metadata = json!({
            "songId":key.song_id,
            "difficulty":(loaded.len()==1).then_some(conditions[0].difficulty),
            "accuracy":chart_keys.iter().all(|k| k.1==chart_keys[0].1).then_some(chart_keys[0].1),
            "seatConditions":conditions.iter().enumerate().map(|(i,c)|
                json!({"difficulty":c.difficulty,"accuracyProfiles":c.profiles,"orderIndex":orders[i],"seed":seeds[i]})
            ).collect::<Vec<_>>(),
            "orderIndices":samples[0].iter().map(|d| d.order).collect::<Vec<_>>(),
            "seeds":samples[0].iter().map(|d| d.seed).collect::<Vec<_>>(),
            "sampling":sampling, "objective":objective_name,
        });
        let mut search = Search::new(charts, samples, objective, ss.as_ref().ok().copied())?;
        let mut inventories = Vec::new();
        let mut discovered = Vec::new();
        for (i, player) in players.iter().enumerate() {
            let cursor = match player.get("lockedTeam").filter(|v| !v.is_null()) {
                Some(team) => {
                    let c = fixed_candidate(&self.master, key.song_id, player, team)
                        .map_err(|e| format!("seat {}: {e}", i + 1))?;
                    search.push_candidate(i, c);
                    None
                }
                None => Some(
                    Cursor::new(&self.master, key.song_id, player)
                        .map_err(|e| format!("seat {}: {e}", i + 1))?
                        .seed_large_box(&self.master),
                ),
            };
            discovered.push(cursor.is_none());
            inventories.push(cursor);
        }
        let id = self.cooperative_room.as_ref().map_or(Ok(1), |r| {
            r.id.checked_add(1).ok_or("room search ID overflow")
        })?;
        let mut run = Run {
            id,
            conditions: metadata,
            validation,
            ss,
            search,
            inventories,
            discovered,
            next_seat: 0,
            collecting: true,
            failure: None,
        };
        let mut out = run.advance(&self.master, budget, started, generation)?;
        if let Some(draws) = sample_draws {
            out["sampleDraws"] = json!(draws);
        }
        self.cooperative_room = Some(Box::new(run));
        Ok(out)
    }

    pub(crate) fn room_search_step(
        &mut self,
        input: &Value,
        started: f64,
        generation: u64,
    ) -> Result<Value, String> {
        if input.as_object().is_some_and(|fields| {
            fields.keys().any(|k| {
                !matches!(
                    k.as_str(),
                    "op" | "handle" | "searchId" | "maxWork" | "budgetMs"
                )
            })
        }) {
            return Err(
                "roomSearchStep only accepts the existing search ID and a new budget".into(),
            );
        }
        let budget = Budget::read(input)?;
        let run = self
            .cooperative_room
            .as_mut()
            .ok_or("room search not started")?;
        if input["searchId"].as_u64() != Some(run.id) {
            return Err("unknown or stale room search ID".into());
        }
        if let Some(err) = &run.failure {
            return Err(err.clone());
        }
        run.advance(&self.master, budget, started, generation)
            .inspect_err(|err| {
                run.failure = Some(err.clone());
            })
    }
}

impl Run {
    fn unit(&mut self, master: &Master) -> Result<(), String> {
        if !self.collecting || self.search.has_probe() {
            return self.search.step_one(master);
        }
        let n = self.inventories.len();
        let Some(i) = (0..n)
            .map(|offset| (offset + self.next_seat) % n)
            .find(|&i| self.inventories[i].is_some())
        else {
            self.search.finish_inventory()?;
            self.collecting = false;
            return Ok(());
        };
        let cursor = self.inventories[i].as_mut().unwrap();
        match cursor.advance(master)? {
            Visit::Advanced => {}
            Visit::Candidate(c) => {
                self.discovered[i] = true;
                self.search.push_candidate(i, c);
            }
            Visit::Seed(c) => {
                self.discovered[i] = true;
                self.search.push_seed(i, c);
            }
            Visit::Finished => {
                if !self.discovered[i] {
                    return Err(format!("seat {} has no legal formation", i + 1));
                }
                self.inventories[i] = None;
            }
        }
        self.next_seat = (i + 1) % n;
        Ok(())
    }

    fn advance(
        &mut self,
        master: &Master,
        budget: Budget,
        started: f64,
        generation: u64,
    ) -> Result<Value, String> {
        let mut work = 0;
        let stop = loop {
            if self.search.complete() {
                break "complete";
            }
            if generation != crate::cancellation_generation() {
                break "cancelled";
            }
            if work >= budget.work {
                break "workLimit";
            }
            if work > 0 && clock::milliseconds() - started >= budget.ms {
                break "timeLimit";
            }
            match self.unit(master) {
                Err(e) if e == "cancelled" => break "cancelled",
                result => result?,
            }
            work += 1;
        };
        let mut out = self.search.response();
        out["searchId"] = json!(self.id);
        out.as_object_mut()
            .unwrap()
            .extend(self.conditions.as_object().unwrap().clone());
        out["release"] = json!(master.release_id);
        if self.validation {
            if let Some(hits) = out["statistics"]["ssHits"].as_u64() {
                let n = out["statistics"]["sampleCount"].as_u64().unwrap();
                out["statistics"]["ssRateWilson95"] = json!(sampling::wilson95(hits, n));
            }
        }
        out["stopReason"] = json!(stop);
        out["stepWork"] = json!(work);
        out["ss"] = match self.ss {
            Ok(line) => {
                let total = out["room"]["total"].as_i64();
                json!({"roomThreshold":line,"achieved":total.map(|s|s>=line),"pointsNeeded":total.map(|s|(line-s).max(0))})
            }
            Err(ref reason) => json!({"unavailable":reason}),
        };
        Ok(out)
    }
}
