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

use std::collections::HashMap;
use std::env;
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use rpsfish::book::Book;
use rpsfish::{
    AnalysisLine, AnalysisUpdate, Color, EvalParams, Goal, MATE_SCORE, MATE_THRESHOLD, Mode, Move,
    Position, SearchLimits, SearchStats, Searcher, perft,
};

const MAX_SEARCH_DEPTH: u8 = 127;

/// Largest transposition table this CLI will allocate.
///
/// A guard against a mistyped argument asking for a terabyte, not a policy.
/// The browser's table is fixed in `wasm.rs` and is not this one, so the old
/// 512 MiB ceiling here protected nothing while making an offline analysis
/// throw away most of the work a multi-billion-node search does.
const MAX_HASH_MEGABYTES: usize = 16_384;

/// Transposition table this CLI allocates when nothing asks for another size.
const DEFAULT_HASH_MEGABYTES: usize = 16;

/// Most principal variations the search will keep at once.
const MAX_VARIATIONS: usize = 8;

/// Shallowest book record this engine will play without searching.
///
/// A book is built in two passes and the first one is a survey: every legal
/// move ranked cheaply, so that each becomes a position the deep pass can
/// expand. Those survey stubs are real records with real scores, and they are
/// exactly the ones a live search at a tournament time control would beat. The
/// floor is what separates a position the book has *studied* from one it has
/// merely *listed*, and twelve is comfortably above what any survey band
/// reaches while being far below what the deep pass gives an opening.
const DEFAULT_BOOK_MIN_DEPTH: u8 = 12;

/// Search speed assumed until a real search has been timed.
///
/// Only ever used to compare a book record's effort against this engine's, and
/// only for the very first move of a session, after which
/// `RpsiSession::observe_speed` replaces it with what this host actually does.
const DEFAULT_NPS_ESTIMATE: u64 = 4_000_000;

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            print_usage();
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: Vec<String>) -> Result<(), String> {
    let command = arguments.first().map_or("search", String::as_str);
    match command {
        "search" => {
            let options = parse_search_options(&arguments[1..])?;
            run_search(options)
        }
        "perft" => {
            let mode = parse_mode(arguments.get(1).map_or("total-war", String::as_str))?;
            let depth = parse_depth(arguments.get(2), 4)?;
            run_perft(mode, depth);
            Ok(())
        }
        "rpsi" => run_rpsi(parse_rpsi_options(&arguments[1..])?),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        unknown => Err(format!("unknown command {unknown:?}")),
    }
}

#[derive(Clone, Debug)]
struct SearchOptions {
    mode: Mode,
    limits: SearchLimits,
    variations: usize,
    hash_megabytes: usize,
    params: EvalParams,
    /// Moves to play from the starting position before searching.
    line: Vec<Move>,
}

fn parse_search_options(arguments: &[String]) -> Result<SearchOptions, String> {
    let deep = arguments.iter().any(|argument| argument == "--deep");
    let mut options = SearchOptions {
        mode: Mode::TotalWar,
        limits: if deep {
            SearchLimits {
                max_depth: MAX_SEARCH_DEPTH,
                max_nodes: Some(50_000_000),
                move_time: Some(Duration::from_secs(60)),
            }
        } else {
            SearchLimits {
                max_depth: 8,
                max_nodes: Some(5_000_000),
                move_time: Some(Duration::from_secs(10)),
            }
        },
        variations: 1,
        hash_megabytes: if deep { 32 } else { DEFAULT_HASH_MEGABYTES },
        params: EvalParams::DEFAULT,
        line: Vec::new(),
    };

    let mut index = 0;
    if let Some(mode) = arguments.first().filter(|value| !value.starts_with('-')) {
        options.mode = parse_mode(mode)?;
        index += 1;
    }
    if let Some(depth) = arguments.get(index).filter(|value| !value.starts_with('-')) {
        options.limits.max_depth = parse_depth(Some(depth), options.limits.max_depth)?;
        index += 1;
    }

    while index < arguments.len() {
        let flag = arguments[index].as_str();
        index += 1;
        match flag {
            "--deep" => {}
            "--depth" => {
                options.limits.max_depth = parse_depth(arguments.get(index), 0)?;
                index += 1;
            }
            "--nodes" => {
                options.limits.max_nodes =
                    Some(parse_positive::<u64>(arguments.get(index), "node limit")?);
                index += 1;
            }
            "--time-ms" => {
                let milliseconds = parse_positive::<u64>(arguments.get(index), "time limit")?;
                options.limits.move_time = Some(Duration::from_millis(milliseconds));
                index += 1;
            }
            "--multipv" => {
                options.variations =
                    parse_positive::<usize>(arguments.get(index), "MultiPV count")?;
                index += 1;
            }
            "--param" => {
                read_param(&mut options.params, arguments.get(index))?;
                index += 1;
            }
            "--params-file" => {
                read_params_file(&mut options.params, arguments.get(index))?;
                index += 1;
            }
            "--show-params" => {
                for (name, value) in options.params.entries() {
                    println!("{name}={value}");
                }
            }
            "--hash-mb" => {
                options.hash_megabytes = read_hash_megabytes(arguments.get(index))?;
                index += 1;
            }
            "--line" => {
                let text = arguments
                    .get(index)
                    .ok_or_else(|| "missing --line moves".to_owned())?;
                options.line = parse_line(text)?;
                index += 1;
            }
            unknown => return Err(format!("unknown search option {unknown:?}")),
        }
    }

    Ok(options)
}

/// Read `--param k=v,..` into a weight set.
fn read_param(params: &mut EvalParams, value: Option<&String>) -> Result<(), String> {
    let entry = value.ok_or_else(|| "missing --param assignment".to_owned())?;
    params
        .apply_assignments(entry.split(','))
        .map_err(|error| format!("--param: {error}"))
}

/// Read `--params-file <path>` into a weight set, one assignment per line.
fn read_params_file(params: &mut EvalParams, value: Option<&String>) -> Result<(), String> {
    let path = value.ok_or_else(|| "missing --params-file path".to_owned())?;
    let body = std::fs::read_to_string(path).map_err(|error| format!("reading {path}: {error}"))?;
    params
        .apply_assignments(body.lines())
        .map_err(|error| format!("{path}: {error}"))
}

/// Read `--hash-mb <n>`, refusing a table this CLI will not allocate.
fn read_hash_megabytes(value: Option<&String>) -> Result<usize, String> {
    let megabytes = parse_positive::<usize>(value, "hash size")?;
    if megabytes > MAX_HASH_MEGABYTES {
        return Err(format!(
            "hash size must not exceed {MAX_HASH_MEGABYTES} MiB"
        ));
    }
    Ok(megabytes)
}

/// Read a move sequence written in the archive's squares, as `"d3-d4 d7-d6"`.
///
/// Move numbers are skipped, so a line pasted out of a stored game works
/// without being cleaned up first.
fn parse_line(text: &str) -> Result<Vec<Move>, String> {
    text.split([' ', ',', '\t', '\n'])
        .filter(|token| !token.is_empty() && !token.ends_with('.'))
        .map(|token| {
            Move::parse(token).ok_or_else(|| format!("{token:?} is not a move like \"d3-d4\""))
        })
        .collect()
}

