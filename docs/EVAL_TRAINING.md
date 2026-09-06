# RPSFish evaluation training run

An autonomous loop for improving RPSFish's evaluation in **Infiltration (V3)**
and **Total War (V5)**. The agent forms a hypothesis, implements it as data,
measures it against the current engine, and keeps it only if the measurement
says to. These are the only two modes the engine ships.

Everything below is written for an agent with no memory of how this engine
works. Read it all before the first change.

---

## 1. The one rule

**Nothing about strength is decided by reading a diff.** A change ships only
when a sequential probability ratio test says `ACCEPT` on a seed independent of
the one used to find it. `inconclusive` means keep playing, not "probably
fine." A plausible-sounding weight that measures neutral is a *finding*, not a
failure — record it and move on.

This is not a style preference. The repo has a worked example: hand-picking
plausible positive values for `total_war_scarcity_*` (30/12/4) measured **−52
Elo**; SPSA then found *negative* values that confirmed at **+60.7 Elo**. Human
intuition about these weights has been wrong by 110 Elo in a single case.

---

## 2. Where things are

Work in `RPSFish/`. It is its own git repo.

| path | what |
| --- | --- |
| `src/evaluation.rs` | Every weight and every feature. Most changes live here. |
| `src/model.rs` | `ModeRules` — the rule facts evaluation may branch on. |
| `src/position.rs` | Rule-derived facts (`is_immortal_kind`, `predator_count`). |
| `src/search.rs` | Search. **Do not change while measuring evaluation.** |
| `src/bin/arena.rs` | `match` (SPRT) and `tune` (SPSA). The measurement tool. |
| `examples/suite.rs` | Curated positions with proved verdicts. The §7 instrument. |
| `suite/*.txt` | The positions themselves, in the server's game-record FEN. |
| `examples/depthbench.rs` | Depth reached per budget, per mode. |
| `examples/evalbench.rs` | Evaluation cost and search speed. |
| `scripts/bench_web.mjs` | The **browser** build's real speed. Bots run there. |
| `EVAL_RESULTS.md` | Append-only log of every experiment. Create on first run. |

Constraints that are not negotiable:

- **No third-party dependencies.** `Cargo.lock` contains one package. Keep it.
- `cargo fmt --check`, `cargo clippy --all-targets` (zero warnings), and
  `cargo test` must be green before and after every change.
- **Never commit and never push.** Leave changes in the working tree.
- Evaluation must stay symmetric: `evaluate(p) == evaluate(p.mirrored())` and
  under file reversal. `tests/invariants.rs` enforces this; a feature that
  breaks it is wrong, not the test.

---

## 3. The layering rule this engine is built on

`src/evaluation.rs` documents three kinds of knowledge that must not mix:

1. **Facts** are derived from the rules and live on `Position`. Search may
   trust them as proofs. Example: `is_immortal_kind` — the capture hierarchy is
   one 3-cycle and no mode returns a piece to the board, so a kind whose
   predator is extinct can never be captured again. That is a theorem.
2. **Features** are countable properties, computed in `SideSummary::compute`.
   Adding one asserts only that the property is *measurable*, never what it is
   worth.
3. **Weights** are what a feature is worth. They live in `EvalParams` as data.

A new idea almost always enters as a **feature plus a weight defaulting to 0**,
which leaves the shipping engine byte-identical until a measurement earns the
weight a value. Never branch on a specific `Mode` to decide a *rule* — add the
fact to `ModeRules` instead. (Worked example: the zero-piece cliff is gated on
`rules.annihilation_loses`, which is false in Infiltration because a side with
no pieces is stalemated into a draw there rather than losing. The same absence
in Intransitive is a *loss*, because `rules.stalemate_loses` is true there —
two rule facts, no mode branch.)

---

## 4. Traps — read this section twice

Every one of these cost real time in a previous session. They are the reason
this document exists.

### 4.1 SPSA cannot move a weight that starts at zero

`arena tune` sets its perturbation to `(|value| / 5).clamp(4, 40)`. A weight
starting at `0` therefore perturbs by **±4 per iteration** and will crawl for
hundreds of iterations to reach a value of −400.

