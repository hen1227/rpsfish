# RPSI

RPSI is the line-based protocol `rpsfish rpsi` speaks on stdin and stdout to
be driven by a host process — the same shape as UCI, adapted for a game with
no mate and no castling-equivalent baggage. It is implemented in
`RpsiSession` in `src/main.rs`; this document describes exactly what that
implementation accepts and prints, nothing more.

Drive it by hand:

```sh
printf 'rpsi\nisready\nnewgame total-war\ngo movetime 1000\nquit\n' \
  | cargo run --release -- rpsi
```

## Commands the engine accepts

| Command | Effect |
| --- | --- |
| `rpsi` | Identify: prints `id name`, `id author`, `protocol 1`, `rules N`, one `option` line per configurable value, one `mode` line per supported mode, then `rpsiok`. |
| `isready` | Applies any pending `Hash` resize, then prints `readyok`. |
| `setoption name <Name> [value <rest>]` | Sets `Hash`, `MultiPV`, `Selective`, `RuleProofs`, or `Param` (see below). |
| `newgame <mode>` | Resets to the mode's starting position, clears search and history state, and loads that mode's opening book if `--book` was given. |
| `position fen <pieces> <side> <territory> [moves <m> ...]` | Sets the position from a three-field FEN and optionally replays moves from it. There is no `startpos`: a real game is defined by the board it actually started from. |
| `legalmoves <m1> <m2> ...` | Advisory only. Cross-checks the host's legal-move list against the engine's own and prints an `info string` if they disagree — this is how a rules divergence between the host and the engine gets caught immediately instead of surfacing as a rejected move later. |
| `go [depth N] [nodes N] [movetime N] [rtime N] [btime N] [rinc N] [binc N] [infinite]` | Searches under the given limits and prints `info` lines followed by `bestmove`. `rtime`/`btime`/`rinc`/`binc` are read only for the side to move; the engine converts remaining clock plus increment into its own move-time budget. With no limit at all it falls back to a bounded preset rather than searching forever. |
| `stop` | Ends the current search early; the engine still answers with `bestmove`. |
| `quit` | Exits. |

An unrecognized command prints `info string unknown command "..."` and is
otherwise ignored, so an RPSI host can send commands from a newer protocol
version without crashing an older engine.

## setoption values

| Name | Type | Meaning |
| --- | --- | --- |
| `Hash` | spin | Transposition table size in MiB. Takes effect on the next `isready`. |
| `MultiPV` | spin | Number of ranked lines `go` reports. |
| `Selective` | check | Toggles the selective pruning layer (see the main [README](../README.md)). |
| `RuleProofs` | check | Toggles rule-derived score bounds (immortal/majority proofs), so their contribution can be measured. |
| `Param` | string | Comma-separated `key=value` evaluation-weight overrides — the same assignment syntax as the CLI's `--param`. |

There is no `setoption` for anything else. **The engine command line is the
only other configuration channel**: a host never sends `setoption` in a real
game, so a hash size or weight set different from the compiled default has to
be passed as a flag after `rpsi` when the process is launched.

## What `go` prints

One line per completed iteration per requested variation:

```
info depth <d> seldepth <sd> multipv <i> score <cp N|win N|loss N> confidence <c> nodes <n> nps <r> time <ms> pv <moves>
```

`score` is `cp N` for a heuristic score, or `win N` / `loss N` in plies for a
proven result — never `mate`, because this game has no checkmate; a forced
win comes from annihilation, completed territory, or a boundary crossing, and
borrowing chess's word for it would be wrong in the one place precision
matters most. `confidence` is a 0–100 iterative-stability indicator, not a win
probability. A principal variation is coordinate moves separated by spaces,
e.g. `d3-d4 d7-d6`.

The search ends with exactly one `bestmove <move>` line — except when the
position has no legal move, where it prints `info string no legal move in
this position` and no `bestmove` at all, since answering a finished position
would otherwise hang the host waiting for a move that will never come.

An opening-book answer is reported the same way a search result would be,
including an `info` line, so a host cannot tell a book move from a searched
one except by its `nodes 0`.
