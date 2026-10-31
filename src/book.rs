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

//! A persistent opening book: a canonicalized position graph on disk.
//!
//! A single deep search answers one question about one position. A book answers
//! the question a player actually asks -- what should I play, what can the
//! opponent do about it, and how far does that stay true -- by storing a whole
//! graph of analyzed positions and backing values up through it. Every node
//! carries its own search, so a line fifty plies long is fifty full-strength
//! verdicts rather than the tail of one.
//!
//! Three decisions shape everything here:
//!
//! - **Positions are keyed by a canonical representative**, so lines that reach
//!   the same board by different move orders share one analysis. In a game
//!   where nine pieces shuffle between adjacent squares that is most of them.
//! - **The log is append-only.** An overnight run that is interrupted keeps
//!   everything it finished, and re-analyzing a position at a larger budget is
//!   an append rather than a rewrite.
//! - **The graph has cycles**, because pieces can walk back where they came
//!   from. Backing values up therefore cuts repetitions rather than assuming a
//!   tree.
//!
//! This module owns the data and the graph algorithms. Driving searches and
//! spending a compute budget belong to the `book` binary.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::evaluation::EvalParams;
use crate::model::{Color, Mode, Move, Transform};
use crate::position::Position;
use crate::search::{MATE_THRESHOLD, SearchStopReason};

/// Version of the on-disk layout. Bumped when a record stops round-tripping.
pub const FORMAT_VERSION: u32 = 1;

/// Stable interchange format consumed by the website opening-book importer.
///
/// The append-only logs remain the authoritative engine artifact. This format
/// is deliberately a bounded tree in ordinary board coordinates, so a browser
/// never has to understand the book's canonical frames or graph cycles.
pub const WEB_EXPORT_FORMAT: &str = "rps-opening-book/v1";

/// Interchange format for the website's *server-owned* book.
///
/// Where [`WEB_EXPORT_FORMAT`] is a nested tree meant to be downloaded whole,
/// this is the same analysis as a flat graph: every position appears once,
/// keyed by its hash in ordinary board coordinates, and a move names its child
/// by key instead of containing it. The website stores that graph and serves a
/// visitor one position at a time, so a book can be far larger than anything a
/// browser would want to fetch.
///
/// Keying on the *real* position rather than on the line reaching it is what
/// makes transpositions one position instead of many, which is both smaller and
/// truer: two move orders that arrive at the same board really are the same
/// analysis. Repetition is deliberately absent -- it is a property of the path
/// walked, not of the position, so whoever resolves a line decides it.
pub const WEB_GRAPH_FORMAT: &str = "rps-opening-book/v2";

const NODES_FILE: &str = "nodes.log";
const PATHS_FILE: &str = "paths.log";
const META_FILE: &str = "meta.txt";

/// Guard against a pathological graph turning backup into a long walk.
const MAX_BACKUP_DEPTH: usize = 512;

// ---------------------------------------------------------------------------
// Symmetry
// ---------------------------------------------------------------------------

/// Which symmetries a book folds positions by.
///
/// Every setting is sound, because the fold is intersected with the mode's own
/// symmetry group by [`Position::canonical_over`]. What a setting selects is
/// how much merging to *ask* for; what a mode admits is what it gets.
///
/// `Files` is the default because it is the one that pays where it applies.
/// Reversing files merges a position with its left-right mirror, which halves
/// the Infiltration opening immediately: the 23 first moves are 13 classes.
/// Adding the colour swap is correct but nearly inert in a game with a fixed
/// first player, because it flips the side to move and so maps a reachable
/// position onto one with the wrong parity. `Full` is kept so that claim stays
/// measurable rather than assumed.
///
/// **Intransitive merges nothing under any of these.** Its goal is a corner,
/// so reversing files is not a symmetry of its rules and the intersection
/// drops it; the half turn survives but swaps colours, and side-to-move parity
/// keeps those apart just as it does elsewhere. An Intransitive book is
/// therefore about twice the size of an Infiltration book of the same shape,
/// and that is the correct size rather than a regression to tune away.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Symmetry {
    /// Fold only by reversing files.
    Files,
    /// Fold by the whole group.
    Full,
    /// Store every position as it stands.
    None,
}

impl Symmetry {
    #[must_use]
    pub const fn transforms(self) -> &'static [Transform] {
        match self {
            Self::Files => &[Transform::IDENTITY, Transform::FILES],
            Self::Full => &Transform::ALL,
            Self::None => &[Transform::IDENTITY],
        }
    }

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Full => "full",
            Self::None => "none",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "files" => Some(Self::Files),
            "full" => Some(Self::Full),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// The class representative of `position`, and the transform producing it.
    #[must_use]
    pub fn canonical(self, position: &Position) -> (Position, Transform) {
        position.canonical_over(self.transforms())
    }
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// One analyzed move out of a book node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Edge {
    /// The move, named in the canonical parent's frame.
    pub movement: Move,
    /// The parent's search score for this move, relative to the parent's side
    /// to move. Used until the child is expanded and can supply a real value.
    pub score: i32,
    /// Canonical key of the position this move reaches.
    pub child: u64,
    /// Maps the child, in the parent's frame, onto its own canonical form.
    ///
    /// Recomputable, and stored anyway: an export walks thousands of edges and
    /// a mismatch on reload is a cheap check that the graph on disk is the one
    /// this build would produce.
    pub transform: Transform,
}

/// One analyzed position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    pub side_to_move: Color,
    pub depth: u8,
    pub selective_depth: u8,
    /// Nodes the search that produced this record visited.
    pub nodes: u64,
    pub stop: SearchStopReason,
    /// Whether this record depends on the path that reached it.
    ///
    /// A search that saw a repetition scored a position the rules only draw
    /// because of what came before it. Sharing that value with a line that
    /// arrived some other way would be wrong, so the flag travels with it.
    pub path_dependent: bool,
    /// The root score, used when there are no edges to back up through.
    pub score: i32,
    /// Analyzed moves, best first.
    pub edges: Vec<Edge>,
    /// The best line found, in the canonical frame.
    pub principal_variation: Vec<Move>,
}

impl Node {
    /// Whether `other` is worth keeping over `self`.
    ///
    /// Depth first, then nodes searched. A book is re-run at larger budgets, so
    /// two records for one position are normal and the deeper one wins.
    #[must_use]
    pub fn supersedes(&self, other: &Self) -> bool {
        (self.depth, self.nodes) > (other.depth, other.nodes)
    }

    /// Combine two records of one position, keeping the best of each.
    ///
    /// Depth and breadth are separate axes and a run can raise either. A
    /// deeper search has better values; a wider one knows about moves the
    /// deeper one never reported, and without them those moves are not in the
    /// book at all. Keeping only one record would mean a survey pass at high
    /// MultiPV silently losing to an earlier narrow deep pass, or the reverse.
    ///
    /// Values come from the deeper record. Moves only the shallower one lists
    /// are added with their scores clamped to the deeper record's weakest
    /// line: a MultiPV search establishes that its lines are the best ones it
    /// found, so a move it left out is worth no more than the last one it kept.
    /// Clamping can only make a fallback estimate more pessimistic, which is
    /// the safe direction for a value that is a maximum over edges.
    #[must_use]
    pub fn merged_with(&self, other: &Self) -> Self {
        let (deep, shallow) = if self.supersedes(other) {
            (self, other)
        } else {
            (other, self)
        };
        let missing: Vec<&Edge> = shallow
            .edges
            .iter()
            .filter(|edge| !deep.edges.iter().any(|kept| kept.movement == edge.movement))
            .collect();
        if missing.is_empty() {
            return deep.clone();
        }
        // Edges arrive best first, so the last one is the weakest line the
        // deeper search was asked to rank.
        let floor = deep.edges.last().map_or(i32::MAX, |edge| edge.score);
        let mut edges = deep.edges.clone();
        edges.extend(missing.into_iter().map(|edge| Edge {
            score: edge.score.min(floor),
            ..*edge
        }));
        Self {
            edges,
            // The merged record is backed by both searches, and the larger
            // count is the honest summary of that. Keeping the deeper
            // record's smaller count would also make a later run judge this
            // position under-searched forever and re-queue it every time.
            nodes: deep.nodes.max(shallow.nodes),
            ..deep.clone()
        }
    }
}

