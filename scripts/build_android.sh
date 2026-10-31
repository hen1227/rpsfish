#!/bin/sh

# Copyright (C) 2026 Henry Abrahamsen
# SPDX-License-Identifier: LGPL-3.0-or-later

set -eu

# One static library per Android ABI, with the C header beside them:
#
#   target/android/<abi>/librpsfish.a
#   target/android/include/RPSFish.h
#
# Static rather than shared, as on iOS. The app links it into a JNI library of
# its own, so the engine stays a C ABI with no knowledge of Java. Only the
# staticlib crate type is built: a cdylib would need the NDK's linker, and
# nothing here wants one.

RPSFISH_SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
RPSFISH_ENGINE_DIR=$(dirname -- "$RPSFISH_SCRIPT_DIR")
RPSFISH_TARGET_DIR="$RPSFISH_ENGINE_DIR/target"
RPSFISH_ANDROID_DIR="$RPSFISH_TARGET_DIR/android"

# Rust target, then the ABI name Android gives it. Every ABI React Native builds
# for by default, so the app's build never asks for one that is missing.
RPSFISH_ABIS="aarch64-linux-android:arm64-v8a
armv7-linux-androideabi:armeabi-v7a
x86_64-linux-android:x86_64
i686-linux-android:x86"

RPSFISH_TARGETS=$(printf '%s\n' "$RPSFISH_ABIS" | cut -d: -f1 | tr '\n' ' ')
if command -v rustup >/dev/null 2>&1; then
  RPSFISH_CARGO=$(rustup which cargo)
  RPSFISH_RUSTC=$(rustup which rustc)
  RPSFISH_INSTALLED=$(rustup target list --installed)
  for RPSFISH_TARGET in $RPSFISH_TARGETS; do
    if ! printf '%s\n' "$RPSFISH_INSTALLED" | grep -qx "$RPSFISH_TARGET"; then
      echo "Missing Rust target: $RPSFISH_TARGET" >&2
      echo "Install all Android targets with:" >&2
      echo "  rustup target add $RPSFISH_TARGETS" >&2
      exit 1
    fi
  done
else
  RPSFISH_CARGO=cargo
  RPSFISH_RUSTC=rustc
fi

printf '%s\n' "$RPSFISH_ABIS" | while IFS=: read -r RPSFISH_TARGET RPSFISH_ABI; do
  "$RPSFISH_CARGO" \
    --config "build.rustc=\"$RPSFISH_RUSTC\"" \
    rustc \
    --manifest-path "$RPSFISH_ENGINE_DIR/Cargo.toml" \
    --release \
    --target "$RPSFISH_TARGET" \
    --lib \
    --crate-type staticlib
  mkdir -p "$RPSFISH_ANDROID_DIR/$RPSFISH_ABI"
  cp "$RPSFISH_TARGET_DIR/$RPSFISH_TARGET/release/librpsfish.a" \
    "$RPSFISH_ANDROID_DIR/$RPSFISH_ABI/librpsfish.a"
done

mkdir -p "$RPSFISH_ANDROID_DIR/include"
cp "$RPSFISH_ENGINE_DIR/include/RPSFish.h" "$RPSFISH_ANDROID_DIR/include/RPSFish.h"

echo "RPSFish Android libraries written to $RPSFISH_ANDROID_DIR"
