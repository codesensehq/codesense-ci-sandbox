// SPDX-License-Identifier: MIT
import {
  memo,
  useEffect,
  useImperativeHandle,
  useRef,
  type CSSProperties,
  type ReactElement,
  type Ref,
} from "react";

import { ArrowDown } from "lucide-react";

import type { SourceDiffMode } from "@/shared/stores/viewer-store";
import { useCodeMergeView } from "./use-code-merge-view";
import { useMergeViewChrome } from "./use-merge-view-chrome";
import { MergeViewFrame } from "./merge-view-frame";
import { useStructuralDiff, type StructuralDiffStatus } from "./use-structural-diff";

/**
 * SLW-WD-6 — a read-only, syntax-highlighted code diff for source-file review.
 * The review-mode counterpart to `<CodeMergeView>`: it reuses the same
 * `useCodeMergeView` engine (language detection, themes, collapse-unchanged,
 * async CM6 loader) but both sides are read-only with no revert/merge/save
 * controls (RD-1/AC-2). `before` is the baseline (red removals), `after` the
 * worktree version (green additions) — the same strings `RedlineProse` gets.
 *
 * No-flash / theme / mode are handled by remounting the frame on a composite
 * key (§D/§E): a file switch, a light↔dark swap, or a split↔inline toggle
 * rebuilds a fresh view rather than mutating the live one. The consumer does
 * NOT need to add its own `key`.
 */
/** Step through a review diff's changes from outside the view (the host's
 *  n/p keys and its "2 of 7" navigator). */
export interface DiffNavigator {
  next: () => void;
  prev: () => void;
  /** The 1-based worktree line of the change being read (the first change
   *  before the reader has moved); `null` when the diff has no changes. */
  currentLine: () => number | null;
}

export interface ChunkPosition {
  /** 0-based change the cursor is in; `null` while it sits outside every
   *  change (a fresh view's cursor is on line 1, so a change there reads 0). */
  index: number | null;
  count: number;
}

export interface CodeDiffReviewProps {
  /** 1-based inclusive range in the after-side document. */
  afterRange?: { start: number; end: number };
  /** Baseline content (side a — removed lines render red). */
  before: string;
  /** Worktree content (side b — added lines render green). */
  after: string;
  /** Drives language detection for syntax highlighting. Plain when unknown. */
  path?: string;
  /** `"split"` side-by-side (default) or `"inline"` unified single-column. */
  mode?: "split" | "inline";
  /** SLW-DV-6 (AC-6): soft-wrap long lines. A change remounts the view. */
  wordWrap?: boolean;
  /** SLW-DV-6 (AC-4): collapse unchanged regions (default) or show full context
   *  when `false`. A change remounts the view. */
  collapseUnchanged?: boolean;
  /** "light" | "dark" | "auto" — defaults to "auto" (subscribes to next-themes). */
  theme?: "light" | "dark" | "auto";
  /** Px font size; mirrors CodeMergeView's fallback chain. */
  fontSize?: number;
  /** Container height. Default "100%". */
  height?: string | number;
  /** ARIA label for the diff root. Default "Code diff". */
  ariaLabel?: string;
  /** Called once on each CM6 chunk-load rejection. */
  onLoadError?: (err: Error) => void;
  /**
   * SLW-DV-5 (AC-4): follow the newest live edit (`tail -f`). When the user
   * scrolls up, following pauses and a "jump to latest" pill appears to resume.
   * Live updates always patch in place regardless of this flag (AC-6); it only
   * governs whether the viewport tracks the newest change.
   */
  autoFollow?: boolean;
  /**
   * SLW-DV-8b (RD-3): the diff mode — `"line"` (default), `"ignoreWhitespace"`, or
   * `"structural"`. In `"structural"` mode this component computes the parse-tree
   * diff off-thread and falls back to the line diff (with an honest indicator)
   * while it computes, on failure, or for an unsupported language. A change
   * remounts the view.
   */
  diffMode?: SourceDiffMode;
  /** Receives a navigator once the view is ready (and `null` on teardown). */
  navRef?: Ref<DiffNavigator | null>;
  /** Called with the change count once built, and the position after each move
   *  or live update. */
  onChunkPosition?: (position: ChunkPosition) => void;
}

// A sticky header row ABOVE the diff frame — it reserves its own space (rather
// than floating over and hiding the first line) and truncates with an ellipsis.
// Shared by the SLW-DV-8 scope breadcrumb and the SLW-DV-8b structural-mode
// indicator (same idiom, one definition).
const DIFF_HEADER_ROW_STYLE: CSSProperties = {
  flex: "none",
  padding: "2px 8px",
  font: "inherit",
  fontSize: "11px",
  lineHeight: "18px",
  color: "var(--muted-foreground, var(--foreground))",
  backgroundColor: "var(--background)",
  borderBottom: "1px solid var(--border)",
  whiteSpace: "nowrap",
  overflow: "hidden",
  textOverflow: "ellipsis",
};

