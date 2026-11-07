// SPDX-License-Identifier: MIT
import { applyMetadataEdit } from "@/tauri/commands";

/**
 * Write a story's `points` estimate to its frontmatter (SW-SB-6 AC-8).
 *
 * This is NOT a new write path — it is a thin, typed, mockable wrapper over the
 * shipped `apply_metadata_edit` command (the exact seam the metadata band uses).
 * The Rust side runs the read-modify-write on the blocking pool (`await_blocking`)
 * and writes format-preserving + watcher-suppressed, so the renderer never blocks
 * and the rest of the frontmatter block is byte-identical. Frontmatter scalars are
 * strings, so the numeric estimate is stringified for the `SetField` edit.
 *
 * Resolves/rejects with the write's own outcome so the caller keeps its optimistic
 * revert + toast handling (mirrors `writePipelineStatus`).
 */
export function writeStoryPoints(filePath: string, points: number): Promise<void> {
  return applyMetadataEdit(filePath, [
    { kind: "setField", field: "points", value: String(points) },
  ]);
}

/**
 * Set a story's `sprint` tag (SW-SB-15). Same thin, mockable wrapper over
 * `apply_metadata_edit` as {@link writeStoryPoints}. `sprint` is a plain string
 * field (an opaque tag like `"2026-08-A"`), so — unlike `points` — there is no
 * numeric-quoting hazard.
 */
export function writeStorySprint(filePath: string, sprintId: string): Promise<void> {
  return applyMetadataEdit(filePath, [{ kind: "setField", field: "sprint", value: sprintId }]);
}

/**
 * Remove a story's `sprint` tag (SW-SB-15) — the inverse of {@link writeStorySprint}.
 * `ClearField` drops the key entirely rather than blanking it.
 */
export function clearStorySprint(filePath: string): Promise<void> {
  return applyMetadataEdit(filePath, [{ kind: "clearField", field: "sprint" }]);
}
