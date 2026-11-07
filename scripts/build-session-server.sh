#!/bin/bash
# SPDX-License-Identifier: MIT
# Build the codesense-session-server binary and copy it where Tauri bundles it,
# recording its sha256 next to it.
#
# Usage:
#   ./scripts/build-session-server.sh [--release] [--target <triple>]...
#
# With no --target it builds for the host triple: the sidecar the app runs
# (Tauri externalBin). Each --target builds the host agent for another machine,
# e.g. the Linux machines the app reaches over SSH:
#
#   ./scripts/build-session-server.sh --release \
#       --target x86_64-unknown-linux-musl --target aarch64-unknown-linux-musl
#
# How a musl target is built:
#   - Natively when its Rust target is installed (CI runs `rustup target add`).
#     The binary is pure Rust and the musl targets link statically from their
#     self-contained crt, so rust-lld links it for either architecture from
#     any host: no C cross toolchain.
#   - Otherwise, on a non-Linux host, inside Docker the same way: a
#     `rust:<host rustc version>-slim`
#     image with both musl targets added ($CODESENSE_MUSL_BUILDER_IMAGE) and
#     its toolchain pinned over rust-toolchain.toml's channel, the
#     crate registry cached in a named volume ($CODESENSE_MUSL_CARGO_VOLUME),
#     and a target dir of its own under target/musl-docker. Nothing is
#     installed into the host toolchain, and x86_64 builds on an arm64 host
#     without emulation.
#
# Each artifact lands at src-tauri/binaries/codesense-session-server-<triple>,
# with <artifact>.sha256 in `sha256sum` format ("<hex>  <file name>"). The
# terminal emulator worker each of them runs is built for the same triples
# beside them (scripts/build-emulator.sh).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARIES_DIR="$PROJECT_ROOT/src-tauri/binaries"
TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
BIN_NAME="codesense-session-server"

usage() {
    echo "usage: $0 [--release] [--target <triple>]..." >&2
    exit 1
}

PROFILE="debug"
CARGO_FLAGS=()
TARGETS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --release)
            PROFILE="release"
            CARGO_FLAGS+=(--release)
            ;;
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

sha256_line() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1"
    else
        shasum -a 256 "$1"
    fi
}

# Copy the built binary for $2 from $1 to the bundle location and record its
# sha256 beside it.
copy_artifact() {
    local source="$1" triple="$2"
    local name="${3:-$BIN_NAME}-$triple"
    if [[ ! -f "$source" ]]; then
        echo "Error: Built binary not found at $source" >&2
        exit 1
    fi
    mkdir -p "$BINARIES_DIR"
    cp "$source" "$BINARIES_DIR/$name"
    (cd "$BINARIES_DIR" && sha256_line "$name" >"$name.sha256")
    echo "Copied: $BINARIES_DIR/$name ($(cut -d' ' -f1 "$BINARIES_DIR/$name.sha256"))"
}

# `CARGO_TARGET_<TRIPLE>_LINKER`, for a triple.
linker_var() {
    echo "CARGO_TARGET_$(echo "$1" | tr 'a-z-' 'A-Z_')_LINKER"
}

target_installed() {
    # grep reads to the end: an early exit would SIGPIPE rustup under pipefail.
    command -v rustup >/dev/null 2>&1 && rustup target list --installed | grep -x "$1" >/dev/null
}

cd "$PROJECT_ROOT"
emulator_args=()
if [[ "$PROFILE" == "release" ]]; then
    emulator_args+=(--release)
fi
for triple in "${TARGETS[@]}"; do
    emulator_args+=(--target "$triple")
done
bash "$SCRIPT_DIR/build-emulator.sh" "${emulator_args[@]}"

