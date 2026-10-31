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

//! Score the evaluation against positions whose value is already known.
//!
//! `arena` answers "is this stronger on average in self-play", which is the
//! right question for most weights and the wrong one for a pattern self-play
//! from random openings never reaches. EVAL_TRAINING.md §4.2 has the worked
//! example: `total_war_army_doomed` at -5000 changed the result of zero games
//! out of 120, so no sample size can price it, even though the pattern it
//! describes is exactly what a human steers toward.
//!
//! This is the other instrument §7 asks for. Each entry in a suite file is a
//! position with a verdict established by a search deep enough to prove it;
//! the run reports how many the engine gets right at *bot* depth, which is
//! where the mistakes actually happen.
//!
//! ```text
//! cargo run --release --example suite -- --nodes 3000000
//! cargo run --release --example suite -- --param total_war_army_doomed=-400
//! cargo run --release --example suite -- --proofs off
//! ```

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rpsfish::{
    EvalParams, MATE_SCORE, MATE_THRESHOLD, Mode, Position, SearchLimits, Searcher, evaluate_with,
};

/// Which side a position is won for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Verdict {
    Red,
    Blue,
    Draw,
}

impl Verdict {
    fn parse(text: &str) -> Result<Self, String> {
        match text {
            "red" => Ok(Self::Red),
            "blue" => Ok(Self::Blue),
            "draw" => Ok(Self::Draw),
            other => Err(format!("verdict must be red, blue or draw, got {other:?}")),
        }
    }
}

struct Case {
    name: String,
    mode: Mode,
    position: Position,
    verdict: Verdict,
    /// How far past zero the score must sit to count as understood.
    ///
    /// A won position that the engine calls +1 is not a position it
    /// understands; the whole point of the suite is that the sign is not
    /// enough. Zero for a drawn verdict, where the requirement is a band.
    margin: i32,
    note: String,
}

fn main() {
    match run() {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    }
}

fn run() -> Result<bool, String> {
    let options = Options::parse(env::args().skip(1))?;
    let cases = load_all(&options.paths)?;
    if cases.is_empty() {
        return Err("no positions loaded".to_owned());
    }

    println!("suite: {} positions", cases.len());
    if options.nodes == 0 {
        println!("static evaluation only\n");
    } else {
        println!(
            "budget: {} nodes/position, depth cap {}\n",
            options.nodes, options.depth
        );
    }

    let mut searcher = Searcher::new(options.hash_megabytes);
    searcher.set_eval_params(options.params);
    searcher.set_rule_proofs(options.proofs);

    let mut passed = 0_usize;
    let mut sign_only = 0_usize;
    let mut static_passed = 0_usize;
    let mut by_mode: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();

    // Static and searched accuracy are reported apart because they are two
    // different questions. Search can rescue a position it can resolve inside
    // its horizon no matter what the leaves say; the static column is what the
    // evaluation actually knows, and it is the column a weight change moves.
    println!(
        "{:<34} {:>4} {:>10} {:>10} {:>6} result",
        "position", "want", "static", "search", "depth"
    );
    for case in &cases {
        // Every score in this report is from Red's point of view, so that one
        // column can be read down without tracking whose turn it is.
        let perspective = if case.position.side_to_move() == rpsfish::Color::Red {
            1
        } else {
            -1
        };
        let static_score = evaluate_with(&case.position, &options.params) * perspective;
        let (searched, depth) = if options.nodes == 0 {
            (static_score, 0)
        } else {
            let result = searcher.search(
                case.position,
                SearchLimits {
                    max_depth: options.depth,
                    max_nodes: Some(options.nodes),
                    move_time: Some(Duration::from_secs(options.seconds)),
                },
            );
            (result.score * perspective, result.completed_depth)
        };

        let sign_ok = matches_sign(searched, case.verdict);
        let ok = sign_ok && matches_margin(searched, case.verdict, case.margin);
        let static_ok = matches_sign(static_score, case.verdict)
            && matches_margin(static_score, case.verdict, case.margin);
        if ok {
            passed += 1;
        }
        if sign_ok {
            sign_only += 1;
        }
        if static_ok {
            static_passed += 1;
        }
        let entry = by_mode.entry(case.mode.id()).or_insert((0, 0));
        entry.1 += 1;
        if ok {
            entry.0 += 1;
        }

        println!(
            "{:<34} {:>4} {:>10} {:>10} {:>6} {}",
            truncate(&case.name, 34),
            verdict_name(case.verdict),
            format!(
                "{}{}",
                format_score(static_score),
                if static_ok { " " } else { "!" }
            ),
            format_score(searched),
            depth,
            if ok {
                "pass"
            } else if sign_ok {
                "SIGN ONLY"
            } else {
                "FAIL"
            }
        );
        if !ok && !case.note.is_empty() {
            println!("{:>36}{}", "", case.note);
        }
    }

    println!();
    for (mode, (correct, total)) in &by_mode {
        println!("{mode}: {correct}/{total}");
    }
    println!(
        "search {}/{} understood, {}/{} correct sign; static {}/{} understood",
        passed,
        cases.len(),
        sign_only,
        cases.len(),
        static_passed,
        cases.len()
    );

    // A report by default, a gate only when asked. The shipped engine reads 19
    // of 20 and the twentieth is a position sitting exactly on the boundary the
    // suite was built to find, so a runner that failed on any miss would be
    // permanently red and would stop being read.
    if let Some(required) = options.require
        && passed < required
    {
        eprintln!("FAIL: {passed} positions understood, --require {required}");
        return Ok(false);
    }
    Ok(true)
}