fn parse_mode(value: &str) -> Result<Mode, String> {
    Mode::parse(value).ok_or_else(|| format!("unknown mode {value:?}"))
}

fn parse_depth(value: Option<&String>, default: u8) -> Result<u8, String> {
    let Some(value) = value else {
        return if default == 0 {
            Err("missing depth".to_owned())
        } else {
            Ok(default)
        };
    };
    let depth = value
        .parse::<u8>()
        .map_err(|error| format!("invalid depth {value:?}: {error}"))?;
    if depth == 0 || depth > MAX_SEARCH_DEPTH {
        return Err(format!("depth must be between 1 and {MAX_SEARCH_DEPTH}"));
    }
    Ok(depth)
}

fn parse_positive<T>(value: Option<&String>, label: &str) -> Result<T, String>
where
    T: std::str::FromStr + PartialEq + Default,
    T::Err: std::fmt::Display,
{
    let value = value.ok_or_else(|| format!("missing {label}"))?;
    let parsed = value
        .parse::<T>()
        .map_err(|error| format!("invalid {label} {value:?}: {error}"))?;
    if parsed == T::default() {
        return Err(format!("{label} must be at least 1"));
    }
    Ok(parsed)
}

/// Play `line` from `position`, returning where it arrives and the hashes of
/// every position before it.
///
/// The hashes are what make a repetition visible to search: without them a
/// position that has already occurred twice looks like a fresh one, which past
/// the opening is the difference between a draw and a win. Both the `search`
/// command and the RPSI `position` command replay through here, so a line
/// means the same thing however it arrived.
fn replay_line(mut position: Position, line: &[Move]) -> Result<(Position, Vec<u64>), String> {
    let mut history = Vec::with_capacity(line.len());
    for (index, &movement) in line.iter().enumerate() {
        history.push(position.hash());
        position
            .make_move(movement)
            .map_err(|error| format!("move {} ({}): {error}", index + 1, movement.notation()))?;
    }
    Ok((position, history))
}

fn run_search(options: SearchOptions) -> Result<(), String> {
    let (position, history) = replay_line(Position::starting(options.mode), &options.line)
        .map_err(|error| format!("--line: {error}"))?;
    let mut searcher = Searcher::new(options.hash_megabytes);
    searcher.set_eval_params(options.params);

    println!("engine rpsfish {}", env!("CARGO_PKG_VERSION"));
    println!("rules {}", rpsfish::RULES_VERSION);
    println!("mode {} ({})", options.mode.name(), options.mode.id());
    let rules = options.mode.rules();
    println!(
        "rule facts annihilation-loses={} other-loss={} territory={} territory-only-other-loss={} \
         goal={} repetition-draws={} stalemate-loses={} symmetries={} immortality-floor={} \
         majority-floor={}",
        rules.annihilation_loses,
        rules.has_other_loss_condition,
        rules.uses_territory,
        rules.territory_is_the_only_other_loss,
        match rules.goal {
            Goal::None => "none",
            Goal::HomeRank => "home-rank",
            Goal::EnemyCorner => "enemy-corner",
        },
        rules.repetition_draws,
        rules.stalemate_loses,
        rules.symmetries().len(),
        rules.immortality_prevents_loss(),
        rules.territory_majority_prevents_loss(),
    );
    if options.params != EvalParams::DEFAULT {
        println!("weights modified from defaults");
    }
    if !options.line.is_empty() {
        let moves: Vec<String> = options.line.iter().copied().map(Move::notation).collect();
        println!("line {}", moves.join(" "));
        println!("ply {} to move {}", position.ply(), position.side_to_move());
    }
    println!("position {:016x}", position.hash());
    let result = searcher.analyze_with_context_and_updates(
        position,
        options.limits,
        options.variations,
        &history,
        None,
        print_update,
    );

    println!("stop {:?}", result.stop_reason);
    if let Some(best) = result.lines.first() {
        println!("bestmove {} ({})", best.movement.notation(), best.movement);
    } else {
        println!("bestmove (none)");
    }
    Ok(())
}

fn print_update(update: &AnalysisUpdate) {
    let elapsed_ms = update.stats.elapsed.as_millis();
    let nodes_per_second = if elapsed_ms == 0 {
        0
    } else {
        u128::from(update.stats.nodes).saturating_mul(1_000) / elapsed_ms
    };
    for (index, line) in update.lines.iter().enumerate() {
        println!(
            "info depth {} seldepth {} multipv {} score {} confidence {} nodes {} nps {} time {} pv {}",
            update.completed_depth,
            update.stats.selective_depth,
            index + 1,
            format_score(line.score),
            update.confidence,
            update.stats.nodes,
            nodes_per_second,
            elapsed_ms,
            format_principal_variation(line),
        );
    }
}

/// Write a score the way RPSI reads it: `cp N`, or `win N`/`loss N` in plies
/// for a proven result.
///
/// Not `mate`. There is no mate in this game — a forced win comes from
/// annihilation, territory, or a boundary — so borrowing chess's word would be
/// wrong in the one place a reader most needs precision.
fn format_score(score: i32) -> String {
    if score >= MATE_THRESHOLD {
        format!("win {}", MATE_SCORE - score)
    } else if score <= -MATE_THRESHOLD {
        format!("loss {}", MATE_SCORE + score)
    } else {
        format!("cp {score}")
    }
}

/// Write a principal variation as moves, `d3-d4 d7-d6`.
///
/// `Move`'s `Display` prints coordinate pairs, which is useful when staring at
/// a board and useless to a parser, so the wire format spells them out.
fn format_principal_variation(line: &AnalysisLine) -> String {
    line.principal_variation
        .iter()
        .copied()
        .map(Move::notation)
        .collect::<Vec<_>>()
        .join(" ")
}

fn run_perft(mode: Mode, depth: u8) {
    let mut position = Position::starting(mode);
    let started = std::time::Instant::now();
    let nodes = perft(&mut position, depth);
    let elapsed = started.elapsed();
    let nodes_per_second = if elapsed.is_zero() {
        0.0
    } else {
        nodes as f64 / elapsed.as_secs_f64()
    };
    println!("mode: {} ({})", mode.name(), mode.id());
    println!("depth: {depth}");
    println!("nodes: {nodes}");
    println!("elapsed: {elapsed:.3?}");
    println!("nodes/s: {nodes_per_second:.0}");
}

