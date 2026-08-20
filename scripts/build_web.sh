#!/bin/sh

# Copyright (C) 2026 Henry Abrahamsen
#
# This file is part of RPSFish.
#
# RPSFish is free software: you can redistribute it and/or modify it under the
# terms of the GNU Lesser General Public License as published by the Free
# Software Foundation, either version 3 of the License, or (at your option) any
# later version.
#
# RPSFish is distributed in the hope that it will be useful, but WITHOUT ANY
# WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR
# A PARTICULAR PURPOSE. See the GNU Lesser General Public License for more
# details.
#
# You should have received a copy of the GNU Lesser General Public License
# along with RPSFish. If not, see <https://www.gnu.org/licenses/>.
#
# SPDX-License-Identifier: LGPL-3.0-or-later

set -eu

RPSFISH_SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
RPSFISH_ENGINE_DIR=$(dirname -- "$RPSFISH_SCRIPT_DIR")
RPSFISH_WEB_DIR=$(dirname -- "$RPSFISH_ENGINE_DIR")/frontend/public/rpsfish

if command -v rustup >/dev/null 2>&1; then
  RPSFISH_CARGO=$(rustup which cargo)
  RPSFISH_RUSTC=$(rustup which rustc)
  "$RPSFISH_CARGO" \
    --config "build.rustc=\"$RPSFISH_RUSTC\"" \
    build \
    --manifest-path "$RPSFISH_ENGINE_DIR/Cargo.toml" \
    --release \
    --target wasm32-unknown-unknown \
    --lib
else
  cargo build \
    --manifest-path "$RPSFISH_ENGINE_DIR/Cargo.toml" \
    --release \
    --target wasm32-unknown-unknown \
    --lib
fi

mkdir -p "$RPSFISH_WEB_DIR"
cp \
  "$RPSFISH_ENGINE_DIR/target/wasm32-unknown-unknown/release/rpsfish.wasm" \
  "$RPSFISH_WEB_DIR/rpsfish.wasm"
chmod 644 "$RPSFISH_WEB_DIR/rpsfish.wasm"

echo "RPSFish WebAssembly copied to $RPSFISH_WEB_DIR/rpsfish.wasm"
