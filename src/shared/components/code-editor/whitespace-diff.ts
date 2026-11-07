// SPDX-License-Identifier: MIT
/**
 * SLW-DV-8 (AC-1) — whitespace-insensitive diff for the review merge view.
 *
 * `@codemirror/merge` has no native whitespace-insensitive mode, so this builds
 * a `diffConfig.override`: diff each side over a whitespace-normalized image, then
 * remap the resulting change offsets back to the ORIGINAL document coordinates so
 * highlights land on the real text. The override is synchronous (CM calls it on
 * the render thread) — this is the same order of cost as the line diff CM runs
 * today, so no worker is involved. Because CM bypasses its own `scanLimit`/
 * `timeout` once an override is set, the caller passes a bound in `config` and it
 * is forwarded to the underlying `diff`.
 *
 * Kept free of any `@codemirror/*` import so the mapping is unit-testable in
 * isolation: the CM `diff` function and a `Change` factory are injected.
 */

/** The offsets of one changed range — CM's `Change` shape (doc-A / doc-B spans). */
export interface OffsetChange {
  fromA: number;
  toA: number;
  fromB: number;
  toB: number;
}

/** The compute bound forwarded to the underlying diff (CM's `DiffConfig` subset). */
export interface DiffBound {
  scanLimit?: number;
  timeout?: number;
}

/**
 * A whitespace-normalized image of a source string. `text` drops every
 * whitespace character EXCEPT newlines (line structure is preserved, so a moved
 * word still reads as a change while a pure reindent does not). `map` translates
 * a normalized offset back to the original: `map[i]` is the original index of the
 * i-th normalized char, and `map[text.length]` is the original length — so an
 * end-exclusive offset equal to `text.length` maps correctly.
 */
export interface NormalizedImage {
  text: string;
  map: number[];
}

// Any whitespace character. Newlines are handled separately (kept) so line
// structure survives; every other whitespace run — spaces, tabs, `\r`, and
// Unicode spaces such as U+00A0 (NBSP) — is dropped so it never reads as a change.
const WHITESPACE_RE = /\s/;

/**
 * Strip intra-line whitespace, keep newlines, and record the original index of
 * every retained character. Any Unicode whitespace except `\n` is dropped, so
 * CRLF vs LF and a space→NBSP swap never read as a change.
 */
export function normalizeIgnoringWhitespace(src: string): NormalizedImage {
  let text = "";
  const map: number[] = [];
  for (let i = 0; i < src.length; i++) {
    const ch = src[i];
    if (ch === "\n") {
      text += ch;
      map.push(i);
    } else if (WHITESPACE_RE.test(ch)) {
      // dropped — the whole point of "ignore whitespace"
    } else {
      text += ch;
      map.push(i);
    }
  }
  map.push(src.length);
  return { text, map };
}

/** Remap one change from normalized coordinates back to original coordinates. */
export function remapChange(c: OffsetChange, mapA: number[], mapB: number[]): OffsetChange {
  return { fromA: mapA[c.fromA], toA: mapA[c.toA], fromB: mapB[c.fromB], toB: mapB[c.toB] };
}

/**
 * Build a `diffConfig.override`: `(a, b) => Change[]` that ignores whitespace-only
 * differences. `diffFn` is `@codemirror/merge`'s exported `diff`; `makeChange`
 * constructs a real `Change` (so CM's `makePresentable`, which still runs over our
 * output, sees the type it expects); `config` carries the `scanLimit`/`timeout`
 * bound the override must re-apply on its own behalf.
 */
export function createIgnoreWhitespaceOverride<C>(
  diffFn: (a: string, b: string, config?: DiffBound) => readonly OffsetChange[],
  makeChange: (fromA: number, toA: number, fromB: number, toB: number) => C,
  config?: DiffBound,
): (a: string, b: string) => readonly C[] {
  return (a, b) => {
    const na = normalizeIgnoringWhitespace(a);
    const nb = normalizeIgnoringWhitespace(b);
    const changes = diffFn(na.text, nb.text, config);
    return changes.map((c) => {
      const r = remapChange(c, na.map, nb.map);
      return makeChange(r.fromA, r.toA, r.fromB, r.toB);
    });
  };
}
