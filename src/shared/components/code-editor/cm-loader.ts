// SPDX-License-Identifier: MIT
// Lazy loader for CodeMirror 6. Module-level promise singleton with
// retry-on-failure — mirrors the `getD2` pattern in
// `src/features/dashboard/components/markdown-viewer.tsx`.

import type { Extension } from "@codemirror/state";

export type LanguageId =
  | "markdown"
  | "typescript"
  | "javascript"
  | "json"
  | "yaml"
  | "python"
  | "shell"
  | "rust"
  // SW-VE-2: XML via `@codemirror/legacy-modes/mode/xml` (already installed —
  // no new dependency), mirroring the shell/rust legacy-mode precedent.
  | "xml"
  | "css"
  | "html"
  | "go"
  | "toml"
  | "sql"
  | "c"
  | "cpp"
  | "java"
  | "kotlin"
  | "csharp"
  | "swift"
  | "ruby"
  | "lua"
  | "dockerfile"
  | "diff"
  | "properties"
  | "plain";

// Re-exported types from CM6 (types-only — no runtime cost).
export type CmTypes = typeof import("@codemirror/state") &
  typeof import("@codemirror/view") &
  typeof import("@codemirror/commands") &
  typeof import("@codemirror/language");

/**
 * Facade for `@codemirror/merge`. SS-15 used only `MergeView` + the two
 * chunk-navigation commands (split view). SLW-WD-6 adds `unifiedMergeView`
 * for the read-only inline (single-column) diff mode. The full package
 * exports more (`acceptChunk`, `Chunk`, etc.) — expanding the facade stays a
 * one-line additive change for future stories that need more surface.
 */
export interface CmMergeModule {
  MergeView: typeof import("@codemirror/merge").MergeView;
  unifiedMergeView: typeof import("@codemirror/merge").unifiedMergeView;
  goToNextChunk: typeof import("@codemirror/merge").goToNextChunk;
  goToPreviousChunk: typeof import("@codemirror/merge").goToPreviousChunk;
  // The review diff's change navigator reads the chunk list to say "2 of 7".
  getChunks: typeof import("@codemirror/merge").getChunks;
  // SLW-DV-5 (AC-6b): patch the inline `unifiedMergeView` baseline in place when a
  // live edit changes the "before" side, instead of remounting the view.
  getOriginalDoc: typeof import("@codemirror/merge").getOriginalDoc;
  originalDocChangeEffect: typeof import("@codemirror/merge").originalDocChangeEffect;
  // SLW-DV-8 (AC-1): the exported `diff` + `Change` ctor let the ignore-whitespace
  // `diffConfig.override` run the diff over whitespace-normalized line images and
  // return real `Change` instances (which CM's `makePresentable` then aligns).
  diff: typeof import("@codemirror/merge").diff;
  Change: typeof import("@codemirror/merge").Change;
}

