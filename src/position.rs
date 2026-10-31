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

use std::error::Error;
use std::fmt;

use crate::model::{
    Color, EndReason, GameOutcome, Mode, Move, Piece, PieceKind, QUIET_PLY_LIMIT, Square, Transform,
};
use crate::{BOARD_MASK, BOARD_SIZE, FIRST_TO_MOVE, SQUARE_COUNT};

const LARGE_ARMY_ROWS: [&str; 9] = [
    "...SSS...",
    "...PPP...",
    "...RRR...",
    ".........",
    ".........",
    ".........",
    "...rrr...",
    "...ppp...",
    "...sss...",
];

/// Intransitive's opening, copied from `intransitiveStartingPosition` in the
/// Go rules and held to it by `starting_positions_match_current_go_layouts`.
///
/// Two wedges facing across the long diagonal rather than two blocks facing
/// across the middle, because the race runs corner to corner. Each side is
/// banded by kind along the diagonal it advances on: Scissors in front, then
/// Paper, then Rock at the back guarding the corner the enemy is running at.
/// Blue's wedge is banked in front of a1 and Red's in front of i9, so Blue's
/// rocks sit on the diagonal that cuts off the corner Red is running at.
///
/// That banking is the mode's whole opening problem in one picture. The two
/// front lines are both Scissors and cannot capture each other, so nothing is
/// hanging on move one; behind that, each side's Rock guards its own corner
/// and is beaten only by the enemy's Paper, and each side's Scissors is what
/// answers that Paper. Every piece is the answer to the piece behind the piece
/// it faces, which is the cycle the mode is named for.
const DIAGONAL_WEDGE_ROWS: [&str; 9] = [
    ".........",
    "...RP....",
    "..RPS....",
    ".RPS.....",
    ".PS...sp.",
    ".....spr.",
    "....spr..",
    "....pr...",
    ".........",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Position {
    mode: Mode,
    side_to_move: Color,
    pieces: [[u128; 3]; 2],
    /// Union of `pieces[color]`, maintained incrementally.
    ///
    /// Move generation, evaluation, and move ordering all need per-colour
    /// occupancy on every node, so folding three bitboards each time showed up
    /// in profiles. `validate` and `PartialEq` both cover the cache, so a
    /// desynchronized value cannot survive a make/unmake round-trip test.
    occupancy: [u128; 2],
    /// Piece code per square, maintained incrementally.
    ///
    /// `piece_at` used to scan up to six bitboards, and `make_move` needs it
    /// twice per node. One byte per square answers the same question with a
    /// single load. `validate` and `PartialEq` both cover the array, so a
    /// desynchronized entry cannot survive a make/unmake round-trip test.
    board: [u8; SQUARE_COUNT as usize],
    /// Playable squares holding no piece, maintained incrementally.
    ///
    /// Every legality mask starts from this, so folding it out of the two
    /// occupancy boards on each of three kinds per node was pure repetition.
    empty: u128,
    territory: [u128; 2],
    /// Piece and territory populations, maintained incrementally.
    ///
    /// Terminal detection and evaluation both ask for these on every node, and
    /// a 128-bit population count is several instructions on a 64-bit target
    /// and more on WebAssembly.
    piece_counts: [u8; 2],
    territory_counts: [u8; 2],
    ply: u16,
    /// Plies played since the last capture, against [`QUIET_PLY_LIMIT`].
    ///
    /// Deliberately absent from `hash`, the way a chess engine leaves the
    /// halfmove clock out of its Zobrist key: two paths that reach the same
    /// board with different counts are the same position to evaluate and to
    /// order moves for, and giving them separate transposition entries would
    /// cost far more than the handful of nodes near the limit it would fix.
    ///
    /// Zero on a position read from a FEN, because a FEN does not carry it.
    /// The RPSI `position` command sends the board a game began from plus every
    /// move since, so a live game replays the count exactly; see
    /// `replay_line` in `main.rs`.
    quiet_plies: u16,
    hash: u64,
}

/// The `board` entry for a square with no piece.
///
/// Piece codes 0..=5 are `colour * 3 + kind`, matching the Zobrist feature
/// numbering, so a code indexes [`FEATURE_KEYS`] directly.
pub(crate) const NO_PIECE: u8 = 6;

/// Number of distinct [`Position::piece_code_at`] results, including
/// [`NO_PIECE`]. Lets move ordering index a per-victim table by piece code
/// with no branch on "was this a capture".
pub(crate) const PIECE_CODES: usize = 7;

const fn piece_code(piece: Piece) -> u8 {
    (piece.color.index() * 3 + piece.kind.index()) as u8
}

const fn piece_from_code(code: u8) -> Option<Piece> {
    let color = match code / 3 {
        0 => Color::Red,
        1 => Color::Blue,
        _ => return None,
    };
    let kind = match code % 3 {
        0 => PieceKind::Rock,
        1 => PieceKind::Paper,
        _ => PieceKind::Scissors,
    };
    Some(Piece { color, kind })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Undo {
    movement: Move,
    moved: Piece,
    captured: Option<Piece>,
    claimed_territory: bool,
    previous_side: Color,
    previous_ply: u16,
    previous_quiet_plies: u16,
    previous_hash: u64,
}

/// Restores the side to move after a null move.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NullUndo {
    previous_side: Color,
    previous_hash: u64,
}

impl Undo {
    /// Whether the move this undoes can never be repeated across.
    ///
    /// A capture removes a piece and a territory claim paints a tile, and no
    /// mode ever reverses either, so the position before such a move can never
    /// occur again. Repetition detection uses this to bound its scan.
    #[must_use]
    pub const fn is_irreversible(&self) -> bool {
        self.captured.is_some() || self.claimed_territory
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PositionError {
    WrongRowCount { actual: usize },
    WrongRowWidth { row: usize, actual: usize },
    InvalidSymbol { symbol: char, x: usize, y: usize },
    MalformedFen { detail: &'static str },
    PieceOverlap,
    TerritoryOverlap,
    BitsOutsideBoard,
    OccupancyDesynchronized,
}

impl fmt::Display for PositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongRowCount { actual } => {
                write!(formatter, "expected 9 board rows, got {actual}")
            }
            Self::WrongRowWidth { row, actual } => {
                write!(formatter, "row {row} must contain 9 symbols, got {actual}")
            }
            Self::InvalidSymbol { symbol, x, y } => {
                write!(formatter, "invalid board symbol {symbol:?} at ({x}, {y})")
            }
            Self::MalformedFen { detail } => write!(formatter, "malformed FEN: {detail}"),
            Self::PieceOverlap => formatter.write_str("piece bitboards overlap"),
            Self::OccupancyDesynchronized => {
                formatter.write_str("cached occupancy does not match the piece bitboards")
            }
            Self::TerritoryOverlap => formatter.write_str("territory bitboards overlap"),
            Self::BitsOutsideBoard => {
                formatter.write_str("bitboard contains squares outside the board")
            }
        }
    }
}

impl Error for PositionError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MoveError {
    IllegalMove(Move),
}

impl fmt::Display for MoveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IllegalMove(movement) => write!(formatter, "illegal move: {movement}"),
        }
    }
}

