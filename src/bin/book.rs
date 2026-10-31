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

//! Build and read opening books.
//!
//! A single search answers one question. This builds the thing a player wants:
//! the best first move, the best follow-ups, the replies that have to be
//! answered, and how far down each of those stays true -- cached, resumable,
//! and reusable.
//!
//! ```sh
//! cargo run --release --bin book -- build --mode infiltration --max-ply 2 --budget 20000000000
//! cargo run --release --bin book -- show --plies 20
//! cargo run --release --bin book -- study --line "d7-d6 d3-d4 e7-e6"
//! ```
//!
//! Budgets are node counts, never wall-clock, for the same reason the arena
//! uses them: a book built on one machine has to be the book any other machine
//! would have built.

use std::collections::{HashMap, HashSet};
use std::env;
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Instant;

use rpsfish::book::{Book, CertaintyLimits, Edge, Frontier, Node, Symmetry};
use rpsfish::{
    AnalysisResult, EvalParams, Mode, Move, Position, SearchLimits, SearchStopReason, Searcher,
};

const MAX_SEARCH_DEPTH: u8 = 127;

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    match run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "usage:
  book build [options]     expand the book toward --max-ply until --budget runs out
  book show  [options]     print the book's verdict on a position
  book study --line <moves>  grade a line you found yourself, and store it
  book export [options]    write website-importable JSON
  book certify [options]   the openings this book is actually sure of
  book stats [options]     what the book contains
  book compact [options]   drop superseded records from the log

common:
  --dir <path>         book directory              default: book/<mode>
  --mode <total-war|infiltration|intransitive>     default: infiltration
  --symmetry <files|full|none>  positions to fold  default: files
  --params-file <file> evaluation weights          default: shipped weights

build:
  --max-ply <n>        deepest line to build       default: 50
  --budget <nodes>     total search nodes to spend default: 20000000000
  --threads <n>        parallel searches           default: available parallelism
  --hash-mb <n>        transposition table each    default: 64
  --batch <n>          positions per dispatch      default: 4x threads
  --taper <file>       per-ply budgets and margins default: built in

show:
  --line <moves>       position to report on       default: the start
  --plies <n>          principal line length       default: 24
  --width <n>          alternatives to list        default: 6

export:
  --format <graph|tree>  graph: a flat position graph the website serves one
                         position at a time. tree: the nested, download-whole
                         shape.                      default: graph
  --output <path|->    JSON destination             default: stdout
  --plies <n>          deepest line to export       default: 24
  --featured <n>       openings shipped up front    default: 3 (graph only)
  --featured-plies <n> how far each is followed     default: 12 (graph only)

certify (also applied by export --format graph):
  --certain-depth <n>  search depth a claim needs   default: 14
  --certain-plies <n>  longest line to certify      default: 8
  --certain-min <n>    shortest line worth listing  default: 2
  --certain-max <n>    openings to certify, at most default: 8

A certified opening runs while exactly one continuation is strictly best and
stops where the engine is indifferent. The test is on the ranking, not on a
score margin, so it needs no recalibrating when the evaluation is re-tuned.
  --width <n>          responses at each position   default: 6 (tree only)
  --max-positions <n>  cap expanded tree nodes      default: 10000 (tree only)

study:
  --line <moves>       the line to grade, required
  --nodes <n>          per-position budget         default: 30000000
  --multipv <n>        variations per position     default: 4

A line is written in the archive's squares: \"d7-d6 d3-d4 e7-e6\".";

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

struct Options {
    directory: Option<String>,
    mode: Mode,
    symmetry: Symmetry,
    params: EvalParams,
    max_ply: u16,
    budget: u64,
    threads: usize,
    hash_megabytes: usize,
    batch: Option<usize>,
    taper: Taper,
    line: Option<String>,
    plies: usize,
    width: usize,
    nodes: u64,
    variations: usize,
    output: String,
    max_positions: usize,
    graph: bool,
    featured: usize,
    featured_plies: usize,
    certainty: CertaintyLimits,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            directory: None,
            mode: Mode::Infiltration,
            symmetry: Symmetry::Files,
            params: EvalParams::DEFAULT,
            max_ply: 50,
            budget: 20_000_000_000,
            threads: thread::available_parallelism().map_or(1, |value| value.get()),
            hash_megabytes: 64,
            batch: None,
            taper: Taper::default(),
            line: None,
            plies: 24,
            width: 6,
            nodes: 30_000_000,
            variations: 4,
            output: "-".to_owned(),
            max_positions: 10_000,
            graph: true,
            featured: 3,
            featured_plies: 12,
            certainty: CertaintyLimits::default(),
        }
    }
}

