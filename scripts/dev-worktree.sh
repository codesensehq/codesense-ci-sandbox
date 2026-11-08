#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Start a Tauri dev server on a random port so multiple worktrees can run simultaneously.
# Usage: ./scripts/dev-worktree.sh [port]

set -euo pipefail

if (( $# > 1 )); then
  echo "Usage: $0 [port]" >&2
  exit 2
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

# Privacy hooks guard every push to a public remote (shared by all worktrees).
if [[ "${CODESENSE_SKIP_HOOK_INSTALL:-}" != 1 ]]; then
  sh "$SCRIPT_DIR/git-hooks/install.sh" --quiet ||
    echo "warning: privacy hooks not installed; run scripts/git-hooks/install.sh" >&2
fi

# Ensure correct Node version from .nvmrc
NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
if [ -s "$NVM_DIR/nvm.sh" ]; then
  . "$NVM_DIR/nvm.sh" --no-use
  nvm use --silent
fi

PORT="${1:-}"
if [[ -z "$PORT" ]]; then
  PORT="$(node -e 'const server = require("node:net").createServer(); server.listen(0, "127.0.0.1", () => { console.log(server.address().port); server.close(); });')"
fi

if [[ ! "$PORT" =~ ^[0-9]+$ ]] || (( PORT < 1 || PORT > 65535 )); then
  echo "Port must be an integer between 1 and 65535." >&2
  exit 2
fi

echo "Starting dev server on port $PORT"
echo "  Vite:  http://localhost:$PORT"
echo ""

# Start vite on the chosen port, and tell Tauri to use it
FORCE_COLOR=1 pnpm run tauri dev \
  --config "{\"build\":{\"devUrl\":\"http://localhost:$PORT\",\"beforeDevCommand\":\"vite --port $PORT --strictPort\"}}"
