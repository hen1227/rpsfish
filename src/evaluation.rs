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

//! Deterministic leaf evaluation.
//!
//! # Layering rules
//!
//! Three kinds of knowledge are kept strictly apart, because mixing them is
//! how an author's guesses quietly become the engine's ceiling:
//!
//! 1. **Facts** are derived from the rules and live on [`Position`] (for
//!    example "this piece can never be captured again"). They are not
//!    opinions and may be trusted by search as proofs.
//! 2. **Features** are countable properties of a position, computed here.
//!    Adding one asserts only that the property is *measurable*, never how
//!    much it is worth.
//! 3. **Weights** are the value of each feature. They live in [`EvalParams`]
//!    as plain data so an offline tuner owns them. Hand-picked numbers are a
//!    bootstrap, not a conclusion.
//!
//! Anything of the form "and therefore prefer move X" belongs to search or to
//! move ordering, never here.

use crate::board::{NEIGHBORS, neighbors_of};
use crate::model::{Color, Goal, Mode, ModeRules, PieceKind};
use crate::position::Position;
use crate::{BOARD_SIZE, SQUARE_COUNT};

/// The survival-related weights for one mode, gathered so a mode arm names
/// each one instead of passing seven positional integers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SurvivalWeights {
    immortal_piece: i32,
    immortal_with_prey: i32,
    immortal_survivor: i32,
    scarcity: [i32; SCARCITY_BUCKETS],
    doomed_piece: i32,
    army_doomed: i32,
    last_piece: i32,
    two_pieces: i32,
}

/// How far past the point of no return the endgame race term keeps growing.
///
/// Twenty-five tiles is not more lost than thirteen -- both are B#9 -- so it
/// saturates. Left ungoverned the linear form reaches -14503 on the
/// twenty-five tile position, which is a heuristic shouting over a proof:
/// terminal scores start at 29000 and a weight must never get close enough to
/// imitate one.
const DOOMED_ENDGAME_HORIZON: i32 = 9;

/// How many distinct predator counts get their own weight before saturating.
///
/// Index 0 is "one enemy predator left", index 2 is "three or more". Zero
/// predators is immortality and is scored by its own terms.
pub const SCARCITY_BUCKETS: usize = 3;

/// Tiles that win any fill of the board, however the remaining ones are split.
///
/// `Position::make_move_unchecked` only ever claims a tile neither side owns
/// and never releases one, so territory is monotone: a side that reaches this
/// count keeps it, and every tile past it is spare. Two of these do not fit on
/// one board, which is why a single signed difference can carry both sides.
///
/// Derived from `SQUARE_COUNT` rather than written down as forty-one, so a
/// board-size change cannot leave the number behind. Note that an odd board
/// also makes `Position::outcome`'s equal-territory draw arm unreachable:
/// forty and forty-one is the closest a full board comes to level.
const TERRITORY_MAJORITY: i32 = SQUARE_COUNT as i32 / 2 + 1;