// ---------------------------------------------------------------------------
// Certainty
// ---------------------------------------------------------------------------

/// Why a certified opening stops where it does.
///
/// Every one of these is a fact worth showing a reader. "The engine is
/// indifferent between three replies here" tells somebody studying the opening
/// more than a line that carried on regardless would have.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CertaintyStop {
    /// Ran into the ply cap with certainty to spare.
    Plies,
    /// Two or more genuinely different continuations tie for best.
    Indifferent,
    /// The position was not searched to [`CertaintyLimits::depth`].
    Shallow,
    /// The book has not analyzed the position the line arrived at.
    Frontier,
    /// The line is already won or lost, which is a finished game rather than
    /// an opening.
    Decisive,
    /// The line returned to a position it had already visited.
    Repetition,
}

impl CertaintyStop {
    /// A stable identifier for the website. Not a sentence: the reader-facing
    /// wording belongs to whatever is drawing the page, and this has to
    /// survive being stored and compared.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Plies => "plies",
            Self::Indifferent => "indifferent",
            Self::Shallow => "shallow",
            Self::Frontier => "frontier",
            Self::Decisive => "decisive",
            Self::Repetition => "repetition",
        }
    }
}

/// The bar an opening has to clear to be certified.
///
/// Only `depth` and the two lengths are numbers, and none of them is in score
/// units -- see [`Book::certified_openings`] for why that matters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CertaintyLimits {
    /// How deep a position must have been searched for its best move to count
    /// as the engine's claim rather than its guess.
    pub depth: u8,
    /// The longest line to certify. Openings are studied a few moves deep, and
    /// a "certified" line thirty plies long invites a trust it cannot carry.
    pub plies: usize,
    /// The shortest line worth listing as an opening at all.
    pub minimum_plies: usize,
    /// How many openings to certify, at most.
    pub openings: usize,
}

impl Default for CertaintyLimits {
    fn default() -> Self {
        Self {
            // Comfortably below what every mode's opening reaches today
            // (19-24), so the bar is on the *shape* of the analysis rather
            // than on how long the last run happened to get.
            depth: 14,
            plies: 8,
            // One certain reply is the least that says anything: it is the
            // difference between "this is a reasonable first move" and "this
            // is how the game continues". Openings shorter than that are
            // still nameable; they are just not certified.
            minimum_plies: 2,
            openings: 8,
        }
    }
}

/// One opening the book is sure of, and how far that holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedOpening {
    /// The line, in real starting-position coordinates.
    pub line: Vec<Move>,
    /// The backed-up value of the first move, to the side that plays it.
    pub value: i32,
    /// Where the first move places among the book's *distinct* first moves,
    /// counting a mirror pair once. 1 is the book's best opening.
    pub rank: usize,
    /// Why the line ends here.
    pub stop: CertaintyStop,
    /// How many continuations tied at the stopping position. Zero unless
    /// `stop` is [`CertaintyStop::Indifferent`].
    pub alternatives: usize,
    /// The shallowest search along the line, over the positions whose best
    /// move the line follows. The floor under every claim the line makes.
    pub shallowest: u8,
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

/// What a book has to agree about before two records can be compared.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Meta {
    pub format: u32,
    pub mode: Mode,
    pub rules_version: u32,
    pub engine_version: String,
    /// Fingerprint of the evaluation weights the records were produced under.
    pub params: u64,
    pub symmetry: Symmetry,
}

impl Meta {
    #[must_use]
    pub fn new(mode: Mode, params: &EvalParams, symmetry: Symmetry) -> Self {
        Self {
            format: FORMAT_VERSION,
            mode,
            rules_version: crate::RULES_VERSION,
            engine_version: env!("CARGO_PKG_VERSION").to_owned(),
            params: fingerprint(params),
            symmetry,
        }
    }

    /// Why `other` may not be added to a book carrying `self`.
    ///
    /// Scores produced under different weights or different rules are not
    /// comparable, and a book that mixed them would present the difference
    /// between two conventions as a difference between two moves. Refusing is
    /// the only honest option, so this is an error rather than a warning.
    fn conflict(&self, other: &Self) -> Option<String> {
        let mismatch = |label: &str, ours: String, theirs: String| {
            (ours != theirs)
                .then(|| format!("{label} mismatch: book has {ours}, this run has {theirs}"))
        };
        mismatch("format", self.format.to_string(), other.format.to_string())
            .or_else(|| {
                mismatch(
                    "mode",
                    self.mode.id().to_owned(),
                    other.mode.id().to_owned(),
                )
            })
            .or_else(|| {
                mismatch(
                    "rules version",
                    self.rules_version.to_string(),
                    other.rules_version.to_string(),
                )
            })
            .or_else(|| {
                mismatch(
                    "evaluation weights",
                    format!("{:016x}", self.params),
                    format!("{:016x}", other.params),
                )
            })
            .or_else(|| {
                mismatch(
                    "symmetry",
                    self.symmetry.id().to_owned(),
                    other.symmetry.id().to_owned(),
                )
            })
    }

    fn write(&self, path: &Path) -> io::Result<()> {
        let mut text = String::new();
        let _ = writeln!(text, "format {}", self.format);
        let _ = writeln!(text, "mode {}", self.mode.id());
        let _ = writeln!(text, "rules {}", self.rules_version);
        let _ = writeln!(text, "engine {}", self.engine_version);
        let _ = writeln!(text, "params {:016x}", self.params);
        let _ = writeln!(text, "symmetry {}", self.symmetry.id());
        fs::write(path, text)
    }

    fn read(path: &Path) -> Result<Self, String> {
        let body =
            fs::read_to_string(path).map_err(|error| format!("reading {path:?}: {error}"))?;
        let mut format = None;
        let mut mode = None;
        let mut rules = None;
        let mut engine = None;
        let mut params = None;
        let mut symmetry = None;
        for line in body.lines() {
            let mut fields = line.split_whitespace();
            let (Some(key), Some(value)) = (fields.next(), fields.next()) else {
                continue;
            };
            match key {
                "format" => format = value.parse::<u32>().ok(),
                "mode" => mode = Mode::parse(value),
                "rules" => rules = value.parse::<u32>().ok(),
                "engine" => engine = Some(value.to_owned()),
                "params" => params = u64::from_str_radix(value, 16).ok(),
                "symmetry" => symmetry = Symmetry::parse(value),
                _ => {}
            }
        }
        Ok(Self {
            format: format.ok_or("meta.txt has no format")?,
            mode: mode.ok_or("meta.txt has no usable mode")?,
            rules_version: rules.ok_or("meta.txt has no rules version")?,
            engine_version: engine.unwrap_or_else(|| "unknown".to_owned()),
            params: params.ok_or("meta.txt has no weight fingerprint")?,
            symmetry: symmetry.ok_or("meta.txt has no symmetry")?,
        })
    }
}

