//! Conservative LIFE bounds for complete member rosters in damaging plays.
use super::{Chart, Master};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(super) struct Reduction {
    pub(super) from: i32,
    pub(super) through: i32,
    pub(super) points: i32,
}

pub(super) struct Rules {
    pub(super) members: Vec<Protection>,
    pub(super) snaps: Vec<Vec<Protection>>,
    pub(super) reductions: Vec<Vec<Reduction>>,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(super) struct Protection {
    pub(super) healing: i64,
    pub(super) conversion: i64,
    from: i32,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct FormationProtection {
    healing: [i64; 6],
    conversion: i64,
}

impl Default for Protection {
    fn default() -> Self {
        Self { healing: 0, conversion: 0, from: i32::MAX }
    }
}

impl Protection {
    pub(super) const UNBOUNDED: Self = Self { healing: i64::MAX, conversion: i64::MAX, from: i32::MIN };

    pub(super) fn add_conversion(&mut self, (amount, from): (i64, i32)) {
        if amount > 0 {
            self.conversion = self.conversion.saturating_add(amount);
            self.from = self.from.min(from);
        }
    }

    pub(super) fn upper(self, other: Self) -> Self {
        Self {
            healing: self.healing.max(other.healing),
            conversion: self.conversion.max(other.conversion),
            from: self.from.min(other.from),
        }
    }
}

struct Chunk {
    last_note: i32,
    delivery_time: i32,
    counts: [u32; 7],
    events: usize,
}

#[derive(Clone, Copy)]
pub(super) struct Proof {
    pub(super) coefficient: f64,
    prefix: f64,
    mean_prefix: f64,
    mean_coefficient: f64,
    onus: f64,
    pub(super) keeps_breaks: bool,
    pub(super) last_unpenalized_note: i32,
    pub(super) death_by: i32,
}

impl Proof {
    pub(super) fn upper(self, power: f64, unpenalized: Option<f64>, mean: bool) -> f64 {
        let (prefix, coefficient) = if mean {
            (self.mean_prefix, self.mean_coefficient)
        } else {
            (self.prefix, self.coefficient)
        };
        let coarse = (power * coefficient).next_up();
        let Some(upper) = unpenalized.filter(|&value| value.is_finite() && value >= 0.0 && power >= 0.0)
        else { return coarse; };
        let prefix = (power * prefix).next_up();
        let before_death = (prefix * (1.0 - self.onus).next_up()).next_up();
        let after_death = (upper * self.onus).next_up();
        // LIFE is applied to integer note scores. The float multiplication
        // and the three fixed-rank bonuses need outward rounding too.
        let combined = ((after_death + before_death).next_up() * (1.0 + f32::EPSILON as f64)).next_up();
        coarse.min((combined + 4.0).next_up())
    }
}

#[derive(Clone)]
pub(super) struct PartialHealth {
    healing: [i64; 5],
    member_savings: [i64; 5],
    snapshot_savings: [i64; 5],
    first_conversion: i32,
    reductions: Vec<[i32; 5]>,
}

fn add_to_top<T: Copy + Ord>(values: &mut [T; 5], value: T) {
    if value <= values[4] {
        return;
    }
    let index = values.partition_point(|&other| other > value);
    values.copy_within(index..4, index + 1);
    values[index] = value;
}

pub(super) struct ScorePrefix {
    weights: Vec<(i32, [f64; 2])>,
    total: [f64; 2],
    onus: f64,
}

impl ScorePrefix {
    pub(super) fn new(onus: f64, mut weights: Vec<(i32, [f64; 2])>) -> Option<Self> {
        if !(0.0..=1.0).contains(&onus) || weights.is_empty()
            || weights.windows(2).any(|pair| pair[0].0 > pair[1].0)
        {
            return None;
        }
        let mut total = [0.0_f64; 2];
        for (_, weight) in &mut weights {
            for (sum, value) in total.iter_mut().zip(weight) {
                *sum = (*sum + *value).next_up();
                *value = *sum;
            }
        }
        Some(Self { weights, total, onus })
    }

    fn at(&self, last_note: i32, channel: usize) -> f64 {
        let before = self.weights.partition_point(|&(time, _)| time <= last_note);
        before.checked_sub(1).map_or(0.0, |index| self.weights[index].1[channel])
    }

    fn coefficient(&self, prefix: f64, channel: usize) -> f64 {
        ((self.total[channel] * self.onus).next_up()
            + (prefix * (1.0 - self.onus).next_up()).next_up()).next_up()
    }

    pub(super) fn upper_with_last_living_note(&self, power: f64, score_bound: f64, last_note: i32) -> f64 {
        let prefix = self.at(last_note, 0);
        let coefficient = self.coefficient(prefix, 0);
        Proof {
            coefficient, prefix, mean_prefix: prefix, mean_coefficient: coefficient,
            onus: self.onus, keeps_breaks: false, last_unpenalized_note: last_note, death_by: last_note,
        }.upper(power, Some(score_bound), false)
    }
}

pub(super) struct HealthBound {
    initial: i64,
    conversion_cap: i64,
    damage: [i64; 7],
    chunks: Vec<Chunk>,
    reductions: Vec<Vec<i32>>,
    score: Arc<ScorePrefix>,
    members: Vec<Protection>,
    snaps: Vec<Vec<Protection>>,
    max_snap: Vec<Protection>,
    once_per_member: bool,
    mean: bool,
}

impl HealthBound {
    pub(super) fn new(master: &Master, chart: &Chart, rules: Rules, score: Arc<ScorePrefix>, mean: bool) -> Option<Self> {
        if !(1.0..=1_000_000.0).contains(&master.live.life_base)
            || master.live.damage.iter().any(|&value| !(0.0..=100_000.0).contains(&value) || value.fract() != 0.0)
        { return None; }
        let damage = master.live.damage.map(|value| value as i64);
        let mut edges: Vec<_> = chart.full.events.iter().map(|&(_, at)| at)
            .chain(rules.reductions.iter().flatten().flat_map(|row| [row.from, row.through.saturating_add(1)]))
            .chain(chart.times.iter().step_by(16).copied())
            .collect();
        edges.sort_unstable();
        edges.dedup();
        let note_times: rustc_hash::FxHashMap<_, _> = chart.full.notes.iter()
            .map(|note| (note.id, note.time_ms)).collect();
        let mut raw = vec![(i32::MIN, 0, [0u32; 7]); edges.len() + 1];
        let mut delivered = Vec::with_capacity(chart.play.frames.len());
        let mut latest = i32::MIN;
        for (frame, inputs) in chart.play.frames.iter().enumerate() {
            for note in &inputs.judged {
                let time = *note_times.get(&note.note_id)?;
                latest = latest.max(time);
                let grade = note.judgement as usize;
                if damage.get(grade).copied()? == 0 { continue; }
                let (last, delivery, counts) = &mut raw[edges.partition_point(|&edge| edge <= time)];
                *last = (*last).max(time);
                *delivery = (*delivery).max(frame);
                counts[grade] = counts[grade].checked_add(1)?;
            }
            delivered.push(latest);
        }
        let mut chunks = Vec::new();
        let mut openings = Vec::new();
        let mut prefix_delivery = 0;
        for (index, (last_damage, delivery, counts)) in raw.into_iter().enumerate() {
            if last_damage == i32::MIN { continue; }
            prefix_delivery = prefix_delivery.max(delivery);
            let events = chart.full.events.iter().filter(|event| event.1 <= last_damage).count();
            // Delayed MISS delivery can follow newer hits. Keep full score
            // credit through the newest note seen when this damage is known.
            let last_note = delivered[prefix_delivery];
            chunks.push(Chunk { last_note, delivery_time: chart.play.frames[prefix_delivery].time_ms, counts, events });
            openings.push(index.checked_sub(1).map_or(i32::MIN, |index| edges[index]));
        }
        let reductions = rules.reductions.into_iter().map(|rows| {
            openings.iter().map(|&start| rows.iter()
                .filter(|row| row.from <= start && start <= row.through)
                .fold(0i32, |sum, row| sum.saturating_add(row.points).min(10_000))).collect()
        }).collect();
        let events = &chart.full.events;
        let once_per_member = events.len() <= 5 && events.iter().enumerate().all(|(index, &(activation, _))| {
            activation < 5 && events[..index].iter().all(|&(other, _)| other != activation)
        });
        let mut max_snap = vec![Protection::default(); rules.members.len()];
        let mut conversion_caps = Vec::new();
        for skills in &rules.snaps {
            let mut conversion = 0;
            for (maximum, &value) in max_snap.iter_mut().zip(skills) {
                *maximum = maximum.upper(value);
                conversion = conversion.max(value.conversion);
            }
            conversion_caps.push(conversion);
        }
        conversion_caps.sort_unstable_by(|a, b| b.cmp(a));
        let conversion_cap = conversion_caps.into_iter().take(5).fold(0i64, i64::saturating_add);
        Some(Self { initial: master.live.life_base as i64, conversion_cap,
            damage, chunks, reductions, score,
            members: rules.members, snaps: rules.snaps, max_snap, once_per_member, mean })
    }

    pub(super) fn roster(&self, members: &[usize]) -> Option<Proof> {
        self.prove(members, std::array::from_fn(|slot| self.max_snap[members[slot]]), None)
    }

    pub(super) fn snap_protection(&self, member: usize, snap: i32) -> Protection {
        if snap < 0 { Protection::default() } else { self.snaps[snap as usize].get(member).copied().unwrap_or_default() }
    }

    pub(super) fn member_can_reduce_damage(&self, member: usize, options: &[i32]) -> bool {
        self.members[member].conversion > 0 || self.max_snap[member].conversion > 0
            && options.iter().any(|&snap| self.snap_protection(member, snap).conversion > 0)
    }

    pub(super) fn formation_protection(&self, members: &[usize], options: &[Vec<i32>]) -> FormationProtection {
        let mut physical = vec![[Protection::default(); 5]; self.snaps.len()];
        for (slot, (&member, choices)) in members.iter().zip(options).enumerate() {
            for &snap in choices {
                if snap >= 0 { physical[snap as usize][slot] = self.snap_protection(member, snap); }
            }
        }
        let mut best = [Protection::default(); 32];
        for choices in physical {
            if choices.iter().all(|choice| *choice == Protection::default()) { continue; }
            // Descending masks use this physical snapshot at most once.
            for mask in (0..32).rev() {
                let current = best[mask];
                for (slot, choice) in choices.iter().enumerate() {
                    let bit = 1 << slot;
                    if mask & bit != 0 || *choice == Protection::default() { continue; }
                    best[mask | bit] = best[mask | bit].upper(Protection {
                        healing: current.healing.saturating_add(choice.healing),
                        conversion: current.conversion.saturating_add(choice.conversion),
                        from: current.from.min(choice.from),
                    });
                }
            }
        }
        let mut healing = [0i64; 6];
        for (mask, value) in best.iter().enumerate() {
            let count = mask.count_ones() as usize;
            let total = members.iter().enumerate().filter(|&(slot, _)| mask & (1 << slot) != 0)
                .fold(value.healing, |sum, (_, &member)| sum.saturating_add(self.members[member].healing));
            healing[count] = healing[count].max(total);
        }
        FormationProtection { healing, conversion: best[31].conversion }
    }

    pub(super) fn prove(&self, members: &[usize], snaps: [Protection; 5], formation: Option<FormationProtection>) -> Option<Proof> {
        let healing: [i64; 5] = std::array::from_fn(|slot| {
            self.members[members[slot]].healing.saturating_add(snaps[slot].healing)
        });
        let snap_savings = snaps.iter().fold(0i64, |sum, snap| sum.saturating_add(snap.conversion))
            .min(formation.map_or(self.conversion_cap, |caps| caps.conversion.min(self.conversion_cap)));
        let prevented_damage = members.iter().fold(snap_savings, |sum, &member| {
            sum.saturating_add(self.members[member].conversion)
        });
        let first = members.iter().map(|&member| self.members[member].from)
            .chain(snaps.iter().map(|snap| snap.from)).min().unwrap_or(i32::MAX);
        self.prove_caps(healing, prevented_damage, first, formation.map(|caps| caps.healing), |chunk| {
            members.iter().fold(0i32, |sum, &member| (sum + self.reductions[member][chunk]).min(10_000))
        })
    }

    pub(super) fn partial_members(&self, characters: &[Vec<usize>]) -> Vec<PartialHealth> {
        let empty = PartialHealth {
            healing: [0; 5],
            member_savings: [0; 5],
            snapshot_savings: [0; 5],
            first_conversion: i32::MAX,
            reductions: vec![[0; 5]; self.chunks.len()],
        };
        let mut suffix = vec![empty; characters.len() + 1];
        for index in (0..characters.len()).rev() {
            let members = &characters[index];
            let mut current = suffix[index + 1].clone();
            let healing = members.iter().map(|&member| {
                self.members[member].healing.saturating_add(self.max_snap[member].healing)
            }).max().unwrap_or(0);
            add_to_top(&mut current.healing, healing);
            add_to_top(&mut current.member_savings,
                members.iter().map(|&member| self.members[member].conversion).max().unwrap_or(0));
            add_to_top(&mut current.snapshot_savings,
                members.iter().map(|&member| self.max_snap[member].conversion).max().unwrap_or(0));
            current.first_conversion = members.iter().fold(current.first_conversion, |value, &member| {
                value.min(self.members[member].from).min(self.max_snap[member].from)
            });
            for (chunk, active) in current.reductions.iter_mut().enumerate() {
                let bound = members.iter().map(|&member| self.reductions[member][chunk]).max().unwrap_or(0);
                add_to_top(active, bound);
            }
            suffix[index] = current;
        }
        suffix
    }

    pub(super) fn prove_partial(&self, selected: &[usize], left: usize, suffix: &PartialHealth, allow_conversion: bool) -> Option<Proof> {
        if selected.len() + left != 5 {
            return None;
        }
        let mut healing = [0; 5];
        let mut first = if left > 0 { suffix.first_conversion } else { i32::MAX };
        let mut member_savings = suffix.member_savings[..left].iter().copied().fold(0i64, i64::saturating_add);
        let mut snapshot_savings = suffix.snapshot_savings[..left].iter().copied().fold(0i64, i64::saturating_add);
        for (index, &member) in selected.iter().enumerate() {
            healing[index] = self.members[member].healing.saturating_add(self.max_snap[member].healing);
            member_savings = member_savings.saturating_add(self.members[member].conversion);
            snapshot_savings = snapshot_savings.saturating_add(self.max_snap[member].conversion);
            first = first.min(self.members[member].from).min(self.max_snap[member].from);
        }
        healing[selected.len()..].copy_from_slice(&suffix.healing[..left]);
        let prevented = if allow_conversion {
            member_savings.saturating_add(snapshot_savings.min(self.conversion_cap))
        } else { 0 };
        // Each remaining character contributes at most one member. The
        // independent maxima only grant extra recovery and damage reduction.
        self.prove_caps(healing, prevented, first, None, |chunk| {
            let remaining: i32 = suffix.reductions[chunk][..left].iter().sum();
            selected.iter().fold(remaining, |sum, &member| sum + self.reductions[member][chunk]).min(10_000)
        })
    }

    pub(super) fn global(&self, allow_conversion: bool) -> Option<Proof> {
        fn strongest<T: Copy + Default + Ord>(values: impl Iterator<Item = T>) -> [T; 5] {
            let mut best = [T::default(); 5];
            for value in values {
                add_to_top(&mut best, value);
            }
            best
        }
        let healing = strongest(self.members.iter().zip(&self.max_snap)
            .map(|(member, snap)| member.healing.saturating_add(snap.healing)));
        let prevented_damage = if allow_conversion {
            strongest(self.members.iter().map(|member| member.conversion))
                .into_iter().fold(self.conversion_cap, i64::saturating_add)
        } else { 0 };
        let first = self.members.iter().chain(&self.max_snap)
            .map(|protection| protection.from).min().unwrap_or(i32::MAX);
        self.prove_caps(healing, prevented_damage, first, None, |chunk| {
            strongest(self.reductions.iter().map(|row| row[chunk])).into_iter().sum::<i32>().min(10_000)
        })
    }

    fn prove_caps(&self, mut healing: [i64; 5], prevented_damage: i64, first: i32, formation: Option<[i64; 6]>, reduction_at: impl Fn(usize) -> i32) -> Option<Proof> {
        healing.sort_unstable_by(|a, b| b.cmp(a));
        let max = healing[0];
        let mut prefix = [0i64; 6];
        for (slot, &gain) in healing.iter().enumerate() {
            prefix[slot + 1] = prefix[slot].saturating_add(gain);
        }
        // A mask counts the surviving prefixes of orders that have fired the
        // same set of skills. A dead prefix can never recover after LIFE hits zero.
        const REMAINING_ORDERS: [u16; 6] = [120, 24, 6, 2, 1, 1];
        let order_caps = (self.mean && self.once_per_member && healing[0] != healing[4]).then(|| {
            let mut caps = [0i64; 32];
            for mask in 1usize..32 {
                let slot = mask.trailing_zeros() as usize;
                caps[mask] = caps[mask & (mask - 1)].saturating_add(healing[slot]);
            }
            caps
        });
        let mut orders = [0u16; 32];
        orders[0] = 1;
        let mut fired = 0;
        let mut ended = 0u16;
        let mut mean_prefix = 0.0_f64;
        let keeps_breaks = prevented_damage == 0;
        let score_channel = usize::from(keeps_breaks);
        let mut loss = 0i64;
        for (index, chunk) in self.chunks.iter().enumerate() {
            let reduction = reduction_at(index);
            for (count, damage) in chunk.counts.iter().zip(self.damage) {
                let applied = (10_000 - reduction as i64) * damage / 10_000;
                loss = loss.saturating_add(i64::from(*count) * applied);
            }
            let healed = if self.once_per_member {
                formation.map_or(prefix[chunk.events], |caps| prefix[chunk.events].min(caps[chunk.events]))
            }
                else { (chunk.events as i64).saturating_mul(max) };
            let prevented = if chunk.delivery_time < first { 0 } else { prevented_damage };
            if prevented == i64::MAX { return None; }
            let damage = loss.saturating_sub(prevented);
            let dead = damage >= self.initial.saturating_add(healed);
            let mut latest = None;
            if let Some(order_caps) = order_caps.as_ref().filter(|_| ended < 120 && chunk.events > 0) {
                while fired < chunk.events {
                    for mask in 0usize..32 {
                        let count = orders[mask];
                        if mask.count_ones() as usize != fired || count == 0 { continue; }
                        let mut unused = 31 & !mask;
                        while unused != 0 {
                            let bit = 1 << unused.trailing_zeros();
                            orders[mask | bit] += count;
                            unused ^= bit;
                        }
                    }
                    fired += 1;
                }
                let mut newly_ended = 0;
                for mask in 0usize..32 {
                    if mask.count_ones() as usize != fired || orders[mask] == 0 { continue; }
                    let cap = order_caps[mask].min(healed);
                    if damage >= self.initial.saturating_add(cap) {
                        newly_ended += std::mem::take(&mut orders[mask]) * REMAINING_ORDERS[fired];
                    }
                }
                if newly_ended > 0 {
                    let before = self.score.at(chunk.last_note, score_channel);
                    mean_prefix = (mean_prefix + (before * f64::from(newly_ended)).next_up()).next_up();
                    latest = Some(before);
                    ended += newly_ended;
                }
            }
            if dead {
                debug_assert!(order_caps.is_none() || chunk.events == 0 || ended == 120);
                let prefix = latest.unwrap_or_else(|| self.score.at(chunk.last_note, score_channel));
                let mean_prefix = if ended == 0 { prefix } else {
                    let rest = (prefix * f64::from(120 - ended)).next_up();
                    (((mean_prefix + rest).next_up() / 120.0).next_up()).min(prefix)
                };
                let coefficient = |prefix: f64| self.score.coefficient(prefix, score_channel);
                return Some(Proof { coefficient: coefficient(prefix), prefix, mean_prefix,
                    mean_coefficient: coefficient(mean_prefix), onus: self.score.onus, keeps_breaks,
                    last_unpenalized_note: chunk.last_note, death_by: chunk.delivery_time });
            }
        }
        None
    }
}
