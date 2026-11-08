#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Usage: bash scripts/lint-workflows.sh <workflow-dir>
set -euo pipefail
shopt -s nullglob
DIR=${1:?workflow directory required}
FILES=("$DIR"/*.yml "$DIR"/*.yaml)
[ ${#FILES[@]} -gt 0 ] || { echo "workflow-lint: no workflows in $DIR" >&2; exit 1; }
REPO=$(cd "$(dirname "$0")/.." && pwd)
POLICY=$(node "$REPO/scripts/workflow-policy.mjs" "$DIR")
printf '%s\n' "$POLICY"
if ! printf '%s\n' "$POLICY" | grep -Eq '^workflow-policy: [1-9][0-9]* files OK$'; then
  echo "workflow-lint: policy did not confirm validation" >&2
  exit 1
fi
actionlint "${FILES[@]}"
DEPENDABOT=("$DIR"/../dependabot.yml "$DIR"/../dependabot.yaml)
AUDIT=("${FILES[@]}")
for file in "${DEPENDABOT[@]}"; do
  [ ! -f "$file" ] || AUDIT+=("$file")
done
zizmor --no-config --offline --strict-collection --no-ignores --min-severity=medium "${AUDIT[@]}"