/// A fingerprint of a whole weight vector, by name and value.
///
/// Hashing the names alongside the values means adding a weight changes the
/// fingerprint even if every existing weight keeps its value, which is what
/// stops a book from being extended by a build that evaluates differently.
#[must_use]
pub fn fingerprint(params: &EvalParams) -> u64 {
    let mut result = 0xcbf2_9ce4_8422_2325_u64;
    let mut eat = |bytes: &[u8]| {
        for &byte in bytes {
            result ^= u64::from(byte);
            result = result.wrapping_mul(0x1000_0000_01b3);
        }
    };
    for (name, value) in params.entries() {
        eat(name.as_bytes());
        eat(&value.to_le_bytes());
    }
    result
}

// ---------------------------------------------------------------------------
// The book
// ---------------------------------------------------------------------------

/// A canonicalized position graph, backed by an append-only log.
pub struct Book {
    directory: PathBuf,
    meta: Meta,
    root: u64,
    nodes: HashMap<u64, Node>,
    /// One representative path from the starting position per key.
    ///
    /// Replaying it is how a key becomes a position again, and it doubles as
    /// the audit trail: a key whose path does not canonicalize back to it is a
    /// hash collision or a format bug, and either way the book says so instead
    /// of quietly answering with the wrong position.
    paths: HashMap<u64, Vec<Move>>,
    nodes_log: Option<BufWriter<File>>,
    paths_log: Option<BufWriter<File>>,
}

impl std::fmt::Debug for Book {
    /// A summary rather than the graph.
    ///
    /// A book holds hundreds of thousands of nodes, so the derived form would
    /// be unreadable in exactly the situation a debug print is for.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Book")
            .field("directory", &self.directory)
            .field("mode", &self.meta.mode)
            .field("symmetry", &self.meta.symmetry)
            .field("root", &format_args!("{:016x}", self.root))
            .field("positions", &self.nodes.len())
            .finish()
    }
}

impl Book {
    /// Open the book in `directory`, creating it if it is not there.
    pub fn open(
        directory: impl AsRef<Path>,
        mode: Mode,
        params: &EvalParams,
        symmetry: Symmetry,
    ) -> Result<Self, String> {
        let directory = directory.as_ref().to_path_buf();
        let meta_path = directory.join(META_FILE);
        let wanted = Meta::new(mode, params, symmetry);
        let meta = if meta_path.exists() {
            let existing = Meta::read(&meta_path)?;
            if let Some(conflict) = existing.conflict(&wanted) {
                return Err(format!("{}: {conflict}", directory.display()));
            }
            existing
        } else {
            fs::create_dir_all(&directory)
                .map_err(|error| format!("creating {}: {error}", directory.display()))?;
            wanted
                .write(&meta_path)
                .map_err(|error| format!("writing {}: {error}", meta_path.display()))?;
            wanted
        };

        let start = Position::starting(meta.mode);
        let root = meta.symmetry.canonical(&start).0.hash();
        let mut book = Self {
            directory,
            meta,
            root,
            nodes: HashMap::new(),
            paths: HashMap::new(),
            nodes_log: None,
            paths_log: None,
        };
        book.paths.insert(root, Vec::new());
        book.load()?;
        Ok(book)
    }

    /// Open a book that already exists, taking its mode and fold from its own
    /// metadata.
    ///
    /// [`Book::open`] creates a book when the directory is empty, which is
    /// right for a build and wrong for a reader: an engine told to consult a
    /// book it has not got should say so rather than silently write an empty
    /// one and then answer nothing from it. It also spares the reader from
    /// knowing which fold the build chose, which is a fact about the mode's
    /// rules -- see [`ModeRules::symmetries`] -- rather than about the engine
    /// reading the result.
    ///
    /// The weights are still checked, because that check is the whole point:
    /// scores produced under other weights are not comparable with the ones
    /// this engine is about to search with.
    ///
    /// [`ModeRules::symmetries`]: crate::model::ModeRules::symmetries
    pub fn open_existing(directory: impl AsRef<Path>, params: &EvalParams) -> Result<Self, String> {
        let directory = directory.as_ref();
        let meta = Meta::read(&directory.join(META_FILE))?;
        Self::open(directory, meta.mode, params, meta.symmetry)
    }

    /// An in-memory book with no directory behind it, for tests and exports.
    #[must_use]
    pub fn in_memory(mode: Mode, params: &EvalParams, symmetry: Symmetry) -> Self {
        let meta = Meta::new(mode, params, symmetry);
        let root = symmetry.canonical(&Position::starting(mode)).0.hash();
        let mut paths = HashMap::new();
        paths.insert(root, Vec::new());
        Self {
            directory: PathBuf::new(),
            meta,
            root,
            nodes: HashMap::new(),
            paths,
            nodes_log: None,
            paths_log: None,
        }
    }

    #[must_use]
    pub const fn meta(&self) -> &Meta {
        &self.meta
    }

    #[must_use]
    pub const fn root(&self) -> u64 {
        self.root
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    #[must_use]
    pub fn node(&self, key: u64) -> Option<&Node> {
        self.nodes.get(&key)
    }

    #[must_use]
    pub fn path(&self, key: u64) -> Option<&[Move]> {
        self.paths.get(&key).map(Vec::as_slice)
    }

    pub fn keys(&self) -> impl Iterator<Item = u64> + '_ {
        self.nodes.keys().copied()
    }

    /// Replay a key's representative path into a position and its history.
    ///
    /// The history is what the search needs to score a repetition correctly,
    /// and past the opening it is not optional: nine pieces on a 9x9 board walk
    /// back over their own tracks constantly.
    #[must_use]
    pub fn replay(&self, key: u64) -> Option<(Position, Vec<u64>)> {
        let path = self.paths.get(&key)?;
        let mut position = Position::starting(self.meta.mode);
        let mut history = Vec::with_capacity(path.len() + 1);
        for &movement in path {
            history.push(position.hash());
            position.make_move(movement).ok()?;
        }
        Some((position, history))
    }

    // -----------------------------------------------------------------------
    // Writing
    // -----------------------------------------------------------------------

    /// Record one analyzed position, appending it to the log.
    ///
    /// `path` is a real move sequence from the starting position; it is stored
    /// only the first time a key is seen, because any one representative is
    /// enough to rebuild the position.
    pub fn insert(&mut self, key: u64, node: Node, path: &[Move]) -> Result<bool, String> {
        let fresh_path = !self.paths.contains_key(&key);
        if fresh_path {
            self.paths.insert(key, path.to_vec());
            if let Some(writer) = self.paths_log.as_mut() {
                let mut line = format!("p {key:016x}");
                for movement in path {
                    let _ = write!(line, " {}", movement.notation());
                }
                writeln!(writer, "{line}").map_err(|error| format!("appending a path: {error}"))?;
            }
        }
        let merged = match self.nodes.get(&key) {
            Some(existing) => node.merged_with(existing),
            None => node,
        };
        if self.nodes.get(&key) == Some(&merged) {
            return Ok(false);
        }
        if let Some(writer) = self.nodes_log.as_mut() {
            writeln!(writer, "{}", encode_node(key, &merged))
                .map_err(|error| format!("appending a node: {error}"))?;
        }
        self.nodes.insert(key, merged);
        Ok(true)
    }

