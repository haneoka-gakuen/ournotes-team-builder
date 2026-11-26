//! An attainable score budget for a skill-free team at two adjacent integer powers.
use super::{
    replay_seat, validate_placements, Chart, Master, RankSchedule, Reference, Seat, SeatReference,
    Song, FIXED_RANKS,
};
use crate::full::gekisou;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Crossing {
    pub power: i32,
    pub lower_power: i32,
    pub lower_room_score: i64,
    pub room_score: i64,
    pub scores: [i64; 5],
    pub base_score: i64,
    pub section_scores: [i32; 3],
    /// Each row lists the additional score for placing first through fifth in that section.
    pub section_bonuses: [[i64; 5]; 3],
}

pub(crate) enum CrossingError {
    Unavailable(&'static str),
    Replay(String),
}
impl From<String> for CrossingError {
    fn from(message: String) -> Self {
        Self::Replay(message)
    }
}

#[derive(Clone, Copy)]
struct Point {
    power: i32,
    room_score: i64,
    scores: [i64; 5],
    base_score: i64,
    section_scores: [i32; 3],
    bonuses: [[i64; 5]; 3],
}

struct Neutral<'a> {
    master: &'a Master,
    chart: &'a Chart,
    seat: Seat<'a>,
    ranks: RankSchedule,
    factors: [[i32; 5]; 3],
    generation: u64,
}
impl Neutral<'_> {
    fn single(&self, power: i32, ranks: [i32; 3]) -> Result<SeatReference, CrossingError> {
        Ok(replay_seat(
            self.master,
            self.chart,
            Seat { power, ..self.seat },
            ranks,
            self.generation,
        )?)
    }
    // With no skills, rank changes only the fixed award. Each section's own notes and lottery use the same judgement stream and seed.
    fn probe(&self, power: i32) -> Result<Point, CrossingError> {
        let row = self.single(power, self.ranks[0])?;
        let section_scores: [i32; 3] = std::array::from_fn(|i| row.sections[i].score);
        if section_scores.iter().any(|&v| v < 0) {
            return Err(CrossingError::Unavailable(
                "neutral section score exceeds native score range",
            ));
        }
        let bonuses: [[i64; 5]; 3] = std::array::from_fn(|i| {
            self.factors[i].map(|p| i64::from(section_scores[i]) * i64::from(p) / 100)
        });
        if bonuses
            .iter()
            .flatten()
            .any(|&v| !(0..=i64::from(i32::MAX)).contains(&v))
        {
            return Err(CrossingError::Unavailable(
                "neutral placement award exceeds native score range",
            ));
        }
        let scores = self.ranks.map(|ranks| {
            row.base_score
                + (0..3)
                    .map(|i| bonuses[i][ranks[i] as usize - 1])
                    .sum::<i64>()
        });
        if scores
            .iter()
            .any(|&v| !(0..=i64::from(i32::MAX)).contains(&v))
        {
            return Err(CrossingError::Unavailable(
                "neutral personal score exceeds native score range",
            ));
        }
        Ok(Point {
            power,
            base_score: row.base_score,
            section_scores,
            bonuses,
            scores,
            room_score: scores.iter().sum(),
        })
    }
    fn verify(&self, point: Point) -> Result<(), CrossingError> {
        for (i, ranks) in self.ranks.iter().enumerate() {
            let row = self.single(point.power, *ranks)?;
            if point.scores[i] != i64::from(row.simulated_score)
                || point.base_score != row.base_score
                || row.sections.iter().enumerate().any(|(j, s)| {
                    s.score != point.section_scores[j]
                        || i64::from(s.bonus) != point.bonuses[j][ranks[j] as usize - 1]
                })
            {
                return Err(CrossingError::Unavailable(
                    "neutral section projection differs from fixed-placement replay",
                ));
            }
        }
        Ok(())
    }
    fn bracket(&self, guess: i32, target: i64) -> Result<(Point, Point), CrossingError> {
        let mut previous = self.probe(guess)?;
        let up = previous.room_score < target;
        let mut step = 1;
        loop {
            let power = if up {
                previous.power.saturating_add(step)
            } else {
                previous.power.saturating_sub(step).max(0)
            };
            if power == previous.power {
                return Err(CrossingError::Unavailable(
                    "no SS crossing in the available integer power range",
                ));
            }
            let next = self.probe(power)?;
            let (lo, hi) = if up {
                (previous, next)
            } else {
                (next, previous)
            };
            if lo.room_score > hi.room_score {
                return Err(CrossingError::Unavailable(
                    "neutral room score decreases within the sampled power range",
                ));
            }
            if lo.room_score < target && hi.room_score >= target {
                return Ok((lo, hi));
            }
            previous = next;
            step = step.saturating_mul(2);
        }
    }
}

pub(crate) fn neutral_crossing(
    master: &Master,
    chart: &Chart,
    song: &Song,
    seat: Seat<'_>,
    ranks: RankSchedule,
    reference: &Reference,
) -> Result<Crossing, CrossingError> {
    // The viewer can redistribute this output by any valid rank, including ranks absent from its initial schedule.
    validate_placements(master, &song.missions, &FIXED_RANKS)?;
    let [a, b, c] = song.missions[..] else {
        return Err(CrossingError::Replay(
            "three section missions required".into(),
        ));
    };
    let factors = gekisou::ranking_factors(&master.live.gekiso, gekisou::mission_pattern(a, b, c));
    if factors.iter().flatten().any(|&p| p < 0) {
        return Err(CrossingError::Replay(
            "negative neutral placement percentage".into(),
        ));
    }
    let target = reference.room_threshold;
    if !(1..=5 * i64::from(i32::MAX)).contains(&target) {
        return Err(CrossingError::Unavailable(
            "SS threshold is outside the native five-player score range",
        ));
    }

    let neutral = Neutral {
        master,
        chart,
        seat,
        ranks,
        factors,
        generation: crate::cancellation_generation(),
    };
    let estimate = (i128::from(seat.power) * i128::from(target)
        / i128::from(reference.simulated_room_score))
    .clamp(1, i128::from(i32::MAX)) as i32;
    let (mut lo, mut hi) = neutral.bracket(estimate, target)?;
    while hi.power - lo.power > 1 {
        let mid = neutral.probe(lo.power + (hi.power - lo.power) / 2)?;
        if !(lo.room_score..=hi.room_score).contains(&mid.room_score) {
            return Err(CrossingError::Unavailable(
                "neutral room score decreases within the sampled power range",
            ));
        }
        if mid.room_score < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    neutral.verify(lo)?;
    neutral.verify(hi)?;
    Ok(Crossing {
        power: hi.power,
        lower_power: lo.power,
        lower_room_score: lo.room_score,
        room_score: hi.room_score,
        scores: hi.scores,
        base_score: hi.base_score,
        section_scores: hi.section_scores,
        section_bonuses: hi.bonuses,
    })
}