impl Error for MoveError {}

impl Position {
    #[must_use]
    pub fn starting(mode: Mode) -> Self {
        let rows = match mode {
            Mode::TotalWar | Mode::Infiltration => &LARGE_ARMY_ROWS,
            Mode::Intransitive => &DIAGONAL_WEDGE_ROWS,
        };
        Self::from_rows(mode, FIRST_TO_MOVE, rows)
            .expect("embedded starting position must be valid")
    }

    pub fn from_rows(
        mode: Mode,
        side_to_move: Color,
        rows: &[&str],
    ) -> Result<Self, PositionError> {
        if rows.len() != usize::from(BOARD_SIZE) {
            return Err(PositionError::WrongRowCount { actual: rows.len() });
        }

        let mut pieces = [[0_u128; 3]; 2];
        let mut territory = [0_u128; 2];
        for (y, row) in rows.iter().enumerate() {
            let symbols: Vec<char> = row.chars().collect();
            if symbols.len() != usize::from(BOARD_SIZE) {
                return Err(PositionError::WrongRowWidth {
                    row: y,
                    actual: symbols.len(),
                });
            }
            for (x, symbol) in symbols.into_iter().enumerate() {
                let piece = match symbol {
                    '.' => None,
                    'r' => Some(Piece {
                        color: Color::Red,
                        kind: PieceKind::Rock,
                    }),
                    'p' => Some(Piece {
                        color: Color::Red,
                        kind: PieceKind::Paper,
                    }),
                    's' => Some(Piece {
                        color: Color::Red,
                        kind: PieceKind::Scissors,
                    }),
                    'R' => Some(Piece {
                        color: Color::Blue,
                        kind: PieceKind::Rock,
                    }),
                    'P' => Some(Piece {
                        color: Color::Blue,
                        kind: PieceKind::Paper,
                    }),
                    'S' => Some(Piece {
                        color: Color::Blue,
                        kind: PieceKind::Scissors,
                    }),
                    _ => return Err(PositionError::InvalidSymbol { symbol, x, y }),
                };
                if let Some(piece) = piece {
                    let square = Square::from_xy(x as u8, y as u8)
                        .expect("validated coordinates must form a square");
                    pieces[piece.color.index()][piece.kind.index()] |= square.bit();
                    territory[piece.color.index()] |= square.bit();
                }
            }
        }

        Self::from_bitboards(mode, side_to_move, pieces, territory, 0)
    }

    /// Parse the game-record FEN the server stores alongside every game.
    ///
    /// Three space-separated fields: the board, the side to move, and
    /// territory ownership. Both grids are nine `/`-separated ranks written
    /// from rank 1 (Blue's home boundary, `y == 0`) upward, matching
    /// [`Position::from_rows`], with digits standing for runs of empty or
    /// unclaimed squares. The territory field may be omitted for a mode that
    /// does not track it.
    ///
    /// This exists so a position from a real game can be pasted into a test or
    /// a position suite as the record already wrote it, rather than being
    /// transcribed into nine row strings by hand.
    pub fn from_fen(mode: Mode, fen: &str) -> Result<Self, PositionError> {
        let mut fields = fen.split_whitespace();
        let board = fields
            .next()
            .ok_or(PositionError::MalformedFen { detail: "empty" })?;
        let side = fields.next().ok_or(PositionError::MalformedFen {
            detail: "missing side to move",
        })?;
        let territory_field = fields.next();
        if fields.next().is_some() {
            return Err(PositionError::MalformedFen {
                detail: "trailing field",
            });
        }

        let side_to_move = match side {
            "r" | "R" => Color::Red,
            "b" | "B" => Color::Blue,
            _ => {
                return Err(PositionError::MalformedFen {
                    detail: "side to move must be r or b",
                });
            }
        };

        let board_rows = expand_fen_grid(board)?;
        let rows: Vec<&str> = board_rows.iter().map(String::as_str).collect();
        let mut position = Self::from_rows(mode, side_to_move, &rows)?;

        if let Some(field) = territory_field {
            let mut territory = [0_u128; 2];
            for (y, row) in expand_fen_grid(field)?.iter().enumerate() {
                for (x, symbol) in row.chars().enumerate() {
                    let color = match symbol {
                        '.' => continue,
                        'r' => Color::Red,
                        'b' => Color::Blue,
                        _ => return Err(PositionError::InvalidSymbol { symbol, x, y }),
                    };
                    let square = Square::from_xy(x as u8, y as u8)
                        .expect("validated coordinates must form a square");
                    territory[color.index()] |= square.bit();
                }
            }
            position = Self::from_bitboards(mode, side_to_move, position.pieces, territory, 0)?;
        }
        Ok(position)
    }

    /// Write this position in the same FEN the game record uses.
    ///
    /// Round-trips through [`Position::from_fen`] for every mode; the ply
    /// counter is not part of the format and does not survive.
    #[must_use]
    pub fn to_fen(&self) -> String {
        let board = compress_fen_grid(|square| {
            self.piece_at(square).map(|piece| {
                let symbol = match piece.kind {
                    PieceKind::Rock => 'r',
                    PieceKind::Paper => 'p',
                    PieceKind::Scissors => 's',
                };
                match piece.color {
                    Color::Red => symbol,
                    Color::Blue => symbol.to_ascii_uppercase(),
                }
            })
        });
        let side = match self.side_to_move {
            Color::Red => 'r',
            Color::Blue => 'b',
        };
        let territory = compress_fen_grid(|square| {
            self.territory_owner(square).map(|color| match color {
                Color::Red => 'r',
                Color::Blue => 'b',
            })
        });
        format!("{board} {side} {territory}")
    }

