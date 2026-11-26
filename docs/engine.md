# Engine guarantees

What each request guarantees. For usage, see the [README](../README.md).

## Search

A completed search has checked every legal team and proves its result. A search stopped early returns its best teams, marked as unproven.

Member, snapshot, leader and binding constraints apply to every candidate. An unknown required card or binding leaves no legal team, and manual evaluation rejects unknown snapshots.

Ordinary MISS searches prepare converter domains before recovery amounts. When even maximal recovery leaves every order dead before the second or third activation, recovery-specific marginal ceilings enter the corresponding slot rows. Their averaged base-score ceilings remain conservative; surviving domains retain the exact recovery partition and final scoring. Recovery bounds price only the current legal Snapshot choices. A card can dominate another with the same conversion/recovery program and matching score-effect windows only after a conservative power margin covers per-note rounding; cumulative programs remain equal.

## Activity growth inference

`eventGrowthStart` binds recorded song decks and release-pinned growth catalogues to an existing context. `eventGrowthStep` advances complete branch-and-bound search; `eventGrowthPage` resumes its retained frontier for another ten-row page. There is no total result cap or fixed-time completion shortcut. By default all observations use one unchanged growth state. With `preferences.allowGrowth: true`, the displayed domain is the globally minimal change cost and cost + 1. Any feasible incumbent supplies a safe upper bound; once the minimum is proved the cost window is exact.

The objective is `5 * sum(character rank spans) + sum(band instrument-total spans) + account-bonus span`, followed by fewer changed dimensions, distance to the expected growth, higher T.G.W and descending instrument ranges in band order. Expected-growth distance is the minimum across correlated equivalents of `5 * sum(abs(character level - expected level)) + sum(abs(instrument total - expected total))` on the selected record. These are integer change costs, not player-rank likelihoods. The search opens complete cost layers in ascending order. All T.G.W and optional overall-ranking contexts participate before a candidate can be certified.

Interval nodes carry admissible lexicographic bounds: the current cost layer, a lower bound on changed dimensions and expected-growth distance, the fixed T.G.W and upper bounds on instrument minima/maxima. A candidate enters the fixed prefix only when its actual key precedes every remaining interval bound. Equal semantic keys use deterministic certification order. Uncertified exact candidates can fill the active page while computation continues; a full certified page pauses with its frontier intact. Later pages cannot insert smaller-change rows into earlier pages. `complete` requires the entire permitted near-optimal domain to be exhausted.

A separate small-radius scout can supply exact provisional matches; its heuristic discovery order never certifies rows or terminates the proof frontier. Lossless normalized rank, account-bonus and instrument offset profiles partition every cost layer. Necessary per-axis difference relations filter and cache normalized histories before their Cartesian join; partial-history difference bounds skip entire impossible suffix products. Their lazy generators remain in the certifying frontier, so neither state generation nor pagination requires an arbitrary count limit. Differences between identical power functions constrain flat bonuses using cached exact min/max difference curves across every legal instrument level. Canonical whole-history account-bonus branches remove uniform-shift duplicates while retaining all absolute decompositions. Their grade-sum intervals also propagate through the flat domains. Fixed-instrument boxes use a lossless meet-in-the-middle join; large half domains stay in the complete interval tree. Per-equation and equation-difference domain propagation, including integer residue checks, precedes the join. Both ordinary power bounds and these difference bounds are conservative. The supported rules require nonnegative power coefficients; all emitted witnesses are independently checked through shared `team_power`. Caches contain budget-independent power/difference and rank-marginal facts and survive cost-layer changes. Conservative interval reachability under the change budget prunes impossible tiers and profiles before absolute-level search. `profiles.rs` owns normalized-history generation; `search.rs` owns integer bounds, the fixed-instrument join and witness verification.

An optional `total` deck adds an overall-ranking observation. The game response omits its source song, so distinct activity-song power contexts are OR branches. Missing/incomplete overall decks retain song-only inference. Overall witnesses have `challengeId: 0` and retain `contextChallengeId`. Repeated equations remain distinct records; sharing difference/power facts does not force their hidden growth states to be equal.

