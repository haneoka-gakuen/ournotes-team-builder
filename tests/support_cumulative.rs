//! Support-skill 2001 (cumulative matching-judgement score) in the ordinary-live model.
use haneoka_team_engine::{
    live::{self, Conversion, Cumulative, Effect, Gate, Input, Slot},
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
fn cumulative_changes_wait_for_the_judgement_frame() {
    for times in [vec![101, 201, 301], vec![101, 105, 120]] {
        let mut all = input(vec![slot(vec![climbing(6, 5.0)])], None, 3);
        all.times = times;
        let scores = live::score(&all).unwrap();
        assert_eq!(scores.scores[0], 3200.0);
        all.judgements = Some(vec![PERFECT; 3]);
        assert_eq!(scores.scores, live::score(&all).unwrap().scores);
    }
}

#[test]
fn activation_frame_uses_its_complete_judgement_batch() {
    let mut all = input(vec![slot(vec![climbing(6, 5.0)])], None, 3);
    all.skill_times = vec![1];
    all.times = vec![2, 10, 120];
    let scores = live::score(&all).unwrap();
    assert_eq!(scores.scores[0], 3200.0);
    all.judgements = Some(vec![PERFECT; 3]);
    assert_eq!(scores.scores, live::score(&all).unwrap().scores);
}

#[test]
fn converted_judgements_share_the_cumulative_delivery_frame() {
    let mut skill = slot(vec![climbing(6, 5.0)]);
    skill.convert = Some(Conversion {
        judgements: vec![GREAT],
        limit: Some(2.0),
        judgement: PERFECT,
        seconds: None,
        at_start: false,
    });
    let mut played = input(vec![skill], Some(vec![GREAT, GREAT, PERFECT]), 3);
    played.times = vec![101, 105, 120];
    assert_eq!(live::score(&played).unwrap().scores[0], 3200.0);
}

#[test]
fn cumulative_expiry_does_not_count_a_later_delivery_frame() {
    let mut all = input(vec![slot(vec![climbing(6, 0.499)])], None, 2);
    all.times = vec![101, 498];
    let scores = live::score(&all).unwrap();
    assert_eq!(scores.scores[0], 2000.0);
    all.judgements = Some(vec![PERFECT; 2]);
    assert_eq!(scores.scores, live::score(&all).unwrap().scores);
}

#[test]
fn search_bounds_include_the_delayed_support_window() {
    let factor = Cumulative {
        unit: 1,
        cap: 1,
        maximum: 2000,
        ..climbing(1, 0.1)
    };
    let mut all = input(vec![slot(vec![factor])], None, 2);
    all.skill_times = vec![1];
    all.times = vec![20, 111];
    all.note_frac = vec![0.0, 1.0];
    let exact = live::score(&all).unwrap().scores[0];
    assert_eq!(exact, 1200.0);
    let upper = model::skill_weights(&model::linear(&all), &all, &all.slots[0], true);
    assert!(1000.0 + 1000.0 * upper.per_event[0] >= exact);
    let mut bounds = model::PlayBounds::new(&all, &[PERFECT; 2]);
    let (_, high) = bounds.order(
        &all,
        &[PERFECT; 2],
        &all.slots,
        &[0],
        &[0, 1, 2, 3, 4],
        1000.0,
    );
    assert!(high >= exact);
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
        release_judgements: Vec::new(),
    };
    let mut with = slot(vec![climbing(6, 5.0)]);
    with.effects.push(effect);
    let plain = live::score(&input(vec![slot(Vec::new())], None, notes)).unwrap();
    let boosted = live::score(&input(vec![with], None, notes)).unwrap();
    assert!(boosted.scores[0] > plain.scores[0]);
}

fn combo_guard() -> Conversion {
    Conversion {
        judgements: vec![MISS, 2],
        limit: Some(12.0),
        judgement: 3,
        seconds: Some(99999.0),
        at_start: true,
    }
}

#[test]
fn static_combo_guard_preserves_combo_and_life_before_the_live_skill() {
    let mut skill = slot(Vec::new());
    skill.convert = Some(combo_guard());
    let mut played = input(vec![skill], Some(vec![PERFECT, MISS, PERFECT, PERFECT]), 4);
    played.skill_times = vec![1000];
    played.judge_frac[MISS] = 0.0;
    played.judge_frac[3] = 0.5;
    played.damage[MISS] = 1000.0;
    played.life_onus = 0.5;
    played.combo_table = vec![(2, 0.5)];
    assert_eq!(live::score(&played).unwrap().scores[0], 4500.0);
    played.slots[0].convert = None;
    assert_eq!(live::score(&played).unwrap().scores[0], 2000.0);
}

#[test]
fn static_conversion_limit_is_not_reset_by_a_live_skill_event() {
    let mut skill = slot(Vec::new());
    skill.convert = Some(Conversion { limit: Some(2.0), ..combo_guard() });
    let mut played = input(vec![skill], Some(vec![MISS; 4]), 4);
    played.skill_times = vec![250];
    played.judge_frac[MISS] = 0.0;
    played.judge_frac[3] = 0.5;
    assert_eq!(live::score(&played).unwrap().scores[0], 1000.0);
}