    /// Start appending to the log files.
    pub fn open_log(&mut self) -> Result<(), String> {
        let append = |path: PathBuf| -> Result<BufWriter<File>, String> {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map(BufWriter::new)
                .map_err(|error| format!("opening {}: {error}", path.display()))
        };
        self.nodes_log = Some(append(self.directory.join(NODES_FILE))?);
        self.paths_log = Some(append(self.directory.join(PATHS_FILE))?);
        Ok(())
    }

    /// Push everything buffered to disk.
    pub fn flush(&mut self) -> Result<(), String> {
        for writer in [self.nodes_log.as_mut(), self.paths_log.as_mut()]
            .into_iter()
            .flatten()
        {
            writer
                .flush()
                .map_err(|error| format!("flushing the book log: {error}"))?;
        }
        Ok(())
    }

    /// Rewrite the log with one record per position.
    ///
    /// Re-analysis appends rather than overwrites, so a long-lived book
    /// accumulates superseded records. This drops them. It writes beside the
    /// live files and renames, so an interrupted compaction leaves the original
    /// log intact.
    pub fn compact(&mut self) -> Result<(usize, usize), String> {
        self.flush()?;
        self.nodes_log = None;
        self.paths_log = None;

        let write_temp = |name: &str, body: &str| -> Result<(), String> {
            let temporary = self.directory.join(format!("{name}.compacting"));
            fs::write(&temporary, body)
                .map_err(|error| format!("writing {}: {error}", temporary.display()))?;
            fs::rename(&temporary, self.directory.join(name))
                .map_err(|error| format!("replacing {name}: {error}"))
        };

        let mut keys: Vec<u64> = self.nodes.keys().copied().collect();
        keys.sort_unstable();
        let mut nodes_body = String::new();
        for key in &keys {
            let _ = writeln!(nodes_body, "{}", encode_node(*key, &self.nodes[key]));
        }
        let mut path_keys: Vec<u64> = self.paths.keys().copied().collect();
        path_keys.sort_unstable();
        let mut paths_body = String::new();
        for key in &path_keys {
            let _ = write!(paths_body, "p {key:016x}");
            for movement in &self.paths[key] {
                let _ = write!(paths_body, " {}", movement.notation());
            }
            paths_body.push('\n');
        }
        write_temp(NODES_FILE, &nodes_body)?;
        write_temp(PATHS_FILE, &paths_body)?;
        self.open_log()?;
        Ok((keys.len(), path_keys.len()))
    }

    // -----------------------------------------------------------------------
    // Reading
    // -----------------------------------------------------------------------

