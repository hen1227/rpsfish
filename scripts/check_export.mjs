// Check a `book export` file against the rules the website's import enforces.
//
// The backend rejects a malformed book with one 400 and no detail about which
// of ten thousand positions was wrong, and a rejection discovered at publish
// time has already cost the whole build. So the same rules run here, against
// the file, before anything is uploaded. This is a mirror of
// `validateOpeningBook`/`validateOpeningNode` in
// `backend/internal/server/opening_book_routes.go`; if that file's rules move,
// these have to move with them.
//
// usage: node scripts/check_export.mjs <file.json> [--mode V3] [--weights <hex>]

import { readFileSync } from 'node:fs';

const TREE_FORMAT = 'rps-opening-book/v1';
const GRAPH_FORMAT = 'rps-opening-book/v2';
const MAX_POSITIONS = 100_000;

const argv = process.argv.slice(2);
let file = null;
let wantMode = null;
let wantWeights = null;
for (let index = 0; index < argv.length; index += 1) {
  const flag = argv[index];
  if (flag === '--mode') wantMode = argv[(index += 1)];
  else if (flag === '--weights') wantWeights = argv[(index += 1)];
  else if (flag.startsWith('--')) fail(`unknown flag ${flag}`);
  else file = flag;
}
if (!file) fail('usage: node scripts/check_export.mjs <file.json> [--mode V3] [--weights <hex>]');

function fail(message) {
  console.error(`export check: ${message}`);
  process.exit(1);
}

const validMove = (move) =>
  typeof move === 'string' &&
  move.length === 5 &&
  move[2] === '-' &&
  move[0] >= 'a' && move[0] <= 'i' &&
  move[1] >= '1' && move[1] <= '9' &&
  move[3] >= 'a' && move[3] <= 'i' &&
  move[4] >= '1' && move[4] <= '9' &&
  move.slice(0, 2) !== move.slice(3);

let book;
try {
  book = JSON.parse(readFileSync(file, 'utf8'));
} catch (error) {
  fail(`${file}: ${error.message}`);
}

if (book.format !== TREE_FORMAT && book.format !== GRAPH_FORMAT) {
  fail(`format must be "${GRAPH_FORMAT}" or "${TREE_FORMAT}", got ${JSON.stringify(book.format)}`);
}
for (const field of ['modeId', 'modeName', 'engineVersion']) {
  if (typeof book[field] !== 'string' || book[field].trim() === '') fail(`${field} is required`);
}
if (!(book.rulesVersion > 0)) fail('rulesVersion must be positive');
if (!(book.positionCount >= 1 && book.positionCount <= MAX_POSITIONS)) {
  fail(`positionCount must be between 1 and ${MAX_POSITIONS}, got ${book.positionCount}`);
}
if (!Array.isArray(book.mainLine)) fail('mainLine must be an array');
book.mainLine.forEach((move, index) => {
  if (!validMove(move)) fail(`mainLine move ${index + 1} is not notation like d7-d6`);
});
if (wantMode && book.modeId !== wantMode) fail(`modeId is ${book.modeId}, expected ${wantMode}`);
if (wantWeights && book.weights !== wantWeights) {
  fail(`weights are ${book.weights}, expected ${wantWeights}`);
}

if (book.format === GRAPH_FORMAT) {
  checkGraph();
  process.exit(0);
}

if (!(book.maxPly >= 0 && book.maxPly <= 128)) fail('maxPly must be between 0 and 128');
if (!(book.width >= 1 && book.width <= 64)) fail('width must be between 1 and 64');
if (!book.root) fail('root position is required');

