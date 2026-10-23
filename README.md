# ournotes-team-builder

The team-building engine shared by [Haneoka](https://haneoka.org) on the web
(WebAssembly) and in the Haneoka app (native).

## Search and settlement

Search applies member, snapshot, leader and binding constraints to warm teams
and final candidates. Unknown required cards or bindings leave no legal team;
manual evaluation rejects unknown snapshots. Sharded ordinary reward warm
searches expand only their assigned leaders while sharing legal incumbent
teams across shards. Search retains distinct member sets and uses legal
incumbents to prune branches. Snapshot dominance requires
enough interchangeable witnesses to respect the five distinct snapshot slots.
Assignment bounds couple power
and skill effects; deterministic Gekiso bounds also track converter activation
within each mission and its activation limit. Mean and extreme bounds use
five-event matching directly. Scoring-equivalent snapshots share dominance
classes; power-neutral exchanges retain one canonical placement. Rounding
bounds account for the independent judgement channels and the selected roster.
Bounds reuse coefficients for equivalent slot programs while preserving the
order of active judgement converters. Compiled score programs and legal
converter cohorts are shared across snapshot prefixes. Deterministic warm
search scores each neighborhood before replaying its best proposal; the final
search still proves the optimum over every legal team. Deterministic mean and
minimum searches share an envelope of reachable conversion traces before
planning individual rosters. Maximum searches compare the accumulated local
planning work with the chart-wide profile grid before preparing that envelope.
LUCK assignment bounds preprice snapshot choices on fixed power/coefficient
planes, with outward rounding and distinct-snapshot matching. The planner
chooses its representation from the snapshot program structure.
Pure COMBO assignments reuse the selected members' activation timeline to
tighten deterministic bounds before evaluating snapshot combinations.
Nominal LUCK maximum bounds combine the member base with five-event matching
before preparing snapshot assignments. Minimum bounds use the minimum
five-event assignment over relaxed snapshot choices. Snapshot regions group
remaining support programs by mission before refining individual programs. Equal stripped references
share non-Rush support gains while retaining each context's Rush contribution.
Owner-local actor normalization is cached once per search. Nominal LUCK mean
search partitions each slot into context-changing and ordinary snapshot domains,
applying physical assignment bounds before expanding each domain. Maximum-score
search uses the same domains and retains exact warm-search neighbors to seed
every requested distinct-member result. Nominal LUCK minimum assignments also
compare a relaxed worst-order pricing plane.
Fixed-seed initialization bounds draws from total gauge input, including
queued draws. Multi-seed warm searches use isolated proposal charts and
accept candidates only after evaluating every requested seed.
Regional bounds preserve sampled Rush and support joint frequencies and
condition their envelopes on the number of physical random-effect snapshots.
Fixed-seed assignment bounds match activation events with the number of
random-effect snapshots. Growing support domains extend cached native lottery
envelopes.
Positive-seed member bounds price contested physical snapshots once per leader;
nonnegative column prices prevent one snapshot from being assigned to several
members in the relaxed bound.

Event recommendations maximize the selected reward, then the other direct
reward, then mean score.
Fractional reward weights retain finite mean-reward bounds. When their binary
exponents exceed the integer hull's range, bounds compare all rank pairs with
outward rounding to obtain the same upper concave envelope.
The private `rewardPhase` request separates pure COMBO and weighted ordinary
Live Top 1 proofs into
`primary`, `other` and `score`. Later phases take the globally proven reward
values and a legal incumbent floor. Their results carry the stage name;
callers report a complete recommendation only after every stage is proven.
Adjacent phases retain complete settlements for the same inventory, player
and goal; replacing a chart invalidates this cache.
Pure JUST compares all three in one traversal;
other modes search the exact equality groups of the preceding rewards.
Pure COMBO and pure JUST ties keep the original selected-reward objective,
preserving its snapshot bonus constraints and
compiled scoring data. Long-term CP conversion belongs to the selected
reward; the secondary reward is the direct per-play payout. Skip settlements
return both payouts using the same values as their objective.
Gekiso rewards use the score search's default room of five equal-score players.
Each sampled personal score is compared with the battle rank threshold: both
the room total and its native five-player threshold scale by five. Rank shares
count every sampled outcome before averaging rewards; ordinary solo rewards
retain their personal rank thresholds. Manual activity evaluations with
positive samples use the same settlement, including actual score extrema and
rank shares.
Sampled Gekiso rewards reuse integer note-score prefixes across skill orders
with identical active factors, preserving per-note and mission settlement.
LUCK recommendations also seed the search with a legal team that maximizes the
other reward bonus while retaining the primary-bonus roster or snapshots.
Reward passes share their replay contexts. Pure LUCK bounds share lottery
cores across equivalent integer-gauge programs, preserving native member and
support registration order and each requested seed. Both envelope and core
caches retain at most 2 MiB. Rush projections use the native
sustained-support timeline, including its frame tail, while sharing contexts
across support values.
Reward bounds compile identical skill templates once and retain the original
member and snapshot slots for matching. Bonus-based snapshot filtering keeps
a reward upper bound for every excluded branch. Pure COMBO roster bounds reject
impossible member sets before snapshot preparation. Reward dual bounds reuse
valid event assignments before building matrices, and try their most recent
pruning parameter first. Primary-only bounds prepare the secondary reward when
its tie phase needs it. Necessary reward-bonus floors
also constrain physical snapshot matching; unvisited skill orders retain infinite
upper bounds when the reward curve already proves the roster cannot improve.
Pure LUCK recommendations partition snapshot choices by random-effect programs.
For domains with up to two random-effect snapshots, native traces preserve each
program and actor order. Previously weak seeds tighten the physical score bounds;
unexamined seeds keep the full reward ceiling. Integer settlement certificates
retain exact reward ties.

Ordinary score targets first prove the mean-score Top K. When every retained
team reaches the target in all 120 orders, those teams also prove the target
optimum. Other targets retain the mean teams as seeds for the complete search.
Target bounds combine mean-score bounds with five-event maximum bounds,
including the secondary mean-score term.
Successive score phases keep the request's original cancellation generation.

Finite Live Boost plans maximize total event points over the selected budget.
They settle the discrete CP distribution across skill orders, including the
expected challenge count and remaining CP. Equal totals prefer mean score.
Circular convolution and exponentiation reuse each reward distribution without
enumerating individual play sequences.

Gekiso's minimum COMBO BONUS is the sustained base multiplier after the first
COMBO activation, evaluated at initial LIFE. Later combo thresholds affect
scoring and do not satisfy this prerequisite. Charts without COMBO missions
ignore the prerequisite.

Deterministic Gekiso results and manual evaluations use complete native
replay over all 120 skill orders. Orders share the full native state until
their next ordinary skill event differs; each branch retains the original
frame, RNG, LIFE and score history. Fixed-seed numeric searches initialize
legacy heuristic weights only when their compiled bound is unavailable.
Exact COMBO BONUS searches bound remaining power by the attainable bonus sum,
and member cuts retain their rounded prefix sums along the recursion.
Equal minimum or maximum scores prefer the higher mean. Ties fill score
summaries when the comparison needs them. Maximum searches replay orders by their
strict upper bounds and fill the remaining orders for retained teams.
Positive seed counts enumerate seeds from
zero; zero retains the nominal LUCK calculation.
The private `seedVariant` selects the sampled or Rush-free warm projection.
Retained proposals are evaluated on the full requested seed set before they
are reported; the complete search still proves every legal formation. Fixed-seed bounds enumerate
reachable lottery programs in native frame order and combine COMBO, JUST and
LUCK coefficients for the same physical snapshot assignment. Combined regional
rows are shared across rosters, filled per member and reclaimed by an 8 MiB
clock cache that counts allocated rows separately from their shared sources.
Positive-seed
search divides leaders among shards and keeps equal-power leader positions
distinct, since their random-effect order can change settlement. With five
forced members, lottery preparation uses those members' programs. Nominal LUCK
keeps member sets together to reuse their contexts. Replays without active
random effects share one identical seed result. Sampled scoring shares native
integer note-score prefixes across ordinary skill orders and falls back to full
replay when judgement conversion or LIFE conditions need it. LIFE follows the
game's damage, healing and recovery order. Mission-end release uses END + 1 ms.
Pure COMBO reward models share native combo traces and memoized support gains
within an 8 MiB trace cache; the complete model cache retains 32 entries.
Their owner conditions, ordinary skill windows and cumulative-counter layout
are prepared once for the same chart and inventory; each trace still computes
its native score weights separately.
COMBO scripts normalize each inventory member once per search.
Timed nonnegative COMBO gains retain finite search bounds by remaining active
in the upper envelope; exact replay preserves their activation and expiry.
Independent cumulative COMBO supports share a native replay and retain their
owner-specific commands within the existing 64-program cache. Fixed-seed replays
clone an initialized model and reset each seed's RNG before
advancing its own state. Native effects with edge-only handlers apply at
activation and finish; one-shot cumulative factors update when their count changes.
Lottery envelopes share a cached fingerprint. Rush and support changes accumulate
at their first affected notes, then one prefix pass combines all requested seeds.
Random-support bounds cap the active count by a matching of slots to distinct
physical snapshots. Fixed-seed regional components
share up to 16 skill/window preparations by conversion trace. Lottery-command
counts and note weights are priced from the current envelope. Physical snapshot
matching resolves its final two rows directly with distinct available columns.
Regional bounds read each member's cached row slice once for its snapshot offers.
Nominal Max and Min assignments reuse the current roster's base in their scalar
and joint power/skill cuts. Each assignment domain refreshes that base. Fixed-seed
Min also bounds physical snapshots in a fixed ordinary-skill order and retains
the mean bound: a team's worst-order score is at most its score in any fixed order.

Member programs are interned before search; the 32 MiB note-weight cache gives
recently used rows a second eviction chance. Trigger dependencies are classified
once, and unchanged input frames skip their corresponding checks. Settled
sustained states return directly, and update calls reuse their mission-gate result.
Context preparation records factors in replay order and keeps each note's latest
observation after history rewinds. It omits unused integer note totals. Frame
buffers are reused, recorded frames share unchanged range-state slices, and LIFE
without commands returns its initial value.

Pure JUST reward bounds share whole-inventory coefficient rows by converter
trace and price power and score together before snapshot enumeration. Necessary
badge-bonus floors also constrain the member bounds; excluded snapshot branches
retain their reward upper certificates. Coarse pure JUST bounds reuse the
120-order coefficient distribution before preparing joint power/score prices.
Pure JUST member pruning admits stronger ordinary skills when every snapshot
has an equivalent normalized Gekisou score program. Replacements within one
character require a power gain above the native rounding margin.
Fully bound fixed-seed teams bypass free-team warm searches and still complete
the ordinary exact search and proof.

Context and coefficient caches are local to the search and retain the skill
programs, owner attributes and random inputs that can affect their values.
Bounded LUCK caches evict cold entries incrementally. Snapshot choice rows
are shared across pricing planes; priced frontiers retain their exact plane key.

## License

AGPL-3.0-only, with an additional attribution term under section 7(b): any
interface that shows results computed by this engine, or by a work based on it,
must display "Team-building algorithm from Haneoka — https://haneoka.org" with
an explicit hyperlink to https://haneoka.org. See [NOTICE](NOTICE) and
[LICENSE](LICENSE).

The private `deferScores` request flag applies only to unsampled, non-LUCK
maximum-score Top K searches. Pending hits carry `pendingScores: true` and a
null score; callers complete every candidate at the distinct-member primary
boundary with native 120-order evaluation before publishing or claiming a full
result. Requests without this flag retain complete scores.