    pub fn from_bitboards(
        mode: Mode,
        side_to_move: Color,
        pieces: [[u128; 3]; 2],
        territory: [u128; 2],
        ply: u16,
    ) -> Result<Self, PositionError> {
        let mut board = [NO_PIECE; SQUARE_COUNT as usize];
        for color in Color::ALL {
            for kind in PieceKind::ALL {
                let mut remaining = pieces[color.index()][kind.index()];
                while remaining != 0 {
                    let index = remaining.trailing_zeros() as usize;
                    if index >= SQUARE_COUNT as usize {
                        return Err(PositionError::BitsOutsideBoard);
                    }
                    board[index] = piece_code(Piece { color, kind });
                    remaining &= remaining - 1;
                }
            }
        }
        let occupancy = [
            fold_kinds(&pieces[Color::Red.index()]),
            fold_kinds(&pieces[Color::Blue.index()]),
        ];
        let mut position = Self {
            mode,
            side_to_move,
            pieces,
            occupancy,
            board,
            empty: BOARD_MASK & !(occupancy[0] | occupancy[1]),
            territory,
            piece_counts: [
                occupancy[0].count_ones() as u8,
                occupancy[1].count_ones() as u8,
            ],
            territory_counts: [
                territory[0].count_ones() as u8,
                territory[1].count_ones() as u8,
            ],
            ply,
            // A board handed over on its own carries no history, so nothing is
            // known to have gone uncaptured. Replaying a line from here is what
            // fills this in; see the field's own note.
            quiet_plies: 0,
            hash: 0,
        };
        position.validate()?;
        position.hash = position.recompute_hash();
        Ok(position)
    }

    pub fn validate(&self) -> Result<(), PositionError> {
        let mut occupied = 0_u128;
        for color in Color::ALL {
            for kind in PieceKind::ALL {
                let board = self.pieces[color.index()][kind.index()];
                if board & !BOARD_MASK != 0 {
                    return Err(PositionError::BitsOutsideBoard);
                }
                if occupied & board != 0 {
                    return Err(PositionError::PieceOverlap);
                }
                occupied |= board;
            }
        }

        if self.territory[0] & !BOARD_MASK != 0 || self.territory[1] & !BOARD_MASK != 0 {
            return Err(PositionError::BitsOutsideBoard);
        }
        if self.territory[0] & self.territory[1] != 0 {
            return Err(PositionError::TerritoryOverlap);
        }
        for color in Color::ALL {
            if self.occupancy[color.index()] != fold_kinds(&self.pieces[color.index()]) {
                return Err(PositionError::OccupancyDesynchronized);
            }
        }
        for index in 0..SQUARE_COUNT {
            let square = Square::from_index_unchecked(index);
            let expected = self.scan_piece_at(square).map_or(NO_PIECE, piece_code);
            if self.board[usize::from(index)] != expected {
                return Err(PositionError::OccupancyDesynchronized);
            }
        }
        if self.empty != BOARD_MASK & !(self.occupancy[0] | self.occupancy[1]) {
            return Err(PositionError::OccupancyDesynchronized);
        }
        for color in Color::ALL {
            let index = color.index();
            if u32::from(self.piece_counts[index]) != self.occupancy[index].count_ones()
                || u32::from(self.territory_counts[index]) != self.territory[index].count_ones()
            {
                return Err(PositionError::OccupancyDesynchronized);
            }
        }
        Ok(())
    }

    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    #[must_use]
    pub const fn side_to_move(&self) -> Color {
        self.side_to_move
    }

    #[must_use]
    pub const fn ply(&self) -> u16 {
        self.ply
    }

    /// Plies played since the last capture.
    #[must_use]
    pub const fn quiet_plies(&self) -> u16 {
        self.quiet_plies
    }

    /// Whether this position has run out the no-capture draw.
    ///
    /// A weaker claim than it looks, and deliberately the last thing any caller
    /// asks: it says only that nothing has been taken for a long time, so a
    /// position that is *also* won, blockaded, or a third repetition is that
    /// instead. [`Position::adjudicate`] orders the two, and so does
    /// `countQuietPlyLocked` in the Go backend.
    #[must_use]
    pub const fn quiet_limit_reached(&self) -> bool {
        self.quiet_plies >= QUIET_PLY_LIMIT
    }

    #[must_use]
    pub const fn hash(&self) -> u64 {
        self.hash
    }

    #[must_use]
    pub const fn piece_bitboard(&self, color: Color, kind: PieceKind) -> u128 {
        self.pieces[color.index()][kind.index()]
    }

    #[must_use]
    pub const fn territory_bitboard(&self, color: Color) -> u128 {
        self.territory[color.index()]
    }

    #[must_use]
    pub const fn occupied_by(&self, color: Color) -> u128 {
        self.occupancy[color.index()]
    }

    #[must_use]
    pub const fn occupied(&self) -> u128 {
        self.occupancy[0] | self.occupancy[1]
    }

    #[must_use]
    pub const fn piece_count(&self, color: Color) -> u32 {
        self.piece_counts[color.index()] as u32
    }

    #[must_use]
    pub const fn territory_count(&self, color: Color) -> u32 {
        self.territory_counts[color.index()] as u32
    }

    /// Playable squares holding no piece.
    #[must_use]
    pub const fn empty_squares(&self) -> u128 {
        self.empty
    }

    #[must_use]
    pub fn neutral_territory(&self) -> u128 {
        BOARD_MASK & !(self.territory[0] | self.territory[1])
    }

    /// How many tiles neither side owns.
    #[must_use]
    pub const fn neutral_territory_count(&self) -> u32 {
        SQUARE_COUNT as u32 - self.territory_counts[0] as u32 - self.territory_counts[1] as u32
    }

    #[must_use]
    pub const fn piece_at(&self, square: Square) -> Option<Piece> {
        piece_from_code(self.board[square.index() as usize])
    }

    /// The piece code at `square`, or [`NO_PIECE`].
    #[must_use]
    pub(crate) const fn piece_code_at(&self, square: Square) -> u8 {
        self.board[square.index() as usize]
    }

    /// Recompute the occupant of one square from the bitboards.
    ///
    /// Only [`Position::validate`] uses this: it is the independent reference
    /// that keeps the incremental `board` array honest.
    fn scan_piece_at(&self, square: Square) -> Option<Piece> {
        let bit = square.bit();
        for color in Color::ALL {
            if self.occupied_by(color) & bit == 0 {
                continue;
            }
            for kind in PieceKind::ALL {
                if self.piece_bitboard(color, kind) & bit != 0 {
                    return Some(Piece { color, kind });
                }
            }
        }
        None
    }

    /// Whether `square` holds a piece of either colour.
    #[must_use]
    pub const fn is_occupied(&self, square: Square) -> bool {
        self.empty & square.bit() == 0
    }

    /// Squares `color` may legally move onto with a piece of `kind`.
    ///
    /// A destination is legal when it is empty or holds the single enemy kind
    /// that `kind` captures.
    #[must_use]
    pub const fn destinations_mask(&self, color: Color, kind: PieceKind) -> u128 {
        self.empty | self.piece_bitboard(color.other(), kind.prey())
    }

