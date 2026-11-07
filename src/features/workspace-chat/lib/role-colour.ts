// SPDX-License-Identifier: MIT
/**
 * SW-WC-3 — stable role chroma per sender.
 *
 * The chat engine stamps `from` + `fromName` but has no `role` field (the
 * agent-vs-human branch is `from == "human"`). The mocks use six role-tinted
 * swatches (triager, builder, reviewer, scout, resolver, analyst); here we
 * derive one of them deterministically from the handle so a session always
 * gets the same colour across sessions and reloads without carrying a role on
 * the message.
 *
 * This is a visual-only mapping — never used for authorization or routing.
 */

const PALETTE: readonly string[] = [
  "#6ea8f0", // triager-ish blue
  "#4ec28a", // builder-ish green
  "#a898ff", // reviewer-ish violet
  "#e0ac55", // scout-ish amber
  "#ea6a5a", // resolver-ish coral
  "#5ec6dc", // analyst-ish cyan
];

/** FNV-1a 32-bit hash of the handle — fast, deterministic, no deps. */
function hashHandle(handle: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < handle.length; i++) {
    h ^= handle.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/** Pick a stable role colour from the handle. */
export function colourForHandle(handle: string): string {
  if (!handle) return PALETTE[0];
  return PALETTE[hashHandle(handle) % PALETTE.length];
}

/** The initial glyph the avatar carries — first char of handle/fromName,
 *  uppercased, with a stable fallback for empty strings. */
export function initialForHandle(handle: string, fromName?: string | null): string {
  const source = (fromName && fromName.trim()) || handle || "·";
  const first = source.trim().charAt(0);
  return first ? first.toUpperCase() : "·";
}
