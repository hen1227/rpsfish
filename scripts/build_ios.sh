#!/bin/sh

# Copyright (C) 2026 Henry Abrahamsen
# SPDX-License-Identifier: LGPL-3.0-or-later

set -eu

RPSFISH_SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
RPSFISH_ENGINE_DIR=$(dirname -- "$RPSFISH_SCRIPT_DIR")
RPSFISH_TARGET_DIR="$RPSFISH_ENGINE_DIR/target"
RPSFISH_IOS_DIR="$RPSFISH_TARGET_DIR/ios"
RPSFISH_FRAMEWORK="$RPSFISH_IOS_DIR/RPSFish.xcframework"
# Where the app expects to find it, following build_web.sh: the engine builds
# straight into the consumer rather than leaving it to be copied by hand.
RPSFISH_MODULE_DIR=$(dirname -- "$RPSFISH_ENGINE_DIR")/frontend/modules/rpsfish/ios

if [ "$(uname -s)" != "Darwin" ]; then
  echo "RPSFish iOS export requires macOS and Xcode." >&2
  exit 1
fi

for RPSFISH_TOOL in xcodebuild xcrun; do
  if ! command -v "$RPSFISH_TOOL" >/dev/null 2>&1; then
    echo "Missing required tool: $RPSFISH_TOOL" >&2
    exit 1
  fi
done

RPSFISH_TARGETS="aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios"
if command -v rustup >/dev/null 2>&1; then
  RPSFISH_CARGO=$(rustup which cargo)
  RPSFISH_RUSTC=$(rustup which rustc)
  RPSFISH_INSTALLED=$(rustup target list --installed)
  for RPSFISH_TARGET in $RPSFISH_TARGETS; do
    if ! printf '%s\n' "$RPSFISH_INSTALLED" | grep -qx "$RPSFISH_TARGET"; then
      echo "Missing Rust target: $RPSFISH_TARGET" >&2
      echo "Install all iOS targets with:" >&2
      echo "  rustup target add $RPSFISH_TARGETS" >&2
      exit 1
    fi
  done
else
  RPSFISH_CARGO=cargo
  RPSFISH_RUSTC=rustc
fi

for RPSFISH_TARGET in $RPSFISH_TARGETS; do
  "$RPSFISH_CARGO" \
    --config "build.rustc=\"$RPSFISH_RUSTC\"" \
    build \
    --manifest-path "$RPSFISH_ENGINE_DIR/Cargo.toml" \
    --release \
    --target "$RPSFISH_TARGET" \
    --lib
done

# Both slices carry the *same* library name, in directories of their own.
# CocoaPods links a static-library XCFramework with one `-l` flag shared by
# every slice, so a simulator archive called anything else links on a device
# and fails to link in the simulator.
RPSFISH_DEVICE_DIR="$RPSFISH_IOS_DIR/device"
RPSFISH_SIMULATOR_DIR="$RPSFISH_IOS_DIR/simulator"
mkdir -p "$RPSFISH_DEVICE_DIR" "$RPSFISH_SIMULATOR_DIR"

cp "$RPSFISH_TARGET_DIR/aarch64-apple-ios/release/librpsfish.a" \
  "$RPSFISH_DEVICE_DIR/librpsfish.a"

rm -f "$RPSFISH_SIMULATOR_DIR/librpsfish.a"
xcrun lipo -create \
  "$RPSFISH_TARGET_DIR/aarch64-apple-ios-sim/release/librpsfish.a" \
  "$RPSFISH_TARGET_DIR/x86_64-apple-ios/release/librpsfish.a" \
  -output "$RPSFISH_SIMULATOR_DIR/librpsfish.a"

rm -rf "$RPSFISH_FRAMEWORK"
xcodebuild -create-xcframework \
  -library "$RPSFISH_DEVICE_DIR/librpsfish.a" \
  -headers "$RPSFISH_ENGINE_DIR/include" \
  -library "$RPSFISH_SIMULATOR_DIR/librpsfish.a" \
  -headers "$RPSFISH_ENGINE_DIR/include" \
  -output "$RPSFISH_FRAMEWORK"

echo "RPSFish iOS XCFramework written to $RPSFISH_FRAMEWORK"

if [ -d "$RPSFISH_MODULE_DIR" ]; then
  rm -rf "$RPSFISH_MODULE_DIR/RPSFish.xcframework"
  cp -R "$RPSFISH_FRAMEWORK" "$RPSFISH_MODULE_DIR/RPSFish.xcframework"
  echo "RPSFish iOS XCFramework copied to $RPSFISH_MODULE_DIR/RPSFish.xcframework"
else
  echo "Skipped the app copy: $RPSFISH_MODULE_DIR does not exist." >&2
fi
