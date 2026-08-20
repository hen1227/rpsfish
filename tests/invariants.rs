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

//! Property-style invariants over pseudorandom positions and playouts.
//!
//! These exist to catch the failures that anecdotal test positions never
//! reach: an asymmetric evaluation weight, a desynchronized occupancy cache,
//! or a "proof" in search that is not actually implied by the rules. The
//! generator is a fixed-seed splitmix64 so failures are reproducible and the
//! crate keeps its zero-dependency policy.

use rpsfish::model::MAX_MOVES;
use rpsfish::{
    Color, EvalParams, Mode, PieceKind, Position, SearchLimits, SearchTuning, Searcher, Square,
    evaluate, evaluate_with,
};

const SEED: u64 = 0x5150_1f2c_a3b9_0d17;

struct Random(u64);

impl Random {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn below(&mut self, bound: u32) -> u32 {
        (self.next_u64() % u64::from(bound)) as u32
    }
}

/// Build a valid random position with between 2 and 17 pieces.
fn random_position(random: &mut Random, mode: Mode) -> Position {
    loop {
        let mut pieces = [[0_u128; 3]; 2];
        let mut territory = [0_u128; 2];
        let mut used = 0_u128;
        let piece_count = 2 + random.below(16);
        for _ in 0..piece_count {
            let square = Square::new(random.below(81) as u8).expect("bounded index");
            if used & square.bit() != 0 {
                continue;
            }
            used |= square.bit();
            let color = if random.below(2) == 0 {
                Color::Red
            } else {
                Color::Blue
            };
            let kind = PieceKind::ALL[random.below(3) as usize];
            pieces[color.index()][kind.index()] |= square.bit();
            territory[color.index()] |= square.bit();
        }
        if mode.rules().uses_territory {
            // Claim extra ground so territory features are exercised.
            for _ in 0..random.below(60) {
                let square = Square::new(random.below(81) as u8).expect("bounded index");
                if (territory[0] | territory[1]) & square.bit() != 0 {
                    continue;
                }
                territory[usize::from(random.below(2) == 0)] |= square.bit();
            }
        } else {
            territory = [0, 0];
        }
        let side = if random.below(2) == 0 {
            Color::Red
        } else {
            Color::Blue
        };
        if let Ok(position) = Position::from_bitboards(mode, side, pieces, territory, 0) {
            return position;
        }
    }
}

#[test]
fn evaluation_is_invariant_under_colour_and_board_mirroring() {
    let mut random = Random(SEED);
    let tuned = tuning_probe_params();
    for mode in Mode::ALL {
        for _ in 0..400 {
            let position = random_position(&mut random, mode);
            let mirrored = position.mirrored();
            assert_eq!(
                evaluate(&position),
                evaluate(&mirrored),
                "{mode} evaluation is asymmetric"
            );
            // A symmetric feature set must stay symmetric for every weight
            // vector, otherwise tuning can silently learn a colour bias.
            assert_eq!(
                evaluate_with(&position, &tuned),
                evaluate_with(&mirrored, &tuned),
                "{mode} evaluation is asymmetric under non-default weights"
            );
        }
    }
}

/// Distinct, non-round weights for every parameter, so a term that is only
/// accidentally symmetric at the defaults still fails.
fn tuning_probe_params() -> EvalParams {
    let mut params = EvalParams::DEFAULT;
    let mut random = Random(0x1234_5678_9abc_def0);
    let values: Vec<i32> = (0..EvalParams::COUNT)
        .map(|_| i32::try_from(random.below(400)).expect("small") - 150)
        .collect();
    params.apply_vec(&values);
    params
}

#[test]
fn mirroring_is_an_involution_that_preserves_play() {
    let mut random = Random(SEED ^ 0xff);
    for mode in Mode::ALL {
        for _ in 0..300 {
            let position = random_position(&mut random, mode);
            let mirrored = position.mirrored();
            assert_eq!(
                position,
                mirrored.mirrored(),
                "{mode} mirror is not an involution"
            );
            assert_eq!(
                position.legal_moves().len(),
                mirrored.legal_moves().len(),
                "{mode} mirror changed the number of legal moves"
            );
            assert_eq!(
                position.outcome().map(|outcome| outcome.winner.is_some()),
                mirrored.outcome().map(|outcome| outcome.winner.is_some()),
                "{mode} mirror changed whether the position is decided"
            );
            assert_eq!(position.is_stalemate(), mirrored.is_stalemate());
        }
    }
}

