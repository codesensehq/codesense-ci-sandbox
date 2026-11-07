// SPDX-License-Identifier: MIT
import { forwardRef, memo, useImperativeHandle, type ForwardedRef, type ReactElement } from "react";

import { useCodeMergeView, type CodeMergeViewHandle } from "./use-code-merge-view";
import { useMergeViewChrome } from "./use-merge-view-chrome";
import { MergeViewFrame } from "./merge-view-frame";

export type { CodeMergeViewHandle } from "./use-code-merge-view";

export interface CodeMergeViewProps {
  /** Read-only disk-side content (rendered on the left). */
  diskValue: string;
  /** Editable local-side content (rendered on the right; saved on Cmd+S). */
  localValue: string;
  /** Called on Cmd+S with the editable side's current doc. */
  onSave?: (mergedValue: string) => Promise<void>;
  /** Called once on each CM6 chunk-load rejection. */
  onLoadError?: (err: Error) => void;
  /** Drives language detection for syntax highlighting. Defaults to plain text. */
  path?: string;
  /** "light" | "dark" | "auto" — defaults to "auto" (subscribes to next-themes). */
  theme?: "light" | "dark" | "auto";
  /** Px font size; mirrors CodeEditor fontSize fallback chain. */
  fontSize?: number;
  /** Line-number gutter on/off. Default true. */
  gutters?: boolean;
  /** Container height. Default "100%". */
  height?: string | number;
  /** ARIA label for the merge view root. Default "Code merge view". */
  ariaLabel?: string;
  /** The file's line ending (both sides), read when the view is built. Default LF. */
  lineSeparator?: "\n" | "\r\n";
}

function CodeMergeViewImpl(
  props: CodeMergeViewProps,
  ref: ForwardedRef<CodeMergeViewHandle | null>,
): ReactElement {
  const {
    diskValue,
    localValue,
    onSave,
    onLoadError,
    path,
    theme: themeProp = "auto",
    fontSize: fontSizeProp,
    gutters = true,
    height = "100%",
    ariaLabel = "Code merge view",
    lineSeparator,
  } = props;

  const { theme, fontSize, language, container, containerCallback } = useMergeViewChrome({
    themeProp,
    fontSizeProp,
    path,
  });

  const { handle, loadState, retry } = useCodeMergeView({
    container,
    diskValue,
    localValue,
    onSave,
    onLoadError,
    theme,
    fontSize,
    gutters,
    language,
    lineSeparator,
  });

  useImperativeHandle<CodeMergeViewHandle | null, CodeMergeViewHandle | null>(ref, () => handle, [
    handle,
  ]);

  return (
    <MergeViewFrame
      containerRef={containerCallback}
      loadState={loadState}
      retry={retry}
      testId="code-merge-view"
      ariaLabel={ariaLabel}
      loadingLabel={`Loading ${ariaLabel?.toLowerCase() ?? "merge view"}`}
      errorText="Merge view failed to load."
      height={height}
    />
  );
}

export const CodeMergeView = memo(
  forwardRef<CodeMergeViewHandle | null, CodeMergeViewProps>(CodeMergeViewImpl),
);
CodeMergeView.displayName = "CodeMergeView";
