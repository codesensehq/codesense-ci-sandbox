// SPDX-License-Identifier: MIT
import { type CSSProperties, type ReactElement } from "react";

/**
 * Shared presentational chrome for a CodeMirror merge/diff view: the mount
 * container plus the lazy-load skeleton and the chunk-load error/Retry state.
 * Extracted so `<CodeMergeView>` (editable conflict editor) and
 * `<CodeDiffReview>` (SLW-WD-6 read-only review diff) render one frame instead
 * of two copies. The caller owns the `useCodeMergeView` hook and threads its
 * `loadState`/`retry` here; this file owns only the visual states.
 */
export interface MergeViewFrameProps {
  /** Ref callback for the element the merge view mounts into. */
  containerRef: (el: HTMLDivElement | null) => void;
  loadState: "loading" | "ready" | "error";
  retry: () => void;
  /** `data-testid` on the outer frame. */
  testId: string;
  /** ARIA label for the frame root. */
  ariaLabel: string;
  /** Skeleton copy + its `aria-label` while loading. */
  loadingLabel: string;
  /** Error headline shown on chunk-load failure (Retry button is appended). */
  errorText: string;
  /** Container height. Default "100%". */
  height?: string | number;
  /** SLW-WDE-7: Optional layout class applied to the frame root. */
  className?: string;
}

const SKELETON_STYLE_BASE: CSSProperties = {
  width: "100%",
  height: "100%",
  backgroundColor: "var(--muted)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  fontFamily: "'MesloLGS NF', ui-monospace, SFMono-Regular, Menlo, monospace",
  fontSize: "13px",
  color: "var(--muted-foreground, var(--foreground))",
  opacity: 0.7,
};

const ERROR_STYLE_BASE: CSSProperties = {
  width: "100%",
  height: "100%",
  backgroundColor: "var(--muted)",
  display: "flex",
  flexDirection: "column",
  alignItems: "center",
  justifyContent: "center",
  gap: "8px",
  fontFamily: "'MesloLGS NF', ui-monospace, SFMono-Regular, Menlo, monospace",
  fontSize: "13px",
  color: "var(--foreground)",
  padding: "16px",
};

const RETRY_BUTTON_STYLE: CSSProperties = {
  padding: "4px 12px",
  border: "1px solid var(--border)",
  borderRadius: "4px",
  backgroundColor: "var(--background)",
  color: "var(--foreground)",
  cursor: "pointer",
  font: "inherit",
};

export function MergeViewFrame({
  containerRef,
  loadState,
  retry,
  testId,
  ariaLabel,
  loadingLabel,
  errorText,
  height = "100%",
  className,
}: MergeViewFrameProps): ReactElement {
  const outerStyle: CSSProperties = {
    position: "relative",
    width: "100%",
    height,
    minHeight: 0,
    // SLW-WD-7 (AC-4/RD-2): own the VERTICAL scroll of the mounted view. A split
    // `MergeView` disables its inner editors' scrollers (`@codemirror/merge`
    // forces `.cm-mergeView & .cm-scroller { height:auto; overflowY:visible }`)
    // and defers to the unbounded `.cm-mergeView` wrapper — with `overflow:hidden`
    // here that overgrown view was CLIPPED, not scrollable. `overflowY:auto` lets
    // it scroll. `overflowX:hidden` keeps horizontal scrolling per-pane inside
    // each editor (the lib override leaves `overflowX` intact). Harmless to the
    // inline single-`.cm-editor` view: it's exactly `height:100%` and scrolls in
    // its own `.cm-scroller`, so this outer scroller stays dormant. Repairs both
    // the read-only `CodeDiffReview` and the editable `CodeMergeView` (AC-5).
    overflowX: "hidden",
    overflowY: "auto",
  };

  return (
    <div
      data-testid={testId}
      className={className || undefined}
      aria-label={ariaLabel}
      style={outerStyle}
    >
      {/* Container always rendered so the hook's effect can mount into it. */}
      <div
        ref={containerRef}
        style={{
          width: "100%",
          height: "100%",
          visibility: loadState === "ready" ? "visible" : "hidden",
        }}
      />

      {loadState === "loading" && (
        <div
          aria-busy="true"
          aria-label={loadingLabel}
          style={{ ...SKELETON_STYLE_BASE, position: "absolute", inset: 0 }}
        >
          {loadingLabel}
        </div>
      )}

      {loadState === "error" && (
        <div role="alert" style={{ ...ERROR_STYLE_BASE, position: "absolute", inset: 0 }}>
          <div>{errorText}</div>
          <button type="button" style={RETRY_BUTTON_STYLE} onClick={retry}>
            Retry
          </button>
        </div>
      )}
    </div>
  );
}
