// SPDX-License-Identifier: MIT
/**
 * SLW-WDE-1 §B — pure helpers for the in-app read-only file view: decide whether
 * a worktree file is too large to read, whether its content looks binary, and
 * which directories are "noise" (shown but de-emphasized in the tree).
 *
 * Kept pure (no React, no IPC) so the size/binary policy is unit-testable in
 * isolation and the component stays a thin renderer.
 */

/**
 * Files larger than this (bytes) are gated to a placeholder rather than read +
 * rendered. The gate runs on `FileEntry.size` from `list_directory` BEFORE any
 * `readFile`, so a multi-MB file is never pulled over IPC into CodeMirror — the
 * editor inset stays non-blocking (§B). CM6 handles this size comfortably; the
 * cap guards against pathological files, not ordinary source.
 */
export const WORKTREE_MAX_VIEW_BYTES = 1_500_000;

/**
 * Heavy/dependency/build directories: still listed in the tree (so the user can
 * drill in if they must) but de-emphasized and never auto-expanded (AC-2). The
 * Rust `list_directory` drops OS cruft, credential files, and VCS-metadata dirs
 * (`.git`/`.hg`/`.svn`) — but deliberately NOT build/dependency output, so this
 * set is what de-emphasizes those heavy dirs once they reach the tree. Includes
 * the dot-prefixed build/cache dirs (`.next`, `.venv`, …) that dotfile
 * transparency now surfaces: they are output, not curated content, so they read
 * as noise here rather than as prominent rows.
 */
export const WORKTREE_NOISE_DIRS: ReadonlySet<string> = new Set([
  "node_modules",
  "target",
  "dist",
  "build",
  "out",
  "coverage",
  "vendor",
  ".next",
  ".nuxt",
  ".svelte-kit",
  ".turbo",
  ".parcel-cache",
  ".cache",
  ".gradle",
  ".venv",
  ".tox",
  ".mypy_cache",
  ".pytest_cache",
  ".ruff_cache",
]);

/** A directory name that should be de-emphasized in the worktree tree (AC-2). */
export function isNoiseDir(name: string, isDir: boolean): boolean {
  return isDir && WORKTREE_NOISE_DIRS.has(name);
}

/** `true` when a file's byte size exceeds the read threshold — gate it to a
 *  placeholder without reading (§B). `null`/unknown size ⇒ not gated (read and
 *  let the binary check + post-read byte check decide). */
export function isTooLargeToView(size: number | null | undefined): boolean {
  return size != null && size > WORKTREE_MAX_VIEW_BYTES;
}

/**
 * UTF-8 byte length of a string — the post-read size gate for a file whose byte
 * size wasn't known up front (a diff-seeded file, `size: null`). `String.length`
 * counts UTF-16 code units, which UNDER-counts multibyte content vs. the byte
 * budget, so a heavy-CJK file could slip past a char-length check; this measures
 * actual bytes so the gate matches the byte-named threshold.
 */
export function utf8ByteLength(content: string): number {
  return new TextEncoder().encode(content).length;
}

/**
 * Git's binary heuristic mirrored on the FE (see Rust `looks_binary`,
 * `git_sync.rs`): a NUL byte in the first 8 KB ⇒ treat as binary. `read_file`
 * returns lossy UTF-8, but a real NUL (`0x00`) is itself valid UTF-8 and
 * survives the decode as `" "`, so scanning the returned string is a
 * faithful stand-in for scanning the raw bytes.
 */
export function looksBinary(content: string): boolean {
  const n = Math.min(content.length, 8000);
  for (let i = 0; i < n; i++) {
    if (content.charCodeAt(i) === 0) return true;
  }
  return false;
}
