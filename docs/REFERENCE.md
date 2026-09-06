# Reference

Commands, embedding, opening-book tooling, and measurement discipline for
RPSFish, in full. Start with the [README](../README.md) for the short version;
see [PLAN.md](PLAN.md) for the architecture, rule decisions, and milestones
behind these choices.

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
  kind is extinct owns permanently uncapturable pieces. Total War pairs that
  with a tile count no future claim can overturn to prove a side cannot lose,
  and search clamps its score at a draw rather than trusting a finite weight to
  express it. `ModeRules::territory_majority_prevents_loss` is the single
  switch that licenses this, so Infiltration -- which can also be lost at the
  boundary -- correctly does not get it.
- A *feature* may be added freely, but its weight is never asserted. New
  features ship with a weight of zero until the arena gives them one.
- A *plan* never appears in evaluation. "Prefer the uncapturable piece" is not
  encoded anywhere; it emerges from correct material scoring. The one place
  intuition is safe is move ordering, where a wrong guess costs nodes and never
  correctness, so `grants_immortality` is an ordering bonus only.

The Go server remains the authoritative game implementation. RPSFish is an
independent search core whose rule behavior is continuously checked against
Go-generated parity fixtures.

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
cargo run --release -- search total-war --param total_war_immortal_piece=90
cargo run --release -- search infiltration --params-file tuned.txt
cargo run --release --bin arena -- show > tuned.txt
```

Search a starting position:

```sh
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
cargo run --release -- perft total-war 4
```

Build the browser engine and copy it into the website's public assets:

```sh
./scripts/build_web.sh
```

Build an XCFramework for an iOS app:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
./scripts/build_ios.sh
```

The exporter writes `target/ios/RPSFish.xcframework`. It contains an arm64
device library, a universal arm64/x86_64 simulator library, `RPSFish.h`, and a
Clang module map, so Xcode and Swift can import it as `RPSFish`. Add the
XCFramework to the app target and set **Embed** to **Do Not Embed**: this is a
static library, even though Xcode presents it as an XCFramework.

The iOS and browser builds expose the same ABI and therefore share the version
check, position encoding, search limits, history, root filter, and result
getters documented below. On iOS, call every function for one engine instance
from the same serial background queue. The search and its retained state are
iOS-process-global and synchronized, and `rpsfish_analyze` is synchronous;
calling it on the main thread would block the UI. A native host should verify
`rpsfish_abi_version() == RPSFISH_ABI_VERSION` before its first search.