DOCKER_TARGETS=()
for triple in "${TARGETS[@]}"; do
    hash_features=()
    if [[ "$triple" == *-linux-musl ]]; then
        hash_features=(--features "$BIN_NAME/portable-hash")
    fi
    if [[ "$triple" == "$HOST_TRIPLE" ]]; then
        echo "Building $BIN_NAME ($PROFILE) for $triple..."
        cargo build -p "$BIN_NAME" -p codesense-bridge ${hash_features[@]+"${hash_features[@]}"} ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"}
        copy_artifact "$TARGET_DIR/$PROFILE/$BIN_NAME" "$triple"
        copy_artifact "$TARGET_DIR/$PROFILE/codesense-bridge" "$triple" codesense-bridge
    elif [[ "$triple" != *-linux-musl ]]; then
        echo "Error: only the host triple and *-linux-musl targets are built here, not $triple" >&2
        exit 1
    elif target_installed "$triple"; then
        echo "Building $BIN_NAME ($PROFILE) for $triple with rust-lld..."
        env "$(linker_var "$triple")=rust-lld" \
            cargo build --locked -p "$BIN_NAME" -p codesense-bridge --features "$BIN_NAME/portable-hash" --target "$triple" ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"}
        copy_artifact "$TARGET_DIR/$triple/$PROFILE/$BIN_NAME" "$triple"
        copy_artifact "$TARGET_DIR/$triple/$PROFILE/codesense-bridge" "$triple" codesense-bridge
    elif [[ "$(uname -s)" == "Linux" ]]; then
        echo "Error: install the target first: rustup target add $triple" >&2
        exit 1
    else
        DOCKER_TARGETS+=("$triple")
    fi
done

if [[ ${#DOCKER_TARGETS[@]} -gt 0 ]]; then
    if ! docker info >/dev/null 2>&1; then
        echo "Error: ${DOCKER_TARGETS[*]} needs the Rust target installed, or Docker running" >&2
        exit 1
    fi
    RUST_VERSION=$(rustc -V | awk '{print $2}')
    if [[ ! "$RUST_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "Error: the Docker build needs a stable host rustc (rust:<version> image), not $RUST_VERSION" >&2
        exit 1
    fi
    IMAGE="${CODESENSE_MUSL_BUILDER_IMAGE:-codesense-musl-builder:$RUST_VERSION}"
    CARGO_VOLUME="${CODESENSE_MUSL_CARGO_VOLUME:-codesense-musl-cargo-registry}"
    if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
        echo "Building the musl builder image $IMAGE..."
        docker build -t "$IMAGE" - <<EOF
FROM rust:$RUST_VERSION-slim-bookworm
RUN rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
EOF
    fi
    target_args=()
    linker_env=()
    for triple in "${DOCKER_TARGETS[@]}"; do
        target_args+=(--target "$triple")
        linker_env+=(-e "$(linker_var "$triple")=rust-lld")
    done
    echo "Building $BIN_NAME ($PROFILE) in Docker for ${DOCKER_TARGETS[*]}..."
    docker run --rm \
        -v "$PROJECT_ROOT":/src -w /src \
        -v "$CARGO_VOLUME":/usr/local/cargo/registry \
        -e CARGO_TARGET_DIR=/src/target/musl-docker \
        -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
        -e RUSTUP_TOOLCHAIN="$RUST_VERSION" \
        -e CODESENSE_BUILD_COMMIT="$(git -C "$PROJECT_ROOT" rev-parse --short=12 HEAD 2>/dev/null || true)" \
        "${linker_env[@]}" \
        "$IMAGE" \
        cargo build --locked -p "$BIN_NAME" -p codesense-bridge --features "$BIN_NAME/portable-hash" "${target_args[@]}" ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"}
    for triple in "${DOCKER_TARGETS[@]}"; do
        copy_artifact "$PROJECT_ROOT/target/musl-docker/$triple/$PROFILE/$BIN_NAME" "$triple"
        copy_artifact "$PROJECT_ROOT/target/musl-docker/$triple/$PROFILE/codesense-bridge" "$triple" codesense-bridge
    done
fi
