// SPDX-License-Identifier: MIT
/**
 * SW-EW-46: the minimum readable cell size. A terminal narrower than this can't hold
 * a shell prompt, and — worse — collapsing a live prompt from full width down to a
 * sliver corrupts it (zsh/starship redraws with relative cursor math that assumes the
 * emulator does NOT reflow, so its rewrite lands on xterm's reflowed lines → scrambled
 * glyphs). Capping the column/row count so cells never go below this floor (the grid
 * WRAPS to more rows instead) keeps the width change benign. ~220px ≈ 30 cols at a 13px
 * mono cell; ~110px ≈ 6 rows.
 */
export const MIN_CELL_WIDTH_PX = 220;
/** SW-UX-CLI-10: a cell's caption bar (`h-6`), above its terminal. */
export const GRID_CAPTION_HEIGHT_PX = 24;
/** The terminal's ~6 rows plus the cell's caption. */
export const MIN_CELL_HEIGHT_PX = 110 + GRID_CAPTION_HEIGHT_PX;

/**
 * SW-UX-CLI-10: whether a `width`×`height` pane can show `cells` side by side
 * with none below the readable floor — the most the floor allows is
 * ⌊w/MIN_W⌋·⌊h/MIN_H⌋ cells, whatever the preset's shape. An unmeasured pane
 * (0) never refuses.
 */
export function gridFitsCells(width: number, height: number, cells: number): boolean {
  if (width <= 0 || height <= 0) return true;
  const cols = Math.max(1, Math.floor(width / MIN_CELL_WIDTH_PX));
  const rows = Math.max(1, Math.floor(height / MIN_CELL_HEIGHT_PX));
  return cells <= cols * rows;
}
