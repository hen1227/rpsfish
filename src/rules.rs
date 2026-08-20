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

use crate::BOARD_SIZE;
use crate::board::NEIGHBORS;
use crate::model::{Color, Mode, Move, MoveList, PieceKind, Square};
use crate::position::Position;

impl Position {
    #[must_use]
    pub fn legal_moves(&self) -> MoveList {
        if self.outcome().is_some() {
            return MoveList::new();
        }
        self.legal_moves_for(self.side_to_move())
    }

    /// Generate every legal move for `color`, ignoring whose turn it is.
    ///
    /// Destinations come from a precomputed neighbour table intersected with a
    /// per-kind legality mask, so no per-square coordinate arithmetic or
    /// `piece_at` lookup happens in the hot path.
    #[must_use]
    pub fn legal_moves_for(&self, color: Color) -> MoveList {
        let mut result = MoveList::new();
        self.generate_moves_into(color, &mut |movement| result.push(movement));
        result
    }

    /// Feed every legal move for `color` to `push`, ignoring whose turn it is.
    ///
    /// Search owns its own move storage so that it never pays for zeroing a
    /// 272-entry list at each of millions of nodes, and terminal detection has
    /// already happened by the time search asks for moves. Both needs are why
    /// this hands moves to a sink instead of returning a [`MoveList`].
    pub(crate) fn generate_moves_into(&self, color: Color, push: &mut impl FnMut(Move)) {
        for kind in PieceKind::ALL {
            let mut sources = self.piece_bitboard(color, kind);
            if sources == 0 {
                continue;
            }
            let allowed = self.destinations_mask(color, kind);
            while sources != 0 {
                let source_index = sources.trailing_zeros() as u8;
                let from = Square::from_index_unchecked(source_index);
                let mut targets = NEIGHBORS[usize::from(source_index)] & allowed;
                while targets != 0 {
                    let to = Square::from_index_unchecked(targets.trailing_zeros() as u8);
                    push(Move::new(from, to));
                    targets &= targets - 1;
                }
                sources &= sources - 1;
            }
        }
    }

    /// Feed only the moves [`Position::is_tactical`] accepts to `push`.
    ///
    /// Quiescence used to generate every legal move and then filter, which
    /// meant building two full lists to search a handful of moves. Restricting
    /// the destination mask up front generates exactly the same set directly.
    pub(crate) fn generate_tactical_into(&self, color: Color, push: &mut impl FnMut(Move)) {
        let interesting = self.tactical_destinations(color);
        if interesting == 0 {
            return;
        }
        for kind in PieceKind::ALL {
            let mut sources = self.piece_bitboard(color, kind);
            if sources == 0 {
                continue;
            }
            let allowed = self.destinations_mask(color, kind) & interesting;
            if allowed == 0 {
                continue;
            }
            while sources != 0 {
                let source_index = sources.trailing_zeros() as u8;
                let from = Square::from_index_unchecked(source_index);
                let mut targets = NEIGHBORS[usize::from(source_index)] & allowed;
                while targets != 0 {
                    let to = Square::from_index_unchecked(targets.trailing_zeros() as u8);
                    push(Move::new(from, to));
                    targets &= targets - 1;
                }
                sources &= sources - 1;
            }
        }
    }

    /// Squares whose capture, goal, or last-tile claim makes a move tactical.
    ///
    /// Kept in one place so it cannot drift from [`Position::is_tactical`];
    /// the `move_generation_agrees_with_single_move_legality` invariant test
    /// compares the two.
    #[must_use]
    pub(crate) fn tactical_destinations(&self, color: Color) -> u128 {
        // Any occupied destination a legal move can reach is enemy prey, so
        // enemy occupancy is exactly the capture set.
        let mut mask = self.occupied_by(color.other());
        if self.mode() == Mode::Infiltration {
            mask |= match color {
                Color::Red => crate::board::RED_TARGET_MASK,
                Color::Blue => crate::board::BLUE_TARGET_MASK,
            };
        }
        if self.mode() == Mode::TotalWar && self.neutral_territory_count() == 1 {
            mask |= self.neutral_territory();
        }
        mask
    }

    /// Whether `color` has at least one legal move.
    ///
    /// Cheaper than generating the full list, which matters because stalemate
    /// is now an official terminal result and is therefore tested often.
    #[must_use]
    pub fn has_any_legal_move(&self, color: Color) -> bool {
        for kind in PieceKind::ALL {
            let allowed = self.destinations_mask(color, kind);
            let mut sources = self.piece_bitboard(color, kind);
            while sources != 0 {
                if NEIGHBORS[sources.trailing_zeros() as usize] & allowed != 0 {
                    return true;
                }
                sources &= sources - 1;
            }
        }
        false
    }

    #[must_use]
    pub fn is_legal_move(&self, movement: Move) -> bool {
        if movement.is_none() || self.outcome().is_some() {
            return false;
        }
        let Some(moved) = self.piece_at(movement.from()) else {
            return false;
        };
        if moved.color != self.side_to_move() {
            return false;
        }
        if NEIGHBORS[usize::from(movement.from().index())] & movement.to().bit() == 0 {
            return false;
        }
        self.destinations_mask(moved.color, moved.kind) & movement.to().bit() != 0
    }

