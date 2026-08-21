# RPSFish implementation plan

## 1. Decision summary

RPSFish will be a custom Rust engine inspired by Stockfish's deterministic
search architecture. It will not fork Stockfish and will not begin with a
learned policy or value network.

The first production engine will use:

- one compact rules/search core shared by all three modes;
- iterative-deepening negamax with alpha-beta pruning;
- a fixed-size transposition table;
- deterministic move ordering and capture quiescence;
- separate handcrafted evaluators for Annihilation, Total War, and
  Infiltration;
- a single-threaded WebAssembly build running in a browser Web Worker;
- the same Rust core compiled as a persistent native server worker;
- offline self-play and learned evaluation only after the deterministic engine
  is correct, measurable, and deployed.

The Go backend remains authoritative for accounts, clocks, matchmaking, and
legal move validation. An engine result is always a proposal that the server
must validate before it changes an online game.

## 2. Goals

### Product goals

1. Provide responsive computer opponents for every current game mode.
2. Prefer client-side computation for casual web games to avoid server cost.
3. Retain a native server option for ranked games, weak clients, native mobile
   clients, automated self-play, and analysis.
4. Support multiple difficulty levels without maintaining different engines.
5. Make engine releases reproducible and comparable by engine version, rules
   version, evaluator version, and search limits.

### Engineering goals

1. Exact, deterministic rules behavior with cross-language parity tests.
2. No heap allocation in move generation or make/unmake hot paths.
3. A compact position that is cheap to mutate and restore.
4. Search cancellation, time limits, node limits, and depth limits.
5. Stable APIs that do not expose internal bitboards to frontend or backend
   callers.
6. Native and WASM builds from the same source and evaluator weights.
7. Instrumentation sufficient to compare strength and performance by mode.

### Initial non-goals

- Forking or adapting Stockfish source.
- Multithreaded search.
- GPU inference.
- Training in clients.
- AlphaZero/PUCT search.
- Solving every mode to a game-theoretic result.
- Replacing the Go server's authoritative state and clock code.

## 3. Rules contract (resolved, `RULES_VERSION` 2)

The deterministic engine is only as valid as its terminal rules. Those rules
are now settled and implemented identically in Go and Rust.

- **Mode terminal rules:** annihilation, completed territory, and infiltration
  boundary arrival. These are part of `Position::outcome`.
- **Engine terminal rules, inherited by every mode:**
  - a player with no legal move **draws** (stalemate);
  - the third occurrence of a position on the game path is a draw.
- **Outside the engine position:** clocks, resignation, abandonment, and draw
  offers. Timeouts must not be treated as proof that the final board position
  was strategically lost when generating training labels.

`Position::adjudicate` returns the complete board answer including stalemate;
`Position::outcome` remains the cheap board-only test used inside search, where
the empty move list already reveals a stalemate.

Two consequences worth stating explicitly:

1. **Infiltration wipeouts are draws.** Infiltration has no annihilation win
   condition, so a side reduced to zero pieces simply has no legal move. That
   is now a stalemate draw rather than an undefined state. This resolves the
   previously open question rather than adding a mode-specific rule.
2. **Stalemate being a draw is what licenses the uncapturable-piece proof.**
   If no-legal-move were a loss, owning an uncapturable piece would not
   guarantee at least a draw, and the bound in section 8 would be unsound.

`NoMoveOutcome::Loss` is retained on `Searcher` so a future rule set can
declare the opposite, but no shipped configuration uses it.

### Mode capability table

Generic components must not branch on a specific `Mode` to decide a *rule*.
`Mode::rules()` returns a `ModeRules` value declaring what a mode can do, and
search, evaluation, and move ordering consult only that. Adding a rule set
means adding one `ModeRules` value plus its own evaluator and weights.

| Mode | annihilation loses | other loss condition | territory | boundary goal | immortality floor |
| --- | --- | --- | --- | --- | --- |
| Annihilation | yes | no | no | no | **yes** |
| Total War | yes | yes (territory) | yes | no | no |
| Infiltration | no | yes (boundary) | no | yes | no |

## 4. Mode strategy

### Annihilation

Search profile:

