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

//! Opening book storage and graph invariants.
//!
//! A book is a long-lived artifact: it is appended to over many runs, read back
//! by a later build, and eventually shipped. The failures worth catching are
//! therefore the ones that only show up on reload -- a record that does not
//! round-trip, a log truncated by a killed run, a graph whose cycles make a
//! value depend on how it was reached.

use std::fs;
use std::path::PathBuf;

use rpsfish::book::{Book, CertaintyLimits, CertaintyStop, Edge, Node, Symmetry, closer_to_mate};
use rpsfish::{Color, EvalParams, MATE_SCORE, Mode, Move, Position, SearchStopReason, Transform};

/// A scratch directory that removes itself.
///
/// The crate takes no dependencies, so there is no `tempfile` to reach for.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("rpsfish-book-test-{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("scratch directory");
        Self(path)
    }

    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn movement(text: &str) -> Move {
    Move::parse(text).expect("test move must parse")
}

fn node(score: i32, edges: Vec<Edge>) -> Node {
    Node {
        side_to_move: Color::Red,
        depth: 20,
        selective_depth: 26,
        nodes: 1_234_567,
        stop: SearchStopReason::NodeLimit,
        path_dependent: false,
        score,
        edges,
        principal_variation: vec![movement("d7-d6"), movement("d3-d4")],
    }
}

fn edge(text: &str, score: i32, child: u64, transform: u8) -> Edge {
    Edge {
        movement: movement(text),
        score,
        child,
        transform: Transform::from_code(transform).expect("valid transform"),
    }
}

#[test]
fn a_book_round_trips_through_its_log() {
    let scratch = Scratch::new("round-trip");
    let root;
    {
        let mut book = Book::open(
            scratch.path(),
            Mode::Infiltration,
            &EvalParams::DEFAULT,
            Symmetry::Files,
        )
        .expect("book opens");
        book.open_log().expect("log opens");
        root = book.root();
        book.insert(
            root,
            node(
                14,
                vec![edge("d8-c7", 14, 0xaaaa, 1), edge("f9-g8", -2, 0xbbbb, 0)],
            ),
            &[],
        )
        .expect("insert");
        book.insert(0xaaaa, node(-14, Vec::new()), &[movement("d8-c7")])
            .expect("insert");
        book.flush().expect("flush");
    }

    let reloaded = Book::open(
        scratch.path(),
        Mode::Infiltration,
        &EvalParams::DEFAULT,
        Symmetry::Files,
    )
    .expect("book reopens");
    assert_eq!(reloaded.len(), 2);
    assert_eq!(reloaded.root(), root);
    let stored = reloaded.node(root).expect("root survived the round trip");
    assert_eq!(stored.edges.len(), 2);
    assert_eq!(stored.edges[0].movement, movement("d8-c7"));
    assert_eq!(stored.edges[0].child, 0xaaaa);
    assert_eq!(stored.edges[0].transform, Transform::FILES);
    assert_eq!(stored.principal_variation.len(), 2);
    assert_eq!(stored.depth, 20);
    assert_eq!(stored.nodes, 1_234_567);
    assert_eq!(reloaded.path(0xaaaa), Some([movement("d8-c7")].as_slice()));
}

#[test]
fn a_deeper_record_supersedes_a_shallower_one() {
    let scratch = Scratch::new("supersede");
    let mut book = Book::open(
        scratch.path(),
        Mode::Infiltration,
        &EvalParams::DEFAULT,
        Symmetry::Files,
    )
    .expect("book opens");
    book.open_log().expect("log opens");
    let root = book.root();

    let mut shallow = node(10, vec![edge("d8-c7", 10, 0xaaaa, 0)]);
    shallow.depth = 12;
    let mut deep = node(40, vec![edge("d8-c7", 40, 0xaaaa, 0)]);
    deep.depth = 24;

    assert!(book.insert(root, deep, &[]).expect("insert"));
    // Re-running a book at a smaller budget must not undo the work already in
    // it, or an interrupted deep pass would be silently replaced by the quick
    // survey that follows it.
    assert!(!book.insert(root, shallow, &[]).expect("insert"));
    assert_eq!(book.node(root).expect("root").depth, 24);
    assert_eq!(book.node(root).expect("root").score, 40);
    book.flush().expect("flush");

    let reloaded = Book::open(
        scratch.path(),
        Mode::Infiltration,
        &EvalParams::DEFAULT,
        Symmetry::Files,
    )
    .expect("book reopens");
    assert_eq!(reloaded.node(root).expect("root").depth, 24);
}