/// What `rpsfish rpsi` accepts on its own command line.
///
/// The host never sends `setoption`: it asks for a handshake, a position and a
/// move, and nothing else. So for an engine playing online the command in
/// `rpsbot.py -- ./rpsfish rpsi ...` is the only place a hash size or a tuned
/// weight set can come from, and these are the flags `search` already takes.
#[derive(Clone, Debug)]
struct RpsiOptions {
    hash_megabytes: usize,
    variations: usize,
    params: EvalParams,
    /// Directory holding one book per mode, named by wire code: the `book/`
    /// that `scripts/build_books.sh` writes `book/V6` into. Absent means play
    /// every move by searching, which is what this engine did before books
    /// were readable at all.
    book_root: Option<PathBuf>,
    book_min_depth: u8,
    /// Search exactly this deep on every move, ignoring the clock, and do not
    /// consult a book. `None` is the normal engine.
    ///
    /// This is what makes the binary usable as a *yardstick* -- one of the fixed
    /// rungs the host's rating scale is defined by. A rung's whole job is to be
    /// the same opponent every time, and the ordinary engine is not: it is given
    /// a clock, so it plays better on an idle machine than on a busy one, and a
    /// rung whose strength tracked server load would be measuring the server.
    /// Fixing the depth fixes the opponent. It costs what it costs, which is the
    /// host's problem and not the scale's.
    ///
    /// The book goes with the clock for the same reason and it is worth saying
    /// out loud, because it is a flag quietly turning another one off: a book
    /// move is not a depth-N search, and a book is a file that gets rebuilt, so
    /// an engine that played from one would change strength on a day nobody
    /// touched it.
    force_depth: Option<u8>,
}

impl RpsiOptions {
    const DEFAULT: Self = Self {
        hash_megabytes: DEFAULT_HASH_MEGABYTES,
        variations: 1,
        params: EvalParams::DEFAULT,
        book_root: None,
        book_min_depth: DEFAULT_BOOK_MIN_DEPTH,
        force_depth: None,
    };
}

fn parse_rpsi_options(arguments: &[String]) -> Result<RpsiOptions, String> {
    let mut options = RpsiOptions::DEFAULT;
    let mut index = 0;
    while index < arguments.len() {
        let flag = arguments[index].as_str();
        index += 1;
        match flag {
            "--hash-mb" => {
                options.hash_megabytes = read_hash_megabytes(arguments.get(index))?;
                index += 1;
            }
            "--multipv" => {
                options.variations =
                    parse_positive::<usize>(arguments.get(index), "MultiPV count")?;
                if options.variations > MAX_VARIATIONS {
                    return Err(format!("MultiPV count must not exceed {MAX_VARIATIONS}"));
                }
                index += 1;
            }
            "--param" => {
                read_param(&mut options.params, arguments.get(index))?;
                index += 1;
            }
            "--params-file" => {
                read_params_file(&mut options.params, arguments.get(index))?;
                index += 1;
            }
            "--book" => {
                let value = arguments
                    .get(index)
                    .ok_or_else(|| "--book needs a directory".to_owned())?;
                options.book_root = Some(PathBuf::from(value));
                index += 1;
            }
            "--book-min-depth" => {
                options.book_min_depth = parse_depth(arguments.get(index), DEFAULT_BOOK_MIN_DEPTH)?;
                index += 1;
            }
            "--force-depth" => {
                options.force_depth = Some(parse_depth(arguments.get(index), 0)?);
                index += 1;
            }
            // Unlike an unknown protocol line, which must be ignored, an
            // unknown flag is a typo in a command somebody is about to leave
            // running for a week. Refusing it now is the kinder failure.
            unknown => return Err(format!("unknown rpsi option {unknown:?}")),
        }
    }
    Ok(options)
}

/// Everything the RPSI loop carries between commands.
///
/// A host re-sends the whole position on every move, so none of this is
/// load-bearing except the transposition table — but keeping it means a
/// `setoption` survives until the next one, which is what the protocol
/// promises.
struct RpsiSession {
    mode: Mode,
    position: Position,
    history: Vec<u64>,
    searcher: Searcher,
    params: EvalParams,
    hash_megabytes: usize,
    /// Set by `setoption name Hash` and applied at the next `isready`, which
    /// is where UCI allows slow setup to happen.
    requested_hash_megabytes: usize,
    selective: bool,
    rule_proofs: bool,
    variations: usize,
    book_root: Option<PathBuf>,
    book_min_depth: u8,
    /// See [`RpsiOptions::force_depth`]: a fixed depth, no clock, no book.
    force_depth: Option<u8>,
    /// The book for the mode being played, reloaded by `newgame`.
    book: Option<OpeningBook>,
    /// Nodes per second, measured from the last search that ran long enough
    /// to mean anything. Only used to ask whether a book record looked harder
    /// at a position than this engine is about to.
    observed_nps: u64,
}

/// A loaded opening book, with the backed-up values its moves are ranked by.
///
/// [`Book::values`] walks the whole graph to back child values up through
/// their parents, so it is computed once when the book is opened rather than
/// once per move. The book itself never changes while a game is played.
struct OpeningBook {
    mode: Mode,
    book: Book,
    values: HashMap<u64, i32>,
    /// What the book's log looked like when it was read.
    ///
    /// A book is normally static, but it is not always: a build appends to
    /// `nodes.log` for hours, and a bot left running across one would
    /// otherwise hold the snapshot it happened to start with until somebody
    /// restarted it. Comparing this is one `stat` per game against re-reading
    /// a graph per game, and it means a rebuilt book is simply picked up.
    stamp: Option<std::time::SystemTime>,
}

/// When a book's append-only record last changed, if it can be read at all.
///
/// `None` is a fine answer and means "cannot tell" -- for a directory that is
/// not there, and for anything else the filesystem declines to say. Two
/// `None`s comparing equal is the conservative reading: keep what is loaded.
fn book_stamp(directory: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(directory.join("nodes.log"))
        .and_then(|data| data.modified())
        .ok()
}

impl RpsiSession {
    fn new(options: RpsiOptions) -> Self {
        let mode = Mode::TotalWar;
        let mut searcher = Searcher::new(options.hash_megabytes);
        searcher.set_eval_params(options.params);
        Self {
            mode,
            position: Position::starting(mode),
            history: Vec::new(),
            searcher,
            params: options.params,
            hash_megabytes: options.hash_megabytes,
            requested_hash_megabytes: options.hash_megabytes,
            selective: true,
            rule_proofs: true,
            variations: options.variations,
            book_root: options.book_root,
            book_min_depth: options.book_min_depth,
            force_depth: options.force_depth,
            book: None,
            observed_nps: DEFAULT_NPS_ESTIMATE,
        }
    }

    /// Open the book for the mode about to be played, or report why not.
    ///
    /// Failure is never fatal and never silent: a missing directory, a book
    /// built under other weights, and a corrupt log all end with this engine
    /// searching every move, which is what it did before it could read a book
    /// at all. The `info string` is how the bot's owner finds out, since a
    /// book that quietly failed to load looks exactly like a book that had
    /// nothing to say.
    fn load_book(&mut self) {
        let Some(root) = self.book_root.as_ref() else {
            self.book = None;
            return;
        };
        let directory = root.join(self.mode.id());
        let stamp = book_stamp(&directory);
        // A bot game opens with the clock already running, so `newgame` is on
        // the engine's time even though the protocol allows it to be slow.
        // Reading a graph off disk and backing every value up through it is
        // seconds of work on a full book, so the one already loaded is kept
        // unless the mode changed or the book on disk did.
        if self
            .book
            .as_ref()
            .is_some_and(|book| book.mode == self.mode && book.stamp == stamp)
        {
            return;
        }
        self.book = None;
        match Book::open_existing(&directory, &self.params) {
            Ok(book) => {
                let values = book.values();
                println!(
                    "info string book {} positions for {} from {}",
                    book.len(),
                    self.mode.id(),
                    directory.display(),
                );
                self.book = Some(OpeningBook {
                    mode: self.mode,
                    book,
                    values,
                    stamp,
                });
            }
            Err(error) => println!("info string book unavailable: {error}"),
        }
    }

