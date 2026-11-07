// SPDX-License-Identifier: MIT
import { useMemo, useState } from "react";

import { useViewerStore } from "@/shared/stores/viewer-store";
import { getEffectiveCodeEditorFontPx } from "@/shared/lib/font-sizes";
import { detectLanguageId } from "./language-detection";
import { useEditorTheme } from "./use-editor-theme";
import type { LanguageId } from "./cm-loader";

/**
 * Shared chrome plumbing for the CodeMirror merge/diff components
 * (`CodeMergeView`, `CodeDiffReview`): resolve the effective theme (prop or
 * next-themes), the effective font size (prop or the viewer-store code-editor
 * setting), the syntax-highlight language (from `path`), and the mount
 * container. Extracted so both components own only their own concern
 * (editable-vs-review) and share this identical wiring once (DRY).
 */
export interface MergeViewChromeOptions {
  /** "light" | "dark" | "auto" — "auto" subscribes to next-themes. */
  themeProp: "light" | "dark" | "auto";
  /** Explicit px font size; falls back to the viewer-store setting, then 13. */
  fontSizeProp?: number;
  /** Drives language detection; `plain` when unknown/absent. */
  path?: string;
}

export interface MergeViewChrome {
  theme: "light" | "dark";
  fontSize: number;
  language: LanguageId;
  container: HTMLDivElement | null;
  containerCallback: (el: HTMLDivElement | null) => void;
}

export function useMergeViewChrome({
  themeProp,
  fontSizeProp,
  path,
}: MergeViewChromeOptions): MergeViewChrome {
  const codeEditorFontSize = useViewerStore((s) => s.settings?.codeEditorFontSize);
  const codeEditorFontSizePx = useViewerStore((s) => s.settings?.codeEditorFontSizePx);
  // SLW-WDE-7: effective app mode for the code editor resolved
  // via the shared hook so the diff and the single editor stay in lockstep (RD-6).
  const { theme } = useEditorTheme(themeProp);
  const language: LanguageId = useMemo(() => detectLanguageId(path), [path]);
  const fontSize =
    fontSizeProp ??
    (codeEditorFontSize
      ? getEffectiveCodeEditorFontPx(codeEditorFontSize, codeEditorFontSizePx)
      : 13);

  const [container, setContainer] = useState<HTMLDivElement | null>(null);

  return { theme, fontSize, language, container, containerCallback: setContainer };
}
