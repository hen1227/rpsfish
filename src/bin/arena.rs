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

//! Candidate-versus-baseline measurement.
//!
//! The engine's weights are guesses until something adjudicates them. This
//! binary is that adjudicator: paired openings, fixed node budgets, per-mode
//! reporting, and a sequential probability ratio test so a change is accepted
//! or rejected by evidence rather than by whoever wrote it.
//!
//! ```text
//! cargo run --release --bin arena -- match --games 400 --candidate annihilation_scarcity_1=25
//! cargo run --release --bin arena -- tune --mode annihilation --iterations 60
//! ```

use std::env;
use std::fmt::Write as _;
use std::process::ExitCode;
use std::sync::Mutex;
use std::thread;

use rpsfish::selfplay::{
    DEFAULT_MAX_PLIES, EngineConfig, GameResult, MatchRunner, Random, balanced_opening,
};
use rpsfish::{EvalParams, Mode, SearchTuning};

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    match run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            eprintln!();
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
usage:
  arena match [options]            paired candidate-vs-baseline match with SPRT
  arena tune  [options]            SPSA weight tuning against a fixed baseline
  arena show                       print the current default weights

common options:
  --mode <annihilation|total-war|infiltration|all>   default: all (match), annihilation (tune)
  --nodes <n>          per-move node budget          default: 20000
  --openings <plies>   random opening length         default: 6
  --threads <n>        parallel games                default: available parallelism
  --seed <n>           opening seed                  default: 1
  --candidate <k=v,..> candidate weight overrides    repeatable
  --baseline <k=v,..>  baseline weight overrides     repeatable
  --candidate-proofs <on|off>  rule-derived score bounds   default: on
  --baseline-proofs  <on|off>  rule-derived score bounds   default: on
  --candidate-selective <on|off>  null move, futility, LMR  default: on
  --baseline-selective  <on|off>  null move, futility, LMR  default: on
  --candidate-reduction-scale <n> late-move reduction growth, hundredths
  --baseline-reduction-scale  <n> late-move reduction growth, hundredths

match options:
  --games <n>          game pairs per mode           default: 200
  --elo0 <x> --elo1 <y>   SPRT hypotheses            default: 0 and 5
  --alpha <a> --beta <b>  error rates                default: 0.05 and 0.05

tune options:
  --iterations <n>     SPSA iterations               default: 40
  --pairs <n>          game pairs per iteration      default: 12
  --params <names>     comma-separated subset        default: the mode's weights
  --learning <x>       SPSA step scale               default: 5.0
  --out <file>         write tuned weights           default: stdout only";

