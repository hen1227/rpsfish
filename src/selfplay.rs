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

//! Deterministic self-play, used by the arena and by future tuning runs.
//!
//! Nothing here is reachable from the browser build. It lives in the crate
//! only so that game adjudication has exactly one implementation: an arena
//! that scored games differently from search would make every measurement
//! worthless.

use std::time::Duration;

use crate::evaluation::EvalParams;
use crate::model::{Color, EndReason, GameOutcome, Mode, Move};
use crate::position::Position;
use crate::search::{SearchLimits, SearchTuning, Searcher};

/// Hard ply ceiling for a self-play game.
///
/// Threefold repetition ends nearly every cyclic game, but a mode could in
/// principle wander for a very long time. A game stopped by this cap is scored
/// as a draw and flagged, so a tuning run can see how often it happens.
pub const DEFAULT_MAX_PLIES: u16 = 400;

/// One side's engine configuration.
#[derive(Clone, Copy, Debug)]
pub struct EngineConfig {
    pub params: EvalParams,
    pub limits: SearchLimits,
    pub hash_megabytes: usize,
    /// Whether rule-derived score bounds are active. Production is always
    /// `true`; the arena can turn it off to measure their contribution.
    pub rule_proofs: bool,
    /// Whether the selective search layer is active. Production is always
    /// `true`; the arena turns it off to measure what the pruning is worth.
    pub selective: bool,
    /// The selective layer's shape.
    pub tuning: SearchTuning,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            params: EvalParams::DEFAULT,
            // Node limits rather than time limits: strength measurements have
            // to be reproducible across machines and thread counts.
            limits: SearchLimits {
                max_depth: 64,
                max_nodes: Some(20_000),
                move_time: None,
            },
            hash_megabytes: 4,
            rule_proofs: true,
            selective: true,
            tuning: SearchTuning::DEFAULT,
        }
    }
}

impl EngineConfig {
    #[must_use]
    pub fn with_nodes(mut self, nodes: u64) -> Self {
        self.limits.max_nodes = Some(nodes);
        self
    }

    #[must_use]
    pub fn with_move_time(mut self, move_time: Option<Duration>) -> Self {
        self.limits.move_time = move_time;
        self
    }
}

/// The result of a game from Red's point of view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GameResult {
    RedWin,
    Draw,
    BlueWin,
}

impl GameResult {
    /// Red's score: 2 for a win, 1 for a draw, 0 for a loss.
    ///
    /// Halves are avoided so that tallies stay exact integers.
    #[must_use]
    pub const fn red_points(self) -> u32 {
        match self {
            Self::RedWin => 2,
            Self::Draw => 1,
            Self::BlueWin => 0,
        }
    }

    #[must_use]
    pub const fn from_perspective(self, color: Color) -> Self {
        match (self, color) {
            (Self::Draw, _) | (_, Color::Red) => self,
            (Self::RedWin, Color::Blue) => Self::BlueWin,
            (Self::BlueWin, Color::Blue) => Self::RedWin,
        }
    }
}

#[derive(Clone, Debug)]
pub struct GameRecord {
    pub result: GameResult,
    pub reason: EndReason,
    pub plies: u16,
    /// True when the ply ceiling stopped the game rather than a rule.
    pub truncated: bool,
}

/// A reusable pair of searchers, so a long match does not reallocate a
/// transposition table for every game.
pub struct MatchRunner {
    red: Searcher,
    blue: Searcher,
}

impl MatchRunner {
    #[must_use]
    pub fn new(red: &EngineConfig, blue: &EngineConfig) -> Self {
        let mut runner = Self {
            red: Searcher::new(red.hash_megabytes),
            blue: Searcher::new(blue.hash_megabytes),
        };
        for (searcher, config) in [(&mut runner.red, red), (&mut runner.blue, blue)] {
            searcher.set_eval_params(config.params);
            searcher.set_rule_proofs(config.rule_proofs);
            searcher.set_selective(config.selective);
            searcher.set_tuning(config.tuning);
        }
        runner
    }

    /// Play one game from `opening`, which must be a legal move sequence from
    /// the mode's starting position.
    pub fn play(
        &mut self,
        mode: Mode,
        opening: &[Move],
        red: &EngineConfig,
        blue: &EngineConfig,
        max_plies: u16,
    ) -> GameRecord {
        self.red.clear();
        self.blue.clear();

        let mut position = Position::starting(mode);
        let mut history = vec![position.hash()];
        for &movement in opening {
            position
                .make_move(movement)
                .expect("opening moves must be legal");
            history.push(position.hash());
        }

        let mut plies = 0_u16;
        loop {
            if let Some(outcome) = position.adjudicate() {
                return record(outcome, plies, false);
            }
            if occurrences(&history, position.hash()) >= 3 {
                return record(GameOutcome::draw(EndReason::Repetition), plies, false);
            }
            if plies >= max_plies {
                return record(GameOutcome::draw(EndReason::Repetition), plies, true);
            }

            let (searcher, limits) = match position.side_to_move() {
                Color::Red => (&mut self.red, red.limits),
                Color::Blue => (&mut self.blue, blue.limits),
            };
            let prior = &history[..history.len() - 1];
            let result = searcher.search_with_context(position, limits, prior, None);
            let Some(movement) = result.best_move else {
                // Only reachable if the position was already terminal, which
                // `adjudicate` above has ruled out.
                return record(GameOutcome::draw(EndReason::Stalemate), plies, false);
            };
            position
                .make_move(movement)
                .expect("engine move must be legal");
            history.push(position.hash());
            plies = plies.saturating_add(1);
        }
    }
}

