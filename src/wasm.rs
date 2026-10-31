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

//! Dependency-free C ABI used by the browser analysis worker and the iOS and
//! Android builds.
//!
//! The board crosses the ABI as low/high `u64` halves of the core's eight
//! `u128` bitboards. Results stay in adapter-owned storage and are read through
//! small scalar getters, avoiding an allocator or language-specific glue
//! generator. WebAssembly state is thread-local; iOS and Android state is
//! process-global and mutex-protected, because a serial GCD queue may migrate
//! between threads and nothing promises an Android caller one thread either.
//! The same exported symbols are linked into both static libraries; only the
//! WebAssembly clock import below is platform-specific.

// Rust 2024 classifies stable symbol names as unsafe attributes. The exported
// functions themselves contain no unsafe operations.
#![allow(unsafe_code)]

#[cfg(not(any(target_os = "ios", target_os = "android")))]
use std::cell::RefCell;
#[cfg(any(target_os = "ios", target_os = "android"))]
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use crate::{
    AnalysisResult, Color, Mode, Move, PieceKind, Position, SearchLimits, SearchStopReason,
    Searcher, Square,
};

// Eight rather than three because the bot ladder samples among root moves to
// be weak, and three near-equivalent moves is not enough material to be weak
// with. Measured top1-to-topN score gaps (600 random openings per mode, depths
// 2-11) roughly triple going from three lines to eight, which is the range a
// difficulty profile spends. Each line costs a full-window root search, so
// callers that only want the best move should still ask for one.
const MAX_VARIATIONS: usize = 8;
const MAX_PV_LENGTH: usize = 127;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
unsafe extern "C" {
    fn rpsfish_now_ms() -> f64;
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn monotonic_time_ms() -> f64 {
    // The browser worker supplies performance.now(), which is monotonic and
    // lets the core enforce time limits during a synchronous WASM search.
    unsafe { rpsfish_now_ms() }
}

#[derive(Clone, Copy)]
struct WasmLine {
    from: i32,
    to: i32,
    score: i32,
    pv_from: [i32; MAX_PV_LENGTH],
    pv_to: [i32; MAX_PV_LENGTH],
    pv_count: i32,
}

impl WasmLine {
    const EMPTY: Self = Self {
        from: -1,
        to: -1,
        score: 0,
        pv_from: [-1; MAX_PV_LENGTH],
        pv_to: [-1; MAX_PV_LENGTH],
        pv_count: 0,
    };
}

#[derive(Clone, Copy)]
struct WasmAnalysis {
    lines: [WasmLine; MAX_VARIATIONS],
    count: i32,
    score: i32,
    depth: i32,
    selective_depth: i32,
    confidence: i32,
    nodes: i32,
    elapsed_ms: i32,
    stop_reason: i32,
}

impl WasmAnalysis {
    const EMPTY: Self = Self {
        lines: [WasmLine::EMPTY; MAX_VARIATIONS],
        count: 0,
        score: 0,
        depth: 0,
        selective_depth: 0,
        confidence: 0,
        nodes: 0,
        elapsed_ms: 0,
        stop_reason: 0,
    };

    fn from_result(result: AnalysisResult) -> Self {
        let mut analysis = Self {
            score: result.score,
            depth: i32::from(result.completed_depth),
            selective_depth: i32::from(result.stats.selective_depth),
            confidence: i32::from(result.confidence),
            nodes: i32::try_from(result.stats.nodes).unwrap_or(i32::MAX),
            elapsed_ms: i32::try_from(result.stats.elapsed.as_millis()).unwrap_or(i32::MAX),
            stop_reason: stop_reason_code(result.stop_reason),
            ..Self::EMPTY
        };
        for (index, line) in result.lines.into_iter().take(MAX_VARIATIONS).enumerate() {
            let mut wasm_line = WasmLine {
                from: i32::from(line.movement.from().index()),
                to: i32::from(line.movement.to().index()),
                score: line.score,
                ..WasmLine::EMPTY
            };
            for (ply, movement) in line
                .principal_variation
                .into_iter()
                .take(MAX_PV_LENGTH)
                .enumerate()
            {
                wasm_line.pv_from[ply] = i32::from(movement.from().index());
                wasm_line.pv_to[ply] = i32::from(movement.to().index());
                wasm_line.pv_count += 1;
            }
            analysis.lines[index] = wasm_line;
            analysis.count += 1;
        }
        analysis
    }
}

#[cfg(not(any(target_os = "ios", target_os = "android")))]
thread_local! {
    static LAST_ANALYSIS: RefCell<WasmAnalysis> = const { RefCell::new(WasmAnalysis::EMPTY) };
    static POSITION_HISTORY: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    /// Root moves the next analysis is restricted to, cleared once it runs.
    static ROOT_FILTER: RefCell<Vec<Move>> = const { RefCell::new(Vec::new()) };
    // A deep search visits tens of millions of nodes, so a small table
    // throws away most of the work it does. Thirty-two megabytes of browser
    // memory buys two million entries.
    static SEARCHER: RefCell<Searcher> = RefCell::new(Searcher::new(32));
}

#[cfg(any(target_os = "ios", target_os = "android"))]
static LAST_ANALYSIS: Mutex<WasmAnalysis> = Mutex::new(WasmAnalysis::EMPTY);
#[cfg(any(target_os = "ios", target_os = "android"))]
static POSITION_HISTORY: Mutex<Vec<u64>> = Mutex::new(Vec::new());
#[cfg(any(target_os = "ios", target_os = "android"))]
static ROOT_FILTER: Mutex<Vec<Move>> = Mutex::new(Vec::new());
#[cfg(any(target_os = "ios", target_os = "android"))]
static SEARCHER: LazyLock<Mutex<Searcher>> = LazyLock::new(|| Mutex::new(Searcher::new(32)));

fn with_last_analysis<Result>(action: impl FnOnce(&mut WasmAnalysis) -> Result) -> Result {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        action(
            &mut LAST_ANALYSIS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        LAST_ANALYSIS.with(|state| action(&mut state.borrow_mut()))
    }
}

fn with_history<Result>(action: impl FnOnce(&mut Vec<u64>) -> Result) -> Result {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        action(
            &mut POSITION_HISTORY
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        POSITION_HISTORY.with(|state| action(&mut state.borrow_mut()))
    }
}

fn with_root_filter<Result>(action: impl FnOnce(&mut Vec<Move>) -> Result) -> Result {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        action(
            &mut ROOT_FILTER
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        ROOT_FILTER.with(|state| action(&mut state.borrow_mut()))
    }
}

fn with_searcher<Result>(action: impl FnOnce(&mut Searcher) -> Result) -> Result {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        action(
            &mut SEARCHER
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        SEARCHER.with(|state| action(&mut state.borrow_mut()))
    }
}

#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analyze(
    mode: i32,
    side_to_move: i32,
    ply: i32,
    red_rock_low: u64,
    red_rock_high: u64,
    red_paper_low: u64,
    red_paper_high: u64,
    red_scissors_low: u64,
    red_scissors_high: u64,
    blue_rock_low: u64,
    blue_rock_high: u64,
    blue_paper_low: u64,
    blue_paper_high: u64,
    blue_scissors_low: u64,
    blue_scissors_high: u64,
    red_territory_low: u64,
    red_territory_high: u64,
    blue_territory_low: u64,
    blue_territory_high: u64,
    max_depth: i32,
    max_nodes: i32,
    max_time_ms: i32,
    variations: i32,
) -> i32 {
    let Ok(position) = position_from_abi(
        mode,
        side_to_move,
        ply,
        [
            red_rock_low,
            red_rock_high,
            red_paper_low,
            red_paper_high,
            red_scissors_low,
            red_scissors_high,
            blue_rock_low,
            blue_rock_high,
            blue_paper_low,
            blue_paper_high,
            blue_scissors_low,
            blue_scissors_high,
            red_territory_low,
            red_territory_high,
            blue_territory_low,
            blue_territory_high,
        ],
    ) else {
        return -2;
    };

    let prior_hashes = with_history(|history| history.clone());
    // The filter is spent by the search it was built for. A restriction that
    // outlived its call would silently answer the next question about a
    // different position with the wrong root.
    let root_moves = with_root_filter(core::mem::take);
    let result = with_searcher(|searcher| {
        searcher.analyze_root_moves(
            position,
            SearchLimits {
                max_depth: u8::try_from(max_depth.clamp(1, MAX_PV_LENGTH as i32)).unwrap_or(8),
                max_nodes: Some(u64::try_from(max_nodes.max(1)).unwrap_or(350_000)),
                move_time: Some(Duration::from_millis(
                    u64::try_from(max_time_ms.max(1)).unwrap_or(2_500),
                )),
            },
            usize::try_from(variations.clamp(1, MAX_VARIATIONS as i32)).unwrap_or(1),
            &prior_hashes,
            &root_moves,
        )
    });
    let analysis = WasmAnalysis::from_result(result);
    let count = analysis.count;
    with_last_analysis(|slot| *slot = analysis);
    count
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_history_clear() {
    with_history(Vec::clear);
}

/// How many prior positions the engine currently holds.
#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_history_len() -> i32 {
    with_history(|history| i32::try_from(history.len()).unwrap_or(i32::MAX))
}