fn run(arguments: &[String]) -> Result<(), String> {
    let command = arguments.first().map_or("match", String::as_str);
    match command {
        "match" => run_match(&Options::parse(&arguments[1..], Command::Match)?),
        "tune" => run_tune(&Options::parse(&arguments[1..], Command::Tune)?),
        "show" => {
            print_params(&EvalParams::DEFAULT);
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        unknown => Err(format!("unknown command {unknown:?}")),
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Command {
    Match,
    Tune,
}

struct Options {
    modes: Vec<Mode>,
    nodes: u64,
    opening_plies: u16,
    threads: usize,
    seed: u64,
    games: usize,
    elo0: f64,
    elo1: f64,
    alpha: f64,
    beta: f64,
    iterations: usize,
    pairs: usize,
    learning: f64,
    baseline: EvalParams,
    candidate: EvalParams,
    tuned_names: Vec<String>,
    output: Option<String>,
    baseline_proofs: bool,
    candidate_proofs: bool,
    baseline_selective: bool,
    candidate_selective: bool,
    baseline_tuning: SearchTuning,
    candidate_tuning: SearchTuning,
}

impl Options {
    fn parse(arguments: &[String], command: Command) -> Result<Self, String> {
        let mut options = Self {
            modes: if command == Command::Tune {
                vec![Mode::Annihilation]
            } else {
                Mode::ALL.to_vec()
            },
            nodes: 20_000,
            opening_plies: 6,
            threads: thread::available_parallelism().map_or(1, |value| value.get()),
            seed: 1,
            games: 200,
            elo0: 0.0,
            elo1: 5.0,
            alpha: 0.05,
            beta: 0.05,
            iterations: 40,
            pairs: 12,
            learning: 5.0,
            baseline: EvalParams::DEFAULT,
            candidate: EvalParams::DEFAULT,
            tuned_names: Vec::new(),
            output: None,
            baseline_proofs: true,
            candidate_proofs: true,
            baseline_selective: true,
            candidate_selective: true,
            baseline_tuning: SearchTuning::DEFAULT,
            candidate_tuning: SearchTuning::DEFAULT,
        };

        let mut index = 0;
        while index < arguments.len() {
            let flag = arguments[index].as_str();
            index += 1;
            let mut value = || -> Result<&str, String> {
                let result = arguments
                    .get(index)
                    .ok_or_else(|| format!("{flag} needs a value"))?;
                index += 1;
                Ok(result.as_str())
            };
            match flag {
                "--mode" => options.modes = parse_modes(value()?)?,
                "--nodes" => options.nodes = parse_number(value()?, "nodes")?,
                "--openings" => options.opening_plies = parse_number(value()?, "openings")?,
                "--threads" => options.threads = parse_number(value()?, "threads")?,
                "--seed" => {
                    options.seed = value()?.parse().map_err(|_| "invalid seed".to_owned())?
                }
                "--games" => options.games = parse_number(value()?, "games")?,
                "--iterations" => options.iterations = parse_number(value()?, "iterations")?,
                "--pairs" => options.pairs = parse_number(value()?, "pairs")?,
                "--learning" => options.learning = parse_float(value()?, "learning")?,
                "--elo0" => options.elo0 = parse_float(value()?, "elo0")?,
                "--elo1" => options.elo1 = parse_float(value()?, "elo1")?,
                "--alpha" => options.alpha = parse_float(value()?, "alpha")?,
                "--beta" => options.beta = parse_float(value()?, "beta")?,
                "--candidate" => {
                    let entry = value()?.to_owned();
                    options
                        .candidate
                        .apply_assignments(entry.split(','))
                        .map_err(|error| format!("--candidate: {error}"))?;
                }
                "--baseline" => {
                    let entry = value()?.to_owned();
                    options
                        .baseline
                        .apply_assignments(entry.split(','))
                        .map_err(|error| format!("--baseline: {error}"))?;
                }
                "--params" => {
                    options.tuned_names = value()?
                        .split(',')
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(ToOwned::to_owned)
                        .collect();
                }
                "--out" => options.output = Some(value()?.to_owned()),
                "--baseline-proofs" => options.baseline_proofs = parse_switch(value()?)?,
                "--candidate-proofs" => options.candidate_proofs = parse_switch(value()?)?,
                "--baseline-selective" => options.baseline_selective = parse_switch(value()?)?,
                "--candidate-selective" => options.candidate_selective = parse_switch(value()?)?,
                "--baseline-reduction-scale" => {
                    options.baseline_tuning.reduction_scale =
                        parse_number(value()?, "reduction scale")?;
                }
                "--candidate-reduction-scale" => {
                    options.candidate_tuning.reduction_scale =
                        parse_number(value()?, "reduction scale")?;
                }
                unknown => return Err(format!("unknown option {unknown:?}")),
            }
        }
        if options.threads == 0 {
            options.threads = 1;
        }
        for name in &options.tuned_names {
            if EvalParams::DEFAULT.get(name).is_none() {
                return Err(format!("unknown evaluation parameter {name:?}"));
            }
        }
        Ok(options)
    }

    fn engine(&self, params: EvalParams, side: Side) -> EngineConfig {
        let mut config = EngineConfig::default().with_nodes(self.nodes);
        config.params = params;
        match side {
            Side::Baseline => {
                config.rule_proofs = self.baseline_proofs;
                config.selective = self.baseline_selective;
                config.tuning = self.baseline_tuning;
            }
            Side::Candidate => {
                config.rule_proofs = self.candidate_proofs;
                config.selective = self.candidate_selective;
                config.tuning = self.candidate_tuning;
            }
        }
        config
    }

    /// Whether the two arms differ in anything but evaluation weights.
    fn search_differs(&self) -> bool {
        self.baseline_proofs != self.candidate_proofs
            || self.baseline_selective != self.candidate_selective
            || self.baseline_tuning != self.candidate_tuning
    }
}

/// Which arm of a match an engine configuration belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Side {
    Baseline,
    Candidate,
}

fn parse_switch(value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => Ok(true),
        "off" | "false" | "0" | "no" => Ok(false),
        other => Err(format!("expected on or off, got {other:?}")),
    }
}

fn parse_modes(value: &str) -> Result<Vec<Mode>, String> {
    if value.eq_ignore_ascii_case("all") {
        return Ok(Mode::ALL.to_vec());
    }
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| Mode::parse(entry).ok_or_else(|| format!("unknown mode {entry:?}")))
        .collect()
}

