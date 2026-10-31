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

/// How many plies may pass with nothing captured before the game is a draw.
///
/// `game.QuietPlyLimit` in the Go backend, in the same unit -- one per side per
/// turn, so two hundred is a hundred moves each. Not part of [`ModeRules`] because no mode
/// gets an opinion about it: it is the rule that makes every game finite, and
/// the search needs that as much as the server does. A repetition draw only
/// catches a position that comes back, and an army with room to wander need
/// never repeat one.
pub const QUIET_PLY_LIMIT: u16 = 200;

/// File letters `a` to `i`, left to right.
///
/// The archive names squares `a1` to `i9`, and so does the review screen. A
/// second convention would make every stored opening line ambiguous about
/// which square it meant, so the engine reads and writes only this one. See
/// `backend/internal/notation`.
const FILE_LETTERS: [u8; BOARD_SIZE as usize] = *b"abcdefghi";

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
    /// What a side wins outright by standing on, if anything.
    pub goal: Goal,
    /// Holding fewer tiles once the board fills is the *only* other loss, and
    /// a claimed tile is never claimed again.
    ///
    /// Two claims in one, because a bound needs both.
    /// `has_other_loss_condition` says only that *some* other loss exists, and
    /// a proof has to close every one of them; this says which one it is, so a
    /// mode that later adds a third way to lose cannot silently inherit the
    /// bound. The second half is what turns a tile lead into a floor rather
    /// than an estimate: `Position::make_move_unchecked` only ever sets a
    /// territory bit, and only for a tile neither side already owns.
    pub territory_is_the_only_other_loss: bool,
    /// The third occurrence of a position is a draw.
    ///
    /// False in every mode, because the server plays it that way: see
    /// `game.RepetitionDrawEnabled` in the Go backend, which these three
    /// answers track. It was false only in Intransitive for a while; the
    /// argument has two real sides, and a repeated position is a claim to half
    /// a point in a game decided by what is left on the board and a defensive
    /// resource in a race for one tile.
    ///
    /// When it is false the search must not fold a repeat into a draw score,
    /// and a self-play walk cannot rely on repetition to end a game. Neither is
    /// the problem it once was: [`QUIET_PLY_LIMIT`] bounds every game in every
    /// mode, and unlike repetition it cannot be shuffled around by an army with
    /// room to wander.
    ///
    /// Still a field rather than a constant, because it is what a mode says
    /// about itself, and the day the server turns the rule back on it comes
    /// back one mode at a time.
    pub repetition_draws: bool,
    /// Having no legal move loses rather than draws.
    ///
    /// The other half of the same idea: a race is not something a side may sit
    /// out, so blockading yourself loses it. This is a loss condition, so it
    /// also closes the uncapturable-piece bound -- an immortal piece is no use
    /// to a side that cannot move it.
    pub stalemate_loses: bool,
}

/// The set of squares a side wins outright by occupying, as a shape.
///
/// A rule fact rather than a mask, because the shape is what the rest of the
/// engine has to reason about: a whole rank leaves every file equivalent, and
/// a single corner does not. [`ModeRules::symmetries`] reads this, and so does
/// every measure of how far a piece has left to run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Goal {
    /// Nothing is won by arriving. The game is decided by what is left.
    None,
    /// The enemy's whole home rank -- Red's is `y == 0`, Blue's the last.
    HomeRank,
    /// The single corner the enemy's army opened in: one end of the rank
    /// [`Goal::HomeRank`] would have given the same side.
    EnemyCorner,
}

impl Goal {
    /// The squares `color` wins by standing on.
    #[must_use]
    pub const fn mask(self, color: Color) -> u128 {
        match (self, color) {
            (Self::None, _) => 0,
            (Self::HomeRank, Color::Red) => crate::board::RED_TARGET_MASK,
            (Self::HomeRank, Color::Blue) => crate::board::BLUE_TARGET_MASK,
            (Self::EnemyCorner, Color::Red) => crate::board::RED_CORNER_MASK,
            (Self::EnemyCorner, Color::Blue) => crate::board::BLUE_CORNER_MASK,
        }
    }

    /// Whether anything is won by arriving somewhere.
    #[must_use]
    pub const fn is_some(self) -> bool {
        !matches!(self, Self::None)
    }

