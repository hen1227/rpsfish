# EVAL_RESULTS.md

Append-only log of every evaluation experiment run against RPSFish, including
the nulls. Written for an agent with no memory of the previous session: a
recorded null result is what stops the next run repeating it.

Target modes: **Infiltration (V3)**, **Total War (V5)** and **Intransitive
(V6)** — the modes the engine ships. V6 arrived on 2026-09-02 and so appears
only in the last session. Rows below that name Annihilation (V1) predate its
removal and are kept as the record of runs that happened; that mode no longer
exists and cannot be re-measured.

Conventions:

- "games N" is the arena's own count, i.e. `--games P` plays `2P` games.
- Every row names its seed. A tuning seed and a confirmation seed are never
  the same.
- `arena` default node budget is 20,000/move unless a row says otherwise.
- Hardware: 14-core Apple silicon, 24 GB. 12 arena threads throughout.

---

## Session 2026-08-22

Baseline: working tree as found, `EvalParams::DEFAULT` unchanged. `cargo fmt
--check`, `cargo clippy --all-targets` (zero warnings) and `cargo test` all
green before the first change.

### 0. Instrument check — does the arena still self-calibrate?

Identical engines must score exactly 0.5000 (§4.2).

| mode | games | seed | result |
| --- | --- | --- | --- |
| Infiltration | 240 | 4242 | W 120 D 0 L 120, score 0.5000 — calibrated |

### H1 — Doomed material: is a permanently-huntable piece worth less?

**Hypothesis.** A piece whose hunter can never be hunted back cannot be
defended, only run with, so it should be priced below a piece that can trade
itself off. The four features already existed at weight 0 and had never been
tuned (§6).

**Code change.** None. `*_doomed_piece`, `*_army_doomed`, `*_last_piece` and
`*_two_pieces` are already implemented in `SideSummary::compute` with unit
tests in `src/evaluation.rs`.

#### H1a — Firing probe (§4.2)

Before spending games on a grid, check each weight actually changes games.
An extreme weight that still returns exactly 0.5000 with W == L means the
feature never fires in self-play and no sample size will help.

| weight | mode | probe | games | seed | result | fires? |
| --- | --- | --- | --- | --- | --- | --- |
| `infiltration_doomed_piece` | V3 | −5000 | 120 | 4242 | W 44 D 0 L 76, 0.3667 | yes, strongly |
| `infiltration_army_doomed` | V3 | −5000 | 120 | 4242 | W 60 D 0 L 60, **0.5000** | **no — inert** |
| `total_war_doomed_piece` | V5 | −5000 | 120 | 4242 | W 38 D 35 L 47, 0.4625 | yes |
| `total_war_army_doomed` | V5 | −5000 | 120 | 4242 | W 40 D 41 L 39, 0.5042 | barely |
| `total_war_last_piece` | V5 | −5000 | 120 | 4242 | W 41 D 39 L 40, 0.5042 | barely |
| `total_war_two_pieces` | V5 | −5000 | 120 | 4242 | W 36 D 41 L 43, 0.4708 | yes |

`infiltration_army_doomed` reproduces the §4.2 pattern exactly: a side all of
whose pieces are permanently huntable does not arise in Infiltration self-play
from random openings. Not tuned; no sample size can move it.
`infiltration_last_piece` / `infiltration_two_pieces` are gated off by
`ModeRules::annihilation_loses` in V3 by construction and were not probed.

#### H1b — Grids, 600 games per point, seed 4242, 20k nodes

Infiltration, `infiltration_doomed_piece`:

| value | score | Elo | 95% CI |
| --- | --- | --- | --- |
| −400 | 0.4075 | −65.0 | [−93.8, −37.1] |
| −200 | 0.4375 | −43.7 | [−72.0, −15.9] |
| −100 | 0.4592 | −28.4 | [−56.6, −0.7] |
| −40 | 0.4808 | −13.3 | [−41.3, +14.4] |
| **0 (shipped)** | 0.5000 | 0.0 | — |
| +40 | 0.5008 | +0.6 | [−27.3, +28.4] |
| +100 | 0.4908 | −6.4 | [−34.3, +21.4] |
| +200 | 0.4092 | −63.8 | [−92.6, −35.9] |
| +400 | 0.4000 | −70.4 | [−99.3, −42.5] |

Cleanly unimodal with the peak at the shipped value of 0. This is an
informative null, not a noisy one: the weight fires often, and every non-zero
value measured is worse.

Total War, `total_war_doomed_piece`:

| value | score | Elo | 95% CI |
| --- | --- | --- | --- |
| −400 | 0.4775 | −15.6 | [−40.3, +8.8] |
| −200 | 0.4842 | −11.0 | [−35.7, +13.6] |
| −100 | 0.4792 | −14.5 | [−39.0, +9.9] |
| −40 | 0.4942 | −4.1 | [−28.5, +20.3] |
| **0 (shipped)** | 0.5000 | 0.0 | — |
| +40 | 0.4892 | −7.5 | [−32.1, +17.0] |
| +100 | 0.4900 | −6.9 | [−31.4, +17.5] |
| +200 | 0.4633 | −25.5 | [−50.9, −0.4] |
| +400 | 0.4442 | −39.0 | [−65.1, −13.3] |

Total War, the zero-piece cliff and the army-wide term:

| value | score | Elo | 95% CI |
| --- | --- | --- | --- |
| `total_war_last_piece=-600` | 0.5042 | +2.9 | [−21.7, +27.5] |
| `total_war_last_piece=-300` | 0.5050 | +3.5 | [−21.1, +28.1] |
| `total_war_last_piece=-120` | 0.5025 | +1.7 | [−22.7, +26.2] |
| `total_war_last_piece=+120` | 0.4917 | −5.8 | [−30.4, +18.7] |
| `total_war_two_pieces=-300` | 0.4850 | −10.4 | [−35.0, +14.0] |
| `total_war_two_pieces=-120` | 0.4917 | −5.8 | [−30.3, +18.7] |
| `total_war_two_pieces=+120` | 0.4975 | −1.7 | [−26.3, +22.8] |
| `total_war_two_pieces=+300` | 0.4875 | −8.7 | [−33.6, +16.1] |
| `total_war_army_doomed=-300` | 0.5008 | +0.6 | [−24.1, +25.3] |
| `total_war_army_doomed=+300` | 0.4858 | −9.8 | [−34.7, +14.9] |

**Verdict: NULL. Nothing shipped; all four weights stay at 0 in both modes.**

Every point in every grid sits inside the noise band or below it, so by §5 the
family is not tuned further. The one flicker worth naming for the next agent is
`total_war_last_piece`, which is weakly positive at −120/−300/−600 (+1.7 to
+3.5 Elo) and negative at +120. If that is real it is a sub-5 Elo effect and
needs ~10,000 games to see, which is a worse use of a budget than an untried
hypothesis. Left at 0.

The wider lesson matches §4.2: the doomed-material pattern is real and is
exactly what a human exploits, but engine self-play from random openings does
not reach those positions often enough for the arena to price them. This is a
§7 problem, not a weight problem.

### H2 — Infiltration scarcity: is a piece one capture from immortality worth more?

**Hypothesis.** In a race mode, a piece facing exactly one surviving enemy
predator is one capture away from being permanently safe, and a permanently
safe piece can walk to the goal. That near-immortality should be priced above a
piece that three predators still hunt. The three Infiltration buckets ship at 0
after a previous 30-iteration SPSA run drifted to −1/4/−2 (§6).

**Code change.** None. `infiltration_scarcity_1..3` already exist.

#### H2a — Structured grid, 1000 games per point, seed 4242

The three buckets were first screened as directions rather than as independent
axes, because a uniform shift across all three buckets is just a material
adjustment in disguise and says nothing new.

