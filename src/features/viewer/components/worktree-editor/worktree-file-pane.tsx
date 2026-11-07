// SPDX-License-Identifier: MIT
import { FileWarning } from "lucide-react";
import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { basename } from "@/shared/lib/basename";
import {
  CodeEditor,
  CodeMergeView,
  detectIndentation,
  detectLanguageId,
  detectLineEnding,
  summarizeLineChanges,
  computeLineChanges,
  useEditorAffordances,
  type CodeEditorHandle,
  type CodeMergeViewHandle,
  type CursorInfo,
  type Indentation,
  type LineChangeSummary,
  type LineEnding,
  type LanguageId,
} from "@/shared/components/code-editor";

import {
  TOO_LARGE_MESSAGE,
  useFileEditor,
  type FileDraft,
  type SelectedFile,
} from "../../lib/use-file-editor";
import { WORKTREE_MAX_VIEW_BYTES } from "../../lib/worktree-file-view";
import type { WorktreeEditorBackend } from "../../lib/worktree-editor-backend";
import { ReconcileBar, ReconcileErrorBanner, StaleConflictBanner } from "../conflict-banners";
import { FileWarningPlaceholder } from "../file-warning-placeholder";
import { EditorSkeleton, EditorStateMessage } from "./editor-states";

/** What the workbench chrome (tab strip, status bar, breadcrumbs) shows for a
 *  pane. Reported upward whenever any of it changes. */
export interface PaneStatus {
  kind: "loading" | "readError" | "placeholder" | "editable" | "removed" | "idle";
  dirty: boolean;
  saving: boolean;
  saveError: string | null;
  /** Disk diverged under unsaved edits (the conflict guard is armed). */
  stale: boolean;
  reconciling: boolean;
  language: LanguageId;
  lineEnding: LineEnding;
  indentation: Indentation;
  cursor: CursorInfo | null;
  /** Line changes vs the committed version; `null` when unknown/unsupported. */
  changes: LineChangeSummary | null;
  /** Whether a committed baseline is known (false: unsupported/not loaded). */
  baselineKnown: boolean;
  /** Soft wrap as currently applied (per-type default or the override). */
  lineWrap: boolean;
}

/** Imperative calls the chrome makes on a pane (⌘S from the tab strip, go to
 *  line from quick open, close guards). */
export interface PaneApi {
  save: () => Promise<void>;
  revealLine: (line: number, column?: number) => void;
  openSearch: () => void;
  focus: () => void;
  isDirty: () => boolean;
  /** The pane is being closed on purpose: drop its draft instead of keeping it. */
  markDiscard: () => void;
  /** Lift an open merge's hand edits into the buffer and leave the merge view. */
  backOutOfReconcile: () => void;
  isReconciling: () => boolean;
}

/** What an unmounting pane keeps: its dirty buffer, or — mid-merge — the
 *  merge's right side (`merged`), which lives only in the merge view. */
export function draftToKeep(
  d: FileDraft & { dirty: boolean },
  merged: string | null,
  disk: string | null,
): FileDraft | null {
  if (merged != null) {
    // The merge's right side is the user's latest word on this file, even
    // when it equals the old baseline (they merged their edits away).
    // Against the old baseline, so the remount re-arms the conflict guard.
    if (merged !== d.lastSaved) return { buffer: merged, lastSaved: d.lastSaved };
    // Equal to the old baseline: kept dirty against the disk the merge was
    // showing, which a remount then checks like any restored draft.
    if (disk != null && merged !== disk) return { buffer: merged, lastSaved: disk };
    return null;
  }
  return d.dirty ? { buffer: d.buffer, lastSaved: d.lastSaved } : null;
}

export interface WorktreeFilePaneProps {
  /** The tab's identity (its repo-relative path). Every report upward is keyed
   *  by it, never by `file.path`, which moves with the root. */
  id: string;
  /** Where the file is read and written now. */
  file: SelectedFile;
  active: boolean;
  backend: WorktreeEditorBackend;
  /** The workspace's external-change signal (watcher nonce). */
  changedNonce: number;
  /** A preserved unsaved draft to restore on mount (unfocus round-trip). */
  restore: FileDraft | null;
  /** Mirror (or clear, with `null`) this pane's unsaved draft. */
  onPersistDraft: (id: string, draft: FileDraft | null) => void;
  onStatus: (id: string, status: PaneStatus) => void;
  registerApi: (id: string, api: PaneApi | null) => void;
  /** Soft-wrap override from the status bar (`null` = the per-type default). */
  wrapOverride: boolean | null;
}

/**
 * One open file in the worktree editor: its read / edit / save / conflict
 * state ({@link useFileEditor}), its committed baseline for the change gutter,
 * and the editor (or merge view, or a read-only state) that shows it.
 *
 * Every open tab keeps its pane mounted — hidden when inactive — so switching
 * tabs keeps the undo history, cursor, scroll and an in-progress merge, and a
 * background tab still reconciles disk changes the moment they land.
 */
