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
    Color, EvalParams, Mode, Move, PieceKind, Position, SearchLimits, SearchTuning, Searcher,
    Square, Transform, evaluate, evaluate_with, perft,
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

/// The simplest member of a mode's symmetry group that exchanges the two sides.
///
/// Where the goal is a rank or absent that is [`Transform::COLORS`], a
/// vertical flip and a colour swap. Where the goal is a corner the vertical
/// flip alone lands on the wrong file, `COLORS` is not in the group at all,
/// and the answer is the half turn. Lowest code wins, so a mode holding the
/// whole group still gets the plain mirror rather than the half turn.
///
/// Tests that mean "swap the sides over" ask for this rather than calling
/// `mirrored`, which is only one of the two things a corner mode needs.
fn colour_swap(mode: Mode) -> Transform {
    mode.rules()
        .symmetries()
        .iter()
        .copied()
        .filter(|transform| transform.swaps_colors())
        .min_by_key(|transform| transform.code())
        .unwrap_or_else(|| panic!("{mode} has no colour-swapping symmetry"))
}

#[test]
fn evaluation_is_invariant_under_colour_and_board_mirroring() {
    let mut random = Random(SEED);
    let tuned = tuning_probe_params();
    for mode in Mode::ALL {
        let swap = colour_swap(mode);
        for _ in 0..400 {
            let position = random_position(&mut random, mode);
            let mirrored = position.transformed(swap);
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
        let swap = colour_swap(mode);
        for _ in 0..300 {
            let position = random_position(&mut random, mode);
            let mirrored = position.transformed(swap);
            assert_eq!(
                position,
                mirrored.transformed(swap),
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
            for color in Color::ALL {
                let legal = position.legal_moves_for(color);
                let goal = mode.rules().goal.mask(color);
                let expected = legal.iter().any(|movement| movement.to().bit() & goal != 0);
                let actual = position.goal_move_for(color);
                assert_eq!(actual.is_some(), expected, "{mode} goal-move query");
                if let Some(movement) = actual {
                    assert!(legal.contains(movement));
                    assert_ne!(movement.to().bit() & goal, 0);
                }
            }
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

/// Every mode ends the game where the side to move cannot play; the mode
/// decides whose result it is. Checked over random positions in all three, so a
/// mode whose rule table said one thing and whose adjudication did another
/// would be caught wherever the generator happened to produce a blockade.
#[test]
fn a_no_move_position_is_adjudicated_by_its_own_modes_rule() {
    let mut random = Random(SEED ^ 0x99);
    let mut seen = [0_usize; 3];
    for (index, mode) in Mode::ALL.into_iter().enumerate() {
        let loses = mode.rules().stalemate_loses;
        for _ in 0..4_000 {
            let position = random_position(&mut random, mode);
            match (position.outcome(), position.adjudicate()) {
                (Some(outcome), Some(adjudicated)) => assert_eq!(outcome, adjudicated),
                (None, Some(adjudicated)) => {
                    assert!(position.is_stalemate());
                    assert!(!position.has_any_legal_move(position.side_to_move()));
                    let expected = loses.then_some(position.side_to_move().other());
                    assert_eq!(
                        adjudicated.winner, expected,
                        "{mode} scored a no-move position wrongly"
                    );
                    seen[index] += 1;
                }
                (None, None) => assert!(position.has_any_legal_move(position.side_to_move())),
                (Some(_), None) => panic!("a decided position must stay decided"),
            }
        }
    }
    assert!(
        seen.iter().sum::<usize>() > 0,
        "the generator never produced a no-move position"
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
        for _ in 0..180 {
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
            //
            // "Decided" has to be said in the mode's own unit of account. A
            // score inside one piece is the evaluation declining to call the
            // position, and two searches that walk different trees will land
            // on opposite sides of zero there for reasons that are arithmetic
            // rather than tactical -- one position in this sweep scores +1
            // against -1. Reading a signum off that is reading noise. Above a
            // piece the claim has teeth, and that is the range where dropping
            // a forced line would show up.
            //
            // Stated as a three-way verdict rather than a signum, because
            // "decided" has to be allowed to be *neither*. Comparing signs the
            // moment one of the two searches is confident asks the other to
            // have found the same thing at the same depth, which is not the
            // claim: a selective search legitimately needs another ply to see
            // a line an exhaustive one already has, and reports the evaluation
            // meanwhile. What is never legitimate is the two of them pointing
            // opposite ways, and that is what this compares.
            let undecided = EvalParams::DEFAULT.material(mode);
            let verdict = |score: i32| {
                if score >= undecided {
                    1
                } else if score <= -undecided {
                    -1
                } else {
                    0
                }
            };
            assert!(
                verdict(plain.score) + verdict(pruned.score) != 0 || verdict(plain.score) == 0,
                "{mode} {}: exhaustive scored {} but selective scored {}",
                position.to_fen(),
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

#[test]
fn the_symmetry_group_preserves_play_and_evaluation() {
    let mut random = Random(SEED ^ 0x5a5a);
    let tuned = tuning_probe_params();
    for mode in Mode::ALL {
        for _ in 0..300 {
            let position = random_position(&mut random, mode);
            for &transform in mode.rules().symmetries() {
                let image = position.transformed(transform);
                assert_eq!(
                    position,
                    image.transformed(transform),
                    "{mode} transform {} is not an involution",
                    transform.code()
                );
                assert_eq!(
                    evaluate(&position),
                    evaluate(&image),
                    "{mode} evaluation is not invariant under transform {}",
                    transform.code()
                );
                assert_eq!(
                    evaluate_with(&position, &tuned),
                    evaluate_with(&image, &tuned),
                    "{mode} evaluation is asymmetric under transform {} at non-default weights",
                    transform.code()
                );
                assert_eq!(
                    position.outcome().map(|outcome| outcome.winner.is_some()),
                    image.outcome().map(|outcome| outcome.winner.is_some()),
                    "{mode} transform {} changed whether the position is decided",
                    transform.code()
                );

                // Every legal move must map onto a legal move, which is the
                // property a book relies on when it replays a stored line in a
                // transformed frame.
                let mut mapped: Vec<Move> = position
                    .legal_moves()
                    .iter()
                    .map(|&movement| transform.movement(movement))
                    .collect();
                let mut actual: Vec<Move> = image.legal_moves().iter().copied().collect();
                let sort_key = |movement: &Move| (movement.from().index(), movement.to().index());
                mapped.sort_by_key(sort_key);
                actual.sort_by_key(sort_key);
                assert_eq!(
                    mapped,
                    actual,
                    "{mode} transform {} did not map the legal moves onto each other",
                    transform.code()
                );
            }
        }
    }
}

#[test]
fn perft_agrees_across_the_symmetry_group() {
    for mode in Mode::ALL {
        let position = Position::starting(mode);
        for depth in 1..=3 {
            let expected = perft(&mut position.clone(), depth);
            for &transform in mode.rules().symmetries() {
                let mut image = position.transformed(transform);
                assert_eq!(
                    expected,
                    perft(&mut image, depth),
                    "{mode} perft({depth}) differs under transform {}",
                    transform.code()
                );
            }
        }
    }
}

#[test]
fn canonicalization_picks_one_representative_for_a_whole_class() {
    let mut random = Random(SEED ^ 0x0f0f);
    for mode in Mode::ALL {
        for _ in 0..300 {
            let position = random_position(&mut random, mode);
            let (canonical, transform) = position.canonical();
            assert_eq!(
                canonical,
                position.transformed(transform),
                "{mode} canonical transform does not produce the canonical position"
            );
            for &other in mode.rules().symmetries() {
                let (image_canonical, image_transform) = position.transformed(other).canonical();
                assert_eq!(
                    canonical, image_canonical,
                    "{mode} two members of one class canonicalize differently"
                );
                // Composition is XOR, so translating a stored line into a
                // sibling frame is one operation rather than a search.
                assert_eq!(
                    canonical,
                    position.transformed(other.then(image_transform)),
                    "{mode} transform composition is not XOR of the codes"
                );
            }
            assert_eq!(
                canonical.canonical().1,
                Transform::IDENTITY,
                "{mode} canonicalization is not idempotent"
            );
        }
    }
}

#[test]
fn the_infiltration_start_is_invariant_under_reversing_files_only() {
    let start = Position::starting(Mode::Infiltration);

    // Reversing files is a true invariance, so the 23 first moves collapse to
    // 13 classes and a book stores one analysis for each mirrored pair.
    assert_eq!(start, start.transformed(Transform::FILES));

    // Swapping colours is not, and the reason is the side to move rather than
    // the board: the armies are reflections of each other, but Red moves
    // first. The image is the position you would reach if Blue moved first,
    // which no game reaches. This is why the book keys on the files transform
    // by default -- the colour transform is sound but merges almost nothing,
    // because side-to-move parity keeps the two universes apart.
    let swapped = start.transformed(Transform::COLORS);
    assert_ne!(start, swapped, "the colour swap must change something");
    assert_eq!(start.side_to_move().other(), swapped.side_to_move());
    for color in Color::ALL {
        for kind in PieceKind::ALL {
            assert_eq!(
                start.piece_bitboard(color, kind),
                swapped.piece_bitboard(color, kind),
                "the starting armies are reflections of each other"
            );
        }
    }
}

#[test]
fn square_and_move_notation_round_trips() {
    for index in 0..81 {
        let square = Square::new(index).expect("valid square");
        let text = square.notation();
        assert_eq!(
            Some(square),
            Square::parse(&text),
            "square {text} does not round-trip"
        );
    }
    // The archive's corners, so a drift away from `backend/internal/notation`
    // fails here rather than in a stored line nobody re-reads.
    assert_eq!(Square::from_xy(0, 0), Square::parse("a1"));
    assert_eq!(Square::from_xy(8, 8), Square::parse("i9"));
    assert_eq!(
        Square::from_xy(3, 6).map(Square::notation).as_deref(),
        Some("d7")
    );

    let position = Position::starting(Mode::Infiltration);
    for &movement in position.legal_moves().iter() {
        let text = movement.notation();
        assert_eq!(
            Some(movement),
            Move::parse(&text),
            "move {text} does not round-trip"
        );
    }
    // PGN's piece letter and capture marker are accepted and discarded.
    assert_eq!(Move::parse("d7-d6"), Move::parse("Rd7xd6"));
    assert!(
        Move::parse("d7-d7").is_none(),
        "a null move is a marker, not a move"
    );
    assert!(Move::parse("j1-a2").is_none());
    assert!(Move::parse("d7d6").is_none());
}