fn parse_number<T: std::str::FromStr>(value: &str, label: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {label} {value:?}"))
}

fn parse_float(value: &str, label: &str) -> Result<f64, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {label} {value:?}"))
}

// ---------------------------------------------------------------------------
// Match
// ---------------------------------------------------------------------------

/// Wins, draws and losses from the candidate's point of view.
#[derive(Clone, Copy, Debug, Default)]
struct Tally {
    wins: u32,
    draws: u32,
    losses: u32,
    truncated: u32,
    plies: u64,
}

impl Tally {
    fn add(&mut self, result: GameResult, truncated: bool, plies: u16) {
        match result {
            GameResult::RedWin => self.wins += 1,
            GameResult::Draw => self.draws += 1,
            GameResult::BlueWin => self.losses += 1,
        }
        if truncated {
            self.truncated += 1;
        }
        self.plies += u64::from(plies);
    }

    fn merge(&mut self, other: Self) {
        self.wins += other.wins;
        self.draws += other.draws;
        self.losses += other.losses;
        self.truncated += other.truncated;
        self.plies += other.plies;
    }

    const fn games(&self) -> u32 {
        self.wins + self.draws + self.losses
    }

    fn score(&self) -> f64 {
        if self.games() == 0 {
            return 0.5;
        }
        (f64::from(self.wins) + 0.5 * f64::from(self.draws)) / f64::from(self.games())
    }
}

fn run_match(options: &Options) -> Result<(), String> {
    let changed = changed_params(&options.baseline, &options.candidate);
    println!("arena match");
    println!("  nodes/move    {}", options.nodes);
    println!("  opening plies {}", options.opening_plies);
    println!("  game pairs    {} per mode", options.games);
    println!("  threads       {}", options.threads);
    println!("  seed          {}", options.seed);
    if changed.is_empty() && !options.search_differs() {
        println!("  candidate     identical to baseline (sanity run)");
    } else if changed.is_empty() {
        println!("  candidate     weights unchanged");
    } else {
        println!("  candidate     {changed}");
    }
    if options.baseline_proofs != options.candidate_proofs {
        println!(
            "  rule proofs   baseline {}, candidate {}",
            switch_label(options.baseline_proofs),
            switch_label(options.candidate_proofs)
        );
    }
    if options.baseline_selective != options.candidate_selective {
        println!(
            "  selective     baseline {}, candidate {}",
            switch_label(options.baseline_selective),
            switch_label(options.candidate_selective)
        );
    }
    if options.baseline_tuning.reduction_scale != options.candidate_tuning.reduction_scale {
        println!(
            "  lmr scale     baseline {}, candidate {}",
            options.baseline_tuning.reduction_scale, options.candidate_tuning.reduction_scale
        );
    }
    println!();

    let mut combined = Tally::default();
    for &mode in &options.modes {
        let tally = play_match(
            options,
            mode,
            options.baseline,
            options.candidate,
            options.games,
        );
        println!("{}", report(mode.name(), &tally, options));
        combined.merge(tally);
    }
    if options.modes.len() > 1 {
        println!("{}", report("combined", &combined, options));
        println!(
            "note: a combined pass that hides a per-mode regression is still a\n\
             regression. Check every mode above before shipping."
        );
    }
    Ok(())
}

