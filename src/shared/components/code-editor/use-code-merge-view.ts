// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";
import type { Extension } from "@codemirror/state";

import {
  getCodeMirror,
  getCodeMirrorMerge,
  type CmModule,
  type CmMergeModule,
  type LanguageId,
} from "./cm-loader";
import { buildLanguageExtension } from "./language-detection";
import { MONO_FONT_STACK } from "./editor-affordances";
import { lightTheme } from "./themes/light";
import { darkTheme } from "./themes/dark";
import { buildSaveKeybinding } from "./extensions/save-keybinding";
import { buildLineSeparatorExtensions } from "./use-code-editor";
import { logger } from "@/shared/lib/logger";
import {
  captureTopAnchor,
  createFlashController,
  findScrollParent,
  minimalChange,
  restoreTopAnchor,
  scrollPosToCenter,
  type FlashController,
} from "./live-diff";
import { createIgnoreWhitespaceOverride, type OffsetChange } from "./whitespace-diff";
import type { SourceDiffMode } from "@/shared/stores/viewer-store";
import {
  breadcrumbModeFor,
  resolveScopeChain,
  type BreadcrumbMode,
  type ScopeTree,
} from "./scope-breadcrumb";

// SLW-DV-5: how long a changed line pulses (AC-3), and how far an upward scroll
// must move to count as the user taking over (AC-4 pause-on-scroll-up).
const FLASH_DURATION_MS = 700;
const SCROLL_UP_PAUSE_PX = 4;

// SLW-DV-8 (AC-2): the breadcrumb needs layout (`posAtCoords`) to resolve the
// viewport-top scope. On a static, never-scrolled diff the first frame after mount
// can precede layout; retry a bounded number of frames so the bar still appears
// without an infinite spin if layout never comes.
const MAX_BREADCRUMB_LAYOUT_RETRIES = 30;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

export interface CodeMergeViewHandle {
  getLocalValue: () => string;
  getDiskValue: () => string;
  setLocalValue: (next: string) => void;
  setDiskValue: (next: string) => void;
  goToNextChunk: () => boolean;
  goToPreviousChunk: () => boolean;
  getChunkCount: () => number;
  /** Where the cursor sits among the changes: the 0-based index of the change
   *  it is in (`null` while it is outside every change), and how many there
   *  are. Works for split and inline views alike. */
  getChunkPosition: () => { index: number | null; count: number };
  /** The cursor's offset in the worktree ("b") document. */
  getCaret: () => number;
  /** The 1-based line, in the worktree ("b") document, where change `index`
   *  starts; `null` when there is no such change. */
  getChunkLine: (index: number) => number | null;
  /** Put the cursor at the start of change `index` and scroll it into view —
   *  what "next change" does, for a change the cursor already sits in. */
  revealChunk: (index: number) => void;
  revealRange: (start: number, end: number) => void;
}

export interface UseCodeMergeViewOptions {
  container: HTMLDivElement | null;
  diskValue: string;
  localValue: string;
  onSave?: (mergedValue: string) => Promise<void>;
  onLoadError?: (err: Error) => void;
  theme: "light" | "dark";
  language: LanguageId;
  fontSize: number;
  gutters: boolean;
  /**
   * SLW-WD-6: read-only review mode. Both sides become read-only, the
   * revert/merge controls and the save/history/edit keymaps are dropped —
   * a review surface, not a conflict-resolution editor. Default `false`
   * (the editable disk-vs-local behavior SS-15 shipped). In review mode the
   * content is immutable per mount, so the prop-sync effects are skipped;
   * a file/theme switch is a remount (the consumer keys the container).
   */
  review?: boolean;
  /**
   * SLW-WD-6: `"split"` (side-by-side `MergeView`, the default) or
   * `"inline"` (single-column `unifiedMergeView` — removed lines red,
   * added lines green). Inline is a different CM construction, not a flag
   * on `MergeView`; both share language/theme/gutter/loader wiring.
   */
  mode?: "split" | "inline";
  /**
   * SLW-DV-6 (AC-6): soft line-wrapping on long lines. A structural extension,
   * so a change rebuilds the view — consumers key their container on it.
   * Default `false` (no wrap).
   */
  wordWrap?: boolean;
  /**
   * SLW-DV-6 (AC-7): compose `@codemirror/search` (⌘F find, ⌘G next, Esc close)
   * into the diff. Opt-in — mirrors the single-editor `use-code-editor` flag so
   * only surfaces that want find-in-diff pay for the panel. Default `false`.
   */
  search?: boolean;
  /**
   * SLW-DV-6 (AC-4): collapse unchanged regions (the default) or show full
   * context. `false` = "expand all context". Structural, so a change rebuilds
   * the view. Default `true`.
   */
  collapseUnchanged?: boolean;
  /**
   * SLW-DV-5 (AC-4): in review mode, follow the newest live change like
   * `tail -f`. Ignored outside review. When the user scrolls up the view
   * pauses (surfaced via `live.paused`) and re-engages through `live.jumpToLatest`.
   */
  autoFollow?: boolean;
  /**
   * SLW-DV-8b (RD-3): the diff mode. `"line"` (default) = the standard line diff;
   * `"ignoreWhitespace"` = DV-8's synchronous whitespace-normalized override;
   * `"structural"` = the pre-computed parse-tree diff (`structuralChanges` below).
   * Structural (composed at build), so a change rebuilds the view.
   */
  diffMode?: SourceDiffMode;
  /**
   * SLW-DV-8b (AC-1/AC-3): the off-thread structural-diff result, in original
   * coordinates. Only read when `diffMode === "structural"`; the override is a
   * closure over these already-computed changes (the heavy parse ran in a worker,
   * never on the render thread). A new array identity rebuilds the view.
   */
  structuralChanges?: readonly OffsetChange[];
  /**
   * The file's line ending, applied to both sides so a CRLF file is shown,
   * merged and handed back (`getLocalValue`, ⌘S) with its CRLF breaks. Read
   * when the view is built. Default `"\n"`.
   */
  lineSeparator?: "\n" | "\r\n";
}