    /// Replay the log into memory.
    ///
    /// A partial trailing line is dropped rather than rejected: the writer
    /// appends, so the only way to see one is a run that was killed mid-write,
    /// and everything before it is still good. Losing the last few seconds of a
    /// twelve-hour run is the whole point of an append-only log.
    fn load(&mut self) -> Result<(), String> {
        for (file, line_kind) in [(PATHS_FILE, b'p'), (NODES_FILE, b'n')] {
            let path = self.directory.join(file);
            let Ok(handle) = File::open(&path) else {
                continue;
            };
            let mut reader = BufReader::new(handle);
            let mut line = String::new();
            loop {
                line.clear();
                let read = reader
                    .read_line(&mut line)
                    .map_err(|error| format!("reading {}: {error}", path.display()))?;
                if read == 0 {
                    break;
                }
                if !line.ends_with('\n') {
                    break;
                }
                let trimmed = line.trim_end();
                if trimmed.is_empty() || !trimmed.starts_with(line_kind as char) {
                    continue;
                }
                match line_kind {
                    b'p' => {
                        if let Some((key, moves)) = decode_path(trimmed) {
                            self.paths.insert(key, moves);
                        }
                    }
                    _ => {
                        if let Some((key, node)) = decode_node(trimmed) {
                            let merged = match self.nodes.get(&key) {
                                Some(existing) => node.merged_with(existing),
                                None => node,
                            };
                            self.nodes.insert(key, merged);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Graph
    // -----------------------------------------------------------------------

    /// The backed-up value of a position, relative to its side to move.
    ///
    /// Expanded children supply their own negated value; unexpanded ones fall
    /// back to the score the parent's own search gave the move.
    ///
    /// This walks the graph below `key` from scratch. Anything that needs more
    /// than a handful of answers should call [`Book::values`] once instead.
    #[must_use]
    pub fn value(&self, key: u64) -> Option<i32> {
        let mut memo = HashMap::new();
        let mut path = Vec::new();
        self.nodes
            .contains_key(&key)
            .then(|| self.backed_up(key, &mut path, &mut memo).0)
    }

    /// Back every position in the book up, in one pass.
    ///
    /// The memo is shared across the whole sweep, so the work is linear in the
    /// number of edges. Asking [`Book::value`] about each position in turn
    /// gives the same answers for quadratic work, which a book of any size
    /// cannot afford: rebuilding the frontier alone asks about every edge
    /// twice.
    ///
    /// A position whose value depends on a repetition is absent, because it
    /// does not have one value to report.
    #[must_use]
    pub fn values(&self) -> HashMap<u64, i32> {
        let mut memo = HashMap::with_capacity(self.nodes.len());
        let mut path = Vec::new();
        for &key in self.nodes.keys() {
            if memo.contains_key(&key) {
                continue;
            }
            path.clear();
            self.backed_up(key, &mut path, &mut memo);
        }
        memo
    }

    /// Negamax over a graph that has cycles.
    ///
    /// A key already on the current path is a repetition, and the cycle is cut
    /// there whatever the mode says it is worth. In a mode that draws a
    /// repetition it scores zero, which is the rule. In one that does not, the
    /// game simply continues and the book has no value for "the line goes round
    /// again" -- so the node's own searched score stands in, and the cut is
    /// still marked, which is what stops the answer becoming a shared fact.
    ///
    /// Either way the value of a node depends in principle on the path that
    /// reached it, which is why a result is only memoized when no cycle was cut
    /// underneath it. This is the pragmatic answer to graph history
    /// interaction, not a complete one: a position whose only refutation runs
    /// through a repetition can be scored differently from two different lines.
    fn backed_up(
        &self,
        key: u64,
        path: &mut Vec<u64>,
        memo: &mut HashMap<u64, i32>,
    ) -> (i32, bool) {
        if path.contains(&key) {
            let cut = if self.meta.mode.rules().repetition_draws {
                0
            } else {
                self.nodes.get(&key).map_or(0, |node| node.score)
            };
            return (cut, false);
        }
        if let Some(&value) = memo.get(&key) {
            return (value, true);
        }
        let Some(node) = self.nodes.get(&key) else {
            return (0, true);
        };
        if node.edges.is_empty() || path.len() >= MAX_BACKUP_DEPTH {
            return (node.score, true);
        }

        path.push(key);
        let mut best = i32::MIN;
        let mut cycle_free = true;
        for edge in &node.edges {
            let candidate = if self.nodes.contains_key(&edge.child) {
                let (child, child_cycle_free) = self.backed_up(edge.child, path, memo);
                cycle_free &= child_cycle_free;
                closer_to_mate(-child)
            } else {
                edge.score
            };
            best = best.max(candidate);
        }
        path.pop();

        // A node reached through a repetition was scored under a history this
        // caller may not share, so its value never becomes a shared fact.
        cycle_free &= !node.path_dependent;
        if cycle_free {
            memo.insert(key, best);
        }
        (best, cycle_free)
    }

    /// The book's best line from `position`, following backed-up values.
    ///
    /// Takes a position rather than a key on purpose. A node's moves are named
    /// in its class representative's frame, and the caller's board may be the
    /// mirror of that; handing back the stored names would produce a line that
    /// reads plausibly and cannot be played. Starting from the position is what
    /// makes the frame impossible to forget.
    #[must_use]
    pub fn principal_line(
        &self,
        position: &Position,
        max_plies: usize,
        values: &HashMap<u64, i32>,
    ) -> Vec<Move> {
        let (canonical, mut frame) = self.meta.symmetry.canonical(position);
        let mut line = Vec::new();
        let mut seen = HashSet::new();
        let mut current = canonical.hash();
        while line.len() < max_plies && seen.insert(current) {
            let Some(node) = self.nodes.get(&current) else {
                break;
            };
            let Some(edge) = self.best_edge(node, values) else {
                break;
            };
            // Each hop renames the moves into the next node's frame, so the
            // line reads in the coordinates of the position it started from.
            line.push(frame.movement(edge.movement));
            frame = frame.then(edge.transform);
            current = edge.child;
        }
        line
    }

    /// The edge with the best backed-up value.
    #[must_use]
    pub fn best_edge<'a>(&self, node: &'a Node, values: &HashMap<u64, i32>) -> Option<&'a Edge> {
        node.edges
            .iter()
            .max_by_key(|edge| self.edge_value(edge, values))
    }

    /// What a move is worth to the side that plays it.
    ///
    /// An expanded child's own value wins, because it was searched a ply
    /// deeper and therefore knows more than the estimate that reached it. A
    /// child the book has not opened yet keeps that estimate.
    #[must_use]
    pub fn edge_value(&self, edge: &Edge, values: &HashMap<u64, i32>) -> i32 {
        values
            .get(&edge.child)
            .map_or(edge.score, |&value| closer_to_mate(-value))
    }

    /// Export a bounded website-facing move tree as JSON.
    ///
    /// Moves are renamed into the coordinates a player actually sees. The
    /// native book is a cyclic canonical graph, whereas the website presents a
    /// line explorer, so repetitions are marked and cut. `max_positions`
    /// keeps a broad analysis from accidentally producing an enormous upload;
    /// ranked moves still remain visible when their child cannot be expanded.
    pub fn web_export(
        &self,
        max_plies: usize,
        width: usize,
        max_positions: usize,
    ) -> Result<String, String> {
        if width == 0 {
            return Err("website export width must be at least one".to_owned());
        }
        if max_positions == 0 {
            return Err("website export position limit must be at least one".to_owned());
        }
        if !self.nodes.contains_key(&self.root) {
            return Err("the starting position is not in the book yet".to_owned());
        }

        let values = self.values();
        let start = Position::starting(self.meta.mode);
        // The recursive export visits the best edge first, so reserving one
        // position per main-line ply guarantees every advertised core move is
        // actually reachable in the tree even under a very small cap.
        let main_line = self.principal_line(
            &start,
            max_plies.min(max_positions.saturating_sub(1)),
            &values,
        );
        let mut root = String::new();
        let mut path = HashSet::new();
        let mut positions = 0_usize;
        append_web_node(
            self,
            start,
            0,
            max_plies,
            width,
            max_positions,
            &values,
            &mut path,
            &mut positions,
            &mut root,
        )?;

        let mut output = String::new();
        let _ = write!(
            output,
            "{{\"format\":\"{WEB_EXPORT_FORMAT}\",\"modeId\":\"{}\",\"modeName\":\"{}\",\"engineVersion\":\"{}\",\"rulesVersion\":{},\"weights\":\"{:016x}\",\"symmetry\":\"{}\",\"maxPly\":{},\"width\":{},\"positionCount\":{},\"mainLine\":[",
            self.meta.mode.id(),
            self.meta.mode.name(),
            self.meta.engine_version,
            self.meta.rules_version,
            self.meta.params,
            self.meta.symmetry.id(),
            max_plies,
            width,
            positions,
        );
        for (index, movement) in main_line.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            let _ = write!(output, "\"{}\"", movement.notation());
        }
        output.push_str("],\"root\":");
        output.push_str(&root);
        output.push_str("}\n");
        Ok(output)
    }

    /// Export the whole book as a flat, real-coordinate position graph.
    ///
    /// Breadth-first from the starting position, so a position's `maxPly` is
    /// the length of the *shortest* line that reaches it, and every position is
    /// written exactly once no matter how many move orders arrive at it.
    ///
    /// `featured` names the openings worth having before the first tap: the
    /// `featured_moves` best first moves, each followed along its own principal
    /// continuation for `featured_plies`. The website ships those with its
    /// first response and fetches everything else on demand.
    ///
    /// A move carries both `searched` and `child`. `searched` says the engine
    /// analyzed the position behind the move; `child` is its key, and is null
    /// when that position is not in this payload -- past `max_plies`, say. The
    /// two differ exactly where the book knows more than the export carries,
    /// which is what lets the website say "analyzed, but not from here".
    pub fn web_export_flat(
        &self,
        max_plies: usize,
        featured_moves: usize,
        featured_plies: usize,
        certainty: CertaintyLimits,
    ) -> Result<String, String> {
        if !self.nodes.contains_key(&self.root) {
            return Err("the starting position is not in the book yet".to_owned());
        }

        let values = self.values();
        let start = Position::starting(self.meta.mode);

        let mut emitted: HashSet<u64> = HashSet::new();
        let mut queue: VecDeque<(Position, usize)> = VecDeque::new();
        let mut bodies: Vec<String> = Vec::new();
        emitted.insert(start.hash());
        queue.push_back((start, 0));

        while let Some((position, ply)) = queue.pop_front() {
            let (canonical, frame) = self.meta.symmetry.canonical(&position);
            let key = canonical.hash();
            let Some(node) = self.nodes.get(&key) else {
                continue;
            };

            let score = values.get(&key).copied().unwrap_or(node.score);
            let mut body = String::new();
            let _ = write!(
                body,
                "{{\"key\":\"{:016x}\",\"turn\":\"{}\",\"score\":{score},\"depth\":{},\"selectiveDepth\":{},\"nodes\":{},\"moves\":[",
                position.hash(),
                position.side_to_move(),
                node.depth,
                node.selective_depth,
                node.nodes,
            );

            for (rank, (_, edge, edge_value)) in
                self.ranked_edges(node, &values).into_iter().enumerate()
            {
                let movement = frame.movement(edge.movement);
                let mut child = position;
                child
                    .make_move(movement)
                    .map_err(|error| format!("exporting {}: {error}", movement.notation()))?;
                let actual_child = self.meta.symmetry.canonical(&child).0.hash();
                if actual_child != edge.child {
                    return Err(format!(
                        "exporting {}: edge names {:016x}, replay reaches {actual_child:016x}",
                        movement.notation(),
                        edge.child,
                    ));
                }

                let searched = self.nodes.contains_key(&edge.child);
                let child_key = child.hash();
                if searched && ply < max_plies && emitted.insert(child_key) {
                    queue.push_back((child, ply + 1));
                }
                // Breadth-first order means anything reachable in fewer plies
                // than this one was already enqueued, so membership here is the
                // whole answer to "will this key appear in the payload".
                let reference = searched && emitted.contains(&child_key);

                if rank > 0 {
                    body.push(',');
                }
                let _ = write!(
                    body,
                    "{{\"move\":\"{}\",\"score\":{edge_value},\"rank\":{},\"searched\":{searched},\"mainLine\":{},\"child\":",
                    movement.notation(),
                    rank + 1,
                    rank == 0,
                );
                if reference {
                    let _ = write!(body, "\"{child_key:016x}\"");
                } else {
                    body.push_str("null");
                }
                body.push('}');
            }
            body.push_str("]}");
            bodies.push(body);
        }

        let main_line = self.principal_line(&start, max_plies, &values);
        let featured = self.featured_lines(&start, featured_moves, featured_plies, &values);

        let mut output = String::new();
        let _ = write!(
            output,
            "{{\"format\":\"{WEB_GRAPH_FORMAT}\",\"modeId\":\"{}\",\"modeName\":\"{}\",\"engineVersion\":\"{}\",\"rulesVersion\":{},\"weights\":\"{:016x}\",\"symmetry\":\"{}\",\"maxPly\":{max_plies},\"rootKey\":\"{:016x}\",\"positionCount\":{},\"mainLine\":[",
            self.meta.mode.id(),
            self.meta.mode.name(),
            self.meta.engine_version,
            self.meta.rules_version,
            self.meta.params,
            self.meta.symmetry.id(),
            start.hash(),
            bodies.len(),
        );
        write_line(&mut output, &main_line);
        output.push_str("],\"featured\":[");
        for (index, line) in featured.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            output.push('[');
            write_line(&mut output, line);
            output.push(']');
        }

        // The certified openings are what the website leads with, and they are
        // deliberately a separate list from `featured` rather than a
        // replacement for it: `featured` is "the best openings, followed a
        // fixed distance", which is what the bootstrap prefetches positions
        // for, and this is "the openings we are prepared to stand behind",
        // which is a claim and carries its own evidence.
        output.push_str("],\"certified\":[");
        for (index, opening) in self
            .certified_openings(certainty, &values)
            .iter()
            .enumerate()
        {
            if index > 0 {
                output.push(',');
            }
            let _ = write!(
                output,
                "{{\"rank\":{},\"value\":{},\"stop\":\"{}\",\"alternatives\":{},\"depth\":{},\"line\":[",
                opening.rank,
                opening.value,
                opening.stop.id(),
                opening.alternatives,
                opening.shallowest,
            );
            write_line(&mut output, &opening.line);
            output.push_str("]}");
        }
        let _ = write!(
            output,
            "],\"certainty\":{{\"depth\":{},\"plies\":{},\"minimumPlies\":{},\"openings\":{}}}",
            certainty.depth, certainty.plies, certainty.minimum_plies, certainty.openings,
        );
        output.push_str(",\"positions\":[");
        for (index, body) in bodies.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            output.push_str(body);
        }
        output.push_str("]}\n");
        Ok(output)
    }

    /// The openings worth downloading up front, best first.
    ///
    /// One line per candidate first move rather than a subtree: a visitor
    /// following a recommended opening walks a line, and a line is a hundredth
    /// of the bytes of the subtree around it.
    fn featured_lines(
        &self,
        start: &Position,
        moves: usize,
        plies: usize,
        values: &HashMap<u64, i32>,
    ) -> Vec<Vec<Move>> {
        if moves == 0 || plies == 0 {
            return Vec::new();
        }
        let (canonical, frame) = self.meta.symmetry.canonical(start);
        let Some(node) = self.nodes.get(&canonical.hash()) else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        for (_, edge, _) in self.ranked_edges(node, values).into_iter().take(moves) {
            let movement = frame.movement(edge.movement);
            let mut child = *start;
            if child.make_move(movement).is_err() {
                continue;
            }
            let mut line = vec![movement];
            line.extend(self.principal_line(&child, plies.saturating_sub(1), values));
            lines.push(line);
        }
        lines
    }

    /// The openings this book is actually sure of, best first.
    ///
    /// [`Book::featured_lines`] answers "what are the best openings", which a
    /// book can always answer: rank the first moves, follow each principal
    /// line, stop at a ply count. That is the question the website used to ask,
    /// and the reason it shipped lines nobody should have trusted -- a
    /// principal line runs to whatever length it was asked for whether or not
    /// the search behind it ever separated the moves along it.
    ///
    /// This answers the harder question, and answers it by *ranking* rather
    /// than by score magnitude: a line continues while exactly one
    /// continuation is strictly best, and ends at the first position where the
    /// engine is indifferent. That is where the opening stops being one
    /// opening, so it is the honest place to stop drawing it.
    ///
    /// Being a rank test rather than a margin test is deliberate. A threshold
    /// in score units would have to be recalibrated every time the evaluation
    /// is re-tuned -- every score in a mode moves when its weights do, so a
    /// window fitted to one set of weights silently certifies too much or
    /// nothing at all under the next. "Strictly better than the best
    /// alternative" survives any positive rescaling of the evaluation, and
    /// needs no constant to go stale.
    ///
    /// The first move is exempt from that test, because it is the player's
    /// choice rather than the engine's claim: every distinct first move is an
    /// opening somebody may want to play, and its rank already says how the
    /// book rates it. Everything after it is the engine asserting how the line
    /// goes, and that assertion has to be unambiguous to be worth printing.
    #[must_use]
    pub fn certified_openings(
        &self,
        limits: CertaintyLimits,
        values: &HashMap<u64, i32>,
    ) -> Vec<CertifiedOpening> {
        let start = Position::starting(self.meta.mode);
        let (canonical, frame) = self.meta.symmetry.canonical(&start);
        let Some(root) = self.nodes.get(&canonical.hash()) else {
            return Vec::new();
        };

        let mut openings = Vec::new();
        // Mirror twins share a canonical child, so folding on the child key
        // folds them without knowing anything about the mode's geometry. In
        // Infiltration `d9-c8` and `f9-g8` are one opening drawn twice and
        // tie to the point; listing both would spend half of a short list
        // saying the same thing.
        let mut distinct = HashSet::new();
        for (_, edge, value) in self.ranked_edges(root, values) {
            if openings.len() >= limits.openings {
                break;
            }
            if !distinct.insert(edge.child) {
                continue;
            }
            let movement = frame.movement(edge.movement);
            let mut position = start;
            if position.make_move(movement).is_err() {
                continue;
            }
            let rank = distinct.len();
            let mut certified = self.certify_from(position, movement, value, rank, limits, values);
            if certified.line.len() >= limits.minimum_plies {
                certified.value = value;
                openings.push(certified);
            }
        }
        openings
    }

    /// Walk one opening as far as the engine stays sure of it.
    ///
    /// `position` is the board after `first`, and the walk continues from
    /// there, so the whole line reads in the coordinates of the real starting
    /// position rather than of whichever class representative the book stored.
    fn certify_from(
        &self,
        mut position: Position,
        first: Move,
        value: i32,
        rank: usize,
        limits: CertaintyLimits,
        values: &HashMap<u64, i32>,
    ) -> CertifiedOpening {
        let mut certified = CertifiedOpening {
            line: vec![first],
            value,
            rank,
            stop: CertaintyStop::Plies,
            alternatives: 0,
            shallowest: u8::MAX,
        };
        // A cycle cannot be certified past its second visit: the book has no
        // single value for "the line goes round again", and a line that
        // repeats is not an opening anybody is following.
        let mut seen = HashSet::new();
        while certified.line.len() < limits.plies {
            let (canonical, frame) = self.meta.symmetry.canonical(&position);
            let key = canonical.hash();
            if !seen.insert(key) {
                certified.stop = CertaintyStop::Repetition;
                break;
            }
            let Some(node) = self.nodes.get(&key) else {
                certified.stop = CertaintyStop::Frontier;
                break;
            };
            if node.depth < limits.depth {
                certified.stop = CertaintyStop::Shallow;
                break;
            }
            // A forced result is a finished game rather than an opening. The
            // test is against the mate score and not against a window, for
            // the same reason the rest of this is a rank test: re-tuning moves
            // every ordinary score in the book and leaves this one alone.
            let backed_up = values.get(&key).copied().unwrap_or(node.score);
            if backed_up.abs() >= MATE_THRESHOLD {
                certified.stop = CertaintyStop::Decisive;
                break;
            }

            let ranked = self.ranked_edges(node, values);
            let Some(&(_, best, best_value)) = ranked.first() else {
                certified.stop = CertaintyStop::Frontier;
                break;
            };
            // The best *genuine* alternative: the first ranked move that does
            // not reach the same position the winner does.
            let alternative = ranked
                .iter()
                .find(|(_, edge, _)| edge.child != best.child)
                .map(|&(_, _, value)| value);
            if let Some(runner_up) = alternative
                && runner_up >= best_value
            {
                certified.stop = CertaintyStop::Indifferent;
                certified.alternatives = ranked
                    .iter()
                    .filter(|(_, _, value)| *value >= best_value)
                    .count();
                break;
            }

            certified.shallowest = certified.shallowest.min(node.depth);
            let movement = frame.movement(best.movement);
            if position.make_move(movement).is_err() {
                certified.stop = CertaintyStop::Frontier;
                break;
            }
            certified.line.push(movement);
        }
        if certified.shallowest == u8::MAX {
            certified.shallowest = 0;
        }
        certified
    }

    /// A node's moves in the order the website shows them: best first.
    ///
    /// Ties break toward the backed-up best move and then toward the order the
    /// search produced, so a rank is stable across exports of the same book.
    fn ranked_edges<'a>(
        &self,
        node: &'a Node,
        values: &HashMap<u64, i32>,
    ) -> Vec<(usize, &'a Edge, i32)> {
        let best = self.best_edge(node, values).map(|edge| edge.movement);
        let mut ranked: Vec<(usize, &Edge, i32)> = node
            .edges
            .iter()
            .enumerate()
            .map(|(index, edge)| (index, edge, self.edge_value(edge, values)))
            .collect();
        ranked.sort_by(|first, second| {
            second
                .2
                .cmp(&first.2)
                .then_with(|| {
                    let first_main = Some(first.1.movement) == best;
                    let second_main = Some(second.1.movement) == best;
                    second_main.cmp(&first_main)
                })
                .then_with(|| first.0.cmp(&second.0))
        });
        ranked
    }
}

