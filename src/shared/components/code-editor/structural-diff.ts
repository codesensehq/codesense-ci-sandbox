// SPDX-License-Identifier: MIT
/**
 * SLW-DV-8b (AC-1) — structural (parse-tree) diff for the review merge view.
 *
 * The idea is the DV-8 `whitespace-diff.ts` pattern with a smarter projection:
 * instead of diffing whitespace-normalized characters, we diff the sequence of
 * *parse-tree leaf tokens*. Whitespace, indentation, and line breaks are not
 * tokens in a Lezer tree, so two versions that differ only in formatting produce
 * the same token sequence and the diff comes back empty — a formatting-only reflow
 * disappears and the remaining changes align by structure.
 *
 * The output is CM's `Change` shape in ORIGINAL document coordinates: each changed
 * token run is mapped back to its real `from`/`to` offsets so highlights land on
 * the real text. Like `whitespace-diff.ts`, this module is kept free of any
 * `@codemirror/*` or DOM runtime import (Lezer `Tree` is a type-only import) so the
 * projection + remap are unit-testable in isolation and safe to run inside a Web
 * Worker; the array-diff function is injected (jsdiff `diffArrays`).
 *
 * Parsing (which needs the language grammar) lives in `structural-diff-parsers.ts`
 * and runs inside `structural-diff.worker.ts`, never on the render thread — a
 * structural parse is too heavy to be a synchronous `diffConfig.override` (AC-3).
 */

import type { Tree } from "@lezer/common";

import type { LanguageId } from "./cm-loader";
import type { OffsetChange } from "./whitespace-diff";

// ---------------------------------------------------------------------------
// Language support
// ---------------------------------------------------------------------------

/**
 * The languages with a real nested Lezer parse tree where a structural diff is
 * meaningful. `rust`/`shell`/`xml` are flat `StreamLanguage` grammars (no tree),
 * `plain` has none, and `markdown` never reaches the code diff (`RedlineProse`
 * owns it) — so structural applies to exactly this set and everything else falls
 * back to the line diff. One owner of this rule (DRY): the toggle gate and the
 * worker both read it through `isStructuralLanguage`.
 */
const STRUCTURAL_LANGUAGES: ReadonlySet<LanguageId> = new Set<LanguageId>([
  "typescript", // covers .ts + .tsx
  "javascript", // covers .js + .jsx
  "json",
  "yaml",
  "python",
]);

export function isStructuralLanguage(language: LanguageId): boolean {
  return STRUCTURAL_LANGUAGES.has(language);
}

// ---------------------------------------------------------------------------
// Projection: parse tree → token stream
// ---------------------------------------------------------------------------

/** One parse-tree leaf token: its text (the diff signature) + real offsets. */
export interface StructuralToken {
  sig: string;
  from: number;
  to: number;
}

/**
 * Walk a Lezer tree to its leaf tokens, in document order. Whitespace and
 * indentation are not tokens, so they never appear here — that is what makes the
 * diff formatting-insensitive. Zero-width nodes (error/skipped) are dropped.
 * `sliceDoc` reads the original text for a leaf's `[from, to)` span.
 */
export function treeToStructuralTokens(
  tree: Tree,
  sliceDoc: (from: number, to: number) => string,
): StructuralToken[] {
  const tokens: StructuralToken[] = [];
  const cursor = tree.cursor();
  for (;;) {
    // Descend to the leftmost leaf.
    if (cursor.firstChild()) continue;
    if (cursor.to > cursor.from) {
      tokens.push({ sig: sliceDoc(cursor.from, cursor.to), from: cursor.from, to: cursor.to });
    }
    // Ascend until a next sibling exists; if we unwind past the root, we're done.
    for (;;) {
      if (cursor.nextSibling()) break;
      if (!cursor.parent()) return tokens;
    }
  }
}

// ---------------------------------------------------------------------------
// Diff: token stream → OffsetChange[]
// ---------------------------------------------------------------------------

/** One part of a jsdiff `diffArrays` result (the subset this module reads). */
export interface ArrayDiffPart {
  value: string[];
  added?: boolean;
  removed?: boolean;
}

interface Region {
  fromA: number;
  toA: number;
  fromB: number;
  toB: number;
  sawA: boolean;
  sawB: boolean;
}

/**
 * Diff two token streams and return the changed regions in ORIGINAL document
 * coordinates. `diffArraysFn` is jsdiff's `diffArrays` (injected so this stays
 * pure). Consecutive removed/added runs between two unchanged runs collapse into
 * one `OffsetChange` (A-span = the removed tokens, B-span = the added tokens); a
 * pure insertion gets an empty A-span at the current A cursor, a pure deletion an
 * empty B-span at the current B cursor — CM's `Change` semantics. The regions are
 * emitted left-to-right, so they are ordered and non-overlapping in both docs.
 */
export function structuralOffsetChanges(
  tokensA: StructuralToken[],
  tokensB: StructuralToken[],
  diffArraysFn: (a: string[], b: string[]) => ArrayDiffPart[],
): OffsetChange[] {
  const parts = diffArraysFn(
    tokensA.map((t) => t.sig),
    tokensB.map((t) => t.sig),
  );

  const changes: OffsetChange[] = [];
  let iA = 0;
  let iB = 0;
  // Char offset just past the last consumed unchanged token on each side — the
  // anchor for an empty (insertion/deletion) span.
  let posA = 0;
  let posB = 0;
  let region: Region | null = null;

  const flush = (): void => {
    if (!region) return;
    changes.push({
      fromA: region.sawA ? region.fromA : posA,
      toA: region.sawA ? region.toA : posA,
      fromB: region.sawB ? region.fromB : posB,
      toB: region.sawB ? region.toB : posB,
    });
    region = null;
  };

  for (const part of parts) {
    const n = part.value.length;
    if (!part.added && !part.removed) {
      flush();
      iA += n;
      iB += n;
      if (iA > 0) posA = tokensA[iA - 1].to;
      if (iB > 0) posB = tokensB[iB - 1].to;
    } else if (part.removed) {
      const from = tokensA[iA]?.from ?? posA;
      const to = tokensA[iA + n - 1]?.to ?? posA;
      if (!region)
        region = { fromA: from, toA: to, fromB: posB, toB: posB, sawA: true, sawB: false };
      else {
        if (!region.sawA) region.fromA = from;
        region.toA = to;
        region.sawA = true;
      }
      iA += n;
    } else {
      const from = tokensB[iB]?.from ?? posB;
      const to = tokensB[iB + n - 1]?.to ?? posB;
      if (!region)
        region = { fromA: posA, toA: posA, fromB: from, toB: to, sawA: false, sawB: true };
      else {
        if (!region.sawB) region.fromB = from;
        region.toB = to;
        region.sawB = true;
      }
      iB += n;
    }
  }
  flush();
  return changes;
}

// ---------------------------------------------------------------------------
// Worker message contract (structured-clones cleanly both directions)
// ---------------------------------------------------------------------------

export interface StructuralDiffRequest {
  requestId: number;
  before: string;
  after: string;
  language: LanguageId;
}

export type StructuralDiffResponse =
  | { requestId: number; ok: true; changes: OffsetChange[] }
  | { requestId: number; ok: false; error: string };