/// Play `pairs` opening pairs, swapping colours within each pair.
///
/// Colour swapping removes any first-move advantage from the estimate, and
/// per-opening determinism means the result does not depend on `--threads`.
fn play_match(
    options: &Options,
    mode: Mode,
    baseline: EvalParams,
    candidate: EvalParams,
    pairs: usize,
) -> Tally {
    let openings: Vec<Vec<rpsfish::Move>> = (0..pairs)
        .filter_map(|index| {
            let mut random = Random::new(
                options
                    .seed
                    .wrapping_mul(0x9e37_79b9_7f4a_7c15)
                    .wrapping_add(index as u64)
                    .wrapping_add(u64::from(mode as u8) << 48),
            );
            balanced_opening(mode, options.opening_plies, 150, &mut random, 64)
        })
        .collect();

    let candidate_engine = options.engine(candidate, Side::Candidate);
    let baseline_engine = options.engine(baseline, Side::Baseline);
    let shared = Mutex::new(Tally::default());
    let thread_count = options.threads.min(openings.len().max(1));

    thread::scope(|scope| {
        for thread_index in 0..thread_count {
            let openings = &openings;
            let shared = &shared;
            scope.spawn(move || {
                let mut local = Tally::default();
                let mut candidate_red = MatchRunner::new(&candidate_engine, &baseline_engine);
                let mut baseline_red = MatchRunner::new(&baseline_engine, &candidate_engine);
                for opening in openings.iter().skip(thread_index).step_by(thread_count) {
                    let first = candidate_red.play(
                        mode,
                        opening,
                        &candidate_engine,
                        &baseline_engine,
                        DEFAULT_MAX_PLIES,
                    );
                    local.add(first.result, first.truncated, first.plies);

                    let second = baseline_red.play(
                        mode,
                        opening,
                        &baseline_engine,
                        &candidate_engine,
                        DEFAULT_MAX_PLIES,
                    );
                    // The candidate played Blue here, so flip the result.
                    local.add(
                        second.result.from_perspective(rpsfish::Color::Blue),
                        second.truncated,
                        second.plies,
                    );
                }
                shared.lock().expect("arena mutex").merge(local);
            });
        }
    });

    shared.into_inner().expect("arena mutex")
}

fn report(label: &str, tally: &Tally, options: &Options) -> String {
    let games = tally.games();
    let score = tally.score();
    let statistics = Statistics::new(tally, options.elo0, options.elo1);
    let mut text = String::new();
    let _ = writeln!(text, "{label}:");
    let _ = writeln!(
        text,
        "  games {games}  W {} D {} L {}  score {:.4}",
        tally.wins, tally.draws, tally.losses, score
    );
    let _ = writeln!(
        text,
        "  elo   {:+.1}  [{:+.1}, {:+.1}] 95%   LOS {:.1}%",
        statistics.elo,
        statistics.elo_low,
        statistics.elo_high,
        statistics.los * 100.0
    );
    let _ = writeln!(
        text,
        "  sprt  LLR {:+.2}  bounds [{:.2}, {:.2}]  elo0 {:.1} elo1 {:.1}  ->  {}",
        statistics.llr,
        lower_bound(options.alpha, options.beta),
        upper_bound(options.alpha, options.beta),
        options.elo0,
        options.elo1,
        verdict(statistics.llr, options.alpha, options.beta),
    );
    if games > 0 {
        let _ = writeln!(
            text,
            "  length {:.1} plies average, {} truncated",
            tally.plies as f64 / f64::from(games),
            tally.truncated
        );
    }
    text
}

struct Statistics {
    elo: f64,
    elo_low: f64,
    elo_high: f64,
    llr: f64,
    los: f64,
}