/// Drop prior positions past `length`, keeping the rest.
///
/// Reviewing a game walks one position at a time, and rebuilding the whole
/// history for each step is quadratic in the length of the game. Truncating
/// and pushing one position makes it linear.
#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_history_truncate(length: i32) -> i32 {
    with_history(|history| {
        if let Ok(length) = usize::try_from(length) {
            history.truncate(length);
        }
        i32::try_from(history.len()).unwrap_or(i32::MAX)
    })
}

/// Forget any root restriction that has not been used yet.
#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_root_filter_clear() {
    with_root_filter(Vec::clear);
}

/// Restrict the next `rpsfish_analyze` to this move, and return how many
/// moves the restriction now holds.
///
/// Squares are board indices, `y * 9 + x`. A restricted analysis reports no
/// lines at all when none of the requested moves is legal in the position,
/// which is how a caller learns it asked about the wrong board.
#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_root_filter_push(from: i32, to: i32) -> i32 {
    let square = |index: i32| u8::try_from(index).ok().and_then(Square::new);
    let (Some(from), Some(to)) = (square(from), square(to)) else {
        return -2;
    };
    with_root_filter(|filter| {
        filter.push(Move::new(from, to));
        i32::try_from(filter.len()).unwrap_or(i32::MAX)
    })
}