export interface CmModule {
  // Core surfaces the editor uses. Field types come from the modules
  // themselves (we re-expose under stable names).
  EditorState: typeof import("@codemirror/state").EditorState;
  EditorView: typeof import("@codemirror/view").EditorView;
  Compartment: typeof import("@codemirror/state").Compartment;
  Transaction: typeof import("@codemirror/state").Transaction;
  Text: typeof import("@codemirror/state").Text;
  // SLW-DV-5 (AC-3): transient flash-on-change decorations — a StateField holds a
  // DecorationSet driven by add/clear StateEffects.
  StateField: typeof import("@codemirror/state").StateField;
  StateEffect: typeof import("@codemirror/state").StateEffect;
  Decoration: typeof import("@codemirror/view").Decoration;
  // SLW-DV-5 (AC-6b): build a ChangeSet to patch the inline merge view's baseline.
  ChangeSet: typeof import("@codemirror/state").ChangeSet;
  keymap: typeof import("@codemirror/view").keymap;
  placeholder: typeof import("@codemirror/view").placeholder;
  lineNumbers: typeof import("@codemirror/view").lineNumbers;
  highlightActiveLine: typeof import("@codemirror/view").highlightActiveLine;
  drawSelection: typeof import("@codemirror/view").drawSelection;
  // SW-VE-11: render tabs/spaces as glyphs (whitespace toggle). Composed only
  // when the `showWhitespace` option is on, via a compartment.
  highlightWhitespace: typeof import("@codemirror/view").highlightWhitespace;
  history: typeof import("@codemirror/commands").history;
  defaultKeymap: typeof import("@codemirror/commands").defaultKeymap;
  historyKeymap: typeof import("@codemirror/commands").historyKeymap;
  indentWithTab: typeof import("@codemirror/commands").indentWithTab;
  // SW-VE-8: in-editor find/replace (⌘F). `search()` provides the panel +
  // match highlighting; `searchKeymap` binds ⌘F → open, ⌘G → next, Esc →
  // close. Composed only where a host opts in (the editable context inset),
  // via the `search` option, so the read-only viewers stay unchanged.
  search: typeof import("@codemirror/search").search;
  searchKeymap: typeof import("@codemirror/search").searchKeymap;
  // SW-VE-9: imperative find-panel control, surfaced through `CodeEditorHandle`
  // so the editable inset can query/close the panel deterministically instead
  // of guessing from focus/DOM. `closeSearchPanel` returns whether it closed a
  // panel; `searchPanelOpen` reads the open state off `EditorState`.
  closeSearchPanel: typeof import("@codemirror/search").closeSearchPanel;
  searchPanelOpen: typeof import("@codemirror/search").searchPanelOpen;
  syntaxHighlighting: typeof import("@codemirror/language").syntaxHighlighting;
  defaultHighlightStyle: typeof import("@codemirror/language").defaultHighlightStyle;
  // SLW-WDE-7: `HighlightStyle` (build the codesense syntax palette) + the Lezer
  // `tags` object (the highlight tokens the palette maps). `tags` lives in
  // `@lezer/highlight`, not `@codemirror/language`.
  HighlightStyle: typeof import("@codemirror/language").HighlightStyle;
  tags: typeof import("@lezer/highlight").tags;
  StreamLanguage: typeof import("@codemirror/language").StreamLanguage;
  // SW-VE-11: code folding (`foldGutter` renders the fold rail, `foldKeymap`
  // binds fold/unfold) and configurable indent width (`indentUnit`). Composed
  // via compartments only when the corresponding option resolves on.
  foldGutter: typeof import("@codemirror/language").foldGutter;
  foldKeymap: typeof import("@codemirror/language").foldKeymap;
  indentUnit: typeof import("@codemirror/language").indentUnit;
  // SLW-DV-8 (AC-2): read the current (possibly partial) parse tree to resolve the
  // scope breadcrumb for the visible range — no full-document parse required.
  syntaxTree: typeof import("@codemirror/language").syntaxTree;
  // The worktree editor's IDE affordances (bracket matching, auto-close,
  // multi-cursor, selection-match highlighting) and the change gutter's
  // primitives. Composed only when a host opts in, so other editors are unchanged.
  bracketMatching: typeof import("@codemirror/language").bracketMatching;
  indentOnInput: typeof import("@codemirror/language").indentOnInput;
  closeBrackets: typeof import("@codemirror/autocomplete").closeBrackets;
  closeBracketsKeymap: typeof import("@codemirror/autocomplete").closeBracketsKeymap;
  rectangularSelection: typeof import("@codemirror/view").rectangularSelection;
  crosshairCursor: typeof import("@codemirror/view").crosshairCursor;
  highlightSelectionMatches: typeof import("@codemirror/search").highlightSelectionMatches;
  openSearchPanel: typeof import("@codemirror/search").openSearchPanel;
  gutter: typeof import("@codemirror/view").gutter;
  GutterMarker: typeof import("@codemirror/view").GutterMarker;
  ViewPlugin: typeof import("@codemirror/view").ViewPlugin;
  RangeSet: typeof import("@codemirror/state").RangeSet;
  EditorSelection: typeof import("@codemirror/state").EditorSelection;

  // Language factories. Each returns an Extension; markdown/javascript take
  // optional configs the editor may pass through.
  languages: {
    markdown: () => Extension;
    typescript: () => Extension; // typescript: true, jsx: true (covers .ts and .tsx)
    javascript: () => Extension; // jsx: true (covers .js and .jsx)
    json: () => Extension;
    yaml: () => Extension;
    python: () => Extension;
    shell: () => Extension;
    rust: () => Extension;
    xml: () => Extension; // SW-VE-2: legacy-mode XML (StreamLanguage)
    css: () => Extension;
    html: () => Extension;
    go: () => Extension;
    toml: () => Extension;
    sql: () => Extension;
    c: () => Extension;
    cpp: () => Extension;
    java: () => Extension;
    kotlin: () => Extension;
    csharp: () => Extension;
    swift: () => Extension;
    ruby: () => Extension;
    lua: () => Extension;
    dockerfile: () => Extension;
    diff: () => Extension;
    properties: () => Extension;
  };
}

