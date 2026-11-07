// SPDX-License-Identifier: MIT
import type { Extension } from "@codemirror/state";
import type { CmModule } from "../cm-loader";
import { codesenseSyntaxHighlighting, SYNTAX_TOKENS } from "./highlight";
import { EDITOR_ACTIVE_LINE_ROLE } from "@/themes/css";

/**
 * Keep `keep`% of every syntax token's colour, the rest the foreground's. The diff's
 * green/red tints lower the contrast of the palette's quieter tokens (comments,
 * light-mode keywords) below WCAG AA; mixing toward the foreground restores it in
 * both themes and under any editor palette, while each token keeps its hue.
 * Measured against the GitHub light/dark palettes: ≥5:1 on changed lines (80%),
 * ≥4.5:1 on changed words (60%, over their stronger tint).
 */
function legibleSyntax(keep: number): Record<string, string> {
  return Object.fromEntries(
    SYNTAX_TOKENS.map((t) => [
      `--cs-diff-code-${t}`,
      `color-mix(in oklab, var(--cs-code-${t}) ${keep}%, var(--foreground))`,
    ]),
  );
}

/**
 * Build a CM6 theme extension wired to codesense's CSS custom properties.
 * The `dark` flag controls CM6's internal dark-mode hint (used by syntax
 * highlighting fallbacks). All color values come from `var(--...)` at
 * runtime, so swapping codesense's theme via `next-themes` automatically
 * repaints the editor without needing a fresh extension.
 *
 * `<CodeEditor>` keeps both light and dark theme extensions in a Compartment
 * and reconfigures it on `resolvedTheme` change (see D-20). The shared
 * builder lives here so the only meaningful difference between
 * `themes/light.ts` and `themes/dark.ts` is the `dark` argument.
 */
