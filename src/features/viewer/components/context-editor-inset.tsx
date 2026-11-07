// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";
import { Check, Columns2, Save, TriangleAlert } from "lucide-react";

import { basename } from "@/shared/lib/basename";
import { cn } from "@/lib/utils";
import { CodeEditor, CodeMergeView, useEditorAffordances } from "@/shared/components/code-editor";
import type { CodeEditorHandle, CodeMergeViewHandle } from "@/shared/components/code-editor";
import { isLivePreviewEligible } from "@/shared/lib/viewer-file-type";
import type { ViewSnapshot } from "@/shared/lib/view-snapshot";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { MarkdownPreviewPane } from "./markdown-preview-pane";
import { fileContentCache } from "@/shared/lib/file-content-cache";
import { FileDeletedPlaceholder } from "@/shared/components/file-deleted-placeholder";
import { closeActiveFileTab } from "@/shared/lib/close-active-file-tab";
import { writeContentFile } from "@/tauri/commands";
import { markContentPathChanged } from "@/shared/stores/file-refresh-store";
import { InfoTip } from "@/components/ui/tooltip";
import { EditingInScope } from "@/features/dashboard/components/scope-badge";
import type { FileScope } from "@/features/dashboard/lib/file-scope";

import { isTooLargeToView, looksBinary, utf8ByteLength } from "../lib/worktree-file-view";
import { useFileEditor, useScopedChangeNonce } from "../lib/use-file-editor";
import { FileWarningPlaceholder } from "./file-warning-placeholder";
import { ReconcileBar, ReconcileErrorBanner, StaleConflictBanner } from "./conflict-banners";
import { ChromeIconButton } from "./overlay-shell";

// ---------------------------------------------------------------------------
// SW-VE-3/4 — deliberate, concurrent-safe edit mode for a context file. Consumes
// the shared `useFileEditor` core (`../lib/use-file-editor.ts`) — the SAME state
// machine the source-worktree editor uses — swapping only the write verb
// (`writeContentFile`, watcher-suppressed + cross-scope re-materialize) and the
// change signal (the content-repo `useFileRefreshStore`, scoped to the open file
// by `useScopedChangeNonce`).
//
// SW-VE-3 shipped the editable half + a save-time-only pre-save guard. SW-VE-4
// upgrades it to the full worktree parity: a PROACTIVE watcher-driven stale guard
// (warn WHILE editing, block the clobbering save) and the `CodeMergeView`
// reconcile as the sole sanctioned clobber door (Take theirs / Keep mine /
// hand-merge + ⌘S) — retiring VE-3's blunt one-shot `overwrite()`. A watcher-
// driven vanish (the agent deleted the open file, clean buffer) self-heals to a
// "file was removed" empty state; a dirty buffer is kept intact (a save recreates
// the file — never destroy unsaved edits).
//
// This lives OUTSIDE the shared `renderViewer`/`CodeFileViewer` (which stays
// read-only-pure so a review surface reusing it never gains an editable
// buffer, the SW-VE-2 R-5 lesson). The context viewer mounts THIS inset in place
// of the read-only viewer for the file being edited (the mirror of
// `SourceEditorInset` vs `SourceDiffOverlay`).
// ---------------------------------------------------------------------------

/** The cached content for `path` IF it is viewable text (not binary/oversized) —
 *  used to seed the editor INSTANTLY when the file was just being viewed, so
 *  entering edit feels like an in-place flip, not a remount flash (AC-1). The
 *  reconcile/guard is the correctness backstop against a stale seed. */
function viewableCacheContent(path: string): string | null {
  const cached = fileContentCache.get(path)?.content;
  if (cached == null) return null;
  if (looksBinary(cached) || isTooLargeToView(utf8ByteLength(cached))) return null;
  return cached;
}

// AC-6: the agent-ownership advisory is cautionary for EVERY file (no reliable
// ownership signal exists at ship time — README decision #3) and shown ONCE per
// file per app session. This module set records which files have already shown it.
const advisoryShownThisSession = new Set<string>();

