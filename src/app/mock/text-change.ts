// SPDX-License-Identifier: MIT
import { applyPatch, reversePatch, structuredPatch } from "diff";

type Overlap = { ours: string; theirs: string; at: number };

/** Apply a single edit to the current body; undo refuses conflicts, saves keep both sides. */
export function applyMockTextChange(
  before: string,
  after: string,
  current: string,
  undo = false,
): { body: string; overlaps: Overlap[] } {
  const base = undo ? after : before;
  const target = undo ? before : after;
  if (current === target || target === base) return { body: current, overlaps: [] };
  if (current === base) return { body: target, overlaps: [] };
  // Unrelated adjacent lines must not become required patch context.
  const patch = structuredPatch("body", "body", before, after, "", "", { context: 0 });
  // With competing edits, an empty side means overlapping whole-text insertion or removal.
  const body =
    undo && (!base || !current || !target)
      ? false
      : applyPatch(current, undo ? reversePatch(patch) : patch);
  if (body !== false) return { body, overlaps: [] };
  if (undo) throw new Error("later changes overlap this revision; restore a whole version instead");
  return {
    body: `${current}${current.endsWith("\n") ? "" : "\n"}${after}`,
    overlaps: [{ ours: after, theirs: current, at: 1 }],
  };
}
