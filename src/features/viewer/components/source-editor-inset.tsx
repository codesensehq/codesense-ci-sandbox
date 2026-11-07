// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { LocateFixed, PanelLeft, PanelLeftClose, RefreshCw, Save, Search, X } from "lucide-react";
import { toast } from "sonner";

import { cn } from "@/lib/utils";
import { basename } from "@/shared/lib/basename";
import { toastFailure } from "@/shared/lib/with-error-toast";
import type { Workspace, WorktreeEditorDraft } from "@/shared/stores/viewer-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { getKindCapabilities } from "@/shared/lib/workspace-kind-capabilities";
import { setScratchFileUseTeam } from "@/tauri/commands";
import { workspaceSourceRepo, isRemoteWorkspace } from "@/shared/lib/workspace-location";
import { useMachine } from "@/features/machines/stores/machines-store";
import { machineOfflineWord } from "@/features/machines/components/machine-location-badge";
import { ScratchItemContextMenu } from "@/features/scratch-space/components/scratch-item-actions";

import { useWorktreeWatch } from "../lib/use-worktree-watch";
import { useRemoteWatchNonce, useRemoteWorktreeWatch } from "../lib/use-remote-worktree-watch";
import { useRemoteCadence } from "../lib/use-remote-cadence";
import { useSessionCwd } from "../hooks/use-session-cwd";
import { useSessionCwdRevision } from "../hooks/use-session-cwd-revision";
import { relativeToRoot, useScratchOverlayFiles } from "../lib/use-scratch-overlay-files";
import { closeTab, cycleTab, openTab } from "../lib/editor-tabs";
import { useWorktreeEditorRevealStore } from "../lib/open-in-worktree-editor";
import { describeFileError, type FileDraft, type SelectedFile } from "../lib/use-file-editor";
import {
  useWorktreeEditorBackend,
  type WorktreeEditorBackend,
} from "../lib/worktree-editor-backend";
import { WorktreeFileTree, type WorktreeFileSelection } from "./worktree-file-tree";
import { ScratchHeaderBadge } from "./scratch-file-badge";
import { OpenInExternalEditorButton } from "./open-in-external-editor-button";
import { OverlayShell, ChromeIconButton } from "./overlay-shell";
import {
  WorktreeFilePane,
  type PaneApi,
  type PaneStatus,
} from "./worktree-editor/worktree-file-pane";
import { useFocusWhenShown } from "@/shared/lib/warm-scopes";
import { EditorTabStrip, type TabState } from "./worktree-editor/editor-tab-strip";
import { EditorStatusBar } from "./worktree-editor/editor-status-bar";
import { EditorBreadcrumbs } from "./worktree-editor/editor-breadcrumbs";
import { EditorQuickOpen } from "./worktree-editor/editor-quick-open";
import { EditorStateMessage, Kbd } from "./worktree-editor/editor-states";

/** A pending discard confirmation: closing a dirty tab, or the whole editor
 *  while any tab is dirty. Held until confirmed or cancelled — unsaved edits
 *  are never dropped silently. Paths are repo-relative, like the tabs. */
type PendingDiscard = { type: "tab"; path: string } | { type: "editor"; dirty: string[] };

/** Below this container width the file tree becomes a drawer over the editor
 *  instead of a column beside it. */
const NARROW_PX = 640;

interface SourceEditorInsetProps {
  workspace: Workspace;
  /** The owning team: a retained hidden team's editor holds no worktree watch. */
  teamKey?: string;
  /** The file to bring forward on mount (repo-relative), e.g. the diff's
   *  "Open in editor" selection or the remembered active tab. */
  seedRelPath: string | null;
  /** A 1-based line in `seedRelPath` to land on once it opens — the change the
   *  diff was showing when the user chose "Open in code editor". */
  seedLine?: number | null;
  /** The tabs that were open last time (repo-relative), restored on mount. */
  initialTabs?: readonly string[];
  /** Reports the active file (repo-relative; `null` when none) so the tile can
   *  resume it. */
  onSelectRelPath?: (relPath: string | null) => void;
  /** Reports the open tabs (repo-relative) for the same reason. */
  onTabsChange?: (relPaths: string[]) => void;
  /** Preserved unsaved drafts by repo-relative path (an unfocus round-trip). */
  restoreDrafts?: Readonly<Record<string, WorktreeEditorDraft>>;
  /** Mirror (or clear) one file's unsaved draft. */
  onPersistDraft?: (relPath: string, draft: WorktreeEditorDraft | null) => void;
  /** Close the editor. */
  onClose: () => void;
}

/**
 * The worktree code editor: a file tree, tabs over a full editor (find and
 * replace, multi-cursor, folding, a change gutter against `HEAD`), quick open
 * (⌘P) and go to line (⌃G), breadcrumbs and a status bar — floated over the
 * workspace tile. The same surface edits a worktree on this Mac or on a remote
 * machine; {@link useWorktreeEditorBackend} picks the side once.
 *
 * Saves go through each pane's {@link useFileEditor}: a write never clobbers a
 * change that landed underneath (watcher, write-time baseline check, or the
 * remote hash lock all route to the same conflict guard and merge view).
 */