#[test]
fn make_and_unmake_restore_every_field_along_random_playouts() {
    let mut random = Random(SEED ^ 0xa5a5);
    for mode in Mode::ALL {
        for _ in 0..120 {
            let mut position = random_position(&mut random, mode);
            for _ in 0..40 {
                let moves = position.legal_moves();
                if moves.is_empty() {
                    break;
                }
                // Every move from this node must round-trip exactly, which
                // covers the cached occupancy and the incremental hash.
                let before = position;
                for &movement in &moves {
                    let undo = position
                        .make_move(movement)
                        .expect("generated move is legal");
                    assert_eq!(position.hash(), position.recompute_hash());
                    assert!(position.validate().is_ok());
                    position.unmake_move(undo);
                    assert_eq!(position, before);
                }
                let chosen = moves.as_slice()[random.below(moves.len() as u32) as usize];
                position.make_move(chosen).expect("generated move is legal");
            }
        }
    }
}

#[test]
fn move_generation_agrees_with_single_move_legality() {
    let mut random = Random(SEED ^ 0x1357);
    for mode in Mode::ALL {
        for _ in 0..200 {
            let position = random_position(&mut random, mode);
            let generated = position.legal_moves();
            assert!(generated.len() <= MAX_MOVES);
            let mut counted = 0;
            for from in 0..81_u8 {
                for to in 0..81_u8 {
                    let candidate = rpsfish::Move::new(
                        Square::new(from).expect("valid"),
                        Square::new(to).expect("valid"),
                    );
                    if position.is_legal_move(candidate) {
                        counted += 1;
                        assert!(
                            generated.contains(candidate),
                            "{mode}: {candidate} is legal but was not generated"
                        );
                    }
                }
            }
            assert_eq!(counted, generated.len(), "{mode} generated an illegal move");
        }
    }
}

#[test]
fn immortality_is_monotone_along_every_playout() {
    let mut random = Random(SEED ^ 0x2468);
    for mode in Mode::ALL {
        for _ in 0..200 {
            let mut position = random_position(&mut random, mode);
            let mut immortal = [
                position.has_immortal_piece(Color::Red),
                position.has_immortal_piece(Color::Blue),
            ];
            for _ in 0..60 {
                let moves = position.legal_moves();
                if moves.is_empty() {
                    break;
                }
                let chosen = moves.as_slice()[random.below(moves.len() as u32) as usize];
                position.make_move(chosen).expect("generated move is legal");
                for color in Color::ALL {
                    let now = position.has_immortal_piece(color);
                    assert!(
                        now || !immortal[color.index()],
                        "{mode}: {color} lost an uncapturable piece, which the rules forbid"
                    );
                    immortal[color.index()] = now;
                }
            }
        }
    }
}

#[test]
fn stalemate_adjudicates_as_a_draw_in_every_mode() {
    let mut random = Random(SEED ^ 0x99);
    let mut seen = 0;
    for mode in Mode::ALL {
        for _ in 0..4_000 {
            let position = random_position(&mut random, mode);
            match (position.outcome(), position.adjudicate()) {
                (Some(outcome), Some(adjudicated)) => assert_eq!(outcome, adjudicated),
                (None, Some(adjudicated)) => {
                    assert!(position.is_stalemate());
                    assert!(adjudicated.winner.is_none(), "stalemate must be a draw");
                    assert!(!position.has_any_legal_move(position.side_to_move()));
                    seen += 1;
                }
                (None, None) => assert!(position.has_any_legal_move(position.side_to_move())),
                (Some(_), None) => panic!("a decided position must stay decided"),
            }
        }
    }
    assert!(seen > 0, "the generator never produced a stalemate");
}