    /// Enemy pieces of the one kind that captures `kind`.
    #[must_use]
    pub fn predators_of(&self, color: Color, kind: PieceKind) -> u128 {
        self.piece_bitboard(color.other(), kind.predator())
    }

    /// How many enemy pieces can still capture `color`'s pieces of `kind`.
    #[must_use]
    pub fn predator_count(&self, color: Color, kind: PieceKind) -> u32 {
        self.predators_of(color, kind).count_ones()
    }

    /// Whether `color`'s pieces of `kind` can never be captured again.
    ///
    /// The capture hierarchy is a single 3-cycle, so each kind has exactly one
    /// predator, and no mode ever returns a piece to the board. Once that
    /// predator kind is extinct the property is permanent, which is why search
    /// may treat it as a proof rather than as a heuristic.
    #[must_use]
    pub fn is_immortal_kind(&self, color: Color, kind: PieceKind) -> bool {
        self.predators_of(color, kind) == 0
    }

    /// All of `color`'s pieces that can never be captured again.
    #[must_use]
    pub fn immortal_pieces(&self, color: Color) -> u128 {
        PieceKind::ALL
            .into_iter()
            .filter(|&kind| self.is_immortal_kind(color, kind))
            .fold(0_u128, |result, kind| {
                result | self.piece_bitboard(color, kind)
            })
    }

    /// Whether `color` owns at least one piece the enemy can never capture.
    #[must_use]
    pub fn has_immortal_piece(&self, color: Color) -> bool {
        self.immortal_pieces(color) != 0
    }

    /// Whether the enemy can still reduce `color` to zero pieces.
    #[must_use]
    pub fn can_be_annihilated(&self, color: Color) -> bool {
        !self.has_immortal_piece(color)
    }

    #[must_use]
    pub fn territory_owner(&self, square: Square) -> Option<Color> {
        let bit = square.bit();
        Color::ALL
            .into_iter()
            .find(|&color| self.territory_bitboard(color) & bit != 0)
    }

    /// The board-only terminal result, if the position is already decided.
    ///
    /// This deliberately excludes stalemate and repetition, which depend on
    /// move generation and on the game path. Use [`Position::adjudicate`] for
    /// the complete answer.
    #[must_use]
    pub fn outcome(&self) -> Option<GameOutcome> {
        let red_count = self.piece_count(Color::Red);
        let blue_count = self.piece_count(Color::Blue);

        match self.mode {
            Mode::TotalWar => {
                if let Some(outcome) = zero_piece_outcome(red_count, blue_count) {
                    return Some(outcome);
                }
                if self.neutral_territory_count() == 0 {
                    let red = self.territory_count(Color::Red);
                    let blue = self.territory_count(Color::Blue);
                    return Some(match red.cmp(&blue) {
                        std::cmp::Ordering::Greater => {
                            GameOutcome::win(Color::Red, EndReason::Territory)
                        }
                        std::cmp::Ordering::Less => {
                            GameOutcome::win(Color::Blue, EndReason::Territory)
                        }
                        std::cmp::Ordering::Equal => GameOutcome::draw(EndReason::Territory),
                    });
                }
                None
            }
            // One arm for both goal modes: they differ in the shape of the
            // goal and in the name the win goes under, and in nothing else.
            // The shape comes from the rules table and the name from the mode,
            // so a third goal mode would add neither branch nor line here.
            Mode::Infiltration | Mode::Intransitive => {
                let goal = self.mode.rules().goal;
                let reason = match self.mode {
                    Mode::Intransitive => EndReason::Corner,
                    _ => EndReason::Infiltration,
                };
                // The side that just moved is checked first, so a move onto
                // one's own goal wins even in the position where the other
                // side is also standing on theirs. Such a position is not
                // reachable in a real game -- the first arrival ends it -- but
                // a suite or a book may hand one over, and search must not see
                // the answer flip with the side to move.
                let previous = self.side_to_move.other();
                if self.occupied_by(previous) & goal.mask(previous) != 0 {
                    return Some(GameOutcome::win(previous, reason));
                }
                for color in Color::ALL {
                    if self.occupied_by(color) & goal.mask(color) != 0 {
                        return Some(GameOutcome::win(color, reason));
                    }
                }
                None
            }
        }
    }

    /// Whether the side to move has no legal move in an undecided position.
    ///
    /// A terminal test rather than a search setting: every mode ends the game
    /// here. Which result it is belongs to [`ModeRules::stalemate_loses`], so
    /// this answers only that the game is over -- see [`Position::adjudicate`].
    #[must_use]
    pub fn is_stalemate(&self) -> bool {
        self.outcome().is_none() && !self.has_any_legal_move(self.side_to_move)
    }

    /// The complete terminal result for this position, including stalemate and
    /// the no-capture draw.
    ///
    /// Repetition remains path-dependent and is therefore owned by the caller
    /// that knows the game history.
    ///
    /// The order is the rules': what the mode decides, then whether the side to
    /// move is stuck, then the no-capture draw. Reversing the last two would
    /// turn a blockade on the hundredth quiet move -- a win in a mode where
    /// being stuck loses -- into half a point.
    #[must_use]
    pub fn adjudicate(&self) -> Option<GameOutcome> {
        self.outcome()
            .or_else(|| {
                if self.has_any_legal_move(self.side_to_move) {
                    return None;
                }
                Some(if self.mode.rules().stalemate_loses {
                    GameOutcome::win(self.side_to_move.other(), EndReason::Stalemate)
                } else {
                    GameOutcome::draw(EndReason::Stalemate)
                })
            })
            .or_else(|| {
                self.quiet_limit_reached()
                    .then(|| GameOutcome::draw(EndReason::NoCapture))
            })
    }

    /// The position with colours swapped and the board flipped about the
    /// midline.
    ///
    /// Red advances toward `y == 0` and Blue toward `y == BOARD_SIZE - 1`, so
    /// this maps each side's ranks onto the other's. That is the whole story
    /// where a goal is a rank or absent, and evaluation is invariant under it
    /// there. It is *not* the whole story for a corner goal, which names a
    /// file too: this carries Red's corner onto the wrong end of Blue's rank,
    /// and [`Transform::HALF_TURN`] is the member that gets it right. Ask
    /// [`crate::model::ModeRules::symmetries`] before folding with it.
    #[must_use]
    pub fn mirrored(&self) -> Self {
        let mut pieces = [[0_u128; 3]; 2];
        for color in Color::ALL {
            for kind in PieceKind::ALL {
                pieces[color.other().index()][kind.index()] =
                    flip_vertical(self.piece_bitboard(color, kind));
            }
        }
        let territory = [
            flip_vertical(self.territory_bitboard(Color::Blue)),
            flip_vertical(self.territory_bitboard(Color::Red)),
        ];
        Self::from_bitboards(
            self.mode,
            self.side_to_move.other(),
            pieces,
            territory,
            self.ply,
        )
        .expect("mirroring a valid position must stay valid")
    }

