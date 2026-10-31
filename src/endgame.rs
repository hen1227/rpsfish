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

//! Precomputed board geometry shared by move generation and evaluation.
//!
//! The tables are `const`, so they cost no startup work and are identical in
//! native and WebAssembly builds.

//! Exact one-piece-per-side corner endgames. Retrograde analysis solves all
//! three RPS relationships; type rotation makes the other six identical.

use std::collections::VecDeque;
use std::sync::OnceLock;

use crate::board::NEIGHBORS;
use crate::{Color, Goal, PieceKind, Position, SQUARE_COUNT};

const SQUARES: usize = SQUARE_COUNT as usize;
const STATES: usize = 2 * SQUARES * SQUARES;
const UNKNOWN: i16 = i16::MIN;
static TABLES: OnceLock<[Vec<i16>; 3]> = OnceLock::new();

/// Distance to a forced win or loss, or zero for a draw. Signed distances use
/// an extra unit so that a win in one is +2 and a loss in one is -2.
/// None means this material or rule set is outside the solved domain.
pub(crate) fn probe(position: &Position) -> Option<i16> {
    let rules = position.mode().rules();
    if rules.goal != Goal::EnemyCorner
        || !rules.stalemate_loses
        || rules.uses_territory
        || position.piece_count(Color::Red) != 1
        || position.piece_count(Color::Blue) != 1
        || position.outcome().is_some()
    {
        return None;
    }
    let locate = |color| {
        PieceKind::ALL
            .into_iter()
            .find_map(|kind| {
                let pieces = position.piece_bitboard(color, kind);
                (pieces != 0).then(|| (pieces.trailing_zeros() as usize, kind))
            })
            .expect("one piece")
    };
    let (red, red_kind) = locate(Color::Red);
    let (blue, blue_kind) = locate(Color::Blue);
    let relation = if red_kind == blue_kind {
        0
    } else if red_kind.captures(blue_kind) {
        1
    } else {
        2
    };
    let tables = TABLES.get_or_init(|| std::array::from_fn(build));
    let value = tables[relation][index(red, blue, position.side_to_move().index())];
    let remaining = crate::model::QUIET_PLY_LIMIT.saturating_sub(position.quiet_plies());
    // With only two pieces a capture ends the game by stalemate. There are
    // no nonterminal clock resets, so distance alone settles the quiet clock.
    if value.unsigned_abs().saturating_sub(1) > remaining || remaining == 0 {
        Some(0)
    } else {
        Some(value)
    }
}

const fn index(red: usize, blue: usize, side: usize) -> usize {
    (red * SQUARES + blue) * 2 + side
}

const fn decode(state: usize) -> (usize, usize, usize) {
    (state / 2 / SQUARES, state / 2 % SQUARES, state % 2)
}

const fn valid(red: usize, blue: usize) -> bool {
    red != blue && red != 0 && blue != SQUARES - 1
}