export interface UseCodeMergeViewResult {
  handle: CodeMergeViewHandle | null;
  loadState: "loading" | "ready" | "error";
  loadError: Error | null;
  retry: () => void;
  /**
   * SLW-DV-5 (AC-4): live-follow state for the review diff. `paused` is true once
   * the user scrolls up away from the latest edit; `jumpToLatest` scrolls back to
   * the newest change and resumes following. `null` outside review mode.
   */
  live: { paused: boolean; jumpToLatest: () => void } | null;
  /**
   * SLW-DV-8 (AC-2): the enclosing-scope chain (outermost → innermost) at the top
   * of the viewport, or `null` when the language has no derivable scope / the top
   * sits at file level. Review mode only; recomputed on scroll + live edits.
   */
  breadcrumb: string[] | null;
}

// ---------------------------------------------------------------------------
// Internal refs
// ---------------------------------------------------------------------------

type MergeViewInstance = InstanceType<CmMergeModule["MergeView"]>;
type EditorViewInstance = InstanceType<CmModule["EditorView"]>;

/**
 * The built view is either a split `MergeView` (two editors) or an inline
 * `unifiedMergeView` (a single `EditorView` whose `original` is the "disk"
 * side). Split keeps SS-15's editable/read-only behavior; inline is review
 * only. The union lets one hook drive both without duplicating the shared
 * language/theme/gutter/loader wiring (SLW-WD-6 §C).
 */
type BuiltView =
  | { kind: "split"; mergeView: MergeViewInstance }
  | { kind: "inline"; editor: EditorViewInstance; original: string };

interface MergeRefs {
  view: BuiltView;
  cm: CmModule;
  mergeMod: CmMergeModule;
  /** SLW-DV-5: flash controller for the after side (review mode only). */
  flash: FlashController | null;
  /** SLW-DV-5: last content applied to each side — the base for the next minimal diff. */
  appliedBefore: string;
  appliedAfter: string;
  /** SLW-DV-5: the WD-7 outer scroller that owns vertical scroll for this view. */
  scroller: HTMLElement | null;
}

/** The DOM root of whichever view kind was built (for inline styling). */
function viewDom(view: BuiltView): HTMLElement {
  return view.kind === "split" ? view.mergeView.dom : view.editor.dom;
}

/**
 * SLW-DV-6 (AC-8): the diff-compute bound shared by both view kinds. The diff
 * runs synchronously on the renderer thread when the view is built, so `timeout`
 * caps the detailed pass (it falls back to a fast, imprecise diff rather than
 * running away) and `scanLimit` caps per-range effort. This keeps a very large /
 * very different file from freezing the UI — the app-side complement to the
 * `SourceDiffPane` size gate, which avoids diffing enormous files at all.
 */
const DIFF_CONFIG = { scanLimit: 500, timeout: 1000 } as const;

/** The shape CM's `MergeView`/`unifiedMergeView` accept as `diffConfig`. */
type DiffConfigLike = {
  scanLimit?: number;
  timeout?: number;
  override?: (a: string, b: string) => readonly InstanceType<CmMergeModule["Change"]>[];
};

/**
 * SLW-DV-8b (RD-3): the diff config for a view, one exhaustive switch over the
 * mode. `"line"` is the plain bounded `DIFF_CONFIG` (unchanged default path).
 * `"ignoreWhitespace"` (DV-8) attaches a synchronous override over
 * whitespace-normalized images. `"structural"` (DV-8b) attaches a closure over the
 * already-computed off-thread changes — the expensive parse never runs here, so the
 * override is O(1). Both overrides carry `DIFF_CONFIG`'s bound because CM skips
 * `scanLimit`/`timeout` once an override is present, and both mint real `Change`
 * instances so CM's `makePresentable` aligns them.
 */
function buildDiffConfig(
  mergeMod: CmMergeModule,
  diffMode: SourceDiffMode,
  structuralChanges: readonly OffsetChange[] | undefined,
): DiffConfigLike {
  // One owner of "mint a CM `Change` from four offsets" — both overrides need it.
  const makeChange = (fromA: number, toA: number, fromB: number, toB: number) =>
    new mergeMod.Change(fromA, toA, fromB, toB);
  switch (diffMode) {
    case "line":
      return DIFF_CONFIG;
    case "ignoreWhitespace":
      return {
        ...DIFF_CONFIG,
        override: createIgnoreWhitespaceOverride(mergeMod.diff, makeChange, DIFF_CONFIG),
      };
    case "structural":
      return {
        ...DIFF_CONFIG,
        override: () =>
          (structuralChanges ?? []).map((c) => makeChange(c.fromA, c.toA, c.fromB, c.toB)),
      };
  }
}

