use super::{
    room_search::{fixed_candidate, solver::Search, Cursor, Visit},
    room_ss_reference::{parse_accuracy, SongKey},
    Engine,
};
use crate::{
    clock,
    full::play::Accuracy,
    gekiso::{ss_reference::ss_room_threshold, Chart},
    master::Master,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

pub(super) struct Run {
    id: u64,
    key: SongKey,
    accuracy: Accuracy,
    orders: Vec<usize>,
    seeds: Vec<i32>,
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

fn judgement(input: &Value, players: &[Value]) -> Result<Accuracy, String> {
    let mut shared = parse_accuracy(input.get("accuracy"))?;
    for player in players {
        if let Some(own) = parse_accuracy(player.get("accuracy"))? {
            if shared.is_some_and(|a| {
                (a.just, a.great, a.miss_every) != (own.just, own.great, own.miss_every)
            }) {
                return Err("room seats must share one judgement profile".into());
            }
            shared = Some(own);
        }
    }
    let a = shared.unwrap_or_default();
    if !(0.0..=1.0).contains(&a.just) || !(0.0..=1.0).contains(&a.great) || a.miss_every < 0 {
        return Err("invalid room accuracy".into());
    }
    Ok(a)
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
        let accuracy = judgement(input, players)?;
        let orders = seat_values::<usize>(input, players, "orderIndices", "orderIndex")?;
        let seeds = seat_values::<i32>(input, players, "seeds", "seed")?;
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
                        | "accuracy"
                )
            }) {
                return Err(format!("unsupported room seat {} field: {field}", i + 1));
            }
            if let Some(allowed) = player.get("allowedDifficulties") {
                let allowed: Vec<i32> =
                    serde_json::from_value(allowed.clone()).map_err(|e| e.to_string())?;
                if !allowed.contains(&key.difficulty) {
                    return Err(format!(
                        "seat {} cannot play the selected difficulty",
                        i + 1
                    ));
                }
            }
            if orders[i] >= crate::live::orders().len() {
                return Err(format!("seat {} skill order out of range", i + 1));
            }
        }
        self.load_source(input, &input["song"])?;
        let cache_key = (key.song_id, key.difficulty);
        let cached = &self.charts[&cache_key];
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
        let mut chart = Chart::new(
            &self.master,
            &self.sources[&cache_key],
            cached.difficulty.play_level,
            cached.song.missions.clone(),
            accuracy,
            vec![1; 3],
        )?;
        chart.music_type = cached.song.music_type;
        let ss = ss_room_threshold(&self.master, &cached.song, players.len());
        let mut search = Search::new(chart, orders.clone(), seeds.clone())?;
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
            key,
            accuracy,
            orders,
            seeds,
            ss,
            search,
            inventories,
            discovered,
            next_seat: 0,
            collecting: true,
            failure: None,
        };
        let out = run.advance(&self.master, budget, started, generation)?;
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
        out["songId"] = json!(self.key.song_id);
        out["difficulty"] = json!(self.key.difficulty);
        out["release"] = json!(master.release_id);
        out["accuracy"] = json!(self.accuracy);
        out["orderIndices"] = json!(self.orders);
        out["seeds"] = json!(self.seeds);
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