fn build(relation: usize) -> Vec<i16> {
    let mut values = vec![UNKNOWN; STATES];
    let mut remaining = vec![0_u8; STATES];
    let mut longest = vec![0_i16; STATES];
    let mut queue = VecDeque::new();
    for red in 0..SQUARES {
        for blue in 0..SQUARES {
            if !valid(red, blue) {
                continue;
            }
            for side in 0..2 {
                let state = index(red, blue, side);
                let (mine, enemy, goal) = if side == 0 {
                    (red, blue, 0)
                } else {
                    (blue, red, SQUARES - 1)
                };
                let captures = relation == side + 1;
                let neighbors = NEIGHBORS[mine];
                let wins = (captures && neighbors & (1_u128 << enemy) != 0)
                    || (neighbors & (1_u128 << goal) != 0 && (enemy != goal || captures));
                if wins {
                    values[state] = 2; // win in one
                    queue.push_back(state);
                } else {
                    remaining[state] = (neighbors & !(1_u128 << enemy)).count_ones() as u8;
                    if remaining[state] == 0 {
                        values[state] = -1;
                        queue.push_back(state);
                    }
                }
            }
        }
    }
    // The FIFO processes increasing distances. A losing successor proves the
    // fastest win; only when every successor wins is the predecessor lost,
    // with distance set by the defense that delays longest.
    while let Some(state) = queue.pop_front() {
        let (red, blue, side) = decode(state);
        let moved = 1 - side;
        let destination = if moved == 0 { red } else { blue };
        let mut sources = NEIGHBORS[destination];
        while sources != 0 {
            let source = sources.trailing_zeros() as usize;
            sources &= sources - 1;
            let (previous_red, previous_blue) = if moved == 0 {
                (source, blue)
            } else {
                (red, source)
            };
            if !valid(previous_red, previous_blue) {
                continue;
            }
            let parent = index(previous_red, previous_blue, moved);
            if values[parent] != UNKNOWN {
                continue;
            }
            if values[state] < 0 {
                values[parent] = -values[state] + 1;
                queue.push_back(parent);
            } else {
                remaining[parent] -= 1;
                longest[parent] = longest[parent].max(values[state]);
                if remaining[parent] == 0 {
                    values[parent] = -longest[parent] - 1;
                    queue.push_back(parent);
                }
            }
        }
    }
    // Unresolved cycles admit indefinite defense and therefore reach the
    // game's no-capture draw. Unreachable terminal diagrams are never probed.
    for value in &mut values {
        if *value == UNKNOWN {
            *value = 0;
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Mode, Square};

    fn position(red: usize, blue: usize, side: usize, relation: usize) -> Position {
        let mut pieces = [[0_u128; 3]; 2];
        pieces[0][0] = 1_u128 << red;
        pieces[1][[0, 2, 1][relation]] = 1_u128 << blue;
        Position::from_bitboards(Mode::Intransitive, Color::ALL[side], pieces, [0, 0], 0).unwrap()
    }

    fn score(value: i16) -> i32 {
        if value > 0 {
            30_001 - i32::from(value)
        } else if value < 0 {
            -30_001 - i32::from(value)
        } else {
            0
        }
    }

    #[test]
    fn every_table_entry_agrees_with_all_legal_successors() {
        let mut checked = 0;
        let mut draws = 0;
        for relation in 0..3 {
            for red in 0..SQUARES {
                for blue in 0..SQUARES {
                    if !valid(red, blue) {
                        continue;
                    }
                    for side in 0..2 {
                        let mut pos = position(red, blue, side, relation);
                        let value = probe(&pos).unwrap();
                        let mut best = i32::MIN;
                        for &movement in &pos.legal_moves() {
                            let undo = pos.make_move(movement).unwrap();
                            let candidate = if let Some(outcome) = pos.adjudicate() {
                                assert_eq!(outcome.winner, Some(Color::ALL[side]));
                                score(2)
                            } else {
                                let reply = probe(&pos).unwrap();
                                let backed = if reply > 0 {
                                    -reply - 1
                                } else if reply < 0 {
                                    -reply + 1
                                } else {
                                    0
                                };
                                score(backed)
                            };
                            best = best.max(candidate);
                            pos.unmake_move(undo);
                        }
                        assert_eq!(score(value), best, "relation={relation} {}", pos.to_fen());
                        checked += 1;
                        draws += usize::from(value == 0);
                    }
                }
            }
        }
        assert!(checked > 37_000);
        assert!(draws < checked);
    }

    #[test]
    fn equal_distance_races_depend_on_tempo_and_the_draw_clock() {
        for side in 0..2 {
            let pos = position(30, 50, side, 0); // d4 and f6, three steps each
            assert_eq!(probe(&pos), Some(6)); // the mover wins in five plies
        }
        let mut pos = position(40, 42, 0, 0); // e5 and g5, four steps each
        for _ in 0..49 {
            for (from, to) in [(40, 49), (42, 51), (49, 40), (51, 42)] {
                pos.make_move(crate::Move::new(
                    Square::new(from).unwrap(),
                    Square::new(to).unwrap(),
                ))
                .unwrap();
            }
        }
        assert_eq!(probe(&pos), Some(0));
    }

    #[test]
    fn an_immortal_corner_holder_can_still_be_forced_to_move() {
        // Same-kind pieces cannot capture each other. Red holds i9, but it
        // is Red's turn and its only piece must leave. Blue then arrives.
        let pos = position(80, 70, 0, 0);
        assert_eq!(probe(&pos), Some(-3)); // loss in two plies
    }
}