/** Whether two scope chains are element-wise equal (the breadcrumb dedup). */
function sameScopeChain(a: string[] | null, b: string[] | null): boolean {
  if (a === b) return true;
  if (!a || !b || a.length !== b.length) return false;
  return a.every((label, i) => label === b[i]);
}

/** The editor that carries the "after" (worktree) document for a built view. */
function afterEditor(view: BuiltView): EditorViewInstance {
  return view.kind === "split" ? view.mergeView.b : view.editor;
}

interface LiveUpdateParams {
  before: string;
  after: string;
  autoFollow: boolean;
  paused: boolean;
  /** Marks the imminent scrollTop write as ours so the scroll listener ignores it. */
  beginProgrammaticScroll: () => void;
  /** Records the newest change position so `jumpToLatest` can return to it. */
  setLastChangePos: (pos: number) => void;
}

/**
 * SLW-DV-5 (AC-6b) — patch a review merge view IN PLACE for a live content
 * change: dispatch the minimal edit into each changed side, flash the new
 * after-lines (AC-3), then either follow the newest change or hold the current
 * top line steady (AC-2/AC-4). Never remounts; a no-op when nothing changed.
 */
function applyReviewLiveUpdate(refs: MergeRefs, p: LiveUpdateParams): void {
  const beforeChange = minimalChange(refs.appliedBefore, p.before);
  const afterChange = minimalChange(refs.appliedAfter, p.after);
  if (!beforeChange && !afterChange) return;

  const after = afterEditor(refs.view);
  const scroller = refs.scroller;
  const following = p.autoFollow && !p.paused;

  // Anchor: the line + exact offset at the top of the viewport BEFORE the patch,
  // so an insert above the fold can be corrected for afterward, pixel-stable
  // (AC-2). Only when not following.
  const anchor = !following && scroller && afterChange ? captureTopAnchor(after, scroller) : null;

  // A live patch must never throw out of the effect: if the mirrored applied*
  // ever drifts from the real doc (a coalesced/rejected prior dispatch), the
  // minimal change's positions can overrun and CM throws — fall back to a full
  // resync of that side so the diff self-heals instead of dying half-patched.
  try {
    if (beforeChange) {
      patchBeforeSide(refs, p.before, beforeChange);
      refs.appliedBefore = p.before;
    }
    if (afterChange) {
      patchAfterSide(refs, after, p.after, afterChange);
      refs.appliedAfter = p.after;
    }
  } catch (e) {
    logger.warn("useCodeMergeView", `live patch fell back to resync — ${e}`);
    return;
  }

  if (!afterChange) return;
  const flashTo = afterChange.from + afterChange.insert.length;
  refs.flash?.flash(after, afterChange.from, flashTo, FLASH_DURATION_MS);
  p.setLastChangePos(flashTo);

  if (!scroller) return;
  // Defer the scroll a frame so CM has laid out the new line heights first.
  window.requestAnimationFrame(() => {
    // The file may have switched (view torn down) between the dispatch and here.
    if (!after.dom.isConnected) return;
    p.beginProgrammaticScroll();
    if (following) {
      scrollPosToCenter(after, scroller, flashTo);
    } else if (anchor) {
      restoreTopAnchor(after, scroller, remapAnchor(anchor.pos, afterChange), anchor.viewportY);
    }
  });
}

/** Dispatch a minimal change into the after editor, resyncing the whole doc if
 *  the mirrored `applied*` drifted and the minimal positions overrun (AC-6b). */
function patchAfterSide(
  refs: MergeRefs,
  after: EditorViewInstance,
  nextAfter: string,
  change: { from: number; to: number; insert: string },
): void {
  const noHistory = refs.cm.Transaction.addToHistory.of(false);
  try {
    after.dispatch({ changes: change, annotations: [noHistory] });
  } catch {
    const doc = after.state.doc;
    after.dispatch({
      changes: { from: 0, to: doc.length, insert: nextAfter },
      annotations: [noHistory],
    });
  }
}

/** Dispatch a minimal change into the before/baseline side (split `a` doc, or
 *  the inline merge view's `original`), with the same full-resync fallback. */
function patchBeforeSide(
  refs: MergeRefs,
  nextBefore: string,
  change: { from: number; to: number; insert: string },
): void {
  const noHistory = refs.cm.Transaction.addToHistory.of(false);
  if (refs.view.kind === "split") {
    const a = refs.view.mergeView.a;
    try {
      a.dispatch({ changes: change, annotations: [noHistory] });
    } catch {
      const doc = a.state.doc;
      a.dispatch({
        changes: { from: 0, to: doc.length, insert: nextBefore },
        annotations: [noHistory],
      });
    }
    return;
  }
  const editor = refs.view.editor;
  const apply = (spec: { from: number; to: number; insert: string }): void => {
    const len = refs.mergeMod.getOriginalDoc(editor.state).length;
    const changes = refs.cm.ChangeSet.of(spec, len);
    editor.dispatch({ effects: refs.mergeMod.originalDocChangeEffect(editor.state, changes) });
  };
  try {
    apply(change);
  } catch {
    apply({ from: 0, to: refs.mergeMod.getOriginalDoc(editor.state).length, insert: nextBefore });
  }
  refs.view.original = nextBefore;
}

/** Map the pre-patch top-visible position through one change so the anchored
 *  line keeps its viewport offset: unchanged if the edit is below it, shifted by
 *  the net delta if above, pinned to the edit start if it straddled the line. */
