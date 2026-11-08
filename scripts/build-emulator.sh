#!/bin/bash
# SPDX-License-Identifier: MIT
# Build the terminal emulator worker (src-emulator/) into one executable per
# platform: the esbuild bundle compiled with Bun (`bun build --compile`), so the
# runtime and the worker ship, sign and version-pin as a single file.
#
# Usage:
#   ./scripts/build-emulator.sh [--release] [--target <triple>]...
#
# With no --target it builds for the host triple (the worker the app ships). Each
# --target builds for a machine the host agent is installed on, e.g.
#
#   ./scripts/build-emulator.sh --target x86_64-unknown-linux-musl --target aarch64-unknown-linux-musl
#
# Every Bun used here (the compiler for this machine and the runtime of each
# target) is a release zip pinned by sha256 below, downloaded once into
# $CODESENSE_BUN_CACHE (default target/bun). A Linux target gets Bun's glibc
# build: the musl build still needs libstdc++, so it is no more portable.
#
# Each artifact lands at src-tauri/binaries/codesense-emulator-<triple>, with
# <artifact>.sha256 in `sha256sum` format ("<hex>  <file name>"). The worker's
# notices (codesense-emulator-NOTICE.md) and its JavaScript bundle
# (codesense-emulator.cjs, shipped so the LGPL runtime can be relinked) are
# copied beside them.
#
# For a macOS target it also writes src-tauri/binaries/codesense-emulator, the
# copy the app bundle ships (tauri.conf.json bundle.macOS.files). It is signed
# here, not by Tauri: Tauri signs every externalBin with the app's one
# entitlements file, and only this binary may carry the JIT entitlement
# (src-tauri/emulator.entitlements.plist). A --release build with
# $APPLE_SIGNING_IDENTITY set (src-tauri/.env) signs it with that identity, the
# hardened runtime and a secure timestamp, which notarization needs and which
# asks Apple's timestamp server over the network. Any other build keeps Bun's
# ad-hoc signature, so `tauri dev` works offline.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/download-retry.sh
. "$SCRIPT_DIR/lib/download-retry.sh"
BINARIES_DIR="$PROJECT_ROOT/src-tauri/binaries"
BUNDLE="$PROJECT_ROOT/target/emulator/codesense-emulator.cjs"
BIN_NAME="codesense-emulator"
NOTICES="$PROJECT_ROOT/src-tauri/resources/emulator/NOTICE.md"
ENTITLEMENTS="$PROJECT_ROOT/src-tauri/emulator.entitlements.plist"
BUN_VERSION="1.4.2"
BUN_CACHE="${CODESENSE_BUN_CACHE:-$PROJECT_ROOT/target/bun}/$BUN_VERSION"

usage() {
    echo "usage: $0 [--release] [--target <triple>]..." >&2
    exit 1
}

RELEASE=0
TARGETS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --release) RELEASE=1 ;;
        --target)
            [[ $# -ge 2 ]] || usage
            TARGETS+=("$2")
            shift
            ;;
        *) usage ;;
    esac
    shift
done

HOST_TRIPLE=$(rustc -vV | awk '/^host:/ {print $2}')
if [[ -z "$HOST_TRIPLE" ]]; then
    echo "Error: Could not determine Rust target triple" >&2
    exit 1
fi
if [[ ${#TARGETS[@]} -eq 0 ]]; then
    TARGETS=("$HOST_TRIPLE")
fi

# The Bun release asset for a Rust triple.
bun_asset() {
    case "$1" in
        aarch64-apple-darwin) echo "bun-darwin-aarch64" ;;
        x86_64-apple-darwin) echo "bun-darwin-x64" ;;
        # baseline: runs on x86_64 CPUs without AVX2, which remote hosts may be.
        x86_64-unknown-linux-gnu | x86_64-unknown-linux-musl) echo "bun-linux-x64-baseline" ;;
        aarch64-unknown-linux-gnu | aarch64-unknown-linux-musl) echo "bun-linux-aarch64" ;;
        *) return 1 ;;
    esac
}

# `bun build --target` for a Bun release asset.
bun_compile_target() {
    case "$1" in
        bun-darwin-aarch64) echo "bun-darwin-arm64" ;;
        bun-darwin-x64) echo "bun-darwin-x64" ;;
        bun-linux-x64-baseline) echo "bun-linux-x64-baseline" ;;
        bun-linux-aarch64) echo "bun-linux-arm64" ;;
    esac
}

