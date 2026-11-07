// SPDX-License-Identifier: MIT
import { commitTextEditOnBlur } from "@/shared/lib/text-menu-focus";
import { useEffect, useRef, useState } from "react";
import type { Extension } from "@codemirror/state";
import type { EditorState as CmEditorState } from "@codemirror/state";
import type { EditorView as CmEditorView, ViewUpdate as CmViewUpdate } from "@codemirror/view";

import type { ViewSnapshot } from "@/shared/lib/view-snapshot";

import { getCodeMirror, type CmModule, type LanguageId } from "./cm-loader";
import { buildLanguageExtension } from "./language-detection";
import { MONO_FONT_STACK } from "./editor-affordances";
import { lightTheme } from "./themes/light";
import { darkTheme } from "./themes/dark";
import { buildSaveKeybinding } from "./extensions/save-keybinding";
import { hasConflict } from "./extensions/conflict-detection";
import { getChangeGutter } from "./extensions/change-gutter";
import { breadcrumbModeFor, resolveScopeChain, type ScopeTree } from "./scope-breadcrumb";

export interface CodeEditorHandle {
  // Content
  getValue: () => string;
  setValue: (next: string) => void;
  clearHistory: () => void;

  // Per-instance state preservation (D-18). A-1 patch: getState returns
  // null when the underlying view has been destroyed (unmount, retry). The
  // host's tab-switch save path may call getState during its own unmount
  // cleanup — returning null instead of throwing keeps the API consistent
  // with the rest of the null-safe handle surface.
  getState: () => CmEditorState | null;
  setState: (state: CmEditorState) => void;

  // Focus + selection
  focus: () => void;
  blur: () => void;
  getCursor: () => { line: number; ch: number };
  setSelection: (range: {
    from: { line: number; ch: number };
    to: { line: number; ch: number };
  }) => void;

  // Scroll
  getScrollPos: () => number;
  scrollTo: (pos: number) => void;

  // RF-perf-12: a LIGHTWEIGHT, serializable view snapshot — scroll + selection
  // as doc offsets. Deliberately NOT the CmEditorState / doc / undo history, so
  // persisting it across an unmount can't re-pin the memory RF-perf-10 reclaims
  // (AC-8). `getViewSnapshot` returns null when the view is gone (null-safe like
  // `getState`); `applyViewSnapshot` clamps offsets to the current doc.
  getViewSnapshot: () => ViewSnapshot | null;
  applyViewSnapshot: (snap: ViewSnapshot) => void;

  // Conflict
  checkConflict: (diskVersion: string) => boolean;

  // SW-VE-9: imperative find-panel control. Lets the host (the editable
  // context inset) coordinate Escape deterministically — query whether find is
  // open, then close it — instead of guessing from focus / DOM state. Null-safe
  // like the rest of the handle (false / no-op when the view is gone).
  /** Close the CM6 search panel if open; returns whether a panel was closed. */
  closeSearch: () => boolean;
  /** Whether the CM6 search panel is currently open. */
  isSearchOpen: () => boolean;
  /** Open the find/replace panel (no-op without the `search` option). */
  openSearch: () => void;
  /** Move the cursor to a 1-based line (and optional 1-based column), centred
   *  in view, and focus the editor. Out-of-range input is clamped. */
  revealLine: (line: number, column?: number) => void;
}

/** Where the cursor is and what is selected — the status bar's readout. */
export interface CursorInfo {
  /** 1-based line of the main cursor. */
  line: number;
  /** 1-based column (UTF-16 units, matching what the user counts on screen). */
  column: number;
  /** Number of cursors/selections (>1 with multi-cursor). */
  selections: number;
  /** Characters selected across every range. */
  selectedChars: number;
  /** Enclosing scope chain at the cursor (outermost first), or `null`. */
  scope: string[] | null;
  /** Lines in the document (go to line's range). */
  totalLines: number;
}

export type LoadState = "loading" | "ready" | "error";

