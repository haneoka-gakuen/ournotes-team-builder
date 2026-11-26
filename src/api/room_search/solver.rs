//! Incremental Cartesian search over one fixed chart, judgement stream and per-seat skill order.
use super::Candidate;
use crate::{
    gekiso::{room, Chart},
    master::Master,
};
use num_bigint::BigUint;
use num_traits::ToPrimitive;
use serde_json::{json, Value};
use std::collections::BTreeSet;

enum Phase {
    Inventory,
    Bounds,
    Search,
    Complete,
}

struct Winner {
    indices: Vec<usize>,
    replay: room::Replay,
}

struct Bounds {
    // Only complete sets of histories enter these tables. A partial maximum is a lower bound.
    scores: Vec<Vec<i32>>,
    seat_max: Vec<i32>,
    seat: usize,
    history: usize,
    partial_max: i32,
}

pub(crate) struct Search {
    chart: Chart,
    orders: Vec<usize>,
    seeds: Vec<i32>,
    candidates: Vec<Vec<Candidate>>,
    best: Option<Winner>,
    phase: Phase,
    pending_probe: Option<Vec<usize>>,
    unplayed_seeds: Vec<(usize, usize)>,
    probes: BTreeSet<Vec<usize>>,
    bounds: Option<Bounds>,
    cursor: Vec<usize>,
    strides: Vec<BigUint>,
    total: Option<BigUint>,
    evaluated: BigUint,
    pruned: BigUint,
    bound_replays: BigUint,
}

// Keep exact counts in JavaScript, including candidate products beyond Number.MAX_SAFE_INTEGER.
fn count(n: &BigUint) -> Value {
    match n.to_u64() {
        Some(small) if small < (1 << 53) => json!(small),
        _ => json!(n.to_string()),
    }
}

impl Search {
    pub(crate) fn new(chart: Chart, orders: Vec<usize>, seeds: Vec<i32>) -> Result<Self, String> {
        let n = orders.len();
        if !(2..=5).contains(&n) || seeds.len() != n {
            return Err("room search requires two to five matching seats, orders and seeds".into());
        }
        if orders.iter().any(|&i| i >= crate::live::orders().len()) {
            return Err("room skill order out of range".into());
        }
        Ok(Self {
            chart,
            orders,
            seeds,
            candidates: (0..n).map(|_| Vec::new()).collect(),
            best: None,
            phase: Phase::Inventory,
            pending_probe: None,
            unplayed_seeds: Vec::new(),
            probes: BTreeSet::new(),
            bounds: None,
            cursor: vec![0; n],
            strides: Vec::new(),
            total: None,
            evaluated: BigUint::default(),
            pruned: BigUint::default(),
            bound_replays: BigUint::default(),
        })
    }

    pub(crate) fn push_candidate(&mut self, seat: usize, candidate: Candidate) {
        debug_assert!(matches!(self.phase, Phase::Inventory));
        self.candidates[seat].push(candidate);
        let size = self.candidates[seat].len();
        if let Some(best) = &self.best {
            // Geometric sampling improves the incumbent during collection with O(log size) probes.
            if size.is_power_of_two() {
                let mut indices = best.indices.clone();
                indices[seat] = size - 1;
                self.pending_probe = Some(indices);
            }
        } else if self.candidates.iter().all(|teams| !teams.is_empty()) {
            self.pending_probe = Some(vec![0; self.candidates.len()]);
        }
    }

    pub(crate) fn has_probe(&self) -> bool {
        self.pending_probe.is_some() || (self.best.is_some() && !self.unplayed_seeds.is_empty())
    }

    pub(crate) fn push_seed(&mut self, seat: usize, candidate: Candidate) {
        let index = self.candidates[seat].len();
        self.push_candidate(seat, candidate);
        self.unplayed_seeds.push((seat, index));
    }