export function buildCodesenseTheme(cm: CmModule, dark: boolean): Extension {
  // Inline color tokens that don't map cleanly to codesense's chrome
  // variables — picked to be readable on both themes and overridable by
  // future tokens added to src/index.css.
  const selection = "var(--accent)";
  const lineHighlight = `var(--theme-${EDITOR_ACTIVE_LINE_ROLE.split(".").join("-")})`;

  const baseTheme = cm.EditorView.theme(
    {
      "&": {
        color: "var(--foreground)",
        backgroundColor: "var(--background)",
        height: "100%",
      },
      ".cm-content": {
        caretColor: "var(--foreground)",
        // Font family + size are inline-styled on the editor root from the
        // component (see Task 5.3); they intentionally do NOT live in the
        // theme so the `fontSize` prop wins.
      },
      ".cm-cursor, .cm-dropCursor": {
        borderLeftColor: "var(--foreground)",
      },
      "&.cm-focused .cm-selectionBackground, ::selection, .cm-selectionBackground": {
        backgroundColor: selection,
        // SLW-WDE-7 (AC-4): 0.25 read too faint against the app-token shell in
        // both modes; 0.3 keeps the selection legible without swamping text.
        opacity: "0.3",
      },
      ".cm-activeLine": {
        backgroundColor: lineHighlight,
      },
      ".cm-activeLineGutter": {
        backgroundColor: lineHighlight,
      },
      ".cm-gutters": {
        backgroundColor: "var(--background)",
        color: "var(--muted-foreground, var(--foreground))",
        border: "none",
        borderRight: "1px solid var(--border)",
      },
      ".cm-lineNumbers .cm-gutterElement": {
        padding: "0 8px 0 4px",
      },
      ".cm-scroller": {
        // Family + size set via component inline style; this only sets
        // monospace-friendly line-height.
        lineHeight: "1.5",
      },
      ".cm-placeholder": {
        color: "var(--muted-foreground, var(--foreground))",
        opacity: "0.6",
      },

      // SLW-WDE-7 (AC-4/RD-4): shell-polish states CM6 would otherwise leave
      // default, all from app tokens. Bands use color-mix so the underlying
      // hue stays a single source of truth (light/dark swap repaints live).
      ".cm-matchingBracket": {
        backgroundColor: "color-mix(in srgb, var(--accent) 28%, transparent)",
        outline: "1px solid color-mix(in srgb, var(--accent) 55%, transparent)",
      },
      ".cm-nonmatchingBracket": {
        backgroundColor: "color-mix(in srgb, var(--destructive) 25%, transparent)",
      },
      ".cm-searchMatch": {
        backgroundColor: "color-mix(in srgb, var(--accent) 22%, transparent)",
        outline: "1px solid color-mix(in srgb, var(--accent) 45%, transparent)",
      },
      ".cm-searchMatch.cm-searchMatch-selected": {
        backgroundColor: "color-mix(in srgb, var(--accent) 45%, transparent)",
      },
      ".cm-selectionMatch": {
        backgroundColor: "color-mix(in srgb, var(--accent) 18%, transparent)",
      },
      // The change gutter: a 3px rail beside the line numbers. Bars for added
      // and modified lines, a small wedge where lines were deleted — the same
      // status hues as the source diff, so a mark means the same thing in both.
      ".cm-change-gutter": {
        width: "4px",
        paddingLeft: "1px",
      },
      ".cm-change-gutter .cm-gutterElement": {
        position: "relative",
      },
      ".cm-change-mark": {
        position: "absolute",
        left: "0",
        width: "3px",
        top: "1px",
        bottom: "1px",
        borderRadius: "1px",
      },
      ".cm-change-mark.cm-change-added": {
        backgroundColor: "var(--cs-status-added)",
      },
      ".cm-change-mark.cm-change-modified": {
        backgroundColor: "var(--cs-status-modified)",
      },
      ".cm-change-mark.cm-change-deleted": {
        // A small wedge pointing at the seam where lines were removed.
        top: "-4px",
        bottom: "auto",
        width: "5px",
        height: "8px",
        borderRadius: "0",
        backgroundColor: "var(--cs-status-deleted)",
        clipPath: "polygon(0 0, 100% 50%, 0 100%)",
      },
      // The overview ruler: change marks + the cursor placed along the whole
      // file, laid over the scrollbar lane so it costs no horizontal space.
      ".cm-overview-ruler": {
        position: "absolute",
        top: "0",
        right: "0",
        bottom: "0",
        width: "6px",
        pointerEvents: "none",
        zIndex: "3",
      },
      ".cm-overview-mark": {
        position: "absolute",
        left: "1px",
        right: "1px",
        height: "3px",
        borderRadius: "1px",
      },
      ".cm-overview-mark.cm-change-added": { backgroundColor: "var(--cs-status-added)" },
      ".cm-overview-mark.cm-change-modified": { backgroundColor: "var(--cs-status-modified)" },
      ".cm-overview-mark.cm-change-deleted": { backgroundColor: "var(--cs-status-deleted)" },
      ".cm-overview-cursor": {
        position: "absolute",
        left: "0",
        right: "0",
        height: "2px",
        backgroundColor: "var(--foreground)",
        opacity: "0.45",
      },

      // Find/replace: CodeMirror's stock panel, restyled onto the app's form
      // controls so it reads as part of CodeSense rather than a browser widget.
      ".cm-panels": {
        backgroundColor: "var(--card, var(--background))",
        color: "var(--foreground)",
        // Panel chrome speaks the app's UI face, not the code font.
        fontFamily: "var(--font-sans, ui-sans-serif, system-ui, sans-serif)",
      },
      ".cm-panels.cm-panels-top": {
        borderBottom: "1px solid var(--border)",
      },
      ".cm-panels.cm-panels-bottom": {
        borderTop: "1px solid var(--border)",
      },
      ".cm-search": {
        position: "relative",
        padding: "4px 34px 4px 6px",
        fontSize: "12px",
        lineHeight: "30px",
      },
      ".cm-search > input, .cm-search > button, .cm-search > label": {
        margin: "0 3px",
        verticalAlign: "middle",
      },
      ".cm-textfield": {
        height: "24px",
        minWidth: "180px",
        padding: "0 8px",
        border: "1px solid var(--input, var(--border))",
        borderRadius: "5px",
        backgroundColor: "var(--background)",
        color: "var(--foreground)",
        fontFamily: "inherit",
        fontSize: "12px",
        outline: "none",
      },
      ".cm-textfield:focus": {
        borderColor: "var(--ring)",
        boxShadow: "0 0 0 2px color-mix(in srgb, var(--ring) 25%, transparent)",
      },
      ".cm-button": {
        height: "24px",
        padding: "0 9px",
        border: "1px solid var(--border)",
        borderRadius: "5px",
        backgroundImage: "none",
        backgroundColor: "var(--background)",
        color: "var(--foreground)",
        fontFamily: "inherit",
        fontSize: "12px",
        cursor: "pointer",
        textTransform: "capitalize",
      },
      ".cm-button:hover": {
        backgroundColor: "var(--muted)",
      },
      ".cm-button:active": {
        backgroundImage: "none",
        backgroundColor: "color-mix(in srgb, var(--muted) 70%, var(--foreground) 10%)",
      },
      ".cm-search label": {
        display: "inline-flex",
        alignItems: "center",
        gap: "4px",
        color: "var(--muted-foreground, var(--foreground))",
        fontSize: "12px",
        textTransform: "capitalize",
        cursor: "pointer",
      },
      ".cm-search input[type=checkbox]": {
        accentColor: "var(--accent)",
        margin: "0",
      },
      ".cm-search button[name=close]": {
        position: "absolute",
        top: "6px",
        right: "8px",
        width: "22px",
        height: "22px",
        padding: "0",
        border: "none",
        borderRadius: "5px",
        background: "transparent",
        color: "var(--muted-foreground, var(--foreground))",
        fontSize: "16px",
        lineHeight: "22px",
        cursor: "pointer",
      },
      ".cm-search button[name=close]:hover": {
        backgroundColor: "var(--muted)",
        color: "var(--foreground)",
      },
      // The code editor's find/replace (marked `cs-editor-search` by
      // useCodeEditor): a wrapping flex layout — find row, then replace row —
      // so a narrow tile never strands one checkbox on a line of its own, and
      // sentence-case labels (the editor supplies the phrases). Other hosts of
      // the search panel (the merge view) keep the flow layout above.
      "&.cs-editor-search .cm-search": {
        display: "flex",
        flexWrap: "wrap",
        alignItems: "center",
        gap: "6px",
        padding: "6px 36px 6px 8px",
        lineHeight: "normal",
      },
      "&.cs-editor-search .cm-search > input, &.cs-editor-search .cm-search > button, &.cs-editor-search .cm-search > label":
        { margin: "0", order: "1" },
      "&.cs-editor-search .cm-search > br": { display: "none" },
      // The row break: a full-width flex item between the find and replace rows.
      "&.cs-editor-search .cm-search::after": { content: '""', flexBasis: "100%", order: "2" },
      "&.cs-editor-search .cm-search > [name=replace], &.cs-editor-search .cm-search > [name=replaceAll]":
        { order: "3" },
      "&.cs-editor-search .cm-search .cm-textfield": { flex: "0 1 220px", minWidth: "140px" },
      "&.cs-editor-search .cm-search label": { textTransform: "none" },
      "&.cs-editor-search .cm-search label:first-of-type": { marginLeft: "4px" },
      "&.cs-editor-search .cm-button": { textTransform: "none" },
      ".cm-foldGutter .cm-gutterElement": {
        padding: "0 3px",
        color: "var(--muted-foreground, var(--foreground))",
        opacity: "0.55",
        cursor: "pointer",
      },
      ".cm-foldGutter .cm-gutterElement:hover": {
        opacity: "1",
      },
      ".cm-activeLineGutter .cm-gutterElement, .cm-activeLineGutter": {
        color: "var(--foreground)",
      },

      // SLW-WDE-7 (AC-3/RD-3): @codemirror/merge diff decorations from the git-
      // status tokens. Class names verified against @codemirror/merge 6.12.1.
      // Added/changed lines (both split sides + unified additions) read green;
      // deleted lines/chunks (unified removals) read red; the changed *token*
      // gets a stronger tint. Split shows changed lines on BOTH sides (the lib
      // tints them green here, and the baseline side is re-tinted red below via
      // its `cm-merge-a` editor class); inline unified is add-green / remove-red.
      // `&.cm-merge-b` matches the library's own side-scoped rule, which would
      // otherwise outrank a bare class and keep its stock tint on the right.
      ".cm-changedLine, &.cm-merge-b .cm-changedLine, .cm-insertedLine, .cm-inlineChangedLine": {
        backgroundColor: "color-mix(in srgb, var(--cs-status-added) 10%, transparent)",
      },
      ".cm-changedLine, .cm-insertedLine, .cm-inlineChangedLine, .cm-deletedChunk, .cm-deletedLine":
        legibleSyntax(80),
      ".cm-changedText, .cm-deletedText": legibleSyntax(60),
      // Split view: the left (baseline) editor carries `cm-merge-a`, so its
      // changed lines read as removals — direction from color, not arrows.
      "&.cm-merge-a .cm-changedLine": {
        backgroundColor: "color-mix(in srgb, var(--cs-status-deleted) 10%, transparent)",
      },
      // `background` (not -Color) also clears the library's 2px underline
      // gradient, so a changed word reads as one tinted span.
      // The doubled side class outranks the library's light/dark-scoped
      // underline rules for the split editors.
      ".cm-changedText, &.cm-merge-b.cm-merge-b .cm-changedText": {
        background: "color-mix(in srgb, var(--cs-status-added) 22%, transparent)",
        borderRadius: "2px",
      },
      "&.cm-merge-a.cm-merge-a .cm-changedText": {
        background: "color-mix(in srgb, var(--cs-status-deleted) 22%, transparent)",
      },
      ".cm-changedLineGutter, &.cm-merge-b.cm-merge-b .cm-changedLineGutter, .cm-inlineChangedLineGutter":
        {
          background: "color-mix(in srgb, var(--cs-status-added) 75%, transparent)",
        },
      "&.cm-merge-a .cm-changedLineGutter, .cm-deletedLineGutter": {
        background: "color-mix(in srgb, var(--cs-status-deleted) 75%, transparent)",
      },
      // A unified removal block: one tint on the chunk, none restacked per line.
      ".cm-deletedChunk": {
        backgroundColor: "color-mix(in srgb, var(--cs-status-deleted) 10%, transparent)",
      },
      ".cm-deletedChunk .cm-deletedLine": {
        backgroundColor: "transparent",
      },
      ".cm-deletedLine": {
        backgroundColor: "color-mix(in srgb, var(--cs-status-deleted) 10%, transparent)",
      },
      ".cm-deletedChunk .cm-deletedText, .cm-deletedText": {
        background: "color-mix(in srgb, var(--cs-status-deleted) 22%, transparent)",
        borderRadius: "2px",
      },
      ".cm-collapsedLines": {
        background: "color-mix(in srgb, var(--muted) 65%, transparent)",
        color: "var(--muted-foreground, var(--foreground))",
        borderBlock: "1px solid color-mix(in srgb, var(--border) 70%, transparent)",
        fontSize: "0.85em",
      },
      ".cm-collapsedLines:hover": {
        color: "var(--foreground)",
      },
    },
    { dark },
  );

  // SLW-WDE-7 (RD-1/AC-1): the shared codesense syntax palette, REPLACING
  // CodeMirror's stock `defaultHighlightStyle`. `useCodeMergeView` consumes the
  // same extension, so editor and diff highlight identically by construction.
  return [baseTheme, codesenseSyntaxHighlighting(cm)];
}