/// Write moves as a comma-separated run of JSON strings, without the brackets.
fn write_line(output: &mut String, line: &[Move]) {
    for (index, movement) in line.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        let _ = write!(output, "\"{}\"", movement.notation());
    }
}

#[allow(clippy::too_many_arguments)]
fn append_web_node(
    book: &Book,
    position: Position,
    ply: usize,
    max_plies: usize,
    width: usize,
    max_positions: usize,
    values: &HashMap<u64, i32>,
    path: &mut HashSet<u64>,
    positions: &mut usize,
    output: &mut String,
) -> Result<(), String> {
    let (canonical, frame) = book.meta.symmetry.canonical(&position);
    let key = canonical.hash();
    let node = book
        .nodes
        .get(&key)
        .ok_or_else(|| format!("position {key:016x} disappeared during export"))?;
    path.insert(key);
    *positions += 1;

    let score = values.get(&key).copied().unwrap_or(node.score);
    let _ = write!(
        output,
        "{{\"turn\":\"{}\",\"score\":{},\"depth\":{},\"selectiveDepth\":{},\"nodes\":{},\"moves\":[",
        position.side_to_move(),
        score,
        node.depth,
        node.selective_depth,
        node.nodes,
    );

    let best = book.best_edge(node, values).map(|edge| edge.movement);
    for (rank, (_, edge, edge_value)) in book
        .ranked_edges(node, values)
        .into_iter()
        .take(width)
        .enumerate()
    {
        if rank > 0 {
            output.push(',');
        }
        let movement = frame.movement(edge.movement);
        let searched = book.nodes.contains_key(&edge.child);
        let repetition = path.contains(&edge.child);
        let is_main = Some(edge.movement) == best;
        let mut child = position;
        child
            .make_move(movement)
            .map_err(|error| format!("exporting {}: {error}", movement.notation()))?;
        let actual_child = book.meta.symmetry.canonical(&child).0.hash();
        if actual_child != edge.child {
            return Err(format!(
                "exporting {}: edge names {:016x}, replay reaches {actual_child:016x}",
                movement.notation(),
                edge.child,
            ));
        }

        let _ = write!(
            output,
            "{{\"move\":\"{}\",\"score\":{},\"rank\":{},\"searched\":{},\"mainLine\":{},\"repetition\":{},\"child\":",
            movement.notation(),
            edge_value,
            rank + 1,
            searched,
            is_main,
            repetition,
        );
        if searched && !repetition && ply < max_plies && *positions < max_positions {
            append_web_node(
                book,
                child,
                ply + 1,
                max_plies,
                width,
                max_positions,
                values,
                path,
                positions,
                output,
            )?;
        } else {
            output.push_str("null");
        }
        output.push('}');
    }
    output.push_str("]}");
    path.remove(&key);
    Ok(())
}

