# RPSFish

RPSFish is a deterministic, Stockfish-inspired search engine for the three
game modes of RPS Strategy — Total War, Infiltration, and Intransitive. It
provides the game's computer opponents, offline position analysis, and the
opening books the website serves.

## How it works

A compact position (eight 81-bit bitboards, one `u128` each) is searched with
iterative-deepening negamax alpha-beta: a transposition table, capture
quiescence, aspiration windows, MultiPV, and a selective pruning layer (null
move, late-move reductions, futility pruning, internal iterative reduction)
that is switched off entirely in the one mode where a forced move can be
worse than a pass.

Evaluation keeps three kinds of knowledge apart on purpose: rule *facts*
(what the rules guarantee, usable as proofs — e.g. a side that can never
capture again cannot lose), *features* (countable properties of a position),
and *weights* (what a feature is worth). A new feature ships at weight zero;
only the `arena` binary's paired, colour-swapped match testing and SPSA
tuning ever earns it a nonzero value. Nothing about strength is decided by
reading a diff. See [docs/REFERENCE.md](docs/REFERENCE.md#design-rule-facts-features-weights)
for the full design rule and [docs/PLAN.md](docs/PLAN.md) for the rest of the
architecture.

The Go game server remains the authoritative rules implementation. RPSFish's
own rule behavior is checked against Go-generated parity fixtures, and every
engine result is a proposal the server validates before it changes a real
game.

## What it provides

- **`rpsfish`** — a diagnostic CLI: `search`, `perft`, and an `rpsi` mode that
  speaks a UCI-shaped protocol ([docs/RPSI.md](docs/RPSI.md)) on stdin/stdout,
  so any RPSI-speaking host can drive it.
- **`arena`** — the measurement tool: paired match testing (SPRT) between two
  weight sets, and SPSA tuning.
- **`book`** — builds, reads, and exports offline opening books (a
  canonicalized position graph, not a move tree).
- **A Rust library crate** (`rlib`/`cdylib`/`staticlib`) with no third-party
  dependencies.
- **A dependency-free WebAssembly adapter** for running search in a browser
  Web Worker.
- **An iOS XCFramework** (`scripts/build_ios.sh`) for static linking into a
  native app.

The full command reference, the C ABI, and the opening-book format are in
[docs/REFERENCE.md](docs/REFERENCE.md).

## License

RPSFish is free software licensed under the [GNU Lesser General Public License
v3.0 or later](COPYING.LESSER). The LGPL builds on the [GNU General Public
License v3.0](COPYING), so both texts ship with the crate. Every source file
carries the notice and an `SPDX-License-Identifier: LGPL-3.0-or-later` tag.

In practice this means the engine may be linked into a larger work — including
a proprietary one — provided recipients can replace this library with a
modified version and relink. Modifications to RPSFish itself must be released
under the same license.

**Contributions do not require a CLA.** This repository has no contributor
license agreement; contributions are accepted under the LGPL-3.0-or-later
terms above.

Copyright (C) 2026 Henry Abrahamsen.
