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

//! Search-speed benchmark: how deep the engine gets, and how fast.
//!
//! Depth is the number the user sees, so it is the number this measures. A
//! fixed wall-clock budget per position answers "how deep in the browser",
//! while the node totals stay useful when the machine is busy.
//!
//! usage: cargo run --release --example depthbench -- [--time-ms N] [--nodes N]
//!        [--depth N] [--positions N] [--plies N] [--mode M] [--multipv N]
//!        [--seed N] [--hash-mb N] [--rows]

use std::time::Duration;

use rpsfish::selfplay::{Random, random_opening};
use rpsfish::{Mode, Position, SearchLimits, Searcher};

struct Options {
    time_ms: Option<u64>,
    nodes: Option<u64>,
    depth: u8,
    positions: usize,
    plies: u16,
    modes: Vec<Mode>,
    variations: usize,
    seed: u64,
    hash_megabytes: usize,
    rows: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            time_ms: Some(3_000),
            nodes: None,
            depth: 40,
            positions: 6,
            plies: 10,
            modes: Mode::ALL.to_vec(),
            variations: 1,
            seed: 20_260_820,
            hash_megabytes: 8,
            rows: false,
        }
    }
}

fn main() {
    let options = parse(std::env::args().skip(1).collect::<Vec<_>>());
    let limits = SearchLimits {
        max_depth: options.depth,
        max_nodes: options.nodes,
        move_time: options.time_ms.map(Duration::from_millis),
    };
    println!(
        "budget: depth<={} nodes={:?} time={:?}ms multipv={} hash={}MB",
        options.depth, options.nodes, options.time_ms, options.variations, options.hash_megabytes
    );

    let mut grand_depth = 0_u32;
    let mut grand_positions = 0_u32;
    for &mode in &options.modes {
        let suite = build_suite(mode, options.positions, options.plies, options.seed);
        let mut total_depth = 0_u32;
        let mut total_nodes = 0_u64;
        let mut total_seconds = 0.0_f64;
        let mut worst = u8::MAX;
        for (index, position) in suite.iter().enumerate() {
            // A fresh searcher per position: a warm table from an unrelated
            // position would flatter the numbers.
            let mut searcher = Searcher::new(options.hash_megabytes);
            let result = searcher.analyze(*position, limits, options.variations);
            total_depth += u32::from(result.completed_depth);
            total_nodes += result.stats.nodes;
            total_seconds += result.stats.elapsed.as_secs_f64();
            worst = worst.min(result.completed_depth);
            if options.rows {
                println!("  {} #{index} rows {}", mode.id(), encode_rows(position));
            }
            println!(
                "  {:<12} #{index} depth {:>2}  seldepth {:>3}  nodes {:>10}  {:>9.0} n/s  score {:>6}",
                mode.name(),
                result.completed_depth,
                result.stats.selective_depth,
                result.stats.nodes,
                result.stats.nodes as f64 / result.stats.elapsed.as_secs_f64().max(1e-9),
                result.score,
            );
        }
        let count = suite.len().max(1) as f64;
        println!(
            "{:<14} mean depth {:>5.2}  min {:>2}  nodes {:>11}  {:>9.0} n/s",
            mode.name(),
            f64::from(total_depth) / count,
            worst,
            total_nodes,
            total_nodes as f64 / total_seconds.max(1e-9),
        );
        grand_depth += total_depth;
        grand_positions += suite.len() as u32;
    }
    println!(
        "OVERALL mean depth {:.3} over {grand_positions} positions",
        f64::from(grand_depth) / f64::from(grand_positions.max(1))
    );
}

/// The starting position plus deterministic random openings.
fn build_suite(mode: Mode, positions: usize, plies: u16, seed: u64) -> Vec<Position> {
    let mut suite = vec![Position::starting(mode)];
    let mut random = Random::new(seed ^ u64::from(mode as u8));
    while suite.len() < positions.max(1) {
        let Some(opening) = random_opening(mode, plies, &mut random) else {
            continue;
        };
        let mut position = Position::starting(mode);
        for movement in opening {
            position.make_move(movement).expect("opening must be legal");
        }
        suite.push(position);
    }
    suite
}

/// The nine board rows, so the WebAssembly harness can replay the same suite.
fn encode_rows(position: &Position) -> String {
    use rpsfish::{Color, PieceKind, Square};
    let mut result = String::new();
    for y in 0..9_u8 {
        if y > 0 {
            result.push('/');
        }
        for x in 0..9_u8 {
            let square = Square::from_xy(x, y).expect("valid square");
            let mut symbol = '.';
            for color in Color::ALL {
                for kind in PieceKind::ALL {
                    if position.piece_bitboard(color, kind) & square.bit() != 0 {
                        symbol = match (color, kind) {
                            (Color::Red, PieceKind::Rock) => 'r',
                            (Color::Red, PieceKind::Paper) => 'p',
                            (Color::Red, PieceKind::Scissors) => 's',
                            (Color::Blue, PieceKind::Rock) => 'R',
                            (Color::Blue, PieceKind::Paper) => 'P',
                            (Color::Blue, PieceKind::Scissors) => 'S',
                        };
                    }
                }
            }
            result.push(symbol);
        }
    }
    result.push(' ');
    result.push(if position.side_to_move() == rpsfish::Color::Red {
        'r'
    } else {
        'b'
    });
    result
}

fn parse(arguments: Vec<String>) -> Options {
    let mut options = Options::default();
    let mut index = 0;
    while index < arguments.len() {
        let flag = arguments[index].as_str();
        let value = |offset: usize| -> String {
            arguments
                .get(index + offset)
                .cloned()
                .unwrap_or_else(|| panic!("missing value for {flag}"))
        };
        match flag {
            "--time-ms" => {
                let parsed: u64 = value(1).parse().expect("integer");
                options.time_ms = (parsed > 0).then_some(parsed);
                index += 2;
            }
            "--nodes" => {
                let parsed: u64 = value(1).parse().expect("integer");
                options.nodes = (parsed > 0).then_some(parsed);
                index += 2;
            }
            "--depth" => {
                options.depth = value(1).parse().expect("integer");
                index += 2;
            }
            "--positions" => {
                options.positions = value(1).parse().expect("integer");
                index += 2;
            }
            "--plies" => {
                options.plies = value(1).parse().expect("integer");
                index += 2;
            }
            "--multipv" => {
                options.variations = value(1).parse().expect("integer");
                index += 2;
            }
            "--seed" => {
                options.seed = value(1).parse().expect("integer");
                index += 2;
            }
            "--hash-mb" => {
                options.hash_megabytes = value(1).parse().expect("integer");
                index += 2;
            }
            "--mode" => {
                options.modes = match value(1).as_str() {
                    "all" => Mode::ALL.to_vec(),
                    name => vec![Mode::parse(name).expect("known mode")],
                };
                index += 2;
            }
            "--rows" => {
                options.rows = true;
                index += 1;
            }
            unknown => panic!("unknown flag {unknown}"),
        }
    }
    options
}