Every record must exactly reproduce its integer power. Character ranks differ by at most two, each band's instrument total by at most ten, and all-character totals share a realizable unseen-character envelope. Ranks of characters absent from a particular deck remain variables. Caps come from current mission EXP. The `preferences.characterLevels` and `instrumentLevels` maps supply per-character and per-band centers, defaulting to 18 and 75. Their radii default to 3 and 10; zero fixes that level. Every witness and correlated equivalent stays inside these intervals and the task caps. Character rank 10 and band-total 40 are hard floors. `preferences.vipBonus` optionally fixes an existing T.G.W bonus. Growth caps are calculated once per request and reused across overall-song contexts; interval feasibility is checked before offset-profile enumeration.

Uniform role-rank/account-bonus translations are grouped as correlated `equivalents`. Family identity includes every original record's effective bonuses, relative account bonus and instruments, so distinct historical patterns are retained. Outer ranges are marginal envelopes, not a cartesian product. Families are deduplicated on certification. `certified`, `costBound`, `examinedBoxes` and `prunedBoxes` expose proof progress and anchor-box work without adding UI columns.

## Event rewards

A `rewardPlan` maximises a nonnegative integer-weighted sum of PT, CP and badges, followed by up to two ordered tie levels.

For ordinary Live searches with a minimum rank, conservative score bounds can rule out a chart or classify a team's skill orders. When the bounds cross the rank threshold, the engine checks the exact orders. A streaming `run` with top-level `reportRankChecks: true` can also report `{ song, checkedTeams }`. Individual searches count checks per chart. A shared-team search reports all chart checks together with `song: null`. The running count includes rejected teams and repeated checks of the same formation. When a search returns, it also reports any checks since the previous update.

| Target | Result |
| --- | --- |
| Score target | The mean-score top K is proven first. If every team reaches the target in all 120 skill orders, it is also the target optimum |
| Finite Live Boost | Maximises total event points over the budget, including the CP distribution across skill orders and the expected challenge count |

Rewards settle with the client's integer rules before outcomes are compared:

- Normal lives and skips earn CP from the rank's CP value and the Live Boost multiplier. Challenge lives and skips consume CP and earn none. Boost multipliers come from the release's consumption row; an unboosted normal live uses 1×.
- PT follows `CalculateEventPoint`: signed Int32 arithmetic on `10000 + bonus`, rank points and the event-point multiplier, divided by 10000 toward zero.
- Item rewards follow `ApplyEventBonus`: Int64 products of Int32 inputs, divided toward zero and capped at Int32 maximum per row, before the row's selection probability applies.

`rewardPlan.criterion` is `mean` (settle each outcome, then average) or `max` (the greatest reward tuple over all observed outcomes and 120 skill orders). Rewards not listed in the plan do not take part in comparison.

`rewardPlan.tieBreak: "score"` compares average score and then total power after every configured reward level; `"power"` compares only power. These additional `rewardRanking` entries are exact fractions. Shared-song searches use their equal-weight means across charts. Omitting `tieBreak` retains reward-only ranking.

`rewardRoom` sets the Gekiso event room: `{mode: "battle", players, otherAverageScore}` uses `trunc(sqrt(5 / players) * battleRequiredScore * players)` minus the other players' scores, and `{mode: "solo"}` uses the personal thresholds. Fixed-team evaluation reads it from `request.event.rewardRoom`.

LUCK uses seeds 0 to `luckSamples - 1`. A completed search is optimal for those samples; the response keeps its sample count and `sampled` flag.

Gekiso's minimum COMBO BONUS is the sustained base multiplier after the first COMBO activation, at initial LIFE. Later thresholds affect scoring but do not satisfy the minimum.

## Gekiso

