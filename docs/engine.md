# Engine guarantees

What each request guarantees. For usage, see the [README](../README.md).

## Search

A completed search has checked every legal team and proves its result. A search stopped early returns its best teams, marked as unproven.

Member, snapshot, leader and binding constraints apply to every candidate. An unknown required card or binding leaves no legal team, and manual evaluation rejects unknown snapshots.

## Activity growth inference

`eventGrowthStart` binds recorded song decks and release-pinned growth catalogues to an existing context. `eventGrowthStep` advances complete branch-and-bound search; `eventGrowthPage` resumes its retained frontier for another ten-row page. There is no total result cap or fixed-time completion shortcut. The displayed domain is the globally minimal change cost and cost + 1. Any feasible incumbent supplies a safe upper bound; once the minimum is proved the cost window is exact.

The objective is `5 * sum(character rank spans) + sum(band instrument-total spans) + account-bonus span`, followed by fewer changed dimensions, higher T.G.W and descending instrument ranges in band order. These are integer change costs, not player-rank likelihoods. The search opens complete cost layers in ascending order. All T.G.W and optional overall-ranking contexts participate before a candidate can be certified.

Interval nodes carry admissible lexicographic bounds: the current cost layer, a lower bound on changed dimensions, the fixed T.G.W and upper bounds on instrument minima/maxima. A candidate enters the fixed prefix only when its actual key precedes every remaining interval bound. Equal semantic keys use deterministic certification order. Uncertified exact candidates can fill the active page while computation continues; a full certified page pauses with its frontier intact. Later pages cannot insert smaller-change rows into earlier pages. `complete` requires the entire permitted near-optimal domain to be exhausted.

A separate small-radius scout can supply exact provisional matches; its heuristic discovery order never certifies rows or terminates the proof frontier. Lossless normalized rank, account-bonus and instrument offset profiles partition every cost layer. Necessary per-axis difference relations filter and cache normalized histories before their Cartesian join; partial-history difference bounds skip entire impossible suffix products. Their lazy generators remain in the certifying frontier, so neither state generation nor pagination requires an arbitrary count limit. Differences between identical power functions constrain flat bonuses using cached exact min/max difference curves across every legal instrument level. Canonical whole-history account-bonus branches remove uniform-shift duplicates while retaining all absolute decompositions. Their grade-sum intervals also propagate through the flat domains. Fixed-instrument boxes use a lossless meet-in-the-middle join; large half domains stay in the complete interval tree. Per-equation and equation-difference domain propagation, including integer residue checks, precedes the join. Both ordinary power bounds and these difference bounds are conservative. The supported rules require nonnegative power coefficients; all emitted witnesses are independently checked through shared `team_power`. Caches contain budget-independent power/difference and rank-marginal facts and survive cost-layer changes. Conservative interval reachability under the change budget prunes impossible tiers and profiles before absolute-level search. `profiles.rs` owns normalized-history generation; `search.rs` owns integer bounds, the fixed-instrument join and witness verification.

An optional `total` deck adds an overall-ranking observation. The game response omits its source song, so distinct activity-song power contexts are OR branches. Missing/incomplete overall decks retain song-only inference. Overall witnesses have `challengeId: 0` and retain `contextChallengeId`. Repeated equations remain distinct records; sharing difference/power facts does not force their hidden growth states to be equal.

Every record must exactly reproduce its integer power. Character ranks differ by at most two, each band's instrument total by at most ten, and all-character totals share a realizable unseen-character envelope. Ranks of characters absent from a particular deck remain variables. Caps come from current mission EXP. Normal floors are rank fifteen and band-total fifty; lower domains are considered only if the complete normal domain has no solution. No unverified chronology constraint or absolute-rank prior is applied.

Uniform role-rank/account-bonus translations are grouped as correlated `equivalents`. Family identity includes every original record's effective bonuses, relative account bonus and instruments, so distinct historical patterns are retained. Outer ranges are marginal envelopes, not a cartesian product. Families are deduplicated on certification. `certified`, `costBound`, `examinedBoxes` and `prunedBoxes` expose proof progress and anchor-box work without adding UI columns.

## Event rewards

A `rewardPlan` maximises a nonnegative integer-weighted sum of PT, CP and badges, followed by up to two ordered tie levels.

| Target | Result |
| --- | --- |
| Score target | The mean-score top K is proven first. If every team reaches the target in all 120 skill orders, it is also the target optimum |
| Finite Live Boost | Maximises total event points over the budget, including the CP distribution across skill orders and the expected challenge count |

Rewards settle with the client's integer rules before outcomes are compared:

- Normal lives and skips earn CP from the rank's CP value and the Live Boost multiplier. Challenge lives and skips consume CP and earn none. Boost multipliers come from the release's consumption row; an unboosted normal live uses 1×.
- PT follows `CalculateEventPoint`: signed Int32 arithmetic on `10000 + bonus`, rank points and the event-point multiplier, divided by 10000 toward zero.
- Item rewards follow `ApplyEventBonus`: Int64 products of Int32 inputs, divided toward zero and capped at Int32 maximum per row, before the row's selection probability applies.

`rewardPlan.criterion` is `mean` (settle each outcome, then average) or `max` (the greatest reward tuple over all observed outcomes and 120 skill orders). Rewards not listed in the plan do not take part in comparison.

`rewardRoom` sets the Gekiso event room: `{mode: "battle", players, otherAverageScore}` uses `trunc(sqrt(5 / players) * battleRequiredScore * players)` minus the other players' scores, and `{mode: "solo"}` uses the personal thresholds. Fixed-team evaluation reads it from `request.event.rewardRoom`.

LUCK uses seeds 0 to `luckSamples - 1`. A completed search is optimal for those samples; the response keeps its sample count and `sampled` flag.

Gekiso's minimum COMBO BONUS is the sustained base multiplier after the first COMBO activation, at initial LIFE. Later thresholds affect scoring but do not satisfy the minimum.

## Gekiso

Deterministic results and manual evaluations replay all 120 skill orders. A LUCK sample count of 0 gives nominal expectations, and a positive count averages fixed seeds 0 to n-1.

`goal.arena` sets the league band, music type and three mission overrides for one call. Zero keeps the song's own value.

## Song rules

`request.songRules` overrides the song type and each of the three Gekiso ranges. Zero keeps the source value, and type 99 selects ALL. Live, Gekiso, rewards, manual evaluation and timelines all use the same overrides.

## Multiple songs

`request.aggregation: "mean"` searches one formation over all `goal.songs`. Score objectives use mean scores, and event objectives average their selected rewards.

In Arena, `mode: "shared"` searches one leader, member set and snapshot assignment for the equal-weight average over every chart. Only this joint search proves the result; per-song optima are seeds.

## Caches

| Operation | Behaviour |
| --- | --- |
| `chart` | Reports `cachedCharts`: at most four, in eviction order |
| `deferScores` | Private flag for unsampled, non-LUCK maximum-score searches. Hits carry `pendingScores: true` and a null score, and the caller completes them |