#[test]
fn a_log_truncated_mid_write_keeps_everything_before_it() {
    let scratch = Scratch::new("truncated");
    let root;
    {
        let mut book = Book::open(
            scratch.path(),
            Mode::Infiltration,
            &EvalParams::DEFAULT,
            Symmetry::Files,
        )
        .expect("book opens");
        book.open_log().expect("log opens");
        root = book.root();
        book.insert(root, node(14, vec![edge("d8-c7", 14, 0xaaaa, 0)]), &[])
            .expect("insert");
        book.insert(0xaaaa, node(-14, Vec::new()), &[movement("d8-c7")])
            .expect("insert");
        book.flush().expect("flush");
    }

    // A run killed mid-append leaves a line with no newline on the end. It is
    // the record in flight, and nothing after it exists.
    let log = scratch.path().join("nodes.log");
    let mut body = fs::read_to_string(&log).expect("log readable");
    body.push_str("n deadbeefdeadbeef R 20 26 5 nodes 0 7");
    fs::write(&log, body).expect("log writable");

    let reloaded = Book::open(
        scratch.path(),
        Mode::Infiltration,
        &EvalParams::DEFAULT,
        Symmetry::Files,
    )
    .expect("book reopens over a torn log");
    assert_eq!(reloaded.len(), 2, "a torn last line must not cost the run");
    assert!(reloaded.node(0xdead_beef_dead_beef).is_none());
}

#[test]
fn a_book_refuses_records_made_under_different_weights() {
    let scratch = Scratch::new("weights");
    {
        Book::open(
            scratch.path(),
            Mode::Infiltration,
            &EvalParams::DEFAULT,
            Symmetry::Files,
        )
        .expect("book opens");
    }
    let mut other = EvalParams::DEFAULT;
    other
        .apply_assignments(["infiltration_advancement=99"])
        .expect("weight assigns");

    let error = Book::open(scratch.path(), Mode::Infiltration, &other, Symmetry::Files)
        .expect_err("a book built under other weights is not comparable");
    assert!(error.contains("evaluation weights"), "{error}");

    let error = Book::open(
        scratch.path(),
        Mode::Infiltration,
        &EvalParams::DEFAULT,
        Symmetry::Full,
    )
    .expect_err("a book folded differently keys positions differently");
    assert!(error.contains("symmetry"), "{error}");

    let error = Book::open(
        scratch.path(),
        Mode::TotalWar,
        &EvalParams::DEFAULT,
        Symmetry::Files,
    )
    .expect_err("a book is about one mode");
    assert!(error.contains("mode"), "{error}");
}

/// Backing up a graph with a cycle in it has to terminate, and what it cuts the
/// cycle with has to be what repeating is worth under the mode's own rules.
///
/// Two separate claims, and only the second one moves. Termination is a
/// property of the walk and is unconditional. The cut is zero where a mode
/// draws on a third occurrence and the node's own score where it does not —
/// which is every mode at the moment, so `repeats_is_a_draw` reads false below
/// and the arms are written out rather than folded away.
#[test]
fn backing_up_a_cycle_terminates_and_cuts_it_by_the_modes_rule() {
    let repeat_draws = Mode::Infiltration.rules().repetition_draws;
    let mut book = Book::in_memory(Mode::Infiltration, &EvalParams::DEFAULT, Symmetry::Files);
    let root = book.root();
    let other = 0x1111_u64;

    // Two positions that walk back into each other, which is what a pair of
    // pieces shuffling looks like to the graph. Each side thinks it is winning
    // by 90 if it just repeats.
    book.insert(root, node(90, vec![edge("d8-c7", 90, other, 0)]), &[])
        .expect("insert");
    book.insert(
        other,
        node(90, vec![edge("c7-d8", 90, root, 0)]),
        &[movement("d8-c7")],
    )
    .expect("insert");
    assert_eq!(
        book.value(root),
        Some(if repeat_draws { 0 } else { 90 }),
        "a line whose only continuation repeats is worth what repeating is worth"
    );

    // Give the root an alternative worth 45 and it is preferred only when
    // repeating is worth less than that, which is exactly when a repeat draws.
    // The replacement has to be a deeper record, because a book never lets a
    // later shallow pass overwrite work already in it.
    let mut wider = node(
        90,
        vec![edge("d8-c7", 90, other, 0), edge("f9-g8", 45, 0x2222, 0)],
    );
    wider.depth = 24;
    assert!(book.insert(root, wider, &[]).expect("insert"));
    assert_eq!(book.value(root), Some(if repeat_draws { 45 } else { 90 }));
}

