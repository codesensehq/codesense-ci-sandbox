// SPDX-License-Identifier: MIT
import type { Extension } from "@codemirror/state";
import type { TagStyle } from "@codemirror/language";
import type { CmModule } from "../cm-loader";

// SLW-WDE-7 — the single codesense syntax-highlight style, shared by the
// editor (`build-theme.ts`) and the diff (`use-code-merge-view.ts`). It
// REPLACES CodeMirror's stock `defaultHighlightStyle` at both sites (RD-1) so
// the read-only editor (WDE-1), the editable editor (WDE-2), and the source
// diff (WD-6 split + inline) share one palette by construction.
//
// Every color is a `var(--cs-code-*)` custom property (defined in
// the active theme). Because the colors are CSS vars,
// a mode or app-theme swap repaints live with no new
// extension (RD-2/AC-2/AC-9).

/** The syntax tokens, by `--cs-code-<name>` palette entry. */
export const SYNTAX_TOKENS = [
  "keyword",
  "string",
  "comment",
  "number",
  "constant",
  "function",
  "variable",
  "type",
  "operator",
  "punctuation",
  "meta",
  "regexp",
] as const;

/**
 * A token's color: an in-diff override first (`--cs-diff-code-*`, set only on
 * changed lines and words — see `build-theme.ts` — so syntax stays legible on the
 * green/red tints), else the palette entry.
 */
function code(name: (typeof SYNTAX_TOKENS)[number]): string {
  return `var(--cs-diff-code-${name}, var(--cs-code-${name}))`;
}

/**
 * Map the common Lezer highlight tags to the codesense `--cs-code-*` palette.
 * Expressed as a function of `tags` (from the lazy CM module) so it stays
 * decoupled from the async loader. Each concrete tag appears in exactly one
 * rule — `HighlightStyle.define` treats a tag→style mapping as single-valued.
 */
export function codeSyntaxSpec(tags: CmModule["tags"]): TagStyle[] {
  return [
    {
      tag: [
        tags.keyword,
        tags.controlKeyword,
        tags.moduleKeyword,
        tags.definitionKeyword,
        tags.operatorKeyword,
        tags.self,
      ],
      color: code("keyword"),
    },
    { tag: [tags.string, tags.docString, tags.character], color: code("string") },
    {
      tag: [tags.comment, tags.lineComment, tags.blockComment, tags.docComment],
      color: code("comment"),
      fontStyle: "italic",
    },
    { tag: [tags.number, tags.integer, tags.float], color: code("number") },
    { tag: [tags.bool, tags.null, tags.atom, tags.literal], color: code("constant") },
    {
      tag: [
        tags.function(tags.variableName),
        tags.function(tags.propertyName),
        tags.labelName,
        tags.macroName,
      ],
      color: code("function"),
    },
    {
      tag: [tags.variableName, tags.propertyName, tags.attributeName],
      color: code("variable"),
    },
    {
      tag: [tags.typeName, tags.className, tags.namespace, tags.tagName],
      color: code("type"),
    },
    { tag: [tags.operator, tags.derefOperator, tags.unit], color: code("operator") },
    {
      tag: [
        tags.punctuation,
        tags.separator,
        tags.bracket,
        tags.angleBracket,
        tags.squareBracket,
        tags.paren,
        tags.brace,
      ],
      color: code("punctuation"),
    },
    {
      tag: [tags.meta, tags.annotation, tags.processingInstruction],
      color: code("meta"),
    },
    { tag: [tags.regexp, tags.escape], color: code("regexp") },
    { tag: tags.heading, color: code("keyword"), fontWeight: "bold" },
    { tag: [tags.link, tags.url], color: code("function") },
    { tag: tags.invalid, color: "var(--destructive)" },
  ];
}

/** Build the codesense `HighlightStyle` from the lazy CM module. */
export function buildCodesenseHighlightStyle(
  cm: CmModule,
): ReturnType<CmModule["HighlightStyle"]["define"]> {
  return cm.HighlightStyle.define(codeSyntaxSpec(cm.tags));
}

/**
 * The shared syntax-highlighting extension. Consumed by `buildCodesenseTheme`
 * (editor) and `useCodeMergeView` (diff) instead of
 * `syntaxHighlighting(defaultHighlightStyle)` — one style, both surfaces.
 */
export function codesenseSyntaxHighlighting(cm: CmModule): Extension {
  return cm.syntaxHighlighting(buildCodesenseHighlightStyle(cm));
}
