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

// Measure the WebAssembly build the way the browser worker drives it.
//
// usage: node scripts/bench_web.mjs [--wasm path] [--time-ms N] [--nodes N]
//        [--depth N] [--throttle N] [--multipv N] [--single]
import { readFile } from 'node:fs/promises';
import { performance } from 'node:perf_hooks';

const argument = (name, fallback) => {
  const index = process.argv.indexOf(`--${name}`);
  return index >= 0 ? process.argv[index + 1] : fallback;
};

const wasmPath = argument('wasm', 'target/wasm32-unknown-unknown/release/rpsfish.wasm');
const maxTimeMs = Number(argument('time-ms', 3000));
const maxNodes = Number(argument('nodes', 1e9));
const maxDepth = Number(argument('depth', 40));
const throttleMs = Number(argument('throttle', 0));
const variations = Number(argument('multipv', 1));

const ANNIHILATION = ['.........', '.........', '.........', '.R.....s.', '.P.....p.', '.S.....r.', '.........', '.........', '.........'];
const LARGE_ARMY = ['...SSS...', '...PPP...', '...RRR...', '.........', '.........', '.........', '...rrr...', '...ppp...', '...sss...'];
const OFFSETS = { r: 0, p: 1, s: 2, R: 3, P: 4, S: 5 };

const encode = (rows, side, mode) => {
  const boards = Array.from({ length: 8 }, () => 0n);
  rows.forEach((row, y) => {
    [...row].forEach((symbol, x) => {
      const bit = 1n << BigInt(y * 9 + x);
      const offset = OFFSETS[symbol];
      if (offset !== undefined) boards[offset] |= bit;
      // Total War tracks territory; the starting layouts own their own squares.
      if (mode === 1 && offset !== undefined) boards[offset < 3 ? 6 : 7] |= bit;
    });
  });
  const halves = boards.flatMap((value) => [BigInt.asUintN(64, value), BigInt.asUintN(64, value >> 64n)]);
  return [mode, side, 0, ...halves];
};

const suite = [
  { name: 'Annihilation', encoded: encode(ANNIHILATION, 0, 0) },
  { name: 'Total War', encoded: encode(LARGE_ARMY, 0, 1) },
  { name: 'Infiltration', encoded: encode(LARGE_ARMY, 0, 2) },
];

const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const bytes = await readFile(wasmPath);
const { instance } = await WebAssembly.instantiate(bytes, {
  env: { rpsfish_now_ms: () => performance.now() },
});
const engine = instance.exports;
console.log(`abi ${engine.rpsfish_abi_version()} rules ${engine.rpsfish_rules_version()} size ${(bytes.length / 1024).toFixed(1)}KiB`);

let totalDepth = 0;
for (const entry of suite) {
  engine.rpsfish_search_clear();
  engine.rpsfish_history_clear();
  const started = performance.now();
  let cumulativeNodes = 0;
  let depth = 0;
  let stop = 0;
  if (process.argv.includes('--single')) {
    // One call, one iterative deepening. Shows what the worker's per-depth
    // restart costs: with a warm table, almost nothing.
    engine.rpsfish_analyze(...entry.encoded, maxDepth, maxNodes, maxTimeMs, variations);
    cumulativeNodes = engine.rpsfish_analysis_nodes();
    depth = engine.rpsfish_analysis_depth();
    stop = engine.rpsfish_analysis_stop_reason();
  } else {
    for (let target = 1; target <= maxDepth; target += 1) {
      const remainingNodes = maxNodes - cumulativeNodes;
      const remainingTime = maxTimeMs - Math.round(performance.now() - started);
      if (remainingNodes <= 0 || remainingTime <= 0) break;
      engine.rpsfish_analyze(...entry.encoded, target, remainingNodes, remainingTime, variations);
      cumulativeNodes += engine.rpsfish_analysis_nodes();
      depth = engine.rpsfish_analysis_depth();
      stop = engine.rpsfish_analysis_stop_reason();
      if (depth < target || stop === 1 || stop === 2) break;
      if (throttleMs > 0) await wait(throttleMs);
    }
  }
  const elapsed = performance.now() - started;
  totalDepth += depth;
  console.log(
    `  ${entry.name.padEnd(13)} depth ${String(depth).padStart(2)}  seldepth ${String(engine.rpsfish_analysis_selective_depth()).padStart(3)}` +
      `  nodes ${String(cumulativeNodes).padStart(10)}  ${Math.round((cumulativeNodes * 1000) / elapsed).toString().padStart(9)} n/s` +
      `  ${elapsed.toFixed(0)}ms  stop ${stop}`,
  );
}
console.log(`WASM mean depth ${(totalDepth / suite.length).toFixed(3)}`);
