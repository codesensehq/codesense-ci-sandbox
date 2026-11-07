// SPDX-License-Identifier: MIT
import {
  forwardRef,
  memo,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type ForwardedRef,
  type ReactElement,
} from "react";
import type { Extension } from "@codemirror/state";

import { useViewerStore } from "@/shared/stores/viewer-store";
import { useInstanceCount } from "@/shared/hooks/use-instance-count";
import { getEffectiveCodeEditorFontPx } from "@/shared/lib/font-sizes";
import type { ViewSnapshot } from "@/shared/lib/view-snapshot";
import { useCodeEditor, type CodeEditorHandle, type CursorInfo } from "./use-code-editor";
import { detectLanguageId } from "./language-detection";
import { useEditorTheme } from "./use-editor-theme";
import type { LanguageId } from "./cm-loader";

export type { CodeEditorHandle, CursorInfo } from "./use-code-editor";

export interface CodeEditorProps {
  /** Editor content. Pass UTF-8-decoded string (use `decodeUtf8` for bytes). */
  value: string;
  /** Per-keystroke change handler. */
  onChange: (next: string) => void;
  /**
   * Force a specific CM6 language id; "auto" detects from `path`. Default "auto".
   * Accepts any string (per AC-3) — unknown strings are treated as plain text.
   */
  language?: LanguageId | "auto" | string;
  /** Path used for language auto-detection (e.g. ".md" → markdown). */
  path?: string;
  /** Apply CM6's readOnly extension + reflect aria-readonly. Default false. */
  readOnly?: boolean;
  /** "auto" follows next-themes; explicit values force-override. Default "auto". */
  theme?: "light" | "dark" | "auto";
  /** Editor outer height; passes through to the container. Default "100%". */
  height?: string | number;
  /** Px font size. Default: viewer-store's codeEditorFontSize preset. */
  fontSize?: number;
  /** Show line-number gutter. Default true. */
  gutters?: boolean;
  /** SW-VE-8: soft-wrap long lines. Default false (code default; the context
   *  inset passes true for prose). Live-reconfigured via a compartment. */
  lineWrap?: boolean;
  /** SW-VE-8: enable in-editor find/replace (⌘F). Default false — read-only
   *  viewers stay unchanged; the editable context inset opts in. */
  search?: boolean;
  /** SW-VE-11: code folding (fold gutter + keymap). Default false. */
  folding?: boolean;
  /** SW-VE-11: render tabs/spaces as glyphs. Default false. */
  showWhitespace?: boolean;
  /** SW-VE-11: indent width (indentUnit + hard-tab render width). Default 2. */
  tabSize?: number;
  /** SW-VE-11: editor font-family (mono for code; a prose reading stack when the
   *  prose-font setting is on). Default: the monospace stack. */
  fontFamily?: string;
  /** Cmd+S handler (focused) and blur-if-dirty handler. */
  onSave?: (value: string) => Promise<void>;
  /** Called once on each chunk-load rejection. */
  onLoadError?: (err: Error) => void;
  /** Gates blur-save behavior. Editor does NOT render any dirty indicator (D-5). */
  dirty?: boolean;
  /** CM6 placeholder extension content. */
  placeholder?: string;
  /** First-mount autofocus only. Host calls handle.focus() for tab-switch focusing. */
  autoFocus?: boolean;
  /** Sets aria-label on the editor root; defaults to "Code editor". */
  ariaLabel?: string;
  /** Indent with hard tabs (the worktree editor detects this per file). */
  indentWithTabs?: boolean;
  /** `"\r\n"` keeps a CRLF file CRLF on save. Default LF joining. */
  lineSeparator?: "\n" | "\r\n";
  /** Full-editor affordances (brackets, multi-cursor, Tab indent, ⌥⌘F…). */
  ide?: boolean;
  /** Committed text for the change gutter; `undefined` = no gutter. */
  changeBaseline?: string | null;
  /** Cursor/selection readout for a status bar (once per frame at most). */
  onCursorChange?: (info: CursorInfo) => void;
  /** Surface-specific CM6 extensions composed AFTER SS-14's defaults. */
  customExtensions?: Extension[];
  /** RF-perf-12: called with a lightweight `{scrollTop, selection}` snapshot in
   *  the view's unmount cleanup (before the CM view is destroyed), so a host can
   *  persist cursor/scroll across an unmount→remount. */
  onBeforeDestroy?: (snap: ViewSnapshot) => void;
}

