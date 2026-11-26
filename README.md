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
| `roomSsReference` | Estimate five players' individual contributions to the multiplayer SS line on one chart |
| `roomSearchStart`, `roomSearchStep` | Optimize the mean room score or SS frequency for 2–5 owned teams with individual charts and judgement profiles |
| `evaluateAll` | Score one fixed team on each of several songs |
| `eventGrowthStart`, `eventGrowthStep`, `eventGrowthPage` | Infer character and band growth from recorded event decks |

`cancel()` stops running searches. For other languages, the same engine is exposed as C functions that take and return JSON strings, such as `haneoka_context_json`.

### Two-player Gekiso prototype

The native example reads two owned inventories and finds the pair with the highest combined final score on one chart. Each team's power, leader, equipped snapshots, and skills come from its own inventory and player bonuses. The example returns both formations, both final scores, and a breakdown of the three sections.

```sh
cargo run --release --locked --example gekiso_room -- examples/gekiso_room/fixture.json --verify
```

Both seats play one difficulty with a shared judgement profile. Each seat has a fixed ordinary skill order and random seed. The leader occupies the first slot. The remaining slots follow character ID order. Equal mission value, section score, and Perfect count give both players first place. The search checks every legal pair in the supplied small inventories. The maximum applies to that play scenario.

With `--verify`, the example also runs each team through the original single-player simulator with all eight possible first/second-place histories. It finds the unique pair of histories that matches both players' standings, checks the exhaustive maximum, and prints the nine sample pairs when the input supplies them. The [sample and its data sources](examples/gekiso_room/FIXTURE.md) describe the inventories and individual-score comparison.

The result also reports the two-player SS threshold, whether the pair reached it and the remaining points.

### Cooperative Gekiso rooms

Each player supplies their own inventory and account bonuses. Add a `lockedTeam` to keep that player's formation, or leave the seat free to choose its members, leader and snapshots. Every section ranks all players together. The result contains each formation, personal and combined scores, the three section standings and the points needed for SS at the actual room size.

```sh
python examples/gekiso_room/make_room.py target/room-five.json --seats 5
cargo run --release --locked --example gekiso_room_search -- target/room-five.json
```

The example drives the same reusable session operations as a web worker. Use `--steps 1 --budget-ms 16` for one time slice. An application can pause between slices and continue the same search by ID. Partial results keep the best room found so far. See [search conditions and proof](docs/engine.md#cooperative-room-search).

For the mixed-difficulty example, use the international release master and its chart catalogue. The three players in `mixed_room.json` have separate inventories and judgement settings. This example compares formations on 32 shared LUCK and skill-order draws, then tests the selected teams on a fresh validation stream. Set `objective` to `ssRate` to prioritize reaching the room SS threshold.

```sh
python examples/gekiso_room/make_sampled_room.py master.json catalog/catalog.json target/mixed-room.json
cargo run --release --locked --example gekiso_room_search -- target/mixed-room.json --validation-samples 1024
```

### Five-player SS reference

To browse every available song and difficulty from a saved team-builder release, build the chart catalogue and open the generated HTML file. The page shows the shared base score and five final scores. Changing one section placement swaps it with another player's placement in that section.

```sh
python examples/gekiso_room/catalog.py master.json catalog
cargo run --release --locked --example gekiso_ss -- master.json catalog/catalog.json --html room-ss.html > room-ss.json
```

The catalogue records the region, release, chart hashes, scheduled charts and acquisition errors. Pass `--as-of` with an ISO timestamp and UTC offset to reproduce a particular release cutoff. The [engine contract](docs/engine.md#five-player-ss-reference) also supports a selected team from one player's inventory.

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

- [Engine guarantees](docs/engine.md): what each request option guarantees, including event reward settlement and growth inference.
- [Timing](docs/timing.md): cumulative judgement timing options and the original client paths behind them.

## License

AGPL-3.0-only, with an additional attribution term under section 7(b): any interface that shows results computed by this engine, or by a work based on it, must display "Team-building algorithm from Haneoka — https://haneoka.org" with an explicit hyperlink to https://haneoka.org. See [NOTICE](NOTICE) and [LICENSE](LICENSE).
