//! Resolve the CP conversion once before distributing a reward search.
use super::Engine;
use crate::{
    master, progress,
    reward_plan::{Fraction, Metric, Plan, Term},
};
use serde_json::{json, Value};

impl Engine {
    pub(super) fn resolve_reward_plan(
        &mut self,
        input: &Value,
        started: f64,
        now: &dyn Fn() -> f64,
    ) -> Result<Value, String> {
        let request = &input["request"];
        let mut plan = Plan::parse(&request["goal"]["rewardPlan"])?;
        if let Some(auto) = plan.auto_cp.take() {
            let mut calibration = input.clone();
            let cal = &mut calibration["request"];
            for key in [
                "resolveRewardPlan",
                "shard",
                "floors",
                "cutoff",
                "rewardCutoff",
                "rewardPhase",
                "completed",
                "seeds",
                "seedOnly",
                "seeded",
            ] {
                cal.as_object_mut().unwrap().remove(key);
            }
            cal["k"] = json!(1);
            cal["goal"]["songs"] = json!(auto.songs);
            cal["goal"]["route"] = json!("challenge");
            cal["goal"]["consumption"] = json!(auto.consumption);
            cal["goal"]["ranking"] = json!("primary");
            cal["goal"]["gekiso"] = Value::Null;
            cal["goal"]["cpExchange"] = Value::Null;
            let mut challenge_plan = plan.clone();
            challenge_plan.levels.truncate(1);
            cal["goal"]["rewardPlan"] = json!(challenge_plan);
            let response =
                progress::with_callback(None, || self.run_inner(&calibration, started, now))?;
            if master::rows(&response["results"])
                .iter()
                .any(|row| row["proven"] != true)
                || response["overall"][0].is_null()
            {
                let songs = master::rows(&request["goal"]["songs"]);
                let songs = if songs.is_empty() {
                    vec![&Value::Null]
                } else {
                    songs
                };
                return Ok(
                    json!({"results":songs.iter().map(|song|json!({"song":song,"hits":[],"proven":false,"bound":null,"stats":crate::search::Stats::default()})).collect::<Vec<_>>(),
                    "overall":[],"unknownCards":response["unknownCards"],"elapsedMs":now()-started,"stopReason":response["stopReason"]}),
                );
            }
            let best = Fraction::from_json(&response["overall"][0]["rewardRanking"][0])
                .ok_or("reward-plan-calibration")?;
            let weight = best
                .divided(auto.consumption as u32)
                .rounded()
                .filter(|&value| value <= 9_007_199_254_740_991)
                .ok_or("reward-plan-rate")?;
            if weight > 0 {
                plan.levels[0].push(Term {
                    metric: Metric::ChallengePoints,
                    weight,
                });
            }
        }
        if request["resolveRewardPlan"] == true {
            return Ok(
                json!({"results":[],"overall":[],"unknownCards":[],"elapsedMs":now()-started,"resolvedRewardPlan":plan}),
            );
        }
        let mut resolved = input.clone();
        resolved["request"]["goal"]["rewardPlan"] = json!(plan);
        let mut response = self.run_inner(&resolved, started, now)?;
        response["resolvedRewardPlan"] = json!(plan);
        Ok(response)
    }
}
