# ournotes-team-builder

The team-building engine shared by [Haneoka](https://haneoka.org) on the web
(WebAssembly) and in the Haneoka app (native).

## Search and settlement

Search applies member, snapshot, leader and binding constraints to warm teams
and final candidates. It retains distinct member sets and uses legal incumbents
to prune branches. Snapshot dominance requires enough interchangeable witnesses
to respect the five distinct snapshot slots. Assignment bounds couple power
and skill effects; deterministic Gekiso bounds also track converter activation
within each mission and its activation limit. Mean and extreme bounds use
five-event matching directly. Scoring-equivalent snapshots share dominance
classes; power-neutral exchanges retain one canonical placement. Rounding
bounds account for the independent judgement channels and the selected roster.
Bounds reuse coefficients for equivalent slot programs while preserving the
order of active judgement converters.

Event recommendations maximize the selected reward, then the other direct
reward, then mean score. Each later search stays in the exact equality group
of the preceding rewards. Long-term CP conversion belongs to the selected
reward; the secondary reward is the direct per-play payout. Skip settlements
return both payouts using the same values as their objective.

Gekiso's minimum COMBO BONUS is the sustained base multiplier after the first
COMBO activation, evaluated at initial LIFE. Later combo thresholds affect
scoring and do not satisfy this prerequisite. Charts without COMBO missions
ignore the prerequisite.

Deterministic Gekiso candidates and manual evaluations use complete native
replay over all 120 skill orders. Positive seed counts enumerate seeds from
zero; zero retains the nominal LUCK calculation. Replays without active
random effects share one identical seed result. LIFE follows the game's
damage, healing and recovery order. Mission-end release uses END + 1 ms.
Context and coefficient caches are local to the search and retain the skill
programs, owner attributes and random inputs that can affect their values.

## License

AGPL-3.0-only, with an additional attribution term under section 7(b): any
interface that shows results computed by this engine, or by a work based on it,
must display "Team-building algorithm from Haneoka — https://haneoka.org" with
an explicit hyperlink to https://haneoka.org. See [NOTICE](NOTICE) and
[LICENSE](LICENSE).
