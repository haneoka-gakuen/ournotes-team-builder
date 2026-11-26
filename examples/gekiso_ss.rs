//! Build the five-player SS catalogue against one release and its chart files.
use haneoka_team_engine::{api::Engine, runtime_chart};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs,
    io::BufReader,
    path::{Component, Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    song_id: i32,
    difficulty: i32,
    title: String,
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Catalog {
    identity: Value,
    as_of: Option<String>,
    charts: Vec<Entry>,
    missing: Vec<Value>,
}

fn compute(engine: &mut Engine, folder: &Path, entry: &Entry) -> Result<Value, String> {
    if entry
        .path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("expected a chart file relative to the catalogue".into());
    }
    let bytes = fs::read(folder.join(&entry.path)).map_err(|e| e.to_string())?;
    engine.insert_chart(
        entry.song_id,
        entry.difficulty,
        runtime_chart::from_bytes(&bytes)?,
    )?;
    let mut row = engine.room_ss_reference(&json!({
        "song": {"songId":entry.song_id, "difficulty":entry.difficulty}
    }))?;
    row["title"] = json!(entry.title);
    Ok(row)
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let usage = "usage: gekiso_ss <master.json> <catalog.json> [--html <output.html>]";
    let data: Value =
        serde_json::from_reader(BufReader::new(fs::File::open(args.next().ok_or(usage)?)?))?;
    let path = PathBuf::from(args.next().ok_or(usage)?);
    let html = match args.next() {
        Some(flag) if flag == "--html" => Some(args.next().ok_or(usage)?),
        Some(_) => return Err(usage.into()),
        None => None,
    };
    if args.next().is_some() {
        return Err(usage.into());
    }
    let catalog: Catalog = serde_json::from_reader(BufReader::new(fs::File::open(&path)?))?;
    if data["identity"] != catalog.identity {
        return Err("release and chart catalogue identity differ".into());
    }
    let folder = path.parent().unwrap_or(Path::new("."));
    let mut engine = Engine::compile(&data)?;
    let mut charts = Vec::new();
    let mut errors = Vec::new();
    for entry in &catalog.charts {
        match compute(&mut engine, folder, entry) {
            Ok(row) => charts.push(row),
            Err(reason) => errors.push(json!({"songId":entry.song_id, "difficulty":entry.difficulty,"title":entry.title,"reason":reason})),
        }
    }
    let output = json!({"identity":catalog.identity,"asOf":catalog.as_of,"charts":charts,"missing":catalog.missing,"errors":errors});
    if let Some(html) = html {
        let document = include_str!("gekiso_room/ss_catalog.html").replace(
            "<!--CATALOG_JSON-->",
            &serde_json::to_string(&output)?.replace('<', "\\u003c"),
        );
        fs::write(html, document)?;
    }
    serde_json::to_writer(std::io::stdout().lock(), &output)?;
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            std::process::ExitCode::FAILURE
        }
    }
}