Deterministic results and manual evaluations replay all 120 skill orders. Mean searches subtract completed orders from their individual score ceilings and stop rejected candidates once the remaining ceiling cannot reach the incumbent. Accepted teams keep the complete distribution. Single-team improvements stream immediately. Sampled mean and maximum searches can share fully scored candidate floors while independent workers continue their proofs. Fixed-seed pure-LUCK leaves use their selected performers for the Rush ceiling before preparing a broader support-family bound. Lottery preparation reuses condition scheduling and skips inactive frames only with no damage, pending draws, timed effects or unsupported condition dependencies; note, skill, standby and range boundaries remain explicit. A LUCK sample count of 0 gives nominal expectations, and a positive count averages fixed seeds 0 to n-1.

For no-damage COMBO charts, GREAT retains judgement-specific score bounds and conditional COMBO traces. Ordinary GREAT-to-PERFECT pulses are bounded by their activation windows and conversion quotas. Stable LIFE projections retain these converters; unsupported programs use the full replay path. Final scores still replay all orders. COMBO levels and conditional branches within one search share the same immutable card preparation; conversion pulses and their per-note ordinary factor envelopes are prepared once per complete skill template. Shared charts combine fixed-formation coefficients before matching physical Snapshots and cache complete-formation mean ceilings.

Deferred ordinary recovery can share control traces when native prefix replay proves LIFE is already zero before the first activation and no active effect can restore it. Other controls and every score order remain in the replay. Shared-song member bounds combine each song's current roster power and skill ceilings before refining the remaining nominal LUCK programs.

`goal.arena` sets the league band, music type and three mission overrides for one call. Zero keeps the song's own value.

`sectionMetrics` evaluates a fixed team with the same `request` shape as `evaluateAll`. It returns `metrics`, with each song's three `missions`, total `samples` and three `distributions` of `{points, count}`. All 120 skill orders and seeds 0 through n-1 contribute; a zero or omitted seed count selects 32 samples per order. The count-only replay preserves ordinary conversion, recovery and Gekiso effects. Equivalent orders share replay only when their mission counts are identical. It does not compare opponents or settle total scores.

### Cooperative room search

Use `roomSearchStart` with a song and two to five players. Cache each played chart through `chart` or include the matching `charts` entries in the start request. Each seat has its own `members`, `snaps`, `player`, `unknownPolicy` and `constraints`. Its `difficulty` and `accuracy` override the song difficulty and room accuracy respectively. If supplied, that seat's `allowedDifficulties` must include the selected difficulty. A seat with no profile inherits the room default (otherwise all JUST where eligible, no GREAT or misses).

Without `sampling`, the room uses each seat's fixed `orderIndex` (0–119) and signed 32-bit LUCK `seed`, both defaulting to zero. Legacy arrays `orderIndices` and `seeds` must agree with any per-seat values.

The optional `lockedTeam` supplies five member instance keys, five snapshot keys or nulls, and an optional leader key. Its slot order is preserved with the leader moved to the first slot. Free seats enumerate every legal member set, choice of leader and assignment of distinct owned snapshots. For a free seat the nonleader slots follow character order. The engine checks ownership and constraints against each seat's own box. Once every seat has a legal candidate, it replays a first room, then tests more rooms during inventory enumeration. When a box offers more choices than five unbound snapshot slots and has alternative members, it also seeds the search with legal formations chosen for trained power on this song. Seeds are counted once and only affect which results can be found early.

Each seat runs to its own settlement frame for the same mission. The engine compares mission value, section score and PERFECT count in that order. Rank is one plus the number of seats strictly ahead. The model receives its award on its own frame, then continues on its own chart through the ending. Each section's `timeMs` is the latest settlement time. When frames differ, `seatTimesMs` contains each player's settlement time in seat order.

#### Average room score and SS frequency

Add these fields to compare all candidate formations on common draws.

```json
{"objective":"ssRate","sampling":{"count":32,"seed":20261131,"skillOrders":"uniform"},"includeSamples":true}
```