| candidate (s1, s2, s3) | score | Elo | 95% CI |
| --- | --- | --- | --- |
| −6, −6, +5 (Total War's tuned shape) | 0.4655 | −24.0 | [−45.7, −2.5] |
| −12, −12, +10 | 0.4160 | −58.9 | [−80.9, −37.4] |
| −18, −18, +15 | 0.3235 | −128.2 | REJECT |
| −24, −24, +20 | 0.2505 | −190.4 | REJECT |
| +6, +6, −5 | 0.5015 | +1.0 | [−20.5, +22.6] |
| +12, +12, −10 | 0.4985 | −1.0 | [−22.6, +20.5] |
| −10, −10, −10 (uniform) | 0.4820 | −12.5 | [−34.1, +8.9] |
| +10, +10, +10 (uniform) | 0.4955 | −3.1 | [−24.6, +18.4] |
| 0, 0, +10 | 0.4890 | −7.6 | [−29.2, +13.8] |
| 0, 0, +20 | 0.4390 | −42.6 | [−64.4, −21.1] |
| **−12, 0, 0** | 0.4600 | −27.9 | [−49.5, −6.4] |
| **+12, 0, 0** | **0.5395** | **+27.5** | **[+6.0, +49.2]** |

Two findings. First, **Total War's tuned shape is actively harmful in
Infiltration** and gets worse the harder it is applied — the +60 Elo those
weights are worth in V5 does not transfer, so the §6 suggestion to look for
"the same" values there is answered: no. Second, bucket 1 on its own is
antisymmetric about zero (+27.5 / −27.9), which is what a real gradient looks
like and what noise does not.

#### H2b — Axis refinement, 1000 games per point, seed 4242

| candidate | score | Elo | 95% CI |
| --- | --- | --- | --- |
| `scarcity_1=+6` | 0.5190 | +13.2 | [−8.3, +34.8] |
| `scarcity_1=+12` | 0.5395 | +27.5 | [+6.0, +49.2] |
| `scarcity_1=+20` | 0.5005 | +0.3 | [−21.2, +21.9] |
| `scarcity_1=+30` | 0.5330 | +23.0 | [+1.5, +44.6] |
| `scarcity_1=+45` | 0.4590 | −28.6 | [−50.3, −7.1] |
| `scarcity_1=+60` | 0.4285 | −50.0 | [−71.9, −28.5] |
| `scarcity_1=+90` | 0.4155 | −59.3 | [−81.4, −37.6] |
| `scarcity_2=−20` | 0.4580 | −29.3 | [−50.9, −7.8] |
| `scarcity_2=−10` | 0.4655 | −24.0 | [−45.7, −2.5] |
| `scarcity_2=+10` | 0.4565 | −30.3 | [−52.0, −8.8] |
| `scarcity_2=+20` | 0.4340 | −46.1 | [−68.0, −24.6] |
| `scarcity_3=−10` | 0.5180 | +12.5 | [−9.0, +34.1] |
| `scarcity_3=−20` | 0.4995 | −0.3 | [−21.9, +21.2] |

Bucket 1 is positive across 6–30 and clearly negative from 45 up; the dip at
+20 is inside the ±21 Elo band and is noise, not structure. Buckets 2 and 3 are
worse than zero in both directions. Region: `scarcity_1` in [6, 30].

#### H2c — SPSA, seed 4242, 120 iterations x 32 pairs

Seeded at `infiltration_scarcity_1=16` per §4.1 rather than at 0.

```
tuned: infiltration_scarcity_1 16 -> 15, scarcity_2 0 -> 1, scarcity_3 0 -> -2
```

SPSA stayed inside the grid's region instead of walking out of it, and left the
other two buckets at rounding noise. Cost note for the next agent: this run
took 26 minutes of wall clock for 7,680 games, against about 4 minutes for the
same number of games in `match`. SPSA re-joins its thread pool every iteration
and only had 32 pairs to spread over 12 threads, so it parallelizes badly.
Grid-then-confirm is far cheaper here than tune-then-confirm.

#### H2d — Confirmation on independent seeds

Seed 4242 was used to find and tune this weight, so it is not used again here.

| candidate | nodes | games | seed | score | Elo | 95% CI | LLR | verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `scarcity_1=15` | 20k | 3000 | 20260822 | 0.5337 | +23.4 | [+11.0, +35.9] | +2.62 | inconclusive |
| `scarcity_1=15, 2=1, 3=-2` (exact SPSA) | 20k | 3000 | 20260822 | 0.5342 | +23.8 | [+11.4, +36.3] | — | inconclusive |
| **`scarcity_1=15`** | **20k** | **5200** | **20260822** | **0.5339** | **+23.6** | **[+14.2, +33.1]** | **+4.58** | **ACCEPT** |
| `scarcity_1=15` | 150k | 1200 | 31337 | 0.5646 | +45.1 | [+25.6, +65.0] | +2.17 | inconclusive |
| **`scarcity_1=15`** | **150k** | **1800** | **31337** | **0.5603** | **+42.1** | **[+26.1, +58.2]** | **+3.03** | **ACCEPT** |

The exact SPSA vector and the single-weight version are statistically
indistinguishable (+23.4 vs +23.8 over the same 3000 games), and the grid
measured buckets 2 and 3 as worse than zero in isolation, so the simpler
single-weight version ships.

The §4.6 transfer check is the interesting row. At 7.5x the node budget the
measured gain nearly doubles, +23.6 to +42.1, on a third seed. This weight gets
*more* valuable with depth, which is the right direction for a bot that ships
at depth 19.

**Verdict: SHIP. `infiltration_scarcity_1: 0 -> 15`.**
Confirmed +23.6 Elo at 20k nodes and +42.1 Elo at 150k nodes, SPRT ACCEPT on
two seeds neither of which was used to find or tune the value.

Total War is untouched: the weight only enters `infiltration_score`. Identical
engines still score exactly 0.5000 after the change (400 games, seed 20260822),
so the arena remains calibrated.

### H3 — Infiltration over-values permanence: is an uncapturable piece worth less in a race?

**Hypothesis.** Infiltration is won by reaching a row, not by surviving. A
piece that can never be captured still has to walk nine ranks, and an
uncapturable piece that cannot advance wins nothing. The three survival weights
(`immortal_piece` 60, `immortal_with_prey` 30, `immortal_survivor` 0) are
priced against a mode where survival *is* the win condition, and against
`infiltration_material` 35 they make an uncapturable piece worth nearly three
ordinary ones. §6 asks whether the zero `immortal_survivor` is a gap; the
measurement says the opposite — the whole block is too generous.

**Code change.** None. All three weights already exist.

Baseline for this and every later Infiltration row is the shipped engine
*including* H2's `infiltration_scarcity_1=15`.

#### H3a — One weight at a time, 1000 games per point, seed 4242

| candidate | score | Elo | 95% CI |
| --- | --- | --- | --- |
| `immortal_survivor=-240` | 0.5670 | +46.8 | [+25.3, +68.7] |
| `immortal_survivor=-160` | 0.5510 | +35.6 | [+14.1, +57.3] |
| `immortal_survivor=-80` | 0.5320 | +22.3 | [+0.8, +43.9] |
| **`immortal_survivor=0` (shipped)** | 0.5000 | 0.0 | — |
| `immortal_survivor=+40` | 0.4800 | −13.9 | [−35.5, +7.6] |
| `immortal_survivor=+80` | 0.4675 | −22.6 | [−44.3, −1.1] |
| `immortal_survivor=+160` (Annihilation's value) | 0.4505 | −34.5 | [−56.3, −13.0] |
| `immortal_piece=0` | 0.5555 | +38.7 | [+17.2, +60.5] |
| `immortal_piece=15` | 0.5255 | +17.7 | [−3.8, +39.4] |
| `immortal_piece=30` | 0.5250 | +17.4 | [−4.1, +39.0] |
| `immortal_piece=45` | 0.5475 | +33.1 | [+11.6, +54.9] |
| **`immortal_piece=60` (shipped)** | 0.5000 | 0.0 | — |
| `immortal_piece=90` | 0.4790 | −14.6 | [−36.2, +6.9] |
| `immortal_piece=120` | 0.4715 | −19.8 | [−41.5, +1.7] |
| `immortal_with_prey=-30` | 0.5505 | +35.2 | [+13.7, +57.0] |
| `immortal_with_prey=0` | 0.5255 | +17.7 | [−3.7, +39.3] |
| **`immortal_with_prey=30` (shipped)** | 0.5000 | 0.0 | — |
| `immortal_with_prey=60` | 0.4755 | −17.0 | [−38.6, +4.4] |
| `immortal_with_prey=120` | 0.4630 | −25.8 | [−47.4, −4.3] |

Three weights measured independently, all monotone downward, none of them
previously suspected. The §6 guess that `immortal_survivor=0` might be a gap
next to Annihilation's 160 is refuted in the strongest available way: copying
Annihilation's 160 into Infiltration measures **−34.5 Elo**.

#### H3b — Combinations, 1000 games per point, seed 4242

| (piece, with_prey, survivor) | score | Elo | 95% CI |
| --- | --- | --- | --- |
| (30, 0, −80) | 0.5630 | +44.0 | [+22.5, +65.9] |
| (45, 15, −40) | 0.5465 | +32.4 | [+10.9, +54.1] |
| (15, 0, −120) | 0.5165 | +11.5 | [−10.0, +33.1] |
| (0, 0, −160) | 0.4895 | −7.3 | [−28.8, +14.2] |

The effect does not stack indefinitely. Zeroing the block outright and paying
−160 on top of it is *worse than shipped*, so this is a re-pricing, not a
removal. Region located; the shape inside it is unresolvable at 1000 games per
point (±22 Elo), which is what the tuner is for.

#### H3c — SPSA, seed 4242, 50 iterations x 60 pairs

Seeded at (30, 0, −80) per §4.1.

```
tuned: immortal_piece 30 -> 27, immortal_with_prey 0 -> 0, immortal_survivor -80 -> -72
```

Movement is inside the tuner's own rounding, so the round numbers ship.

#### H3d — Confirmation on independent seeds

| candidate | nodes | games | seed | score | Elo | 95% CI | LLR | verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **(30, 0, −80)** | **20k** | **3000** | **20260822** | **0.5522** | **+36.4** | **[+23.9, +48.9]** | **>2.94** | **ACCEPT** |
| `immortal_survivor=-240` alone | 20k | 3000 | 20260822 | 0.5368 | +25.6 | [+13.2, +38.1] | — | inconclusive |
| (30, 0, −80) | 150k | 1800 | 31337 | 0.5550 | +38.4 | [+22.4, +54.5] | +2.72 | inconclusive |
| **(30, 0, −80)** | **150k** | **2300** | **31337** | **0.5522** | **+36.4** | **[+22.3, +50.6]** | **+3.29** | **ACCEPT** |

The three-weight combination beats the best single weight on the independent
seed (+36.4 vs +25.6) and holds its value exactly at 7.5x the node budget.

**Verdict: SHIP.**
`infiltration_immortal_piece: 60 -> 30`,
`infiltration_immortal_with_prey: 30 -> 0`,
`infiltration_immortal_survivor: 0 -> -80`.
+36.4 Elo, SPRT ACCEPT at 20k nodes (seed 20260822) and again at 150k nodes
(seed 31337), measured against a baseline that already contains H2.

### H4 — Total War: how much is the option to claim a tile worth?

**Hypothesis.** Total War ends when the last neutral tile is claimed, so the
ability to claim is the engine's contact with the win condition.
`total_war_frontier` (per legal move onto a still-neutral tile) has never been
tuned; it is a hand-picked 8 against a `total_war_territory` of 28, i.e. an
option to claim is priced at 29% of the tile itself. §4.3 establishes that the
territory/material balance around it is near-optimal, which makes the untuned
term next to it the interesting one.

**Code change.** None. `total_war_frontier` already exists.

#### H4a — Broad Total War screen, 800 games per point, seed 4242

Every Total War weight that had never been screened, one at a time:

| candidate | score | Elo | 95% CI |
| --- | --- | --- | --- |
| `immortal_survivor=-160` | 0.4931 | −4.8 | [−26.3, +16.8] |
| `immortal_survivor=-80` | 0.4994 | −0.4 | [−21.7, +20.9] |
| `immortal_survivor=+80` | 0.4931 | −4.8 | [−25.8, +16.2] |
| `immortal_survivor=+160` | 0.4913 | −6.1 | [−27.1, +14.8] |
| `immortal_piece=0` | 0.5038 | +2.6 | [−18.6, +23.8] |
| `immortal_piece=20` | 0.4856 | −10.0 | [−31.1, +11.0] |
| `immortal_piece=80` | 0.4950 | −3.5 | [−24.5, +17.6] |
| `immortal_with_prey=0` | 0.5025 | +1.7 | [−19.2, +22.7] |
| `immortal_with_prey=20` | 0.4838 | −11.3 | [−32.4, +9.8] |
| `immortal_with_prey=80` | 0.4925 | −5.2 | [−26.3, +15.8] |
| `lead_scale=0` | 0.5031 | +2.2 | [−18.7, +23.1] |
| `lead_scale=8` | 0.5081 | +5.6 | [−15.3, +26.7] |
| `lead_scale=32` | 0.4756 | −17.0 | [−37.7, +3.7] |
| `lead_scale=64` | 0.4819 | −12.6 | [−34.0, +8.7] |
| `mobility=0` | 0.4263 | −51.6 | [−72.7, −30.9] |
| `mobility=3` | 0.5206 | +14.3 | [−7.6, +36.4] |
| `mobility=6` | 0.4619 | −26.5 | [−48.5, −4.8] |
| `capture=0` | 0.4775 | −15.6 | [−36.1, +4.7] |
| `capture=8` | 0.4662 | −23.5 | [−44.4, −2.7] |
| `capture=32` | 0.4587 | −28.7 | [−49.8, −7.9] |
| `frontier=0` | 0.1244 | −339.0 | REJECT |
| `frontier=4` | 0.2744 | −168.9 | REJECT |
| **`frontier=16`** | **0.5887** | **+62.3** | **ACCEPT** |
| `frontier=32` | 0.3531 | −105.2 | REJECT |

Two answers here. **Total War's immortality block is genuinely flat** — unlike
Infiltration's, all six points sit inside ±11 Elo, so the §6 guess that
`total_war_immortal_survivor=0` is a gap is a null in this mode too, for the
opposite reason: there is nothing there to find. `lead_scale` and `capture` are
likewise at or near their optimum, and the §6 idea of a sharper endgame
territory term gets no encouragement from `lead_scale` (0 and 8 are neutral, 32
and 64 are worse). `mobility` is worth a look at 3 but is inside the band.

**`frontier` is the outlier, and it is enormous.** Halving it costs 169 Elo,
zeroing it costs 339, and doubling it gains 62.

#### H4b — Frontier axis, 800 games per point, seed 4242

| value | score | Elo | 95% CI | verdict |
| --- | --- | --- | --- | --- |
| 0 | 0.1244 | −339.0 | — | REJECT |
| 4 | 0.2744 | −168.9 | — | REJECT |
| **8 (shipped)** | 0.5000 | 0.0 | — | — |
| 10 | 0.5962 | +67.7 | [+47.5, +88.4] | ACCEPT |
| 12 | 0.6275 | +90.6 | [+70.3, +111.5] | ACCEPT |
| 14 | 0.6044 | +73.6 | [+53.6, +94.1] | ACCEPT |
| 15 | 0.6175 | +83.2 | [+63.2, +103.8] | ACCEPT |
| 16 | 0.5887 | +62.3 | [+42.6, +82.5] | ACCEPT |
| 17 | 0.5681 | +47.6 | [+27.6, +68.0] | inconclusive |
| 18 | 0.6000 | +70.4 | [+50.5, +90.8] | ACCEPT |
| 20 | 0.5300 | +20.9 | [+1.3, +40.6] | inconclusive |
| 24 | 0.4731 | −18.7 | [−38.5, +1.0] | inconclusive |

A broad plateau from 10 to 18 rather than a spike, which is what a real
optimum looks like; the shipped 8 sits just off its left edge. The scatter
inside the plateau is the ±20 Elo band, so the argmax at 12 is not meaningfully
better than 15.

#### H4c — SPSA, seed 4242, 30 iterations x 40 pairs

Seeded at 13 per §4.1.

```
tuned: total_war_frontier 13 -> 15
```

#### H4d — Confirmation on the independent seed 20260822, 20k nodes

| candidate | games | score | Elo | 95% CI | verdict |
| --- | --- | --- | --- | --- | --- |
| **`frontier=13`** | 2000 | 0.6300 | **+92.5** | [+79.3, +105.9] | **ACCEPT** |
| `frontier=15` | 2000 | 0.6182 | +83.8 | [+70.8, +96.9] | ACCEPT |

Both confirm; 13 is the better of the two on a seed neither was chosen on, so
13 ships rather than the tuner's 15.

#### H4e — Node-budget transfer, seed 31337, 150k nodes

| candidate | games | score | Elo | 95% CI | LLR | verdict |
| --- | --- | --- | --- | --- | --- | --- |
| **`frontier=13`** | 800 | 0.6462 | **+104.7** | [+83.2, +126.9] | +4.78 | **ACCEPT** |

The gain grows again with depth, +92.5 to +104.7 at 7.5x the budget.

**Verdict: SHIP. `total_war_frontier: 8 -> 13`.**
+92.5 Elo at 20k nodes (seed 20260822) and +104.7 Elo at 150k nodes (seed
31337), SPRT ACCEPT on both, neither seed used to find or tune the value. This
is the largest single result of the session. Infiltration is untouched: the
weight only enters `total_war_score`.

### H5 — Does Total War's territory/material balance move once frontier does?

**Hypothesis.** §4.3's finding that territory 28 / material 70 beats every
nearby alternative was measured with `total_war_frontier` at 8. H4 moved
frontier to 13, which changes what a claimable tile is worth relative to an
owned one, so the balance around it might have shifted with it.

**Code change.** None.

Re-screened at the new baseline, 800 games per point, seed 4242. Note that the
draw rate roughly doubles at `frontier=13`, which tightens the confidence
interval to about ±16 Elo.

| candidate | score | Elo | 95% CI |
| --- | --- | --- | --- |
| `territory=20` | 0.4531 | −32.7 | REJECT |
| `territory=24` | 0.4900 | −6.9 | [−21.4, +7.5] |
| **`territory=28` (shipped)** | 0.5000 | 0.0 | — |
| `territory=32` | 0.4988 | −0.9 | [−17.5, +15.7] |
| `territory=36` | 0.5125 | +8.7 | [−7.7, +25.1] |
| `territory=44` | 0.4931 | −4.8 | [−22.0, +12.4] |
| `material=50` | 0.4856 | −10.0 | [−24.6, +4.6] |
| `material=60` | 0.4863 | −9.6 | [−24.5, +5.3] |
| **`material=70` (shipped)** | 0.5000 | 0.0 | — |
| `material=80` | 0.4863 | −9.6 | [−24.9, +5.7] |
| `material=90` | 0.4600 | −27.9 | [−42.9, −12.9] |
| `mobility=2` | 0.5194 | +13.5 | [−3.7, +30.7] |
| `mobility=3` | 0.4825 | −12.2 | [−31.5, +7.1] |
| `mobility=4` | 0.4713 | −20.0 | [−40.1, +0.0] |
| `capture=12` | 0.5031 | +2.2 | [−13.9, +18.3] |
| `capture=20` | 0.4913 | −6.1 | [−21.9, +9.7] |
| `capture=24` | 0.4975 | −1.7 | [−18.4, +14.9] |
| `lead_scale=8` | 0.5081 | +5.6 | [−10.1, +21.4] |
| `lead_scale=24` | 0.4906 | −6.5 | [−22.8, +9.8] |

**Verdict: NULL. Nothing shipped.** §4.3's conclusion survives the frontier
change intact — the shipped territory/material balance still beats everything
nearby, and capture and lead_scale are at their optimum too. Mobility at 2 is
the only flicker (+13.5, CI crosses zero) and it flips sign at 3 and 4, so it
is not followed up.

This is worth recording precisely because it is the boring answer: after a
+92 Elo change to the term next door, the balance did *not* need retuning.

### H6 — Infiltration race terms, and a boundary-fork feature

**Hypothesis (a).** H3 showed Infiltration over-values survival. The mirror
image would be that it under-values the race, so `advancement` (9), `closest`
(32) and `goal_threat` (180) should all want raising.

**Hypothesis (b).** Infiltration is won by reaching a row, and
`infiltration_goal_threat` scores *moves* that reach it. Three pieces
converging on one boundary square score three times what one piece scores, even
though a single enemy reply answers all three. Two pieces threatening two
*different* squares cannot both be parried by one move, which is the
king-safety analogue §6 asks for and which a move count cannot express.

**Code change for (b).** Added `SideSummary::goal_squares` (popcount of the
union of reachable boundary squares) and `goal_forks` (`goal_squares - 1`,
floored at zero), with weights `infiltration_goal_squares` and
`infiltration_goal_forks` defaulting to 0, and four unit tests pinning when
they fire. `arena show` listed both at 0 and identical engines still scored
exactly 0.5000, so the shipping engine was unchanged.

#### H6a — Firing probe, 120 games, seed 4242

| weight | probe | result | fires? |
| --- | --- | --- | --- |
| `infiltration_goal_squares` | −3000 | 0.0083 | yes, decisively |
| `infiltration_goal_forks` | −3000 | 0.0542 | yes, decisively |

#### H6b — Grid, 1000 games per point, seed 4242

| candidate | score | Elo | 95% CI |
| --- | --- | --- | --- |
| `goal_squares=-60` | 0.5250 | +17.4 | [−4.1, +39.0] |
| `goal_squares=+60` | 0.4985 | −1.0 | [−22.6, +20.5] |
| `goal_squares=+120` | 0.5010 | +0.7 | [−20.8, +22.2] |
| `goal_forks=+60` | 0.5030 | +2.1 | [−19.4, +23.6] |
| `goal_forks=+150` | 0.5060 | +4.2 | [−17.3, +25.7] |
| `goal_forks=+300` | 0.5015 | +1.0 | [−20.4, +22.5] |
| `advancement=5` | 0.5190 | +13.2 | [−8.3, +34.8] |
| `advancement=14` | 0.4880 | −8.3 | [−29.9, +13.2] |
| `advancement=20` | 0.4425 | −40.1 | [−62.0, −18.6] |
| `advancement=30` | 0.3715 | −91.3 | REJECT |
| `closest=16` | 0.4485 | −35.9 | [−57.7, −14.4] |
| `closest=48` | 0.5155 | +10.8 | [−10.7, +32.4] |
| `closest=64` | 0.4520 | −33.5 | [−55.2, −11.9] |
| `closest=96` | 0.3990 | −71.2 | REJECT |
| `goal_threat=120` | 0.5250 | +17.4 | [−4.1, +39.0] |
| `goal_threat=240` | 0.4985 | −1.0 | [−22.6, +20.5] |
| `goal_threat=320` | 0.5000 | −0.0 | [−21.5, +21.5] |
| `goal_threat=450` | 0.4960 | −2.8 | [−24.3, +18.7] |
| `material=25` | 0.4985 | −1.0 | [−22.6, +20.5] |
| `material=45` | 0.4900 | −6.9 | [−28.5, +14.5] |
| `material=60` | 0.4640 | −25.1 | [−46.8, −3.6] |
| `mobility=0` | 0.5100 | +6.9 | [−14.5, +28.5] |
| `mobility=4` | 0.4855 | −10.1 | [−31.7, +11.4] |
| `mobility=6` | 0.4475 | −36.6 | [−58.4, −15.1] |
| `capture=0` | 0.4815 | −12.9 | [−34.5, +8.6] |
| `capture=24` | 0.4970 | −2.1 | [−23.6, +19.4] |
| `capture=36` | 0.4610 | −27.2 | [−48.9, −5.7] |

**Verdict (a): NULL.** The race terms are *not* under-valued; raising any of
them costs Elo, and `advancement=30` and `closest=96` are outright rejects. The
H3 story does not generalize — Infiltration over-prices survival, but it prices
the race about right. `mobility` and `capture` are also at their optimum, which
answers the §6 note that mobility "has never been tuned in isolation": it has
now, in both target modes, and it is fine where it is.

**Verdict (b): NULL, and conclusively so — the feature is redundant.**

Note two pairs of rows above. `goal_squares=-60` and `goal_threat=120` returned
**identical** W/D/L (525/0/475), as did `goal_squares=+60` and
`goal_threat=240` (496/5/499). A follow-up confirmed it exactly:

| candidate | games | W/D/L |
| --- | --- | --- |
| `goal_threat=300` | 1000 | 499/4/497 |
| `goal_squares=+120` | 1000 | 499/4/497 |

Since `goal_threat` ships at 180, `goal_squares=+x` reproduces
`goal_threat=180+x` game for game. That means `goal_squares == goal_moves` in
every position that arose in 1000 games — the two counters never disagreed. The
reason is geometric: the boundary is a single row, so a piece one step from it
reaches two or three *distinct* squares by itself, and two pieces both touching
the row at once is rare enough never to have decided a game here. `goal_forks`
inherits the same collinearity, offset by a constant.

**The feature was reverted.** It is a genuine new counter with passing tests,
but it measures a quantity the engine already has, and a permanently-zero term
in the leaf loop of the mode H2 and H3 just improved is pure cost. §4.4 warns
that one position cannot separate two terms; this is the same trap one level
up — a whole *mode* failed to separate them.

The code did not go to waste: the same "distinct destinations rather than
destination moves" counter was rebuilt for Total War as
`total_war_claim_squares`, where the geometry is the opposite way round (63
neutral tiles, pieces standing shoulder to shoulder, overlap everywhere) and
where H4 has just shown the move-counting version to be the most valuable
weight in the mode. See H7.

### H7 — Total War: is the frontier a set of tiles or a set of moves?

**Hypothesis.** H4 showed `total_war_frontier` is the most valuable weight in
the mode, and it counts *moves* onto neutral tiles. Four pieces standing around
one neutral tile score four times what one piece scores, but only one of them
can ever claim it. Counting distinct reachable tiles instead should separate a
wide frontier from a traffic jam in front of a narrow one. Unlike the
Infiltration version in H6, the geometry here favours the distinction: 63
neutral tiles, nine pieces per side, and overlapping neighbourhoods everywhere.

**Code change.** `SideSummary::claim_squares` — a popcount of the union of
reachable neutral tiles, accumulated in the same pass that already computes
`claimable` — plus `total_war_claim_squares` defaulting to 0 and four unit
tests. `arena show` listed it at 0 and identical engines scored exactly 0.5000
(200 games, seed 20260822), so the shipping engine was unchanged.

#### H7a — Grid, 800 games per point, seed 4242, baseline `frontier=13`

| (frontier, claim_squares) | score | Elo | 95% CI | verdict |
| --- | --- | --- | --- | --- |
| (13, −20) | 0.0650 | −463.2 | — | REJECT |
| (13, −10) | 0.2231 | −216.7 | — | REJECT |
| **(13, 0) shipped** | 0.5000 | 0.0 | — | — |
| (13, +6) | 0.5669 | +46.7 | [+31.4, +62.3] | ACCEPT |
| (13, +10) | 0.5969 | +68.2 | [+52.3, +84.3] | ACCEPT |
| (13, +20) | 0.5656 | +45.9 | [+28.8, +63.2] | inconclusive |
| (13, +40) | 0.4231 | −53.8 | — | REJECT |
| (10, 6) | 0.5769 | +53.8 | [+38.2, +69.7] | ACCEPT |
| (8, 10) | 0.6112 | +78.6 | [+62.8, +94.8] | ACCEPT |
| **(8, 14)** | 0.6250 | **+88.7** | [+73.1, +104.7] | ACCEPT |
| (6, 14) | 0.6056 | +74.5 | [+57.6, +91.8] | ACCEPT |
| **(4, 20)** | 0.6244 | **+88.3** | [+70.6, +106.4] | ACCEPT |
| (4, 24) | 0.5919 | +64.6 | [+46.3, +83.2] | ACCEPT |
| (0, 18) | 0.5981 | +69.1 | [+50.4, +88.1] | ACCEPT |
| (0, 22) | 0.6131 | +80.0 | [+61.0, +99.5] | ACCEPT |
| (0, 26) | 0.6219 | +86.4 | [+66.9, +106.5] | ACCEPT |
| (0, 30) | 0.5463 | +32.2 | [+12.7, +52.0] | inconclusive |
| (0, 34) | 0.5031 | +2.2 | [−18.1, +22.5] | inconclusive |
| (0, 40) | 0.4675 | −22.6 | [−43.7, −1.7] | inconclusive |
| (0, 50) | 0.3994 | −70.9 | — | REJECT |
| (−6, 32) | 0.5188 | +13.0 | [−6.8, +32.9] | inconclusive |

The counter is not redundant here — it is *better than the thing it duplicates*.
`(0, 26)`, which counts tiles and ignores moves entirely, measures +86.4 Elo
against a frontier weight that was itself +92.5 Elo an hour earlier. There is a
ridge running from (0, 26) to (8, 14) along which everything measures +75 to
+89.

#### H7b — SPSA, seed 4242, 40 iterations x 40 pairs

Seeded at (8, 14).

```
tuned: total_war_frontier 8 -> 6, total_war_claim_squares 14 -> 16
```

#### H7c — Confirmation, seed 20260822, 20k nodes, 2000 games each

| (frontier, claim_squares) | score | Elo | 95% CI | verdict |
| --- | --- | --- | --- | --- |
| (6, 16) — the tuner's point | 0.6188 | +84.1 | [+73.6, +94.8] | ACCEPT |
| (8, 14) | 0.6042 | +73.5 | [+63.9, +83.3] | ACCEPT |
| **(4, 20)** | 0.6370 | **+97.7** | [+86.2, +109.4] | **ACCEPT** |

All three confirm on the independent seed. (4, 20) is best there by more than
the intervals overlap, so it ships rather than the tuner's (6, 16) — the same
outcome as H4, where the grid point beat the SPSA point on the seed neither was
chosen on.

#### H7d — Node-budget transfer, seed 31337, 150k nodes

| candidate | games | score | Elo | 95% CI | LLR | verdict |
| --- | --- | --- | --- | --- | --- | --- |
| **(4, 20)** | 800 | 0.6106 | **+78.2** | [+59.3, +97.5] | +4.34 | **ACCEPT** |

Holds at 7.5x the budget, though unlike H2 and H4 it gives some back (+97.7 to
+78.2) rather than growing. Games also get long: 178 plies average with 28 of
800 hitting the 400-ply ceiling. Truncated games score as draws for both arms
and colours are swapped inside every pair, so this compresses the measured Elo
rather than biasing it.

**Verdict: SHIP.**
`total_war_frontier: 13 -> 4`, `total_war_claim_squares: 0 -> 20`.
+97.7 Elo at 20k nodes (seed 20260822) and +78.2 Elo at 150k nodes (seed
31337), SPRT ACCEPT on both, measured against a baseline that already contains
H4's `frontier=13`. Note the two weights are only meaningful as a pair:
`frontier=4` with `claim_squares=0` measures −169 Elo.

---

## Session summary — 2026-08-22

### Everything shipped, measured as one change against the engine as found

Both rows below are the whole session's diff versus the session-start weights,
on seeds used for nothing else in this document.

| mode | nodes | games | seed | W/D/L | score | Elo | 95% CI | LLR | verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **Infiltration (V3)** | 20k | 3000 | 900913 | 1747/10/1243 | 0.5840 | **+58.9** | [+46.4, +71.6] | +7.17 | **ACCEPT** |
| **Infiltration (V3)** | 150k | 1200 | 555777 | 715/10/475 | 0.6000 | **+70.4** | [+50.7, +90.7] | +3.50 | **ACCEPT** |
| **Total War (V5)** | 20k | 2000 | 900913 | 1473/268/259 | 0.8035 | **+244.6** | [+228.1, +262.2] | +34.70 | **ACCEPT** |
| **Total War (V5)** | 150k | 800 | 555777 | 570/98/132 | 0.7738 | **+213.6** | [+188.5, +240.9] | +10.77 | **ACCEPT** |

### The diff

`src/evaluation.rs` only.

| weight | was | now | from |
| --- | --- | --- | --- |
| `infiltration_scarcity_1` | 0 | **15** | H2 |
| `infiltration_immortal_piece` | 60 | **30** | H3 |
| `infiltration_immortal_with_prey` | 30 | **0** | H3 |
| `infiltration_immortal_survivor` | 0 | **−80** | H3 |
| `total_war_frontier` | 8 | **4** | H4, then H7 |
| `total_war_claim_squares` | — | **20** (new) | H7 |

Plus one new feature, `SideSummary::claim_squares`, and four unit tests for it.
No new dependencies; `Cargo.lock` still contains one package. Nothing in
`src/search.rs` was touched at any point, so every measurement above compares
weight vectors and nothing else (§4.7).

### Evaluation cost

`examples/evalbench`, session-start engine versus shipped engine, same machine,
same 3,000,000-node budget:

| | session-start | shipped |
| --- | --- | --- |
| `evaluate` | 57.9 ns/call | 58.6 ns/call (+1.2%) |
| Total War depth | 11 | **13** |
| Total War | 4.15 Mnps | 4.13 Mnps |
| Infiltration depth | 15 | 15 |
| Infiltration | 4.77 Mnps | 4.87 Mnps |

The new counter costs 1.2% per evaluation and returns two plies in Total War at
the same node count, because the better-shaped score prunes more. §8 notes
Total War is the expensive mode and that depth is the largest strength lever;
this moves it in the right direction rather than the wrong one.

### Hypotheses tried, in order

| # | hypothesis | verdict |
| --- | --- | --- |
| H1 | Doomed material is worth less than the engine thinks | **null** — all four weights stay at 0 |
| H2 | A piece one capture from immortality is worth more in a race | **ship** — `infiltration_scarcity_1=15` |
| H3 | Infiltration over-values permanence | **ship** — the immortality block re-priced |
| H4 | The option to claim a tile is mispriced in Total War | **ship** — `total_war_frontier` retuned |
| H5 | Territory/material must shift once frontier does | **null** — §4.3's balance survives intact |
| H6a | Infiltration under-values the race | **null** — the race terms are already right |
| H6b | Distinct boundary squares beat boundary moves | **null** — exactly collinear; reverted |
| H7 | Distinct claimable tiles beat claiming moves | **ship** — `total_war_claim_squares=20` |

Four nulls and four ships, from eight hypotheses.

### Notes for whoever runs this next

1. **`arena tune` parallelizes badly.** It rejoins its thread pool every
   iteration over only `--pairs` openings, and ran at roughly 200% CPU against
   `match`'s 950% on the same 12 threads. A 7,680-game SPSA run took 26 minutes
   where the same games in `match` take about 4. Grid-then-confirm is the
   cheaper loop here, and in both H4 and H7 the grid point beat the SPSA point
   on the seed neither was chosen on.
2. **Screen at 800–1000 games, not 300 pairs.** The ±22 Elo band at 1000 games
   was repeatedly the limiting factor; several apparent dips inside a plateau
   (H2's `scarcity_1=20`, H3's `immortal_piece=15`) turned out to be noise.
3. **Test probes should be written relative to `EvalParams::DEFAULT`,** not as
   absolute weights. Two of this session's tests broke the moment the default
   they probed stopped being 0.
4. **The §4.6 transfer check earns its keep in both directions.** H2 and H4
   both grew at 7.5x the node budget (+23.6 to +42.1, +92.5 to +104.7); H7 gave
   some back (+97.7 to +78.2). None of them reversed, but the sizes moved
   enough that a shipping decision between two close candidates should be made
   at the higher budget.
5. **The §7 flagship case is still open.** H1 confirms the arena cannot see it
   (§4.2): the doomed-material pattern is real, it is what a human exploits, and
   engine self-play from random openings does not reach it often enough to
   price. A curated position suite remains the right instrument, and nothing in
   this session built one.
6. **Unused seeds.** 4242 was used for every screen and tune; 20260822, 31337,
   900913 and 555777 were used for confirmation. Pick fresh ones.

---

## Session 2026-08-22 (second run)

Baseline: the working tree left by the session above, i.e. `EvalParams::DEFAULT`
with `infiltration_scarcity_1=15`, the re-priced Infiltration immortality block,
`total_war_frontier=4` and `total_war_claim_squares=20`. `cargo fmt --check`,
`cargo clippy --all-targets` (zero warnings) and `cargo test` all green before
the first change.

Prompted by a real game: **Obsidian vs Henhen1227**, Total War, after
121.Pf4-g3. The review showed **-1.15** at depth 12/13 on 264,393 nodes in a
position that is mate in 9. This is §7's flagship case, reached from a game
rather than from a construction.

### H8 — A side one capture from losing is in a race, and the race has a length

**Hypothesis.** In Total War, a side down to a single permanently huntable
piece is not merely worse — it loses unless the last neutral tile is claimed
first. Whether it survives is a race between the clock (tiles left) and the
hunt (how far the nearest predator has to travel). Neither quantity is in the
evaluation.

Why the existing weights cannot express it, all three of which stay at 0:

| weight | fires on | why that is the wrong set |
| --- | --- | --- |
| `total_war_last_piece` | one piece | an *immortal* last piece cannot be taken at all |
| `total_war_army_doomed` | all pieces doomed | four doomed pieces still cost four captures |
| `total_war_doomed_piece` | per doomed piece | linear; says nothing about reaching zero |

H1 gridded all three and measured nulls. That result stands. The conjunction is
a different feature, and a linear sum of the three cannot produce it: any
weights that make the fatal case expensive make the two harmless cases
expensive by exactly as much.

#### H8a — Reproducing the position

Reconstructed from the review screenshot: Red Paper g3; Blue Scissors d3 and
g7, Rock h8, Paper b8; territory 42–26 with 13 neutral; Blue to move. Red owns
no Rock and can never obtain one, so Blue's Scissors are immortal and Red's
only piece is huntable for the rest of the game. Territory in Total War is
claimed only from *neutral* tiles (`Position::make_move_unchecked`), so it is
monotonic: Blue's ceiling is 26+13 = 39 against Red's 42. Blue cannot win on
territory and has no plan except the hunt.

| depth | score (Red) |
| --- | --- |
| static | **+99** |
| 12 | −176 |
| 13 | −106 |
| 16 | −258 |
| 18 | −290 |
| 20 | **B#9 — forced win for Blue** |

Depth 13 at −106 matches the −1.15 the review displayed. The engine needs depth
20 for a nine-ply forced loss, seven plies past any shipped bot.

Static decomposition, which is the whole answer to "why only −1.15":

| term | contribution |
| --- | --- |
| `total_war_territory` (16-tile lead) | **+448** |
| `total_war_material` (−3 pieces) | −210 |
| `total_war_immortal_piece` (Blue ×3) | −120 |
| `total_war_immortal_with_prey` (Blue ×2) | −80 |
| `total_war_claim_squares` | +60 |
| mobility, frontier, lead_scale, scarcity | +1 |
| **everything describing Red's predicament** | **0** |

#### H8b — Building the instrument (§7)

`examples/suite.rs` plus `suite/total_war_endgames.txt`, and
`Position::from_fen` / `to_fen` in `src/position.rs` so entries can be pasted
out of a stored game record. The runner reports searched and static accuracy
separately and prints proven results as `B#9` rather than a number.

Verdicts are established at depth 24–26 with 0.8–4 G nodes and **only proven
verdicts are kept**. Three constructed positions could not be resolved and were
dropped; one of them, `D-doomed-army-two-runners`, had been given a guessed
verdict of "blue" that the search then contradicted at +61.

The family that made the feature measurable is eleven positions with the *same
pieces on the same squares*, varying only the clock and the hunter distance:

| hunters at | tiles left | result |
| --- | --- | --- |
| 3 squares | 1 | R#2 |
| 3 squares | 2 | R#4 |
| 3 squares | 3 | R#6 |
| 3 squares | 4 | R#8 |
| 3 squares | 5 | **B#13** |
| 3 squares | 7 / 9 / 13 / 25 | **B#9** |
| 7 squares | 4 | R#7 |
| 7 squares | 6 | R#11 |
| 7 squares | 10 / 12 | **B#14** |

Two readings, both of which changed the design:

- More time is not more lost. 25 tiles and 13 tiles are both B#9, so the term
  must **saturate**. Left linear it reached −14503 on the 25-tile position;
  terminal scores start at 29000 and a heuristic must never get close enough to
  imitate a proof.
- **Hunter distance is the other half.** A first attempt used a hand-picked
  constant of 3 tiles' grace, fitted to the near-hunter family. The far-hunter
  family then showed the hunted side surviving *six* tiles, which that constant
  gets flatly wrong. Replacing the constant with the actual Chebyshev distance
  to the nearest predator separates every verified position: **every win for
  the hunted side scores 0 or 1, every loss scores 2 or more.**

Both are §4.4 in practice — one position, or one family varied along one axis,
would have shipped the constant.

#### H8c — The feature

```rust
race_length = (neutral - hunt_distance - 1).clamp(0, 9)   // when, and only when,
                                                          // a side has exactly one
                                                          // piece and it is doomed
```

`hunt_distance` is only scanned when that gate holds, so an ordinary position
pays nothing. A flat (unscaled) version of the same conjunction was implemented
first and is **worse than nothing**: it fires on the positions the hunted side
wins. Removed rather than shipped at 0.

#### H8d — Suite, `--nodes 300000` (the review's own budget)

| `total_war_doomed_endgame` | searched | static |
| --- | --- | --- |
| **0 (before)** | 10/20 | 9/20 |
| −300 | 18/20 | 17/20 |
| **−600 (shipped)** | **19/20** | **18/20** |
| −900 | 19/20 | 18/20 |
| −1200 | 20/20 | 19/20 |

Across budgets, which is §4.6 applied to the suite:

| weight | 100k | 300k | 3M |
| --- | --- | --- | --- |
| 0 | 10/20 | 10/20 | 15/20 |
| −600 | 19/20 | 19/20 | **20/20** |
| −1200 | 20/20 | 20/20 | 19/20 |

−600 is the small end of a flat plateau (−600 and −900 are identical) and grows
with depth. −1200 buys one more position at twice the magnitude; the one it
buys is `race-05-tiles-left`, the single position sitting exactly on the
0/1-versus-2 boundary. Not worth doubling every score in the family for.

Every guard position keeps its original score exactly, because the term is zero
there: `race-01` 949, `race-03` 917, `race-03-hunters-far` 387,
`near-hunters-02` 921, `near-hunters-04` 888, `far-hunters-04` 358,
`far-hunters-06` 302, `last-piece-is-immortal` 307.

Flagship, at the review's budget: **static +99 → −5301, searched −258 → −4804.**

The number that matters most is the depth the forced win first appears at,
because that is what decides whether any shipped bot ever sees it:

| depth | before | shipped |
| --- | --- | --- |
| 9 | −71 | −3556 |
| 11 | −102 | −4187 |
| **12** | −102 | **B#9** |
| 13 | −103 | B#9 |
| 20 | B#9 | — |

Eight plies earlier, and Total War's four-second browser budget reaches depth
13 (§8). The position went from one no bot could see to one every bot can.

#### H8e — Arena: the cost, on seeds used for nothing else

| mode | nodes | games | seed | W/D/L | score | Elo | 95% CI |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Total War | 20k | 3000 | 8675309 | 1067/865/1068 | 0.4998 | **−0.1** | [−10.6, +10.4] |
| Total War | 150k | 800 | 31415 | 303/195/302 | 0.5006 | **+0.4** | [−20.5, +21.4] |
| Infiltration | 20k | 2000 | 8675309 | 995/10/995 | **0.5000** | −0.0 | [−15.2, +15.2] |

Infiltration returns *exactly* equal wins and losses, the arena's
identical-engine signature (§4.2), confirming the change is Total War only.

**This is not an SPRT ACCEPT and does not claim one.** §1's rule is for
strength claims; this is §7's exception — suite accuracy bought at no Elo cost.
The screening grid says why no sample size would help: at −600, −800 and −1000
the arena returned *byte-identical* W 704 D 592 L 704 over 2000 games, and even
−5000 moved 0.4958. The pattern does not occur in self-play from random
openings, which is exactly H1's finding and exactly why the suite exists.

#### H8f — Cost

`examples/evalbench`, this session's change alone, same machine, 3,000,000
nodes:

| | without | with |
| --- | --- | --- |
| `evaluate` | 57.0 ns/call | 60.2 ns/call |
| Total War | depth 13, 4.33 Mnps | depth 13, 4.31 Mnps |
| Infiltration | depth 15, 5.04 Mnps | depth 15, 5.07 Mnps |

The `evaluate` figure is inside run-to-run noise — three consecutive runs of the
*same* binary gave 67.7, 59.5 and 58.7 ns. Nodes per second and depth, which are
the stable numbers, do not move.

**Verdict: SHIP.** `total_war_doomed_endgame = -600`.

### Session summary — 2026-08-22 (second run)

| # | hypothesis | verdict |
| --- | --- | --- |
| H8 | A side one capture from losing is in a race with a measurable length | **ship** — `total_war_doomed_endgame=-600` |

### The diff

| file | what |
| --- | --- |
| `src/evaluation.rs` | `total_war_doomed_endgame = -600`, the race term, `hunt_distance`, `DOOMED_ENDGAME_HORIZON`, four unit tests |
| `src/position.rs` | `from_fen` / `to_fen` and four unit tests, so suite entries are pasted rather than transcribed |
| `examples/suite.rs` | new — the §7 runner |
| `suite/total_war_endgames.txt` | new — 20 positions, every verdict proved by search |
| `README.md`, `EVAL_TRAINING.md` | document the second instrument; §7's flagship case marked closed |

No new dependencies; `Cargo.lock` still holds one package. `src/search.rs` was
not touched, so every arena number above compares weight vectors and nothing
else (§4.7).

### Notes for whoever runs this next

1. **The suite is the instrument for anything the arena scores 0.5000 on.**
   It took about as long to build as one hypothesis and it made a measurement
   possible that four arena grids could not. Add to it whenever a real game
   shows a position the engine misreads.
2. **Do not put an unproved verdict in `suite/`.** One guessed verdict was
   contradicted by the search that was supposed to confirm it. Drop the
   position instead.
3. **A hand-picked constant inside a feature is still a hand-picked weight.**
   `DOOMED_ENDGAME_GRACE = 3` looked measured — it was fitted to one family —
   and the next family refuted it. Varying a second axis replaced it with a
   rule-derived quantity. Check for a second axis before accepting a constant.
4. **Still open, and now cheap to test.** `race-05-tiles-left` is the one
   position the shipped weight misses. Three near-misses are worth a look:
   a two-doomed-piece version of the same race (never resolved at depth 26, so
   it needs a shallower construction); the same race term in Annihilation,
   which was not touched; and a **search** change rather than an evaluation one
   — Total War's `proof_bounds` currently returns nothing, but a side that
   holds an immortal piece *and* leads on territory by more than the neutral
   count cannot lose at all, since territory is monotonic. That is a real
   proof, and it is a §4.7 change: measure it on its own.
5. **Unused seeds.** 4242, 20260822, 31337, 900913, 555777, 77 and 20260820
   were spent by the first session; 11235, 8675309 and 31415 by this one.

---

## Session 2026-08-22 (third run)

Prompted by a human player's report: *"the current version of RPSFish for v5
way over values territory. Usually the games come down to annihilation and I
beat it with the bot having like 20 more territory than me. Territory only
matters if you fill in every square and it's never beneficial to have more than
41 tiles."*

Both halves are right, and the first half is right by about 400 Elo.

### 0. Instrument check, and the flag nobody had used

Identical engines, V5, 400 games, 20k nodes, seed 99173: `0.5000`, W144 D112
L144, 0 truncated. Calibrated.

Then the run that should have been made two sessions ago. `arena` takes
`--baseline` as well as `--candidate`, and `Options::search_differs`
(`arena.rs:265`) reads only the *search* switches, never the weights — so a
weights-only asymmetry gives both arms identical search and the SPRT is exactly
as valid as for any weight change. Every previous Total War measurement in this
file was symmetric self-play.

Baseline = the shipped engine with its whole territory subsystem zeroed
(`total_war_territory`, `total_war_frontier`, `total_war_claim_squares`,
`total_war_lead_scale` all 0). Candidate = the shipped engine.

| nodes | games | seed | shipped scores | Elo | end reasons (W/D/L) |
| --- | --- | --- | --- | --- | --- |
| 20k | 600 | 99173 | 0.1075 | **−367.7** | annihilation 0/0/503, territory 32/0/0 |
| 150k | 240 | 99173 | 0.0750 | **−436.4** | annihilation 0/0/210, territory 6/0/0 |

0 truncated in both. **Every one of the shipped engine's 503 losses was by
annihilation and every one of its 32 wins was by territory** — the complaint,
reproduced mechanically. The defect *grows* with depth, so it is worse in the
shipping bots (depth 16+) than in the arena.

The `EndReason` tally that makes that last column possible is new
(`Tally::reasons`, `arena.rs`); `GameRecord` always carried the reason and
`Tally::add` threw it away. It is the highest-value ten lines in the session:
a score says how often, never how, and in a mode with two win conditions
pulling opposite ways the how is the whole question.

### Why H5 and §4.3 missed it

They are not wrong; they were measuring one coordinate at a time with the other
three held at shipped values. `frontier=0` alone is −339 (H4a) and
`claim_squares=−20` is −463 (H7a), because an engine that pays for owning tiles
but cannot see reachable ones is incoherent. The minimum is at the **corner** of
the four-cube, off every axis H5 explored. A coordinate-wise grid finds a bad
coordinate; it cannot find a bad subsystem.

And the +190 Elo that H4 and H7 measured for the frontier pair is real — in
self-play, where both engines paint, the paint race is fair and painting better
wins. It was measuring "paints better", not "plays better".

### H9 — Territory is real, but the price of a tile is not a constant

**Hypothesis.** Winning the fill is one binary outcome worth one thing, so a
forty-tile lead is the same win as a one-tile lead. Any term that scales with
the size of the lead can be run up arbitrarily by walking onto empty ground,
which does not fight back, and will therefore always outbid the army that has
to survive to collect it.

**Screens, all against the territory-blind baseline, 600 games, 20k nodes,
seed 6180339.** Negative means worse than having no territory evaluation.

| candidate | Elo | shape |
| --- | --- | --- |
| shipped flat block | −367.7 | unbounded in the lead |
| `lead * claimed / 81` at 28 | −169.1 | unbounded |
| ... at 56 | −346.1 | unbounded |
| ... at 84 | −520.9 | unbounded |
| whole block × 0.25, plus the ramp | −332.8 | unbounded |
| `lead_scale=300` (sign only) | −49.0 | bounded |
| `lead_scale=600` | −34.9 | bounded |
| `lead_scale=900` | −57.9 | bounded |
| `race=70` | −63.2 | bounded, saturating |
| `race=140` | −77.1 | bounded, saturating |
| `race=210` | −105.0 | bounded, saturating |
| `race=350` | −161.2 | bounded, saturating |
| `race=70, lead_scale=400` | −21.5 | bounded + haste |
| **`race=70, majority_haste=4`** | **−13.9** | bounded + gated haste |
| `race=70, majority_haste=8` | −17.4 | |
| `race=140, majority_haste=5` | −26.1 | |
| `race=70, lead_scale=400, majority_haste=4` | −56.7 | double-counted |

Two clean readings. **Every unbounded shape loses monotonically in the size of
the reward.** And every *saturating* shape draws 34–53% of its games by
repetition: an engine whose territory term has maxed out has no reason left to
claim, so it sits on the lead instead of cashing it. A bounded term needs a
companion that pays for finishing.

`total_war_majority_haste` is that companion, and it is gated on holding
[`TERRITORY_MAJORITY`] rather than on the sign of the lead. The sign is the
wrong gate twice: it fires while the lead is still a coin flip, and it steps
at level territory, which is not monotone and so can be crossed back and
forth. A majority cannot be crossed back.

**Adding any flat term back on top of the winner costs Elo** (600 games, seed
31622776): `territory=7` −64.4, `territory=14` −158.4. Zero is the value.

**Confirmation of `race=70, majority_haste=4`:**

| opponent | nodes | games | seed | score | Elo | verdict |
| --- | --- | --- | --- | --- | --- | --- |
| shipped | 20k | 2000 | 1414213 | 0.7680 | **+207.9** [+191.8, +225.0] | SPRT ACCEPT |
| shipped | 150k | 800 | 1414213 | 0.8306 | **+276.2** [+248.8, +306.8] | SPRT ACCEPT |
| territory-blind | 20k | 1000 | 1414213 | 0.5245 | +17.0 [−2.2, +36.4] | level |
| territory-blind | 20k | 600 | 31622776 | 0.5183 | +12.7 [−11.8, +37.5] | level |

Against the shipped engine the end reasons invert: the new weights win 1411
games by annihilation where the old ones won none. Against the territory-blind
engine it is level on score while winning **389 of 1000 games on territory**,
where the blind engine wins none — so the mode's own win condition is back in
play at no measured cost in strength.

V3 regression: 800 games, `0.5000`, W400 D0 L400 — the identical-engine
signature, as required, since every changed weight is `total_war_*`.

**Cost.** `depthbench --mode total-war --time-ms 3000` now reaches mean depth
**16.83, min 16**, against the 13–16 recorded in `frontend/src/engine/bots/profiles.ts`
for the same budget. Four fewer terms in the leaf and flatter scores buy two to
three plies in the mode that had the least.

**Suite.** V5 16/19 against the shipped 17/19; correct sign 18/20, unchanged.
The `static` column drops 18/20 → 11/20, and that number is not comparable
between the two: `matches_margin` uses a fixed 300cp bar
(`examples/suite.rs:229`, default set at `:454`) that was chosen when a
territory lead could be worth 1120cp on its own. Every one of the new
"failures" is a *margin* miss on a position scoring −18 to −44 where positive
was wanted, not a sign error. **The margin metric is not scale-invariant and
cannot fairly compare two evaluations with different scales.** Left alone
rather than lowered, because lowering the bar to suit a candidate is moving the
goalposts; recorded here so the next reader does not mistake it for a reading
regression.

**Verdict: SHIP.** `total_war_territory` 28→0, `total_war_frontier` 4→0,
`total_war_claim_squares` 20→0, `total_war_lead_scale` 16→0,
`total_war_territory_race` 0→70, `total_war_majority_haste` 0→4.

Two features were added, measured, and removed again rather than left at zero:
`total_war_territory_urgency` (`lead * claimed / 81`, the four rows above —
monotonically harmful) and `total_war_territory_excess` (per tile past the
majority; algebraically subsumed by the race term's clamp, which saturates at
exactly the same point). The cliff analysis that came out of the second one is
kept in `total_war_territory_race`'s doc comment, because it generalises: a
flat "the fill is decided" bonus makes the evaluation *drop* when a side claims
the tile that wins it the game, so search declines the move. **Territory is
monotone and irreversible, so an upward step at a territory threshold is safe —
search can be encouraged across it and can never come back to farm it — while a
downward step is disqualifying whatever the monotonicity.**

### H10 — The forty-one tile theorem in search (§4.7, measured alone)

Closes run 2's note #4. In Total War a side holding an uncapturable piece and
`ours >= theirs + neutral` **cannot lose**: it cannot be annihilated (a kind
whose predator is extinct is safe forever — one 3-cycle, no piece ever returns
to the board), it cannot lose the fill (a claim either raises `ours` or moves a
tile out of `neutral` and into `theirs`, so `theirs + neutral` never rises), and
stalemate and repetition are draws. `ours >= theirs + neutral` is algebraically
`ours >= 41`.

A **win** is not provable, and that is not a limitation of the proof but a fact
about the game: an uncapturable piece can be walled in by pieces it does not
prey on, and a stalemate or repetition ends the game at zero with neutral tiles
still on the board. So the bound is `>= 0`, never `> 0`.

**Code.** New `ModeRules::territory_is_the_only_other_loss` and the derived
`territory_majority_prevents_loss()`, so no rule is decided by matching on a
`Mode` (§3). `is_proven_draw` and `proof_bounds` now share one predicate,
`is_guaranteed_at_least_a_draw`. V1's behaviour is byte-identical by
construction: the `immortality_prevents_loss` branch returns exactly what the
old code did. `is_proven_draw`'s exact-zero arm stays unreachable in V5 —
two majorities do not fit on one board.

**Measured alone**, evaluation held at `DEFAULT` on both arms, via
`--baseline-proofs off --candidate-proofs on`:

| mode | games | seed | score | Elo |
| --- | --- | --- | --- | --- |
| Total War | 2000 | 2718281 | 0.4993 | −0.5 [−13.4, +12.4] |
| Infiltration | 600 | 2718281 | 0.5000 (W299 D2 L299) | −0.0 |
| Annihilation | 600 | 2718281 | 0.4625 | −26.1 |

**Verdict: SHIP, at zero Elo.** V5 is neutral but it is *not* the never-fired
signature — W713 D571 L716 over 2000 games, so the proof does change games. What
it buys is a correct *reported* score: the engine no longer returns a negative
number for a position it provably cannot lose, which is what the review screen
and the eval bar display. `a_permanent_territory_majority_with_an_immortal_piece_is_never_scored_as_losing`
pins exactly that — the raw heuristic is negative and the search returns `>= 0`.
Infiltration is byte-identical, as the rule facts require. The Annihilation row
is not a regression: that arm disables the *pre-existing* V1 bound, and nothing
in this file had ever priced it. **The V1 proof is worth about +26 Elo** — a free
datum, recorded because run 1 wanted it.

`examples/suite.rs` gained `--proofs on|off` to make this scoreable there. The
suite does not move (16/19 either way): only `last-piece-is-immortal` satisfies
the conjunction and it already passed.

### Also fixed in passing

- `leaf_score`'s doc comment was glued onto `material_weight`
  (`search.rs:1652`), leaving `leaf_score` undocumented. Relocated and extended
  for the second licence.
- New `EvalParams::material(mode)`, replacing the private duplicate in
  `Searcher::material_weight`. The evaluation's unit of account was previously
  spelled out in three places, including by hand in
  `frontend/src/engine/bots/profiles.ts`.
- `EndReason::ALL` / `EndReason::index()`, for the tally.
- `tests/invariants.rs`'s selective-versus-exhaustive check compared
  `score.signum()` on raw scores. With a flatter evaluation, near-zero scores
  are common and exactly one position in the 360-position sweep scores +1
  against −1 — two searches walking different trees landing either side of
  zero for arithmetic rather than tactical reasons. The assertion now applies
  above one piece in the mode's own units (`EvalParams::material`), which is
  where "decided" means anything and where a dropped forced line would show.

### Frontend integration

The bots a player actually faces run the WebAssembly build, and three constants
outside this repo are calibrated to the *magnitude* of evaluation scores rather
than to strength. All three go stale when weights move.

| what | before | after | how |
| --- | --- | --- | --- |
| `MODE_SCORE_SCALE.V5.choice` | 20 | **15** | `npm run arena -- --spread` |
| `MODE_SCORE_SCALE.V5.material` | 70 | 70 | unchanged; `total_war_material` did not move |
| `WIN_PROBABILITY_SCALE.V5` | 0.003848 | **0.003236** | `npm run calibrate:review` |

`--spread` also re-measured the two untouched modes at 6 and 44 against the 7
and 50 shipped, which is seed noise, so they were left alone.

The win-probability re-fit needed two seeds to be worth acting on. On seed
6180339 the *control* modes moved with unchanged weights — V1 5.5% low, V3 28%
high — so a single fit is not evidence of a shift. V5 came back 0.003380 on
6180339 and 0.003091 on 1414213, agreeing in direction on ~30,600 positions
each (five times V3's sample); the shipped value is their mean. The direction is
what the re-pricing implies: the mode has two win conditions and the evaluation
now states the territory one far more weakly, so a given score predicts the
result less sharply.

Checks: `npm run arena -- --selftest` exactly 0.5000 in all three modes;
`npm test` 7/7; V5 ladder monotone on the rungs the arena's node budgets do not
cover — Napkin(d3) +394 over Pebble(d2), Snips(d4) +394 over Napkin, Boulder(d7)
+597 over Snips, 16 games each. Crane and Obsidian were not re-run: at 3–4
seconds a move a 12-game match takes hours, and their regime is exactly what the
150k-node arena rows above measure.

`examples/depthbench --mode total-war --time-ms 3000` reaches mean depth 16.83
(min 16) against the 13–16 recorded in `botProfiles.js` for the same budget, so
the shipping bots also got about two plies deeper.

### Notes for whoever runs this next

1. **Use `--baseline`.** Symmetric self-play cannot see a defect both engines
   share, and this one cost 400 Elo while sitting in plain sight behind two
   sessions of measurement. Before trusting any weight, ask what it measures
   against an opponent that does not share it. §4.2 said the arena is blind to
   what a human steers toward; the sharper statement is that the arena is blind
   to whatever both arms believe.
2. **Read the end-reason column.** "Every loss by annihilation, every win by
   territory" diagnosed this in one line after four grids had not.
3. **Bounded and monotone are different requirements, and a territory term
   needs both.** Bounded stops the lead outbidding the army; monotone-upward
   stops the engine refusing the move that wins. Saturating alone buys stalling.
4. **Scale-dependent metrics cannot compare evaluations of different scales.**
   The suite's 300cp margin, `MODE_SCORE_SCALE.V5.choice`, and
   `WIN_PROBABILITY_SCALE.V5` are all calibrated to the old inflated scale.
   The last two are re-fitted downstream; the first is left alone deliberately.
5. **Still open.** `far-hunters-06` and `near-hunters-04` are the two V5 suite
   positions the new weights read as barely negative when the truth is a Red
   win. Both are races the hunted side *wins*, and
   `total_war_doomed_endgame`'s `race_length` clamps at 0 from below
   (`evaluation.rs`), so it is silent exactly when the hunt arrives *after* the
   board fills. Letting that quantity go negative, with its own bound, is the
   obvious next hypothesis and it is cheap.
6. **Unused seeds.** Spent across all three sessions: 4242, 20260822, 31337,
   900913, 555777, 77, 20260820, 11235, 8675309, 31415, 99173, 6180339,
   1414213, 31622776, 2718281.

---

## Session 2026-09-02 — Intransitive (V6) comes to the engine

New mode, so this session has no baseline to beat: the question is not "is the
engine better" but "are the weights this mode ships with the ones the arena
picked". They are not the ones that were guessed. Every row is Intransitive
self-play at 20k nodes/move; V3 and V5 weights were not touched and re-measured
at exactly 0.5000 as an instrument check (300 games each, seed 99).

The mode is Infiltration's rule set with the goal shrunk from a rank to the
single corner the enemy's army opened in. The engine gained the mode as a
`Goal` shape in `ModeRules` rather than as a branch: `Goal::HomeRank` and
`Goal::EnemyCorner` differ in a mask, a distance, and a symmetry group, and
nothing else in search or evaluation asks which mode it is in.

### The bootstrap guess, and why it was wrong

Intransitive's weights started as Infiltration's tuned block, on the reasoning
that the two modes share every rule but the shape of the goal. Three terms were
moved *up* from Infiltration's on the argument that a one-tile goal is more
urgent than a nine-tile one, and three corner-specific terms were added.

Every one of those six guesses was wrong, and in the same direction.

### H1 — Is a corner race more urgent than a rank race?

**Hypothesis.** Against a point goal the nearest piece is the threat and the
defender has one square to cover, so a move of race progress should be worth
*more* than Infiltration's 32/9, not less.

**Result: backwards.** Candidate against `closest`/`advancement` at 44/9:

| closest / advancement | games | seed | Elo | 95% CI | LOS |
| --- | --- | --- | --- | --- | --- |
| 0 / 0 | 800 | 4242 | +7.8 | [-14.3, +30.0] | 75.6% |
| **15 / 3** | 800 | 4242 | **+56.5** | [+33.9, +79.7] | 100% |
| 25 / 5 | 800 | 4242 | +33.1 | [+10.3, +56.3] | 99.8% |
| 44 / 0 | 800 | 4242 | -30.9 | [-54.0, -8.1] | 0.4% |

Confirmed at 15/3 on an independent seed: **+58.7** [+36.1, +81.9], LOS 100%,
800 games, seed 31337. Bracketed either side against the new default, both
worse: 10/2 is -8.7 [-29.0, +11.5] and 20/4 is -19.6 [-41.4, +2.1]. The curve
is single-peaked and 15/3 is the peak.

**Why.** The same narrowness that makes a corner quick to reach makes it cheap
to hold: three approach squares, and a tile a single body can sit on. A lead in
a race the defender can simply shut is not a lead. Note the first row — pricing
the race terms at *Infiltration's* values is no better than switching them off
entirely, which is the measurement that made the point.

Note also the last row, which is the reason to move the pair together: keeping
`closest` high while zeroing `advancement` is worse than the default. The
summed term is what stops the engine committing one runner and leaving the
rest of the army behind it.

### H2 — Is the ground around the corner worth holding?

**Hypothesis.** A corner goal has a four-square shell (the tile and its three
approaches) that a rank goal has no analogue for. Occupying it should be worth
real material.

**Result: null, and shipped at zero.** Measured against the guessed block
(guard 26, plug 90, seal 260), 800 games at seed 20260902:

| candidate | Elo | 95% CI | LOS |
| --- | --- | --- | --- |
| guard 0 | +12.6 | [-9.5, +34.8] | 86.8% |
| guard 8 | -25.7 | [-47.8, -3.7] | 1.1% |
| all three off | +53.8 | [+30.8, +77.4] | 100% |
| **guard 0, plug 40, seal 120** | **+74.1** | [+51.3, +97.5] | 100% |

`intransitive_corner_guard` is kept as a computed feature at weight 0. The null
is worth keeping: the intuitive reading of a corner-goal mode is that the
ground near the goal is valuable, and it is not. Only the tile itself is.

### H3 — Is the corner blockade real?

**Hypothesis.** A nine-square rank cannot be stood on and one square can, so a
defender occupying its own corner converts the enemy's race into a capture
problem — and if that defender is uncapturable, the corner is shut for good.

**Result: real, at half the guessed price.** The winning row above is the same
experiment: `plug` 40 and `seal` 120 beat both the guess (90/260) and switching
the blockade off entirely, by +74.1 and +20.3 Elo respectively.

Deliberately **not** wired up as a search bound, unlike Total War's tile floor.
Sealing one corner proves nothing about the other, and the sealing side still
has to survive; `ModeRules::immortality_prevents_loss` and
`territory_majority_prevents_loss` are both correctly false for this mode.

### H4 — Is an uncapturable piece a liability, as Infiltration says?

**Found by the suite, not by the arena.** `suite/intransitive_corners.txt` holds
a pair of positions identical but for one piece's kind: Red's defender beside
Blue's runner is a Scissors in one and a Rock in the other. Scissors takes
Paper and the game is drawn; Paper takes Rock and Blue mates. The evaluation
scored the *losing* Rock at -54 and the drawing Scissors at -126 — it preferred
the worse army, because the better one's piece was uncapturable and
`intransitive_immortal_survivor` was carrying Infiltration's tuned -80.

**Hypothesis.** That -80 is right for a rank goal and wrong for a corner goal.
An uncapturable piece facing a nine-square goal line still has to walk it, which
is what made the flat bonus overpriced in Infiltration. Facing a corner it does
not have to walk anywhere: it can sit on the one tile the enemy needs.

**Result: confirmed, and the term wants zero rather than a positive value.**
800 games at 20k nodes, seed 555777, against `immortal_piece` 45 / `survivor`
-80:

| candidate | Elo | 95% CI | LOS |
| --- | --- | --- | --- |
| **survivor 0** | **+41.5** | [+18.9, +64.3] | 100% |
| survivor 0, piece 80 | +42.8 | [+20.1, +65.8] | 100% |
| survivor 0, piece 30 | +43.7 | [+20.9, +66.8] | 100% |
| survivor +60 | +23.1 | [+0.3, +46.0] | 97.6% |

The three `survivor 0` rows are one number measured three times, so
`immortal_piece` is left at 45 rather than moved on a difference the sample
cannot see. The suite's static column went 13/28 to 16/28 understood, and the
pair that found this now reads -46 for the draw and -134 for the loss.

### Everything shipped, as one change

| games | seed | result | Elo | 95% CI | LOS | SPRT |
| --- | --- | --- | --- | --- | --- | --- |
| 1000 | 6180339 | W 547 D 53 L 400, 0.5735 | +51.4 | [+30.4, +72.8] | 100% | — |
| 1000 | 2718281 | W 566 D 61 L 373, 0.5965 | **+67.9** | [+46.9, +89.4] | 100% | **ACCEPT** |

against the bootstrap guess, on seeds used by none of the grids. The first row
is the race and corner work alone; the second adds H4 and is the shipped state.
64.7 plies average, 0 truncated, every decisive game by corner.

### The diff

```
intransitive_closest             44 -> 15
intransitive_advancement          9 -> 3
intransitive_corner_guard        26 -> 0
intransitive_corner_plug         90 -> 40
intransitive_corner_seal        260 -> 120
intransitive_immortal_survivor  -80 -> 0
```

### Notes for whoever runs this next

1. **A goal's *shape* is a rule, and it decides the symmetry group.** Reversing
   files is an exact symmetry of a rank goal and not of a corner goal, so
   `ModeRules::symmetries` is now the authority and `Position::canonical_over`
   intersects any requested fold with it. A book configured `--symmetry files`
   silently merges nothing in V6, which is correct; the README's old claim that
   file reversal is exact "in every mode" was true when written and is not now.
2. **`MODE_KEYS` is indexed by wire code, not by `Mode::ALL` position.** Adding
   a third mode overran it, and the failure showed up as a panic in ten
   unrelated tests rather than as anything about modes. There is now a test.
3. **The race terms and the corner terms interact, and the sign flips.** The
   corner block measured +9.7 Elo against the *old* race weights and -25.2
   against the corrected ones. Neither number was reproducible as a statement
   about the corner block alone. Grid the subsystem.
4. **Unmeasured, and the obvious next hypotheses.** `intransitive_doomed_piece`
   and `intransitive_army_doomed` are both 0, copied from Infiltration. There
   is an argument they should not be: a doomed piece can never be the blockade,
   because it gets eaten off the corner, so in a corner-goal mode a doomed
   piece is worth strictly less than in a rank-goal one. That argument is
   untested. The same goes for `intransitive_goal_threat` at 220, which is the
   one term still carrying its guessed value upward from Infiltration's 180.
5. **Seeds spent this session.** 20260902, 4242, 31337, 6180339, 7, 99.

## Session 2026-09-02 (second run) — Intransitive, jointly, and a book to play from

The morning's session bootstrapped V6 from Infiltration and corrected six
weights one subsystem at a time. This one asks whether the *rest* of the vector
— the terms inherited unexamined, and the three the previous notes flagged as
unmeasured — is worth anything, and it answers by moving all of them at once.

Two things came out of it: a weight set worth about +35 Elo at the tuning
budget and more than twice that at a realistic one, and an engine that can play
from an opening book at all, which it could not before.

### H5 — Is the inherited half of the V6 vector mispriced?

**Hypothesis.** Six V6 weights were measured in the first session. The other
thirteen were copied from Infiltration or guessed, and the previous notes
already suspected three of them (`intransitive_doomed_piece`,
`intransitive_army_doomed`, `intransitive_goal_threat`). But note 3 of that
session is the reason not to test them one at a time: the corner block measured
+9.7 Elo against the old race weights and −25.2 against the corrected ones, so
a coordinate's value here depends on where the others are standing.

**Method.** SPSA over the whole mode vector — 100 iterations of 32 pairs at 20k
nodes, seed 1414213 — then verification against the weights it replaced on
seeds the tuner never saw.

**Result: confirmed, and it is a joint result rather than a sum of parts.**

| budget | games | seed    | Elo   | 95% CI          | LOS  |
| ------ | ----- | ------- | ----- | --------------- | ---- |
| 20k    | 2000  | 1732050 | +34.2 | [+19.5, +48.9]  | 100% |
| 100k   |  800  | 2236067 | +48.1 | [+25.3, +71.2]  | 100% |
| 200k   |  600  | 2449489 | +82.6 | [+56.9, +109.2] | 100% |

Subsets recover very little of it. On the same seed and sample as row one,
shipping only the four coordinates that individually clear the noise —
`material`, `immortal_piece`, `goal_threat`, `corner_seal` — is **+12.2**
[−2.1, +26.5], and `material` with `immortal_piece` alone is **+5.4** [−8.9,
+19.7]. Two thirds of the gain is in coordinates that, read one at a time, look
like nothing.

**The budget column is the part worth keeping.** A weight set that won only at
the tuning budget would be an artifact of shallow search; this one wins by more
the longer either side thinks, and a bot game is 5+3 — roughly 70M nodes on the
first move, three orders of magnitude past where it was tuned.

### The noise floor, measured rather than assumed

`intransitive_last_piece` and `intransitive_two_pieces` are gated off by
`ModeRules` in this mode: zero pieces is a stalemate draw, so there is no cliff
at zero to price and no value given to either can change a single game. SPSA
cannot see that, and walked them to **−12 and −5**.

That is a free calibration of the whole run, and it is worth doing deliberately
in any future SPSA pass: *leave a known-inert coordinate in the vector and read
the noise floor off it.* Both are shipped at 0 rather than at what the tuner
said, because shipping a number that cannot change a game would be a claim this
measurement does not support.

It also settles the two hypotheses the last session flagged. `doomed_piece` and
`army_doomed` landed at −9 and +3 — inside the floor, in opposite directions.
The argument for pricing a doomed piece below Infiltration's is still untested;
`examples/suite.rs` explains why self-play is the wrong instrument for it.

### H6 — Is `goal_threat` at 220 the guess it looks like?

**Result: it is a guess, and it barely matters.** 1200 games at 20k nodes per
point, seed 8675309, against the shipped 220:

| value | Elo   | 95% CI         |
| ----- | ----- | -------------- |
| 0     | −59.0 | [−78.2, −40.2] |
| 100   | +9.6  | [−9.2, +28.4]  |
| 160   | +8.1  | [−10.5, +26.8] |
| 300   | −7.5  | [−26.3, +11.2] |
| 400   | −2.9  | [−21.7, +15.9] |

The mode wants an immediate goal threat priced and does not much mind where in
100..=220. Only switching it off is clearly wrong. SPSA independently settled
on 190, which agrees with the scan rather than refining it.

### The engine can now play from a book

Books existed and the playing engine could not read one: `book.rs` was reachable
from the `book` binary and from the website export, and `main.rs` never
mentioned it. Every game, including every tournament game from the same opening
position, was searched from move one.

`rpsi` now takes `--book <dir>`, pointing at the directory `build_books.sh`
writes, and `newgame` opens the book for the mode being played. A book move is
answered without searching, which is worth two separate things at 5+3: the move
comes from a search two to six times deeper than the clock would allow, and the
17 seconds the first move would have cost stay on the clock.

Five gates decide whether a record is played rather than searched, and each one
closes a way a book can be *worse* than thinking:

- **The record looked at fewer nodes than this search is about to.** This is
  the gate that matters most, and it is not obvious: *a book is built for
  breadth, and its deep end is weaker than thinking.* `openings.taper` spends
  400M nodes on a first move and 5M forty plies in, while a 5+3 clock buys
  something like 70M every move — so past roughly ply 3 the shipped taper knows
  *less* about a position than the engine would find out by searching it. The
  engine times its own searches and compares node counts directly, because a
  depth floor cannot tell the difference: depth is a fact about a position's
  branching as much as about effort spent.
- `path_dependent` — the record's value came from a search that saw a
  repetition, so it belongs to the line that reached it and not to this game.
- `depth < --book-min-depth` (12 by default) — a survey stub. The survey pass
  ranks every opening cheaply so the deep pass has edges to expand.
- the move is not legal here — the book and the rules have diverged, which is
  worth falling back on rather than arguing with.
- the move lands on a position this game has already stood in. **Repetition is
  the one rule a book deliberately does not store**, because it depends on the
  line walked rather than the position, so the engine has to supply it.

`newgame` keeps a book already open rather than re-reading it, because a bot
game starts with the clock already running and backing every value up through a
full graph is seconds of the engine's own time. It re-reads when the mode
changes, or when `nodes.log` has changed underneath it — which is one `stat` per
game and means a bot left running across a rebuild picks the new book up instead
of holding the snapshot it happened to start with.

### The taper that matches the gate

The node gate is only half the answer, because it makes the shipped book
*usable for three moves* rather than making it better. `scripts/play.taper` is
the other half: the same two-pass build with every band above what a clock can
buy — 250M at ply 2 down to a floor of 100M at ply 20, against a 5+3 budget
that starts near 70M and decays toward 25M as the clock converges on the
increment. It buys depth with width, and falling out of a narrow book costs
nothing, because the engine then does what it did everywhere before.

`build_books.sh --deep-taper <file>` selects it; the default is unchanged, since
the website's explorer still wants breadth. Plies 0 and 1 are `survey.taper`'s
own bands exactly, so a survey already run is not paid for twice.

Absence is never fatal and never silent: a missing directory, a book built
under other weights, and a corrupt log all end with the engine searching every
move and the reason on an `info string`.

### Two things that would have failed quietly

**`scripts/build_books.sh` could not finish a corner-goal book.** Its two build
passes pass `--symmetry`; `compact`, `stats`, `show` and `export` did not, and
so used the default `files`. A book refuses to open under a fold other than its
own, so all four would have died with `symmetry mismatch: book has none, this
run has files` — *after* the entire node budget had been spent. It cost nothing
while both books were `files`, which is why it survived.

**The survey taper ranked 23 openings.** `survey.taper` searched the root at
MultiPV 23, described in its own comment as "every legal opening, in both
modes", which was true when the only two modes shared a starting position.
Intransitive opens from two diagonal wedges of ten and has **36**. The last
thirteen openings would have had no edge, no node, and therefore no book — so
an opponent playing one of them as Red would have taken us out of book on move
one. Raised to 36; a mode with fewer openings simply ranks all it has.

### The diff

```
intransitive_material            35 -> 49
intransitive_mobility             2 -> -1
intransitive_capture             12 -> 9
intransitive_advancement          3 -> 7
intransitive_closest             15 -> 13
intransitive_goal_threat        220 -> 190
intransitive_corner_guard         0 -> -2
intransitive_corner_plug         40 -> 32
intransitive_corner_seal        120 -> 92
intransitive_immortal_piece      45 -> 63
intransitive_immortal_with_prey   0 -> 9
intransitive_immortal_survivor    0 -> -5
intransitive_scarcity_2           0 -> 2
intransitive_scarcity_3           0 -> -11
intransitive_doomed_piece         0 -> -9
intransitive_army_doomed          0 -> 3
```

`suite/intransitive_corners.txt` is unchanged at 8/8 searched, 8/8 correct
sign, 5/8 understood statically. The suite's margin column is not comparable
across this change — one piece went from 35 to 49, so every score in the mode
moved — but the sign and search columns are.

### Notes for whoever runs this next

1. **Leave an inert coordinate in every SPSA vector.** It costs nothing and it
   is the only honest noise floor a run has. Without it, `doomed_piece` at −9
   reads as a finding.
2. **`MODE_SCORE_SCALE.V6.choice` is now stale.** `material` was hand-copied
   and has been moved to 49; `choice` is a *fitted* p90 root-score gap measured
   under the old scale and wants re-measuring with `npm run arena -- --spread`.
   It is left at 26 rather than guessed, and nothing consumes it while the
   public engine is held out of V6. `WIN_PROBABILITY_SCALE` has no V6 entry and
   so needs nothing.
3. **The weight fingerprint is global, so a V6 change bricks the V3 and V5
   books too.** `book::fingerprint` hashes the whole `EvalParams`, not the
   mode's slice of it, so `book/V3` and `book/V5` now refuse to open under
   these weights: `evaluation weights mismatch: book has c7b11204eebaf247,
   this run has e9359568efe4840c`. The engine says so and searches, which is
   exactly what it did before it could read a book at all, so nothing is worse
   than it was — but those two books are dead weight until rebuilt, and
   `build_books.sh` will archive them to `book/archive/` the moment it is run
   for either mode. Rebuilding all three is the only way to have books in all
   three, and it is a fresh build rather than an extension.
4. **The book is not in the WASM build**, and should not be: `lib.rs` excludes
   `book` from the browser, which has no filesystem. This is a native-bot
   feature only.
5. **Seeds spent this session.** 8675309, 1414213, 1732050, 2236067, 2449489.
