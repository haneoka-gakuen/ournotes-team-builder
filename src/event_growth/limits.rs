//! Attainable character ranks from the pinned catalogue and authored EXP missions.
use crate::{
    master::{self, Master},
    power,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn time(value: &Value) -> f64 {
    if let Some(rows) = value.as_array() {
        return rows
            .iter()
            .map(time)
            .find(|value| *value > 0.0)
            .unwrap_or(0.0);
    }
    master::number(value, 0.0)
}
fn published(value: &Value, cutoff: f64) -> bool {
    if value["__unreleasedSource"] == true || value["__publicationPending"] == true {
        return false;
    }
    let starts = [
        "publicStartAt",
        "releasedAt",
        "publishedAt",
        "releaseAt",
        "startAt",
    ]
    .iter()
    .find_map(|key| (!value[*key].is_null()).then_some(&value[*key]));
    let event = time(&value["eventPublishedAt"]);
    starts.is_none_or(|value| time(value) <= cutoff || event > 0.0 && event <= cutoff)
        && (value["catalogMetadata"].is_null() || published(&value["catalogMetadata"], cutoff))
}
#[derive(Default)]
struct Available {
    members: Vec<i32>,
    snaps: Vec<i32>,
    stamps: usize,
    friendship: i32,
    songs: usize,
}
fn ids(value: &Value, key: &str, id: i32) -> bool {
    master::rows(&value[key])
        .iter()
        .any(|value| master::int(value) == id)
}
fn available(
    data: &Value,
    sources: &Value,
    character: i32,
    cutoff: f64,
    friend_cap: i32,
) -> Available {
    let members: Vec<_> = master::rows(&data["members"])
        .into_iter()
        .filter(|row| master::int(&row["characterId"]) == character && published(row, cutoff))
        .map(|row| master::int(&row["id"]))
        .collect();
    let snaps = master::rows(&data["snapshots"])
        .into_iter()
        .filter(|row| ids(row, "characterIds", character) && published(row, cutoff))
        .map(|row| master::int(&row["id"]))
        .collect();
    let mut songs: BTreeSet<_> = master::rows(&data["songs"])
        .into_iter()
        .filter(|row| published(row, cutoff))
        .map(|row| master::int(&row["musicId"]))
        .collect();
    for challenge in master::rows(&data["challengeMusicTable"]["rows"]) {
        let event = &data["events"][master::int(&challenge["eventId"]).to_string()];
        let start = time(&challenge["stratAt"]);
        let start = if start > 0.0 {
            start
        } else {
            time(&event["startAt"])
        };
        if start > 0.0 && start <= cutoff {
            songs.insert(master::int(&challenge["liveMusicId"]));
        }
    }
    let stamps = master::rows(&sources["stamps"])
        .into_iter()
        .filter(|row| ids(row, "characterIds", character) && published(row, cutoff))
        .count();
    let friendship = master::rows(&sources["friendships"])
        .into_iter()
        .filter(|row| ids(row, "characterIds", character))
        .filter(|row| {
            master::rows(&row["characterIds"]).into_iter().all(|id| {
                master::rows(&data["members"])
                    .into_iter()
                    .any(|card| card["characterId"] == *id && published(card, cutoff))
            })
        })
        .count() as i32
        * friend_cap;
    Available {
        members,
        snaps,
        stamps,
        friendship,
        songs: songs.len(),
    }
}
pub(super) fn calculate(
    master: &Master,
    data: &Value,
    sources: &Value,
    as_of: f64,
) -> Result<BTreeMap<i32, i32>, String> {
    let friend_cap = master::rows(&sources["friendshipRanks"])
        .into_iter()
        .map(|row| master::int(&row["rank"]))
        .max()
        .unwrap_or(0);
    let missions = master::rows(&sources["missions"]);
    if missions.is_empty() || friend_cap == 0 {
        return Err("character-growth-rules-missing".into());
    }
    let mut cache = BTreeMap::new();
    let mut caps = BTreeMap::new();
    for &character in master.characters.keys() {
        let mut exp = 0.0;
        for mission in &missions {
            if time(&mission["startAt"]) > as_of {
                continue;
            }
            let reward: f64 = master::rows(&mission["rewards"])
                .into_iter()
                .filter(|row| row["resourceType"] == 1 && row["resourceId"] == 4)
                .map(|row| master::number(&row["resourceCount"], 0.0))
                .sum();
            if reward == 0.0 {
                continue;
            }
            let end = time(&mission["endAt"]);
            let cutoff = if end > 0.0 { as_of.min(end) } else { as_of };
            let collection = cache
                .entry((character, cutoff as i64))
                .or_insert_with(|| available(data, sources, character, cutoff, friend_cap));
            let value = master::int(&mission["value"]);
            let maximum = match master::int(&mission["missionType"]) {
                12 => collection.members.len() as i32,
                14 => collection.snaps.len() as i32,
                16 => collection.stamps as i32,
                25 => collection.friendship,
                80 => i32::MAX,
                111 => collection.songs as i32,
                kind @ (42 | 48 | 54) => collection
                    .members
                    .iter()
                    .map(|id| {
                        let card = &master.members[id];
                        let growth = power::max_member_growth(master, card);
                        match kind {
                            48 => i32::from(growth.awake >= value),
                            54 => i32::from(growth.rank >= value),
                            _ => {
                                let leader = master
                                    .member_ranks
                                    .get(&card.rank_group)
                                    .into_iter()
                                    .flat_map(|rows| rows.values())
                                    .map(|row| row.leader_skill_level)
                                    .max()
                                    .unwrap_or(1);
                                i32::from(leader >= value)
                                    + i32::from(growth.live_skill_level >= value)
                                    + i32::from(growth.gekiso_skill_level >= value)
                            }
                        }
                    })
                    .sum(),
                66 => collection
                    .snaps
                    .iter()
                    .filter(|id| power::max_snap_growth(master, &master.snaps[id]).rank >= value)
                    .count() as i32,
                _ => return Err("character-growth-mission-unsupported".into()),
            };
            if maximum >= master::int(&mission["achievementCount"]) {
                exp += reward;
            }
        }
        let rank = master::rows(&data["progression"]["characterRanks"])
            .into_iter()
            .filter(|row| master::number(&row["exp"], 0.0) <= exp)
            .map(|row| master::int(&row["rank"]))
            .max()
            .ok_or("character-growth-rank-missing")?;
        caps.insert(character, rank);
    }
    Ok(caps)
}