- `count` is 1–4096 and `seed` is an unsigned 32-bit sampling seed. Each seat gets its own signed LUCK seed per play for the native lottery. A counter-based generator with a SplitMix64 finalizer keys the draws by the sampling seed, seat seed, seat index, draw index, stream and random variable. Increasing the count preserves the prefix.
- `skillOrders:"fixed"` (default) uses each seat's specified order. `"uniform"` draws independently from all 120 orders per seat per play. Both modes use the same LUCK draw schedule.
- A seat may provide one `accuracy`, or, with sampling, 1–16 equally weighted `accuracyProfiles`. For example, `[{"just":0.9,"great":0.02},{"just":0.7,"great":0.06,"missEvery":90}]` models two performance levels. Profiles are sampled independently per seat and play. Repeated entries add weight. Within a profile, the engine spreads a GREAT quota over eligible notes, assigns JUST among the remaining eligible notes in JUST sections and applies periodic misses.
- `meanScore` (default) maximizes the sum of room scores across draws, then the number of SS hits. `ssRate` prioritizes SS hits, then the score sum. Both use stable candidate order for a final tie. Equality with the song's threshold for the room size counts as a hit. `ssRate` needs a known threshold.
- `room`, `teams[].score` and `ss.achieved` describe the first draw of the winning formation. `statistics` holds `sampleCount`, integer `scoreSum`, `meanTotal`, `minTotal`, `maxTotal`, per-seat `meanScores`, `ssHits` and `ssRate`. SS statistics are null when the threshold is unavailable. `seatConditions` records each seat's inputs. Shared `difficulty` or `accuracy` is null when there is no common value. `includeSamples:true` adds every draw's actual seat difficulty, accuracy, order and LUCK seed to the start response.
- Each formation must finish its entire sample set before it can replace the winner. `sampleProgress.roomReplays` counts completed room replays. `completedForCurrentCombination` counts completed draws for the pending formation. A completed search proves the chosen objective for these formations and draws.
- To estimate performance after selection, put the winning teams into the original seats' `lockedTeam` fields, retain the performance and order settings and set `sampling.stream:"validation"`. This stream uses fresh draws even with the original sampling seed. All seats must be locked. The result includes `statistics.ssRateWilson95`, a 95% Wilson interval for the modeled hit probability under independent draws. Keep validation outcomes out of team selection. Actual player performance and in-game frequencies can differ. Use a separate handle to preserve an unfinished original search.

```json
{"op":"roomSearchStep","handle":1,"searchId":1,"budgetMs":16,"maxWork":1000}
```

Start and step accept a soft time budget in milliseconds (default 16) and an optional maximum number of work units. One unit advances the inventory cursor, replays one room or one single-seat rank history, or handles a search-tree block. Setup, response serialization and a single unit can take longer than the budget. A zero time budget allows one unit. `maxWork: 0` inspects the current state. Stop issuing step calls to pause. Native cancellation can also interrupt a replay and leaves its unfinished unit available to retry. Continue with the returned `searchId` in the same live handle. Starting another room replaces the previous search on success. Repeating a successful step advances another slice. Disposing the handle drops its search.

| Result | Meaning |
| --- | --- |
| `teams`, `room`, `ss` | Best known legal formations, personal scores, section metrics, total and the room-sized SS line. The formations and room are null until a full room has been evaluated. |
| `phase`, `complete`, `stopReason` | Inventory enumeration, bound calculation, Cartesian search or completion. Only a completed search proves the maximum for the supplied scenario. |
| `candidateCounts`, `totalCombinations` | Counts discovered per seat and the size of their Cartesian product, which is known after enumeration. Large combination and work counters are decimal strings beyond JavaScript's safe integer range. |
| `evaluated`, `pruned`, `boundReplays` | Distinct formations scored over all draws, formations excluded by proof, and single-seat histories played for bounds. At completion the first two counts sum to the total number of rooms. |
| `upperBound` | Ceiling for the chosen objective, in room points for meanScore or in [0, 1] for ssRate. At completion it equals the winning objective. |