impl Options {
    fn parse(arguments: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        let mut index = 0;
        while index < arguments.len() {
            let flag = arguments[index].as_str();
            index += 1;
            let mut value = || -> Result<&str, String> {
                let found = arguments
                    .get(index)
                    .ok_or_else(|| format!("{flag} needs a value"))?;
                index += 1;
                Ok(found.as_str())
            };
            match flag {
                "--dir" => options.directory = Some(value()?.to_owned()),
                "--mode" => {
                    let text = value()?;
                    options.mode =
                        Mode::parse(text).ok_or_else(|| format!("unknown mode {text:?}"))?;
                }
                "--symmetry" => {
                    let text = value()?;
                    options.symmetry = Symmetry::parse(text)
                        .ok_or_else(|| format!("unknown symmetry {text:?}"))?;
                }
                "--params-file" => {
                    let path = value()?;
                    let body = std::fs::read_to_string(path)
                        .map_err(|error| format!("reading {path}: {error}"))?;
                    options
                        .params
                        .apply_assignments(body.lines())
                        .map_err(|error| format!("{path}: {error}"))?;
                }
                "--max-ply" => options.max_ply = parse_number(value()?, "max ply")?,
                "--budget" => options.budget = parse_number(value()?, "budget")?,
                "--threads" => options.threads = parse_number(value()?, "threads")?,
                "--hash-mb" => options.hash_megabytes = parse_number(value()?, "hash size")?,
                "--batch" => options.batch = Some(parse_number(value()?, "batch")?),
                "--taper" => {
                    let path = value()?;
                    let body = std::fs::read_to_string(path)
                        .map_err(|error| format!("reading {path}: {error}"))?;
                    options.taper = Taper::parse(&body)?;
                }
                "--line" => options.line = Some(value()?.to_owned()),
                "--plies" => options.plies = parse_number(value()?, "plies")?,
                "--width" => options.width = parse_number(value()?, "width")?,
                "--nodes" => options.nodes = parse_number(value()?, "nodes")?,
                "--multipv" => options.variations = parse_number(value()?, "MultiPV count")?,
                "--output" => options.output = value()?.to_owned(),
                "--format" => {
                    options.graph = match value()? {
                        "graph" | "flat" => true,
                        "tree" | "nested" => false,
                        other => return Err(format!("unknown export format {other:?}")),
                    };
                }
                "--featured" => {
                    options.featured = parse_number(value()?, "featured")?;
                }
                "--featured-plies" => {
                    options.featured_plies = parse_number(value()?, "featured-plies")?;
                }
                "--max-positions" => {
                    options.max_positions = parse_number(value()?, "position limit")?
                }
                "--certain-depth" => {
                    let depth: usize = parse_number(value()?, "certainty depth")?;
                    options.certainty.depth = u8::try_from(depth)
                        .map_err(|_| "certainty depth must fit in a byte".to_owned())?;
                }
                "--certain-plies" => {
                    options.certainty.plies = parse_number(value()?, "certainty plies")?;
                }
                "--certain-min" => {
                    options.certainty.minimum_plies =
                        parse_number(value()?, "certainty minimum plies")?;
                }
                "--certain-max" => {
                    options.certainty.openings = parse_number(value()?, "certified openings")?;
                }
                unknown => return Err(format!("unknown option {unknown:?}")),
            }
        }
        options.threads = options.threads.max(1);
        Ok(options)
    }

    fn directory(&self) -> String {
        self.directory
            .clone()
            .unwrap_or_else(|| format!("book/{}", self.mode.id()))
    }

    fn open(&self) -> Result<Book, String> {
        Book::open(self.directory(), self.mode, &self.params, self.symmetry)
    }
}

fn parse_number<T: std::str::FromStr>(value: &str, label: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {label} {value:?}"))
}

// ---------------------------------------------------------------------------
// Taper
// ---------------------------------------------------------------------------