    pub(crate) fn finish_inventory(&mut self) -> Result<(), String> {
        if !matches!(self.phase, Phase::Inventory) {
            return Ok(());
        }
        if let Some(s) = self.candidates.iter().position(Vec::is_empty) {
            return Err(format!("seat {} has no legal formation", s + 1));
        }
        let n = self.candidates.len();
        let mut product = BigUint::from(1u8);
        for teams in self.candidates.iter().rev() {
            self.strides.push(product.clone());
            product *= teams.len();
        }
        self.strides.reverse();
        // Compare the work for all bounds with replaying every remaining room (n seats per room).
        let teams: u128 = self.candidates.iter().map(|v| v.len() as u128).sum();
        let bound_cost = BigUint::from(teams * (n as u128).pow(3));
        let remaining = &product - &self.evaluated;
        self.phase = if self.evaluated == product {
            Phase::Complete
        } else if bound_cost < remaining * n {
            self.bounds = Some(Bounds {
                scores: vec![Vec::new(); n],
                seat_max: vec![i32::MIN; n],
                seat: 0,
                history: 0,
                partial_max: i32::MIN,
            });
            Phase::Bounds
        } else {
            Phase::Search
        };
        self.total = Some(product);
        Ok(())
    }

    pub(crate) fn complete(&self) -> bool {
        matches!(self.phase, Phase::Complete)
    }

    fn evaluate(&mut self, master: &Master, indices: &[usize]) -> Result<(), String> {
        let seats: Vec<_> = indices
            .iter()
            .enumerate()
            .map(|(s, &i)| self.candidates[s][i].seat(self.orders[s], self.seeds[s]))
            .collect();
        let replay = room::replay(master, &self.chart, seats)?;
        self.evaluated += 1u8;
        if self.best.as_ref().is_none_or(|best| {
            replay.total > best.replay.total
                || (replay.total == best.replay.total && indices < best.indices.as_slice())
        }) {
            self.best = Some(Winner {
                indices: indices.to_vec(),
                replay,
            });
        }
        Ok(())
    }

    fn next_bound(&mut self, master: &Master) -> Result<(), String> {
        let bounds = self.bounds.as_mut().unwrap();
        let n = self.candidates.len();
        let s = bounds.seat;
        let k = bounds.history;
        let ranks = [k / (n * n), (k / n) % n, k % n].map(|r| (r + 1) as i32);
        let candidate = &self.candidates[s][bounds.scores[s].len()];
        let score = room::score_for_ranks(
            master,
            &self.chart,
            candidate.seat(self.orders[s], self.seeds[s]),
            ranks,
        )?;
        self.bound_replays += 1u8;
        bounds.partial_max = bounds.partial_max.max(score);
        bounds.history += 1;
        if bounds.history == n.pow(3) {
            bounds.scores[s].push(bounds.partial_max);
            bounds.seat_max[s] = bounds.seat_max[s].max(bounds.partial_max);
            bounds.history = 0;
            bounds.partial_max = i32::MIN;
            if bounds.scores[s].len() == self.candidates[s].len() {
                bounds.seat += 1;
            }
            if bounds.seat == n {
                self.phase = Phase::Search;
            }
        }
        Ok(())
    }

    // The suffixes of a prefix occupy consecutive positions in mixed-radix order.
    fn successor(&self, prefix_len: usize) -> Option<Vec<usize>> {
        let mut next = self.cursor.clone();
        for s in (0..prefix_len).rev() {
            if next[s] + 1 < self.candidates[s].len() {
                next[s] += 1;
                next[s + 1..].fill(0);
                return Some(next);
            }
        }
        None
    }

    fn prunable_prefix(&self, bounds: &Bounds, best: &Winner) -> Option<usize> {
        let mut upper: i64 = bounds.seat_max.iter().map(|&v| i64::from(v)).sum();
        let loses =
            |v| v < best.replay.total || (v == best.replay.total && self.cursor >= best.indices);
        if loses(upper) {
            return Some(0);
        }
        for (s, &i) in self.cursor.iter().enumerate() {
            upper += i64::from(bounds.scores[s][i]) - i64::from(bounds.seat_max[s]);
            if loses(upper) {
                return Some(s + 1);
            }
        }
        None
    }