export interface UseCodeEditorOptions {
  /** Container DOM element to mount the CM6 view into. */
  container: HTMLDivElement | null;
  /** Initial / sync-target value. See D-15. */
  value: string;
  /** Per-keystroke change handler. */
  onChange?: (next: string) => void;
  /** Save handler invoked by Cmd+S (focused) and by blur-if-dirty. */
  onSave?: (value: string) => Promise<void>;
  /** Called once on each CM6 chunk-load rejection. */
  onLoadError?: (err: Error) => void;
  /** Reflected on editor root + announced via aria-live (D-21). */
  readOnly?: boolean;
  /** Gates blur-save (D-5). Editor does not render a dirty indicator. */
  dirty?: boolean;
  /** "light" | "dark" — resolved by component from prop + next-themes. */
  theme: "light" | "dark";
  /** Px font size; honor prop if provided, else viewer-store preset. */
  fontSize: number;
  /** Line-number gutter on/off (D-22). */
  gutters: boolean;
  /** SW-VE-8: soft-wrap long lines on/off. Live-reconfigured via a compartment
   *  (mirrors `gutters`). Prose wraps, code doesn't — the host resolves the
   *  effective boolean from the `editorLineWrap` setting + file type. Default
   *  false (code default), so existing non-prose callers stay unchanged. */
  lineWrap?: boolean;
  /** Language id detected from path/content. */
  language: LanguageId;
  /** CM6 placeholder extension content. */
  placeholder?: string;
  /** SW-VE-8: compose in-editor find/replace (⌘F) — the `@codemirror/search`
   *  panel + keymap. Opt-in (default off) so read-only viewers stay unchanged;
   *  the editable context inset turns it on. Captured at mount (static per
   *  editor instance). */
  search?: boolean;
  /** SW-VE-11: code folding (fold gutter + fold keymap). Live-reconfigured via a
   *  compartment (mirrors `gutters`). The host resolves the effective boolean
   *  from `editorFolding` + file type. Default false so existing mounts are
   *  unchanged. */
  folding?: boolean;
  /** SW-VE-11: render tabs/spaces as glyphs. Live-reconfigured via a compartment.
   *  Default false. */
  showWhitespace?: boolean;
  /** SW-VE-11: indent width — sets both `indentUnit` (spaces inserted) and the
   *  hard-tab render width (`EditorState.tabSize`). Live-reconfigured via a
   *  compartment. Default 2. */
  tabSize?: number;
  /** SW-VE-11: editor font-family. Prose files get a proportional reading stack
   *  when the prose-font setting is on; code stays monospace. Applied as an
   *  inline style (mirrors `fontSize`), not a compartment. Default: the mono
   *  stack, so existing mounts are unchanged. */
  fontFamily?: string;
  /** Indent with hard tabs instead of `tabSize` spaces (detected per file by
   *  the worktree editor). Default false. */
  indentWithTabs?: boolean;
  /** Line separator the doc joins with. `"\r\n"` keeps a CRLF file CRLF on
   *  save; default (`"\n"`) leaves CodeMirror's LF joining. */
  lineSeparator?: "\n" | "\r\n";
  /** The full-editor affordances: bracket matching + auto-close, indent on
   *  input, multi-cursor (⌘-click, ⌥-drag rectangles), selection-match
   *  highlighting, Tab to indent (Esc then Tab still moves focus), ⌥⌘F for
   *  replace. Captured at mount. Default false. */
  ide?: boolean;
  /** Committed text for the change gutter + overview ruler. `undefined` turns
   *  the gutter off; `null` shows the gutter with no baseline (a new file). */
  changeBaseline?: string | null;
  /** Called (at most once per frame) when the cursor, selection or doc moves. */
  onCursorChange?: (info: CursorInfo) => void;
  /** Surface-specific CM6 extensions composed AFTER SS-14 defaults. */
  customExtensions?: Extension[];
  /** RF-perf-12: invoked with a lightweight `{scrollTop, selection}` snapshot in
   *  the view's unmount cleanup, BEFORE the CM view is destroyed — so the read
   *  is reliable (a parent-component cleanup runs AFTER destroy and would read
   *  null). Lets a host persist cursor/scroll across an unmount→remount. */
  onBeforeDestroy?: (snap: ViewSnapshot) => void;
}

export interface UseCodeEditorResult {
  handle: CodeEditorHandle | null;
  loadState: LoadState;
  loadError: Error | null;
  /** Re-invoke `getCodeMirror()`; the loader is a singleton that retries on rejection. */
  retry: () => void;
}

interface ViewRefs {
  view: CmEditorView;
  cm: CmModule;
  themeCompartment: InstanceType<CmModule["Compartment"]>;
  languageCompartment: InstanceType<CmModule["Compartment"]>;
  readOnlyCompartment: InstanceType<CmModule["Compartment"]>;
  guttersCompartment: InstanceType<CmModule["Compartment"]>;
  lineWrapCompartment: InstanceType<CmModule["Compartment"]>;
  foldCompartment: InstanceType<CmModule["Compartment"]>;
  whitespaceCompartment: InstanceType<CmModule["Compartment"]>;
  tabSizeCompartment: InstanceType<CmModule["Compartment"]>;
  customCompartment: InstanceType<CmModule["Compartment"]>;
  placeholderCompartment: InstanceType<CmModule["Compartment"]>;
  lineSeparatorCompartment: InstanceType<CmModule["Compartment"]>;
  changeGutterCompartment: InstanceType<CmModule["Compartment"]>;
  /** Whether the host opted into find (`search`); `openSearch` is a no-op without it. */
  searchEnabled: boolean;
  /**
   * E3-H2: stable, non-compartment extensions captured at mount time —
   * drawSelection, highlightActiveLine, saveBinding, keymap,
   * updateListener, blurListener. These are closures that survive across
   * state rebuilds (clearHistory) and don't carry per-state config.
   * History is intentionally NOT in this list — clearHistory rebuilds it
   * fresh via `cm.history()` to wipe undo/redo.
   */
  stableExtensions: Extension[];
}

function buildLanguageExtensions(cm: CmModule, language: LanguageId): Extension[] {
  const ext = buildLanguageExtension(cm, language);
  return ext ? [ext] : [];
}

function buildGuttersExtensions(cm: CmModule, gutters: boolean): Extension[] {
  return gutters ? [cm.lineNumbers()] : [];
}

// SW-VE-8: soft line-wrapping. `EditorView.lineWrapping` is a static theme
// extension on the already-facaded `EditorView` — no cm-loader change needed.
function buildLineWrapExtensions(cm: CmModule, lineWrap: boolean): Extension[] {
  return lineWrap ? [cm.EditorView.lineWrapping] : [];
}

// SW-VE-11: code folding — the fold gutter rail + the fold/unfold keymap. The
// keymap lives inside the compartment so it's absent (no stray bindings) when
// folding resolves off.
function buildFoldingExtensions(cm: CmModule, folding: boolean): Extension[] {
  return folding ? [cm.foldGutter(), cm.keymap.of(cm.foldKeymap)] : [];
}

// SW-VE-11: render tabs/spaces as glyphs.
function buildWhitespaceExtensions(cm: CmModule, showWhitespace: boolean): Extension[] {
  return showWhitespace ? [cm.highlightWhitespace()] : [];
}

// SW-VE-11: indent width — `indentUnit` drives inserted-indent width, and
// `EditorState.tabSize` the render width of a literal hard tab. Both track the
// setting so 2/4/8 is consistent whether the file uses spaces or tabs.
//
// Defense-in-depth at the CM6 boundary: `tabSize` is a public `number` prop, so
// clamp to a sane positive integer before `" ".repeat()` — an out-of-range value
// (negative / NaN / huge) would otherwise throw a RangeError and break the view.
// Every caller funnels through here, so this one guard is crash-safe for all.
function buildTabSizeExtensions(cm: CmModule, tabSize: number, useTabs = false): Extension[] {
  const safe = Number.isFinite(tabSize) ? Math.min(16, Math.max(1, Math.floor(tabSize))) : 2;
  return [cm.indentUnit.of(useTabs ? "\t" : " ".repeat(safe)), cm.EditorState.tabSize.of(safe)];
}

