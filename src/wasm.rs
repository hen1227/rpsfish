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

//! Dependency-free WebAssembly adapter used by the browser analysis worker.
//!
//! The board crosses the ABI as low/high `u64` halves of the core's eight
//! `u128` bitboards. Results stay in worker-local storage and are read through
//! small scalar getters, avoiding an allocator or JavaScript glue generator.

// Rust 2024 classifies stable symbol names as unsafe attributes. The exported
// functions themselves contain no unsafe operations.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::time::Duration;

use crate::{
    AnalysisResult, Color, Mode, PieceKind, Position, SearchLimits, SearchStopReason, Searcher,
};

const MAX_VARIATIONS: usize = 3;
const MAX_PV_LENGTH: usize = 127;

#[link(wasm_import_module = "env")]
unsafe extern "C" {
    fn rpsfish_now_ms() -> f64;
}

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

thread_local! {
    static LAST_ANALYSIS: RefCell<WasmAnalysis> = const { RefCell::new(WasmAnalysis::EMPTY) };
    static POSITION_HISTORY: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    // A deep search visits tens of millions of nodes, so a small table
    // throws away most of the work it does. Thirty-two megabytes of browser
    // memory buys two million entries.
    static SEARCHER: RefCell<Searcher> = RefCell::new(Searcher::new(32));
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

    let prior_hashes = POSITION_HISTORY.with(|history| history.borrow().clone());
    let result = SEARCHER.with(|searcher| {
        searcher.borrow_mut().analyze_with_context(
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
        )
    });
    let analysis = WasmAnalysis::from_result(result);
    let count = analysis.count;
    LAST_ANALYSIS.with(|slot| slot.replace(analysis));
    count
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_history_clear() {
    POSITION_HISTORY.with(|history| history.borrow_mut().clear());
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
    POSITION_HISTORY.with(|history| {
        let mut history = history.borrow_mut();
        history.push(position.hash());
        i32::try_from(history.len()).unwrap_or(i32::MAX)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_abi_version() -> i32 {
    3
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
    SEARCHER.with(|searcher| searcher.borrow_mut().clear());
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_count() -> i32 {
    LAST_ANALYSIS.with(|slot| slot.borrow().count)
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
    LAST_ANALYSIS.with(|slot| slot.borrow().score)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_depth() -> i32 {
    LAST_ANALYSIS.with(|slot| slot.borrow().depth)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_selective_depth() -> i32 {
    LAST_ANALYSIS.with(|slot| slot.borrow().selective_depth)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_confidence() -> i32 {
    LAST_ANALYSIS.with(|slot| slot.borrow().confidence)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_nodes() -> i32 {
    LAST_ANALYSIS.with(|slot| slot.borrow().nodes)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_elapsed_ms() -> i32 {
    LAST_ANALYSIS.with(|slot| slot.borrow().elapsed_ms)
}

#[unsafe(no_mangle)]
pub extern "C" fn rpsfish_analysis_stop_reason() -> i32 {
    LAST_ANALYSIS.with(|slot| slot.borrow().stop_reason)
}

fn analysis_line(index: i32) -> Option<WasmLine> {
    let index = usize::try_from(index).ok()?;
    LAST_ANALYSIS.with(|slot| slot.borrow().lines.get(index).copied())
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
        0 => Some(Mode::Annihilation),
        1 => Some(Mode::TotalWar),
        2 => Some(Mode::Infiltration),
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
    }
}

// Keep this import used in the ABI layout documentation and make accidental
// PieceKind reordering visible during compilation.
const _: [PieceKind; 3] = PieceKind::ALL;
