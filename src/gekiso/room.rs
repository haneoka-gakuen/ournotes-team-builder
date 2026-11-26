//! Joint replay of two to five teams, each on its own chart and judgement stream.
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
    /// The latest settlement frame among the seats in this section.
    pub time_ms: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seat_times_ms: Option<Vec<i32>>,
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
    replay_mixed(master, &vec![chart; seats.len()], seats)
}

struct PlayingSeat<'m, 'c> {
    model: super::Model<'m>,
    chart: &'c Chart,
    next_frame: usize,
}

impl PlayingSeat<'_, '_> {
    /// Stop on the native settlement frame. Its award must be supplied before the next frame.
    fn until_rank(&mut self, generation: u64) -> Result<Option<(usize, i32, Standing)>, String> {
        while let Some(frame) = self.chart.play.frames.get(self.next_frame) {
            if generation != crate::cancellation_generation() {
                return Err("cancelled".into());
            }
            let dt = self.chart.play.dt[self.next_frame];
            self.next_frame += 1;
            if let Some((index, standing)) =
                self.model
                    .frame_to_section_rank(frame.time_ms, &frame.judged, dt)?
            {
                return Ok(Some((index, frame.time_ms, standing)));
            }
        }
        Ok(None)
    }
}

pub fn replay_mixed(
    master: &Master,
    charts: &[&Chart],
    seats: &[Seat<'_>],
) -> Result<Replay, String> {
    if !(2..=5).contains(&seats.len()) || charts.len() != seats.len() {
        return Err("a cooperative room requires two to five seats".into());
    }
    let missions = &charts[0].setup.missions;
    if missions.len() != 3
        || charts
            .iter()
            .any(|chart| chart.setup.fevers.len() != 3 || chart.setup.missions != *missions)
    {
        return Err("a room replay requires the same three Gekiso missions on every chart".into());
    }
    let mut players = seats
        .iter()
        .zip(charts)
        .map(|(seat, &chart)| {
            Ok(PlayingSeat {
                model: seat.start(master, chart)?,
                chart,
                next_frame: 0,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut sections = Vec::with_capacity(3);
    let generation = crate::cancellation_generation();
    for (index, &mission) in missions.iter().enumerate() {
        let mut standings = Vec::with_capacity(seats.len());
        let mut times = Vec::with_capacity(seats.len());
        for (s, player) in players.iter_mut().enumerate() {
            let Some((range, time, own)) = player.until_rank(generation)? else {
                return Err(format!("seat {} ended before section {}", s + 1, index + 1));
            };
            if range != index {
                return Err(format!("inconsistent room section at {time} ms"));
            }
            standings.push(own);
            times.push(time);
        }
        let settled = players
            .iter_mut()
            .zip(&standings)
            .map(|(player, own)| {
                let rank = 1 + standings.iter().filter(|other| other.beats(own)).count() as i32;
                SectionSeat::finish(&mut player.model, index, *own, rank)
            })
            .collect();
        let time_ms = *times.iter().max().unwrap();
        sections.push(Section {
            index,
            mission,
            time_ms,
            seat_times_ms: times.iter().any(|&time| time != time_ms).then_some(times),
            seats: settled,
        });
    }
    let mut scores = Vec::with_capacity(seats.len());
    for mut player in players {
        if player.until_rank(generation)?.is_some() {
            return Err("room replay produced an extra section".into());
        }
        scores.push(player.model.score.score.score);
    }
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
