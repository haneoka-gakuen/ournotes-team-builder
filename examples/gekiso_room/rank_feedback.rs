//! Isolate rank-triggered scoring on the supplied chart with one synthetic support skill.
use haneoka_team_engine::{
    full::{conditions::Performer, effects::ONE_SHOT},
    gekiso::{room, Chart},
    master::{Condition, Master, SkillEffect},
};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn check(master: &Master, chart: &Chart) -> Result<Value, String> {
    const TEST_ID: i32 = -7012;
    let mut master = master.clone();
    Arc::make_mut(&mut master.conditions).insert(
        TEST_ID,
        Condition {
            id: TEST_ID,
            kind: 7012,
            targets: vec![],
            values: vec![1.0],
            positive: true,
        },
    );
    Arc::make_mut(&mut master.condition_sets).insert(TEST_ID, vec![vec![TEST_ID]]);
    Arc::make_mut(&mut master.support_skills).insert(
        TEST_ID,
        vec![SkillEffect {
            id: TEST_ID,
            level: 1,
            trigger_type: ONE_SHOT,
            execute_limit: 1,
            reset_group: 0,
            kind: 2000,
            value: 10000,
            seconds: f64::from(chart.music_length) / 1000.0,
            condition_group: 0,
            trigger_group: TEST_ID,
            release_group: 0,
            cumulative_id: 0,
            targets: vec![],
            limit_count: 0,
            max_value: 0,
        }],
    );
    let plain = vec![Performer::default(); 5];
    let mut probe = plain.clone();
    probe[0].support_skills.push((TEST_ID, 1));
    let run = |deck, opponent_power| {
        room::replay(
            &master,
            chart,
            [
                room::Seat {
                    deck,
                    power: 100_000,
                    order_index: 0,
                    seed: 0,
                },
                room::Seat {
                    deck: &plain,
                    power: opponent_power,
                    order_index: 0,
                    seed: 0,
                },
            ],
        )
    };
    let skill_first = run(&probe, 50_000)?;
    let skill_second = run(&probe, 300_000)?;
    let plain_first = run(&plain, 50_000)?;
    let plain_second = run(&plain, 300_000)?;
    let first = |r: &room::Replay| r.sections[0].seats[0];
    let later = |r: &room::Replay| r.sections[1].seats[0].score;
    let base = |r: &room::Replay| {
        r.sections.iter().fold(r.scores[0], |score, section| {
            score.wrapping_sub(section.seats[0].bonus)
        })
    };

    if first(&skill_first).rank != 1
        || first(&skill_second).rank != 2
        || first(&plain_first).rank != 1
        || first(&plain_second).rank != 2
        || first(&skill_first).score != first(&skill_second).score
        || first(&skill_first).score != first(&plain_first).score
        || first(&skill_second).score != first(&plain_second).score
    {
        return Err(
            "rank feedback control did not isolate a first-section placement change".into(),
        );
    }
    if later(&skill_first) <= later(&plain_first)
        || later(&skill_second) != later(&plain_second)
        || base(&skill_first) <= base(&plain_first)
        || base(&skill_second) != base(&plain_second)
    {
        return Err("first place must activate the synthetic skill for subsequent notes".into());
    }
    let result = |r: &room::Replay| {
        json!({
            "firstSectionRank": first(r).rank, "firstSectionScore": first(r).score,
            "secondSectionScore": later(r), "personalScore": r.scores[0],
            "scoreWithoutPlacementBonuses": base(r),
        })
    };
    Ok(json!({
        "synthetic": true,
        "scenario": "one synthetic support skill: condition 7012 (first place), effect 2000 (+100% note score); identical base power, seed and order",
        "opponentPowers": [50000, 300000],
        "withSkill": [result(&skill_first), result(&skill_second)],
        "withoutSkill": [result(&plain_first), result(&plain_second)],
    }))
}