fn matches_sign(score: i32, verdict: Verdict) -> bool {
    match verdict {
        Verdict::Red => score > 0,
        Verdict::Blue => score < 0,
        Verdict::Draw => true,
    }
}

fn matches_margin(score: i32, verdict: Verdict, margin: i32) -> bool {
    match verdict {
        Verdict::Red => score >= margin,
        Verdict::Blue => score <= -margin,
        // A draw is a band around zero rather than a threshold past it.
        Verdict::Draw => score.abs() <= margin,
    }
}

const fn verdict_name(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Red => "red",
        Verdict::Blue => "blue",
        Verdict::Draw => "draw",
    }
}

/// Render a score, spelling a proven result out as the number of plies to it.
///
/// A suite entry is usually a position whose whole point is that the engine
/// only sees the win far too deep to be useful, so how far away the win is is
/// the number worth reading.
fn format_score(score: i32) -> String {
    if score.abs() >= MATE_THRESHOLD {
        let sign = if score > 0 { "R" } else { "B" };
        format!("{sign}#{}", MATE_SCORE - score.abs())
    } else {
        score.to_string()
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_owned()
    } else {
        text.chars().take(width - 1).chain(['~']).collect()
    }
}

struct Options {
    paths: Vec<PathBuf>,
    nodes: u64,
    depth: u8,
    seconds: u64,
    hash_megabytes: usize,
    params: EvalParams,
    /// Fail the run below this many understood positions, when set.
    require: Option<usize>,
    /// Rule-derived score bounds, so a search proof can be scored here.
    ///
    /// Mirrors `arena`'s `--candidate-proofs`. The suite is the instrument for
    /// anything self-play scores 0.5000 on, and a proof that only fires in a
    /// handful of endgames is exactly that, so it needs a switch to be
    /// measured against. Note the `static` column cannot move for a search
    /// change: it calls `evaluate_with` directly and never reaches the clamp.
    proofs: bool,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self {
            paths: Vec::new(),
            // Roughly what a bot reaches in Total War in a four-second browser
            // budget (EVAL_TRAINING.md §8), because that is the depth whose
            // mistakes matter.
            nodes: 3_000_000,
            depth: 64,
            seconds: 600,
            hash_megabytes: 256,
            params: EvalParams::DEFAULT,
            require: None,
            proofs: true,
        };
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--file" => options
                    .paths
                    .push(PathBuf::from(next_value(&mut arguments, "--file")?)),
                "--nodes" => options.nodes = parse_number(&mut arguments, "--nodes")?,
                "--depth" => {
                    options.depth = u8::try_from(parse_number(&mut arguments, "--depth")?)
                        .map_err(|_| "--depth out of range".to_owned())?;
                }
                "--seconds" => options.seconds = parse_number(&mut arguments, "--seconds")?,
                "--hash-mb" => {
                    options.hash_megabytes =
                        usize::try_from(parse_number(&mut arguments, "--hash-mb")?)
                            .map_err(|_| "--hash-mb out of range".to_owned())?;
                }
                "--static" => options.nodes = 0,
                "--proofs" => {
                    let value = next_value(&mut arguments, "--proofs")?;
                    options.proofs = match value.as_str() {
                        "on" => true,
                        "off" => false,
                        other => {
                            return Err(format!("--proofs: expected on or off, got {other:?}"));
                        }
                    };
                }
                "--require" => {
                    options.require = Some(
                        usize::try_from(parse_number(&mut arguments, "--require")?)
                            .map_err(|_| "--require out of range".to_owned())?,
                    );
                }
                "--param" => {
                    let assignment = next_value(&mut arguments, "--param")?;
                    options
                        .params
                        .apply_assignments(assignment.split(','))
                        .map_err(|error| format!("--param: {error}"))?;
                }
                "--params-file" => {
                    let path = next_value(&mut arguments, "--params-file")?;
                    let text = fs::read_to_string(&path)
                        .map_err(|error| format!("cannot read {path}: {error}"))?;
                    options
                        .params
                        .apply_assignments(text.lines())
                        .map_err(|error| format!("--params-file: {error}"))?;
                }
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        if options.paths.is_empty() {
            options.paths.push(default_suite_directory());
        }
        Ok(options)
    }
}