- tactical, lower branching factor, and capture-sensitive;
- cycles and mutually non-capturing survivor combinations are important;
- capture quiescence and repetition handling are mandatory;
- reduced-material tablebases may eventually be practical.

Initial evaluation features:

- piece-count difference;
- whether surviving types counter opposing surviving types;
- immediately available and threatened captures;
- mobility;
- distance between favorable attacker/defender pairs.

Later experiments:

- exact retrograde tablebases by material class;
- NNUE only if it beats the handcrafted evaluator at equal node/time budgets.

### Total War

Search profile:

- territory ownership is monotonic, but movement within owned territory can
  repeat forever;
- state hashing must include every territory owner, not just pieces;
- the evaluator must balance territory, future frontier access, survival, and
  annihilation threats;
- long horizons make learned evaluation especially promising later.

Initial evaluation features:

- territory difference;
- immediately claimable neutral destinations;
- neutral frontier access and mobility;
- piece-count and capture-pressure differences;
- bonus for reducing the remaining neutral count while ahead.

Later experiments:

- mode-specific NNUE;
- PUCT/policy-value search as a measured challenger, not an assumed upgrade.

### Infiltration

Search profile:

- a deterministic race with tactical blockers and captures;
- near-boundary moves require strong ordering and search extensions;
- raw geometric distance is useful but insufficient when the path is unsafe.

Initial evaluation features:

- closest safe distance to the target boundary;
- total advancement;
- number of pieces with legal forward progress;
- immediate boundary threats and forced defensive captures;
- mobility and remaining material.

Later experiments:

- mode-specific NNUE;
- a small policy head only if alpha-beta move ordering becomes a measured
  bottleneck.

## 5. Architecture

### Current crate layout

```text
RPSFish/
├── Cargo.toml
├── PLAN.md
├── README.md
└── src/
    ├── lib.rs          public API and shared constants
    ├── model.rs        colors, pieces, squares, moves, modes, outcomes
    ├── position.rs     compact bitboards, hashing, make/unmake, validation
    ├── rules.rs        move generation, terminal rules, perft
    ├── evaluation.rs   deterministic mode-specific leaf evaluators
    ├── search.rs       iterative deepening, alpha-beta, qsearch, TT, PV
    └── main.rs         diagnostic search/perft CLI
```

### Planned workspace expansion

After the core API stabilizes, split deployment adapters without moving hot
search code:

```text
crates/
├── rpsfish-core/       current library
├── rpsfish-worker/     versioned stdin/socket native worker
└── rpsfish-wasm/       wasm-bindgen API for a Web Worker
web/
└── engine.worker.ts    load, cancel, and message lifecycle
training/
├── selfplay/           native parallel game generation
├── arena/              candidate-versus-baseline evaluation
└── nnue/               future PyTorch training/export pipeline
fixtures/
└── parity/             Go-generated versioned JSON positions
```

Keeping deployment code outside the core prevents browser APIs, serialization,
and async runtimes from contaminating the search hot path.

## 6. Position representation

The API-facing Go `GameState` is deliberately not the search representation.
RPSFish uses 81-bit sets stored in `u128`:

```text
pieces[color][rock|paper|scissors] -> u128
territory[red|blue]                -> u128
side to move                       -> Color
mode                               -> Mode
ply                                -> u16
incremental Zobrist hash           -> u64
```

Important invariants:

1. All set bits are below square 81.
2. Piece bitboards never overlap.
3. Red and Blue territory never overlap.
4. Every piece has exactly one color and type.
5. The cached hash equals a full recomputation.
6. `make_move` followed by `unmake_move` restores every byte of the position.

Squares use the server's existing coordinates: `(0, 0)` is Blue's boundary and
`(8, 8)` is Red's boundary. Red infiltrates toward `y = 0`; Blue infiltrates
toward `y = 8`.

Move generation writes into a fixed-capacity `MoveList`. Make/unmake returns a
small `Undo` record containing the captured piece, territory claim, counters,
and previous hash. Search does not clone the entire position at every node.

## 7. Hashing and history

Zobrist keys cover:

- every `(color, piece type, square)` feature;
- Red and Blue territory on every square;
- side to move;
- game mode.

