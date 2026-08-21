# RPSFish

RPSFish is a deterministic, Stockfish-inspired engine for the three RPS
Strategy modes. The crate contains a compact rules/search position,
mode-specific evaluation, iterative-deepening principal-variation and MultiPV
search with aspiration windows and a selective pruning layer, a transposition
table, repetition handling, perft support, a diagnostic CLI, a self-play arena,
and a dependency-free WebAssembly adapter. Completed depths can be streamed
while a deeper iteration is still running, so callers always have a trustworthy
result to display.

## Design rule: facts, features, weights

Three kinds of knowledge are kept strictly apart, because mixing them lets the
author's guesses become the engine's ceiling.

| Layer | Lives in | Who decides | Example |
| --- | --- | --- | --- |
| **Rule facts** | `Position`, `Mode::rules` | the rules | "this piece can never be captured again" |
| **Features** | `evaluation.rs` | the engineer | "I own three such pieces" |
| **Weights** | `EvalParams` | the arena | "each is worth 70" |
| **Plans** | search, move ordering | search | "so move that piece" |

Consequences that follow from this split:

- A *fact* may be used as a proof. Because the capture hierarchy is a single
  3-cycle and no mode ever returns a piece to the board, a side whose predator
  kind is extinct owns permanently uncapturable pieces. In a mode where
  annihilation is the only way to lose, that side cannot lose, and search
  clamps its score at a draw rather than trusting a finite weight to express
  it. If both sides are uncapturable the game value is exactly zero and the
  subtree is pruned. `ModeRules::immortality_prevents_loss` is the single
  switch that licenses this, so Total War and Infiltration correctly do not
  get it.
- A *feature* may be added freely, but its weight is never asserted. New
  features ship with a weight of zero until the arena gives them one.
- A *plan* never appears in evaluation. "Prefer the uncapturable piece" is not
  encoded anywhere; it emerges from correct material scoring. The one place
  intuition is safe is move ordering, where a wrong guess costs nodes and never
  correctness, so `grants_immortality` is an ordering bonus only.

The Go server remains the authoritative game implementation. RPSFish is an
independent search core whose rule behavior will be continuously checked
against Go-generated parity fixtures.

See [PLAN.md](PLAN.md) for the architecture, rule decisions, integration plan,
performance targets, and milestones.

## Commands

Run all checks:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Weights are data, not constants. Load a tuned set or override individual
values:

```sh
cargo run --release -- search annihilation --param annihilation_immortal_piece=90
cargo run --release -- search total-war --params-file tuned.txt
cargo run --release --bin arena -- show > tuned.txt
```

Search a starting position:

```sh
cargo run --release -- search annihilation 8
cargo run --release -- search total-war 6
cargo run --release -- search infiltration 6
```

Ask for a long, bounded analysis or set each limit explicitly:

```sh
cargo run --release -- search total-war --deep
cargo run --release -- search total-war --depth 24 --nodes 10000000 --time-ms 30000 --multipv 3
```

The CLI prints an `info` line after every fully completed depth, including the
score, selective depth, node count, nodes/second, stability confidence, and
principal variation. `--deep` aims toward depth 127 but retains 50-million-node
and 60-second safety caps. Search remains single-threaded with fixed-size hash
memory; `--hash-mb` changes that memory budget.

Count the legal-move tree:

```sh
cargo run --release -- perft annihilation 4
```

Build the browser engine and copy it into the Expo website's public assets:

```sh
./scripts/build_web.sh
```

The web API accepts the eight engine bitboards as low/high 64-bit halves and
exposes the last analysis through scalar getters. `rpsfish_abi_version()` is
currently `3`; it adds in-search time limits, caller-selected MultiPV, principal
variations, selective depth, elapsed time, and confidence. The history exports
let callers provide earlier positions for threefold-repetition scoring. The
frontend invokes it in a dedicated Web Worker, emits a snapshot after every
completed depth, and accepts `maxDepth`, `maxNodes`, `maxTimeMs`, `variations`,
and `throttleMs` limits. The browser boundary clamps requests to depth 127,
100 million nodes, 120 seconds, and eight variations. Eight rather than three
because the difficulty ladder is built on sampling among root moves, and the
measured score range across the top eight is roughly triple the range across
the top three — the extra lines are what let a weak profile play a plausible
bad move rather than a random legal one. The shipped Deep preset
now takes the full node allowance and stops on its 30-second budget instead:
the engine reaches 20 million nodes in a few seconds, so the old cap was
ending deep searches early and costing several plies.

Rust callers can use `Searcher::analyze_with_updates` or
`Searcher::analyze_with_context_and_updates`. The callback receives an
`AnalysisUpdate` only after a complete iteration. Confidence is a 0-100
iterative-stability indicator based on depth, best-move persistence, and score
movement; it is deliberately not presented as a win probability.

## Selective search

Depth is what a user sees, so it is worth being explicit about how it is
bought. Above the plain principal-variation search sits one selective layer,
switched by `Searcher::set_selective`:

