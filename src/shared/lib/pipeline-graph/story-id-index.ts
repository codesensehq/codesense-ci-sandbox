// SPDX-License-Identifier: MIT
/**
 * SW-SB-9 — location-independent dependency resolution.
 *
 * A `story_id` is a stable, directory-independent alias for a story. Resolving
 * `depends_on` by story_id lets a story reference any other regardless of where
 * either sits in the nav tree — so reorganizing the left nav never silently
 * breaks the dependency DAG.
 *
 * This is the shared primitive the TS pipeline-graph edge resolver
 * (`derived-tree-to-react-flow.ts`) consumes for `depends_on`. The Rust
 * relationship resolver (`context_query/relations.rs`) carries the same logic; the
 * same cross-dir / move / duplicate-poison scenarios are unit-tested on both
 * sides.
 *
 * Uniqueness is load-bearing: global resolution is only sound if a story_id maps
 * to exactly ONE story. A story_id claimed by ≥2 stories is POISONED — stored as
 * `null` and resolved as "unresolved" — so a collision can never silently pick an
 * arbitrary target (the drift itself is surfaced by `validate_story`'s
 * `DUPLICATE_STORY_ID`). This mirrors the `stemToFileName` null-poison already
 * used for ambiguous filename stems in the graph resolver.
 */

/**
 * Build a `story_id → value` index, poisoning any id claimed more than once.
 * Entries with an empty/absent id are skipped. The second (and later) claim of
 * an id overwrites its value with `null` (the poison marker).
 */
export function buildStoryIdIndex<T>(
  entries: Iterable<readonly [string | undefined | null, T]>,
): Map<string, T | null> {
  const index = new Map<string, T | null>();
  for (const [id, value] of entries) {
    if (!id) continue;
    // First claim wins the slot; any later claim poisons it (ambiguous → null).
    index.set(id, index.has(id) ? null : value);
  }
  return index;
}

/**
 * Resolve a bare `depends_on` ref against the story_id index. Returns the target
 * only on a UNIQUE hit. Returns `undefined` when the ref is not a known story_id
 * OR the id is poisoned (ambiguous) — in both cases the caller falls through to
 * path/stem resolution (or records the edge unresolved).
 */
export function resolveStoryIdRef<T>(index: Map<string, T | null>, ref: string): T | undefined {
  const hit = index.get(ref);
  // `null` (poisoned) and `undefined` (absent) both mean "do not resolve here".
  return hit == null ? undefined : hit;
}