function remapAnchor(
  anchorPos: number,
  change: { from: number; to: number; insert: string },
): number {
  if (anchorPos <= change.from) return anchorPos;
  if (anchorPos >= change.to) return anchorPos + change.insert.length - (change.to - change.from);
  return change.from;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function buildThemeExtension(cm: CmModule, mode: "light" | "dark"): Extension {
  return mode === "light" ? lightTheme(cm) : darkTheme(cm);
}

function buildLanguageExtensions(cm: CmModule, language: LanguageId): Extension[] {
  const ext = buildLanguageExtension(cm, language);
  return ext ? [ext] : [];
}

function buildGuttersExtensions(cm: CmModule, gutters: boolean): Extension[] {
  return gutters ? [cm.lineNumbers()] : [];
}

interface BuildViewParams {
  container: HTMLDivElement;
  review: boolean;
  mode: "split" | "inline";
  before: string;
  after: string;
  sharedExtensions: Extension[];
  readOnlyExts: Extension[];
  buildEditableExtensions: () => Extension[];
  /** AC-4: collapse unchanged regions, or show full context when `false`. */
  collapseUnchanged: boolean;
  /** SLW-DV-5: extensions added ONLY to the after side (e.g. the flash field). */
  afterExtensions: Extension[];
  /** SLW-DV-6 (AC-8) / SLW-DV-8 (AC-1): the diff-compute bound, optionally carrying
   *  the ignore-whitespace override. */
  diffConfig: DiffConfigLike;
}

/**
 * Build the CodeMirror view for the requested mode. Split → a two-editor
 * `MergeView` (SS-15's shape; editable `b` unless `review`). Inline → a
 * single `EditorView` with `unifiedMergeView` (removed red / added green),
 * read-only + no accept/reject controls when `review` (the only inline caller
 * today is the read-only `CodeDiffReview`). Both share the caller's
 * language/theme/gutter extensions so that wiring lives once.
 */
function buildView(cm: CmModule, mergeMod: CmMergeModule, p: BuildViewParams): BuiltView {
  // AC-4: `undefined` shows full context (no collapsing); the object collapses
  // unchanged runs with a small margin around each hunk.
  const collapseUnchanged = p.collapseUnchanged ? { margin: 3, minSize: 4 } : undefined;

  if (p.mode === "inline") {
    const editor = new cm.EditorView({
      parent: p.container,
      doc: p.after,
      extensions: [
        ...p.sharedExtensions,
        ...mergeMod.unifiedMergeView({
          original: p.before,
          // No accept/reject gutter buttons in review — this is a read-only
          // review surface, not conflict resolution (AC-2).
          mergeControls: !p.review,
          highlightChanges: true,
          syntaxHighlightDeletions: true,
          collapseUnchanged,
          // SLW-DV-6 (AC-8): bound the synchronous diff so a pathological file
          // can't run quadratically on the renderer thread. SLW-DV-8 (AC-1): may
          // carry the ignore-whitespace override.
          diffConfig: p.diffConfig,
        }),
        ...(p.review ? p.readOnlyExts : []),
        ...p.afterExtensions,
      ],
    });
    return { kind: "inline", editor, original: p.before };
  }

  const mergeView = new mergeMod.MergeView({
    parent: p.container,
    orientation: "a-b",
    // Revert arrows only exist for the editable conflict editor; a review
    // diff has none (AC-2).
    ...(p.review ? {} : { revertControls: "a-to-b" as const }),
    highlightChanges: true,
    gutter: true,
    collapseUnchanged,
    // SLW-DV-6 (AC-8): same diff-compute bound as inline. SLW-DV-8 (AC-1): may
    // carry the ignore-whitespace override.
    diffConfig: p.diffConfig,
    a: {
      doc: p.before,
      extensions: [...p.sharedExtensions, ...p.readOnlyExts],
    },
    b: {
      doc: p.after,
      extensions: p.review
        ? [...p.sharedExtensions, ...p.readOnlyExts, ...p.afterExtensions]
        : [...p.sharedExtensions, ...p.buildEditableExtensions()],
    },
  });
  return { kind: "split", mergeView };
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

export function useCodeMergeView(options: UseCodeMergeViewOptions): UseCodeMergeViewResult {
  const {
    container,
    diskValue,
    localValue,
    onSave,
    onLoadError,
    theme,
    language,
    fontSize,
    gutters,
    review = false,
    mode = "split",
    wordWrap = false,
    search = false,
    collapseUnchanged = true,
    autoFollow = false,
    diffMode = "line",
    structuralChanges,
    lineSeparator = "\n",
  } = options;

  const onSaveRef = useRef<typeof onSave>(onSave);
  const inFlightSaveRef = useRef<Promise<void> | null>(null);
  onSaveRef.current = onSave;

  const mergeRefs = useRef<MergeRefs | null>(null);
  const [handle, setHandle] = useState<CodeMergeViewHandle | null>(null);
  const [loadState, setLoadState] = useState<"loading" | "ready" | "error">("loading");
  const [loadError, setLoadError] = useState<Error | null>(null);
  const [retryToken, setRetryToken] = useState(0);

  // Refs for initial values so re-renders don't reset content.
  const diskValueRef = useRef(diskValue);
  const localValueRef = useRef(localValue);
  diskValueRef.current = diskValue;
  localValueRef.current = localValue;
  const lineSeparatorRef = useRef(lineSeparator);
  lineSeparatorRef.current = lineSeparator;

  // SLW-DV-5 (AC-4): live-follow state. `livePaused` toggles the "jump to latest"
  // pill; the refs let the imperative CM callbacks read current values without
  // re-subscribing effects, and mark our own scrollTop writes as programmatic so
  // the scroll listener doesn't mistake them for the user scrolling up.
  const [livePaused, setLivePaused] = useState(false);
  const livePausedRef = useRef(false);
  livePausedRef.current = livePaused;
  const programmaticScrollRef = useRef(false);
  const lastChangePosRef = useRef<number | null>(null);

  const beginProgrammaticScroll = useCallback(() => {
    programmaticScrollRef.current = true;
    // Clear a couple of frames later — after the async scroll event has fired.
    window.requestAnimationFrame(() =>
      window.requestAnimationFrame(() => {
        programmaticScrollRef.current = false;
      }),
    );
  }, []);

  const jumpToLatest = useCallback(() => {
    setLivePaused(false);
    const r = mergeRefs.current;
    if (!r || !r.scroller) return;
    const scroller = r.scroller;
    const after = afterEditor(r.view);
    const pos = lastChangePosRef.current ?? after.state.doc.length;
    // Defer to a frame and mark it programmatic INSIDE that frame — same as the
    // live-follow scroll — so the resulting scroll event can't be misread as a
    // user scroll-up and immediately re-pause the jump (AC-4).
    window.requestAnimationFrame(() => {
      if (!after.dom.isConnected) return;
      beginProgrammaticScroll();
      scrollPosToCenter(after, scroller, pos);
    });
  }, [beginProgrammaticScroll]);

  // SLW-DV-8 (AC-2): scope breadcrumb. The resolution mode follows the language
  // (code / markdown / none) and is only active in review mode. Recompute is
  // throttled to one animation frame so a scroll burst resolves the tree once.
  const [breadcrumb, setBreadcrumb] = useState<string[] | null>(null);
  const breadcrumbRef = useRef<string[] | null>(null);
  const breadcrumbRafRef = useRef<number | null>(null);
  const breadcrumbRetryRef = useRef(0);
  const recomputeRef = useRef<() => void>(() => {});
  const breadcrumbModeRef = useRef<BreadcrumbMode>("none");
  breadcrumbModeRef.current = review ? breadcrumbModeFor(language) : "none";

  const recomputeBreadcrumb = useCallback(() => {
    const r = mergeRefs.current;
    const mode = breadcrumbModeRef.current;
    const commit = (chain: string[] | null): void => {
      const next = chain?.length ? chain : null;
      // Element-wise compare so the state updates only on a real scope change —
      // no separator/join key (which could collide across chains or, as a raw
      // delimiter, corrupt the source encoding).
      if (sameScopeChain(next, breadcrumbRef.current)) return;
      breadcrumbRef.current = next;
      setBreadcrumb(next);
    };
    if (!r || mode === "none") {
      commit(null);
      return;
    }
    const after = afterEditor(r.view);
    if (!after.dom.isConnected) return;
    // Best-effort, like the other DOM-coord helpers (live-diff.ts): `posAtCoords`
    // throws when there is no layout yet (headless, or a transient pre-measure
    // frame), so any measurement/resolution gap is a no-op that leaves the prior
    // breadcrumb rather than crashing the effect.
    try {
      // The document position at the top of the viewport. `x` must come from the
      // AFTER editor's own content — in split view it is the RIGHT pane, and the
      // outer scroller's left edge sits over the LEFT pane, so measuring at the
      // scroller's `x` would resolve a position in the wrong editor. `y` comes from
      // whichever element owns vertical scroll (the WD-7 outer scroller, or the
      // editor itself when there is none).
      const scrollEl = r.scroller ?? after.scrollDOM;
      const topY = scrollEl.getBoundingClientRect().top + 4;
      const leftX = after.scrollDOM.getBoundingClientRect().left + 4;
      const pos = after.posAtCoords({ x: leftX, y: topY });
      // `null` = no layout yet: keep the prior breadcrumb (rather than snapping to
      // the file-top scope) and retry on a later frame up to a bound, so a static
      // diff the user never scrolls still resolves its scope once layout lands.
      if (pos == null) {
        if (breadcrumbRetryRef.current < MAX_BREADCRUMB_LAYOUT_RETRIES) {
          breadcrumbRetryRef.current += 1;
          breadcrumbRafRef.current = window.requestAnimationFrame(() => {
            breadcrumbRafRef.current = null;
            recomputeRef.current();
          });
        }
        return;
      }
      breadcrumbRetryRef.current = 0;
      const tree = r.cm.syntaxTree(after.state) as unknown as ScopeTree;
      commit(
        resolveScopeChain(tree, pos, (from, to) => after.state.doc.sliceString(from, to), mode),
      );
    } catch {
      /* no layout / no tree yet — keep the current breadcrumb */
    }
  }, []);
  recomputeRef.current = recomputeBreadcrumb;

  const scheduleBreadcrumb = useCallback((): void => {
    if (breadcrumbRafRef.current != null) return;
    breadcrumbRafRef.current = window.requestAnimationFrame(() => {
      breadcrumbRafRef.current = null;
      recomputeBreadcrumb();
    });
  }, [recomputeBreadcrumb]);

  // -------------------------------------------------------------------
  // Construction effect
  // -------------------------------------------------------------------
  useEffect(() => {
    if (!container) return;
    let cancelled = false;
    let detachScroll: (() => void) | null = null;

    // Each rebuilt view (file switch / structural toggle) gets its own breadcrumb
    // layout-retry budget. The hook is not remounted on a file switch (only the
    // keyed frame is), so without this reset an exhausted counter from a prior
    // view would disable the retry for every subsequent one.
    breadcrumbRetryRef.current = 0;

    setLoadState("loading");
    setLoadError(null);

    Promise.all([getCodeMirror(), getCodeMirrorMerge()])
      .then(([cm, mergeMod]) => {
        if (cancelled) return;

        const themeExt = buildThemeExtension(cm, theme);
        const langExts = buildLanguageExtensions(cm, language);
        const gutterExts = buildGuttersExtensions(cm, gutters);

        // SLW-DV-6 (AC-3): F7 / Shift+F7 next/prev-change is cross-tool muscle
        // memory; bound alongside the original Alt-↓/↑ on the same focused-editor
        // keymap (no global handler, so terminal keys are never stolen).
        const chunkNavKeymap = cm.keymap.of([
          { key: "Alt-ArrowDown", run: mergeMod.goToNextChunk },
          { key: "Alt-ArrowUp", run: mergeMod.goToPreviousChunk },
          { key: "F7", run: mergeMod.goToNextChunk },
          { key: "Shift-F7", run: mergeMod.goToPreviousChunk },
        ]);

        const sharedExtensions: Extension[] = [
          // SLW-WDE-7 (RD-1): `themeExt` (build-theme) already carries the shared
          // codesense syntax highlight, so the stock `defaultHighlightStyle` line
          // that used to sit here is gone — one palette, editor + diff.
          themeExt,
          ...langExts,
          ...gutterExts,
          cm.drawSelection(),
          cm.highlightActiveLine(),
          chunkNavKeymap,
          // SLW-DV-6 (AC-6): soft-wrap long lines when requested.
          ...(wordWrap ? [cm.EditorView.lineWrapping] : []),
          // SLW-DV-6 (AC-7): find-in-diff (⌘F/⌘G/Esc) — the same panel + keymap
          // the single editor ships (SW-VE-8), opt-in per the `search` flag.
          ...(search ? [cm.search({ top: true }), cm.keymap.of(cm.searchKeymap)] : []),
          ...buildLineSeparatorExtensions(cm, lineSeparatorRef.current),
        ];

        // Read-only extensions applied to a side that must not be edited: the
        // disk side always, and BOTH sides in review mode (SLW-WD-6 AC-2).
        const readOnlyExts: Extension[] = [
          cm.EditorView.editable.of(false),
          cm.EditorState.readOnly.of(true),
        ];

        // SLW-DV-5 (AC-3): the after side carries the flash field in review mode.
        const flash = review ? createFlashController(cm) : null;

        const view = buildView(cm, mergeMod, {
          container,
          review,
          mode,
          before: diskValueRef.current,
          after: localValueRef.current,
          sharedExtensions,
          readOnlyExts,
          collapseUnchanged,
          diffConfig: buildDiffConfig(mergeMod, diffMode, structuralChanges),
          afterExtensions: flash ? [flash.extension] : [],
          // The save/history/edit stack is built lazily — only the editable
          // (non-review) split path uses it (SLW-WD-6 §B/AC-2).
          buildEditableExtensions: () => [
            buildSaveKeybinding(cm, {
              getOnSave: () => onSaveRef.current,
              getReadOnly: () => false,
              getInFlight: () => inFlightSaveRef.current,
              setInFlight: (p) => {
                inFlightSaveRef.current = p;
              },
            }),
            cm.keymap.of([...cm.defaultKeymap, ...cm.historyKeymap]),
            cm.history(),
          ],
        });

        // Inline styles on the built view root (split MergeView or inline editor).
        const dom = viewDom(view);
        // SW-VE-11: diff/merge is code-comparison — always monospace (the prose
        // reading-font never applies here). Shared const de-dupes the literal.
        dom.style.fontFamily = MONO_FONT_STACK;
        dom.style.fontSize = `${fontSize}px`;
        dom.style.colorScheme = theme;

        const scroller = review ? findScrollParent(container) : null;
        mergeRefs.current = {
          view,
          cm,
          mergeMod,
          flash,
          appliedBefore: diskValueRef.current,
          appliedAfter: localValueRef.current,
          scroller,
        };

        // SLW-DV-5 (AC-4): pause follow when the user scrolls the outer frame up,
        // and re-engage when they scroll back to the bottom. Our own scrolls set
        // `programmaticScrollRef`, so only genuine user scrolls flip the pill.
        if (review && scroller) {
          let prev = scroller.scrollTop;
          const onScroll = (): void => {
            // AC-2: the enclosing scope changes with the viewport top on ANY
            // scroll (user or programmatic follow), so recompute before the
            // pause-logic early-return below.
            scheduleBreadcrumb();
            const cur = scroller.scrollTop;
            // Track our OWN (follow) scrolls in `prev` too — otherwise the
            // baseline goes stale during a programmatic jump and the user's next
            // upward scroll is measured against the wrong position and misses.
            if (programmaticScrollRef.current) {
              prev = cur;
              return;
            }
            const atBottom =
              cur + scroller.clientHeight >= scroller.scrollHeight - SCROLL_UP_PAUSE_PX;
            if (cur < prev - SCROLL_UP_PAUSE_PX && !atBottom) setLivePaused(true);
            else if (atBottom) setLivePaused(false);
            prev = cur;
          };
          scroller.addEventListener("scroll", onScroll, { passive: true });
          detachScroll = () => scroller.removeEventListener("scroll", onScroll);
        }

        setHandle(buildHandle(mergeRefs));
        setLoadState("ready");
        // AC-2: resolve the initial breadcrumb once the view + parse tree exist.
        scheduleBreadcrumb();
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
      if (detachScroll) detachScroll();
      if (breadcrumbRafRef.current != null) {
        window.cancelAnimationFrame(breadcrumbRafRef.current);
        breadcrumbRafRef.current = null;
      }
      const r = mergeRefs.current;
      if (r) {
        r.flash?.dispose(); // cancel pending flash timers so they don't pin the view
        if (r.view.kind === "split") r.view.mergeView.destroy();
        else r.view.editor.destroy();
        mergeRefs.current = null;
      }
      setHandle(null);
      inFlightSaveRef.current = null;
    };
    // `review`/`mode` are structural — a change means "rebuild the whole view",
    // so they belong in the deps (a consumer that flips mode without remounting
    // the container still gets a correct rebuild; the remount-by-key in
    // CodeDiffReview is then a redundant safety net, not a load-bearing one).
    // diskValue/localValue stay OUT (synced via refs) so typing doesn't reset.
    // `wordWrap`/`search`/`collapseUnchanged`/`diffMode` are structural (composed
    // at build) so a change rebuilds the view, same as `review`/`mode`.
    // `structuralChanges` is a stable ref from `useStructuralDiff` (a new identity
    // only when a fresh off-thread result lands) — including it rebuilds the view
    // with the new structural override (AC-4 remount-on-content-change).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    container,
    retryToken,
    review,
    mode,
    wordWrap,
    search,
    collapseUnchanged,
    diffMode,
    structuralChanges,
  ]);

  // -------------------------------------------------------------------
  // Live review content-sync (SLW-DV-5 AC-6b)
  // -------------------------------------------------------------------

  // In review mode a new before/after for the SAME view (an agent edit while the
  // file stays open) patches the built view IN PLACE — the minimal edit is
  // dispatched into each changed side, changed lines flash, and follow/anchor
  // keeps the viewport sane. This is what makes the diff update live without the
  // teardown-and-rebuild that a remount would cause (AC-6). The editable path
  // keeps its own diskValue sync below; the two never both run (guarded on review).
  useEffect(() => {
    if (!review) return;
    const r = mergeRefs.current;
    if (!r) return;
    applyReviewLiveUpdate(r, {
      before: diskValue,
      after: localValue,
      autoFollow,
      paused: livePausedRef.current,
      beginProgrammaticScroll,
      setLastChangePos: (pos) => {
        lastChangePosRef.current = pos;
      },
    });
    // AC-2: a live edit can change the enclosing scope of the viewport top.
    scheduleBreadcrumb();
  }, [diskValue, localValue, review, autoFollow, beginProgrammaticScroll, scheduleBreadcrumb]);

  // -------------------------------------------------------------------
  // Prop-sync effects (editable split path only)
  // -------------------------------------------------------------------

  // In review mode the effects below are inert — the live sync above owns review
  // updates. The editable conflict editor keeps them for the disk-side / theme
  // waves it needs.

  // diskValue sync: dispatch into side a when the prop changes. The
  // initial value is set at construction time; this effect handles
  // subsequent updates (e.g., AC-10 wave during merge).
  useEffect(() => {
    if (review) return;
    const r = mergeRefs.current;
    if (!r || r.view.kind !== "split") return;
    const doc = r.view.mergeView.a.state.doc;
    if (r.view.mergeView.a.state.sliceDoc() === diskValue) return;
    r.view.mergeView.a.dispatch({
      changes: { from: 0, to: doc.length, insert: diskValue },
      annotations: [r.cm.Transaction.addToHistory.of(false)],
    });
  }, [diskValue, review]);

  // Theme / language / gutters — MergeView.reconfigure re-applies
  // top-level MergeConfig; per-side extension changes (theme, lang,
  // gutters) require the reconfigure path since MergeView doesn't
  // expose per-side compartments.
  useEffect(() => {
    if (review) return;
    const r = mergeRefs.current;
    if (!r || r.view.kind !== "split") return;
    r.view.mergeView.reconfigure({
      orientation: "a-b",
      revertControls: "a-to-b",
      highlightChanges: true,
      gutter: gutters,
      collapseUnchanged: { margin: 3, minSize: 4 },
      diffConfig: DIFF_CONFIG,
    });
    r.view.mergeView.dom.style.colorScheme = theme;
  }, [theme, language, gutters, review]);

  // fontSize (inline style)
  useEffect(() => {
    const r = mergeRefs.current;
    if (!r) return;
    viewDom(r.view).style.fontSize = `${fontSize}px`;
  }, [fontSize]);

  const retry = (): void => {
    setRetryToken((n) => n + 1);
  };

  // `paused` (which drives the "jump to latest" pill) is meaningful only while
  // following: with auto-follow off there is nothing to re-engage, so a prior
  // scroll-up must not leave the pill showing (AC-4).
  const live = review ? { paused: autoFollow && livePaused, jumpToLatest } : null;

  return { handle, loadState, loadError, retry, live, breadcrumb };
}

// ---------------------------------------------------------------------------
// Handle factory
// ---------------------------------------------------------------------------

function buildHandle(refs: React.MutableRefObject<MergeRefs | null>): CodeMergeViewHandle {
  const readMerge = (): MergeRefs | null => refs.current;

  // The "local" (editable/after) side is `mergeView.b` in split and the sole
  // editor's doc in inline; the "disk" (before) side is `mergeView.a` in split
  // and the captured `original` in inline. Chunk navigation runs against the
  // editor that carries the merge state (`b` / the inline editor). Inline is
  // review-only so `setLocalValue`/`setDiskValue` are no-ops there.
  const chunkView = (r: MergeRefs): EditorViewInstance =>
    r.view.kind === "split" ? r.view.mergeView.b : r.view.editor;

  return {
    getLocalValue: () => {
      const r = readMerge();
      if (!r) return "";
      return r.view.kind === "split"
        ? r.view.mergeView.b.state.sliceDoc()
        : r.view.editor.state.sliceDoc();
    },
    getDiskValue: () => {
      const r = readMerge();
      if (!r) return "";
      return r.view.kind === "split" ? r.view.mergeView.a.state.sliceDoc() : r.view.original;
    },

    setLocalValue: (next) => {
      const r = readMerge();
      if (!r || r.view.kind !== "split") return;
      const doc = r.view.mergeView.b.state.doc;
      r.view.mergeView.b.dispatch({
        changes: { from: 0, to: doc.length, insert: next },
        annotations: [r.cm.Transaction.addToHistory.of(false)],
      });
    },

    setDiskValue: (next) => {
      const r = readMerge();
      if (!r || r.view.kind !== "split") return;
      const doc = r.view.mergeView.a.state.doc;
      r.view.mergeView.a.dispatch({
        changes: { from: 0, to: doc.length, insert: next },
        annotations: [r.cm.Transaction.addToHistory.of(false)],
      });
    },

    goToNextChunk: () => {
      const r = readMerge();
      if (!r) return false;
      return r.mergeMod.goToNextChunk(chunkView(r));
    },

    goToPreviousChunk: () => {
      const r = readMerge();
      if (!r) return false;
      const view = chunkView(r);
      // CM counts a caret at `toB` (the start of the line just after a change)
      // as inside that change, so "previous" would skip it. That change is the
      // previous one: land on it.
      const head = view.state.selection.main.head;
      const chunks = r.mergeMod.getChunks(view.state)?.chunks ?? [];
      const justAfter = chunks.find((c) => head === c.toB && head > c.endB);
      if (justAfter) {
        view.dispatch({
          selection: { anchor: justAfter.fromB },
          scrollIntoView: true,
          userEvent: "select.byChunk",
        });
        return true;
      }
      return r.mergeMod.goToPreviousChunk(view);
    },

    getChunkPosition: () => {
      const r = readMerge();
      if (!r) return { index: null, count: 0 };
      const view = chunkView(r);
      const chunks = r.mergeMod.getChunks(view.state)?.chunks ?? [];
      const head = view.state.selection.main.head;
      // The "b" side carries the merge state in both view kinds (`chunkView`).
      // `toB` is one past the chunk's trailing line break (the next line's
      // start); `endB` is the end of its last line, so a cursor on the line
      // after a change is not counted as in it.
      const at = chunks.findIndex((c) => head >= c.fromB && head <= c.endB);
      return { index: at >= 0 ? at : null, count: chunks.length };
    },

    getCaret: () => {
      const r = readMerge();
      return r ? chunkView(r).state.selection.main.head : 0;
    },

    getChunkLine: (index) => {
      const r = readMerge();
      if (!r) return null;
      const view = chunkView(r);
      const chunk = r.mergeMod.getChunks(view.state)?.chunks[index];
      if (!chunk) return null;
      const doc = view.state.doc;
      return doc.lineAt(Math.min(chunk.fromB, doc.length)).number;
    },

    revealRange: (start, end) => {
      const r = readMerge();
      if (!r) return;
      const view = chunkView(r),
        doc = view.state.doc;
      const first = doc.line(Math.min(Math.max(1, start), doc.lines));
      const last = doc.line(Math.min(Math.max(start, end), doc.lines));
      view.dispatch({
        selection: { anchor: first.from, head: last.to },
        scrollIntoView: true,
        userEvent: "select.reveal",
      });
    },

    revealChunk: (index) => {
      const r = readMerge();
      if (!r) return;
      const view = chunkView(r);
      const chunk = r.mergeMod.getChunks(view.state)?.chunks[index];
      if (!chunk) return;
      view.dispatch({
        selection: { anchor: Math.min(chunk.fromB, view.state.doc.length) },
        scrollIntoView: true,
        userEvent: "select.reveal",
      });
    },

    getChunkCount: () => {
      const r = readMerge();
      if (!r) return 0;
      // Only the split MergeView exposes a chunk list. Inline reports 0 — it's a
      // review-only surface with no handle exposed, so no consumer reads this;
      // chunk *navigation* still works inline (see `chunkView`). If an inline
      // consumer ever needs a real count, add `getChunks` to the merge facade.
      return r.view.kind === "split" ? r.view.mergeView.chunks.length : 0;
    },
  };
}