macro_rules! eval_params {
    ($( $(#[$attribute:meta])* $name:ident = $default:expr ),+ $(,)? ) => {
        /// Every tunable evaluation weight, as data rather than as constants.
        ///
        /// Field order is the tuner's parameter vector order and is stable.
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub struct EvalParams {
            $( $(#[$attribute])* pub $name: i32, )+
        }

        impl EvalParams {
            /// The bootstrap weights. These are informed guesses and are
            /// expected to be replaced by tuned values.
            pub const DEFAULT: Self = Self { $( $name: $default, )+ };

            /// Stable parameter names, in vector order.
            pub const NAMES: &'static [&'static str] = &[ $( stringify!($name), )+ ];

            /// Number of tunable parameters.
            pub const COUNT: usize = Self::NAMES.len();

            /// Read one weight by name.
            #[must_use]
            pub const fn get(&self, name: &str) -> Option<i32> {
                // `const` string matching needs byte comparison.
                let bytes = name.as_bytes();
                $(
                    if equal_bytes(bytes, stringify!($name).as_bytes()) {
                        return Some(self.$name);
                    }
                )+
                None
            }

            /// Overwrite one weight by name, reporting whether it existed.
            pub fn set(&mut self, name: &str, value: i32) -> bool {
                $(
                    if name == stringify!($name) {
                        self.$name = value;
                        return true;
                    }
                )+
                false
            }

            /// The weights as a tuner-facing vector.
            #[must_use]
            pub fn to_vec(&self) -> Vec<i32> {
                vec![ $( self.$name, )+ ]
            }

            /// Overwrite the weights from a tuner-facing vector.
            ///
            /// Extra entries are ignored and missing entries are left alone,
            /// so a stored vector from an older parameter set still loads.
            pub fn apply_vec(&mut self, values: &[i32]) {
                let mut index = 0_usize;
                $(
                    if let Some(&value) = values.get(index) {
                        self.$name = value;
                    }
                    index += 1;
                )+
                let _ = index;
            }

            /// Apply `name=value` assignments, returning the first bad entry.
            pub fn apply_assignments<'a, I>(&mut self, entries: I) -> Result<(), String>
            where
                I: IntoIterator<Item = &'a str>,
            {
                for entry in entries {
                    let entry = entry.trim();
                    if entry.is_empty() || entry.starts_with('#') {
                        continue;
                    }
                    let Some((name, value)) = entry.split_once('=') else {
                        return Err(format!("expected name=value, got {entry:?}"));
                    };
                    let name = name.trim();
                    let value = value
                        .trim()
                        .parse::<i32>()
                        .map_err(|error| format!("invalid value for {name:?}: {error}"))?;
                    if !self.set(name, value) {
                        return Err(format!("unknown evaluation parameter {name:?}"));
                    }
                }
                Ok(())
            }

            /// Name/value pairs in vector order.
            #[must_use]
            pub fn entries(&self) -> Vec<(&'static str, i32)> {
                Self::NAMES.iter().copied().zip(self.to_vec()).collect()
            }
        }
    };
}

const fn equal_bytes(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

eval_params! {
    // ---- Total War --------------------------------------------------------
    /// Retired to zero. Territory is real but a *flat* price for it is not:
    /// the fill is one binary outcome, so a forty-tile lead is the same win as
    /// a one-tile lead, and pricing it forty times higher lets a lead that
    /// empty tiles handed over for free outbid the army that has to survive to
    /// collect it. Measured against a copy of the engine with this whole
    /// subsystem removed, the flat form loses 367.7 Elo at 20k nodes and 436.4
    /// at 150k -- worse the deeper it looks -- and *every* one of its 503
    /// losses was by annihilation while every one of its 32 wins was by
    /// territory. Re-adding even a small flat term on top of the shipped
    /// replacement costs 64.4 Elo at 7 and 158.4 at 14.
    ///
    /// [`total_war_territory_race`] is the bounded form that replaced it. See
    /// EVAL_RESULTS.md H9 for the full grid; EVAL_TRAINING.md 4.3 and H5, which
    /// found 28 optimal, were measuring one coordinate at a time with the other
    /// three held here, and the minimum is at the corner of the four-cube.
    total_war_territory = 0,
    total_war_material = 70,
    total_war_mobility = 1,
    total_war_capture = 16,
    /// Per legal move onto a still-neutral tile.
    ///
    /// Total War ends when the last neutral tile is claimed, so this is the
    /// evaluation's only direct contact with the win condition and it turns
    /// out to be the most valuable weight in the mode. The shipped 8 was a
    /// hand-picked guess sitting just off the left edge of a broad plateau
    /// running from 10 to 18: 4 measures -169 Elo and 0 measures -339, while
    /// every value from 10 to 18 measures between +48 and +91.
    ///
    /// 13 confirmed at +92.5 Elo (2000 games, 20k nodes, seed 20260822) and
    /// +104.7 Elo (800 games, 150k nodes, seed 31337), both SPRT ACCEPT.
    /// `total_war_claim_squares` then took most of the weight back off it
    /// again; the two are tuned as a pair and neither is meaningful alone.
    /// This value on its own, with `claim_squares` at 0, measures -169 Elo.
    ///
    /// Retired to zero with the rest of the block. Both numbers above are
    /// symmetric self-play, where each side pays the same bounty for standing
    /// next to empty ground and the paint race is therefore fair; the term is
    /// measuring "paints better", not "plays better". Against an opponent that
    /// ignores territory and hunts pieces, paying an army to spread out across
    /// open ground is what gets it eaten one piece at a time. The block is only
    /// removable together -- this weight alone at 0 measures -339 -- which is
    /// exactly why a coordinate-wise grid could not find it.
    total_war_frontier = 0,
    /// Per *distinct* still-neutral tile reachable next move.
    ///
    /// `total_war_frontier` counts moves, so four pieces crowding one tile
    /// score four times what one piece scores even though only one of them can
    /// ever claim it. This counts tiles instead, which separates a wide
    /// frontier from a traffic jam in front of a narrow one, and it turns out
    /// to be the better of the two: shifting nearly all of the frontier weight
    /// onto it is worth another +97.7 Elo on top of the frontier retune.
    ///
    /// Confirmed at +97.7 Elo (2000 games, 20k nodes, seed 20260822) and
    /// +78.2 Elo (800 games, 150k nodes, seed 31337), both SPRT ACCEPT on
    /// seeds independent of the 4242 the pair was found and tuned on.
    ///
    /// The same counter built for Infiltration's boundary row measured
    /// *exactly* collinear with the move count there and was reverted; the
    /// geometry is what differs, not the idea. See EVAL_RESULTS.md H6.
    ///
    /// Retired to zero with the rest of the block; see
    /// [`total_war_frontier`] for why a self-play gain of this size was still
    /// pointing the wrong way, and H9 for the measurement that shows it.
    total_war_claim_squares = 0,
    /// Scales a territory lead by how little neutral ground is left. Applied
    /// as `lead_sign * claimed * weight / 96`, so 16 is exactly one sixth.
    ///
    /// Only the *sign* of the lead reaches this, which is its limit. Driven to
    /// 600 with the flat terms off, so the ramp is the whole territory
    /// evaluation, it reads `suite/` better than anything else measured
    /// (19/20 correct sign against the shipped 18/20) and plays worse than
    /// having no territory evaluation at all: -37.7 Elo [-67.0, -8.8] over 500
    /// games at 20k nodes, seed 99173, stretching games from 122 plies to 191
    /// with a truncation. A sign knows the lead exists and not whether it is
    /// worth defending, so the engine sits on it. See
    /// [`total_war_majority_haste`] for the form that replaced it.
    total_war_lead_scale = 0,
    /// The whole territory race, bounded, as one number that cannot be outbid.
    ///
    /// Applied as `lead.clamp(-(neutral + 1), neutral + 1) * weight /
    /// (neutral + 1)`, so it is zero at level territory, saturates at exactly
    /// +-`weight`, and reaches saturation precisely when a side holds
    /// [`TERRITORY_MAJORITY`]. The two facts that make it the right shape are
    /// both rules, not fits: each side's remaining need sums with the other's
    /// to `neutral + 1`, so a lead of that size is unbeatable; and
    /// `|lead| >= neutral + 1` is algebraically the same statement as holding
    /// forty-one tiles.
    ///
    /// The ceiling is the point. Winning the fill is one binary outcome worth
    /// one thing, so a forty-tile lead is the same win as a one-tile lead --
    /// but every *unbounded* form prices it forty times higher, and a lead is
    /// cheap to run up because empty tiles do not fight back. Measured against
    /// a copy of the engine with the territory subsystem removed entirely,
    /// every unbounded shape loses monotonically in the size of the reward
    /// (600 games each, 20k nodes, seed 6180339): the shipped flat term -367.7
    /// Elo; a lead scaled by how full the board is (`lead * claimed / 81`)
    /// -169.1 at 28, -346.1 at 56 and -520.9 at 84; the whole block scaled to
    /// a quarter of itself -332.8. Every single loss in all five runs was by
    /// annihilation and every single win was by territory. An engine that can
    /// be shown an arbitrarily large territory number will always take it over
    /// its army.
    ///
    /// The clamp is also where the "forty-one tiles is enough" rule lives, and
    /// it has to enter as a ceiling rather than as a step. A flat "the fill is
    /// decided" bonus was the obvious alternative and arithmetic disqualifies
    /// it: with Red on 40 tiles against 20 the flat term is 28 * 20 = 560, so
    /// replacing it with a fixed 588 on reaching 41 makes the evaluation
    /// *drop* when Red claims the tile that wins the game, and Red has other
    /// moves that keep 560. Staying upward needs 28 * 40 = 1120, which prices
    /// that one tile at sixteen pieces. The rule this leaves for any later
    /// territory term: because territory is monotone and irreversible, an
    /// *upward* step at a territory threshold is safe -- search can be
    /// encouraged across it and can never come back to farm it -- while a
    /// *downward* step is disqualifying whatever the monotonicity, because
    /// search simply refuses the move.
    total_war_territory_race = 70,
    /// Per claimed tile, once a side's count already wins any fill.
    ///
    /// The other half of [`TERRITORY_MAJORITY`], and the reason a ceiling is
    /// not the whole story: a side that has won the fill still has to *reach*
    /// it, and a term that saturates gives it no reason to. Bounded shapes
    /// measured against the territory-blind engine draw 34% to 53% of their
    /// games by repetition -- the engine sits on a lead it cannot increase
    /// instead of cashing it. (Measured while a repeat was still a draw. The
    /// shape of the failure is unchanged now that it is not: the engine still
    /// sits, and the game is drawn on the hundredth move with nothing taken
    /// rather than on the third repeat.)
    ///
    /// So this pays the side that has already won the fill for the board being
    /// full, which is the one thing it still wants. `total_war_lead_scale` is
    /// the same idea keyed on the *sign* of the lead, and the sign is the
    /// wrong gate twice over: it fires while the lead is still a coin flip,
    /// and it steps discontinuously at level territory, which is not a
    /// monotone quantity and so can be crossed back and forth. A majority
    /// cannot be crossed back -- territory only ever grows -- so gating here
    /// is an upward step on a monotone quantity, which is the one shape a
    /// territory term is allowed to have.
    ///
    /// Measured against the territory-blind engine at 300 game pairs, seed
    /// 6180339, with `total_war_territory_race` at 70: the majority gate at 4
    /// is -13.9 Elo and at 8 -17.4, against -21.5 for `total_war_lead_scale`
    /// at 400 doing the same job off the sign. Running both together
    /// double-counts and measures -56.7. Confirmed at +17.0 Elo [-2.2, +36.4]
    /// over 1000 pairs on seed 1414213 and +12.7 [-11.8, +37.5] on 31622776 --
    /// level with an engine that ignores territory entirely, while winning 389
    /// of those 1000 games on territory where the blind engine wins none.
    total_war_majority_haste = 4,
    total_war_immortal_piece = 40,
    total_war_immortal_with_prey = 40,
    total_war_immortal_survivor = 0,
    /// Per piece bucketed by how many enemy predators it still faces: one,
    /// two, or three or more.
    ///
    /// These are the only tuned weights in this file, and they are a warning
    /// about the rest. Hand-picking plausible *positive* values (30/12/4)
    /// measured -52 Elo over 1200 game pairs. SPSA then found *negative*
    /// values, which confirmed at +60.7 Elo (1400 games, 6k nodes, seed 77)
    /// and +53.6 Elo (1000 games, 14k nodes, seed 20260820) on seeds
    /// independent of the tuning run.
    total_war_scarcity_1 = -6,
    total_war_scarcity_2 = -6,
    total_war_scarcity_3 = 5,
    /// Per piece whose predator kind the enemy can never lose.
    total_war_doomed_piece = 0,
    /// Flat, when *every* piece a side owns is permanently huntable.
    total_war_army_doomed = 0,
    /// Flat, at one piece remaining, where annihilation loses the game.
    total_war_last_piece = 0,
    /// Flat, at two pieces remaining.
    total_war_two_pieces = 0,
    /// Per tile the same one-doomed-piece side must still see claimed after
    /// the hunt reaches it, saturating at [`DOOMED_ENDGAME_HORIZON`].
    ///
    /// Total War ends when the last tile is claimed, so a side one capture
    /// from losing is in a race: claim out the board, or be caught. The length
    /// of that race is the neutral count less the distance the nearest hunter
    /// still has to travel, and on every position in `suite/` that quantity
    /// separates the two outcomes exactly -- every verified win for the hunted
    /// side scores zero or one, every verified loss two or more.
    /// The conjunction is what neither `total_war_army_doomed` nor
    /// `total_war_last_piece` can express on its own. A last piece that is
    /// *immortal* is not a liability at all -- it cannot be taken, so its
    /// owner cannot be annihilated -- and a doomed army of four pieces still
    /// costs the enemy four captures. Only "one piece, and the capture that
    /// ends the game is available forever" describes a side one move from
    /// losing, and a linear sum of the two existing weights prices both
    /// harmless cases exactly as heavily as the fatal one.
    ///
    /// Self-play cannot price this. At -5000 it changes zero games out of two
    /// thousand, which is EVAL_TRAINING.md 4.2 exactly, and the same is true
    /// of the four zeroed weights above it. It is measured on `suite/`
    /// instead: 10/20 positions read correctly at the review's own budget
    /// before, 19/20 after, at a cost of 0.0 Elo over 2000 games. A flat
    /// version of the same conjunction was tried and is worse than nothing,
    /// because it also fires on the positions the hunted side wins.
    total_war_doomed_endgame = -600,

    // ---- Infiltration -----------------------------------------------------
    infiltration_material = 35,
    infiltration_mobility = 2,
    infiltration_capture = 12,
    /// Summed forward progress of every piece.
    infiltration_advancement = 9,
    /// Forward progress of the single most advanced piece.
    infiltration_closest = 32,
    /// Per legal move that would reach the target boundary immediately.
    infiltration_goal_threat = 180,
    /// Permanence is worth much less in a race than in a survival mode.
    ///
    /// An uncapturable piece still has to walk nine ranks, and one that cannot
    /// advance wins nothing. All three of these were priced as if Infiltration
    /// were the retired Annihilation mode, which made an uncapturable piece
    /// worth nearly three ordinary ones against `infiltration_material` 35.
    /// Copying that mode's `immortal_survivor` of 160 into this one measures
    /// -34.5 Elo; the values below confirmed together at +36.4 Elo (3000 games, 20k
    /// nodes, seed 20260822) and +36.4 Elo (2300 games, 150k nodes, seed
    /// 31337), both SPRT ACCEPT on seeds independent of the tuning run.
    ///
    /// This is a re-pricing and not a removal: zeroing the block outright and
    /// paying -160 on top measured -7.3 Elo, i.e. worse than the old values.
    infiltration_immortal_piece = 30,
    infiltration_immortal_with_prey = 0,
    infiltration_immortal_survivor = -80,
    /// Per piece bucketed by how many enemy predators it still faces: one,
    /// two, or three or more.
    ///
    /// Bucket 1 -- a piece with exactly one surviving enemy predator -- is the
    /// only one worth anything here, and it wants a *positive* value, which is
    /// the opposite sign to Total War's. Infiltration is a race, and a piece
    /// one capture away from being permanently safe is a piece that may get to
    /// walk to the goal; Total War is not a race, and there the same feature
    /// tunes negative. Total War's own tuned shape (-6/-6/+5) measures -24 Elo
    /// when copied into this mode and -190 Elo when scaled up four times, so
    /// the two modes genuinely disagree.
    ///
    /// 15 confirmed at +23.6 Elo (5200 games, 20k nodes, seed 20260822) and
    /// +42.1 Elo (1800 games, 150k nodes, seed 31337), both SPRT ACCEPT and
    /// both on seeds independent of the 4242 used to find and tune it. The
    /// gain grows with depth rather than washing out, which is the behaviour
    /// §4.6 asks for.
    infiltration_scarcity_1 = 15,
    /// Measured worse than zero in both directions at 1000 games; left alone.
    infiltration_scarcity_2 = 0,
    infiltration_scarcity_3 = 0,
    /// Per piece whose predator kind the enemy can never lose.
    infiltration_doomed_piece = 0,
    /// Flat, when *every* piece a side owns is permanently huntable.
    infiltration_army_doomed = 0,
    /// Zero pieces is a stalemate draw in Infiltration rather than a loss, so
    /// these two exist only so the arena can confirm that. `ModeRules` gates
    /// them off, and they stay listed to keep the vector uniform across modes.
    infiltration_last_piece = 0,
    infiltration_two_pieces = 0,

    // ---- Intransitive -----------------------------------------------------
    // Safe routes replace geometric advancement: a runner behind an
    // uncapturable blocker has not made useful progress. The second runner
    // and local RPS support price pressure the first runner cannot supply.
    // Paired matches, ablations, and independent confirmation are recorded
    // in docs/INTRANSITIVE.md. Unchanged weights retain their earlier fit.
    intransitive_material = 49,
    intransitive_mobility = -1,
    intransitive_capture = 9,
    /// Retained for old parameter files; replaced by safe corner routes.
    intransitive_advancement = 0,
    /// Retained for old parameter files; replaced by safe corner routes.
    intransitive_closest = 0,
    intransitive_goal_threat = 190,
    intransitive_corner_guard = -2,
    intransitive_corner_plug = 32,
    /// A currently uncapturable plug is valuable, but is not a draw proof:
    /// its owner may be forced to move it away. The two-piece solver handles
    /// that distinction exactly.
    intransitive_corner_seal = 92,
    intransitive_immortal_piece = 63,
    intransitive_immortal_with_prey = 9,
    intransitive_immortal_survivor = -5,
    intransitive_scarcity_1 = 15,
    intransitive_scarcity_2 = 2,
    intransitive_scarcity_3 = -11,
    intransitive_doomed_piece = -9,
    intransitive_army_doomed = 3,
    /// Losing the last piece loses by stalemate. These are now live features
    /// under that rule, but remain untuned at zero.
    intransitive_last_piece = 0,
    intransitive_two_pieces = 0,

    // Appended to preserve the order of existing tuning vectors.
    /// Progress of the nearest runner along a currently safe, legal route.
    intransitive_safe_path = 30,
    /// Progress of the second runner along such a route.
    intransitive_second_path = 15,
    /// Pieces covered by a friendly capturer of their predator.
    intransitive_support = 8,

}

impl EvalParams {
    /// What one piece is worth in this mode.
    ///
    /// The evaluation's unit of account. Weights are per-mode and their scales
    /// differ by an order of magnitude, so anything that wants to express a
    /// margin -- a pruning window, a resignation threshold, a test's idea of
    /// "decided" -- has to say it in these units rather than in a fixed
    /// centipawn. `Searcher`'s pruning margins read it, and the frontend's
    /// `MODE_SCORE_SCALE` hand-copies it.
    #[must_use]
    pub const fn material(&self, mode: Mode) -> i32 {
        match mode {
            Mode::TotalWar => self.total_war_material,
            Mode::Infiltration => self.infiltration_material,
            Mode::Intransitive => self.intransitive_material,
        }
    }
}

impl Default for EvalParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Evaluate a non-terminal position with the default weights.
#[must_use]
pub fn evaluate(position: &Position) -> i32 {
    evaluate_with(position, &EvalParams::DEFAULT)
}

/// Evaluate a non-terminal position from the side-to-move perspective.
///
/// Terminal scoring belongs to search. Keeping terminal values out of the
/// heuristic prevents a finite feature weight from competing with a proven
/// win, loss, or draw.
#[must_use]
pub fn evaluate_with(position: &Position, params: &EvalParams) -> i32 {
    let rules = position.mode().rules();
    let red = SideSummary::compute(position, Color::Red, rules);
    let blue = SideSummary::compute(position, Color::Blue, rules);
    // Adding a mode means adding one arm here plus its own weights. Nothing
    // else in the engine needs to change.
    let red_score = match position.mode() {
        Mode::TotalWar => total_war_score(position, params, &red, &blue),
        Mode::Infiltration => infiltration_score(params, &red, &blue),
        Mode::Intransitive => {
            intransitive_score(params, &red, &blue) + corner_score(position, params)
        }
    };
    if position.side_to_move() == Color::Red {
        red_score
    } else {
        -red_score
    }
}

fn total_war_score(
    position: &Position,
    params: &EvalParams,
    red: &SideSummary,
    blue: &SideSummary,
) -> i32 {
    let red_territory = i32_from_u32(position.territory_count(Color::Red));
    let blue_territory = i32_from_u32(position.territory_count(Color::Blue));
    let territory = red_territory - blue_territory;
    let neutral = i32_from_u32(position.neutral_territory_count());
    let claimed = i32::from(SQUARE_COUNT) - neutral;
    let lead_scaling = territory.signum() * claimed * params.total_war_lead_scale / 96;
    // The lead against the claims still to come. The two sides' remaining
    // needs sum to this, so a lead of that size cannot be caught and anything
    // past it is the same win -- which is why this saturates instead of
    // scaling. Division is safe: a terminal board never reaches evaluation,
    // and the denominator is at least one regardless.
    let horizon = neutral + 1;
    let race = territory.clamp(-horizon, horizon) * params.total_war_territory_race / horizon;
    // Who, if anyone, has already won the fill, and so wants it to arrive.
    // Both at once cannot happen: two majorities do not fit on one board.
    let decided = i32::from(red_territory >= TERRITORY_MAJORITY)
        - i32::from(blue_territory >= TERRITORY_MAJORITY);
    // Both operands are already counted for other terms, so the conjunction
    // costs one multiply and no extra pass over the board.
    let doomed_endgame = red.army_doomed * red.last_piece - blue.army_doomed * blue.last_piece;
    let race_length = if doomed_endgame == 0 {
        0
    } else {
        let hunted = if doomed_endgame > 0 {
            Color::Red
        } else {
            Color::Blue
        };
        (neutral - hunt_distance(position, hunted) - 1).clamp(0, DOOMED_ENDGAME_HORIZON)
    };
    territory * params.total_war_territory
        + race
        + decided * claimed * params.total_war_majority_haste
        + doomed_endgame * race_length * params.total_war_doomed_endgame
        + delta(red.pieces, blue.pieces, params.total_war_material)
        + delta(red.mobility, blue.mobility, params.total_war_mobility)
        + delta(red.captures, blue.captures, params.total_war_capture)
        + delta(red.claimable, blue.claimable, params.total_war_frontier)
        + delta(
            red.claim_squares,
            blue.claim_squares,
            params.total_war_claim_squares,
        )
        + lead_scaling
        + survival_delta(
            red,
            blue,
            SurvivalWeights {
                immortal_piece: params.total_war_immortal_piece,
                immortal_with_prey: params.total_war_immortal_with_prey,
                immortal_survivor: params.total_war_immortal_survivor,
                scarcity: [
                    params.total_war_scarcity_1,
                    params.total_war_scarcity_2,
                    params.total_war_scarcity_3,
                ],
                doomed_piece: params.total_war_doomed_piece,
                army_doomed: params.total_war_army_doomed,
                last_piece: params.total_war_last_piece,
                two_pieces: params.total_war_two_pieces,
            },
        )
}

fn infiltration_score(params: &EvalParams, red: &SideSummary, blue: &SideSummary) -> i32 {
    delta(red.pieces, blue.pieces, params.infiltration_material)
        + delta(red.mobility, blue.mobility, params.infiltration_mobility)
        + delta(red.captures, blue.captures, params.infiltration_capture)
        + delta(
            red.advancement,
            blue.advancement,
            params.infiltration_advancement,
        )
        + delta(red.closest, blue.closest, params.infiltration_closest)
        + delta(
            red.goal_moves,
            blue.goal_moves,
            params.infiltration_goal_threat,
        )
        + survival_delta(
            red,
            blue,
            SurvivalWeights {
                immortal_piece: params.infiltration_immortal_piece,
                immortal_with_prey: params.infiltration_immortal_with_prey,
                immortal_survivor: params.infiltration_immortal_survivor,
                scarcity: [
                    params.infiltration_scarcity_1,
                    params.infiltration_scarcity_2,
                    params.infiltration_scarcity_3,
                ],
                doomed_piece: params.infiltration_doomed_piece,
                army_doomed: params.infiltration_army_doomed,
                last_piece: params.infiltration_last_piece,
                two_pieces: params.infiltration_two_pieces,
            },
        )
}

/// Intransitive's arm.
///
/// Infiltration's race terms over a corner-shaped goal, plus the three terms
/// the corner adds. The race half reads the same because the mode *is* a race;
/// the corner half is what stops it from being only that, and is where a side
/// that cannot win the race goes instead.
fn intransitive_score(params: &EvalParams, red: &SideSummary, blue: &SideSummary) -> i32 {
    delta(red.pieces, blue.pieces, params.intransitive_material)
        + delta(red.mobility, blue.mobility, params.intransitive_mobility)
        + delta(red.captures, blue.captures, params.intransitive_capture)
        + delta(
            red.advancement,
            blue.advancement,
            params.intransitive_advancement,
        )
        + delta(red.closest, blue.closest, params.intransitive_closest)
        + delta(
            red.goal_moves,
            blue.goal_moves,
            params.intransitive_goal_threat,
        )
        + delta(
            red.corner_guard,
            blue.corner_guard,
            params.intransitive_corner_guard,
        )
        + delta(
            red.corner_plug,
            blue.corner_plug,
            params.intransitive_corner_plug,
        )
        + delta(
            red.corner_seal,
            blue.corner_seal,
            params.intransitive_corner_seal,
        )
        + survival_delta(
            red,
            blue,
            SurvivalWeights {
                immortal_piece: params.intransitive_immortal_piece,
                immortal_with_prey: params.intransitive_immortal_with_prey,
                immortal_survivor: params.intransitive_immortal_survivor,
                scarcity: [
                    params.intransitive_scarcity_1,
                    params.intransitive_scarcity_2,
                    params.intransitive_scarcity_3,
                ],
                doomed_piece: params.intransitive_doomed_piece,
                army_doomed: params.intransitive_army_doomed,
                last_piece: params.intransitive_last_piece,
                two_pieces: params.intransitive_two_pieces,
            },
        )
}

const fn delta(red: i32, blue: i32, weight: i32) -> i32 {
    (red - blue) * weight
}

/// These are estimates against the current defenders, never race proofs:
/// opponents can move, sacrifice a blocker, or change the piece hierarchy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CornerFeatures {
    paths: [i32; 2],
    support: i32,
}

impl CornerFeatures {
    fn compute(position: &Position, color: Color, routes: bool) -> Self {
        let mut features = Self::default();
        let goal = position.mode().rules().goal.mask(color);
        let empty = position.empty_squares();
        for kind in PieceKind::ALL {
            let mine = position.piece_bitboard(color, kind);
            if mine == 0 {
                continue;
            }
            let prey = position.piece_bitboard(color.other(), kind.prey());
            let danger = neighbors_of(position.piece_bitboard(color.other(), kind.predator()));
            let safe = (empty | prey) & (!danger | goal);
            features.support += (mine & neighbors_of(position.piece_bitboard(color, kind.prey())))
                .count_ones() as i32;
            if routes && goal & safe != 0 {
                // Reverse BFS: own pieces may end a route but cannot be
                // walked through. Enemy non-prey block it; predator reach is
                // unsafe everywhere except the goal, where arrival wins.
                let mut frontier = goal;
                let mut visited = goal;
                let mut runners = mine;
                for distance in 1..=Goal::SPAN {
                    let reached = neighbors_of(frontier) & !visited;
                    let mut arrivals = reached & runners;
                    while arrivals != 0 {
                        let progress = Goal::SPAN + 1 - distance;
                        if progress > features.paths[0] {
                            features.paths[1] = features.paths[0];
                            features.paths[0] = progress;
                        } else if progress > features.paths[1] {
                            features.paths[1] = progress;
                        }
                        arrivals &= arrivals - 1;
                    }
                    runners &= !reached;
                    if runners == 0 || features.paths[1] >= Goal::SPAN + 1 - distance {
                        break;
                    }
                    frontier = reached & safe;
                    if frontier == 0 {
                        break;
                    }
                    visited |= frontier;
                }
            }
        }
        features
    }
}

fn corner_score(position: &Position, params: &EvalParams) -> i32 {
    let routes = params.intransitive_safe_path != 0 || params.intransitive_second_path != 0;
    if !routes && params.intransitive_support == 0 {
        return 0;
    }
    let red = CornerFeatures::compute(position, Color::Red, routes);
    let blue = CornerFeatures::compute(position, Color::Blue, routes);
    delta(red.paths[0], blue.paths[0], params.intransitive_safe_path)
        + delta(red.paths[1], blue.paths[1], params.intransitive_second_path)
        + delta(red.support, blue.support, params.intransitive_support)
}

/// Score both sides' permanence of material.
///
/// Immortality is irreversible, so unlike mobility or distance it describes a
/// property the rest of the game keeps. The scarcity buckets extend that into
/// a gradient: a kind whose last predator is still alive is one capture from
/// becoming permanent, which a boolean feature cannot express.
fn survival_delta(red: &SideSummary, blue: &SideSummary, weights: SurvivalWeights) -> i32 {
    survival_score(red, weights) - survival_score(blue, weights)
}

fn survival_score(side: &SideSummary, weights: SurvivalWeights) -> i32 {
    let mut result = side.immortal_pieces * weights.immortal_piece
        + side.immortal_with_prey * weights.immortal_with_prey
        + side.doomed_pieces * weights.doomed_piece
        + side.army_doomed * weights.army_doomed
        + side.last_piece * weights.last_piece
        + side.two_pieces * weights.two_pieces;
    if side.immortal_pieces > 0 {
        result += weights.immortal_survivor;
    }
    for (bucket, weight) in weights.scarcity.into_iter().enumerate() {
        result += side.scarcity[bucket] * weight;
    }
    result
}

/// Countable properties of one side, gathered in a single pass.
///
/// Evaluation runs at every leaf, so this deliberately avoids building move
/// lists: destinations come from the neighbour table and every feature is a
/// popcount over the same mask.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SideSummary {
    pieces: i32,
    mobility: i32,
    captures: i32,
    claimable: i32,
    /// Distinct still-neutral tiles reachable next move.
    ///
    /// Never larger than `claimable` and often much smaller, because pieces
    /// stand next to each other and their neighbourhoods overlap. The gap
    /// between the two is how much of an apparent frontier is duplicated
    /// effort.
    claim_squares: i32,
    goal_moves: i32,
    advancement: i32,
    closest: i32,
    kinds_with_prey: i32,
    immortal_pieces: i32,
    /// Pieces whose predator kind the enemy can never lose.
    ///
    /// The dual of `immortal_pieces`, and the one the evaluation was missing.
    /// A piece is doomed when the enemy owns the kind that captures it *and*
    /// this side can never capture that kind back, which is a rule-derived
    /// fact rather than an opinion: the hierarchy is one 3-cycle and no mode
    /// ever returns a piece to the board, so a hunter with no surviving
    /// predator hunts for the rest of the game.
    doomed_pieces: i32,
    /// One when every piece this side owns is doomed, zero otherwise.
    ///
    /// A side in that state cannot defend its material at all, only run. The
    /// per-piece count alone cannot say that, because two doomed pieces out of
    /// nine and two out of two are the same number.
    army_doomed: i32,
    /// One when a side is down to a single piece and losing it loses the game.
    ///
    /// Material is otherwise linear, which prices the last piece exactly like
    /// the fifth. Where annihilation or stalemate loses, the last piece
    /// can be worth the whole game.
    last_piece: i32,
    /// One when a side is down to two pieces and reaching zero loses.
    two_pieces: i32,
    immortal_with_prey: i32,
    /// Pieces standing in the four-square shell around the corner this side
    /// has to defend.
    ///
    /// Meaningless in a rank-goal mode and left at zero there. In Intransitive
    /// it is most of the defence: every path to the goal ends on one of three
    /// squares, so a piece parked on one is a piece the runner has to solve
    /// rather than walk past.
    corner_guard: i32,
    /// One when this side occupies the very corner it defends.
    ///
    /// Standing on the tile is categorically different from standing beside
    /// it, because the game ends the instant an enemy piece arrives: a
    /// defender on the corner cannot be walked around, only captured, and only
    /// by the one kind that beats it.
    corner_plug: i32,
    /// One when that occupying piece can never be captured at all.
    ///
    /// The enemy cannot capture the plug, but forced moves can reopen the
    /// corner. This is deliberately not a search bound.
    corner_seal: i32,
    scarcity: [i32; SCARCITY_BUCKETS],
}

impl SideSummary {
    fn compute(position: &Position, color: Color, rules: ModeRules) -> Self {
        let mut summary = Self {
            pieces: i32_from_u32(position.piece_count(color)),
            ..Self::default()
        };
        let empty = position.empty_squares();
        let neutral = if rules.uses_territory {
            position.neutral_territory()
        } else {
            0
        };
        // What this side wins by reaching, and the corner it has to keep the
        // enemy off. Both come from the rules table rather than the mode, so
        // the shapes are the only thing that differs between goal modes.
        let target = rules.goal.mask(color);
        let defended = rules.goal.mask(color.other());
        let defended_shell = rules.goal.shell(color.other());

        let mut claim_reach = 0_u128;

        for kind in PieceKind::ALL {
            let mine = position.piece_bitboard(color, kind);
            if mine == 0 {
                continue;
            }
            let count = i32_from_u32(mine.count_ones());
            let prey = position.piece_bitboard(color.other(), kind.prey());
            if prey != 0 {
                summary.kinds_with_prey += 1;
            }

            let predators = position.predator_count(color, kind);
            if predators == 0 {
                summary.immortal_pieces += count;
                if prey != 0 {
                    summary.immortal_with_prey += count;
                }
            } else {
                let bucket = (predators as usize).min(SCARCITY_BUCKETS) - 1;
                summary.scarcity[bucket] += count;
                // The hunter is here and can never be hunted back, so these
                // pieces stay capturable for the rest of the game.
                if position.is_immortal_kind(color.other(), kind.predator()) {
                    summary.doomed_pieces += count;
                }
            }

            if defended_shell != 0 {
                summary.corner_guard += i32_from_u32((mine & defended_shell).count_ones());
                if mine & defended != 0 {
                    summary.corner_plug = 1;
                    // The plug cannot be captured when no predator survives.
                    // It can still be forced to leave, so this is a feature
                    // and never a search bound.
                    if predators == 0 {
                        summary.corner_seal = 1;
                    }
                }
            }

            let allowed = empty | prey;
            let mut sources = mine;
            while sources != 0 {
                let index = sources.trailing_zeros() as usize;
                let destinations = NEIGHBORS[index] & allowed;
                summary.mobility += i32_from_u32(destinations.count_ones());
                summary.captures += i32_from_u32((destinations & prey).count_ones());
                if neutral != 0 {
                    let reachable = destinations & neutral;
                    summary.claimable += i32_from_u32(reachable.count_ones());
                    claim_reach |= reachable;
                }
                if target != 0 {
                    summary.goal_moves += i32_from_u32((destinations & target).count_ones());
                    let progress = progress_toward_goal(rules.goal, color, index);
                    summary.advancement += progress;
                    summary.closest = summary.closest.max(progress);
                }
                sources &= sources - 1;
            }
        }

        summary.claim_squares = i32_from_u32(claim_reach.count_ones());
        summary.army_doomed =
            i32::from(summary.pieces > 0 && summary.doomed_pieces == summary.pieces);
        // Gated on the rule, not on the mode. Infiltration leaves a side with
        // no pieces unable to move, which is a stalemate draw rather than a
        // loss, so there is no cliff at zero to price.
        if rules.annihilation_loses || rules.stalemate_loses {
            summary.last_piece = i32::from(summary.pieces == 1);
            summary.two_pieces = i32::from(summary.pieces == 2);
        }
        summary
    }
}

/// How far a piece at `index` has advanced toward its side's goal.
///
/// Moves remaining, subtracted from the longest run the goal can ask for, so
/// bigger is nearer and an unmoved piece scores zero in either mode. The shape
/// does the work: against a rank only the rank counts and every file is as
/// good as any other, while against a corner a piece has to arrive on one
/// file as well, and a runner that has crossed eight ranks up the wrong edge
/// has made no progress at all. Pricing an Intransitive race by rank would
/// reward exactly that piece.
const fn progress_toward_goal(goal: Goal, color: Color, index: usize) -> i32 {
    Goal::SPAN - goal.distance(color, index)
}

/// Moves the nearest hunter still needs to reach a side's last piece.
///
/// Chebyshev distance, because a move is a king step. Only reached when that
/// side is down to one permanently huntable piece, which is rare enough that
/// the scan costs nothing in an ordinary position, and it is the variable the
/// clock alone cannot see: the suite's race positions flip from won to lost at
/// five remaining tiles with the hunters three squares away and at ten with
/// them seven squares away. Subtracting it turns "tiles left" into "tiles left
/// after the hunt arrives", which is what actually decides the race.
fn hunt_distance(position: &Position, color: Color) -> i32 {
    for kind in PieceKind::ALL {
        let mine = position.piece_bitboard(color, kind);
        if mine == 0 {
            continue;
        }
        // The caller has already established there is exactly one.
        let index = mine.trailing_zeros() as usize;
        let x = (index % BOARD_SIZE as usize) as i32;
        let y = (index / BOARD_SIZE as usize) as i32;
        let mut hunters = position.piece_bitboard(color.other(), kind.predator());
        let mut nearest = i32::from(BOARD_SIZE);
        while hunters != 0 {
            let hunter = hunters.trailing_zeros() as usize;
            let distance = ((hunter % BOARD_SIZE as usize) as i32 - x)
                .abs()
                .max(((hunter / BOARD_SIZE as usize) as i32 - y).abs());
            nearest = nearest.min(distance);
            hunters &= hunters - 1;
        }
        return nearest;
    }
    0
}

fn i32_from_u32(value: u32) -> i32 {
    i32::try_from(value).expect("board-derived count must fit in i32")
}

#[cfg(test)]
mod tests {
    use super::{EvalParams, TERRITORY_MAJORITY, evaluate, evaluate_with};
    use crate::SQUARE_COUNT;
    use crate::model::{Color, Goal, Mode, PieceKind};
    use crate::position::Position;

    #[test]
    fn all_starting_evaluations_are_balanced() {
        for mode in Mode::ALL {
            let position = Position::starting(mode);
            assert_eq!(evaluate(&position), 0, "{mode} must start balanced");
        }
    }

    #[test]
    fn infiltration_rewards_forward_progress() {
        let rows = [
            "....R....",
            ".........",
            ".........",
            ".........",
            "....r....",
            ".........",
            ".........",
            ".........",
            ".........",
        ];
        let red = Position::from_rows(Mode::Infiltration, Color::Red, &rows)
            .expect("position must parse");
        assert!(evaluate(&red) > 0);
    }

    /// The shape of a real lost game: Red down to one Paper against two Blue
    /// Scissors, well ahead on territory.
    ///
    /// Red has no Rock and can never obtain one, so Blue's Scissors can never
    /// be captured and Red's only piece is huntable for the rest of the game.
    /// Search proves this position lost for Red, but not until depth 23, which
    /// no shipped bot or review reaches. That is what these weights exist to
    /// tell it sooner.
    fn one_doomed_paper_against_two_immortal_scissors() -> Position {
        let rows = [
            ".........",
            ".........",
            "...S.....",
            ".....p...",
            ".........",
            ".........",
            "......S..",
            ".P.....R.",
            ".........",
        ];
        Position::from_rows(Mode::TotalWar, Color::Red, &rows).expect("position must parse")
    }

    #[test]
    fn a_piece_is_doomed_when_its_hunter_can_never_be_hunted_back() {
        let position = one_doomed_paper_against_two_immortal_scissors();
        let baseline = evaluate(&position);

        // Red owns exactly one doomed piece and Blue owns none, so the whole
        // per-piece weight lands on Red once and nowhere else.
        let mut params = EvalParams::DEFAULT;
        params.total_war_doomed_piece = -1_000;
        assert_eq!(evaluate_with(&position, &params), baseline - 1_000);

        // Every piece Red has is doomed, so the army-wide term fires too.
        let mut params = EvalParams::DEFAULT;
        params.total_war_army_doomed = -1_000;
        assert_eq!(evaluate_with(&position, &params), baseline - 1_000);

        // And Red is down to the piece whose loss ends the game.
        let mut params = EvalParams::DEFAULT;
        params.total_war_last_piece = -1_000;
        assert_eq!(evaluate_with(&position, &params), baseline - 1_000);

        let mut params = EvalParams::DEFAULT;
        params.total_war_two_pieces = -1_000;
        assert_eq!(
            evaluate_with(&position, &params),
            baseline,
            "Red has one piece, not two"
        );
    }

    /// The endgame race term with the weight taken back out, so a probe can
    /// diff one position against itself instead of against another position.
    fn without_the_endgame_race() -> EvalParams {
        EvalParams {
            total_war_doomed_endgame: 0,
            ..EvalParams::DEFAULT
        }
    }

    fn endgame_race_term(position: &Position, weight: i32) -> i32 {
        let priced = EvalParams {
            total_war_doomed_endgame: weight,
            ..EvalParams::DEFAULT
        };
        evaluate_with(position, &priced) - evaluate_with(position, &without_the_endgame_race())
    }

    #[test]
    fn the_endgame_race_is_the_clock_less_the_distance_to_the_hunter() {
        // One Red Paper on a1 with eight tiles still unclaimed, and one Blue
        // Scissors that is either three files away or six. Nothing else about
        // the two positions differs, so the whole gap between them is the
        // travel the hunt still has to do.
        let near = Position::from_fen(
            Mode::TotalWar,
            "p2S5/9/9/9/9/9/9/9/9 r rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/\
             rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/8b",
        )
        .expect("position must parse");
        let far = Position::from_fen(
            Mode::TotalWar,
            "p5S2/9/9/9/9/9/9/9/9 r rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/\
             rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/8b",
        )
        .expect("position must parse");
        assert_eq!(near.neutral_territory_count(), 8);
        assert_eq!(far.neutral_territory_count(), 8);

        // Eight tiles less three squares of travel less the move the capture
        // itself takes.
        assert_eq!(endgame_race_term(&near, -100), -400);
        assert_eq!(endgame_race_term(&far, -100), -100);
    }

    #[test]
    fn the_endgame_race_saturates_rather_than_scaling_with_an_empty_board() {
        // `from_rows` claims only the squares the pieces stand on, so five of
        // the eighty-one tiles are owned and seventy-six are still in play --
        // far past the point where more of them make the position more lost.
        let position = one_doomed_paper_against_two_immortal_scissors();
        assert_eq!(position.neutral_territory_count(), 76);
        assert_eq!(endgame_race_term(&position, -10), -90);
    }

    #[test]
    fn an_immortal_last_piece_is_not_an_endgame_liability() {
        // Red's lone Rock is hunted by Paper and Blue owns none, so it can
        // never be captured and Red can never be annihilated. This is the
        // position `total_war_last_piece` scores exactly like a doomed one.
        let rows = [
            ".........",
            ".........",
            "...S.....",
            ".........",
            "....r....",
            ".........",
            "......S..",
            ".......R.",
            ".........",
        ];
        let position =
            Position::from_rows(Mode::TotalWar, Color::Red, &rows).expect("position must parse");
        let baseline = evaluate(&position);

        let params = EvalParams {
            total_war_last_piece: -1_000,
            ..EvalParams::DEFAULT
        };
        assert_eq!(
            evaluate_with(&position, &params),
            baseline - 1_000,
            "the existing cliff term cannot tell the two cases apart"
        );

        assert_eq!(
            endgame_race_term(&position, -1_000),
            0,
            "an uncapturable last piece is not one capture from losing"
        );
    }

    #[test]
    fn a_doomed_army_of_two_is_not_one_capture_from_losing() {
        // Both of Red's Papers are condemned, so `total_war_army_doomed`
        // fires, but Blue still needs two captures rather than one.
        let rows = [
            ".........",
            ".........",
            "...S.....",
            ".....p...",
            "....p....",
            ".........",
            "......S..",
            ".P.....R.",
            ".........",
        ];
        let position =
            Position::from_rows(Mode::TotalWar, Color::Red, &rows).expect("position must parse");
        let baseline = evaluate(&position);

        let params = EvalParams {
            total_war_army_doomed: -1_000,
            ..EvalParams::DEFAULT
        };
        assert_eq!(
            evaluate_with(&position, &params),
            baseline - 1_000,
            "the existing army-wide term cannot tell the two cases apart"
        );

        assert_eq!(
            endgame_race_term(&position, -1_000),
            0,
            "two doomed pieces still cost the enemy two captures"
        );
    }

    #[test]
    fn a_hunter_that_can_still_be_hunted_does_not_doom_its_prey() {
        // Red holds all three kinds, so every Blue hunter has a Red predator
        // of its own and nothing on the board is condemned.
        let rows = [
            ".........",
            ".........",
            "...S.....",
            ".....p...",
            "....r..s.",
            ".........",
            "......S..",
            ".P.....R.",
            ".........",
        ];
        let position =
            Position::from_rows(Mode::TotalWar, Color::Red, &rows).expect("position must parse");
        let baseline = evaluate(&position);
        for params in [
            EvalParams {
                total_war_doomed_piece: -1_000,
                ..EvalParams::DEFAULT
            },
            EvalParams {
                total_war_army_doomed: -1_000,
                ..EvalParams::DEFAULT
            },
            EvalParams {
                total_war_last_piece: -1_000,
                ..EvalParams::DEFAULT
            },
            EvalParams {
                total_war_two_pieces: -1_000,
                ..EvalParams::DEFAULT
            },
        ] {
            assert_eq!(evaluate_with(&position, &params), baseline);
        }
    }

    #[test]
    fn doom_is_decided_per_kind_rather_than_per_side() {
        // Red's Paper is safe -- Red's Rock can answer Blue's Scissors -- but
        // Red's Rock is not: its hunter is Blue's Paper, and Red owns no
        // Scissors to hunt back. One of Red's two pieces is condemned, which a
        // side-wide test would get wrong in both directions.
        let rows = [
            ".........",
            ".........",
            "...S.....",
            ".....p...",
            "....r....",
            ".........",
            "......S..",
            ".P.....R.",
            ".........",
        ];
        let position =
            Position::from_rows(Mode::TotalWar, Color::Red, &rows).expect("position must parse");
        let baseline = evaluate(&position);

        let params = EvalParams {
            total_war_doomed_piece: -1_000,
            ..EvalParams::DEFAULT
        };
        assert_eq!(evaluate_with(&position, &params), baseline - 1_000);

        // Not the whole army, though, so the army-wide term stays silent.
        let params = EvalParams {
            total_war_army_doomed: -1_000,
            ..EvalParams::DEFAULT
        };
        assert_eq!(evaluate_with(&position, &params), baseline);

        let params = EvalParams {
            total_war_two_pieces: -1_000,
            ..EvalParams::DEFAULT
        };
        assert_eq!(evaluate_with(&position, &params), baseline - 1_000);
    }

    #[test]
    fn the_zero_piece_cliff_is_gated_on_the_rule_rather_than_the_mode() {
        // Infiltration leaves a side with no pieces unable to move, which is a
        // stalemate draw and not a loss, so there is no cliff at zero to
        // price. The gate reads `ModeRules`, so this stays true if a future
        // mode changes its mind.
        let rows = [
            ".........",
            ".........",
            "...S.....",
            ".....p...",
            ".........",
            ".........",
            "......S..",
            ".P.....R.",
            ".........",
        ];
        let position =
            Position::from_rows(Mode::Infiltration, Color::Red, &rows).expect("must parse");
        let baseline = evaluate(&position);
        let params = EvalParams {
            infiltration_last_piece: -1_000,
            ..EvalParams::DEFAULT
        };
        assert_eq!(evaluate_with(&position, &params), baseline);

        // Being permanently huntable still matters there, so that term is live.
        let params = EvalParams {
            infiltration_doomed_piece: -1_000,
            ..EvalParams::DEFAULT
        };
        assert_eq!(evaluate_with(&position, &params), baseline - 1_000);
    }

    #[test]
    fn the_new_survival_terms_stay_symmetric_under_mirroring() {
        let position = one_doomed_paper_against_two_immortal_scissors();
        let params = EvalParams {
            total_war_doomed_piece: -37,
            total_war_army_doomed: -211,
            total_war_last_piece: -409,
            total_war_two_pieces: -113,
            total_war_doomed_endgame: -17,
            ..EvalParams::DEFAULT
        };
        assert_eq!(
            evaluate_with(&position, &params),
            evaluate_with(&position.mirrored(), &params),
            "a colour-blind feature must not become colour-dependent"
        );
    }

    /// Two Red Rocks two files apart in open country, with Blue's lone piece
    /// wedged into a corner.
    ///
    /// Each Rock has eight neutral neighbours and the two share three of them,
    /// so sixteen claiming moves land on thirteen distinct tiles. Blue's
    /// corner Scissors has three neighbours and no overlap with anything. The
    /// two terms therefore differ on this position, which is what makes them
    /// separable at all (§4.4).
    fn two_red_rocks_with_overlapping_frontiers() -> Position {
        let rows = [
            "........S",
            ".........",
            ".........",
            ".........",
            "...r.r...",
            ".........",
            ".........",
            ".........",
            ".........",
        ];
        Position::from_rows(Mode::TotalWar, Color::Red, &rows).expect("position must parse")
    }

    #[test]
    fn distinct_claimable_tiles_are_counted_apart_from_claiming_moves() {
        let position = two_red_rocks_with_overlapping_frontiers();
        let baseline = evaluate(&position);

        // Thirteen distinct tiles for Red against three for Blue. Both
        // probes move their weight by exactly -1000 from its shipped value, so
        // the delta reads off as the count itself whatever the defaults are.
        let params = EvalParams {
            total_war_claim_squares: EvalParams::DEFAULT.total_war_claim_squares - 1_000,
            ..EvalParams::DEFAULT
        };
        assert_eq!(evaluate_with(&position, &params), baseline - 10_000);

        // Sixteen claiming moves for Red against three for Blue. The two
        // counts disagree here, which is the whole point of the feature: if
        // they always agreed it would carry no information the frontier term
        // does not already carry.
        let params = EvalParams {
            total_war_frontier: EvalParams::DEFAULT.total_war_frontier - 1_000,
            ..EvalParams::DEFAULT
        };
        assert_eq!(evaluate_with(&position, &params), baseline - 13_000);
    }

    #[test]
    fn crowding_one_tile_does_not_widen_the_frontier() {
        // A Red Rock boxed into the corner by two friendly Rocks. All three
        // can step onto b2, and between them they reach five tiles with seven
        // moves, so the gap between the two counts is exactly the crowding.
        let rows = [
            "r........",
            "rr.......",
            ".........",
            ".........",
            ".........",
            ".........",
            ".........",
            ".........",
            "........S",
        ];
        let position = Position::from_rows(Mode::TotalWar, Color::Red, &rows).expect("must parse");
        let baseline = evaluate(&position);

        let squares = EvalParams {
            total_war_claim_squares: EvalParams::DEFAULT.total_war_claim_squares - 1_000,
            ..EvalParams::DEFAULT
        };
        let moves = EvalParams {
            total_war_frontier: EvalParams::DEFAULT.total_war_frontier - 1_000,
            ..EvalParams::DEFAULT
        };
        assert_ne!(
            evaluate_with(&position, &squares),
            evaluate_with(&position, &moves),
            "crowded pieces must count fewer tiles than moves"
        );
        assert!(evaluate_with(&position, &squares) > evaluate_with(&position, &moves));
        assert_ne!(evaluate_with(&position, &squares), baseline);
    }

    #[test]
    fn the_claim_terms_stay_silent_in_modes_without_territory() {
        // `ModeRules::uses_territory` gates the neutral mask off, so the same
        // shape in Infiltration must leave both counts at zero on both sides.
        let rows = [
            "........S",
            ".........",
            ".........",
            ".........",
            "...r.r...",
            ".........",
            ".........",
            ".........",
            ".........",
        ];
        let position =
            Position::from_rows(Mode::Infiltration, Color::Red, &rows).expect("must parse");
        let baseline = evaluate(&position);
        let params = EvalParams {
            total_war_claim_squares: EvalParams::DEFAULT.total_war_claim_squares - 1_000,
            ..EvalParams::DEFAULT
        };
        assert_eq!(evaluate_with(&position, &params), baseline);
    }

    #[test]
    fn the_claim_square_term_stays_symmetric_under_mirroring() {
        let position = two_red_rocks_with_overlapping_frontiers();
        let params = EvalParams {
            total_war_claim_squares: -71,
            ..EvalParams::DEFAULT
        };
        assert_eq!(
            evaluate_with(&position, &params),
            evaluate_with(&position.mirrored(), &params),
        );
    }

    #[test]
    fn parameters_round_trip_by_name_and_by_vector() {
        assert_eq!(EvalParams::NAMES.len(), EvalParams::COUNT);
        let mut params = EvalParams::DEFAULT;
        for name in EvalParams::NAMES {
            assert!(params.get(name).is_some(), "{name} must be readable");
        }
        assert!(params.set("total_war_material", 1_234));
        assert_eq!(params.get("total_war_material"), Some(1_234));
        assert!(!params.set("not_a_parameter", 1));

        let vector = params.to_vec();
        assert_eq!(vector.len(), EvalParams::COUNT);
        let mut restored = EvalParams::DEFAULT;
        restored.apply_vec(&vector);
        assert_eq!(restored, params);
    }

    #[test]
    fn assignments_reject_unknown_names_and_bad_values() {
        let mut params = EvalParams::DEFAULT;
        params
            .apply_assignments(["total_war_material=300", "# comment", ""])
            .expect("valid assignments must apply");
        assert_eq!(params.total_war_material, 300);
        assert!(params.apply_assignments(["nope=1"]).is_err());
        assert!(params.apply_assignments(["total_war_material=x"]).is_err());
        assert!(params.apply_assignments(["total_war_material"]).is_err());
    }

    #[test]
    fn weights_actually_drive_the_score() {
        let rows = [
            ".........",
            ".........",
            ".........",
            ".........",
            "....r.S..",
            "..r......",
            ".........",
            ".........",
            ".........",
        ];
        let position =
            Position::from_rows(Mode::TotalWar, Color::Red, &rows).expect("position must parse");
        let mut doubled = EvalParams::DEFAULT;
        doubled.total_war_material *= 2;
        assert_ne!(
            evaluate_with(&position, &EvalParams::DEFAULT),
            evaluate_with(&position, &doubled)
        );
    }

    /// A Total War position with a chosen territory split and nothing else
    /// going on, so a probe can vary the split alone.
    ///
    /// Red's Rock sits on a1 and Blue's Paper on i1, both on Red-owned tiles
    /// and far enough apart that neither is attacked, so every piece-derived
    /// feature is identical across the whole family and only the tiles differ.
    fn total_war_territory_split(territory: &str) -> Position {
        Position::from_fen(
            Mode::TotalWar,
            &format!("r7P/9/9/9/9/9/9/9/9 r {territory}"),
        )
        .expect("position must parse")
    }

    /// The bounded race term in isolation, priced against itself.
    ///
    /// Diffing one position against itself with the weight removed cancels
    /// every other feature, so this is comparable *across* positions -- which
    /// the raw evaluation is not, because a different neutral count changes
    /// the frontier and claim-square counts too.
    fn race_term(position: &Position, weight: i32) -> i32 {
        let priced = EvalParams {
            total_war_territory_race: weight,
            ..EvalParams::DEFAULT
        };
        let bare = EvalParams {
            total_war_territory_race: 0,
            ..EvalParams::DEFAULT
        };
        evaluate_with(position, &priced) - evaluate_with(position, &bare)
    }

    fn haste_term(position: &Position, weight: i32) -> i32 {
        let priced = EvalParams {
            total_war_majority_haste: weight,
            ..EvalParams::DEFAULT
        };
        let bare = EvalParams {
            total_war_majority_haste: 0,
            ..EvalParams::DEFAULT
        };
        evaluate_with(position, &priced) - evaluate_with(position, &bare)
    }

    const RED_FORTY: &str =
        "rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrr5/bbbbbbbbb/bbbbbbbbb/bbbbbbbbb/bbb6";
    const RED_FORTY_ONE: &str =
        "rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrrr4/bbbbbbbbb/bbbbbbbbb/bbbbbbbbb/bbb6";
    const RED_FORTY_TWO: &str =
        "rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrrrr3/bbbbbbbbb/bbbbbbbbb/bbbbbbbbb/bbb6";
    const RED_FORTY_FIVE: &str =
        "rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/bbbbbbbbb/bbbbbbbbb/bbbbbbbbb/bbb6";

    #[test]
    fn the_race_term_saturates_exactly_at_the_majority() {
        // The two rule facts the shape rests on, as one assertion each: a lead
        // the size of the remaining claims cannot be caught, and holding
        // forty-one tiles is the same statement.
        let clinched = total_war_territory_split(RED_FORTY_ONE);
        let spare = total_war_territory_split(RED_FORTY_FIVE);
        assert_eq!(clinched.territory_count(Color::Red), 41);
        assert_eq!(spare.territory_count(Color::Red), 45);

        // Saturated at the weight itself, and no further past it, so the four
        // extra tiles are worth exactly nothing.
        assert_eq!(race_term(&clinched, -1_000), -1_000);
        assert_eq!(race_term(&spare, -1_000), -1_000);
    }

    #[test]
    fn the_race_term_is_a_fraction_of_the_weight_while_the_fill_is_open() {
        // Ten tiles against thirty-five with forty-six still unclaimed is a
        // real lead and nowhere near a decided one, so it must price well
        // under the ceiling -- this is the property the flat term lacked.
        let open = total_war_territory_split("rrrrrrrrr/r8/bbbbb4/9/9/9/9/9/9");
        assert_eq!(open.territory_count(Color::Red), 10);
        assert_eq!(open.territory_count(Color::Blue), 5);
        assert_eq!(open.neutral_territory_count(), 66);
        // A five-tile lead against sixty-seven remaining needs is 5/67 of the
        // ceiling, not five times a flat rate.
        assert_eq!(race_term(&open, -6_700), -500);
    }

    #[test]
    fn the_tile_that_decides_the_fill_keeps_its_full_value() {
        // The property that disqualifies a flat "decided" bonus: walking Red
        // from forty to forty-two tiles, no step may pay less than the one
        // before it, or search would decline the move that wins the game.
        let steps = [RED_FORTY, RED_FORTY_ONE, RED_FORTY_TWO, RED_FORTY_FIVE]
            .map(|territory| race_term(&total_war_territory_split(territory), 1_000));
        for pair in steps.windows(2) {
            assert!(
                pair[1] >= pair[0],
                "the race term stepped down, from {} to {}",
                pair[0],
                pair[1]
            );
        }
        assert!(steps[1] > steps[0], "the clinching tile must still pay");
        assert_eq!(steps[3], steps[2], "tiles past the majority pay nothing");
    }

    #[test]
    fn haste_pays_only_the_side_that_has_already_won_the_fill() {
        // Gated on the majority and not on the lead: at forty tiles Red is
        // ahead by ten and gets nothing, because the fill is still open.
        let open = total_war_territory_split(RED_FORTY);
        let clinched = total_war_territory_split(RED_FORTY_ONE);
        assert_eq!(haste_term(&open, -1_000), 0);
        // Seventy-one of the eighty-one tiles are claimed at forty-one and
        // thirty, so the winner is paid for seventy-one of them.
        assert_eq!(clinched.neutral_territory_count(), 10);
        assert_eq!(haste_term(&clinched, -1_000), -71_000);
    }

    #[test]
    fn haste_grows_as_the_board_fills_so_a_won_fill_gets_finished() {
        // Same majority, more of the board claimed: the reward rises, which is
        // what stops the engine sitting on a lead it cannot increase.
        let earlier = haste_term(&total_war_territory_split(RED_FORTY_ONE), 1);
        let later = haste_term(&total_war_territory_split(RED_FORTY_FIVE), 1);
        assert!(later > earlier, "{later} must exceed {earlier}");
    }

    #[test]
    fn the_majority_is_the_count_no_split_of_the_rest_can_beat() {
        // Documents why the search-side proof may phrase the same fact as
        // `ours >= theirs + neutral` instead, and fails loudly if the board
        // ever becomes even, which is when the two stop agreeing.
        assert_eq!(TERRITORY_MAJORITY, 41);
        assert!(TERRITORY_MAJORITY > i32::from(SQUARE_COUNT) - TERRITORY_MAJORITY);
        assert!(i32::from(SQUARE_COUNT) % 2 == 1);
    }

    // ---- Intransitive -----------------------------------------------------

    /// The default weights with every Intransitive term switched off.
    ///
    /// Terms are measured by switching exactly one back on, so that the number
    /// an assertion names is the term's own contribution rather than its
    /// contribution plus whatever the other weights happen to say about the
    /// same board.
    fn intransitive_silent() -> EvalParams {
        let mut params = EvalParams::DEFAULT;
        for name in EvalParams::NAMES {
            if name.starts_with("intransitive_") {
                assert!(params.set(name, 0), "{name} must exist");
            }
        }
        params
    }

    /// A board holding only the listed pieces, with Red to move.
    fn intransitive_board(cells: &[(usize, usize, u8)]) -> Position {
        let mut rows = [[b'.'; 9]; 9];
        for &(x, y, symbol) in cells {
            assert_eq!(rows[y][x], b'.', "two pieces on one square");
            rows[y][x] = symbol;
        }
        let owned: Vec<String> = rows
            .iter()
            .map(|row| String::from_utf8(row.to_vec()).expect("ascii"))
            .collect();
        let borrowed: Vec<&str> = owned.iter().map(String::as_str).collect();
        Position::from_rows(Mode::Intransitive, Color::Red, &borrowed).expect("position must parse")
    }

    #[test]
    fn safe_routes_respect_the_corner_occupants_kind() {
        let open = intransitive_board(&[(2, 2, b'r'), (7, 7, b'R')]);
        assert_eq!(
            super::CornerFeatures::compute(&open, Color::Red, true).paths,
            [7, 0]
        );
        let blocked = intransitive_board(&[(2, 2, b'r'), (0, 0, b'R')]);
        assert_eq!(
            super::CornerFeatures::compute(&blocked, Color::Red, true).paths,
            [0, 0]
        );
        let capturable = intransitive_board(&[(2, 2, b'r'), (0, 0, b'S')]);
        assert_eq!(
            super::CornerFeatures::compute(&capturable, Color::Red, true).paths,
            [7, 0]
        );
    }

    #[test]
    fn safe_routes_allow_a_winning_arrival_on_an_attacked_goal() {
        let position = intransitive_board(&[(1, 1, b'r'), (0, 0, b'S'), (0, 1, b'P')]);
        assert_eq!(
            super::CornerFeatures::compute(&position, Color::Red, true).paths,
            [8, 0]
        );
    }

    #[test]
    fn safe_routes_do_not_walk_through_friendly_pieces_or_count_one_twice() {
        let position = intransitive_board(&[(2, 2, b'r'), (1, 1, b's'), (7, 7, b'R')]);
        // Scissors is one step away. Rock needs a three-step detour around it.
        assert_eq!(
            super::CornerFeatures::compute(&position, Color::Red, true).paths,
            [8, 6]
        );
    }

    #[test]
    fn local_matchups_distinguish_support_from_same_kind_company() {
        let supported = intransitive_board(&[(4, 4, b'r'), (3, 4, b's'), (5, 4, b'P')]);
        let unsupported = intransitive_board(&[(4, 4, b'r'), (3, 4, b'r'), (5, 4, b'P')]);
        assert_eq!(
            super::CornerFeatures::compute(&supported, Color::Red, false).support,
            1
        );
        assert_eq!(
            super::CornerFeatures::compute(&unsupported, Color::Red, false).support,
            0
        );
    }

    #[test]
    fn a_corner_race_is_measured_diagonally_rather_than_by_rank() {
        // The mode's central positional fact, and the one an Infiltration-shaped
        // evaluation gets wrong: Red's goal is the a1 corner, not the whole of
        // rank 1, so crossing ranks up the i-file buys nothing at all.
        //
        // One Red Scissors alone on the board, and one weight switched on, so
        // the number is the race term and nothing else.
        let mut params = intransitive_silent();
        params.intransitive_closest = 100;
        let progress =
            |x: usize, y: usize| evaluate_with(&intransitive_board(&[(x, y, b's')]), &params);

        assert_eq!(progress(0, 0), 800, "a piece on the goal corner is home");
        assert_eq!(progress(4, 4), 400, "the centre is four moves away");
        assert_eq!(
            progress(1, 1),
            700,
            "and one diagonal step short is one move"
        );

        // Both of these have crossed eight ranks toward where Infiltration's
        // goal would be. Neither has made any progress toward a corner, and a
        // rank-based term would score the first of them a full win.
        assert_eq!(
            progress(8, 0),
            0,
            "eight ranks up the wrong file is nothing"
        );
        assert_eq!(
            progress(0, 8),
            0,
            "and the right file eight ranks back is too"
        );

        // The same statement without the weights: distance is Chebyshev, so a
        // piece that must still cross files cannot be nearer than that count.
        for y in 0..9 {
            assert_eq!(
                Goal::EnemyCorner.distance(Color::Red, y * 9 + 8),
                8,
                "every square on the i-file is eight moves from a1"
            );
        }
    }

    #[test]
    fn the_corner_terms_belong_to_the_side_defending_that_corner() {
        // Blue's Scissors sits on a1, the corner *Red* is running at, so every
        // corner term must pay Blue. Scored from Red to move, so Blue's gains
        // come back negative.
        let position = intransitive_board(&[(0, 0, b'S'), (4, 8, b's')]);
        let silent = intransitive_silent();
        let baseline = evaluate_with(&position, &silent);

        let mut params = silent;
        params.intransitive_corner_plug = 1_000;
        assert_eq!(evaluate_with(&position, &params), baseline - 1_000);

        // Red owns no Rock, so nothing on the board can ever take that
        // Scissors: the plug is permanent and the seal fires as well.
        let mut params = silent;
        params.intransitive_corner_seal = 1_000;
        assert_eq!(evaluate_with(&position, &params), baseline - 1_000);

        // The occupied corner is inside its own shell, so the guard counts it.
        let mut params = silent;
        params.intransitive_corner_guard = 1_000;
        assert_eq!(evaluate_with(&position, &params), baseline - 1_000);

        // And a piece of Red's sitting in the same shell is defending nothing:
        // that is Red's *goal*, not the corner Red has to hold.
        let attacker = intransitive_board(&[(0, 0, b'S'), (1, 1, b's')]);
        let mut params = silent;
        params.intransitive_corner_guard = 1_000;
        assert_eq!(
            evaluate_with(&attacker, &params),
            evaluate_with(&attacker, &silent) - 1_000,
            "the guard term must not pay a piece besieging the enemy corner"
        );
    }

    #[test]
    fn a_plug_only_seals_while_nothing_on_the_board_can_take_it() {
        // The same Blue Scissors on a1, but now Red owns a Rock, which is the
        // one kind that captures Scissors. The corner is still plugged and no
        // longer sealed -- Red has to walk that Rock over, which is a race
        // rather than a wall.
        let position = intransitive_board(&[(0, 0, b'S'), (4, 4, b'r')]);
        let silent = intransitive_silent();

        let mut params = silent;
        params.intransitive_corner_plug = 1_000;
        assert_eq!(
            evaluate_with(&position, &params),
            evaluate_with(&position, &silent) - 1_000,
            "the corner is still occupied, so the plug fires"
        );

        let mut params = silent;
        params.intransitive_corner_seal = 1_000;
        assert_eq!(
            evaluate_with(&position, &params),
            evaluate_with(&position, &silent),
            "the seal must stay silent while a predator is still on the board"
        );
    }

    #[test]
    fn the_corner_terms_stay_out_of_the_modes_without_a_corner() {
        // `Goal::shell` is zero for a rank goal and for no goal at all, so
        // these three weights are inert outside Intransitive. This is what
        // stops the tuner learning them in a mode where they mean nothing.
        for mode in [Mode::TotalWar, Mode::Infiltration] {
            let rows = [
                "........S",
                ".........",
                ".........",
                ".........",
                ".........",
                ".........",
                ".........",
                ".........",
                "....s....",
            ];
            let position =
                Position::from_rows(mode, Color::Red, &rows).expect("position must parse");
            let baseline = evaluate(&position);

            let mut params = EvalParams::DEFAULT;
            params.intransitive_corner_guard = 1_000;
            params.intransitive_corner_plug = 1_000;
            params.intransitive_corner_seal = 1_000;
            assert_eq!(
                evaluate_with(&position, &params),
                baseline,
                "{mode} read an Intransitive weight"
            );
        }
    }

    #[test]
    fn the_opening_is_level() {
        // The wedges are a half turn of each other, so the mode opens dead
        // level, and no first move is anywhere near either corner.
        let start = Position::starting(Mode::Intransitive);
        assert_eq!(evaluate(&start), 0);
        assert!(start.outcome().is_none());
        for movement in start.legal_moves().iter() {
            assert!(
                !start.is_goal_move(*movement),
                "{} reaches a corner from the opening",
                movement.notation()
            );
        }
    }

    #[test]
    fn geometric_advancement_measures_progress_toward_the_corner() {
        // Test the retained geometric feature explicitly. A measured route
        // evaluator may prefer sideways development that opens another
        // runner's path, so this is not a constraint on the default policy.
        let params = EvalParams {
            intransitive_advancement: 7,
            intransitive_closest: 13,
            intransitive_safe_path: 0,
            intransitive_second_path: 0,
            intransitive_support: 0,
            ..EvalParams::DEFAULT
        };
        let start = Position::starting(Mode::Intransitive);
        let mover = start.side_to_move();
        let best = start
            .legal_moves()
            .iter()
            .map(|&movement| {
                let mut after = start;
                after.make_move(movement).expect("generated move is legal");
                (-evaluate_with(&after, &params), movement)
            })
            .max_by_key(|(score, _)| *score)
            .expect("the opening has moves")
            .1;

        let goal = Goal::EnemyCorner;
        let before = goal.distance(mover, usize::from(best.from().index()));
        let after = goal.distance(mover, usize::from(best.to().index()));
        assert!(
            after < before,
            "best opening move {} does not approach the corner",
            best.notation()
        );
        assert_ne!(
            best.from().x(),
            best.to().x(),
            "{} crosses a rank without crossing a file, which is Infiltration's idea of progress",
            best.notation()
        );
        assert_ne!(best.from().y(), best.to().y());
        assert_eq!(
            start.piece_at(best.from()).expect("a piece moves").kind,
            PieceKind::Scissors,
            "the piece that leads is the one the wedge banks in front"
        );
    }
}
