//! Support-skill 2001 (cumulative matching-judgement score) in the ordinary-live model.
use haneoka_team_engine::{
    live::{self, Cumulative, Effect, Gate, Input, Slot},
    model,
};

const PERFECT: usize = 5;
const MISS: usize = 1;
const GREAT: usize = 4;
const JUST: usize = 6;

fn f(value: f64) -> f64 {
    value as f32 as f64
}

fn slot(cumulative: Vec<Cumulative>) -> Slot {
    Slot {
        effects: Vec::new(),
        cumulative,
        extension_ms: 0.0,
        recovery: 0,
        convert: None,
    }
}

/// Every two matching judgements add 20 %, at most `cap` steps.
fn climbing(cap: i32, seconds: f64) -> Cumulative {
    Cumulative {
        judgements: vec![PERFECT, JUST],
        unit: 2,
        value: 2000,
        maximum: 12000,
        cap,
        seconds,
    }
}

fn input(slots: Vec<Slot>, judgements: Option<Vec<usize>>, notes: usize) -> Input {
    let judge_frac = [0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
    Input {
        times: (0..notes).map(|i| 100 + i as i32 * 100).collect(),
        note_frac: vec![1.0; notes],
        ap_combo: vec![1.0; notes],
        combo_table: Vec::new(),
        judge_frac,
        converted_count: 1,
        difficulty_factor: 1.0,
        adjustment: 1.0,
        skill_times: vec![0],
        life_base: 1000.0,
        life_onus: 1.0,
        damage: [0.0; 7],
        power: 1000,
        slots,
        judgements,
        assist: 1.0,
        luck_percent: 100.0,
    }
}

/// The score an order must reach when the factor only changes on the schedule.
fn expected(notes: usize, judgements: &[usize], cumulative: &Cumulative) -> f64 {
    let end = ((cumulative.seconds * 1000.0) as f32).ceil() as i32;
    let base = 1000.0;
    let (mut count, mut mill) = (0, 0);
    let mut total = 0.0;
    for (i, &judgement) in judgements.iter().enumerate().take(notes) {
        let time = 100 + i as i32 * 100;
        if time >= end {
            mill = 0;
        } else if cumulative.judgements.contains(&judgement) {
            count += 1;
            let steps = (count / cumulative.unit).min(cumulative.cap);
            let mut value = cumulative.value * steps;
            if cumulative.maximum > 0 && cumulative.maximum < value {
                value = cumulative.maximum;
            }
            mill = f(f(value as f64 / 10000.0) * 100000.0) as i32;
        }
        let up = f(1.0 + f(mill as f64 / 100000.0));
        total += f(f(1.0 * f(1.0 * base * up)) / 1.0).floor();
    }
    total
}

#[test]
fn all_perfect_matches_the_schedule() {
    let factor = climbing(6, 5.0);
    let notes = 12;
    let scores = live::score(&input(vec![slot(vec![factor.clone()])], None, notes)).unwrap();
    let perfect = vec![PERFECT; notes];
    assert_eq!(scores.scores[0], expected(notes, &perfect, &factor));
    assert!(scores.scores[0] > live::score(&input(vec![slot(Vec::new())], None, notes)).unwrap().scores[0]);
}

#[test]
fn played_perfect_orders_agree_with_all_perfect() {
    let notes = 12;
    let all = live::score(&input(vec![slot(vec![climbing(6, 5.0)])], None, notes)).unwrap();
    let played = live::score(&input(
        vec![slot(vec![climbing(6, 5.0)])],
        Some(vec![PERFECT; notes]),
        notes,
    ))
    .unwrap();
    assert_eq!(all.scores, played.scores);
}

#[test]
fn same_time_notes_agree_with_all_perfect() {
    let notes = 6;
    let mut all_input = input(vec![slot(vec![climbing(6, 5.0)])], None, notes);
    all_input.times[1] = all_input.times[0];
    let mut played_input = input(
        vec![slot(vec![climbing(6, 5.0)])],
        Some(vec![PERFECT; notes]),
        notes,
    );
    played_input.times[1] = played_input.times[0];
    let all = live::score(&all_input).unwrap();
    let played = live::score(&played_input).unwrap();
    assert_eq!(all.scores, played.scores);
}

#[test]
fn a_miss_does_not_advance_the_counter() {
    let notes = 8;
    let factor = climbing(6, 5.0);
    let mut judgements = vec![PERFECT; notes];
    judgements[1] = MISS;
    judgements[2] = GREAT;
    let played =
        live::score(&input(vec![slot(vec![factor.clone()])], Some(judgements), notes)).unwrap();
    let perfect = live::score(&input(vec![slot(vec![factor])], Some(vec![PERFECT; notes]), notes))
        .unwrap();
    assert!(played.scores[0] < perfect.scores[0]);
}

#[test]
fn just_advances_the_counter() {
    let factor = climbing(6, 5.0);
    let notes = 8;
    let judgements = vec![JUST; notes];
    let scores = live::score(&input(
        vec![slot(vec![factor.clone()])],
        Some(judgements.clone()),
        notes,
    ))
    .unwrap();
    assert_eq!(scores.scores[0], expected(notes, &judgements, &factor));
}

#[test]
fn the_window_end_removes_the_factor() {
    let factor = climbing(6, 0.5);
    let notes = 12;
    let scores = live::score(&input(vec![slot(vec![factor.clone()])], None, notes)).unwrap();
    let perfect = vec![PERFECT; notes];
    assert_eq!(scores.scores[0], expected(notes, &perfect, &factor));
    assert!(scores.scores[0] < live::score(&input(vec![slot(vec![climbing(6, 5.0)])], None, notes)).unwrap().scores[0]);
}

#[test]
fn live_extension_does_not_extend_cumulative_factors() {
    let notes = 12;
    let plain = live::score(&input(vec![slot(vec![climbing(6, 0.5)])], None, notes)).unwrap();
    let extended_factor = climbing(6, 0.5);
    let mut extended_slot = slot(vec![extended_factor]);
    extended_slot.extension_ms = 1000.0;
    let extended = live::score(&input(vec![extended_slot], None, notes)).unwrap();
    assert_eq!(plain.scores, extended.scores);
}

#[test]
fn the_maximum_and_cap_saturate() {
    let factor = Cumulative {
        unit: 1,
        value: 2000,
        maximum: 4000,
        cap: 10,
        ..climbing(10, 5.0)
    };
    let notes = 12;
    let scores = live::score(&input(vec![slot(vec![factor.clone()])], None, notes)).unwrap();
    let perfect = vec![PERFECT; notes];
    assert_eq!(scores.scores[0], expected(notes, &perfect, &factor));
}

#[test]
fn an_empty_counter_changes_nothing() {
    let notes = 6;
    let base = live::score(&input(vec![slot(Vec::new())], None, notes)).unwrap();
    let factor = Cumulative {
        judgements: Vec::new(),
        ..climbing(6, 5.0)
    };
    let empty = live::score(&input(vec![slot(vec![factor])], None, notes)).unwrap();
    assert_eq!(base.scores, empty.scores);
}

#[test]
fn a_score_skill_still_stacks_on_top() {
    let notes = 6;
    let effect = Effect {
        kind: 2000,
        delta: 50000,
        seconds: 5.0,
        gate: Gate {
            kind: 0,
            value: 0.0,
            positive: true,
        },
        judgements: Vec::new(),
    };
    let mut with = slot(vec![climbing(6, 5.0)]);
    with.effects.push(effect);
    let plain = live::score(&input(vec![slot(Vec::new())], None, notes)).unwrap();
    let boosted = live::score(&input(vec![with], None, notes)).unwrap();
    assert!(boosted.scores[0] > plain.scores[0]);
}

#[test]
fn search_bounds_price_the_cumulative_cap() {
    let notes = 12;
    let input = input(vec![slot(vec![climbing(3, 5.0)])], None, notes);
    let linear = model::linear(&input);
    let upper = model::skill_weights(&linear, &input, &input.slots[0], true);
    let lower = model::skill_weights(&linear, &input, &input.slots[0], false);
    assert!(upper.per_event[0] > 0.0);
    assert_eq!(lower.per_event[0], 0.0);

    let judgements = vec![PERFECT; notes];
    let mut bounds = model::PlayBounds::new(&input, &judgements);
    let (low, high) = bounds.order(
        &input,
        &judgements,
        &input.slots,
        &[0],
        &[0, 1, 2, 3, 4],
        1000.0,
    );
    assert!(high > low);
}

#[test]
fn large_cumulative_caps_keep_search_bounds_above_replay() {
    for (unit, value, maximum, cap) in [
        (1, 3000, 6000, 999999),
        (2, 2000, 4000, i32::MAX),
        (1, 3000, 0, 999999),
    ] {
        let factor = Cumulative {
            unit,
            value,
            maximum,
            cap,
            ..climbing(cap, 5.0)
        };
        let input = input(vec![slot(vec![factor])], None, 12);
        let exact = live::score(&input).unwrap().scores[0];
        let linear = model::linear(&input);
        let upper = model::skill_weights(&linear, &input, &input.slots[0], true);
        assert!(upper.per_event[0] > 0.0);

        let judgements = vec![PERFECT; 12];
        let mut bounds = model::PlayBounds::new(&input, &judgements);
        let (_, high) = bounds.order(
            &input,
            &judgements,
            &input.slots,
            &[0],
            &[0, 1, 2, 3, 4],
            1000.0,
        );
        assert!(high >= exact, "cap {cap}: bound {high} < replay {exact}");
    }
}
