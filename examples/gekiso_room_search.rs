//! Drive the same cooperative room session operations used by a browser worker.
use haneoka_team_engine::session;
use serde_json::{json, Value};
use std::{env, fs::File, io::BufReader, process::ExitCode};

fn drive(request: &Value, budget: &Value, steps: u64) -> Result<Value, String> {
    let mut current = session::execute(request)?;
    let draws = current.as_object_mut().unwrap().remove("sampleDraws");
    let mut step = budget.clone();
    step["op"] = json!("roomSearchStep");
    step["handle"] = request["handle"].clone();
    step["searchId"] = current["searchId"].clone();
    let mut count = 1;
    while current["complete"] != true && count < steps && current["stopReason"] != "cancelled" {
        current = session::execute(&step)?;
        count += 1;
    }
    current["steps"] = json!(count);
    if let Some(draws) = draws {
        current["sampleDraws"] = draws;
    }
    Ok(current)
}

fn run() -> Result<Value, Box<dyn std::error::Error>> {
    let usage = "usage: gekiso_room_search <input.json> [--budget-ms N] [--max-work N] [--steps N] [--validation-samples N]";
    let mut args = env::args().skip(1);
    let file = File::open(args.next().ok_or(usage)?)?;
    let mut budget = json!({"budgetMs":16});
    let mut steps = u64::MAX;
    let mut validation = None;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(usage)?;
        match flag.as_str() {
            "--budget-ms" => {
                let ms: f64 = value.parse()?;
                if !ms.is_finite() || ms < 0.0 {
                    return Err("expected finite nonnegative milliseconds".into());
                }
                budget["budgetMs"] = json!(ms);
            }
            "--max-work" => budget["maxWork"] = json!(value.parse::<u64>()?),
            "--steps" => steps = value.parse()?,
            "--validation-samples" => {
                let n: usize = value.parse()?;
                if !(1..=4096).contains(&n) {
                    return Err("validation sample count must be 1 to 4096".into());
                }
                validation = Some(n);
            }
            _ => return Err(usage.into()),
        }
    }
    if steps == 0 || budget["maxWork"] == 0 && steps == u64::MAX {
        return Err("specify a positive step count for an inspection-only run".into());
    }
    let mut input: Value = serde_json::from_reader(BufReader::new(file))?;
    let handle =
        session::execute(&json!({"op":"create","data":input["data"].take()}))?["handle"].clone();
    let result = (|| -> Result<Value, String> {
        if let Some(chart) = input.get_mut("chart").map(Value::take) {
            session::execute(
                &json!({"op":"chart","handle":handle,"song":input["song"],"chart":chart}),
            )?;
        }
        if let Some(charts) = input.get("charts") {
            for entry in charts.as_array().ok_or("charts must be an array")? {
                let mut load = entry.clone();
                let fields = load.as_object_mut().ok_or("expected a chart entry")?;
                fields.insert("op".into(), json!("chart"));
                fields.insert("handle".into(), handle.clone());
                session::execute(&load)?;
            }
        }
        let mut request = budget.clone();
        for field in [
            "song",
            "players",
            "accuracy",
            "orderIndices",
            "seeds",
            "sampling",
            "objective",
            "includeSamples",
        ] {
            if let Some(value) = input.get_mut(field) {
                request[field] = value.take();
            }
        }
        if validation.is_some()
            && (request["sampling"].is_null() || request["sampling"]["stream"] == "validation")
        {
            return Err("independent validation requires a sampled search".into());
        }
        request["op"] = json!("roomSearchStart");
        request["handle"] = handle.clone();
        let mut current = drive(&request, &budget, steps)?;
        if let Some(count) = validation {
            if let Some(teams) = current["teams"].as_array() {
                for (player, selected) in request["players"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .zip(teams)
                {
                    player["lockedTeam"] = selected["team"].clone();
                }
                request["sampling"]["count"] = json!(count);
                request["sampling"]["stream"] = json!("validation");
                let validated = drive(&request, &budget, u64::MAX)?;
                current["validation"] = validated;
            } else {
                current["validation"] =
                    json!({"unavailable":"no complete candidate in the search budget"});
            }
        }
        Ok(current)
    })();
    session::execute(&json!({"op":"dispose","handle":handle}))?;
    Ok(result?)
}

fn main() -> ExitCode {
    match run() {
        Ok(value) => {
            println!("{value:#}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
