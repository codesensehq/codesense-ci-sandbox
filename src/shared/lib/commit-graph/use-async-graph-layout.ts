// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { VIEWER_PERF } from "@/shared/lib/viewer-perf-config";
import { layoutCommitGraph, type CommitLaneInput, type CommitGraphLayout } from "./layout";
import { runGraphLayout } from "./run-graph-layout";

const EMPTY_LAYOUT: CommitGraphLayout = { rows: [], width: 0 };

export interface AsyncGraphLayoutResult {
  /** The laid-out graph: fresh when settled, the last good layout while a
   *  re-layout is in flight (keep-previous), or empty when cold / empty / failed. */
  layout: CommitGraphLayout;
  /** True ONLY when cold (nothing has ever laid out yet) — caller shows a skeleton. */
  layoutPending: boolean;
  /** True if the layout failed/timed out — caller shows an error + retry (or the
   *  plain linear fallback, AC-10). */
  layoutFailed: boolean;
  /** A re-layout is in flight but a prior graph is on screen (non-gating). */
  layoutRefreshing: boolean;
  /** Re-run the layout (error-state retry). */
  retryLayout: () => void;
}

interface GraphResult {
  /** The `commits` identity this result was produced for (pending is derived by
   *  comparing it to the current input — no setState needed to enter pending). */
  input: CommitLaneInput[];
  layout: CommitGraphLayout | null;
  failed: boolean;
}

/**
 * SLW-WD-9 (RD-2/AC-4): lay out the commit graph off the main thread so a
 * multi-thousand-commit history can't freeze the renderer. Mirrors
 * `useAsyncLayout`'s generation guard (drop stale results) + keep-previous window
 * (show the prior good graph during a re-layout) over the commit-graph types.
 *
 * When `VIEWER_PERF.layoutInWorker` is off (rollback / the automatic test path
 * where `Worker` is absent), it lays out synchronously in a memo — identical
 * output, no pending state.
 */
export function useAsyncGraphLayout(commits: CommitLaneInput[]): AsyncGraphLayoutResult {
  const useWorker = VIEWER_PERF.layoutInWorker;

  // Synchronous path (flag off / tests): lay out in a memo, no pending state.
  const syncLayout = useMemo(
    () => (!useWorker && commits.length > 0 ? layoutCommitGraph(commits) : null),
    [useWorker, commits],
  );

  const [result, setResult] = useState<GraphResult | null>(null);
  const [retryNonce, setRetryNonce] = useState(0);
  const genRef = useRef(0);
  useEffect(() => {
    if (!useWorker || commits.length === 0) return;
    const gen = ++genRef.current;
    const handle = runGraphLayout(commits);
    handle.promise
      .then((laidOut) => {
        if (gen === genRef.current) setResult({ input: commits, layout: laidOut, failed: false });
      })
      .catch(() => {
        if (gen === genRef.current) setResult({ input: commits, layout: null, failed: true });
      });
    return () => handle.cancel();
  }, [useWorker, commits, retryNonce]);

  const retryLayout = useCallback(() => {
    setResult(null); // clear so the retry shows a skeleton, not the stale error
    setRetryNonce((n) => n + 1);
  }, []);

  if (!useWorker) {
    return {
      layout: syncLayout ?? EMPTY_LAYOUT,
      layoutPending: false,
      layoutFailed: false,
      layoutRefreshing: false,
      retryLayout,
    };
  }

  // An empty history shows nothing (the rail still renders the Working node) —
  // never keep a prior graph. Short-circuit before keep-previous.
  if (commits.length === 0) {
    return {
      layout: EMPTY_LAYOUT,
      layoutPending: false,
      layoutFailed: false,
      layoutRefreshing: false,
      retryLayout,
    };
  }

  const settled = result !== null && result.input === commits;
  if (settled) {
    return {
      layout: result.layout ?? EMPTY_LAYOUT,
      layoutPending: false,
      layoutFailed: result.failed,
      layoutRefreshing: false,
      retryLayout,
    };
  }

  // Keep-previous: a re-layout is in flight and a prior good graph exists — keep
  // showing it (the gen guard guarantees only the latest result is applied).
  if (result !== null && result.layout !== null && result.layout.rows.length > 0) {
    return {
      layout: result.layout,
      layoutPending: false,
      layoutFailed: false,
      layoutRefreshing: true,
      retryLayout,
    };
  }

  // Cold: nothing has ever laid out. The only case that shows the skeleton.
  return {
    layout: EMPTY_LAYOUT,
    layoutPending: true,
    layoutFailed: false,
    layoutRefreshing: false,
    retryLayout,
  };
}
