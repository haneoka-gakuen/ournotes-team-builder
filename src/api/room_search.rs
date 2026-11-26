//! Exhaustive two-seat search for small inventories under one fixed play scenario.
use crate::{
    full::conditions::Performer,
    gekiso::{room, Chart},
    master::Master,
    power::{self, BoxCards, Music, Player},
    search::{self, Constraints, Team},
};
use serde_json::{json, Value};

pub struct Candidate {
    /// Owned instance keys in engine team format. The leader occupies slot zero.
    pub team: Value,
    pub deck: Vec<Performer>,
    pub power: i32,
}

impl Candidate {
    pub fn seat(&self, order_index: usize, seed: i32) -> room::Seat<'_> {
        room::Seat {
            deck: &self.deck,
            power: self.power,
            order_index,
            seed,
        }
    }
}

struct Inventory<'a> {
    master: &'a Master,
    cards: BoxCards,
    limits: Constraints,
    unbound_snaps: Vec<i32>,
    player: Player,
    music: Music,
}

impl Inventory<'_> {
    fn pick_members(
        &self,
        team: &mut Team,
        from: usize,
        out: &mut Vec<Candidate>,
    ) -> Result<(), String> {
        if team.members.len() == 5 {
            if self
                .limits
                .required_members
                .iter()
                .chain(self.limits.bindings.keys())
                .all(|member| team.members.contains(member))
            {
                self.assign_snaps(team, out)?;
            }
            return Ok(());
        }
        let leader = team.members.is_empty();
        for i in from..self.cards.members.len() {
            let character = self.cards.members[i].card.character_id;
            if (leader && self.limits.leader.is_some_and(|locked| locked != i as i32))
                || self.limits.excluded_members.contains(&i)
                || team
                    .members
                    .iter()
                    .any(|&j| self.cards.members[j].card.character_id == character)
            {
                continue;
            }
            team.members.push(i);
            // Distinct character IDs give nonleaders stable slots even across account-specific keys.
            self.pick_members(team, if leader { 0 } else { i + 1 }, out)?;
            team.members.pop();
        }
        Ok(())
    }

    fn assign_snaps(&self, team: &mut Team, out: &mut Vec<Candidate>) -> Result<(), String> {
        let slot = team.snaps.len();
        if slot == 5 {
            if search::valid_seed(&self.cards.members, &self.cards.snaps, &self.limits, team) {
                out.push(self.prepare(team)?);
            }
            return Ok(());
        }
        let choices = self
            .limits
            .bindings
            .get(&team.members[slot])
            .map_or(self.unbound_snaps.as_slice(), std::slice::from_ref);
        for &snap in choices {
            if snap >= 0 && team.snaps.contains(&snap) {
                continue;
            }
            team.snaps.push(snap);
            self.assign_snaps(team, out)?;
            team.snaps.pop();
        }
        Ok(())
    }

    fn prepare(&self, team: &Team) -> Result<Candidate, String> {
        let members: Vec<_> = team
            .members
            .iter()
            .map(|&i| self.cards.members[i].clone())
            .collect();
        let snaps: Vec<_> = team
            .snaps
            .iter()
            .map(|&i| usize::try_from(i).ok().map(|i| self.cards.snaps[i].clone()))
            .collect();
        let power = power::team_power(
            self.master,
            &self.player,
            &members,
            &snaps,
            0,
            Some(&self.music),
            &[],
        )?;
        let deck = members
            .iter()
            .zip(&snaps)
            .map(|(m, s)| Performer::from_member(self.master, m, s.as_ref()))
            .collect();
        let team = json!({
            "leader": members[0].key,
            "members": members.iter().map(|m| &m.key).collect::<Vec<_>>(),
            "snaps": snaps.iter().map(|s| s.as_ref().map(|s| &s.key)).collect::<Vec<_>>(),
        });
        Ok(Candidate {
            team,
            deck,
            power: i32::try_from(power.total).map_err(|_| "room-team-power-overflow")?,
        })
    }
}

