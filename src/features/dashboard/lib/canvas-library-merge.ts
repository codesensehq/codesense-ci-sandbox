// SPDX-License-Identifier: MIT
// Pure helpers for the canvas wireframe library (SW-SC-5; collapsed to one
// team tier by SW-SC-8). Kept free of any `@excalidraw/excalidraw` import so
// they unit-test without the heavy engine chunk.

/** A library item carries a stable id; that's all these helpers need. */
export interface LibraryItemLike {
  id: string;
}

/** Wrap the team bag's raw `.excalidrawlib` JSON as a Blob so Excalidraw's
 *  own loader normalizes both the v1 (`library`) and v2 (`libraryItems`)
 *  envelopes on load — we never parse the format ourselves. */
export function packBlob(content: string): Blob {
  return new Blob([content], { type: "application/json" });
}

/**
 * Serialize the whole library panel to the v2 `.excalidrawlib` envelope the
 * team bag is stored as (SW-SC-8 D3). There is one tier now, so the entire
 * bag is persisted with no provisioned/personal filtering — a native
 * per-item delete sticks because the next write simply drops it. An empty
 * panel serializes to an empty bag (never `null`/delete) so deleting the
 * last item persists rather than orphaning the seed marker.
 */
export function libraryEnvelope<T extends LibraryItemLike>(items: readonly T[]): string {
  return JSON.stringify({ type: "excalidrawlib", version: 2, libraryItems: items });
}
