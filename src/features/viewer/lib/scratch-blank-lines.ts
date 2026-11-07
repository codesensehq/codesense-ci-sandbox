// SPDX-License-Identifier: MIT
// SW-SN-8 AC-8: blank-line preservation for markdown-stored scratch notes.
//
// SN-1 flipped scratch-note storage from HTML to markdown (`getMarkdown()`).
// The `tiptap-markdown` serializer DROPS empty paragraphs entirely (verified:
// `getMarkdown()` of `<p>a</p><p></p><p></p><p>b</p>` returns `"a\n\nb"`), so
// intentional vertical spacing — the empty paragraphs the user typed for
// breathing room — is lost on the save→reload round-trip (SN-1 shipped this as a
// documented lossy interim). Under HTML storage those empty paragraphs survived
// as empty `<p>`.
//
// Fix, at the DOCUMENT level (not the serialized string — the string never
// contains the blank lines, since the serializer already dropped them): before
// serializing, replace each empty paragraph with a paragraph containing a single
// non-breaking space. An NBSP paragraph is not "empty", so the serializer emits
// it as a line and the markdown parser reconstructs it on load — the round-trip
// is stable and idempotent, and it renders as a blank line. The patched doc is
// fed through the editor's OWN markdown serializer, so all other serialization
// (options, escaping, code fences) is byte-identical to `getMarkdown()`.
//
// That serialize-OUT half only self-heals a note that stays inside the editor's
// save path. The instant a note is written as plain markdown by ANOTHER writer
// (an agent editing `{id}.md` directly, an external edit, or a
// body authored before this fix), its blank lines are real empty lines with no
// NBSP. On the next load `tiptap-markdown` (markdown-it) DROPS those empty
// paragraphs on the way IN, the live doc no longer has them, and the following
// save bakes the loss in permanently. `restoreBlankParagraphs` is the missing
// parse-IN counterpart: it rewrites surplus blank lines in a raw markdown STRING
// into NBSP lines BEFORE the string reaches the editor, so blank-line spacing
// survives regardless of which writer produced the file.

import { Fragment, type Node as PMNode, type Schema } from "@tiptap/pm/model";

const NBSP = "\u00A0";

const isEmptyParagraph = (node: PMNode): boolean =>
  node.type.name === "paragraph" && node.content.size === 0;

/**
 * Return a copy of `doc` in which every top-level empty paragraph — EXCEPT a
 * single trailing one — carries a single non-breaking space, so the markdown
 * serializer preserves it as a blank line instead of dropping it.
 *
 * Scope:
 *  - Top-level only — an empty paragraph nested in a list item or blockquote is
 *    structural, so we never recurse (no stray ` ` inside those).
 *  - Leading and interior empty paragraphs are always preserved (deliberate
 *    vertical spacing at the top of / between content).
 *  - A single TRAILING empty paragraph collapses — it's the editor's fresh
 *    cursor line, not intentional spacing, so notes don't accumulate a
 *    persistent trailing NBSP as you type. Two+ trailing empties keep all but
 *    that last one.
 * Code blocks are untouched — their blank lines live inside a text node.
 *
 * Pure: does not mutate the input. Idempotent — an NBSP paragraph is non-empty,
 * so a second pass leaves it unchanged.
 */
export function withPreservedBlankParagraphs(doc: PMNode, schema: Schema): PMNode {
  const children: PMNode[] = [];
  doc.forEach((child) => children.push(child));
  const lastIdx = children.length - 1;

  // Preserve every top-level empty paragraph EXCEPT a single trailing one (the
  // editor's fresh cursor line). Leading and interior blanks are intentional
  // spacing; two+ trailing blanks keep all but the last.
  const preserved = children.map((child, i) =>
    isEmptyParagraph(child) && i !== lastIdx ? child.copy(Fragment.from(schema.text(NBSP))) : child,
  );
  return doc.copy(Fragment.fromArray(preserved));
}

/** A line markdown treats as blank (empty, or only spaces/tabs — a trailing CRLF
 *  `\r` is tolerated so the fix isn't inert on external-writer content). An NBSP
 *  line is NOT blank — it's a preserved paragraph — so this test excludes ` `,
 *  which keeps `restoreBlankParagraphs` idempotent. */
const isBlankLine = (line: string): boolean => /^[ \t]*\r?$/.test(line);

/** The fence marker (```` ``` ````, `~~~`, or longer) if `line` OPENS a fenced
 *  code block: up to 3 spaces of indent (4+ is an indented code block, not a
 *  fence — CommonMark), then 3+ of the same char. Returns the marker so the
 *  closer can be length-checked; null otherwise. */
const fenceOpener = (line: string): string | null => {
  const m = line.match(/^ {0,3}(`{3,}|~{3,})/);
  return m ? m[1] : null;
};

/** Whether `line` CLOSES an open fence opened with `open`. Per CommonMark a
 *  closing fence is the same char, at least as long as the opener, indented ≤3,
 *  and carries no info string (only trailing whitespace). */
const closesFence = (line: string, open: string): boolean => {
  const m = line.match(/^ {0,3}(`{3,}|~{3,})[ \t]*\r?$/);
  return m !== null && m[1][0] === open[0] && m[1].length >= open.length;
};

