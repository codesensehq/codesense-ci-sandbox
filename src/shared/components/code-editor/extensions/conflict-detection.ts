// SPDX-License-Identifier: MIT
// Conflict detection is fully imperative (D-6). There is no CM6 extension
// here — the editor's `checkConflict(diskVersion)` method on `CodeEditorHandle`
// is a pure string compare against the current editor content. The host
// owns the watcher subscription, decodes disk bytes, calls this method, and
// renders any banner/merge UX itself.
//
// This module exists as the named home for the comparison helper, so future
// extensions to the conflict story (multi-way diff hints, line-number
// anchors, etc.) have a clear file to land in.

/**
 * True IFF `diskVersion` differs from `localVersion`. Exact string compare —
 * line-ending normalization, BOM stripping, and trailing-whitespace
 * canonicalization are explicitly NOT performed here. If the host wants
 * fuzzy matching, the host normalizes before calling.
 */
export function hasConflict(localVersion: string, diskVersion: string): boolean {
  return diskVersion !== localVersion;
}