#[test]
fn values_come_from_expanded_children_and_fall_back_to_the_parents_score() {
    let mut book = Book::in_memory(Mode::Infiltration, &EvalParams::DEFAULT, Symmetry::Files);
    let root = book.root();
    let child = 0x3333_u64;

    // The parent's own search liked this move; the child, searched deeper,
    // says the opposite. Backing up is what lets the deeper verdict win.
    book.insert(root, node(0, vec![edge("d8-c7", 70, child, 0)]), &[])
        .expect("insert");
    assert_eq!(
        book.value(root),
        Some(70),
        "an unexpanded child keeps the parent's estimate"
    );

    book.insert(child, node(120, Vec::new()), &[movement("d8-c7")])
        .expect("insert");
    assert_eq!(
        book.value(root),
        Some(-120),
        "an expanded child overrides the estimate that reached it"
    );
    assert_eq!(
        book.principal_line(&Position::starting(Mode::Infiltration), 4, &book.values()),
        vec![movement("d8-c7")]
    );
}

#[test]
fn a_proven_result_stays_ordered_by_how_long_it_takes() {
    // A mate score carries a distance, so backing it up a level has to move it
    // one ply further out or a forced win in three reads like one in thirty.
    assert_eq!(closer_to_mate(MATE_SCORE - 3), MATE_SCORE - 4);
    assert_eq!(closer_to_mate(-MATE_SCORE + 3), -MATE_SCORE + 4);
    // Ordinary scores are estimates with no distance in them and must not move.
    assert_eq!(closer_to_mate(42), 42);
    assert_eq!(closer_to_mate(-42), -42);
}

#[test]
fn the_frontier_ranks_the_best_line_first_and_deepest() {
    let mut book = Book::in_memory(Mode::Infiltration, &EvalParams::DEFAULT, Symmetry::Files);
    let root = book.root();
    let start = Position::starting(Mode::Infiltration);

    // Build one real ply so the frontier's replay has legal moves to make.
    let mut edges = Vec::new();
    for (index, &candidate) in start.legal_moves().as_slice().iter().take(4).enumerate() {
        let mut child = start;
        child.make_move(candidate).expect("legal");
        let (canonical, transform) = Symmetry::Files.canonical(&child);
        edges.push(Edge {
            movement: candidate,
            // A clear best move, then a near-tie, then two that are refuted.
            score: [50, 44, -400, -500][index],
            child: canonical.hash(),
            transform,
        });
    }
    let best_child = edges[0].child;
    book.insert(root, node(50, edges.clone()), &[])
        .expect("insert");

    let frontier = book.frontier(6, |_| 30, |_, _, _| false, 64);
    assert!(!frontier.is_empty());
    assert_eq!(
        frontier[0].key, best_child,
        "the cheapest line is the one worth deepening first"
    );
    assert!(
        frontier
            .windows(2)
            .all(|pair| pair[0].regret <= pair[1].regret),
        "the frontier must be ordered by regret"
    );
    // The two refuted moves are further from best play than the margin allows,
    // so no effort goes into them. That is the whole reason a fifty-ply book
    // is affordable.
    assert_eq!(frontier.len(), 2, "moves beyond the margin are not queued");
    for item in &frontier {
        assert!(item.regret <= 30);
        assert_eq!(item.ply, 1);
        assert_eq!(item.path.len(), 1);
        assert_eq!(item.history.len(), 1);
        // Every queued position must be the one its path actually reaches.
        let mut replayed = Position::starting(Mode::Infiltration);
        for &played in &item.path {
            replayed.make_move(played).expect("the path must be legal");
        }
        assert_eq!(replayed, item.position);
        assert_eq!(Symmetry::Files.canonical(&replayed).0.hash(), item.key);
    }
}

