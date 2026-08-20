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
pub mod evaluation;
pub mod model;
pub mod position;
pub mod rules;
pub mod search;
/// Self-play and measurement support. Excluded from the browser build.
#[cfg(not(target_arch = "wasm32"))]
pub mod selfplay;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use evaluation::{EvalParams, evaluate, evaluate_with};
pub use model::{
    Color, EndReason, GameOutcome, Mode, ModeRules, Move, MoveList, Piece, PieceKind, Square,
};
pub use position::{MoveError, NullUndo, Position, PositionError, Undo};
pub use rules::perft;
pub use search::{
    AnalysisLine, AnalysisResult, AnalysisUpdate, DEFAULT_REDUCTION_SCALE, NoMoveOutcome,
    SearchLimits, SearchResult, SearchStats, SearchStopReason, SearchTuning, Searcher,
};

/// Version of the rule set this build implements.
///
/// Bumped when a rule that changes legal play or terminal results changes.
/// Version 2 makes a side with no legal move an official draw in every mode,
/// matching the Go backend, and replaces the previous search-configurable
/// no-move behaviour.
pub const RULES_VERSION: u32 = 2;

/// Number of files and ranks on the square board.
pub const BOARD_SIZE: u8 = 9;

/// Number of playable squares.
pub const SQUARE_COUNT: u8 = BOARD_SIZE * BOARD_SIZE;

/// Bits corresponding to the 81 playable squares.
pub const BOARD_MASK: u128 = (1_u128 << SQUARE_COUNT) - 1;
