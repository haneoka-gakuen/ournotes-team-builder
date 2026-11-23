//! Conservative LIFE bounds for complete member rosters in damaging plays.
use super::{Chart, Master};

#[derive(Clone, Copy)]
pub(super) struct Reduction {
    pub(super) from: i32,
    pub(super) through: i32,
    pub(super) points: i32,
}

pub(super) struct Rules {
    pub(super) member_healing: Vec<i64>,
    pub(super) snap_healing: Vec<i64>,
    pub(super) prevented_damage: i64,
    pub(super) reductions: Vec<Vec<Reduction>>,
}

struct Chunk {
    last_note: i32,
    counts: [u32; 7],
    events: usize,
}

pub(super) struct HealthBound {
    initial: i64,
    prevented_damage: i64,
    damage: [i64; 7],
    onus: f64,
    chunks: Vec<Chunk>,
    reductions: Vec<Vec<i32>>,
    score_weights: Vec<(i32, f64)>,
    total: f64,
    member_healing: Vec<i64>,
    snap_healing: Vec<i64>,
    max_snap_healing: i64,
    once_per_member: bool,
}

impl HealthBound {
    pub(super) fn new(master: &Master, chart: &Chart, rules: Rules, mut score_weights: Vec<(i32, f64)>) -> Option<Self> {
        if !(1.0..=1_000_000.0).contains(&master.live.life_base)
            || !(0.0..=1.0).contains(&master.live.life_onus)
            || rules.prevented_damage == i64::MAX
            || master.live.damage.iter().any(|&value| !(0.0..=100_000.0).contains(&value) || value.fract() != 0.0)
        { return None; }
        let damage = master.live.damage.map(|value| value as i64);
        let mut edges: Vec<_> = chart.full.events.iter().map(|&(_, at)| at)
            .chain(rules.reductions.iter().flatten().flat_map(|row| [row.from, row.through.saturating_add(1)]))
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
            chunks.push(Chunk { last_note, counts, events });
            openings.push(index.checked_sub(1).map_or(i32::MIN, |index| edges[index]));
        }
        let reductions = rules.reductions.into_iter().map(|rows| {
            openings.iter().map(|&start| rows.iter()
                .filter(|row| row.from <= start && start <= row.through)
                .fold(0i32, |sum, row| sum.saturating_add(row.points).min(10_000))).collect()
        }).collect();
        let mut total = 0.0_f64;
        for (_, weight) in &mut score_weights {
            total = (total + *weight).next_up();
            *weight = total;
        }
        let events = &chart.full.events;
        let once_per_member = events.len() <= 5 && events.iter().enumerate().all(|(index, &(activation, _))| {
            activation < 5 && events[..index].iter().all(|&(other, _)| other != activation)
        });
        let max_snap_healing = rules.snap_healing.iter().copied().max().unwrap_or(0);
        Some(Self { initial: master.live.life_base as i64, prevented_damage: rules.prevented_damage,
            damage, onus: master.live.life_onus, chunks, reductions, score_weights, total,
            member_healing: rules.member_healing, snap_healing: rules.snap_healing,
            max_snap_healing, once_per_member })
    }

    pub(super) fn roster(&self, members: &[usize]) -> f64 {
        self.coefficient(members, [self.max_snap_healing; 5])
    }

    pub(super) fn snap_healing(&self, snap: i32) -> i64 {
        if snap < 0 { 0 } else { self.snap_healing[snap as usize] }
    }

    pub(super) fn coefficient(&self, members: &[usize], snaps: [i64; 5]) -> f64 {
        let mut healing: [i64; 5] = std::array::from_fn(|slot| {
            self.member_healing[members[slot]].saturating_add(snaps[slot])
        });
        healing.sort_unstable_by(|a, b| b.cmp(a));
        let max = healing[0];
        let mut prefix = [0i64; 6];
        for (slot, &gain) in healing.iter().enumerate() {
            prefix[slot + 1] = prefix[slot].saturating_add(gain);
        }
        let mut loss = 0i64;
        for (index, chunk) in self.chunks.iter().enumerate() {
            let reduction = members.iter()
                .fold(0i32, |sum, &member| (sum + self.reductions[member][index]).min(10_000));
            for (count, damage) in chunk.counts.iter().zip(self.damage) {
                let applied = (10_000 - reduction as i64) * damage / 10_000;
                loss = loss.saturating_add(i64::from(*count) * applied);
            }
            let healed = if self.once_per_member { prefix[chunk.events] }
                else { (chunk.events as i64).saturating_mul(max) };
            if loss.saturating_sub(self.prevented_damage) >= self.initial.saturating_add(healed) {
                let alive = self.score_weights.partition_point(|&(time, _)| time <= chunk.last_note);
                let prefix = alive.checked_sub(1).map_or(0.0, |index| self.score_weights[index].1);
                return ((self.total * self.onus).next_up() + (prefix * (1.0 - self.onus).next_up()).next_up()).next_up();
            }
        }
        f64::INFINITY
    }
}