fn record(outcome: GameOutcome, plies: u16, truncated: bool) -> GameRecord {
    let result = match outcome.winner {
        None => GameResult::Draw,
        Some(Color::Red) => GameResult::RedWin,
        Some(Color::Blue) => GameResult::BlueWin,
    };
    GameRecord {
        result,
        reason: outcome.reason,
        plies,
        truncated,
    }
}

fn occurrences(history: &[u64], hash: u64) -> usize {
    history.iter().filter(|&&entry| entry == hash).count()
}

/// A deterministic splitmix64 stream.
///
/// Openings must be reproducible from a seed alone so that a reported match
/// can be replayed exactly.
#[derive(Clone, Copy, Debug)]
pub struct Random(u64);

impl Random {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    pub fn below(&mut self, bound: usize) -> usize {
        if bound <= 1 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }

    /// A uniform sign, for SPSA perturbations.
    pub fn sign(&mut self) -> i32 {
        if self.next_u64() & 1 == 0 { -1 } else { 1 }
    }
}

/// Generate a random legal opening of `plies` moves.
///
/// Returns `None` if the walk reached a decided or stalemated position, which
/// would make the opening unplayable. Callers retry with the next seed.
#[must_use]
pub fn random_opening(mode: Mode, plies: u16, random: &mut Random) -> Option<Vec<Move>> {
    let mut position = Position::starting(mode);
    let mut moves = Vec::with_capacity(usize::from(plies));
    for _ in 0..plies {
        let legal = position.legal_moves();
        if legal.is_empty() || position.outcome().is_some() {
            return None;
        }
        let movement = legal.as_slice()[random.below(legal.len())];
        position
            .make_move(movement)
            .expect("generated move is legal");
        moves.push(movement);
    }
    (position.adjudicate().is_none()).then_some(moves)
}

/// Generate openings that are not already decisive.
///
/// An opening whose static score already favours one side inflates variance
/// and biases a paired match, so candidates outside `balance` are rejected.
#[must_use]
pub fn balanced_opening(
    mode: Mode,
    plies: u16,
    balance: i32,
    random: &mut Random,
    attempts: usize,
) -> Option<Vec<Move>> {
    for _ in 0..attempts {
        let Some(opening) = random_opening(mode, plies, random) else {
            continue;
        };
        let mut position = Position::starting(mode);
        for &movement in &opening {
            position
                .make_move(movement)
                .expect("opening moves must be legal");
        }
        if crate::evaluation::evaluate(&position).abs() <= balance {
            return Some(opening);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_MAX_PLIES, EngineConfig, GameResult, MatchRunner, Random, balanced_opening,
        random_opening,
    };
    use crate::model::{Color, Mode};

    #[test]
    fn every_mode_plays_a_complete_game() {
        let config = EngineConfig::default().with_nodes(1_500);
        for mode in Mode::ALL {
            let mut runner = MatchRunner::new(&config, &config);
            let record = runner.play(mode, &[], &config, &config, DEFAULT_MAX_PLIES);
            assert!(record.plies > 0, "{mode} produced no moves");
            assert!(!record.truncated, "{mode} hit the ply ceiling");
        }
    }

    #[test]
    fn self_play_is_reproducible_for_a_fixed_seed() {
        let config = EngineConfig::default().with_nodes(1_200);
        let mut random = Random::new(7);
        let opening = random_opening(Mode::Annihilation, 4, &mut random).expect("opening exists");
        let mut first = MatchRunner::new(&config, &config);
        let mut second = MatchRunner::new(&config, &config);
        let left = first.play(Mode::Annihilation, &opening, &config, &config, 200);
        let right = second.play(Mode::Annihilation, &opening, &config, &config, 200);
        assert_eq!(left.result, right.result);
        assert_eq!(left.plies, right.plies);
    }

    #[test]
    fn balanced_openings_are_legal_and_undecided() {
        let mut random = Random::new(99);
        for mode in Mode::ALL {
            let opening = balanced_opening(mode, 6, 120, &mut random, 200)
                .unwrap_or_else(|| panic!("{mode} must yield a balanced opening"));
            let mut position = crate::position::Position::starting(mode);
            for &movement in &opening {
                position.make_move(movement).expect("legal opening move");
            }
            assert!(position.adjudicate().is_none());
        }
    }

    #[test]
    fn perspective_flips_only_decisive_results() {
        assert_eq!(
            GameResult::RedWin.from_perspective(Color::Blue),
            GameResult::BlueWin
        );
        assert_eq!(
            GameResult::RedWin.from_perspective(Color::Red),
            GameResult::RedWin
        );
        assert_eq!(
            GameResult::Draw.from_perspective(Color::Blue),
            GameResult::Draw
        );
    }
}