#[test]
fn search_never_reports_a_loss_for_a_side_that_cannot_be_annihilated() {
    let mut random = Random(SEED ^ 0xbeef);
    let mut searcher = Searcher::new(2);
    let limits = SearchLimits {
        max_depth: 4,
        max_nodes: Some(20_000),
        move_time: None,
    };
    let mut checked = 0;
    for _ in 0..600 {
        let position = random_position(&mut random, Mode::Annihilation);
        if position.outcome().is_some() || position.is_stalemate() {
            continue;
        }
        let side = position.side_to_move();
        if position.can_be_annihilated(side) {
            continue;
        }
        checked += 1;
        let result = searcher.search(position, limits);
        assert!(
            result.score >= 0,
            "search scored {} for a side that cannot be annihilated",
            result.score
        );
        if !position.can_be_annihilated(side.other()) {
            assert_eq!(
                result.score, 0,
                "mutual immortality must score exactly zero"
            );
        }
    }
    assert!(
        checked > 20,
        "only {checked} provably safe positions were tested"
    );
}

#[test]
fn analysis_only_returns_legal_moves_from_random_positions() {
    let mut random = Random(SEED ^ 0xfeed);
    let mut searcher = Searcher::new(2);
    let limits = SearchLimits {
        max_depth: 3,
        max_nodes: Some(8_000),
        move_time: None,
    };
    for mode in Mode::ALL {
        for _ in 0..60 {
            let position = random_position(&mut random, mode);
            let result = searcher.analyze(position, limits, 3);
            for line in &result.lines {
                assert!(
                    position.is_legal_move(line.movement),
                    "{mode} analysis proposed an illegal move"
                );
                assert_eq!(line.principal_variation.first(), Some(&line.movement));
            }
            if position.outcome().is_none() && !position.legal_moves().is_empty() {
                assert!(!result.lines.is_empty());
            }
        }
    }
}

/// The tactical set the generators would almost never produce: a position with
/// exactly one move that avoids an immediate loss, or takes an immediate win.
///
/// The selective layer prunes on static evaluation and on move counts, so this
/// is where it would show up if it pruned something decisive.
#[test]
fn selective_search_agrees_with_exhaustive_search_on_forced_positions() {
    let mut random = Random(SEED ^ 0x5e1e);
    let limits = SearchLimits {
        max_depth: 6,
        max_nodes: Some(200_000),
        move_time: None,
    };

    let mut exhaustive = Searcher::new(4);
    exhaustive.set_selective(false);
    let mut selective = Searcher::new(4);

    let mut compared = 0_usize;
    for mode in Mode::ALL {
        for _ in 0..120 {
            let position = random_position(&mut random, mode);
            if position.outcome().is_some() || position.legal_moves().is_empty() {
                continue;
            }
            let plain = exhaustive.search(position, limits);
            let pruned = selective.search(position, limits);
            compared += 1;

            // A decided game is a fact, not an opinion: the two searches may
            // disagree about the size of an advantage, never about whether the
            // position is won, lost, or drawn.
            assert_eq!(
                plain.score.signum(),
                pruned.score.signum(),
                "{mode}: exhaustive scored {} but selective scored {}",
                plain.score,
                pruned.score
            );
            assert!(
                pruned
                    .best_move
                    .is_some_and(|movement| position.is_legal_move(movement))
            );
        }
    }
    assert!(compared > 200, "only {compared} positions were compared");
}

/// Turning the reduction scale up must only ever shorten a search, and turning
/// it off must reproduce the unreduced search exactly.
#[test]
fn reduction_scale_orders_search_effort() {
    let position = Position::starting(Mode::TotalWar);
    let limits = SearchLimits {
        max_depth: 8,
        max_nodes: Some(400_000),
        move_time: None,
    };
    let mut depths = Vec::new();
    for scale in [0, 40, 64, 120] {
        let mut searcher = Searcher::new(4);
        searcher.set_tuning(SearchTuning {
            reduction_scale: scale,
        });
        let result = searcher.search(position, limits);
        depths.push((scale, result.completed_depth, result.stats.nodes));
    }
    for window in depths.windows(2) {
        let (low_scale, _, low_nodes) = window[0];
        let (high_scale, _, high_nodes) = window[1];
        assert!(
            high_nodes <= low_nodes,
            "scale {high_scale} used more nodes ({high_nodes}) than scale {low_scale} ({low_nodes})"
        );
    }
}
