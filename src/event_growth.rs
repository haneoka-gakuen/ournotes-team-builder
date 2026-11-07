//! Joint inverse power search over saved activity decks. Uses the same compiled
//! Master, growth resolution and integer power arithmetic as normal team building.
mod limits;
mod search;

use crate::{
    clock,
    master::{self, Master, Stat3},
    power::{self, Member, MemberGrowth, Music, Player, Snap, SnapGrowth},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    event_id: i32,
    as_of: f64,
    observations: Vec<ReadingInput>,
    sources: Value,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadingInput {
    challenge_id: i32,
    row: RowInput,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RowInput {
    profile_id: Option<String>,
    total_power: Option<i64>,
    cards: Vec<CardInput>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CardInput {
    slot: usize,
    member_card_id: Option<i32>,
    member_exp: Option<i64>,
    member_awake_count: Option<i32>,
    member_rank: Option<i32>,
    support_card_id: Option<i32>,
    support_exp: Option<i64>,
    support_rank: Option<i32>,
}
#[derive(Clone, Copy, Serialize)]
pub struct Range {
    pub min: i32,
    pub max: i32,
}
#[derive(Clone, Copy)]
struct Total {
    min: i32,
    max: i32,
    bonus: i32,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Character {
    id: i32,
    level: i32,
    min_level: i32,
    cap: i32,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Instrument {
    id: i32,
    total: i32,
    min_total: i32,
    cap: i32,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Witness {
    challenge_id: i32,
    levels: Vec<i32>,
    instruments: Vec<i32>,
    total_rank: Range,
    total_value: i32,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Equivalent {
    characters: Vec<Character>,
    instruments: Vec<Instrument>,
    total_rank: Range,
    witnesses: Vec<Witness>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Configuration {
    id: String,
    characters: Vec<Character>,
    instruments: Vec<Instrument>,
    total_rank: Range,
    vip_bonus: i32,
    matched_challenges: Vec<i32>,
    relaxed: bool,
    changed: bool,
    change_cost: i32,
    changed_dimensions: usize,
    witnesses: Vec<Witness>,
    equivalents: Vec<Equivalent>,
}
#[derive(Clone)]
struct Reading {
    challenge: i32,
    target: i64,
    team: Vec<Member>,
    snaps: Vec<Option<Snap>>,
    music: Music,
    percents: Vec<Stat3>,
    characters: Vec<usize>,
    bands: Vec<usize>,
    signature: String,
}
struct Band {
    id: i32,
    items: Vec<(i32, i32)>,
    cap: i32,
}
struct Problem {
    master: Master,
    event: i32,
    readings: Vec<Reading>,
    originals: Vec<(i32, usize)>,
    ids: Vec<i32>,
    caps: Vec<i32>,
    all_caps: BTreeMap<i32, i32>,
    unseen_cap: i32,
    unseen_count: i32,
    bands: Vec<Band>,
    totals: Vec<Total>,
    vips: Vec<(i32, i32)>,
    flat_sources: Vec<(i32, i32)>,
    max_flat: i32,
    conflict: bool,
    kernels: BTreeMap<i32, search::KernelBank>,
}
fn empty_player() -> Player {
    Player {
        character_ranks: BTreeMap::new(),
        total_rank: 0,
        band_items: BTreeMap::new(),
        vip_rank: 1,
        character_memory: BTreeMap::new(),
        music_memory: BTreeMap::new(),
    }
}
fn prepare(
    master: &Master,
    data: &Value,
    event: i32,
    input: ReadingInput,
) -> Result<Reading, String> {
    let mut cards = input.row.cards;
    cards.sort_by_key(|card| card.slot);
    if cards.len() != 5 || cards.iter().enumerate().any(|(i, card)| i != card.slot) {
        return Err("missingGrowth".into());
    }
    let level = |kind: &str, group: i32, exp: i64| {
        master::rows(&data["progression"][kind])
            .into_iter()
            .filter(|row| {
                master::int(&row["group"]) == group
                    && master::number(&row["exp"], 0.0) <= exp as f64
            })
            .map(|row| master::int(&row["level"]))
            .max()
            .unwrap_or(0)
    };
    let mut team = Vec::new();
    let mut snaps = Vec::new();
    for card in &cards {
        let member = card
            .member_card_id
            .and_then(|id| master.members.get(&id))
            .ok_or("missingGrowth")?;
        let awake = card.member_awake_count.ok_or("missingGrowth")?;
        let growth = MemberGrowth {
            level: level(
                "memberCardLevels",
                member.level_group,
                card.member_exp.ok_or("missingGrowth")?,
            )
            .min(power::member_level_cap(master, member, awake)),
            awake,
            rank: card.member_rank.ok_or("missingGrowth")?,
            live_skill_level: 1,
            gekiso_skill_level: 1,
        };
        team.push(
            power::resolve_member(master, &card.slot.to_string(), member, growth)
                .ok_or("missingGrowth")?,
        );
        let snap = match card.support_card_id.filter(|id| *id != 0) {
            None => None,
            Some(id) => {
                let source = master.snaps.get(&id).ok_or("missingGrowth")?;
                let rank = card.support_rank.ok_or("missingGrowth")?;
                Some(
                    power::resolve_snap(
                        master,
                        &card.slot.to_string(),
                        source,
                        SnapGrowth {
                            level: level(
                                "supportCardLevels",
                                source.level_group,
                                card.support_exp.ok_or("missingGrowth")?,
                            )
                            .min(power::snap_level_cap(master, source, rank)),
                            rank,
                        },
                    )
                    .ok_or("missingGrowth")?,
                )
            }
        };
        snaps.push(snap);
    }
    if team
        .iter()
        .map(|member| member.card.character_id)
        .collect::<BTreeSet<_>>()
        .len()
        != 5
    {
        return Err("missingGrowth".into());
    }
    let challenge = master
        .challenge_music
        .iter()
        .find(|song| song.id == input.challenge_id && song.event_id == event)
        .ok_or("missingRules")?;
    let song = master
        .songs
        .get(&challenge.music_id)
        .ok_or("missingRules")?;
    let music = Music {
        music_type: if challenge.music_type == 0 {
            song.music_type
        } else {
            challenge.music_type
        },
        best_tags: song.best_tags.clone(),
        memory_key: song.id,
    };
    let percents = power::leader_percents(
        master,
        &power::leader_profile(master, &team[2]),
        &team,
        2,
        Some(&music),
    );
    let mut signature: Vec<_> = team
        .iter()
        .enumerate()
        .map(|(i, member)| {
            let snap = snaps[i].as_ref();
            (
                member.card.character_id,
                json!([
                    member.card.id,
                    member.growth.level,
                    member.growth.awake,
                    member.growth.rank,
                    snap.map(|s| s.card.id),
                    snap.map(|s| s.growth.level),
                    snap.map(|s| s.growth.rank),
                    percents[i],
                    member.card.card_type == 99
                        || music.music_type == 99
                        || member.card.card_type == music.music_type,
                    member
                        .card
                        .best_tags
                        .iter()
                        .any(|tag| music.best_tags.contains(tag))
                ]),
            )
        })
        .collect();
    signature.sort_by_key(|row| row.0);
    Ok(Reading {
        challenge: input.challenge_id,
        target: input
            .row
            .total_power
            .filter(|value| *value > 0)
            .ok_or("missingGrowth")?,
        team,
        snaps,
        music,
        percents,
        characters: Vec::new(),
        bands: Vec::new(),
        signature: json!(signature).to_string(),
    })
}
impl Problem {
    fn new(master: &Master, data: &Value, request: Request) -> Result<Self, String> {
        if !master.events.contains_key(&request.event_id) {
            return Err("missingRules".into());
        }
        let memories: Vec<_> = data["runtimeRules"]["tables"]
            .as_object()
            .into_iter()
            .flat_map(|rows| rows.iter())
            .filter(|(key, _)| key.starts_with("memory"))
            .collect();
        if memories.len() != 5 || memories.iter().any(|(_, row)| row["status"] != "empty") {
            return Err("missingRules".into());
        }
        if master
            .character_rank_bonus
            .iter()
            .enumerate()
            .any(|(i, &(rank, bonus))| rank != i as i32 + 1 || bonus != i as i32 * 5)
            || master.total_rank_bonus.iter().any(|row| row.1 % 5 != 0)
        {
            return Err("missingRules".into());
        }
        let all_caps = limits::calculate(master, data, &request.sources, request.as_of)
            .map_err(|_| "missingRules")?;
        let owner = request
            .observations
            .first()
            .and_then(|row| row.row.profile_id.clone());
        if request.observations.is_empty()
            || request
                .observations
                .iter()
                .any(|row| row.row.profile_id != owner)
        {
            return Err("missingGrowth".into());
        }
        let mut originals = request
            .observations
            .into_iter()
            .map(|row| prepare(master, data, request.event_id, row))
            .collect::<Result<Vec<_>, _>>()?;
        originals.sort_by_key(|row| row.challenge);
        let ids: Vec<_> = originals
            .iter()
            .flat_map(|row| row.team.iter().map(|member| member.card.character_id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let caps: Vec<_> = ids.iter().map(|id| all_caps[id]).collect();
        let mut bands = Vec::new();
        for band_id in originals
            .iter()
            .flat_map(|row| row.team.iter().map(|member| member.card.band_id))
            .collect::<BTreeSet<_>>()
        {
            let mut items = Vec::new();
            for item in master::rows(&data["bandItems"])
                .into_iter()
                .filter(|row| master::int(&row["bandId"]) == band_id)
            {
                let cap = master::rows(&item["levels"])
                    .iter()
                    .map(|row| master::int(&row["level"]))
                    .max()
                    .unwrap_or(0);
                for level in 1..=cap {
                    let effects: Vec<_> = master::rows(&item["effects"])
                        .into_iter()
                        .filter(|row| master::int(&row["level"]) == level)
                        .collect();
                    if effects.len() != 1 {
                        return Err("missingRules".into());
                    }
                    let effect = effects[0];
                    let targets = master::rows(&effect["targets"]);
                    if master::int(&effect["effectType"]) != 1000
                        || master::int(&effect["effectValue"]) != level * 10
                        || targets.len() != 1
                        || master::int(&targets[0]["bandId"]) != band_id
                        || master::int(&targets[0]["characterId"]) != 0
                        || master::int(&targets[0]["cardType"]) != 0
                    {
                        return Err("missingRules".into());
                    }
                }
                if cap <= 0 {
                    return Err("missingRules".into());
                }
                items.push((master::int(&item["bandItemId"]), cap));
            }
            if items.is_empty() {
                return Err("missingRules".into());
            }
            bands.push(Band {
                id: band_id,
                cap: items.iter().map(|row| row.1).sum(),
                items,
            });
        }
        let mut readings: Vec<Reading> = Vec::new();
        let mut mapping = Vec::new();
        for mut row in originals {
            row.characters = row
                .team
                .iter()
                .map(|member| {
                    ids.iter()
                        .position(|id| *id == member.card.character_id)
                        .unwrap()
                })
                .collect();
            row.bands = row
                .team
                .iter()
                .map(|member| {
                    bands
                        .iter()
                        .position(|band| band.id == member.card.band_id)
                        .unwrap()
                })
                .collect();
            let model = readings
                .iter()
                .position(|other| other.signature == row.signature && other.target == row.target)
                .unwrap_or(readings.len());
            mapping.push((row.challenge, model));
            if model == readings.len() {
                readings.push(row);
            }
        }
        let conflict = readings.iter().enumerate().any(|(i, row)| {
            readings[..i]
                .iter()
                .any(|other| row.signature == other.signature)
        });
        let total_cap = all_caps.values().sum::<i32>();
        let mut totals: Vec<_> = master
            .total_rank_bonus
            .iter()
            .filter(|row| row.0 <= total_cap)
            .map(|&(min, bonus)| Total {
                min,
                bonus,
                max: total_cap,
            })
            .collect();
        for i in 0..totals.len().saturating_sub(1) {
            totals[i].max = totals[i + 1].min - 1;
        }
        let max_flat = master
            .character_rank_bonus
            .last()
            .map(|row| row.1)
            .unwrap_or(0)
            / 5
            + totals.last().map(|row| row.bonus).unwrap_or(0) / 5;
        let mut flat_sources = vec![(1, 25); max_flat as usize + 1];
        for total in &totals {
            for &(rank, bonus) in master.character_rank_bonus.iter() {
                flat_sources[((bonus + total.bonus) / 5) as usize] = (rank, total.min);
            }
        }
        let mut vips = BTreeMap::from([(0, 1)]);
        for (&rank, &bonus) in master.vip_bonus.iter() {
            vips.entry(bonus).or_insert(rank);
        }
        let vips: Vec<_> = vips.into_iter().rev().collect();
        let kernel_count = readings.len() * 5 * (max_flat as usize + 1);
        let kernels = vips
            .iter()
            .map(|&(bonus, _)| {
                (
                    bonus,
                    Arc::new(std::sync::Mutex::new(vec![None; kernel_count])),
                )
            })
            .collect();
        Ok(Self {
            master: master.clone(),
            event: request.event_id,
            unseen_cap: total_cap - caps.iter().sum::<i32>(),
            unseen_count: master.characters.len() as i32 - ids.len() as i32,
            readings,
            originals: mapping,
            ids,
            caps,
            bands,
            all_caps,
            totals,
            vips,
            flat_sources,
            max_flat,
            conflict,
            kernels,
        })
    }
    fn player(&self, levels: &[i32], instruments: &[i32], total: i32, vip: i32) -> Player {
        let mut player = empty_player();
        player.character_ranks = self
            .ids
            .iter()
            .copied()
            .zip(levels.iter().copied())
            .collect();
        player.total_rank = total;
        player.vip_rank = vip;
        for (band, &total) in self.bands.iter().zip(instruments) {
            let mut remaining = total;
            for &(id, cap) in &band.items {
                let value = remaining.min(cap);
                player.band_items.insert(id, value);
                remaining -= value;
            }
        }
        player
    }
}

struct Phase {
    tiers: Vec<search::Tier>,
    cursor: usize,
    probe: bool,
    candidates: usize,
    done: bool,
}
impl Phase {
    fn new(problem: Arc<Problem>, relaxed: bool, radius: (i32, i32)) -> Self {
        let tiers = problem
            .vips
            .iter()
            .map(|&(bonus, rank)| {
                search::Tier::new(Arc::clone(&problem), bonus, rank, relaxed, radius)
            })
            .collect();
        Self {
            tiers,
            cursor: 0,
            probe: true,
            candidates: 0,
            done: false,
        }
    }
    fn advance(&mut self) -> Option<search::Ranked> {
        if self.cursor >= self.tiers.len() {
            self.probe = false;
            self.cursor = self
                .tiers
                .iter()
                .position(|tier| !tier.finished())
                .unwrap_or(self.tiers.len());
            if self.cursor == self.tiers.len() || self.candidates >= 256 {
                self.done = true;
                return None;
            }
        }
        let tier = &mut self.tiers[self.cursor];
        if tier.finished() {
            self.cursor += 1;
            return None;
        }
        let candidate = tier.advance();
        if tier.finished() || self.probe && (candidate.is_some() || tier.visited >= 4096) {
            self.cursor += 1;
        }
        if candidate.is_some() {
            self.candidates += 1;
        }
        candidate
    }
    fn stable_ready(&self, rows: &[search::Ranked]) -> bool {
        if self.probe || rows.len() < 10 || rows.iter().any(|row| row.configuration.changed) {
            return false;
        }
        let worst = rows.last().unwrap().configuration.vip_bonus;
        self.tiers.iter().all(|tier| {
            tier.finished() || tier.vip < worst || tier.vip == worst && tier.produced >= 10
        })
    }
}
pub struct Search {
    problem: Option<Arc<Problem>>,
    phases: Vec<Phase>,
    rows: Vec<search::Ranked>,
    preference: usize,
    active: usize,
    slice_ms: f64,
    sweep: bool,
    elapsed_ms: f64,
    complete: bool,
    status: &'static str,
    generation: u64,
}
const PREFERENCES: [bool; 2] = [false, true];
const RADII: [(i32, i32); 5] = [(0, 0), (1, 1), (1, 3), (2, 5), (2, 10)];
impl Search {
    pub fn new(master: &Master, data: &Value, input: &Value) -> Result<Self, String> {
        let request: Request =
            serde_json::from_value(input.clone()).map_err(|error| error.to_string())?;
        let prepared = Problem::new(master, data, request);
        let mut search = Self {
            problem: None,
            phases: Vec::new(),
            rows: Vec::new(),
            preference: 0,
            active: 0,
            slice_ms: 0.0,
            sweep: false,
            elapsed_ms: 0.0,
            complete: false,
            status: "ready",
            generation: crate::cancellation_generation(),
        };
        match prepared {
            Ok(problem) => {
                search.problem = Some(Arc::new(problem));
                search.add_phase();
            }
            Err(error) => {
                search.complete = true;
                search.status = if error == "missingGrowth" {
                    "missingGrowth"
                } else {
                    "missingRules"
                };
            }
        }
        Ok(search)
    }
    fn add_phase(&mut self) {
        self.active = self.phases.len();
        self.phases.push(Phase::new(
            Arc::clone(self.problem.as_ref().unwrap()),
            PREFERENCES[self.preference],
            RADII[self.active],
        ));
        self.slice_ms = 0.0;
    }
    fn schedule(&mut self) {
        if self.phases.iter().all(|phase| phase.done) {
            if self.phases.len() < RADII.len() {
                self.add_phase();
                return;
            }
            if !self.rows.is_empty() {
                self.complete = true;
                return;
            }
            self.preference += 1;
            if self.preference == PREFERENCES.len() {
                self.complete = true;
                self.status = if self.problem.as_ref().unwrap().conflict {
                    "conflictingRecords"
                } else {
                    "noMatch"
                };
                return;
            }
            self.phases.clear();
            self.sweep = false;
            self.add_phase();
            return;
        }
        if !self.sweep && self.phases.len() < RADII.len() {
            self.add_phase();
            return;
        }
        self.sweep = true;
        self.active = (1..=self.phases.len())
            .map(|offset| (self.active + offset) % self.phases.len())
            .find(|&index| !self.phases[index].done)
            .unwrap();
        self.slice_ms = 0.0;
    }
    pub fn step(&mut self, budget_ms: f64) {
        let begin = clock::milliseconds();
        let stop = begin + budget_ms;
        while !self.complete && clock::milliseconds() < stop {
            if crate::cancellation_generation() != self.generation {
                self.complete = true;
                break;
            }
            // A larger history domain can supply an early witness while exact searches keep their frontiers.
            let allowance = if self.sweep {
                [64.0, 48.0, 32.0, 24.0, 16.0][self.active]
            } else if self.active == 0 {
                160.0
            } else {
                60.0
            };
            if self.phases[self.active].done || self.slice_ms >= allowance {
                self.schedule();
                continue;
            }
            let before = clock::milliseconds();
            let candidate = self.phases[self.active].advance();
            self.slice_ms += clock::milliseconds() - before;
            if let Some(mut candidate) = candidate {
                if let Some(index) = self.rows.iter().position(|row| {
                    row.configuration.id == candidate.configuration.id
                        || row.display == candidate.display
                }) {
                    if candidate.compare(&self.rows[index]).is_lt() {
                        candidate.configuration.id = self.rows[index].configuration.id.clone();
                        self.rows[index] = candidate;
                    }
                } else {
                    self.rows.push(candidate);
                }
                self.rows.sort_by(search::Ranked::compare);
                self.rows.truncate(10);
            }
            if self.phases[0].stable_ready(&self.rows)
                || !self.rows.is_empty()
                    && self.elapsed_ms + clock::milliseconds() - begin >= 2000.0
            {
                self.complete = true;
            }
        }
        self.elapsed_ms += clock::milliseconds() - begin;
    }
    pub fn response(&self) -> Value {
        json!({"rows":self.rows.iter().map(|row|&row.configuration).collect::<Vec<_>>(),"count":self.rows.len(),
            "complete":self.complete,"status":self.status,"rankCaps":self.problem.as_ref().map(|problem|&problem.all_caps)})
    }
}
