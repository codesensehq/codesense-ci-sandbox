// SPDX-License-Identifier: MIT
/**
 * SW-EW-35: the 2-char source-repo glyph chip (multi-repo teams). Originally
 * shared by the `WorkspacePill` and the split-tile `TerminalTileChrome`;
 * SLW-NC-1 Phase 2 adds a third consumer in a different feature (the installed
 * group header in the dashboard pipeline nav), so this pure presentational leaf
 * was promoted from `features/viewer/components/` to `shared/components/`.
 *
 * Renders nothing when `glyph` is empty. `glyphColor` (the SW-EW-1 AC-9
 * identity-color primitive, from `getRepoColor`) tints the text; omitted falls
 * back to the chrome default. `testId` is optional — consumers that assert on
 * it pass one.
 */
export function RepoGlyph({
  glyph,
  glyphColor,
  testId,
}: {
  glyph: string | undefined;
  glyphColor?: string;
  /** Accepted for caller ergonomics (the full `owner/repo`); no longer rendered
   *  as a native hover title. */
  slug?: string;
  testId?: string;
}) {
  if (!glyph) return null;
  return (
    <span
      {...(testId ? { "data-testid": testId } : {})}
      style={{ fontSize: "0.7em", ...(glyphColor ? { color: glyphColor } : {}) }}
      className="flex-shrink-0 font-mono opacity-70 px-1 py-px rounded bg-muted leading-none"
    >
      {glyph}
    </span>
  );
}