The key generator is deterministic and embedded, so identical positions hash
the same in native tests and WASM. The transposition table never serializes
across engine versions.

Repetition is path-dependent. Search maintains a hash stack containing the
history supplied by the caller plus the current search line. A repetition
adjudication returns directly and is not stored as a universal TT fact. Full
graph-history interaction remains a known advanced search issue and will be
covered by regression positions before aggressive pruning is introduced.

## 8. Search design

### Rule-derived score bounds

Facts implied by the rules are proofs, not heuristics, and belong in search
rather than in a weight that other terms can outvote:

- If `ModeRules::immortality_prevents_loss` and a side owns an uncapturable
  piece, that side cannot lose. Its score is clamped at a draw.
- If both sides own one, neither can lose, the state space is finite, and
  repetition is a draw, so the game value is exactly zero and the subtree is
  pruned.

Clamping at the leaf suffices for the bound to hold at every ancestor because
immortality is monotone: extinction never reverses, so a side that is
uncapturable at a node is uncapturable in every descendant. That monotonicity
is asserted directly by a property test over random playouts.

`Searcher::set_rule_proofs(false)` exists only so the arena can measure what
the bounds are worth. A proof that cannot be A/B tested is a belief.

### Implemented baseline

1. Iterative deepening from depth 1 through the configured maximum.
2. Negamax alpha-beta with principal-variation root tracking.
3. Fixed-size, power-of-two transposition table.
4. Exact/lower/upper TT bounds and mate-score ply normalization.
5. TT move, tactical, killer, counter-move, per-colour history, and
   mode-progress move ordering, scored once per node rather than once per
   comparison.
6. Capture-oriented quiescence with immediate objective moves included,
   generated directly from a tactical destination mask.
7. Terminal scoring that prefers faster wins and slower losses.
8. Configurable third-occurrence repetition draw, scanned only over the
   current reversible run.
9. Configurable no-legal-move loss or draw.
10. Depth, node, wall-clock, and external atomic cancellation. The clock is
    sampled every 1,024 nodes, because in the browser build reading it is a
    call out to the host.
11. Principal-variation extraction through validated TT moves.
12. Aspiration windows at the root, widening on a fail.
13. MultiPV that keeps alpha-beta at the root: within each variation pass only
    the first move gets a full window.
14. A selective layer behind `Searcher::set_selective`: null-move pruning,
    reverse futility, move-count and futility pruning of quiet moves,
    logarithmic late-move reductions, internal iterative reduction, and delta
    pruning in quiescence.

Items 12 to 14 were on the deferred list below and moved up only after the
arena measured them. The selective layer as a whole is worth +141 Elo at a
20,000-node budget over 1,200 game pairs, and is positive in every mode. Its
aggressiveness is data, not a constant: `SearchTuning::reduction_scale` ships
at the largest value that measured no Elo loss rather than the one that printed
the largest depth.

### Deliberately deferred search features

Add these only behind benchmarks and self-play significance testing:

- razoring;
- singular and other extensions beyond immediate objective moves;
- static exchange evaluation for capture ordering;
- multi-threaded shared-table search;
- opening books and mode tablebases.

Chess-tuned pruning constants must never be copied blindly. Each optimization
must win mode-specific games under browser-representative time controls without
introducing correctness failures. Pruning margins are therefore expressed in
units of the mode's own material weight rather than in centipawns, because a
piece is worth 220 in Annihilation and 35 in Infiltration, and one fixed margin
would prune almost nothing in the first mode and almost everything in the
second.

## 9. Evaluation design

All evaluation functions first compute a Red-relative score and then normalize
it to the side to move for negamax. Terminal results are never handled by the
heuristic evaluator.

Evaluation requirements:

- deterministic integer arithmetic;
- symmetric results when colors and board orientation are mirrored;
- no allocation;
- feature components that can be logged separately during tuning;
- weights stored together with an evaluator version.

Handcrafted weights are a bootstrap mechanism, and they are now *data*:
`EvalParams` holds every weight, is settable by name or as a vector, and is
loadable from a file by both the CLI and the arena. Nothing in `evaluation.rs`
is a `const` that a tuner cannot reach.