export function SourceEditorInset({
  workspace: ws,
  teamKey,
  seedRelPath,
  seedLine,
  initialTabs,
  onSelectRelPath,
  onTabsChange,
  restoreDrafts,
  onPersistDraft,
  onClose,
}: SourceEditorInsetProps) {
  const backend = useWorktreeEditorBackend(ws);
  const remote = isRemoteWorkspace(ws);
  // The effective root: a remote tab keeps `ws.cwd` empty so nothing local
  // treats the remote directory as a path on this Mac.
  const reportedRoot = remote ? (ws.location.cwd ?? "") : ws.cwd;
  const rootPath = reportedRoot.replace(/\/+$/, "");
  // A worktree AT "/" trims to "" (so paths still join as "/" + rel) but is a
  // real root, unlike a root not reported yet.
  const rootReady = reportedRoot.trim() !== "";
  const rootLabel = rootReady && rootPath === "" ? "/" : basename(rootPath) || rootPath;
  const machine = useMachine(remote ? ws.location.machineId : "");
  // The machine's display name once the machines list has loaded; its id
  // meanwhile, so the status bar never says just "Remote".
  const machineLabel = remote ? (machine?.label ?? ws.location.machineId) : null;
  // Only once the machines list has loaded: an unknown machine isn't "offline".
  const machineOffline = remote && machine ? machineOfflineWord(machine) : null;

  const toAbs = useCallback((rel: string) => `${rootPath}/${rel}`, [rootPath]);
  // One stable object per (path, size), so memoized panes skip unrelated renders.
  const filesRef = useRef(new Map<string, SelectedFile>());
  const fileFor = (rel: string): SelectedFile => {
    const path = toAbs(rel);
    const listed = sizesRef.current.get(rel);
    const size = listed && listed.root === rootPath ? listed.size : null;
    const known = filesRef.current.get(rel);
    if (known && known.path === path && known.size === size) return known;
    const next = { path, size };
    filesRef.current.set(rel, next);
    return next;
  };
  const toRel = useCallback(
    (abs: string) => (abs.startsWith(`${rootPath}/`) ? abs.slice(rootPath.length + 1) : abs),
    [rootPath],
  );

  // ── Tabs ────────────────────────────────────────────────────────────────
  // Identity follows file-buffer-model: tabs, panes, statuses, drafts, sizes and
  // line jumps are keyed by the repo-relative path. The root is only how a path
  // reaches disk (`toAbs`), so a root that arrives or moves after the tabs were
  // seeded re-targets the open panes in place and never re-keys them.
  const [tabs, setTabs] = useState<string[]>(() => {
    const seeded = [...(initialTabs ?? [])];
    if (seedRelPath && !seeded.includes(seedRelPath)) seeded.push(seedRelPath);
    return seeded;
  });
  const [active, setActive] = useState<string | null>(
    () => seedRelPath ?? tabs[tabs.length - 1] ?? null,
  );
  const apisRef = useRef(new Map<string, PaneApi>());
  // The draft store's side of the ownership handoff (file-buffer-model): drafts
  // for tabs with no mounted pane. Seeded once from the stored drafts at mount
  // (so a later store write never re-seeds a live pane); a pane takes its draft
  // when it registers and hands one back when it unmounts; closing a tab on
  // purpose drops it.
  const [initialDrafts] = useState(() => new Map(Object.entries(restoreDrafts ?? {})));
  const initialDraftsRef = useRef(initialDrafts);
  const persistRef = useRef(onPersistDraft);
  useEffect(() => {
    persistRef.current = onPersistDraft;
  });
  const [pending, setPending] = useState<PendingDiscard | null>(null);
  // Byte sizes the tree reported, for the pre-read size gate.
  // Each size remembers the root it was listed under: a different root lists
  // different files, and an old size would gate reads (or a carried draft) on
  // the wrong file's size.
  const sizesRef = useRef(new Map<string, { root: string; size: number | null }>());
  const [statuses, setStatuses] = useState<ReadonlyMap<string, PaneStatus>>(() => new Map());
  const [palette, setPalette] = useState<{ query: string } | null>(null);
  const [wrapOverride, setWrapOverride] = useState<boolean | null>(null);

  // The latest committed tab set, for handlers that finish after an await (a
  // slow save) and must not act on the list they captured at click time.
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  });
  // A tab that leaves the set (closed, or evicted at the cap) leaves no status
  // or size behind.
  useEffect(() => {
    const open = new Set(tabs);
    for (const p of sizesRef.current.keys()) if (!open.has(p)) sizesRef.current.delete(p);
    setStatuses((prev) => {
      if ([...prev.keys()].every((p) => open.has(p))) return prev;
      return new Map([...prev].filter(([p]) => open.has(p)));
    });
  }, [tabs]);

  // Report the active file + tab set up so a re-open resumes them.
  const reportRef = useRef({ onSelectRelPath, onTabsChange });
  useEffect(() => {
    reportRef.current = { onSelectRelPath, onTabsChange };
  });
  useEffect(() => {
    reportRef.current.onSelectRelPath?.(active);
  }, [active]);
  useEffect(() => {
    reportRef.current.onTabsChange?.(tabs);
  }, [tabs]);

  // A line jump for a pane that isn't mounted yet waits here until the pane
  // registers; the pane itself holds it until its editor has loaded, so a slow
  // read (a remote machine) never drops it.
  const pendingRevealRef = useRef(new Map<string, { line: number; column: number }>());

  // Unsaved work in a tab, including one whose pane isn't mounted (the backend
  // went away for a moment): its kept draft. A pane that unmounts keeps a draft
  // for exactly the work it still holds (and clears it otherwise), so the last
  // status an unmounted pane reported — possibly a stale "dirty" — is not used:
  // a Save could never satisfy it.
  const hasUnsavedWork = useCallback((rel: string) => {
    const api = apisRef.current.get(rel);
    if (api) return api.isDirty() || api.isReconciling();
    return initialDraftsRef.current.has(rel);
  }, []);

  const openFile = useCallback(
    (
      file: { rel: string; size: number | null },
      at?: { line: number | null; column: number | null },
    ) => {
      const { rel } = file;
      const known = sizesRef.current.get(rel);
      if (file.size != null || !known || known.root !== rootPath) {
        sizesRef.current.set(rel, { root: rootPath, size: file.size });
      }
      // A tab mid-merge or under the conflict guard holds work outside its
      // buffer (the hand-merged side, the guarded divergence): never evict it.
      // Nor one whose pane is down holding a kept draft: evicting it would let
      // the surface drop that stored draft.
      const keep = (p: string) => {
        const st = statuses.get(p);
        return hasUnsavedWork(p) || st?.reconciling === true || st?.stale === true;
      };
      setTabs((prev) => openTab(prev, rel, { active, isDirty: keep }));
      setActive(rel);
      if (at?.line != null) {
        const target = { line: at.line, column: at.column ?? 1 };
        const api = apisRef.current.get(rel);
        if (api) api.revealLine(target.line, target.column);
        else pendingRevealRef.current.set(rel, target);
      }
    },
    [active, hasUnsavedWork, statuses, rootPath],
  );

  // The seed line applies to the mount it arrived with, once.
  const seedLineRef = useRef(seedRelPath && seedLine != null ? seedLine : null);
  useEffect(() => {
    const line = seedLineRef.current;
    seedLineRef.current = null;
    if (line != null && seedRelPath) {
      const api = apisRef.current.get(seedRelPath);
      if (api) api.revealLine(line, 1);
      else pendingRevealRef.current.set(seedRelPath, { line, column: 1 });
    }
    // Mount-only by design: later seed changes are the user browsing.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // Requests from other tools (`openInWorktreeEditor`): open the file, jump.
  const revealRequest = useViewerRevealRequest(ws.id);
  const consumeReveal = useWorktreeEditorRevealStore((s) => s.consume);
  const openFileRef = useRef(openFile);
  useEffect(() => {
    openFileRef.current = openFile;
  });
  useEffect(() => {
    if (!revealRequest) return;
    openFileRef.current(
      { rel: revealRequest.path, size: null },
      { line: revealRequest.line, column: revealRequest.column },
    );
    consumeReveal(ws.id, revealRequest.nonce);
  }, [revealRequest, consumeReveal, ws.id]);

  // Functional updates against the latest tabs: a close that lands after a slow
  // save must not drop tabs opened while it ran.
  const removeTab = useCallback((path: string) => {
    apisRef.current.get(path)?.markDiscard();
    initialDraftsRef.current.delete(path);
    const current = tabsRef.current;
    setTabs((prev) => prev.filter((t) => t !== path));
    setActive((prev) => closeTab(current, path, prev).active);
  }, []);

  const requestCloseTab = useCallback(
    (path: string) => {
      const api = apisRef.current.get(path);
      if (api?.isReconciling()) api.backOutOfReconcile();
      if (hasUnsavedWork(path)) {
        setActive(path);
        setPending({ type: "tab", path });
        return;
      }
      removeTab(path);
    },
    [removeTab, hasUnsavedWork],
  );

  // ── Closing the editor ──────────────────────────────────────────────────
  const closeNow = useCallback(() => {
    for (const api of apisRef.current.values()) api.markDiscard();
    // Tabs whose pane is down hold their draft in the store: closing the
    // editor closes them on purpose too (the prompt already asked).
    for (const rel of initialDraftsRef.current.keys()) persistRef.current?.(rel, null);
    initialDraftsRef.current.clear();
    onClose();
  }, [onClose]);
  // The tabs holding unsaved work right now — a mounted pane's buffer, or the
  // stored draft of a tab whose pane is down. Any in-progress hand-merge is
  // lifted into its buffer first, so every exit shares the same no-drop rule.
  const collectDirty = useCallback(() => {
    for (const api of apisRef.current.values()) {
      if (api.isReconciling()) api.backOutOfReconcile();
    }
    return tabsRef.current.filter((t) => {
      const api = apisRef.current.get(t);
      return api ? api.isDirty() : initialDraftsRef.current.has(t);
    });
  }, []);
  const requestCloseEditor = useCallback(() => {
    const dirty = collectDirty();
    if (dirty.length > 0) {
      setPending({ type: "editor", dirty });
      return;
    }
    closeNow();
  }, [collectDirty, closeNow]);

  const confirmDiscard = useCallback(() => {
    const p = pending;
    setPending(null);
    if (!p) return;
    if (p.type === "tab") {
      removeTab(p.path);
      return;
    }
    // The prompt named the files dirty when it opened. One edited while it was
    // up was never offered: ask again rather than discard it unseen.
    const now = collectDirty();
    if (now.some((t) => !p.dirty.includes(t))) {
      setPending({ type: "editor", dirty: now });
      return;
    }
    closeNow();
  }, [pending, removeTab, closeNow, collectDirty]);

  // The prompt currently up, read after an await: a "Keep editing" (or another
  // prompt) while the saves ran means the user changed their mind.
  const pendingRef = useRef(pending);
  useEffect(() => {
    pendingRef.current = pending;
  });
  const saveThenFinish = useCallback(async () => {
    const p = pending;
    if (!p) return;
    const paths = p.type === "tab" ? [p.path] : p.dirty;
    // Settled, not all: one failing save must still reach the still-dirty
    // check below instead of leaving the prompt stuck on an unhandled rejection.
    await Promise.allSettled(paths.map((path) => apisRef.current.get(path)?.save()));
    if (pendingRef.current !== p) return;
    // A tab with no mounted pane couldn't be saved: still unsaved, never closed.
    const stillDirty = paths.filter((path) => {
      const api = apisRef.current.get(path);
      return api ? api.isDirty() : hasUnsavedWork(path);
    });
    if (stillDirty.length > 0) {
      // A save failed or hit a conflict: stay open so the user sees why.
      setPending(null);
      setActive(stillDirty[0] ?? null);
      return;
    }
    setPending(null);
    if (p.type === "tab") {
      removeTab(p.path);
      return;
    }
    // A file edited while the prompt was up wasn't saved above: ask about it
    // rather than close over it.
    const now = collectDirty();
    if (now.length > 0) {
      setPending({ type: "editor", dirty: now });
      return;
    }
    closeNow();
  }, [pending, removeTab, closeNow, collectDirty, hasUnsavedWork]);

  // ── Watchers (external-change signal) ───────────────────────────────────
  const sourceRepo = useMemo(() => workspaceSourceRepo(ws), [ws]);
  const isRemoteRepo = sourceRepo?.kind === "remote";
  // Hook rules: cadence runs unconditionally with a stable sentinel locally.
  const cadence = useRemoteCadence(sourceRepo ?? ({ kind: "local", cwd: "" } as const));
  // The remote watcher is acquired before the nonce hook attaches its listener.
  useRemoteWorktreeWatch(
    isRemoteRepo && sourceRepo ? sourceRepo.machineId : null,
    isRemoteRepo && sourceRepo ? sourceRepo.handle : null,
    isRemoteRepo,
    isRemoteRepo ? cadence.watcherMs : undefined,
  );
  const remoteChangedNonce = useRemoteWatchNonce(
    isRemoteRepo && sourceRepo ? sourceRepo.machineId : null,
    isRemoteRepo && sourceRepo ? sourceRepo.handle : null,
  );
  const localChangedNonce = useViewerStore((s) => s.worktreeChangedPath[ws.id]?.nonce ?? 0);
  const changedNonce = isRemoteRepo ? remoteChangedNonce : localChangedNonce;
  useWorktreeWatch(ws.id, ws.cwd, !isRemoteRepo, teamKey);

  // ── Quick open's file list ──────────────────────────────────────────────
  const [fileList, setFileList] = useState<{
    paths: string[] | null;
    truncated: boolean;
    error: string | null;
    branch: string | null;
    nonce: number;
  }>({ paths: null, truncated: false, error: null, branch: null, nonce: -1 });
  // Only the newest request's answer lands: a slow older list never replaces it.
  const listRequestRef = useRef(0);
  const loadFileList = useCallback(() => {
    const request = ++listRequestRef.current;
    if (!backend) {
      setFileList((prev) => ({
        ...prev,
        error: "This worktree isn’t ready yet — try again in a moment.",
      }));
      return;
    }
    const nonce = changedNonce;
    backend
      .listFiles()
      .then((list) => {
        if (request !== listRequestRef.current) return;
        setFileList({
          paths: list.paths,
          truncated: list.truncated,
          error: null,
          branch: list.branch,
          nonce,
        });
      })
      .catch((e: unknown) => {
        if (request !== listRequestRef.current) return;
        setFileList((prev) => ({
          ...prev,
          error: describeFileError(e),
          nonce,
        }));
      });
  }, [backend, changedNonce]);
  // The branch shows in the status bar from the start; the list itself is
  // refreshed whenever the palette opens on a worktree that changed since.
  useEffect(() => {
    loadFileList();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- mount + backend change only
  }, [backend]);
  const openPalette = useCallback(
    (query: string) => {
      setPalette({ query });
      if (fileList.nonce !== changedNonce || fileList.error) loadFileList();
    },
    [fileList.nonce, fileList.error, changedNonce, loadFileList],
  );

  // ── Layout: tree column vs drawer ───────────────────────────────────────
  const rootRef = useRef<HTMLDivElement>(null);
  const [narrow, setNarrow] = useState(false);
  useEffect(() => {
    const el = rootRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(([entry]) => {
      if (entry) setNarrow(entry.contentRect.width < NARROW_PX);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  const [treePref, setTreePref] = useState<boolean | null>(null);
  const showTree = treePref ?? !narrow;
  const [treeRefresh, setTreeRefresh] = useState(0);

  const selectFromTree = useCallback(
    (file: WorktreeFileSelection) => {
      openFile({ rel: toRel(file.path), size: file.size });
      if (narrow) setTreePref(false);
    },
    [openFile, narrow, toRel],
  );

  // ── Pane plumbing (stable callbacks so memoized panes don't re-render) ──
  const onStatus = useCallback((path: string, status: PaneStatus) => {
    setStatuses((prev) => {
      const old = prev.get(path);
      if (old && shallowEqualStatus(old, status)) return prev;
      const next = new Map(prev);
      next.set(path, status);
      return next;
    });
  }, []);
  const registerApi = useCallback((rel: string, api: PaneApi | null) => {
    if (!api) {
      apisRef.current.delete(rel);
      return;
    }
    apisRef.current.set(rel, api);
    // The pane owns its draft from here (file-buffer-model).
    initialDraftsRef.current.delete(rel);
    const pendingJump = pendingRevealRef.current.get(rel);
    if (pendingJump) {
      pendingRevealRef.current.delete(rel);
      api.revealLine(pendingJump.line, pendingJump.column);
    }
  }, []);
  const onPanePersistDraft = useCallback((relPath: string, draft: FileDraft | null) => {
    const persist = persistRef.current;
    // A pane that unmounts while its tab stays open (the backend went away for a
    // moment) re-seeds from its draft when it comes back; without this the
    // remount would open clean from disk and clear the stored draft.
    if (draft) initialDraftsRef.current.set(relPath, { relPath, ...draft });
    else initialDraftsRef.current.delete(relPath);
    persist?.(
      relPath,
      draft ? { relPath, buffer: draft.buffer, lastSaved: draft.lastSaved } : null,
    );
  }, []);

  // ── Scratch overlay (local only) ────────────────────────────────────────
  const { files: scratchFiles, refresh: refreshScratch } = useScratchOverlayFiles(ws.cwd);
  const activeRel = active ? relativeToRoot(rootPath, toAbs(active)) : null;
  const activeScratch = activeRel === null ? undefined : scratchFiles.get(activeRel);
  const [scratchBusy, setScratchBusy] = useState(false);
  const chooseScratchFile = useCallback(
    async (logical: string, useTeam: boolean) => {
      setScratchBusy(true);
      try {
        await setScratchFileUseTeam(ws.cwd, logical, useTeam);
        refreshScratch();
        if (useTeam) {
          toast("Using the team's file here", {
            description: "Your copy is still in Private.",
            action: { label: "Undo", onClick: () => void chooseScratchFile(logical, false) },
          });
        }
      } catch (err) {
        toastFailure(useTeam ? "Couldn't switch to the team's file" : "Couldn't switch back", err);
      } finally {
        setScratchBusy(false);
      }
    },
    [ws.cwd, refreshScratch],
  );

  // ── Keyboard ────────────────────────────────────────────────────────────
  const activeStatus = active ? (statuses.get(active) ?? null) : null;
  const activeApi = () => (active ? apisRef.current.get(active) : undefined);
  // A retained editor stays mounted while its team is hidden: take focus again when it returns.
  useFocusWhenShown(teamKey, active !== null && active !== undefined, () => activeApi()?.focus());
  const effectiveWrap = activeStatus?.lineWrap ?? wrapOverride ?? false;
  const toggleWrap = () => setWrapOverride(!effectiveWrap);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const mod = e.metaKey || e.ctrlKey;
    const target = e.target as HTMLElement | null;
    const inCode = !!target?.closest?.(".cm-editor");
    const claim = () => {
      e.preventDefault();
      e.stopPropagation();
    };
    if (e.key === "Escape") {
      // Escape belongs to this editor: the overlay-level Escape underneath
      // must never also fire.
      e.stopPropagation();
      if (pending) {
        setPending(null);
        requestAnimationFrame(() => activeApi()?.focus());
        return;
      }
      const api = activeApi();
      if (api?.isReconciling()) {
        api.backOutOfReconcile();
        return;
      }
      // Inside the code (or after CodeMirror used the key — closing find,
      // collapsing cursors) Escape never closes the whole editor.
      if (e.defaultPrevented || inCode || target?.closest?.(".cm-panels")) return;
      requestCloseEditor();
      return;
    }
    if (mod && !e.shiftKey && e.code === "KeyF") {
      // Find (and ⌥ replace) belongs to the editor: inside the code CodeMirror
      // has already opened its panel; from the chrome, open it on the active
      // file. Either way the app's document find and terminal find must not
      // also open underneath.
      claim();
      if (!inCode) activeApi()?.openSearch();
      return;
    }
    if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "p") {
      claim();
      openPalette("");
      return;
    }
    if (e.ctrlKey && !e.metaKey && !e.altKey && e.key.toLowerCase() === "g") {
      claim();
      openPalette(":");
      return;
    }
    if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "s" && !inCode) {
      // ⌘S from the chrome (tab strip, status bar). Inside the code the
      // editor's own keymap saves.
      claim();
      void activeApi()?.save();
      return;
    }
    if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "w") {
      claim();
      // With no file open, ⌘W closes the editor itself (the app hands ⌘W to the
      // editor whenever focus is inside it, so it must never be swallowed).
      if (active) requestCloseTab(active);
      else requestCloseEditor();
      return;
    }
    if (e.ctrlKey && e.key === "Tab") {
      claim();
      setActive(cycleTab(tabs, active, e.shiftKey ? -1 : 1));
      return;
    }
    // ⌥Z by physical key, so it works on any layout — but inside the code a
    // layout whose ⌥Z types a letter (ż, ž…) keeps it. US Ω is the one given up,
    // as in VS Code.
    const typesLetter =
      inCode && [...e.key].length === 1 && /\p{L}/u.test(e.key) && !/^[zZΩ]$/.test(e.key);
    if (e.altKey && !mod && e.code === "KeyZ" && !typesLetter) {
      claim();
      toggleWrap();
      return;
    }
    // A chord the code editor itself used (⌘] indent, ⌘D next match, ⇧⌘K
    // delete line, ⌥⌘↑ add cursor…) is the editor's: the app's shortcuts
    // (tab cycling, split, clear terminal, pane focus) must not also fire.
    if (inCode && e.defaultPrevented) e.stopPropagation();
  };

  // ── Render ──────────────────────────────────────────────────────────────
  const tabStates = useMemo(() => {
    const m = new Map<string, TabState>();
    for (const t of tabs) {
      const s = statuses.get(t);
      m.set(t, { dirty: s?.dirty ?? false, stale: s?.stale ?? false });
    }
    return m;
  }, [tabs, statuses]);

  // The unsaved-changes prompt takes focus so Enter saves and Escape (handled
  // above) goes back to the code.
  const confirmSaveRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (pending) confirmSaveRef.current?.focus();
  }, [pending]);

  // Tree reveal: follows the active file, and the header button asks again
  // (after the user collapsed a folder, or with the tree hidden).
  const [revealNonce, setRevealNonce] = useState(0);
  const treeReveal = useMemo(
    () => (active ? { path: toAbs(active), nonce: revealNonce } : null),
    [active, revealNonce, toAbs],
  );
  const revealActiveInTree = () => {
    setTreePref(true);
    setRevealNonce((n) => n + 1);
  };

  const branch = fileList.branch ?? (remote ? (ws.location.branch ?? null) : null);
  const editableActive = activeStatus?.kind === "editable";
  const pendingName = pending?.type === "tab" ? basename(pending.path) : null;

  return (
    <OverlayShell
      testId={`source-editor-inset-${ws.id}`}
      ariaLabel={`Code editor — ${ws.storyId ?? "worktree"}`}
      onKeyDown={onKeyDown}
      // An editor is read for long stretches: fully opaque, not the
      // see-through glance overlays share.
      className="opacity-100"
    >
      <div
        ref={rootRef}
        data-worktree-editor=""
        className="relative flex min-h-0 min-w-0 flex-1 flex-col bg-[var(--background)]"
      >
        <div className="relative flex min-h-0 flex-1">
          {/* Files: a column when there's room, a drawer over the editor when not. */}
          {showTree && (
            <aside
              aria-label="Files"
              className={cn(
                "flex min-h-0 flex-col border-r border-[var(--border)] bg-[var(--card)]",
                narrow
                  ? "absolute inset-y-0 left-0 z-10 w-[min(18rem,85%)] shadow-[8px_0_24px_-12px_rgb(0_0_0/0.4)]"
                  : "w-60 shrink-0",
              )}
            >
              <div className="flex h-9 shrink-0 items-center gap-1 border-b border-[var(--border)] pl-3 pr-1.5">
                <span className="text-xs font-semibold text-foreground">Files</span>
                <span
                  className="min-w-0 flex-1 truncate text-[0.7rem] text-muted-foreground"
                  title={rootReady && rootPath === "" ? "/" : rootPath}
                >
                  {rootLabel}
                </span>
                <ChromeIconButton
                  testId={`source-editor-reveal-${ws.id}`}
                  ariaLabel="Reveal the open file"
                  tooltip="Reveal the open file"
                  tooltipSide="bottom"
                  disabled={!active}
                  onClick={revealActiveInTree}
                >
                  <LocateFixed size={12} />
                </ChromeIconButton>
                <ChromeIconButton
                  ariaLabel="Refresh files"
                  tooltip="Refresh"
                  tooltipSide="bottom"
                  onClick={() => setTreeRefresh((n) => n + 1)}
                >
                  <RefreshCw size={12} />
                </ChromeIconButton>
                <ChromeIconButton
                  ariaLabel="Hide files"
                  tooltip="Hide files"
                  tooltipSide="bottom"
                  onClick={() => setTreePref(false)}
                >
                  <PanelLeftClose size={13} />
                </ChromeIconButton>
              </div>
              <div className="min-h-0 flex-1 overflow-y-auto px-1.5 py-1.5">
                {backend ? (
                  <WorktreeFileTree
                    rootPath={rootPath}
                    ready={rootReady}
                    selectedPath={active ? toAbs(active) : null}
                    onSelect={selectFromTree}
                    listDirectory={backend.listDirectory}
                    refreshToken={treeRefresh}
                    reveal={treeReveal}
                    scratchFiles={scratchFiles}
                    wrapScratchRow={(file, row) => (
                      <ScratchItemContextMenu
                        item={{
                          orgSlug: file.orgSlug,
                          logical: file.logical,
                          where: file.origin === "allOrgs" ? "allOrgs" : "org",
                          inertKind: file.inertKind,
                          defaultTeam: file.team,
                        }}
                        onDone={refreshScratch}
                      >
                        {row}
                      </ScratchItemContextMenu>
                    )}
                  />
                ) : (
                  <p className="px-1 py-1 text-xs text-muted-foreground">
                    The machine is still preparing this worktree.
                  </p>
                )}
              </div>
            </aside>
          )}
          {narrow && showTree && (
            <div
              aria-hidden
              className="absolute inset-0 z-[5] bg-black/20"
              onMouseDown={() => setTreePref(false)}
            />
          )}

          <section className="relative flex min-w-0 flex-1 flex-col">
            <EditorTabStrip
              tabs={tabs}
              active={active}
              states={tabStates}
              rootPath={rootPath}
              onActivate={setActive}
              onClose={requestCloseTab}
              leading={
                !showTree && (
                  <ChromeIconButton
                    ariaLabel="Show files"
                    tooltip="Show files"
                    tooltipSide="bottom"
                    onClick={() => setTreePref(true)}
                  >
                    <PanelLeft size={13} />
                  </ChromeIconButton>
                )
              }
              trailing={
                <>
                  <ChromeIconButton
                    testId={`source-editor-quick-open-${ws.id}`}
                    ariaLabel="Quick open"
                    tooltip={
                      <span className="flex items-center gap-1.5">
                        Quick open <Kbd>⌘P</Kbd>
                      </span>
                    }
                    tooltipSide="bottom"
                    onClick={() => openPalette("")}
                  >
                    <Search size={13} />
                  </ChromeIconButton>
                  {editableActive && !activeStatus?.reconciling && (
                    <ChromeIconButton
                      testId={`source-editor-save-${ws.id}`}
                      ariaLabel="Save"
                      tooltip={
                        <span className="flex items-center gap-1.5">
                          Save <Kbd>⌘S</Kbd>
                        </span>
                      }
                      tooltipSide="bottom"
                      disabled={!activeStatus?.dirty && !activeStatus?.saveError}
                      onClick={() => void activeApi()?.save()}
                    >
                      <Save size={13} />
                    </ChromeIconButton>
                  )}
                  {!remote && <OpenInExternalEditorButton cwd={ws.cwd} wsId={ws.id} />}
                  <ChromeIconButton
                    testId={`source-editor-back-${ws.id}`}
                    ariaLabel="Close editor"
                    tooltip={
                      <span className="flex items-center gap-1.5">
                        Close <Kbd>Esc</Kbd>
                      </span>
                    }
                    tooltipSide="bottom"
                    onClick={requestCloseEditor}
                  >
                    <X size={13} />
                  </ChromeIconButton>
                </>
              }
            />

            {pending && (
              <div
                role="alertdialog"
                aria-label="Discard unsaved changes?"
                data-testid="worktree-discard-confirm"
                className="flex shrink-0 flex-wrap items-center gap-2 border-b border-[var(--border)] bg-[color-mix(in_srgb,var(--cs-status-modified)_10%,var(--background))] px-3 py-1.5 text-xs"
              >
                <span className="min-w-0 flex-1 truncate">
                  {pending.type === "tab"
                    ? `${pendingName} has unsaved changes.`
                    : pending.dirty.length === 1
                      ? `${basename(pending.dirty[0] ?? "")} has unsaved changes.`
                      : `${pending.dirty.length} files have unsaved changes.`}
                </span>
                <button
                  ref={confirmSaveRef}
                  type="button"
                  data-testid="worktree-discard-confirm-save"
                  onClick={() => void saveThenFinish()}
                  className="shrink-0 rounded-md border border-[var(--border)] bg-[var(--background)] px-2 py-0.5 font-medium hover:bg-muted focus-visible:outline-2 focus-visible:[outline-color:var(--ring)]"
                >
                  {pending.type === "editor" && pending.dirty.length > 1 ? "Save all" : "Save"}
                </button>
                <button
                  type="button"
                  data-testid="worktree-discard-confirm-yes"
                  onClick={confirmDiscard}
                  className="shrink-0 rounded-md border border-destructive/40 px-2 py-0.5 font-medium text-destructive hover:bg-destructive/10 focus-visible:outline-2 focus-visible:[outline-color:var(--ring)]"
                >
                  Discard
                </button>
                <button
                  type="button"
                  data-testid="worktree-discard-confirm-no"
                  onClick={() => setPending(null)}
                  className="shrink-0 rounded-md px-2 py-0.5 font-medium text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:[outline-color:var(--ring)]"
                >
                  Keep editing
                </button>
              </div>
            )}

            {active && activeStatus?.kind !== "idle" && (
              <div className="flex shrink-0 items-center">
                <div className="min-w-0 flex-1">
                  <EditorBreadcrumbs
                    relPath={active}
                    absPath={toAbs(active)}
                    scope={activeStatus?.cursor?.scope ?? null}
                  />
                </div>
                {activeScratch && (
                  <div className="flex h-7 items-center border-b border-[var(--border)] pr-2">
                    <ScratchHeaderBadge
                      file={activeScratch}
                      busy={scratchBusy}
                      onUseTeam={(useTeam) =>
                        void chooseScratchFile(activeScratch.logical, useTeam)
                      }
                    />
                  </div>
                )}
              </div>
            )}

            <div className="relative flex min-h-0 flex-1 flex-col">
              {tabs.length === 0 && <EmptyWorkbench onQuickOpen={() => openPalette("")} />}
              {tabs.length > 0 && !(backend && rootReady) && (
                <EditorStateMessage
                  title={
                    machineOffline
                      ? `${machineLabel ?? "The machine"} is ${machineOffline}`
                      : "Waiting for the worktree…"
                  }
                  detail="Your open files and unsaved changes come back when it’s ready."
                  testId="worktree-waiting"
                />
              )}
              {/* Panes wait for the root: before it is known a path can't reach
                  disk. Keyed by repo-relative path, so a root change re-targets
                  them in place (file-buffer-model). */}
              {backend &&
                rootReady &&
                tabs.map((rel) => (
                  <WorktreeFilePane
                    key={rel}
                    id={rel}
                    file={fileFor(rel)}
                    active={rel === active}
                    backend={backend}
                    changedNonce={changedNonce}
                    restore={draftFor(initialDraftsRef.current, rel)}
                    onPersistDraft={onPanePersistDraft}
                    onStatus={onStatus}
                    registerApi={registerApi}
                    wrapOverride={wrapOverride}
                  />
                ))}
              {palette && (
                <EditorQuickOpen
                  // ⌃G over ⌘P (or the reverse) switches mode: a fresh palette.
                  key={palette.query === ":" ? "line" : "file"}
                  initialQuery={palette.query}
                  files={fileList.paths}
                  filesError={fileList.error}
                  truncated={fileList.truncated}
                  recent={tabs}
                  lineCount={editableActive ? (activeStatus?.cursor?.totalLines ?? null) : null}
                  onOpenFile={(rel, line, column) =>
                    openFile({ rel, size: null }, { line, column })
                  }
                  onGoToLine={(line, column) => activeApi()?.revealLine(line, column ?? 1)}
                  onClose={() => {
                    setPalette(null);
                    requestAnimationFrame(() => activeApi()?.focus());
                  }}
                />
              )}
            </div>
          </section>
        </div>

        <EditorStatusBar
          location={backend?.location ?? (remote ? "remote" : "local")}
          machineLabel={machineLabel}
          machineOffline={machineOffline}
          branch={branch}
          status={activeStatus}
          wrap={effectiveWrap}
          onToggleWrap={toggleWrap}
          onGoToLine={() => openPalette(":")}
        />
      </div>
    </OverlayShell>
  );
}