/** Whether `line` starts an ORDINARY, top-level paragraph — not a markdown block
 *  construct (indented content, blockquote, ATX heading, list item, table row,
 *  thematic break, or code fence). Only blank runs flanked by plain paragraph
 *  lines on BOTH sides are expanded into NBSP paragraphs; a run adjacent to a
 *  structural block is collapsed to a single separator, so lists/quotes/code are
 *  never split by an injected column-0 NBSP. This is the parse-in mirror of the
 *  top-level-only scope of `withPreservedBlankParagraphs`.
 *
 *  ANY leading whitespace disqualifies a line: a 1–3-space indent is a list-item
 *  continuation paragraph (injecting a column-0 NBSP there terminates the list
 *  and ejects its tail), and 4+ is an indented code block. Conservative by
 *  design — a false negative just leaves a blank unpreserved (pre-existing
 *  behavior), never corrupts structure. */
const isPlainParagraphLine = (line: string): boolean => {
  const l = line.replace(/\r$/, "");
  if (l.trim() === "") return false;
  if (/^[ \t]/.test(l)) return false; // any indentation → list continuation / indented code
  if (fenceOpener(l) !== null) return false; // code fence (with or without an info string)
  if (/^(?:>|#{1,6}\s|[-*+]\s|\d+[.)]\s|\|)/.test(l)) return false; // quote/heading/list/table
  if (/^(?:-{3,}|\*{3,}|_{3,})[ \t]*$/.test(l)) return false; // thematic break
  return true;
};

/**
 * Rewrite a raw markdown STRING so surplus blank lines survive the load into the
 * editor as visible blank lines. The parse-IN counterpart to
 * `serializeWithBlankLines`: a run of N consecutive blank lines BETWEEN two
 * ordinary paragraph lines encodes 1 separator plus (N-1) empty paragraphs, so
 * each of those (N-1) surplus blanks is emitted as an NBSP line — exactly the
 * shape the serialize-out path produces. A single blank line (a normal
 * separator) is left untouched, so ordinary markdown is unchanged.
 *
 * Structure-safe: a blank run adjacent to a list, blockquote, heading, table,
 * indented/fenced code — or at the start/end of the note — is collapsed to a
 * single separator rather than expanded, so those blocks are never split by an
 * injected NBSP paragraph. Fenced code blocks pass through verbatim.
 *
 * Pure and idempotent (existing NBSP lines are not treated as blank, so a second
 * pass is a no-op).
 */
export function restoreBlankParagraphs(md: string): string {
  if (!md.includes("\n")) return md;

  const out: string[] = [];
  let openFence: string | null = null;
  let blankRun = 0;
  let prevLine: string | null = null;

  // Flush an accumulated run of blank lines. The first is the normal paragraph
  // separator; the surplus (N-1) become NBSP paragraphs ONLY when the run sits
  // between two plain paragraph lines — otherwise a structural block would split.
  const flush = (nextLine: string | null) => {
    if (blankRun === 0) return;
    out.push("");
    const expand =
      prevLine !== null &&
      nextLine !== null &&
      isPlainParagraphLine(prevLine) &&
      isPlainParagraphLine(nextLine);
    if (expand) {
      for (let j = 1; j < blankRun; j++) {
        out.push(NBSP);
        out.push("");
      }
    }
    blankRun = 0;
  };

  for (const line of md.split("\n")) {
    if (openFence) {
      out.push(line);
      if (closesFence(line, openFence)) openFence = null;
      prevLine = line;
      continue;
    }
    const opener = fenceOpener(line);
    if (opener) {
      flush(line);
      openFence = opener;
      out.push(line);
      prevLine = line;
      continue;
    }
    if (isBlankLine(line)) {
      blankRun++;
      continue;
    }
    flush(line);
    out.push(line);
    prevLine = line;
  }
  flush(null);
  return out.join("\n");
}

/** Minimal shape of the `tiptap-markdown` serializer stored on the editor. */
export interface MarkdownDocSerializer {
  serialize(content: PMNode): string;
}

/**
 * Serialize a ProseMirror doc to markdown with empty paragraphs preserved as
 * blank lines. Uses the editor's own `tiptap-markdown` serializer so output is
 * identical to `getMarkdown()` except for the preserved blank lines.
 */
export function serializeWithBlankLines(
  doc: PMNode,
  schema: Schema,
  serializer: MarkdownDocSerializer,
): string {
  // A note with no meaningful text (fresh/cleared, or only blank/whitespace/NBSP
  // paragraphs left after deleting the text) must round-trip to "" — so the
  // placeholder shows and no invisible, hard-to-delete NBSP lingers. We strip
  // whitespace AND NBSP before testing, since a prior save may have left NBSP
  // sentinel paragraphs that `textContent` would otherwise count as content.
  //
  // But only collapse when the doc is nothing but paragraphs: a code block (or
  // list/heading) is structure, not blank \u2014 a whitespace-only code block must be
  // preserved, never dropped as "empty".
  const hasNoMeaningfulText = doc.textContent.replace(/[\s\u00A0]+/g, "").length === 0;
  let onlyParagraphs = true;
  doc.forEach((child) => {
    if (child.type.name !== "paragraph") onlyParagraphs = false;
  });
  if (hasNoMeaningfulText && onlyParagraphs) return "";
  return serializer.serialize(withPreservedBlankParagraphs(doc, schema));
}
