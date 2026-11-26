//! Joint replay of two prepared teams on the same chart and judgement stream.
use super::{order_model, sections::Standing, Chart};
use crate::{full::conditions::Performer, master::Master};
use serde::Serialize;

#[derive(Clone, Copy)]
pub struct Seat<'a> {
    pub deck: &'a [Performer],
    pub power: i32,
    /// Index in the engine's 120 ordinary skill orders.
    pub order_index: usize,
    pub seed: i32,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SectionSeat {
    pub metric: i32,
    pub score: i32,
    pub perfect: i32,
    pub rank: i32,
    pub percent: i32,
    pub bonus: i32,
}

impl SectionSeat {
    pub(super) fn finish(
        model: &mut super::Model<'_>,
        index: usize,
        own: Standing,
        rank: i32,
    ) -> Self {
        model.finish_section_frame(index, rank);
        let bonus = &model.gk.as_ref().unwrap().rank_bonus[index];
        Self {
            metric: own.metric,
            score: own.score,
            perfect: own.perfect,
            rank,
            percent: bonus.percent,
            bonus: bonus.bonus,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub index: usize,
    pub mission: i32,
    pub time_ms: i32,
    pub seats: [SectionSeat; 2],
}

#[derive(Debug, Serialize)]
pub struct Replay {
    pub scores: [i32; 2],
    pub total: i64,
    pub sections: Vec<Section>,
}

pub fn replay(master: &Master, chart: &Chart, seats: [Seat<'_>; 2]) -> Result<Replay, String> {
    if chart.setup.fevers.len() != 3 || chart.setup.missions.len() != 3 {
        return Err("a room replay requires three Gekiso sections".into());
    }
    let start = |seat: Seat<'_>| {
        if seat.deck.len() != 5 {
            return Err("each room seat requires five members".into());
        }
        order_model(
            master,
            chart,
            seat.deck,
            seat.power,
            seat.order_index,
            seat.seed,
        )
    };
    let [mut left, mut right] = [start(seats[0])?, start(seats[1])?];
    let mut sections = Vec::with_capacity(3);
    let generation = crate::cancellation_generation();
    for (k, frame) in chart.play.frames.iter().enumerate() {
        if generation != crate::cancellation_generation() {
            return Err("cancelled".into());
        }
        let dt = chart.play.dt[k];
        let a = left.frame_to_section_rank(frame.time_ms, &frame.judged, dt)?;
        let b = right.frame_to_section_rank(frame.time_ms, &frame.judged, dt)?;
        let (index, standings) = match (a, b) {
            (None, None) => continue,
            (Some((i, a)), Some((j, b))) if i == j && i == sections.len() => (i, [a, b]),
            _ => return Err(format!("inconsistent room section at {} ms", frame.time_ms)),
        };
        let [a, b] = standings;
        sections.push(Section {
            index,
            mission: chart.setup.missions[index],
            time_ms: frame.time_ms,
            seats: [
                SectionSeat::finish(&mut left, index, a, 1 + i32::from(b.beats(&a))),
                SectionSeat::finish(&mut right, index, b, 1 + i32::from(a.beats(&b))),
            ],
        });
    }
    if sections.len() != 3 {
        return Err(format!(
            "room replay settled {} of 3 sections",
            sections.len()
        ));
    }
    let scores = [left.score.score.score, right.score.score.score];
    Ok(Replay {
        scores,
        total: scores.iter().map(|&score| i64::from(score)).sum(),
        sections,
    })
}