/** Test seam: reset the once-per-session advisory tracking. */
export function resetAgentOwnershipAdvisoryForTests(): void {
  advisoryShownThisSession.clear();
}

interface ContextEditorInsetProps {
  /** The absolute path of the file being edited. Keyed by the caller, so the
   *  inset is a fresh instance per file. */
  filePath: string;
  /** The file's own-writable scope (never inherited — the caller gates entry on
   *  `!scope.isInherited`), for the "Editing in <scope>" chip. */
  scope: FileScope | null;
  /** SW-VE-4: the content-repo change signal for this file's repo — the
   *  `useFileRefreshStore` refresh key + the burst's changed-path set. Scoped to
   *  `filePath` by `useScopedChangeNonce` so an agent write to the OPEN file
   *  (never an unrelated sibling) drives the stale guard / clean-take-disk. */
  refreshKey: number;
  changedPaths: Set<string> | null;
  /** Leave edit mode (back to the read-only viewer). Called after a clean exit or
   *  a confirmed discard. */
  onExit: () => void;
  /** Lifts the dirty flag so the parent can reflect it (chip dirty-dot) and gate a
   *  tab-close on the file being edited. */
  onDirtyChange?: (dirty: boolean) => void;
  /** SW-VE-3: false when the inset is mounted-but-hidden (kept alive to preserve
   *  its buffer, e.g. under the git-sync review surface). While inactive the CM6
   *  editor is `readOnly` and the reconcile surface is suppressed so the
   *  `display:none` blur can't fire a write — keeping the review surface strictly
   *  write-free. Default true. */
  active?: boolean;
  /** RF-perf-12: composed session key (`fileViewKey(teamKey, filePath)`) under
   *  which the editor's cursor/scroll is snapshotted across an unmount→remount.
   *  Absent → no snapshotting. */
  viewStateKey?: string;
}

/**
 * SW-VE-3/4 — the deliberate, concurrent-safe context-file editor. Read-only
 * stays the default; the user enters here explicitly (the "Read-only · Edit" chip
 * or ⌘E). Type into the buffer, ⌘S / Save writes via the re-materializing
 * `write_content_file`. A concurrent agent write to the open file surfaces live:
 * a clean buffer silently re-reads; a dirty buffer arms the amber stale banner and
 * blocks the clobbering save until the user Reloads (take disk) or Resolves in the
 * `CodeMergeView` (Take theirs / Keep mine / hand-merge — the sole clobber door).
 * Exit (Done / Esc) routes a dirty buffer through an inline discard-confirm.
 */
