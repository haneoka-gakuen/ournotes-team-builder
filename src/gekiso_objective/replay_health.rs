//! Conservative LIFE bounds for complete member rosters in damaging plays.
use super::{Chart, Master};

#[derive(Clone, Copy)]
pub(super) struct Reduction {
    pub(super) from: i32,
    pub(super) through: i32,
    pub(super) points: i32,
}

pub(super) struct Rules {
    pub(super) healing_per_event: i64,
    pub(super) prevented_damage: i64,
    pub(super) reductions: Vec<Vec<Reduction>>,
}

struct Chunk {
    last_note: i32,
    counts: [u32; 7],
    healing: i64,
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
        let mut raw = vec![(i32::MIN, [0u32; 7]); edges.len() + 1];
        let mut previous = i32::MIN;
        for note in chart.play.frames.iter().flat_map(|frame| &frame.judged) {
            if note.time_ms < previous { return None; }
            previous = note.time_ms;
            let grade = note.judgement as usize;
            if damage.get(grade).copied()? == 0 { continue; }
            let (last, counts) = &mut raw[edges.partition_point(|&time| time <= note.time_ms)];
            *last = note.time_ms;
            counts[grade] = counts[grade].checked_add(1)?;
        }
        let mut chunks = Vec::new();
        let mut openings = Vec::new();
        for (index, (last_note, counts)) in raw.into_iter().enumerate() {
            if last_note == i32::MIN { continue; }
            let live_events = chart.full.events.iter().filter(|event| event.1 <= last_note).count() as i64;
            chunks.push(Chunk { last_note, counts, healing: live_events.saturating_mul(rules.healing_per_event) });
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
        Some(Self { initial: master.live.life_base as i64, prevented_damage: rules.prevented_damage,
            damage, onus: master.live.life_onus, chunks, reductions, score_weights, total })
    }

    pub(super) fn coefficient(&self, members: &[usize]) -> f64 {
        let mut loss = 0i64;
        for (index, chunk) in self.chunks.iter().enumerate() {
            let reduction = members.iter()
                .fold(0i32, |sum, &member| (sum + self.reductions[member][index]).min(10_000));
            for (count, damage) in chunk.counts.iter().zip(self.damage) {
                let applied = (10_000 - reduction as i64) * damage / 10_000;
                loss = loss.saturating_add(i64::from(*count) * applied);
            }
            if loss.saturating_sub(self.prevented_damage) >= self.initial.saturating_add(chunk.healing) {
                let alive = self.score_weights.partition_point(|&(time, _)| time <= chunk.last_note);
                let prefix = alive.checked_sub(1).map_or(0.0, |index| self.score_weights[index].1);
                return ((self.total * self.onus).next_up() + (prefix * (1.0 - self.onus).next_up()).next_up()).next_up();
            }
        }
        f64::INFINITY
    }
}