    /// The goal squares for `color` plus everything that reaches them, or zero
    /// when the goal is too wide for the answer to mean anything.
    ///
    /// Only a point goal gets one. A rank's approach is two whole ranks, which
    /// no side can occupy and no evaluation should try to count; a corner's is
    /// four squares, which one piece can sit in and three can fill. That gap
    /// is why Intransitive has a blockade and Infiltration does not.
    #[must_use]
    pub const fn shell(self, color: Color) -> u128 {
        match (self, color) {
            (Self::None | Self::HomeRank, _) => 0,
            (Self::EnemyCorner, Color::Red) => crate::board::RED_CORNER_SHELL,
            (Self::EnemyCorner, Color::Blue) => crate::board::BLUE_CORNER_SHELL,
        }
    }

    /// Moves `color` still needs to walk a piece at `index` onto its goal.
    ///
    /// Chebyshev, because a move is a king step, so this is the count of moves
    /// on an empty board rather than a proxy for it. For a rank the nearer end
    /// of the walk is the only thing that matters and the files drop out; for
    /// a corner both coordinates count, which is the whole difference between
    /// running at a wall and running at a point.
    #[must_use]
    pub const fn distance(self, color: Color, index: usize) -> i32 {
        let x = (index % BOARD_SIZE as usize) as i32;
        let y = (index / BOARD_SIZE as usize) as i32;
        let last = BOARD_SIZE as i32 - 1;
        let rank = match color {
            Color::Red => y,
            Color::Blue => last - y,
        };
        match self {
            Self::None => 0,
            Self::HomeRank => rank,
            // Red's corner is a1 and Blue's is the far corner of Blue's own
            // goal rank, matching `goalCorner` in the Go rules.
            Self::EnemyCorner => {
                let file = match color {
                    Color::Red => x,
                    Color::Blue => last - x,
                };
                if file > rank { file } else { rank }
            }
        }
    }

    /// The longest run this goal can demand, and so the span of
    /// [`Goal::distance`].
    ///
    /// The same number for both shapes on a square board -- a corner is the
    /// far end of a rank, not further than it -- which is what lets one
    /// advancement weight mean the same thing in either mode.
    pub const SPAN: i32 = BOARD_SIZE as i32 - 1;
}