Multi-draw searches visit every legal combination and score every draw. Single-draw searches evaluate small products directly. For larger single-draw products the engine compares the cost of building bounds with the cost of replaying the remaining rooms. For a room of size `n`, a bounded candidate replays **all `n³` possible three-section rank histories**, including the effects of rank on later skills. Its greatest final score bounds its contribution with any partners. Summing per-seat ceilings bounds a block of rooms sharing a prefix of selected candidates. A block is skipped only when it cannot improve the incumbent under the deterministic score and enumeration-order tie break. Candidate storage and the resume cursor live in memory. A time-limited result on a large box can remain far from its upper bound.

### Five-player SS reference

After caching the chart in a session, call `roomSsReference` with the selected song and difficulty. Like `evaluateAll`, it can also receive chart data in the call's `charts` array.

```json
{"op":"roomSsReference","handle":1,"song":{"songId":100013,"difficulty":3}}
```

By default, five copies of one skill-free reference team play the same difficulty. Eligible notes receive JUST and the others receive PERFECT. The ordinary skill order and LUCK seed are both zero. Each player's default placement is constant across all three sections, from first through fifth. A placement scenario assumes these ranks. The cooperative solver obtains actual ranks by comparing each player's section performance.

For the song view, display `reference.roomThreshold` and `neutralCrossing.baseScore`. Each entry in `neutralCrossing.scores` is a final personal score at the shared integer power `neutralCrossing.power`. The engine replays that power and the preceding power for every seat to verify that the five scores cross the multiplayer SS line. If a crossing is unavailable, the response supplies a null crossing and the reason in `neutralCrossingUnavailable`.

Only the section score earns the placement award. At the crossing, `sectionBonuses[section][rank - 1]` is the integer award for that section and placement. Swapping two players' placements within a section redistributes their scores and preserves the room total for this neutral team. To change ties, submit a new `scenario.rankSchedule`. Each of its five rows contains one player's three placements. In each column, tied positions use competition ranking, for example 1, 1, 3, 4, 5.

For a reference with actual skills, supply both `request` (the owner's members, snaps, player bonuses and constraints) and `team` (five member keys and their five equipped snap keys or nulls, with an optional leader key). A player's allowed difficulties and judgement conditions carry over. The optional `scenario` object accepts `accuracy`, `orderIndex`, `seed` and `rankSchedule`. The engine feeds each assigned placement back into that team's skills for the following section. Each of the five entries in `reference.seats` contains the simulated personal score, its base score after removing the three awards, and the section breakdown. The integer `targetScore` shares divide the SS line in proportion to the simulated scores. These are planning estimates. With skills the base scores can differ by placement, and an actual room can reach SS with many different individual scores. A single LUCK seed describes one outcome.

## Song rules

`request.songRules` overrides the song type and each of the three Gekiso ranges. Zero keeps the source value, and type 99 selects ALL. Live, Gekiso, rewards, manual evaluation and timelines all use the same overrides.

## Multiple songs

`request.aggregation: "mean"` searches one formation over all `goal.songs`. Score objectives use mean scores, and event objectives average their selected rewards. Equal prepared power tables and skill-program identities share setup and dominance dimensions; every chart keeps its own score coefficients and complete result. Ordinary accuracy searches intersect each song's converter and recovery domains and prune partial domains with the individual song ceilings.

In Arena, `mode: "shared"` searches one leader, member set and snapshot assignment for the equal-weight average over every chart. Only this joint search proves the result; per-song optima are seeds.

Mean-score envelopes maximize each Snapshot's complete mean contribution before combining slots. Cached means preserve directed rounding and physical Snapshot matching.

## Caches

| Operation | Behaviour |
| --- | --- |
| `chart` | Reports `cachedCharts`: at most four, in eviction order |
| `deferScores` | Private flag for unsampled, non-LUCK maximum-score searches. Hits carry `pendingScores: true` and a null score, and the caller completes them |