// SLW-DV-8b (AC-5): the honesty indicator's copy for each structural status —
// what is actually on screen and, when it fell back, why. `idle` never reaches
// here (the row only renders while structural is requested).
function structuralIndicatorLabel(status: StructuralDiffStatus): string | null {
  switch (status) {
    case "ready":
      return "Structural diff";
    case "computing":
      return "Structural diff — computing…";
    case "unsupported":
      return "Structural diff unavailable for this language — showing line diff";
    case "failed":
      return "Structural diff unavailable — showing line diff";
    case "idle":
      return null;
  }
}

const JUMP_PILL_STYLE: CSSProperties = {
  position: "absolute",
  bottom: "12px",
  left: "50%",
  transform: "translateX(-50%)",
  display: "inline-flex",
  alignItems: "center",
  gap: "6px",
  padding: "4px 12px",
  borderRadius: "9999px",
  border: "1px solid var(--border)",
  backgroundColor: "var(--background)",
  color: "var(--foreground)",
  font: "inherit",
  fontSize: "12px",
  cursor: "pointer",
  boxShadow: "0 2px 8px rgba(0, 0, 0, 0.18)",
  zIndex: 2,
};

function CodeDiffReviewImpl(props: CodeDiffReviewProps): ReactElement {
  const {
    before,
    after,
    path,
    mode = "split",
    wordWrap = false,
    collapseUnchanged = true,
    theme: themeProp = "auto",
    fontSize: fontSizeProp,
    height = "100%",
    ariaLabel = "Code diff",
    onLoadError,
    autoFollow = false,
    diffMode = "line",
    navRef,
    onChunkPosition,
    afterRange,
  } = props;

  const { theme, fontSize, language, container, containerCallback } = useMergeViewChrome({
    themeProp,
    fontSizeProp,
    path,
  });

  // SLW-DV-8b (AC-1/AC-3/AC-4): resolve the structural diff off-thread when
  // requested. `structural` is only the EFFECTIVE mode once fresh changes have
  // landed for the current content; while computing, on failure, or for an
  // unsupported language we render the plain line diff (AC-3) and the indicator
  // below tells the user why (AC-5).
  const structuralRequested = diffMode === "structural";
  const { changes: structuralChanges, status: structuralStatus } = useStructuralDiff(
    before,
    after,
    language,
    structuralRequested,
  );
  const structuralReady =
    structuralRequested && structuralStatus === "ready" && structuralChanges != null;
  const effectiveMode: SourceDiffMode = structuralReady
    ? "structural"
    : structuralRequested
      ? "line"
      : diffMode;

  const { handle, loadState, retry, live, breadcrumb } = useCodeMergeView({
    container,
    diskValue: before,
    localValue: after,
    onLoadError,
    theme,
    fontSize,
    gutters: true,
    language,
    review: true,
    mode,
    wordWrap,
    collapseUnchanged,
    autoFollow,
    diffMode: effectiveMode,
    structuralChanges: structuralReady ? structuralChanges : undefined,
    // SLW-DV-6 (AC-7): the review diff always offers find-in-diff.
    search: true,
  });

  useEffect(() => {
    if (handle && afterRange) handle.revealRange(afterRange.start, afterRange.end);
  }, [handle, after, afterRange?.start, afterRange?.end]);

  // A fresh view starts with the cursor at the top of the document, which is
  // INSIDE a change that begins on line 1 — so CM's "next change" would skip
  // straight past it. Until the reader moves, the first "next" lands on the
  // first change instead.
  const movedRef = useRef(false);
  // Where the caret was when the view was built: a click or key that leaves
  // it there (a scrollbar, Tab, Escape) is not a move.
  const startCaretRef = useRef(0);
  useEffect(() => {
    movedRef.current = false;
    startCaretRef.current = handle?.getCaret() ?? 0;
  }, [handle]);

  // The navigator moves the cursor chunk to chunk (CM scrolls it into view) and
  // reports where it landed, so the host's counter tracks the reader.
  // The latest callback, read at call time: an inline host callback that sets
  // state must not re-run the report effect (a render loop) or rebuild the
  // navigator every render.
  const onChunkPositionRef = useRef(onChunkPosition);
  useEffect(() => {
    onChunkPositionRef.current = onChunkPosition;
  });
  const report = (at: ChunkPosition) => onChunkPositionRef.current?.(at);
  useImperativeHandle<DiffNavigator | null, DiffNavigator | null>(
    navRef,
    () =>
      handle
        ? {
            next: () => {
              const firstMove = !movedRef.current;
              movedRef.current = true;
              const at = handle.getChunkPosition();
              if (firstMove && at.index === 0) {
                // The reader may have scrolled away from it: bring it back.
                handle.revealChunk(0);
                report(handle.getChunkPosition());
                return;
              }
              // CM refuses to move from inside the only change; bring that
              // change back into view instead of doing nothing.
              if (!handle.goToNextChunk() && at.count === 1) handle.revealChunk(0);
              report(handle.getChunkPosition());
            },
            prev: () => {
              movedRef.current = true;
              if (!handle.goToPreviousChunk() && handle.getChunkPosition().count === 1) {
                handle.revealChunk(0);
              }
              report(handle.getChunkPosition());
            },
            currentLine: () => handle.getChunkLine(handle.getChunkPosition().index ?? 0),
          }
        : null,
    [handle],
  );
  // A click or caret key that moved the caret: the reader has moved (so the
  // first "next" no longer re-reveals change 0). The host always hears the
  // position: it clears its counter on a switch that may not rebuild the view.
  const reportPosition = () => {
    if (!handle) return;
    if (handle.getCaret() !== startCaretRef.current) movedRef.current = true;
    report(handle.getChunkPosition());
  };
  // A fresh view (or a live edit that adds/removes a change) re-reports the
  // count, always: the host clears its counter on a file or basis switch.
  useEffect(() => {
    if (!handle) return;
    onChunkPositionRef.current?.(handle.getChunkPosition());
  }, [handle, before, after]);

  // §D/§E: a fresh view per (file, mode, wrap, collapse, theme, font) — remounting
  // the frame rebuilds the read-only view cleanly (no stale doc, no flicker,
  // correct theme). A live edit to the SAME file keeps this key stable
  // (wrap/collapse only change on an explicit toggle), so it patches in place
  // (SLW-DV-5 AC-6) rather than remounting.
  const viewKey = `${path ?? ""}|${mode}|${wordWrap ? "wrap" : "nowrap"}|${
    collapseUnchanged ? "collapse" : "expand"
  }|${effectiveMode}|${theme}|${fontSize}`;

  // SLW-DV-8 (AC-2): the breadcrumb is a header row that reserves its own space
  // above the diff (never overlapping the first line). SLW-DV-5 (AC-4): the pill
  // lives OUTSIDE the keyed frame so it survives a theme/mode remount and floats
  // over the diff without joining its scroll.
  const scopeLabel = breadcrumb?.join(" › ");

  // SLW-DV-8b (AC-5): state what is actually on screen whenever structural is
  // requested — never silently reinterpret the diff.
  const structuralStatusLabel = structuralRequested
    ? structuralIndicatorLabel(structuralStatus)
    : null;

  return (
    <div style={{ display: "flex", flexDirection: "column", height, width: "100%" }}>
      {structuralStatusLabel && (
        <div
          style={DIFF_HEADER_ROW_STYLE}
          data-testid="code-diff-structural-status"
          role="status"
          aria-live="polite"
          aria-label={structuralStatusLabel}
        >
          {structuralStatusLabel}
        </div>
      )}
      {scopeLabel && (
        <div
          style={DIFF_HEADER_ROW_STYLE}
          data-testid="code-diff-breadcrumb"
          aria-label={`Scope: ${scopeLabel}`}
        >
          {scopeLabel}
        </div>
      )}
      <div
        style={{ position: "relative", flex: 1, minHeight: 0 }}
        // A click or a caret key inside the diff moves the reader too: the
        // host's "i of N" follows it, not just the n/p navigator.
        onMouseUp={reportPosition}
        onKeyUp={reportPosition}
      >
        <MergeViewFrame
          key={viewKey}
          containerRef={containerCallback}
          loadState={loadState}
          retry={retry}
          testId="code-diff-review"
          ariaLabel={ariaLabel}
          loadingLabel="Loading diff…"
          errorText="Couldn't load the diff view."
          height="100%"
        />
        {live?.paused && (
          <button
            type="button"
            data-testid="code-diff-jump-to-latest"
            style={JUMP_PILL_STYLE}
            onClick={live.jumpToLatest}
          >
            <ArrowDown size={13} aria-hidden />
            Jump to latest
          </button>
        )}
      </div>
    </div>
  );
}

export const CodeDiffReview = memo(CodeDiffReviewImpl);
CodeDiffReview.displayName = "CodeDiffReview";
