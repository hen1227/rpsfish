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

use crate::board::{BLUE_TARGET_MASK, NEIGHBORS, RED_TARGET_MASK};
use crate::model::{Color, Mode, ModeRules, PieceKind};
use crate::position::Position;
use crate::{BOARD_SIZE, SQUARE_COUNT};

/// How many distinct predator counts get their own weight before saturating.
///
/// Index 0 is "one enemy predator left", index 2 is "three or more". Zero
/// predators is immortality and is scored by its own terms.
pub const SCARCITY_BUCKETS: usize = 3;

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
    // ---- Annihilation -----------------------------------------------------
    /// Centipawn-equivalent value of one extra piece.
    annihilation_material = 220,
    annihilation_mobility = 2,
    annihilation_capture = 24,
    /// Per piece kind that currently has something it can eat.
    annihilation_coverage = 30,
    /// Proximity of attackers to the pieces they prey on.
    annihilation_pressure = 3,
    /// Per piece that can never be captured again.
    annihilation_immortal_piece = 70,
    /// Extra per immortal piece that still has live prey, because such a piece
    /// wins material for free rather than only surviving.
    annihilation_immortal_with_prey = 110,
    /// Flat bonus for owning at least one immortal piece.
    annihilation_immortal_survivor = 160,
    /// Per piece bucketed by how many enemy predators it still faces: one,
    /// two, or three or more.
    ///
    /// These give search a gradient toward extinction instead of a cliff at
    /// exactly zero predators. Annihilation gives each side at most one piece
    /// per kind, so predator counts here are only ever zero or one and the
    /// feature is close to inert: a 2000-game match moved it exactly zero Elo.
    /// Left at zero rather than guessed.
    annihilation_scarcity_1 = 0,
    annihilation_scarcity_2 = 0,
    annihilation_scarcity_3 = 0,

    // ---- Total War --------------------------------------------------------
    total_war_territory = 28,
    total_war_material = 70,
    total_war_mobility = 1,
    total_war_capture = 16,
    /// Per legal move onto a still-neutral tile.
    total_war_frontier = 8,
    /// Scales a territory lead by how little neutral ground is left. Applied
    /// as `lead_sign * claimed * weight / 96`, so 16 is exactly one sixth.
    total_war_lead_scale = 16,
    total_war_immortal_piece = 40,
    total_war_immortal_with_prey = 40,
    total_war_immortal_survivor = 0,
    /// Predator-scarcity buckets; see the Annihilation entries above.
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
    infiltration_immortal_piece = 60,
    infiltration_immortal_with_prey = 30,
    infiltration_immortal_survivor = 0,
    /// Predator-scarcity buckets; see the Annihilation entries above. A
    /// 30-iteration SPSA run drifted only to -1/4/-2, which is indistinguishable
    /// from zero, so these stay at zero until a longer run says otherwise.
    infiltration_scarcity_1 = 0,
    infiltration_scarcity_2 = 0,
    infiltration_scarcity_3 = 0,
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
        Mode::Annihilation => annihilation_score(position, params, &red, &blue),
        Mode::TotalWar => total_war_score(position, params, &red, &blue),
        Mode::Infiltration => infiltration_score(params, &red, &blue),
    };
    if position.side_to_move() == Color::Red {
        red_score
    } else {
        -red_score
    }
}

fn annihilation_score(
    position: &Position,
    params: &EvalParams,
    red: &SideSummary,
    blue: &SideSummary,
) -> i32 {
    let pressure =
        favorable_distance(position, Color::Red) - favorable_distance(position, Color::Blue);
    delta(red.pieces, blue.pieces, params.annihilation_material)
        + delta(red.mobility, blue.mobility, params.annihilation_mobility)
        + delta(red.captures, blue.captures, params.annihilation_capture)
        + delta(
            red.kinds_with_prey,
            blue.kinds_with_prey,
            params.annihilation_coverage,
        )
        + pressure * params.annihilation_pressure
        + survival_delta(
            red,
            blue,
            params.annihilation_immortal_piece,
            params.annihilation_immortal_with_prey,
            params.annihilation_immortal_survivor,
            [
                params.annihilation_scarcity_1,
                params.annihilation_scarcity_2,
                params.annihilation_scarcity_3,
            ],
        )
}

fn total_war_score(
    position: &Position,
    params: &EvalParams,
    red: &SideSummary,
    blue: &SideSummary,
) -> i32 {
    let territory = i32_from_u32(position.territory_count(Color::Red))
        - i32_from_u32(position.territory_count(Color::Blue));
    let claimed = i32::from(SQUARE_COUNT) - i32_from_u32(position.neutral_territory_count());
    let lead_scaling = territory.signum() * claimed * params.total_war_lead_scale / 96;
    territory * params.total_war_territory
        + delta(red.pieces, blue.pieces, params.total_war_material)
        + delta(red.mobility, blue.mobility, params.total_war_mobility)
        + delta(red.captures, blue.captures, params.total_war_capture)
        + delta(red.claimable, blue.claimable, params.total_war_frontier)
        + lead_scaling
        + survival_delta(
            red,
            blue,
            params.total_war_immortal_piece,
            params.total_war_immortal_with_prey,
            params.total_war_immortal_survivor,
            [
                params.total_war_scarcity_1,
                params.total_war_scarcity_2,
                params.total_war_scarcity_3,
            ],
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
            params.infiltration_immortal_piece,
            params.infiltration_immortal_with_prey,
            params.infiltration_immortal_survivor,
            [
                params.infiltration_scarcity_1,
                params.infiltration_scarcity_2,
                params.infiltration_scarcity_3,
            ],
        )
}