    /// The book's answer for this position, with the depth and score behind
    /// it, if it has one worth playing unsearched.
    ///
    /// Four gates, and each one closes a way a book can be worse than simply
    /// thinking. A record marked `path_dependent` was scored by a search that
    /// saw a repetition, so its value belongs to the line that reached it and
    /// not to this game. A record below `book_min_depth` is a survey stub. A
    /// move the current position rejects means the book and the rules have
    /// diverged, which is worth falling back on rather than arguing with. And
    /// a move into a position this game has already stood in walks toward the
    /// repetition draw that a book deliberately does not store -- the one
    /// piece of the rules that lives in the move list rather than the board.
    fn book_move(&self, affordable: Option<u64>) -> Option<(Move, i32, u8)> {
        let entry = self.book.as_ref()?;
        if entry.mode != self.mode {
            return None;
        }
        let (canonical, frame) = entry.book.meta().symmetry.canonical(&self.position);
        let node = entry.book.node(canonical.hash())?;
        if node.path_dependent || node.depth < self.book_min_depth {
            return None;
        }
        // The book is only worth playing where it looked harder than this
        // engine is about to. That is not true everywhere in a book, and the
        // reason is that a book is built for breadth: `openings.taper` spends
        // 400M nodes on the first move and 5M forty plies in, while a 5+3
        // clock buys something like 70M every move. So the deep end of a book
        // is *weaker* than thinking, and a depth floor alone cannot tell --
        // depth is a fact about a position's branching as much as about
        // effort. Node counts compare directly.
        if affordable.is_some_and(|budget| node.nodes < budget) {
            return None;
        }
        let edge = entry.book.best_edge(node, &entry.values)?;
        let movement = frame.movement(edge.movement);
        if !self.position.is_legal_move(movement) {
            return None;
        }
        let mut after = self.position;
        after.make_move(movement).ok()?;
        if self.history.contains(&after.hash()) {
            return None;
        }
        Some((
            movement,
            entry.book.edge_value(edge, &entry.values),
            node.depth,
        ))
    }

    /// Rebuild the searcher around a new table size, carrying every setting
    /// over. `Searcher` owns its table, so resizing means replacing it.
    fn rebuild_searcher(&mut self) {
        self.searcher = Searcher::new(self.hash_megabytes);
        self.searcher.set_eval_params(self.params);
        self.searcher.set_selective(self.selective);
        self.searcher.set_rule_proofs(self.rule_proofs);
    }

    fn identify(&self) {
        println!("id name RPSFish {}", env!("CARGO_PKG_VERSION"));
        println!("id author {}", env!("CARGO_PKG_AUTHORS"));
        println!("protocol 1");
        println!("rules {}", rpsfish::RULES_VERSION);
        // The declared default is what this run starts from, command line
        // included, so a log of the handshake says how the engine was launched.
        println!(
            "option name Hash type spin default {} min 1 max {MAX_HASH_MEGABYTES}",
            self.hash_megabytes,
        );
        println!(
            "option name MultiPV type spin default {} min 1 max {MAX_VARIATIONS}",
            self.variations,
        );
        println!("option name Selective type check default true");
        println!("option name RuleProofs type check default true");
        println!("option name Param type string default");
        for mode in Mode::ALL {
            println!("mode {} {}", mode.id(), mode.name());
        }
        println!("rpsiok");
    }

    fn set_option(&mut self, arguments: &[&str]) {
        // `setoption name <id> [value <rest>]`, where the value may contain
        // spaces and so runs to the end of the line.
        let Some(name_index) = arguments.iter().position(|token| *token == "name") else {
            println!("info string setoption without a name");
            return;
        };
        let value_index = arguments.iter().position(|token| *token == "value");
        let name_end = value_index.unwrap_or(arguments.len());
        let name = arguments[name_index + 1..name_end].join(" ");
        let value = value_index
            .map(|index| arguments[index + 1..].join(" "))
            .unwrap_or_default();

        match name.as_str() {
            "Hash" => match value.parse::<usize>() {
                Ok(megabytes) if (1..=MAX_HASH_MEGABYTES).contains(&megabytes) => {
                    self.requested_hash_megabytes = megabytes;
                }
                _ => println!("info string Hash must be between 1 and {MAX_HASH_MEGABYTES}"),
            },
            "MultiPV" => match value.parse::<usize>() {
                Ok(count) if (1..=MAX_VARIATIONS).contains(&count) => self.variations = count,
                _ => println!("info string MultiPV must be between 1 and {MAX_VARIATIONS}"),
            },
            "Selective" => {
                self.selective = value != "false";
                self.searcher.set_selective(self.selective);
            }
            "RuleProofs" => {
                self.rule_proofs = value != "false";
                self.searcher.set_rule_proofs(self.rule_proofs);
            }
            // Weights are data, so the same assignment syntax `--param` takes
            // works here, validation included.
            "Param" => match self.params.apply_assignments(value.split(',')) {
                Ok(()) => self.searcher.set_eval_params(self.params),
                Err(error) => println!("info string Param: {error}"),
            },
            other => println!("info string unknown option {other:?}"),
        }
    }

    fn ready(&mut self) {
        if self.requested_hash_megabytes != self.hash_megabytes {
            self.hash_megabytes = self.requested_hash_megabytes;
            self.rebuild_searcher();
        }
        println!("readyok");
    }

    fn new_game(&mut self, arguments: &[&str]) {
        match arguments.first().and_then(|value| Mode::parse(value)) {
            Some(mode) => self.mode = mode,
            None => {
                println!("info string newgame without a known mode, keeping the current one");
            }
        }
        self.searcher.clear();
        self.position = Position::starting(self.mode);
        self.history.clear();
        // A book is per mode and a game may change the mode, so this is where
        // it is opened. `newgame` is also the one command the protocol allows
        // to be slow, which is what reading a graph off disk needs.
        self.load_book();
    }

    /// `position fen <pieces> <side> <territory> [moves <m>...]`
    ///
    /// There is no `startpos`: a game records the board it was actually played
    /// from, so assuming the mode's current opening would occasionally analyse
    /// a different game than the one being played.
    fn set_position(&mut self, arguments: &[&str]) {
        if arguments.first() != Some(&"fen") || arguments.len() < 4 {
            println!("info string position needs a three-field FEN");
            return;
        }
        let fen = arguments[1..4].join(" ");
        let position = match Position::from_fen(self.mode, &fen) {
            Ok(position) => position,
            Err(error) => {
                println!("info string position: {error}");
                return;
            }
        };

        let mut line = Vec::new();
        if let Some(index) = arguments.iter().position(|token| *token == "moves") {
            for token in &arguments[index + 1..] {
                let Some(movement) = Move::parse(token) else {
                    println!("info string position: {token:?} is not a move like \"d3-d4\"");
                    return;
                };
                line.push(movement);
            }
        }

        match replay_line(position, &line) {
            Ok((position, history)) => {
                self.position = position;
                self.history = history;
            }
            Err(error) => println!("info string position: {error}"),
        }
    }

