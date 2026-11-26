//! Allocate a five-player SS target in proportion to five fixed-placement replays of one team.
pub(crate) mod neutral;
use super::{
    room::{fixed_replay, Seat, SectionSeat},
    Chart,
};
use crate::{
    chart::battle_room_total,
    full::gekisou::mission_pattern,
    master::{Master, Song},
};
use serde::Serialize;

/// One row per player and one column per section. Ties use competition ranking (1, 1, 3).
pub type RankSchedule = [[i32; 3]; 5];
pub const FIXED_RANKS: RankSchedule = [[1; 3], [2; 3], [3; 3], [4; 3], [5; 3]];

pub fn ss_room_threshold(master: &Master, song: &Song, players: usize) -> Result<i64, String> {
    if !(1..=5).contains(&players) {
        return Err("a Gekiso room requires one to five players".into());
    }
    let ss = master
        .score_ranks
        .get(&song.rank_group)
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.rank == 7 && row.battle_required > 0)
        })
        .ok_or_else(|| format!("missing battle SS grade for song {}", song.id))?;
    Ok(battle_room_total(ss.battle_required, players as u64))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeatReference {
    pub ranks: [i32; 3],
    pub sections: Vec<SectionSeat>,
    pub simulated_score: i32,
    /// Actual simulated score after removing only the three placement rewards.
    pub base_score: i64,
    /// Integer share of the room SS line, proportional to the full simulated score.
    pub target_score: i64,
    /// Base score scaled by the same room-wide ratio, rounded to nearest.
    pub target_base_score: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reference {
    pub room_threshold: i64,
    pub simulated_room_score: i64,
    /// Present if rank feedback left all five base scores equal.
    pub common_target_base_score: Option<i64>,
    pub seats: Vec<SeatReference>,
}

pub(crate) fn require_bonus(
    master: &Master,
    pattern: i32,
    section: usize,
    rank: i32,
) -> Result<(), String> {
    if master
        .live
        .gekiso
        .ranks
        .iter()
        .any(|&[p, s, r, _]| p == pattern && s == (section + 1) as i32 && r == rank)
    {
        Ok(())
    } else {
        Err(format!(
            "missing placement bonus for section {} rank {rank}",
            section + 1
        ))
    }
}

fn validate_placements(
    master: &Master,
    missions: &[i32],
    ranks: &RankSchedule,
) -> Result<(), String> {
    let &[a, b, c] = missions else {
        return Err("SS reference requires three missions".into());
    };
    if !missions.iter().all(|&m| (1..=3).contains(&m)) {
        return Err("SS reference requires valid section missions".into());
    }
    let pattern = mission_pattern(a, b, c);
    for section in 0..3 {
        let mut ranked = ranks.map(|seat| seat[section]);
        ranked.sort();
        for (i, &rank) in ranked.iter().enumerate() {
            if i > 0 && rank == ranked[i - 1] {
                continue;
            }
            if rank != (i + 1) as i32 {
                return Err(format!(
                    "invalid five-player placements in section {}",
                    section + 1
                ));
            }
            require_bonus(master, pattern, section, rank)?;
        }
    }
    Ok(())
}

fn replay_seat(
    master: &Master,
    chart: &Chart,
    seat: Seat<'_>,
    placements: [i32; 3],
    generation: u64,
) -> Result<SeatReference, String> {
    let mut sections = Vec::with_capacity(3);
    let simulated_score = fixed_replay(master, chart, seat, placements, generation, |s| {
        sections.push(s)
    })?;
    let base_score =
        i64::from(simulated_score) - sections.iter().map(|s| i64::from(s.bonus)).sum::<i64>();
    if simulated_score < 0 || base_score < 0 {
        return Err("negative SS reference score".into());
    }
    Ok(SeatReference {
        ranks: placements,
        sections,
        simulated_score,
        base_score,
        target_score: 0,
        target_base_score: 0,
    })
}

/// The player profile, skill order and seed stay fixed across the five replays.
/// Placements are supplied scenarios; their target shares do not guarantee a team will obtain them.
pub fn reference(
    master: &Master,
    chart: &Chart,
    song: &Song,
    seat: Seat<'_>,
    ranks: RankSchedule,
) -> Result<Reference, String> {
    if seat.deck.len() != 5 || seat.power <= 0 {
        return Err("SS reference requires five performers and positive power".into());
    }
    if chart.setup.fevers.len() != 3 || chart.setup.missions != song.missions {
        return Err("SS reference requires three sections matching the selected song".into());
    }
    validate_placements(master, &song.missions, &ranks)?;
    let room_threshold = ss_room_threshold(master, song, 5)?;
    let mut seats = Vec::with_capacity(5);
    let generation = crate::cancellation_generation();
    for placements in ranks {
        seats.push(replay_seat(master, chart, seat, placements, generation)?);
    }
    let simulated_room_score = seats
        .iter()
        .map(|s| i64::from(s.simulated_score))
        .sum::<i64>();
    if simulated_room_score == 0 {
        return Err("SS reference has zero simulated room score".into());
    }
    let total = i128::from(simulated_room_score);
    let threshold = i128::from(room_threshold);
    let mut remainders = Vec::with_capacity(5);
    let mut remaining = room_threshold;
    for (i, seat) in seats.iter_mut().enumerate() {
        let product = threshold * i128::from(seat.simulated_score);
        seat.target_score = (product / total) as i64;
        seat.target_base_score =
            ((threshold * i128::from(seat.base_score) + total / 2) / total) as i64;
        remaining -= seat.target_score;
        remainders.push((std::cmp::Reverse(product % total), i));
    }
    // Largest fractional part first, with equal fractions resolved in input seat order.
    remainders.sort_unstable();
    for (_, i) in remainders.into_iter().take(remaining as usize) {
        seats[i].target_score += 1;
    }
    let common_target_base_score = seats
        .iter()
        .all(|s| s.base_score == seats[0].base_score)
        .then_some(seats[0].target_base_score);
    Ok(Reference {
        room_threshold,
        simulated_room_score,
        common_target_base_score,
        seats,
    })
}