    /// The position with files reversed (`x -> BOARD_SIZE - 1 - x`).
    ///
    /// Colours and the side to move are untouched: reversing files is a
    /// symmetry of the board's *geometry* alone, since neighbourhoods are
    /// eight-connected. Whether it is a symmetry of the rules depends on the
    /// mode. Infiltration's target rows are whole rows and Total War has no
    /// goal at all, so there it maps every legal move onto a legal move of the
    /// same value; Intransitive's corner moves to the far file, so there it
    /// does not. [`crate::model::ModeRules::symmetries`] is the authority.
    #[must_use]
    pub fn flipped_horizontally(&self) -> Self {
        let mut pieces = [[0_u128; 3]; 2];
        for color in Color::ALL {
            for kind in PieceKind::ALL {
                pieces[color.index()][kind.index()] =
                    flip_horizontal(self.piece_bitboard(color, kind));
            }
        }
        let territory = [
            flip_horizontal(self.territory_bitboard(Color::Red)),
            flip_horizontal(self.territory_bitboard(Color::Blue)),
        ];
        Self::from_bitboards(self.mode, self.side_to_move, pieces, territory, self.ply)
            .expect("reversing the files of a valid position must stay valid")
    }

    /// Apply one member of the symmetry group.
    #[must_use]
    pub fn transformed(&self, transform: Transform) -> Self {
        let mut result = *self;
        if transform.reverses_files() {
            result = result.flipped_horizontally();
        }
        if transform.swaps_colors() {
            result = result.mirrored();
        }
        result
    }

    /// The representative of this position's symmetry class, and the transform
    /// that produces it.
    ///
    /// Every position in a class canonicalizes to the same representative, so
    /// an opening book keyed on the representative's hash merges lines that
    /// reach the same board picture by different move orders, from either
    /// colour's point of view. Ties go to the lowest transform code, which is
    /// what makes the answer deterministic for a position that is invariant
    /// under part of the group -- the Infiltration starting position is
    /// invariant under all of it.
    #[must_use]
    pub fn canonical(&self) -> (Self, Transform) {
        self.canonical_over(self.mode.rules().symmetries())
    }

    /// The representative of this position's class under a subset of the
    /// group, and the transform that produces it.
    ///
    /// A book folds by whichever subset actually merges positions in the mode
    /// it covers, so the subset is a stored setting rather than a constant.
    /// `transforms` must contain [`Transform::IDENTITY`] and be closed under
    /// composition, or two members of a class can disagree about their
    /// representative.
    #[must_use]
    pub fn canonical_over(&self, transforms: &[Transform]) -> (Self, Transform) {
        // A caller may ask for any subset of the geometry's group; only the
        // subset this mode's *rules* admit is sound to fold by. Intersecting
        // here rather than at each caller is deliberate -- this is the single
        // point every fold passes through, so a mode whose goal names a file
        // cannot be merged with its mirror by any path, including an opening
        // book that was configured before the mode existed. Asking for more
        // than a mode allows silently gets less, which costs merging and never
        // correctness.
        let allowed = self.mode.rules().symmetries();
        let mut best = *self;
        let mut best_transform = Transform::IDENTITY;
        let mut best_key = best.symmetry_key();
        for &transform in transforms {
            if transform == Transform::IDENTITY || !allowed.contains(&transform) {
                continue;
            }
            let candidate = self.transformed(transform);
            let key = candidate.symmetry_key();
            if key < best_key {
                best = candidate;
                best_transform = transform;
                best_key = key;
            }
        }
        (best, best_transform)
    }

    /// A total order on board pictures, for choosing a class representative.
    ///
    /// Deliberately not the Zobrist hash: two distinct images colliding would
    /// silently pick a representative that depends on which member you started
    /// from, and every stored line under that key would then disagree.
    fn symmetry_key(&self) -> ([u128; 8], usize) {
        (
            [
                self.pieces[0][0],
                self.pieces[0][1],
                self.pieces[0][2],
                self.pieces[1][0],
                self.pieces[1][1],
                self.pieces[1][2],
                self.territory[0],
                self.territory[1],
            ],
            self.side_to_move.index(),
        )
    }

    pub fn make_move(&mut self, movement: Move) -> Result<Undo, MoveError> {
        if !self.is_legal_move(movement) {
            return Err(MoveError::IllegalMove(movement));
        }
        Ok(self.make_move_unchecked(movement))
    }

    pub(crate) fn make_move_unchecked(&mut self, movement: Move) -> Undo {
        let from = movement.from();
        let to = movement.to();
        let from_bit = from.bit();
        let to_bit = to.bit();
        let moved_code = self.board[from.index() as usize];
        let moved = piece_from_code(moved_code).expect("legal move must have a source piece");
        let captured_code = self.board[to.index() as usize];
        let claimed_territory =
            self.mode == Mode::TotalWar && (self.territory[0] | self.territory[1]) & to_bit == 0;
        let undo = Undo {
            movement,
            moved,
            captured: piece_from_code(captured_code),
            claimed_territory,
            previous_side: self.side_to_move,
            previous_ply: self.ply,
            previous_quiet_plies: self.quiet_plies,
            previous_hash: self.hash,
        };

        let moved_color = moved.color.index();
        let moved_kind = moved.kind.index();
        self.pieces[moved_color][moved_kind] ^= from_bit | to_bit;
        self.occupancy[moved_color] ^= from_bit | to_bit;
        self.hash ^= piece_code_key(moved_code, from) ^ piece_code_key(moved_code, to);
        self.board[from.index() as usize] = NO_PIECE;
        self.board[to.index() as usize] = moved_code;
        self.empty |= from_bit;
        self.empty &= !to_bit;
        if let Some(captured) = undo.captured {
            self.pieces[captured.color.index()][captured.kind.index()] ^= to_bit;
            self.occupancy[captured.color.index()] ^= to_bit;
            self.piece_counts[captured.color.index()] -= 1;
            self.hash ^= piece_code_key(captured_code, to);
        }
        if claimed_territory {
            self.territory[moved_color] |= to_bit;
            self.territory_counts[moved_color] += 1;
            self.hash ^= territory_key(moved.color, to);
        }
        self.side_to_move = self.side_to_move.other();
        self.hash ^= side_key();
        self.ply = self.ply.saturating_add(1);
        // A capture and nothing else. Claiming a tile is progress in Total War
        // but not progress this rule counts, because the mode already ends the
        // game on it when the board fills; the Go backend counts the same way.
        self.quiet_plies = if undo.captured.is_some() {
            0
        } else {
            self.quiet_plies.saturating_add(1)
        };
        undo
    }