/// Per-position budget and width, as a function of how deep the line already is.
///
/// Effort belongs where it changes a decision. A first move is worth minutes
/// because everything else hangs off it; a position forty plies into a sideline
/// is worth a second, because by then the question is whether the line survives
/// at all rather than exactly how well. The taper is a file rather than a
/// constant so the shape can be argued with.
#[derive(Clone, Copy, Debug)]
struct Band {
    from_ply: u16,
    nodes: u64,
    variations: usize,
    margin: i32,
}

#[derive(Clone, Debug)]
struct Taper {
    bands: Vec<Band>,
}

impl Default for Taper {
    fn default() -> Self {
        Self {
            bands: vec![
                Band {
                    from_ply: 0,
                    nodes: 300_000_000,
                    variations: 8,
                    margin: 60,
                },
                Band {
                    from_ply: 7,
                    nodes: 60_000_000,
                    variations: 6,
                    margin: 45,
                },
                Band {
                    from_ply: 15,
                    nodes: 20_000_000,
                    variations: 4,
                    margin: 30,
                },
                Band {
                    from_ply: 25,
                    nodes: 6_000_000,
                    variations: 3,
                    margin: 20,
                },
                Band {
                    from_ply: 37,
                    nodes: 2_000_000,
                    variations: 2,
                    margin: 12,
                },
            ],
        }
    }
}

impl Taper {
    /// Read a taper file: one `<from-ply> <nodes> <multipv> <margin>` per line.
    fn parse(body: &str) -> Result<Self, String> {
        let mut bands = Vec::new();
        for (number, line) in body.lines().enumerate() {
            let text = line.split('#').next().unwrap_or_default().trim();
            if text.is_empty() {
                continue;
            }
            let fields: Vec<&str> = text.split_whitespace().collect();
            let [from_ply, nodes, variations, margin] = fields.as_slice() else {
                return Err(format!(
                    "taper line {}: expected `<from-ply> <nodes> <multipv> <margin>`",
                    number + 1
                ));
            };
            bands.push(Band {
                from_ply: parse_number(from_ply, "from-ply")?,
                nodes: parse_number(nodes, "nodes")?,
                variations: parse_number(variations, "multipv")?,
                margin: parse_number(margin, "margin")?,
            });
        }
        if bands.is_empty() {
            return Err("taper file has no bands".to_owned());
        }
        bands.sort_by_key(|band| band.from_ply);
        Ok(Self { bands })
    }