The C API accepts the eight engine bitboards as low/high 64-bit halves and
exposes the last analysis through scalar getters. `rpsfish_abi_version()` is
currently `4`. Version 3 added in-search time limits, caller-selected MultiPV,
principal variations, selective depth, elapsed time, and confidence; version 4
adds incremental history maintenance and a root-move restriction, described
under [Reviewing a sequence of positions](#reviewing-a-sequence-of-positions).
The history exports let callers provide earlier positions for
threefold-repetition scoring. A browser host typically invokes it from a
dedicated Web Worker, emitting a snapshot after every completed depth under a
caller-chosen `maxDepth`, `maxNodes`, `maxTimeMs`, `variations`, and
`throttleMs`.

Rust callers can use `Searcher::analyze_with_updates` or
`Searcher::analyze_with_context_and_updates`. The callback receives an
`AnalysisUpdate` only after a complete iteration. Confidence is a 0-100
iterative-stability indicator based on depth, best-move persistence, and score
movement; it is deliberately not presented as a win probability.

## Play online

`rpsfish rpsi` speaks [RPSI](RPSI.md) on stdin and stdout — the same
UCI-shaped protocol every engine on the game server speaks, which is why this
engine contains no server-specific code at all. Driving it by hand is one line:

```sh
printf 'rpsi\nisready\nnewgame V5\nquit\n' | cargo run --release -- rpsi
```

Playing real games uses the client bot script the game server hands its bot
authors, with this engine after the `--` instead of theirs:

```sh
cargo build --release
pip install websockets
python3 rpsbot.py -- ./target/release/rpsfish rpsi
```

The first run asks seven questions and saves the answers to `rpsbot.conf`, one
of which is the server — leave it at the default for the public site, or
answer `ws://localhost:8080/ws` to play against a server you are running
yourself. Another is how many games to play at once: each one runs its own
`rpsfish` process with its own hash table, so three slots at `--hash-mb 512`
is 1.5 GiB and three searches competing for cores.

**The engine command line is the only configuration channel.** A host asks for a
handshake, a position and a move, and never sends `setoption`, so the flags
after `rpsi` are the only way an online bot gets a table larger than the 16 MiB
default or a weight set other than the compiled one:

```sh
python3 rpsbot.py -- ./rpsfish rpsi --hash-mb 512 --params-file tuned.txt
```

`rpsbot.conf` never records that command, so the flags belong on the line every
time — which also means two bots with different weights are two directories and
two commands, not two builds.

How much of the clock to spend is the engine's decision, not the protocol's:
`go` reports both clocks and the increment, and RPSFish takes a twentieth of
what remains plus three quarters of the increment, never coming within 200 ms of
flagging.

## Reviewing a sequence of positions

Grading a whole game is not the same job as analysing one position, and two
things follow from that.

**A game is one lineage, not a hundred unrelated searches.** Every position in
a game is a child of the one before it, so a caller walking forward can keep
the transposition table for the entire walk. `rpsfish_history_len` and
`rpsfish_history_truncate` let the repetition history be extended by one
position per step instead of rebuilt, which is the difference between
quadratic and linear work over a long game. Measured over 41-position
engine-quality games, keeping both is worth **1.18-1.22x less time and
1.14-1.23x fewer nodes** than analysing each position from scratch; on games of
random moves the saving falls to about 1.1x, because a random move's subtree
was never searched deeply enough for its entries to be worth anything.

**A move's loss is a subtraction, and subtraction needs two comparable
numbers.** Two separate searches of one position, both correct, still disagree
by a few points: a fixed depth is only fixed until the table starts answering
from a deeper one. `Searcher::analyze_root_moves` therefore takes a list of
root moves and considers only those, so a caller can score the best move and
the played move in a single search — one window, one table, one ordering. The
browser reaches it through `rpsfish_root_filter_clear` and
`rpsfish_root_filter_push`, and the restriction is spent by the next
`rpsfish_analyze` rather than persisting.

A restricted search stores **no transposition entry for its root**: it did not
look at every move, so its verdict is not the position's verdict, and leaving
it behind would let a later search take a cutoff on a number that was never
true. A restriction whose moves are all illegal returns `NoLegalMove` with no
lines, so asking about the wrong board is an answer rather than a guess. Both
are tested in `src/search.rs`.

The website's move-review feature is built on exactly this.

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
   **+141 Elo** over 1,200 game pairs, and positive in every mode (Total War
   +128, Infiltration +226; the since-retired V1 measured +81).
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
cargo run --release --bin arena -- match --games 500 --candidate total_war_immortal_piece=110
cargo run --release --bin arena -- tune --mode total-war --iterations 40 --out tuned.txt
cargo run --release --bin arena -- match --mode infiltration --baseline-proofs off
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

### When the arena is the wrong instrument

The arena answers "is this stronger on average in self-play", and some real
defects never come up there. A side reduced to one permanently huntable piece
is the worked example: the pattern decides real games and is exactly what a
human plays for, but self-play from random openings reaches it so rarely that
an evaluation weight set to -5000 changes zero games out of two thousand. No
sample size fixes that.

Those go in `suite/`, as positions whose verdict a very deep search has already
proved, scored at the depth the shipped bot actually reaches:

```sh
cargo run --release --example suite
cargo run --release --example suite -- --nodes 300000 --param total_war_doomed_endgame=0
cargo run --release --example suite -- --require 19   # exit non-zero below this
```

The run reports searched and static accuracy separately, because they are
different questions: search can rescue anything it can resolve inside its
horizon, while the static column is what the evaluation itself knows. Positions
are written in the same FEN the server stores in `game_pgn`, so one can be
pasted straight out of a real game. A change that lifts suite accuracy at no
Elo cost is worth shipping; a suite entry whose verdict is not proven is worth
nothing, so unresolved positions are dropped rather than guessed at.

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

## Opening books

A search answers one question about one position. A book answers the one a
player asks: what should I play, what can the opponent do about it, and how far
down does that stay true. The `book` binary builds and reads one.

```sh
cargo run --release --bin book -- build --mode infiltration --max-ply 50
cargo run --release --bin book -- show --plies 24
cargo run --release --bin book -- study --line "d3-d4 d7-d6 e3-e4"
```

To build the books the website actually ships, use the script rather than the
binary directly. It runs the whole sequence for every mode -- survey, deepen,
compact, export, check -- with tapers that interlock, and it archives a book
built under superseded weights instead of stopping on the error that book would
otherwise raise at the first command:

```sh
scripts/build_books.sh                    # both modes, 80e9 nodes each
scripts/build_books.sh --budget 20e9 v5   # one mode, quick pass
scripts/build_books.sh --publish          # build, then PUT to the website
```

Depth fifty is a property of the book, not of a search. Node counts roughly
double per ply, so a single depth-50 search of the Infiltration start is around
3e16 nodes; what a book gives instead is a fifty-ply graph in which *every*
node carries its own depth-13-to-22 search. Each ply gets a fresh horizon and a
fresh quiescence, which is worth more than the tail of one nominal deep line.

Four decisions make that affordable and reusable:

1. **Positions are keyed by a canonical representative.** Which symmetries a
   mode admits is a property of its *goal*, and `ModeRules::symmetries` is the
   authority; `Position::canonical_over` intersects whatever a caller asks for
   with that set, so no configuration can fold a mode further than its rules
   allow. Reversing files is exact where the goal is a whole rank or absent --
   neighbourhoods are eight-connected, the Infiltration targets are whole rows,
   and no evaluation term depends on a file -- so there a position and its
   mirror share one analysis, and Infiltration's 23 first moves are 12
   positions. `--symmetry full` additionally folds the colour swap; it is sound
   but merges almost nothing, because flipping the side to move puts the image
   on the wrong parity for a game with a fixed first player.

   **Intransitive folds nothing.** Its goal is a single corner, so reversing
   files carries that corner to the far side of the board and is not a symmetry
   of the rules at all; the half turn is, but it swaps colours and so hits the
   same parity wall. An Intransitive book is about twice the size of an
   Infiltration book covering the same ground, and that is the right size.
2. **Lines that are already refuted are not deepened.** A move more than the
   ply's margin worse than best play drops out, exactly as `1.e4 e5` is theory
   to move twenty-five and `1.e4 h5` is not. Regret accumulates along a line,
   so the tree stays narrow where it should.
3. **Budgets are node counts, tapered by ply, never wall-clock** -- the same
   rule the arena follows, and for the same reason. `--taper` makes the shape
   data: `<from-ply> <nodes> <multipv> <margin>` per line. `scripts/deep.taper`
   is the built-in shape written out; `scripts/survey.taper` ranks every first
   move instead of deepening one; `scripts/openings.taper` is what the shipped
   books are built with, and is steeper than the built-in shape on purpose.
   The frontier is ordered by accumulated regret rather than by ply, and only a
   position already in the book can be expanded, so exactly one unanalyzed
   position has zero regret: the tip of the main line. **The main line therefore
   advances one ply per batch no matter how large the batch is.** What a ply
   costs near the root is what sets how deep the whole run gets, which is why
   the shipped shape leaves the 300M band at ply 1 rather than at ply 6, and why
   `--batch` defaults to the thread count in the script -- a batch four times
   that size buys the same one ply for four times the nodes.
4. **The log is append-only.** An overnight run that is interrupted keeps
   everything it finished, and re-analyzing a position at a larger budget
   appends rather than rewrites. `book compact` drops superseded records.

Give the workers **small** tables. A single search is happy with a large one,
but twelve of them doing random access into half a gigabyte apiece saturate
memory bandwidth before they saturate the cores. Measured on a 14-core machine,
twelve concurrent 150M-node Infiltration searches:

| table each | aggregate | depth reached |
| --- | --- | --- |
| 512 MiB | 34.5 Mnps | 20 |
| 128 MiB | 42.5 Mnps | 20 |
| 32 MiB | 45.7 Mnps | 20 |

Same depth, a third more throughput, so `--hash-mb` defaults to 64 rather than
to the largest number that fits. A single `rpsfish search` is the other case
and should be given as much table as it wants.

A book is improved as well as extended. A run re-queues any position whose
stored record falls below what its ply is now worth -- fewer nodes or fewer
ranked moves than the taper asks for -- at most once per run. Records then
merge rather than replace: values come from the deeper search, and moves only
a wider one listed are added with their scores clamped to the deeper search's
weakest line, because a MultiPV search that left a move out established that it
is worth no more than the last one it kept. Widening a book therefore cannot
lose depth, and deepening it cannot lose breadth.

`meta.txt` records the mode, `RULES_VERSION`, engine version, symmetry, and a
fingerprint of the evaluation weights. A book refuses records produced under
different weights or rules rather than mixing them, because those scores are
not comparable and the difference between two conventions would read as a
difference between two moves.

The graph has cycles, because pieces walk back over their own tracks. Backing
values up cuts a repetition to a draw rather than assuming a tree, and only
memoizes a value when no cycle was cut underneath it. That is the pragmatic
answer to graph history interaction, not a complete one: it never invents a
value, but a position whose only refutation runs through a repetition can be
scored differently from two different lines.

`rpsfish search --line "d3-d4 d7-d6"` searches any position by replaying moves
into it, keeping the moves before it as repetition history.

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
- A book is built offline. `book export` turns its canonical graph into the
  website's real-coordinate one; `nodes.log` and `paths.log` remain the
  authoritative artifact. The exported graph is about twice the size of the
  book, because folding a position and its mirror into one analysis is a
  statement about *analysis*, and both images are still boards a player can
  reach.
- Book building is parallel across positions, one searcher per thread, each
  with its own table. A worker keeps its table between positions because
  sibling openings transpose heavily, so a per-position node count depends on
  what that worker looked at before it. Every record stores the depth and node
  count it actually reached, and a single-threaded run is byte-reproducible.
- Threefold repetition and stalemate are official draws in Total War and
  Infiltration, in the Go backend and in the engine. `RULES_VERSION` is 3 and
  `rpsfish_rules_version()` exports it across the WASM boundary; the ABI shape
  is unchanged at 3, but terminal results are not, so cached analyses must be
  keyed on both.
- **Intransitive is the exception, and it is a rule fact rather than a search
  setting.** `ModeRules::repetition_draws` is false there and
  `ModeRules::stalemate_loses` is true, so a repeat is ordinary play and the
  side that cannot move has lost. Both are read off the mode at the root of
  every search (`Searcher::begin_search`), because one searcher is reused
  across modes and must not carry one mode's rules into another's position.
- **The selective layer is switched off wholesale in Intransitive.** The null
  move, both futility rules, move-count pruning, the late-move reductions and
  the quiescence cutoff all assume that having to move is never worse than
  passing; a mode where being unable to move loses is zugzwang everywhere, and
  every one of those shortcuts can then report a forced loss as a small plus.
  `selective_search_agrees_with_exhaustive_search_on_forced_positions` is what
  catches it. The cost is depth in one mode, and it has not been measured — see
  the stale-artifact note below.
- Blue opens. `FIRST_TO_MOVE` in `lib.rs` mirrors `game.FirstToMove` in the Go
  rules, and `Position::starting` reads it.
- Intransitive's goal corners are a1 for Red and i9 for Blue: each side runs at
  the corner the *other* army is banked in front of.
- Evaluation weights that were introduced as measurable-but-unproven features
  are set to zero. The predator-scarcity buckets are the current example: see
  the note in `evaluation.rs`.

### Stale after the rules changed to `RULES_VERSION` 3

The rules move underneath fitted numbers and built artifacts, and neither has
been re-made. What is known to be stale, and why:

- **Every published opening book and opening graph.** A book is keyed by
  position hash and the hash includes the side to move, so the root of a book
  built at rules 2 is not the root of a game today — in every mode, because
  Blue now opens. The server degrades gracefully (`dealOpening` falls through
  to a random opening when the walk fails) rather than serving a wrong line,
  but no mode has a working book until the books are rebuilt with
  `scripts/build_books.sh` and re-exported.
- **Intransitive's fitted constants.** Its goal corners moved, which moves
  every corner term, and its search is no longer selective, which moves what
  a fixed node budget can see. `intransitive_*` in `evaluation.rs` and the
  website's matching difficulty-profile constant were fitted under the old
  rules and want re-measuring against a baseline (`arena --baseline`), not
  re-guessing.
- **The cost of losing the selective layer in Intransitive.** Unmeasured. The
  honest number is a node count or a depth at a fixed budget, from
  `rpsfish search`, against the same position under the other two modes.

## Export an opening book to the website

The native opening graph can be turned into a bounded, real-coordinate JSON
tree that the website imports directly:

```sh
cargo run --release --bin book -- export \
  --dir book/V3 --mode infiltration --format graph \
  --plies 40 --featured 3 --featured-plies 12 \
  --output book/export/V3-openings.json
```

Use `--output -` to stream JSON to stdout (for example, into the website's
admin import endpoint). Ranked alternatives, search metadata, cycle markers,
and the backed-up principal `mainLine` are included. Human names are not:
they are kept by the website so a new engine scan cannot overwrite them.

Two shapes come out of `book export`. `--format graph` is the default and is
what the website stores: a flat position graph, `rps-opening-book/v2`, where
every position appears once keyed by its hash in real board coordinates and a
move names its child by key. `--format tree` writes the older nested shape,
which is bounded by `--width` and `--max-positions` because it was meant to be
downloaded whole.

The graph is not bounded by either, because nothing downloads it. The website
stores it and serves a visitor one position at a time -- a 21 KB bootstrap and
then about a kilobyte per click, against 9.6 MB for the whole V3 graph. Keying
on the position rather than on the line is what lets two move orders that reach
the same board share one analysis; a tree has to write that position out once
per line that reaches it, which is why the tree export needed a cap at all.

`--featured` picks the openings that come down in the bootstrap: the best first
moves, each followed along its own principal continuation for
`--featured-plies`, so a recommended line can be clicked through with no network
at all.

`scripts/check_export.mjs` checks a file against the rules the website's import
enforces, and reports the depth the tree actually reached rather than the cap it
was given:

```sh
node scripts/check_export.mjs book/export/V3-openings.json --mode V3
```

The backend answers a malformed book with one 400 and no indication of which of
eight thousand positions was wrong, and by then the build has already been paid
for. The check mirrors the validation the website's import endpoint enforces;
if that validation changes, this one has to change with it.