    pub fn unmake_move(&mut self, undo: Undo) {
        let from = undo.movement.from();
        let to = undo.movement.to();
        let from_bit = from.bit();
        let to_bit = to.bit();
        let moved_color = undo.moved.color.index();
        self.pieces[moved_color][undo.moved.kind.index()] ^= from_bit | to_bit;
        self.occupancy[moved_color] ^= from_bit | to_bit;
        self.board[from.index() as usize] = piece_code(undo.moved);
        self.empty &= !from_bit;
        self.board[to.index() as usize] = match undo.captured {
            Some(captured) => {
                self.pieces[captured.color.index()][captured.kind.index()] |= to_bit;
                self.occupancy[captured.color.index()] |= to_bit;
                self.piece_counts[captured.color.index()] += 1;
                piece_code(captured)
            }
            None => {
                self.empty |= to_bit;
                NO_PIECE
            }
        };
        if undo.claimed_territory {
            self.territory[moved_color] &= !to_bit;
            self.territory_counts[moved_color] -= 1;
        }
        self.side_to_move = undo.previous_side;
        self.ply = undo.previous_ply;
        self.quiet_plies = undo.previous_quiet_plies;
        self.hash = undo.previous_hash;
    }

    /// Pass the turn without moving, for null-move pruning.
    ///
    /// Every mode's board-only outcome is either independent of whose turn it
    /// is or, in Infiltration, decided by whether a side already stands on its
    /// target row. Neither changes when only the side to move flips, so a
    /// position that was undecided before the pass is undecided after it.
    pub(crate) fn make_null_move(&mut self) -> NullUndo {
        let undo = NullUndo {
            previous_side: self.side_to_move,
            previous_hash: self.hash,
        };
        self.side_to_move = self.side_to_move.other();
        self.hash ^= side_key();
        undo
    }

    pub(crate) fn unmake_null_move(&mut self, undo: NullUndo) {
        self.side_to_move = undo.previous_side;
        self.hash = undo.previous_hash;
    }

    #[must_use]
    pub fn recompute_hash(&self) -> u64 {
        let mut result = mode_key(self.mode);
        if self.side_to_move == Color::Blue {
            result ^= side_key();
        }
        for color in Color::ALL {
            for kind in PieceKind::ALL {
                let mut board = self.piece_bitboard(color, kind);
                while board != 0 {
                    let index = board.trailing_zeros() as u8;
                    let square = Square::from_index_unchecked(index);
                    result ^= piece_key(Piece { color, kind }, square);
                    board &= board - 1;
                }
            }
            let mut board = self.territory_bitboard(color);
            while board != 0 {
                let index = board.trailing_zeros() as u8;
                let square = Square::from_index_unchecked(index);
                result ^= territory_key(color, square);
                board &= board - 1;
            }
        }
        result
    }
}

/// Expand one `/`-separated FEN grid into nine nine-character rows.
///
/// Digits are runs of unoccupied or unclaimed squares. Row zero is rank 1, the
/// same order [`Position::from_rows`] uses, so both callers agree on which end
/// of the board a record starts from.
fn expand_fen_grid(field: &str) -> Result<Vec<String>, PositionError> {
    let mut rows = Vec::with_capacity(usize::from(BOARD_SIZE));
    for (index, row) in field.split('/').enumerate() {
        let mut expanded = String::with_capacity(usize::from(BOARD_SIZE));
        for symbol in row.chars() {
            match symbol.to_digit(10) {
                Some(run) if run > 0 => {
                    for _ in 0..run {
                        expanded.push('.');
                    }
                }
                Some(_) => {
                    return Err(PositionError::MalformedFen {
                        detail: "zero-length run",
                    });
                }
                None => expanded.push(symbol),
            }
        }
        if expanded.chars().count() != usize::from(BOARD_SIZE) {
            return Err(PositionError::WrongRowWidth {
                row: index,
                actual: expanded.chars().count(),
            });
        }
        rows.push(expanded);
    }
    if rows.len() != usize::from(BOARD_SIZE) {
        return Err(PositionError::WrongRowCount { actual: rows.len() });
    }
    Ok(rows)
}

/// Render a per-square symbol table back into a `/`-separated FEN grid.
fn compress_fen_grid(symbol_at: impl Fn(Square) -> Option<char>) -> String {
    let mut result = String::new();
    for y in 0..BOARD_SIZE {
        if y > 0 {
            result.push('/');
        }
        let mut run = 0_u32;
        for x in 0..BOARD_SIZE {
            let square = Square::from_xy(x, y).expect("board coordinates must form a square");
            match symbol_at(square) {
                Some(symbol) => {
                    if run > 0 {
                        result.push(char::from_digit(run, 10).expect("run fits one digit"));
                        run = 0;
                    }
                    result.push(symbol);
                }
                None => run += 1,
            }
        }
        if run > 0 {
            result.push(char::from_digit(run, 10).expect("run fits one digit"));
        }
    }
    result
}

fn fold_kinds(boards: &[u128; 3]) -> u128 {
    boards[0] | boards[1] | boards[2]
}

/// Reflect a bitboard about the horizontal midline (`y -> BOARD_SIZE - 1 - y`).
fn flip_vertical(board: u128) -> u128 {
    let mut result = 0_u128;
    for y in 0..BOARD_SIZE {
        let row = (board >> (y * BOARD_SIZE)) & ((1_u128 << BOARD_SIZE) - 1);
        result |= row << ((BOARD_SIZE - 1 - y) * BOARD_SIZE);
    }
    result
}

/// Reflect a bitboard about the vertical midline (`x -> BOARD_SIZE - 1 - x`).
fn flip_horizontal(board: u128) -> u128 {
    let row_mask = (1_u128 << BOARD_SIZE) - 1;
    let mut result = 0_u128;
    for y in 0..BOARD_SIZE {
        let row = (board >> (y * BOARD_SIZE)) & row_mask;
        let mut reversed = 0_u128;
        for x in 0..BOARD_SIZE {
            if row & (1 << x) != 0 {
                reversed |= 1 << (BOARD_SIZE - 1 - x);
            }
        }
        result |= reversed << (y * BOARD_SIZE);
    }
    result
}