#[test]
fn conversion_duration_is_independent_of_member_extension() {
    let mut skill = slot(Vec::new());
    skill.extension_ms = 5000.0;
    skill.convert = Some(Conversion { seconds: Some(0.2), at_start: false, ..combo_guard() });
    let mut played = input(vec![skill], Some(vec![MISS; 2]), 2);
    played.times = vec![100, 300];
    played.judge_frac[MISS] = 0.0;
    played.judge_frac[3] = 0.5;
    assert_eq!(live::score(&played).unwrap().scores[0], 500.0);
}

#[test]
fn low_judgement_releases_extra_score_after_conversion() {
    let mut skill = slot(Vec::new());
    skill.effects.push(Effect {
        kind: 2000, delta: 10000, seconds: 5.0,
        gate: Gate { kind: 0, value: 0.0, positive: true },
        judgements: Vec::new(), release_judgements: vec![1, 2, 3],
    });
    skill.convert = Some(combo_guard());
    let mut played = input(vec![skill], Some(vec![PERFECT, MISS, PERFECT, PERFECT]), 4);
    played.judge_frac[MISS] = 0.0;
    played.judge_frac[3] = 0.5;
    assert_eq!(live::score(&played).unwrap().scores[0], 3600.0);
}

#[test]
fn release_uses_the_last_matching_judgement_time_of_its_first_frame() {
    let mut skill = slot(Vec::new());
    skill.effects.push(Effect {
        kind: 2000, delta: 10000, seconds: 5.0,
        gate: Gate { kind: 0, value: 0.0, positive: true },
        judgements: Vec::new(), release_judgements: vec![1, 2, 3],
    });
    let mut played = input(vec![skill], Some(vec![PERFECT, 3, 3, PERFECT]), 4);
    played.times = vec![100, 201, 202, 300];
    played.judge_frac[3] = 0.5;
    assert_eq!(live::score(&played).unwrap().scores[0], 3150.0);
    let mut bounds = model::PlayBounds::new(&played, played.judgements.as_ref().unwrap());
    assert_eq!(bounds.order(&played, played.judgements.as_ref().unwrap(), &played.slots,
        &[0], &[0, 1, 2, 3, 4], 1000.0), (f64::NEG_INFINITY, f64::INFINITY));
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
fn cumulative_caps_keep_search_bounds_above_replay() {
    for (unit, value, maximum, cap) in [
        (1, 3000, 6000, 999999),
        (2, 2000, 4000, i32::MAX),
        (1, 3000, 0, 999999),
        (1, -3000, 6000, 12),
        (1, -3000, 6000, 999999),
        (2, -2000, 4000, i32::MAX),
        (1, 500_000_000, 6000, 12),
        (1, -500_000_000, 6000, 12),
        (1, 0, 0, i32::MAX),
        (1, -500_000_000, 0, 1),
        (1, 1, 6000, 13),
    ] {
        let factor = Cumulative {
            unit,
            value,
            maximum,
            cap,
            ..climbing(cap, 5.0)
        };
        let played = input(vec![slot(vec![factor.clone()])], Some(vec![PERFECT; 12]), 12);
        let input = input(vec![slot(vec![factor])], None, 12);
        let prefix = live::score(&input).unwrap().scores[0];
        let native = live::score(&played).unwrap().scores[0];
        if (maximum <= 0 && value != 0) || i64::from(value).abs() >= 500_000_000 || cap > 12 {
            assert_eq!(prefix, native);
        }
        let exact = prefix.max(native);
        let linear = model::linear(&input);
        let upper = model::skill_weights(&linear, &input, &input.slots[0], true);
        if value > 0 || value.checked_mul(cap).is_none() {
            assert!(upper.per_event[0] > 0.0);
        } else {
            assert_eq!(upper.per_event[0], 0.0);
        }

        let judgements = vec![PERFECT; 12];
        let mut bounds = model::PlayBounds::new(&input, &judgements);
        let (low, high) = bounds.order(
            &input,
            &judgements,
            &input.slots,
            &[0],
            &[0, 1, 2, 3, 4],
            1000.0,
        );
        assert!(low <= exact, "cap {cap}: lower {low} > replay {exact}");
        assert!(high >= exact, "cap {cap}: bound {high} < replay {exact}");
        if value != 0 && cap > 12 {
            assert_eq!(low, f64::NEG_INFINITY);
            assert_eq!(high, f64::INFINITY);
        }
    }

    for value in [-100_000_000, 100_000_000] {
        let mut ghost_slot = slot(vec![Cumulative {
            unit: 1,
            value,
            maximum: if value < 0 { 6000 } else { value },
            cap: 1,
            seconds: 0.15,
            ..climbing(1, 0.15)
        }]);
        ghost_slot.effects.push(Effect {
            kind: 2000,
            delta: 70_000,
            seconds: 5.0,
            gate: Gate { kind: 0, value: 0.0, positive: true },
            judgements: Vec::new(),
            release_judgements: Vec::new(),
        });
        let mut ghost = input(vec![ghost_slot], None, 2);
        ghost.power = 1_000_000;
        ghost.note_frac = vec![0.0, 1.0];
        let exact = live::score(&ghost).unwrap().scores[0];
        let judgements = vec![PERFECT; 2];
        let mut bounds = model::PlayBounds::new(&ghost, &judgements);
        let (low, high) = bounds.order(&ghost, &judgements, &ghost.slots, &[0], &[0, 1, 2, 3, 4], ghost.power as f64);
        assert!(low <= exact, "signed-factor residue: lower {low} > replay {exact}");
        assert!(high >= exact, "signed-factor residue: bound {high} < replay {exact}");
    }
}