const fn delta(red: i32, blue: i32, weight: i32) -> i32 {
    (red - blue) * weight
}

/// Score both sides' permanence of material.
///
/// Immortality is irreversible, so unlike mobility or distance it describes a
/// property the rest of the game keeps. The scarcity buckets extend that into
/// a gradient: a kind whose last predator is still alive is one capture from
/// becoming permanent, which a boolean feature cannot express.
fn survival_delta(
    red: &SideSummary,
    blue: &SideSummary,
    per_piece: i32,
    with_prey: i32,
    survivor: i32,
    scarcity: [i32; SCARCITY_BUCKETS],
) -> i32 {
    survival_score(red, per_piece, with_prey, survivor, scarcity)
        - survival_score(blue, per_piece, with_prey, survivor, scarcity)
}

fn survival_score(
    side: &SideSummary,
    per_piece: i32,
    with_prey: i32,
    survivor: i32,
    scarcity: [i32; SCARCITY_BUCKETS],
) -> i32 {
    let mut result = side.immortal_pieces * per_piece + side.immortal_with_prey * with_prey;
    if side.immortal_pieces > 0 {
        result += survivor;
    }
    for (bucket, weight) in scarcity.into_iter().enumerate() {
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
    goal_moves: i32,
    advancement: i32,
    closest: i32,
    kinds_with_prey: i32,
    immortal_pieces: i32,
    immortal_with_prey: i32,
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
        let target = if rules.uses_boundary_goal {
            target_mask(color)
        } else {
            0
        };

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
            }

            let allowed = empty | prey;
            let mut sources = mine;
            while sources != 0 {
                let index = sources.trailing_zeros() as usize;
                let destinations = NEIGHBORS[index] & allowed;
                summary.mobility += i32_from_u32(destinations.count_ones());
                summary.captures += i32_from_u32((destinations & prey).count_ones());
                if neutral != 0 {
                    summary.claimable += i32_from_u32((destinations & neutral).count_ones());
                }
                if target != 0 {
                    summary.goal_moves += i32_from_u32((destinations & target).count_ones());
                    let progress = progress_toward_goal(color, index);
                    summary.advancement += progress;
                    summary.closest = summary.closest.max(progress);
                }
                sources &= sources - 1;
            }
        }
        summary
    }
}

const fn target_mask(color: Color) -> u128 {
    match color {
        Color::Red => RED_TARGET_MASK,
        Color::Blue => BLUE_TARGET_MASK,
    }
}

/// How far a piece at `index` has advanced toward its own target row.
const fn progress_toward_goal(color: Color, index: usize) -> i32 {
    let y = (index / BOARD_SIZE as usize) as i32;
    match color {
        Color::Red => BOARD_SIZE as i32 - 1 - y,
        Color::Blue => y,
    }
}

/// Total closeness of each attacker to the nearest piece it preys on.
///
/// Only Annihilation uses this, and it is the one feature whose cost is not a
/// popcount, so it stays out of [`SideSummary`].
fn favorable_distance(position: &Position, color: Color) -> i32 {
    let mut result = 0_i32;
    for attacker_kind in PieceKind::ALL {
        let defenders = position.piece_bitboard(color.other(), attacker_kind.prey());
        if defenders == 0 {
            continue;
        }
        let mut attackers = position.piece_bitboard(color, attacker_kind);
        while attackers != 0 {
            let attacker = attackers.trailing_zeros() as usize;
            let attacker_x = (attacker % BOARD_SIZE as usize) as i32;
            let attacker_y = (attacker / BOARD_SIZE as usize) as i32;
            let mut nearest = i32::MAX;
            let mut remaining = defenders;
            while remaining != 0 {
                let defender = remaining.trailing_zeros() as usize;
                let distance = ((defender % BOARD_SIZE as usize) as i32 - attacker_x)
                    .abs()
                    .max(((defender / BOARD_SIZE as usize) as i32 - attacker_y).abs());
                nearest = nearest.min(distance);
                remaining &= remaining - 1;
            }
            result += i32::from(BOARD_SIZE) - nearest;
            attackers &= attackers - 1;
        }
    }
    result
}

fn i32_from_u32(value: u32) -> i32 {
    i32::try_from(value).expect("board-derived count must fit in i32")
}

#[cfg(test)]
mod tests {
    use super::{EvalParams, evaluate, evaluate_with};
    use crate::model::{Color, Mode};
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

    #[test]
    fn parameters_round_trip_by_name_and_by_vector() {
        assert_eq!(EvalParams::NAMES.len(), EvalParams::COUNT);
        let mut params = EvalParams::DEFAULT;
        for name in EvalParams::NAMES {
            assert!(params.get(name).is_some(), "{name} must be readable");
        }
        assert!(params.set("annihilation_material", 1_234));
        assert_eq!(params.get("annihilation_material"), Some(1_234));
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
            .apply_assignments(["annihilation_material=300", "# comment", ""])
            .expect("valid assignments must apply");
        assert_eq!(params.annihilation_material, 300);
        assert!(params.apply_assignments(["nope=1"]).is_err());
        assert!(
            params
                .apply_assignments(["annihilation_material=x"])
                .is_err()
        );
        assert!(params.apply_assignments(["annihilation_material"]).is_err());
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
        let position = Position::from_rows(Mode::Annihilation, Color::Red, &rows)
            .expect("position must parse");
        let mut doubled = EvalParams::DEFAULT;
        doubled.annihilation_material *= 2;
        assert_ne!(
            evaluate_with(&position, &EvalParams::DEFAULT),
            evaluate_with(&position, &doubled)
        );
    }
}