#[test]
fn folding_by_files_merges_mirrored_first_moves() {
    // The starting position is its own mirror, so a first move and its
    // reflection reach the same position. Storing that once is what halves an
    // Infiltration book.
    let start = Position::starting(Mode::Infiltration);
    let mut folded = std::collections::HashSet::new();
    let mut unfolded = std::collections::HashSet::new();
    for &candidate in start.legal_moves().as_slice() {
        let mut child = start;
        child.make_move(candidate).expect("legal");
        folded.insert(Symmetry::Files.canonical(&child).0.hash());
        unfolded.insert(Symmetry::None.canonical(&child).0.hash());
    }
    assert_eq!(unfolded.len(), 23, "Infiltration opens with 23 moves");
    // Eleven mirrored pairs and one move that is its own mirror.
    assert_eq!(
        folded.len(),
        12,
        "which are 12 positions once mirrors merge"
    );
}

#[test]
fn a_wider_record_contributes_its_extra_moves_to_a_deeper_one() {
    let mut book = Book::in_memory(Mode::Infiltration, &EvalParams::DEFAULT, Symmetry::Files);
    let root = book.root();

    // A narrow deep pass, then a wide shallow survey. Neither alone is the
    // book you want: the first knows two moves well, the second knows that a
    // third move exists at all.
    let mut deep = node(
        30,
        vec![edge("d8-c7", 30, 0xaaaa, 0), edge("f9-g8", 12, 0xbbbb, 0)],
    );
    deep.depth = 24;
    let mut wide = node(
        28,
        vec![
            edge("d8-c7", 28, 0xaaaa, 0),
            edge("f9-g8", 10, 0xbbbb, 0),
            edge("e7-e6", 25, 0xcccc, 0),
        ],
    );
    wide.depth = 14;

    book.insert(root, deep, &[]).expect("insert");
    book.insert(root, wide, &[]).expect("insert");

    let stored = book.node(root).expect("root");
    assert_eq!(stored.depth, 24, "values still come from the deeper search");
    assert_eq!(
        stored.nodes, 1_234_567,
        "the merged record is backed by both searches, so it reports the larger effort"
    );
    assert_eq!(
        stored.edges.len(),
        3,
        "the wider search still contributes its move"
    );
    let extra = stored
        .edges
        .iter()
        .find(|edge| edge.movement == movement("e7-e6"))
        .expect("the extra move survived");
    // The deep search ranked two moves and left this one out, so it is worth
    // no more than the weaker of the two it kept. Trusting the shallow +25
    // would have made this position look better than the deep search said.
    assert_eq!(extra.score, 12);
    assert_eq!(book.value(root), Some(30));
}

#[test]
fn refreshing_requeues_a_record_that_no_longer_meets_its_ply_budget() {
    let mut book = Book::in_memory(Mode::Infiltration, &EvalParams::DEFAULT, Symmetry::Files);
    let root = book.root();
    book.insert(root, node(10, Vec::new()), &[])
        .expect("insert");

    // A book whose every position is already analyzed has nothing to do.
    assert!(book.frontier(4, |_| 100, |_, _, _| false, 32).is_empty());

    // Unless a later run decides the standard has gone up, which is how a
    // survey pass widens a root an earlier narrow pass had already fixed.
    let queued = book.frontier(4, |_| 100, |_, _, _| true, 32);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].key, root);
    assert_eq!(queued[0].ply, 0);
    assert!(queued[0].path.is_empty());
}

#[test]
fn the_frontier_stops_once_it_has_the_positions_it_was_asked_for() {
    let mut book = Book::in_memory(Mode::Infiltration, &EvalParams::DEFAULT, Symmetry::Files);
    let root = book.root();
    let start = Position::starting(Mode::Infiltration);

    let mut edges = Vec::new();
    for &candidate in start.legal_moves().as_slice() {
        let mut child = start;
        child.make_move(candidate).expect("legal");
        let (canonical, transform) = Symmetry::Files.canonical(&child);
        edges.push(Edge {
            movement: candidate,
            score: 0,
            child: canonical.hash(),
            transform,
        });
    }
    book.insert(root, node(0, edges), &[]).expect("insert");

    // Every move is equally good here, so the margin excludes nothing and the
    // whole fringe qualifies. The limit is what keeps the walk bounded.
    let unbounded = book.frontier(6, |_| 100, |_, _, _| false, 1_000);
    assert_eq!(unbounded.len(), 12, "23 first moves are 12 positions");
    for limit in 1..=12 {
        let bounded = book.frontier(6, |_| 100, |_, _, _| false, limit);
        assert_eq!(bounded.len(), limit);
    }
}