**Always seed a non-zero starting point before tuning**, via
`--baseline name=value`, then let SPSA refine from there. Or grid-search first
to find the region and tune second.

### 4.2 Exactly 0.5000 with equal W/L means the feature never fired

The arena's own sanity check is that two identical engines score exactly
`0.5000`. If a candidate scores `W 40 D 40 L 40`, the change did not alter a
single game — the feature is not firing in self-play, and no amount of extra
games will help.

This really happens. `total_war_army_doomed` (fires when *every* piece a side
owns is permanently huntable) scored exactly 40/40/40 over 120 games. The
pattern is real — it is precisely what a human exploits — but it does not occur
in engine self-play from random openings.

**Implication:** the arena measures average strength in self-play. It is blind
to patterns a human steers toward deliberately. A null result there does not
mean a defect is not real; it means the arena is the wrong instrument for it.
See §7.

**The sharper statement, learned the hard way:** the arena is blind to whatever
*both arms believe*. A weight both engines share cancels out of the score even
when it is actively harmful, and no sample size recovers it. The fix is not
always a position suite — `arena` takes `--baseline` as well as `--candidate`,
so you can hold an opponent that does *not* share the belief and measure
against it. `Options::search_differs` reads only the search switches, never the
weights, so a weights-only asymmetry leaves search identical on both arms and
the SPRT as valid as for any weight change. **Before trusting a weight, ask
what it measures against an opponent that does not have it.** This is how H9
found a 400 Elo defect that four symmetric grids had walked past.

### 4.3 The existing weights are near a local optimum

Do not assume the obvious story. In Total War the intuitive diagnosis — "the
bot over-values territory and trades its army away" — is **wrong**:

| candidate | Elo |
| --- | --- |
| `total_war_material=90` (from 70) | −65.0 |
| `total_war_material=110` | −117.8 |
| `total_war_material=140` | −117.2 |
| `total_war_territory=16` (from 28) | −5.8 |
| `total_war_territory=10` | −20.9 |

The shipped balance of territory 28 / material 70 beats every nearby
alternative. Start from the assumption that the current values are good and
that a gain has to be *found*, not reasoned out.

**Superseded in part — read this before trusting the table above.** Every row
in it moves *one* coordinate with the others held at their shipped values, and
that is why it missed a 400 Elo defect sitting in the middle of it. Total War's
four territory weights (`territory`, `frontier`, `claim_squares`, `lead_scale`)
are one subsystem, and the minimum is at the corner of the four-cube, off every
axis this table explores: with all four zeroed the engine gained **367.7 Elo at
20k nodes and 436.4 at 150k**. The territory evaluation has since been replaced
by a bounded term (see EVAL_RESULTS.md H9). A coordinate-wise grid can find a
bad coordinate; it cannot find a bad subsystem. The intuitive diagnosis *was*
right — the mode over-valued territory — and every measurement here still
agreed with it, because the flaw was in the measurement's shape, not its
arithmetic.

### 4.4 One test position cannot compare two terms

If two candidate terms each fire exactly once in your probe position, they
produce an identical constant offset and the position tells you nothing about
which is better. Terms are distinguished by *where else* they fire. Compare
them over a position suite or over games, never over one position.

### 4.5 Sample size

600 games gives roughly a ±25 Elo confidence interval. A real 10 Elo gain is
invisible at that size. Budget **2000+ games** (±13 Elo) for anything you
intend to ship, and treat short runs as screening only.

### 4.6 Node budget transfers badly

`arena` defaults to 20,000 nodes/move — roughly depth 8–10. The shipped bot
plays at depth 13–19. A weight tuned at 20k nodes may not help at 10M. Screen
cheap, then **confirm the winner at `--nodes 500000` or higher** before
shipping.

### 4.7 Do not change search while measuring evaluation

`arena` compares two weight vectors. If you also change search, `search_differs`
makes the comparison meaningless. One variable at a time.

---

## 5. The loop

Repeat until the budget runs out. Log every iteration to `EVAL_RESULTS.md`,
including the failures — a recorded null result stops the next agent repeating
it.

