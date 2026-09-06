# Breakaway detection: can RPSFish know a race is already lost?

An investigation, not a plan of record. Nothing here is implemented.

`EVAL_TRAINING.md` §"King-safety analogue for Infiltration" names the gap this
would fill:

> The mode is won by reaching a row. `infiltration_goal_threat` scores a move
> that reaches it *now*; **there is no term for an opponent who is two moves
> away and unstoppable.**

The proposal under investigation: keep a per-piece table of the distance from
that piece to every square, maintain it across moves by adding a `{-1, 0, +1}`
mask, take a min over the three pieces of each kind, and read the answer off a
comparison of two tables.

---

## Verdict, up front

**The goal is worth pursuing. The mechanism is not — and it turns out not to be
needed.**

- The incremental update has a **fatal flaw**: the `±1` bound holds only for the
  table belonging to the piece that moved. Every move also changes the *terrain*
  for the other seventeen, and an obstructed king distance can change by an
  unbounded amount, including from infinite to finite.
  → [Why the ±1 mask does not hold](#why-the-1-mask-does-not-hold)
- The `±1` bound **is** exact for unobstructed distance — but there the whole
  table is a compile-time constant, so the machinery buys nothing. The update
  rule is right exactly where it is unnecessary, and wrong exactly where it is
  needed.
- **Rebuilding from scratch is already cheap enough.** One multi-source BFS over
  the whole board measures at ~19 ns, against ~61 ns for the entire current
  `evaluate()`. Eighty-one squares is too small for incrementalism to win, and
  the bookkeeping would cost more than the rebuild it replaces.
- Two ideas inside the proposal survive and are better than stated:
  **multi-source BFS** dissolves the "min over three tables" problem, and the
  two-table comparison genuinely *is* the whole answer — as a single fused
  sweep rather than a post-hoc compare.
- There is a **second trap, unrelated to the mechanism**, that would be easy to
  walk into: `arena` plays fixed-**node** matches, so it is structurally blind
  to how much an evaluation term costs. A term can pass SPRT convincingly and
  still lose Elo in the browser.
  → [The measurement trap](#the-measurement-trap)

Recommendation: **build the feature, drop the incremental state**, as a
stateless bitboard sweep gated the way `hunt_distance` is already gated.

---

## What it costs

Native `--release`, this machine. `evalbench` is the repo's own instrument; the
BFS figures come from a standalone micro-benchmark using the same geometry
(9×9 packed into a `u128`, `index = y*9 + x`).

| Operation | Cost |
| --- | --- |
| `evaluate()` — the whole current evaluation | **~61 ns** |
| One search node (Infiltration, ~5.0 M nodes/s) | **~200 ns** |
| `spread8` — one BFS layer, shift/mask form | 2.0 ns |
| `spread8` — one BFS layer, `NEIGHBORS[]` loop form | 2.4 ns |
| One multi-source BFS, all layers | **~19 ns** |
| Full bundle: 6 threat sweeps + 6 safe sweeps | **~127 ns** (worst case 186) |
| Gate: "is any piece near the goal at all?" | **1.2 ns** |

**Treat the nanoseconds as order-of-magnitude only.** `EVAL_RESULTS.md` records
three consecutive runs of one binary at 67.7, 59.5 and 58.7 ns and concludes:
*"Nodes per second and depth, which are the stable numbers, do not move."* Three
runs here gave 60.7 / 63.2 / 61.8 ns with depth pinned at 12 (Total War) and 15
(Infiltration) every time. The decisive measurement for anything below is
`depthbench`, on a real implementation — which does not exist yet.

Two things to read off the table.

**Ungated, the bundle is about twice the current evaluation and about 0.9× a
whole node.** Evaluation is currently about a quarter to a third of node cost
(61 ns of ~200). Adding 127 ns would take a node to ~330 ns and search from
~5.0 M nps to ~3.0 M. `EVAL_TRAINING.md` records that node count roughly doubles
per ply, so an effective branching factor near 2 makes a 1.7× slowdown worth
about **0.8 plies**. That is a lot to pay for one term.

**Gated, it is free.** The rejection test costs 1.2 ns, and the evaluation
already computes what it needs — `SideSummary::closest`, the forward progress of
the most advanced piece, is gathered in the same pass. In an opening nobody is
close and the term never runs; in the endgames where the race *is* the game, it
runs and 127 ns is cheap.

Counter-intuitively the bundle costs *more* on a sparser board (186 ns at six
pieces, 146 ns at eighteen): multi-source BFS from three seeds floods the board
in fewer layers than from one, so a full army is cheaper per sweep than a
skeleton. The worst case is the middlegame-to-endgame transition, which is also
where the term matters most. Worth knowing before tuning a gate.

---

## Why the ±1 mask does not hold

For a **fixed obstacle set** the bound is real. Distance is a metric, the piece
steps from `A` to an adjacent `B`, so `|d_B(s) − d_A(s)| ≤ d(A,B) = 1` for every
square `s`. The mover's own passability is unchanged too — it could stand on `A`
and it can stand on `B` — so **for the moving piece's own table the update is
exactly correct**.

It fails for the other seventeen, because a move is not only a change of source.
It is a change of terrain:

- the piece **vacates** `A`, which was blocking every friend and every enemy that
  could not capture it;
- it **occupies** `B`, blocking a different set;
- if it **captures**, it also removes a piece, unblocking `B` for everyone the
  victim was stopping.

A concrete counterexample on this board. A red rock is blocked by all eight of
its friends and by every blue rock and blue paper — fourteen candidate blockers
against nine files, so a wall spanning a whole row is constructible. With the
wall complete, the rock's distance to everything beyond it is **infinite**. Move
one wall piece one square off the row and it becomes about four. One king step,
one table, `∞ → 4`. No `{-1, 0, +1}` mask expresses that.

Partial walls give the same effect in miniature: a corridor mouth plugged by one
piece adds however many moves the detour costs, not one.

This is not patchable. Maintaining shortest paths under arbitrary vertex
insertion and deletion is dynamic all-pairs shortest paths — a genuinely hard
problem, and hopeless to beat a rebuild that costs 19 ns.

### Where the bound *does* hold, it is free anyway

On an empty board the distance is Chebyshev, `max(|dx|, |dy|)`, and the `±1`
claim is exact. But then the table needs no maintenance at all:

```rust
/// Unobstructed king distance between any two squares. 6,561 bytes, const.
pub const DIST: [[u8; 81]; 81] = build_distances();
```

A lookup. No state, no update, correct after every move including captures.
Worth adding on its own merits — but it removes the only case in which the
incremental scheme was correct.

---

## Why incremental loses to rebuilding here

Even setting correctness aside, the state does not fit this search.

`Position` carries exactly eight incrementally-maintained things: the six piece
bitboards, `occupancy`, `board: [u8; 81]`, `empty`, `territory`, two count
pairs, `ply` and `hash`. About 288 bytes, `Copy`. `Undo` is seven fields —
a move, a piece, an optional capture, a territory flag, the side, the ply, the
hash — around **24 bytes**. `make_move_unchecked` is a couple of dozen
operations; `make_null_move` is two. That is how the engine reaches 5 M
nodes/second.

The proposed state is **18 tables × 81 entries = 1,458 bytes**: sixty times the
undo record. Alpha-beta is a depth-first walk that makes and unmakes at every
node, so it must be copied per node or undone by a delta — and the deltas are
the unbounded ones above. Copying 1.4 KB per node is already on the order of the
rebuild it is avoiding, and the rebuild is *correct*.

There is also a maintenance cost the codebase makes explicit. `Position` comments
note that `validate()` and `PartialEq` both cover the cached `occupancy` field
precisely so *"a desynchronized value cannot survive a make/unmake round-trip
test"*. Any new incremental table inherits that obligation: a field on `Undo`, a
branch in `validate`, a term in `PartialEq`, and coverage in the invariant tests.

And there is nowhere clean to hang it. Null-move search flips the side without
touching the board. Transposition hits return before reaching a leaf. The search
re-enters the same position by different move orders. Each is an opportunity for
incrementally maintained state to drift.

The general rule this is an instance of: **incremental evaluation pays when the
rebuild is superlinear in something large.** Here the rebuild is a handful of
shift-and-mask operations over a fixed 81-bit board.

---

## What to build instead

The proposal's real insight — *keep it as tables, then do one simple operation on
two tables* — is right. It wants a different shape.

### 1. Multi-source BFS dissolves the min-over-three

Instead of three tables per kind plus a min, **seed the BFS with all three pieces
at once**. One sweep gives, for every square, the fewest moves in which *any*
piece of that kind can stand there — the min, computed rather than maintained.
Eighteen sweeps collapse to six: one per side per kind.

### 2. `spread8` makes a layer a few instructions

There is no shift-based neighbour expansion in the repo today. Every expansion
is `NEIGHBORS[index]` inside a `trailing_zeros` loop — five call sites, in
`rules.rs` (movegen, `has_any_legal_move`, legality) and one in `evaluation.rs`.
There are no file masks. For expanding a whole frontier at once:

```rust
#[inline(always)]
const fn spread8(b: u128) -> u128 {
    let west = (b & !FILE_A) >> 1;
    let east = (b & !FILE_I) << 1;
    let row = b | west | east;
    ((row << 9) | (row >> 9) | west | east) & BOARD_MASK
}
```

(`BOARD_MASK` lives in `lib.rs`, not `board.rs`.) Measured at 2.0 ns against
2.4 ns for the table loop — a smaller win than expected, because frontiers are
sparse. Take it for the clarity, not the clock.

Note these are **constant** shifts. The `SQUARE_BITS` doc comment warns that
*variable* 128-bit shifts are expensive, "more than that on WebAssembly" —
constant shifts by 1 and 9 are a different animal. Even so, the wasm build
lowers `u128` to `i64` pairs and bots run at 2.7–3.1 M nps rather than the
native 4.5–5.0, so **the native ratios above must be re-measured in wasm**.
`scripts/bench_web.mjs` exists for exactly that.

### 3. The safe run is one fused sweep, not a compare

The proposal builds both tables and then compares them. Better: interleave them.
A runner is safe on square `s` after its own move `k` when

```
threat(s) > k + (side_to_move_is_runner ? 0 : 1)
```

That is **monotone decreasing in `k`** — arriving later is never easier — so a
square unusable at its earliest arrival is unusable at every later one, and a
plain layered walk with the filter applied per layer is both correct and
complete. Which gives the whole test as one loop:

```rust
let mut frontier = my_pieces_of_this_kind;      // multi-source
let mut seen = frontier;
for k in 1.. {
    let next = spread8(frontier) & allowed & !seen;
    seen |= next;
    if next & goal_mask != 0 { return Some(k); } // arriving wins before the reply
    frontier = next & !threat_within(k + tempo); // cumulative union, built as we go
}
```

This answers the *exact* question rather than an approximation, and the two
sweeps together are the ~38 ns in the table.

*(For the record: restricting to shortest paths does collapse safety to a purely
pointwise test, `threat(s) > d(s) + tempo`, since `k = d(s)` on a shortest path.
That is the "simple operation on two tables" the proposal was reaching for, and
it is a sound sufficient condition. The fused sweep costs the same and is exact,
so there is no reason to take the weaker one.)*

### 4. Gate it the way `hunt_distance` is gated

There is already a precedent in the evaluation for a scan too expensive to run
always. `hunt_distance` is a Chebyshev scan reached only when one side is down to
a single permanently-huntable piece, and its comment says so: *"rare enough that
the scan costs nothing in an ordinary position."* The gate itself is free —
*"Both operands are already counted for other terms, so the conjunction costs
one multiply and no extra pass over the board."*

A breakaway sweep gated on `closest` (already gathered), on the mode being
Infiltration, and on piece count follows the established shape exactly. An
ungated BFS at every leaf does not — and note where it would land: `leaf_score`
is called at **every non-PV interior node and every qsearch node**, with no eval
cache and no lazy-eval escape hatch. The static evaluation is the *input* to
reverse futility and null-move pruning, so it is computed before those can save
you.

If the term turns out to need a lazy-eval short-circuit to be affordable, that is
a **`search.rs` change**, and `EVAL_TRAINING.md` §4.7 says not to touch
`search.rs` while measuring evaluation. It would have to land and be measured as
its own experiment first.

---

## What it would buy

### The term the evaluation is missing

`breakaway(side) -> Option<u8>` — the fewest moves in which some piece of that
side reaches the goal row by a route no predator can cut off. Compare the two
and count the tempo: the side to move arrives on ply `2n − 1` and the other on
`2n`, so the side to move is first whenever its run is no longer than its
opponent's. Bounded, symmetric, and it sees what the search cannot.

Be clear about where the value is. At depth 15 the engine already resolves a
four-move breakaway tactically — eight plies is well inside the horizon. **The
term only earns its keep on long, quiet races**, which is exactly the case
`EVAL_TRAINING.md` describes and exactly where a horizon fails.

### Which piece is holding the position

This is the strongest part of the proposal and it nearly falls out for free. The
threat sweep is a multi-source BFS over at most three predators. Re-run it with
one of them left out; if a breakaway appears, **that piece is what is holding the
race together**.

From at most three extra sweeps (~60 ns, and only once the gate has fired):

- a capture-target bonus aimed at exactly the right enemy piece;
- **move ordering** — try captures of the holding piece first;
- **trade evaluation** — "trading this pair opens a breakaway" becomes something
  the engine reads rather than something it must search into.

The extinction case is already handled: `Position::is_immortal_kind` is a single
bitboard test, and a piece whose predators are all gone can never be intercepted,
so its breakaway distance is just its blocked distance. The new value is the
*non*-extinction case — three papers on the board, and removing one specific one
opens the road.

This part belongs at the root and the first few plies, as move ordering or
null-move threat extraction, not at every leaf.

### A nearly free by-product

The union of layers `≤ k` popcounts to "squares this kind can stand on within `k`
moves" — a multi-move generalisation of the existing depth-1 `claim_squares`,
which was itself worth **+97.7 Elo** when it replaced the move-counting version.
Whether depth-3 reach adds anything over depth-1 is empirical, but it costs one
popcount once the sweep has run.

---

## The measurement trap

This deserves its own heading because it is the thing most likely to produce a
confident wrong answer.

**`arena` plays fixed-node matches.** The default is 20,000 nodes per move, and
the SPRT verdict is computed from games where *both* engines pay the same
per-node price. An expensive evaluation term therefore costs nothing in the
arena. Its entire downside is invisible to the instrument that decides whether
it ships.

Meanwhile the deployment that matters — the bot in the browser — has a fixed
four-second *clock*. There, a slower evaluation converts directly into fewer
plies.

So a breakaway term could plausibly:

- pass SPRT at 20k nodes with a clean positive LLR,
- pass again at `--nodes 500000`,
- and still lose Elo in the browser, because it costs 0.8 plies of a 19-ply
  search.

Every arena result must be paired with `depthbench --time-ms 3000` (depth per
wall clock, which is what an expensive term degrades) *and* `depthbench --nodes`
(does the better-shaped score prune more?), and finally `scripts/bench_web.mjs`
for the wasm build the bots actually run.

The encouraging precedent: `EVAL_RESULTS.md` records a change that cost **+1.2%
per evaluation and returned two plies in Total War at the same node count**,
"because the better-shaped score prunes more". A term that makes the engine
understand races could plausibly pay for itself the same way. That is the
outcome to test for, not to assume.

---

## Risks, gaps, and things that would bite

**The model is optimistic for the runner in two independent ways, and false
positives are the dangerous direction** — a search steers toward positions whose
evaluation it can fool.

1. **Interception is not only capture.** An enemy piece that merely *stands* in
   the corridor stops the runner without taking it. The sweep treats blockers as
   static, so it never sees a blue rock step into the path. Folding "any enemy
   piece that can occupy `s` by move `k`" into the threat union fixes this and
   costs one more sweep per runner kind. **The website's Reach tool (a separate
   project) has the same gap** and should get the same fix.
2. **Own pieces are static too**, which cuts the other way and produces false
   negatives. Less dangerous — it under-rates real chances.
3. **Tempo is assumed free.** The runner is credited with spending every move
   running. If it must answer a threat elsewhere, the run is slower.

Mitigations worth building in from the start: trust *"cut off"* more than *"clear
run"* — make the term mostly a penalty; clamp its magnitude the way
`total_war_doomed_endgame` is clamped; require a margin before paying a bonus.

**Double-counting.** `infiltration_goal_threat` (180), `infiltration_closest`
(32), `infiltration_advancement` (9) and the immortal/doomed family already price
parts of this. The weights will have to be refitted together, not bolted on.

**Scale-dependent constants go stale.** Moving evaluation weights invalidates
fitted constants elsewhere in the tuning pipeline, and the suite's `static`
column uses a fixed 300cp margin that is explicitly **not scale-invariant** —
`EVAL_RESULTS.md` warns it cannot fairly compare two evaluations of different
scales.

**Symmetry is enforced.** `tests/invariants.rs` requires
`evaluate(p) == evaluate(p.mirrored())` and invariance under file reversal. A
breakaway term is naturally symmetric, but the sweep must not, say, break ties
by lowest square index in a way that leaks orientation into a score.

**Rebuild and cache-bust the web engine.** An evaluation change moves neither
`RULES_VERSION` nor the ABI, so nothing fails loudly — but the browser caches the
wasm under `RPSFISH_ASSET_VERSION` in `engine/rpsfish/client.ts`, and a stale
cache means the site quietly keeps the old engine. The opening book is built with
the engine and may want rebuilding.

**Stored review accuracies were computed with the old evaluation.** Changing it
changes what past games would score. Not a blocker; worth knowing.

**House rules.** No third-party dependencies. `cargo fmt --check`, `cargo clippy
--all-targets` clean, `cargo test` green. Never commit, never push. New counters
land with the weight defaulting to **0** plus a unit test that pins when they
fire.

---

## Is it worth pursuing?

**Yes — with the mechanism replaced, and with the measurement done honestly.**

For: it fills a gap the tuning notes already identify; the measured cost is
~127 ns against a ~61 ns evaluation and a ~200 ns node; the gate that makes that
affordable costs 1.2 ns and reuses a number the evaluation already has; there is
a precedent (`hunt_distance`) for exactly this shape of gated expensive term; and
the "which piece is holding the race" derivative is a strong signal nothing in
the engine currently approximates.

Against: it is one more approximate term in an evaluation that already carries
forty-odd fitted weights; it is optimistic in the direction search exploits; the
instrument that would judge it is blind to its main cost; and every number has to
be re-earned through SPRT plus wall-clock benchmarks.

A staged approach that fails cheaply:

1. **`DIST[81][81]`, `FILE_A`/`FILE_I`, `spread8` in `board.rs`.** Small, useful
   on their own, no behaviour change, no weights.
2. **`breakaway(position, color)` behind the `closest` gate**, weight fixed at 0,
   with unit tests against hand-built endgames. Two the frontend tool is already
   verified against make good fixtures: red rock `e5` / blue paper `a1` is a win
   in 4 with 4 moves to spare; move that paper to `e3` and there is no safe run
   at all, cut off on `d4`.
3. **Measure before tuning.** `depthbench --time-ms 3000` and `--nodes 3000000`,
   native *and* wasm. If wall-clock depth drops by more than a ply and node-count
   depth does not improve, stop here — that is the honest failure point and it
   costs two days, not two weeks.
4. **One term, SPRT it**, then confirm at `--nodes 500000` and in the browser. If
   a clean breakaway differential is not worth Elo on its own, the elaborate
   versions will not be either.
5. Only then: the holding-piece analysis, as **move ordering** rather than
   evaluation, where its cost is amortised over a whole subtree instead of paid
   per leaf.

If step 3 shows the wasm penalty is much worse than the native ~1.7×, stop. The
browser is the deployment that matters, and a term that costs a ply there is not
worth a fractional-Elo gain in a fixed-node arena.

---

## Appendix: reproducing the numbers

Engine-side figures:

```bash
cd RPSFish && cargo run --release --example evalbench
```

The BFS figures come from a standalone micro-benchmark (kept out of the repo)
that copies only the board geometry, so it can be compiled with plain
`rustc -O` and compared against `evalbench` on the same machine.