#[test]
fn a_principal_line_reads_in_the_coordinates_of_the_board_it_starts_from() {
    // A position stored under its mirror is the case that matters: the book's
    // own move names are then the reflections of the ones a player can make,
    // and handing those back produces a line that reads plausibly and cannot
    // be played.
    let mut book = Book::in_memory(Mode::Infiltration, &EvalParams::DEFAULT, Symmetry::Files);
    let start = Position::starting(Mode::Infiltration);

    // Blue opens, so a playable first move comes off Blue's own paper rank.
    // Whichever way the fold falls, one of a move and its mirror is stored
    // under the other, so the pair is searched rather than guessed at.
    let (opening, after, canonical, frame) = ["d2-c3", "f2-g3", "d2-e3", "f2-e3"]
        .iter()
        .find_map(|token| {
            let opening = movement(token);
            let mut after = start;
            after.make_move(opening).ok()?;
            let (canonical, frame) = Symmetry::Files.canonical(&after);
            (frame != rpsfish::Transform::IDENTITY).then_some((opening, after, canonical, frame))
        })
        .expect("one of an opening and its mirror is stored under the other");

    // The reply, named the way a player would name it and the way the book
    // stores it.
    let reply = *after
        .legal_moves()
        .as_slice()
        .first()
        .expect("a reply exists");
    let mut child = after;
    child.make_move(reply).expect("legal");
    let (child_canonical, child_frame) = Symmetry::Files.canonical(&child);

    book.insert(
        canonical.hash(),
        node(
            30,
            vec![Edge {
                movement: frame.movement(reply),
                score: 30,
                child: child_canonical.hash(),
                transform: frame.then(child_frame),
            }],
        ),
        &[opening],
    )
    .expect("insert");

    let values = book.values();
    assert_eq!(
        book.principal_line(&after, 4, &values),
        vec![reply],
        "the line must be playable on the board it was asked about"
    );
    // And asked about the mirror board, it comes back mirrored to match.
    let mirrored = after.transformed(rpsfish::Transform::FILES);
    assert_eq!(
        book.principal_line(&mirrored, 4, &values),
        vec![rpsfish::Transform::FILES.movement(reply)]
    );
}

