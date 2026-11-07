// SPDX-License-Identifier: MIT
import { useEffect, useState, type CSSProperties, type ReactNode, type Ref } from "react";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";

import { parseToRedline } from "@/shared/lib/markdown-worker/client";

/**
 * SW-GS-10 — the shared redline render tail. Block-aligns two strings into
 * annotated ins/del HAST via the SW-GS-3 `parseToRedline` worker and renders it,
 * with skeleton + error states. Extracted from `MarkdownRedlineViewer` so the
 * conflict resolved-vs-ours viewer reuses it (DRY): callers own their FETCH +
 * CHROME (width / theme / font / scroll container / ref); this owns parse →
 * render. The one place redline rendering evolves.
 */
type ParsedResult = { phase: "error"; message: string } | { phase: "ready"; node: ReactNode };

export interface RedlineProseProps {
  /** The two sides to diff; `null` on either side = upstream still loading. */
  before: string | null;
  after: string | null;
  /** An upstream (fetch) error to surface instead of parsing. */
  error?: string | null;
  /** File identity: a change resets to the skeleton (a file switch); a
   *  before/after change at the SAME key keeps the prior render (an edit reload),
   *  preserving the shipped no-flash-on-reload behavior. */
  resetKey: string;
  className?: string;
  style?: CSSProperties;
  proseRef?: Ref<HTMLDivElement>;
}

export function RedlineProse({
  before,
  after,
  error,
  resetKey,
  className,
  style,
  proseRef,
}: RedlineProseProps): React.JSX.Element {
  const [result, setResult] = useState<ParsedResult | null>(null);
  // Reset in the render phase on a file switch (adjust-during-render); a
  // before/after change at the same key keeps the prior node until the reparse
  // resolves (no skeleton flash on an edit reload).
  const [seenKey, setSeenKey] = useState(resetKey);
  if (seenKey !== resetKey) {
    setSeenKey(resetKey);
    setResult(null);
  }

  useEffect(() => {
    if (before == null || after == null) return;
    let cancelled = false;
    const handle = parseToRedline(before, after);
    handle.promise
      .then((parsed) => {
        if (!cancelled) {
          setResult({ phase: "ready", node: toJsxRuntime(parsed.hast, { Fragment, jsx, jsxs }) });
        }
      })
      .catch((e: unknown) => {
        if (!cancelled) {
          setResult({ phase: "error", message: e instanceof Error ? e.message : String(e) });
        }
      });
    return () => {
      cancelled = true;
      handle.cancel();
    };
  }, [before, after]);

  const content =
    error != null ? (
      <p className="text-sm text-destructive">Couldn’t render redline: {error}</p>
    ) : result == null ? (
      <div className="space-y-2" aria-hidden>
        {[0, 1, 2, 3].map((i) => (
          <div key={i} className="h-5 animate-pulse rounded bg-muted/50" />
        ))}
      </div>
    ) : result.phase === "error" ? (
      <p className="text-sm text-destructive">Couldn’t render redline: {result.message}</p>
    ) : (
      result.node
    );

  return (
    <div ref={proseRef} className={className} style={style}>
      {content}
    </div>
  );
}