let loadPromise: Promise<CmModule> | null = null;

async function loadCmModule(): Promise<CmModule> {
  const [
    state,
    view,
    commands,
    language,
    highlight,
    search,
    autocomplete,
    markdown,
    javascript,
    json,
    yaml,
    python,
    css,
    html,
    shellMode,
    rustMode,
    xmlMode,
    goMode,
    tomlMode,
    sqlMode,
    clikeMode,
    swiftMode,
    rubyMode,
    luaMode,
    dockerfileMode,
    diffMode,
    propertiesMode,
  ] = await Promise.all([
    import("@codemirror/state"),
    import("@codemirror/view"),
    import("@codemirror/commands"),
    import("@codemirror/language"),
    import("@lezer/highlight"),
    import("@codemirror/search"),
    import("@codemirror/autocomplete"),
    import("@codemirror/lang-markdown"),
    import("@codemirror/lang-javascript"),
    import("@codemirror/lang-json"),
    import("@codemirror/lang-yaml"),
    import("@codemirror/lang-python"),
    import("@codemirror/lang-css"),
    import("@codemirror/lang-html"),
    import("@codemirror/legacy-modes/mode/shell"),
    import("@codemirror/legacy-modes/mode/rust"),
    import("@codemirror/legacy-modes/mode/xml"),
    import("@codemirror/legacy-modes/mode/go"),
    import("@codemirror/legacy-modes/mode/toml"),
    import("@codemirror/legacy-modes/mode/sql"),
    import("@codemirror/legacy-modes/mode/clike"),
    import("@codemirror/legacy-modes/mode/swift"),
    import("@codemirror/legacy-modes/mode/ruby"),
    import("@codemirror/legacy-modes/mode/lua"),
    import("@codemirror/legacy-modes/mode/dockerfile"),
    import("@codemirror/legacy-modes/mode/diff"),
    import("@codemirror/legacy-modes/mode/properties"),
  ]);

  return {
    EditorState: state.EditorState,
    EditorView: view.EditorView,
    Compartment: state.Compartment,
    Transaction: state.Transaction,
    Text: state.Text,
    StateField: state.StateField,
    StateEffect: state.StateEffect,
    Decoration: view.Decoration,
    ChangeSet: state.ChangeSet,
    keymap: view.keymap,
    placeholder: view.placeholder,
    lineNumbers: view.lineNumbers,
    highlightActiveLine: view.highlightActiveLine,
    drawSelection: view.drawSelection,
    highlightWhitespace: view.highlightWhitespace,
    history: commands.history,
    defaultKeymap: commands.defaultKeymap,
    historyKeymap: commands.historyKeymap,
    indentWithTab: commands.indentWithTab,
    search: search.search,
    searchKeymap: search.searchKeymap,
    closeSearchPanel: search.closeSearchPanel,
    searchPanelOpen: search.searchPanelOpen,
    syntaxHighlighting: language.syntaxHighlighting,
    defaultHighlightStyle: language.defaultHighlightStyle,
    HighlightStyle: language.HighlightStyle,
    tags: highlight.tags,
    StreamLanguage: language.StreamLanguage,
    foldGutter: language.foldGutter,
    foldKeymap: language.foldKeymap,
    indentUnit: language.indentUnit,
    syntaxTree: language.syntaxTree,
    bracketMatching: language.bracketMatching,
    indentOnInput: language.indentOnInput,
    closeBrackets: autocomplete.closeBrackets,
    closeBracketsKeymap: autocomplete.closeBracketsKeymap,
    rectangularSelection: view.rectangularSelection,
    crosshairCursor: view.crosshairCursor,
    highlightSelectionMatches: search.highlightSelectionMatches,
    openSearchPanel: search.openSearchPanel,
    gutter: view.gutter,
    GutterMarker: view.GutterMarker,
    ViewPlugin: view.ViewPlugin,
    RangeSet: state.RangeSet,
    EditorSelection: state.EditorSelection,
    languages: {
      markdown: () => markdown.markdown(),
      typescript: () => javascript.javascript({ typescript: true, jsx: true }),
      javascript: () => javascript.javascript({ jsx: true }),
      json: () => json.json(),
      yaml: () => yaml.yaml(),
      python: () => python.python(),
      shell: () => language.StreamLanguage.define(shellMode.shell),
      rust: () => language.StreamLanguage.define(rustMode.rust),
      xml: () => language.StreamLanguage.define(xmlMode.xml),
      css: () => css.css(),
      html: () => html.html(),
      go: () => language.StreamLanguage.define(goMode.go),
      toml: () => language.StreamLanguage.define(tomlMode.toml),
      sql: () => language.StreamLanguage.define(sqlMode.standardSQL),
      c: () => language.StreamLanguage.define(clikeMode.c),
      cpp: () => language.StreamLanguage.define(clikeMode.cpp),
      java: () => language.StreamLanguage.define(clikeMode.java),
      kotlin: () => language.StreamLanguage.define(clikeMode.kotlin),
      csharp: () => language.StreamLanguage.define(clikeMode.csharp),
      swift: () => language.StreamLanguage.define(swiftMode.swift),
      ruby: () => language.StreamLanguage.define(rubyMode.ruby),
      lua: () => language.StreamLanguage.define(luaMode.lua),
      dockerfile: () => language.StreamLanguage.define(dockerfileMode.dockerFile),
      diff: () => language.StreamLanguage.define(diffMode.diff),
      properties: () => language.StreamLanguage.define(propertiesMode.properties),
    },
  };
}

