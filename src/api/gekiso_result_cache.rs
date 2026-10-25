//! One completed Gekiso proof per engine; request budgets do not change its domain.
use crate::chart::Source;
use serde_json::{json, Value};

use crate::master::Master;
use std::{
    any::Any,
    sync::{Arc, Weak},
};

type WeakTable = Weak<dyn Any + Send + Sync>;

// Exhaustive destructuring makes a new Master dependency a compile-time change.
macro_rules! master_identity {
    ($master:expr, $access:ident) => {{
        let Master {
            server,
            release_id,
            params,
            live,
            characters,
            members,
            snaps,
            member_levels,
            member_awake,
            member_ranks,
            member_level_limits,
            snap_levels,
            snap_ranks,
            character_rank_bonus,
            total_rank_bonus,
            max_character_rank,
            vip_bonus,
            band_items,
            targets,
            conditions,
            condition_sets,
            cumulative,
            leader_skills,
            live_skills,
            support_skills,
            gekiso_skills,
            gekiso_support_skills,
            live_categories,
            gekiso_categories,
            songs,
            score_ranks,
            challenge_music,
            events,
            live_challenge_points,
            boosts,
            challenge_boosts,
        } = $master;
        (
            server,
            release_id,
            max_character_rank,
            [
                $access(params),
                $access(live),
                $access(characters),
                $access(members),
                $access(snaps),
                $access(member_levels),
                $access(member_awake),
                $access(member_ranks),
                $access(member_level_limits),
                $access(snap_levels),
                $access(snap_ranks),
                $access(character_rank_bonus),
                $access(total_rank_bonus),
                $access(vip_bonus),
                $access(band_items),
                $access(targets),
                $access(conditions),
                $access(condition_sets),
                $access(cumulative),
                $access(leader_skills),
                $access(live_skills),
                $access(support_skills),
                $access(gekiso_skills),
                $access(gekiso_support_skills),
                $access(live_categories),
                $access(gekiso_categories),
                $access(songs),
                $access(score_ranks),
                $access(challenge_music),
                $access(events),
                $access(live_challenge_points),
                $access(boosts),
                $access(challenge_boosts),
            ],
        )
    }};
}

fn weak<T: Any + Send + Sync>(table: &Arc<T>) -> WeakTable {
    let erased: Arc<dyn Any + Send + Sync> = table.clone();
    Arc::downgrade(&erased)
}

fn pointer<T>(table: &Arc<T>) -> *const () {
    Arc::as_ptr(table).cast()
}

struct MasterIdentity {
    server: String,
    release_id: String,
    max_character_rank: i32,
    // Weak references prevent address reuse without keeping old table contents alive.
    tables: Vec<WeakTable>,
}

impl MasterIdentity {
    fn new(master: &Master) -> Self {
        let (server, release_id, max_character_rank, tables) = master_identity!(master, weak);
        Self {
            server: server.clone(),
            release_id: release_id.clone(),
            max_character_rank: *max_character_rank,
            tables: Vec::from(tables),
        }
    }

    fn matches(&self, master: &Master) -> bool {
        let (server, release_id, max_character_rank, tables) = master_identity!(master, pointer);
        self.server == *server
            && self.release_id == *release_id
            && self.max_character_rank == *max_character_rank
            && self
                .tables
                .iter()
                .map(|table| table.as_ptr() as *const ())
                .eq(tables)
    }
}

const PAYLOAD_LIMIT: usize = 1024 * 1024;

struct Entry {
    master: MasterIdentity,
    request: Value,
    source: Source,
    response: Value,
}

#[derive(Default)]
pub(super) struct CompletedGekiso {
    entry: Option<Box<Entry>>,
}

impl CompletedGekiso {
    pub(super) fn key(request: &Value) -> Option<Value> {
        if request["seedOnly"] == true
            || request["deferScores"] == true
            || !request["shard"].is_null()
        {
            return None;
        }
        let mut key = request.as_object()?.clone();
        key.remove("deadline");
        key.remove("timeLimitMs");
        key.remove("requestId");
        key.remove("contextKey");
        Some(Value::Object(key))
    }

    pub(super) fn get(&self, master: &Master, request: &Value, source: &Source) -> Option<Value> {
        self.entry
            .as_ref()
            .filter(|entry| {
                entry.master.matches(master) && &entry.request == request && &entry.source == source
            })
            .map(|entry| entry.response.clone())
    }

    pub(super) fn remember(
        &mut self,
        master: &Master,
        request: Value,
        source: &Source,
        response: &Value,
    ) {
        let complete = response["results"].as_array().is_some_and(|results| {
            !results.is_empty() && results.iter().all(|result| result["proven"] == true)
        });
        let settled = response["overall"].as_array().is_some_and(|hits| {
            hits.iter()
                .all(|hit| hit["pendingScores"] != true && !hit["score"].is_null())
        });
        if !complete || !settled {
            return;
        }
        // Bound retained JSON payload, including the chart; Value node overhead is separate.
        let bytes = request
            .to_string()
            .len()
            .saturating_add(
                json!([
                    &master.server,
                    &master.release_id,
                    master.max_character_rank
                ])
                .to_string()
                .len(),
            )
            .saturating_add(json!(source).to_string().len())
            .saturating_add(response.to_string().len());
        if bytes <= PAYLOAD_LIMIT {
            self.entry = Some(Box::new(Entry {
                master: MasterIdentity::new(master),
                request,
                source: source.clone(),
                response: response.clone(),
            }));
        }
    }

    pub(super) fn clear(&mut self) {
        self.entry = None;
    }
}
