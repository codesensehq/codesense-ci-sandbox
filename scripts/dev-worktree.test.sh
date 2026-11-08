#!/usr/bin/env bash
# SPDX-License-Identifier: MIT

set -euo pipefail

# The hook installer has its own tests; never touch this clone's real hooks.
export CODESENSE_SKIP_HOOK_INSTALL=1

TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

cat >"$TEST_DIR/pnpm" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$FORCE_COLOR" >"$DEV_WORKTREE_CAPTURE"
printf '%s\n' "$@" >>"$DEV_WORKTREE_CAPTURE"
EOF
chmod +x "$TEST_DIR/pnpm"

CAPTURE_FILE="$TEST_DIR/arguments"
(
  cd "$TEST_DIR"
  PATH="$TEST_DIR:$PATH" \
    NVM_DIR="$TEST_DIR/no-nvm" \
    DEV_WORKTREE_CAPTURE="$CAPTURE_FILE" \
    "$OLDPWD/scripts/dev-worktree.sh" 7776 >/dev/null
)

EXPECTED_FILE="$TEST_DIR/expected"
cat >"$EXPECTED_FILE" <<'EOF'
1
run
tauri
dev
--config
{"build":{"devUrl":"http://localhost:7776","beforeDevCommand":"vite --port 7776 --strictPort"}}
EOF

diff -u "$EXPECTED_FILE" "$CAPTURE_FILE"

PATH="$TEST_DIR:$PATH" \
  NVM_DIR="$TEST_DIR/no-nvm" \
  DEV_WORKTREE_CAPTURE="$CAPTURE_FILE" \
  ./scripts/dev-worktree.sh >/dev/null
RANDOM_PORT="$(sed -n 's#.*localhost:\([0-9][0-9]*\).*#\1#p' "$CAPTURE_FILE" | head -1)"
[[ "$RANDOM_PORT" =~ ^[0-9]+$ ]]
(( RANDOM_PORT >= 1 && RANDOM_PORT <= 65535 ))

if NVM_DIR="$TEST_DIR/no-nvm" ./scripts/dev-worktree.sh invalid >"$TEST_DIR/stdout" 2>"$TEST_DIR/stderr"; then
  echo "Expected an invalid port to fail." >&2
  exit 1
fi
grep -Fqx 'Port must be an integer between 1 and 65535.' "$TEST_DIR/stderr"

if NVM_DIR="$TEST_DIR/no-nvm" ./scripts/dev-worktree.sh 7776 7777 >"$TEST_DIR/stdout" 2>"$TEST_DIR/stderr"; then
  echo "Expected extra arguments to fail." >&2
  exit 1
fi
grep -Fqx 'Usage: ./scripts/dev-worktree.sh [port]' "$TEST_DIR/stderr"
