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

use crate::{BOARD_SIZE, SQUARE_COUNT};

/// Squares reachable in one king-style step, indexed by square.
pub const NEIGHBORS: [u128; SQUARE_COUNT as usize] = build_neighbors();

/// Squares on Red's infiltration target row (`y == 0`).
pub const RED_TARGET_MASK: u128 = (1_u128 << BOARD_SIZE) - 1;

/// Squares on Blue's infiltration target row (`y == BOARD_SIZE - 1`).
pub const BLUE_TARGET_MASK: u128 = RED_TARGET_MASK << (SQUARE_COUNT - BOARD_SIZE);

/// The single square Red wins on in Intransitive: the near file of Red's target
/// row, which is the a1 corner Blue's army opened in.
pub const RED_CORNER_MASK: u128 = 1;

/// The single square Blue wins on in Intransitive: the far file of Blue's
/// target row, which is the corner Red's army opened in.
pub const BLUE_CORNER_MASK: u128 = 1_u128 << (SQUARE_COUNT - 1);

/// Red's goal corner together with the three squares that reach it.
///
/// The whole approach to an Intransitive win, and the reason the mode plays
/// nothing like Infiltration: a rank goal is nine squares wide and cannot be
/// sealed, while this is four squares and can. Whoever owns these owns the
/// race, so evaluation counts them directly.
pub const RED_CORNER_SHELL: u128 = RED_CORNER_MASK | NEIGHBORS[0];

/// Blue's goal corner together with the three squares that reach it.
pub const BLUE_CORNER_SHELL: u128 = BLUE_CORNER_MASK | NEIGHBORS[SQUARE_COUNT as usize - 1];

/// Squares on each row, indexed by `y`.
pub const ROWS: [u128; BOARD_SIZE as usize] = build_rows();

/// Single-bit masks indexed by square.
///
/// A 128-bit variable shift costs several instructions on a 64-bit target and
/// more than that on WebAssembly, and `Square::bit` is called on nearly every
/// node, so the shift is precomputed.
pub const SQUARE_BITS: [u128; SQUARE_COUNT as usize] = build_square_bits();

/// Union of the king neighborhoods of a bitboard, with no file wraparound.
/// Used for type-specific attack maps and breadth-first corner routes.
#[must_use]
pub const fn neighbors_of(squares: u128) -> u128 {
    const FILE_A: u128 = crate::BOARD_MASK / ((1_u128 << BOARD_SIZE) - 1);
    const FILE_I: u128 = FILE_A << (BOARD_SIZE - 1);
    let horizontal = ((squares & !FILE_A) >> 1) | ((squares & !FILE_I) << 1);
    let wide = squares | horizontal;
    (horizontal | (wide >> BOARD_SIZE) | (wide << BOARD_SIZE)) & crate::BOARD_MASK
}

const fn build_neighbors() -> [u128; SQUARE_COUNT as usize] {
    let mut table = [0_u128; SQUARE_COUNT as usize];
    let mut index = 0_usize;
    while index < SQUARE_COUNT as usize {
        let x = (index % BOARD_SIZE as usize) as i32;
        let y = (index / BOARD_SIZE as usize) as i32;
        let mut mask = 0_u128;
        let mut y_offset = -1_i32;
        while y_offset <= 1 {
            let mut x_offset = -1_i32;
            while x_offset <= 1 {
                if x_offset != 0 || y_offset != 0 {
                    let neighbor_x = x + x_offset;
                    let neighbor_y = y + y_offset;
                    if neighbor_x >= 0
                        && neighbor_y >= 0
                        && neighbor_x < BOARD_SIZE as i32
                        && neighbor_y < BOARD_SIZE as i32
                    {
                        mask |= 1_u128 << (neighbor_y * BOARD_SIZE as i32 + neighbor_x);
                    }
                }
                x_offset += 1;
            }
            y_offset += 1;
        }
        table[index] = mask;
        index += 1;
    }
    table
}

const fn build_square_bits() -> [u128; SQUARE_COUNT as usize] {
    let mut table = [0_u128; SQUARE_COUNT as usize];
    let mut index = 0_usize;
    while index < SQUARE_COUNT as usize {
        table[index] = 1_u128 << index;
        index += 1;
    }
    table
}