export function buildLineSeparatorExtensions(cm: CmModule, sep: "\n" | "\r\n"): Extension[] {
  if (sep !== "\r\n") return [];
  return [cm.EditorState.lineSeparator.of("\r\n"), crlfInsertFilter(cm)];
}

/** Under a CRLF separator CodeMirror splits inserted text on `\r\n` only, so a
 *  pasted or typed LF (or lone CR) text would keep its breaks INSIDE lines and the
 *  save would write mixed endings. Turn every stray `\n`/`\r` an insertion
 *  carries into a real line break. A line break and the stray character are
 *  both one position long, so the transaction's selection still applies. */
function crlfInsertFilter(cm: CmModule): Extension {
  return cm.EditorState.transactionFilter.of((tr) => {
    if (!tr.docChanged) return tr;
    // Programmatic syncs (value prop, handle.setValue) carry addToHistory=false
    // and load the file's bytes as-is, exactly like the initial mount: a lone CR
    // or LF inside a line round-trips unchanged rather than being split only on
    // a reload. Only user input (typing, paste, drop) is normalised.
    if (tr.annotation(cm.Transaction.addToHistory) === false) return tr;
    // Undo/redo put back text the document already held (a lone CR included):
    // bytes the user never typed must not be rewritten on the way back.
    if (tr.isUserEvent("undo") || tr.isUserEvent("redo")) return tr;
    let stray = false;
    tr.changes.iterChanges((_fa, _ta, _fb, _tb, inserted) => {
      if (stray) return;
      for (const line of inserted.iterLines()) {
        if (/[\r\n]/.test(line)) {
          stray = true;
          break;
        }
      }
    });
    if (!stray) return tr;
    const changes: { from: number; to: number; insert: InstanceType<CmModule["Text"]> }[] = [];
    tr.changes.iterChanges((fromA, toA, _fb, _tb, inserted) => {
      const lines: string[] = [];
      for (const line of inserted.iterLines()) lines.push(...line.split(/[\r\n]/));
      changes.push({ from: fromA, to: toA, insert: cm.Text.of(lines.length ? lines : [""]) });
    });
    const userEvent = tr.annotation(cm.Transaction.userEvent);
    const addToHistory = tr.annotation(cm.Transaction.addToHistory);
    return {
      changes,
      selection: tr.selection,
      effects: tr.effects,
      scrollIntoView: tr.scrollIntoView,
      annotations: [
        ...(userEvent != null ? [cm.Transaction.userEvent.of(userEvent)] : []),
        ...(addToHistory != null ? [cm.Transaction.addToHistory.of(addToHistory)] : []),
      ],
      filter: false,
    };
  });
}

function buildChangeGutterExtensions(cm: CmModule, enabled: boolean): Extension[] {
  return enabled ? [getChangeGutter(cm).extension] : [];
}

// The find/replace panel, in the app's words: sentence case, and labels that
// say what the toggles do. The class scopes the panel's flex layout + case in
// the theme to this editor (the merge view's panel keeps its own).
const SEARCH_PHRASES: Record<string, string> = {
  Find: "Find",
  Replace: "Replace",
  next: "Next",
  previous: "Previous",
  all: "All",
  "match case": "Match case",
  regexp: "Regex",
  "by word": "Whole word",
  replace: "Replace",
  "replace all": "Replace all",
  close: "Close",
};

function buildSearchExtensions(cm: CmModule): Extension[] {
  return [
    cm.search({ top: true }),
    cm.EditorState.phrases.of(SEARCH_PHRASES),
    cm.EditorView.editorAttributes.of({ class: "cs-editor-search" }),
  ];
}

// The worktree editor's full-editor affordances, one bundle so every host that
// opts in gets the same set.
function buildIdeExtensions(cm: CmModule, searchEnabled: boolean): Extension[] {
  return [
    cm.bracketMatching(),
    cm.closeBrackets(),
    cm.indentOnInput(),
    cm.EditorState.allowMultipleSelections.of(true),
    cm.rectangularSelection(),
    cm.crosshairCursor(),
    cm.highlightSelectionMatches(),
    cm.keymap.of([
      ...cm.closeBracketsKeymap,
      // Tab indents. CodeMirror's tab-focus escape hatch keeps keyboard users
      // unstuck: Esc then Tab (or ⌃M to toggle) moves focus out as usual.
      cm.indentWithTab,
      // Find-and-replace only where find is on: without the `search` option
      // there is no configured panel to open.
      ...(searchEnabled ? [{ key: "Mod-Alt-f", run: cm.openSearchPanel }] : []),
    ]),
  ];
}

function cursorInfoOf(cm: CmModule, view: CmEditorView, language: LanguageId): CursorInfo {
  const state = view.state;
  const main = state.selection.main;
  const line = state.doc.lineAt(main.head);
  let selectedChars = 0;
  for (const r of state.selection.ranges) selectedChars += r.to - r.from;
  let scope: string[] | null = null;
  const mode = breadcrumbModeFor(language);
  if (mode !== "none") {
    try {
      scope = resolveScopeChain(
        cm.syntaxTree(state) as unknown as ScopeTree,
        main.head,
        (from, to) => state.doc.sliceString(from, to),
        mode,
      );
    } catch {
      scope = null;
    }
  }
  return {
    line: line.number,
    column: main.head - line.from + 1,
    selections: state.selection.ranges.length,
    selectedChars,
    scope,
    totalLines: state.doc.lines,
  };
}