fn inventory<'a>(master: &'a Master, song_id: i32, seat: &Value) -> Result<Inventory<'a>, String> {
    let mut cards = super::owned_box(master, seat)?;
    if !cards.unknown.is_empty() {
        return Err(format!(
            "unresolved room inventory cards: {}",
            cards.unknown.join(", ")
        ));
    }
    fn duplicate_keys<'a>(mut keys: impl Iterator<Item = &'a str>) -> bool {
        let mut seen = std::collections::HashSet::new();
        keys.any(|key| !seen.insert(key))
    }
    if duplicate_keys(cards.members.iter().map(|m| m.key.as_str()))
        || duplicate_keys(cards.snaps.iter().map(|s| s.key.as_str()))
    {
        return Err("duplicate card instance key in room inventory".into());
    }
    cards.members.sort_by(|a, b| {
        (a.card.character_id, a.card.id, &a.key).cmp(&(b.card.character_id, b.card.id, &b.key))
    });
    cards
        .snaps
        .sort_by(|a, b| (a.card.id, &a.key).cmp(&(b.card.id, &b.key)));
    let constraints = &seat["constraints"];
    let mut bindings = std::collections::BTreeMap::new();
    for pair in crate::master::rows(&constraints["bindings"]) {
        if let Some(member) = pair[0].as_str() {
            if bindings
                .insert(member, &pair[1])
                .is_some_and(|prev| prev != &pair[1])
            {
                return Err(format!("conflicting room snap bindings for {member}"));
            }
        }
    }
    let limits = super::constraints(master, constraints, &cards.members, &cards.snaps);
    if limits.min_bonus.is_some_and(|v| v > 0.0) {
        return Err("event bonus constraints are unavailable in a gekiso room".into());
    }
    let available = (0..cards.snaps.len()).filter(|i| {
        !limits.no_snaps
            && !limits.excluded_snaps.contains(i)
            && !limits.bindings.values().any(|&locked| locked == *i as i32)
    });
    let unbound_snaps = std::iter::once(-1)
        .chain(available.map(|i| i as i32))
        .collect();
    Ok(Inventory {
        master,
        cards,
        limits,
        unbound_snaps,
        player: power::player_state(master, &seat["player"]),
        music: power::music_view(master, song_id, None)?,
    })
}

/// Uses the existing inventory request format and resolves growth and account bonuses per seat.
/// No candidate ranking or pruning based on individual strength is performed.
pub fn candidates(master: &Master, song_id: i32, seat: &Value) -> Result<Vec<Candidate>, String> {
    let inventory = inventory(master, song_id, seat)?;
    let mut candidates = Vec::new();
    inventory.pick_members(
        &mut Team {
            members: Vec::with_capacity(5),
            snaps: Vec::with_capacity(5),
        },
        0,
        &mut candidates,
    )?;
    Ok(candidates)
}

/// Resolve one selected team from its owner's inventory without generating alternatives.
pub(super) fn fixed_candidate(
    master: &Master,
    song_id: i32,
    request: &Value,
    team: &Value,
) -> Result<Candidate, String> {
    let inventory = inventory(master, song_id, request)?;
    let cards = &inventory.cards;
    if !["members", "snaps"]
        .iter()
        .all(|&field| team[field].as_array().is_some_and(|slots| slots.len() == 5))
    {
        return Err("selected team must list five member and snap slots".into());
    }
    let mut selected = super::team_from_keys(team, &cards.members, &cards.snaps)
        .ok_or("expected five owned members and five owned or empty snap slots")?;
    if let Some(leader) = team.get("leader") {
        let position = selected
            .members
            .iter()
            .position(|&i| Some(cards.members[i].key.as_str()) == leader.as_str())
            .ok_or("selected leader must be on the team")?;
        selected.members[..=position].rotate_right(1);
        selected.snaps[..=position].rotate_right(1);
    }
    if !search::valid_seed(&cards.members, &cards.snaps, &inventory.limits, &selected) {
        return Err("selected team violates formation or inventory constraints".into());
    }
    inventory.prepare(&selected)
}

pub struct Solution {
    pub indices: [usize; 2],
    pub replay: room::Replay,
    pub evaluated: usize,
}

/// Enumerates the Cartesian product; equal totals retain the first legal pair.
pub fn solve(
    master: &Master,
    chart: &Chart,
    candidates: [&[Candidate]; 2],
    order_indices: [usize; 2],
    seeds: [i32; 2],
) -> Result<Solution, String> {
    let mut best: Option<([usize; 2], room::Replay)> = None;
    let mut evaluated = 0;
    for (i, a) in candidates[0].iter().enumerate() {
        for (j, b) in candidates[1].iter().enumerate() {
            let replay = room::replay(
                master,
                chart,
                [
                    a.seat(order_indices[0], seeds[0]),
                    b.seat(order_indices[1], seeds[1]),
                ],
            )?;
            evaluated += 1;
            if best
                .as_ref()
                .is_none_or(|(_, current)| replay.total > current.total)
            {
                best = Some(([i, j], replay));
            }
        }
    }
    let (indices, replay) = best.ok_or_else(|| {
        let seat = if candidates[0].is_empty() { 1 } else { 2 };
        format!("seat {seat} has no legal formation")
    })?;
    Ok(Solution {
        indices,
        replay,
        evaluated,
    })
}