export function ContextEditorInset({
  filePath,
  scope,
  refreshKey,
  changedPaths,
  onExit,
  onDirtyChange,
  active = true,
  viewStateKey,
}: ContextEditorInsetProps) {
  // SW-VE-4: the per-file change nonce driving the reconcile — advances only when
  // an agent write implicates THIS file (per-path precision the raw per-repo
  // refresh key lacks).
  const changedNonce = useScopedChangeNonce(filePath, refreshKey, changedPaths);

  const editor = useFileEditor({ path: filePath, size: null }, changedNonce, {
    write: async (path, content, baseline) => {
      await writeContentFile(path, content, baseline);
      markContentPathChanged("", path);
    },
    seed: viewableCacheContent,
    treatMissingAsRemoved: true,
  });
  const { state, buffer, setBuffer, dirty, save, saveError, retryRead } = editor;
  const { showStaleBanner, reloadFromDisk, keepEditing } = editor;
  const { reconciling, reconcileDisk, reconcileError } = editor;
  const { reconcileDiskMoved, refreshReconcile } = editor;
  const { startReconcile, cancelReconcile, takeTheirs, resolveSave } = editor;

  const [pendingDiscard, setPendingDiscard] = useState(false);
  // AC-6: show the cautionary advisory the first time this file is edited this
  // session; a dismiss (or the first show) marks it seen so it never re-nags.
  const [showAdvisory, setShowAdvisory] = useState(() => !advisoryShownThisSession.has(filePath));
  useEffect(() => {
    advisoryShownThisSession.add(filePath);
  }, [filePath]);

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  // SW-VE-4: a handle to the merge view (null until CM6 loads) — "Keep mine"
  // NO-OPs while loading so we never resolve (clobber the agent) against a disk
  // the user hasn't seen. Once ready it writes the editable right side (the buffer
  // if untouched, or the in-progress hand-merge).
  const mergeRef = useRef<CodeMergeViewHandle | null>(null);
  // SW-VE-9: a handle to the editable CodeEditor so Escape can query/close the
  // CM6 find panel deterministically. `panelOpenAtKeydown` is recorded in the
  // capture phase — BEFORE CM6's own searchKeymap can close the panel and mutate
  // its state — so the bubble-phase decision doesn't depend on focus location or
  // on a panel DOM node that may already be gone (the VE-8 best-effort trap).
  const editorRef = useRef<CodeEditorHandle | null>(null);
  // RF-perf-12: capture the editor's cursor/scroll on unmount and re-apply it on
  // remount (team switch / eviction). Snapshot is lightweight offsets only — no
  // CmEditorState/doc/history (AC-8). This callback ref ALSO populates the
  // imperative `editorRef` (search) so both consumers share one handle; restore
  // fires once the handle is ready (doc loaded → offsets valid), gated once per
  // file by `lastRestoredKeyRef`.
  const lastRestoredKeyRef = useRef<string | null>(null);
  const handleEditorRef = useCallback(
    (h: CodeEditorHandle | null) => {
      editorRef.current = h;
      if (h && viewStateKey && lastRestoredKeyRef.current !== viewStateKey) {
        const snap = useViewerStore.getState().readViewState(viewStateKey);
        if (snap) h.applyViewSnapshot(snap);
        lastRestoredKeyRef.current = viewStateKey;
      }
    },
    [viewStateKey],
  );
  const captureEditorState = useCallback(
    (snap: ViewSnapshot) => {
      if (viewStateKey) useViewerStore.getState().captureViewState(viewStateKey, snap);
    },
    [viewStateKey],
  );
  const panelOpenAtKeydown = useRef(false);
  const keepMine = useCallback(() => {
    const merged = mergeRef.current?.getLocalValue();
    if (merged == null) return; // merge view not ready — nothing reviewed yet
    void resolveSave(merged);
  }, [resolveSave]);

  // SW-VE-4: back out of reconcile WITHOUT resolving — but carry any in-progress
  // hand-merge back into the buffer first (the merge view's `localValue` is
  // seed-only, never synced back), so no exit path drops typed edits. `stale`
  // stays armed, so the preserved buffer still can't clobber.
  const backOutOfReconcile = useCallback(() => {
    const merged = mergeRef.current?.getLocalValue();
    if (merged != null && merged !== buffer) setBuffer(merged);
    cancelReconcile();
  }, [buffer, setBuffer, cancelReconcile]);

  // Exiting with unsaved edits confirms first (AC-4). A clean buffer leaves
  // immediately. If the merge view is up, back out of it first (lifting the
  // hand-merge) so the discard confirm reflects the real on-screen content.
  const requestExit = useCallback(() => {
    if (reconciling) {
      backOutOfReconcile();
      setPendingDiscard(true);
      return;
    }
    if (dirty) {
      setPendingDiscard(true);
      return;
    }
    onExit();
  }, [dirty, onExit, reconciling, backOutOfReconcile]);

  const editable = state.kind === "editable";
  const filename = basename(filePath);
  // SW-VE-8 + SW-VE-11: all per-type editor affordances (gutter, line-wrap,
  // folding, whitespace, tab size, prose vs mono font) resolved in ONE shared
  // place from the global settings, so every editor mount stays consistent.
  // Theme is still inherited by <CodeEditor> from the store (AC-11).
  const affordances = useEditorAffordances(filePath);

  // SW-VE-10: the rendered live-preview pane. Markdown-only (raw source stays
  // canonical for html/code/text); opt-in per edit session (default off, no
  // persisted setting — weight discipline, VE-8 lesson); and gated on `active`
  // so a sync-hidden inset mounts NO pane — the pane owns its own deferred render,
  // so while it's unmounted (closed or hidden) there is zero preview work at all.
  const previewEligible = isLivePreviewEligible(filePath);
  const [showPreview, setShowPreview] = useState(false);
  const previewOpen = previewEligible && showPreview && active;
  // SW-VE-4: keep the merge view mounted whenever reconciling — NOT gated on
  // `active`. When the git-sync review surface turns on, the parent `display:none`s
  // this whole inset (active=false ⟺ hidden), so a hidden merge view is already
  // write-free (its ⌘S/buttons can't be focused or clicked). Gating the MOUNT on
  // `active` instead would unmount CM6 mid-hand-merge and silently drop the typed
  // resolution — the one exit path that skips the `backOutOfReconcile` lift every
  // other exit uses ("never silently drop unsaved edits").
  const showMerge = editable && reconciling;

  return (
    <div
      data-testid="context-editor-inset"
      className="flex h-full min-h-0 flex-col"
      onKeyDownCapture={(e) => {
        // SW-VE-9: peek at find-open state BEFORE CM6's searchKeymap can handle
        // this Escape (capture runs outer→inner). This is the authoritative
        // signal the bubble handler decides on — not focus, not the panel DOM.
        if (e.key !== "Escape") return;
        panelOpenAtKeydown.current = editorRef.current?.isSearchOpen() ?? false;
      }}
      onKeyDown={(e) => {
        if (e.key !== "Escape") return;
        // Inset-owned modal states own Escape FIRST — even over the find panel —
        // so Escape always cancels a pending discard-confirm or backs out of an
        // open reconcile. Stop-propagate so viewer-level Escape handlers don't
        // also fire.
        if (pendingDiscard) {
          e.stopPropagation();
          setPendingDiscard(false);
          return;
        }
        if (reconciling) {
          e.stopPropagation();
          backOutOfReconcile();
          return;
        }
        // SW-VE-9 (AC-2/3): if the find panel was open when Escape was pressed,
        // this Escape belongs to find — close it (idempotent: CM6's own
        // searchKeymap may have already closed a panel-focused case) and do NOT
        // exit edit. The decision hinges on the captured pre-Escape state, so it
        // holds regardless of cursor location or whether CM6 already tore the
        // panel down. A SECOND Escape (find now closed) falls through to exit.
        if (panelOpenAtKeydown.current) {
          editorRef.current?.closeSearch();
          e.stopPropagation();
          return;
        }
        // Escape exits the editor (dirty-guarded via requestExit).
        e.stopPropagation();
        requestExit();
      }}
    >
      <div className="flex select-none items-center gap-1.5 border-b border-[var(--border)] px-2.5 py-1.5 text-xs">
        {/* AC-1: dirty dot beside the file name. */}
        {editable && dirty && (
          <InfoTip label="Unsaved changes" side="bottom">
            <span
              data-testid="context-editor-dirty-dot"
              aria-label="Unsaved changes"
              className="size-1.5 shrink-0 rounded-full bg-sky-500"
            />
          </InfoTip>
        )}
        <InfoTip label={filePath} side="bottom">
          <span className="min-w-0 flex-1 truncate font-mono text-[0.7rem] text-muted-foreground">
            {filename}
          </span>
        </InfoTip>
        {scope && <EditingInScope scope={scope} />}
        {/* SW-VE-10: markdown-only rendered-preview toggle. Hidden while
            reconciling (the merge view owns the surface then). Opt-in, default
            off; `active` gating lives in `previewOpen`, not here, so the button
            stays visible/tabbable even under a hidden inset. */}
        {editable && !reconciling && previewEligible && (
          <ChromeIconButton
            testId="context-editor-preview-toggle"
            ariaLabel={showPreview ? "Hide rendered preview" : "Show rendered preview"}
            tooltip="Rendered preview"
            active={showPreview}
            onClick={() => setShowPreview((v) => !v)}
          >
            <Columns2 size={13} />
          </ChromeIconButton>
        )}
        {/* Hidden while reconciling: the plain save is guarded (stale) and the
            merge view owns the resolving write (⌘S / Keep mine). */}
        {editable && !reconciling && (
          <ChromeIconButton
            testId="context-editor-save"
            ariaLabel="Save"
            tooltip="Save · ⌘S"
            onClick={() => void save(buffer)}
          >
            <Save size={13} className={cn(!dirty && "opacity-40")} />
          </ChromeIconButton>
        )}
        <ChromeIconButton
          testId="context-editor-done"
          ariaLabel="Done editing"
          tooltip="Done · Esc"
          onClick={requestExit}
        >
          <Check size={13} />
        </ChromeIconButton>
      </div>

      {/* AC-6: cautionary agent-ownership advisory — non-blocking, dismissible,
          once per file per session. Cautionary for EVERY file (no ownership
          signal exists — README #3). */}
      {showAdvisory && (
        <div
          role="note"
          data-testid="context-editor-advisory"
          className="flex items-center gap-2 border-b border-amber-500/40 bg-amber-500/10 px-2.5 py-1 text-[0.7rem] text-amber-700 dark:text-amber-400"
        >
          <TriangleAlert size={13} className="shrink-0" aria-hidden />
          <span className="min-w-0 flex-1">
            Agents may edit this file — your edits could be overwritten. Edit deliberately.
          </span>
          <button
            type="button"
            data-testid="context-editor-advisory-dismiss"
            onClick={() => setShowAdvisory(false)}
            className="shrink-0 rounded border border-amber-500/50 px-1.5 py-0.5 font-medium hover:bg-amber-500/20"
          >
            Got it
          </button>
        </div>
      )}

      {/* AC-1/AC-3 (SW-VE-4): an agent wrote the open file under a dirty buffer —
          the shared conflict UI (identical to the worktree editor). The stale
          banner blocks the clobbering save; the reconcile bar drives the
          `CodeMergeView`. Hidden appropriately: banner while not reconciling, bar
          while the merge view is up. */}
      {showStaleBanner && !reconciling && (
        <StaleConflictBanner
          testIdPrefix="context-editor"
          onResolve={startReconcile}
          onReload={reloadFromDisk}
          onKeepEditing={keepEditing}
        />
      )}
      {showMerge && (
        <ReconcileBar
          testIdPrefix="context-editor"
          onTakeTheirs={takeTheirs}
          onKeepMine={keepMine}
          onCancel={backOutOfReconcile}
          diskMoved={reconcileDiskMoved}
          onRefresh={refreshReconcile}
        />
      )}
      {reconcileError && (
        <ReconcileErrorBanner testIdPrefix="context-editor" message={reconcileError} />
      )}

      {/* AC-4 (VE-3): a save failure surfaces inline (non-blocking) with Retry; the
          buffer stays dirty so no edits are lost. Hidden while reconciling — the
          plain save is guarded there, so its Retry would be a dead no-op. */}
      {saveError && !reconciling && (
        <div
          role="alert"
          data-testid="context-editor-save-error"
          className="flex items-center gap-2 border-b border-destructive/40 bg-destructive/10 px-2.5 py-1 text-[0.7rem] text-destructive"
        >
          <span className="min-w-0 flex-1 truncate">Couldn’t save this file.</span>
          <button
            type="button"
            onClick={() => void save(buffer)}
            className="shrink-0 rounded border border-destructive/50 px-1.5 py-0.5 font-medium hover:bg-destructive/20"
          >
            Retry
          </button>
        </div>
      )}

      {/* AC-4 (VE-3): inline discard confirm — never a native blocking dialog. */}
      {pendingDiscard && (
        <div
          role="alertdialog"
          aria-label="Discard unsaved changes?"
          data-testid="context-editor-discard-confirm"
          className="flex items-center gap-2 border-b border-[var(--border)] bg-amber-500/10 px-2.5 py-1 text-[0.7rem]"
        >
          <span className="min-w-0 flex-1 truncate">Discard unsaved changes?</span>
          <button
            type="button"
            data-testid="context-editor-discard-confirm-yes"
            onClick={() => {
              setPendingDiscard(false);
              onExit();
            }}
            className="shrink-0 rounded border border-destructive/50 px-1.5 py-0.5 font-medium text-destructive hover:bg-destructive/20"
          >
            Discard
          </button>
          <button
            type="button"
            data-testid="context-editor-discard-confirm-no"
            onClick={() => setPendingDiscard(false)}
            className="shrink-0 rounded border border-border px-1.5 py-0.5 font-medium hover:bg-accent"
          >
            Keep editing
          </button>
        </div>
      )}

      <div className="min-h-0 flex-1 overflow-hidden p-0">
        {state.kind === "loading" && (
          <div
            aria-hidden
            data-testid="context-editor-skeleton"
            className="h-full w-full animate-pulse rounded bg-muted/40"
          />
        )}
        {state.kind === "readError" && (
          <div
            role="alert"
            data-testid="context-editor-read-error"
            className="flex h-full flex-col items-center justify-center gap-2 px-8 text-center text-xs text-destructive"
          >
            <span className="max-w-full truncate">Couldn’t read this file.</span>
            <button
              type="button"
              onClick={retryRead}
              className="rounded border border-border px-2 py-0.5 text-[0.7rem] font-medium text-foreground hover:bg-accent"
            >
              Retry
            </button>
          </div>
        )}
        {/* AC-4 (SW-VE-4): the agent deleted the open file while the buffer was
            clean — self-heal to the shared "was removed" empty state (mirrors the
            read-only viewer), never a stuck render. A DIRTY buffer never reaches
            here (the hook keeps the edits so a save recreates the file). */}
        {state.kind === "removed" && (
          <FileDeletedPlaceholder
            fileName={filename}
            onClose={() => closeActiveFileTab(filePath)}
          />
        )}
        {state.kind === "placeholder" && (
          <FileWarningPlaceholder
            message={state.message}
            filename={filename}
            testId="context-editor-placeholder"
          />
        )}
        {/* SW-VE-4 (AC-3): the reconcile surface — the EDITABLE disk-vs-buffer merge
            view. Keyed by path only, so a mid-reconcile `reconcileDisk` refresh
            updates the left side live WITHOUT remounting (right-side hand-merge
            preserved). ⌘S → the sanctioned `resolveSave`. */}
        {showMerge && (
          <CodeMergeView
            key={filePath}
            ref={mergeRef}
            diskValue={reconcileDisk}
            localValue={buffer}
            onSave={resolveSave}
            path={filePath}
            height="100%"
            ariaLabel={`Reconcile ${filename}`}
          />
        )}
        {editable && !showMerge && (
          <div className="flex h-full min-h-0">
            {/* SW-VE-10: raw CM6 editor (canonical bytes) on the left; when the
                preview is open it takes half the width, else full. */}
            <div
              className={cn(
                "min-h-0",
                previewOpen ? "w-1/2 border-r border-[var(--border)]" : "w-full",
              )}
            >
              <CodeEditor
                key={filePath}
                ref={handleEditorRef}
                value={buffer}
                onChange={setBuffer}
                onSave={save}
                dirty={dirty}
                readOnly={!active}
                path={filePath}
                gutters={affordances.gutters}
                lineWrap={affordances.lineWrap}
                folding={affordances.folding}
                showWhitespace={affordances.showWhitespace}
                tabSize={affordances.tabSize}
                fontFamily={affordances.fontFamily}
                search
                height="100%"
                ariaLabel={`Editor for ${filename}`}
                onBeforeDestroy={captureEditorState}
              />
            </div>
            {/* SW-VE-10: rendered projection of the live buffer (deferred inside
                the pane). Only
                mounted when open — no preview cost when off or under a hidden
                sync inset. */}
            {previewOpen && (
              <div className="min-h-0 w-1/2">
                <MarkdownPreviewPane content={buffer} proseInvert={false} />
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