    /// The host's legal-move list is advisory, so this only cross-checks it.
    ///
    /// A disagreement means the two rule implementations have diverged, which
    /// is worth shouting about the moment it happens rather than discovering
    /// through a rejected move in a live game.
    fn check_legal_moves(&self, arguments: &[&str]) {
        let mine = self.position.legal_moves();
        let rejected: Vec<&str> = arguments
            .iter()
            .copied()
            .filter(|token| !Move::parse(token).is_some_and(|movement| mine.contains(movement)))
            .collect();
        if !rejected.is_empty() {
            println!(
                "info string legalmoves mismatch: host offered {}, which this position rejects",
                rejected.join(" ")
            );
        }
        if mine.len() != arguments.len() {
            println!(
                "info string legalmoves mismatch: host sent {} moves, this position has {}",
                arguments.len(),
                mine.len()
            );
        }
    }

    fn go(&mut self, arguments: &[&str], stop: &AtomicBool) {
        stop.store(false, Ordering::Relaxed);
        let limits = self.parse_go(arguments);
        // A book answer is still an answer, so it owes the host the same lines
        // a search would print -- one `info` describing where the move came
        // from, then the `bestmove`. `nodes 0` is the honest count: this move
        // cost this engine nothing, which is the other half of what a book
        // buys at a real time control.
        let book_move = match self.force_depth {
            // See RpsiOptions::force_depth. A book move is not a depth-N search,
            // and a book is a file somebody rebuilds.
            Some(_) => None,
            None => self.book_move(self.affordable_nodes(limits)),
        };
        if let Some((movement, score, depth)) = book_move {
            println!(
                "info depth {depth} score {} nodes 0 pv {}",
                format_score(score),
                movement.notation(),
            );
            println!("info string book move {}", movement.notation());
            println!("bestmove {}", movement.notation());
            return;
        }
        let variations = self.variations;
        let history = std::mem::take(&mut self.history);
        let result = self.searcher.analyze_with_context_and_updates(
            self.position,
            limits,
            variations,
            &history,
            Some(stop),
            print_update,
        );
        self.history = history;
        self.observe_speed(&result.stats);
        match result.lines.first() {
            Some(best) => println!("bestmove {}", best.movement.notation()),
            // A host should never ask about a finished position, but answering
            // nothing would hang it. Saying so is better than silence.
            None => println!("info string no legal move in this position"),
        }
    }

    /// Roughly how many nodes this engine can search under `limits`.
    ///
    /// `None` when nothing bounds the search, in which case no record can be
    /// said to know more than thinking would.
    fn affordable_nodes(&self, limits: SearchLimits) -> Option<u64> {
        match (limits.max_nodes, limits.move_time) {
            // A node limit is the answer already, and the smaller of the two
            // is what actually stops the search.
            (Some(nodes), None) => Some(nodes),
            (None, Some(time)) => Some(self.nodes_in(time)),
            (Some(nodes), Some(time)) => Some(nodes.min(self.nodes_in(time))),
            (None, None) => None,
        }
    }

    fn nodes_in(&self, time: Duration) -> u64 {
        (time.as_secs_f64() * self.observed_nps as f64) as u64
    }

    /// Learn this host's speed from a search that actually ran.
    ///
    /// The rate matters only as a way of asking "would thinking beat the
    /// book here", so it is measured rather than assumed -- the same engine
    /// runs on a laptop and on a server, and a fixed constant would be wrong
    /// on both. Short searches are ignored because their elapsed time is
    /// mostly startup.
    fn observe_speed(&mut self, stats: &SearchStats) {
        let seconds = stats.elapsed.as_secs_f64();
        if seconds >= 0.2 && stats.nodes > 0 {
            self.observed_nps = (stats.nodes as f64 / seconds) as u64;
        }
    }

    fn parse_go(&self, arguments: &[&str]) -> SearchLimits {
        // A yardstick reads nothing off the `go` line. Not the clock, not a node
        // budget, not `infinite`: the point of the flag is that this engine is
        // the same opponent in every game it ever plays, and every one of those
        // would make it a different one depending on what the host asked for.
        if let Some(depth) = self.force_depth {
            return SearchLimits {
                max_depth: depth,
                max_nodes: None,
                move_time: None,
            };
        }
        let mut limits = SearchLimits {
            max_depth: MAX_SEARCH_DEPTH,
            max_nodes: None,
            move_time: None,
        };
        let mut remaining_ms = None;
        let mut increment_ms = 0;
        let mut infinite = false;
        let value = |index: usize| -> Option<u64> {
            arguments.get(index + 1).and_then(|text| text.parse().ok())
        };

        for (index, token) in arguments.iter().enumerate() {
            match *token {
                "infinite" => infinite = true,
                "depth" => {
                    if let Some(depth) = value(index) {
                        limits.max_depth = depth.min(u64::from(MAX_SEARCH_DEPTH)) as u8;
                    }
                }
                "nodes" => limits.max_nodes = value(index),
                "movetime" => limits.move_time = value(index).map(Duration::from_millis),
                "rtime" if self.position.side_to_move() == Color::Red => {
                    remaining_ms = value(index);
                }
                "btime" if self.position.side_to_move() == Color::Blue => {
                    remaining_ms = value(index);
                }
                "rinc" if self.position.side_to_move() == Color::Red => {
                    increment_ms = value(index).unwrap_or(0);
                }
                "binc" if self.position.side_to_move() == Color::Blue => {
                    increment_ms = value(index).unwrap_or(0);
                }
                _ => {}
            }
        }

        if limits.move_time.is_none() && !infinite {
            if let Some(remaining) = remaining_ms {
                limits.move_time = Some(time_budget(remaining, increment_ms));
            } else if limits.max_nodes.is_none() && limits.max_depth == MAX_SEARCH_DEPTH {
                // No budget of any kind was given. Searching toward depth 127
                // forever would hang the host, so fall back to the preset the
                // `search` command uses without `--deep`.
                limits.max_depth = 8;
                limits.max_nodes = Some(5_000_000);
                limits.move_time = Some(Duration::from_secs(10));
            }
        }
        limits
    }
}

/// How long to think, given what is left on the clock.
///
/// A twentieth of the remaining time plus most of the increment, which is the
/// standard shape, and never closer than 200 ms to flagging. The margin is the
/// important part: losing on time is worse than any move a deeper search would
/// have found.
fn time_budget(remaining_ms: u64, increment_ms: u64) -> Duration {
    const MARGIN_MS: u64 = 200;
    const FLOOR_MS: u64 = 50;
    let target = remaining_ms / 20 + increment_ms * 3 / 4;
    let ceiling = remaining_ms.saturating_sub(MARGIN_MS).max(FLOOR_MS);
    Duration::from_millis(target.clamp(FLOOR_MS, ceiling))
}