// The tree walk, and the numbers the summary reports. `deepest` is the deepest
// ply the export actually reaches, which is the one thing the header does not
// say: `maxPly` is the cap that was asked for, not the depth that was reached.
let count = 0;
let deepest = 0;
let searchedEdges = 0;
let repetitions = 0;
let truncated = 0;
const walk = (node, ply, where) => {
  count += 1;
  const number = count;
  if (count > MAX_POSITIONS) fail(`opening tree exceeds ${MAX_POSITIONS} positions`);
  if (ply > deepest) deepest = ply;
  // `game.PlayerColor` is a capitalized string on the Go side and `Color`'s
  // Display is the same, so these are the two the import will accept.
  if (node.turn !== 'Red' && node.turn !== 'Blue') {
    fail(`position ${number} (${where}) has invalid side to move: ${JSON.stringify(node.turn)}`);
  }
  for (const field of ['depth', 'selectiveDepth']) {
    if (!(node[field] >= 0 && node[field] <= 127)) {
      fail(`position ${number} (${where}) has invalid ${field} ${node[field]}`);
    }
  }
  const moves = node.moves ?? [];
  if (moves.length > book.width) {
    fail(`position ${number} (${where}) has ${moves.length} moves, wider than width ${book.width}`);
  }
  const seen = new Set();
  let mainMoves = 0;
  moves.forEach((move, index) => {
    const at = `${where}${where ? ' ' : ''}${move.move}`;
    if (!validMove(move.move)) fail(`position ${number} move ${index + 1} is not notation like d7-d6`);
    if (seen.has(move.move)) fail(`position ${number} (${where}) repeats move ${move.move}`);
    seen.add(move.move);
    if (move.rank !== index + 1) {
      fail(`position ${number} move ${move.move} has rank ${move.rank}, expected ${index + 1}`);
    }
    if (move.mainLine) mainMoves += 1;
    if (move.searched) searchedEdges += 1;
    if (move.repetition) repetitions += 1;
    if (move.child) {
      if (!move.searched || move.repetition) {
        fail(`position ${number} move ${move.move} has an inconsistent child`);
      }
      if (ply >= book.maxPly) fail(`position ${number} expands beyond maxPly ${book.maxPly}`);
      walk(move.child, ply + 1, at);
    } else if (move.searched && !move.repetition) {
      truncated += 1;
    }
  });
  if (mainMoves > 1) fail(`position ${number} (${where}) marks more than one main-line move`);
};
walk(book.root, 0, '');
if (count !== book.positionCount) {
  fail(`positionCount says ${book.positionCount}, tree contains ${count}`);
}

const rootScore = book.root.score;
console.log(
  `ok        ${file}\n` +
    `mode      ${book.modeName} (${book.modeId})\n` +
    `engine    ${book.engineVersion}, rules ${book.rulesVersion}, weights ${book.weights}\n` +
    `tree      ${count} positions, ${deepest} plies deep (cap ${book.maxPly}), width ${book.width}\n` +
    `edges     ${searchedEdges} analyzed, ${repetitions} repetitions, ${truncated} cut by the cap\n` +
    `root      ${rootScore >= 0 ? '+' : ''}${rootScore}\n` +
    `main line ${book.mainLine.join(' ')}`
);

/**
 * Check a flat position graph.
 *
 * The one thing a graph can be wrong about that a tree cannot is *closure*: a
 * move naming a child the payload never wrote becomes a line the website can
 * start walking and cannot finish. Everything else here mirrors
 * `validateOpeningGraph` in the backend.
 */
