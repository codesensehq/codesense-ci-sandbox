// SPDX-License-Identifier: MIT
/**
 * Computes a clamped ratio during drag-resize operations.
 *
 * Works for any axis (horizontal or vertical). Callers pass the
 * appropriate positional coordinates and container dimension.
 *
 * @param startRatio   - ratio at drag start (0-1)
 * @param startPos     - cursor position at drag start (px)
 * @param currentPos   - current cursor position (px)
 * @param containerSize - container dimension along the drag axis (px)
 * @param minPx        - minimum size in pixels for the ratio side (default 80)
 * @param invertDelta  - flip delta sign: dragging toward lower coords increases ratio (default false)
 * @param maxRatio     - optional upper bound override (defaults to `1 - minPx/containerSize`)
 */
export function computeClampedRatio(
  startRatio: number,
  startPos: number,
  currentPos: number,
  containerSize: number,
  minPx = 80,
  invertDelta = false,
  maxRatio?: number,
): number {
  if (containerSize <= 0) return startRatio;

  const rawDelta = (currentPos - startPos) / containerSize;
  const deltaRatio = invertDelta ? -rawDelta : rawDelta;
  const newRatio = startRatio + deltaRatio;

  const minR = minPx / containerSize;
  const maxR = maxRatio ?? 1 - minR;
  // When container is too small for both minimums, lock at 50%
  if (minR >= maxR) return 0.5;
  return Math.max(minR, Math.min(newRatio, maxR));
}

/**
 * Migrates a persisted scratch-note dimension to a ratio (0-1).
 *
 * Handles: legacy pixel widths (> 1), already-migrated ratios,
 * NaN, Infinity, non-numbers, and missing values.
 *
 * @param raw - the persisted value (pixel width or ratio)
 * @param viewportWidth - current viewport width for pixel→ratio conversion
 */
export function migrateScratchNoteRatio(raw: unknown, viewportWidth: number): number {
  if (typeof raw !== "number" || !Number.isFinite(raw)) return 0.33;
  // Legacy pixel value (> 1) → convert to ratio
  if (raw > 1) {
    return Math.max(0.15, Math.min(raw / viewportWidth, 0.5));
  }
  // Already a ratio
  return Math.max(0.15, Math.min(raw, 0.5));
}