| Device | What it assumes | Why it is safe to be wrong |
| --- | --- | --- |
| Aspiration windows | this iteration scores near the last one | a miss costs one re-search with a wider window |
| Null move | passing is worse than playing | verified by a real search whenever it fails high; disabled with one piece left, where a forced move really can be worse than a pass |
| Reverse futility | a position far above beta stays there | only in a zero-width window, never in the principal variation |
| Move-count and futility pruning | ordering already tried what mattered | never applied to the first move, so every score is backed by a search |
| Late-move reductions | late moves are worse | re-searched at full depth when the short search beats alpha |
| Internal iterative reduction | an unordered node is not worth full depth | fills the table so the next visit is ordered |
| Delta pruning in quiescence | one capture cannot bridge a large deficit | goal moves and extinction captures are exempt |

Two rules keep this from becoming a pile of beliefs:

1. Every device is off when `selective` is off, and the layer as a whole is
   measured against its own absence. At a 20,000-node budget it is worth
   **+141 Elo** over 1,200 game pairs, and positive in all three modes
   (Annihilation +81, Total War +128, Infiltration +226).
2. The aggressiveness is data, not a constant. `SearchTuning::reduction_scale`
   is the growth rate of late-move reductions, and larger values buy depth at
   the cost of accuracy. The shipped value is the largest one that measured no
   Elo loss, not the one that printed the biggest depth.

```sh
cargo run --release --bin arena -- match --candidate-selective on --baseline-selective off
cargo run --release --bin arena -- match --candidate-reduction-scale 64
```

Pruning may never contradict a rule fact. `selective_search_agrees_with_exhaustive_search_on_forced_positions`
holds the selective and exhaustive searches to the same verdict on whether a
position is won, lost, or drawn.

## Measuring a change

Nothing about strength is decided by reading a diff. The `arena` binary plays
paired games from random balanced openings at a fixed node budget, swaps
colours within each pair, and reports a sequential probability ratio test per
mode:

```sh
cargo run --release --bin arena -- match --games 500 --candidate annihilation_immortal_piece=110
cargo run --release --bin arena -- tune --mode total-war --iterations 40 --out tuned.txt
cargo run --release --bin arena -- match --mode annihilation --baseline-proofs off
```

Rules that make the numbers trustworthy:

1. Node limits, never wall-clock limits. Results must be reproducible across
   machines and `--threads` values.
2. Paired colour-swapped openings. Two identical engines score exactly 0.5000
   with exactly equal wins and losses, which is the arena's own sanity check.
3. Per-mode reporting. A combined pass that hides a per-mode regression is a
   regression.
4. A candidate ships only on `ACCEPT`. `inconclusive` means keep playing, not
   "probably fine".

Even rule-derived proofs are measurable: `--baseline-proofs off` turns the
uncapturable-piece bounds off on one side so their contribution can be tested
rather than assumed.

The `evalbench` example reports evaluation cost and per-mode search speed:

```sh
cargo run --release --example evalbench
```

`depthbench` reports the number the user actually sees: how deep each mode gets
inside a budget, over the starting position plus deterministic random openings.
A fixed node budget compares search efficiency reproducibly; a fixed time
budget answers "how deep on this machine".

```sh
cargo run --release --example depthbench -- --nodes 3000000 --time-ms 0
cargo run --release --example depthbench -- --time-ms 3000 --mode total-war
```

`bench_web.mjs` drives the WebAssembly build the way the browser worker does,
which is the only measurement that includes the WASM code generation and the
host clock calls:

```sh
./scripts/build_web.sh
node scripts/bench_web.mjs --time-ms 30000 --nodes 100000000 --depth 127
```

## Current boundaries

- The crate has no third-party dependencies.
- Search is single-threaded, fixed-memory, and deterministic for a fixed
  position and node limit. Equal ordering scores break ties in generation
  order, so a repeated search visits the same nodes in the same sequence.
- Deadline and cancellation checks are sampled every 1,024 nodes rather than
  taken at every node, because in the browser build reading the clock is a call
  out to the host. The deadline therefore lands within a fraction of a
  millisecond of the request rather than exactly on it.
- Repetition detection scans only the current reversible run, and within it
  only the plies that share the position's side to move. Both restrictions are
  exact rather than approximate, and
  `repetition_detection_matches_a_full_path_scan` compares the bounded scan
  against a naive count along real playouts.
- The CLI is diagnostic. The versioned WASM ABI is implemented; a persistent
  native-server worker remains a future integration layer.
- Threefold repetition and stalemate are both official draws in the Go backend
  and in the engine. `RULES_VERSION` is 2 and `rpsfish_rules_version()` exports
  it across the WASM boundary; the ABI shape is unchanged at 3, but terminal
  results are not, so cached analyses must be keyed on both.
- A side with no legal move draws. `NoMoveOutcome::Loss` still exists so a
  future rule set can declare the opposite, but nothing ships with it.
- Evaluation weights that were introduced as measurable-but-unproven features
  are set to zero. The predator-scarcity buckets are the current example: see
  the note in `evaluation.rs`.

## License

RPSFish is free software licensed under the [GNU Lesser General Public License
v3.0 or later](COPYING.LESSER). The LGPL builds on the [GNU General Public
License v3.0](COPYING), so both texts ship with the crate.

In practice this means the engine may be linked into a larger work — including
a proprietary one — provided recipients can replace this library with a
modified version and relink. Modifications to RPSFish itself must be released
under the same license. Every source file carries the notice and an
`SPDX-License-Identifier: LGPL-3.0-or-later` tag.

Copyright (C) 2026 Henry Abrahamsen.
