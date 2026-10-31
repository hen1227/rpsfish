# Intransitive engine study — 2026-09-06

The new engine beat the pre-task RPSFish working-tree snapshot at both tested
node budgets, including when the old engine received twice as many nodes.
These are local paired-match estimates against that engine, not a public rating.

| Nodes per move, new / old | Games | Wins / draws / losses | Score | Estimated Elo, 95% CI | Pair-based LLR |
| --- | ---: | --- | ---: | --- | ---: |
| 20k / 20k | 1,200 | 1,038 / 48 / 114 | 88.50% | +354.5 [+327.4, +385.3] | +37.13 |
| 100k / 100k | 600 | 487 / 27 / 86 | 83.42% | +280.6 [+247.1, +319.2] | +11.27 |
| 20k / 40k | 600 | 496 / 25 / 79 | 84.75% | +297.9 [+263.1, +338.4] | +12.36 |

All three pass the approximate SPRT's +2.944 acceptance threshold, with
hypotheses 0 and +5 Elo and alpha/beta 0.05. Confidence intervals use opening
pairs as the independent samples. No confirmation game reached the ply cap.
Raw records are in [intransitive-results](intransitive-results/).

## What the game rewards

A corner race is also a capture problem. A Rock guarding a1 makes a Red Paper
runner relevant; being closer with Scissors does not by itself solve that
defense. The evaluator now searches outward from the goal for each piece kind,
excluding friendly blockers, enemy pieces that kind cannot capture, and squares
attacked by its predators. It permits an attacked goal square because arrival
wins before a recapture. Own pieces can end a route but cannot be walked through.

The nearest two reachable pieces receive weights 30 and 15, using progress
`9 - distance` for routes of at most eight moves; absent routes score zero.
These are estimates against the current defenders, not proofs about opponents
who can move. The old geometric advancement and closest-piece bonuses remain
available for parameter-file compatibility, but their defaults are zero.

Support receives weight 8 per covered piece. For example, Scissors near a Rock
can capture the Paper that takes that Rock. This values the actual RPS
relationship; another Rock nearby does not provide the same protection.

An uncapturable corner occupant is useful but cannot be assumed to hold forever.
If it is the owner's only piece, the turn rule can force it off the corner. The
existing plug and seal bonuses stay heuristic. The last-piece and two-piece
features now recognize that losing every piece loses by stalemate in this mode;
their weights remain zero because they have not earned a fitted value.

## What search now resolves

* An immediate corner arrival wins even if static evaluation would cut off.
* When the enemy threatens arrival, quiescence searches every legal defense,
  including quiet plugs. It cannot stand pat, and evasions are not reduced.
* A compact retrograde solver handles every one-piece-per-side position.
  Three relative type matchups cover all nine actual pairs. It stores exact
  win/loss distances in approximately 77 KiB, initialized on first use.
* With two pieces, every capture ends the game. Thus no nonterminal move can
  reset the quiet clock, and the solved distance determines whether the
  200-ply draw arrives first. The solver is tested against every legal successor
  of all 37,926 nonterminal table states.
* Intransitive transposition keys distinguish quiet clocks within 128 plies of
  the draw. Earlier clocks share a key because no permitted search can reach
  the draw from there. Position hashes used for repetition and books are unchanged.

The more aggressive selective-search experiment was rejected: it lost roughly
57 Elo in its initial screen and missed a forced loss. Intransitive keeps the
conservative search. Applying the new corner tactics or clock policy to other
modes was also rejected after separate measurements. Their final paired checks
returned exactly 0.5000 with equal wins and losses.

## How the changes were selected

The first safe-route screen scored +354.5 Elo over 800 games against the same
search without route evaluation. Halving the route weights lost 126.5 Elo
against 30/15; doubling them was inconclusive. Removing geometric bonuses added
97.1 Elo in its screen, and adding RPS support added 86.0. Their combination
scored +140.2 over 1,000 games against routes alone. These are separate ablations,
not gains to add together.

Additional safe-mobility, hanging-piece, trapped-piece, and pursuit terms did not
improve the chosen route evaluator and were removed. A 60-iteration SPSA run
settled near the selected values; its only rounded changes were seal 92 to 90
and immortal-piece 63 to 64. Those unconfirmed changes were not adopted. Similar
strong candidates drew frequently, so that tuning run could not establish a
finer optimum.

Exploratory screens used seed 4422, SPSA used 314159265, and final matches used
202609061, 202609062, and 202609063. Each opening is replayed with candidate and
baseline colors swapped. Each engine retains its own search state during a
game and clears it between games. The snapshot runner uses six random opening
plies, a 4 MiB table, depth cap 64, and fixed node limits. Both implementations
must accept every move and agree on every terminal result. The identical-engine
self-check returned 1 win, 98 draws, 1 loss, and 50 exactly balanced pairs.

Evaluation costs more per node: about 5.5M versus 8.9M nodes/second in the 100k
run. Giving the old engine double the node budget tests a conservative allowance
for that cost; it still lost decisively. These are throughput observations, not
fixed wall-clock tournament results.

The general arena's old 400-ply cutoff sometimes declared unfinished games drawn.
Its default backstop now exceeds the maximum legal sequence of quiet intervals
and captures. Final snapshot matches used a flagged 1,200-ply backstop and none
hit it. Repetition is not a draw in the current rules.

## Reproduction and validation

The pre-task source snapshot, native comparison harness, and exploratory logs
are retained locally under `target/intransitive-study/`. This preserves the
actual dirty working tree used as the baseline, rather than substituting a
Git revision that omitted the user's existing changes. Keep that directory if
running `cargo clean`.

```sh
cargo run --release --manifest-path target/intransitive-study/lab/Cargo.toml -- 600 20000 202609061 4 V6
cargo run --release --manifest-path target/intransitive-study/lab/Cargo.toml -- 300 100000 202609062 4 V6
cargo run --release --manifest-path target/intransitive-study/lab/Cargo.toml -- 300 20000 202609063 4 V6 '' on 40 6 2
cargo test --release --lib --tests
cargo test --lib
cargo clippy --lib --tests -- -D warnings
cargo run --release --example suite -- --file suite/intransitive_corners.txt --nodes 20000 --hash-mb 4 --require 9
./scripts/build_web.sh
node scripts/bench_web.mjs --nodes 20000 --time-ms 1000 --single
./scripts/build_ios.sh
```

The corner suite was repaired for the current a1/i9 goals and stalemate-loss
rule, then extended with a forced-departure case. Search solves all nine cases
at 20k nodes. The static evaluator intentionally cannot prove every tempo or
zugzwang result. Tests cover safe-route blockers, winning arrivals into attack,
RPS support, exact endgames, quiet defenses, draw-clock cache separation,
legality, symmetry, make/unmake, and search limits.

Final validation passed 164 release tests, 104 debug library tests, strict
Clippy checks, and both exact-endgame browser smoke cases. The copied comparison
harness also built and reproduced an exactly balanced identical-engine pair.
Release-test and browser output are retained with the match logs.

The browser benchmark now includes the actual Intransitive starting wedges.
The native binary, browser WASM, and device/simulator iOS libraries are rebuilt
from the same source. ABI and rules versions remain unchanged. Old opening
books require rebuilding under the new evaluation fingerprint; the engine's
existing compatibility check rejects incompatible books.