Two rules follow:

1. **A new feature ships with a weight of zero.** Adding a feature asserts only
   that the property is measurable. The arena assigns its value.
2. **Symmetry is a test, not an intention.** `Position::mirrored` swaps colours
   and flips the board, and a property test asserts evaluation is invariant
   under it *for randomized weight vectors*, not just the defaults. Otherwise
   tuning can learn a colour bias that no anecdotal position reveals.

The predator-scarcity buckets are the worked example. They give search a
gradient toward extinction instead of a cliff at exactly zero predators, and
they demonstrated exactly the hazard this section exists to prevent: plausible
hand-picked positive weights measured **-52 Elo** in Total War, while SPSA
found *negative* weights that measured **+61 Elo** on an independent seed.

## 10. Public search API

The stable conceptual request is:

```text
SearchRequest
  protocol_version
  engine_version
  rules_version
  mode
  side_to_move
  pieces and territory
  prior position hashes
  limits { depth, nodes, move time }
  skill configuration
```

The result is:

```text
SearchResult
  best move or no move
  side-to-move score
  completed depth
  selective depth
  searched nodes
  elapsed time
  stop reason
  principal variation
  engine/evaluator version
```

Internal bitboards are not the long-term wire format. The worker and WASM
adapters will accept a versioned, validated snapshot that can evolve without
tying JavaScript or Go to Rust layout details.

## 11. Browser deployment

The browser engine will run in a dedicated Web Worker:

```text
React Native Web UI
        |
        | SearchRequest / cancel
        v
engine.worker.ts
        |
        v
rpsfish_wasm
```

Browser rules:

1. Never search on the main UI thread.
2. Start with one search thread and a bounded TT.
3. Feature-detect WASM and fall back to the server when loading fails.
4. Cancel when the game snapshot changes, the player leaves, or the tab becomes
   hidden.
5. Cache content-addressed WASM and evaluator assets with immutable headers.
6. Cap memory, CPU time, and consecutive searches to protect batteries.
7. Use both time and node ceilings; node limits make strength tests
   reproducible while time limits protect UX.
8. Treat client-computed AI as casual/unranked unless the server independently
   computes the move.

Multithreaded WASM is deferred. It requires cross-origin isolation headers and
separate deployment testing. A single-thread engine is simpler, portable, and
likely sufficient for the first 9x9 release.

## 12. Server deployment

The native engine will be a persistent ARM64 process rather than a library
loaded through `cgo` initially:

```text
Go bot manager
    |
    | versioned request over pipes or Unix socket
    v
RPSFish worker pool
```

The process boundary offers:

- crash and memory isolation;
- independent Rust and Go builds;
- easy concurrency limits on the Orange Pi;
- rolling engine upgrades;
- the same executable for self-play and analysis;
- negligible protocol overhead because messages occur once per root search,
  not once per node.

The Go server must validate that the returned move is legal for the exact
snapshot and discard stale results. Worker processes receive fixed CPU/memory
budgets and are restarted after protocol errors or crashes.

## 13. Go/Rust parity strategy

There must not be two silently diverging rule implementations.

The Go backend will generate versioned fixtures containing:

- mode and side to move;
- complete piece and territory state;
- terminal result, if any;
- every legal move in stable coordinate order;
- the resulting state after each legal move;
- perft counts for shallow depths where practical.

Fixture sources include:

1. all three starting positions;
2. every capture matchup;
3. each objective-ending move;
4. no-move and repetition cases;
5. seeded random legal playouts;
6. every historical production regression.

Rust tests consume these fixtures. Go tests retain their own expected results.
CI rejects any rules change until both implementations intentionally move to a
new `rules_version`.

## 14. Testing and verification

### Correctness layers

- Unit tests for coordinates, capture hierarchy, mode outcomes, and hashing.
- Make/unmake round trips for every starting move and random legal sequences.
- Perft snapshots for each mode.
- Property-style tests using fixed pseudorandom seeds and no external crate.
- Search tests for immediate wins, forced defensive moves, repetition, no legal
  moves, cancellation, and hard node limits.
