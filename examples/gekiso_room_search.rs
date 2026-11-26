//! Drive the same cooperative room session operations used by a browser worker.
use haneoka_team_engine::session;
use serde_json::{json, Value};
use std::{env, fs::File, io::BufReader, process::ExitCode};

fn run() -> Result<Value, Box<dyn std::error::Error>> {
    let usage = "usage: gekiso_room_search <input.json> [--budget-ms N] [--max-work N] [--steps N]";
    let mut args = env::args().skip(1);
    let file = File::open(args.next().ok_or(usage)?)?;
    let mut budget = json!({"budgetMs":16});
    let mut steps = u64::MAX;
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
        session::execute(
            &json!({"op":"chart","handle":handle,"song":input["song"],"chart":input["chart"].take()}),
        )?;
        let mut request = budget.clone();
        for field in ["song", "players", "accuracy", "orderIndices", "seeds"] {
            if let Some(value) = input.get_mut(field) {
                request[field] = value.take();
            }
        }
        request["op"] = json!("roomSearchStart");
        request["handle"] = handle.clone();
        let mut current = session::execute(&request)?;
        let mut step = budget;
        step["op"] = json!("roomSearchStep");
        step["handle"] = handle.clone();
        step["searchId"] = current["searchId"].clone();
        let mut count = 1;
        while current["complete"] != true && count < steps && current["stopReason"] != "cancelled" {
            current = session::execute(&step)?;
            count += 1;
        }
        current["steps"] = json!(count);
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
