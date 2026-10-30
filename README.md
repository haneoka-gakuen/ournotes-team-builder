# ournotes-team-builder

The team-building engine for BanG Dream! Our Notes. Given a card inventory and a song, it finds the best team, and proves that no legal team scores higher.

It powers the team builder on [Haneoka](https://haneoka.org), as WebAssembly in the browser and as native code in the Haneoka app, so both give the same results.

## What it does

- **Finds teams** for score, team power, event rewards and Gekiso, on one song or averaged across several.
- **Proves the result.** A search that finishes has checked every legal team. One stopped early returns its best teams, marked as unproven.
- **Evaluates a fixed team** with a full replay of the chart, including skill order and LUCK outcomes.
- **Runs anywhere.** Native and WebAssembly builds share the same algorithms and numeric behaviour.

## Install

```toml
[dependencies]
ournotes-team-builder = { git = "https://github.com/haneoka-gakuen/ournotes-team-builder" }
```

The library is named `haneoka_team_engine`.

## Usage

Every call goes through `session::execute`, which takes and returns JSON. Create a session from a game data document, then run requests against it.

```rust
use haneoka_team_engine::session;
use serde_json::json;

let created = session::execute(&json!({ "op": "create", "data": game_data }))?;
let handle = created["handle"].clone();

let output = session::execute(&json!({
    "op": "run",
    "handle": handle,
    "request": request,
    "charts": charts,
}))?;
```

[`examples/bench.rs`](examples/bench.rs) is a complete program. Run it with a JSON file holding `data`, `request` and `charts`:

```sh
cargo run --release --example bench -- input.json
```

| Operation | Purpose |
| --- | --- |
| `create` | Load game data and return a session handle |
| `chart` | Cache a chart ahead of a run |
| `run` | Search for the best teams |
| `evaluate` | Score one fixed team |
| `timeline` | Show when a fixed team's skills activate |
| `qualify` | Check a request before running it |
| `growth` | Rank card upgrades by how much each one gains |
| `rates` | Report each song's scoring weights, rank thresholds and skill windows |

`cancel()` stops running searches. For other languages, the same engine is exposed as C functions that take and return JSON strings, such as `haneoka_context_json`.

## Build

```sh
cargo build --release
cargo test
```

The crate builds a Rust library, a C shared library and a static library. For the web:

```sh
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown
```

## Learn more

[Engine notes](docs/engine-notes.md) cover how the search works and what each request option guarantees.

## License

AGPL-3.0-only, with an additional attribution term under section 7(b): any interface that shows results computed by this engine, or by a work based on it, must display "Team-building algorithm from Haneoka — https://haneoka.org" with an explicit hyperlink to https://haneoka.org. See [NOTICE](NOTICE) and [LICENSE](LICENSE).