# From https://github.com/oven-sh/bun/releases/download/bun-v$BUN_VERSION/SHASUMS256.txt
bun_sha256() {
    case "$1" in
        bun-darwin-aarch64) echo "90987a3a16d7db556d886ac3d551e7b6d3edf0a1cf43acaed622e8676be1d12f" ;;
        bun-darwin-x64) echo "80520d7e17526308c9185d261679ac6d27798d3803a0e9f7ff9121ab8affb012" ;;
        bun-linux-x64-baseline) echo "c678040f14fe0440eb839d37cbd0ce4c051a32da72806ac97de6a6aab6bf728f" ;;
        bun-linux-aarch64) echo "54328bbc2d9c8e0c9f892c544d66c57a83b84139e34909e5ee81758f1ac8fda7" ;;
    esac
}

sha256_hex() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# Print the path of the pinned Bun for $1 (a release asset), downloading and
# verifying it on first use.
fetch_bun() {
    local asset="$1"
    local binary="$BUN_CACHE/$asset/bun"
    if [[ -x "$binary" ]]; then
        echo "$binary"
        return
    fi
    local expected zip actual
    expected="$(bun_sha256 "$asset")"
    mkdir -p "$BUN_CACHE"
    zip="$(mktemp "$BUN_CACHE/.$asset.XXXXXX")"
    # retries transient failures (scripts/lib/download-retry.sh); the checksum below applies either way
    download_with_retry "$zip" \
        "https://github.com/oven-sh/bun/releases/download/bun-v$BUN_VERSION/$asset.zip" \
        --proto '=https' --proto-redir '=https' ||
        { rm -f "$zip"; echo "Error: could not download $asset.zip" >&2; exit 1; }
    actual="$(sha256_hex "$zip")"
    if [[ "$actual" != "$expected" ]]; then
        rm -f "$zip"
        echo "Error: $asset.zip sha256 $actual, expected $expected" >&2
        exit 1
    fi
    rm -rf "${BUN_CACHE:?}/$asset"
    unzip -q "$zip" -d "$BUN_CACHE"
    rm -f "$zip"
    chmod +x "$binary"
    echo "$binary"
}

# The Bun that runs the compiler on this machine.
compiler_asset() {
    case "$(uname -sm)" in
        "Darwin arm64") echo "bun-darwin-aarch64" ;;
        "Darwin x86_64") echo "bun-darwin-x64" ;;
        "Linux x86_64") echo "bun-linux-x64-baseline" ;;
        "Linux aarch64" | "Linux arm64") echo "bun-linux-aarch64" ;;
        *)
            echo "Error: no Bun to compile with on $(uname -sm)" >&2
            exit 1
            ;;
    esac
}

cd "$PROJECT_ROOT"
for triple in "${TARGETS[@]}"; do
    if ! bun_asset "$triple" >/dev/null; then
        echo "Error: no emulator runtime for $triple" >&2
        exit 1
    fi
done

echo "Bundling the emulator worker..."
# esbuild strips types without checking them. The bundle runs under the pinned Bun
# (compiled below), never the repo's Node, so --target only sets syntax lowering and
# stays node20 independent of .nvmrc.
node_modules/.bin/tsc -p src-emulator
mkdir -p "$(dirname "$BUNDLE")"
node_modules/.bin/esbuild src-emulator/main.ts --bundle --platform=node --target=node20 \
    --format=cjs --outfile="$BUNDLE" --log-level=warning

COMPILER="$(fetch_bun "$(compiler_asset)")"
mkdir -p "$BINARIES_DIR"
cp "$NOTICES" "$BINARIES_DIR/$BIN_NAME-NOTICE.md"
cp "$BUNDLE" "$BINARIES_DIR/$BIN_NAME.cjs"
for triple in "${TARGETS[@]}"; do
    asset="$(bun_asset "$triple")"
    runtime="$(fetch_bun "$asset")"
    name="$BIN_NAME-$triple"
    echo "Compiling $BIN_NAME for $triple (Bun $BUN_VERSION, $asset)..."
    # A worker must never pick up a .env or bunfig.toml from its working directory.
    "$COMPILER" build --compile --target="$(bun_compile_target "$asset")" \
        --compile-executable-path="$runtime" \
        --no-compile-autoload-dotenv --no-compile-autoload-bunfig \
        "$BUNDLE" --outfile "$BINARIES_DIR/$name" >/dev/null
    (cd "$BINARIES_DIR" && echo "$(sha256_hex "$name")  $name" >"$name.sha256")
    echo "Built: $BINARIES_DIR/$name ($(cut -d' ' -f1 "$BINARIES_DIR/$name.sha256"))"
    if [[ "$triple" == *-apple-darwin ]]; then
        app_copy="$BINARIES_DIR/$BIN_NAME"
        cp "$BINARIES_DIR/$name" "$app_copy"
        if [[ $RELEASE -eq 1 && -n "${APPLE_SIGNING_IDENTITY:-}" ]]; then
            codesign --force --timestamp --options runtime \
                --entitlements "$ENTITLEMENTS" -s "$APPLE_SIGNING_IDENTITY" "$app_copy"
            echo "Signed: $app_copy (JIT entitlement only)"
        fi
    fi
done
