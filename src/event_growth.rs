//! Joint inverse power search over saved activity decks. Uses the same compiled
//! Master, growth resolution and integer power arithmetic as normal team building.
mod limits;
mod profiles;
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

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    event_id: i32,
    as_of: f64,
    observations: Vec<ReadingInput>,
    #[serde(default)]
    total: Option<RowInput>,
    sources: Value,
    primary_challenge_id: Option<i32>,
    rank_range: Option<RankRange>,
}
#[derive(Clone, Copy, Deserialize)]
struct RankRange {
    min: Option<i32>,
    max: Option<i32>,
}
impl RankRange {
    fn restrict(self, total: Total) -> Option<Total> {
        let min = total.min.max(self.min.unwrap_or(0));
        let max = total.max.min(self.max.unwrap_or(i32::MAX));
        (min <= max).then_some(Total { min, max, ..total })
    }
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadingInput {
    challenge_id: i32,
    #[serde(skip)]
    context_challenge_id: Option<i32>,
    row: RowInput,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RowInput {
    profile_id: Option<String>,
    total_power: Option<i64>,
    cards: Vec<CardInput>,
}
#[derive(Clone, Deserialize)]
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
    context_challenge_id: i32,
    levels: Vec<i32>,
    instruments: Vec<i32>,
    total_rank: Range,
    total_value: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    outside_rank: Option<Range>,
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
    matched_total: bool,
    changed: bool,
    change_cost: i32,
    changed_dimensions: usize,
    witnesses: Vec<Witness>,
    equivalents: Vec<Equivalent>,
}
#[derive(Clone)]
struct Reading {
    challenge: i32,
    context_challenge: i32,
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
    master: Arc<Master>,
    event: i32,
    readings: Vec<Reading>,
    originals: Vec<(i32, usize)>,
    ids: Vec<i32>,
    caps: Vec<i32>,
    all_caps: BTreeMap<i32, i32>,
    unseen_cap: i32,
    unseen_count: i32,
    unseen_span: i32,
    bands: Vec<Band>,
    totals: Vec<Total>,
    primary_totals: Vec<Total>,
    vips: Vec<(i32, i32)>,
    max_flat: i32,
    conflict: bool,
    kernels: BTreeMap<i32, search::KernelBank>,
    deltas: search::DeltaBank,
    marginals: BTreeMap<i32, search::MarginalBank>,
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
        .find(|song| {
            song.id == input.context_challenge_id.unwrap_or(input.challenge_id)
                && song.event_id == event
        })
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
    let effects = &master.events.get(&event).ok_or("missingRules")?.effects;
    if percents.iter().flatten().any(|&value| value < 0)
        || team.iter().any(|member| {
            member.own.iter().any(|&value| value < 0)
                || master.params.music_type_base + member.music_type_rate < 0
                || master.params.music_tag_base + member.music_tag_rate < 0
                || power::member_event_percent(effects, member, 2) < 0
        })
        || snaps.iter().flatten().any(|snap| {
            master.params.type_link_base + snap.type_link_rate < 0
                || snap.percent.iter().any(|&value| value < 0)
                || power::snap_event_percent(effects, snap, 2) < 0
        })
    {
        return Err("missingRules".into());
    }
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
        context_challenge: challenge.id,
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
    fn new(
        master: &Arc<Master>,
        data: &Value,
        request: &Request,
        mut originals: Vec<Reading>,
    ) -> Result<Self, String> {
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
            || master
                .total_rank_bonus
                .iter()
                .any(|row| row.1 % 5 != 0 || row.1 < 0)
            || master.vip_bonus.values().any(|&bonus| bonus < 0)
            || master
                .total_rank_bonus
                .windows(2)
                .any(|pair| pair[0].1 > pair[1].1)
        {
            return Err("missingRules".into());
        }
        let all_caps = limits::calculate(master, data, &request.sources, request.as_of)
            .map_err(|_| "missingRules")?;
        originals.sort_by_key(|row| {
            (
                request
                    .primary_challenge_id
                    .is_some_and(|id| id != row.challenge),
                row.challenge,
            )
        });
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
            mapping.push((row.challenge, readings.len()));
            readings.push(row);
        }
        let conflict = readings.iter().enumerate().any(|(i, row)| {
            readings[..i]
                .iter()
                .any(|other| row.signature == other.signature && row.target != other.target)
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
        let mut merged: Vec<Total> = Vec::new();
        for total in totals {
            if let Some(previous) = merged
                .last_mut()
                .filter(|previous| previous.bonus == total.bonus)
            {
                previous.max = total.max;
            } else {
                merged.push(total);
            }
        }
        let totals = merged;
        if totals.is_empty() {
            return Err("missingRules".into());
        }
        let primary_totals = match request.rank_range {
            Some(range) => totals
                .iter()
                .filter_map(|&total| range.restrict(total))
                .collect(),
            None => totals.clone(),
        };
        let max_flat = master
            .character_rank_bonus
            .last()
            .map(|row| row.1)
            .unwrap_or(0)
            / 5
            + totals.last().map(|row| row.bonus).unwrap_or(0) / 5;
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
            master: Arc::clone(master),
            event: request.event_id,
            unseen_cap: total_cap - caps.iter().sum::<i32>(),
            unseen_count: master.characters.len() as i32 - ids.len() as i32,
            unseen_span: all_caps
                .iter()
                .filter(|(id, _)| !ids.contains(id))
                .map(|(_, cap)| (cap - 1).clamp(0, RANK_SPAN))
                .sum(),
            readings,
            originals: mapping,
            ids,
            caps,
            bands,
            all_caps,
            totals,
            primary_totals,
            marginals: vips
                .iter()
                .map(|&(bonus, _)| (bonus, Arc::new(std::sync::Mutex::new(BTreeMap::new()))))
                .collect(),
            vips,
            max_flat,
            conflict,
            deltas: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
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

pub struct Search {
    problems: Vec<Arc<Problem>>,
    tiers: Vec<search::Tier>,
    scouts: Vec<search::Tier>,
    scout_cursor: usize,
    scout_ms: f64,
    scout_slice: f64,
    scout_tick: usize,
    rows: Vec<Arc<search::Ranked>>,
    previews: BTreeMap<String, Arc<search::Ranked>>,
    seen: BTreeSet<String>,
    incumbent: Option<i32>,
    minimum: Option<i32>,
    budget: i32,
    page: usize,
    examined: usize,
    pruned: usize,
    complete: bool,
    status: &'static str,
    rejected_challenges: Vec<i32>,
    generation: u64,
}
const PAGE_SIZE: usize = 10;
const RANK_SPAN: i32 = 2;
const ITEM_SPAN: i32 = 10;
impl Search {
    pub fn new(master: &Master, data: &Value, input: &Value) -> Result<Self, String> {
        let mut request: Request =
            serde_json::from_value(input.clone()).map_err(|error| error.to_string())?;
        if request.rank_range.is_some_and(|range| {
            let min = range.min.unwrap_or(0);
            min < 0 || range.max.unwrap_or(i32::MAX) < min
        }) {
            return Err("invalid character rank range".into());
        }
        let mut rejected_challenges = Vec::new();
        let prepared = (|| {
            let compiled = Arc::new((*master).clone());
            let owner = request
                .observations
                .first()
                .map(|row| &row.row)
                .or(request.total.as_ref())
                .and_then(|row| row.profile_id.clone());
            let mut originals = Vec::new();
            for row in std::mem::take(&mut request.observations) {
                let id = row.challenge_id;
                if row.row.profile_id != owner {
                    return Err("missingGrowth".into());
                }
                match prepare(master, data, request.event_id, row) {
                    Ok(reading) => originals.push(reading),
                    Err(_)
                        if request
                            .primary_challenge_id
                            .is_some_and(|primary| primary != id) =>
                    {
                        rejected_challenges.push(id);
                    }
                    Err(error) => {
                        rejected_challenges.push(id);
                        return Err(error);
                    }
                }
            }
            if request
                .primary_challenge_id
                .is_some_and(|id| id != 0 && !originals.iter().any(|row| row.challenge == id))
            {
                return Err("missingGrowth".into());
            }
            let mut problems = Vec::new();
            if let Some(total) = request
                .total
                .take()
                .filter(|total| total.profile_id == owner)
            {
                let mut contexts = BTreeSet::new();
                let mut total_error = "missingGrowth".to_string();
                for challenge in master
                    .challenge_music
                    .iter()
                    .filter(|song| song.event_id == request.event_id)
                {
                    let reading = ReadingInput {
                        challenge_id: 0,
                        context_challenge_id: Some(challenge.id),
                        row: total.clone(),
                    };
                    match prepare(master, data, request.event_id, reading) {
                        Ok(reading) if contexts.insert(reading.signature.clone()) => {
                            let mut readings = originals.clone();
                            readings.push(reading);
                            problems
                                .push(Arc::new(Problem::new(&compiled, data, &request, readings)?));
                        }
                        Ok(_) => {}
                        Err(error) => total_error = error,
                    }
                }
                if problems.is_empty() {
                    rejected_challenges.push(0);
                    if request.primary_challenge_id == Some(0) {
                        return Err(total_error);
                    }
                }
            }
            if problems.is_empty() {
                if originals.is_empty() || request.primary_challenge_id == Some(0) {
                    return Err("missingGrowth".into());
                }
                problems.push(Arc::new(Problem::new(
                    &compiled, data, &request, originals,
                )?));
            }
            let deltas = Arc::clone(&problems[0].deltas);
            for problem in problems.iter_mut().skip(1) {
                Arc::get_mut(problem).unwrap().deltas = Arc::clone(&deltas);
            }
            Ok::<_, String>(problems)
        })();
        let mut search = Self {
            problems: Vec::new(),
            tiers: Vec::new(),
            scouts: Vec::new(),
            scout_cursor: 0,
            scout_ms: 0.0,
            scout_slice: 0.0,
            scout_tick: 0,
            rows: Vec::new(),
            previews: BTreeMap::new(),
            seen: BTreeSet::new(),
            incumbent: None,
            minimum: None,
            budget: 0,
            page: 0,
            examined: 0,
            pruned: 0,
            complete: false,
            status: "ready",
            rejected_challenges,
            generation: crate::cancellation_generation(),
        };
        match prepared {
            Ok(problems) => {
                search.problems = problems;
                if search
                    .problems
                    .iter()
                    .all(|problem| problem.primary_totals.is_empty())
                {
                    search.complete = true;
                    search.status = "noMatch";
                } else {
                    search.add_tiers();
                    search.add_scouts();
                }
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
    fn add_tiers(&mut self) {
        self.examined += self.tiers.iter().map(|tier| tier.visited).sum::<usize>();
        self.pruned += self.tiers.iter().map(|tier| tier.pruned).sum::<usize>();
        self.tiers = self
            .problems
            .iter()
            .flat_map(|problem| {
                problem.vips.iter().map(|&(bonus, rank)| {
                    search::Tier::new(Arc::clone(problem), bonus, rank, self.budget, false)
                })
            })
            .collect();
    }
    fn offer(&mut self, row: Arc<search::Ranked>) {
        let cost = row.configuration.change_cost;
        if self.incumbent.is_none_or(|old| cost < old) {
            self.incumbent = Some(cost);
        }
        if cost == 0 || cost == self.budget {
            self.minimum = Some(self.minimum.map(|old| old.min(cost)).unwrap_or(cost));
        }
        let limit = self.incumbent.unwrap() + 1;
        self.previews
            .retain(|_, row| row.configuration.change_cost <= limit);
        let key = &row.configuration.id;
        if cost <= limit
            && !self.seen.contains(key)
            && self
                .previews
                .get(key)
                .is_none_or(|old| row.compare(old).is_lt())
        {
            self.previews.insert(key.clone(), row);
        }
    }
    fn clear_scouts(&mut self) {
        self.examined += self.scouts.iter().map(|tier| tier.visited).sum::<usize>();
        self.pruned += self.scouts.iter().map(|tier| tier.pruned).sum::<usize>();
        self.scouts.clear();
        self.scout_cursor = 0;
        self.scout_slice = 0.0;
    }
    fn add_scouts(&mut self) {
        self.scouts = self
            .problems
            .iter()
            .flat_map(|problem| {
                problem.vips.iter().map(|&(bonus, rank)| {
                    let maximum = problem.ids.len() as i32 * RANK_SPAN * 5
                        + problem.bands.len() as i32 * ITEM_SPAN
                        + problem.totals.last().unwrap().bonus
                        - problem.totals[0].bonus;
                    search::Tier::new(Arc::clone(problem), bonus, rank, maximum, true)
                })
            })
            .collect();
        self.scouts.sort_by_key(|tier| std::cmp::Reverse(tier.vip));
    }
    fn next_budget(&mut self) {
        let mut maximum = self
            .problems
            .iter()
            .map(|problem| {
                problem.ids.len() as i32 * RANK_SPAN * 5
                    + problem.bands.len() as i32 * ITEM_SPAN
                    + problem.totals.last().unwrap().bonus
                    - problem.totals[0].bonus
            })
            .max()
            .unwrap();
        if let Some(incumbent) = self.incumbent {
            maximum = maximum.min(incumbent + 1);
        }
        if self.budget < maximum {
            self.budget += 1;
            if self.incumbent == Some(self.budget) {
                self.minimum = self.incumbent;
            }
            self.add_tiers();
            return;
        }
        self.complete = true;
        self.page = self.page.min(self.rows.len().saturating_sub(1) / PAGE_SIZE);
        if self.rows.is_empty() {
            self.status = if self.problems.iter().any(|problem| problem.conflict) {
                "conflictingRecords"
            } else {
                "noMatch"
            };
        }
    }
    pub fn step(&mut self, budget_ms: f64) {
        let stop = clock::milliseconds() + budget_ms;
        while !self.complete
            && self.rows.len() < (self.page + 1) * PAGE_SIZE
            && clock::milliseconds() < stop
        {
            if crate::cancellation_generation() != self.generation {
                self.complete = true;
                break;
            }
            self.scout_tick += 1;
            if self.scout_cursor < self.scouts.len()
                && (self.scout_ms < 200.0 || self.scout_tick.is_multiple_of(4))
            {
                let before = clock::milliseconds();
                let scout = &mut self.scouts[self.scout_cursor];
                scout.advance();
                let elapsed = clock::milliseconds() - before;
                self.scout_ms += elapsed;
                self.scout_slice += elapsed;
                let previews = std::mem::take(&mut scout.latest);
                let finished = scout.finished();
                if !previews.is_empty() {
                    for preview in previews {
                        self.offer(preview);
                    }
                    self.clear_scouts();
                } else if finished || self.scout_slice >= 16.0 {
                    self.scout_slice = 0.0;
                    self.scout_cursor = (1..=self.scouts.len())
                        .map(|offset| (self.scout_cursor + offset) % self.scouts.len())
                        .find(|&i| !self.scouts[i].finished())
                        .unwrap_or(self.scouts.len());
                }
                continue;
            }
            let next = self
                .tiers
                .iter()
                .enumerate()
                .filter(|(_, tier)| !tier.finished())
                .min_by(|(i, a), (j, b)| {
                    a.bound()
                        .unwrap()
                        .cmp(b.bound().unwrap())
                        .then(b.ready_rank().cmp(&a.ready_rank()))
                        .then(a.visited.cmp(&b.visited))
                        .then(i.cmp(j))
                })
                .map(|(i, _)| i);
            let Some(index) = next else {
                self.next_budget();
                continue;
            };
            let candidate = self.tiers[index].advance();
            let discovered = std::mem::take(&mut self.tiers[index].latest);
            if !discovered.is_empty() {
                self.clear_scouts();
            }
            for preview in discovered {
                self.offer(preview);
            }
            if let Some(candidate) = candidate {
                if self.seen.insert(candidate.configuration.id.clone()) {
                    debug_assert!(self
                        .rows
                        .last()
                        .is_none_or(|old| !candidate.compare(old).is_lt()));
                    self.previews.remove(&candidate.configuration.id);
                    self.rows.push(candidate);
                }
            }
        }
    }
    pub fn set_page(&mut self, page: usize) {
        self.page = page.min(self.rows.len() / PAGE_SIZE);
    }
    pub fn response(&self) -> Value {
        let end = (self.page + 1) * PAGE_SIZE;
        let mut rows: Vec<_> = self
            .rows
            .iter()
            .skip(self.page * PAGE_SIZE)
            .take(PAGE_SIZE)
            .collect();
        let mut pending: Vec<_> = self.previews.values().collect();
        pending.sort_by(|a, b| a.compare(b));
        rows.extend(pending.into_iter().take(PAGE_SIZE - rows.len()));
        json!({"rows":rows.into_iter().map(|row|&row.configuration).collect::<Vec<_>>(),
            "count":self.rows.len() + self.previews.len(), "certified":self.rows.len(), "costBound":self.budget,
            "minimumCost":self.minimum, "costLimit":self.incumbent.map(|cost|cost+1),
            "examinedBoxes":self.examined + self.tiers.iter().map(|tier| tier.visited).sum::<usize>() + self.scouts.iter().map(|tier|tier.visited).sum::<usize>(),
            "prunedBoxes":self.pruned + self.tiers.iter().map(|tier| tier.pruned).sum::<usize>() + self.scouts.iter().map(|tier|tier.pruned).sum::<usize>(),
            "page":self.page,"pageSize":PAGE_SIZE,"awaitingPage":!self.complete && self.rows.len() >= end,
            "hasMore":!self.complete || self.rows.len() > end,"complete":self.complete,"status":self.status,
            "rankCaps":self.problems.first().map(|problem|&problem.all_caps),
             "rejectedChallenges":self.rejected_challenges})
    }
}