**Step 1 — Pick one hypothesis.** State it as a sentence about the game, not
about a number. "A piece whose hunter can never be hunted back is worth less
than a piece that can defend itself" is a hypothesis. "Try −200" is not.

**Step 2 — Implement it as a feature with a zero weight.** Add the counter to
`SideSummary`, the weight to the `eval_params!` block with default `0`, and a
unit test in `src/evaluation.rs`'s test module that pins *when it fires* by
setting the weight to `-1000` and asserting the exact score delta. Confirm
`cargo test` is green and that `arena show` prints the new weight at 0.
(`rpsfish search --show-params` prints them too; `--show-params` and
`--params-file` are flags on the `rpsfish` binary and on
`examples/suite.rs`, but *not* on `arena`, which takes `--candidate` and
`--baseline` instead.)

**Step 3 — Screen with a grid.** 300 game pairs per point, one seed, several
magnitudes. You are looking for a region, not a value.

```bash
cargo run --release --bin arena -- match --mode infiltration --games 300 \
  --threads 12 --seed 4242 --candidate infiltration_myweight=-200
```

If every point is inside the noise band and none is promising, stop — record
the null and go to the next hypothesis. Do not tune a weight that shows no
signal.

**Step 4 — Tune.** Seed the best grid point as the baseline (see §4.1) and let
SPSA refine, alone or alongside related weights.

```bash
cargo run --release --bin arena -- tune --mode infiltration \
  --baseline infiltration_myweight=-150 --params infiltration_myweight \
  --iterations 60 --pairs 24 --out tuned.txt
```

**Step 5 — Confirm on an independent seed.** This is the step that decides.
Use a seed you did not tune on, 1000+ pairs, and the mode in question.

```bash
cargo run --release --bin arena -- match --mode infiltration --games 1000 \
  --threads 12 --seed 20260822 --candidate "$(tr '\n' ',' < tuned.txt)"
```

Ship on `ACCEPT`. Revert on reject. On `inconclusive`, either play more games
or record it as neutral and move on.

**Step 6 — Check the other mode.** A weight is per-mode, but a *feature* is
shared code. Confirm the mode you did not tune has not regressed.

**Step 7 — Record.** Append to `EVAL_RESULTS.md`: the hypothesis, the code
change, every measurement with its seed and game count, and the verdict. Then
return to Step 1.

---

## 6. Ideas worth trying

Not instructions — starting points. Kill any of them cheaply if the screen is
flat.

- **Doomed material.** `*_doomed_piece`, `*_army_doomed`, `*_last_piece` and
  `*_two_pieces` are implemented and stay at 0: H1 gridded all four in the
  arena and every point measured inside or below the noise band. Their
  *conjunction* is a different feature and did ship — see H8. Do not re-grid
  the four singly.
- **`*_immortal_survivor` is zero in both modes** — the since-removed V1
  shipped 160. The feature fires when a side owns any uncapturable piece —
  i.e. cannot be annihilated. Its being zero may be a gap or may be correct.
- **Infiltration scarcity buckets are all zero**, abandoned after a 30-iteration
  SPSA run drifted to −1/4/−2. Total War's equivalents tuned to −6/−6/+5 and
  are worth +60 Elo. A longer Infiltration run may find the same.
- ~~**Non-linear territory in Total War.**~~ **Done — see EVAL_RESULTS.md H9.**
  The whole territory subsystem was re-priced: `total_war_territory`,
  `frontier`, `claim_squares` and `lead_scale` are all 0 and a bounded
  `total_war_territory_race` plus a majority-gated `total_war_majority_haste`
  replaced them, worth +276 Elo at bot depth. What is *still* open is the other
  half of the endgame race: `total_war_doomed_endgame`'s `race_length` clamps at
  zero from below, so it says nothing when the hunt arrives *after* the board
  fills — which is exactly the two V5 suite positions the new weights misread.
- **King-safety analogue for Infiltration.** The mode is won by reaching a row.
  `infiltration_goal_threat` scores a move that reaches it *now*; there is no
  term for an opponent who is two moves away and unstoppable.
- **Mobility is weighted 1–2 everywhere** and has never been tuned in isolation.

---

