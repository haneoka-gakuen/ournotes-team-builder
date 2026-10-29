//! Full-live chart enumeration and native judgement/frame streams.
use super::core;
use crate::{
    chart::Source,
    master::Master,
    num::{f, round},
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveNote {
    pub id: i32,
    pub time_ms: i32,
    pub op: i32,
    pub judgement_type: i32,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgedNote {
    pub note_id: i32,
    pub judgement: i32,
    pub time_ms: i32,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    pub time_ms: i32,
    pub judged: Vec<JudgedNote>,
}
#[derive(Clone)]
pub struct Chart {
    pub notes: Vec<LiveNote>,
    pub events: Vec<(usize, i32)>,
    pub fevers: Vec<(i32, i32)>,
    pub converted: i32,
    pub last_timing: i32,
    pub level: f64,
}
#[derive(Clone)]
pub struct Setup {
    pub fevers: Vec<(i32, i32)>,
    pub missions: Vec<i32>,
    /// Placement in each range (1 = first); a missing range is first.
    pub ranks: Vec<i32>,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Accuracy {
    #[serde(default)]
    pub great: f64,
    #[serde(default = "one")]
    pub just: f64,
    #[serde(default)]
    pub miss_every: i32,
}
fn one() -> f64 {
    1.0
}
impl Default for Accuracy {
    fn default() -> Self {
        Self {
            great: 0.0,
            just: 1.0,
            miss_every: 0,
        }
    }
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Play {
    pub frames: Vec<Frame>,
    pub dt: Vec<f64>,
    pub just_eligible_ids: Vec<i32>,
}
pub fn is_judged(op: i32) -> bool {
    ![0, 80, 82, 100, 103, 121, 122, 123].contains(&op)
}
pub fn compile(master: &Master, source: &Source, level: f64) -> Result<Chart, String> {
    let enumeration = source
        .enumeration
        .as_ref()
        .filter(|e| !e.is_empty())
        .ok_or("chart without runtime enumeration")?;
    let notes: Vec<_> = enumeration
        .iter()
        .enumerate()
        .map(|(i, n)| LiveNote {
            id: i as i32 + 1,
            time_ms: n.time_ms,
            op: n.op,
            judgement_type: n.judgement_type,
        })
        .collect();
    let sum = notes
        .iter()
        .filter_map(|n| master.live.note_percent.get(&n.op))
        .fold(0i32, |sum, &p| crate::num::i32(sum as f64 + p));
    let last_timing = notes.iter().map(|n| n.time_ms).max().unwrap_or(0).max(0);
    Ok(Chart {
        notes,
        events: source.skill_times_ms.iter().copied().enumerate().collect(),
        fevers: source.fever_ms.clone(),
        converted: core::ceil_i32(f(f(sum as f64) / 100.0)),
        last_timing,
        level,
    })
}
fn choose(count: usize, wanted: i64, mut select: impl FnMut(usize)) {
    let mut remainder = 0i64;
    for i in 0..count {
        remainder += wanted;
        if remainder >= count as i64 {
            remainder -= count as i64;
            select(i);
        }
    }
}
fn project_combo_openings(
    master: &Master,
    setup: &Setup,
    times: &[i32],
    rows: &mut [(usize, &LiveNote, i32)],
) {
    for (range, &(start, end)) in setup.fevers.iter().enumerate() {
        if setup.missions.get(range) != Some(&1) {
            continue;
        }
        let first = rows.partition_point(|row| row.1.time_ms < start);
        let Some(&(frame, note, _)) = rows.get(first).filter(|row| row.1.time_ms <= end) else {
            continue;
        };
        let Some(&next_time) = times.get(frame + 1).filter(|&&time| time < end) else {
            continue;
        };
        let mut last = first;
        while last < rows.len() && rows[last].0 == frame && rows[last].1.time_ms <= end {
            last += 1;
        }
        if last - first < 2
            || !rows[first..last].iter().all(|row| {
                row.1.time_ms == note.time_ms
                    && matches!(row.1.op, 1 | 40)
                    && master
                        .live
                        .perfect_after_ms
                        .get(&row.1.judgement_type)
                        .is_some_and(|&after| {
                            next_time as i64 - row.1.time_ms as i64 <= after as i64
                        })
            })
        {
            continue;
        }
        for row in &mut rows[first + 1..last] {
            row.0 = frame + 1;
        }
    }
}
pub fn judgement_stream(
    master: &Master,
    chart: &Chart,
    setup: Option<&Setup>,
    accuracy: Accuracy,
) -> Result<Play, String> {
    let mut judged: Vec<_> = chart.notes.iter().filter(|n| is_judged(n.op)).collect();
    judged.sort_by_key(|n| (n.time_ms, n.id));
    let mut last = chart.last_timing;
    for &(_, time) in &chart.events {
        last = last.max(time);
    }
    if let Some(setup) = setup {
        if setup.fevers.len() > 3 {
            return Err("more than three fevers".into());
        }
        for &(_, end) in &setup.fevers {
            last = last.max(end);
        }
    }
    let mut times = Vec::new();
    for k in 0.. {
        let time = (k as f64 * 1000.0 / 60.0).floor() as i32;
        if time > last + 2000 {
            break;
        }
        times.push(time);
    }
    let mut windows = Vec::new();
    if let Some(setup) = setup {
        for (i, &(start, end)) in setup.fevers.iter().enumerate() {
            let mission = *setup.missions.get(i).ok_or("fewer missions than fevers")?;
            if mission != 3 {
                continue;
            }
            let first = times.partition_point(|&t| t < start);
            if first >= times.len() {
                continue;
            }
            let end = times
                .iter()
                .enumerate()
                .skip(first + 1)
                .find(|(_, t)| **t >= end)
                .map(|(i, _)| i)
                .unwrap_or(times.len());
            windows.push((first, end));
        }
    }
    let mut rows: Vec<_> = judged
        .iter()
        .map(|note| {
            let frame = times.partition_point(|&t| t < note.time_ms);
            let just = setup.is_some()
                && master.live.just_types.contains(&note.judgement_type)
                && windows.iter().any(|&(a, b)| a <= frame && frame < b);
            (frame, *note, if just { 6 } else { 5 })
        })
        .collect();
    let mut grade = vec![5; rows.len()];
    choose(
        rows.len(),
        round(rows.len() as f64 * accuracy.great) as i64,
        |i| grade[i] = 4,
    );
    let eligible: Vec<_> = rows
        .iter()
        .enumerate()
        .filter(|(i, row)| grade[*i] != 4 && row.2 == 6)
        .map(|(i, _)| i)
        .collect();
    choose(
        eligible.len(),
        round(eligible.len() as f64 * accuracy.just) as i64,
        |i| grade[eligible[i]] = 6,
    );
    if accuracy.miss_every > 0 {
        let every = accuracy.miss_every as usize;
        for i in ((every - 1)..rows.len()).step_by(every) {
            grade[i] = 1;
        }
    }
    if let Some(setup) = setup {
        project_combo_openings(master, setup, &times, &mut rows);
    }
    let mut out: Vec<_> = times
        .iter()
        .map(|&time| Frame {
            time_ms: time,
            judged: Vec::new(),
        })
        .collect();
    for (i, &(frame, n, _)) in rows.iter().enumerate() {
        out[frame].judged.push(JudgedNote {
            note_id: n.id,
            judgement: grade[i],
            time_ms: n.time_ms,
        });
    }
    Ok(Play {
        frames: out,
        dt: vec![f(1.0 / 60.0); times.len()],
        just_eligible_ids: eligible.into_iter().map(|i| rows[i].1.id).collect(),
    })
}
