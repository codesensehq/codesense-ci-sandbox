#!/bin/bash
# SPDX-License-Identifier: MIT
# Fetch the signed alfypops git-ai build before bundling CodeSense.
# Downloads the pinned release asset over HTTPS with no login, then checks its SHA-256 against the pin.
# Usage: scripts/fetch-git-ai.sh [--dry-run]   (--dry-run prints the URL it would fetch and exits)
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
pin="$root/src-tauri/git-ai.pin"
dry_run=0
case "${1:-}" in
  '') ;;
  --dry-run) dry_run=1 ;;
  *) printf 'Usage: %s [--dry-run]\n' "$0" >&2; exit 2 ;;
esac
version='' asset='' platform='' sha256='' repo='' tag=''
while IFS='=' read -r key value; do
  case "$key" in
    ''|'#'*) continue ;;
    version|asset|platform|sha256|repo|tag) printf -v "$key" '%s' "$value" ;;
    *) printf 'Unknown git-ai pin key: %s\n' "$key" >&2; exit 1 ;;
  esac
done < "$pin"
if [[ -z "$version" || -z "$asset" || "$platform" != macos-arm64 || "$repo" != alfypops/git-ai || "$tag" != "v$version" || ! "$sha256" =~ ^[0-9a-f]{64}$ ]]; then
  printf 'Invalid git-ai pin: %s\n' "$pin" >&2
  exit 1
fi
url="https://github.com/$repo/releases/download/$tag/$asset"
if (( dry_run )); then
  printf '%s\n' "$url"
  exit 0
fi
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch" "$root/src-tauri/resources/.git-ai.tmp.$$"' EXIT
# -q ignores ~/.curlrc, and no Authorization header is ever sent, so no credential follows the CDN redirect.
status=0
code="$(curl -q --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --tlsv1.2 \
  --retry 3 --write-out '%{http_code}' --output "$scratch/$asset" "$url")" || status=$?
if (( status != 0 )); then
  # Remove this gh fallback once the git-ai repository is public; the anonymous download then covers everyone.
  if [[ "$code" != 404 && "$code" != 401 ]]; then
    printf 'git-ai download failed (curl exit %s, HTTP %s): %s\n' "$status" "${code:-none}" "$url" >&2
    exit 1
  fi
  if ! command -v gh >/dev/null 2>&1 || ! gh auth status >/dev/null 2>&1; then
    printf 'git-ai release asset not available anonymously (HTTP %s): %s\n' "$code" "$url" >&2
    printf 'If the git-ai repository is still private, install the GitHub CLI, run gh auth login, then retry.\n' >&2
    exit 1
  fi
  printf 'Anonymous download returned HTTP %s; using the logged-in GitHub CLI instead.\n' "$code" >&2
  rm -f "$scratch/$asset"
  gh release download "$tag" --repo "$repo" -p "$asset" --dir "$scratch"
fi
actual="$(shasum -a 256 "$scratch/$asset")"
actual="${actual%% *}"
if [[ "$actual" != "$sha256" ]]; then
  printf 'git-ai SHA-256 mismatch: expected %s, got %s\n' "$sha256" "$actual" >&2
  exit 1
fi
mkdir -p "$root/src-tauri/resources"
# Copy beside the target, then rename, so an interrupted copy never leaves a partial binary in place.
install -m 755 "$scratch/$asset" "$root/src-tauri/resources/.git-ai.tmp.$$"
mv -f "$root/src-tauri/resources/.git-ai.tmp.$$" "$root/src-tauri/resources/git-ai"
printf 'Bundled git-ai %s verified at src-tauri/resources/git-ai\n' "$version"
