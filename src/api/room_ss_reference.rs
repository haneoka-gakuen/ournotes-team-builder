use super::{room_search, Engine};
use crate::{
    full::{conditions::Performer, play::Accuracy},
    gekiso::{
        room::Seat,
        ss_reference::{
            neutral::{neutral_crossing, CrossingError},
            reference, RankSchedule, FIXED_RANKS,
        },
        Chart,
    },
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SongKey {
    pub(super) song_id: i32,
    pub(super) difficulty: i32,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct Scenario {
    accuracy: Option<Value>,
    order_index: usize,
    seed: i32,
    rank_schedule: Option<RankSchedule>,
}

pub(super) fn parse_accuracy(input: Option<&Value>) -> Result<Option<Accuracy>, String> {
    input
        .map(|value| {
            let fields = value
                .as_object()
                .ok_or("reference accuracy must be an object")?;
            if let Some(field) = fields
                .keys()
                .find(|field| !matches!(field.as_str(), "great" | "just" | "missEvery"))
            {
                return Err(format!("unknown reference accuracy field: {field}"));
            }
            serde_json::from_value(value.clone()).map_err(|e| e.to_string())
        })
        .transpose()
}

impl Engine {
    /// Per-song SS estimates under an explicit five-player rank history and shared reference team.
    pub fn room_ss_reference(&mut self, input: &Value) -> Result<Value, String> {
        let key: SongKey =
            serde_json::from_value(input["song"].clone()).map_err(|e| e.to_string())?;
        let scenario: Scenario = input
            .get("scenario")
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        let request = input.get("request");
        if let Some(settings) = request {
            for field in ["goal", "songRules", "gekiso", "arena"] {
                if !settings[field].is_null() {
                    return Err(format!("room SS reference does not accept request.{field}; use song and scenario for the room conditions"));
                }
            }
        }
        let scenario_accuracy = parse_accuracy(scenario.accuracy.as_ref())?;
        let own_accuracy = parse_accuracy(request.and_then(|r| r.get("accuracy")))?;
        if let (Some(a), Some(b)) = (scenario_accuracy, own_accuracy) {
            if (a.just, a.great, a.miss_every) != (b.just, b.great, b.miss_every) {
                return Err("reference team requests different judgement conditions".into());
            }
        }
        let accuracy = scenario_accuracy.or(own_accuracy).unwrap_or_default();
        if !(0.0..=1.0).contains(&accuracy.great)
            || !(0.0..=1.0).contains(&accuracy.just)
            || accuracy.miss_every < 0
        {
            return Err("invalid reference accuracy".into());
        }
        if scenario.order_index >= crate::live::orders().len() {
            return Err("reference skill order out of range".into());
        }
        if let Some(allowed) = request.and_then(|r| r.get("allowedDifficulties")) {
            let allowed: Vec<i32> =
                serde_json::from_value(allowed.clone()).map_err(|e| e.to_string())?;
            if !allowed.contains(&key.difficulty) {
                return Err("reference team cannot play the selected difficulty".into());
            }
        }
        let candidate = match (request, input.get("team")) {
            (Some(request), Some(team)) => Some(room_search::fixed_candidate(
                &self.master,
                key.song_id,
                request,
                team,
            )?),
            (None, None) => None,
            _ => {
                return Err("supply both request inventory and team for an owned reference".into())
            }
        };
        self.load_source(input, &input["song"])?;
        let chart_key = (key.song_id, key.difficulty);
        let cached = &self.charts[&chart_key];
        let song = &cached.song;
        let mut chart = Chart::new(
            &self.master,
            &self.sources[&chart_key],
            cached.difficulty.play_level,
            song.missions.clone(),
            accuracy,
            vec![1; 3],
        )?;
        chart.music_type = song.music_type;
        let neutral = std::array::from_fn::<_, 5, _>(|_| Performer::default());
        let rank_schedule = scenario.rank_schedule.unwrap_or(FIXED_RANKS);
        let seat = match &candidate {
            Some(c) => c.seat(scenario.order_index, scenario.seed),
            None => Seat {
                deck: &neutral,
                power: 1_000_000,
                order_index: scenario.order_index,
                seed: scenario.seed,
            },
        };
        let result = reference(&self.master, &chart, song, seat, rank_schedule)?;
        let mut out = json!({
            "songId":key.song_id,"difficulty":key.difficulty,"missions":song.missions,
            "referenceProfile":if candidate.is_some() {"ownedTeam"} else {"neutral"},
            "referencePower":seat.power,"accuracy":accuracy,"orderIndex":scenario.order_index,
            "seed":scenario.seed,"reference":result,
        });
        if let Some(c) = candidate {
            out["team"] = c.team;
        } else {
            match neutral_crossing(&self.master, &chart, song, seat, rank_schedule, &result) {
                Ok(crossing) => out["neutralCrossing"] = json!(crossing),
                Err(CrossingError::Unavailable(reason)) => {
                    out["neutralCrossing"] = Value::Null;
                    out["neutralCrossingUnavailable"] = json!(reason);
                }
                Err(CrossingError::Replay(err)) => return Err(err),
            }
        }
        Ok(out)
    }
}