- Differential parity tests against Go fixtures.
- Native debug, native release, and `wasm32-unknown-unknown` builds in CI.

### Strength verification

The `arena` binary is the adjudicator, and its rules are not negotiable:

- Node limits, never wall-clock limits, so results are reproducible across
  machines and thread counts.
- Paired openings with colours swapped inside each pair. Two identical engines
  must score exactly 0.5000 with exactly equal wins and losses; that is the
  arena's self-check.
- Random balanced openings, rejecting any whose static score already favours a
  side.
- Per-mode SPRT with explicit `elo0`/`elo1`/`alpha`/`beta`. A candidate ships
  on `ACCEPT`; `inconclusive` means keep playing.
- Independent confirmation at a different seed and node budget before a tuned
  weight becomes a default, because SPSA output is fitted to its own run.

- Maintain a fixed suite of tactical and strategic positions per mode.
- Play every candidate against the last released baseline with colors swapped.
- Report win/draw/loss, Elo estimate, confidence interval, nodes per second,
  average depth, and timeout/cancellation rates separately per mode.
- Require both short browser-like and longer server-like time controls.
- Reject changes that gain overall Elo by severely regressing one mode unless a
  mode-specific configuration explains the tradeoff.

### Performance verification

Track at least:

- legal moves generated per second;
- make/unmake pairs per second;
- perft nodes per second;
- search nodes per second by mode;
- TT hit/cutoff rates;
- qsearch fraction;
- average branching factor;
- WASM download size, initialization time, peak memory, and search latency.

Correctness comes before node speed. Benchmarks become release gates only after
stable representative positions exist.

## 15. Difficulty system

Difficulty should remain deterministic and explainable:

- hard levels use larger node/time/depth budgets;
- lower levels may choose among several root moves whose scores are within a
  configured window, using a request seed;
- intentionally bad moves must still be legal and must not come from corrupting
  the evaluator;
- the strongest level always chooses the completed search's best move;
- published difficulty presets include exact engine, evaluator, and limits.

Node budgets are preferred for repeatable ratings. Wall-clock caps remain a
second safety boundary on client devices.

Status: the shipped ladder implements this in the web client rather than in the
engine — profiles live in `frontend/engine/botProfiles.js` and the root sampler
in `frontend/engine/botEngine.js`, over the ordinary MultiPV WASM entry point.
Two consequences follow, and both are deliberate for now:

- The engine exposes no difficulty ABI, so `MAX_VARIATIONS` in `wasm.rs` is the
  ladder's real constraint on how wide a candidate set a weak profile can
  sample. It is 8 rather than 3 for that reason.
- `arena.rs` cannot measure the ladder, because the policy it would be
  measuring is not in this crate. `frontend/scripts/botArena.mjs` fills that
  gap with the same discipline — seeded, paired, colour-swapped, Elo with an
  interval — driving the shipped worker and WASM. Moving the sampler into Rust
  behind a seeded entry point would let one arena measure both, and would let a
  native-server bot play at the same strength as the browser one.

## 16. Learned evaluation phase

Learning begins only after deterministic self-play produces trustworthy games.

Planned pipeline:

1. Run versioned native self-play with randomized balanced openings.
2. Store sampled positions, root search scores, principal variations, and final
   outcomes.
3. Exclude corrupt games and identify timeout/resignation labels separately.
4. Train one small value/NNUE model per mode in Python.
5. Quantize and export a minimal versioned weight format.
6. Implement integer inference in Rust and verify it against Python vectors.
7. A/B test the learned evaluator against the handcrafted evaluator at equal
   time and node budgets.
8. Ship only statistically stronger models with bounded WASM size and latency.

Candidate sparse inputs are:

- 486 piece-square features (`81 × 2 colors × 3 types`);
- 162 Total War territory-square features;
- side to move and compact mode-specific scalar features.

Policy/value MCTS remains an experiment after NNUE. It earns adoption only by
beating deterministic search under the actual browser compute and download
budget.

## 17. Observability and reproducibility

Every diagnostic result and self-play game records:

- engine semantic version and build commit;
- rules and evaluator versions;
- native/WASM target and enabled features;
- position hash;
- limits and stop reason;
- depth, nodes, elapsed time, score, and PV;
- deterministic random seed, if root weakening is enabled.

Release builds expose a short identity string. Server history may record it for
bot games, but private search internals should not be trusted when reported by a
client.

## 18. Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Rust and Go rules diverge | Versioned Go-generated differential fixtures and server validation |
| Cyclic games poison search or training | Official repetition/no-progress rules, path hashing, capped self-play |
| Handcrafted evaluator creates blind spots | Tactical suites, self-play A/B tests, later NNUE |
| Browser search freezes UI | Dedicated worker, cancellation, hard time/node limits |
| WASM is too slow or large | Compact dependency-free core, release size gates, server fallback |
| Native mobile cannot run web WASM | Server fallback first; Rust native Expo module later |
| Client tampers with ranked AI | Server computes all rating-affecting bot moves |
| Aggressive pruning introduces tactical bugs | Add one optimization at a time behind regression and Elo gates |
| Orange Pi overload | Persistent bounded worker pool and queue/backpressure metrics |

## 19. Milestones and acceptance criteria

### M0 — Rules freeze and green baseline

- Go backend tests pass.
- No-move, repetition, and Infiltration elimination behavior are explicit.
- Starting layouts and `rules_version` are fixed.

### M0.5 — Rules resolved (implemented)

- Stalemate is a draw in Go and Rust; `RULES_VERSION` is 2.
- `ModeRules` capability table drives every generic rule decision.
- The stale Go starting-position tests are repaired and derive their squares
  from each mode's declared layout.

### M1 — Deterministic core (implemented in the initial scaffold)

- Compact state for all modes.
- Legal moves, capture hierarchy, mode outcomes, make/unmake, and hashing.
- Unit tests and shallow perft.
- No external dependencies.

### M2 — Search baseline (implemented in the initial scaffold)

- Iterative-deepening alpha-beta, TT, qsearch, move ordering, limits, PV.
- Handcrafted mode evaluators.
- Diagnostic CLI that returns legal moves in every starting mode.

### M3 — Cross-language parity

- Go fixture generator and Rust fixture consumer.
- Thousands of seeded positions per mode agree on moves and transitions.
- CI validates both languages.

### M4 — Native worker and arena

- Versioned worker protocol and Go process pool.
- Automated self-play, paired openings, Elo reports, and crash recovery.
- ARM64 deployment and resource limits.

### M5 — Browser WASM

- Worker-based Expo Web integration.
- Cancellation and auto server fallback.
- Performance and memory gates across Chrome, Safari, and Firefox.
- Casual bot mode released without affecting ranked ratings.

### M6 — Search strengthening

- Curated regression suites.
- Only statistically validated pruning, extensions, and tuning changes.
- Published difficulty presets.

### M7 — Learned evaluator

- Reproducible self-play dataset and Python training pipeline.
- Quantized Rust inference matching Python vectors.
- Mode-specific NNUE beats the deterministic baseline under client limits.

### M8 — Optional advanced search

- Tablebase feasibility study for Annihilation.
- PUCT benchmark for Total War and Infiltration.
- Multithreaded server and browser experiments based on measured need.

## 20. Immediate next work

Completed since this plan was written:

- Go starting-position tests repaired and made layout-derived.
- Official no-move (stalemate draw), repetition, and Infiltration elimination
  rules decided and implemented in both languages.
- Rust move/outcome semantics reviewed against those decisions.
- Release-native search benchmarked (`cargo run --release --example evalbench`).
- Weights extracted to `EvalParams`; arena with paired SPRT and SPSA shipped.

Remaining:

1. Add a Go parity-fixture exporter and consume it from Rust tests, including
   stalemate and repetition cases now that both are official.
2. Establish per-mode perft constants from those fixtures.
3. Tune the full weight vector per mode, not just the scarcity buckets, then
   confirm each result on an independent seed.
4. Add the native worker protocol before wiring any UI.
5. Investigate the Annihilation self-play truncation rate: roughly a fifth of
   games reach the 400-ply ceiling, which suggests a no-progress rule is worth
   specifying per mode.