const KNOWN_LANGUAGE_IDS: ReadonlySet<string> = new Set<LanguageId>([
  "markdown",
  "typescript",
  "javascript",
  "json",
  "yaml",
  "python",
  "shell",
  "rust",
  "xml",
  "css",
  "html",
  "go",
  "toml",
  "sql",
  "c",
  "cpp",
  "java",
  "kotlin",
  "csharp",
  "swift",
  "ruby",
  "lua",
  "dockerfile",
  "diff",
  "properties",
  "plain",
]);

function resolveLanguage(
  prop: LanguageId | "auto" | string,
  path?: string,
  content?: string,
): LanguageId {
  if (prop === "auto") return detectLanguageId(path, content);
  // L-4 patch: spec says `language?: string | "auto"`. Unknown strings
  // narrow to "plain" rather than being passed through as an invalid id.
  return KNOWN_LANGUAGE_IDS.has(prop) ? (prop as LanguageId) : "plain";
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

function CodeEditorImpl(
  props: CodeEditorProps,
  ref: ForwardedRef<CodeEditorHandle | null>,
): ReactElement {
  const {
    value,
    onChange,
    language = "auto",
    path,
    readOnly = false,
    theme: themeProp = "auto",
    height = "100%",
    fontSize: fontSizeProp,
    gutters = true,
    lineWrap = false,
    search = false,
    folding = false,
    showWhitespace = false,
    tabSize = 2,
    fontFamily,
    onSave,
    onLoadError,
    dirty = false,
    placeholder,
    autoFocus = false,
    ariaLabel = "Code editor",
    indentWithTabs,
    lineSeparator,
    ide,
    changeBaseline,
    onCursorChange,
    customExtensions,
    onBeforeDestroy,
  } = props;

  // RF-perf-4 (AC-1): count this CodeMirror instance for the gated memory line.
  // One <CodeEditor> ≈ one EditorView; the keep-alive LRU holds inactive-tab
  // editors mounted (display:none), so this attributes that footprint.
  useInstanceCount("editor");

  const codeEditorFontSize = useViewerStore((s) => s.settings?.codeEditorFontSize);
  const codeEditorFontSizePx = useViewerStore((s) => s.settings?.codeEditorFontSizePx);
  // SLW-WDE-7: effective app mode for the code editor, resolved
  // once in the shared hook so every mount of <CodeEditor> (WDE-1, WDE-2) is
  // themed without threading a prop. `theme` carries the effective-dark (RD-6).
  const { theme } = useEditorTheme(themeProp);
  // E2-M3 patch: `value` deliberately excluded from deps. Shebang
  // detection only matters when path lacks a definitive extension AND
  // the content's first line is a shebang — and shebangs don't change
  // at runtime, so re-detecting on every keystroke just runs an O(first
  // line length) split for no behavior change. The first render's value
  // is captured by closure; subsequent path changes still re-detect.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const detectedLanguage = useMemo(() => resolveLanguage(language, path, value), [language, path]);

  const fontSize =
    fontSizeProp ??
    (codeEditorFontSize
      ? getEffectiveCodeEditorFontPx(codeEditorFontSize, codeEditorFontSizePx)
      : 13);

  const [container, setContainer] = useState<HTMLDivElement | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const containerCallback = (el: HTMLDivElement | null): void => {
    containerRef.current = el;
    setContainer(el);
  };

  const { handle, loadState, retry } = useCodeEditor({
    container,
    value,
    onChange,
    onSave,
    onLoadError,
    readOnly,
    dirty,
    theme,
    fontSize,
    gutters,
    lineWrap,
    language: detectedLanguage,
    placeholder,
    search,
    folding,
    showWhitespace,
    tabSize,
    fontFamily,
    indentWithTabs,
    lineSeparator,
    ide,
    changeBaseline,
    onCursorChange,
    customExtensions,
    onBeforeDestroy,
  });

  // H-5 patch: expose the imperative handle as `CodeEditorHandle | null`
  // (it really is null during loading / error). The previous
  // `handle as CodeEditorHandle` cast lied to TS and crashed hosts using
  // `ref.current.focus()` without optional chaining. The explicit type
  // arg keeps React's overload from inferring T = CodeEditorHandle.
  useImperativeHandle<CodeEditorHandle | null, CodeEditorHandle | null>(ref, () => handle, [
    handle,
  ]);

  // First-mount autoFocus (AC-3): once the editor is ready, focus once per
  // mount cycle. L-1 patch: reset the one-shot guard whenever the editor
  // transitions OUT of "ready" (load-error followed by retry → fresh
  // autoFocus on the recovered editor).
  const autoFocusedRef = useRef(false);
  useEffect(() => {
    if (loadState !== "ready") {
      autoFocusedRef.current = false;
      return;
    }
    if (!autoFocus) return;
    if (autoFocusedRef.current) return;
    handle?.focus();
    autoFocusedRef.current = true;
  }, [loadState, autoFocus, handle]);

  // L-11 patch: only render the aria-live text when readOnly actually
  // changes. On first mount the region is empty so screen readers don't
  // announce "Editor is editable" for every editor that opens. The text
  // appears for one render after a transition and is cleared on the next
  // tick so the live region doesn't echo on every unrelated re-render.
  const prevReadOnlyRef = useRef<boolean | null>(null);
  const [liveText, setLiveText] = useState("");
  useEffect(() => {
    if (prevReadOnlyRef.current === null) {
      prevReadOnlyRef.current = readOnly;
      return;
    }
    if (prevReadOnlyRef.current === readOnly) return;
    prevReadOnlyRef.current = readOnly;
    // setState inside the effect is intentional here: the announcement
    // text is derived from a *transition* of `readOnly`, not its current
    // value. The condition above gates against cascading re-renders (the
    // effect only setStates when readOnly actually changed since last run).
    setLiveText(readOnly ? "Editor is read-only" : "Editor is editable");
    const timer = setTimeout(() => setLiveText(""), 500);
    return () => clearTimeout(timer);
  }, [readOnly]);

  const outerStyle: CSSProperties = {
    position: "relative",
    width: "100%",
    height,
    minHeight: 0,
    overflow: "hidden",
  };

  return (
    <div
      data-codesense-code-editor=""
      // SW-VE-8: mark editors that own ⌘F via the composed `@codemirror/search`
      // panel, so document-level ⌘F routing can bow out for THIS editor without
      // over-suppressing find on read-only viewers (which render `.cm-editor`
      // but have no search extension).
      data-cm-search={search ? "" : undefined}
      aria-label={ariaLabel}
      style={outerStyle}
    >
      <div
        aria-live="polite"
        style={{
          position: "absolute",
          width: 1,
          height: 1,
          padding: 0,
          margin: -1,
          overflow: "hidden",
          clip: "rect(0,0,0,0)",
          border: 0,
        }}
      >
        {liveText}
      </div>

      {/* Container always rendered so the CM6 effect can mount into it. */}
      <div
        ref={containerCallback}
        style={{
          width: "100%",
          height: "100%",
          visibility: loadState === "ready" ? "visible" : "hidden",
        }}
      />

      {loadState === "loading" && (
        <div
          aria-busy="true"
          aria-label={`${ariaLabel} loading`}
          style={{ ...SKELETON_STYLE_BASE, position: "absolute", inset: 0 }}
        >
          Loading editor…
        </div>
      )}

      {loadState === "error" && (
        <div role="alert" style={{ ...ERROR_STYLE_BASE, position: "absolute", inset: 0 }}>
          <div>Editor failed to load.</div>
          <button type="button" style={RETRY_BUTTON_STYLE} onClick={retry}>
            Retry
          </button>
        </div>
      )}
    </div>
  );
}

export const CodeEditor = memo(
  forwardRef<CodeEditorHandle | null, CodeEditorProps>(CodeEditorImpl),
);
CodeEditor.displayName = "CodeEditor";