/** The pending `openInWorktreeEditor` request for this workspace, if any. */
function useViewerRevealRequest(workspaceId: string) {
  return useWorktreeEditorRevealStore((s) => s.requests[workspaceId] ?? null);
}

/** The stored draft for a tab (the stored object itself, so its identity is
 *  stable across renders; a pane reads it only at mount). */
function draftFor(
  drafts: ReadonlyMap<string, WorktreeEditorDraft>,
  relPath: string,
): FileDraft | null {
  return drafts.get(relPath) ?? null;
}

function shallowEqualStatus(a: PaneStatus, b: PaneStatus): boolean {
  return (
    a.kind === b.kind &&
    a.dirty === b.dirty &&
    a.saving === b.saving &&
    a.saveError === b.saveError &&
    a.stale === b.stale &&
    a.reconciling === b.reconciling &&
    a.language === b.language &&
    a.lineEnding === b.lineEnding &&
    a.indentation === b.indentation &&
    a.cursor === b.cursor &&
    a.changes === b.changes &&
    a.baselineKnown === b.baselineKnown &&
    a.lineWrap === b.lineWrap
  );
}

/** No file open: say what the editor does and how to start, rather than an
 *  empty pane. */
function EmptyWorkbench({ onQuickOpen }: { onQuickOpen: () => void }) {
  return (
    <EditorStateMessage title="No file open" testId="worktree-empty">
      <p className="max-w-[36ch] text-xs text-muted-foreground">
        Pick a file from the list, or search the whole worktree.
      </p>
      <div className="mt-3 grid grid-cols-[auto_auto] items-center gap-x-4 gap-y-1.5 text-xs text-muted-foreground">
        <button
          type="button"
          onClick={onQuickOpen}
          className="justify-self-end rounded-md text-foreground underline-offset-4 hover:underline focus-visible:outline-2 focus-visible:[outline-color:var(--ring)]"
        >
          Quick open
        </button>
        <Kbd>⌘P</Kbd>
        <span className="justify-self-end">Go to line</span>
        <Kbd>⌃G</Kbd>
        <span className="justify-self-end">Find and replace</span>
        <span className="flex gap-1">
          <Kbd>⌘F</Kbd>
          <Kbd>⌥⌘F</Kbd>
        </span>
        <span className="justify-self-end">Next tab</span>
        <Kbd>⌃Tab</Kbd>
      </div>
    </EditorStateMessage>
  );
}

