// SPDX-License-Identifier: MIT
/**
 * SLW-DV-8b (AC-1/AC-4) — resolve the structural diff for the current content,
 * off the main thread. Mirrors `commit-graph/use-async-graph-layout.ts`: a
 * generation guard drops stale worker results, and the in-flight status is
 * DERIVED (by comparing the stored result's input to the current content) rather
 * than written synchronously in the effect — so there is no cascading setState.
 *
 * The deliberate divergence from the graph hook is NO keep-previous across a
 * content change. Structural changes are valid ONLY for the exact `before`/`after`
 * they were computed from — a cached `() => changes` override applied to a doc that
 * has since been mutated (DV-5's in-place live patch) would mis-highlight or throw
 * in CM's `makePresentable`. So while the stored result's input differs from the
 * current content the status is `computing` (the consumer shows the line diff), and
 * only a result whose input still matches is served as `ready` (AC-4). A cached
 * result for UNCHANGED content survives a toggle off→on — no needless recompute.
 */

import { useEffect, useRef, useState } from "react";

import type { LanguageId } from "./cm-loader";
import type { OffsetChange } from "./whitespace-diff";
import { isStructuralLanguage } from "./structural-diff";
import { runStructuralDiff, type StructuralDiffHandle } from "./run-structural-diff";

// AC-4: how long content must go idle before the off-thread recompute fires, so a
// burst of live edits collapses into ONE parse rather than a worker job per change.
export const STRUCTURAL_RECOMPUTE_DEBOUNCE_MS = 150;

export type StructuralDiffStatus = "idle" | "unsupported" | "computing" | "ready" | "failed";

export interface StructuralDiffResult {
  /** The changes for the current content when `status === "ready"`, else `null`. */
  changes: OffsetChange[] | null;
  status: StructuralDiffStatus;
}

interface StoredResult {
  input: { before: string; after: string; language: LanguageId };
  /** `null` = the worker failed / timed out for this input. */
  changes: OffsetChange[] | null;
}

export function useStructuralDiff(
  before: string,
  after: string,
  language: LanguageId,
  enabled: boolean,
): StructuralDiffResult {
  const supported = enabled && isStructuralLanguage(language);
  const [result, setResult] = useState<StoredResult | null>(null);
  const genRef = useRef(0);

  useEffect(() => {
    if (!supported) {
      // Invalidate any in-flight compute so its late result can't land; the status
      // is derived below, so no synchronous setState is needed here.
      genRef.current += 1;
      return;
    }
    const gen = ++genRef.current;
    const input = { before, after, language };
    // AC-4: debounce so a live-edit burst coalesces into one parse once edits go
    // idle. While the timer is pending the stored result no longer matches the
    // current content, so the status derived below reads `computing` and the
    // consumer shows the line diff — never a stale structural override.
    let handle: StructuralDiffHandle | null = null;
    const timer = setTimeout(() => {
      handle = runStructuralDiff(input);
      handle.promise
        .then((changes) => {
          if (gen === genRef.current) setResult({ input, changes });
        })
        .catch(() => {
          if (gen === genRef.current) setResult({ input, changes: null });
        });
    }, STRUCTURAL_RECOMPUTE_DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
      handle?.cancel();
    };
  }, [supported, before, after, language]);

  if (!enabled) return { changes: null, status: "idle" };
  if (!isStructuralLanguage(language)) return { changes: null, status: "unsupported" };

  // "settled" only when the stored result was computed for THIS exact content —
  // otherwise a content change reads as computing (never a stale override, AC-4).
  const settled =
    result != null &&
    result.input.before === before &&
    result.input.after === after &&
    result.input.language === language;
  if (!settled) return { changes: null, status: "computing" };
  if (result.changes == null) return { changes: null, status: "failed" };
  return { changes: result.changes, status: "ready" };
}