/// Speak RPSI on stdin and stdout. See `docs/RPSI.md`.
///
/// Input is read on its own thread so that `stop` can be observed while the
/// main thread is inside a search: the reader sets the cancellation flag the
/// searcher already polls every 1024 nodes, so stopping never waits for the
/// main thread to become free.
fn run_rpsi(options: RpsiOptions) -> Result<(), String> {
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel::<String>();
    let reader_stop = Arc::clone(&stop);
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let command = line.trim();
            if command == "stop" || command == "quit" {
                reader_stop.store(true, Ordering::Relaxed);
            }
            let quitting = command == "quit";
            if sender.send(line).is_err() || quitting {
                break;
            }
        }
    });

    let mut session = RpsiSession::new(options);
    while let Ok(line) = receiver.recv() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some((command, arguments)) = tokens.split_first() else {
            continue;
        };
        match *command {
            "rpsi" => session.identify(),
            "isready" => session.ready(),
            "setoption" => session.set_option(arguments),
            "newgame" => session.new_game(arguments),
            "position" => session.set_position(arguments),
            "legalmoves" => session.check_legal_moves(arguments),
            "go" => session.go(arguments, &stop),
            // Both are handled by the reader thread setting the flag. By the
            // time either arrives here any search has already returned.
            "stop" => {}
            "quit" => break,
            unknown => println!("info string unknown command {unknown:?}"),
        }
    }
    Ok(())
}

