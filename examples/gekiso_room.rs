//! Native demonstrator for exact two-player rooms on one fixed play scenario.
#[path = "gekiso_room/oracle.rs"]
mod oracle;
#[path = "gekiso_room/rank_feedback.rs"]
mod rank_feedback;

use haneoka_team_engine::{
    api::room_search::{self, Candidate},
    full::play::Accuracy,
    gekiso::{room, Chart},
    master::{self, Master},
    power, runtime_chart,
};
use oracle::Histories;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Song {
    song_id: i32,
    difficulty: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Input {
    data: Value,
    chart: Value,
    song: Song,
    players: [Value; 2],
    accuracy: Accuracy,
    seeds: [i32; 2],
    order_indices: [usize; 2],
    teams: Option<[[Value; 3]; 2]>,
}

impl Input {
    fn chart(&self, master: &Master) -> Result<Chart, String> {
        for (seat, player) in self.players.iter().enumerate() {
            if let Some(allowed) = player.get("allowedDifficulties") {
                let allowed: Vec<i32> =
                    serde_json::from_value(allowed.clone()).map_err(|e| e.to_string())?;
                if !allowed.contains(&self.song.difficulty) {
                    return Err(format!(
                        "seat {} cannot play the requested difficulty",
                        seat + 1
                    ));
                }
            }
            if let Some(accuracy) = player.get("accuracy") {
                let own: Accuracy =
                    serde_json::from_value(accuracy.clone()).map_err(|e| e.to_string())?;
                if (own.just, own.great, own.miss_every)
                    != (
                        self.accuracy.just,
                        self.accuracy.great,
                        self.accuracy.miss_every,
                    )
                {
                    return Err(format!(
                        "seat {} requests different judgement conditions",
                        seat + 1
                    ));
                }
            }
        }
        if ![self.accuracy.just, self.accuracy.great]
            .iter()
            .all(|v| (0.0..=1.0).contains(v))
            || self.accuracy.miss_every < 0
        {
            return Err("invalid room accuracy".into());
        }
        let song = master.songs.get(&self.song.song_id).ok_or("unknown song")?;
        let level = song
            .difficulties
            .iter()
            .find(|d| d.difficulty == self.song.difficulty)
            .ok_or("unknown song difficulty")?
            .play_level;
        let source = runtime_chart::convert(&self.chart)?;
        let mut chart = Chart::new(
            master,
            &source,
            level,
            song.missions.clone(),
            self.accuracy,
            vec![1; 3],
        )?;
        chart.music_type = power::music_view(master, song.id, None)?.music_type;
        Ok(chart)
    }
}

fn verify(
    input: &Input,
    master: &Master,
    chart: &Chart,
    boxes: [&[Candidate]; 2],
    solution: &room_search::Solution,
) -> Result<Value, String> {
    let histories = |seat: usize| {
        boxes[seat]
            .iter()
            .map(|team| {
                Histories::build(
                    master,
                    chart,
                    team,
                    input.order_indices[seat],
                    input.seeds[seat],
                )
            })
            .collect::<Result<Vec<_>, _>>()
    };
    let [a, b] = [histories(0)?, histories(1)?];
    let mut maximum = i64::MIN;
    let mut count = 0usize;
    for left in &a {
        for right in &b {
            let row = oracle::reconcile(left, right, &chart.setup.missions)?;
            maximum = maximum.max(row.total);
            count += 1;
        }
    }
    let [i, j] = solution.indices;
    let winner = oracle::reconcile(&a[i], &b[j], &chart.setup.missions)?;
    if count != solution.evaluated
        || solution.replay.total != maximum
        || json!(winner) != json!(solution.replay)
    {
        return Err("exhaustive single-seat histories disagree with room solution".into());
    }
    let mut verified = json!({"pairs":count, "maximum":maximum});
    let histories = [&a, &b];
    verified["candidates"] = json!([0, 1].map(|s| boxes[s]
        .iter()
        .zip(histories[s])
        .map(|(team, h)| {
            json!({"team":team.team,"power":team.power,"firstPlaceScore":h.first_place_score()})
        })
        .collect::<Vec<_>>()));
    if let Some(teams) = &input.teams {
        let mut indices = [[0usize; 3]; 2];
        for s in 0..2 {
            for (k, team) in teams[s].iter().enumerate() {
                let index = boxes[s]
                    .iter()
                    .position(|c| {
                        c.team["members"] == team["members"]
                            && c.team["snaps"] == team["snaps"]
                            && team.get("leader").is_none_or(|l| l == &c.team["leader"])
                    })
                    .ok_or_else(|| format!("illegal matrix team {} on seat {}", k + 1, s + 1))?;
                if indices[s][..k].contains(&index) {
                    return Err("duplicate matrix team".into());
                }
                indices[s][k] = index;
            }
        }
        let mut matrix = Vec::new();
        for (row, &i) in indices[0].iter().enumerate() {
            for (col, &j) in indices[1].iter().enumerate() {
                let direct = room::replay(
                    master,
                    chart,
                    [
                        boxes[0][i].seat(input.order_indices[0], input.seeds[0]),
                        boxes[1][j].seat(input.order_indices[1], input.seeds[1]),
                    ],
                )?;
                let oracle = oracle::reconcile(&a[i], &b[j], &chart.setup.missions)?;
                if json!(direct) != json!(oracle) {
                    return Err(format!("matrix mismatch at {},{}", row + 1, col + 1));
                }
                matrix.push(json!({"pair":[row+1,col+1],"replay":direct}));
            }
        }
        verified["matrix"] = json!(matrix);
        verified["matrixIndividualScores"] =
            json!([0, 1].map(|s| indices[s].map(|i| histories[s][i].first_place_score())));
    }
    Ok(verified)
}

fn run() -> Result<Value, Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: gekiso_room <input.json> [--verify | --check-rank-skill]")?;
    let mode = args.next();
    if !matches!(mode.as_deref(), None | Some("--verify" | "--check-rank-skill")) {
        return Err("unknown option".into());
    }
    if args.next().is_some() {
        return Err("unexpected arguments".into());
    }
    let input: Input =
        serde_json::from_reader(std::io::BufReader::new(std::fs::File::open(path)?))?;
    let master = master::compile(&input.data)?;
    let chart = input.chart(&master)?;
    if mode.as_deref() == Some("--check-rank-skill") {
        return Ok(rank_feedback::check(&master, &chart)?);
    }
    let seats = |s: usize| room_search::candidates(&master, input.song.song_id, &input.players[s]);
    let [a, b] = [seats(0)?, seats(1)?];
    let boxes = [&a[..], &b[..]];
    let solution = room_search::solve(&master, &chart, boxes, input.order_indices, input.seeds)?;
    let teams = [0,1].map(|s| {
        let candidate = &boxes[s][solution.indices[s]];
        json!({"team": candidate.team, "power": candidate.power, "score": solution.replay.scores[s]})
    });
    let mut output = json!({"release":master.release_id, "songId":input.song.song_id, "difficulty":input.song.difficulty,
        "accuracy":input.accuracy, "orderIndices":input.order_indices, "seeds":input.seeds,
        "candidateCounts":[a.len(),b.len()], "pairsEvaluated":solution.evaluated,
        "teams":teams, "room":solution.replay});
    output["ss"] = match haneoka_team_engine::gekiso::ss_reference::ss_room_threshold(
        &master, &master.songs[&input.song.song_id], 2,
    ) {
        Ok(threshold) => json!({"roomThreshold":threshold, "achieved":solution.replay.total >= threshold,
            "pointsNeeded":(threshold - solution.replay.total).max(0)}),
        Err(reason) => json!({"unavailable":reason}),
    };
    if mode.as_deref() == Some("--verify") {
        output["verification"] = verify(&input, &master, &chart, boxes, &solution)?;
    }
    Ok(output)
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(output) => {
            println!("{output:#}");
            std::process::ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err}");
            std::process::ExitCode::FAILURE
        }
    }
}