impl Statistics {
    fn new(tally: &Tally, elo0: f64, elo1: f64) -> Self {
        let games = f64::from(tally.games());
        if games == 0.0 {
            return Self {
                elo: 0.0,
                elo_low: 0.0,
                elo_high: 0.0,
                llr: 0.0,
                los: 0.5,
            };
        }
        let wins = f64::from(tally.wins) / games;
        let draws = f64::from(tally.draws) / games;
        let score = wins + 0.5 * draws;
        // Variance of a single game's score, where a game scores 0, 0.5 or 1.
        let variance = (wins + 0.25 * draws - score * score).max(1e-12) / games;
        let deviation = variance.sqrt();
        Self {
            elo: elo_from_score(score),
            elo_low: elo_from_score((score - 1.96 * deviation).clamp(0.0, 1.0)),
            elo_high: elo_from_score((score + 1.96 * deviation).clamp(0.0, 1.0)),
            llr: generalized_llr(score, variance, elo0, elo1),
            los: likelihood_of_superiority(tally),
        }
    }
}

/// Normalized generalized SPRT log-likelihood ratio.
///
/// This is the standard normal approximation: the two hypotheses are compared
/// through the observed score and its variance rather than through a fitted
/// trinomial, which keeps it usable from the very first games.
fn generalized_llr(score: f64, variance: f64, elo0: f64, elo1: f64) -> f64 {
    if variance <= 0.0 {
        return 0.0;
    }
    let score0 = score_from_elo(elo0);
    let score1 = score_from_elo(elo1);
    (score1 - score0) * (2.0 * score - score0 - score1) / (2.0 * variance)
}

fn lower_bound(alpha: f64, beta: f64) -> f64 {
    (beta / (1.0 - alpha)).ln()
}

fn upper_bound(alpha: f64, beta: f64) -> f64 {
    ((1.0 - beta) / alpha).ln()
}

fn verdict(llr: f64, alpha: f64, beta: f64) -> &'static str {
    if llr >= upper_bound(alpha, beta) {
        "ACCEPT candidate"
    } else if llr <= lower_bound(alpha, beta) {
        "REJECT candidate"
    } else {
        "inconclusive, keep playing"
    }
}

fn score_from_elo(elo: f64) -> f64 {
    1.0 / (1.0 + 10_f64.powf(-elo / 400.0))
}

fn elo_from_score(score: f64) -> f64 {
    if score <= 0.0 {
        return -800.0;
    }
    if score >= 1.0 {
        return 800.0;
    }
    -400.0 * (1.0 / score - 1.0).log10()
}

/// Probability that the candidate is genuinely stronger, from decisive games.
fn likelihood_of_superiority(tally: &Tally) -> f64 {
    let decisive = f64::from(tally.wins + tally.losses);
    if decisive == 0.0 {
        return 0.5;
    }
    let z = (f64::from(tally.wins) - f64::from(tally.losses)) / decisive.sqrt();
    0.5 * (1.0 + erf(z / 2_f64.sqrt()))
}

/// Abramowitz and Stegun 7.1.26, accurate to about 1.5e-7.
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0
        - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t
            + 0.254_829_592)
            * t
            * (-x * x).exp();
    sign * y
}

// ---------------------------------------------------------------------------
// Tune
// ---------------------------------------------------------------------------

