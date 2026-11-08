#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Public tree and change-range scan. Scanner output stays in a private temporary
# directory: only locators and counts reach CI logs, never credential values.
set -euo pipefail
umask 077
REPO=$(cd "$(dirname "$0")/.." && pwd)
cd "$REPO"
LOG=$(mktemp -d "${TMPDIR:-/tmp}/secret-scan.XXXXXX")
trap 'rm -rf "$LOG"' EXIT

for tool in gitleaks trufflehog; do
  pin=$(awk -v t="$tool" '$1 == t { print $2; exit }' release/scanners.lock)
  if [ "$tool" = gitleaks ]; then
    have=$(gitleaks version 2>/dev/null) || { echo "secret-scan: gitleaks version check failed" >&2; exit 1; }
  else
    have=$(trufflehog --version 2>&1) || { echo "secret-scan: trufflehog version check failed" >&2; exit 1; }
  fi
  have=$(printf '%s\n' "$have" | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
  [ -n "$pin" ] && [ "$have" = "$pin" ] || { echo "secret-scan: $tool version does not match lock" >&2; exit 1; }
done

# Use exactly the same revisions and fallback semantics as the privacy scan.
range=$(node scripts/ci-scan-range.mjs)
[ -n "$range" ] || exit 1

rc=0
for mode in dir git; do
  extra=()
  if [ "$mode" = git ]; then extra+=("--log-opts=$range"); label=history
  else label=tree; fi
  status=0
  gitleaks "$mode" --no-banner --redact -c release/public-export-gitleaks.toml \
    --report-format json --report-path "$LOG/$label.json" ${extra[@]+"${extra[@]}"} . >"$LOG/$label.log" 2>&1 || status=$?
  node scripts/ci-secret-report.mjs gitleaks "$LOG/$label.json" "$status" "$label" || rc=1
done
status=0
trufflehog filesystem --no-update --no-verification --fail --json \
  --exclude-paths release/public-export-trufflehog.exclude . >"$LOG/trufflehog.jsonl" 2>"$LOG/trufflehog.log" || status=$?
node scripts/ci-secret-report.mjs trufflehog "$LOG/trufflehog.jsonl" "$status" tree || rc=1
exit "$rc"
