// Copyright (C) 2026 Henry Abrahamsen
//
// This file is part of RPSFish.
//
// RPSFish is free software: you can redistribute it and/or modify it under the
// terms of the GNU Lesser General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option) any
// later version.
//
// RPSFish is distributed in the hope that it will be useful, but WITHOUT ANY
// WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR
// A PARTICULAR PURPOSE. See the GNU Lesser General Public License for more
// details.
//
// You should have received a copy of the GNU Lesser General Public License
// along with RPSFish. If not, see <https://www.gnu.org/licenses/>.
//
// SPDX-License-Identifier: LGPL-3.0-or-later

#![deny(unsafe_code)]

//! Deterministic search core for RPS Strategy.
//!
//! The public API intentionally separates compact engine positions from the
//! JSON-oriented Go server state. Deployment adapters should validate and
//! convert snapshots at their boundaries.

pub mod board;
/// Persistent opening books. Excluded from the browser build, which has no
/// filesystem to keep one on.
#[cfg(not(target_arch = "wasm32"))]
pub mod book;
mod endgame;
pub mod evaluation;
pub mod model;
pub mod position;
pub mod rules;
pub mod search;
/// Self-play and measurement support. Excluded from the browser build.
#[cfg(not(target_arch = "wasm32"))]
pub mod selfplay;
// The dependency-free C ABI is used directly by WebAssembly and exported as a
// static library on iOS and Android. It is also compiled for unit tests so the
// native ABI can be exercised on the host without a simulator.
#[cfg(any(target_arch = "wasm32", target_os = "ios", target_os = "android", test))]
mod wasm;

pub use evaluation::{EvalParams, evaluate, evaluate_with};
pub use model::{
    Color, EndReason, GameOutcome, Goal, Mode, ModeRules, Move, MoveList, Piece, PieceKind, Square,
    Transform,
};
pub use position::{MoveError, NullUndo, Position, PositionError, Undo};
pub use rules::perft;
pub use search::{
    AnalysisLine, AnalysisResult, AnalysisUpdate, DEFAULT_REDUCTION_SCALE, MATE_SCORE,
    MATE_THRESHOLD, SearchLimits, SearchResult, SearchStats, SearchStopReason, SearchTuning,
    Searcher,
};

/// Version of the rule set this build implements.
///
/// Bumped when a rule that changes legal play or terminal results changes.
/// Version 2 made a side with no legal move an official draw in every mode,
/// matching the Go backend, and replaced the previous search-configurable
/// no-move behaviour. Version 3 is three changes at once, all following the Go
/// rules: Blue opens rather than Red, Intransitive's goal corners move to a1
/// and i9, and Intransitive alone has no repetition draw and loses on being
/// unable to move.
///
/// **Deliberately not bumped for [`model::QUIET_PLY_LIMIT`].** That draw was
/// meant to be part of version 3 and was simply never implemented anywhere;
/// bots have been written against the published rules, not against what the
/// engine happened to do. Calling it a new version would say the rules moved
/// under those bots, which is the opposite of what happened -- and it would
/// refuse every book on disk over a rule the books were already assumed to be
/// playing under.
///
/// A book carries this number and refuses to open against a different one, so
/// bumping it means every book on disk has to be rebuilt. Worth it when the
/// rules genuinely moved, because a book scored under other terminal results
/// recommends moves for a game nobody is playing; not worth it for a rule
/// everyone already believed was in force.
pub const RULES_VERSION: u32 = 3;

/// The side that opens a game.
///
/// `game.FirstToMove` in the Go rules, which is where the rule lives. Named
/// here because a starting position, a self-play walk and a `position startpos`
/// command all have to agree with it.
pub const FIRST_TO_MOVE: model::Color = model::Color::Blue;

/// Number of files and ranks on the square board.
pub const BOARD_SIZE: u8 = 9;

/// Number of playable squares.
pub const SQUARE_COUNT: u8 = BOARD_SIZE * BOARD_SIZE;

/// Bits corresponding to the 81 playable squares.
pub const BOARD_MASK: u128 = (1_u128 << SQUARE_COUNT) - 1;
