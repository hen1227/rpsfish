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

use std::fmt;

use crate::board::SQUARE_BITS;
use crate::{BOARD_SIZE, SQUARE_COUNT};

/// The maximum number of directed adjacent moves on a 9x9 king-move graph.
///
/// The board has 272 undirected adjacent edges, and a legal move can use an
/// edge in at most one direction for a given position.
pub const MAX_MOVES: usize = 272;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Color {
    Red = 0,
    Blue = 1,
}

impl Color {
    pub const ALL: [Self; 2] = [Self::Red, Self::Blue];

    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Red => Self::Blue,
            Self::Blue => Self::Red,
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

impl fmt::Display for Color {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Red => formatter.write_str("Red"),
            Self::Blue => formatter.write_str("Blue"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum PieceKind {
    Rock = 0,
    Paper = 1,
    Scissors = 2,
}

impl PieceKind {
    pub const ALL: [Self; 3] = [Self::Rock, Self::Paper, Self::Scissors];

    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    #[must_use]
    pub const fn captures(self, defender: Self) -> bool {
        matches!(
            (self, defender),
            (Self::Rock, Self::Scissors)
                | (Self::Scissors, Self::Paper)
                | (Self::Paper, Self::Rock)
        )
    }

    /// The single kind that captures `self`.
    ///
    /// The hierarchy is one 3-cycle, so every kind has exactly one predator.
    /// No mode ever adds a piece, so once a side's last piece of a kind dies,
    /// the enemy pieces it preyed on can never be captured again.
    #[must_use]
    pub const fn predator(self) -> Self {
        match self {
            Self::Rock => Self::Paper,
            Self::Paper => Self::Scissors,
            Self::Scissors => Self::Rock,
        }
    }

    /// The single kind that `self` captures.
    #[must_use]
    pub const fn prey(self) -> Self {
        match self {
            Self::Rock => Self::Scissors,
            Self::Paper => Self::Rock,
            Self::Scissors => Self::Paper,
        }
    }
}

impl fmt::Display for PieceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rock => formatter.write_str("Rock"),
            Self::Paper => formatter.write_str("Paper"),
            Self::Scissors => formatter.write_str("Scissors"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Piece {
    pub color: Color,
    pub kind: PieceKind,
}

/// The rule facts that search and evaluation are allowed to reason about.
///
/// Everything a generic component needs to know about a mode lives here, so
/// adding a rule set means adding one `ModeRules` value rather than editing
/// search, evaluation, or move ordering. Nothing outside this table may
/// branch on a specific `Mode` to decide a *rule*; mode-specific *weights*
/// remain in `EvalParams`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModeRules {
    /// Losing every piece immediately loses the game.
    pub annihilation_loses: bool,
    /// Some non-annihilation condition can also lose the game for a side.
    ///
    /// This is the single fact that licenses the uncapturable-piece bound in
    /// search: when it is `false`, a side that cannot be reduced to zero
    /// pieces cannot lose at all.
    pub has_other_loss_condition: bool,
    /// Territory ownership is tracked and can decide the game.
    pub uses_territory: bool,
    /// Reaching the far home row wins immediately.
    pub uses_boundary_goal: bool,
}

impl ModeRules {
    /// Whether a side holding an uncapturable piece is guaranteed at least a
    /// draw for the rest of the game.
    ///
    /// True only when annihilation is the sole way to lose and stalemate is a
    /// draw, both of which are properties of the rule set rather than of any
    /// position.
    #[must_use]
    pub const fn immortality_prevents_loss(self) -> bool {
        self.annihilation_loses && !self.has_other_loss_condition
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Mode {
    Annihilation = 0,
    TotalWar = 1,
    Infiltration = 2,
}

impl Mode {
    pub const ALL: [Self; 3] = [Self::Annihilation, Self::TotalWar, Self::Infiltration];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Annihilation => "V1",
            Self::TotalWar => "V5",
            Self::Infiltration => "V3",
        }
    }

    /// The rule facts for this mode.
    ///
    /// Annihilation is the only current mode whose sole loss condition is
    /// losing every piece. Total War can still be lost on territory, and
    /// Infiltration can still be lost at the boundary, so neither grants the
    /// uncapturable-piece guarantee.
    #[must_use]
    pub const fn rules(self) -> ModeRules {
        match self {
            Self::Annihilation => ModeRules {
                annihilation_loses: true,
                has_other_loss_condition: false,
                uses_territory: false,
                uses_boundary_goal: false,
            },
            Self::TotalWar => ModeRules {
                annihilation_loses: true,
                has_other_loss_condition: true,
                uses_territory: true,
                uses_boundary_goal: false,
            },
            // Infiltration has no annihilation rule in the Go backend: a side
            // reduced to zero pieces simply has no legal move, which is a
            // stalemate draw.
            Self::Infiltration => ModeRules {
                annihilation_loses: false,
                has_other_loss_condition: true,
                uses_territory: false,
                uses_boundary_goal: true,
            },
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Annihilation => "Annihilation",
            Self::TotalWar => "Total War",
            Self::Infiltration => "Infiltration",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "v1" | "annihilation" => Some(Self::Annihilation),
            "v5" | "total-war" | "total_war" | "totalwar" => Some(Self::TotalWar),
            "v3" | "infiltration" => Some(Self::Infiltration),
            _ => None,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Square(u8);

impl Square {
    #[must_use]
    pub const fn new(index: u8) -> Option<Self> {
        if index < SQUARE_COUNT {
            Some(Self(index))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn from_xy(x: u8, y: u8) -> Option<Self> {
        if x < BOARD_SIZE && y < BOARD_SIZE {
            Some(Self(y * BOARD_SIZE + x))
        } else {
            None
        }
    }

    /// Construct a square known to be in range.
    #[must_use]
    pub(crate) const fn from_index_unchecked(index: u8) -> Self {
        Self(index)
    }

    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn x(self) -> u8 {
        self.0 % BOARD_SIZE
    }

    #[must_use]
    pub const fn y(self) -> u8 {
        self.0 / BOARD_SIZE
    }

    #[must_use]
    pub const fn bit(self) -> u128 {
        SQUARE_BITS[self.0 as usize]
    }
}

impl fmt::Display for Square {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "({}, {})", self.x(), self.y())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Move {
    from: Square,
    to: Square,
}

impl Move {
    pub const NONE: Self = Self {
        from: Square::from_index_unchecked(0),
        to: Square::from_index_unchecked(0),
    };

    #[must_use]
    pub const fn new(from: Square, to: Square) -> Self {
        Self { from, to }
    }

    #[must_use]
    pub const fn from(self) -> Square {
        self.from
    }

    #[must_use]
    pub const fn to(self) -> Square {
        self.to
    }

    #[must_use]
    pub const fn is_none(self) -> bool {
        self.from.index() == self.to.index()
    }
}

impl fmt::Display for Move {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_none() {
            formatter.write_str("(none)")
        } else {
            write!(formatter, "{} -> {}", self.from, self.to)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MoveList {
    moves: [Move; MAX_MOVES],
    len: usize,
}

impl Default for MoveList {
    fn default() -> Self {
        Self {
            moves: [Move::NONE; MAX_MOVES],
            len: 0,
        }
    }
}

impl MoveList {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, candidate: Move) {
        assert!(self.len < MAX_MOVES, "move list capacity exceeded");
        self.moves[self.len] = candidate;
        self.len += 1;
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub fn contains(&self, candidate: Move) -> bool {
        self.as_slice().contains(&candidate)
    }

    #[must_use]
    pub fn as_slice(&self) -> &[Move] {
        &self.moves[..self.len]
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Move> {
        self.as_slice().iter()
    }
}

impl<'a> IntoIterator for &'a MoveList {
    type Item = &'a Move;
    type IntoIter = std::slice::Iter<'a, Move>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndReason {
    Annihilation,
    Territory,
    Infiltration,
    /// The side to move has no legal move. This is an official draw.
    Stalemate,
    /// The third occurrence of a position on the game path.
    Repetition,
}

impl EndReason {
    /// The wire identifier used by the Go backend's `endReason` field.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Annihilation => "annihilation",
            Self::Territory => "territory",
            Self::Infiltration => "infiltration",
            Self::Stalemate => "stalemate",
            Self::Repetition => "repetition",
        }
    }
}

impl fmt::Display for EndReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GameOutcome {
    pub winner: Option<Color>,
    pub reason: EndReason,
}

impl GameOutcome {
    #[must_use]
    pub const fn win(winner: Color, reason: EndReason) -> Self {
        Self {
            winner: Some(winner),
            reason,
        }
    }

    #[must_use]
    pub const fn draw(reason: EndReason) -> Self {
        Self {
            winner: None,
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_coordinates_round_trip() {
        for index in 0..SQUARE_COUNT {
            let square = Square::new(index).expect("square must be valid");
            assert_eq!(Square::from_xy(square.x(), square.y()), Some(square));
        }
        assert_eq!(Square::new(SQUARE_COUNT), None);
        assert_eq!(Square::from_xy(9, 0), None);
    }

    #[test]
    fn capture_cycle_is_directional() {
        assert!(PieceKind::Rock.captures(PieceKind::Scissors));
        assert!(PieceKind::Scissors.captures(PieceKind::Paper));
        assert!(PieceKind::Paper.captures(PieceKind::Rock));
        assert!(!PieceKind::Scissors.captures(PieceKind::Rock));
        assert!(!PieceKind::Rock.captures(PieceKind::Rock));
    }

    #[test]
    fn predator_and_prey_invert_the_capture_cycle() {
        for kind in PieceKind::ALL {
            assert!(kind.predator().captures(kind));
            assert!(kind.captures(kind.prey()));
            assert_eq!(kind.predator().prey(), kind);
            assert_eq!(kind.prey().predator(), kind);
            assert_eq!(
                PieceKind::ALL
                    .into_iter()
                    .filter(|&candidate| candidate.captures(kind))
                    .count(),
                1
            );
        }
    }
}
