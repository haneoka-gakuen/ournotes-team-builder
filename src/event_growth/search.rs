//! Incremental bounds over effective fixed bonuses. Historical witnesses use
//! small integer domains; stable records are searched before widening growth.
use super::*;
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashSet},
};
pub(super) type KernelBank = Arc<std::sync::Mutex<Vec<Option<Arc<Vec<i64>>>>>>;

#[derive(Clone)]
pub(super) struct Ranked {
    pub configuration: Configuration,
    variation: i32,
    dimensions: usize,
    quality: f64,
    pub display: String,
}
impl Ranked {
    pub fn compare(&self, other: &Self) -> Ordering {
        self.configuration
            .changed
            .cmp(&other.configuration.changed)
            .then(self.variation.cmp(&other.variation))
            .then(self.dimensions.cmp(&other.dimensions))
            .then(
                other
                    .configuration
                    .vip_bonus
                    .cmp(&self.configuration.vip_bonus),
            )
            .then(self.quality.total_cmp(&other.quality))
            .then(self.configuration.id.cmp(&other.configuration.id))
    }
}
struct Node {
    lo: Vec<i32>,
    hi: Vec<i32>,
    cost: f64,
    order: u64,
    shift: i32,
    candidate: Option<Ranked>,
}
impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.order == other.order && self.cost == other.cost
    }
}
impl Eq for Node {}
impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .total_cmp(&self.cost)
            .then(other.order.cmp(&self.order))
    }
}
#[derive(Clone)]
struct Canonical {
    levels: Vec<i32>,
    total: Total,
    range: Range,
    quality: f64,
}
#[derive(Clone, Copy)]
struct Bounds {
    total: Total,
    min: i32,
    max: i32,
    upper_height: f64,
}

