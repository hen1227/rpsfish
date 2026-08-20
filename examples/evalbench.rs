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

//! Compare the cost of the current single-pass feature extraction against the
//! previous move-list based approach, then report search speed per mode.

use std::time::Instant;

use rpsfish::model::PieceKind;
use rpsfish::selfplay::{Random, random_opening};
use rpsfish::{Color, Mode, Position, SearchLimits, Searcher, evaluate};

/// The feature gathering the evaluator used before: one full `MoveList` per
/// side per feature. Kept here only as a benchmark reference point.
fn legacy_features(position: &Position) -> i32 {
    let mut total = 0_i32;
    for color in Color::ALL {
        total += position.legal_moves_for(color).len() as i32;
        total += position
            .legal_moves_for(color)
            .iter()
            .filter(|movement| position.piece_at(movement.to()).is_some())
            .count() as i32;
        total += position
            .legal_moves_for(color)
            .iter()
            .filter(|movement| position.territory_owner(movement.to()).is_none())
            .count() as i32;
    }
    for kind in PieceKind::ALL {
        total += position.piece_bitboard(Color::Red, kind).count_ones() as i32;
    }
    total
}

fn main() {
    let mut positions = Vec::new();
    let mut random = Random::new(20_260_820);
    for mode in Mode::ALL {
        for _ in 0..40 {
            let Some(opening) = random_opening(mode, 12, &mut random) else {
                continue;
            };
            let mut position = Position::starting(mode);
            for movement in opening {
                position.make_move(movement).expect("legal");
            }
            positions.push(position);
        }
    }
    println!("sample positions: {}", positions.len());

    let rounds = 4_000;
    let started = Instant::now();
    let mut sink = 0_i64;
    for _ in 0..rounds {
        for position in &positions {
            sink += i64::from(evaluate(position));
        }
    }
    let current = started.elapsed();

    let started = Instant::now();
    let mut legacy_sink = 0_i64;
    for _ in 0..rounds {
        for position in &positions {
            legacy_sink += i64::from(legacy_features(position));
        }
    }
    let legacy = started.elapsed();

    let calls = (rounds * positions.len()) as f64;
    println!(
        "single-pass evaluate:      {:>8.1} ns/call  ({calls:.0} calls)",
        current.as_secs_f64() * 1e9 / calls
    );
    println!(
        "move-list feature gather:  {:>8.1} ns/call  ({:.1}x slower, and it is only part of the old evaluator)",
        legacy.as_secs_f64() * 1e9 / calls,
        legacy.as_secs_f64() / current.as_secs_f64()
    );
    println!("checksums {sink} {legacy_sink}");
    println!();

    for mode in Mode::ALL {
        let mut searcher = Searcher::new(32);
        let limits = SearchLimits {
            max_depth: 64,
            max_nodes: Some(3_000_000),
            move_time: None,
        };
        let position = Position::starting(mode);
        let result = searcher.analyze(position, limits, 1);
        let seconds = result.stats.elapsed.as_secs_f64();
        println!(
            "{:<14} depth {:>2}  seldepth {:>3}  nodes {:>9}  {:>10.0} nodes/s",
            mode.name(),
            result.completed_depth,
            result.stats.selective_depth,
            result.stats.nodes,
            result.stats.nodes as f64 / seconds.max(1e-9),
        );
    }
}
