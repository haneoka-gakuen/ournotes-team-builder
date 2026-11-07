# Engine guarantees

What each request guarantees. For usage, see the [README](../README.md).

## Search

A completed search has checked every legal team and proves its result. A search stopped early returns its best teams, marked as unproven.

Member, snapshot, leader and binding constraints apply to every candidate. An unknown required card or binding leaves no legal team, and manual evaluation rejects unknown snapshots.

## Activity growth inference

`eventGrowthStart` binds recorded song decks and release-pinned growth catalogues to an existing engine context. `eventGrowthStep` advances the retained search with a small computation budget. It uses the normal card resolver, slot-power arithmetic and shared parameter curves; no chart is required.

Up to ten feasible configurations are retained globally. Stable exact witnesses rank first, followed by smaller character, instrument and account-bonus changes. T.G.W follows those changes, with no reserved count per tier. Equal-evidence ties use descending instrument totals; character rank height has no ranking weight. Wider historical domains are explored progressively while exact searches retain their frontiers; a candidate is checked against the narrowest domain that explains it.

Equivalent decompositions of character-rank and account-total bonuses are grouped in `equivalents`. The outer character and account-total ranges are marginal envelopes; each equivalent contains correlated levels and its own song witnesses. Their cartesian product is not a valid configuration. Only currently obtainable mission caps constrain maximum ranks; there is no percentile-of-cap admission stage. Historical joins minimize the actual change cost rather than accepting the first feasible join.

Each song has an integer witness that exactly reproduces its recorded power. Across witnesses, character ranks differ by at most two and each band's instrument total by at most ten. Account totals must be realizable within the same unseen-character envelope. Rank caps come from currently obtainable mission EXP; normal instrument totals start at fifty.

Here `complete` marks the end of bounded candidate collection, separate from the exhaustive proof contract of ordinary team search. Closing or cancelling the owning context releases its inference state.

## Event rewards

A `rewardPlan` maximises a nonnegative integer-weighted sum of PT, CP and badges, followed by up to two ordered tie levels.

| Target | Result |
| --- | --- |
| Score target | The mean-score top K is proven first. If every team reaches the target in all 120 skill orders, it is also the target optimum |
| Finite Live Boost | Maximises total event points over the budget, including the CP distribution across skill orders and the expected challenge count |

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