    fn band(&self, ply: u16) -> Band {
        *self
            .bands
            .iter()
            .rev()
            .find(|band| band.from_ply <= ply)
            .unwrap_or(&self.bands[0])
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

fn run(arguments: &[String]) -> Result<(), String> {
    let command = arguments.first().map_or("help", String::as_str);
    let options = Options::parse(arguments.get(1..).unwrap_or_default())?;
    match command {
        "build" => run_build(&options),
        "show" => run_show(&options),
        "study" => run_study(&options),
        "export" => run_export(&options),
        "certify" => run_certify(&options),
        "stats" => run_stats(&options),
        "compact" => run_compact(&options),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        unknown => Err(format!("unknown command {unknown:?}")),
    }
}

// ---------------------------------------------------------------------------
// website export
// ---------------------------------------------------------------------------

fn run_export(options: &Options) -> Result<(), String> {
    let book = options.open()?;
    let json = if options.graph {
        book.web_export_flat(
            options.plies,
            options.featured,
            options.featured_plies,
            options.certainty,
        )?
    } else {
        book.web_export(options.plies, options.width, options.max_positions)?
    };
    if options.output == "-" {
        print!("{json}");
        return Ok(());
    }
    std::fs::write(&options.output, json)
        .map_err(|error| format!("writing {}: {error}", options.output))?;
    println!("exported {} positions to {}", book.len(), options.output);
    Ok(())
}

// ---------------------------------------------------------------------------
// Turning a search into a book record
// ---------------------------------------------------------------------------

/// Convert one analysis of `position` into a record in the canonical frame.
///
/// Everything a node stores is named in its class representative's coordinates,
/// so a line that reaches this position by a different move order -- or as the
/// mirror image of this one -- reads the same record. `frame` is the transform
/// onto that representative, and because the group is its own inverse the same
/// operation translates back.
fn record(position: &Position, symmetry: Symmetry, result: &AnalysisResult) -> (u64, Node) {
    let (canonical, frame) = symmetry.canonical(position);
    let mut edges = Vec::with_capacity(result.lines.len());
    for line in &result.lines {
        let mut child = *position;
        if child.make_move(line.movement).is_err() {
            continue;
        }
        let (child_canonical, child_frame) = symmetry.canonical(&child);
        edges.push(Edge {
            movement: frame.movement(line.movement),
            // A score is relative to whoever is to move, and every transform
            // maps the mover onto the mover, so it crosses frames unchanged.
            score: line.score,
            child: child_canonical.hash(),
            transform: frame.then(child_frame),
        });
    }
    let principal_variation = result
        .lines
        .first()
        .map(|line| {
            line.principal_variation
                .iter()
                .map(|&movement| frame.movement(movement))
                .collect()
        })
        .unwrap_or_default();

    (
        canonical.hash(),
        Node {
            side_to_move: canonical.side_to_move(),
            depth: result.completed_depth,
            selective_depth: result.stats.selective_depth,
            nodes: result.stats.nodes,
            stop: result.stop_reason,
            // What is detected here is the clear case: the search declared the
            // root itself a repetition, so its value came from the history
            // rather than from the board. A position whose refutation merely
            // runs through a repetition deeper down is not caught, which is
            // why the graph walk cuts cycles as well.
            path_dependent: result.stop_reason == SearchStopReason::Repetition,
            score: result.score,
            edges,
            principal_variation,
        },
    )
}

// ---------------------------------------------------------------------------
// build
// ---------------------------------------------------------------------------

fn run_build(options: &Options) -> Result<(), String> {
    let mut book = options.open()?;
    book.open_log()?;
    let batch_size = options.batch.unwrap_or(options.threads * 4).max(1);

    println!("book      {}", options.directory());
    println!("mode      {} ({})", options.mode.name(), options.mode.id());
    println!("symmetry  {}", options.symmetry.id());
    println!("root      {:016x}", book.root());
    println!("known     {} positions", book.len());
    println!("max ply   {}", options.max_ply);
    println!("budget    {} nodes", options.budget);
    println!(
        "threads   {} x {} MiB",
        options.threads, options.hash_megabytes
    );
    println!();

    // Searchers outlive the batches on purpose. Sibling openings transpose
    // heavily, so a table carried from one position to the next is a large free
    // speedup; the price is that a per-position node count depends on what the
    // worker looked at before it, which is why every record stores the depth
    // and node count it actually reached.
    //
    // The tables are deliberately small. Twelve workers each doing random
    // access into half a gigabyte saturate memory bandwidth long before they
    // saturate the cores: measured on a 14-core machine, twelve searches of
    // 150M nodes each ran at 34.5 Mnps aggregate with 512 MiB apiece and
    // 45.7 Mnps with 32 MiB, and reached exactly the same depth either way.
    // Bigger is worse here, which is the opposite of the single-search case.
    let mut searchers: Vec<Searcher> = (0..options.threads)
        .map(|_| {
            let mut searcher = Searcher::new(options.hash_megabytes);
            searcher.set_eval_params(options.params);
            searcher
        })
        .collect();

    let started = Instant::now();
    let mut spent = 0_u64;
    let mut analyzed = 0_usize;
    let mut batch_number = 0_usize;
    // A position is re-analyzed at most once per run. The taper says what a
    // ply is worth now, and a record made under an earlier, cheaper taper is
    // brought up to it -- but a record this run already produced is never
    // queued again, so a merge that keeps an older record's node count cannot
    // turn into a loop.
    let mut refreshed: HashSet<u64> = HashSet::new();

    while spent < options.budget {
        let taper = &options.taper;
        let batch = book.frontier(
            options.max_ply,
            |ply| taper.band(ply).margin,
            |ply, key, node| {
                let band = taper.band(ply);
                !refreshed.contains(&key)
                    && matches!(
                        node.stop,
                        SearchStopReason::NodeLimit
                            | SearchStopReason::DepthLimit
                            | SearchStopReason::TimeLimit
                            | SearchStopReason::Cancelled
                    )
                    && (node.nodes < band.nodes || node.edges.len() < band.variations)
            },
            batch_size,
        );
        if batch.is_empty() {
            println!(
                "frontier empty: every plausible line is built to ply {}",
                options.max_ply
            );
            break;
        }

        let params = options.params;
        // Longest job first, and claimed rather than assigned.
        //
        // A batch comes back ordered by regret, and a position's budget is a
        // function of its ply, so a batch holds a mix of plies and therefore a
        // mix of costs that can span two orders of magnitude -- 400M nodes near
        // the root against 700k forty plies down. Dealing that out round-robin
        // and letting each worker run its own share to the end means the batch
        // takes as long as the unluckiest worker's share, and the rest of the
        // machine waits: measured on a 14-core build of the Infiltration book,
        // twelve workers with one position each ran at 3.5 Mnps aggregate
        // against 42.5 Mnps peak, which is 8% of the machine.
        //
        // So workers claim the next unclaimed position instead, and the claim
        // order puts the most expensive positions first. Handing out the long
        // jobs while there is still work to hide them behind is what keeps the
        // tail short; the last position claimed is the shortest one, so the
        // idle time at the end of a batch is bounded by the *cheapest* job
        // rather than by the most expensive.
        //
        // A single-threaded run is still byte-reproducible: one worker claims
        // 0, 1, 2, ... in order, and the results are sorted back into batch
        // order below regardless.
        let mut order: Vec<usize> = (0..batch.len()).collect();
        order.sort_by_key(|&position| {
            std::cmp::Reverse(options.taper.band(batch[position].ply).nodes)
        });
        let claimed = AtomicUsize::new(0);
        let collected: Mutex<Vec<(usize, AnalysisResult)>> = Mutex::new(Vec::new());
        thread::scope(|scope| {
            for searcher in searchers.iter_mut() {
                let batch = &batch;
                let order = &order;
                let claimed = &claimed;
                let collected = &collected;
                let taper = &options.taper;
                scope.spawn(move || {
                    searcher.set_eval_params(params);
                    let mut mine = Vec::new();
                    loop {
                        let next = claimed.fetch_add(1, Ordering::Relaxed);
                        let Some(&position) = order.get(next) else {
                            break;
                        };
                        let item = &batch[position];
                        let band = taper.band(item.ply);
                        let limits = SearchLimits {
                            max_depth: MAX_SEARCH_DEPTH,
                            max_nodes: Some(band.nodes),
                            move_time: None,
                        };
                        let result = searcher.analyze_with_context(
                            item.position,
                            limits,
                            band.variations,
                            &item.history,
                        );
                        mine.push((position, result));
                    }
                    collected.lock().expect("book batch lock").extend(mine);
                });
            }
        });

        let mut results = collected.into_inner().expect("book batch lock");
        // Sorted, so the log is written in batch order rather than in whatever
        // order the workers happened to finish.
        results.sort_by_key(|(position, _)| *position);
        for (position, result) in &results {
            let item: &Frontier = &batch[*position];
            refreshed.insert(item.key);
            spent += result.stats.nodes;
            analyzed += 1;
            let (key, node) = record(&item.position, options.symmetry, result);
            if key != item.key {
                return Err(format!(
                    "position {:016x} canonicalized to {key:016x} on re-analysis; \
                     the book and this build disagree about representatives",
                    item.key
                ));
            }
            book.insert(key, node, &item.path)?;
        }
        book.flush()?;

        batch_number += 1;
        report_progress(&book, batch_number, analyzed, spent, options, started);
    }

    book.flush()?;
    println!();
    println!("stored    {} positions", book.len());
    println!("spent     {spent} nodes in {:.1?}", started.elapsed());
    Ok(())
}

fn report_progress(
    book: &Book,
    batch: usize,
    analyzed: usize,
    spent: u64,
    options: &Options,
    started: Instant,
) {
    let elapsed = started.elapsed();
    let seconds = elapsed.as_secs_f64().max(1e-9);
    let values = book.values();
    let value = values.get(&book.root()).copied().unwrap_or(0);
    let line = book.principal_line(&Position::starting(options.mode), 8, &values);
    let text: Vec<String> = line.iter().copied().map(Move::notation).collect();
    let deepest = book
        .keys()
        .filter_map(|key| book.path(key).map(<[Move]>::len))
        .max()
        .unwrap_or(0);
    println!(
        "batch {batch:<5} positions {analyzed:<7} stored {:<7} ply {deepest:<3} \
         nodes {spent:<14} nps {:<9.0} {:>5.1}% root {value:+} pv {}",
        book.len(),
        spent as f64 / seconds,
        100.0 * spent as f64 / options.budget as f64,
        text.join(" ")
    );
}

// ---------------------------------------------------------------------------
// certify
// ---------------------------------------------------------------------------

/// Print the openings this book is prepared to stand behind.
///
/// Its own command as well as part of an export, because it is the question to
/// ask *during* a long build: whether another few hours of search bought
/// another certified opening is the only reason to run those hours.
fn run_certify(options: &Options) -> Result<(), String> {
    let book = options.open()?;
    let values = book.values();
    let openings = book.certified_openings(options.certainty, &values);

    println!("book      {}", options.directory());
    println!("mode      {} ({})", options.mode.name(), options.mode.id());
    println!("stored    {} positions", book.len());
    println!(
        "bar       depth {}, {} plies at most, {} at least, {} openings at most",
        options.certainty.depth,
        options.certainty.plies,
        options.certainty.minimum_plies,
        options.certainty.openings,
    );
    println!();

    if openings.is_empty() {
        println!("nothing is certified yet.");
        println!();
        println!("A line is certified while exactly one continuation is strictly best.");
        println!("An empty list means the search has not separated this mode's openings");
        println!("from each other yet -- more budget, not a lower bar, is the answer.");
        return Ok(());
    }

    println!("  rank  value  plies  depth  stops on      line");
    for opening in &openings {
        let text: Vec<String> = opening
            .line
            .iter()
            .copied()
            .map(Move::notation)
            .collect::<Vec<_>>();
        println!(
            "  {:>4}  {:>+5}  {:>5}  {:>5}  {:<12}  {}",
            opening.rank,
            opening.value,
            opening.line.len(),
            opening.shallowest,
            opening.stop.id(),
            text.join(" "),
        );
        if opening.alternatives > 1 {
            println!(
                "                                      ^ {} replies tie here",
                opening.alternatives,
            );
        }
    }
    println!();
    println!(
        "{} certified of {} distinct first moves.",
        openings.len(),
        {
            let (canonical, _) = options
                .symmetry
                .canonical(&Position::starting(options.mode));
            book.node(canonical.hash()).map_or(0, |node| {
                node.edges
                    .iter()
                    .map(|edge| edge.child)
                    .collect::<HashSet<_>>()
                    .len()
            })
        }
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// show
// ---------------------------------------------------------------------------

fn run_show(options: &Options) -> Result<(), String> {
    let book = options.open()?;
    let (position, _, path) = replay_line(options)?;
    let (canonical, _) = options.symmetry.canonical(&position);
    let key = canonical.hash();

    println!("book      {}", options.directory());
    println!("mode      {} ({})", options.mode.name(), options.mode.id());
    println!("stored    {} positions", book.len());
    println!(
        "line      {}",
        if path.is_empty() {
            "(starting position)".to_owned()
        } else {
            path.iter()
                .copied()
                .map(Move::notation)
                .collect::<Vec<_>>()
                .join(" ")
        }
    );
    println!("key       {key:016x}");
    println!("to move   {}", position.side_to_move());

    let Some(node) = book.node(key) else {
        println!();
        println!("this position is not in the book yet");
        return Ok(());
    };

    let values = book.values();
    let value = values.get(&key).copied().unwrap_or(node.score);
    println!(
        "verdict   {value:+} for {} (searched to depth {}, seldepth {}, {} nodes)",
        position.side_to_move(),
        node.depth,
        node.selective_depth,
        node.nodes
    );
    println!();

    // The frame is what renames a stored move back into the coordinates of the
    // position actually on the board.
    let frame = options.symmetry.canonical(&position).1;
    let mut ranked: Vec<(&Edge, i32)> = node
        .edges
        .iter()
        .map(|edge| (edge, book.edge_value(edge, &values)))
        .collect();
    ranked.sort_by_key(|(_, value)| -value);

    println!("  move     value  searched  status");
    for (edge, edge_value) in ranked.iter().take(options.width) {
        let child = book.node(edge.child);
        println!(
            "  {:<8} {:>+6}  {:>8}  {}",
            frame.movement(edge.movement).notation(),
            edge_value,
            child.map_or_else(|| "-".to_owned(), |node| node.depth.to_string()),
            child.map_or("not expanded", |_| "in book"),
        );
    }

    let line = book.principal_line(&position, options.plies, &values);
    println!();
    println!("principal line ({} plies):", line.len());
    for (index, chunk) in line.chunks(2).enumerate() {
        let moves: Vec<String> = chunk.iter().copied().map(Move::notation).collect();
        println!("  {:>3}. {}", index + 1, moves.join("   "));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// study
// ---------------------------------------------------------------------------

fn run_study(options: &Options) -> Result<(), String> {
    let text = options
        .line
        .as_ref()
        .ok_or("study needs --line \"d7-d6 d3-d4 ...\"")?;
    let moves = parse_line(text)?;
    if moves.is_empty() {
        return Err("study needs at least one move".to_owned());
    }

    let mut book = options.open()?;
    book.open_log()?;
    let mut searcher = Searcher::new(options.hash_megabytes);
    searcher.set_eval_params(options.params);
    let limits = SearchLimits {
        max_depth: MAX_SEARCH_DEPTH,
        max_nodes: Some(options.nodes),
        move_time: None,
    };

    println!("book      {}", options.directory());
    println!("line      {}", text.trim());
    println!("budget    {} nodes per position", options.nodes);
    println!();
    println!("  ply  played      value   best        value   loss");

    let mut position = Position::starting(options.mode);
    let mut history: Vec<u64> = Vec::new();
    let mut path: Vec<Move> = Vec::new();
    let mut total_loss = 0_i32;

    for (index, &played) in moves.iter().enumerate() {
        if !position.legal_moves().as_slice().contains(&played) {
            return Err(format!(
                "move {} of the line, {}, is not legal here",
                index + 1,
                played.notation()
            ));
        }

        let result = searcher.analyze_with_context(position, limits, options.variations, &history);
        let (key, node) = record(&position, options.symmetry, &result);
        book.insert(key, node, &path)?;

        let best = result
            .lines
            .first()
            .ok_or_else(|| format!("no analysis at ply {}", index + 1))?;
        // If the played move is already one of the variations, its score came
        // out of the same search as the best move's and the two are directly
        // comparable. If it is not, a second search restricted to that one move
        // reproduces the same depth, window, and table -- which is what
        // `analyze_root_moves` exists for.
        let played_score = match result.lines.iter().find(|line| line.movement == played) {
            Some(line) => line.score,
            None => searcher
                .analyze_root_moves(position, limits, 1, &history, &[played])
                .lines
                .first()
                .map_or(best.score, |line| line.score),
        };
        let loss = best.score - played_score;
        total_loss += loss.max(0);
        println!(
            "  {:>3}  {:<10} {:>+6}   {:<10} {:>+6}   {:>4}",
            index + 1,
            played.notation(),
            played_score,
            best.movement.notation(),
            best.score,
            if loss > 0 {
                loss.to_string()
            } else {
                "-".to_owned()
            },
        );

        history.push(position.hash());
        path.push(played);
        position
            .make_move(played)
            .map_err(|error| format!("replaying the line: {error}"))?;
    }

    // The position the line arrives at is worth a record of its own: it is the
    // frontier node a later `build` will expand from.
    let result = searcher.analyze_with_context(position, limits, options.variations, &history);
    let (key, node) = record(&position, options.symmetry, &result);
    book.insert(key, node, &path)?;
    book.flush()?;

    println!();
    println!("total loss along the line: {total_loss}");
    println!(
        "final position {:+} for {}, best {}",
        result.score,
        position.side_to_move(),
        result
            .lines
            .first()
            .map_or_else(|| "(none)".to_owned(), |line| line.movement.notation())
    );
    println!("{} positions in the book", book.len());
    Ok(())
}

// ---------------------------------------------------------------------------
// stats and compact
// ---------------------------------------------------------------------------

fn run_stats(options: &Options) -> Result<(), String> {
    let book = options.open()?;
    println!("book      {}", options.directory());
    println!("mode      {} ({})", options.mode.name(), options.mode.id());
    println!("symmetry  {}", options.symmetry.id());
    println!("engine    {}", book.meta().engine_version);
    println!("rules     {}", book.meta().rules_version);
    println!("weights   {:016x}", book.meta().params);
    println!("positions {}", book.len());

    if book.is_empty() {
        return Ok(());
    }

    let mut by_ply: HashMap<usize, usize> = HashMap::new();
    let mut searched = 0_u64;
    let mut edges = 0_usize;
    let mut expanded_edges = 0_usize;
    let mut depth_total = 0_u64;
    for key in book.keys() {
        let node = book.node(key).expect("key came from the book");
        searched += node.nodes;
        depth_total += u64::from(node.depth);
        edges += node.edges.len();
        expanded_edges += node
            .edges
            .iter()
            .filter(|edge| book.node(edge.child).is_some())
            .count();
        if let Some(path) = book.path(key) {
            *by_ply.entry(path.len()).or_default() += 1;
        }
    }

    println!("edges     {edges} ({expanded_edges} lead to positions already in the book)");
    println!("searched  {searched} nodes");
    println!(
        "depth     {:.1} average",
        depth_total as f64 / book.len() as f64
    );
    println!(
        "root      {:+}",
        book.values().get(&book.root()).copied().unwrap_or(0)
    );
    println!();
    println!("  ply  positions");
    let mut plies: Vec<usize> = by_ply.keys().copied().collect();
    plies.sort_unstable();
    for ply in plies {
        println!("  {ply:>3}  {}", by_ply[&ply]);
    }
    Ok(())
}

fn run_compact(options: &Options) -> Result<(), String> {
    let mut book = options.open()?;
    let (nodes, paths) = book.compact()?;
    println!(
        "compacted {} to {nodes} node records and {paths} paths",
        options.directory()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Lines
// ---------------------------------------------------------------------------

fn parse_line(text: &str) -> Result<Vec<Move>, String> {
    text.split([' ', ',', '\t', '\n'])
        .filter(|token| !token.is_empty())
        // A PGN move number is not a move, so a line pasted out of an archive
        // works without being cleaned up first.
        .filter(|token| !token.ends_with('.'))
        .map(|token| {
            Move::parse(token).ok_or_else(|| format!("{token:?} is not a move like \"d7-d6\""))
        })
        .collect()
}

fn replay_line(options: &Options) -> Result<(Position, Vec<u64>, Vec<Move>), String> {
    let moves = match options.line.as_deref() {
        Some(text) => parse_line(text)?,
        None => Vec::new(),
    };
    let mut position = Position::starting(options.mode);
    let mut history = Vec::with_capacity(moves.len());
    for (index, &movement) in moves.iter().enumerate() {
        history.push(position.hash());
        position.make_move(movement).map_err(|error| {
            format!(
                "move {} of the line, {}: {error}",
                index + 1,
                movement.notation()
            )
        })?;
    }
    Ok((position, history, moves))
}

#[cfg(test)]
mod tests {
    use super::{Taper, parse_line};
    use rpsfish::Move;

    #[test]
    fn a_taper_reports_the_band_covering_a_ply() {
        let taper = Taper::default();
        assert_eq!(taper.band(1).nodes, 300_000_000);
        assert_eq!(taper.band(6).nodes, 300_000_000);
        assert_eq!(taper.band(7).nodes, 60_000_000);
        assert_eq!(taper.band(50).variations, 2);
        // Effort must fall as a line lengthens, or the deep tail costs more
        // than the opening it hangs off.
        let mut previous = u64::MAX;
        for ply in 0..60 {
            let nodes = taper.band(ply).nodes;
            assert!(nodes <= previous, "taper rises at ply {ply}");
            previous = nodes;
        }
    }

    #[test]
    fn a_taper_file_overrides_the_built_in_shape() {
        let taper = Taper::parse("# comment\n0 100 2 30\n\n10 50 1 10\n").expect("parses");
        assert_eq!(taper.band(0).nodes, 100);
        assert_eq!(taper.band(9).margin, 30);
        assert_eq!(taper.band(10).variations, 1);
        assert!(Taper::parse("0 100 2").is_err());
        assert!(Taper::parse("# nothing but a comment").is_err());
    }

    #[test]
    fn a_line_reads_the_archives_moves_and_ignores_move_numbers() {
        let line = parse_line("1. d7-d6 d3-d4  e7xe6").expect("parses");
        assert_eq!(
            line,
            vec![
                Move::parse("d7-d6").expect("valid"),
                Move::parse("d3-d4").expect("valid"),
                Move::parse("e7-e6").expect("valid"),
            ]
        );
        assert!(parse_line("d7-d6 nonsense").is_err());
    }
}