fn run_tune(options: &Options) -> Result<(), String> {
    let mode = *options
        .modes
        .first()
        .ok_or_else(|| "tune needs one mode".to_owned())?;
    let names = if options.tuned_names.is_empty() {
        default_tuned_names(mode)
    } else {
        options.tuned_names.clone()
    };
    if names.is_empty() {
        return Err("no tunable parameters selected".to_owned());
    }

    println!("arena tune ({})", mode.name());
    println!("  parameters {}", names.join(", "));
    println!("  iterations {}", options.iterations);
    println!("  pairs/iter {}", options.pairs);
    println!("  nodes/move {}", options.nodes);
    println!();
    println!("SPSA perturbs every parameter at once and only needs two matches");
    println!("per iteration, so the run cost does not grow with the parameter");
    println!("count. Weight signs come from game results, never from judgement.");
    println!();

    // The parameter vector is carried as f64. Rounding after every iteration
    // would pin any weight whose steps are smaller than one unit -- including
    // every weight that starts at zero, which is exactly the interesting case.
    let mut theta: Vec<f64> = names
        .iter()
        .map(|name| f64::from(options.baseline.get(name).unwrap_or(0)))
        .collect();
    // Perturbation size per parameter, scaled to its magnitude so a weight of
    // 220 and a weight of 2 both move meaningfully.
    let steps: Vec<f64> = names
        .iter()
        .map(|name| {
            let value = f64::from(options.baseline.get(name).unwrap_or(0).abs());
            (value / 5.0).clamp(4.0, 40.0)
        })
        .collect();

    let mut current = options.baseline;
    let mut random = Random::new(options.seed ^ 0xd1ce_5eed);
    for iteration in 0..options.iterations {
        // Standard SPSA decay exponents: the perturbation shrinks slowly and
        // the step size shrinks faster, so early iterations explore and later
        // ones settle.
        let perturbation_decay = (iteration as f64 + 1.0).powf(0.101);
        let learning = options.learning / (iteration as f64 + 1.0).powf(0.602);

        let signs: Vec<f64> = names.iter().map(|_| f64::from(random.sign())).collect();
        let mut plus = current;
        let mut minus = current;
        let mut sizes = Vec::with_capacity(names.len());
        for (index, name) in names.iter().enumerate() {
            let size = (steps[index] / perturbation_decay).max(1.0);
            sizes.push(size);
            let offset = (size * signs[index]).round() as i32;
            let base = theta[index].round() as i32;
            plus.set(name, base + offset);
            minus.set(name, base - offset);
        }

        let tally = play_match(options, mode, minus, plus, options.pairs);
        // Score above 0.5 means the `plus` direction won, so move that way.
        // This is the whole gradient estimate: game results, nothing else.
        let gradient = tally.score() - 0.5;
        for index in 0..names.len() {
            theta[index] += learning * gradient * signs[index] * sizes[index];
        }
        for (index, name) in names.iter().enumerate() {
            current.set(name, theta[index].round() as i32);
        }

        println!(
            "iter {:>3}  plus-score {:.3}  W {} D {} L {}  c {:.1}  a {:.2}  now [{}]",
            iteration + 1,
            tally.score(),
            tally.wins,
            tally.draws,
            tally.losses,
            sizes.first().copied().unwrap_or(0.0),
            learning,
            theta
                .iter()
                .map(|value| format!("{value:.1}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }

    println!();
    println!("tuned weights (verify with `arena match` before shipping):");
    let changed = changed_params(&options.baseline, &current);
    println!(
        "  {}",
        if changed.is_empty() {
            "no net movement".to_owned()
        } else {
            changed
        }
    );
    if let Some(path) = &options.output {
        let body: String = current
            .entries()
            .into_iter()
            .map(|(name, value)| format!("{name}={value}\n"))
            .collect();
        std::fs::write(path, body).map_err(|error| format!("writing {path}: {error}"))?;
        println!("  written to {path}");
    }
    Ok(())
}

/// The weights belonging to one mode, identified by name prefix.
fn default_tuned_names(mode: Mode) -> Vec<String> {
    let prefix = match mode {
        Mode::Annihilation => "annihilation_",
        Mode::TotalWar => "total_war_",
        Mode::Infiltration => "infiltration_",
    };
    EvalParams::NAMES
        .iter()
        .filter(|name| name.starts_with(prefix))
        .map(|name| (*name).to_owned())
        .collect()
}

fn changed_params(baseline: &EvalParams, candidate: &EvalParams) -> String {
    baseline
        .entries()
        .into_iter()
        .zip(candidate.entries())
        .filter(|((_, before), (_, after))| before != after)
        .map(|((name, before), (_, after))| format!("{name}: {before} -> {after}"))
        .collect::<Vec<_>>()
        .join(", ")
}

const fn switch_label(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

fn print_params(params: &EvalParams) {
    for (name, value) in params.entries() {
        println!("{name}={value}");
    }
}