function checkGraph() {
  if (!Array.isArray(book.positions)) fail('positions must be an array');
  if (typeof book.rootKey !== 'string' || !book.rootKey) fail('rootKey is required');
  if (book.positions.length !== book.positionCount) {
    fail(`positionCount says ${book.positionCount}, payload holds ${book.positions.length}`);
  }
  if (!Array.isArray(book.featured)) fail('featured must be an array of lines');

  const keys = new Set();
  for (const [index, position] of book.positions.entries()) {
    if (typeof position.key !== 'string' || !position.key) fail(`position ${index + 1} has no key`);
    if (keys.has(position.key)) fail(`position key ${position.key} appears twice`);
    keys.add(position.key);
  }
  if (!keys.has(book.rootKey)) fail(`rootKey ${book.rootKey} is not one of the positions`);

  let edges = 0;
  let frontier = 0;
  let widest = 0;
  for (const position of book.positions) {
    if (position.turn !== 'Red' && position.turn !== 'Blue') {
      fail(`position ${position.key} has invalid side to move: ${JSON.stringify(position.turn)}`);
    }
    for (const field of ['depth', 'selectiveDepth']) {
      if (!(position[field] >= 0 && position[field] <= 127)) {
        fail(`position ${position.key} has invalid ${field} ${position[field]}`);
      }
    }
    const moves = position.moves ?? [];
    widest = Math.max(widest, moves.length);
    const seen = new Set();
    let mainMoves = 0;
    moves.forEach((move, index) => {
      if (!validMove(move.move)) {
        fail(`position ${position.key} move ${index + 1} is not notation like d7-d6`);
      }
      if (seen.has(move.move)) fail(`position ${position.key} repeats move ${move.move}`);
      seen.add(move.move);
      if (move.rank !== index + 1) {
        fail(`position ${position.key} move ${move.move} has rank ${move.rank}, expected ${index + 1}`);
      }
      if (move.mainLine) mainMoves += 1;
      edges += 1;
      if (move.child === null || move.child === undefined) {
        if (move.searched) frontier += 1;
        return;
      }
      if (!move.searched) {
        fail(`position ${position.key} move ${move.move} names a child but is not marked searched`);
      }
      if (!keys.has(move.child)) {
        fail(`position ${position.key} move ${move.move} names child ${move.child}, which is not in the payload`);
      }
    });
    if (mainMoves > 1) fail(`position ${position.key} marks more than one main-line move`);
  }

  for (const [index, line] of [book.mainLine, ...book.featured].entries()) {
    if (!Array.isArray(line)) fail(`line ${index + 1} is not an array`);
    line.forEach((move, moveIndex) => {
      if (!validMove(move)) fail(`line ${index + 1} move ${moveIndex + 1} is not notation like d7-d6`);
    });
  }

  // Walk every advertised line the way the server will, so a book that
  // validates is also a book that can be browsed.
  //
  // The last move of a line only has to be *ranked*. A principal variation ends
  // on the best move out of its final position, and the position behind that
  // move is exactly the one the scan had not reached -- so it has no child, and
  // that is the frontier rather than a broken line.
  const byKey = new Map(book.positions.map((position) => [position.key, position]));
  const walk = (label, line) => {
    let current = byKey.get(book.rootKey);
    line.forEach((notation, index) => {
      const edge = current?.moves?.find((candidate) => candidate.move === notation);
      if (!edge) fail(`${label} names ${notation} at move ${index + 1}, which is not a ranked move`);
      if (index === line.length - 1) return;
      if (!edge.child) fail(`${label} stops being walkable at move ${index + 1} (${notation})`);
      current = byKey.get(edge.child);
    });
  };
  walk('mainLine', book.mainLine);
  book.featured.forEach((line, index) => walk(`featured line ${index + 1}`, line));

  // The certified openings are what the website leads with, in its own voice,
  // so they are held to more than the shape of their notation. Absent from an
  // export made before certification existed, which reads as "none" -- the
  // honest account of a scan that never asked.
  const STOPS = new Set(['plies', 'indifferent', 'shallow', 'frontier', 'decisive', 'repetition']);
  const certified = book.certified ?? [];
  if (!Array.isArray(certified)) fail('certified must be an array of openings');
  certified.forEach((opening, index) => {
    const label = `certified opening ${index + 1}`;
    if (!Array.isArray(opening.line) || opening.line.length === 0) {
      fail(`${label} has no moves`);
    }
    opening.line.forEach((move, moveIndex) => {
      if (!validMove(move)) fail(`${label} move ${moveIndex + 1} is not notation like d7-d6`);
    });
    if (!STOPS.has(opening.stop)) {
      fail(`${label} stops on ${JSON.stringify(opening.stop)}, which is not a reason the server knows`);
    }
    // A tie count on a line that did not stop at a tie would publish a
    // certainty nobody measured.
    if (opening.alternatives > 0 && opening.stop !== 'indifferent') {
      fail(`${label} counts ${opening.alternatives} tied replies but stops on ${opening.stop}`);
    }
    if (!Number.isInteger(opening.rank) || opening.rank < 1) fail(`${label} has rank ${opening.rank}`);
    if (!Number.isInteger(opening.depth) || opening.depth < 0 || opening.depth > 127) {
      fail(`${label} has invalid depth ${opening.depth}`);
    }
    walk(label, opening.line);
  });
  // Mirror twins are one opening drawn twice, and the engine folds them on the
  // canonical child key. A repeated first move here means that fold broke.
  const firsts = new Set();
  certified.forEach((opening, index) => {
    const first = opening.line[0];
    if (firsts.has(first)) fail(`certified opening ${index + 1} repeats the first move ${first}`);
    firsts.add(first);
  });

  console.log(
    `ok        ${file}\n` +
      `mode      ${book.modeName} (${book.modeId})\n` +
      `engine    ${book.engineVersion}, rules ${book.rulesVersion}, weights ${book.weights}\n` +
      `graph     ${book.positions.length} positions, ${edges} moves, widest ${widest}, ` +
      `${frontier} at the frontier\n` +
      `root      ${book.positions.find((p) => p.key === book.rootKey).score >= 0 ? '+' : ''}` +
      `${book.positions.find((p) => p.key === book.rootKey).score}\n` +
      `main line ${book.mainLine.join(' ')}\n` +
      `certified ${certified.length}${
        book.certainty
          ? ` (depth ${book.certainty.depth}+, up to ${book.certainty.plies} plies)`
          : ''
      }\n` +
      certified
        .map((opening) => `  #${opening.rank} ${opening.line.join(' ')} [${opening.stop}]`)
        .join('\n') +
      (certified.length ? '\n' : '') +
      book.featured.map((line, index) => `featured${index + 1} ${line.join(' ')}`).join('\n')
  );
}
