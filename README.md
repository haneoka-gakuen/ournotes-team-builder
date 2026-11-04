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

## Event reward settlement

Normal lives and normal skips earn CP from the rank's CP value and the Live Boost event-point multiplier. Challenge lives and challenge skips consume CP and earn none. Boost multipliers come from the selected release's matching consumption row; an unboosted normal live uses 1×.

PT follows `CalculateEventPoint`: add 10000 to the bonus basis points, multiply the base rank points and event-point multiplier with signed Int32 wrapping, then divide by 10000 toward zero. Item rewards follow `ApplyEventBonus`: multiply the signed Int32 inputs in signed Int64, divide toward zero and cap each reward row at Int32 maximum. Each item's selection probability is applied after that row settles. Search bounds enumerate reachable bonus totals when native wrapping breaks monotonicity.

An event goal can set `rewardPlan: {unit: 1, criterion: "mean", levels: [[{metric: "points", weight: 1}]]}`. Each outcome settles before averaging; fractions and ordered tie levels are compared exactly. `criterion: "max"` selects the lexicographically greatest reward tuple among the observed outcomes, including all 120 skill orders. It uses score ceilings and reachable grade payouts for pruning; mean-reward bounds never exclude a maximum-reward candidate. Unlisted rewards do not participate in comparison.

Gekiso event goals accept `rewardRoom: {mode: "battle", players: 5, otherAverageScore: 10000000}` or `{mode: "solo"}`. Battle room thresholds use `trunc(sqrt(5 / players) * battleRequiredScore * players)`. An explicit other-player average is subtracted for the other `players - 1` players; otherwise everyone is assumed to score equally. Solo Mission uses the personal score thresholds. Thresholds are projected once per request and shared by settlement and search bounds; the default five-player equal-score room retains the existing chart without copying it. Fixed-team evaluation accepts the same option under `request.event.rewardRoom`.

LUCK uses the requested seeds 0 through `luckSamples - 1`. A completed search proves the optimum for those samples and orders. The response retains its sample count and `sampled` flag; it does not turn the finite sample into a full-distribution expectation.

## Cumulative judgement timing

Ordinary score requests can set `goal.play.cumulativeTiming` to `"best"` or `"worst"` with `criterion: "max"`. Evaluation uses the same field on `request.play`. Each of the 120 skill orders receives its own timing maximum or minimum, then the best order determines the team's score. Omitting the field preserves nominal scoring.

`play.frameRate` selects 30, 60, 90 or 120 updates per second, with 60 as the default. These modes optimize cumulative-counter delivery with the existing base-judgement pattern, chart-time score ledger, ordinary combo, conversion windows and member-skill schedule. Updates are evenly spaced from music time 0. Canonical charts retain native input geometry and line ownership. The release document supplies `liveTools.judgementAreaOffsets` from the same release's options resource.

The activation frame counts its complete judgement batch and records its initial factor at the skill event's chart time. Later steps use their delivery frame's time. Expiry removes the factor at the event's chart time plus its duration, even when the next update detects expiry later. [Native timing](docs/timing.md) records the original client paths behind these boundaries.

Whole-frame input-unit replay applies native touch selection, synchronous priority, asynchronous delivery and press-enter claims. Taps require a press enter. Early Trace and normal slide-tail delivery require a release; held late delivery writes the nominal judgement time. Flicks retain the actual flick event's time and begin position, including reserved delivery on a later frame. Line completion can force an unjudged head to Miss. Combo and connection notes follow the Trace path. Unscored guide and hidden nodes produce Pass, rather than independent cumulative inputs. Critical and Trace judgement types accept only their native grade sets.

Monotone delivery dominance must hold for every affected counter. Equivalent choices prefer nominal timing; conflicting windows remain enumerated. A selected optimum must pass input replay. Necessary local input conflicts partition the remaining domains; bounded caches and search queues return an error when their budget is exhausted. Numeric card dominance based on the nominal model is disabled in these modes.

An ordinary `timeline` request with a `timing` context returns `worst` and `optimal` alongside nominal `score` and the input-checked guide's `best`. The guide advances one frame rounded up to whole milliseconds: 34, 17, 12 or 9 ms for the selected frame rate. Timeline bounds evaluate only the selected order's equivalence class; full-team evaluation still returns all 120 orders. Each guide step's `noteIndices` addresses the canonical judged-note stream; unchanged co-timed notes remain outside that guide step.

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