#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_history_push(
    mode: i32,
    side_to_move: i32,
    ply: i32,
    red_rock_low: u64,
    red_rock_high: u64,
    red_paper_low: u64,
    red_paper_high: u64,
    red_scissors_low: u64,
    red_scissors_high: u64,
    blue_rock_low: u64,
    blue_rock_high: u64,
    blue_paper_low: u64,
    blue_paper_high: u64,
    blue_scissors_low: u64,
    blue_scissors_high: u64,
    red_territory_low: u64,
    red_territory_high: u64,
    blue_territory_low: u64,
    blue_territory_high: u64,
) -> i32 {
    let Ok(position) = position_from_abi(
        mode,
        side_to_move,
        ply,
        [
            red_rock_low,
            red_rock_high,
            red_paper_low,
            red_paper_high,
            red_scissors_low,
            red_scissors_high,
            blue_rock_low,
            blue_rock_high,
            blue_paper_low,
            blue_paper_high,
            blue_scissors_low,
            blue_scissors_high,
            red_territory_low,
            red_territory_high,
            blue_territory_low,
            blue_territory_high,
        ],
    ) else {
        return -2;
    };
    with_history(|history| {
        history.push(position.hash());
        i32::try_from(history.len()).unwrap_or(i32::MAX)
    })
}

/// The shape of this ABI.
///
/// Version 4 adds incremental history maintenance (`rpsfish_history_len`,
/// `rpsfish_history_truncate`) and a root-move restriction
/// (`rpsfish_root_filter_clear`, `rpsfish_root_filter_push`), which together
/// let a caller review a whole game without rebuilding state per position.
#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_abi_version() -> i32 {
    4
}