pub(super) struct Tier {
    problem: Arc<Problem>,
    pub vip: i32,
    vip_rank: i32,
    minimums: Vec<i32>,
    item_min: i32,
    rank_span: i32,
    item_span: i32,
    relaxed: bool,
    queue: BinaryHeap<Node>,
    kernels: KernelBank,
    seen: HashSet<String>,
    order: u64,
    pub visited: usize,
    pub produced: usize,
}
impl Tier {
    pub fn new(
        problem: Arc<Problem>,
        vip: i32,
        vip_rank: i32,
        preference: f64,
        radius: (i32, i32),
    ) -> Self {
        let minimums = problem
            .caps
            .iter()
            .map(|&cap| {
                if preference < 0.0 {
                    1
                } else {
                    15.max((cap as f64 * preference).ceil() as i32)
                }
            })
            .collect();
        let kernels = Arc::clone(&problem.kernels[&vip]);
        let mut tier = Self {
            problem,
            vip,
            vip_rank,
            minimums,
            item_min: if preference < 0.0 { 0 } else { 50 },
            rank_span: radius.0,
            item_span: radius.1,
            relaxed: preference < 0.0,
            queue: BinaryHeap::new(),
            kernels,
            seen: HashSet::new(),
            order: 0,
            visited: 0,
            produced: 0,
        };
        if tier
            .problem
            .caps
            .iter()
            .zip(&tier.minimums)
            .any(|(cap, min)| cap < min)
            || tier
                .problem
                .bands
                .iter()
                .any(|band| band.cap < tier.item_min)
            || radius == (0, 0) && tier.problem.conflict
        {
            return tier;
        }
        let mut root = Node {
            lo: tier
                .minimums
                .iter()
                .map(|rank| rank - 1)
                .chain(tier.problem.bands.iter().map(|_| tier.item_min))
                .collect(),
            hi: tier
                .problem
                .caps
                .iter()
                .map(|rank| rank - 1 + tier.problem.totals.last().unwrap().bonus / 5)
                .chain(tier.problem.bands.iter().map(|band| band.cap))
                .collect(),
            cost: 0.0,
            order: 0,
            shift: 0,
            candidate: None,
        };
        if tier.tighten(&mut root) {
            tier.queue.push(root);
        }
        tier
    }
    pub fn finished(&self) -> bool {
        self.queue.is_empty() || self.produced >= 32
    }
    fn kernel(&mut self, observation: usize, slot: usize, flat: i32) -> Arc<Vec<i64>> {
        let index = (observation * 5 + slot) * (self.problem.max_flat as usize + 1) + flat as usize;
        if let Some(cached) = self.kernels.lock().unwrap()[index].as_ref() {
            return Arc::clone(cached);
        }
        let ctx = &self.problem;
        let reading = &ctx.readings[observation];
        let member = &reading.team[slot];
        let snap = reading.snaps[slot].as_ref();
        let source = ctx.flat_sources[flat as usize];
        let mut player = empty_player();
        player
            .character_ranks
            .insert(member.card.character_id, source.0);
        player.total_rank = source.1;
        player.vip_rank = self.vip_rank;
        let effects = &ctx.master.events[&ctx.event].effects;
        let base = power::slot_base(
            &ctx.master,
            &player,
            member,
            Some(&reading.music),
            power::member_event_percent(effects, member, 2),
        );
        let fixed = base.fixed_points
            + power::snap_points(
                &ctx.master,
                &base,
                member,
                snap,
                snap.map(|snap| power::snap_event_percent(effects, snap, 2))
                    .unwrap_or(0),
            )
            + power::leader_points(&base, &reading.percents[slot]);
        let values = Arc::new(
            (0..=ctx.bands[reading.bands[slot]].cap)
                .map(|sum| {
                    fixed + power::points(&power::pct_floor(&base.common, &[sum as i64 * 10; 3]))
                })
                .collect(),
        );
        self.kernels.lock().unwrap()[index] = Some(Arc::clone(&values));
        values
    }
    fn options(&self, node: &Node) -> Vec<Bounds> {
        self.problem
            .totals
            .iter()
            .filter_map(|&total| {
                let offset = total.bonus / 5;
                let low: Vec<_> = self
                    .minimums
                    .iter()
                    .enumerate()
                    .map(|(i, &min)| min.max(node.lo[i] - offset + 1))
                    .collect();
                let high: Vec<_> = self
                    .problem
                    .caps
                    .iter()
                    .enumerate()
                    .map(|(i, &cap)| cap.min(node.hi[i] - offset + 1))
                    .collect();
                if low.iter().zip(&high).any(|(lo, hi)| lo > hi) {
                    return None;
                }
                let low_sum = low.iter().sum::<i32>();
                let min = total.min.max(low_sum + self.problem.unseen_count);
                let max = total
                    .max
                    .min(high.iter().sum::<i32>() + self.problem.unseen_cap);
                let outside = if self.problem.unseen_cap == 0 {
                    1.0
                } else {
                    (max - low_sum) as f64 / self.problem.unseen_cap as f64
                };
                let height = high
                    .iter()
                    .zip(&self.problem.caps)
                    .map(|(&rank, &cap)| rank as f64 / cap as f64)
                    .fold(outside, f64::min);
                (min <= max).then_some(Bounds {
                    total,
                    min,
                    max,
                    upper_height: height,
                })
            })
            .collect()
    }
    fn lower_cost(&self, node: &Node) -> f64 {
        self.options(node)
            .into_iter()
            .map(|option| (1.0 - option.upper_height) * 1000.0)
            .fold(f64::INFINITY, f64::min)
    }
    fn quality(&self, levels: &[i32], range: Range) -> f64 {
        let sum = levels.iter().sum::<i32>();
        let normalized: Vec<_> = levels
            .iter()
            .zip(&self.problem.caps)
            .map(|(&rank, &cap)| rank as f64 / cap as f64)
            .collect();
        let average = normalized.iter().sum::<f64>() / normalized.len() as f64;
        let representative = (sum + (self.problem.unseen_cap as f64 * average).round() as i32)
            .clamp(range.min, range.max);
        let outside = if self.problem.unseen_cap == 0 {
            average
        } else {
            (representative - sum) as f64 / self.problem.unseen_cap as f64
        };
        let height = normalized.iter().copied().fold(outside, f64::min);
        let spread = (normalized
            .iter()
            .map(|value| (value - average).powi(2))
            .sum::<f64>()
            + (outside - average).powi(2))
            / (normalized.len() + 1) as f64;
        (1.0 - height) * 1000.0 + spread * 10.0
    }
    fn value(
        &mut self,
        observation: usize,
        node: &Node,
        upper: bool,
        variable: Option<(usize, i32)>,
    ) -> i64 {
        let problem = Arc::clone(&self.problem);
        let reading = &problem.readings[observation];
        let drift = observation != 0;
        let pick = |index: usize| {
            variable
                .filter(|row| row.0 == index)
                .map(|row| row.1)
                .unwrap_or(if upper {
                    node.hi[index]
                } else {
                    node.lo[index]
                })
        };
        let direction = if upper { 1 } else { -1 };
        (0..5)
            .map(|slot| {
                let flat = (pick(reading.characters[slot])
                    + if drift { direction * node.shift } else { 0 })
                .clamp(0, problem.max_flat);
                let band = reading.bands[slot];
                let gear = (pick(problem.ids.len() + band)
                    + if drift { direction * self.item_span } else { 0 })
                .clamp(self.item_min, problem.bands[band].cap);
                self.kernel(observation, slot, flat)[gear as usize]
            })
            .sum()
    }
    fn tighten(&mut self, node: &mut Node) -> bool {
        let options = self.options(node);
        if options.is_empty() {
            return false;
        }
        node.shift = 0;
        if self.rank_span != 0 || self.item_span != 0 {
            let distance = self.problem.master.characters.len() as i32 * self.rank_span;
            for option in &options {
                for total in &self.problem.totals {
                    if total.min <= option.max + distance && total.max >= option.min - distance {
                        node.shift = node
                            .shift
                            .max(self.rank_span + (total.bonus - option.total.bonus).abs() / 5);
                    }
                }
            }
        }
        for _ in 0..2 {
            let mut changed = false;
            for observation in 0..self.problem.readings.len() {
                let target = self.problem.readings[observation].target;
                if target < self.value(observation, node, false, None)
                    || target > self.value(observation, node, true, None)
                {
                    return false;
                }
                for variable in self.problem.ids.len()..node.lo.len() {
                    let (mut lo, mut hi) = (node.lo[variable], node.hi[variable]);
                    while lo < hi {
                        let mid = (lo + hi) / 2;
                        if self.value(observation, node, true, Some((variable, mid))) < target {
                            lo = mid + 1;
                        } else {
                            hi = mid;
                        }
                    }
                    if lo > node.lo[variable] {
                        node.lo[variable] = lo;
                        changed = true;
                    }
                    (lo, hi) = (node.lo[variable], node.hi[variable]);
                    while lo < hi {
                        let mid = (lo + hi + 1) / 2;
                        if self.value(observation, node, false, Some((variable, mid))) > target {
                            hi = mid - 1;
                        } else {
                            lo = mid;
                        }
                    }
                    if lo < node.hi[variable] {
                        node.hi[variable] = lo;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        node.cost = self.lower_cost(node);
        node.cost.is_finite()
    }
    fn canonical(&self, flat: &[i32]) -> Vec<Canonical> {
        let mut options: Vec<_> = self
            .problem
            .totals
            .iter()
            .filter_map(|&total| {
                let levels: Vec<_> = flat
                    .iter()
                    .map(|value| value - total.bonus / 5 + 1)
                    .collect();
                if levels
                    .iter()
                    .enumerate()
                    .any(|(i, &value)| value < self.minimums[i] || value > self.problem.caps[i])
                {
                    return None;
                }
                let sum = levels.iter().sum::<i32>();
                let range = Range {
                    min: total.min.max(sum + self.problem.unseen_count),
                    max: total.max.min(sum + self.problem.unseen_cap),
                };
                (range.min <= range.max).then(|| Canonical {
                    quality: self.quality(&levels, range),
                    levels,
                    total,
                    range,
                })
            })
            .collect();
        options.sort_by(|a, b| {
            a.quality
                .total_cmp(&b.quality)
                .then(b.range.min.cmp(&a.range.min))
        });
        options
    }
    pub fn advance(&mut self) -> Option<Ranked> {
        let node = self.queue.pop()?;
        self.visited += 1;
        if let Some(candidate) = node.candidate {
            if self.seen.insert(candidate.configuration.id.clone()) {
                self.produced += 1;
                return Some(candidate);
            }
            return None;
        }
        let size = self.problem.ids.len();
        let split = (0..node.lo.len())
            .filter(|&i| node.hi[i] > node.lo[i])
            .max_by_key(|&i| (node.hi[i] - node.lo[i]) * if i < size { 1 } else { 2 });
        if let Some(index) = split {
            let mid = (node.lo[index] + node.hi[index]) / 2;
            for high in [true, false] {
                self.order += 1;
                let mut child = Node {
                    lo: node.lo.clone(),
                    hi: node.hi.clone(),
                    cost: 0.0,
                    order: self.order,
                    shift: 0,
                    candidate: None,
                };
                if high {
                    child.lo[index] = mid + 1;
                } else {
                    child.hi[index] = mid;
                }
                if self.tighten(&mut child) {
                    self.queue.push(child);
                }
            }
            return None;
        }
        if self.value(0, &node, false, None) != self.problem.readings[0].target {
            return None;
        }
        let flat = &node.lo[..size];
        let gears = &node.lo[size..];
        let mut best = f64::INFINITY;
        for option in self.canonical(flat) {
            if option.quality >= best {
                break;
            }
            if let Some(candidate) = self.configuration(flat, gears, &option) {
                self.order += 1;
                let cost = candidate.quality;
                best = best.min(cost);
                self.queue.push(Node {
                    lo: node.lo.clone(),
                    hi: node.hi.clone(),
                    cost,
                    order: self.order,
                    shift: node.shift,
                    candidate: Some(candidate),
                });
            }
        }
        None
    }
    fn configuration(&mut self, flat: &[i32], gears: &[i32], option: &Canonical) -> Option<Ranked> {
        let original = (self.rank_span, self.item_span);
        let mut found = None;
        for radius in super::RADII {
            if radius.0 > original.0 || radius.1 > original.1 {
                continue;
            }
            (self.rank_span, self.item_span) = radius;
            found = self.configuration_at_radius(flat, gears, option);
            if found.is_some() {
                break;
            }
        }
        (self.rank_span, self.item_span) = original;
        found
    }
    fn configuration_at_radius(
        &mut self,
        flat: &[i32],
        gears: &[i32],
        option: &Canonical,
    ) -> Option<Ranked> {
        let anchor = Witness {
            challenge_id: self.problem.readings[0].challenge,
            levels: option.levels.clone(),
            instruments: gears.to_vec(),
            total_rank: option.range,
            total_value: option.range.min,
        };
        let own = option.levels.iter().sum::<i32>();
        let mut local: Vec<Option<Vec<Witness>>> = vec![None; self.problem.readings.len()];
        let mut witnesses = vec![anchor];
        let outside = Range {
            min: option.range.min - own,
            max: self
                .problem
                .unseen_cap
                .min(option.range.max - own + self.problem.unseen_count * self.rank_span),
        };
        let accepted = self.join(
            1,
            option,
            gears,
            &mut local,
            &mut witnesses,
            &option.levels,
            &option.levels,
            gears,
            gears,
            outside,
        )?;
        let size = self.problem.ids.len();
        let minimum: Vec<_> = (0..size)
            .map(|i| accepted.iter().map(|row| row.levels[i]).min().unwrap())
            .collect();
        let maximum: Vec<_> = (0..size)
            .map(|i| accepted.iter().map(|row| row.levels[i]).max().unwrap())
            .collect();
        let minimum_items: Vec<_> = (0..gears.len())
            .map(|i| accepted.iter().map(|row| row.instruments[i]).min().unwrap())
            .collect();
        let maximum_items: Vec<_> = (0..gears.len())
            .map(|i| accepted.iter().map(|row| row.instruments[i]).max().unwrap())
            .collect();
        let ctx = &self.problem;
        for witness in &accepted {
            let reading = ctx
                .readings
                .iter()
                .find(|reading| reading.challenge == witness.challenge_id)?;
            let player = ctx.player(
                &witness.levels,
                &witness.instruments,
                witness.total_value,
                self.vip_rank,
            );
            if power::team_power(
                &ctx.master,
                &player,
                &reading.team,
                &reading.snaps,
                2,
                Some(&reading.music),
                &ctx.master.events[&ctx.event].effects,
            )
            .ok()?
            .total
                != reading.target
            {
                return None;
            }
        }
        let globals: Vec<_> = accepted
            .iter()
            .map(|row| {
                ctx.totals
                    .iter()
                    .find(|total| total.min <= row.total_value && total.max >= row.total_value)
                    .unwrap()
                    .bonus
            })
            .collect();
        let global_span = globals.iter().max().unwrap() - globals.iter().min().unwrap();
        let variation = minimum
            .iter()
            .zip(&maximum)
            .map(|(lo, hi)| (hi - lo) * 5)
            .sum::<i32>()
            + minimum_items
                .iter()
                .zip(&maximum_items)
                .map(|(lo, hi)| hi - lo)
                .sum::<i32>()
            + global_span;
        let dimensions = minimum
            .iter()
            .zip(&maximum)
            .filter(|(lo, hi)| lo != hi)
            .count()
            + minimum_items
                .iter()
                .zip(&maximum_items)
                .filter(|(lo, hi)| lo != hi)
                .count()
            + usize::from(global_span != 0);
        let id = format!(
            "v{}q{}b{}",
            self.vip,
            flat.iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join("-"),
            gears
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join("-")
        );
        let configuration = Configuration {
            id,
            characters: ctx
                .ids
                .iter()
                .enumerate()
                .map(|(i, &id)| Character {
                    id,
                    level: maximum[i],
                    min_level: minimum[i],
                    cap: ctx.caps[i],
                })
                .collect(),
            instruments: ctx
                .bands
                .iter()
                .enumerate()
                .map(|(i, band)| Instrument {
                    id: band.id,
                    total: maximum_items[i],
                    min_total: minimum_items[i],
                    cap: band.cap,
                })
                .collect(),
            total_rank: accepted[0].total_rank,
            vip_bonus: self.vip,
            matched_challenges: ctx.originals.iter().map(|row| row.0).collect(),
            relaxed: self.relaxed,
            changed: variation != 0,
            witnesses: ctx
                .originals
                .iter()
                .map(|&(challenge, model)| {
                    let mut row = accepted
                        .iter()
                        .find(|row| row.challenge_id == ctx.readings[model].challenge)
                        .unwrap()
                        .clone();
                    row.challenge_id = challenge;
                    row
                })
                .collect(),
        };
        let display = json!([
            self.vip,
            configuration
                .characters
                .iter()
                .map(|row| [row.min_level, row.level])
                .collect::<Vec<_>>(),
            configuration
                .instruments
                .iter()
                .map(|row| [row.min_total, row.total])
                .collect::<Vec<_>>()
        ])
        .to_string();
        Some(Ranked {
            quality: self.quality(&option.levels, accepted[0].total_rank),
            configuration,
            variation,
            dimensions,
            display,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn join(
        &mut self,
        index: usize,
        option: &Canonical,
        gears: &[i32],
        local: &mut [Option<Vec<Witness>>],
        witnesses: &mut Vec<Witness>,
        min_levels: &[i32],
        max_levels: &[i32],
        min_items: &[i32],
        max_items: &[i32],
        outside: Range,
    ) -> Option<Vec<Witness>> {
        if index == self.problem.readings.len() {
            let own = option.levels.iter().sum::<i32>();
            let upper = (option.range.max - own).clamp(outside.min, outside.max);
            return Some(
                witnesses
                    .iter()
                    .map(|witness| {
                        let sum = witness.levels.iter().sum::<i32>();
                        Witness {
                            total_value: witness
                                .total_rank
                                .min
                                .max(sum + upper - self.problem.unseen_count * self.rank_span),
                            total_rank: Range {
                                min: witness.total_rank.min.max(
                                    sum + outside.min - self.problem.unseen_count * self.rank_span,
                                ),
                                max: witness.total_rank.max.min(sum + outside.max),
                            },
                            ..witness.clone()
                        }
                    })
                    .collect(),
            );
        }
        if local[index].is_none() {
            local[index] = Some(self.local_witnesses(index, option, gears));
        }
        for position in 0..local[index].as_ref().unwrap().len() {
            let witness = local[index].as_ref().unwrap()[position].clone();
            if witness.levels.iter().enumerate().any(|(i, &value)| {
                max_levels[i].max(value) - min_levels[i].min(value) > self.rank_span
            }) || witness.instruments.iter().enumerate().any(|(i, &value)| {
                max_items[i].max(value) - min_items[i].min(value) > self.item_span
            }) {
                continue;
            }
            let sum = witness.levels.iter().sum::<i32>();
            let range = Range {
                min: outside.min.max(witness.total_rank.min - sum),
                max: outside
                    .max
                    .min(witness.total_rank.max - sum + self.problem.unseen_count * self.rank_span),
            };
            if range.min > range.max {
                continue;
            }
            let lo: Vec<_> = min_levels
                .iter()
                .zip(&witness.levels)
                .map(|(a, b)| (*a).min(*b))
                .collect();
            let hi: Vec<_> = max_levels
                .iter()
                .zip(&witness.levels)
                .map(|(a, b)| (*a).max(*b))
                .collect();
            let items_lo: Vec<_> = min_items
                .iter()
                .zip(&witness.instruments)
                .map(|(a, b)| (*a).min(*b))
                .collect();
            let items_hi: Vec<_> = max_items
                .iter()
                .zip(&witness.instruments)
                .map(|(a, b)| (*a).max(*b))
                .collect();
            witnesses.push(witness);
            let accepted = self.join(
                index + 1,
                option,
                gears,
                local,
                witnesses,
                &lo,
                &hi,
                &items_lo,
                &items_hi,
                range,
            );
            witnesses.pop();
            if accepted.is_some() {
                return accepted;
            }
        }
        None
    }
    fn local_witnesses(
        &mut self,
        observation: usize,
        option: &Canonical,
        gears: &[i32],
    ) -> Vec<Witness> {
        let ctx = Arc::clone(&self.problem);
        let totals: Vec<_> = ctx
            .totals
            .iter()
            .copied()
            .filter(|total| {
                total.min <= option.range.max + ctx.master.characters.len() as i32 * self.rank_span
                    && total.max
                        >= option.range.min - ctx.master.characters.len() as i32 * self.rank_span
            })
            .collect();
        let mut totals = totals;
        totals.sort_by_key(|total| (total.bonus - option.total.bonus).abs());
        let reading = &ctx.readings[observation];
        let min_gear: Vec<_> = gears
            .iter()
            .map(|value| self.item_min.max(value - self.item_span))
            .collect();
        let max_gear: Vec<_> = ctx
            .bands
            .iter()
            .zip(gears)
            .map(|(band, value)| band.cap.min(value + self.item_span))
            .collect();
        let min_level: Vec<_> = option
            .levels
            .iter()
            .enumerate()
            .map(|(i, value)| self.minimums[i].max(value - self.rank_span))
            .collect();
        let max_level: Vec<_> = option
            .levels
            .iter()
            .enumerate()
            .map(|(i, value)| ctx.caps[i].min(value + self.rank_span))
            .collect();
        let mut result = Vec::new();
        for total in totals {
            let values: Vec<Vec<_>> = (0..5)
                .map(|slot| {
                    (min_level[reading.characters[slot]]..=max_level[reading.characters[slot]])
                        .map(|level| self.kernel(observation, slot, level - 1 + total.bonus / 5))
                        .collect()
                })
                .collect();
            let mut levels = option.levels.clone();
            let mut stack = vec![(0usize, 0usize)];
            while let Some((position, domain_index)) = stack.pop() {
                if position < 5 {
                    let character = reading.characters[position];
                    let mut domain: Vec<_> =
                        (min_level[character]..=max_level[character]).collect();
                    domain.sort_by_key(|level| ((level - option.levels[character]).abs(), -level));
                    if domain_index >= domain.len() {
                        levels[character] = option.levels[character];
                        continue;
                    }
                    levels[character] = domain[domain_index];
                    stack.push((position, domain_index + 1));
                    let mut low = 0;
                    let mut high = 0;
                    for slot in 0..5 {
                        let character = reading.characters[slot];
                        let l = if slot <= position {
                            levels[character]
                        } else {
                            min_level[character]
                        };
                        let h = if slot <= position {
                            levels[character]
                        } else {
                            max_level[character]
                        };
                        low += values[slot][(l - min_level[character]) as usize]
                            [min_gear[reading.bands[slot]] as usize];
                        high += values[slot][(h - min_level[character]) as usize]
                            [max_gear[reading.bands[slot]] as usize];
                    }
                    if low <= reading.target && high >= reading.target {
                        stack.push((position + 1, 0));
                    }
                    continue;
                }
                let sum = levels.iter().sum::<i32>();
                let range = Range {
                    min: total.min.max(sum + ctx.unseen_count),
                    max: total.max.min(sum + ctx.unseen_cap),
                };
                if range.min > range.max {
                    continue;
                }
                let arrays: Vec<Vec<i64>> = ctx
                    .bands
                    .iter()
                    .enumerate()
                    .map(|(band, _)| {
                        (min_gear[band]..=max_gear[band])
                            .map(|gear| {
                                (0..5)
                                    .filter(|&slot| reading.bands[slot] == band)
                                    .map(|slot| {
                                        values[slot][(levels[reading.characters[slot]]
                                            - min_level[reading.characters[slot]])
                                            as usize][gear as usize]
                                    })
                                    .sum()
                            })
                            .collect()
                    })
                    .collect();
                let mut items = vec![0; gears.len()];
                collect_items(
                    0,
                    reading.target,
                    &arrays,
                    &min_gear,
                    gears,
                    &mut items,
                    &mut |instruments| {
                        result.push(Witness {
                            challenge_id: reading.challenge,
                            levels: levels.clone(),
                            instruments: instruments.to_vec(),
                            total_rank: range,
                            total_value: range.min,
                        });
                    },
                );
            }
        }
        result
    }
}
fn collect_items(
    index: usize,
    remaining: i64,
    arrays: &[Vec<i64>],
    minimums: &[i32],
    preferred: &[i32],
    current: &mut [i32],
    emit: &mut impl FnMut(&[i32]),
) {
    let values = &arrays[index];
    if index + 1 == arrays.len() {
        let mut offset = values.partition_point(|value| *value < remaining);
        while offset < values.len() && values[offset] == remaining {
            current[index] = offset as i32 + minimums[index];
            emit(current);
            offset += 1;
        }
    } else {
        let rest_low: i64 = arrays[index + 1..].iter().map(|array| array[0]).sum();
        let rest_high: i64 = arrays[index + 1..]
            .iter()
            .map(|array| *array.last().unwrap())
            .sum();
        let mut order: Vec<_> = (0..values.len()).collect();
        order.sort_by_key(|&offset| {
            (
                (offset as i32 + minimums[index] - preferred[index]).abs(),
                offset,
            )
        });
        for offset in order {
            let next = remaining - values[offset];
            if next < rest_low || next > rest_high {
                continue;
            }
            current[index] = offset as i32 + minimums[index];
            collect_items(index + 1, next, arrays, minimums, preferred, current, emit);
        }
    }
}
