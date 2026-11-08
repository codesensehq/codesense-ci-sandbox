#!/bin/sh
# SPDX-License-Identifier: MIT
# Installs the privacy hooks into this clone's hooks directory (core.hooksPath
# is respected when it is set in this repository's own config). A hook already
# there moves to chained/<name> and still runs first, under its own name.
# Safe to run again.
# Usage: scripts/git-hooks/install.sh [--quiet]
set -eu

quiet=
[ "${1:-}" = --quiet ] && quiet=1
MARKER="codesense-privacy-hook-dispatcher"

common=$(git rev-parse --path-format=absolute --git-common-dir)
hooks=$(git rev-parse --path-format=absolute --git-path hooks)
case "$hooks/" in
  "$common"/*) ;;
  *)
    # A global or system hooksPath is shared with every other repository.
    case $(git config --show-scope --get core.hooksPath | cut -f1) in
      local | worktree) ;;
      *)
        echo "privacy hooks: NOT installed: core.hooksPath ($hooks) comes from global or system config." >&2
        echo "privacy hooks: set a repository-local core.hooksPath, or chain scripts/git-hooks/pre-push and commit-msg from it yourself." >&2
        exit 1
        ;;
    esac
    ;;
esac

mkdir -p "$hooks"
lock="$hooks/.privacy-hooks-install.lock"
if ! mkdir "$lock" 2>/dev/null; then
  echo "privacy hooks: another install is running (remove $lock if it is stale)" >&2
  exit 1
fi
trap 'rmdir "$lock"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$hooks/chained"

# The rule for a commit or push this checkout cannot scan, copied into each
# dispatcher because the checkout it runs in may not have this directory.
FALLBACK=$(cat "$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)/fallback.sh")

for hook in pre-push commit-msg; do
  target="$hooks/$hook"
  chained="$hooks/chained/$hook"
  if { [ -e "$target" ] || [ -L "$target" ]; } && ! grep -q "$MARKER" "$target" 2>/dev/null; then
    if [ -e "$chained" ] || [ -L "$chained" ]; then
      echo "privacy hooks: $target and $chained both exist; merge them by hand" >&2
      exit 1
    fi
    if [ -L "$target" ]; then
      # Keep a relative link pointing where it did.
      link=$(readlink "$target")
      case $link in
        /*) ;;
        *) link="$hooks/$link" ;;
      esac
      ln -s "$link" "$chained"
      rm "$target"
    else
      mv "$target" "$chained"
    fi
    echo "privacy hooks: moved the existing $hook hook to $chained; it still runs first" >&2
    [ -x "$chained" ] ||
      echo "privacy hooks: warning: $chained is not executable, so it does not run (nor did it before)" >&2
  fi
  tmp="$target.tmp.$$"
  if [ "$hook" = pre-push ]; then
    # pre-push reads the ref list on stdin: buffer it for both hooks.
    cat >"$tmp" <<HOOK
#!/bin/sh
# $MARKER (scripts/git-hooks/install.sh)
$FALLBACK
dir=\$(CDPATH='' cd -- "\$(dirname -- "\$0")" && pwd) || exit 1
top=\$(git rev-parse --show-toplevel 2>/dev/null) || top=
input=\$(mktemp) || exit 1
trap 'rm -f "\$input"' EXIT
cat >"\$input"
if [ -x "\$dir/chained/pre-push" ]; then
  "\$dir/chained/pre-push" "\$@" <"\$input" || exit \$?
fi
public=
if is_public "\${2:-\$1}"; then
  public=1
  require_inventory
fi
if [ -n "\$top" ] && [ -x "\$top/scripts/git-hooks/pre-push" ] && command -v node >/dev/null 2>&1; then
  "\$top/scripts/git-hooks/pre-push" "\$@" <"\$input"
  exit \$?
fi
if [ -n "\$public" ]; then
  unscanned "a push to \$1"
fi
HOOK
  else
    cat >"$tmp" <<HOOK
#!/bin/sh
# $MARKER (scripts/git-hooks/install.sh)
$FALLBACK
dir=\$(CDPATH='' cd -- "\$(dirname -- "\$0")" && pwd) || exit 1
if [ -x "\$dir/chained/$hook" ]; then
  "\$dir/chained/$hook" "\$@" || exit \$?
fi
public=
if public_remote; then
  public=1
  require_inventory
fi
top=\$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
if [ -x "\$top/scripts/git-hooks/$hook" ] && command -v node >/dev/null 2>&1; then
  exec "\$top/scripts/git-hooks/$hook" "\$@"
fi
if [ -n "\$public" ]; then
  unscanned "a commit message"
fi
HOOK
  fi
  chmod +x "$tmp"
  mv "$tmp" "$target"
done
[ -n "$quiet" ] || echo "privacy hooks installed in $hooks"