/**
 * The tile-level mount that makes the worktree editor a first-class surface,
 * independent of the source diff. A CHEAP GATE: it subscribes only to the open
 * flag + seed and renders nothing (no watcher, no reads) until the editor is
 * open over the focused, ready story tile. Mounted beside `SourceDiffOverlay` /
 * `StoryHud` in `workspace-terminal-panels.tsx`.
 *
 * Session memory: the open tabs (`worktreeEditorTabs`) and the active file
 * (`worktreeEditorFile` — also what the diff's "Open in editor" sets) survive a
 * close/reopen, and each tab's unsaved draft survives an unfocus.
 */
export function WorktreeEditorSurface({
  workspace: ws,
  teamKey,
  activeTabSessionId,
  isFocused,
}: {
  workspace: Workspace;
  /** Owning tile's teamKey, threaded in so close() can pass the triple the
   *  store action requires. */
  teamKey: string;
  /** The sessionId of the owning tile's active tab (the open flag is per tab). */
  activeTabSessionId: string | undefined;
  isFocused: boolean;
}) {
  const open = useViewerStore((s) =>
    activeTabSessionId ? (s.worktreeEditorOpenBySession[activeTabSessionId] ?? false) : false,
  );
  const seedRelPath = useViewerStore((s) => s.worktreeEditorFile[ws.id] ?? null);
  const revealLine = useViewerStore((s) => s.worktreeEditorRevealLine[ws.id]);
  const setRevealLine = useViewerStore((s) => s.setWorktreeEditorRevealLine);
  const tabs = useViewerStore((s) => s.worktreeEditorTabs[ws.id]);
  const drafts = useViewerStore((s) => s.worktreeEditorDraft[ws.id]);
  const toggleOpen = useViewerStore((s) => s.toggleWorktreeEditorOpen);
  const setEditorFile = useViewerStore((s) => s.setWorktreeEditorFile);
  const setEditorTabs = useViewerStore((s) => s.setWorktreeEditorTabs);
  const setDraft = useViewerStore((s) => s.setWorktreeEditorDraft);

  // A draft for a file no longer open is stale — drop it so it never re-seeds a
  // phantom buffer if that file is reopened later.
  useEffect(() => {
    if (!drafts) return;
    const open = new Set([...(tabs ?? []), ...(seedRelPath ? [seedRelPath] : [])]);
    for (const rel of Object.keys(drafts)) {
      if (!open.has(rel)) setDraft(ws.id, rel, null);
    }
  }, [drafts, tabs, seedRelPath, setDraft, ws.id]);

  // SW-VE-12: the editor mounts on every folder-rooted kind. The gate was
  // `caps.sourceDiffToggle` (story-only); now `caps.sourceCapable` — TRUE on
  // every live kind — so the editor renders against the tab's cwd whether
  // or not it is a git working tree.
  const mounted =
    open && isFocused && ws.status === "ready" && getKindCapabilities(ws.kind).sourceCapable;
  // SW-VE-14: `useSessionCwdRevision` subscribes to the per-session OSC 7
  // event stream (`pty-osc7`), so `useSessionCwd` re-reads on every real
  // `cd` and the editor's file tree retargets without a session restart.
  // For local tabs the non-null reading overrides `ws.cwd`; remote tabs
  // keep `location.cwd` since the local resolver can't address a remote
  // shell (SW-VE-15 wires the remote path).
  //
  // TODO (deferred from SW-VE-12 R3-M5): `localChangedNonce` above still
  // keys `worktreeChangedPath[ws.id]` on raw ws.id. Now that liveCwd
  // mutates mid-session the editor + overlay watchers can observe
  // different dirs under one key → crossed events. Revisit with a
  // composite id `${ws.id}|${effectiveWs.cwd}` if a seam test or
  // real-app observation flags a cross (overlay + editor share the
  // same effectiveCwd per tab today, so not surfaced in practice).
  const cwdRevision = useSessionCwdRevision(activeTabSessionId);
  const liveCwd = useSessionCwd(activeTabSessionId, cwdRevision);
  const effectiveWs = useMemo<Workspace>(() => {
    if (!liveCwd || isRemoteWorkspace(ws)) return ws;
    if (liveCwd === ws.cwd) return ws;
    return { ...ws, cwd: liveCwd };
  }, [ws, liveCwd]);
  // The armed line is one-shot: the inset captures it on mount, then it is
  // cleared so a later reopen resumes without jumping back.
  useEffect(() => {
    if (mounted && revealLine) setRevealLine(ws.id, null);
  }, [mounted, revealLine, setRevealLine, ws.id]);

  if (!mounted) return null;

  return (
    <SourceEditorInset
      workspace={effectiveWs}
      teamKey={teamKey}
      seedRelPath={seedRelPath}
      seedLine={revealLine && revealLine.path === seedRelPath ? revealLine.line : null}
      initialTabs={tabs}
      onSelectRelPath={(rel) => setEditorFile(ws.id, rel)}
      onTabsChange={(rels) => setEditorTabs(ws.id, rels)}
      restoreDrafts={drafts}
      onPersistDraft={(rel, next) => setDraft(ws.id, rel, next)}
      onClose={() => {
        if (!activeTabSessionId) return;
        toggleOpen({ sessionId: activeTabSessionId, workspaceId: ws.id, teamKey, open: false });
      }}
    />
  );
}

export type { WorktreeEditorBackend };
