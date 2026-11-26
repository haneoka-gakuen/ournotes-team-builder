use haneoka_team_engine::{
    api::room_search::Candidate,
    full::model::{Model, Params},
    gekiso::{
        room::{Replay, Section, SectionSeat},
        Chart,
    },
    live,
    master::Master,
};

struct History {
    score: i32,
    sections: Vec<(i32, SectionSeat)>,
}

pub struct Histories(Vec<History>);

impl Histories {
    pub fn first_place_score(&self) -> i32 {
        self.0[0].score
    }

    /// The single-seat simulator receives every possible rank history in advance.
    pub fn build(
        master: &Master,
        chart: &Chart,
        team: &Candidate,
        order: usize,
        seed: i32,
    ) -> Result<Self, String> {
        let order = live::orders().get(order).ok_or("invalid ordinary order")?;
        let events: Vec<_> = chart
            .full
            .events
            .iter()
            .map(|&(slot, time)| {
                order
                    .get(slot)
                    .copied()
                    .map(|member| (member, time))
                    .ok_or("invalid skill slot")
            })
            .collect::<Result<_, _>>()?;
        let mut histories = Vec::new();
        for mask in 0..8 {
            let mut setup = chart.setup.clone();
            setup.ranks = (0..3).map(|k| 1 + ((mask >> k) & 1)).collect();
            let params = Params {
                music_type: chart.music_type,
                power: team.power,
                level: chart.full.level,
                converted: chart.full.converted,
                music_length: chart.music_length,
                assist: 1.0,
            };
            let mut model = Model::new(
                master,
                &team.deck,
                &chart.full.notes,
                &events,
                params,
                Some(&setup),
                (seed, false),
            )?;
            let mut sections = Vec::new();
            for (frame, &dt) in chart.play.frames.iter().zip(&chart.play.dt) {
                model.frame(frame.time_ms, &frame.judged, dt)?;
                let g = model.gk.as_ref().unwrap();
                if let Some(bonus) = g.rank_bonus.get(sections.len()) {
                    let index = bonus.range;
                    let state = &g.controller.states[index];
                    let metric = match setup.missions[index] {
                        1 => state.max_combo,
                        2 => state.luck.total_bonus,
                        3 => state.just,
                        _ => return Err("unrecognized section mission".into()),
                    };
                    sections.push((
                        frame.time_ms,
                        SectionSeat {
                            metric,
                            score: state.score(),
                            perfect: state.perfect,
                            rank: bonus.rank,
                            percent: bonus.percent,
                            bonus: bonus.bonus,
                        },
                    ));
                }
            }
            if sections.len() != 3 {
                return Err("incomplete single-seat history".into());
            }
            histories.push(History {
                score: model.score.score.score,
                sections,
            });
        }
        Ok(Self(histories))
    }
}

fn standing(s: &SectionSeat) -> (i32, i32, i32) {
    (s.metric, s.score, s.perfect)
}

/// Select the unique pair of histories consistent with all three observed rankings.
pub fn reconcile(a: &Histories, b: &Histories, missions: &[i32]) -> Result<Replay, String> {
    let mut result = None;
    for left in &a.0 {
        for right in &b.0 {
            let consistent = left
                .sections
                .iter()
                .zip(&right.sections)
                .all(|((t, x), (u, y))| {
                    let ranks = match standing(x).cmp(&standing(y)) {
                        std::cmp::Ordering::Greater => [1, 2],
                        std::cmp::Ordering::Less => [2, 1],
                        std::cmp::Ordering::Equal => [1, 1],
                    };
                    t == u && [x.rank, y.rank] == ranks
                });
            if !consistent {
                continue;
            }
            if result.is_some() {
                return Err("ambiguous rank histories".into());
            }
            let sections = left
                .sections
                .iter()
                .zip(&right.sections)
                .enumerate()
                .map(|(index, ((t, x), (_, y)))| {
                    Section {
                        index,
                        mission: missions[index],
                        time_ms: *t,
                        seat_times_ms: None,
                        seats: vec![*x, *y],
                    }
                })
                .collect();
            result = Some(Replay {
                scores: vec![left.score, right.score],
                total: i64::from(left.score) + i64::from(right.score),
                sections,
            });
        }
    }
    result.ok_or_else(|| "no consistent rank histories".into())
}
