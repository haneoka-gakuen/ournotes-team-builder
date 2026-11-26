//! Joint replay of two to five prepared teams on the same chart and judgement stream.
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

impl Seat<'_> {
    fn start<'a>(self, master: &'a Master, chart: &Chart) -> Result<super::Model<'a>, String> {
        if self.deck.len() != 5 {
            return Err("each room seat requires five members".into());
        }
        order_model(
            master,
            chart,
            self.deck,
            self.power,
            self.order_index,
            self.seed,
        )
    }
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
    pub seats: Vec<SectionSeat>,
}

#[derive(Debug, Serialize)]
pub struct Replay {
    pub scores: Vec<i32>,
    pub total: i64,
    pub sections: Vec<Section>,
}

pub fn replay<'a>(
    master: &Master,
    chart: &Chart,
    seats: impl AsRef<[Seat<'a>]>,
) -> Result<Replay, String> {
    let seats = seats.as_ref();
    if !(2..=5).contains(&seats.len()) {
        return Err("a cooperative room requires two to five seats".into());
    }
    if chart.setup.fevers.len() != 3 || chart.setup.missions.len() != 3 {
        return Err("a room replay requires three Gekiso sections".into());
    }
    let mut models = seats
        .iter()
        .map(|seat| seat.start(master, chart))
        .collect::<Result<Vec<_>, _>>()?;
    let mut standings = Vec::with_capacity(seats.len());
    let mut sections = Vec::with_capacity(3);
    let generation = crate::cancellation_generation();
    for (frame, &dt) in chart.play.frames.iter().zip(&chart.play.dt) {
        if generation != crate::cancellation_generation() {
            return Err("cancelled".into());
        }
        let index = sections.len();
        for model in &mut models {
            if let Some((range, own)) =
                model.frame_to_section_rank(frame.time_ms, &frame.judged, dt)?
            {
                if range != index || range >= chart.setup.missions.len() {
                    return Err(format!("inconsistent room section at {} ms", frame.time_ms));
                }
                standings.push(own);
            }
        }
        if standings.is_empty() {
            continue;
        }
        if standings.len() != seats.len() {
            return Err(format!("inconsistent room section at {} ms", frame.time_ms));
        }
        let settled = models
            .iter_mut()
            .zip(&standings)
            .map(|(model, own)| {
                let rank = 1 + standings.iter().filter(|other| other.beats(own)).count() as i32;
                SectionSeat::finish(model, index, *own, rank)
            })
            .collect();
        sections.push(Section {
            index,
            mission: chart.setup.missions[index],
            time_ms: frame.time_ms,
            seats: settled,
        });
        standings.clear();
    }
    if sections.len() != 3 {
        return Err(format!(
            "room replay settled {} of 3 sections",
            sections.len()
        ));
    }
    let scores: Vec<_> = models
        .iter()
        .map(|player| player.score.score.score)
        .collect();
    let total = scores.iter().map(|&score| i64::from(score)).sum();
    Ok(Replay {
        scores,
        total,
        sections,
    })
}

/// Replay one of a candidate's possible rank histories, including its effect on later skills.
pub(crate) fn score_for_ranks(
    master: &Master,
    chart: &Chart,
    seat: Seat<'_>,
    ranks: [i32; 3],
) -> Result<i32, String> {
    fixed_replay(
        master,
        chart,
        seat,
        ranks,
        crate::cancellation_generation(),
        |_| {},
    )
}

pub(super) fn fixed_replay(
    master: &Master,
    chart: &Chart,
    seat: Seat<'_>,
    ranks: [i32; 3],
    generation: u64,
    mut record: impl FnMut(SectionSeat),
) -> Result<i32, String> {
    let mut model = seat.start(master, chart)?;
    let mut settled = 0;
    for (frame, &dt) in chart.play.frames.iter().zip(&chart.play.dt) {
        if generation != crate::cancellation_generation() {
            return Err("cancelled".into());
        }
        if let Some((index, standing)) =
            model.frame_to_section_rank(frame.time_ms, &frame.judged, dt)?
        {
            if index != settled || index >= ranks.len() {
                return Err("unexpected fixed-placement section".into());
            }
            record(SectionSeat::finish(
                &mut model,
                index,
                standing,
                ranks[index],
            ));
            settled += 1;
        }
    }
    if settled != ranks.len() {
        return Err("incomplete fixed-placement replay".into());
    }
    Ok(model.score.score.score)
}