    #[must_use]
    pub fn is_capture(&self, movement: Move) -> bool {
        self.is_occupied(movement.to())
    }

    /// Whether a move takes the enemy's last piece of its kind.
    ///
    /// Such a capture permanently makes every friendly piece that kind preyed
    /// on uncapturable, so it is worth searching before ordinary captures.
    #[must_use]
    pub fn is_extinction_capture(&self, movement: Move) -> bool {
        self.piece_at(movement.to()).is_some_and(|defender| {
            self.piece_bitboard(defender.color, defender.kind)
                .count_ones()
                == 1
        })
    }

    /// Whether a move makes friendly pieces permanently uncapturable.
    ///
    /// Removing the enemy's last piece of kind `K` immortalizes every friendly
    /// piece of kind `K.prey()`, but only matters when such pieces exist. This
    /// is used for move ordering only: getting it wrong costs nodes, never
    /// correctness.
    #[must_use]
    pub fn grants_immortality(&self, movement: Move) -> bool {
        self.piece_at(movement.to()).is_some_and(|defender| {
            self.piece_bitboard(defender.color, defender.kind)
                .count_ones()
                == 1
                && self.piece_bitboard(defender.color.other(), defender.kind.prey()) != 0
        })
    }

    #[must_use]
    pub fn is_territory_claim(&self, movement: Move) -> bool {
        self.mode() == Mode::TotalWar && self.territory_owner(movement.to()).is_none()
    }

    #[must_use]
    pub fn is_infiltration_goal(&self, movement: Move) -> bool {
        if self.mode() != Mode::Infiltration {
            return false;
        }
        match self.side_to_move() {
            Color::Red => movement.to().y() == 0,
            Color::Blue => movement.to().y() == BOARD_SIZE - 1,
        }
    }

    #[must_use]
    pub fn is_tactical(&self, movement: Move) -> bool {
        self.is_capture(movement)
            || self.is_infiltration_goal(movement)
            || (self.is_territory_claim(movement) && self.neutral_territory_count() == 1)
    }
}

/// Count leaf nodes at an exact depth using legal mode moves.
///
/// Terminal nodes before the requested depth contribute zero, matching common
/// move-generator perft semantics.
#[must_use]
pub fn perft(position: &mut Position, depth: u8) -> u64 {
    if depth == 0 {
        return 1;
    }
    let moves = position.legal_moves();
    let mut nodes = 0_u64;
    for &movement in &moves {
        let undo = position.make_move_unchecked(movement);
        nodes = nodes.saturating_add(perft(position, depth - 1));
        position.unmake_move(undo);
    }
    nodes
}

#[cfg(test)]
mod tests {
    use crate::model::{Color, Mode, Move, PieceKind, Square};
    use crate::position::Position;

    use super::perft;

    #[test]
    fn starting_move_counts_are_stable() {
        assert_eq!(
            Position::starting(Mode::Annihilation).legal_moves().len(),
            20
        );
        assert_eq!(Position::starting(Mode::TotalWar).legal_moves().len(), 23);
        assert_eq!(
            Position::starting(Mode::Infiltration).legal_moves().len(),
            23
        );
    }

    #[test]
    fn make_unmake_restores_starting_positions_and_hashes() {
        for mode in Mode::ALL {
            let mut position = Position::starting(mode);
            let original = position;
            for &movement in &position.legal_moves() {
                let undo = position
                    .make_move(movement)
                    .expect("generated move must be legal");
                assert_eq!(position.hash(), position.recompute_hash());
                position.unmake_move(undo);
                assert_eq!(position, original);
            }
        }
    }

    #[test]
    fn only_winning_piece_type_can_capture() {
        let rows = [
            ".........",
            ".........",
            ".........",
            ".........",
            "....S....",
            "...r.....",
            ".........",
            ".........",
            ".........",
        ];
        let position = Position::from_rows(Mode::Annihilation, Color::Red, &rows)
            .expect("position must parse");
        let from = Square::from_xy(3, 5).expect("valid square");
        let to = Square::from_xy(4, 4).expect("valid square");
        assert!(position.is_legal_move(Move::new(from, to)));
        assert_eq!(
            position.piece_at(from).expect("piece must exist").kind,
            PieceKind::Rock
        );
    }

    #[test]
    fn shallow_perft_is_deterministic() {
        let mut position = Position::starting(Mode::Annihilation);
        assert_eq!(perft(&mut position, 0), 1);
        assert_eq!(perft(&mut position, 1), 20);
        assert_eq!(position, Position::starting(Mode::Annihilation));
    }

    #[test]
    fn starting_perft_baselines_are_stable() {
        let mut annihilation = Position::starting(Mode::Annihilation);
        let mut total_war = Position::starting(Mode::TotalWar);
        let mut infiltration = Position::starting(Mode::Infiltration);
        assert_eq!(perft(&mut annihilation, 4), 148_225);
        assert_eq!(perft(&mut total_war, 3), 14_789);
        assert_eq!(perft(&mut infiltration, 3), 14_789);
    }
}