fn print_usage() {
    eprintln!(
        "usage:\n  rpsfish search [mode] [depth] [--deep] [--depth N] [--nodes N] [--time-ms N] [--multipv N] [--hash-mb N] [--line \"d3-d4 d7-d6\"] [--param k=v,..] [--params-file F] [--show-params]\n  rpsfish perft <total-war|infiltration> [depth]\n  rpsfish rpsi [--hash-mb N] [--multipv N] [--param k=v,..] [--params-file F] [--book DIR] [--book-min-depth N] [--force-depth N]\n\n--deep searches toward depth 127 with 50M-node and 60-second safety caps; explicit limits override the preset.\n--line plays moves from the starting position first, so any position can be searched; the moves before it are kept as repetition history.\nEvaluation weights are data: --param and --params-file load a tuned set produced by the arena binary.\nrpsi speaks the engine protocol on stdin and stdout, which is how a host drives this engine; see docs/RPSI.md.\n--book names the directory holding one book per mode, as scripts/build_books.sh writes it; a mode with no book there is searched as usual.\nA host never sends setoption, so a bot playing online is configured by the flags on its rpsi command line.
--force-depth searches exactly that deep on every move, ignores the clock entirely and plays no book, which is what a fixed rung of a rating scale has to do. It is not a time control: a deep setting can take far longer than the host allows and lose on time."
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        DEFAULT_BOOK_MIN_DEPTH, DEFAULT_HASH_MEGABYTES, DEFAULT_NPS_ESTIMATE, MAX_SEARCH_DEPTH,
        MAX_VARIATIONS, RpsiOptions, RpsiSession, format_score, parse_rpsi_options,
        parse_search_options, replay_line, run, time_budget,
    };
    use rpsfish::book::{Book, Edge, Node, Symmetry};
    use rpsfish::{
        Color, MATE_SCORE, Mode, Move, Position, SearchStats, SearchStopReason, Transform,
    };

    #[test]
    fn help_and_bad_arguments_are_handled() {
        assert!(run(vec!["help".to_owned()]).is_ok());
        assert!(run(vec!["unknown".to_owned()]).is_err());
        assert!(
            run(vec![
                "search".to_owned(),
                "total-war".to_owned(),
                "0".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn deep_preset_remains_bounded_and_can_be_overridden() {
        let arguments = [
            "infiltration".to_owned(),
            "--deep".to_owned(),
            "--depth".to_owned(),
            "24".to_owned(),
            "--nodes".to_owned(),
            "12345".to_owned(),
            "--time-ms".to_owned(),
            "678".to_owned(),
            "--multipv".to_owned(),
            "2".to_owned(),
        ];
        let options = parse_search_options(&arguments).expect("options must parse");
        assert_eq!(options.limits.max_depth, 24);
        assert_eq!(options.limits.max_nodes, Some(12_345));
        assert_eq!(options.limits.move_time.unwrap().as_millis(), 678);
        assert_eq!(options.variations, 2);

        let deep = parse_search_options(&["--deep".to_owned()]).expect("preset must parse");
        assert_eq!(deep.limits.max_depth, MAX_SEARCH_DEPTH);
        assert!(deep.limits.max_nodes.is_some());
        assert!(deep.limits.move_time.is_some());
    }

    #[test]
    fn weights_can_be_overridden_on_the_command_line() {
        let options = parse_search_options(&[
            "total-war".to_owned(),
            "--param".to_owned(),
            "total_war_material=333,total_war_mobility=7".to_owned(),
        ])
        .expect("options must parse");
        assert_eq!(options.params.total_war_material, 333);
        assert_eq!(options.params.total_war_mobility, 7);

        assert!(
            parse_search_options(&["--param".to_owned(), "nope=1".to_owned()]).is_err(),
            "unknown weights must be rejected rather than silently ignored"
        );
    }

    #[test]
    fn scores_are_written_the_way_rpsi_reads_them() {
        assert_eq!(format_score(0), "cp 0");
        assert_eq!(format_score(-137), "cp -137");
        // A proven result is win/loss in plies, never chess's "mate".
        assert_eq!(format_score(MATE_SCORE - 7), "win 7");
        assert_eq!(format_score(7 - MATE_SCORE), "loss 7");
    }

    #[test]
    fn a_time_budget_always_leaves_room_to_answer() {
        // A healthy clock spends roughly a twentieth plus most of the increment.
        assert_eq!(time_budget(300_000, 3_000).as_millis(), 17_250);
        // Down to the last moments it still returns something playable, and
        // never asks for more time than remains.
        for remaining in [0, 1, 50, 199, 200, 1_000] {
            let budget = u64::try_from(time_budget(remaining, 0).as_millis()).unwrap();
            assert!(budget >= 50, "{remaining}ms produced a {budget}ms budget");
            assert!(
                budget <= remaining.max(50),
                "{remaining}ms produced a {budget}ms budget, which overruns the clock"
            );
        }
    }

    #[test]
    fn replaying_a_line_records_every_position_before_it() {
        let start = Position::starting(Mode::TotalWar);
        // Blue opens, so the line starts off Blue's own rock rank.
        let line: Vec<Move> = ["d3-d4", "d7-d6"]
            .iter()
            .map(|token| Move::parse(token).expect("test moves must parse"))
            .collect();
        let (reached, history) = replay_line(start, &line).expect("legal line must replay");
        assert_eq!(history.len(), line.len());
        assert_eq!(history[0], start.hash());
        assert_ne!(reached.hash(), start.hash());
        // Repetition detection is the whole reason the history exists, so it
        // must hold the positions *before* each move, not after them.
        assert!(!history.contains(&reached.hash()));
    }

    #[test]
    fn replaying_an_illegal_line_names_the_move_that_failed() {
        let illegal = Move::parse("a1-i9").expect("a well-formed move");
        let error = replay_line(Position::starting(Mode::TotalWar), &[illegal])
            .expect_err("an illegal move must not replay");
        assert!(error.contains("a1-i9"), "unhelpful error: {error}");
    }

    #[test]
    fn an_online_bot_is_configured_by_its_command_line() {
        // Nothing sends `setoption` in a real game, so these flags are the
        // only way a bot playing online gets a bigger table or tuned weights.
        let arguments = [
            "--hash-mb".to_owned(),
            "256".to_owned(),
            "--multipv".to_owned(),
            "3".to_owned(),
            "--param".to_owned(),
            "total_war_material=333".to_owned(),
        ];
        let options = parse_rpsi_options(&arguments).expect("options must parse");
        assert_eq!(options.hash_megabytes, 256);
        assert_eq!(options.variations, 3);
        assert_eq!(options.params.total_war_material, 333);

        let session = RpsiSession::new(options);
        assert_eq!(session.hash_megabytes, 256);
        assert_eq!(session.requested_hash_megabytes, 256);
        assert_eq!(session.variations, 3);

        let bare = parse_rpsi_options(&[]).expect("no flags is the ordinary case");
        assert_eq!(bare.hash_megabytes, DEFAULT_HASH_MEGABYTES);
        assert_eq!(bare.variations, 1);
    }

    #[test]
    fn a_mistyped_rpsi_flag_is_refused_rather_than_ignored() {
        // The protocol ignores lines it does not know, but a flag typed into
        // an rpsbot.py command is a mistake worth stopping for.
        for arguments in [
            vec!["--hash".to_owned(), "256".to_owned()],
            vec!["--hash-mb".to_owned()],
            vec!["--params-file".to_owned(), "no-such-file".to_owned()],
            vec!["--param".to_owned(), "nope=1".to_owned()],
            vec!["--multipv".to_owned(), (MAX_VARIATIONS + 1).to_string()],
        ] {
            assert!(
                parse_rpsi_options(&arguments).is_err(),
                "{arguments:?} must not start an engine"
            );
        }
    }

    #[test]
    fn a_position_command_survives_a_malformed_fen() {
        // The protocol has no error reply: a bad line is reported with `info
        // string` and the session carries on with the position it already had.
        let mut session = RpsiSession::new(RpsiOptions::DEFAULT);
        let before = session.position.to_fen();
        session.set_position(&["fen", "not-a-board", "r", "9/9/9/9/9/9/9/9/9"]);
        assert_eq!(session.position.to_fen(), before);
        session.set_position(&["startpos"]);
        assert_eq!(session.position.to_fen(), before);
    }

    #[test]
    fn rpsi_accepts_an_intransitive_position() {
        let mut session = RpsiSession::new(RpsiOptions::DEFAULT);
        session.new_game(&["V6"]);
        session.set_position(&[
            "fen",
            "9/4PR3/4SPR2/5SPR1/1ps3SP1/1rps5/2rps4/3rp4/9",
            "b",
            "9/4bb3/4bbb2/5bbb1/1rr3bb1/1rrr5/2rrr4/3rr4/9",
        ]);

        assert_eq!(session.mode, Mode::Intransitive);
        assert_eq!(session.position.mode(), Mode::Intransitive);
        assert_eq!(session.position.side_to_move(), Color::Blue);
        assert!(!session.position.legal_moves().is_empty());
    }

    #[test]
    fn go_without_any_budget_still_terminates() {
        // A host that sends a bare `go` must not hang: depth 127 with no other
        // limit would search until the heat death of the process.
        let session = RpsiSession::new(RpsiOptions::DEFAULT);
        let limits = session.parse_go(&[]);
        assert!(limits.move_time.is_some() || limits.max_nodes.is_some());
        assert!(limits.max_depth < MAX_SEARCH_DEPTH);
    }

    #[test]
    fn go_reads_the_clock_belonging_to_the_side_to_move() {
        let mut session = RpsiSession::new(RpsiOptions::DEFAULT);
        session.new_game(&["V5"]);
        // Blue moves first from the starting position, so rtime is not its
        // clock; the FEN below hands the same board to Red instead.
        let blue = session.parse_go(&["rtime", "600000", "btime", "60000"]);
        session.set_position(&[
            "fen",
            "3SSS3/3PPP3/3RRR3/9/9/9/3rrr3/3ppp3/3sss3",
            "r",
            "3bbb3/3bbb3/3bbb3/9/9/9/3rrr3/3rrr3/3rrr3",
        ]);
        let red = session.parse_go(&["rtime", "600000", "btime", "60000"]);
        assert!(
            red.move_time > blue.move_time,
            "each side must be budgeted from its own clock: red {:?}, blue {:?}",
            red.move_time,
            blue.move_time
        );
    }

    /// A scratch book directory that removes itself.
    ///
    /// The crate takes no dependencies, so there is no `tempfile` to reach for
    /// -- the same reason `tests/book.rs` grows its own.
    struct ScratchBook(std::path::PathBuf);

    impl ScratchBook {
        /// A book holding one record for Intransitive's opening, listing
        /// `movement` as its best move.
        fn with_opening_record(
            name: &str,
            movement: Move,
            depth: u8,
            path_dependent: bool,
        ) -> Self {
            let root = std::env::temp_dir().join(format!("rpsfish-rpsi-book-{name}"));
            let _ = std::fs::remove_dir_all(&root);
            let scratch = Self(root);
            let mut book = Book::open(
                scratch.directory(),
                Mode::Intransitive,
                &rpsfish::EvalParams::DEFAULT,
                // The fold the build script gives this mode, because a corner
                // goal names a file and so file reversal is not a symmetry of
                // its rules.
                Symmetry::None,
            )
            .expect("book opens");
            book.open_log().expect("log opens");
            let key = book.root();
            book.insert(
                key,
                Node {
                    side_to_move: Color::Red,
                    depth,
                    selective_depth: depth + 6,
                    nodes: 400_000_000,
                    stop: SearchStopReason::NodeLimit,
                    path_dependent,
                    score: 40,
                    edges: vec![Edge {
                        movement,
                        score: 40,
                        child: 0xabcd,
                        transform: Transform::IDENTITY,
                    }],
                    principal_variation: vec![movement],
                },
                &[],
            )
            .expect("insert");
            book.flush().expect("flush");
            scratch
        }

        /// Append a deeper record for the opening, as a running build would.
        fn deepen_to(&self, movement: Move, depth: u8) {
            let mut book = Book::open(
                self.directory(),
                Mode::Intransitive,
                &rpsfish::EvalParams::DEFAULT,
                Symmetry::None,
            )
            .expect("book reopens");
            book.open_log().expect("log opens");
            let key = book.root();
            book.insert(
                key,
                Node {
                    side_to_move: Color::Red,
                    depth,
                    selective_depth: depth + 6,
                    // Deeper *and* wider, so this record supersedes rather
                    // than merging underneath the one already there.
                    nodes: 900_000_000,
                    stop: SearchStopReason::NodeLimit,
                    path_dependent: false,
                    score: 400,
                    edges: vec![Edge {
                        movement,
                        score: 400,
                        child: 0xfeed,
                        transform: Transform::IDENTITY,
                    }],
                    principal_variation: vec![movement],
                },
                &[],
            )
            .expect("insert");
            book.flush().expect("flush");
        }

        /// The root a `--book` flag would name: books live under it by mode.
        fn root(&self) -> &std::path::Path {
            &self.0
        }

        fn directory(&self) -> std::path::PathBuf {
            self.0.join(Mode::Intransitive.id())
        }
    }

    impl Drop for ScratchBook {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn session_reading(scratch: &ScratchBook) -> RpsiSession {
        let mut options = RpsiOptions::DEFAULT;
        options.book_root = Some(scratch.root().to_path_buf());
        let mut session = RpsiSession::new(options);
        session.new_game(&["V6"]);
        session
    }

    #[test]
    fn a_book_record_is_played_without_searching() {
        let start = Position::starting(Mode::Intransitive);
        let expected = *start
            .legal_moves()
            .iter()
            .next()
            .expect("the opening has moves");
        let scratch = ScratchBook::with_opening_record("plays", expected, 24, false);
        let session = session_reading(&scratch);

        assert!(session.book.is_some(), "the book must have opened");
        let (movement, _, depth) = session
            .book_move(None)
            .expect("a deep record for this position must answer");
        assert_eq!(movement, expected);
        assert_eq!(depth, 24);
    }

    #[test]
    fn a_book_is_ignored_where_it_would_be_worse_than_thinking() {
        let start = Position::starting(Mode::Intransitive);
        let expected = *start
            .legal_moves()
            .iter()
            .next()
            .expect("the opening has moves");

        // A survey stub. The record is real and its score is real; it is just
        // shallower than the live search that would replace it.
        let shallow = ScratchBook::with_opening_record("shallow", expected, 4, false);
        assert!(
            session_reading(&shallow).book_move(None).is_none(),
            "a record below the depth floor must fall through to the search"
        );

        // A value that belonged to the line that reached it rather than to
        // the position, which this game arrived at some other way.
        let dependent = ScratchBook::with_opening_record("dependent", expected, 24, true);
        assert!(
            session_reading(&dependent).book_move(None).is_none(),
            "a path-dependent record must fall through to the search"
        );

        // The one thing a book does not store, because it is a property of the
        // path rather than the position: playing this move would stand in a
        // position the game has already stood in, and the value the book holds
        // was scored without knowing that. No mode draws on a repeat now, but
        // the book still may not answer here -- its score for this position
        // belongs to a line that reached it differently.
        let repeating = ScratchBook::with_opening_record("repeat", expected, 24, false);
        let mut session = session_reading(&repeating);
        let mut after = session.position;
        after.make_move(expected).expect("legal");
        session.history.push(after.hash());
        assert!(
            session.book_move(None).is_none(),
            "a book move into a position already seen must fall through"
        );
    }

    #[test]
    fn a_book_gives_way_to_a_search_that_can_afford_more_nodes() {
        let start = Position::starting(Mode::Intransitive);
        let expected = *start
            .legal_moves()
            .iter()
            .next()
            .expect("the opening has moves");
        // `ScratchBook` records 400M nodes, which is what the shipped taper
        // spends on a first move and far more than it spends deep in a line.
        let scratch = ScratchBook::with_opening_record("effort", expected, 24, false);
        let session = session_reading(&scratch);

        assert!(
            session.book_move(Some(100_000_000)).is_some(),
            "a record that looked harder than this search will must be played"
        );
        assert!(
            session.book_move(Some(500_000_000)).is_none(),
            "a record that looked less hard than this search will must give way"
        );
        assert!(
            session.book_move(None).is_some(),
            "an unbounded budget cannot be compared, so the depth floor decides"
        );
    }

    #[test]
    fn a_measured_search_speed_replaces_the_assumed_one() {
        let mut session = RpsiSession::new(RpsiOptions::DEFAULT);
        assert_eq!(session.observed_nps, DEFAULT_NPS_ESTIMATE);

        // Too short to mean anything: mostly startup, so it is ignored.
        session.observe_speed(&SearchStats {
            nodes: 1_000,
            elapsed: Duration::from_millis(10),
            ..Default::default()
        });
        assert_eq!(session.observed_nps, DEFAULT_NPS_ESTIMATE);

        session.observe_speed(&SearchStats {
            nodes: 6_000_000,
            elapsed: Duration::from_secs(2),
            ..Default::default()
        });
        assert_eq!(session.observed_nps, 3_000_000);
        assert_eq!(session.nodes_in(Duration::from_secs(10)), 30_000_000);
    }

    #[test]
    fn a_second_game_in_one_mode_does_not_reload_an_unchanged_book() {
        let start = Position::starting(Mode::Intransitive);
        let expected = *start
            .legal_moves()
            .iter()
            .next()
            .expect("the opening has moves");
        let scratch = ScratchBook::with_opening_record("reload", expected, 24, false);
        let mut session = session_reading(&scratch);
        assert!(session.book.is_some());

        // Corrupting the metadata while leaving `nodes.log` alone is how this
        // test asks whether the second `newgame` went back to disk: a reload
        // would fail on it and leave no book at all. It must not reload,
        // because a bot game starts with the clock already running.
        std::fs::write(scratch.directory().join("meta.txt"), "not a book\n")
            .expect("corrupt the metadata");
        session.new_game(&["V6"]);
        assert!(
            session.book.is_some(),
            "an unchanged book must be kept rather than read again"
        );

        // A different mode is a different book, so that one does pay -- and
        // there is no V5 book here, so it drops to searching.
        session.new_game(&["V5"]);
        assert!(
            session.book.is_none(),
            "changing mode must drop a book that belongs to the old one"
        );
    }

    #[test]
    fn a_rebuilt_book_is_picked_up_without_a_restart() {
        let start = Position::starting(Mode::Intransitive);
        let opening = start.legal_moves();
        let mut moves = opening.iter().copied();
        let first = moves.next().expect("the opening has moves");
        let second = moves.next().expect("the opening has a second move");

        let scratch = ScratchBook::with_opening_record("rebuild", first, 24, false);
        let mut session = session_reading(&scratch);
        assert_eq!(session.book_move(None).expect("answers").0, first);

        // What a build running under a live bot does: append a deeper record
        // for the same position. Nothing restarts, and the next game must use
        // it -- otherwise a bot left running across a rebuild holds whatever
        // snapshot it started with.
        scratch.deepen_to(second, 30);
        session.new_game(&["V6"]);
        assert_eq!(
            session.book_move(None).expect("answers").0,
            second,
            "a book that changed on disk must be read again"
        );
    }

    #[test]
    fn a_missing_book_leaves_the_engine_searching() {
        let mut options = RpsiOptions::DEFAULT;
        options.book_root = Some(std::env::temp_dir().join("rpsfish-rpsi-book-absent"));
        let mut session = RpsiSession::new(options);
        session.new_game(&["V6"]);
        assert!(session.book.is_none());
        assert!(session.book_move(None).is_none());
        // And the default floor is the one the flag documents.
        assert_eq!(session.book_min_depth, DEFAULT_BOOK_MIN_DEPTH);
    }

    #[test]
    fn a_book_flag_is_parsed_onto_the_session() {
        let options = parse_rpsi_options(&[
            "--book".to_owned(),
            "book".to_owned(),
            "--book-min-depth".to_owned(),
            "18".to_owned(),
        ])
        .expect("flags parse");
        assert_eq!(options.book_root, Some(std::path::PathBuf::from("book")));
        assert_eq!(options.book_min_depth, 18);
        assert!(parse_rpsi_options(&["--book".to_owned()]).is_err());
    }
}