#[test]
fn a_web_export_is_a_bounded_real_coordinate_tree() {
    let symmetry = Symmetry::Files;
    let mut book = Book::in_memory(Mode::Infiltration, &EvalParams::DEFAULT, symmetry);
    let start = Position::starting(Mode::Infiltration);
    let opening = start.legal_moves().as_slice()[0];
    let (root_position, root_frame) = symmetry.canonical(&start);

    let mut after = start;
    after.make_move(opening).expect("legal opening");
    let (child_position, child_frame) = symmetry.canonical(&after);
    book.insert(
        root_position.hash(),
        node(
            17,
            vec![Edge {
                movement: root_frame.movement(opening),
                score: 17,
                child: child_position.hash(),
                transform: root_frame.then(child_frame),
            }],
        ),
        &[],
    )
    .expect("root insert");
    let mut reply = node(-17, Vec::new());
    reply.side_to_move = Color::Blue;
    book.insert(child_position.hash(), reply, &[opening])
        .expect("child insert");

    let exported = book.web_export(4, 3, 10).expect("website export");
    assert!(exported.contains(r#""format":"rps-opening-book/v1""#));
    assert!(exported.contains(&format!(r#""mainLine":["{}"]"#, opening.notation())));
    assert!(exported.contains(&format!(r#""move":"{}""#, opening.notation())));
    assert!(exported.contains(r#""positionCount":2"#));
    assert!(exported.contains(r#""turn":"Blue""#));

    let capped = book.web_export(4, 3, 1).expect("capped website export");
    assert!(capped.contains(r#""positionCount":1"#));
    assert!(capped.contains(r#""mainLine":[]"#));
    assert!(capped.contains(r#""searched":true"#));
    assert!(capped.contains(r#""child":null"#));
}

/// The graph export is the same analysis, keyed by real position instead of by
/// line -- so it has to agree with the tree export on what the moves are, and
/// every key it names has to be a key it wrote.
#[test]
fn a_graph_export_is_a_closed_real_coordinate_graph() {
    let symmetry = Symmetry::Files;
    let mut book = Book::in_memory(Mode::Infiltration, &EvalParams::DEFAULT, symmetry);
    let start = Position::starting(Mode::Infiltration);
    let opening = start.legal_moves().as_slice()[0];
    let (root_position, root_frame) = symmetry.canonical(&start);

    let mut after = start;
    after.make_move(opening).expect("legal opening");
    let (child_position, child_frame) = symmetry.canonical(&after);
    book.insert(
        root_position.hash(),
        node(
            17,
            vec![Edge {
                movement: root_frame.movement(opening),
                score: 17,
                child: child_position.hash(),
                transform: root_frame.then(child_frame),
            }],
        ),
        &[],
    )
    .expect("root insert");
    let mut reply = node(-17, Vec::new());
    reply.side_to_move = Color::Blue;
    book.insert(child_position.hash(), reply, &[opening])
        .expect("child insert");

    let exported = book
        .web_export_flat(8, 2, 4, CertaintyLimits::default())
        .expect("graph export");
    assert!(exported.contains(r#""format":"rps-opening-book/v2""#));
    // Keyed on the real position, not on the canonical representative: the
    // start of Infiltration is its own mirror image only by accident, and the
    // website replays real moves.
    assert!(exported.contains(&format!(r#""rootKey":"{:016x}""#, start.hash())));
    assert!(exported.contains(&format!(r#""key":"{:016x}""#, after.hash())));
    assert!(exported.contains(&format!(r#""child":"{:016x}""#, after.hash())));
    assert!(exported.contains(&format!(r#""mainLine":["{}"]"#, opening.notation())));
    assert!(exported.contains(&format!(r#""featured":[["{}"]]"#, opening.notation())));
    assert!(exported.contains(r#""positionCount":2"#));
    assert!(exported.contains(r#""turn":"Blue""#));
    // Nothing nested, and no repetition flag: a graph names its children.
    assert!(!exported.contains(r#""repetition""#));

    // A ply cap stops the walk without inventing a key the payload never wrote:
    // the move stays `searched`, and its `child` goes null.
    let shallow = book
        .web_export_flat(0, 1, 1, CertaintyLimits::default())
        .expect("shallow graph export");
    assert!(shallow.contains(r#""positionCount":1"#));
    assert!(shallow.contains(r#""searched":true"#));
    assert!(shallow.contains(r#""child":null"#));
}

// ---------------------------------------------------------------------------
// Certified openings
// ---------------------------------------------------------------------------

/// Seat one analyzed position: the board reached by `line`, ranking `moves` in
/// the order given.
///
/// This mirrors `record` in the book binary, because the frame arithmetic is
/// precisely what a hand-built book gets wrong. An edge's move is named in the
/// *canonical parent's* frame rather than in the coordinates the test wrote,
/// and a book whose edges skip that step reads plausibly and describes moves
/// nobody can play.
fn seat(
    book: &mut Book,
    symmetry: Symmetry,
    line: &[&str],
    depth: u8,
    score: i32,
    moves: &[(&str, i32)],
) {
    let mut position = Position::starting(Mode::Infiltration);
    let path: Vec<Move> = line.iter().copied().map(movement).collect();
    for step in &path {
        position.make_move(*step).expect("legal test move");
    }
    let (canonical, frame) = symmetry.canonical(&position);
    let edges = moves
        .iter()
        .map(|(text, edge_score)| {
            let step = movement(text);
            let mut child = position;
            child.make_move(step).expect("legal test reply");
            let (child_canonical, child_frame) = symmetry.canonical(&child);
            Edge {
                movement: frame.movement(step),
                score: *edge_score,
                child: child_canonical.hash(),
                transform: frame.then(child_frame),
            }
        })
        .collect();
    book.insert(
        canonical.hash(),
        Node {
            side_to_move: position.side_to_move(),
            depth,
            selective_depth: depth,
            nodes: 8_000_000,
            stop: SearchStopReason::NodeLimit,
            path_dependent: false,
            score,
            edges,
            principal_variation: Vec::new(),
        },
        &path,
    )
    .expect("seat a position");
}

fn scratch_book(name: &str) -> (Scratch, Book) {
    let scratch = Scratch::new(name);
    let book = Book::open(
        scratch.path(),
        Mode::Infiltration,
        &EvalParams::DEFAULT,
        Symmetry::Files,
    )
    .expect("book opens");
    (scratch, book)
}

fn notated(line: &[Move]) -> Vec<String> {
    line.iter().copied().map(Move::notation).collect()
}

/// Whether a line is legal from the real starting position.
///
/// The property every exported line has to have, and the one a frame mistake
/// breaks: a book stores moves in a class representative's coordinates, and
/// the mirror of a legal line reads exactly as plausibly as the line itself.
fn replays(line: &[Move]) -> bool {
    let mut position = Position::starting(Mode::Infiltration);
    line.iter().all(|step| position.make_move(*step).is_ok())
}

#[test]
fn a_certified_opening_runs_while_one_reply_is_strictly_best() {
    let (_scratch, mut book) = scratch_book("certify-strict");
    // Two first moves that are mirror images of each other, so they reach one
    // canonical child and are one opening; one that is not. Blue moves first
    // (see `FIRST_TO_MOVE`), so an opening move comes off ranks 1-3.
    seat(
        &mut book,
        Symmetry::Files,
        &[],
        20,
        40,
        &[("d2-c3", 40), ("f2-g3", 40), ("d3-d4", 5)],
    );
    // Blue's reply is separated, so the line continues.
    seat(
        &mut book,
        Symmetry::Files,
        &["d2-c3"],
        20,
        -40,
        &[("d7-c6", -20), ("f7-g6", -35)],
    );
    // ...and then runs off the edge of the book.
    let values = book.values();
    let certified = book.certified_openings(CertaintyLimits::default(), &values);

    // The mirror pair collapsed: `f2-g3` is the same opening as `d2-c3`.
    assert_eq!(certified.len(), 1, "one opening cleared the two-ply floor");
    // Which half of a mirror pair gets reported is arbitrary -- they are one
    // opening drawn twice. What is not arbitrary is that the line comes back
    // in coordinates somebody can actually play, which is the whole reason
    // the walk carries a frame.
    assert!(
        replays(&certified[0].line),
        "the certified line must be legal from the starting position, got {:?}",
        notated(&certified[0].line),
    );
    assert!(
        matches!(
            notated(&certified[0].line).as_slice(),
            [first, second]
                if (first == "d2-c3" && second == "d7-c6")
                    || (first == "f2-g3" && second == "f7-g6")
        ),
        "expected one half of the mirror pair, got {:?}",
        notated(&certified[0].line),
    );
    assert_eq!(certified[0].stop, CertaintyStop::Frontier);
    assert_eq!(certified[0].rank, 1);
    assert_eq!(certified[0].shallowest, 20);
}

#[test]
fn certification_stops_where_the_engine_is_indifferent() {
    let (_scratch, mut book) = scratch_book("certify-tied");
    seat(&mut book, Symmetry::Files, &[], 20, 40, &[("d2-c3", 40)]);
    // Three replies, two of them tied for best. A line that carried on here
    // would be asserting a continuation the search never chose.
    seat(
        &mut book,
        Symmetry::Files,
        &["d2-c3"],
        20,
        -40,
        &[("d7-c6", -20), ("f7-g6", -20), ("e7-e6", -50)],
    );
    let values = book.values();
    let certified = book.certified_openings(
        CertaintyLimits {
            minimum_plies: 1,
            ..CertaintyLimits::default()
        },
        &values,
    );

    assert_eq!(certified.len(), 1);
    assert_eq!(notated(&certified[0].line), ["d2-c3"]);
    assert_eq!(certified[0].stop, CertaintyStop::Indifferent);
    assert_eq!(certified[0].alternatives, 2, "two replies tied");
}

#[test]
fn a_shallow_position_ends_a_certified_line() {
    let (_scratch, mut book) = scratch_book("certify-shallow");
    seat(&mut book, Symmetry::Files, &[], 20, 40, &[("d2-c3", 40)]);
    seat(
        &mut book,
        Symmetry::Files,
        &["d2-c3"],
        20,
        -40,
        &[("d7-c6", -20), ("f7-g6", -35)],
    );
    // Separated, but barely searched: the ranking is not a claim yet.
    seat(
        &mut book,
        Symmetry::Files,
        &["d2-c3", "d7-c6"],
        6,
        30,
        &[("c3-c4", 30), ("f2-g3", 10)],
    );
    let values = book.values();
    let certified = book.certified_openings(CertaintyLimits::default(), &values);

    assert_eq!(certified.len(), 1);
    assert_eq!(
        notated(&certified[0].line),
        ["d2-c3", "d7-c6"],
        "the shallow position's best move is not certified"
    );
    assert_eq!(certified[0].stop, CertaintyStop::Shallow);
}

#[test]
fn a_forced_result_is_not_an_opening() {
    let (_scratch, mut book) = scratch_book("certify-decisive");
    seat(&mut book, Symmetry::Files, &[], 20, 40, &[("d2-c3", 40)]);
    // A won position is a finished game. The test is against the mate score,
    // so re-tuning the evaluation cannot move this line across the bar.
    seat(
        &mut book,
        Symmetry::Files,
        &["d2-c3"],
        20,
        -(MATE_SCORE - 4),
        &[("d7-c6", -(MATE_SCORE - 5)), ("f7-g6", -(MATE_SCORE - 9))],
    );
    let values = book.values();
    let certified = book.certified_openings(
        CertaintyLimits {
            minimum_plies: 1,
            ..CertaintyLimits::default()
        },
        &values,
    );

    assert_eq!(certified.len(), 1);
    assert_eq!(notated(&certified[0].line), ["d2-c3"]);
    assert_eq!(certified[0].stop, CertaintyStop::Decisive);
}

#[test]
fn the_ply_cap_is_reported_as_its_own_reason() {
    let (_scratch, mut book) = scratch_book("certify-cap");
    seat(&mut book, Symmetry::Files, &[], 20, 40, &[("d2-c3", 40)]);
    seat(
        &mut book,
        Symmetry::Files,
        &["d2-c3"],
        20,
        -40,
        &[("d7-c6", -20), ("f7-g6", -35)],
    );
    let values = book.values();
    let certified = book.certified_openings(
        CertaintyLimits {
            plies: 1,
            minimum_plies: 1,
            ..CertaintyLimits::default()
        },
        &values,
    );

    assert_eq!(certified.len(), 1);
    assert_eq!(notated(&certified[0].line), ["d2-c3"]);
    assert_eq!(
        certified[0].stop,
        CertaintyStop::Plies,
        "certainty to spare, cut by the cap"
    );
}

#[test]
fn an_uncertain_book_certifies_nothing_rather_than_guessing() {
    let (_scratch, mut book) = scratch_book("certify-empty");
    // Every first move ranked, nothing behind any of them: the shape of a
    // survey pass, and the shape V5's book was in when this was written.
    seat(
        &mut book,
        Symmetry::Files,
        &[],
        20,
        40,
        &[("d2-c3", 40), ("d3-d4", 30), ("e3-e4", 5)],
    );
    let values = book.values();
    let certified = book.certified_openings(CertaintyLimits::default(), &values);
    assert!(
        certified.is_empty(),
        "a first move with no analyzed reply is not a certified opening"
    );
}

#[test]
fn a_graph_export_carries_the_certified_openings() {
    let (_scratch, mut book) = scratch_book("certify-export");
    seat(&mut book, Symmetry::Files, &[], 20, 40, &[("d2-c3", 40)]);
    seat(
        &mut book,
        Symmetry::Files,
        &["d2-c3"],
        20,
        -40,
        &[("d7-c6", -20), ("f7-g6", -35)],
    );
    let exported = book
        .web_export_flat(8, 1, 4, CertaintyLimits::default())
        .expect("graph export");
    // `value` is 20 rather than the root edge's own 40: the child is in the
    // book, so its backed-up value is what the move is worth.
    assert!(exported.contains(
        r#""certified":[{"rank":1,"value":20,"stop":"frontier","alternatives":0,"depth":20"#
    ));
    assert!(exported.contains(r#""line":["d2-c3","d7-c6"]"#));
    assert!(
        exported.contains(r#""certainty":{"depth":14,"plies":8,"minimumPlies":2,"openings":8}"#)
    );
}