/**
 * Lazy-load CodeMirror 6 + language packs. Returns a cached promise on
 * subsequent calls. On rejection the cache is cleared so the next call
 * retries — mirrors `getD2()` in `markdown-viewer.tsx:269-302`.
 */
export function getCodeMirror(): Promise<CmModule> {
  if (!loadPromise) {
    const p = loadCmModule().catch((e) => {
      if (loadPromise === p) loadPromise = null;
      throw e;
    });
    loadPromise = p;
  }
  return loadPromise;
}

/**
 * Test-only helper. Clears the singleton so a subsequent `getCodeMirror`
 * call exercises the full load path again. Do NOT call from production code.
 */
export function _resetCmLoaderForTests(): void {
  loadPromise = null;
}

// ──────────────────────────────────────────────────────────────────────
// @codemirror/merge loader (SS-15). Sibling singleton to `getCodeMirror`;
// kept separate so callers that never mount `<CodeMergeView>` don't pay
// the merge view's ~20 KB bundle cost (SS-15 D-3).
// ──────────────────────────────────────────────────────────────────────

let mergeLoadPromise: Promise<CmMergeModule> | null = null;

async function loadCmMergeModule(): Promise<CmMergeModule> {
  const merge = await import("@codemirror/merge");
  return {
    MergeView: merge.MergeView,
    unifiedMergeView: merge.unifiedMergeView,
    goToNextChunk: merge.goToNextChunk,
    goToPreviousChunk: merge.goToPreviousChunk,
    getChunks: merge.getChunks,
    getOriginalDoc: merge.getOriginalDoc,
    originalDocChangeEffect: merge.originalDocChangeEffect,
    diff: merge.diff,
    Change: merge.Change,
  };
}

/**
 * Lazy-load `@codemirror/merge`. Returns a cached promise on subsequent
 * calls. On rejection the cache is cleared so the next call retries —
 * mirrors `getCodeMirror` + the `getD2` pattern in
 * `markdown-viewer.tsx:269-302`.
 */
export function getCodeMirrorMerge(): Promise<CmMergeModule> {
  if (!mergeLoadPromise) {
    const p = loadCmMergeModule().catch((e) => {
      if (mergeLoadPromise === p) mergeLoadPromise = null;
      throw e;
    });
    mergeLoadPromise = p;
  }
  return mergeLoadPromise;
}

/**
 * Test-only helper for `getCodeMirrorMerge`. Same contract as
 * `_resetCmLoaderForTests`.
 */
export function _resetCmMergeLoaderForTests(): void {
  mergeLoadPromise = null;
}

/**
 * Decode a UTF-8 byte stream (as a `number[]`) into a string. SS-13's
 * `readLayerFile` returns bytes in this shape; consumers feed the result
 * into `<CodeEditor value>` (D-11).
 *
 * Uses `TextDecoder("utf-8")` in non-fatal mode (the default), so malformed
 * bytes are replaced with `U+FFFD` (`�`) rather than throwing. This is
 * consistent with the SS-14 contract in D-11: binary files are the host's
 * responsibility to detect and route to a different viewer; SS-14 will not
 * crash on malformed UTF-8 — it will surface visible replacement characters.
 * If the host re-saves such content, the original bytes are not recoverable.
 */
export function decodeUtf8(bytes: number[]): string {
  return new TextDecoder("utf-8").decode(new Uint8Array(bytes));
}
