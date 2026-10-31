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

use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use crate::SQUARE_COUNT;
use crate::evaluation::{EvalParams, evaluate_with};
use crate::model::{
    Color, GameOutcome, Goal, MAX_MOVES, Mode, ModeRules, Move, MoveList, PieceKind,
};
use crate::position::{NO_PIECE, PIECE_CODES, Position};

const INFINITY: i32 = 32_000;
/// The score of a win at the search root, decreasing by one per ply so a
/// faster win outranks a slower one.
pub const MATE_SCORE: i32 = 30_000;
/// Scores at or beyond this magnitude are proven results rather than
/// estimates, and carry a distance that has to be rebased whenever the ply
/// they are measured from changes.
pub const MATE_THRESHOLD: i32 = 29_000;
const MAX_PLY: usize = 128;
/// From/to pairs, the index space for both history and counter moves.
const MOVE_SLOTS: usize = SQUARE_COUNT as usize * SQUARE_COUNT as usize;
/// History is kept per colour: the same from/to pair means opposite things to
/// the two sides, and sharing one table let each side poison the other's
/// ordering.
const HISTORY_SIZE: usize = 2 * MOVE_SLOTS;
/// How many quiet moves at a node can receive a history penalty.
const MAX_PENALIZED_QUIETS: usize = 24;
/// Ceiling on the magnitude of a history score.
const HISTORY_LIMIT: i32 = 100_000;
/// How many nodes pass between deadline and cancellation checks.
const CLOCK_INTERVAL: u64 = 1_024;

/// Largest depth and move index the reduction table covers.
const REDUCTION_SIZE: usize = 64;

/// `ln(n) * 256`, rounded, for `n` in `0..REDUCTION_SIZE`.
///
/// Logarithms are not available in a `const fn`, and the reduction curve wants
/// a logarithm rather than a step function, so the values are tabulated.
const LOG_SCALED: [i32; REDUCTION_SIZE] = [
    0, 0, 177, 281, 355, 412, 459, 498, 532, 562, 589, 614, 636, 657, 676, 693, 710, 725, 740, 754,
    767, 779, 791, 803, 814, 824, 834, 844, 853, 862, 871, 879, 887, 895, 903, 910, 917, 924, 931,
    938, 944, 951, 957, 963, 969, 975, 980, 986, 991, 996, 1001, 1007, 1012, 1016, 1021, 1026,
    1030, 1035, 1039, 1044, 1048, 1052, 1057, 1061,
];

/// The default reduction growth. See [`SearchTuning::reduction_scale`].
pub const DEFAULT_REDUCTION_SCALE: i32 = 40;

/// How aggressively the selective layer prunes, as data rather than as
/// constants.
///
/// Depth is not the goal, strength is, and a reduction that buys three plies
/// by ignoring the move that mattered is a loss. So the shape of the pruning
/// is tunable and gets measured the same way evaluation weights do:
/// `arena match --candidate-reduction-scale N`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchTuning {
    /// How strongly late-move reductions grow with depth and move index, in
    /// hundredths of `ln(depth) * ln(move_index)`.
    ///
    /// Higher searches late moves more shallowly, which buys depth for the
    /// same nodes and risks missing a late move that was actually best.
    pub reduction_scale: i32,
}

impl SearchTuning {
    pub const DEFAULT: Self = Self {
        reduction_scale: DEFAULT_REDUCTION_SCALE,
    };
}

