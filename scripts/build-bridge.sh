#!/bin/bash
# SPDX-License-Identifier: MIT
# Build the codesense-bridge sidecar binary and copy it to the location
# Tauri expects for externalBin bundling.
#
# Usage:
#   ./scripts/build-bridge.sh           # debug build
#   ./scripts/build-bridge.sh --release # release build

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARIES_DIR="$PROJECT_ROOT/src-tauri/binaries"

# Determine build profile
PROFILE="debug"
CARGO_FLAGS=""
if [[ "${1:-}" == "--release" ]]; then
    PROFILE="release"
    CARGO_FLAGS="--release"
fi

# Get the target triple
TARGET_TRIPLE=$(rustc -vV | grep 'host:' | awk '{print $2}')
if [[ -z "$TARGET_TRIPLE" ]]; then
    echo "Error: Could not determine Rust target triple" >&2
    exit 1
fi

echo "Building codesense-bridge ($PROFILE) for $TARGET_TRIPLE..."

# Build the bridge crate
cargo build -p codesense-bridge $CARGO_FLAGS

# Ensure binaries directory exists
mkdir -p "$BINARIES_DIR"

# Copy with target-triple suffix (Tauri sidecar naming convention)
SOURCE="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}/$PROFILE/codesense-bridge"
DEST="$BINARIES_DIR/codesense-bridge-$TARGET_TRIPLE"

if [[ ! -f "$SOURCE" ]]; then
    echo "Error: Built binary not found at $SOURCE" >&2
    exit 1
fi

cp "$SOURCE" "$DEST"
echo "Copied: $DEST"