/// Move a proven result one ply further from the side that owns it.
///
/// A win scores just under [`crate::MATE_SCORE`] and loses a point per ply, so
/// that a faster win outranks a slower one. Backing a proven score up one level
/// has to preserve that, or a mate in three and a mate in thirty look alike.
#[must_use]
pub fn closer_to_mate(score: i32) -> i32 {
    if score >= MATE_THRESHOLD {
        score - 1
    } else if score <= -MATE_THRESHOLD {
        score + 1
    } else {
        score
    }
}

// ---------------------------------------------------------------------------
// Frontier
// ---------------------------------------------------------------------------

/// A position the book has reached but not yet analyzed.
#[derive(Clone, Debug)]
pub struct Frontier {
    pub key: u64,
    pub ply: u16,
    /// How much worse than best play the line reaching this position is.
    ///
    /// Summed over the whole line, from the perspective of whichever side chose
    /// at each step. Zero is the main line.
    pub regret: i32,
    /// The real move sequence that reaches it, from the starting position.
    pub path: Vec<Move>,
    /// The position itself, so a caller does not replay the path again.
    pub position: Position,
    pub history: Vec<u64>,
}

/// Ordering key for the search over lines, smallest regret first.
struct Pending {
    regret: i32,
    ply: u16,
    key: u64,
    path: Vec<Move>,
    position: Position,
    history: Vec<u64>,
}

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        (self.regret, self.ply, self.key) == (other.regret, other.ply, other.key)
    }
}

impl Eq for Pending {}

impl Ord for Pending {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed, because BinaryHeap is a max-heap and the cheapest line is
        // the one worth walking first. Ties break on ply then key so a rebuilt
        // frontier is identical rather than merely equivalent.
        (other.regret, other.ply, other.key).cmp(&(self.regret, self.ply, self.key))
    }
}

impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Book {
    /// Every position reachable from the root by a plausible line that still
    /// wants work.
    ///
    /// "Plausible" is the margin: a move whose backed-up value is more than
    /// `margin(ply)` worse than the best available there is not worth building
    /// theory on, exactly as `1.e4 e5` is theory to move twenty-five and
    /// `1.e4 h5` is not. Regret accumulates along the line, so a second-best
    /// move followed by another second-best move drops out sooner than either
    /// alone.
    ///
    /// The walk settles each position at its cheapest known line, so a
    /// transposition is analyzed once, at whichever move order reaches it most
    /// naturally.
    ///
    /// Results are ordered by regret, then by *decreasing* ply: the cheapest
    /// line is deepened as far as it will go before the alternatives are
    /// widened, which is what makes a book that answers "what is best, then
    /// what happens if they deviate" rather than a uniform survey.
    ///
    /// A position already in the book is normally passed through. `refresh`
    /// is what lets a later run raise the standard: return true for a record
    /// that no longer meets the budget or width its ply is now supposed to
    /// get, and it is queued again while its existing edges still guide the
    /// walk. Without it the first pass over a position would fix it forever,
    /// and a book could be extended but never improved.
    #[must_use]
    pub fn frontier(
        &self,
        max_ply: u16,
        margin: impl Fn(u16) -> i32,
        refresh: impl Fn(u16, u64, &Node) -> bool,
        limit: usize,
    ) -> Vec<Frontier> {
        let values = self.values();
        let mut found: Vec<Frontier> = Vec::new();
        let mut settled: HashMap<u64, i32> = HashMap::new();
        let mut heap = BinaryHeap::new();
        let start = Position::starting(self.meta.mode);
        heap.push(Pending {
            regret: 0,
            ply: 0,
            key: self.root,
            path: Vec::new(),
            position: start,
            history: Vec::new(),
        });

        while let Some(pending) = heap.pop() {
            // Pops come in ascending regret, so once `limit` cheaper lines are
            // in hand nothing still queued can displace them. Without this the
            // walk would materialize the whole fringe -- hundreds of thousands
            // of positions, each carrying its path and history -- to hand back
            // a few dozen.
            if found.len() >= limit && pending.regret > found[limit - 1].regret {
                break;
            }
            match settled.entry(pending.key) {
                std::collections::hash_map::Entry::Occupied(_) => continue,
                entry => entry.or_insert(pending.regret),
            };
            let queued = Frontier {
                key: pending.key,
                ply: pending.ply,
                regret: pending.regret,
                path: pending.path,
                position: pending.position,
                history: pending.history,
            };
            let Some(node) = self.nodes.get(&pending.key) else {
                found.push(queued);
                continue;
            };
            if refresh(queued.ply, queued.key, node) {
                found.push(queued.clone());
            }
            if pending.ply >= max_ply {
                continue;
            }

            // The transform from the real position onto the stored one, which
            // is what renames a stored move back into real coordinates.
            let frame = self.meta.symmetry.canonical(&queued.position).1;
            let best = node
                .edges
                .iter()
                .map(|edge| self.edge_value(edge, &values))
                .max()
                .unwrap_or(node.score);
            for edge in &node.edges {
                let regret = queued.regret + (best - self.edge_value(edge, &values));
                let ply = queued.ply + 1;
                if regret > margin(ply) {
                    continue;
                }
                let movement = frame.movement(edge.movement);
                let mut position = queued.position;
                if position.make_move(movement).is_err() {
                    continue;
                }
                let mut history = queued.history.clone();
                history.push(queued.position.hash());
                let mut path = queued.path.clone();
                path.push(movement);
                heap.push(Pending {
                    regret,
                    ply,
                    key: self.meta.symmetry.canonical(&position).0.hash(),
                    path,
                    position,
                    history,
                });
            }
        }

        found.sort_by(|left, right| {
            (left.regret, right.ply, left.key).cmp(&(right.regret, left.ply, right.key))
        });
        found.truncate(limit);
        found
    }
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

const fn stop_id(stop: SearchStopReason) -> &'static str {
    match stop {
        SearchStopReason::DepthLimit => "depth",
        SearchStopReason::NodeLimit => "nodes",
        SearchStopReason::TimeLimit => "time",
        SearchStopReason::Cancelled => "cancelled",
        SearchStopReason::TerminalPosition => "terminal",
        SearchStopReason::Repetition => "repetition",
        SearchStopReason::NoCapture => "nocapture",
        SearchStopReason::NoLegalMove => "nomove",
    }
}

fn stop_from_id(value: &str) -> Option<SearchStopReason> {
    match value {
        "depth" => Some(SearchStopReason::DepthLimit),
        "nodes" => Some(SearchStopReason::NodeLimit),
        "time" => Some(SearchStopReason::TimeLimit),
        "cancelled" => Some(SearchStopReason::Cancelled),
        "terminal" => Some(SearchStopReason::TerminalPosition),
        "repetition" => Some(SearchStopReason::Repetition),
        "nocapture" => Some(SearchStopReason::NoCapture),
        "nomove" => Some(SearchStopReason::NoLegalMove),
        _ => None,
    }
}

fn encode_node(key: u64, node: &Node) -> String {
    let mut line = format!(
        "n {key:016x} {} {} {} {} {} {} {} {}",
        if node.side_to_move == Color::Red {
            'R'
        } else {
            'B'
        },
        node.depth,
        node.selective_depth,
        node.nodes,
        stop_id(node.stop),
        u8::from(node.path_dependent),
        node.score,
        node.edges.len(),
    );
    for edge in &node.edges {
        let _ = write!(
            line,
            " {}:{}:{:016x}:{}",
            edge.movement.notation(),
            edge.score,
            edge.child,
            edge.transform.code()
        );
    }
    if !node.principal_variation.is_empty() {
        let _ = write!(line, " pv=");
        for (index, movement) in node.principal_variation.iter().enumerate() {
            let _ = write!(
                line,
                "{}{}",
                if index == 0 { "" } else { "," },
                movement.notation()
            );
        }
    }
    line
}

fn decode_node(line: &str) -> Option<(u64, Node)> {
    let mut fields = line.split_whitespace();
    if fields.next() != Some("n") {
        return None;
    }
    let key = u64::from_str_radix(fields.next()?, 16).ok()?;
    let side_to_move = match fields.next()? {
        "R" => Color::Red,
        "B" => Color::Blue,
        _ => return None,
    };
    let depth = fields.next()?.parse().ok()?;
    let selective_depth = fields.next()?.parse().ok()?;
    let nodes = fields.next()?.parse().ok()?;
    let stop = stop_from_id(fields.next()?)?;
    let path_dependent = fields.next()? != "0";
    let score = fields.next()?.parse().ok()?;
    let edge_count: usize = fields.next()?.parse().ok()?;

    let mut edges = Vec::with_capacity(edge_count);
    for _ in 0..edge_count {
        let mut parts = fields.next()?.split(':');
        let movement = Move::parse(parts.next()?)?;
        let edge_score = parts.next()?.parse().ok()?;
        let child = u64::from_str_radix(parts.next()?, 16).ok()?;
        let transform = Transform::from_code(parts.next()?.parse().ok()?)?;
        edges.push(Edge {
            movement,
            score: edge_score,
            child,
            transform,
        });
    }

    let principal_variation = match fields.next() {
        Some(text) => text
            .strip_prefix("pv=")?
            .split(',')
            .map(Move::parse)
            .collect::<Option<Vec<Move>>>()?,
        None => Vec::new(),
    };

    Some((
        key,
        Node {
            side_to_move,
            depth,
            selective_depth,
            nodes,
            stop,
            path_dependent,
            score,
            edges,
            principal_variation,
        },
    ))
}

fn decode_path(line: &str) -> Option<(u64, Vec<Move>)> {
    let mut fields = line.split_whitespace();
    if fields.next() != Some("p") {
        return None;
    }
    let key = u64::from_str_radix(fields.next()?, 16).ok()?;
    let moves = fields.map(Move::parse).collect::<Option<Vec<Move>>>()?;
    Some((key, moves))
}