/// The rule set this build implements.
///
/// The ABI shape is unchanged, but terminal results are not: a side with no
/// legal move is now an official draw. A worker that caches analyses across
/// engine updates must key them on this value as well as on the ABI version.
#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_rules_version() -> i32 {
    crate::RULES_VERSION as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_search_clear() {
    with_searcher(Searcher::clear);
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_count() -> i32 {
    with_last_analysis(|slot| slot.count)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_from(index: i32) -> i32 {
    analysis_line(index).map_or(-1, |line| line.from)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_to(index: i32) -> i32 {
    analysis_line(index).map_or(-1, |line| line.to)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_line_score(index: i32) -> i32 {
    analysis_line(index).map_or(0, |line| line.score)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_pv_length(index: i32) -> i32 {
    analysis_line(index).map_or(0, |line| line.pv_count)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_pv_from(index: i32, ply: i32) -> i32 {
    analysis_line(index)
        .and_then(|line| usize::try_from(ply).ok().map(|ply| (line, ply)))
        .and_then(|(line, ply)| line.pv_from.get(ply).copied())
        .unwrap_or(-1)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_pv_to(index: i32, ply: i32) -> i32 {
    analysis_line(index)
        .and_then(|line| usize::try_from(ply).ok().map(|ply| (line, ply)))
        .and_then(|(line, ply)| line.pv_to.get(ply).copied())
        .unwrap_or(-1)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_score() -> i32 {
    with_last_analysis(|slot| slot.score)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_depth() -> i32 {
    with_last_analysis(|slot| slot.depth)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_selective_depth() -> i32 {
    with_last_analysis(|slot| slot.selective_depth)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_confidence() -> i32 {
    with_last_analysis(|slot| slot.confidence)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_nodes() -> i32 {
    with_last_analysis(|slot| slot.nodes)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_elapsed_ms() -> i32 {
    with_last_analysis(|slot| slot.elapsed_ms)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_stop_reason() -> i32 {
    with_last_analysis(|slot| slot.stop_reason)
}

fn analysis_line(index: i32) -> Option<WasmLine> {
    let index = usize::try_from(index).ok()?;
    with_last_analysis(|slot| slot.lines.get(index).copied())
}

fn join_halves(low: u64, high: u64) -> u128 {
    u128::from(low) | (u128::from(high) << 64)
}

fn position_from_abi(
    mode: i32,
    side_to_move: i32,
    ply: i32,
    halves: [u64; 16],
) -> Result<Position, ()> {
    let mode = mode_from_code(mode).ok_or(())?;
    let side_to_move = color_from_code(side_to_move).ok_or(())?;
    let pieces = [
        [
            join_halves(halves[0], halves[1]),
            join_halves(halves[2], halves[3]),
            join_halves(halves[4], halves[5]),
        ],
        [
            join_halves(halves[6], halves[7]),
            join_halves(halves[8], halves[9]),
            join_halves(halves[10], halves[11]),
        ],
    ];
    let territory = [
        join_halves(halves[12], halves[13]),
        join_halves(halves[14], halves[15]),
    ];
    Position::from_bitboards(
        mode,
        side_to_move,
        pieces,
        territory,
        u16::try_from(ply.max(0)).unwrap_or(u16::MAX),
    )
    .map_err(|_| ())
}

const fn mode_from_code(code: i32) -> Option<Mode> {
    match code {
        1 => Some(Mode::TotalWar),
        2 => Some(Mode::Infiltration),
        3 => Some(Mode::Intransitive),
        _ => None,
    }
}

const fn color_from_code(code: i32) -> Option<Color> {
    match code {
        0 => Some(Color::Red),
        1 => Some(Color::Blue),
        _ => None,
    }
}

const fn stop_reason_code(reason: SearchStopReason) -> i32 {
    match reason {
        SearchStopReason::DepthLimit => 0,
        SearchStopReason::NodeLimit => 1,
        SearchStopReason::TimeLimit => 2,
        SearchStopReason::Cancelled => 3,
        SearchStopReason::TerminalPosition => 4,
        SearchStopReason::Repetition => 5,
        SearchStopReason::NoLegalMove => 6,
        // Appended rather than slotted in beside Repetition, which is where it
        // belongs by meaning: these numbers are the ABI the worker decodes by
        // index, so an existing one may never move.
        SearchStopReason::NoCapture => 7,
    }
}

// Keep this import used in the ABI layout documentation and make accidental
// PieceKind reordering visible during compilation.
const _: [PieceKind; 3] = PieceKind::ALL;

#[cfg(test)]
mod tests {
    use super::*;

    fn halves(value: u128) -> [u64; 2] {
        [value as u64, (value >> 64) as u64]
    }

    fn encoded(position: &Position) -> [u64; 16] {
        let mut words = [0_u64; 16];
        let mut offset = 0;
        for color in [Color::Red, Color::Blue] {
            for kind in PieceKind::ALL {
                let pair = halves(position.piece_bitboard(color, kind));
                words[offset] = pair[0];
                words[offset + 1] = pair[1];
                offset += 2;
            }
        }
        for color in [Color::Red, Color::Blue] {
            let pair = halves(position.territory_bitboard(color));
            words[offset] = pair[0];
            words[offset + 1] = pair[1];
            offset += 2;
        }
        words
    }

    #[test]
    fn c_abi_round_trips_every_starting_position() {
        for (code, mode) in [
            (1, Mode::TotalWar),
            (2, Mode::Infiltration),
            (3, Mode::Intransitive),
        ] {
            let position = Position::starting(mode);
            let side = i32::from(position.side_to_move() == Color::Blue);
            let decoded = position_from_abi(code, side, 0, encoded(&position))
                .expect("built-in mode must be accepted by the ABI");
            assert_eq!(decoded, position, "{mode} must survive the ABI");
        }
    }

    #[test]
    fn c_abi_rejects_unknown_modes_and_invalid_squares() {
        assert_eq!(position_from_abi(99, 0, 0, [0; 16]), Err(()));
        rpsfish_root_filter_clear();
        assert_eq!(rpsfish_root_filter_push(-1, 0), -2);
        assert_eq!(
            rpsfish_root_filter_push(0, i32::from(crate::SQUARE_COUNT)),
            -2
        );
    }

    #[test]
    fn c_abi_versions_match_the_core() {
        assert_eq!(rpsfish_abi_version(), 4);
        assert_eq!(rpsfish_rules_version(), crate::RULES_VERSION as i32);
    }
}