fn zero_piece_outcome(red_count: u32, blue_count: u32) -> Option<GameOutcome> {
    match (red_count, blue_count) {
        (0, 0) => None,
        (0, _) => Some(GameOutcome::win(Color::Blue, EndReason::Annihilation)),
        (_, 0) => Some(GameOutcome::win(Color::Red, EndReason::Annihilation)),
        _ => None,
    }
}

/// Number of hashed board features: six piece kind/colour pairs and two
/// territory owners.
const FEATURE_COUNT: usize = 8;

/// Zobrist keys for every (feature, square) pair.
///
/// The values are exactly what the previous `splitmix64`-per-call computation
/// produced, so stored hashes and printed position keys are unchanged. Making
/// the table `const` removes four or five 64-bit hash computations from every
/// `make_move`/`unmake_move` pair, which is pure search throughput.
const FEATURE_KEYS: [[u64; SQUARE_COUNT as usize]; FEATURE_COUNT] = build_feature_keys();

const SIDE_KEY: u64 = splitmix64(0x7f4a_7c15_d1b5_4a32);

/// Hash keys indexed by a mode's *wire code*, not by its position in
/// [`Mode::ALL`].
///
/// So the table needs one entry per code and a dead entry at zero, which the
/// retired Annihilation mode left behind. Entries are only ever appended:
/// changing one would change every stored position key and every opening book
/// built under it. `mode_keys_cover_every_mode` holds the length to the codes.
const MODE_KEYS: [u64; 4] = [
    splitmix64(0x94d0_49bb_1331_11eb),
    splitmix64(0x94d0_49bb_1331_11eb ^ 1),
    splitmix64(0x94d0_49bb_1331_11eb ^ 2),
    splitmix64(0x94d0_49bb_1331_11eb ^ 3),
];

const fn build_feature_keys() -> [[u64; SQUARE_COUNT as usize]; FEATURE_COUNT] {
    let mut table = [[0_u64; SQUARE_COUNT as usize]; FEATURE_COUNT];
    let mut feature = 0_usize;
    while feature < FEATURE_COUNT {
        let mut square = 0_usize;
        while square < SQUARE_COUNT as usize {
            table[feature][square] = feature_key(feature as u64, square as u8);
            square += 1;
        }
        feature += 1;
    }
    table
}

const fn piece_key(piece: Piece, square: Square) -> u64 {
    FEATURE_KEYS[piece.color.index() * 3 + piece.kind.index()][square.index() as usize]
}

const fn piece_code_key(code: u8, square: Square) -> u64 {
    FEATURE_KEYS[code as usize][square.index() as usize]
}

const fn territory_key(color: Color, square: Square) -> u64 {
    FEATURE_KEYS[6 + color.index()][square.index() as usize]
}

const fn side_key() -> u64 {
    SIDE_KEY
}

const fn mode_key(mode: Mode) -> u64 {
    MODE_KEYS[mode as usize]
}

const fn feature_key(feature: u64, square: u8) -> u64 {
    splitmix64(
        0x9e37_79b9_7f4a_7c15
            ^ feature.wrapping_mul(0xbf58_476d_1ce4_e5b9)
            ^ (square as u64).wrapping_mul(0x94d0_49bb_1331_11eb),
    )
}

const fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starting_positions_match_current_go_layouts() {
        for mode in [Mode::TotalWar, Mode::Infiltration] {
            let position = Position::starting(mode);
            assert_eq!(position.piece_count(Color::Red), 9);
            assert_eq!(position.piece_count(Color::Blue), 9);
            assert_eq!(position.territory_count(Color::Red), 9);
            assert_eq!(position.territory_count(Color::Blue), 9);
        }
        for mode in Mode::ALL {
            assert_eq!(
                Position::starting(mode).side_to_move(),
                FIRST_TO_MOVE,
                "{mode} must open with the side the Go rules open with"
            );
        }
    }

    /// Which corner each side wins on, read off the opening rather than
    /// restated, so mirroring the layout without moving the masks -- or the
    /// reverse -- fails here rather than in a game.
    ///
    /// The twin of `TestVersion6GoalCornersAreTheEnemyHomeCorners` in the Go
    /// rules, and the reason the two can be checked separately: each side's
    /// wedge is banked in front of its *own* corner, so the corner a side
    /// defends is the one nearer its own army, and the corner it runs at is the
    /// other one.
    #[test]
    fn the_intransitive_wedges_are_banked_in_front_of_their_own_corners() {
        let start = Position::starting(Mode::Intransitive);
        let goal = Mode::Intransitive.rules().goal;
        assert_eq!(start.piece_count(Color::Red), 10);
        assert_eq!(start.piece_count(Color::Blue), 10);

        for color in Color::ALL {
            // Nobody opens standing on a goal, or the game would be over.
            assert_eq!(start.occupied_by(color) & goal.mask(color), 0);

            let own = goal.mask(color.other());
            let enemy = goal.mask(color);
            let (mut near, mut far) = (0_i32, 0_i32);
            for index in 0..usize::from(SQUARE_COUNT) {
                if start.occupied_by(color) & (1_u128 << index) == 0 {
                    continue;
                }
                near += chebyshev(index, own);
                far += chebyshev(index, enemy);
            }
            assert!(
                near < far,
                "{color}'s army is not banked in front of the corner it defends"
            );
        }
    }

    /// Chebyshev distance from a square index to a single-square mask.
    fn chebyshev(index: usize, mask: u128) -> i32 {
        let target = mask.trailing_zeros() as usize;
        let size = usize::from(BOARD_SIZE);
        let (x, y) = (index % size, index / size);
        let (tx, ty) = (target % size, target / size);
        let dx = (x as i32 - tx as i32).abs();
        let dy = (y as i32 - ty as i32).abs();
        dx.max(dy)
    }

    #[test]
    fn mode_keys_cover_every_mode_and_separate_them() {
        // The bug this exists for: `MODE_KEYS` is indexed by the wire code, so
        // adding a mode with a new code overruns it, and the failure surfaces
        // as a panic in half the suite rather than as anything about modes.
        for mode in Mode::ALL {
            assert!(
                (mode as usize) < MODE_KEYS.len(),
                "{mode} has wire code {} and MODE_KEYS holds {}",
                mode as usize,
                MODE_KEYS.len()
            );
        }
        for (index, mode) in Mode::ALL.into_iter().enumerate() {
            for other in Mode::ALL.into_iter().skip(index + 1) {
                assert_ne!(
                    mode_key(mode),
                    mode_key(other),
                    "{mode} and {other} share a hash key"
                );
            }
        }
    }

    #[test]
    fn fen_round_trips_through_every_starting_position() {
        for mode in Mode::ALL {
            let position = Position::starting(mode);
            let fen = position.to_fen();
            let parsed = Position::from_fen(mode, &fen).expect("own output must parse");
            assert_eq!(parsed.to_fen(), fen, "{mode} FEN must be stable");
            assert_eq!(
                parsed.piece_count(Color::Red),
                position.piece_count(Color::Red)
            );
            assert_eq!(
                parsed.territory_count(Color::Blue),
                position.territory_count(Color::Blue)
            );
            assert_eq!(parsed.side_to_move(), position.side_to_move());
        }
    }

    #[test]
    fn fen_reads_the_layout_the_game_record_writes() {
        // The record the server stores for a fresh Total War game, copied out
        // of backend/docs/pgn.md. Rank 1 is written first and is Blue's home,
        // so Blue's Scissors lead the board field and Blue's territory leads
        // the territory field -- and Blue is the side to move, because Blue
        // opens.
        let fen = "3SSS3/3PPP3/3RRR3/9/9/9/3rrr3/3ppp3/3sss3 b                    3bbb3/3bbb3/3bbb3/9/9/9/3rrr3/3rrr3/3rrr3";
        let parsed = Position::from_fen(Mode::TotalWar, fen).expect("record FEN must parse");
        assert_eq!(parsed, Position::starting(Mode::TotalWar));
    }

    #[test]
    fn fen_keeps_territory_that_no_piece_stands_on() {
        // The distinction `from_rows` cannot express: claimed ground the owner
        // has since walked away from. Every suite position depends on it.
        let fen = "9/9/9/9/4r4/9/9/9/9 b rrrrrrrrr/9/9/9/9/9/9/9/bbbbbbbbb";
        let parsed = Position::from_fen(Mode::TotalWar, fen).expect("must parse");
        assert_eq!(parsed.piece_count(Color::Red), 1);
        assert_eq!(parsed.territory_count(Color::Red), 9);
        assert_eq!(parsed.territory_count(Color::Blue), 9);
        assert_eq!(parsed.neutral_territory_count(), 63);
        assert_eq!(parsed.side_to_move(), Color::Blue);
    }

    #[test]
    fn malformed_fen_is_rejected_rather_than_guessed_at() {
        for bad in [
            "9/9/9/9/9/9/9/9 r",                       // eight ranks
            "9/9/9/9/8/9/9/9/9 r",                     // a short rank
            "9/9/9/9/9/9/9/9/9 x",                     // no such side
            "9/9/9/9/9/9/9/9/9",                       // no side at all
            "9/9/9/9/9/9/9/9/9 r 9/9/9/9/9/9/9/9/9 9", // a field too many
        ] {
            assert!(
                Position::from_fen(Mode::TotalWar, bad).is_err(),
                "{bad:?} must not parse"
            );
        }
    }

    #[test]
    fn null_moves_only_flip_the_side_and_round_trip_exactly() {
        for mode in Mode::ALL {
            let mut position = Position::starting(mode);
            let original = position;
            let undo = position.make_null_move();
            assert_eq!(position.side_to_move(), original.side_to_move().other());
            assert_eq!(position.hash(), position.recompute_hash());
            assert_ne!(position.hash(), original.hash());
            // Every board fact is untouched, which is what lets search treat a
            // pass as safe in Infiltration: nobody gained a target row.
            for color in Color::ALL {
                assert_eq!(position.occupied_by(color), original.occupied_by(color));
                assert_eq!(
                    position.territory_bitboard(color),
                    original.territory_bitboard(color)
                );
            }
            assert_eq!(position.outcome(), original.outcome());
            position.unmake_null_move(undo);
            assert_eq!(position, original);
        }
    }

    #[test]
    fn cached_hash_matches_full_recomputation() {
        for mode in Mode::ALL {
            let position = Position::starting(mode);
            assert_eq!(position.hash(), position.recompute_hash());
        }
    }

    #[test]
    fn rejects_overlapping_piece_boards() {
        let bit = Square::from_xy(4, 4).expect("valid square").bit();
        let mut pieces = [[0_u128; 3]; 2];
        pieces[Color::Red.index()][PieceKind::Rock.index()] = bit;
        pieces[Color::Blue.index()][PieceKind::Paper.index()] = bit;
        assert_eq!(
            Position::from_bitboards(Mode::TotalWar, Color::Red, pieces, [0_u128; 2], 0,),
            Err(PositionError::PieceOverlap)
        );
    }

    #[test]
    fn each_mode_reports_its_board_objective() {
        let last_piece_rows = [
            ".........",
            ".........",
            ".........",
            ".........",
            "....rS...",
            ".........",
            ".........",
            ".........",
            ".........",
        ];
        let mut last_piece = Position::from_rows(Mode::TotalWar, Color::Red, &last_piece_rows)
            .expect("position must parse");
        let capture = Move::new(
            Square::from_xy(4, 4).expect("valid square"),
            Square::from_xy(5, 4).expect("valid square"),
        );
        last_piece
            .make_move(capture)
            .expect("capture must be legal");
        assert_eq!(
            last_piece.outcome(),
            Some(GameOutcome::win(Color::Red, EndReason::Annihilation))
        );

        let infiltration_rows = [
            ".........",
            "....r....",
            ".........",
            ".........",
            "....R....",
            ".........",
            ".........",
            ".........",
            ".........",
        ];
        let mut infiltration =
            Position::from_rows(Mode::Infiltration, Color::Red, &infiltration_rows)
                .expect("position must parse");
        let boundary_move = Move::new(
            Square::from_xy(4, 1).expect("valid square"),
            Square::from_xy(4, 0).expect("valid square"),
        );
        infiltration
            .make_move(boundary_move)
            .expect("boundary move must be legal");
        assert_eq!(
            infiltration.outcome(),
            Some(GameOutcome::win(Color::Red, EndReason::Infiltration))
        );

        let mut pieces = [[0_u128; 3]; 2];
        pieces[Color::Red.index()][PieceKind::Rock.index()] =
            Square::from_xy(0, 0).expect("valid square").bit();
        pieces[Color::Blue.index()][PieceKind::Rock.index()] =
            Square::from_xy(8, 8).expect("valid square").bit();
        let total_war =
            Position::from_bitboards(Mode::TotalWar, Color::Red, pieces, [BOARD_MASK, 0], 80)
                .expect("position must be valid");
        assert_eq!(
            total_war.outcome(),
            Some(GameOutcome::win(Color::Red, EndReason::Territory))
        );
    }
}
