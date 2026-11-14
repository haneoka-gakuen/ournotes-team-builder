//! Ordinary score and songless-potential objective adapters.
use crate::{
    chart::{self, Chart},
    live::{self, Input, Scores, Slot},
    master::Master,
    model::{self, Linear, PlayBounds, Weights, RELATIVE_ERROR},
    num::window_ms,
    power::{Member, Snap},
    search::{Exact, Objective, Team, Totals},
    skills,
};
use serde_json::{json, Value};
use rustc_hash::FxHashMap;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};
#[derive(Clone, Copy, PartialEq)]
pub enum Criterion {
    Mean,
    Min,
    Max,
}
impl Criterion {
    pub fn from_value(v: &Value) -> Self {
        match v.as_str() {
            Some("min") => Self::Min,
            Some("max") => Self::Max,
            _ => Self::Mean,
        }
    }
    pub fn key(self, scores: &Scores) -> f64 {
        match self {
            Self::Mean => scores.mean,
            Self::Min => scores.min,
            Self::Max => scores.max,
        }
    }
}
#[derive(Clone)]
pub(crate) struct Pair {
    pub high: Weights,
    pub low: Weights,
}
struct PlayedAssignment {
    members: Vec<usize>,
    base: (f64, f64),
    rows: Vec<Arc<Vec<(i32, f64, f64)>>>,
    power: f64,
}
// A fixed conversion and recovery program leaves only additive skill coefficients.
struct PlayedTrajectory {
    id: usize,
    grades: Arc<Vec<usize>>,
    life: Option<live::LifeProgram>,
}
struct PlayedModel {
    id: usize,
    trajectory: usize,
    grades: Arc<Vec<usize>>,
    linear: Linear,
}
struct PlayedDomain {
    early_recovery: Option<Vec<usize>>,
    models: Vec<Arc<PlayedModel>>,
    frequencies: [Vec<(usize, usize, u32)>; 5],
    base: (f64, f64),
    weights: FxHashMap<(usize, usize), (f64, f64)>,
    rows: FxHashMap<(usize, usize), PlayedRow>,
    bytes: usize,
}
struct PlayedRow {
    available: Vec<bool>,
    values: Arc<Vec<(i32, f64, f64)>>,
    bytes: usize,
}
fn recovery_life(input: &Input, grades: &[usize], recoveries: [i32; 5], program: Option<&live::LifeProgram>) -> live::LifeSummary {
    program.and_then(|program| program.summary(input, recoveries)).unwrap_or_else(|| {
        let slots: Vec<_> = recoveries.iter().map(|&recovery| Slot {
            effects: Vec::new(), cumulative: Vec::new(), extension_ms: 0.0, recovery, convert: None,
        }).collect();
        let lives = live::lives(input, grades, &[0, 1, 2, 3, 4], &slots);
        live::LifeSummary {
            first_dead: lives.note.iter().position(|&life| life <= 0.0).unwrap_or(input.times.len()),
            gates: std::array::from_fn(|event| lives.gate.get(&event).copied().unwrap_or(0.0)),
        }
    })
}
struct AccuracySeed {
    weights: Vec<f64>,
    width: usize,
    cap: f64,
    columns: Vec<Vec<f64>>,
}
impl Objective for AccuracySeed {
    fn canonical_leader(&self) -> bool {
        true
    }
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        self.weights[member * self.width + (a + 1) as usize]
            == self.weights[member * self.width + (b + 1) as usize]
    }
    fn linear_assignment(&self) -> Option<(f64, f64, f64)> {
        Some((1.0, 1.0, 0.0))
    }
    fn skill(&mut self, member: usize, snap: i32) -> (f64, f64) {
        let value = self.weights[member * self.width + (snap + 1) as usize];
        (value, value)
    }
    fn bound(&mut self, totals: Totals) -> f64 {
        totals.power + totals.skill.min(self.cap)
    }
    fn member_bound(&mut self, members: &[usize], totals: Totals, left: usize) -> Option<f64> {
        if left != 0 {
            return None;
        }
        let mut values: Vec<_> = self
            .columns
            .iter()
            .map(|column| {
                members
                    .iter()
                    .map(|&member| column[member])
                    .fold(0.0_f64, f64::max)
            })
            .collect();
        values.sort_by(|a, b| b.total_cmp(a));
        Some(totals.power + totals.skill.min(values.into_iter().take(5).sum()))
    }
    fn interval(&mut self, _team: &Team, totals: Totals) -> (f64, f64) {
        let value = totals.power + totals.skill;
        (value, value)
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        let weight = team
            .members
            .iter()
            .zip(&team.snaps)
            .map(|(&member, &snap)| self.weights[member * self.width + (snap + 1) as usize])
            .sum::<f64>();
        Ok(Exact {
            key: power + weight,
            detail: Value::Null,
        })
    }
}
pub struct LiveCore {
    pub input: Input,
    timing: Option<(bool, Result<crate::input_timing::InputTiming, String>)>,
    pub high: Linear,
    pub low: Linear,
    pub factor_cap: f64,
    perfect_cap: Option<f64>,
    pub score_cap: f64,
    pub max_recovery: i32,
    pub drains: bool,
    pub(crate) unbounded: bool,
    played_envelope: bool,
    seed_weights: Option<Vec<f64>>,
    breaks_combo: bool,
    isolated_events: [bool; 5],
    isolated_models: FxHashMap<(usize, usize), (Vec<usize>, Linear)>,
    broad_play: bool,
    program_ids: Vec<usize>,
    program_slots: Vec<Slot>,
    program_converters: Vec<usize>,
    program_without_recovery: Vec<usize>,
    recovery_observed: FxHashMap<[usize; 5], bool>,
    played_trajectories: FxHashMap<[usize; 10], Arc<PlayedTrajectory>>,
    played_lives: FxHashMap<(usize, [i32; 5], bool), Arc<PlayedModel>>,
    played_models: FxHashMap<(usize, usize, [usize; 5], bool), Arc<PlayedModel>>,
    played_weights: FxHashMap<(usize, usize, usize), f64>,
    cumulative_ids: Vec<Vec<usize>>,
    next_trajectory: usize,
    next_model: usize,
    early_models: FxHashMap<(usize, usize, i32), Arc<PlayedModel>>,
    maximum_deaths: FxHashMap<usize, usize>,
    played_domains: FxHashMap<(bool, [usize; 5]), PlayedDomain>,
    played_bytes: usize,
    life_gates: Vec<f64>,
    played_domain_order: VecDeque<(bool, [usize; 5])>,
    played_assignment: Option<PlayedAssignment>,
    played_matching: crate::assignment::Scratch,
    played_prices: [Vec<(usize, f64)>; 5],
    played_allowed: [Vec<bool>; 5],
    pub slots: Arc<Vec<Slot>>,
    _play_profile: Arc<skills::Played>,
    ids: Vec<usize>,
    width: usize,
    weights: Vec<Option<Pair>>,
    team_models: HashMap<i32, (Linear, HashMap<usize, f64>)>,
    play_bounds: Option<PlayBounds>,
    scores: HashMap<(i32, [usize; 5]), Arc<Scores>>,
    power_curves: HashMap<[usize; 5], (i32, [f64; 3], [f64; 3])>,
    score_order: VecDeque<(i32, [usize; 5])>,
    snap_power: Arc<Vec<f64>>,
    power_width: usize,
    base_power: f64,
    max_frontiers: Vec<[Vec<(f64, f64)>; 5]>,
    member_profiles: HashMap<usize, [f64; 5]>,
    suffix_profiles: Vec<[f64; 5]>,
    dominance_margin: f64,
    member_dominance: HashMap<(usize, usize), bool>,
}
/// Resolved skills of every member × snap pair and their shape ids, shared by the objectives of one engine call.
type SlotTable = Arc<skills::Prepared>;
/// Addresses and lengths of the master and the card slices the table was resolved from.
type SlotTableKey = (usize, usize, usize, usize, usize);
// The outer Option is present only while Engine::run owns the request scope.
type RequestSlotTable = Option<(SlotTableKey, SlotTable)>;
thread_local! {
    static SLOT_TABLE: std::cell::RefCell<Option<RequestSlotTable>> =
        const { std::cell::RefCell::new(None) };
}
/// Clears the current request's pointer lookup; the content cache validates itself.
pub fn reset_slot_table() {
    SLOT_TABLE.with(|t| {
        if let Some(table) = t.borrow_mut().as_mut() {
            table.take();
        }
    });
}
pub(crate) struct SlotTableScope(Option<RequestSlotTable>);
impl Drop for SlotTableScope {
    fn drop(&mut self) {
        SLOT_TABLE.with(|t| t.replace(self.0.take()));
    }
}
pub(crate) fn slot_table_scope() -> SlotTableScope {
    SlotTableScope(SLOT_TABLE.with(|t| t.replace(Some(None))))
}
fn slot_table(master: &Master, members: &[Member], snaps: &[Snap]) -> SlotTable {
    let key = (
        master as *const Master as usize,
        members.as_ptr() as usize,
        members.len(),
        snaps.as_ptr() as usize,
        snaps.len(),
    );
    if let Some(table) = SLOT_TABLE.with(|t| {
        t.borrow()
            .as_ref()
            .and_then(|table| table.as_ref())
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| Arc::clone(v))
    }) {
        return table;
    }
    let table = skills::prepare(master, members, snaps);
    SLOT_TABLE.with(|t| {
        if let Some(cache) = t.borrow_mut().as_mut() {
            *cache = Some((key, Arc::clone(&table)));
        }
    });
    table
}
impl LiveCore {
    pub fn new(
        master: &Master,
        chart: &Chart,
        members: &[Member],
        snaps: &[Snap],
        play: &Value,
    ) -> Self {
        let width = snaps.len() + 1;
        let table = slot_table(master, members, snaps);
        let judgements = live::judgements_for_types(&chart.judgement_types, play);
        let mut input = chart::prepare(master, chart, 0, Vec::new(), judgements, 1.0, 100.0);
        input.clock = live::FrameClock::for_play(play);
        let timing = chart::timing_mode(play).map(|best| {
            (
                best,
                crate::input_timing::InputTiming::new(
                    master,
                    chart,
                    input.judgements.as_deref(),
                    input.clock,
                ),
            )
        });
        let play_profile = table.for_play(&input);
        let healthy_play = play_profile.healthy;
        let slots = Arc::clone(&play_profile.slots);
        let mut raise = 0.0_f64;
        let mut perfect_raise = 0.0_f64;
        let mut recovery = 0;
        let mut longest = 0;
        for slot in slots.iter() {
            if input.judgements.is_some() {
                raise = raise.max(
                    slot.effects
                        .iter()
                        .fold(0.0, |sum, e| sum + e.delta as f64 / 100000.0)
                        + slot
                            .cumulative
                            .iter()
                            .map(|factor| factor.at_cap() as f64 / 100000.0)
                            .sum::<f64>(),
                );
                recovery = recovery.max(slot.recovery);
                perfect_raise = perfect_raise.max(
                    slot.effects
                        .iter()
                        .filter(|effect| effect.kind == 2004)
                        .map(|effect| {
                            crate::num::f(effect.delta as f64 / 100000.0)
                                * effect
                                    .judgements
                                    .iter()
                                    .filter(|&&grade| grade == 5)
                                    .count() as f64
                        })
                        .sum(),
                );
            }
            for effect in &slot.effects {
                longest = longest.max(window_ms(effect.seconds, slot.extension_ms));
            }
            for factor in &slot.cumulative {
                longest = longest.max(factor.bound_duration_ms());
            }
        }
        let mut overlap = 1;
        for &time in &input.skill_times {
            overlap = overlap.max(
                input
                    .skill_times
                    .iter()
                    .filter(|&&other| other <= time && time < other + longest)
                    .count(),
            );
        }
        let (ids, intern_len) = (play_profile.ids.clone(), play_profile.shapes);
        // Component IDs follow complete slot IDs in the shared coefficient cache.
        let cumulative_ids = if input.judgements.is_some() {
            let mut factors = FxHashMap::default();
            let mut programs = vec![Vec::new(); intern_len];
            for (slot, &shape) in slots.iter().zip(&ids) {
                if !programs[shape].is_empty() || slot.cumulative.is_empty() {
                    continue;
                }
                programs[shape] = slot.cumulative.iter().map(|factor| {
                    let key = (factor.judgements.clone(), factor.unit, factor.value,
                        factor.maximum, factor.cap, factor.seconds.to_bits());
                    let next = intern_len + factors.len();
                    *factors.entry(key).or_insert(next)
                }).collect();
            }
            programs
        } else { Vec::new() };
        let unbounded_cumulative = slots
            .iter()
            .flat_map(|slot| &slot.cumulative)
            .any(|factor| factor.requires_unbounded_bounds(input.times.len()));
        let tight_play = input.life_base.is_finite()
            && input.life_base >= 0.0
            && timing.is_none()
            && !unbounded_cumulative
            && input.judgements.as_ref().is_some_and(|grades| {
                grades
                    .iter()
                    .all(|&grade| (3..=5).contains(&grade) && input.damage[grade] == 0.0)
            })
            && (0.0..=1.0).contains(&input.life_onus)
            && slots.iter().all(|slot| {
                slot.effects.iter().all(|effect| effect.delta >= 0)
                    && slot.convert.as_ref().is_none_or(|convert| {
                        convert.judgement == 5 && input.damage[convert.judgement] == 0.0
                    })
            });
        let broad_play = !tight_play
            && timing.is_none()
            && !unbounded_cumulative
            && input.judgements.is_some()
            && input.life_base.is_finite()
            && input.life_base >= 0.0
            && input
                .judgements
                .as_ref()
                .unwrap()
                .iter()
                .all(|&grade| grade <= 5)
            && input
                .damage
                .iter()
                .all(|value| value.is_finite() && *value >= 0.0)
            && (0.0..=1.0).contains(&input.life_onus)
            && input
                .combo_table
                .windows(2)
                .all(|rows| rows[0].1 <= rows[1].1)
            && slots.iter().all(|slot| {
                slot.recovery >= 0
                    && slot.effects.iter().all(|effect| effect.delta >= 0)
                    && slot
                        .convert
                        .as_ref()
                        .is_none_or(|convert| (3..=5).contains(&convert.judgement))
            });
        let (program_ids, mut program_slots) = if broad_play {
            let mut programs = HashMap::new();
            let mut program_slots = Vec::new();
            let program_ids = slots
                .iter()
                .map(|slot| {
                    let mut key = vec![slot.recovery as u64];
                    if let Some(convert) = &slot.convert {
                        key.extend([
                            convert.judgement as u64,
                            convert.limit.map_or(u64::MAX, f64::to_bits),
                            convert.at_start as u64,
                            convert.seconds.map_or_else(
                                || {
                                    window_ms(
                                        slot.effects
                                            .first()
                                            .map(|effect| effect.seconds)
                                            .unwrap_or(5.0),
                                        slot.extension_ms,
                                    )
                                },
                                |seconds| window_ms(seconds, 0.0),
                            ) as u64,
                        ]);
                        key.extend(convert.judgements.iter().map(|&grade| grade as u64));
                    }
                    let next = programs.len();
                    *programs.entry(key).or_insert_with(|| {
                        program_slots.push(slot.clone());
                        next
                    })
                })
                .collect();
            (program_ids, program_slots)
        } else {
            (Vec::new(), Vec::new())
        };
        let mut converters = FxHashMap::default();
        let mut program_converters: Vec<usize> = program_slots
            .iter()
            .map(|slot| {
                let Some(convert) = &slot.convert else {
                    return 0;
                };
                let mut signature = vec![
                    convert.at_start as u64,
                    convert.judgement as u64,
                    convert.limit.map_or(u64::MAX, f64::to_bits),
                    convert.seconds.map_or_else(
                        || {
                            window_ms(
                                slot.effects
                                    .first()
                                    .map(|effect| effect.seconds)
                                    .unwrap_or(5.0),
                                slot.extension_ms,
                            )
                        },
                        |seconds| window_ms(seconds, 0.0),
                    ) as u64,
                ];
                signature.extend(convert.judgements.iter().map(|&grade| grade as u64));
                let next = converters.len() + 1;
                *converters.entry(signature).or_insert(next)
            })
            .collect();
        let mut without_recovery = FxHashMap::default();
        for (index, slot) in program_slots.iter().enumerate() {
            if slot.recovery == 0 {
                without_recovery
                    .entry(program_converters[index])
                    .or_insert(index);
            }
        }
        let count = program_slots.len();
        let mut program_without_recovery = Vec::with_capacity(count);
        for program in 0..count {
            let converter = program_converters[program];
            let canonical = *without_recovery.entry(converter).or_insert_with(|| {
                let mut slot = program_slots[program].clone();
                slot.recovery = 0;
                let index = program_slots.len();
                program_slots.push(slot);
                program_converters.push(converter);
                index
            });
            program_without_recovery.push(canonical);
        }
        let played_envelope = tight_play || broad_play;
        let isolated_events = if tight_play
            && healthy_play
            && slots.iter().all(|slot| {
                slot.effects.iter().all(|effect| effect.gate.kind == 0)
                    && slot
                        .convert
                        .as_ref()
                        .is_none_or(|convert| !convert.at_start)
            }) {
            let mut footprint = longest;
            for slot in slots.iter() {
                if let Some(convert) = &slot.convert {
                    footprint = footprint.max(convert.seconds.map_or_else(
                        || {
                            window_ms(
                                slot.effects
                                    .first()
                                    .map(|effect| effect.seconds)
                                    .unwrap_or(5.0),
                                slot.extension_ms,
                            )
                        },
                        |seconds| window_ms(seconds, 0.0),
                    ));
                }
            }
            std::array::from_fn(|event| {
                input.skill_times.get(event).is_some_and(|&start| {
                    input
                        .skill_times
                        .iter()
                        .take(5)
                        .enumerate()
                        .all(|(other, &time)| {
                            event == other
                                || (start as i64 - time as i64).abs() > footprint as i64 + 100
                        })
                })
            })
        } else {
            [false; 5]
        };

        let mut factor_cap = 1.0 + overlap as f64 * raise;
        let mut high = if broad_play {
            model::relaxed_play(&input, input.judgements.as_ref().unwrap(), &slots, width)
        } else if let Some(j) = &input.judgements {
            model::linear_play(&input, j, true, 0, 1.0, recovery)
        } else {
            model::linear(&input)
        };
        let mut low = if let Some(j) = &input.judgements {
            model::linear_play(&input, j, false, 0, 1.0, 0)
        } else {
            high.clone()
        };
        let tight_ap = input.judgements.is_none()
            && timing.is_none()
            && !unbounded_cumulative
            && slots.iter().any(|slot| !slot.cumulative.is_empty())
            && slots
                .iter()
                .all(|slot| slot.effects.iter().all(|effect| effect.delta >= 0));
        high.nominal_cumulative = tight_ap;
        let mut perfect_cap = None;
        if played_envelope || tight_ap {
            let base: Vec<_> = slots
                .iter()
                .step_by(width)
                .map(|slot| live::rounding_profile(slot, None))
                .collect();
            let mut actions = base
                .iter()
                .map(|row| row.0)
                .max()
                .unwrap_or(0)
                .saturating_mul(5);
            let mut magnitude = (base.iter().map(|row| row.1).fold(0.0, f64::max) * 5.0).next_up();
            let mut action_gains = vec![0usize; width - 1];
            let mut magnitude_gains = vec![0.0_f64; width - 1];
            for (member, row) in slots.chunks(width).enumerate() {
                for (column, slot) in row.iter().enumerate().skip(1) {
                    let (count, absolute) = live::rounding_profile(slot, None);
                    action_gains[column - 1] =
                        action_gains[column - 1].max(count.saturating_sub(base[member].0));
                    magnitude_gains[column - 1] =
                        magnitude_gains[column - 1].max((absolute - base[member].1).next_up());
                }
            }
            action_gains.sort_unstable_by(|a, b| b.cmp(a));
            magnitude_gains.sort_unstable_by(|a, b| b.total_cmp(a));
            for gain in action_gains.into_iter().take(5) {
                actions = actions.saturating_add(gain);
            }
            for gain in magnitude_gains.into_iter().take(5) {
                magnitude = (magnitude + gain).next_up();
            }
            let n = (actions as f64 * 2.0 + 2.0) * 2.0_f64.powi(-24);
            let error = if n < 0.25 {
                (n / (1.0 - n) * (1.0 + magnitude * 2.0)).next_up()
            } else {
                f64::INFINITY
            };
            let mut judge = input.judge_frac[5];
            if let Some(grades) = &input.judgements {
                judge = grades
                    .iter()
                    .map(|&grade| input.judge_frac[grade])
                    .fold(0.0, f64::max);
                for slot in slots.iter() {
                    if let Some(convert) = &slot.convert {
                        if grades
                            .iter()
                            .any(|grade| convert.judgements.contains(grade))
                        {
                            judge = judge.max(input.judge_frac[convert.judgement]);
                        }
                    }
                }
            }
            let error_base = input
                .note_frac
                .iter()
                .zip(&input.ap_combo)
                .map(|(&note, &combo)| {
                    input.adjustment * input.difficulty_factor * note * combo * judge
                })
                .sum::<f64>();
            if error_base != 0.0 {
                high.total = (high.total + (error_base * error).next_up()).next_up();
                if tight_ap {
                    low.total = (low.total - (error_base * error).next_up())
                        .next_down()
                        .max(0.0);
                }
            }
            if played_envelope {
                high.ignore_gates = true;
                high.release_windows = model::prepare_releases(&input, &slots);
                if tight_play {
                    for life in high.life_at.values_mut() {
                        *life = input.life_base * 2.0;
                    }
                }
                factor_cap = (factor_cap + error).next_up();
                if tight_play {
                    perfect_cap = Some((perfect_raise * overlap as f64 + error).next_up());
                }
                low.total = 0.0;
                low.prefix.fill(0.0);
                low.perfect_prefix.fill(0.0);
            }
        }
        let unbounded = unbounded_cumulative
            || (input.judgements.is_some()
                && !played_envelope
                && slots.iter().any(Slot::changes_play_timeline));
        let play_bounds = input
            .judgements
            .as_ref()
            .filter(|_| !played_envelope)
            .map(|j| PlayBounds::new(&input, j));
        let drains = input.judgements.as_ref().is_some_and(|grades| {
            grades.iter().map(|&grade| input.damage[grade]).sum::<f64>() >= input.life_base
        });
        let breaks_combo = input
            .judgements
            .as_ref()
            .is_some_and(|grades| grades.iter().any(|&grade| grade <= 2));
        let seed_weights = if played_envelope && breaks_combo {
            let grades = input.judgements.as_ref().unwrap();
            let weights: Vec<_> = slots
                .iter()
                .map(|slot| {
                    slot.convert
                        .as_ref()
                        .filter(|convert| convert.at_start && convert.judgement >= 3)
                        .map_or(0.0, |convert| {
                            let count = grades
                                .iter()
                                .filter(|&&grade| grade <= 2 && convert.judgements.contains(&grade))
                                .count();
                            let repairs = convert.limit.map_or(count as f64, |limit| {
                                limit.ceil().max(1.0).min(count as f64)
                            });
                            repairs * 2147483648.0
                        })
                })
                .collect();
            weights
                .iter()
                .any(|&weight| weight > 0.0)
                .then_some(weights)
        } else {
            None
        };
        let mut life_gates: Vec<_> = slots
            .iter()
            .flat_map(|slot| &slot.effects)
            .filter(|effect| effect.gate.kind == 1)
            .map(|effect| effect.gate.value)
            .collect();
        life_gates.sort_by(f64::total_cmp);
        life_gates.dedup();
        Self {
            input,
            timing,
            high,
            low,
            factor_cap,
            perfect_cap,
            score_cap: f64::INFINITY,
            max_recovery: recovery,
            drains,
            unbounded,
            played_envelope,
            seed_weights,
            breaks_combo,
            isolated_events,
            isolated_models: FxHashMap::default(),
            broad_play,
            program_ids,
            program_slots,
            program_converters,
            program_without_recovery,
            recovery_observed: FxHashMap::default(),
            played_trajectories: FxHashMap::default(),
            played_lives: FxHashMap::default(),
            played_models: FxHashMap::default(),
            played_weights: FxHashMap::default(),
            cumulative_ids,
            next_trajectory: 0,
            next_model: 0,
            early_models: FxHashMap::default(),
            maximum_deaths: FxHashMap::default(),
            played_domains: FxHashMap::default(),
            played_bytes: 0,
            life_gates,
            played_domain_order: VecDeque::new(),
            played_assignment: None,
            played_matching: crate::assignment::Scratch::default(),
            played_prices: std::array::from_fn(|_| Vec::new()),
            played_allowed: std::array::from_fn(|_| Vec::new()),
            slots,
            _play_profile: play_profile,
            ids,
            width,
            weights: vec![None; intern_len],
            team_models: HashMap::new(),
            play_bounds,
            scores: HashMap::new(),
            power_curves: HashMap::new(),
            score_order: VecDeque::new(),
            snap_power: Arc::new(Vec::new()),
            power_width: 0,
            base_power: 0.0,
            max_frontiers: Vec::new(),
            member_profiles: HashMap::new(),
            suffix_profiles: Vec::new(),
            dominance_margin: f64::INFINITY,
            member_dominance: HashMap::new(),
        }
    }
    fn at(&self, member: usize, snap: i32) -> usize {
        member * self.width + (snap + 1) as usize
    }
    pub fn slot(&self, member: usize, snap: i32) -> &Slot {
        &self.slots[self.at(member, snap)]
    }
    pub(crate) fn weight(&mut self, member: usize, snap: i32) -> Pair {
        let index = self.at(member, snap);
        let id = self.ids[index];
        if let Some(pair) = &self.weights[id] {
            return pair.clone();
        }
        let pair = {
            let skill = &self.slots[index];
            let mut upper = model::skill_weights(&self.high, &self.input, skill, true);
            if let Some(j) = &self.input.judgements {
                if skill.convert.is_some() {
                    let extra = model::conversion_weights(
                        &self.input,
                        j,
                        skill,
                        self.factor_cap,
                        self.perfect_cap,
                        &self.high.conversion_combo,
                    );
                    for (event, &value) in extra.iter().enumerate() {
                        upper.per_event[event] += value;
                    }
                    upper.mean = upper.per_event.iter().fold(0.0, |sum, v| sum + v) / 5.0;
                    upper.best = upper
                        .per_event
                        .iter()
                        .copied()
                        .fold(f64::NEG_INFINITY, f64::max);
                }
            }
            if self.isolated_events.iter().any(|&isolated| isolated) {
                for event in 0..5 {
                    if !self.isolated_events[event] {
                        continue;
                    }
                    let original = self.input.judgements.as_ref().unwrap();
                    upper.per_event[event] = if skill.convert.is_none() {
                        model::played_weight(&self.input, &self.high, original, skill, event)
                    } else {
                        let program = self._play_profile.converters[index];
                        let (grades, linear) = self
                            .isolated_models
                            .entry((program, event))
                            .or_insert_with(|| {
                                let grades =
                                    live::isolated_judgements(&self.input, skill, event, original);
                                let linear =
                                    model::linear_play(&self.input, &grades, true, 0, 1.0, 0);
                                (grades, linear)
                            });
                        linear.total - self.high.prefix[self.input.times.len()]
                            + model::played_weight(&self.input, linear, grades, skill, event)
                    };
                }
                upper.mean = upper.per_event.iter().sum::<f64>() / 5.0;
                upper.best = upper
                    .per_event
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max);
            }
            let lower = if self.played_envelope {
                model::Weights {
                    mean: 0.0,
                    best: 0.0,
                    worst: 0.0,
                    per_event: [0.0; 5],
                }
            } else {
                model::skill_weights(&self.low, &self.input, skill, false)
            };
            Pair {
                high: upper,
                low: lower,
            }
        };
        self.weights[id] = Some(pair.clone());
        pair
    }
    pub(crate) fn shape(&self, member: usize, snap: i32) -> usize {
        self.ids[self.at(member, snap)]
    }
    pub fn score_high(&self, t: Totals) -> f64 {
        if self.unbounded { return f64::INFINITY; }

        t.power * (self.high.total + t.skill) / self.input.converted_count as f64
            * (1.0 + RELATIVE_ERROR)
    }
    pub fn score_low(&self, t: Totals) -> f64 {
        if self.unbounded { return f64::NEG_INFINITY; }
        if self.played_envelope && !(0.0..=i32::MAX as f64).contains(&t.power) {return f64::NEG_INFINITY;}

        (t.power * (self.low.total + t.skill_low) / self.input.converted_count as f64
            * (1.0 - RELATIVE_ERROR)
            - self.low.absolute_error)
            .max(0.0)
    }
    pub fn evaluate(&mut self, team: &Team, power: i32) -> Result<Arc<Scores>, String> {
        let ids: [usize; 5] =
            std::array::from_fn(|slot| self.ids[self.at(team.members[slot], team.snaps[slot])]);
        let key = (power, ids);
        if let Some(scores) = self.scores.get(&key) {
            return Ok(Arc::clone(scores));
        }
        self.input.power = power;
        self.input.slots = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &m)| self.slot(m, team.snaps[slot]).clone())
            .collect();
        let (scores, curve) = match &self.timing {
            Some((best, timing))
                if self
                    .input
                    .slots
                    .iter()
                    .any(|slot| !slot.cumulative.is_empty()) =>
            {
                (
                    live::score_reachable(
                        &self.input,
                        timing.as_ref().map_err(Clone::clone)?,
                        *best,
                    )?,
                    None,
                )
            }
            _ if self.played_envelope => live::score_with_curve(&self.input)?,
            _ => (live::score(&self.input)?, None),
        };
        let scores = Arc::new(scores);
        if let Some(curve) = curve {
            let key = self.curve_key(team, ids);
            if self.power_curves.len() >= 4096 {
                self.power_curves.clear();
            }
            let exact = [scores.mean, scores.min, scores.max];
            let entry = self
                .power_curves
                .entry(key)
                .or_insert((power, curve, exact));
            if power > entry.0 {
                *entry = (power, curve, exact);
            }
        }
        if self.scores.len() >= 4096 {
            if let Some(old) = self.score_order.pop_front() {
                self.scores.remove(&old);
            }
        }
        self.score_order.push_back(key);
        self.scores.insert(key, Arc::clone(&scores));
        Ok(scores)
    }
    fn curve_key(&self, team: &Team, mut ids: [usize; 5]) -> [usize; 5] {
        let mut first: Option<&live::Conversion> = None;
        let mut unordered = true;
        for (index, &member) in team.members.iter().enumerate() {
            if let Some(convert) = self
                .slot(member, team.snaps[index])
                .convert
                .as_ref()
                .filter(|convert| convert.at_start)
            {
                if let Some(prior) = first {
                    if convert.judgement != prior.judgement
                        || convert.judgements != prior.judgements
                        || convert.seconds.is_none()
                        || convert.seconds != prior.seconds
                    {
                        unordered = false;
                        break;
                    }
                } else {
                    first = Some(convert);
                }
            }
        }
        if unordered {
            ids.sort_unstable();
        }
        ids
    }
    fn power_upper(&self, team: &Team, power: f64, criterion: Criterion) -> Option<f64> {
        if !(1.0..=i32::MAX as f64).contains(&power) {
            return None;
        }
        let ids =
            std::array::from_fn(|index| self.ids[self.at(team.members[index], team.snaps[index])]);
        let (anchor, curve, exact) = self.power_curves.get(&self.curve_key(team, ids))?;
        let index = match criterion {
            Criterion::Mean => 0,
            Criterion::Min => 1,
            Criterion::Max => 2,
        };
        let value = curve[index];
        let unit = 2.0_f64.powi(-24);
        let rounding = ((1.0 + unit) / (1.0 - unit)).powi(12).next_up();
        let upper = ((value * (power / *anchor as f64).next_up()).next_up() * rounding).next_up();
        Some(if power <= *anchor as f64 {
            upper.min(exact[index])
        } else {
            upper
        })
    }
    pub(crate) fn uses_reachable_timing(&self) -> bool {
        self.timing.is_some()
    }

    pub fn order_bounds(&mut self, team: &Team, power: f64) -> (Vec<f64>, Vec<f64>) {
        if self.unbounded { return (vec![f64::NEG_INFINITY; 120], vec![f64::INFINITY; 120]); }

        let mut lows = vec![0.0; 120];
        let mut highs = vec![0.0; 120];
        if let Some(mut bounds) = self.play_bounds.take() {
            let skills: Vec<_> = team
                .members
                .iter()
                .enumerate()
                .map(|(slot, &m)| self.slot(m, team.snaps[slot]).clone())
                .collect();
            let ids: Vec<_> = team
                .members
                .iter()
                .enumerate()
                .map(|(slot, &m)| self.ids[self.at(m, team.snaps[slot])])
                .collect();
            let j = self.input.judgements.as_ref().unwrap();
            for (index, order) in live::orders().iter().enumerate() {
                let (low, high) = bounds.order(&self.input, j, &skills, &ids, order, power);
                lows[index] = low;
                highs[index] = high;
            }
            self.play_bounds = Some(bounds);
        } else {
            let weights: Vec<_> = team
                .members
                .iter()
                .enumerate()
                .map(|(slot, &m)| self.weight(m, team.snaps[slot]))
                .collect();
            for (index, order) in live::orders().iter().enumerate() {
                let (mut low, mut high) = (0.0, 0.0);
                for (event, &slot) in order.iter().enumerate() {
                    low += weights[slot].low.per_event[event];
                    high += weights[slot].high.per_event[event];
                }
                lows[index] = self.score_low(Totals {
                    power,
                    skill_low: low,
                    ..Totals::default()
                });
                highs[index] = self.score_high(Totals {
                    power,
                    skill: high,
                    ..Totals::default()
                });
            }
        }
        (lows, highs)
    }
    pub(crate) fn prepare_dominance(&mut self, power_cap: f64) {
        if self.unbounded || self.timing.is_some() {
            return;
        }

        if self.broad_play && self.input.damage[3..=5].iter().all(|&damage| damage == 0.0)
            && self.slots.iter().all(|slot| slot.effects.iter().all(|effect| effect.delta >= 0)
                && slot.cumulative.iter().all(|factor| factor.value >= 0 && factor.at_cap() >= 0)) {
            let mut input = self.input.clone();
            let least = input.judge_frac[3..=5].iter().copied().fold(f64::INFINITY, f64::min);
            input.judge_frac[3..=5].fill(least);
            let base = model::linear_play(&input, input.judgements.as_ref().unwrap(), false, 0, 1.0, 0).total / input.converted_count as f64;
            let judge = self.input.judge_frac.iter().copied().fold(0.0_f64, f64::max);
            let upper = self.input.note_frac.iter().zip(&self.input.ap_combo).fold(0.0_f64, |sum, (&note, &combo)|
                (sum + self.input.adjustment * self.input.difficulty_factor * note * combo * judge).next_up())
                * self.factor_cap / self.input.converted_count as f64;
            let error = (self.high.total - self.high.prefix.last().copied().unwrap_or(0.0)).max(0.0) / input.converted_count as f64;
            if base > 0.0 && (upper * power_cap + input.times.len() as f64) < i32::MAX as f64 {
                self.dominance_margin = ((2.0 * (RELATIVE_ERROR * upper + error) * power_cap + 2.0 * input.times.len() as f64 + 2.0) / base).next_up() + 1.0;
            }
        }
        if self.input.judgements.is_none()
            && self
                .slots
                .iter()
                .all(|s| s.effects.iter().all(|e| e.delta >= 0))
            && self.low.total > 0.0
        {
            let base = self.low.total / self.input.converted_count as f64;
            let rounding_gap = (self.high.total - self.low.total).max(0.0) / self.low.total;
            self.dominance_margin = (((2.0 * RELATIVE_ERROR + rounding_gap * (1.0 + RELATIVE_ERROR)) * power_cap
                + self.low.absolute_error / base)
                / (1.0 - RELATIVE_ERROR))
                .next_up()
                + 1.0;
        }
    }
    fn program_dominates(&self, better: usize, worse: usize) -> bool {
        let (a, b) = (&self.slots[better], &self.slots[worse]);
        if self.program_ids[better] != self.program_ids[worse] || a.cumulative != b.cumulative { return false; }
        let mut used = vec![false; a.effects.len()];
        for effect in &b.effects {
            let candidate = a.effects.iter().enumerate().filter(|&(index, row)| !used[index]
                && row.delta >= effect.delta && row.kind == effect.kind && row.gate == effect.gate
                && row.judgements == effect.judgements && row.release_judgements == effect.release_judgements
                && window_ms(row.seconds, a.extension_ms) == window_ms(effect.seconds, b.extension_ms))
                .min_by_key(|(_, row)| row.delta).map(|(index, _)| index);
            let Some(index) = candidate else { return false; };
            used[index] = true;
        }
        true
    }
    pub(crate) fn numeric_member_dominance(
        &mut self,
        better: usize,
        worse: usize,
        power_delta: f64,
    ) -> bool {
        if power_delta <= self.dominance_margin {
            return false;
        }
        if let Some(&answer) = self.member_dominance.get(&(better, worse)) {
            return answer;
        }
        if self.broad_play {
            let answer = (0..self.width).all(|column| self.program_dominates(better * self.width + column, worse * self.width + column));
            self.member_dominance.insert((better, worse), answer);
            return answer;
        }
        let answer = (-1..self.width as i32 - 1).all(|snap| {
            let a = self.weight(better, snap).low.per_event;
            let b = self.weight(worse, snap).high.per_event;
            a.iter().zip(b).all(|(&a, b)| a >= b)
        });
        self.member_dominance.insert((better, worse), answer);
        answer
    }
    pub(crate) fn numeric_dominance(
        &mut self,
        member: usize,
        better: i32,
        worse: i32,
        power_delta: f64,
    ) -> bool {
        if power_delta <= self.dominance_margin {
            return false;
        }
        if self.broad_play { return self.program_dominates(self.at(member, better), self.at(member, worse)); }
        let a = self.weight(member, better).low.per_event;
        let b = self.weight(member, worse).high.per_event;
        a.iter().zip(b).all(|(&a, b)| a >= b)
    }
    fn global_profile(&mut self, member: usize) -> [f64; 5] {
        if let Some(&row) = self.member_profiles.get(&member) {
            return row;
        }
        let mut row = [0.0_f64; 5];
        for snap in -1..self.width as i32 - 1 {
            let weights = self.weight(member, snap).high.per_event;
            for event in 0..5 {
                row[event] = row[event].max(weights[event]);
            }
        }
        self.member_profiles.insert(member, row);
        row
    }
    fn prepare_max_members(&mut self, characters: &[Vec<usize>]) {
        self.suffix_profiles = vec![[0.0; 5]; characters.len() + 1];
        for i in (0..characters.len()).rev() {
            let mut row = self.suffix_profiles[i + 1];
            for &member in &characters[i] {
                let profile = self.global_profile(member);
                for event in 0..5 {
                    row[event] = row[event].max(profile[event]);
                }
            }
            self.suffix_profiles[i] = row;
        }
    }
    fn member_max_bound(&mut self, members: &[usize], totals: Totals, from: usize) -> Option<f64> {
        if self.unbounded { return None; }

        let suffix = *self.suffix_profiles.get(from)?;
        let matrix: [[f64; 5]; 5] = std::array::from_fn(|slot| {
            if slot < members.len() {
                self.global_profile(members[slot])
            } else {
                suffix
            }
        });
        let mut dp = [f64::NEG_INFINITY; 32];
        dp[0] = 0.0;
        for mask in 1usize..32 {
            let slot = mask.count_ones() as usize - 1;
            for event in 0..5 {
                if mask & (1 << event) != 0 {
                    dp[mask] =
                        dp[mask].max((dp[mask ^ (1 << event)] + matrix[slot][event]).next_up());
                }
            }
        }
        Some(self.score_high(Totals {
            skill: dp[31],
            ..totals
        }))
    }
    fn prepare_max(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.max_frontiers.clear();
        for (slot, &member) in members.iter().enumerate() {
            let mut frontiers: [Vec<(f64, f64)>; 5] = std::array::from_fn(|_| Vec::new());
            for &snap in &options[slot] {
                let power = self.snap_power[member * self.power_width
                    + if snap < 0 {
                        self.power_width - 1
                    } else {
                        snap as usize
                    }];
                let weights = self.weight(member, snap).high.per_event;
                for event in 0..5 {
                    frontiers[event].push((power, weights[event]));
                }
            }
            for frontier in &mut frontiers {
                frontier.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.total_cmp(&a.1)));
                let mut high = f64::NEG_INFINITY;
                frontier.retain(|&(_, weight)| {
                    if weight > high {
                        high = weight;
                        true
                    } else {
                        false
                    }
                });
            }
            self.max_frontiers.push(frontiers);
        }
    }
    fn maximum_bound(
        &mut self,
        members: &[usize],
        assigned: &[i32],
        from: usize,
        totals: Totals,
    ) -> Option<f64> {
        if self.unbounded { return None; }
        if self.max_frontiers.len() != 5 {
            return None;
        }
        let lambda = ((self.high.total + totals.skill) / totals.power.max(1.0)).sqrt();
        if !lambda.is_finite() || lambda <= 0.0 {
            return None;
        }
        let mut matrix = [[0.0; 5]; 5];
        for slot in 0..5 {
            if slot < from {
                let member = members[slot];
                let snap = assigned[slot];
                let power = self.snap_power[member * self.power_width
                    + if snap < 0 {
                        self.power_width - 1
                    } else {
                        snap as usize
                    }];
                let weights = self.weight(member, snap).high.per_event;
                for (event, &weight) in weights.iter().enumerate() {
                    matrix[slot][event] = (lambda * power).next_up() + (weight / lambda).next_up();
                }
            } else {
                for (event, value) in matrix[slot].iter_mut().enumerate() {
                    *value = self.max_frontiers[slot][event]
                        .iter()
                        .map(|&(power, weight)| {
                            ((lambda * power).next_up() + (weight / lambda).next_up()).next_up()
                        })
                        .fold(f64::NEG_INFINITY, f64::max);
                }
            }
        }
        let mut dp = [f64::NEG_INFINITY; 32];
        dp[0] = 0.0;
        for mask in 1usize..32 {
            let slot = mask.count_ones() as usize - 1;
            for event in 0..5 {
                if mask & (1 << event) != 0 {
                    dp[mask] =
                        dp[mask].max((dp[mask ^ (1 << event)] + matrix[slot][event]).next_up());
                }
            }
        }
        let sum =
            ((lambda * self.base_power).next_up() + (self.high.total / lambda).next_up() + dp[31])
                .next_up();
        Some(
            ((sum * sum).next_up() / (4.0 * self.input.converted_count as f64)).next_up()
                * (1.0 + RELATIVE_ERROR),
        )
    }
    fn order_high(&mut self, team: &Team, power: f64) -> f64 {
        let rows: Vec<_> = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &m)| self.weight(m, team.snaps[slot]).high.per_event)
            .collect();
        let mut dp = [f64::NEG_INFINITY; 32];
        dp[0] = 0.0;
        for mask in 1usize..32 {
            let slot = mask.count_ones() as usize - 1;
            for event in 0..5 {
                if mask & (1 << event) != 0 {
                    dp[mask] =
                        dp[mask].max((dp[mask ^ (1 << event)] + rows[slot][event]).next_up());
                }
            }
        }
        self.score_high(Totals {
            power,
            skill: dp[31],
            ..Totals::default()
        })
    }
    fn program(&self, member: usize, snap: i32) -> usize {
        self.program_ids[self.at(member, snap)]
    }
    fn recovery_observable(&mut self, members: &[usize], options: &[Vec<i32>]) -> bool {
        let programs: [usize; 5] =
            std::array::from_fn(|index| self.program(members[index], options[index][0]));
        let mut key = [0; 5];
        let mut registered = 0;
        for &program in &programs {
            if self.program_slots[program]
                .convert
                .as_ref()
                .is_some_and(|convert| convert.at_start)
            {
                key[registered] = self.program_converters[program];
                registered += 1;
            }
        }
        *self.recovery_observed.entry(key).or_insert_with(|| {
            let mut input = self.input.clone();
            input.slots = programs
                .iter()
                .map(|&program| {
                    let mut slot = self.program_slots[program].clone();
                    slot.recovery = 0;
                    if slot
                        .convert
                        .as_ref()
                        .is_some_and(|convert| !convert.at_start)
                    {
                        slot.convert = None;
                    }
                    slot
                })
                .collect();
            let grades = live::converted_judgements(
                &input,
                &[0, 1, 2, 3, 4],
                input.judgements.as_ref().unwrap(),
            );
            if grades.iter().all(|&grade| input.damage[grade] == 0.0)
                && self.program_slots.iter().all(|slot| slot.convert.as_ref().is_none_or(|convert| input.damage[convert.judgement] == 0.0))
                && self.life_gates.iter().all(|&threshold| {
                    (input.life_base >= threshold) == (input.life_base * 2.0 >= threshold)
                })
            {
                return false;
            }
            let Some(life) = live::LifeProgram::new(&input, &grades)
                .and_then(|program| program.play(&input, [0; 5]))
            else {
                return true;
            };
            let first = input.skill_times.iter().take(5).copied().min().unwrap_or(0);
            !life
                .note
                .iter()
                .zip(&input.times)
                .any(|(&life, &time)| life <= 0.0 && time < first)
        })
    }
    fn prepare_played_assignment(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
        ignore_recovery: bool,
        relaxed: bool,
    ) {
        let physical: [usize; 5] = std::array::from_fn(|index| {
            let program = if relaxed {
                options[index]
                    .iter()
                    .map(|&snap| self.program(members[index], snap))
                    .max_by_key(|&program| self.program_slots[program].recovery)
                    .unwrap()
            } else {
                self.program(members[index], options[index][0])
            };
            if ignore_recovery {
                self.program_without_recovery[program]
            } else {
                program
            }
        });
        let at_start = |program: usize| {
            self.program_slots[program]
                .convert
                .as_ref()
                .is_some_and(|convert| convert.at_start)
        };
        let mut key = [0; 5];
        let mut registered = 0;
        for &program in &physical {
            if at_start(program) {
                key[registered] = program;
                registered += 1;
            }
        }
        let mut rest = registered;
        for &program in &physical {
            if !at_start(program) {
                key[rest] = program;
                rest += 1;
            }
        }
        key[registered..].sort_unstable();
        let positions: [usize; 5] = std::array::from_fn(|index| {
            key.iter()
                .position(|&program| program == physical[index])
                .unwrap()
        });
        let domain_key = (relaxed, key);
        let fresh = !self.played_domains.contains_key(&domain_key);
        if fresh {
            if self.played_domains.len() >= 128 {
                if let Some(old) = self.played_domain_order.pop_front() {
                    if let Some(domain) = self.played_domains.remove(&old) {
                        self.played_bytes -= domain.bytes + domain.weights.capacity() * 64;
                    }
                }
            }
            self.played_domain_order.push_back(domain_key);
        }
        if self.played_trajectories.len() >= 256 {
            self.played_trajectories.clear();
        }
        if self.played_lives.len() >= 2048 {
            self.played_lives.clear();
        }
        if self.played_models.len() >= 512 {
            self.played_models.clear();
        }
        if self.early_models.len() >= 256 { self.early_models.clear(); }
        if self.maximum_deaths.len() >= 2048 { self.maximum_deaths.clear(); }
        if self.played_weights.len() >= 32768 {
            self.played_weights.clear();
        }
        let domain = self.played_domains.entry(domain_key).or_insert_with(|| {
            let mut input = self.input.clone();
            input.slots = key
                .iter()
                .map(|&program| self.program_slots[program].clone())
                .collect();
            let mut models: Vec<Arc<PlayedModel>> = Vec::new();
            let mut contexts = FxHashMap::default();
            let mut order_models = Vec::with_capacity(120);
            let mut program_contexts = FxHashMap::<[usize; 5], usize>::default();
            let (mut mean, mut maximum) = (0.0, 0.0_f64);
            for order in live::orders() {
                let program_order = order.map(|position| key[position]);
                if let Some(&context) = program_contexts.get(&program_order) {
                    let value = models[context].linear.total;
                    mean += value / 120.0;
                    maximum = maximum.max(value);
                    order_models.push(context);
                    continue;
                }
                let mut trajectory_key = [0; 10];
                let mut registered = 0;
                for (position, &program) in key.iter().enumerate() {
                    if input.slots[position]
                        .convert
                        .as_ref()
                        .is_some_and(|convert| convert.at_start)
                    {
                        trajectory_key[registered] = self.program_converters[program];
                        registered += 1;
                    }
                }
                for (event, &position) in order.iter().enumerate() {
                    if input.slots[position]
                        .convert
                        .as_ref()
                        .is_some_and(|convert| !convert.at_start)
                    {
                        trajectory_key[5 + event] = self.program_converters[key[position]];
                    }
                }
                // At-start registration remains in physical actor order.
                let trajectory = self
                    .played_trajectories
                    .entry(trajectory_key)
                    .or_insert_with(|| {
                        let grades = Arc::new(live::converted_judgements(
                            &input,
                            order,
                            input.judgements.as_ref().unwrap(),
                        ));
                        let life = live::LifeProgram::new(&input, &grades);
                        self.next_trajectory += 1;
                        Arc::new(PlayedTrajectory {
                            id: self.next_trajectory,
                            grades,
                            life,
                        })
                    });
                let recoveries = order.map(|position| input.slots[position].recovery);
                let model = self
                    .played_lives
                    .entry((trajectory.id, recoveries, relaxed))
                    .or_insert_with(|| {
                        let life = trajectory
                            .life
                            .as_ref()
                            .and_then(|program| program.summary(&input, recoveries))
                            .unwrap_or_else(|| {
                                let lives = live::lives(&input, &trajectory.grades, order, &input.slots);
                                live::LifeSummary {
                                    first_dead: lives.note.iter().position(|&life| life <= 0.0)
                                        .unwrap_or(input.times.len()),
                                    gates: std::array::from_fn(|event| {
                                        lives.gate.get(&event).copied().unwrap_or(0.0)
                                    }),
                                }
                            });
                        let gates = std::array::from_fn(|event| {
                            let life = life.gates[event];
                            self.life_gates
                                .partition_point(|&threshold| threshold <= life)
                        });
                        Arc::clone(
                            self.played_models
                                .entry((trajectory.id, life.first_dead, gates, relaxed))
                                .or_insert_with(|| {
                                    self.next_model += 1;
                                    Arc::new(PlayedModel {
                                        id: self.next_model,
                                        trajectory: trajectory.id,
                                        grades: Arc::clone(&trajectory.grades),
                                        linear: {
                                            let mut linear = model::played_linear(
                                                &input,
                                                &trajectory.grades,
                                                &life,
                                            );
                                            linear.ignore_gates = relaxed;
                                            linear
                                        },
                                    })
                                }),
                        )
                    });
                let context = *contexts.entry(model.id).or_insert_with(|| {
                    let index = models.len();
                    models.push(Arc::clone(model));
                    index
                });
                let value = model.linear.total;
                program_contexts.insert(program_order, context);
                mean += value / 120.0;
                maximum = maximum.max(value);
                order_models.push(context);
            }
            let mut events: Vec<_> = (0..input.skill_times.len().min(5)).collect();
            events.sort_by_key(|&event| (input.skill_times[event], event));
            let early_recovery = if relaxed && events.len() >= 2 {
                let mut latest = None;
                let mut survives = false;
                for model in &models {
                    let dead = *self.maximum_deaths.entry(model.trajectory).or_insert_with(|| {
                        let program = self.played_trajectories.values().find(|trajectory| trajectory.id == model.trajectory)
                            .and_then(|trajectory| trajectory.life.as_ref());
                        recovery_life(&input, &model.grades, [self.max_recovery; 5], program).first_dead
                    });
                    if let Some(&time) = input.times.get(dead) { latest = Some(latest.map_or(time, |prior: i32| prior.max(time))); }
                    else { survives = true; break; }
                }
                if survives { None } else {
                    latest.and_then(|dead| (1..events.len().min(3)).find(|&index|
                        input.clock.frame_time(dead) < input.clock.frame_time(input.skill_times[events[index]])))
                        .map(|index| events[..index].to_vec())
                }
            } else { None };
            let frequencies: [Vec<(usize, usize, u32)>; 5] = std::array::from_fn(|position| {
                let mut counts = vec![[0u32; 5]; models.len()];
                for (order, &context) in live::orders().iter().zip(&order_models) {
                    let event = order.iter().position(|&index| index == position).unwrap();
                    if event >= input.skill_times.len() {
                        continue;
                    }
                    counts[context][event] += 1;
                }
                counts
                    .into_iter()
                    .enumerate()
                    .flat_map(|(context, events)| {
                        events.into_iter().enumerate().filter_map(move |(event, count)| {
                            (count != 0).then_some((context, event, count))
                        })
                    })
                    .collect()
            });
            let error = (self.high.total - self.high.prefix[self.input.times.len()]).max(0.0);
            let bytes = models
                .iter()
                .map(|model| {
                    model.grades.capacity() * std::mem::size_of::<usize>()
                        + (model.linear.prefix.capacity() + model.linear.perfect_prefix.capacity())
                            * std::mem::size_of::<f64>()
                })
                .sum::<usize>()
                + frequencies
                    .iter()
                    .map(|row| row.capacity() * std::mem::size_of::<(usize, usize, u32)>())
                    .sum::<usize>();
            PlayedDomain {
                base: (((if early_recovery.is_some() { 0.0 } else { mean }) + error).next_up(), (maximum + error).next_up()),
                early_recovery,
                models,
                frequencies,
                weights: FxHashMap::default(),
                rows: FxHashMap::default(),
                bytes,
            }
        });
        if fresh {
            self.played_bytes += domain.bytes;
        }
        let capacity = domain.weights.capacity();
        let row_capacity = domain.rows.capacity();
        let bytes = domain.bytes;
        let mut rows = Vec::with_capacity(5);
        for ((position, &member), choices) in members.iter().enumerate().zip(options) {
            let allowed = &mut self.played_allowed[position];
            allowed.resize(self.width, false);
            allowed.fill(false);
            for &snap in choices {
                allowed[(snap + 1) as usize] = true;
            }
            let cached = domain.rows.entry((positions[position], member)).or_insert_with(|| PlayedRow {
                available: vec![false; self.width],
                values: Arc::new(Vec::new()),
                bytes: 0,
            });
            let mut extended = false;
            for &snap in choices {
                let column = (snap + 1) as usize;
                if cached.available[column] {
                    continue;
                }
                let at = member * self.width + column;
                let shape = self.ids[at];
                let weights = *domain.weights.entry((positions[position], shape)).or_insert_with(|| {
                    let slot = &self.slots[at];
                    let (mut mean, mut maximum) = (0.0, 0.0_f64);
                    for &(context, event, count) in &domain.frequencies[positions[position]] {
                        let model = &domain.models[context];
                        let weight_key = (model.id, shape, event);
                        let value = if let Some(&value) = self.played_weights.get(&weight_key) {
                            value
                        } else {
                            let mut value = model::played_effect_weight(
                                &self.input, &model.linear, &model.grades, slot, event,
                            );
                            for (factor, &id) in slot.cumulative.iter().zip(&self.cumulative_ids[shape]) {
                                value += *self.played_weights.entry((model.id, id, event)).or_insert_with(|| {
                                    model::cumulative_upper(&model.linear.prefix, &self.input, factor,
                                        self.input.skill_times[event], Some(&model.grades))
                                });
                            }
                            self.played_weights.insert(weight_key, value);
                            value
                        };
                        let mean_value = if domain.early_recovery.as_ref().is_some_and(|events| events.contains(&event)) {
                            let recovery = slot.recovery;
                            let alternate = self.early_models.entry((model.trajectory, event, recovery)).or_insert_with(|| {
                                let mut recoveries = [self.max_recovery; 5]; recoveries[event] = recovery;
                                let program = self.played_trajectories.values().find(|trajectory| trajectory.id == model.trajectory)
                                    .and_then(|trajectory| trajectory.life.as_ref());
                                let life = recovery_life(&self.input, &model.grades, recoveries, program);
                                self.next_model += 1;
                                Arc::new(PlayedModel { id: self.next_model, trajectory: model.trajectory,
                                    grades: Arc::clone(&model.grades), linear: {
                                        let mut linear = model::played_linear(&self.input, &model.grades, &life);
                                        let first = domain.early_recovery.as_ref().unwrap()[0];
                                        linear.ignore_gates = event != first || self.input.skill_times.iter().take(5).enumerate().any(|(other, &time)|
                                            other != first && self.input.clock.frame_time(time) == self.input.clock.frame_time(self.input.skill_times[first]));
                                        linear
                                    } })
                            });
                            let weight = *self.played_weights.entry((alternate.id, shape, event)).or_insert_with(||
                                model::played_weight(&self.input, &alternate.linear, &alternate.grades, slot, event));
                            (weight + (alternate.linear.total / domain.early_recovery.as_ref().unwrap().len() as f64).next_up()).next_up()
                        } else { value };
                        mean = (mean + ((mean_value * count as f64).next_up() / 120.0).next_up()).next_up();
                        maximum = maximum.max(value);
                    }
                    (mean.next_up(), maximum.next_up())
                });
                Arc::make_mut(&mut cached.values).push((snap, weights.0, weights.1));
                cached.available[column] = true;
                extended = true;
            }
            if extended {
                let row_bytes = cached.available.capacity()
                    + cached.values.capacity() * std::mem::size_of::<(i32, f64, f64)>()
;
                domain.bytes = domain.bytes - cached.bytes + row_bytes;
                cached.bytes = row_bytes;
            }
            // Previously seen choices remain a superset used only for upper bounds.
            rows.push(Arc::clone(&cached.values));
        }
        domain.bytes += (domain.rows.capacity() - row_capacity) * 128;
        self.played_bytes = self.played_bytes - bytes + domain.bytes
            + (domain.weights.capacity() - capacity) * 64;
        self.played_assignment = Some(PlayedAssignment {
            members: members.to_vec(),
            base: domain.base,
            rows,
            power: self.base_power,
        });
        while self.played_bytes > 64 * 1024 * 1024 && self.played_domains.len() > 1 {
            let Some(old) = self.played_domain_order.pop_front() else {
                break;
            };
            if let Some(domain) = self.played_domains.remove(&old) {
                self.played_bytes -= domain.bytes + domain.weights.capacity() * 64;
            }
        }
    }
    pub fn team_high(&mut self, team: &Team, t: Totals) -> f64 {
        if self.unbounded {
            return f64::INFINITY;
        }
        if self.played_envelope {
            return self
                .power_upper(team, t.power, Criterion::Max)
                .map_or(self.score_high(t), |upper| self.score_high(t).min(upper));
        }

        let Some(j) = &self.input.judgements else {
            return self.score_high(t);
        };
        let recovery = team
            .members
            .iter()
            .enumerate()
            .map(|(slot, &m)| self.slot(m, team.snaps[slot]).recovery)
            .max()
            .unwrap_or(0)
            .max(0);
        if recovery >= self.max_recovery {
            return self.score_high(t);
        }
        let entry = self.team_models.entry(recovery).or_insert_with(|| {
            (
                model::linear_play(&self.input, j, true, 0, 1.0, recovery),
                HashMap::new(),
            )
        });
        let mut skill = 0.0;
        for (slot, &member) in team.members.iter().enumerate() {
            let index = member * self.width + (team.snaps[slot] + 1) as usize;
            let id = self.ids[index];
            skill += *entry.1.entry(id).or_insert_with(|| {
                let value = &self.slots[index];
                let mut weights = model::skill_weights(&entry.0, &self.input, value, true);
                if value.convert.is_some() {
                    let extra = model::conversion_weights(
                        &self.input,
                        j,
                        value,
                        self.factor_cap,
                        self.perfect_cap,
                        &self.high.conversion_combo,
                    );
                    for (event, &value) in extra.iter().enumerate() {
                        weights.per_event[event] += value;
                    }
                }
                weights
                    .per_event
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max)
            });
        }
        t.power * (entry.0.total + skill) / self.input.converted_count as f64
            * (1.0 + RELATIVE_ERROR)
    }
}
pub struct LiveObjective {
    pub core: LiveCore,
    pub criterion: Criterion,
    /// Replaces the criterion when set.
    pub target: Option<Target>,
}
impl LiveObjective {
    fn max_bounds(&self) -> bool {
        self.criterion == Criterion::Max && self.target.is_none()
            || self
                .target
                .is_some_and(|t| !t.capped && self.core.score_cap >= t.score)
    }
    fn target_max_bound(&self, high: f64) -> f64 {
        self.target
            .map_or(high, |t| t.ceiling(high.min(self.core.score_cap)))
    }
}
impl Objective for LiveObjective {
    fn seed_objective(&self) -> Option<Box<dyn Objective>> {
        if !self.core.played_envelope {
            return None;
        }
        Some(if let Some(weights) = &self.core.seed_weights {
            let mut maxima = Vec::new();
            let mut columns = Vec::new();
            for column in 1..self.core.width {
                let values: Vec<_> = weights
                    .iter()
                    .skip(column)
                    .step_by(self.core.width)
                    .copied()
                    .collect();
                let maximum = values.iter().copied().fold(0.0_f64, f64::max);
                if maximum > 0.0 {
                    maxima.push(maximum);
                    columns.push(values);
                }
            }
            maxima.sort_by(|a, b| b.total_cmp(a));
            Box::new(AccuracySeed {
                weights: weights.clone(),
                width: self.core.width,
                cap: maxima.into_iter().take(5).sum(),
                columns,
            }) as Box<dyn Objective>
        } else {
            Box::new(crate::api::PowerObjective)
        })
    }
    fn seed_ready(&self, detail: &Value) -> bool { detail["scores"].is_object() }
    fn score_ties(&self) -> bool {
        self.target.is_none() && self.criterion != Criterion::Mean
    }
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        !self.core.unbounded && self.core.shape(member, a) == self.core.shape(member, b)
    }
    fn preserve_snap_options(&self) -> bool {
        self.core.unbounded
    }
    fn conditional_snap_dominance(&self) -> bool {
        !self.core.unbounded
    }
    fn canonical_leader(&self) -> bool {
        !self.core.unbounded && !self.core.played_envelope
    }
    fn numeric_member_dominance(&mut self, better: usize, worse: usize, power_delta: f64) -> bool {
        self.core
            .numeric_member_dominance(better, worse, power_delta)
    }
    fn needs_power_cap(&self) -> bool {
        self.core.input.judgements.is_none() || self.core.broad_play
    }
    fn prepare_dominance(&mut self, power_cap: f64) {
        self.core.prepare_dominance(power_cap);
    }
    fn numeric_snap_dominance(
        &mut self,
        member: usize,
        better: i32,
        worse: i32,
        power_delta: f64,
    ) -> bool {
        self.core
            .numeric_dominance(member, better, worse, power_delta)
    }

    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        if self.max_bounds() {
            self.core.prepare_max_members(characters);
        }
    }
    fn member_bound_at(
        &mut self,
        members: &[usize],
        totals: Totals,
        _left: usize,
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        if self.max_bounds() {
            let high = self.core.member_max_bound(members, totals, from)?;
            Some(self.target_max_bound(high))
        } else {
            None
        }
    }
    fn prepare_power(&mut self, power: Arc<Vec<f64>>, width: usize) {
        if self.core.power_width != width || !Arc::ptr_eq(&self.core.snap_power, &power) {
            self.core.played_assignment = None;
            for domain in self.core.played_domains.values_mut() {
                let bytes = domain.rows.values().map(|row| row.bytes).sum::<usize>()
                    + domain.rows.capacity() * 128;
                domain.rows = FxHashMap::default();
                domain.bytes -= bytes;
                self.core.played_bytes -= bytes;
            }
        }
        self.core.snap_power = power;
        self.core.power_width = width;
    }
    fn prepare_assignment_power(&mut self, power: f64) {
        self.core.base_power = power;
    }
    fn assignment_domains(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
    ) -> Option<Vec<Vec<Vec<i32>>>> {
        if !self.core.broad_play {
            return None;
        }
        Some(
            members
                .iter()
                .zip(options)
                .map(|(&member, choices)| {
                    let mut groups: Vec<(usize, Vec<i32>)> = Vec::new();
                    for &snap in choices {
                        let key = self.core.program_converters[self.core.program(member, snap)];
                        if let Some((_, group)) =
                            groups.iter_mut().find(|(profile, _)| *profile == key)
                        {
                            group.push(snap);
                        } else {
                            groups.push((key, vec![snap]));
                        }
                    }
                    groups.into_iter().map(|(_, group)| group).collect()
                })
                .collect(),
        )
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.core.played_assignment = None;
        if self.core.broad_play
            && members.iter().zip(options).all(|(&member, row)| {
                !row.is_empty()
                    && row.iter().all(|&snap| {
                        self.core.program(member, snap) == self.core.program(member, row[0])
                    })
            })
        {
            self.core.prepare_played_assignment(members, options, false, false);
        }
        if self.max_bounds() {
            self.core.prepare_max(members, options);
        }
    }
    fn refine_assignment_domains(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
    ) -> Option<Vec<Vec<Vec<i32>>>> {
        if !self.core.broad_play || !self.core.recovery_observable(members, options) {
            return None;
        }
        let mut split = false;
        let groups = members
            .iter()
            .zip(options)
            .map(|(&member, choices)| {
                let mut groups: Vec<(usize, Vec<i32>)> = Vec::new();
                for &snap in choices {
                    let key = self.core.program(member, snap);
                    if let Some((_, group)) = groups.iter_mut().find(|(program, _)| *program == key)
                    {
                        group.push(snap);
                    } else {
                        groups.push((key, vec![snap]));
                    }
                }
                split |= groups.len() > 1;
                groups.into_iter().map(|(_, group)| group).collect()
            })
            .collect();
        split.then_some(groups)
    }
    fn prepare_assignment_domain(
        &mut self,
        members: &[usize],
        options: &[Vec<i32>],
        complete: bool,
    ) {
        if !self.core.broad_play {
            self.prepare_assignment(members, options);
            return;
        }
        self.core.played_assignment = None;
        let converters_fixed = members.iter().zip(options).all(|(&member, row)| {
            let first = self.core.program_converters[self.core.program(member, row[0])];
            row.iter().all(|&snap| self.core.program_converters[self.core.program(member, snap)] == first)
        });
        if complete || converters_fixed {
            let ignore_recovery = !self.core.recovery_observable(members, options);
            let relaxed = !ignore_recovery
                && members.iter().zip(options).any(|(&member, choices)| {
                    choices.iter().any(|&snap| {
                        self.core.program(member, snap) != self.core.program(member, choices[0])
                    })
                });
            self.core
                .prepare_played_assignment(members, options, ignore_recovery, relaxed);
        }
        if self.max_bounds() {
            self.core.prepare_max(members, options);
        }
    }
    fn partial_assignment_domains(&self) -> bool {
        !self.core.broad_play || self.max_bounds()
    }
    fn partial_refined_assignment_domains(&self) -> bool { true }
    fn assignment_order_bound(
        &mut self,
        members: &[usize],
        totals: Totals,
        assigned: &[i32],
        from: usize,
        limit: f64,
    ) -> Option<f64> {
        if let Some(domain) = &self.core.played_assignment {
            let (base, rows) = (&domain.base, &domain.rows);
            let mut skill = 0.0;
            let maximum = self.target.is_some() || self.criterion == Criterion::Max;
            for (index, row) in rows.iter().enumerate() {
                let weight = row
                    .iter()
                    .filter(|entry| self.core.played_allowed[index][(entry.0 + 1) as usize]
                        && (index >= from || assigned[index] == entry.0))
                    .map(|entry| if maximum { entry.2 } else { entry.1 })
                    .fold(f64::NEG_INFINITY, f64::max);
                skill = (skill + weight).next_up();
            }
            let scale = (1.0 + RELATIVE_ERROR) / self.core.input.converted_count as f64;
            let base_skill = if maximum { base.1 } else { base.0 };
            let mut high = totals.power * (base_skill + skill) * scale;
            if self.target_max_bound(high) < limit {
                return Some(self.target_max_bound(high));
            }
            let mut lambda = ((base_skill + skill) / totals.power).sqrt();
            if domain.power >= 0.0 && base_skill >= 0.0 && lambda.is_finite() && lambda > 0.0 {
                for _ in 0..3 {
                    let (mut power, mut weight) = (domain.power, base_skill);
                    let mut sum = ((lambda * domain.power).next_up() + (base_skill / lambda).next_up()).next_up();
                    let price = |point: &(f64, f64)| ((lambda * point.0).next_up() + (point.1 / lambda).next_up()).next_up();
                    let columns = self.core.power_width - 1;
                    for (slot, row) in rows.iter().enumerate() {
                        let prices = &mut self.core.played_prices[slot];
                        prices.clear();
                        let mut best = (f64::NEG_INFINITY, (0.0, 0.0));
                        for &(snap, mean, maximum_weight) in row.iter() {
                            if !self.core.played_allowed[slot][(snap + 1) as usize]
                                || slot < from && snap != assigned[slot]
                                || slot >= from && snap >= 0 && assigned[..from].contains(&snap) { continue; }
                            let column = if snap < 0 { columns } else { snap as usize };
                            let point = (self.core.snap_power[members[slot] * self.core.power_width + column], if maximum { maximum_weight } else { mean });
                            if point.0 < 0.0 || point.1 < 0.0 { return Some(self.target_max_bound(high)); }
                            let value = price(&point);
                            if value > best.0 { best = (value, point); }
                            prices.push((if snap < 0 { columns + slot } else { column }, value));
                        }
                        sum = (sum + best.0).next_up();
                        power += best.1.0;
                        weight += best.1.1;
                    }
                    high = high.min((((sum * sum).next_up() / 4.0).next_up() * scale).next_up());
                    if self.target.is_none() && high < limit { break; }
                    let matching = self
                        .core
                        .played_matching
                        .sparse_upper(&self.core.played_prices, columns);
                    let matched = (((lambda * domain.power).next_up()
                        + (base_skill / lambda).next_up())
                    .next_up()
                        + matching)
                        .next_up();
                    high = high
                        .min((((matched * matched).next_up() / 4.0).next_up() * scale).next_up());
                    if self.target.is_none() && high < limit {
                        break;
                    }
                    let next = (weight / power).sqrt();
                    if !next.is_finite() || next <= 0.0 || (next / lambda - 1.0).abs() < 1e-6 {
                        break;
                    }
                    lambda = next;
                }
            }
            return Some(self.target_max_bound(high));
        }
        if self.max_bounds() {
            let high = self.core.maximum_bound(members, assigned, from, totals)?;
            Some(self.target_max_bound(high))
        } else {
            None
        }
    }
    fn snap_program_identity(&self) -> Option<usize> {
        Some(Arc::as_ptr(&self.core._play_profile) as usize)
    }
    fn snap_dominance(&mut self, member: usize, snap: i32) -> Vec<f64> {
        let shape = self.core.shape(member, snap) as f64;
        vec![shape, -shape]
    }
    fn skill(&mut self, member: usize, snap: i32) -> (f64, f64) {
        let w = self.core.weight(member, snap);
        if self.target.is_some() {
            return (w.low.mean, w.high.mean);
        }
        match self.criterion {
            Criterion::Min => (w.low.worst, w.high.mean),
            Criterion::Max => (w.low.mean, w.high.best),
            Criterion::Mean => (w.low.mean, w.high.mean),
        }
    }
    fn bound(&mut self, t: Totals) -> f64 {
        let high = self.core.score_high(t);
        self.target.map_or(high, |target| {
            if !target.capped && self.core.score_cap < target.score {
                TIE * high.min(self.core.score_cap)
            } else {
                target.mean_ceiling(high.min(self.core.score_cap))
            }
        })
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<crate::search::JointTarget>> {
        if self.core.unbounded { return None; }
        let mut score = limit;
        if let Some(target) = self.target {
            score = if !target.capped && self.core.score_cap < target.score {
                (limit.next_down() / TIE).next_down()
            } else {
                target.mean_floor(limit)
            };
        }
        if !score.is_finite() {
            return None;
        }
        Some(vec![crate::search::JointTarget {
            bonus: 0.0,
            product: (score.max(0.0) * self.core.input.converted_count as f64
                / (1.0 + RELATIVE_ERROR))
                .next_down()
                .max(0.0),
        }])
    }
    fn product_base(&self) -> Option<f64> {
        (!self.core.unbounded).then_some(self.core.high.total)
    }
    fn leaf_bound(&mut self, team: &Team, t: Totals) -> Option<f64> {
        if self.core.input.judgements.is_some() {
            let mut high = self.core.team_high(team, t);
            if let Some(curve) = self.core.power_upper(
                team,
                t.power,
                if self.target.is_some() {
                    Criterion::Max
                } else {
                    self.criterion
                },
            ) {
                high = high.min(curve);
            }
            if let Some(domain) = &self.core.played_assignment {
                if domain.members == team.members {
                    let maximum = self.target.is_some() || self.criterion == Criterion::Max;
                    let weights = domain
                        .rows
                        .iter()
                        .zip(&team.snaps)
                        .map(|(row, snap)| {
                            row.iter().find(|entry| entry.0 == *snap).map(|entry| {
                                if maximum {
                                    entry.2
                                } else {
                                    entry.1
                                }
                            })
                        })
                        .collect::<Option<Vec<_>>>();
                    if let Some(weights) = weights {
                        let base = if maximum {
                            domain.base.1
                        } else {
                            domain.base.0
                        };
                        let skill = weights
                            .iter()
                            .fold(0.0_f64, |sum, &value| (sum + value).next_up());
                        high = high.min(
                            t.power * (base + skill) / self.core.input.converted_count as f64
                                * (1.0 + RELATIVE_ERROR),
                        );
                    }
                }
            }

            Some(
                self.target
                    .map_or(high, |target| target.ceiling(high.min(self.core.score_cap))),
            )
        } else {
            None
        }
    }
    fn prefer_snap(&self, member: usize, snap: i32) -> f64 {
        if self.core.breaks_combo {
            if let Some(convert) = &self.core.slot(member, snap).convert {
                if convert.at_start
                    && convert.judgement >= 3
                    && convert.judgements.iter().any(|&grade| grade <= 2)
                {
                    return self.core.max_recovery as f64 + 1.0 + convert.limit.unwrap_or(0.0);
                }
            }
        }
        if self.core.drains {
            self.core.slot(member, snap).recovery as f64
        } else {
            0.0
        }
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
        if let Some(target) = self.target {
            if self.core.score_cap < target.score {
                let low = self.core.score_low(t);
                let high = self.core.score_high(t).min(self.core.score_cap);
                let value = |mean: f64| {
                    if target.capped {
                        mean + TIE * mean
                    } else {
                        TIE * mean
                    }
                };
                return (value(low), value(high));
            }

            let (lows, highs) = self.core.order_bounds(team, t.power);
            return (target.value(&lows), target.value(&highs));
        }
        if self.criterion == Criterion::Mean && self.core.input.judgements.is_none() {
            return (self.core.score_low(t), self.core.score_high(t));
        }
        let (lows, highs) = self.core.order_bounds(team, t.power);
        match self.criterion {
            Criterion::Mean => (
                lows.iter().fold(0.0, |sum, v| sum + v) / 120.0,
                highs.iter().fold(0.0, |sum, v| sum + v) / 120.0,
            ),
            Criterion::Min => (
                lows.into_iter().fold(f64::INFINITY, f64::min),
                highs.into_iter().fold(f64::INFINITY, f64::min),
            ),
            Criterion::Max => (
                lows.into_iter().fold(f64::NEG_INFINITY, f64::max),
                highs.into_iter().fold(f64::NEG_INFINITY, f64::max),
            ),
        }
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        let scores = self.core.evaluate(team, power as i32)?;
        Ok(Exact {
            key: self.target.map_or_else(
                || self.criterion.key(&scores),
                |target| target.value(&scores.scores),
            ),
            detail: json!({"scores":&*scores}),
        })
    }
    fn exact_value(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<f64, String> {
        let scores = self.core.evaluate(team, power as i32)?;
        Ok(self.target.map_or_else(|| self.criterion.key(&scores), |target| target.value(&scores.scores)))
    }
}
pub struct Potential {
    areas: Vec<f64>,
    width: usize,
    window: f64,
}
impl Potential {
    pub fn new(master: &Master, members: &[Member], snaps: &[Snap], window: f64) -> Self {
        let width = snaps.len() + 1;
        let mut areas = Vec::with_capacity(members.len() * width);
        let table = skills::prepare(master, members, snaps);
        for skill in table.slots.iter() {
            let mut area = 0.0;
            for effect in &skill.effects {
                if (effect.kind == 2000 || effect.judgements.contains(&5))
                    && !effect.gate.blocks(master.live.life_base)
                {
                    area += effect.delta as f64 / 100000.0
                        * (window_ms(effect.seconds, skill.extension_ms) as f64 / 1000.0);
                }
            }
            for factor in &skill.cumulative {
                area += factor.at_cap() as f64 / 100000.0
                    * (factor.duration_ms() as f64 / 1000.0);
            }
            areas.push(area);
        }
        Self {
            areas,
            width,
            window,
        }
    }
    fn area(&self, member: usize, snap: i32) -> f64 {
        self.areas[member * self.width + (snap + 1) as usize]
    }
    fn key(&self, power: f64, area: f64) -> f64 {
        power * (1.0 + area / self.window)
    }
}
impl Objective for Potential {
    fn conditional_snap_dominance(&self) -> bool {
        true
    }
    fn canonical_leader(&self) -> bool {
        true
    }
    fn skill(&mut self, member: usize, snap: i32) -> (f64, f64) {
        let area = self.area(member, snap);
        (area, area)
    }
    fn bound(&mut self, t: Totals) -> f64 {
        self.key(t.power, t.skill) * (1.0 + 1e-12)
    }
    fn interval(&mut self, _team: &Team, t: Totals) -> (f64, f64) {
        (self.key(t.power, t.skill_low), self.key(t.power, t.skill))
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        let area = team
            .members
            .iter()
            .enumerate()
            .fold(0.0, |sum, (slot, &m)| sum + self.area(m, team.snaps[slot]));
        let value = self.key(power, area);
        Ok(Exact {
            key: value,
            detail: json!({"potential":value,"area":area}),
        })
    }
}

pub struct ScoreCap {
    pub core: LiveCore,
}
impl Objective for ScoreCap {
    fn exchange_snap_equivalent(&mut self, member: usize, a: i32, b: i32) -> bool {
        !self.core.unbounded && self.core.shape(member, a) == self.core.shape(member, b)
    }
    fn preserve_snap_options(&self) -> bool {
        self.core.unbounded
    }
    fn conditional_snap_dominance(&self) -> bool {
        !self.core.unbounded
    }
    fn canonical_leader(&self) -> bool {
        !self.core.unbounded && !self.core.played_envelope
    }
    fn needs_power_cap(&self) -> bool {
        self.core.input.judgements.is_none()
    }
    fn prepare_dominance(&mut self, power_cap: f64) {
        self.core.prepare_dominance(power_cap);
    }
    fn numeric_snap_dominance(&mut self, m: usize, a: i32, b: i32, delta: f64) -> bool {
        self.core.numeric_dominance(m, a, b, delta)
    }
    fn numeric_member_dominance(&mut self, a: usize, b: usize, delta: f64) -> bool {
        self.core.numeric_member_dominance(a, b, delta)
    }
    fn snap_dominance(&mut self, m: usize, s: i32) -> Vec<f64> {
        let shape = self.core.shape(m, s) as f64;
        vec![shape, -shape]
    }
    fn prepare_power(&mut self, power: Arc<Vec<f64>>, width: usize) {
        self.core.snap_power = power;
        self.core.power_width = width;
    }
    fn prepare_assignment_power(&mut self, power: f64) {
        self.core.base_power = power;
    }
    fn prepare_members(&mut self, characters: &[Vec<usize>]) {
        self.core.prepare_max_members(characters);
    }
    fn member_bound_at(
        &mut self,
        members: &[usize],
        t: Totals,
        _left: usize,
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        self.core.member_max_bound(members, t, from)
    }
    fn prepare_assignment(&mut self, members: &[usize], options: &[Vec<i32>]) {
        self.core.prepare_max(members, options);
    }
    fn assignment_order_bound(
        &mut self,
        members: &[usize],
        t: Totals,
        assigned: &[i32],
        from: usize,
        _limit: f64,
    ) -> Option<f64> {
        self.core.maximum_bound(members, assigned, from, t)
    }
    fn skill(&mut self, m: usize, s: i32) -> (f64, f64) {
        let best = self.core.weight(m, s).high.best;
        (best, best)
    }
    fn bound(&mut self, t: Totals) -> f64 {
        self.core.score_high(t)
    }
    fn joint_targets(&mut self, limit: f64) -> Option<Vec<crate::search::JointTarget>> {
        if self.core.unbounded { return None; }
        Some(vec![crate::search::JointTarget {
            bonus: 0.0,
            product: (limit.max(0.0) * self.core.input.converted_count as f64
                / (1.0 + RELATIVE_ERROR))
                .next_down()
                .max(0.0),
        }])
    }
    fn product_base(&self) -> Option<f64> {
        (!self.core.unbounded).then_some(self.core.high.total)
    }
    fn interval(&mut self, team: &Team, t: Totals) -> (f64, f64) {
        if self.core.unbounded { return (f64::INFINITY, f64::INFINITY); }
        let value = self.core.order_high(team, t.power);
        (value, value)
    }
    fn exact(&mut self, team: &Team, power: f64, _slots: &[f64]) -> Result<Exact, String> {
        if self.core.unbounded { return Ok(Exact { key: f64::INFINITY, detail: Value::Null }); }
        Ok(Exact {
            key: self.core.order_high(team, power),
            detail: Value::Null,
        })
    }
}

/// A score target over the 120 member orders: the share of orders that reach `score`, or the mean score capped at
/// `score`. Exact ties keep the higher mean score, at a weight below one order's share.
#[derive(Clone, Copy)]
pub struct Target {
    pub score: f64,
    pub capped: bool,
    peak: f64,
}
const TIE: f64 = 1e-12;
impl Target {
    pub fn from_value(v: &Value) -> Option<Self> {
        let score = v["score"].as_f64().filter(|s| s.is_finite() && *s > 0.0)?;
        match v["kind"].as_str() {
            Some("at-least") => Some(Self {
                score,
                capped: false,
                peak: 1.0,
            }),
            Some("capped") => Some(Self {
                score,
                capped: true,
                peak: (0..120).fold(0.0, |sum, _| sum + score) / 120.0,
            }),
            _ => None,
        }
    }
    fn value(self, scores: &[f64]) -> f64 {
        let n = scores.len().max(1) as f64;
        let mean = scores.iter().fold(0.0, |sum, v| sum + v) / n;
        let main = if self.capped {
            scores.iter().fold(0.0, |sum, &v| sum + v.min(self.score)) / n
        } else {
            scores.iter().filter(|&&v| v >= self.score).count() as f64 / n
        };
        main + TIE * mean
    }
    fn mean_ceiling(self, high: f64) -> f64 {
        if high == 0.0 {
            return 0.0;
        }
        let main = if self.capped {
            high.min(self.peak)
        } else {
            (high / self.score).next_up().min(1.0)
        };
        (main + (TIE * high).next_up()).next_up()
    }
    fn mean_floor(self, limit: f64) -> f64 {
        let lower = limit.next_down();
        let slope = if self.capped {
            (1.0 + TIE).next_up()
        } else {
            ((1.0 / self.score).next_up() + TIE).next_up()
        };
        let mut floor = (lower / slope).next_down();
        if lower > self.peak {
            floor = floor.max(((lower - self.peak).next_down() / TIE).next_down());
        }
        floor.max(0.0)
    }
    /// Upper bound from an upper bound on every order's score.
    fn ceiling(self, high: f64) -> f64 {
        let main = if self.capped {
            return self.mean_ceiling(high);
        } else if high >= self.score {
            1.0
        } else {
            0.0
        };
        main + TIE * high
    }
}