function buildThemeExtension(cm: CmModule, mode: "light" | "dark"): Extension {
  return mode === "light" ? lightTheme(cm) : darkTheme(cm);
}

function buildPlaceholderExtensions(cm: CmModule, text: string | undefined): Extension[] {
  return text ? [cm.placeholder(text)] : [];
}

export function useCodeEditor(options: UseCodeEditorOptions): UseCodeEditorResult {
  const {
    container,
    value,
    onChange,
    onSave,
    onLoadError,
    readOnly = false,
    dirty = false,
    theme,
    fontSize,
    gutters,
    lineWrap = false,
    language,
    placeholder,
    search,
    folding = false,
    showWhitespace = false,
    tabSize = 2,
    fontFamily = MONO_FONT_STACK,
    indentWithTabs = false,
    lineSeparator = "\n",
    ide = false,
    changeBaseline,
    onCursorChange,
    customExtensions,
    onBeforeDestroy,
  } = options;

  // Mutable refs that closures inside the editor's extensions can read at
  // the right moment (CM6 extensions outlive React renders).
  const onChangeRef = useRef<typeof onChange>(onChange);
  const onSaveRef = useRef<typeof onSave>(onSave);
  const readOnlyRef = useRef(readOnly);
  const dirtyRef = useRef(dirty);
  const inFlightSaveRef = useRef<Promise<void> | null>(null);
  // E2-H1 + E3-H1: this ref tracks values emitted via onChange to the
  // host — i.e. the latest value the host KNOWS about. Updated ONLY in
  // the user-input branch of updateListener (NOT for programmatic
  // dispatches). The iter-1 code updated it unconditionally (which made
  // its H-2 guard dead code and caused handle.setValue to be reverted);
  // iter-2 fixed the assignment placement but added a userEditedSinceSync
  // flag that stranded the host on transform/sanitize. iter-3 drops the
  // flag and accepts that React 18's automatic batching aligns commits
  // closely enough that the H-2 "stale prop wipes user input" race
  // doesn't manifest with synchronously-updated host state (codesense's
  // workbench uses Zustand). Hosts that genuinely debounce / transform
  // onChange should use `handle.setValue()` per the contract in README.
  const lastEmittedValueRef = useRef<string>(value);
  // H-1 patch: read latest `value` inside async `getCodeMirror().then` so the
  // initial doc reflects any prop changes that happened during the loader
  // window. The mount-effect closure would otherwise capture a stale value.
  const valueRef = useRef<string>(value);
  // Set true around programmatic dispatches (value-sync, handle.setValue) so
  // the updateListener doesn't fire onChange and bounce the change back to
  // the host's state.
  const programmaticDispatchRef = useRef(false);
  // M-3 patch: remember the last applied `customExtensions` array by
  // identity, so the reconfigure effect can skip same-reference dispatches
  // when hosts pass inline arrays from re-renders.
  const lastCustomExtensionsRef = useRef<Extension[] | undefined>(customExtensions);
  // RF-perf-12: latest onBeforeDestroy read at teardown (the mount effect's
  // cleanup closure is created once, so it must reach the current callback).
  const onBeforeDestroyRef = useRef<typeof onBeforeDestroy>(onBeforeDestroy);
  const onCursorChangeRef = useRef<typeof onCursorChange>(onCursorChange);
  const languageRef = useRef<LanguageId>(language);
  // The latest change-gutter baseline, re-applied after any state rebuild
  // (clearHistory / setState) since the gutter's field starts empty.
  const changeBaselineRef = useRef<string | null | undefined>(changeBaseline);
  // Latest format props for the async mount: CodeMirror loads lazily, and a
  // prop that settled while it loaded must not be built from a stale closure
  // (a prop-sync effect that ran before the view existed did nothing).
  const formatRef = useRef({ lineSeparator, tabSize, indentWithTabs });

  // Keep refs in sync with latest props.
  onChangeRef.current = onChange;
  onSaveRef.current = onSave;
  readOnlyRef.current = readOnly;
  dirtyRef.current = dirty;
  valueRef.current = value;
  onBeforeDestroyRef.current = onBeforeDestroy;
  onCursorChangeRef.current = onCursorChange;
  languageRef.current = language;
  changeBaselineRef.current = changeBaseline;
  formatRef.current = { lineSeparator, tabSize, indentWithTabs };

  const refs = useRef<ViewRefs | null>(null);
  const [handle, setHandle] = useState<CodeEditorHandle | null>(null);
  const [loadState, setLoadState] = useState<LoadState>("loading");
  const [loadError, setLoadError] = useState<Error | null>(null);
  const [retryToken, setRetryToken] = useState(0);

  // CM6 lifecycle: construct view when container + cm-module are both ready,
  // tear down on unmount.
  useEffect(() => {
    if (!container) return;
    let cancelled = false;

    setLoadState("loading");
    setLoadError(null);

    getCodeMirror()
      .then((cm) => {
        if (cancelled) return;

        const themeCompartment = new cm.Compartment();
        const languageCompartment = new cm.Compartment();
        const readOnlyCompartment = new cm.Compartment();
        const guttersCompartment = new cm.Compartment();
        const lineWrapCompartment = new cm.Compartment();
        const foldCompartment = new cm.Compartment();
        const whitespaceCompartment = new cm.Compartment();
        const tabSizeCompartment = new cm.Compartment();
        const customCompartment = new cm.Compartment();
        const placeholderCompartment = new cm.Compartment();
        const lineSeparatorCompartment = new cm.Compartment();
        const changeGutterCompartment = new cm.Compartment();
        // SW-VE-8: find/replace is static per instance — captured at mount.
        const searchEnabled = search === true;
        const ideEnabled = ide === true;

        // Cursor readout, coalesced to one report per frame so a held arrow key
        // or a large paste doesn't re-render the host on every transaction.
        let cursorFrame: number | null = null;
        const cursorListener = cm.EditorView.updateListener.of((u: CmViewUpdate) => {
          if (!onCursorChangeRef.current) return;
          if (!u.selectionSet && !u.docChanged && !u.focusChanged) return;
          if (cursorFrame != null) return;
          const view = u.view;
          cursorFrame = requestAnimationFrame(() => {
            cursorFrame = null;
            if (refs.current?.view !== view) return;
            onCursorChangeRef.current?.(cursorInfoOf(cm, view, languageRef.current));
          });
        });

        const updateListener = cm.EditorView.updateListener.of((u: CmViewUpdate) => {
          if (!u.docChanged) return;
          // Skip BEFORE updating any tracking refs when the dispatch is
          // programmatic — those refs are only meaningful for user-driven
          // input.
          if (programmaticDispatchRef.current) return;
          const next = u.state.sliceDoc();
          lastEmittedValueRef.current = next;
          onChangeRef.current?.(next);
        });

        const blurListener = cm.EditorView.domEventHandlers({
          blur: (_event: FocusEvent, view: CmEditorView) => {
            commitTextEditOnBlur(view.contentDOM, () => {
              const cb = onSaveRef.current;
              if (!cb) return false;
              if (readOnlyRef.current) return false;
              if (!dirtyRef.current) return false;
              if (inFlightSaveRef.current) return false; // dedup
              const v = view.state.sliceDoc();
              // E2-H2 patch: defer the onSave call into a microtask via
              // `Promise.resolve().then(...)` so a *synchronous* throw from
              // the host's callback becomes a rejection that .catch handles.
              // The iter-1 `Promise.resolve(cb(v))` form evaluated cb(v)
              // immediately and let sync throws escape into CM6's DOM event
              // dispatch. Host owns error UX per D-9; editor must not leak.
              const p = Promise.resolve()
                .then(() => cb(v))
                .catch(() => {})
                .finally(() => {
                  if (inFlightSaveRef.current === p) inFlightSaveRef.current = null;
                });
              inFlightSaveRef.current = p;
            });
            return false;
          },
        });

        const saveBinding = buildSaveKeybinding(cm, {
          getOnSave: () => onSaveRef.current,
          getReadOnly: () => readOnlyRef.current,
          getInFlight: () => inFlightSaveRef.current,
          setInFlight: (p) => {
            inFlightSaveRef.current = p;
          },
        });

        // M-4 patch: `indentWithTab` is intentionally NOT in the default
        // keymap. Binding Tab to indent traps keyboard focus inside the
        // editor (a11y regression). Hosts that want tab-indent compose
        // their own via `customExtensions` (or rebind via Mod-]).
        //
        // E3-H2: split stable (non-compartment, non-history) extensions
        // from compartments and history so clearHistory can rebuild
        // state with FRESH history but LIVE compartment values.
        const stableExtensions: Extension[] = [
          cm.drawSelection(),
          cm.highlightActiveLine(),
          // Keymaps after defaults so saveBinding sees Cmd+S first. SW-VE-8:
          // searchKeymap (⌘F/⌘G/Esc) only when the host opted into find.
          saveBinding,
          cm.keymap.of([
            ...cm.defaultKeymap,
            ...(searchEnabled ? cm.searchKeymap : []),
            ...cm.historyKeymap,
          ]),
          updateListener,
          cursorListener,
          blurListener,
          // SW-VE-8: the find/replace panel (top-docked). Match highlighting +
          // the panel; the keymap above drives it. Off unless opted in.
          ...(searchEnabled ? buildSearchExtensions(cm) : []),
          ...(ideEnabled ? buildIdeExtensions(cm, searchEnabled) : []),
        ];
        const initialExtensions: Extension[] = [
          cm.history(),
          ...stableExtensions,
          guttersCompartment.of(buildGuttersExtensions(cm, gutters)),
          lineWrapCompartment.of(buildLineWrapExtensions(cm, lineWrap)),
          foldCompartment.of(buildFoldingExtensions(cm, folding)),
          whitespaceCompartment.of(buildWhitespaceExtensions(cm, showWhitespace)),
          tabSizeCompartment.of(
            buildTabSizeExtensions(cm, formatRef.current.tabSize, formatRef.current.indentWithTabs),
          ),
          lineSeparatorCompartment.of(
            buildLineSeparatorExtensions(cm, formatRef.current.lineSeparator),
          ),
          changeGutterCompartment.of(
            buildChangeGutterExtensions(cm, changeBaselineRef.current !== undefined),
          ),
          // Latest-prop refs, like the format props above: a language or
          // read-only change while CodeMirror loaded must not be lost.
          languageCompartment.of(buildLanguageExtensions(cm, languageRef.current)),
          themeCompartment.of(buildThemeExtension(cm, theme)),
          readOnlyCompartment.of(cm.EditorState.readOnly.of(readOnlyRef.current)),
          placeholderCompartment.of(buildPlaceholderExtensions(cm, placeholder)),
          customCompartment.of(customExtensions ?? []),
        ];

        // H-1 patch: read latest value from ref (not closure-captured) so
        // a prop change during the async load isn't lost.
        const initialDoc = valueRef.current;
        const startState = cm.EditorState.create({
          doc: initialDoc,
          extensions: initialExtensions,
        });
        lastEmittedValueRef.current = initialDoc;

        const view = new cm.EditorView({
          state: startState,
          parent: container,
        });

        // ARIA + color-scheme on the editor root (D-20, D-21).
        view.dom.style.colorScheme = theme;
        // SW-VE-11: font-family is host-resolved (mono for code, a reading stack
        // for prose when the prose-font setting is on) and live-synced below.
        view.dom.style.fontFamily = fontFamily;
        view.dom.style.fontSize = `${fontSize}px`;
        view.dom.setAttribute("aria-readonly", String(readOnlyRef.current));

        refs.current = {
          view,
          cm,
          themeCompartment,
          languageCompartment,
          readOnlyCompartment,
          guttersCompartment,
          lineWrapCompartment,
          foldCompartment,
          whitespaceCompartment,
          tabSizeCompartment,
          customCompartment,
          placeholderCompartment,
          lineSeparatorCompartment,
          changeGutterCompartment,
          searchEnabled,
          stableExtensions,
        };
        applyChangeBaseline(cm, view, changeBaselineRef.current);
        onCursorChangeRef.current?.(cursorInfoOf(cm, view, languageRef.current));

        setHandle(
          buildHandle(refs, programmaticDispatchRef, lastEmittedValueRef, () =>
            applyChangeBaseline(cm, view, changeBaselineRef.current),
          ),
        );
        setLoadState("ready");
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        const e = err instanceof Error ? err : new Error(String(err));
        setLoadError(e);
        setLoadState("error");
        onLoadError?.(e);
      });

    return () => {
      cancelled = true;
      const r = refs.current;
      if (r) {
        // RF-perf-12: capture a lightweight snapshot while the view is still
        // alive — a host's own cleanup runs AFTER this (child-before-parent)
        // and would read a destroyed view. Guarded so a throwing host callback
        // can't leak into React's commit phase / block `view.destroy()`.
        const cb = onBeforeDestroyRef.current;
        if (cb) {
          try {
            cb(snapshotOf(r.view));
          } catch {
            /* host owns error UX; never block teardown */
          }
        }
        r.view.destroy();
        refs.current = null;
      }
      setHandle(null);
      // M-5 patch: do NOT reset lastEmittedValueRef on cleanup. The next
      // mount-effect path writes it explicitly from `valueRef.current`; an
      // intermediate `""` would cause a brief mismatch under React 18
      // StrictMode double-invocation.
    };
    // We deliberately omit value/theme/etc. — they're synced via separate
    // effects below. The view is created once per container + retry cycle.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [container, retryToken]);

  // ──────────────────────────────────────────────────────────────────────
  // Prop-sync effects: each reconfigures a compartment when the relevant
  // prop changes, without rebuilding the whole view.
  // ──────────────────────────────────────────────────────────────────────

  // value (D-15): sync external prop changes via a transaction; don't
  // re-emit our own onChange echo.
  //
  // E3-H1: simplified to "always sync from prop". The iter-1 H-2 guard
  // was dead code; the iter-2 userEditedSinceSyncRef flag stranded the
  // host on transform/sanitize/debounce. The honest contract is: if the
  // host's `value` prop differs from what we've emitted AND from the
  // current doc, we dispatch the prop. Trade-off: fast typing on a slow
  // host (one that doesn't synchronously update value via setState in
  // onChange) can race — a stale prop render between user input and
  // host commit could wipe an un-flushed keystroke. React 18's
  // automatic batching keeps this from manifesting in practice for
  // hosts that update synchronously (codesense's Zustand-driven
  // workbench). Hosts that genuinely transform/debounce should use
  // `handle.setValue()` instead of the controlled-prop path.
  // line separator: follows the file (a CRLF file stays CRLF on save). Declared
  // BEFORE the value sync so a reload that changes both the text and its line
  // endings compares (and splits) the new text under the new separator.
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    r.view.dispatch({
      effects: r.lineSeparatorCompartment.reconfigure(
        buildLineSeparatorExtensions(r.cm, lineSeparator),
      ),
    });
  }, [lineSeparator]);

  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    if (value === lastEmittedValueRef.current) return;
    const current = r.view.state.sliceDoc();
    if (current === value) {
      lastEmittedValueRef.current = value;
      return;
    }
    programmaticDispatchRef.current = true;
    try {
      r.view.dispatch({
        // `to` is a doc position (a line break counts 1), not `current.length`:
        // under CRLF the sliced string is longer than the doc.
        changes: { from: 0, to: r.view.state.doc.length, insert: value },
        // H-3 patch: external prop syncs must NOT enter the undo stack.
        // Otherwise Cmd+Z reverses an external write as if the user typed it.
        annotations: [r.cm.Transaction.addToHistory.of(false)],
      });
    } finally {
      programmaticDispatchRef.current = false;
    }
    lastEmittedValueRef.current = value;
  }, [value]);

  // theme + color-scheme (D-20)
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    r.view.dispatch({
      effects: r.themeCompartment.reconfigure(buildThemeExtension(r.cm, theme)),
    });
    r.view.dom.style.colorScheme = theme;
  }, [theme]);

  // language (D-17)
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    r.view.dispatch({
      effects: r.languageCompartment.reconfigure(buildLanguageExtensions(r.cm, language)),
    });
  }, [language]);

  // readOnly + aria-readonly (D-21)
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    r.view.dispatch({
      effects: r.readOnlyCompartment.reconfigure(r.cm.EditorState.readOnly.of(readOnly)),
    });
    r.view.dom.setAttribute("aria-readonly", String(readOnly));
  }, [readOnly]);

  // gutters (D-22)
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    r.view.dispatch({
      effects: r.guttersCompartment.reconfigure(buildGuttersExtensions(r.cm, gutters)),
    });
  }, [gutters]);

  // lineWrap (SW-VE-8): live-reconfigure so toggling the setting applies to an
  // open editor with no reload/remount (AC-10).
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    r.view.dispatch({
      effects: r.lineWrapCompartment.reconfigure(buildLineWrapExtensions(r.cm, lineWrap)),
    });
  }, [lineWrap]);

  // folding (SW-VE-11): live-reconfigure so toggling the setting adds/removes the
  // fold gutter + keymap on an open editor with no remount.
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    r.view.dispatch({
      effects: r.foldCompartment.reconfigure(buildFoldingExtensions(r.cm, folding)),
    });
  }, [folding]);

  // whitespace rendering (SW-VE-11): live-reconfigure.
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    r.view.dispatch({
      effects: r.whitespaceCompartment.reconfigure(buildWhitespaceExtensions(r.cm, showWhitespace)),
    });
  }, [showWhitespace]);

  // tab size (SW-VE-11): live-reconfigure indentUnit + tabSize together.
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    r.view.dispatch({
      effects: r.tabSizeCompartment.reconfigure(
        buildTabSizeExtensions(r.cm, tabSize, indentWithTabs),
      ),
    });
  }, [tabSize, indentWithTabs]);

  // change gutter: on/off follows whether a baseline is tracked at all; the
  // committed text itself rides an effect so a refetch never rebuilds the gutter.
  const gutterEnabled = changeBaseline !== undefined;
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    r.view.dispatch({
      effects: r.changeGutterCompartment.reconfigure(
        buildChangeGutterExtensions(r.cm, gutterEnabled),
      ),
    });
    applyChangeBaseline(r.cm, r.view, changeBaselineRef.current);
  }, [gutterEnabled]);
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    applyChangeBaseline(r.cm, r.view, changeBaseline);
  }, [changeBaseline]);

  // placeholder
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    r.view.dispatch({
      effects: r.placeholderCompartment.reconfigure(buildPlaceholderExtensions(r.cm, placeholder)),
    });
  }, [placeholder]);

  // customExtensions — M-3 + E2-M2 patch: shallow-compare against last
  // applied value so hosts passing inline arrays don't thrash the
  // compartment. The iter-1 form short-circuited the every() check when
  // either side was undefined, so prev=undefined vs next=[] dispatched
  // unconditionally. Normalize to [] before comparison so empty and
  // undefined are treated as equivalent.
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    const prev = lastCustomExtensionsRef.current;
    const next = customExtensions;
    if (prev === next) return;
    const prevArr = prev ?? [];
    const nextArr = next ?? [];
    if (prevArr.length === nextArr.length && prevArr.every((e, i) => e === nextArr[i])) {
      lastCustomExtensionsRef.current = next;
      return;
    }
    r.view.dispatch({
      effects: r.customCompartment.reconfigure(nextArr),
    });
    lastCustomExtensionsRef.current = next;
  }, [customExtensions]);

  // fontSize (inline style; no compartment needed)
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    r.view.dom.style.fontSize = `${fontSize}px`;
  }, [fontSize]);

  // fontFamily (SW-VE-11): inline style, mirroring fontSize — live-sync the
  // prose-vs-mono reading font without a remount.
  useEffect(() => {
    const r = refs.current;
    if (!r) return;
    r.view.dom.style.fontFamily = fontFamily;
  }, [fontFamily]);

  const retry = (): void => {
    setRetryToken((n) => n + 1);
  };

  return { handle, loadState, loadError, retry };
}