impl Default for SearchTuning {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Depth reduction for the `n`th move at a given depth.
type ReductionTable = [[u8; REDUCTION_SIZE]; REDUCTION_SIZE];

fn build_reductions(scale: i32) -> Box<ReductionTable> {
    let scale = scale.clamp(0, 400);
    let mut table = Box::new([[0_u8; REDUCTION_SIZE]; REDUCTION_SIZE]);
    for (depth_log, row) in LOG_SCALED.iter().zip(table.iter_mut()) {
        for (index_log, slot) in LOG_SCALED.iter().zip(row.iter_mut()) {
            *slot = (depth_log * index_log * scale / (100 * 256 * 256)).clamp(0, 63) as u8;
        }
    }
    table
}

/// Deepest node that prunes on the static evaluation alone.
const FUTILITY_MAX_DEPTH: i16 = 6;

/// Deepest node that prunes quiet moves by move count.
const LATE_MOVE_MAX_DEPTH: i16 = 8;

/// Shallowest node that may try a null move.
const NULL_MIN_DEPTH: i16 = 3;

/// A scale for pruning margins when a mode's material weight is small.
///
/// Infiltration deliberately values material at 35 because the mode is about
/// advancement, but a margin of a few points would prune almost everything, so
/// the unit never drops below this.
const MIN_PRUNING_UNIT: i32 = 60;

/// Occurrences of one position that make a draw, where the rule applies.
const REPETITION_THRESHOLD: usize = 3;

/// How many quiet moves are searched before move-count pruning starts.
const fn late_move_budget(depth: i16) -> usize {
    (3 + depth * depth) as usize
}

#[derive(Clone, Copy, Debug)]
pub struct SearchLimits {
    pub max_depth: u8,
    pub max_nodes: Option<u64>,
    pub move_time: Option<Duration>,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            max_depth: 8,
            max_nodes: Some(1_000_000),
            move_time: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchStopReason {
    DepthLimit,
    NodeLimit,
    TimeLimit,
    Cancelled,
    TerminalPosition,
    Repetition,
    /// The root has already run out QUIET_PLY_LIMIT moves with no capture, so
    /// the game is drawn before the search begins.
    NoCapture,
    NoLegalMove,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchStats {
    pub nodes: u64,
    pub elapsed: Duration,
    pub selective_depth: u8,
    pub tt_probes: u64,
    pub tt_hits: u64,
    pub tt_cutoffs: u64,
    pub beta_cutoffs: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchResult {
    pub best_move: Option<Move>,
    pub score: i32,
    pub completed_depth: u8,
    pub principal_variation: Vec<Move>,
    pub stop_reason: SearchStopReason,
    pub stats: SearchStats,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalysisLine {
    pub movement: Move,
    pub score: i32,
    pub principal_variation: Vec<Move>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalysisResult {
    pub lines: Vec<AnalysisLine>,
    pub score: i32,
    pub completed_depth: u8,
    /// A 0-100 measure of iterative stability, not a win probability.
    pub confidence: u8,
    pub stop_reason: SearchStopReason,
    pub stats: SearchStats,
}

/// A trustworthy snapshot emitted after a whole iterative-deepening pass.
///
/// An update is never emitted for a partially searched depth. This lets a
/// caller display it immediately and retain it if a later iteration is
/// cancelled or exhausts a resource limit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalysisUpdate {
    pub lines: Vec<AnalysisLine>,
    pub score: i32,
    pub completed_depth: u8,
    /// A 0-100 measure of iterative stability, not a win probability.
    pub confidence: u8,
    pub stats: SearchStats,
}

pub struct Searcher {
    table: TranspositionTable,
    eval_params: EvalParams,
    rule_proofs: bool,
    /// Whether the selective layer (null move, futility, move-count pruning,
    /// and late-move reductions) is active.
    ///
    /// On by default and only ever turned off to measure what it is worth. A
    /// heuristic that cannot be A/B tested is a belief.
    selective: bool,
    tuning: SearchTuning,
    reductions: Box<ReductionTable>,
    /// Whether the selective layer is licensed for the search now running.
    ///
    /// `selective` is what the caller asked for; this is that *and* a mode
    /// whose rules the layer's assumptions hold in. Every pruning and reduction
    /// below reads this rather than the setting, because a searcher reused
    /// across modes -- which is what a session does -- must not carry one
    /// mode's licence into another's position.
    selective_now: bool,
    /// The rules of the mode at the root, refreshed by `begin_search`.
    ///
    /// Two of them decide scores rather than moves -- whether a no-move node
    /// is a draw or a loss, and whether a repeat is half a point -- and both
    /// used to be search settings. They are read from the mode instead so that
    /// one searcher reused across modes, which is what a session does, cannot
    /// score one mode's position under another's rules.
    mode_rules: ModeRules,
    history: Vec<i32>,
    /// The quiet reply that refuted each preceding move.
    ///
    /// A move that answered the opponent's last move well once tends to answer
    /// it well again, which the killer table cannot express because it is
    /// indexed by ply rather than by what was just played.
    counters: Vec<Move>,
    killers: [[Move; 2]; MAX_PLY],
    /// The move played into each ply, so a node can find its counter move.
    path_moves: [Move; MAX_PLY + 1],
    path_hashes: Vec<u64>,
    /// Index in `path_hashes` below which repetition cannot reach.
    ///
    /// Captures and territory claims are irreversible and both change the
    /// hash monotonically, so a position can never repeat across one. Keeping
    /// the floor turns the repetition test from a full-path scan into a scan
    /// of the current reversible run. Zero until search makes its first
    /// irreversible move, and never inside the caller-supplied prefix.
    path_floor: usize,
    /// Index of the root position in `path_hashes`.
    path_root: usize,
    /// The caller's prior hashes, sorted, so counting occurrences in a long
    /// game history is a binary search rather than a scan at every node.
    prior_sorted: Vec<u64>,
    /// Ply at which a null move was just made, so the reply cannot answer a
    /// pass with another pass. `usize::MAX` when no null move is in flight.
    null_ply: usize,
    /// Root moves the current analysis is restricted to, empty for all of
    /// them.
    ///
    /// Grading a played move means comparing it with the best move, and that
    /// comparison is only honest when both numbers come out of the same
    /// search: the same depth, the same window, the same table. Restricting
    /// the root is how the played move gets that treatment.
    root_filter: Vec<Move>,
    /// The scale of one unit of advantage in this mode, for pruning margins.
    pruning_unit: i32,
    /// Scratch move storage for the whole search tree.
    ///
    /// A `MoveList` is 272 entries wide because that is the board's edge
    /// count, so constructing one at every node meant zeroing half a kilobyte
    /// millions of times per second. One arena, indexed by a per-node base
    /// offset, removes that entirely.
    moves: Vec<ScoredMove>,
    stats: SearchStats,
    stop_reason: Option<SearchStopReason>,
    generation: u8,
}

impl Default for Searcher {
    fn default() -> Self {
        Self::new(16)
    }
}

impl Searcher {
    #[must_use]
    pub fn new(transposition_megabytes: usize) -> Self {
        Self {
            table: TranspositionTable::new(transposition_megabytes),
            eval_params: EvalParams::DEFAULT,
            rule_proofs: true,
            selective: true,
            tuning: SearchTuning::DEFAULT,
            reductions: build_reductions(SearchTuning::DEFAULT.reduction_scale),
            selective_now: true,
            mode_rules: Mode::TotalWar.rules(),
            history: vec![0; HISTORY_SIZE],
            counters: vec![Move::NONE; MOVE_SLOTS],
            killers: [[Move::NONE; 2]; MAX_PLY],
            path_moves: [Move::NONE; MAX_PLY + 1],
            path_hashes: Vec::with_capacity(MAX_PLY + 256),
            path_floor: 0,
            path_root: 0,
            prior_sorted: Vec::new(),
            null_ply: usize::MAX,
            root_filter: Vec::new(),
            pruning_unit: MIN_PRUNING_UNIT,
            moves: Vec::with_capacity((MAX_PLY + 2) * MAX_MOVES),
            stats: SearchStats::default(),
            stop_reason: None,
            generation: 0,
        }
    }

    /// Replace the evaluation weights.
    ///
    /// Weights are data, so a tuner or an arena candidate can vary them
    /// without recompiling and without touching search behaviour.
    pub fn set_eval_params(&mut self, params: EvalParams) {
        self.eval_params = params;
    }

    #[must_use]
    pub const fn eval_params(&self) -> &EvalParams {
        &self.eval_params
    }

    /// Enable or disable rule-derived score bounds.
    ///
    /// On by default, and only ever turned off to measure what the bounds are
    /// worth. A proof that cannot be A/B tested is a belief.
    pub fn set_rule_proofs(&mut self, enabled: bool) {
        self.rule_proofs = enabled;
    }

    /// Enable or disable the selective search layer.
    ///
    /// On by default. Turning it off restores a plain principal-variation
    /// search with the original timid reduction, which is the control arm when
    /// measuring what the pruning is worth.
    pub fn set_selective(&mut self, enabled: bool) {
        self.selective = enabled;
    }

    #[must_use]
    pub const fn selective(&self) -> bool {
        self.selective
    }

    /// Replace the selective-search shape.
    pub fn set_tuning(&mut self, tuning: SearchTuning) {
        self.tuning = tuning;
        self.reductions = build_reductions(tuning.reduction_scale);
    }

    #[must_use]
    pub const fn tuning(&self) -> &SearchTuning {
        &self.tuning
    }

    pub fn clear(&mut self) {
        self.table.clear();
        self.history.fill(0);
        self.counters.fill(Move::NONE);
        self.killers.fill([Move::NONE; 2]);
    }

    #[must_use]
    pub fn search(&mut self, position: Position, limits: SearchLimits) -> SearchResult {
        self.search_with_context(position, limits, &[], None)
    }

    /// Search with hashes from positions before `position` and an optional
    /// cancellation flag. The current position hash is appended internally.
    #[must_use]
    pub fn search_with_context(
        &mut self,
        position: Position,
        limits: SearchLimits,
        prior_hashes: &[u64],
        cancellation: Option<&AtomicBool>,
    ) -> SearchResult {
        let started = engine_now();
        let control = SearchControl {
            deadline: engine_deadline(started, limits.move_time),
            max_nodes: limits.max_nodes,
            cancellation,
        };
        self.begin_search(prior_hashes, &position);

        if let Some(outcome) = position.outcome() {
            return self.finish_result(
                started,
                None,
                terminal_score(outcome, position.side_to_move(), 0),
                0,
                SearchStopReason::TerminalPosition,
                Vec::new(),
            );
        }
        if self.is_repetition(position.hash()) {
            return self.finish_result(
                started,
                None,
                0,
                0,
                SearchStopReason::Repetition,
                Vec::new(),
            );
        }
        // After the terminal test and the repetition test, never before: this
        // is the weakest of the three claims about a position, and a root that
        // is won or blockaded is that instead. See `Position::adjudicate`.
        if position.quiet_limit_reached() && position.has_any_legal_move(position.side_to_move()) {
            return self.finish_result(
                started,
                None,
                0,
                0,
                SearchStopReason::NoCapture,
                Vec::new(),
            );
        }

        let root_moves = position.legal_moves();
        if root_moves.is_empty() {
            return self.finish_result(
                started,
                None,
                self.no_move_score(0),
                0,
                SearchStopReason::NoLegalMove,
                Vec::new(),
            );
        }

        let mut completed_depth = 0_u8;
        let mut best_move = root_moves.as_slice().first().copied();
        let mut best_score = self.leaf_score(&position);
        let requested_depth = limits.max_depth.max(1);

        let mut previous_score = None;
        for depth in 1..=requested_depth {
            let Some((candidate, score)) =
                self.search_root_aspirated(position, depth, previous_score, &control)
            else {
                break;
            };
            best_move = Some(candidate);
            best_score = score;
            completed_depth = depth;
            previous_score = Some(score);
        }

        let reason = self.stop_reason.unwrap_or(SearchStopReason::DepthLimit);
        let principal_variation = self.extract_principal_variation(position, completed_depth);
        self.finish_result(
            started,
            best_move,
            best_score,
            completed_depth,
            reason,
            principal_variation,
        )
    }

    /// Return the strongest root moves in score order.
    ///
    /// A single variation uses the faster principal-variation root search.
    /// With MultiPV, each root candidate is searched with a full window so
    /// secondary lines have meaningful scores instead of alpha-beta bounds.
    /// If a limit stops an iteration, the last completely searched depth is
    /// returned.
    #[must_use]
    pub fn analyze(
        &mut self,
        position: Position,
        limits: SearchLimits,
        variations: usize,
    ) -> AnalysisResult {
        self.analyze_with_context(position, limits, variations, &[])
    }

    /// Return the strongest root moves while accounting for positions that
    /// occurred earlier in the game.
    #[must_use]
    pub fn analyze_with_context(
        &mut self,
        position: Position,
        limits: SearchLimits,
        variations: usize,
        prior_hashes: &[u64],
    ) -> AnalysisResult {
        self.analyze_with_context_and_updates(
            position,
            limits,
            variations,
            prior_hashes,
            None,
            |_| {},
        )
    }

    /// Analyze a position and report every fully completed depth.
    #[must_use]
    pub fn analyze_with_updates<F>(
        &mut self,
        position: Position,
        limits: SearchLimits,
        variations: usize,
        on_update: F,
    ) -> AnalysisResult
    where
        F: FnMut(&AnalysisUpdate),
    {
        self.analyze_with_context_and_updates(position, limits, variations, &[], None, on_update)
    }

    /// Analyze with history, cancellation, and completed-depth updates.
    #[must_use]
    pub fn analyze_with_context_and_updates<F>(
        &mut self,
        position: Position,
        limits: SearchLimits,
        variations: usize,
        prior_hashes: &[u64],
        cancellation: Option<&AtomicBool>,
        on_update: F,
    ) -> AnalysisResult
    where
        F: FnMut(&AnalysisUpdate),
    {
        self.analyze_inner(
            position,
            limits,
            variations,
            prior_hashes,
            &[],
            cancellation,
            on_update,
        )
    }

    /// Score only the given root moves, everything else held identical.
    ///
    /// This exists for review: the loss a move cost its player is the
    /// difference between the best move's score and that move's score, and
    /// subtracting two numbers is only meaningful when they were produced the
    /// same way. Searching the child position instead would measure the move
    /// one ply deeper than the alternative it is being compared with.
    ///
    /// Returns `SearchStopReason::NoLegalMove` with no lines when none of the
    /// requested moves is legal here, which is the caller's signal that it
    /// asked about a move this position does not have.
    #[must_use]
    pub fn analyze_root_moves(
        &mut self,
        position: Position,
        limits: SearchLimits,
        variations: usize,
        prior_hashes: &[u64],
        root_moves: &[Move],
    ) -> AnalysisResult {
        self.analyze_inner(
            position,
            limits,
            variations,
            prior_hashes,
            root_moves,
            None,
            |_| {},
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn analyze_inner<F>(
        &mut self,
        position: Position,
        limits: SearchLimits,
        variations: usize,
        prior_hashes: &[u64],
        root_moves: &[Move],
        cancellation: Option<&AtomicBool>,
        mut on_update: F,
    ) -> AnalysisResult
    where
        F: FnMut(&AnalysisUpdate),
    {
        let started = engine_now();
        let control = SearchControl {
            deadline: engine_deadline(started, limits.move_time),
            max_nodes: limits.max_nodes,
            cancellation,
        };
        self.begin_search(prior_hashes, &position);

        if let Some(outcome) = position.outcome() {
            return self.finish_analysis(
                started,
                Vec::new(),
                terminal_score(outcome, position.side_to_move(), 0),
                0,
                100,
                SearchStopReason::TerminalPosition,
            );
        }
        if self.is_repetition(position.hash()) {
            return self.finish_analysis(
                started,
                Vec::new(),
                0,
                0,
                100,
                SearchStopReason::Repetition,
            );
        }
        if position.quiet_limit_reached() && position.has_any_legal_move(position.side_to_move()) {
            return self.finish_analysis(
                started,
                Vec::new(),
                0,
                0,
                100,
                SearchStopReason::NoCapture,
            );
        }

        let legal_moves = position.legal_moves();
        if legal_moves.is_empty() {
            return self.finish_analysis(
                started,
                Vec::new(),
                self.no_move_score(0),
                0,
                100,
                SearchStopReason::NoLegalMove,
            );
        }
        self.root_filter = root_moves
            .iter()
            .copied()
            .filter(|movement| legal_moves.as_slice().contains(movement))
            .collect();
        if !root_moves.is_empty() && self.root_filter.is_empty() {
            return self.finish_analysis(
                started,
                Vec::new(),
                self.no_move_score(0),
                0,
                100,
                SearchStopReason::NoLegalMove,
            );
        }
        let root_count = if self.root_filter.is_empty() {
            legal_moves.len()
        } else {
            self.root_filter.len()
        };

        let requested_variations = variations.max(1).min(root_count);
        let mut best_lines = self.fallback_analysis_lines(position, &legal_moves);
        if !self.root_filter.is_empty() {
            best_lines.retain(|line| self.root_filter.contains(&line.movement));
        }
        best_lines.truncate(requested_variations);
        let mut completed_depth = 0_u8;
        let mut confidence = 0_u8;
        let mut previous_best = None;
        let mut previous_score = None;
        let mut stable_iterations = 0_u8;

        // A restricted root always takes the MultiPV path, whose first move
        // is searched with a full window. That is exactly how the full search
        // scored the best move, and comparability is the whole reason the
        // restriction exists — the aspirated single-line path would answer a
        // different question with a narrower window.
        let single_line = requested_variations == 1 && self.root_filter.is_empty();
        for depth in 1..=limits.max_depth.max(1) {
            let mut lines = if single_line {
                let Some((movement, score)) =
                    self.search_root_aspirated(position, depth, previous_score, &control)
                else {
                    break;
                };
                vec![AnalysisLine {
                    movement,
                    score,
                    principal_variation: self.extract_principal_variation(position, depth),
                }]
            } else {
                let Some(lines) =
                    self.search_root_lines(position, depth, requested_variations, &control)
                else {
                    break;
                };
                lines
            };
            lines.truncate(requested_variations);
            let score = lines
                .first()
                .map_or_else(|| self.leaf_score(&position), |line| line.score);
            let best = lines.first().map(|line| line.movement);
            if best.is_some() && best == previous_best {
                stable_iterations = stable_iterations.saturating_add(1);
            } else {
                stable_iterations = 1;
            }
            let score_delta = previous_score.map_or(u32::MAX, |old: i32| old.abs_diff(score));
            confidence = analysis_confidence(depth, stable_iterations, score_delta, score);
            previous_best = best;
            previous_score = Some(score);
            best_lines = lines;
            completed_depth = depth;

            let mut stats = self.stats.clone();
            stats.elapsed = engine_elapsed(started);
            on_update(&AnalysisUpdate {
                lines: best_lines.clone(),
                score,
                completed_depth,
                confidence,
                stats,
            });
        }

        let score = best_lines
            .first()
            .map_or_else(|| self.leaf_score(&position), |line| line.score);
        let reason = self.stop_reason.unwrap_or(SearchStopReason::DepthLimit);
        self.root_filter.clear();
        self.finish_analysis(
            started,
            best_lines,
            score,
            completed_depth,
            confidence,
            reason,
        )
    }

    fn begin_search(&mut self, prior_hashes: &[u64], root: &Position) {
        let root_hash = root.hash();
        self.mode_rules = root.mode().rules();
        // Corner races are sensitive to forced moves and distant blockades.
        // More aggressive reductions and aspiration searches were tested here
        // but lost strength and missed forced losses. Keep the conservative
        // search in modes where stalemate loses.
        self.selective_now = self.selective && !self.mode_rules.stalemate_loses;
        self.pruning_unit = self.material_weight(root.mode()).max(MIN_PRUNING_UNIT);
        self.null_ply = usize::MAX;
        self.stats = SearchStats::default();
        self.stop_reason = None;
        self.generation = (self.generation + 1) & 0x3f;
        self.path_hashes.clear();
        self.path_hashes.extend_from_slice(prior_hashes);
        self.path_hashes.push(root_hash);
        self.path_root = prior_hashes.len();
        self.path_floor = 0;
        self.prior_sorted.clear();
        self.prior_sorted.extend_from_slice(prior_hashes);
        self.prior_sorted.sort_unstable();
        self.moves.clear();
        self.root_filter.clear();
        self.path_moves[0] = Move::NONE;
        // History is intentionally aged instead of reset between searches so
        // repeated games can retain useful ordering without unbounded values.
        for value in &mut self.history {
            *value /= 2;
        }
    }

    fn finish_result(
        &mut self,
        started: EngineInstant,
        best_move: Option<Move>,
        score: i32,
        completed_depth: u8,
        stop_reason: SearchStopReason,
        principal_variation: Vec<Move>,
    ) -> SearchResult {
        self.stats.elapsed = engine_elapsed(started);
        SearchResult {
            best_move,
            score,
            completed_depth,
            principal_variation,
            stop_reason,
            stats: self.stats.clone(),
        }
    }

    fn finish_analysis(
        &mut self,
        started: EngineInstant,
        lines: Vec<AnalysisLine>,
        score: i32,
        completed_depth: u8,
        confidence: u8,
        stop_reason: SearchStopReason,
    ) -> AnalysisResult {
        self.stats.elapsed = engine_elapsed(started);
        AnalysisResult {
            lines,
            score,
            completed_depth,
            confidence,
            stop_reason,
            stats: self.stats.clone(),
        }
    }

    fn fallback_analysis_lines(
        &self,
        mut position: Position,
        root_moves: &MoveList,
    ) -> Vec<AnalysisLine> {
        let mut lines = Vec::with_capacity(root_moves.len());
        for &movement in root_moves {
            let undo = position.make_move_unchecked(movement);
            lines.push(AnalysisLine {
                movement,
                score: -self.leaf_score(&position),
                principal_variation: vec![movement],
            });
            position.unmake_move(undo);
        }
        sort_analysis_lines(&mut lines);
        lines
    }

    /// Search the root and return the strongest `variations` lines.
    ///
    /// Each pass finds the best of the moves no earlier pass claimed. Within a
    /// pass only the first move gets a full window; the rest are tested
    /// against the running best with a null window and re-searched only when
    /// they beat it. The previous version gave every root move a full window
    /// at every depth, which switched alpha-beta off at the root and cost
    /// several plies.
    fn search_root_lines(
        &mut self,
        mut position: Position,
        depth: u8,
        variations: usize,
        control: &SearchControl<'_>,
    ) -> Option<Vec<AnalysisLine>> {
        if !self.enter_node(control, 0) {
            return None;
        }
        let root_key = transposition_key(&position);
        let tt_move = self
            .probe(root_key)
            .map_or(Move::NONE, |entry| entry.best_move);
        let (base, end, _) = self.stage_moves(&position, tt_move, 0);
        let end = self.restrict_root(base, end);
        let wanted = variations.clamp(1, end - base);
        let mut lines = Vec::with_capacity(wanted);

        for pv_index in 0..wanted {
            let range_base = base + pv_index;
            self.order_from(range_base, end);
            self.sort_range(range_base + 1, end);

            let mut alpha = -INFINITY;
            let beta = INFINITY;
            let mut best_offset = range_base;
            let mut best_score = -INFINITY;
            for offset in range_base..end {
                let movement = self.moves[offset].movement;
                self.path_moves[0] = movement;
                let undo = position.make_move_unchecked(movement);
                let floor = self.push_path(position.hash(), undo.is_irreversible());
                let score = if offset == range_base {
                    -self.negamax(
                        &mut position,
                        i16::from(depth) - 1,
                        -beta,
                        -alpha,
                        1,
                        control,
                    )
                } else {
                    let mut candidate = -self.negamax(
                        &mut position,
                        i16::from(depth) - 1,
                        -alpha - 1,
                        -alpha,
                        1,
                        control,
                    );
                    if candidate > alpha && self.stop_reason.is_none() {
                        candidate = -self.negamax(
                            &mut position,
                            i16::from(depth) - 1,
                            -beta,
                            -alpha,
                            1,
                            control,
                        );
                    }
                    candidate
                };
                self.pop_path(floor);
                position.unmake_move(undo);

                if self.stop_reason.is_some() {
                    self.moves.truncate(base);
                    return None;
                }
                if score > best_score {
                    best_score = score;
                    best_offset = offset;
                }
                alpha = alpha.max(score);
            }

            self.moves.swap(range_base, best_offset);
            let movement = self.moves[range_base].movement;
            // A restricted root did not consider every move, so its verdict
            // is not this position's verdict and must not be stored under
            // this position's key. Sub-tree entries are unaffected: they were
            // searched in full.
            if pv_index == 0 && self.root_filter.is_empty() {
                // Recording the overall best move first is what orders the
                // next depth's first pass.
                self.table.store(TTEntry::new(
                    root_key,
                    score_to_tt(best_score, 0),
                    i16::from(depth),
                    Bound::Exact,
                    movement,
                    self.generation,
                ));
            }
            let mut principal_variation = vec![movement];
            let undo = position.make_move_unchecked(movement);
            principal_variation
                .extend(self.extract_principal_variation(position, depth.saturating_sub(1)));
            position.unmake_move(undo);
            lines.push(AnalysisLine {
                movement,
                score: best_score,
                principal_variation,
            });
        }
        self.moves.truncate(base);

        sort_analysis_lines(&mut lines);
        Some(lines)
    }

    /// Search the root with a window centred on the previous iteration's
    /// score, widening only when the true score falls outside it.
    ///
    /// Most iterations land close to the last one, and a narrow window makes
    /// every node below cheaper. A failed guess costs one re-search, so the
    /// window widens exponentially rather than jumping straight to infinity.
    fn search_root_aspirated(
        &mut self,
        position: Position,
        depth: u8,
        previous_score: Option<i32>,
        control: &SearchControl<'_>,
    ) -> Option<(Move, i32)> {
        let Some(centre) = previous_score.filter(|_| self.selective_now && depth >= 4) else {
            return self.search_root(position, depth, -INFINITY, INFINITY, control);
        };
        if centre.abs() >= MATE_THRESHOLD {
            return self.search_root(position, depth, -INFINITY, INFINITY, control);
        }

        let mut window = (self.pruning_unit / 4).max(8);
        loop {
            let alpha = (centre - window).max(-INFINITY);
            let beta = (centre + window).min(INFINITY);
            let (movement, score) = self.search_root(position, depth, alpha, beta, control)?;
            if (score > alpha || alpha == -INFINITY) && (score < beta || beta == INFINITY) {
                return Some((movement, score));
            }
            window = window.saturating_mul(4);
            if window >= INFINITY {
                return self.search_root(position, depth, -INFINITY, INFINITY, control);
            }
        }
    }

    fn search_root(
        &mut self,
        mut position: Position,
        depth: u8,
        mut alpha: i32,
        beta: i32,
        control: &SearchControl<'_>,
    ) -> Option<(Move, i32)> {
        if !self.enter_node(control, 0) {
            return None;
        }
        let root_key = transposition_key(&position);
        let tt_move = self
            .probe(root_key)
            .map_or(Move::NONE, |entry| entry.best_move);
        let (base, end, _) = self.stage_moves(&position, tt_move, 0);
        let end = self.restrict_root(base, end);

        let original_alpha = alpha;
        let mut best_move = Move::NONE;
        let mut best_score = -INFINITY;

        for move_index in 0..end - base {
            let movement = self.staged_move(base, move_index, end);
            self.path_moves[0] = movement;
            let undo = position.make_move_unchecked(movement);
            let floor = self.push_path(position.hash(), undo.is_irreversible());
            let mut score = if move_index == 0 {
                -self.negamax(
                    &mut position,
                    i16::from(depth) - 1,
                    -beta,
                    -alpha,
                    1,
                    control,
                )
            } else {
                -self.negamax(
                    &mut position,
                    i16::from(depth) - 1,
                    -alpha - 1,
                    -alpha,
                    1,
                    control,
                )
            };
            if move_index > 0 && score > alpha && score < beta && self.stop_reason.is_none() {
                score = -self.negamax(
                    &mut position,
                    i16::from(depth) - 1,
                    -beta,
                    -alpha,
                    1,
                    control,
                );
            }
            self.pop_path(floor);
            position.unmake_move(undo);

            if self.stop_reason.is_some() {
                self.moves.truncate(base);
                return None;
            }
            if score > best_score {
                best_score = score;
                best_move = movement;
            }
            alpha = alpha.max(score);
            if alpha >= beta {
                // Only reachable inside an aspiration window; the caller
                // widens and searches again.
                break;
            }
        }
        self.moves.truncate(base);

        let bound = if best_score <= original_alpha {
            Bound::Upper
        } else if best_score >= beta {
            Bound::Lower
        } else {
            Bound::Exact
        };
        if self.root_filter.is_empty() {
            self.table.store(TTEntry::new(
                root_key,
                score_to_tt(best_score, 0),
                i16::from(depth),
                bound,
                best_move,
                self.generation,
            ));
        }
        Some((best_move, best_score))
    }

    fn negamax(
        &mut self,
        position: &mut Position,
        mut depth: i16,
        mut alpha: i32,
        beta: i32,
        ply: usize,
        control: &SearchControl<'_>,
    ) -> i32 {
        if depth <= 0 {
            return self.quiescence(position, alpha, beta, ply, control);
        }
        if !self.enter_node(control, ply) {
            return self.leaf_score(position);
        }
        if let Some(outcome) = position.outcome() {
            return terminal_score(outcome, position.side_to_move(), ply);
        }
        if self.rule_proofs && is_proven_draw(position) {
            return 0;
        }
        if self.is_repetition(position.hash()) {
            return 0;
        }
        // Last of the three, and it costs a move generation only on the rare
        // node that got this far: a hundredth quiet move that also blockades
        // the opponent is a win in a mode where being stuck loses, not a draw.
        if position.quiet_limit_reached() {
            return if position.has_any_legal_move(position.side_to_move()) {
                0
            } else {
                self.no_move_score(ply)
            };
        }
        if ply >= MAX_PLY - 1 {
            return self.leaf_score(position);
        }

        let corner_race = self.mode_rules.goal == Goal::EnemyCorner;
        if corner_race && position.goal_move_for(position.side_to_move()).is_some() {
            return MATE_SCORE - ply_i32(ply + 1);
        }
        let must_defend = corner_race
            && position
                .goal_move_for(position.side_to_move().other())
                .is_some();
        if self.rule_proofs
            && let Some(value) = crate::endgame::probe(position)
        {
            return endgame_score(value, ply);
        }

        let key = transposition_key(position);
        let original_alpha = alpha;
        let mut tt_move = Move::NONE;
        if let Some(entry) = self.probe(key) {
            tt_move = entry.best_move;
            if i16::from(entry.depth) >= depth {
                let score = score_from_tt(entry.score, ply);
                let cutoff = match entry.bound() {
                    Bound::Exact => Some(score),
                    Bound::Lower if score >= beta => Some(score),
                    Bound::Upper if score <= alpha => Some(score),
                    _ => None,
                };
                if let Some(score) = cutoff {
                    self.stats.tt_cutoffs = self.stats.tt_cutoffs.saturating_add(1);
                    return score;
                }
            }
        }

        // A zero-width window means this node only has to prove a bound, which
        // is what licenses the selective layer below. Principal-variation
        // nodes are searched exactly.
        let narrow = beta - alpha <= 1;
        let selective = self.selective_now && !must_defend && narrow && beta.abs() < MATE_THRESHOLD;
        let mut static_eval = 0_i32;
        if selective {
            static_eval = self.leaf_score(position);

            // Reverse futility: the position is already so far above beta that
            // conceding a depth-scaled margin still fails high.
            if depth <= FUTILITY_MAX_DEPTH
                && static_eval - self.pruning_unit * i32::from(depth) / 2 >= beta
            {
                return static_eval;
            }

            // Null move: hand the opponent a free move. If the position still
            // holds, the real moves do not need a full-depth search. A side
            // down to its last piece is excluded, because there a forced move
            // really can be worse than passing.
            //
            // And a mode where being unable to move loses is excluded outright:
            // that rule is zugzwang everywhere rather than only at one piece,
            // because a side whose every move walks into a capture and a side
            // with no move at all are both lost, and both would be saved by a
            // pass. This is what `selective_search_agrees_with_exhaustive_
            // search_on_forced_positions` catches when the guard is removed --
            // a forced loss came back as a small plus.
            if depth >= NULL_MIN_DEPTH
                && self.null_ply != ply
                && static_eval >= beta
                && position.piece_count(position.side_to_move()) >= 2
            {
                let reduction = 2 + depth / 6;
                let undo = position.make_null_move();
                // A pass is treated as irreversible so that repetition cannot
                // match a real position against one reached through a pass.
                let floor = self.push_path(position.hash(), true);
                let previous_null = self.null_ply;
                self.null_ply = ply + 1;
                self.path_moves[ply] = Move::NONE;
                let score = -self.negamax(
                    position,
                    depth - 1 - reduction,
                    -beta,
                    -beta + 1,
                    ply + 1,
                    control,
                );
                self.null_ply = previous_null;
                self.pop_path(floor);
                position.unmake_null_move(undo);
                if self.stop_reason.is_none() && score >= beta {
                    // Never report a mate that only a pass proved.
                    return if score >= MATE_THRESHOLD { beta } else { score };
                }
            }
        }

        // Internal iterative reduction: with no stored best move, ordering at
        // this node is guesswork, so a deep search of it is likely wasted.
        // Searching one ply shallower fills the table and the next visit is
        // ordered properly.
        if self.selective_now && !must_defend && tt_move.is_none() && depth >= 4 {
            depth -= 1;
        }

        let (base, end, context) = self.stage_moves(position, tt_move, ply);
        if base == end {
            return self.no_move_score(ply);
        }

        let mut best_score = -INFINITY;
        let mut best_move = Move::NONE;
        let mut quiets_seen = 0_usize;
        let mut searched = 0_usize;
        let mut refuted = [Move::NONE; MAX_PENALIZED_QUIETS];
        let mut refuted_count = 0_usize;
        let color = position.side_to_move();
        for move_index in 0..end - base {
            let movement = self.staged_move(base, move_index, end);
            let quiet = !context.is_tactical(position, movement);

            // Pruning never touches the first move, so this node always
            // returns a score backed by a real search.
            if selective && quiet && searched > 0 && best_score > -MATE_THRESHOLD {
                quiets_seen += 1;
                // Move-count pruning: ordering has already tried the moves
                // most likely to matter.
                if depth <= LATE_MOVE_MAX_DEPTH && quiets_seen > late_move_budget(depth) {
                    continue;
                }
                // Futility: a quiet move at a shallow depth cannot plausibly
                // recover this much.
                if depth <= FUTILITY_MAX_DEPTH
                    && static_eval + self.pruning_unit * i32::from(depth + 1) / 2 <= alpha
                {
                    continue;
                }
            }

            searched += 1;
            self.path_moves[ply] = movement;
            let undo = position.make_move_unchecked(movement);
            let floor = self.push_path(position.hash(), undo.is_irreversible());
            let mut score = if move_index == 0 {
                -self.negamax(position, depth - 1, -beta, -alpha, ply + 1, control)
            } else {
                let reduced_depth = depth
                    - 1
                    - if !must_defend {
                        self.reduction(depth, move_index, quiet, narrow)
                    } else {
                        0
                    };
                let mut candidate = -self.negamax(
                    position,
                    reduced_depth,
                    -alpha - 1,
                    -alpha,
                    ply + 1,
                    control,
                );
                if reduced_depth != depth - 1 && candidate > alpha && self.stop_reason.is_none() {
                    candidate =
                        -self.negamax(position, depth - 1, -alpha - 1, -alpha, ply + 1, control);
                }
                candidate
            };
            if move_index > 0 && score > alpha && score < beta && self.stop_reason.is_none() {
                score = -self.negamax(position, depth - 1, -beta, -alpha, ply + 1, control);
            }
            self.pop_path(floor);
            position.unmake_move(undo);

            if self.stop_reason.is_some() {
                self.moves.truncate(base);
                return self.leaf_score(position);
            }
            if score > best_score {
                best_score = score;
                best_move = movement;
            }
            if score > alpha {
                alpha = score;
            }
            if alpha >= beta {
                self.stats.beta_cutoffs = self.stats.beta_cutoffs.saturating_add(1);
                if quiet {
                    self.record_quiet_cutoff(
                        color,
                        movement,
                        depth,
                        ply,
                        &refuted[..refuted_count],
                    );
                }
                break;
            }
            if quiet && refuted_count < MAX_PENALIZED_QUIETS {
                refuted[refuted_count] = movement;
                refuted_count += 1;
            }
        }
        self.moves.truncate(base);
        debug_assert!(
            !best_move.is_none(),
            "the first move is never pruned, so a score is always backed by a search"
        );

        if self.stop_reason.is_none() {
            let bound = if best_score <= original_alpha {
                Bound::Upper
            } else if best_score >= beta {
                Bound::Lower
            } else {
                Bound::Exact
            };
            self.table.store(TTEntry::new(
                key,
                score_to_tt(best_score, ply),
                depth,
                bound,
                best_move,
                self.generation,
            ));
        }
        best_score
    }

    fn quiescence(
        &mut self,
        position: &mut Position,
        mut alpha: i32,
        beta: i32,
        ply: usize,
        control: &SearchControl<'_>,
    ) -> i32 {
        if !self.enter_node(control, ply) {
            return self.leaf_score(position);
        }
        if let Some(outcome) = position.outcome() {
            return terminal_score(outcome, position.side_to_move(), ply);
        }
        if self.rule_proofs && is_proven_draw(position) {
            return 0;
        }
        if self.is_repetition(position.hash()) {
            return 0;
        }
        if ply >= MAX_PLY - 1 {
            return self.leaf_score(position);
        }

        if !position.has_any_legal_move(position.side_to_move()) {
            return self.no_move_score(ply);
        }
        // Below the stalemate test for the same reason it is below it above.
        if position.quiet_limit_reached() {
            return 0;
        }

        if self.rule_proofs
            && let Some(value) = crate::endgame::probe(position)
        {
            return endgame_score(value, ply);
        }

        let us = position.side_to_move();
        let corner_race = self.mode_rules.goal == Goal::EnemyCorner;
        if corner_race && position.goal_move_for(us).is_some() {
            return MATE_SCORE - ply_i32(ply + 1);
        }
        // A threat of arrival is this game's check. Passing is not an option:
        // search the quiet corner plugs as well as captures of the runner.
        // The next node recognizes any unanswered threat as a win in one.
        let must_defend = corner_race && position.goal_move_for(us.other()).is_some();
        let stand_pat = self.leaf_score(position);
        if !must_defend {
            if stand_pat >= beta {
                return stand_pat;
            }
            alpha = alpha.max(stand_pat);
        }

        // Delta pruning: when even winning the best available piece cannot
        // reach alpha, the remaining captures are noise.
        let hopeless =
            !must_defend && self.selective_now && stand_pat + self.pruning_unit * 2 <= alpha;

        let (base, end, context) = if must_defend {
            self.stage_moves(position, Move::NONE, ply)
        } else {
            self.stage_tactical_moves(position, ply)
        };
        for move_index in 0..end - base {
            let movement = self.staged_move(base, move_index, end);
            if hopeless && context.is_quiet_capture(position, movement) {
                continue;
            }
            // Deeper quiescence plies read this to find their counter move, so
            // leaving a stale entry here would order them by an unrelated move.
            self.path_moves[ply] = movement;
            let undo = position.make_move_unchecked(movement);
            let floor = self.push_path(position.hash(), undo.is_irreversible());
            let score = -self.quiescence(position, -beta, -alpha, ply + 1, control);
            self.pop_path(floor);
            position.unmake_move(undo);
            if self.stop_reason.is_some() {
                self.moves.truncate(base);
                return self.leaf_score(position);
            }
            if score >= beta {
                self.moves.truncate(base);
                return score;
            }
            alpha = alpha.max(score);
        }
        self.moves.truncate(base);
        alpha
    }

    fn enter_node(&mut self, control: &SearchControl<'_>, ply: usize) -> bool {
        if self.stop_reason.is_some() {
            return false;
        }
        if let Some(limit) = control.max_nodes
            && self.stats.nodes >= limit
        {
            self.stop_reason = Some(SearchStopReason::NodeLimit);
            return false;
        }
        // Reading the clock is a call out to the host in the browser build, so
        // at millions of nodes per second it cost more than the search it was
        // guarding. Sampling every CLOCK_INTERVAL nodes keeps the deadline
        // accurate to a fraction of a millisecond.
        if self.stats.nodes.is_multiple_of(CLOCK_INTERVAL) && !self.check_clock(control) {
            return false;
        }
        self.stats.nodes = self.stats.nodes.saturating_add(1);
        self.stats.selective_depth = self
            .stats
            .selective_depth
            .max(u8::try_from(ply).unwrap_or(u8::MAX));
        true
    }

    /// Test the cancellation flag and the deadline. Returns whether to go on.
    #[inline(never)]
    fn check_clock(&mut self, control: &SearchControl<'_>) -> bool {
        if control
            .cancellation
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            self.stop_reason = Some(SearchStopReason::Cancelled);
            return false;
        }
        if control.deadline.is_some_and(engine_deadline_reached) {
            self.stop_reason = Some(SearchStopReason::TimeLimit);
            return false;
        }
        true
    }

    fn probe(&mut self, key: u64) -> Option<TTEntry> {
        self.stats.tt_probes = self.stats.tt_probes.saturating_add(1);
        let result = self.table.probe(key);
        if result.is_some() {
            self.stats.tt_hits = self.stats.tt_hits.saturating_add(1);
        }
        result
    }

    /// Generate every legal move into the arena and score it for ordering.
    ///
    /// Returns the arena range plus the per-node ordering facts, which search
    /// reuses to classify quiet moves without recomputing any of them.
    fn stage_moves(
        &mut self,
        position: &Position,
        tt_move: Move,
        ply: usize,
    ) -> (usize, usize, OrderContext) {
        let base = self.moves.len();
        let stack = &mut self.moves;
        let mut order = 0_u16;
        position.generate_moves_into(position.side_to_move(), &mut |movement| {
            stack.push(ScoredMove {
                movement,
                score: 0,
                order,
            });
            order += 1;
        });
        let end = self.moves.len();
        let context = self.score_staged(position, base, end, tt_move, ply);
        (base, end, context)
    }

    /// The same, restricted to the moves quiescence searches.
    fn stage_tactical_moves(
        &mut self,
        position: &Position,
        ply: usize,
    ) -> (usize, usize, OrderContext) {
        let base = self.moves.len();
        let stack = &mut self.moves;
        let mut order = 0_u16;
        position.generate_tactical_into(position.side_to_move(), &mut |movement| {
            stack.push(ScoredMove {
                movement,
                score: 0,
                order,
            });
            order += 1;
        });
        let end = self.moves.len();
        let context = self.score_staged(position, base, end, Move::NONE, ply);
        (base, end, context)
    }

    fn score_staged(
        &mut self,
        position: &Position,
        base: usize,
        end: usize,
        tt_move: Move,
        ply: usize,
    ) -> OrderContext {
        let context = OrderContext::new(
            position,
            tt_move,
            self.killers[ply.min(MAX_PLY - 1)],
            self.counter_move(ply),
        );
        for index in base..end {
            let movement = self.moves[index].movement;
            let score = context.score(position, movement, &self.history);
            self.moves[index].score = score;
        }
        context
    }

    /// How much to shorten the search of a late move.
    ///
    /// Late moves are searched shallowly first and re-searched at full depth
    /// only if the short search beats alpha, so a reduction that is too large
    /// costs a re-search rather than correctness.
    fn reduction(&self, depth: i16, move_index: usize, quiet: bool, narrow: bool) -> i16 {
        if !self.selective_now {
            // The original timid rule, kept as the control arm.
            return i16::from(quiet && depth >= 3 && move_index >= 4);
        }
        if depth < 3 || move_index < 2 {
            return 0;
        }
        let row = (depth as usize).min(REDUCTION_SIZE - 1);
        let column = move_index.min(REDUCTION_SIZE - 1);
        let mut reduction = i16::from(self.reductions[row][column]);
        if quiet {
            reduction += 1;
        } else {
            // A capture or a goal move changes the position enough that a
            // shallow verdict on it is worth less.
            reduction /= 2;
        }
        if !narrow {
            reduction -= 1;
        }
        reduction.clamp(0, depth - 2)
    }

    /// Narrow a staged root range to `root_filter`, returning the new end.
    ///
    /// The kept moves are compacted to the front of the range so every
    /// ordering helper below works unchanged; each entry keeps its own
    /// generation order, so ties still break the way they would have.
    fn restrict_root(&mut self, base: usize, end: usize) -> usize {
        if self.root_filter.is_empty() {
            return end;
        }
        let filter = core::mem::take(&mut self.root_filter);
        let mut kept = base;
        for index in base..end {
            if filter.contains(&self.moves[index].movement) {
                self.moves.swap(kept, index);
                kept += 1;
            }
        }
        self.moves.truncate(kept);
        self.root_filter = filter;
        kept
    }

    /// Put the arena range in the order search should try it.
    ///
    /// Called with `index == base` it finds the best move in one pass, which is
    /// all that is needed when the node cuts off immediately, and that is the
    /// common case. Called again for the second move it sorts the remainder
    /// once, so a node that does search everything pays `n log n` cheap
    /// integer comparisons instead of a quadratic selection sort.
    fn order_from(&mut self, index: usize, end: usize) {
        if index + 2 >= end {
            if index + 2 == end && self.moves[index + 1].score > self.moves[index].score {
                self.moves.swap(index, index + 1);
            }
            return;
        }
        let mut best = index;
        for candidate in index + 1..end {
            if self.moves[candidate].score > self.moves[best].score {
                best = candidate;
            }
        }
        self.moves.swap(index, best);
    }

    /// Sort a whole arena range by descending ordering score.
    fn sort_range(&mut self, start: usize, end: usize) {
        if start + 1 < end {
            self.moves[start..end].sort_unstable_by(|left, right| {
                right
                    .score
                    .cmp(&left.score)
                    .then_with(|| left.order.cmp(&right.order))
            });
        }
    }

    /// The move to try at `offset` within a staged range.
    fn staged_move(&mut self, base: usize, offset: usize, end: usize) -> Move {
        match offset {
            0 => self.order_from(base, end),
            1 => self.sort_range(base + 1, end),
            _ => {}
        }
        self.moves[base + offset].movement
    }

    /// Record a made move on the repetition path, returning the floor to
    /// restore when it is unmade.
    ///
    /// A capture or a territory claim cannot be undone by later play, so the
    /// repetition scan may stop there.
    fn push_path(&mut self, hash: u64, irreversible: bool) -> usize {
        let previous_floor = self.path_floor;
        self.path_hashes.push(hash);
        if irreversible {
            self.path_floor = self.path_hashes.len() - 1;
        }
        previous_floor
    }

    fn pop_path(&mut self, previous_floor: usize) {
        self.path_hashes.pop();
        self.path_floor = previous_floor;
    }

    /// The quiet move that refuted the move played into this ply.
    fn counter_move(&self, ply: usize) -> Move {
        if ply == 0 {
            return Move::NONE;
        }
        let previous = self.path_moves[ply - 1];
        if previous.is_none() {
            Move::NONE
        } else {
            self.counters[move_slot(previous)]
        }
    }

    fn record_quiet_cutoff(
        &mut self,
        color: Color,
        movement: Move,
        depth: i16,
        ply: usize,
        refuted: &[Move],
    ) {
        let bonus = i32::from(depth).saturating_mul(i32::from(depth)).min(2_000);
        let history = &mut self.history[history_index(color, movement)];
        *history = history.saturating_add(bonus).min(HISTORY_LIMIT);
        // The quiet moves searched before the cutoff were, at this node, worse
        // than the one that cut. Saying so is as informative as the bonus.
        for &failed in refuted {
            let entry = &mut self.history[history_index(color, failed)];
            *entry = entry.saturating_sub(bonus).max(-HISTORY_LIMIT);
        }
        if ply < MAX_PLY && self.killers[ply][0] != movement {
            self.killers[ply][1] = self.killers[ply][0];
            self.killers[ply][0] = movement;
        }
        if ply > 0 {
            let previous = self.path_moves[ply - 1];
            if !previous.is_none() {
                self.counters[move_slot(previous)] = movement;
            }
        }
    }

    /// What one piece is worth in this mode, as the scale for margins.
    ///
    /// Pruning margins have to be expressed in the same units as evaluation,
    /// and those units differ by an order of magnitude between modes, so the
    /// weights supply the scale rather than a hard-coded centipawn.
    const fn material_weight(&self, mode: Mode) -> i32 {
        self.eval_params.material(mode)
    }

    /// The heuristic score of a leaf, clamped to what the rules already prove.
    ///
    /// See [`proof_bounds`]. Clamping at the leaf is enough for the bound to
    /// hold at every ancestor, because both halves of every licence are
    /// inherited: a side that owns an uncapturable piece owns one in every
    /// descendant position, since extinction never reverses, and a tile count
    /// that already matches everything the enemy can still come to own still
    /// matches it after any claim. Terminal nodes are scored before this is
    /// reached, so the clamp can never hide a proven mate -- and it never needs
    /// to, because no terminal in the subtree is a loss for the guaranteed
    /// side.
    fn leaf_score(&self, position: &Position) -> i32 {
        let score = evaluate_with(position, &self.eval_params);
        if !self.rule_proofs {
            return score;
        }
        match proof_bounds(position) {
            Some((lower, upper)) => score.clamp(lower, upper),
            None => score,
        }
    }

    fn is_repetition(&self, hash: u64) -> bool {
        if !self.mode_rules.repetition_draws {
            return false;
        }
        let threshold = REPETITION_THRESHOLD;
        // Two exact restrictions, not approximations. Only the reversible run
        // can hold a repetition, because a capture or a claim can never be
        // undone. And within the plies search made, sides alternate, so only
        // every second entry can share this position's side to move.
        let bound = if self.path_floor == 0 {
            self.path_root
        } else {
            self.path_floor
        };
        let mut seen = 0_usize;
        let mut index = self.path_hashes.len();
        while index > bound {
            index -= 1;
            if self.path_hashes[index] == hash {
                seen += 1;
                if seen >= threshold {
                    return true;
                }
            }
            if index == bound {
                break;
            }
            index -= 1;
        }
        // The caller's prefix carries no alternation guarantee, so every entry
        // counts. It is fixed for the search, hence pre-sorted.
        if self.path_floor == 0 {
            seen += self.prefix_occurrences(hash);
        }
        seen >= threshold
    }

    fn prefix_occurrences(&self, hash: u64) -> usize {
        let start = self.prior_sorted.partition_point(|&value| value < hash);
        let end = self.prior_sorted.partition_point(|&value| value <= hash);
        end - start
    }

    /// What a node whose side has no legal move is worth, from that side's
    /// point of view. The mode decides; see `ModeRules::stalemate_loses`.
    fn no_move_score(&self, ply: usize) -> i32 {
        if self.mode_rules.stalemate_loses {
            -MATE_SCORE + ply_i32(ply)
        } else {
            0
        }
    }

    fn extract_principal_variation(&self, mut position: Position, depth: u8) -> Vec<Move> {
        let mut result = Vec::with_capacity(usize::from(depth));
        let mut seen = Vec::with_capacity(usize::from(depth));
        for _ in 0..depth {
            if seen.contains(&position.hash()) {
                break;
            }
            seen.push(position.hash());
            let Some(entry) = self.table.probe(transposition_key(&position)) else {
                break;
            };
            if entry.best_move.is_none() || !position.is_legal_move(entry.best_move) {
                break;
            }
            result.push(entry.best_move);
            position.make_move_unchecked(entry.best_move);
            if position.outcome().is_some() {
                break;
            }
        }
        result
    }
}

/// A move together with its ordering score, as stored in the search arena.
#[derive(Clone, Copy)]
struct ScoredMove {
    movement: Move,
    score: i32,
    /// Position in generation order, so equal scores keep a stable, and
    /// therefore reproducible, sequence.
    order: u16,
}

/// Everything move ordering needs about a node, gathered once.
///
/// The old ordering function asked the position a dozen questions per move and
/// was called from a comparison callback, so a node with 60 moves evaluated it
/// several hundred times. Every one of those questions has the same answer for
/// every move at a node, or reduces to a bitboard test against a mask, so they
/// are hoisted here.
#[derive(Clone, Copy)]
struct OrderContext {
    tt_move: Move,
    /// The side to move, which selects the history table.
    color: Color,
    killers: [Move; 2],
    counter: Move,
    /// Capture bonus indexed by the destination's piece code, so an empty
    /// destination scores zero with no branch.
    capture_bonus: [i32; PIECE_CODES],
    /// Destinations that win immediately in a boundary-goal mode.
    goal_mask: u128,
    /// Unclaimed tiles, when the mode tracks territory.
    claim_mask: u128,
    /// The last unclaimed tile, which is the only claim that decides a game.
    last_claim_mask: u128,
    /// The shape the side to move is running at, for scoring progress.
    goal: Goal,
    /// Whether anything is won by arriving, so progress is worth scoring.
    advances: bool,
}

impl OrderContext {
    fn new(position: &Position, tt_move: Move, killers: [Move; 2], counter: Move) -> Self {
        let us = position.side_to_move();
        let them = us.other();
        let rules = position.mode().rules();

        // Ordering is the one place engine intuition is safe: a wrong guess
        // costs nodes, never correctness. Captures that permanently remove a
        // threat are therefore tried first even though evaluation is not
        // allowed to assume they are best.
        let mut capture_bonus = [0_i32; PIECE_CODES];
        for kind in PieceKind::ALL {
            let victims = position.piece_bitboard(them, kind);
            if victims == 0 {
                continue;
            }
            let mut bonus = 200_000;
            if victims.count_ones() == 1 {
                bonus += 50_000;
                if position.piece_bitboard(us, kind.prey()) != 0 {
                    bonus += 40_000;
                }
            }
            capture_bonus[them.index() * 3 + kind.index()] = bonus;
        }

        let goal_mask = rules.goal.mask(us);
        let claim_mask = if position.mode() == Mode::TotalWar {
            position.neutral_territory()
        } else {
            0
        };
        Self {
            tt_move,
            color: us,
            killers,
            counter,
            capture_bonus,
            goal_mask,
            claim_mask,
            last_claim_mask: if position.neutral_territory_count() == 1 {
                claim_mask
            } else {
                0
            },
            goal: rules.goal,
            advances: rules.goal.is_some(),
        }
    }

    fn score(&self, position: &Position, movement: Move, history: &[i32]) -> i32 {
        if movement == self.tt_move {
            return 1_000_000;
        }
        let to = movement.to();
        let to_bit = to.bit();
        let mut score = self.capture_bonus[position.piece_code_at(to) as usize];
        if self.goal_mask & to_bit != 0 {
            score += 180_000;
        }
        if self.claim_mask & to_bit != 0 {
            score += 30_000;
        }
        if movement == self.killers[0] {
            score += 90_000;
        } else if movement == self.killers[1] {
            score += 80_000;
        } else if movement == self.counter {
            score += 70_000;
        }
        score += history[history_index(self.color, movement)];
        if self.advances {
            // Moves closer to the goal, by the goal's own distance. Against a
            // rank this is the rank change the old signed-`y` term computed;
            // against a corner it is the Chebyshev step, which is the only one
            // of the two that knows a sideways shuffle along the back rank
            // gains nothing. Ordering, so a wrong answer costs nodes and never
            // correctness -- but a corner race ordered by rank sends the
            // search up the wrong edge first, and that costs a lot of nodes.
            let index = usize::from(movement.from().index());
            let progress = self.goal.distance(self.color, index)
                - self.goal.distance(self.color, usize::from(to.index()));
            score += progress * 1_000;
        }
        score
    }

    /// The [`Position::is_tactical`] answer, from the hoisted masks.
    fn is_tactical(&self, position: &Position, movement: Move) -> bool {
        let to_bit = movement.to().bit();
        position.piece_code_at(movement.to()) != NO_PIECE
            || (self.goal_mask | self.last_claim_mask) & to_bit != 0
    }

    /// A capture that neither wins a game nor removes the last of a kind.
    ///
    /// These are the quiescence moves worth skipping when the position is too
    /// far behind for any single capture to matter; a goal move or an
    /// extinction capture changes the game rather than the material count.
    fn is_quiet_capture(&self, position: &Position, movement: Move) -> bool {
        let to = movement.to();
        (self.goal_mask | self.last_claim_mask) & to.bit() == 0
            && self.capture_bonus[position.piece_code_at(to) as usize] <= 200_000
    }
}

struct SearchControl<'a> {
    deadline: Option<EngineInstant>,
    max_nodes: Option<u64>,
    cancellation: Option<&'a AtomicBool>,
}

#[cfg(not(target_arch = "wasm32"))]
type EngineInstant = Instant;

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy)]
struct EngineInstant(f64);

#[cfg(not(target_arch = "wasm32"))]
fn engine_now() -> EngineInstant {
    Instant::now()
}

#[cfg(target_arch = "wasm32")]
fn engine_now() -> EngineInstant {
    EngineInstant(crate::wasm::monotonic_time_ms())
}

#[cfg(not(target_arch = "wasm32"))]
fn engine_deadline(started: EngineInstant, duration: Option<Duration>) -> Option<EngineInstant> {
    duration.and_then(|value| started.checked_add(value))
}

#[cfg(target_arch = "wasm32")]
fn engine_deadline(started: EngineInstant, duration: Option<Duration>) -> Option<EngineInstant> {
    duration.map(|value| EngineInstant(started.0 + value.as_secs_f64() * 1_000.0))
}

#[cfg(not(target_arch = "wasm32"))]
fn engine_elapsed(started: EngineInstant) -> Duration {
    started.elapsed()
}

#[cfg(target_arch = "wasm32")]
fn engine_elapsed(started: EngineInstant) -> Duration {
    Duration::from_secs_f64(((crate::wasm::monotonic_time_ms() - started.0) / 1_000.0).max(0.0))
}

#[cfg(not(target_arch = "wasm32"))]
fn engine_deadline_reached(deadline: EngineInstant) -> bool {
    Instant::now() >= deadline
}

#[cfg(target_arch = "wasm32")]
fn engine_deadline_reached(deadline: EngineInstant) -> bool {
    crate::wasm::monotonic_time_ms() >= deadline.0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum Bound {
    Empty = 0,
    Exact = 1,
    Lower = 2,
    Upper = 3,
}

impl Bound {
    const fn from_bits(bits: u8) -> Self {
        match bits & 3 {
            1 => Self::Exact,
            2 => Self::Lower,
            3 => Self::Upper,
            _ => Self::Empty,
        }
    }
}

/// One transposition slot, packed into exactly 16 bytes.
///
/// The natural layout padded to 24, which cost a third of the table's entries
/// and straddled cache lines for no reason. The bound needs two bits and the
/// generation only has to differ between searches, so they share a byte, and
/// search depth cannot exceed `MAX_PLY` so it fits in one.
#[derive(Clone, Copy, Debug)]
struct TTEntry {
    key: u64,
    score: i32,
    best_move: Move,
    depth: i8,
    bound_generation: u8,
}

impl TTEntry {
    fn new(
        key: u64,
        score: i32,
        depth: i16,
        bound: Bound,
        best_move: Move,
        generation: u8,
    ) -> Self {
        Self {
            key,
            score,
            best_move,
            // Clamping can only understate a depth, which costs a cutoff and
            // never causes a wrong one.
            depth: depth.clamp(-1, i16::from(i8::MAX)) as i8,
            bound_generation: (generation << 2) | bound as u8,
        }
    }

    const fn bound(self) -> Bound {
        Bound::from_bits(self.bound_generation)
    }

    const fn generation(self) -> u8 {
        self.bound_generation >> 2
    }
}

impl Default for TTEntry {
    fn default() -> Self {
        Self {
            key: 0,
            score: 0,
            best_move: Move::NONE,
            depth: -1,
            bound_generation: Bound::Empty as u8,
        }
    }
}

struct TranspositionTable {
    entries: Vec<TTEntry>,
    mask: usize,
}

impl TranspositionTable {
    fn new(megabytes: usize) -> Self {
        let requested_bytes = megabytes.saturating_mul(1024 * 1024);
        let requested_entries = (requested_bytes / mem::size_of::<TTEntry>()).max(1);
        let len = previous_power_of_two(requested_entries);
        Self {
            entries: vec![TTEntry::default(); len],
            mask: len - 1,
        }
    }

    fn clear(&mut self) {
        self.entries.fill(TTEntry::default());
    }

    fn probe(&self, key: u64) -> Option<TTEntry> {
        let entry = self.entries[key as usize & self.mask];
        (entry.bound() != Bound::Empty && entry.key == key).then_some(entry)
    }

    fn store(&mut self, candidate: TTEntry) {
        let slot = &mut self.entries[candidate.key as usize & self.mask];
        if slot.bound() == Bound::Empty
            || slot.key == candidate.key
            || candidate.depth >= slot.depth
            || candidate.generation() != slot.generation()
        {
            *slot = candidate;
        }
    }
}

fn previous_power_of_two(value: usize) -> usize {
    if value <= 1 {
        1
    } else {
        1_usize << (usize::BITS - 1 - value.leading_zeros())
    }
}

/// Repetition and books identify a board; a search bound also depends on how
/// many quiet plies remain. Reusing a fresh position's score at ply 199 can
/// turn a forced draw into a fictitious win. Far from the limit no search can
/// reach it (even quiescence stops before MAX_PLY), so those clocks share a
/// key. Inside that horizon, retain the exact counter. This keeps ordinary
/// transpositions useful without borrowing a result across a possible draw.
const fn transposition_key(position: &Position) -> u64 {
    // This policy is part of the measured corner-search change. The other
    // modes retain their existing table policy and independently tuned search.
    if !matches!(position.mode().rules().goal, Goal::EnemyCorner) {
        return position.hash();
    }
    let horizon = crate::model::QUIET_PLY_LIMIT - MAX_PLY as u16;
    let clock = position.quiet_plies().saturating_sub(horizon);
    position.hash() ^ (clock as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

fn endgame_score(value: i16, ply: usize) -> i32 {
    match value.cmp(&0) {
        std::cmp::Ordering::Greater => MATE_SCORE - ply_i32(ply) - i32::from(value) + 1,
        std::cmp::Ordering::Less => -MATE_SCORE + ply_i32(ply) - i32::from(value) - 1,
        std::cmp::Ordering::Equal => 0,
    }
}

const fn move_slot(movement: Move) -> usize {
    movement.from().index() as usize * SQUARE_COUNT as usize + movement.to().index() as usize
}

const fn history_index(color: Color, movement: Move) -> usize {
    color.index() * MOVE_SLOTS + move_slot(movement)
}

fn sort_analysis_lines(lines: &mut [AnalysisLine]) {
    lines.sort_unstable_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| {
                left.movement
                    .from()
                    .index()
                    .cmp(&right.movement.from().index())
            })
            .then_with(|| left.movement.to().index().cmp(&right.movement.to().index()))
    });
}

fn analysis_confidence(depth: u8, stable_iterations: u8, score_delta: u32, score: i32) -> u8 {
    if score.abs() >= MATE_THRESHOLD {
        return 99;
    }

    let depth_points = depth.saturating_mul(5).min(45);
    let stability_points = stable_iterations.saturating_mul(8).min(35);
    let score_points = match score_delta {
        0..=25 => 20,
        26..=75 => 15,
        76..=200 => 10,
        201..=500 => 5,
        _ => 0,
    };
    5_u8.saturating_add(depth_points)
        .saturating_add(stability_points)
        .saturating_add(score_points)
        .min(99)
}

/// Whether the rules alone already guarantee `color` at least a draw.
///
/// Two disjoint licences, both read off [`ModeRules`] rather than off a
/// [`Mode`], so a mode added later cannot inherit either by accident:
///
/// - Annihilation is the *only* loss condition. Then an uncapturable piece is
///   the whole proof. `Position::is_immortal_kind` is a theorem -- the capture
///   hierarchy is one 3-cycle and no mode returns a piece to the board -- so
///   the owner's piece count can never reach zero, and stalemate and
///   repetition are draws.
/// - A territory deficit on a filled board is the only *other* loss condition.
///   Then both doors have to be shut at once. The uncapturable piece shuts
///   annihilation. A tile count that already matches everything the enemy can
///   still come to own shuts territory, and it stays shut: a claim either adds
///   to our count or moves a tile out of `neutral` and into theirs, which
///   leaves `theirs + neutral` exactly where it was. So the inequality is
///   inherited by every descendant and can never be repaired.
///
/// This proves "cannot lose" and never "wins". A majority can go uncashed: an
/// uncapturable piece can still be walled in by pieces it does not prey on,
/// and a stalemate or a repetition ends the game at zero with neutral tiles
/// still on the board.
#[must_use]
fn is_guaranteed_at_least_a_draw(position: &Position, rules: ModeRules, color: Color) -> bool {
    if rules.immortality_prevents_loss() {
        return position.has_immortal_piece(color);
    }
    if rules.territory_majority_prevents_loss() {
        // The tile counts are three array reads and come first deliberately:
        // on all but a handful of leaves nobody holds an unbeatable count, and
        // then the scan for an uncapturable piece never runs at all.
        //
        // `>=` rather than `>`, because a level fill is a draw and a draw is
        // not a loss. On this board that is theoretical -- 81 is odd, so the
        // test is `territory_count(color) >= 41` and equality is unreachable.
        return position.territory_count(color)
            >= position.territory_count(color.other()) + position.neutral_territory_count()
            && position.has_immortal_piece(color);
    }
    false
}

/// Whether the rules already decide this position as a draw.
///
/// When neither side can lose, the state space is finite and repetition is a
/// draw, so the game value is exactly zero. This is a proof, not a heuristic,
/// and it prunes the entire subtree.
///
/// Only mutual immortality reaches it. Two territory guarantees cannot hold at
/// once: each needs more than half the board and there is only one board.
#[must_use]
fn is_proven_draw(position: &Position) -> bool {
    let rules = position.mode().rules();
    is_guaranteed_at_least_a_draw(position, rules, Color::Red)
        && is_guaranteed_at_least_a_draw(position, rules, Color::Blue)
}

/// Score bounds implied by the rules alone, from the side-to-move view.
///
/// Returns `None` when the rules prove nothing, which is the common case.
#[must_use]
fn proof_bounds(position: &Position) -> Option<(i32, i32)> {
    let rules = position.mode().rules();
    if !rules.immortality_prevents_loss() && !rules.territory_majority_prevents_loss() {
        // No rule set here licenses a bound, so this stays one branch ahead of
        // anything that reads the board.
        return None;
    }
    let us = position.side_to_move();
    let we_are_safe = is_guaranteed_at_least_a_draw(position, rules, us);
    let they_are_safe = is_guaranteed_at_least_a_draw(position, rules, us.other());
    match (we_are_safe, they_are_safe) {
        (true, true) => Some((0, 0)),
        (true, false) => Some((0, INFINITY)),
        (false, true) => Some((-INFINITY, 0)),
        (false, false) => None,
    }
}

fn terminal_score(outcome: GameOutcome, side_to_move: Color, ply: usize) -> i32 {
    match outcome.winner {
        None => 0,
        Some(winner) if winner == side_to_move => MATE_SCORE - ply_i32(ply),
        Some(_) => -MATE_SCORE + ply_i32(ply),
    }
}

fn score_to_tt(score: i32, ply: usize) -> i32 {
    if score >= MATE_THRESHOLD {
        score + ply_i32(ply)
    } else if score <= -MATE_THRESHOLD {
        score - ply_i32(ply)
    } else {
        score
    }
}

fn score_from_tt(score: i32, ply: usize) -> i32 {
    if score >= MATE_THRESHOLD {
        score - ply_i32(ply)
    } else if score <= -MATE_THRESHOLD {
        score + ply_i32(ply)
    } else {
        score
    }
}

fn ply_i32(ply: usize) -> i32 {
    i32::try_from(ply).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use crate::evaluation::{EvalParams, evaluate_with};
    use crate::model::{Color, EndReason, Mode, Move, QUIET_PLY_LIMIT, Square};
    use crate::position::Position;

    use super::{
        SearchLimits, SearchStopReason, Searcher, build_reductions, previous_power_of_two,
        proof_bounds,
    };

    /// A Total War board where Red holds an unbeatable tile count.
    ///
    /// Red's lone Rock is the whole army. Blue's pieces are Scissors, which
    /// Rock preys on and which cannot prey on Rock, so as long as Blue owns no
    /// Paper the Rock can never be captured and Red can never be annihilated.
    /// The tile split is the parameter, so one caller can straddle the
    /// majority and another can break the conjunction.
    fn total_war_majority(board: &str, territory: &str) -> Position {
        Position::from_fen(Mode::TotalWar, &format!("{board} r {territory}"))
            .expect("position must parse")
    }

    const EIGHT_SCISSORS_AND_A_LONE_ROCK: &str = "SSSSSSSS1/9/9/9/r8/9/9/9/9";
    const RED_HOLDS_FORTY_ONE: &str =
        "rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrrr4/bbbbbbbbb/bbbbbbbbb/bbbbbbbbb/bbb6";
    const RED_HOLDS_FORTY: &str =
        "rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrrrrrrr/rrrr5/bbbbbbbbb/bbbbbbbbb/bbbbbbbbb/bbbb5";

    #[test]
    fn a_permanent_territory_majority_with_an_immortal_piece_is_never_scored_as_losing() {
        // Forty-one tiles cannot be beaten by any split of the remaining ten,
        // and the lone Rock cannot be captured, so both of Total War's loss
        // conditions are shut and the worst Red can do is draw.
        let position = total_war_majority(EIGHT_SCISSORS_AND_A_LONE_ROCK, RED_HOLDS_FORTY_ONE);
        assert_eq!(position.territory_count(Color::Red), 41);
        assert_eq!(position.territory_count(Color::Blue), 30);
        assert_eq!(position.neutral_territory_count(), 10);
        assert!(position.has_immortal_piece(Color::Red));
        assert!(
            evaluate_with(&position, &EvalParams::DEFAULT) < 0,
            "the raw heuristic must dislike this position for the clamp to matter"
        );

        let mut searcher = Searcher::new(1);
        let result = searcher.search(position, depth(4));
        assert!(
            result.score >= 0,
            "search returned {} for a provably unloseable position",
            result.score
        );
    }

    #[test]
    fn one_tile_short_of_a_majority_gets_no_bound() {
        // The boundary test, and the one that catches an off-by-one between
        // `>` and `>=`: forty tiles against thirty-one and ten neutral is
        // exactly one claim away from unbeatable, and until that claim lands
        // Blue can still reach forty-one.
        let position = total_war_majority(EIGHT_SCISSORS_AND_A_LONE_ROCK, RED_HOLDS_FORTY);
        assert_eq!(position.territory_count(Color::Red), 40);
        assert_eq!(position.territory_count(Color::Blue), 31);
        assert_eq!(position.neutral_territory_count(), 10);
        assert!(position.has_immortal_piece(Color::Red));
        assert_eq!(proof_bounds(&position), None);
    }

    #[test]
    fn a_territory_majority_without_an_immortal_piece_gets_no_bound() {
        // The other half of the conjunction. One Blue Paper is enough to hunt
        // the Rock forever, so the tile count buys nothing -- this is the
        // suite's whole `race-*` family in miniature, where Red holds up to
        // sixty-four tiles and still loses.
        let position = total_war_majority("PSSSSSSS1/9/9/9/r8/9/9/9/9", RED_HOLDS_FORTY_ONE);
        assert_eq!(position.territory_count(Color::Red), 41);
        assert!(!position.has_immortal_piece(Color::Red));
        assert_eq!(proof_bounds(&position), None);
    }

    #[test]
    fn the_territory_bound_is_symmetric_under_mirroring() {
        // `mirrored` swaps the colours *and* the side to move, so a bound
        // stated from the mover's view must come back literally equal. This is
        // the test that catches reading the wrong colour's tile count.
        for territory in [RED_HOLDS_FORTY_ONE, RED_HOLDS_FORTY] {
            for board in [EIGHT_SCISSORS_AND_A_LONE_ROCK, "PSSSSSSS1/9/9/9/r8/9/9/9/9"] {
                let position = total_war_majority(board, territory);
                assert_eq!(
                    proof_bounds(&position),
                    proof_bounds(&position.mirrored()),
                    "bound differs under mirroring"
                );
            }
        }
    }

    #[test]
    fn two_territory_guarantees_cannot_hold_at_once() {
        // Why `is_proven_draw`'s exact-zero arm stays unreachable in Total
        // War: both sides would need more than half of one board.
        let position = total_war_majority(EIGHT_SCISSORS_AND_A_LONE_ROCK, RED_HOLDS_FORTY_ONE);
        let red = position.territory_count(Color::Red);
        let blue = position.territory_count(Color::Blue);
        let neutral = position.neutral_territory_count();
        assert!(red >= blue + neutral);
        assert!(blue < red + neutral);
    }

    fn depth(depth: u8) -> SearchLimits {
        SearchLimits {
            max_depth: depth,
            max_nodes: None,
            move_time: None,
        }
    }

    fn corner_quiescence(fen: &str, alpha: i32, beta: i32) -> i32 {
        let mut position = Position::from_fen(Mode::Intransitive, fen).unwrap();
        let mut searcher = Searcher::new(1);
        searcher.set_rule_proofs(false); // exercise the search, independently of the tablebase
        searcher.begin_search(&[], &position);
        let control = super::SearchControl {
            deadline: None,
            max_nodes: Some(20_000),
            cancellation: None,
        };
        let result = searcher.quiescence(&mut position, alpha, beta, 0, &control);
        assert!(searcher.stop_reason.is_none());
        result
    }

    #[test]
    fn a_goal_win_precedes_stand_pat_even_in_a_narrow_window() {
        // Rock captures into a1. The Paper on a2 attacks that square, but
        // arrival wins before it can recapture.
        let fen = "S8/Pp7/9/9/9/9/9/9/9 r";
        // Paper cannot capture Scissors; switch the runner to Rock.
        let fen = fen.replace("Pp", "Pr");
        assert_eq!(corner_quiescence(&fen, -1001, -1000), super::MATE_SCORE - 1);
    }

    #[test]
    fn quiescence_searches_a_quiet_corner_plug() {
        // Neither Scissors can take the other. Red must step h9-i9 to stop
        // S h8-i9. Capture-only quiescence cannot see this defense.
        let fen = "9/9/9/9/9/9/9/7S1/7s1 r";
        let score = corner_quiescence(fen, -super::INFINITY, super::INFINITY);
        assert!(score.abs() < super::MATE_THRESHOLD, "{score}");
        let position = Position::from_fen(Mode::Intransitive, fen).unwrap();
        let result = Searcher::new(1).search(position, depth(1));
        assert_eq!(result.best_move.unwrap().notation(), "h9-i9");
    }

    #[test]
    fn an_unanswerable_corner_threat_is_a_loss_at_the_horizon() {
        // A Rock plug is no defense against Paper. Remote extra material
        // cannot buy a pass, even if the static score clears beta.
        let fen = "9/9/9/3rr4/3r5/9/9/7P1/7r1 r";
        assert_eq!(
            corner_quiescence(fen, -super::INFINITY, super::INFINITY),
            -super::MATE_SCORE + 2,
        );
        assert!(corner_quiescence(fen, -101, -100) <= -100);
    }

    #[test]
    fn transpositions_respect_the_no_capture_clock() {
        let fresh = Position::from_fen(Mode::Intransitive, "9/9/9/9/4r1R2/9/9/9/9 r").unwrap();
        let mut old = fresh;
        for _ in 0..49 {
            for movement in ["e5-e6", "g5-g6", "e6-e5", "g6-g5"] {
                old.make_move(Move::parse(movement).unwrap()).unwrap();
            }
        }
        assert_eq!(fresh.hash(), old.hash(), "the board and side are identical");
        assert_eq!(old.quiet_plies(), 196);
        assert_ne!(
            super::transposition_key(&fresh),
            super::transposition_key(&old)
        );
        let mut searcher = Searcher::new(4);
        let early = searcher.search(fresh, depth(8));
        assert!(early.score >= super::MATE_THRESHOLD);
        let late = searcher.search(old, depth(8));
        assert_eq!(late.score, 0, "neither runner can arrive before the draw");
        assert_eq!(late.score, Searcher::new(4).search(old, depth(8)).score);
    }

    #[test]
    fn previous_power_is_safe_for_table_sizes() {
        assert_eq!(previous_power_of_two(0), 1);
        assert_eq!(previous_power_of_two(1), 1);
        assert_eq!(previous_power_of_two(2), 2);
        assert_eq!(previous_power_of_two(3), 2);
        assert_eq!(previous_power_of_two(9), 8);
    }

    #[test]
    fn finds_legal_starting_move_in_every_mode() {
        let mut searcher = Searcher::new(1);
        for mode in Mode::ALL {
            let position = Position::starting(mode);
            let result = searcher.search(position, depth(2));
            assert_eq!(result.stop_reason, SearchStopReason::DepthLimit);
            assert!(
                result
                    .best_move
                    .is_some_and(|movement| position.is_legal_move(movement)),
                "{mode} must return a legal move"
            );
            assert_eq!(result.completed_depth, 2);
        }
    }

    #[test]
    fn analysis_returns_three_ranked_legal_moves() {
        let position = Position::starting(Mode::TotalWar);
        let mut searcher = Searcher::new(1);
        let result = searcher.analyze(position, depth(2), 3);

        assert_eq!(result.lines.len(), 3);
        assert_eq!(result.completed_depth, 2);
        assert!(
            result
                .lines
                .windows(2)
                .all(|lines| lines[0].score >= lines[1].score)
        );
        assert!(
            result
                .lines
                .iter()
                .all(|line| position.is_legal_move(line.movement))
        );
        assert_eq!(result.score, result.lines[0].score);
    }

    #[test]
    fn analysis_streams_only_complete_depths_with_principal_variations() {
        let position = Position::starting(Mode::TotalWar);
        let mut searcher = Searcher::new(1);
        let mut updates = Vec::new();
        let result = searcher.analyze_with_updates(position, depth(3), 2, |update| {
            updates.push(update.clone());
        });

        assert_eq!(
            updates
                .iter()
                .map(|update| update.completed_depth)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(
            updates
                .windows(2)
                .all(|pair| pair[0].stats.nodes < pair[1].stats.nodes)
        );
        for update in &updates {
            for line in &update.lines {
                assert_eq!(line.principal_variation.first(), Some(&line.movement));
                assert!(line.principal_variation.len() <= usize::from(update.completed_depth));
            }
        }
        let last = updates.last().expect("depth updates must be emitted");
        assert_eq!(result.completed_depth, last.completed_depth);
        assert_eq!(result.lines, last.lines);
        assert_eq!(result.confidence, last.confidence);
    }

    #[test]
    fn single_pv_uses_the_faster_root_search() {
        let position = Position::starting(Mode::TotalWar);
        let mut single_searcher = Searcher::new(1);
        let single = single_searcher.analyze(position, depth(3), 1);
        let mut multi_searcher = Searcher::new(1);
        let multi = multi_searcher.analyze(position, depth(3), 3);

        assert_eq!(single.lines.len(), 1);
        assert!(single.stats.nodes < multi.stats.nodes);
        assert_eq!(
            single.lines[0].principal_variation.first(),
            Some(&single.lines[0].movement)
        );
    }

    #[test]
    fn partial_analysis_iteration_keeps_last_completed_update() {
        let position = Position::starting(Mode::TotalWar);
        let mut searcher = Searcher::new(1);
        let mut depths = Vec::new();
        let result = searcher.analyze_with_updates(
            position,
            SearchLimits {
                max_depth: 20,
                max_nodes: Some(50),
                move_time: None,
            },
            1,
            |update| depths.push(update.completed_depth),
        );

        assert_eq!(result.stop_reason, SearchStopReason::NodeLimit);
        assert_eq!(result.completed_depth, 1);
        assert_eq!(depths, vec![1]);
        assert_eq!(result.stats.nodes, 50);
    }

    #[test]
    fn finds_immediate_annihilation_win() {
        let rows = [
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
        let position =
            Position::from_rows(Mode::TotalWar, Color::Red, &rows).expect("position must parse");
        let mut searcher = Searcher::new(1);
        let result = searcher.search(position, depth(2));
        let best = result.best_move.expect("winning move must exist");
        assert_eq!((best.to().x(), best.to().y()), (5, 4));
        assert!(result.score > 29_000);
    }

    #[test]
    fn finds_immediate_infiltration_win() {
        let rows = [
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
        let position = Position::from_rows(Mode::Infiltration, Color::Red, &rows)
            .expect("position must parse");
        let mut searcher = Searcher::new(1);
        let result = searcher.search(position, depth(2));
        let best = result.best_move.expect("winning move must exist");
        assert_eq!(best.to().y(), 0);
        assert!(result.score > 29_000);
    }

    #[test]
    fn prefers_the_capture_that_makes_its_own_pieces_immortal() {
        // Red can win a piece two ways. Taking Blue's only Rock with the Paper
        // also makes the Red Scissors permanently uncapturable; taking one of
        // Blue's two Papers with the Scissors wins the same material and
        // nothing else.
        let rows = [
            "........P",
            "...R.....",
            "...p.....",
            ".........",
            ".......S.",
            ".........",
            ".....P...",
            ".....s...",
            ".r.......",
        ];
        let position =
            Position::from_rows(Mode::TotalWar, Color::Red, &rows).expect("position must parse");
        let mut searcher = Searcher::new(1);
        let result = searcher.search(position, depth(2));
        let best = result.best_move.expect("a capture must exist");
        assert_eq!(
            (
                (best.from().x(), best.from().y()),
                (best.to().x(), best.to().y())
            ),
            ((3, 2), (3, 1))
        );
    }

    #[test]
    fn hard_node_limit_is_respected() {
        let position = Position::starting(Mode::TotalWar);
        let mut searcher = Searcher::new(1);
        let result = searcher.search(
            position,
            SearchLimits {
                max_depth: 20,
                max_nodes: Some(100),
                move_time: None,
            },
        );
        assert_eq!(result.stop_reason, SearchStopReason::NodeLimit);
        assert!(result.stats.nodes <= 100);
        assert!(result.best_move.is_some());
    }

    #[test]
    fn cancellation_is_observed_before_search() {
        let position = Position::starting(Mode::TotalWar);
        let cancellation = AtomicBool::new(true);
        let mut searcher = Searcher::new(1);
        let result = searcher.search_with_context(position, depth(8), &[], Some(&cancellation));
        assert_eq!(result.stop_reason, SearchStopReason::Cancelled);
        assert_eq!(result.stats.nodes, 0);
    }

    /// A root the host says has already occurred twice is not a drawn game, so
    /// the search has to answer it with a move like any other.
    ///
    /// The inverse of what this used to assert, and it inverts again the day
    /// `ModeRules::repetition_draws` does. The scan that would find the repeat
    /// is not switched off with the rule and is still covered on its own, by
    /// `repetition_detection_matches_a_full_path_scan`.
    #[test]
    fn a_third_known_occurrence_is_played_on() {
        for mode in Mode::ALL {
            let position = Position::starting(mode);
            let mut searcher = Searcher::new(1);
            let result = searcher.search_with_context(
                position,
                depth(4),
                &[position.hash(), position.hash()],
                None,
            );
            assert_eq!(
                result.stop_reason == SearchStopReason::Repetition,
                mode.rules().repetition_draws,
                "{mode} stopped on a repeat it does not draw on",
            );
            assert!(result.best_move.is_some(), "{mode} answered with no move");
        }
    }

    /// A move that walks into a twice-seen position is scored on its merits,
    /// because reaching one is not a draw.
    ///
    /// Stated as "the history changes nothing" rather than "the score is not
    /// zero": zero is a perfectly ordinary score for a level position, so an
    /// assertion about the number would pass for the wrong reason. What has to
    /// hold is that the same move is worth the same with and without a history
    /// saying it repeats.
    #[test]
    fn analysis_scores_a_third_occurrence_on_its_merits() {
        // Infiltration, not Total War: claiming a tile is irreversible and
        // would stop the repetition scan before it reached the repeat.
        let position = Position::starting(Mode::Infiltration);
        assert!(
            !Mode::Infiltration.rules().repetition_draws,
            "this test states what happens when the rule is off",
        );
        let movement = position.legal_moves().as_slice()[0];
        let mut repeated = position;
        repeated
            .make_move(movement)
            .expect("generated move must be legal");

        let score_with = |history: &[u64]| {
            let mut searcher = Searcher::new(1);
            let result = searcher.analyze_with_context(
                position,
                depth(1),
                position.legal_moves().len(),
                history,
            );
            result
                .lines
                .iter()
                .find(|line| line.movement == movement)
                .expect("repeating move must be analyzed")
                .score
        };
        assert_eq!(
            score_with(&[repeated.hash(), repeated.hash()]),
            score_with(&[]),
        );
    }

    /// The same immobile picture in two modes, searched by one searcher, which
    /// is what a session does. The score has to follow the mode rather than
    /// whatever the previous search left behind.
    #[test]
    fn a_no_move_node_is_scored_by_its_own_modes_rule() {
        // A Red rock ringed by Blue papers can neither move nor capture,
        // because paper beats rock. The papers are placed off both goal
        // corners so neither side has already won in Intransitive.
        let rows = [
            ".........",
            ".........",
            "...PPP...",
            "...PrP...",
            "...PPP...",
            ".........",
            ".........",
            ".........",
            ".........",
        ];
        let mut searcher = Searcher::new(1);

        let drawn =
            Position::from_rows(Mode::TotalWar, Color::Red, &rows).expect("position must parse");
        assert!(drawn.legal_moves().is_empty());
        assert!(drawn.is_stalemate());
        let draw = searcher.search(drawn, depth(2));
        assert_eq!(draw.stop_reason, SearchStopReason::NoLegalMove);
        assert_eq!(draw.score, 0);
        assert!(drawn.adjudicate().expect("terminal").winner.is_none());

        let lost = Position::from_rows(Mode::Intransitive, Color::Red, &rows)
            .expect("position must parse");
        let loss = searcher.search(lost, depth(2));
        assert_eq!(loss.stop_reason, SearchStopReason::NoLegalMove);
        assert!(loss.score < -29_000, "score was {}", loss.score);
        assert_eq!(
            lost.adjudicate().expect("terminal").winner,
            Some(Color::Blue),
        );

        // And back again, so the searcher is not merely sticky.
        assert_eq!(searcher.search(drawn, depth(2)).score, 0);
    }

    /// Repeating a position is play in every mode, so the search must not hand
    /// a repeat back as half a point in any of them.
    ///
    /// Written against `ModeRules::repetition_draws` rather than against its
    /// current value, so it is still the right test the day a mode answers
    /// otherwise: what it pins is that the search agrees with the mode. That
    /// agreement is the whole reason the field exists — a search that folded a
    /// repeat into a draw score where the rules do not would steer toward one,
    /// and a search that did not where they do would steer away.
    #[test]
    fn the_search_scores_a_repeat_the_way_its_mode_does() {
        for mode in Mode::ALL {
            let position = Position::starting(mode);
            let mut searcher = Searcher::new(1);
            let history = [position.hash(), position.hash()];
            let result = searcher.search_with_context(position, depth(2), &history, None);
            assert_eq!(
                result.stop_reason == SearchStopReason::Repetition,
                mode.rules().repetition_draws,
                "{mode} scored a third occurrence wrongly",
            );
        }
    }

    /// The bound on a game's length, and the one no mode may lift.
    ///
    /// Played out rather than asserted on a hand-built position, because
    /// `quiet_plies` is maintained by `make_move` and the point is that two
    /// hundred quiet plies *arrive* at zero however they were played. Two
    /// pieces walk loops of eight and seven, so the board only comes back to a
    /// position it has been in twice before after 4 x lcm(8, 7) = 224 plies.
    /// That is past the limit, which means this test says the same thing
    /// whether or not a mode draws on repetition — on a two-square shuffle it
    /// would be about the wrong rule.
    #[test]
    fn a_hundred_moves_with_no_capture_is_a_draw_in_every_mode() {
        const BLUE_LOOP: [(u8, u8); 8] = [
            (1, 3),
            (2, 3),
            (3, 3),
            (3, 4),
            (3, 5),
            (2, 5),
            (1, 5),
            (1, 4),
        ];
        const RED_LOOP: [(u8, u8); 7] = [(6, 3), (7, 3), (8, 3), (8, 4), (8, 5), (7, 5), (6, 4)];
        // Two pieces in the middle of an empty board, clear of Infiltration's
        // home ranks and Intransitive's corners so no win condition fires, and
        // five files apart so they never come near each other. Rows run from
        // Blue's home boundary upward, and `R` is Blue's rock.
        const WALKING_ROOM: [&str; 9] = [
            ".........",
            ".........",
            ".........",
            ".R....r..",
            ".........",
            ".........",
            ".........",
            ".........",
            ".........",
        ];
        let square = |(x, y): (u8, u8)| Square::from_xy(x, y).expect("valid square");

        for mode in Mode::ALL {
            let mut position = Position::from_rows(mode, crate::FIRST_TO_MOVE, &WALKING_ROOM)
                .expect("position must parse");
            for ply in 0..QUIET_PLY_LIMIT {
                assert!(
                    position.adjudicate().is_none(),
                    "{mode} ended after {ply} quiet plies",
                );
                assert_eq!(position.quiet_plies(), ply);
                let (loop_squares, step) = if ply % 2 == 0 {
                    (&BLUE_LOOP[..], usize::from(ply / 2) % BLUE_LOOP.len())
                } else {
                    (&RED_LOOP[..], usize::from(ply / 2) % RED_LOOP.len())
                };
                let movement = Move::new(
                    square(loop_squares[step]),
                    square(loop_squares[(step + 1) % loop_squares.len()]),
                );
                position.make_move(movement).expect("walk must be legal");
            }
            assert_eq!(position.quiet_plies(), QUIET_PLY_LIMIT);
            let outcome = position.adjudicate().expect("the game must be over");
            assert_eq!(outcome.reason, EndReason::NoCapture, "{mode}");
            assert!(outcome.winner.is_none(), "{mode}");

            let mut searcher = Searcher::new(1);
            let result = searcher.search(position, depth(2));
            assert_eq!(result.stop_reason, SearchStopReason::NoCapture, "{mode}");
            assert_eq!(result.score, 0, "{mode}");
        }
    }

    #[test]
    fn territory_modes_do_not_get_the_immortality_guarantee() {
        // The same material picture in Total War is genuinely lost: Blue owns
        // the board and wins on territory even though Red cannot be captured.
        let mut pieces = [[0_u128; 3]; 2];
        pieces[Color::Red.index()][crate::model::PieceKind::Rock.index()] =
            crate::model::Square::from_xy(4, 4)
                .expect("valid square")
                .bit();
        pieces[Color::Blue.index()][crate::model::PieceKind::Scissors.index()] =
            crate::model::Square::from_xy(0, 0)
                .expect("valid square")
                .bit();
        let blue_territory = crate::BOARD_MASK
            & !crate::model::Square::from_xy(4, 4)
                .expect("valid square")
                .bit();
        let position = Position::from_bitboards(
            Mode::TotalWar,
            Color::Red,
            pieces,
            [
                crate::model::Square::from_xy(4, 4)
                    .expect("valid square")
                    .bit(),
                blue_territory,
            ],
            40,
        )
        .expect("position must be valid");
        assert!(!position.can_be_annihilated(Color::Red));
        assert!(!position.mode().rules().immortality_prevents_loss());

        let mut searcher = Searcher::new(1);
        let result = searcher.search(position, depth(3));
        assert!(
            result.score < 0,
            "Total War must still allow a territory loss, got {}",
            result.score
        );
    }

    /// The bounded repetition scan must count exactly what a full scan counts.
    ///
    /// `is_repetition` stops at the last irreversible move and, above it, looks
    /// at every second entry. Both restrictions are claimed to be exact rather
    /// than approximate, so this compares them against the naive count along
    /// real playouts, with a caller-supplied prefix that has no alternation
    /// guarantee of its own.
    #[test]
    fn repetition_detection_matches_a_full_path_scan() {
        fn naive(path: &[u64], hash: u64) -> bool {
            path.iter().filter(|&&entry| entry == hash).count() >= 3
        }

        let mut state = 0x51e7_a3c1_0f2b_9d45_u64;
        let mut next = move || {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut value = state;
            value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            value ^ (value >> 31)
        };

        let mut checked = 0_usize;
        // Every mode, with the rule forced on for the length of the test.
        //
        // `is_repetition` answers false outright where the mode does not draw
        // on a repeat, and no mode does now — so filtering on the rule, which
        // this used to do, would leave nothing to check. The rule and the scan
        // are separate things: whether a repeat is worth half a point is the
        // mode's to say, and *finding* one in a bounded window is this code's,
        // and it has to keep working for the day the rule comes back.
        for mode in Mode::ALL {
            for round in 0..12 {
                let mut position = Position::starting(mode);
                // Half the runs carry a prefix, so both the pre-sorted history
                // path and the bare search path get exercised.
                let prefix: Vec<u64> = if round % 2 == 0 {
                    Vec::new()
                } else {
                    vec![position.hash(), position.hash() ^ 1, position.hash()]
                };
                let mut searcher = Searcher::new(1);
                searcher.begin_search(&prefix, &position);
                searcher.mode_rules.repetition_draws = true;

                let mut reference = prefix.clone();
                reference.push(position.hash());
                let mut undos = Vec::new();

                for _ in 0..48 {
                    let legal = position.legal_moves();
                    if legal.is_empty() || position.outcome().is_some() {
                        break;
                    }
                    let movement = legal.as_slice()[(next() as usize) % legal.len()];
                    let undo = position.make_move_unchecked(movement);
                    let floor = searcher.push_path(position.hash(), undo.is_irreversible());
                    reference.push(position.hash());
                    undos.push((undo, floor));

                    assert_eq!(
                        searcher.is_repetition(position.hash()),
                        naive(&reference, position.hash()),
                        "{mode}: bounded scan disagreed with the full scan"
                    );
                    checked += 1;
                }

                // Unwinding must restore the floor exactly, or a later sibling
                // would scan the wrong range.
                while let Some((undo, floor)) = undos.pop() {
                    reference.pop();
                    searcher.pop_path(floor);
                    position.unmake_move(undo);
                    assert_eq!(
                        searcher.is_repetition(position.hash()),
                        naive(&reference, position.hash())
                    );
                }
                assert_eq!(searcher.path_floor, 0);
            }
        }
        assert!(checked > 500, "only {checked} path states were compared");
    }

    /// Restricting the root must answer about exactly the moves it was asked
    /// about, in score order.
    ///
    /// This is what makes a move grade meaningful. Asking one search about
    /// {best move, played move} scores both under identical conditions —
    /// same window, same table, same ordering — so their difference is the
    /// loss and nothing else. Two separate searches would each be exact and
    /// still differ by a few points, because a fixed depth is only fixed
    /// until the transposition table starts answering from a deeper one.
    #[test]
    fn a_restricted_root_answers_about_exactly_the_requested_moves() {
        for mode in Mode::ALL {
            let position = Position::starting(mode);
            let legal = position.legal_moves();
            let reference = Searcher::new(4).analyze(position, depth(5), legal.len());
            let best = reference.lines[0].movement;
            let worst = reference.lines.last().expect("lines").movement;
            assert_ne!(best, worst);

            let paired =
                Searcher::new(4).analyze_root_moves(position, depth(5), 2, &[], &[worst, best]);
            assert_eq!(paired.lines.len(), 2, "{mode}");
            assert_eq!(
                paired.lines[0].movement, best,
                "{mode}: the stronger move did not come first"
            );
            assert_eq!(paired.lines[1].movement, worst, "{mode}");
            assert!(
                paired.lines[0].score >= paired.lines[1].score,
                "{mode}: lines are not in score order"
            );

            let alone = Searcher::new(4).analyze_root_moves(position, depth(5), 4, &[], &[worst]);
            assert_eq!(alone.lines.len(), 1, "{mode}: a filter of one gave more");
            assert_eq!(alone.lines[0].movement, worst, "{mode}");
            assert_eq!(
                alone.lines[0].principal_variation.first().copied(),
                Some(worst),
                "{mode}: the variation does not start with its own move"
            );
        }
    }

    /// A restricted search saw only part of the root, so its verdict is not
    /// this position's verdict. Storing it would let a later search take a
    /// transposition cutoff on a number that was never true.
    #[test]
    fn a_restricted_root_leaves_no_entry_behind() {
        let position = Position::starting(Mode::TotalWar);
        let legal = position.legal_moves();
        let full = Searcher::new(4).analyze(position, depth(4), legal.len());
        let worst = full.lines.last().expect("a legal position has lines");
        assert_ne!(worst.movement, full.lines[0].movement);

        let mut searcher = Searcher::new(4);
        let restricted = searcher.analyze_root_moves(position, depth(4), 1, &[], &[worst.movement]);
        assert_eq!(restricted.lines[0].movement, worst.movement);
        assert!(
            searcher.table.probe(position.hash()).is_none(),
            "a restricted analysis stored a root verdict it is not entitled to"
        );

        // The restriction is spent, not sticky: the next analysis sees the
        // whole position again.
        let after = searcher.analyze(position, depth(4), legal.len());
        assert_eq!(after.lines.len(), full.lines.len());
    }

    /// Asking about a move the position does not have is a question, not a
    /// crash, and the answer must be distinguishable from "no good move".
    #[test]
    fn a_restricted_root_reports_an_illegal_request() {
        let position = Position::starting(Mode::TotalWar);
        let illegal = Move::new(
            Square::new(0).expect("corner"),
            Square::new(80).expect("corner"),
        );
        let result = Searcher::new(4).analyze_root_moves(position, depth(4), 1, &[], &[illegal]);
        assert!(result.lines.is_empty());
        assert_eq!(result.stop_reason, SearchStopReason::NoLegalMove);
    }

    /// Reductions must grow with both depth and lateness, and vanish at zero.
    #[test]
    fn the_reduction_table_is_monotone_in_depth_and_move_index() {
        let table = build_reductions(super::DEFAULT_REDUCTION_SCALE);
        for depth in 1..super::REDUCTION_SIZE {
            for index in 1..super::REDUCTION_SIZE {
                if depth > 1 {
                    assert!(table[depth][index] >= table[depth - 1][index]);
                }
                if index > 1 {
                    assert!(table[depth][index] >= table[depth][index - 1]);
                }
            }
        }
        assert_eq!(table[2][2], 0, "a shallow early move is never reduced");
        assert!(table[32][32] > table[8][8]);
        assert!(
            build_reductions(0)
                .iter()
                .all(|row| row.iter().all(|&r| r == 0))
        );
    }
}
