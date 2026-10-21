//! Synchronous compute-thread notifications; the host forwards them to its UI.
use crate::{
    chart::Chart,
    master::{self, ScoreRank},
    power::{Member, Snap},
    search::Hit,
};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    ffi::{c_char, CString},
};
pub type Callback = extern "C" fn(*const c_char);
type HitIdentity = (Vec<usize>, Vec<i32>, u64, u64);
#[derive(Default)]
struct State {
    callback: Option<Callback>,
    members: Vec<String>,
    snaps: Vec<String>,
    song: Value,
    goal: Value,
    ranks: Vec<ScoreRank>,
    battle: bool,
    last: f64,
    last_hits: Vec<HitIdentity>,
}
thread_local! {static STATE:RefCell<State>=RefCell::new(State::default());}
pub fn with_callback<T>(callback: Option<Callback>, run: impl FnOnce() -> T) -> T {
    struct Restore(State);
    impl Drop for Restore {
        fn drop(&mut self) {
            STATE.with(|s| {
                s.replace(std::mem::take(&mut self.0));
            });
        }
    }
    let previous = STATE.with(|s| {
        s.replace(State {
            callback,
            last: f64::NEG_INFINITY,
            ..State::default()
        })
    });
    let _restore = Restore(previous);
    run()
}
pub fn keys(members: &[Member], snaps: &[Snap]) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.callback.is_some() {
            s.members = members.iter().map(|m| m.key.clone()).collect();
            s.snaps = snaps.iter().map(|s| s.key.clone()).collect();
        }
    })
}
pub fn song(song: &Value) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.song = song.clone();
        s.last = f64::NEG_INFINITY;
        s.last_hits.clear();
    })
}
pub fn mode(goal: &Value) {
    STATE.with(|s| s.borrow_mut().goal = goal.clone());
}
pub fn chart(chart: Option<&Chart>, battle: bool) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.ranks = chart.map(|c| c.ranks.clone()).unwrap_or_default();
        s.battle = battle;
    });
}
fn hit_json(state: &State, hit: &Hit) -> Value {
    let mut value = json!({"song":state.song,"members":hit.team.members.iter().map(|&i|&state.members[i]).collect::<Vec<_>>(),"snaps":hit.team.snaps.iter().map(|&j|if j<0{None}else{Some(&state.snaps[j as usize])}).collect::<Vec<_>>(),"power":hit.power,"slotPowers":hit.slot_powers,"key":hit.key});
    let detail = &hit.detail;
    if !detail["comboBonus"].is_null() {
        value["comboBonus"] = detail["comboBonus"].clone();
    }
    let scores = &detail["scores"];
    if !scores.is_null() {
        let values: Vec<_> = master::rows(&scores["scores"])
            .into_iter()
            .map(|v| master::number(v, 0.0))
            .collect();
        if values.len() == 120 {
            let mut sorted = values.clone();
            sorted.sort_by(f64::total_cmp);
            let mut ranks = std::collections::BTreeMap::<i32, i32>::new();
            for &score in &values {
                let rank = state
                    .ranks
                    .iter()
                    .filter(|r| {
                        if state.battle {
                            r.battle_required > 0 && r.battle_required as f64 <= score
                        } else {
                            r.required as f64 <= score
                        }
                    })
                    .map(|r| r.rank)
                    .max()
                    .unwrap_or(2)
                    .max(2);
                *ranks.entry(rank).or_default() += 1;
            }
            let order_keys = |index: usize| {
                crate::live::orders()[index]
                    .map(|slot| state.members[hit.team.members[slot]].clone())
            };
            value["score"] = json!({"mean":scores["mean"],"min":scores["min"],"max":scores["max"],"median":(sorted[59]+sorted[60])/2.0,"orders":values.iter().map(|&v|crate::num::round(v)).collect::<Vec<_>>(),"bestOrder":order_keys(master::int(&scores["maxOrder"]) as usize),"worstOrder":order_keys(master::int(&scores["minOrder"]) as usize),"ranks":ranks});
        }
    }
    if !value["score"].is_null() {
        if let Some(ranges) = scores.get("rangeScores") {
            value["score"]["rangeScores"] = ranges.clone();
        }
    }
    let exchange = &state.goal["cpExchange"];
    let denominator = master::number(&exchange["denominator"], 1.0);
    let numerator = master::number(&exchange["numerator"], 0.0);
    if !detail["settlement"].is_null() {
        let d = &detail["settlement"];
        let count = master::number(&d["observations"], 1.0);
        let cp = master::number(&d["cpSum"], 0.0) / count;
        value["event"] = json!({"mean":master::number(&d["mean"],0.0)/denominator,"bonusPercent":master::number(&detail["bonus"],0.0)/100.0,"challengePoints":cp,"perPlayMin":d["directMin"],"perPlayMax":d["directMax"],"rewardSum":d["rewardSum"],"comparisonSum":d["comparisonSum"],"orders":d["observations"],"directMean":master::number(&d["rewardSum"],0.0)/count,"otherMean":d["otherMean"],"convertedMean":cp*numerator/denominator});
        value["gekiso"] = json!({"just":detail["just"],"luckSamples":detail["luckSamples"],"sampled":detail["sampled"],"rank":1});
    } else if !detail["bonus"].is_null() {
        let cp = master::number(&detail["challengePoints"], 0.0);
        let mean = master::number(&detail["mean"], 0.0) / denominator;
        let plan = state.goal["kind"] == "plan";
        let low = master::rows(&detail["perOrder"])
            .into_iter()
            .map(|v| master::number(v, 0.0))
            .reduce(f64::min)
            .unwrap_or(mean);
        let high = master::rows(&detail["perOrder"])
            .into_iter()
            .map(|v| master::number(v, 0.0))
            .reduce(f64::max)
            .unwrap_or(mean);
        value["event"] = json!({"mean":if plan{mean-master::number(&state.goal["planWeight"],0.0)*cp}else{mean},"bonusPercent":master::number(&detail["bonus"],0.0)/100.0,"challengePoints":cp,"perPlayMin":if plan{0.0}else{master::number(&detail["directMin"],low)},"perPlayMax":if plan{0.0}else{master::number(&detail["directMax"],high)},"directMean":detail["directMean"],"otherMean":detail["otherMean"]});
        if state.goal["ranking"] == "pt-only" {
            value["event"]["rewardSum"] = detail["rewardSum"].clone();
            value["event"]["comparisonSum"] = detail["comparisonSum"].clone();
            value["event"]["orders"] = json!(120);
            value["event"]["directMean"] = json!(master::number(&detail["rewardSum"], 0.0) / 120.0);
            value["event"]["convertedMean"] = json!(cp * numerator / denominator);
        }
    }
    if !detail["potential"].is_null() {
        value["potential"] = json!({"value":detail["potential"],"area":detail["area"]});
    }
    crate::api::normalize_hit(&mut value);
    value
}
pub fn emit(hits: &[Hit]) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(callback) = state.callback else {
            return;
        };
        let now = crate::clock::milliseconds();
        if hits.is_empty() || now - state.last < 250.0 {
            return;
        }
        let identity: Vec<_> = hits
            .iter()
            .map(|hit| {
                (
                    hit.team.members.clone(),
                    hit.team.snaps.clone(),
                    hit.key.to_bits(),
                    hit.power.to_bits(),
                )
            })
            .collect();
        if identity == state.last_hits {
            return;
        }
        state.last = now;
        state.last_hits = identity;
        if hits.iter().any(|h| {
            h.team.members.iter().any(|&i| i >= state.members.len())
                || h.team.snaps.iter().any(|&j| j >= state.snaps.len() as i32)
        }) {
            return;
        }
        let values: Vec<_> = hits.iter().map(|hit| hit_json(&state, hit)).collect();
        let message = CString::new(json!({"song":state.song,"hits":values}).to_string())
            .expect("JSON has no NUL");
        callback(message.as_ptr());
    })
}
/// The callback of the current call, for messages sent outside the hit stream.
pub fn callback() -> Option<Callback> {
    STATE.with(|s| s.borrow().callback)
}
pub fn send(callback: Option<Callback>, value: &Value) {
    if let Some(callback) = callback {
        let message = CString::new(value.to_string()).expect("JSON has no NUL");
        callback(message.as_ptr());
    }
}