export const WorktreeFilePane = memo(function WorktreeFilePane({
  id,
  file,
  active,
  backend,
  changedNonce,
  restore,
  onPersistDraft,
  onStatus,
  registerApi,
  wrapOverride,
}: WorktreeFilePaneProps) {
  const editor = useFileEditor(file, changedNonce, {
    write: backend.write,
    remote: backend.remote,
    restore,
    // Every open tab stays mounted; a background remote tab still notices an
    // in-place machine edit, just on a slower beat than the one in front.
    pollMs: active ? 2500 : 10000,
  });
  const { state, buffer, setBuffer, dirty, save, saveError, saving, getDraft, diskText } = editor;
  const { showStaleBanner, reloadFromDisk, keepEditing, stale } = editor;
  const { reconciling, reconcileDisk, reconcileError } = editor;
  const { reconcileDiskMoved, refreshReconcile } = editor;
  const { startReconcile, cancelReconcile, takeTheirs, resolveSave, retryRead } = editor;

  const affordances = useEditorAffordances(file.path);
  const lineWrap = wrapOverride ?? affordances.lineWrap;
  const language = useMemo(() => detectLanguageId(file.path), [file.path]);
  // Format detection reads the disk text: it only moves on a read or a save,
  // never per keystroke.
  const lineEnding = useMemo(() => detectLineEnding(diskText), [diskText]);
  const indentation = useMemo(
    () => detectIndentation(diskText, { useTabs: false, size: affordances.tabSize }),
    [diskText, affordances.tabSize],
  );

  // The committed baseline for the change gutter. Refetched when the worktree
  // changes underneath (a commit or checkout moves HEAD) — for the visible tab
  // only, so a busy agent doesn't cost a git call per open tab per write — and
  // when a tab is brought forward, so it never lags far behind a terminal
  // `git commit`.
  const [head, setHead] = useState<string | null | undefined>(undefined);
  const editable = state.kind === "editable";
  // Flips when the tab is brought forward, so activation refetches too.
  const headSignal = active ? changedNonce : -1;
  // A new location (root move) or backend has its own HEAD: no stale marks
  // from the old one while its baseline loads.
  const [headFor, setHeadFor] = useState({ path: file.path, backend });
  if (headFor.path !== file.path || headFor.backend !== backend) {
    setHeadFor({ path: file.path, backend });
    setHead(undefined);
  }
  useEffect(() => {
    if (!editable) return;
    let cancelled = false;
    backend
      .readHead(file.path)
      .then((text) => {
        if (!cancelled) setHead(text);
      })
      .catch(() => {
        // A failed refetch keeps the last baseline (the gutter doesn't vanish
        // on a blip); with none yet there is simply no gutter.
      });
    return () => {
      cancelled = true;
    };
  }, [backend, file.path, editable, headSignal]);

  // Change summary for the status bar: computed off the live buffer only for
  // the active pane, debounced so typing stays cheap.
  const [computedChanges, setComputedChanges] = useState<LineChangeSummary | null>(null);
  // A new (untracked) file (`head === null`) has no committed version to count
  // against: no summary, as the gutter shows no marks — never "unchanged".
  const tracking = active && head != null && editable;
  useEffect(() => {
    if (!tracking || head == null) return;
    const timer = setTimeout(() => {
      setComputedChanges(summarizeLineChanges(computeLineChanges(head, buffer)));
    }, 200);
    return () => clearTimeout(timer);
  }, [tracking, head, buffer]);
  const changes = tracking ? computedChanges : null;

  const [cursor, setCursor] = useState<CursorInfo | null>(null);

  // Report upward. Built field-by-field so an unrelated re-render doesn't
  // allocate a "changed" status object.
  useEffect(() => {
    onStatus(id, {
      kind: state.kind,
      dirty,
      saving,
      saveError,
      stale,
      reconciling,
      language,
      lineEnding,
      indentation,
      cursor,
      changes,
      baselineKnown: head !== undefined,
      lineWrap,
    });
  }, [
    onStatus,
    id,
    state.kind,
    dirty,
    saving,
    saveError,
    stale,
    reconciling,
    language,
    lineEnding,
    indentation,
    cursor,
    changes,
    head,
    lineWrap,
  ]);

  // The merge splits both sides on the buffer's separator. Disk an agent
  // rewrote with the other line ending is shown in the buffer's, so its lines
  // still line up (Take theirs adopts the disk text as it is).
  const mergeDisk = useMemo(
    () => withLineEnding(reconcileDisk, lineEnding),
    [reconcileDisk, lineEnding],
  );

  const editorRef = useRef<CodeEditorHandle | null>(null);
  // A line jump asked for before the editor exists (still reading, CodeMirror
  // still loading, or the merge view up) lands the moment it mounts.
  const pendingJumpRef = useRef<{ line: number; column?: number } | null>(null);
  const attachEditor = useCallback((handle: CodeEditorHandle | null) => {
    editorRef.current = handle;
    const jump = pendingJumpRef.current;
    if (handle && jump) {
      pendingJumpRef.current = null;
      handle.revealLine(jump.line, jump.column);
    }
  }, []);
  const mergeRef = useRef<CodeMergeViewHandle | null>(null);

  // "Keep mine" writes whatever the merge's editable side holds — but only once
  // the merge view has loaded, so nothing resolves against an unseen disk.
  const keepMine = useCallback(() => {
    const merged = mergeRef.current?.getLocalValue();
    if (merged == null) return;
    void resolveSave(merged);
  }, [resolveSave]);

  // Leaving the merge without resolving carries any hand-merge back into the
  // buffer (the merge's right side is CM-local), so no exit drops edits.
  const backOutOfReconcile = useCallback(() => {
    const merged = mergeRef.current?.getLocalValue();
    if (merged != null && merged !== buffer) setBuffer(merged);
    cancelReconcile();
  }, [buffer, setBuffer, cancelReconcile]);

  // Drafts: clear the instant the buffer goes clean; on unmount keep a dirty
  // buffer (an unfocus) unless the pane was closed on purpose.
  const discardRef = useRef(false);
  // An unmount mid-merge (the tile lost focus) must keep the hand-merged right
  // side, which lives only in the merge view. A layout cleanup runs before the
  // merge view's own teardown, while its handle can still be read; the draft
  // cleanup below (a passive one, after the merge view is gone) picks it up.
  const mergedAtUnmountRef = useRef<string | null>(null);
  useLayoutEffect(
    () => () => {
      mergedAtUnmountRef.current = mergeRef.current?.getLocalValue() ?? null;
    },
    [],
  );
  const latest = useRef({ getDraft, onPersistDraft, path: id, reconcileDisk });
  useEffect(() => {
    latest.current = { getDraft, onPersistDraft, path: id, reconcileDisk };
  });
  useEffect(() => {
    if (!dirty) latest.current.onPersistDraft(latest.current.path, null);
  }, [dirty]);
  useEffect(
    () => () => {
      const {
        getDraft: snapshot,
        onPersistDraft: persist,
        path,
        reconcileDisk: disk,
      } = latest.current;
      if (discardRef.current) {
        persist(path, null);
        return;
      }
      persist(path, draftToKeep(snapshot(), mergedAtUnmountRef.current, disk));
    },
    [],
  );

  // The chrome's handle on this pane.
  const apiState = useRef({ save, getDraft, reconciling, backOutOfReconcile });
  useEffect(() => {
    apiState.current = { save, getDraft, reconciling, backOutOfReconcile };
  });
  useEffect(() => {
    const api: PaneApi = {
      // The model's latest buffer (a hand-merge just lifted in included), not
      // the last committed render's.
      save: () => apiState.current.save(apiState.current.getDraft().buffer),
      revealLine: (line, column) => {
        if (editorRef.current) editorRef.current.revealLine(line, column);
        else pendingJumpRef.current = { line, column };
      },
      openSearch: () => editorRef.current?.openSearch(),
      focus: () => editorRef.current?.focus(),
      // Read from the model's refs, not render state, so a check right after an
      // awaited save sees the save's result.
      // Under the conflict guard the tab holds work even when its buffer
      // reads clean against the old baseline (file-buffer-model).
      isDirty: () => apiState.current.getDraft().dirty,
      markDiscard: () => {
        discardRef.current = true;
      },
      backOutOfReconcile: () => apiState.current.backOutOfReconcile(),
      // From the machine, so a check right after backing out sees it.
      isReconciling: () => apiState.current.getDraft().reconciling,
    };
    registerApi(id, api);
    return () => registerApi(id, null);
  }, [registerApi, id]);

  // Re-focus the editor when its tab comes forward (autoFocus is mount-only).
  useEffect(() => {
    if (active) editorRef.current?.focus();
  }, [active]);

  const name = basename(file.path);

  return (
    <div
      className="flex min-h-0 flex-1 flex-col"
      hidden={!active}
      data-testid={active ? "worktree-pane-active" : "worktree-pane"}
      data-path={file.path}
    >
      {showStaleBanner && !reconciling && (
        <StaleConflictBanner
          testIdPrefix="worktree"
          where={backend.remote ? "on the machine" : "on disk"}
          onResolve={startReconcile}
          onReload={reloadFromDisk}
          onKeepEditing={keepEditing}
        />
      )}
      {reconciling && (
        <ReconcileBar
          testIdPrefix="worktree"
          where={backend.remote ? "on the machine" : "on disk"}
          onTakeTheirs={takeTheirs}
          onKeepMine={keepMine}
          onCancel={backOutOfReconcile}
          diskMoved={reconcileDiskMoved}
          onRefresh={refreshReconcile}
        />
      )}
      {reconcileError && <ReconcileErrorBanner testIdPrefix="worktree" message={reconcileError} />}
      {/* A refused save that armed the conflict guard is explained by the
          banner above; repeating it here would offer a Retry that's blocked. */}
      {saveError && !reconciling && !stale && (
        <div
          role="alert"
          data-testid="worktree-save-error"
          className="flex items-center gap-2 border-b border-destructive/30 bg-destructive/10 px-3 py-1.5 text-xs text-destructive"
        >
          <span className="min-w-0 flex-1 truncate" title={saveError}>
            Couldn’t save {name}. {saveError}
          </span>
          <button
            type="button"
            onClick={() => void save(buffer)}
            className="shrink-0 rounded-md border border-destructive/40 px-2 py-0.5 font-medium hover:bg-destructive/15 focus-visible:outline-2 focus-visible:[outline-color:var(--ring)]"
          >
            Retry
          </button>
        </div>
      )}

      <div className="relative min-h-0 flex-1 overflow-hidden">
        {state.kind === "loading" && <EditorSkeleton />}
        {state.kind === "readError" && (
          <EditorStateMessage
            tone="destructive"
            testId="worktree-file-error"
            title="Couldn’t read this file."
            detail={state.message}
            action={
              <button
                type="button"
                onClick={retryRead}
                className="rounded-md border border-border px-2.5 py-1 text-xs font-medium text-foreground hover:bg-muted focus-visible:outline-2 focus-visible:[outline-color:var(--ring)]"
              >
                Retry
              </button>
            }
          />
        )}
        {state.kind === "placeholder" && state.message === TOO_LARGE_MESSAGE && (
          <TooLargeState name={name} size={file.size} />
        )}
        {state.kind === "placeholder" && state.message !== TOO_LARGE_MESSAGE && (
          <FileWarningPlaceholder
            message={state.message}
            filename={name}
            testId="worktree-file-placeholder"
          />
        )}
        {state.kind === "removed" && (
          <EditorStateMessage title="This file was removed." detail={name} />
        )}
        {editable && reconciling && (
          <CodeMergeView
            key={id}
            ref={mergeRef}
            diskValue={mergeDisk}
            localValue={buffer}
            onSave={resolveSave}
            path={file.path}
            lineSeparator={lineEnding}
            height="100%"
            ariaLabel={`Reconcile ${name}`}
          />
        )}
        {editable && !reconciling && (
          <CodeEditor
            key={id}
            ref={attachEditor}
            value={buffer}
            onChange={setBuffer}
            onSave={save}
            path={file.path}
            gutters
            lineWrap={lineWrap}
            folding={affordances.folding}
            showWhitespace={affordances.showWhitespace}
            tabSize={indentation.size}
            indentWithTabs={indentation.useTabs}
            lineSeparator={lineEnding}
            fontFamily={affordances.fontFamily}
            search
            ide
            changeBaseline={head}
            onCursorChange={setCursor}
            autoFocus={active}
            height="100%"
            ariaLabel={`Editor for ${name}`}
          />
        )}
      </div>
    </div>
  );
});