## 7. When the arena is the wrong instrument

The arena answers "is this stronger on average in self-play." Some real defects
do not show up there (§4.2). If you believe a defect is real but unmeasurable,
build a **curated position suite** instead:

1. Collect positions with a known correct verdict — ideally from real games,
   verified by a very deep search (`--nodes 2000000000`, which can reach depth
   24+ and often proves a result outright).
2. Score the engine on how many it gets right at *bot depth* (13–19), not at
   the depth used to establish the truth.
3. Report suite accuracy alongside Elo. A change that lifts suite accuracy
   without costing Elo is worth shipping even at 0 Elo.

This suite now exists, in `suite/`, and `examples/suite.rs` runs it:

```bash
cargo run --release --example suite -- --nodes 300000
cargo run --release --example suite -- --nodes 300000 --param total_war_doomed_endgame=0
```

Positions are written in the FEN the server stores in `game_pgn`, so one can be
pasted out of a real game unaltered. The report separates *searched* accuracy
from *static* accuracy on purpose: search rescues anything inside its horizon,
so the static column is the one a weight change moves and the one that says
what the evaluation actually knows.

Two rules the first suite earned the hard way:

1. **A verdict no search has proved does not belong in the file.** Three
   plausible-looking positions were built, could not be resolved at depth 26,
   and were dropped. One of them had a *guessed* verdict that later evidence
   contradicted. An unproved entry does not measure the engine; it measures the
   author.
2. **Vary one thing at a time across a family, not one position (§4.4).** The
   suite's race positions are the same pieces on the same squares with only the
   clock and the hunter distance changed. That is what turned a hand-picked
   constant into a measured quantity — see H8 in EVAL_RESULTS.md.

The flagship case below is what the suite was built around. It is now **closed**:

> **Total War.** Red has one Paper; Blue has two Scissors, a Paper, and a Rock.
> Red leads on territory 42–26 with 13 neutral tiles left. Red has no Rock and
> can never obtain one, so Blue's Scissors can never be captured and Red's only
> piece is huntable for the rest of the game. It is mate in 9.
>
> Static evaluation was **+11 (Red better)**, and depth 20 was needed before
> the engine saw the forced loss — seven plies past what any shipped bot
> reaches. `total_war_doomed_endgame` prices the race the position actually
> turns on; the same position now evaluates to −5301 statically and is read
> correctly at the review's own node budget.

---

## 8. Reference numbers

Measured on a 14-core Apple-silicon laptop, 24 GB. Re-measure rather than trust
these on other hardware.

**Native, single-threaded:** ~4.7–5.2 M nps in Infiltration; node count roughly
doubles per ply. Depth 18 ≈ 28 M nodes ≈ 5.4 s. Depth 24 ≈ 1.8 G nodes ≈ 6 min.

**Parallel:** give each worker a *small* table. Twelve concurrent 150 M-node
searches reached 34.5 Mnps at 512 MiB each and 45.7 Mnps at 32 MiB each, at
identical depth. Bigger tables are worse under parallelism.

**WebAssembly** (what bots actually run), 4-second budget:

| mode | nps | depth in 4 s |
| --- | --- | --- |
| Infiltration | 3.1 M | 19 |
| Total War | 2.7 M | 13 |

Infiltration depth cost: 17 → 1.5 s, 18 → 2.2 s, 19 → 3.0 s, 20 → 8.7 s.

**Total War is the expensive mode** — six plies shallower than Infiltration on
the same clock. Anything that makes Total War evaluation cheaper buys depth
directly, and depth is this engine's largest strength lever.

**Arena throughput:** ~120 games in 6 s at 20k nodes on 12 threads. 2000 games
is about two minutes. Measurement is cheap here; use it generously.

---

## 9. Stopping and reporting

Stop when the budget is spent or three consecutive hypotheses screen flat.

Final report: every hypothesis tried, its measurement, and its verdict;
the diff of what is being kept; confirmation that fmt, clippy, and tests are
green; and the honest bottom line — the total Elo confirmed on independent
seeds, per mode. If that number is zero, say so. A session that establishes
five things that do not work has done real work, and saying otherwise poisons
the next one.
