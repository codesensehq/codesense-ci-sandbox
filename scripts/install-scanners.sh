#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Install pinned scanners from release/scanners.lock into <dest-dir>.
# Every asset is downloaded to a scratch dir and checked against its sha256 before anything is
# installed; one wrong checksum aborts the whole install and leaves <dest-dir> untouched.
# Usage: scripts/install-scanners.sh <dest-dir> [--lock <file>] [--cache <dir>] [--workflows-only] [--tool <name>]...
# Without selectors, install gitleaks, trufflehog, actionlint and zizmor. cargo-deny and cargo-nextest are opt-in.
# The platform is darwin-arm64 or linux-amd64, from uname; SCANNER_PLATFORM overrides it.
# --cache <dir> keeps each verified archive as <dir>/<sha256>.tar.gz and reuses it on later runs.
# A cached archive is checked against the lock exactly like a download; a mismatch is discarded and downloaded again.
set -euo pipefail
umask 022
REPO=$(cd "$(dirname "$0")/.." && pwd)
LOCK="$REPO/release/scanners.lock"
DEST=""
CACHE=""
SELECTED=()
while [ $# -gt 0 ]; do
  case "$1" in
    --workflows-only) SELECTED+=(actionlint zizmor); shift ;;
    --tool)
      case "${2:-}" in
        gitleaks|trufflehog|actionlint|zizmor|cargo-deny|cargo-nextest) SELECTED+=("$2"); shift 2 ;;
        *) echo "install-scanners: unknown tool selector ${2:-<missing>}" >&2; exit 2 ;;
      esac ;;
    --lock) LOCK=${2:?--lock needs a file}; shift 2 ;;
    --cache) CACHE=${2:?--cache needs a directory}; shift 2 ;;
    -*) echo "install-scanners: unknown option $1" >&2; exit 2 ;;
    *) [ -z "$DEST" ] || { echo "usage: install-scanners.sh <dest-dir> [--lock <file>] [--cache <dir>] [--workflows-only] [--tool <name>]..." >&2; exit 2; }
       DEST=$1; shift ;;
  esac
done
[ -n "$DEST" ] || { echo "usage: install-scanners.sh <dest-dir> [--lock <file>] [--cache <dir>] [--workflows-only] [--tool <name>]..." >&2; exit 2; }
[ ${#SELECTED[@]} -gt 0 ] || SELECTED=(gitleaks trufflehog actionlint zizmor)
[ -f "$LOCK" ] || { echo "install-scanners: no lock file at $LOCK" >&2; exit 1; }

PLATFORM=${SCANNER_PLATFORM:-}
if [ -z "$PLATFORM" ]; then
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) PLATFORM=darwin-arm64 ;;
    Linux-x86_64) PLATFORM=linux-amd64 ;;
    *) echo "install-scanners: no pinned assets for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
  esac
fi

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# Downloads retry transient failures (scripts/lib/download-retry.sh). The whole run shares one 360s
# budget, so all its downloads end within 8 minutes, well inside the shortest calling job's 15-minute
# timeout; a job installs all its tools in one run so they share it. No download is used until its
# sha256 matches the lock.
# shellcheck source=lib/download-retry.sh
. "$REPO/scripts/lib/download-retry.sh"
DOWNLOAD_DEADLINE=$((SECONDS + DOWNLOAD_RETRY_BUDGET))