// ──────────────────────────────────────────────────────────────────────
// Imperative handle factory (D-18 + AC-4)
// ──────────────────────────────────────────────────────────────────────

/**
 * RF-perf-12: read a LIGHTWEIGHT view snapshot (scroll + selection offsets) off
 * a live CM view. Shared by `getViewSnapshot` and the unmount-cleanup capture so
 * they can't drift. Holds only plain numbers — never the EditorState (AC-8).
 */
function snapshotOf(view: CmEditorView): ViewSnapshot {
  const sel = view.state.selection.main;
  return {
    scrollTop: view.scrollDOM.scrollTop,
    selection: { anchor: sel.anchor, head: sel.head },
  };
}

/** Push the committed text into the change gutter (no-op while it's off). */
function applyChangeBaseline(
  cm: CmModule,
  view: CmEditorView,
  baseline: string | null | undefined,
): void {
  if (baseline === undefined) return;
  const gutter = getChangeGutter(cm);
  view.dispatch({ effects: gutter.setBaseline.of(baseline) });
}

function buildHandle(
  refs: React.MutableRefObject<ViewRefs | null>,
  programmaticDispatchRef: React.MutableRefObject<boolean>,
  lastEmittedValueRef: React.MutableRefObject<string>,
  reapplyBaseline: () => void,
): CodeEditorHandle {
  // M-2 patch: handle methods are safe to call after the view has been
  // destroyed (unmount, retry). Reads return sensible defaults; writes
  // become no-ops. Hosts that want to react to the dead handle should
  // check `loadState !== "ready"` before calling.
  const readView = (): ViewRefs | null => refs.current;

  return {
    getValue: () => readView()?.view.state.sliceDoc() ?? "",
    setValue: (next) => {
      const r = readView();
      if (!r) return;
      programmaticDispatchRef.current = true;
      try {
        r.view.dispatch({
          changes: { from: 0, to: r.view.state.doc.length, insert: next },
          // H-3 patch: handle.setValue is a programmatic content reset;
          // must not enter the undo stack.
          annotations: [r.cm.Transaction.addToHistory.of(false)],
        });
      } finally {
        programmaticDispatchRef.current = false;
      }
    },
    clearHistory: () => {
      const r = readView();
      if (!r) return;
      // E3-H2 patch: rebuild state with FRESH history() but LIVE
      // compartment values read via `Compartment.get(state)`. The iter-2
      // implementation reused the mount-time `extensions` array, which
      // baked in each compartment's `.of(initialValue)` — so any
      // post-mount theme/language/readOnly/gutters/customExtensions
      // change was silently reverted on clearHistory.
      const s = r.view.state;
      const live = (
        compartment: InstanceType<CmModule["Compartment"]>,
        fallback: Extension,
      ): Extension => {
        const cur = compartment.get(s);
        return cur ?? fallback;
      };
      const fresh = r.cm.EditorState.create({
        doc: r.view.state.doc,
        selection: r.view.state.selection,
        extensions: [
          r.cm.history(), // fresh — the point of clearHistory
          ...r.stableExtensions,
          // Compartments wrapped with their current live extensions so
          // post-mount reconfigures are preserved across the rebuild.
          r.guttersCompartment.of(live(r.guttersCompartment, [])),
          r.lineWrapCompartment.of(live(r.lineWrapCompartment, [])),
          r.foldCompartment.of(live(r.foldCompartment, [])),
          r.whitespaceCompartment.of(live(r.whitespaceCompartment, [])),
          r.tabSizeCompartment.of(live(r.tabSizeCompartment, [])),
          r.languageCompartment.of(live(r.languageCompartment, [])),
          r.themeCompartment.of(live(r.themeCompartment, [])),
          r.readOnlyCompartment.of(
            live(r.readOnlyCompartment, r.cm.EditorState.readOnly.of(false)),
          ),
          r.placeholderCompartment.of(live(r.placeholderCompartment, [])),
          r.lineSeparatorCompartment.of(live(r.lineSeparatorCompartment, [])),
          r.changeGutterCompartment.of(live(r.changeGutterCompartment, [])),
          r.customCompartment.of(live(r.customCompartment, [])),
        ],
      });
      r.view.setState(fresh);
      reapplyBaseline();
    },
    getState: () => {
      // A-1 patch: return null when the view is gone (unmount, retry).
      // Consistent with the rest of the null-safe handle surface (M-2)
      // and with the D-18 tab-switch save-on-unmount use case where the
      // host calls getState during its own teardown. Hosts gate via
      // `if (s) ...` before passing to setState.
      const r = readView();
      return r ? r.view.state : null;
    },
    setState: (state) => {
      const r = readView();
      if (!r) return;
      r.view.setState(state);
      reapplyBaseline();
      // E3-M1 patch: sync lastEmittedValueRef to the new state's doc so
      // the next render's value-sync effect operates against the correct
      // baseline. For the documented tab-switch use case the host
      // updates its `value` prop in lockstep, so the prop matches the
      // ref and the effect early-bails (no spurious dispatch).
      lastEmittedValueRef.current = state.sliceDoc();
    },
    focus: () => {
      readView()?.view.focus();
    },
    blur: () => {
      readView()?.view.contentDOM.blur();
    },
    getCursor: () => {
      const r = readView();
      if (!r) return { line: 0, ch: 0 };
      const head = r.view.state.selection.main.head;
      const line = r.view.state.doc.lineAt(head);
      return { line: line.number - 1, ch: head - line.from };
    },
    setSelection: (range) => {
      const r = readView();
      if (!r) return;
      // L-8 patch: clamp line + ch to valid ranges so out-of-bounds input
      // (e.g. restoring a saved selection after the doc shrank) doesn't
      // throw RangeError from doc.line().
      const doc = r.view.state.doc;
      const lineCount = doc.lines;
      const clampLine = (n: number): number => Math.max(0, Math.min(lineCount - 1, Math.floor(n)));
      const fromLineIdx = clampLine(range.from.line);
      const toLineIdx = clampLine(range.to.line);
      const fromLine = doc.line(fromLineIdx + 1);
      const toLine = doc.line(toLineIdx + 1);
      const fromCh = Math.max(0, Math.min(fromLine.length, Math.floor(range.from.ch)));
      const toCh = Math.max(0, Math.min(toLine.length, Math.floor(range.to.ch)));
      const from = fromLine.from + fromCh;
      const to = toLine.from + toCh;
      r.view.dispatch({ selection: { anchor: from, head: to } });
    },
    getScrollPos: () => readView()?.view.scrollDOM.scrollTop ?? 0,
    scrollTo: (pos) => {
      const r = readView();
      if (!r) return;
      // L-9 patch: reject NaN / Infinity / negative; clamp to 0+.
      const safe = Math.max(0, Math.floor(Number.isFinite(pos) ? pos : 0));
      r.view.scrollDOM.scrollTop = safe;
    },
    getViewSnapshot: () => {
      const r = readView();
      return r ? snapshotOf(r.view) : null;
    },
    applyViewSnapshot: (snap) => {
      const r = readView();
      if (!r) return;
      if (snap.selection) {
        // Clamp offsets to the current doc — a saved selection may outlive an
        // edit that shrank the file (mirrors setSelection's L-8 guard).
        const len = r.view.state.doc.length;
        const clamp = (n: number): number => Math.max(0, Math.min(len, Math.floor(n)));
        r.view.dispatch({
          selection: { anchor: clamp(snap.selection.anchor), head: clamp(snap.selection.head) },
        });
      }
      if (typeof snap.scrollTop === "number") {
        const safe = Math.max(0, Math.floor(Number.isFinite(snap.scrollTop) ? snap.scrollTop : 0));
        r.view.scrollDOM.scrollTop = safe;
      }
    },
    checkConflict: (diskVersion) => {
      const r = readView();
      if (!r) return false;
      return hasConflict(r.view.state.sliceDoc(), diskVersion);
    },
    // SW-VE-9: `closeSearchPanel` already returns the did-close boolean the
    // handle contract wants — pass it through with no wrapper bookkeeping.
    closeSearch: () => {
      const r = readView();
      return r ? r.cm.closeSearchPanel(r.view) : false;
    },
    isSearchOpen: () => {
      const r = readView();
      return r ? r.cm.searchPanelOpen(r.view.state) : false;
    },
    openSearch: () => {
      const r = readView();
      if (r?.searchEnabled) r.cm.openSearchPanel(r.view);
    },
    revealLine: (line, column = 1) => {
      const r = readView();
      if (!r) return;
      const doc = r.view.state.doc;
      const safeLine = Math.max(1, Math.min(doc.lines, Math.floor(Number(line) || 1)));
      const target = doc.line(safeLine);
      const safeCol = Math.max(1, Math.min(target.length + 1, Math.floor(Number(column) || 1)));
      const pos = target.from + safeCol - 1;
      r.view.dispatch({
        selection: { anchor: pos },
        effects: r.cm.EditorView.scrollIntoView(pos, { y: "center" }),
      });
      r.view.focus();
    },
  };
}
