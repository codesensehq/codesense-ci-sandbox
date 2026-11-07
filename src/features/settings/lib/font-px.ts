// SPDX-License-Identifier: MIT
import { clampFontPx } from "@/shared/lib/font-sizes";

/**
 * Compute the value to write to `*FontSizePx` from a user-typed px input.
 *
 * Returns:
 *  - `null` when the input is empty, not a positive integer, or otherwise
 *    malformed — caller skips the write and typically resets the input.
 *    Explicitly rejects decimals, negatives, leading signs, and anything
 *    that isn't plain digits so "14.5" and "-5" don't silently truncate.
 *  - `0` when the clamped value equals `canonicalPresetPx` — clears the
 *    override so the preset button is the single source of truth and
 *    re-clicking the preset doesn't visually no-op.
 *  - The clamped px value otherwise.
 *
 * Used by all three font-size rows (viewer, tree, terminal) so the
 * "typed-equals-preset clears override" rule lives in one place.
 */
export function commitFontPxOverride(typedRaw: string, canonicalPresetPx: number): number | null {
  const trimmed = typedRaw.trim();
  if (trimmed.length === 0) return null;
  // Positive integer only — digits with no sign, no decimal, no whitespace.
  if (!/^\d+$/.test(trimmed)) return null;
  const n = Number(trimmed);
  if (!Number.isFinite(n) || !Number.isInteger(n) || n <= 0) return null;
  const clamped = clampFontPx(n);
  return clamped === canonicalPresetPx ? 0 : clamped;
}