STAGE=$(mktemp -d "${TMPDIR:-/tmp}/install-scanners.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT
TOOLS=()
while read -r tool version platform sum url; do
  case "$tool" in ''|\#*) continue ;; esac
  [ "$platform" = "$PLATFORM" ] || continue
  case "$tool" in gitleaks|trufflehog|actionlint|zizmor|cargo-deny|cargo-nextest) ;; *) echo "install-scanners: unknown tool $tool in $LOCK" >&2; exit 1 ;; esac
  selected=0
  for wanted in "${SELECTED[@]}"; do
    [ "$wanted" != "$tool" ] || selected=1
  done
  [ "$selected" = 1 ] || continue
  [[ "$sum" =~ ^[0-9a-f]{64}$ ]] || { echo "install-scanners: bad sha256 for $tool in $LOCK" >&2; exit 1; }
  echo "install-scanners: $tool $version ($PLATFORM)"
  got=""
  cached=0
  if [ -n "$CACHE" ] && [ -f "$CACHE/$sum.tar.gz" ] && cp "$CACHE/$sum.tar.gz" "$STAGE/$tool.tar.gz"; then
    got=$(sha256 "$STAGE/$tool.tar.gz")
    if [ "$got" = "$sum" ]; then
      echo "install-scanners: $tool archive from cache"
      cached=1
    else
      echo "install-scanners: cached $tool archive does not match the lock; downloading it again" >&2
      rm -f "$CACHE/$sum.tar.gz"
      got=""
    fi
  fi
  if [ -z "$got" ]; then
    download_with_retry "$STAGE/$tool.tar.gz" "$url" --proto '=https,file' --proto-redir '=https' ||
      { echo "install-scanners: download failed for $tool $version; nothing installed" >&2; exit 1; }
    got=$(sha256 "$STAGE/$tool.tar.gz")
  fi
  if [ "$got" != "$sum" ]; then
    echo "install-scanners: CHECKSUM MISMATCH for $tool $version ($PLATFORM): expected $sum, got $got; nothing installed" >&2
    exit 1
  fi
  if [ -n "$CACHE" ] && [ "$cached" = 0 ]; then
    # best effort: a cache that cannot be written only means the next run downloads again
    { mkdir -p "$CACHE" && cp "$STAGE/$tool.tar.gz" "$CACHE/.$sum.tar.gz.$$" && mv -f "$CACHE/.$sum.tar.gz.$$" "$CACHE/$sum.tar.gz"; } 2>/dev/null ||
      { rm -f "$CACHE/.$sum.tar.gz.$$"; echo "install-scanners: could not cache the $tool archive in $CACHE" >&2; }
  fi
  mkdir "$STAGE/$tool"
  # only the binary itself is taken from the archive
  # cargo-nextest releases contain exactly the root member cargo-nextest.
  member=$tool
  strip=0
  if [ "$tool" = cargo-deny ]; then
    case "$PLATFORM" in
      linux-amd64) triple=x86_64-unknown-linux-musl ;;
      darwin-arm64) triple=aarch64-apple-darwin ;;
      *) echo "install-scanners: no cargo-deny archive layout for $PLATFORM" >&2; exit 1 ;;
    esac
    member="cargo-deny-$version-$triple/cargo-deny"
    strip=1
  fi
  tar -xzf "$STAGE/$tool.tar.gz" -C "$STAGE/$tool" --strip-components="$strip" "$member" ||
    { echo "install-scanners: $tool not found in its archive; nothing installed" >&2; exit 1; }
  [ -f "$STAGE/$tool/$tool" ] && [ ! -L "$STAGE/$tool/$tool" ] ||
    { echo "install-scanners: $tool in its archive is not a regular file; nothing installed" >&2; exit 1; }
  TOOLS+=("$tool")
done <"$LOCK"
for wanted in "${SELECTED[@]}"; do
  found=0
  for tool in ${TOOLS[@]+"${TOOLS[@]}"}; do
    [ "$wanted" != "$tool" ] || found=1
  done
  [ "$found" = 1 ] || { echo "install-scanners: no $PLATFORM entry for selected tool $wanted in $LOCK; nothing installed" >&2; exit 1; }
done
[ ${#TOOLS[@]} -gt 0 ] || { echo "install-scanners: no $PLATFORM entries in $LOCK" >&2; exit 1; }

# every asset verified: stage each binary under a temp name in DEST, then rename them all into place.
# A failure while staging removes what it staged (and DEST, if this run created it), so DEST is as it was.
STAGED=()
MADE_DEST=""
unstage() {
  for t in ${STAGED[@]+"${STAGED[@]}"}; do rm -f "$t"; done
  [ -z "$MADE_DEST" ] || rmdir "$DEST" 2>/dev/null || true
  rm -rf "$STAGE"
}
trap unstage EXIT
[ -d "$DEST" ] || { mkdir -p "$DEST" && MADE_DEST=1; }
for tool in "${TOOLS[@]}"; do
  t="$DEST/.$tool.new"
  if ! rm -f "$t"; then
    echo "install-scanners: cannot stage $tool at $t; $DEST unchanged" >&2; exit 1
  fi
  STAGED+=("$t")
  if ! { cp "$STAGE/$tool/$tool" "$t" && chmod 0755 "$t"; }; then
    echo "install-scanners: cannot stage $tool at $t; $DEST unchanged" >&2; exit 1
  fi
done
for tool in "${TOOLS[@]}"; do mv -f "$DEST/.$tool.new" "$DEST/$tool"; done
STAGED=()
MADE_DEST=""
echo "install-scanners: installed ${TOOLS[*]} into $DEST"
