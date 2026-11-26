use super::{inventory, search, Candidate, Inventory, Master, Team, Value};

pub(crate) enum Visit {
    Advanced,
    Candidate(Candidate),
    Seed(Candidate),
    Finished,
}

/// Depths 0..5 choose members (leader first). Depths 5..10 equip their snapshots.
/// The next index at every depth survives a time slice, including rejected branches.
pub(crate) struct Cursor {
    inventory: Inventory,
    team: Team,
    next: [usize; 10],
    depth: usize,
    seeds: Vec<Team>,
    emitted_seeds: usize,
}

impl Cursor {
    pub(crate) fn new(master: &Master, song_id: i32, owner: &Value) -> Result<Self, String> {
        Ok(Self {
            inventory: inventory(master, song_id, owner)?,
            team: Team {
                members: Vec::with_capacity(5),
                snaps: Vec::with_capacity(5),
            },
            next: [0; 10],
            depth: 0,
            seeds: Vec::new(),
            emitted_seeds: 0,
        })
    }

    /// Try well-equipped alternatives early when the very first lineup alone has a wide
    /// snapshot search. The ordinary cursor still visits all legal formations.
    pub(crate) fn seed_large_box(mut self, master: &Master) -> Self {
        if self.inventory.unbound_snaps.len() > 5 && self.inventory.cards.members.len() > 5 {
            self.seeds = self.inventory.power_seeds(master);
        }
        self
    }

    fn backtrack(&mut self) -> Visit {
        if self.depth == 0 {
            return Visit::Finished;
        }
        self.depth -= 1;
        if self.depth < 5 {
            self.team.members.pop();
        } else {
            self.team.snaps.pop();
        }
        Visit::Advanced
    }

    pub(crate) fn advance(&mut self, master: &Master) -> Result<Visit, String> {
        if let Some(seed) = self.seeds.get(self.emitted_seeds) {
            let candidate = self.inventory.prepare(master, seed)?;
            self.emitted_seeds += 1;
            return Ok(Visit::Seed(candidate));
        }
        let index = self.next[self.depth];
        let inv = &self.inventory;
        let members = &inv.cards.members;
        if self.depth < 5 {
            let Some(member) = members.get(index) else {
                return Ok(self.backtrack());
            };
            self.next[self.depth] += 1;
            if (self.depth == 0
                && inv
                    .limits
                    .leader
                    .is_some_and(|leader| leader != index as i32))
                || inv.limits.excluded_members.contains(&index)
                || self
                    .team
                    .members
                    .iter()
                    .any(|&i| members[i].card.character_id == member.card.character_id)
            {
                return Ok(Visit::Advanced);
            }
            self.team.members.push(index);
            self.depth += 1;
            // Nonleaders choose increasing inventory indices. Any member may follow the leader.
            self.next[self.depth] = if (2..5).contains(&self.depth) {
                index + 1
            } else {
                0
            };
            if self.depth == 5
                && inv
                    .limits
                    .required_members
                    .iter()
                    .chain(inv.limits.bindings.keys())
                    .any(|member| !self.team.members.contains(member))
            {
                return Ok(self.backtrack());
            }
            return Ok(Visit::Advanced);
        }
        let slot = self.depth - 5;
        let choices = inv
            .limits
            .bindings
            .get(&self.team.members[slot])
            .map_or(inv.unbound_snaps.as_slice(), std::slice::from_ref);
        let Some(&snap) = choices.get(index) else {
            return Ok(self.backtrack());
        };
        self.next[self.depth] += 1;
        if snap >= 0 && self.team.snaps.contains(&snap) {
            return Ok(Visit::Advanced);
        }
        self.team.snaps.push(snap);
        if slot < 4 {
            self.depth += 1;
            self.next[self.depth] = 0;
            return Ok(Visit::Advanced);
        }
        let seeded = self
            .seeds
            .iter()
            .any(|s| s.members == self.team.members && s.snaps == self.team.snaps);
        let result =
            if !seeded && search::valid_seed(members, &inv.cards.snaps, &inv.limits, &self.team) {
                inv.prepare(master, &self.team).map(Visit::Candidate)
            } else {
                Ok(Visit::Advanced)
            };
        self.team.snaps.pop();
        result
    }
}

impl Inventory {
    fn power_seeds(&self, master: &Master) -> Vec<Team> {
        use crate::power::{slot_base, snap_points};
        use std::cmp::Reverse;

        let cards = &self.cards;
        let limits = &self.limits;
        let bases: Vec<_> = cards
            .members
            .iter()
            .map(|m| slot_base(master, &self.player, m, Some(&self.music), 0))
            .collect();
        let mut by_power: Vec<_> = (0..cards.members.len())
            .filter(|i| !limits.excluded_members.contains(i))
            .collect();
        by_power.sort_by_key(|&i| Reverse(bases[i].fixed_points));
        let mut required = limits.required_members.clone();
        required.extend(limits.bindings.keys().copied());
        required.sort_unstable();
        required.dedup();
        let mut seeds: Vec<Team> = Vec::new();
        for offset in 0..by_power.len().min(5) {
            let leader = limits.leader.map_or(by_power[offset], |i| i as usize);
            if leader >= cards.members.len() {
                break;
            }
            let mut members = vec![leader];
            // Required cards get the first available slots. The offset explores other members
            // and leaders before a single five-card lineup exhausts its snapshot assignments.
            for &i in required
                .iter()
                .chain(by_power.iter().cycle().skip(offset).take(by_power.len()))
            {
                if members.len() == 5 {
                    break;
                }
                if members.iter().all(|&j| {
                    cards.members[i].card.character_id != cards.members[j].card.character_id
                }) {
                    members.push(i);
                }
            }
            if members.len() != 5 {
                continue;
            }
            members[1..].sort_unstable();
            let mut snaps: Vec<_> = members
                .iter()
                .map(|m| limits.bindings.get(m).copied().unwrap_or(-1))
                .collect();
            for (slot, &member) in members.iter().enumerate() {
                if limits.bindings.contains_key(&member) {
                    continue;
                }
                let available = self
                    .unbound_snaps
                    .iter()
                    .copied()
                    .filter(|&s| s >= 0 && !snaps.contains(&s));
                if let Some(snap) = available.max_by_key(|&s| {
                    (
                        limits.required_snaps.contains(&(s as usize)),
                        snap_points(
                            master,
                            &bases[member],
                            &cards.members[member],
                            Some(&cards.snaps[s as usize]),
                            0,
                        ),
                        Reverse(s),
                    )
                }) {
                    snaps[slot] = snap;
                }
            }
            let team = Team { members, snaps };
            if search::valid_seed(&cards.members, &cards.snaps, limits, &team)
                && !seeds
                    .iter()
                    .any(|s| s.members == team.members && s.snaps == team.snaps)
            {
                seeds.push(team);
            }
        }
        seeds
    }
}