const fn build_rows() -> [u128; BOARD_SIZE as usize] {
    let mut table = [0_u128; BOARD_SIZE as usize];
    let mut y = 0_usize;
    while y < BOARD_SIZE as usize {
        table[y] = RED_TARGET_MASK << (y * BOARD_SIZE as usize);
        y += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::{
        BLUE_CORNER_MASK, BLUE_CORNER_SHELL, BLUE_TARGET_MASK, NEIGHBORS, RED_CORNER_MASK,
        RED_CORNER_SHELL, RED_TARGET_MASK, ROWS, SQUARE_BITS,
    };
    use crate::model::Square;
    use crate::{BOARD_MASK, BOARD_SIZE, SQUARE_COUNT};

    #[test]
    fn neighbor_counts_match_board_geometry() {
        let corner = Square::from_xy(0, 0).expect("valid square");
        let edge = Square::from_xy(4, 0).expect("valid square");
        let center = Square::from_xy(4, 4).expect("valid square");
        assert_eq!(NEIGHBORS[usize::from(corner.index())].count_ones(), 3);
        assert_eq!(NEIGHBORS[usize::from(edge.index())].count_ones(), 5);
        assert_eq!(NEIGHBORS[usize::from(center.index())].count_ones(), 8);
    }

    #[test]
    fn bulk_neighbors_match_the_square_table() {
        for &bits in SQUARE_BITS.iter().chain([crate::BOARD_MASK, 0].iter()) {
            let expected = SQUARE_BITS
                .iter()
                .enumerate()
                .filter(|(_, bit)| bits & **bit != 0)
                .fold(0, |mask, (index, _)| mask | NEIGHBORS[index]);
            assert_eq!(super::neighbors_of(bits), expected);
        }
    }

    #[test]
    fn neighborhood_is_symmetric_and_on_board() {
        for index in 0..SQUARE_COUNT {
            let mask = NEIGHBORS[usize::from(index)];
            assert_eq!(mask & !BOARD_MASK, 0);
            assert_eq!(mask & (1_u128 << index), 0);
            let mut remaining = mask;
            while remaining != 0 {
                let neighbor = remaining.trailing_zeros() as usize;
                assert_ne!(NEIGHBORS[neighbor] & (1_u128 << index), 0);
                remaining &= remaining - 1;
            }
        }
    }

    #[test]
    fn undirected_edge_count_matches_move_capacity() {
        let directed: u32 = (0..SQUARE_COUNT)
            .map(|index| NEIGHBORS[usize::from(index)].count_ones())
            .sum();
        assert_eq!(directed / 2, crate::model::MAX_MOVES as u32);
    }

    #[test]
    fn square_bits_match_the_shift_they_replace() {
        for index in 0..SQUARE_COUNT {
            assert_eq!(SQUARE_BITS[usize::from(index)], 1_u128 << index);
        }
    }

    #[test]
    fn corner_masks_are_single_squares_at_the_ends_of_the_target_rows() {
        // The corner goal has to be one end of the rank goal the same side
        // would have had, or the two modes would disagree about which way Red
        // is running. `goalCorner` in the Go rules says the same thing.
        assert_eq!(RED_CORNER_MASK.count_ones(), 1);
        assert_eq!(BLUE_CORNER_MASK.count_ones(), 1);
        assert_eq!(RED_CORNER_MASK & RED_TARGET_MASK, RED_CORNER_MASK);
        assert_eq!(BLUE_CORNER_MASK & BLUE_TARGET_MASK, BLUE_CORNER_MASK);

        let red = Square::from_xy(0, 0).expect("valid square");
        let blue = Square::from_xy(BOARD_SIZE - 1, BOARD_SIZE - 1).expect("valid square");
        assert_eq!(RED_CORNER_MASK, red.bit());
        assert_eq!(BLUE_CORNER_MASK, blue.bit());

        // Opposite corners, so no square is a goal for both sides and the two
        // races never collide on one tile.
        assert_eq!(RED_CORNER_MASK & BLUE_CORNER_MASK, 0);
    }

    #[test]
    fn corner_shells_are_the_corner_and_its_three_approaches() {
        // Four squares, not nine: this is the number that makes an
        // Intransitive corner sealable where an Infiltration rank is not.
        for (shell, corner) in [
            (RED_CORNER_SHELL, RED_CORNER_MASK),
            (BLUE_CORNER_SHELL, BLUE_CORNER_MASK),
        ] {
            assert_eq!(shell.count_ones(), 4);
            assert_eq!(shell & corner, corner);
            let index = corner.trailing_zeros() as usize;
            assert_eq!(shell, corner | NEIGHBORS[index]);
        }

        // The two shells are far enough apart on a nine-wide board that no
        // single piece defends one while attacking the other.
        assert_eq!(RED_CORNER_SHELL & BLUE_CORNER_SHELL, 0);
    }

    #[test]
    fn target_masks_are_the_outer_rows() {
        assert_eq!(RED_TARGET_MASK, ROWS[0]);
        assert_eq!(BLUE_TARGET_MASK, ROWS[usize::from(BOARD_SIZE - 1)]);
        assert_eq!(
            ROWS.iter().fold(0_u128, |result, row| result | row),
            BOARD_MASK
        );
    }
}