impl ModeRules {
    /// The transforms that are exact symmetries of this rule set.
    ///
    /// [`Transform`] is the whole group the board's geometry admits; this is
    /// the subgroup the *rules* admit, and the two differ as soon as a goal
    /// names a file. Reversing files is a symmetry of a rank goal because a
    /// rank spans every file, and is not one of a corner goal, which would
    /// move to the other end of the board. The half turn survives both: it is
    /// the file reversal and the colour swap done together, so the file it
    /// moves the corner to is the one the colour swap wanted anyway.
    ///
    /// Everything that folds equivalent positions together reads this rather
    /// than [`Transform::ALL`] -- the opening book most of all, since a book
    /// that merged an Intransitive line with its mirror would answer with a
    /// move running at the wrong corner.
    #[must_use]
    pub const fn symmetries(self) -> &'static [Transform] {
        match self.goal {
            Goal::None | Goal::HomeRank => &Transform::ALL,
            Goal::EnemyCorner => &[Transform::IDENTITY, Transform::HALF_TURN],
        }
    }

    /// Whether a side holding an uncapturable piece is guaranteed at least a
    /// draw for the rest of the game.
    ///
    /// True only when annihilation is the sole way to lose and stalemate is a
    /// draw, both of which are properties of the rule set rather than of any
    /// position.
    #[must_use]
    pub const fn immortality_prevents_loss(self) -> bool {
        self.annihilation_loses && !self.has_other_loss_condition && !self.stalemate_loses
    }

    /// Whether an uncapturable piece *plus* a tile count no future claim can
    /// overturn guarantees a side at least a draw.
    ///
    /// Two loss conditions, so both have to be shut at once: the uncapturable
    /// piece shuts annihilation and the tile count shuts territory. In a mode
    /// where stalemate and repetition are draws, nothing worse than zero is
    /// left; one where being unable to move loses has a third way out that no
    /// count bounds, so it gets no bound here either. Note this proves "cannot
    /// lose" and never "wins" -- a majority can go uncashed, because a
    /// blockade or a stalemate ends the game at a draw with neutral tiles
    /// still on the board.
    #[must_use]
    pub const fn territory_majority_prevents_loss(self) -> bool {
        self.annihilation_loses && self.territory_is_the_only_other_loss && !self.stalemate_loses
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Mode {
    // Discriminants are the wasm wire codes and outlived the retired
    // Annihilation mode that held 0; the worker's `MODE_CODES` hand-copies
    // them, so they stay put rather than being repacked.
    TotalWar = 1,
    Infiltration = 2,
    Intransitive = 3,
}

impl Mode {
    pub const ALL: [Self; 3] = [Self::TotalWar, Self::Infiltration, Self::Intransitive];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::TotalWar => "V5",
            Self::Infiltration => "V3",
            Self::Intransitive => "V6",
        }
    }

    /// The rule facts for this mode.
    ///
    /// Total War can be lost on territory as well as by annihilation -- but
    /// that is its *only* other loss and territory never moves backwards, so
    /// an uncapturable piece plus an unbeatable tile count is a guarantee
    /// there. Infiltration can be lost at the boundary, which no count bounds,
    /// so it gets neither. Intransitive is Infiltration's rule set with the
    /// goal shrunk to a point, so it answers identically here and differs only
    /// in [`ModeRules::goal`] -- and, through it, in
    /// [`ModeRules::symmetries`].
    #[must_use]
    pub const fn rules(self) -> ModeRules {
        match self {
            Self::TotalWar => ModeRules {
                annihilation_loses: true,
                has_other_loss_condition: true,
                uses_territory: true,
                goal: Goal::None,
                territory_is_the_only_other_loss: true,
                repetition_draws: false,
                stalemate_loses: false,
            },
            // Infiltration has no annihilation rule in the Go backend: a side
            // reduced to zero pieces simply has no legal move, which is a
            // stalemate draw.
            Self::Infiltration => ModeRules {
                annihilation_loses: false,
                has_other_loss_condition: true,
                uses_territory: false,
                goal: Goal::HomeRank,
                territory_is_the_only_other_loss: false,
                repetition_draws: false,
                stalemate_loses: false,
            },
            // The same annihilation and territory answers as Infiltration, for
            // the same reason: the Go mode delegates to the same standard rules
            // and only replaces the win check. Stalemate is the one place it
            // parts company -- a side with no legal move has lost the race
            // rather than survived it, where in the other two modes it is
            // nobody's fault. Note that makes zero pieces a *loss* here and a
            // draw in Infiltration, from the same `annihilation_loses: false`:
            // the side with nothing left simply cannot move.
            Self::Intransitive => ModeRules {
                annihilation_loses: false,
                has_other_loss_condition: true,
                uses_territory: false,
                goal: Goal::EnemyCorner,
                territory_is_the_only_other_loss: false,
                repetition_draws: false,
                stalemate_loses: true,
            },
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::TotalWar => "Total War",
            Self::Infiltration => "Infiltration",
            Self::Intransitive => "Intransitive",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "v5" | "total-war" | "total_war" | "totalwar" => Some(Self::TotalWar),
            "v3" | "infiltration" => Some(Self::Infiltration),
            "v6" | "intransitive" => Some(Self::Intransitive),
            _ => None,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A symmetry of the board's geometry, used to fold equivalent pictures onto
/// one representative.
///
/// There are two generators:
///
/// - Reversing files (`x -> BOARD_SIZE - 1 - x`). Neighbourhoods are
///   eight-connected and therefore symmetric in `x`.
/// - Swapping colours and reversing ranks, which is [`crate::Position::mirrored`].
///
/// The two act on independent coordinates, so they commute and each is its own
/// inverse: the four combinations form a group under XOR of their codes, and
/// applying a transform twice is the identity. That is what lets a stored
/// analysis be translated into any equivalent position's frame with the same
/// operation that produced it.
///
/// No member changes a side-to-move-relative score, because each maps the
/// mover onto the mover. Nothing here needs a sign flip.
///
/// **Which members are symmetries of the *rules* is a per-mode question**, and
/// [`ModeRules::symmetries`] is the answer -- not this type. Every member is
/// exact where the goal spans whole ranks, but a mode won on a single corner
/// keeps only the identity and the half turn, because reversing files alone
/// moves that corner. Fold with [`ModeRules::symmetries`]; use
/// [`Transform::ALL`] only to talk about the geometry itself.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Transform(u8);

impl Transform {
    /// Leaves the position alone.
    pub const IDENTITY: Self = Self(0);
    /// Reverses files.
    pub const FILES: Self = Self(1);
    /// Swaps colours and reverses ranks.
    pub const COLORS: Self = Self(2);
    /// Rotates the board a half turn and swaps colours.
    ///
    /// Both generators at once, and the only non-identity symmetry a
    /// corner-goal mode has: it carries each side's corner onto the other's,
    /// which is what the file reversal on its own gets wrong.
    pub const HALF_TURN: Self = Self(3);
    /// The whole group, in code order.
    pub const ALL: [Self; 4] = [Self(0), Self(1), Self(2), Self(3)];

    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        if code < 4 { Some(Self(code)) } else { None }
    }

    #[must_use]
    pub const fn code(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn reverses_files(self) -> bool {
        self.0 & 1 != 0
    }

    #[must_use]
    pub const fn swaps_colors(self) -> bool {
        self.0 & 2 != 0
    }

    /// Apply `self` and then `other`.
    ///
    /// The group is abelian and every element is its own inverse, so this is
    /// XOR and `a.then(a)` is the identity.
    #[must_use]
    pub const fn then(self, other: Self) -> Self {
        Self(self.0 ^ other.0)
    }

    /// Map a square through this transform.
    #[must_use]
    pub const fn square(self, square: Square) -> Square {
        let x = if self.reverses_files() {
            BOARD_SIZE - 1 - square.x()
        } else {
            square.x()
        };
        let y = if self.swaps_colors() {
            BOARD_SIZE - 1 - square.y()
        } else {
            square.y()
        };
        Square::from_index_unchecked(y * BOARD_SIZE + x)
    }

    /// Map a move through this transform.
    ///
    /// A colour swap does not rename the mover here: a move is a pair of
    /// squares, and which side owns the piece comes from the position it is
    /// played on.
    #[must_use]
    pub const fn movement(self, movement: Move) -> Move {
        if movement.is_none() {
            return Move::NONE;
        }
        Move::new(self.square(movement.from()), self.square(movement.to()))
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

    /// Read a square written `a1` to `i9`.
    ///
    /// Files `a` to `i` run left to right and ranks `1` to `9` run from Blue's
    /// home boundary (`y == 0`) to Red's, matching the engine's coordinates
    /// exactly.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::parse_bytes(text.as_bytes())
    }

    fn parse_bytes(bytes: &[u8]) -> Option<Self> {
        let [file, rank] = bytes else {
            return None;
        };
        let x = FILE_LETTERS.iter().position(|letter| letter == file)? as u8;
        Self::from_xy(x, rank.checked_sub(b'1')?)
    }

    /// Write this square as `a1` to `i9`.
    #[must_use]
    pub fn notation(self) -> String {
        format!(
            "{}{}",
            FILE_LETTERS[self.x() as usize] as char,
            self.y() + 1
        )
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

    /// Read a move written `d7-d6`, `d7xd6`, or with the leading piece letter
    /// PGN uses (`Rd7-d6`, `Rd7xd6`).
    ///
    /// The piece letter and the capture marker are both accepted and
    /// discarded. Which piece stands on the source square and whether the
    /// destination is occupied are facts about the position, not about the
    /// move, and a move has to parse the same way before a position is known.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut bytes = text.trim().as_bytes();
        if matches!(bytes.first(), Some(b'R' | b'P' | b'S')) {
            bytes = &bytes[1..];
        }
        if bytes.len() != 5 || !matches!(bytes[2], b'-' | b'x') {
            return None;
        }
        let from = Square::parse_bytes(&bytes[0..2])?;
        let to = Square::parse_bytes(&bytes[3..5])?;
        // A move onto its own square is `Move::NONE`, which is a marker rather
        // than a move, so refusing it here keeps the marker unforgeable.
        (from.index() != to.index()).then(|| Self::new(from, to))
    }

    /// Write this move as `d7-d6`.
    ///
    /// The piece letter PGN carries is omitted: a stored line is replayed from
    /// the starting position, so the piece is never in doubt, and leaving it
    /// out keeps the move five bytes wide.
    #[must_use]
    pub fn notation(self) -> String {
        if self.is_none() {
            return "(none)".to_owned();
        }
        format!("{}-{}", self.from.notation(), self.to.notation())
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
    /// A piece reached the enemy's home corner, which wins Intransitive.
    ///
    /// Its own reason rather than [`Self::Infiltration`], because the backend
    /// records the two separately and a game record has to round-trip: a rank
    /// crossed and a corner taken read differently on the result card.
    Corner,
    /// The side to move has no legal move. This is an official draw.
    Stalemate,
    /// The third occurrence of a position on the game path.
    Repetition,
    /// [`QUIET_PLY_LIMIT`] moves have been played with nothing captured.
    NoCapture,
}

impl EndReason {
    pub const ALL: [Self; 7] = [
        Self::Annihilation,
        Self::Territory,
        Self::Infiltration,
        Self::Corner,
        Self::Stalemate,
        Self::Repetition,
        Self::NoCapture,
    ];

    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The wire identifier used by the Go backend's `endReason` field.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Annihilation => "annihilation",
            Self::Territory => "territory",
            Self::Infiltration => "infiltration",
            Self::Corner => "corner",
            Self::Stalemate => "stalemate",
            Self::Repetition => "repetition",
            Self::NoCapture => "no_capture",
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
