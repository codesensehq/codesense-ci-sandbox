#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Run the desktop and host crate suites with the shared CI timeout profile.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
PACKAGES=(-p codesense-guards -p codesense -p codesense-host-proto
  -p codesense-pty-core -p codesense-fs-core -p codesense-session-server)
status=0
cargo nextest run --profile ci "${PACKAGES[@]}" || status=$?
cargo test --doc "${PACKAGES[@]}" --no-fail-fast || status=$?
exit "$status"