/** `text` with its line breaks in `eol`, when it uses the other ending. */
export function withLineEnding(text: string, eol: LineEnding): string {
  if (detectLineEnding(text) === eol) return text;
  return text.replace(/\r?\n/g, eol);
}

/** Decimal megabytes, as Finder shows file sizes. */
function megabytes(bytes: number): string {
  const mb = bytes / 1_000_000;
  return `${mb >= 10 ? Math.round(mb) : Math.round(mb * 10) / 10} MB`;
}

/** A file over the editor's size limit: how big it is and what the editor
 *  takes, so "too large" reads as a limit, not a failure. */
function TooLargeState({ name, size }: { name: string; size: number | null }) {
  return (
    <div
      data-testid="worktree-file-placeholder"
      className="flex h-full flex-col items-center justify-center gap-3 px-8 text-center"
    >
      <FileWarning aria-hidden className="size-8 text-muted-foreground" />
      <div className="space-y-1">
        <p className="text-sm font-medium">Too large to open in the editor</p>
        <p className="font-mono text-xs text-muted-foreground">
          {name}
          {size != null && ` · ${megabytes(size)}`}
        </p>
        <p className="max-w-[44ch] text-xs text-muted-foreground">
          The editor opens files up to {megabytes(WORKTREE_MAX_VIEW_BYTES)}. Use a terminal or your
          own editor for this one.
        </p>
      </div>
    </div>
  );
}