fn default_suite_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("suite")
}

fn next_value(arguments: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("missing value after {flag}"))
}

fn parse_number(arguments: &mut impl Iterator<Item = String>, flag: &str) -> Result<u64, String> {
    next_value(arguments, flag)?
        .replace('_', "")
        .parse::<u64>()
        .map_err(|error| format!("{flag}: {error}"))
}

fn load_all(paths: &[PathBuf]) -> Result<Vec<Case>, String> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut entries: Vec<PathBuf> = fs::read_dir(path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|entry| entry.extension().is_some_and(|suffix| suffix == "txt"))
                .collect();
            entries.sort();
            files.extend(entries);
        } else {
            files.push(path.clone());
        }
    }

    let mut cases = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file)
            .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
        cases.extend(parse_suite(&text).map_err(|error| {
            format!(
                "{}: {error}",
                file.file_name().unwrap_or_default().display()
            )
        })?);
    }
    Ok(cases)
}

/// Parse blank-line-separated `key = value` blocks.
///
/// Deliberately dull: a position that is awkward to write down is a position
/// that will not get written down, and the point of the file is to grow.
fn parse_suite(text: &str) -> Result<Vec<Case>, String> {
    let mut cases = Vec::new();
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    for (number, line) in text.lines().chain(std::iter::once("")).enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            if !fields.is_empty() {
                cases
                    .push(build_case(&fields).map_err(|error| {
                        format!("block ending at line {}: {error}", number + 1)
                    })?);
                fields.clear();
            }
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: expected key = value", number + 1))?;
        fields.insert(key.trim().to_owned(), value.trim().to_owned());
    }
    Ok(cases)
}

fn build_case(fields: &BTreeMap<String, String>) -> Result<Case, String> {
    let get = |key: &str| -> Result<&str, String> {
        fields
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| format!("missing {key}"))
    };
    let mode = match get("mode")? {
        "V3" | "infiltration" => Mode::Infiltration,
        "V5" | "total-war" => Mode::TotalWar,
        "V6" | "intransitive" => Mode::Intransitive,
        other => return Err(format!("unknown mode {other:?}")),
    };
    let position =
        Position::from_fen(mode, get("fen")?).map_err(|error| format!("fen: {error}"))?;
    let margin = fields
        .get("margin")
        .map(|value| value.parse::<i32>())
        .transpose()
        .map_err(|error| format!("margin: {error}"))?
        .unwrap_or(300);
    Ok(Case {
        name: get("name")?.to_owned(),
        mode,
        position,
        verdict: Verdict::parse(get("verdict")?)?,
        margin,
        note: fields.get("note").cloned().unwrap_or_default(),
    })
}