    fn scan(&mut self, master: &Master) -> Result<(), String> {
        let n = self.candidates.len();
        let cut = self
            .bounds
            .as_ref()
            .and_then(|b| self.prunable_prefix(b, self.best.as_ref().unwrap()));
        let next = self.successor(cut.unwrap_or(n));
        if let Some(prefix_len) = cut {
            let mut block = if prefix_len == 0 {
                self.total.as_ref().unwrap().clone()
            } else {
                self.strides[prefix_len - 1].clone()
            };
            for s in prefix_len..n {
                block -= &self.strides[s] * self.cursor[s];
            }
            let seen = match &next {
                Some(end) => self.probes.range(self.cursor.clone()..end.clone()).count(),
                None => self.probes.range(self.cursor.clone()..).count(),
            };
            self.pruned += block - seen;
        } else if !self.probes.contains(&self.cursor) {
            self.evaluate(master, &self.cursor.clone())?;
        }
        if let Some(indices) = next {
            self.cursor = indices;
        } else {
            debug_assert_eq!(Some(&(&self.pruned + &self.evaluated)), self.total.as_ref());
            self.phase = Phase::Complete;
        }
        Ok(())
    }

    /// One cancellable replay or one Cartesian block. Advance replay cursors only on success.
    pub(crate) fn step_one(&mut self, master: &Master) -> Result<(), String> {
        let probe = self.pending_probe.clone().or_else(|| {
            let &(s, i) = self.unplayed_seeds.first()?;
            let mut indices = self.best.as_ref()?.indices.clone();
            indices[s] = i;
            Some(indices)
        });
        if let Some(indices) = probe {
            // Both geometric probes and queued seeds introduce a previously unplayed candidate.
            // A successful room also covers every other seed that happened to be in its lineup.
            debug_assert!(!self.probes.contains(&indices));
            self.evaluate(master, &indices)?;
            self.unplayed_seeds.retain(|&(s, i)| indices[s] != i);
            self.probes.insert(indices);
            self.pending_probe = None;
            if self.total.as_ref() == Some(&self.evaluated) {
                self.phase = Phase::Complete;
            }
            return Ok(());
        }
        match self.phase {
            Phase::Bounds => self.next_bound(master),
            Phase::Search => self.scan(master),
            Phase::Inventory | Phase::Complete => Ok(()),
        }
    }

    pub(crate) fn response(&self) -> Value {
        let phase = match self.phase {
            Phase::Inventory => "inventory",
            Phase::Bounds => "bounds",
            Phase::Search => "search",
            Phase::Complete => "complete",
        };
        let upper: i64 = if self.complete() {
            self.best.as_ref().unwrap().replay.total
        } else {
            // An open inventory or a partially bounded seat can still produce any native i32 score.
            (0..self.candidates.len())
                .map(|s| {
                    let cap = self
                        .bounds
                        .as_ref()
                        .filter(|b| s < b.seat)
                        .map_or(i32::MAX, |b| b.seat_max[s]);
                    i64::from(cap)
                })
                .sum()
        };
        let teams = self.best.as_ref().map(|best| {
            best.indices
                .iter()
                .enumerate()
                .map(|(s, &i)| {
                    let c = &self.candidates[s][i];
                    json!({"team":c.team, "power":c.power, "score":best.replay.scores[s]})
                })
                .collect::<Vec<_>>()
        });
        json!({
            "phase":phase, "complete":self.complete(), "upperBound":upper,
            "candidateCounts":self.candidates.iter().map(Vec::len).collect::<Vec<_>>(),
            "totalCombinations":self.total.as_ref().map(count), "evaluated":count(&self.evaluated),
            "pruned":count(&self.pruned), "boundReplays":count(&self.bound_replays),
            "teams":teams, "room":self.best.as_ref().map(|best| &best.replay)
        })
    }
}
