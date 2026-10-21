//! Compile converted runtime charts into score factors and rank thresholds.
use crate::{
    live,
    master::{Difficulty, Master, ScoreRank, Song},
    num::{f, i32},
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub time_ms: i32,
    pub operate_type: i32,
    pub judgement_type: i32,
    pub judged: bool,
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnumerationNote {
    pub time_ms: i32,
    pub op: i32,
    #[serde(default)]
    pub judgement_type: i32,
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub duration_ms: i32,
    pub notes: Vec<Note>,
    pub skill_times_ms: Vec<i32>,
    pub fever_ms: Vec<(i32, i32)>,
    pub enumeration: Option<Vec<EnumerationNote>>,
}
#[derive(Clone)]
pub struct Chart {
    pub key: String,
    pub song: Song,
    pub difficulty: Difficulty,
    pub times: Vec<i32>,
    pub note_types: Vec<i32>,
    pub judgement_types: Vec<i32>,
    pub note_frac: Vec<f64>,
    pub converted_count: usize,
    pub difficulty_factor: f64,
    pub skill_times: Vec<i32>,
    pub skill_order: Vec<usize>,
    pub fevers: Vec<(i32, i32)>,
    pub duration_ms: i32,
    pub last_note_ms: i32,
    pub ranks: Vec<ScoreRank>,
    pub skip_note_frac: Vec<f64>,
}
pub fn difficulty_factor(level: f64) -> f64 {
    f(f((level - 5.0) * f(0.005)) + 1.0)
}
pub fn compile(master: &Master, song: &Song, difficulty: &Difficulty, source: &Source) -> Chart {
    let judged: Vec<_> = source.notes.iter().filter(|n| n.judged).collect();
    let times: Vec<_> = judged.iter().map(|n| n.time_ms).collect();
    let mut percent_sum = 0i32;
    let note_frac = judged
        .iter()
        .map(|n| {
            let percent = master
                .live
                .note_percent
                .get(&n.operate_type)
                .copied()
                .unwrap_or(0.0);
            percent_sum = i32(percent_sum as f64 + percent);
            f(percent / 100.0)
        })
        .collect();
    let fallback = source
        .notes
        .iter()
        .map(|n| EnumerationNote {
            time_ms: n.time_ms,
            op: n.operate_type,
            judgement_type: n.judgement_type,
        })
        .collect();
    let enumeration = source.enumeration.as_ref().unwrap_or(&fallback);
    let frame = |ms: i32| {
        if ms < 0 {
            0
        } else {
            f(f(ms as f64) / 40.0).ceil() as i32
        }
    };
    let last = enumeration
        .iter()
        .map(|n| n.time_ms)
        .max()
        .unwrap_or(0)
        .max(0);
    let max_frame = frame(last + 1000) + 50;
    let valid: Vec<_> = enumeration
        .iter()
        .filter(|n| n.op != 120 && master.live.note_percent.contains_key(&n.op))
        .collect();
    let capped = |ms| frame(ms).min(max_frame - 1);
    let to_frame = valid.last().map(|n| capped(n.time_ms).max(0)).unwrap_or(-1);
    let skip_note_frac = valid
        .into_iter()
        .filter(|n| capped(n.time_ms) <= to_frame)
        .map(|n| f(master.live.note_percent[&n.op] / 100.0))
        .collect();
    let mut skill_order: Vec<_> = (0..source.skill_times_ms.len()).collect();
    skill_order.sort_by_key(|&i| (source.skill_times_ms[i], i));
    let last_note_ms = times.last().copied().unwrap_or(0);
    Chart {
        key: format!("{}:{}", song.id, difficulty.difficulty),
        song: song.clone(),
        difficulty: difficulty.clone(),
        times,
        note_types: judged.iter().map(|n| n.operate_type).collect(),
        judgement_types: judged.iter().map(|n| n.judgement_type).collect(),
        note_frac,
        converted_count: (f(percent_sum as f64 / 100.0).ceil() as usize).max(1),
        difficulty_factor: difficulty_factor(difficulty.play_level),
        skill_times: source.skill_times_ms.clone(),
        skill_order,
        fevers: source.fever_ms.clone(),
        duration_ms: source.duration_ms,
        last_note_ms,
        ranks: master
            .score_ranks
            .get(&song.rank_group)
            .cloned()
            .unwrap_or_default(),
        skip_note_frac,
    }
}
pub fn score_rank(chart: &Chart, score: f64) -> i32 {
    chart
        .ranks
        .iter()
        .filter(|r| r.required as f64 <= score)
        .map(|r| r.rank)
        .max()
        .unwrap_or(2)
        .max(2)
}
pub fn battle_rank(chart: &Chart, score: f64) -> i32 {
    chart
        .ranks
        .iter()
        .filter(|r| r.battle_required > 0 && r.battle_required as f64 <= score)
        .map(|r| r.rank)
        .max()
        .unwrap_or(2)
        .max(2)
}
pub fn prepare(
    master: &Master,
    chart: &Chart,
    power: i32,
    slots: Vec<live::Slot>,
    judgements: Option<Vec<usize>>,
    assist: f64,
    luck_percent: f64,
) -> live::Input {
    let combo_table = master.live.combo.get(&0).cloned().unwrap_or_default();
    let mut ap_combo = Vec::with_capacity(chart.times.len());
    let mut before = 0;
    for (i, &time) in chart.times.iter().enumerate() {
        if i > 0 && time != chart.times[i - 1] {
            before = i;
        }
        let mut value = 0.0;
        for &(required, cumulative) in &combo_table {
            if required > before {
                break;
            }
            value = cumulative;
        }
        ap_combo.push(f(value.min(1.0) + 1.0));
    }
    let mut judge_frac = [0.0; 7];
    for kind in 1..=6 {
        judge_frac[7 - kind] = f(master
            .live
            .judge_percent
            .get(&(kind as i32))
            .copied()
            .unwrap_or(0.0)
            / 100.0);
    }
    live::Input {
        times: chart.times.clone(),
        note_frac: chart.note_frac.clone(),
        ap_combo,
        combo_table,
        judge_frac,
        converted_count: chart.converted_count,
        difficulty_factor: chart.difficulty_factor,
        adjustment: master.live.adjustment,
        skill_times: chart.skill_times.clone(),
        life_base: master.live.life_base,
        life_onus: master.live.life_onus,
        damage: master.live.damage,
        power,
        slots,
        judgements,
        assist,
        luck_percent,
    }
}
pub fn skip_score(master: &Master, chart: &Chart, power: i32) -> i64 {
    let t = f(f(master.live.adjustment * power as f64) * chart.difficulty_factor);
    let great = f(master.live.judge_percent.get(&3).copied().unwrap_or(80.0) / 100.0);
    let table = master
        .live
        .combo
        .get(&0)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut value = 0.0;
    for &(required, cumulative) in table {
        if required > 0 {
            break;
        }
        value = cumulative;
    }
    let combo = f(value.min(1.0) + 1.0);
    chart
        .skip_note_frac
        .iter()
        .map(|&frac| {
            f(f(f(great * f(frac * t)) * combo) / chart.converted_count as f64).floor() as i64
        })
        .sum()
}
