// SPDX-License-Identifier: MIT
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import { toast } from "sonner";
import {
  ArrowDownToLine,
  Binary,
  Braces,
  Check,
  ChevronDown,
  ChevronUp,
  Code,
  Columns2,
  Copy,
  FileDiff,
  GitBranch,
  FoldVertical,
  ListTree,
  Minimize,
  Pilcrow,
  RotateCw,
  Rows3,
  UnfoldVertical,
  WrapText,
  X,
} from "lucide-react";

import { cn } from "@/lib/utils";
import { basename } from "@/shared/lib/basename";
import { useTrailingDebounce } from "@/shared/lib/use-trailing-debounce";
import { useRecentActivity } from "@/shared/lib/use-recent-activity";
import type { SourceDiffMode, Workspace } from "@/shared/stores/viewer-store";
import {
  useViewerStore,
  resolveSourceDiffOption,
  type SourceDiffViewOption,
} from "@/shared/stores/viewer-store";
import { useTabSourceRepo } from "@/features/viewer/hooks/use-tab-source-repo";
import { useSessionCwd } from "../hooks/use-session-cwd";
import { useSessionCwdRevision } from "../hooks/use-session-cwd-revision";
import { InfoTip } from "@/components/ui/tooltip";
import { RedlineProse } from "@/features/git-sync/components/redline-prose";
import {
  CodeDiffReview,
  detectLanguageId,
  isStructuralLanguage,
  type ChunkPosition,
  type DiffNavigator,
} from "@/shared/components/code-editor";
import { STATUS_LABEL, StatusChip } from "@/features/git-sync/components/file-status-chip";
import type { PublishFile } from "@/features/git-sync/lib/types";
import { routeFileToContext, unrouteFileFromContext } from "@/tauri/commands";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { useMachine } from "@/features/machines/stores/machines-store";

import {
  useSourceChangeset,
  useSourceCommitChangeset,
  useSourceCommitRedline,
  useSourceDiffBase,
  useSourceFileRedline,
} from "../lib/use-source-diff";
import { comparisonLabel, effectiveBaseRef } from "../lib/source-diff-base";
import { useRepoWorktreeWatch } from "../lib/use-repo-worktree-watch";
import { useRemoteWatchNonce } from "../lib/use-remote-worktree-watch";
import { workspaceSourceRepo, isRemoteWorkspace } from "@/shared/lib/workspace-location";
import { useFocusWhenShown, useScopeActive } from "@/shared/lib/warm-scopes";
import { isRoutable, routeStateFromOutcome, type RouteState } from "../lib/route-to-context";
import {
  buildFileTree,
  diffTotals,
  filterFiles,
  flatRows,
  isProseDiffPath,
  navigationOrder,
  prefersTree,
  splitPath,
  stepFile,
  treeRows,
} from "../lib/source-diff-model";
import { LineStat, SourceDiffRail, type RailRouting } from "./source-diff-rail";
import { CommitGraphRail } from "./commit-graph-rail";
import { FileWarningPlaceholder } from "./file-warning-placeholder";
import { OpenInExternalEditorButton } from "./open-in-external-editor-button";
import { setSourceDiffReturnFocus, takeSourceDiffReturnFocus } from "./source-diff-return-focus";
import {
  OverlayShell,
  OverlayHeader,
  OverlayCenteredState,
  ChromeIconButton,
  useOverlayEscape,
} from "./overlay-shell";

/**
 * SLW-DV-6 (AC-8): combined before+after character budget above which the code
 * diff is gated behind an explicit "show anyway" (a minified bundle can be many
 * megabytes on one line, and the diff runs on the renderer thread). ~1.5M chars
 * comfortably covers normal source files while catching the pathological ones.
 */
const LARGE_DIFF_CHARS = 1_500_000;

// SLW-DV-5 (AC-1): coalesce a burst of watcher events onto one re-diff. The
// max-wait force-flushes at least this often so a SUSTAINED sub-debounce edit
// stream (a large file streamed out) still refreshes mid-burst instead of
// freezing until the agent pauses. (AC-5): how long after the last change the
// "editing…" affordance lingers.
const LIVE_REDIFF_DEBOUNCE_MS = 100;
const LIVE_REDIFF_MAX_WAIT_MS = 500;
const ACTIVITY_IDLE_MS = 1200;
/** No folded folders: a filter shows every match. */
const NO_FOLDS: ReadonlySet<string> = new Set();

interface SourceDiffOverlayProps {
  workspace: Workspace;
  /** The owning tile's teamKey, threaded in so the overlay's open/close actions
   *  can pass the full `{sessionId, workspaceId, teamKey}` triple the store now
   *  requires for the per-tab visibility slice. */
  teamKey: string;
  /** The sessionId of this tile's ACTIVE tab, or `undefined` when the tile has
   *  no live tab yet. The overlay's shown state keys on this (per-tab); on
   *  `undefined` the overlay never paints. */
  activeTabSessionId: string | undefined;
  /** Is THIS the focused tile — WD-2 renders ONLY on the focused tile (AC-1), a
   *  deliberate divergence from StoryHud (which renders per-tile). */
  isFocused?: boolean;
  /** A tile is zoomed — while zoomed, Escape belongs to zoom-restore, so the
   *  overlay defers (one Escape must not both un-zoom and close). Defaults false. */
  zoomActive?: boolean;
}

/** SW-TP CAP-3: read one resolved source-diff view option (inline / wordWrap /
 *  expandAll / autoFollow / ignoreWhitespace) for an active tab. Keyed per
 *  `sessionId` so sibling tabs on the same workspace carry independent options.
 *  The selector returns a single resolved boolean, so flipping one option never
 *  re-renders a reader of another (the equality check absorbs unrelated store
 *  writes — including the `settings` read that only `inline` actually needs).
 *  An empty / undefined session reads the default. */
function useSourceDiffOption(sessionId: string | undefined, key: SourceDiffViewOption): boolean {
  return useViewerStore((s) =>
    resolveSourceDiffOption(
      sessionId ? s.sourceDiffViewOptions[sessionId] : undefined,
      key,
      s.settings,
    ),
  );
}

/** The path of the rail file row a key event came from, if any. */
function focusedRowPath(target: HTMLElement | null): string | null {
  const row = target?.closest?.("[data-source-diff-row]");
  return row?.closest<HTMLElement>("[data-path]")?.dataset.path ?? null;
}

/** Move keyboard focus to the rail row for `path`, so stepping from a focused
 *  row keeps going from the row just selected. */
function focusRow(scope: HTMLElement | null, path: string): void {
  const li = [...(scope?.querySelectorAll<HTMLElement>("li[data-path]") ?? [])].find(
    (el) => el.dataset.path === path,
  );
  li?.querySelector<HTMLElement>("[data-source-diff-row]")?.focus({ preventScroll: true });
}

/** A counter that steps whenever either of two independent nonces changes
 *  within one `scope`. A new scope (another workspace) re-bases without a
 *  step: its nonces are another worktree's, not an edit. */
function useCombinedNonce(a: number, b: number, scope: string): number {
  const [state, setState] = useState({ a, b, scope, n: 0 });
  if (state.scope !== scope) {
    setState({ a, b, scope, n: state.n });
    return state.n;
  }
  if (state.a !== a || state.b !== b) {
    const next = { a, b, scope, n: state.n + 1 };
    setState(next);
    return next.n;
  }
  return state.n;
}

/**
 * The per-tab source diff: a review desk floated over the FOCUSED terminal tile
 * for a story worktree, on This Mac or on a machine. The left rail scopes the
 * comparison (whole branch vs uncommitted), filters, and lists the changed files
 * as a tree or flat list; the right pane reads one file's diff with a change
 * navigator and the view controls. Everything is keyboard-drivable once the
 * tool has focus (j/k files, n/p changes, / filter, s split, w wrap, b basis,
 * t tree, c copy path, e open in the code editor, r refresh) — the handler is
 * on the tool's own element, so terminal keys are never stolen.
 *
 * Review-only: the one in-place action is the per-file "→ context" route on a
 * local worktree (no commit, publish or sync controls — those planes stay
 * separate).
 */
export function SourceDiffOverlay({
  workspace: ws,
  teamKey,
  activeTabSessionId,
  isFocused = false,
  zoomActive = false,
}: SourceDiffOverlayProps) {
  const sessionKey = activeTabSessionId ?? "";
  // SW-VE-14: the terminal's live cwd. `useSessionCwdRevision` subscribes to
  // the per-session OSC 7 event stream (`pty-osc7`), deduped at the PTY
  // batcher so the counter only bumps on real `cd`s. Threaded as the
  // `revision` arg of `useSessionCwd` so the hook re-fetches on every bump
  // and `liveCwd` is the single source of truth for the local-tab fetch
  // keys below. Remote tabs ignore the reading — the local probe can't
  // address a machine shell.
  //
  // TODO (deferred from SW-VE-12 R3-M5): the editor's and overlay's
  // worktree watchers both key `worktreeChangedPath[ws.id]`. Now that
  // liveCwd mutates mid-session the two can observe different dirs under
  // one key → crossed events. A follow-up may need a composite id
  // `${ws.id}|${effectiveCwd}` for the watcher-key join. Not surfaced in
  // practice yet (overlay + editor share the same effectiveCwd per tab);
  // revisit if a seam test or real-app observation flags a cross.
  const cwdRevision = useSessionCwdRevision(activeTabSessionId);
  const liveCwd = useSessionCwd(activeTabSessionId, cwdRevision);
  const shown = useViewerStore((s) =>
    activeTabSessionId ? (s.sourceDiffShownBySession[activeTabSessionId] ?? false) : false,
  );
  const selectedFile = useViewerStore((s) =>
    activeTabSessionId ? (s.sourceDiffSelectedFile[activeTabSessionId] ?? null) : null,
  );
  const inline = useSourceDiffOption(activeTabSessionId, "inline");
  const wordWrap = useSourceDiffOption(activeTabSessionId, "wordWrap");
  const expandAll = useSourceDiffOption(activeTabSessionId, "expandAll");
  const autoFollow = useSourceDiffOption(activeTabSessionId, "autoFollow");
  // Read raw: unset means "let the change set's shape decide" (prefersTree).
  const fileTreePref = useViewerStore((s) =>
    activeTabSessionId ? s.sourceDiffViewOptions[activeTabSessionId]?.fileTree : undefined,
  );
  // The worktree editor takes the whole tile when open; this tool yields to it
  // (and it owns Escape) so the two never stack.
  const editorOpen = useViewerStore((s) =>
    activeTabSessionId ? (s.worktreeEditorOpenBySession[activeTabSessionId] ?? false) : false,
  );
  const diffMode = useViewerStore((s) =>
    activeTabSessionId ? (s.sourceDiffMode[activeTabSessionId] ?? "line") : "line",
  );
  const toggleShown = useViewerStore((s) => s.toggleSourceDiffShown);
  const setSelectedFile = useViewerStore((s) => s.setSourceDiffSelectedFile);
  const toggleViewOption = useViewerStore((s) => s.toggleSourceDiffOption);
  const setDiffMode = useViewerStore((s) => s.setSourceDiffMode);
  const toggleEditorOpen = useViewerStore((s) => s.toggleWorktreeEditorOpen);
  const setEditorFile = useViewerStore((s) => s.setWorktreeEditorFile);
  const setEditorRevealLine = useViewerStore((s) => s.setWorktreeEditorRevealLine);
  const editorHotkey = formatKeystrokeDisplay(useEffectiveShortcut("viewer.toggle-code-editor"));
  const historyMode = useViewerStore((s) =>
    activeTabSessionId ? s.sourceDiffHistoryMode[activeTabSessionId] : undefined,
  );
  const selectedSha = useViewerStore((s) =>
    activeTabSessionId ? (s.sourceDiffSelectedSha[activeTabSessionId] ?? null) : null,
  );
  const setHistoryMode = useViewerStore((s) => s.setSourceDiffHistoryMode);
  const baseMode = useViewerStore((s) =>
    activeTabSessionId ? (s.sourceDiffBaseMode[activeTabSessionId] ?? "branchBase") : "branchBase",
  );
  const setBaseMode = useViewerStore((s) => s.setSourceDiffBaseMode);

  const remoteWorkspace = isRemoteWorkspace(ws);
  // History reads local git only: a remembered History choice never applies to
  // a machine's workspace (it has no way back to Working there).
  const inHistory = historyMode === true && !remoteWorkspace;
  // SW-VE-14: local-tab fetch keys read from `effectiveCwd`, which tracks
  // `liveCwd` on every OSC 7 bump — the change-set + file redline retarget
  // automatically on a `cd`. Remote tabs stay anchored to their machine-
  // side handle (the local cwd probe can't speak to a remote shell).
  const effectiveCwd = remoteWorkspace ? ws.cwd : (liveCwd ?? ws.cwd);
  const repo = remoteWorkspace
    ? workspaceSourceRepo(ws)
    : workspaceSourceRepo({ ...ws, cwd: effectiveCwd });
  // The machine's display name once the machines list has loaded; its id
  // meanwhile, so the rail never names nothing.
  const machine = useMachine(remoteWorkspace ? ws.location.machineId : "");
  const machineLabel = remoteWorkspace ? machine?.label || ws.location.machineId : null;
  // Live refresh: a local worktree bumps the store's per-workspace nonce from
  // the OS watcher; a remote one ticks through the machine's polling watcher.
  // Either drives the open redline's re-fetch (the change list also refreshes
  // through the seam's own tick).
  const localNonce = useViewerStore((s) => s.worktreeChangedPath[ws.id]?.nonce ?? 0);
  const resumeNonce = useViewerStore((s) => s.worktreeChangedPath[ws.id]?.resumed ?? 0);
  const remoteNonce = useRemoteWatchNonce(
    repo?.kind === "remote" ? repo.machineId : null,
    repo?.kind === "remote" ? repo.handle : null,
  );
  // One counter that moves whenever either source does (a plain sum could stand
  // still when one rises as the other resets).
  const worktreeNonce = useCombinedNonce(
    localNonce,
    remoteNonce,
    `${ws.id}|${repo?.kind === "remote" ? `${repo.machineId}|${repo.handle}` : ""}`,
  );
  // Coalesce an edit burst onto one re-diff (with a max wait so a sustained
  // stream still refreshes); the "editing…" pulse runs off the raw nonce.
  const liveNonce = useTrailingDebounce(
    worktreeNonce,
    LIVE_REDIFF_DEBOUNCE_MS,
    LIVE_REDIFF_MAX_WAIT_MS,
  );
  const isEditing = useRecentActivity(worktreeNonce, ACTIVITY_IDLE_MS);

  // Optimistic per-file route state, local to this overlay; reset on a
  // workspace switch so a new tile starts clean.
  const [routeStates, setRouteStates] = useState<Map<string, RouteState>>(new Map());
  // Route state belongs to the worktree (every tab of a tile shares it), so only
  // a workspace switch clears it; an op still in flight then writes nothing.
  // A generation, not the id: A → B → A must still drop an op begun on the
  // first A.
  const routeScopeRef = useRef(0);
  // The latest route/un-route per file: an older op settling late must not
  // overwrite a newer one's state.
  const routeSeqRef = useRef(new Map<string, number>());
  useEffect(() => {
    routeScopeRef.current += 1;
  }, [ws.id]);
  const [seenRouteWs, setSeenRouteWs] = useState(ws.id);
  if (seenRouteWs !== ws.id) {
    setSeenRouteWs(ws.id);
    setRouteStates(new Map());
  }

  // Rail-local view state: the filter and folded folders.
  const [query, setQuery] = useState("");
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());
  const filterRef = useRef<HTMLInputElement | null>(null);
  const navRef = useRef<DiffNavigator | null>(null);
  const [chunkPos, setChunkPos] = useState<ChunkPosition | null>(null);
  const [copied, setCopied] = useState(false);
  const [keysOpen, setKeysOpen] = useState(false);
  // A workspace or tab switch starts the rail clean: a filter or folded folder
  // left over from the last worktree must not hide files in this one.
  const viewScope = `${ws.id}|${sessionKey}`;
  const [seenViewScope, setSeenViewScope] = useState(viewScope);
  if (seenViewScope !== viewScope) {
    setSeenViewScope(viewScope);
    setQuery("");
    setCollapsed(new Set());
    setKeysOpen(false);
    setCopied(false);
    setChunkPos(null);
  }
  // A manual refresh re-reads the open file too, not only the list.
  const [refreshNonce, setRefreshNonce] = useState(0);
  const shellRef = useRef<HTMLDivElement | null>(null);

  // The fork-parent base + the config default start; the SAME effective baseRef
  // threads into the change list and the redline so they never disagree.
  const { baseRef: resolvedBaseRef, defaultStart } = useSourceDiffBase(ws);
  // The comparison basis is a Working-mode concept; History keeps its own.
  const baseRef = inHistory ? resolvedBaseRef : effectiveBaseRef(baseMode, resolvedBaseRef);
  // A remote workspace is anchored at the commit the machine prepared it from,
  // which the location records by name.
  const baseLabel =
    remoteWorkspace && baseMode !== "head"
      ? `vs ${ws.location.baseRef || resolvedBaseRef || defaultStart} (fork point)`
      : comparisonLabel(baseMode, resolvedBaseRef, defaultStart);

  // SW-VE-12: the overlay mounts on every folder-rooted kind now, not just
  // `story`. The kind gate is gone — `caps.sourceCapable` is TRUE for every
  // live kind, so the overlay follows `isFocused && shown && ready`. The
  // "no working tree" case is a body-level empty state (see the no-repo
  // branch below), not a hidden overlay. Lazy fetch stays scoped by
  // `effectiveActive` + the live tri-state `tabRepo` so a non-git cwd never
  // hits the seam. The resolver reads `liveCwd` from above — SW-VE-14's
  // OSC 7 revision counter re-runs it on every real `cd`.
  const tabRepo = useTabSourceRepo(ws, liveCwd).repo;
  const active = isFocused && shown && ws.status === "ready";
  // `repo` is the WorkspaceSourceRepo (cwd/remote) consumed by the fetch
  // seam; `tabRepo` is the live resolver tri-state (undefined=in-flight,
  // null=non-git, {root,isRoot}=working tree). Fetching needs BOTH: a known
  // working tree AND a repo descriptor.
  const effectiveActive = active && repo !== null && tabRepo != null;
  useRepoWorktreeWatch(ws.id, repo, effectiveActive, teamKey);
  // A local repo passes its plain cwd (the hooks' back-compat path); a remote
  // one passes the SourceRepo so the seam routes to the machine. The local
  // branch uses `effectiveCwd` so `liveCwd` is the single source of truth —
  // the OSC 7 event that bumps liveCwd mid-session flows through here for
  // free.
  const changesetKey = repo?.kind === "remote" ? repo : effectiveCwd;
  const { files, loading, error, reload } = useSourceChangeset(
    changesetKey,
    baseRef,
    effectiveActive,
    liveNonce,
  );
  // A hidden team shown again reconciles the list at once: the debounce above is for live-edit
  // bursts, and the list is already stale by however long the team was away.
  const seenResumeRef = useRef(resumeNonce);
  useEffect(() => {
    if (seenResumeRef.current === resumeNonce) return;
    seenResumeRef.current = resumeNonce;
    reload();
  }, [resumeNonce, reload]);

  const selected = selectedFile ? (files?.find((f) => f.path === selectedFile) ?? null) : null;
  // Show the selection only while it is still in the (loaded) change set; while
  // the list is genuinely loading keep the prior file up so a refetch doesn't
  // flicker it closed; on an errored list the rail owns the error.
  const paneFile =
    selectedFile != null && (selected != null || (files == null && error == null))
      ? selectedFile
      : null;
  const { redline, error: redlineError } = useSourceFileRedline(
    changesetKey,
    paneFile,
    selected?.renamedFrom,
    baseRef,
    liveNonce + refreshNonce,
  );

  // The rail's rows — and so the j/k order — are exactly what is on screen.
  const visibleFiles = useMemo(() => filterFiles(files ?? [], query), [files, query]);
  // The automatic layout is chosen once per change list (workspace and
  // basis; the tabs of a tile read the same list): a live refresh that
  // crosses the tree threshold must not flip the rail, and the j/k order,
  // under the reader.
  const autoTreeKey = `${ws.id}|${baseRef ?? ""}`;
  // A switch resets the change list in a render pass that still sees the old
  // list: that list is remembered as stale and never chooses the layout.
  const [autoTree, setAutoTree] = useState<{
    key: string;
    tree: boolean | null;
    stale: readonly unknown[] | null;
  }>({ key: autoTreeKey, tree: null, stale: null });
  if (autoTree.key !== autoTreeKey) {
    setAutoTree({ key: autoTreeKey, tree: null, stale: files });
  } else if (
    autoTree.tree == null &&
    files != null &&
    files.length > 0 &&
    files !== autoTree.stale
  ) {
    setAutoTree({ ...autoTree, tree: prefersTree(files) });
  }
  const treeMode =
    fileTreePref ??
    (autoTree.key === autoTreeKey && autoTree.tree != null
      ? autoTree.tree
      : prefersTree(files ?? []));
  // While filtering, a folded folder never hides a match.
  const filtering = query.trim() !== "";
  const rows = useMemo(
    () =>
      treeMode
        ? treeRows(buildFileTree(visibleFiles), filtering ? NO_FOLDS : collapsed)
        : flatRows(visibleFiles),
    [treeMode, visibleFiles, collapsed, filtering],
  );
  const order = useMemo(() => navigationOrder(rows), [rows]);
  const totals = useMemo(() => diffTotals(files ?? []), [files]);

  const hide = () =>
    activeTabSessionId &&
    toggleShown({ sessionId: activeTabSessionId, workspaceId: ws.id, teamKey, shown: false });

  // A warm team's open diff stays mounted but owns no keys.
  const ownsKeys = useScopeActive(teamKey);
  // SW-FX-140: an open diff panel owns Escape, including with the agent terminal focused.
  // The open keyboard map closes first, as its own Escape does elsewhere in the panel.
  useOverlayEscape({
    enabled: shown && isFocused && !editorOpen && ownsKeys,
    zoomActive,
    onDismiss: hide,
    onTerminalEscape: () => (keysOpen ? setKeysOpen(false) : hide()),
  });

  // Focus follows the tool: opening it takes focus (so j/k work at once and keys
  // never land in the terminal hidden underneath); hiding it hands focus back to
  // whatever had it — unless something else has taken it since, or the code
  // editor is opening in its place (it focuses itself).
  // SW-VE-12: no more kind gate. The overlay renders on every folder-rooted
  // tab whenever its session's shown flag is set — the body itself picks the
  // right content for the live resolver state.
  const visible = isFocused && shown && !editorOpen && ws.status === "ready";
  // Focus inside the tool that loses its element (a focused row, Retry, the
  // clear-filter or map-close button, a code-diff control unmounting under a
  // re-render) falls to body, where the tool's keys go dead. After each render
  // put it back on the tool — only while focus was last inside it, so focus
  // the user moved elsewhere is never taken.
  const focusInToolRef = useRef(false);
  useEffect(() => {
    if (!visible) return;
    const onFocusIn = (e: FocusEvent) => {
      focusInToolRef.current = !!shellRef.current?.contains(e.target as Node);
    };
    // A press outside the tool on something unfocusable moves focus to body
    // with no focusin: that is the user leaving, not an element unmounting.
    // Read where focus went once the press settles: a press that keeps focus
    // in the tool (a scrollbar, a control that prevents mousedown) leaves it.
    let pending: number | undefined;
    const onPointerDown = (e: PointerEvent) => {
      if (shellRef.current?.contains(e.target as Node)) return;
      window.clearTimeout(pending);
      pending = window.setTimeout(() => {
        focusInToolRef.current = !!shellRef.current?.contains(document.activeElement);
      }, 0);
    };
    document.addEventListener("focusin", onFocusIn);
    document.addEventListener("pointerdown", onPointerDown, true);
    focusInToolRef.current = !!shellRef.current?.contains(document.activeElement);
    return () => {
      window.clearTimeout(pending);
      document.removeEventListener("focusin", onFocusIn);
      document.removeEventListener("pointerdown", onPointerDown, true);
    };
  }, [visible]);
  useLayoutEffect(() => {
    if (!visible || !focusInToolRef.current) return;
    const active = document.activeElement;
    if (active == null || active === document.body)
      shellRef.current?.focus({ preventScroll: true });
  });
  // The keyboard map belongs to one showing of the tool: hiding it closes it.
  if (!visible && keysOpen) setKeysOpen(false);
  // Read at hide time, so a tab switch while the tool is up neither re-runs the
  // effect nor loses the element focus came from.
  // A layout effect, so it is current before the focus effect's cleanup runs
  // (passive cleanups run after every layout effect of the same commit).
  const tabSessionRef = useRef(activeTabSessionId);
  useLayoutEffect(() => {
    tabSessionRef.current = activeTabSessionId;
  }, [activeTabSessionId]);
  useEffect(() => {
    if (!visible) return;
    const prior = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const shell = shellRef.current;
    const openedOn = tabSessionRef.current;
    // An opener that knows better (the caption menu, the editor handing back)
    // recorded a target; always consume it so a stale one never lingers.
    const recorded = openedOn ? takeSourceDiffReturnFocus(openedOn) : null;
    // A menu item is about to unmount with its menu: never a return target.
    const priorUsable =
      !!prior &&
      prior !== document.body &&
      !shell?.contains(prior) &&
      !prior.closest('[role="menu"]');
    const returnTo = priorUsable ? prior : recorded;
    if (shell && !shell.contains(prior)) shell.focus({ preventScroll: true });
    return () => {
      const sessionId = tabSessionRef.current;
      // The active tab changed while the tool was up: the element focus came
      // from belongs to another tab's terminal, so typing must not go there.
      if (sessionId !== openedOn) return;
      const editorTakingOver =
        !!sessionId && (useViewerStore.getState().worktreeEditorOpenBySession[sessionId] ?? false);
      // The editor taking over keeps the target for the diff's return from it.
      if (editorTakingOver) {
        if (returnTo?.isConnected) setSourceDiffReturnFocus(sessionId, returnTo);
        return;
      }
      const active = document.activeElement;
      if (active && active !== document.body) return;
      if (returnTo?.isConnected) returnTo.focus({ preventScroll: true });
    };
  }, [visible]);
  // A retained tool stays mounted while its team is hidden: take focus again when it returns.
  useFocusWhenShown(teamKey, visible, () => shellRef.current?.focus({ preventScroll: true }));
  // Switching History ↔ Working swaps the shell; keep focus inside the tool.
  useEffect(() => {
    if (!visible) return;
    const active = document.activeElement;
    if (!active || active === document.body) shellRef.current?.focus({ preventScroll: true });
  }, [visible, inHistory]);

  // A fresh reveal with nothing selected lands on the first file so the pane is
  // never dead. A remembered selection that dropped out while hidden falls
  // through to "select a file" instead of jumping.
  useEffect(() => {
    if (active && !inHistory && selectedFile == null && order.length > 0) {
      setSelectedFile(sessionKey, order[0]);
    }
  }, [active, inHistory, selectedFile, order, setSelectedFile, sessionKey]);

  // The open file leaving the change set while the tool is up (the agent
  // committed it, or the basis moved to Uncommitted) steps to its nearest
  // surviving neighbour in the order it was seen in, instead of blanking the
  // pane. Only a selection seen in this view steps: one that dropped out while
  // hidden or unfocused still falls through to "select a file".
  const lastSeenRef = useRef<{ scope: string; file: string; order: readonly string[] } | null>(
    null,
  );
  useEffect(() => {
    if (!active || inHistory) {
      lastSeenRef.current = null;
      return;
    }
    if (selectedFile == null) return;
    if (order.includes(selectedFile)) {
      lastSeenRef.current = { scope: viewScope, file: selectedFile, order };
      return;
    }
    const seen = lastSeenRef.current;
    if (files == null || loading || error != null || selected != null) return;
    if (!seen || seen.scope !== viewScope || seen.file !== selectedFile) return;
    lastSeenRef.current = null;
    const next = nearestSurvivor(seen.order, selectedFile, order);
    if (next != null) setSelectedFile(sessionKey, next);
    // The focused row went with its file (focus fell to body): keep focus in
    // the tool, on the survivor's row, so its keys still answer. Focus held
    // anywhere else (the terminal, an input) is never taken.
    const shell = shellRef.current;
    // Body, or the shell itself (where the focus recovery below parks it).
    const focused = document.activeElement;
    const lost = focused == null || focused === document.body || focused === shell;
    if (shell && lost) {
      if (next != null) focusRow(shell, next);
      if (!shell.contains(document.activeElement)) shell.focus({ preventScroll: true });
    }
  }, [
    active,
    inHistory,
    selectedFile,
    order,
    viewScope,
    files,
    loading,
    error,
    selected,
    setSelectedFile,
    sessionKey,
  ]);

  // Adaptive first lead: History on a clean tree, Working when there are changes.
  // Remote workspaces stay in Working — the commit graph reads local git only.
  useEffect(() => {
    if (!active || historyMode !== undefined || files == null) return;
    if (remoteWorkspace) return;
    setHistoryMode(sessionKey, files.length === 0);
  }, [active, historyMode, files, setHistoryMode, sessionKey, remoteWorkspace]);

  // A new file (or basis) restarts the change counter.
  const chunkKey = `${paneFile ?? ""}|${baseRef ?? ""}|${inline}|${expandAll}|${diffMode}`;
  const [seenChunkKey, setSeenChunkKey] = useState(chunkKey);
  if (seenChunkKey !== chunkKey) {
    setSeenChunkKey(chunkKey);
    setChunkPos(null);
  }
  const onChunkPosition = useCallback((pos: ChunkPosition) => setChunkPos(pos), []);

  // The pane shows a togglable CODE diff only for a resolved, non-binary,
  // non-markdown file — one guard gates every code-diff control so none flash as
  // a dead switch on a fresh open. While stepping between files, the controls
  // stay up as the next diff loads (when the last one was code), so j/k never
  // shifts the header; a file that resolves to binary drops them once known.
  const resolvedCodeDiff =
    paneFile != null &&
    redline != null &&
    !redline.binary &&
    // Identical sides (a mode-only change) render a sentence, not a diff.
    redline.before !== redline.after &&
    redlineError == null &&
    !isProseDiffPath(paneFile);
  const resolved = redline != null || redlineError != null;
  const [lastWasCode, setLastWasCode] = useState(false);
  if (resolved && lastWasCode !== resolvedCodeDiff) setLastWasCode(resolvedCodeDiff);
  const paneIsCodeDiff = resolved
    ? resolvedCodeDiff
    : lastWasCode && paneFile != null && !isProseDiffPath(paneFile);

  // The "copied" tick clears itself.
  useEffect(() => {
    if (!copied) return;
    const t = window.setTimeout(() => setCopied(false), 1400);
    return () => window.clearTimeout(t);
  }, [copied]);

  // SW-VE-12: dropped the `caps.sourceDiffToggle` early-return — the overlay
  // mounts on every folder-rooted kind; a non-git cwd surfaces its own
  // body-level empty state below.
  if (!isFocused) return null;
  if (ws.status !== "ready") return null;
  if (!shown) return null;
  if (editorOpen) return null;

  const selectFile = (path: string) => setSelectedFile(sessionKey, path);

  const setRouteState = (path: string, state: RouteState | null) =>
    setRouteStates((prev) => {
      const next = new Map(prev);
      if (state == null) next.delete(path);
      else next.set(path, state);
      return next;
    });

  // One optimistic wrapper both directions share. Neither path reloads: a
  // routed row keeps its Undo until the next organic refetch drops it.
  const withRouteState = async (
    f: PublishFile,
    op: (slug: string) => Promise<RouteState | null>,
  ) => {
    const slug = ws.sourceRepoSlug;
    if (!slug) {
      setRouteState(f.path, {
        kind: "error",
        message: "This workspace has no source repo to route into.",
      });
      return;
    }
    const scope = routeScopeRef.current;
    const seq = (routeSeqRef.current.get(f.path) ?? 0) + 1;
    routeSeqRef.current.set(f.path, seq);
    setRouteState(f.path, { kind: "routing" });
    let outcome: RouteState | null;
    try {
      outcome = await op(slug);
    } catch (e) {
      outcome = { kind: "error", message: e instanceof Error ? e.message : String(e) };
    }
    // The workspace changed while the route ran: its state is no longer shown.
    if (routeScopeRef.current !== scope) return;
    if (routeSeqRef.current.get(f.path) !== seq) return;
    setRouteState(f.path, outcome);
  };

  // Routing writes through the local worktree, so it is offered on This Mac only.
  // `routeFileToContext` and `unrouteFileFromContext` anchor on `ws.cwd`,
  // NOT `effectiveCwd`: a route is a per-worktree concept, not a per-prompt
  // one, so these writes land on the TILE's worktree even after SW-VE-14's
  // live-cwd retarget moves the editor + diff to a `cd`-landed path.
  const routing: RailRouting | null = remoteWorkspace
    ? null
    : {
        routable: isRoutable,
        state: (path) => routeStates.get(path),
        onRoute: (f) =>
          withRouteState(f, async (slug) =>
            routeStateFromOutcome(await routeFileToContext(ws.cwd, slug, f.path)),
          ),
        onUnroute: (f) =>
          withRouteState(f, async (slug) => {
            await unrouteFileFromContext(ws.cwd, slug, f.path);
            return null;
          }),
        onDismiss: (f) => setRouteState(f.path, null),
      };

  const openEditor = (path: string | null) => {
    if (!activeTabSessionId) return;
    // With no file to open, the editor resumes where the user was.
    if (path != null) setEditorFile(ws.id, path);
    // Land on the change being read, when the open file is the one in the pane.
    const line = path != null && path === paneFile ? (navRef.current?.currentLine() ?? null) : null;
    setEditorRevealLine(ws.id, path != null && line != null ? { path, line } : null);
    toggleEditorOpen({ sessionId: activeTabSessionId, workspaceId: ws.id, teamKey, open: true });
  };

  const refresh = () => {
    reload();
    setRefreshNonce((n) => n + 1);
  };

  const copyPath = (path: string) => {
    const clipboard = typeof navigator !== "undefined" ? navigator.clipboard : undefined;
    if (!clipboard) {
      toast.error("Couldn't copy the path");
      return;
    }
    void clipboard.writeText(path).then(
      () => setCopied(true),
      () => {
        setCopied(false);
        toast.error("Couldn't copy the path");
      },
    );
  };

  // While filtering every folder shows open, so a fold would only land,
  // unseen, once the filter clears.
  const toggleDir = (path: string) =>
    !filtering &&
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });

  // History is a local-git view; on a machine the toggle is not offered.
  const modeToggle = remoteWorkspace ? null : (
    <ChromeIconButton
      testId={`source-diff-mode-toggle-${ws.id}`}
      ariaLabel={historyMode ? "Show working changes" : "Show branch history"}
      tooltip={historyMode ? "Working changes" : "Branch history"}
      onClick={() => setHistoryMode(sessionKey, !historyMode)}
    >
      {historyMode ? <ListTree size={14} /> : <GitBranch size={14} />}
    </ChromeIconButton>
  );

  const hideButton = (
    <ChromeIconButton
      testId={`source-diff-collapse-${ws.id}`}
      ariaLabel="Hide source diff"
      tooltip="Hide · Esc"
      onClick={hide}
    >
      <Minimize size={14} />
    </ChromeIconButton>
  );

  // History mode — the left rail is the all-refs commit graph; the right pane is
  // the selected commit's (or the Working node's) change set through the same pane.
  if (active && inHistory) {
    return (
      <OverlayShell
        ref={shellRef}
        testId={`source-diff-history-${ws.id}`}
        ariaLabel={`Commit history — ${ws.storyId ?? "story"}`}
        tabIndex={0}
        className={SHELL_CLASS}
      >
        <aside
          data-testid={`source-diff-history-rail-${ws.id}`}
          className="flex w-[300px] shrink-0 flex-col gap-1.5 border-r border-[var(--border)] px-2 py-2"
        >
          <div className="flex items-center justify-between gap-1">
            <div className="flex select-none items-center gap-1.5">
              <GitBranch size={13} className="shrink-0 text-muted-foreground" aria-hidden />
              <h2 className="text-[13px] font-semibold text-foreground">History</h2>
            </div>
            {modeToggle}
            {hideButton}
          </div>
          <CommitGraphRail
            ws={ws}
            worktreeNonce={liveNonce}
            activeTabSessionId={activeTabSessionId}
          />
        </aside>
        <section className="flex min-w-0 flex-1 flex-col">
          <CommitDetailPane
            ws={ws}
            effectiveCwd={effectiveCwd}
            sha={selectedSha}
            baseRef={baseRef}
            worktreeNonce={liveNonce}
            inline={inline}
            onToggleInline={() => toggleViewOption(sessionKey, "inline")}
            wordWrap={wordWrap}
            onToggleWordWrap={() => toggleViewOption(sessionKey, "wordWrap")}
            expandAll={expandAll}
            onToggleExpandAll={() => toggleViewOption(sessionKey, "expandAll")}
            diffMode={diffMode}
            onSetDiffMode={(m) => setDiffMode(sessionKey, m)}
            workingFiles={files}
            workingLoading={loading}
            workingError={error}
          />
        </section>
      </OverlayShell>
    );
  }

  // The code editor opens on a file that exists in the worktree: never a
  // deleted one, nor a selection that has left the change set.
  const editableFile =
    paneFile != null && selected != null && selected.status !== "deleted" ? paneFile : null;

  const structuralAvailable = paneFile != null && isStructuralLanguage(detectLanguageId(paneFile));

  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const target = e.target as HTMLElement | null;
    if (
      target &&
      (target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.tagName === "SELECT" ||
        target.isContentEditable)
    ) {
      return;
    }
    // The keyboard map closes before the tool does — from a row's route
    // control too.
    if (e.key === "Escape" && keysOpen) {
      e.preventDefault();
      e.stopPropagation();
      setKeysOpen(false);
      return;
    }
    // A per-row route control owns its own Enter/Space.
    if (target?.closest?.("[data-route-action]")) return;
    // A held toggle key would flip the view back and forth and refetch on
    // every repeat: toggles act once per press (j/k/n/p still repeat).
    if (e.repeat && "bstwr?ce".includes(e.key)) {
      e.preventDefault();
      return;
    }
    const handled = (() => {
      switch (e.key) {
        case "?":
          setKeysOpen((open) => !open);
          return true;
        case "j":
        case "ArrowDown": {
          if (e.key === "ArrowDown" && !target?.closest?.("[data-source-diff-row]")) return false;
          const fromRow = focusedRowPath(target);
          const next = stepFile(order, fromRow ?? selectedFile, 1);
          if (next) {
            selectFile(next);
            if (fromRow) focusRow(shellRef.current, next);
          }
          return true;
        }
        case "k":
        case "ArrowUp": {
          if (e.key === "ArrowUp" && !target?.closest?.("[data-source-diff-row]")) return false;
          const fromRow = focusedRowPath(target);
          const next = stepFile(order, fromRow ?? selectedFile, -1);
          if (next) {
            selectFile(next);
            if (fromRow) focusRow(shellRef.current, next);
          }
          return true;
        }
        case "Enter": {
          if (target?.closest?.("[data-source-diff-pane]")) return false;
          // A focused control (Retry, a toggle, close, a folder) owns its Enter.
          if (
            target?.closest?.("button, a, [role=radio], [role=switch]") &&
            !target.closest("[data-source-diff-row]")
          ) {
            return false;
          }
          // Enter on a Tab-focused row opens that row, not the old selection.
          const rowPath = focusedRowPath(target);
          const next =
            rowPath && order.includes(rowPath)
              ? rowPath
              : selectedFile && order.includes(selectedFile)
                ? selectedFile
                : order[0];
          if (next) selectFile(next);
          return !!next;
        }
        case "n":
          if (!paneIsCodeDiff) return false;
          navRef.current?.next();
          return true;
        case "p":
          if (!paneIsCodeDiff) return false;
          navRef.current?.prev();
          return true;
        case "/":
          filterRef.current?.focus();
          filterRef.current?.select();
          return true;
        case "s":
          if (!paneIsCodeDiff) return false;
          toggleViewOption(sessionKey, "inline");
          return true;
        case "w":
          if (!paneIsCodeDiff) return false;
          toggleViewOption(sessionKey, "wordWrap");
          return true;
        case "b":
          setBaseMode(sessionKey, baseMode === "head" ? "branchBase" : "head");
          return true;
        case "t":
          toggleViewOption(sessionKey, "fileTree", !treeMode);
          return true;
        case "c":
          if (!paneFile) return false;
          copyPath(paneFile);
          return true;
        case "e":
          openEditor(editableFile);
          return true;
        case "r":
          refresh();
          return true;
        default:
          return false;
      }
    })();
    if (handled) e.preventDefault();
  };

  const { dir: paneDir, name: paneName } = splitPath(paneFile ?? "");
  const chunkLabel =
    chunkPos == null || chunkPos.count === 0
      ? null
      : chunkPos.index == null
        ? `${chunkPos.count} ${chunkPos.count === 1 ? "change" : "changes"}`
        : `${chunkPos.index + 1} of ${chunkPos.count}`;

  // SW-VE-12: the tab's cwd resolved to a non-git directory. The rail cannot
  // say anything meaningful (no files to list, nothing to retry), so collapse
  // the whole body to a single centered message — rail hidden, pane full-width.
  // Overlay chrome still mounts so Escape / close land the same way as a
  // populated diff.
  // KNOWN AMBIGUITY (reviewer §Deferred): `useTabSourceRepo`'s resolver
  // `.catch` collapses a transient error to `setRepo(null)` (see
  // `use-tab-source-repo.ts:122-126`), so a flaky probe reads here as
  // "no git repo" instead of surfacing a load failure. Low priority, but
  // a probe-level error channel + a separate body state would un-mask it.
  if (tabRepo === null) {
    // SW-VE-12 R3-M4: the no-repo body renders no filter input, no rail
    // rows, no KeyboardMap — so routing j/k/n/p/t/s/w/b/c/r into the full
    // key dispatcher would be inert at best and misleading at worst. Only
    // Escape (dismiss) and `e` (open editor) have targets in this state;
    // the handler forwards those to the shared dispatcher and swallows
    // everything else.
    const noRepoKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
      if (e.key === "Escape" || e.key === "e" || e.key === "E") onKeyDown(e);
    };
    return (
      <OverlayShell
        ref={shellRef}
        // R3-N1: distinct testid so "overlay is open AND populated" assertions
        // never false-green on a no-git cwd.
        testId={`source-diff-overlay-no-repo-${ws.id}`}
        // R1-N1: both branches read as the generic "folder" fallback — the
        // populated branch's "story" default was out of date once the
        // overlay opened on non-story kinds.
        ariaLabel={`Source diff — ${ws.storyId ?? "folder"}`}
        tabIndex={0}
        onKeyDown={noRepoKeyDown}
        className={SHELL_CLASS}
      >
        <section className="relative flex min-w-0 flex-1 flex-col">
          <OverlayHeader
            className="min-h-10 gap-2 px-3"
            title={
              <span className="flex min-w-0 flex-1 items-center gap-1.5 text-[12px] text-muted-foreground">
                <FileDiff size={13} aria-hidden className="shrink-0" />
                Diff
              </span>
            }
            actions={
              <ChromeIconButton
                ariaLabel="Close diff"
                testId={`source-diff-close-${ws.id}`}
                onClick={() => {
                  if (!activeTabSessionId) return;
                  toggleShown({ sessionId: activeTabSessionId, workspaceId: ws.id, teamKey });
                }}
              >
                <X size={13} />
              </ChromeIconButton>
            }
          />
          <OverlayCenteredState role="status" testId="source-diff-no-repo">
            No git repo at this path.
          </OverlayCenteredState>
        </section>
      </OverlayShell>
    );
  }

  return (
    <OverlayShell
      ref={shellRef}
      testId={`source-diff-overlay-expanded-${ws.id}`}
      // SW-VE-12 N1: fallback is "folder" (not "story") now that the
      // overlay mounts on every folder-rooted kind; symmetric with the
      // no-repo branch above.
      ariaLabel={`Source diff — ${ws.storyId ?? "folder"}`}
      tabIndex={0}
      onKeyDown={onKeyDown}
      className={SHELL_CLASS}
    >
      <SourceDiffRail
        wsId={ws.id}
        files={files}
        // SW-VE-12: a resolver-in-flight (`tabRepo === undefined`) reads as
        // loading too — otherwise the rail falls through to its empty-rows
        // list while we're still asking "is this a working tree?" (impeccable
        // §1.1). The `effectiveActive && files === null && error === null`
        // term closes the one-tick window after the resolver settles but
        // before the first changeset fetch has landed (M4), so the rail
        // never flashes "No uncommitted changes." under a seam that is
        // about to populate. An existing error short-circuits — the rail's
        // own error branch owns that state, Retry button and all.
        loading={
          loading || tabRepo === undefined || (effectiveActive && files === null && error === null)
        }
        error={error}
        onRetry={refresh}
        machineLabel={machineLabel}
        onShowKeys={() => setKeysOpen((open) => !open)}
        keysOpen={keysOpen}
        rows={rows}
        totals={totals}
        query={query}
        onQuery={setQuery}
        filterRef={filterRef}
        treeMode={treeMode}
        onToggleTree={() => toggleViewOption(sessionKey, "fileTree", !treeMode)}
        onToggleDir={toggleDir}
        selectedFile={selectedFile}
        onSelect={selectFile}
        baseMode={baseMode}
        baseLabel={baseLabel}
        onBaseMode={(mode) => setBaseMode(sessionKey, mode)}
        routing={routing}
      />
      <section className="relative flex min-w-0 flex-1 flex-col">
        <OverlayHeader
          className="min-h-10 gap-2 px-3"
          title={
            <div className="flex min-w-0 flex-1 flex-col justify-center">
              {paneFile ? (
                <InfoTip label={paneFile} side="bottom">
                  {/* The directory gives way first so the file name stays readable. */}
                  <span
                    className="flex min-w-0 font-mono text-[12px] text-foreground"
                    data-testid={`source-diff-pane-path-${ws.id}`}
                  >
                    <span className="min-w-0 truncate text-muted-foreground">{paneDir}</span>
                    <span className="shrink-0 font-medium">{paneName}</span>
                  </span>
                </InfoTip>
              ) : files && files.length > 0 ? (
                <span className="text-xs text-muted-foreground">
                  Select a file to view its diff
                </span>
              ) : null}
              {selected?.renamedFrom && (
                <span className="truncate font-mono text-[10.5px] text-muted-foreground">
                  renamed from {selected.renamedFrom}
                </span>
              )}
            </div>
          }
          stats={
            <div className="flex shrink-0 items-center gap-2">
              {isEditing && (
                <span
                  data-testid={`source-diff-activity-${ws.id}`}
                  className="flex items-center gap-1 text-[11px] font-medium text-[var(--cs-status-added)]"
                >
                  <span
                    className="h-1.5 w-1.5 animate-pulse rounded-full bg-[var(--cs-status-added)]"
                    aria-hidden
                  />
                  editing…
                </span>
              )}
              {selected && (
                <span data-testid={`source-diff-pane-stat-${ws.id}`}>
                  <LineStat added={selected.added} removed={selected.removed} />
                </span>
              )}
            </div>
          }
          actions={
            <div className="flex shrink-0 items-center gap-0.5">
              {paneIsCodeDiff && (
                <>
                  <div
                    className="mr-1 flex items-center gap-0.5 rounded-md border border-[var(--border)] px-0.5"
                    data-testid={`source-diff-chunk-nav-${ws.id}`}
                  >
                    <ChromeIconButton
                      testId={`source-diff-prev-change-${ws.id}`}
                      ariaLabel="Previous change"
                      tooltip="Previous change · p"
                      onClick={() => navRef.current?.prev()}
                    >
                      <ChevronUp size={14} />
                    </ChromeIconButton>
                    <span
                      className="min-w-[4.5rem] text-center text-[11px] tabular-nums text-muted-foreground"
                      aria-live="polite"
                    >
                      {chunkLabel ?? "—"}
                    </span>
                    <ChromeIconButton
                      testId={`source-diff-next-change-${ws.id}`}
                      ariaLabel="Next change"
                      tooltip="Next change · n"
                      onClick={() => navRef.current?.next()}
                    >
                      <ChevronDown size={14} />
                    </ChromeIconButton>
                  </div>
                  <DiffCodeControls
                    wsId={ws.id}
                    idPrefix="source-diff"
                    inline={inline}
                    wordWrap={wordWrap}
                    expandAll={expandAll}
                    mode={diffMode}
                    structuralAvailable={structuralAvailable}
                    onToggleInline={() => toggleViewOption(sessionKey, "inline")}
                    onToggleWordWrap={() => toggleViewOption(sessionKey, "wordWrap")}
                    onToggleExpandAll={() => toggleViewOption(sessionKey, "expandAll")}
                    onSetMode={(m) => setDiffMode(sessionKey, m)}
                  />
                  <Divider />
                </>
              )}
              {paneFile != null && (
                <ChromeIconButton
                  testId={`source-diff-follow-toggle-${ws.id}`}
                  ariaLabel={autoFollow ? "Stop following edits" : "Follow edits"}
                  tooltip={autoFollow ? "Following live edits" : "Follow live edits"}
                  aria-pressed={autoFollow}
                  onClick={() => toggleViewOption(sessionKey, "autoFollow")}
                >
                  <ArrowDownToLine
                    size={14}
                    className={autoFollow ? "text-[var(--cs-status-added)]" : undefined}
                  />
                </ChromeIconButton>
              )}
              {paneFile != null && (
                <ChromeIconButton
                  testId={`source-diff-copy-path-${ws.id}`}
                  ariaLabel={copied ? "Path copied" : "Copy file path"}
                  tooltip={copied ? "Copied" : "Copy path · c"}
                  onClick={() => copyPath(paneFile)}
                >
                  {copied ? (
                    <Check size={14} className="text-[var(--cs-status-added)]" />
                  ) : (
                    <Copy size={14} />
                  )}
                </ChromeIconButton>
              )}
              <ChromeIconButton
                testId={`source-diff-open-editor-${ws.id}`}
                ariaLabel="Open in code editor"
                tooltip={`Open in code editor at this change · ${editorHotkey ? editorHotkey : "e"}`}
                onClick={() => openEditor(editableFile)}
              >
                <Code size={14} />
              </ChromeIconButton>
              {!remoteWorkspace && <OpenInExternalEditorButton cwd={ws.cwd} wsId={ws.id} />}
              <ChromeIconButton
                testId={`source-diff-refresh-${ws.id}`}
                ariaLabel="Refresh"
                tooltip="Refresh · r"
                onClick={refresh}
              >
                <RotateCw size={14} />
              </ChromeIconButton>
              <Divider />
              {modeToggle}
              {hideButton}
            </div>
          }
        />
        {/* This pane owns the scroll for the markdown redline; a code diff's
            frame is height:100% and scrolls itself, so this stays dormant. */}
        <div className="min-h-0 flex-1 overflow-auto" data-source-diff-pane>
          {paneFile ? (
            <SourceDiffPane
              filePath={paneFile}
              binary={redline?.binary ?? false}
              status={selected?.status ?? "modified"}
              badges={selected?.badges}
              before={redline?.before ?? null}
              after={redline?.after ?? null}
              inline={inline}
              wordWrap={wordWrap}
              expandAll={expandAll}
              diffMode={diffMode}
              error={redlineError}
              autoFollow={autoFollow}
              navRef={navRef}
              onChunkPosition={onChunkPosition}
            />
          ) : files == null ? null : ( // Loading or errored: the rail carries that state; the pane stays quiet.
            <OverlayCenteredState>
              {files.length > 0 ? "Select a file to view its diff." : "No changes to review."}
            </OverlayCenteredState>
          )}
        </div>
        {keysOpen && <KeyboardMap onClose={() => setKeysOpen(false)} />}
      </section>
    </OverlayShell>
  );
}

/** The diff is a reading surface: fully opaque (the shared shell's slight
 *  translucency lets terminal text ghost through the code) and a touch deeper.
 *  The shell takes focus on open; its controls carry the visible focus rings. */
const SHELL_CLASS = "opacity-100 shadow-[0_18px_48px_-12px_rgb(0_0_0/0.45)] outline-none";

/** Every key the tool answers, in the order a reviewer reaches for them. */
const KEYMAP: ReadonlyArray<{ keys: string[]; label: string }> = [
  { keys: ["j", "k"], label: "Next / previous file" },
  { keys: ["n", "p"], label: "Next / previous change" },
  { keys: ["/"], label: "Filter files" },
  { keys: ["b"], label: "Branch or uncommitted" },
  { keys: ["t"], label: "Folders or flat list" },
  { keys: ["s"], label: "Split or inline" },
  { keys: ["w"], label: "Wrap long lines" },
  { keys: ["c"], label: "Copy file path" },
  { keys: ["e"], label: "Open in code editor" },
  { keys: ["r"], label: "Refresh" },
  { keys: ["Esc"], label: "Hide" },
];

/** The full key list, floated over the diff pane (`?` or the rail's ? button). */
function KeyboardMap({ onClose }: { onClose: () => void }) {
  return (
    <div
      role="region"
      aria-label="Keyboard shortcuts"
      data-testid="source-diff-keymap"
      className="absolute right-3 bottom-3 z-20 w-[min(18rem,calc(100%-1.5rem))] rounded-lg border border-[var(--border)] bg-[var(--popover)] p-3 text-xs shadow-[0_8px_24px_-6px_rgb(0_0_0/0.35)]"
    >
      <div className="mb-2 flex items-center justify-between">
        <h3 className="text-[12px] font-semibold text-foreground">Keyboard</h3>
        <ChromeIconButton ariaLabel="Close keyboard shortcuts" onClick={onClose}>
          <X size={13} />
        </ChromeIconButton>
      </div>
      <dl className="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-1.5">
        {KEYMAP.map(({ keys, label }) => (
          <div key={label} className="contents">
            <dt className="flex gap-1">
              {keys.map((k) => (
                <kbd
                  key={k}
                  className="inline-flex h-5 min-w-5 items-center justify-center rounded border border-[var(--border)] px-1 font-sans text-[10.5px] leading-none text-foreground"
                >
                  {k}
                </kbd>
              ))}
            </dt>
            <dd className="text-muted-foreground">{label}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}

function Divider() {
  return <span aria-hidden className="mx-1 h-4 w-px shrink-0 bg-[var(--border)]" />;
}

/**
 * SLW-DV-6 — the code-diff header control cluster: split↔inline (AC-1),
 * word-wrap (AC-6), and expand-all-context (AC-4). Shared by the Working and
 * History headers so the three toggles are defined once; the caller owns the
 * "is this a code diff?" guard and threads the per-tile state + handlers.
 * `idPrefix` keeps each surface's stable test ids (`source-diff-` / `commit-diff-`).
 */
function DiffCodeControls({
  wsId,
  idPrefix,
  inline,
  wordWrap,
  expandAll,
  mode,
  structuralAvailable,
  onToggleInline,
  onToggleWordWrap,
  onToggleExpandAll,
  onSetMode,
}: {
  wsId: string;
  idPrefix: string;
  inline: boolean;
  wordWrap: boolean;
  expandAll: boolean;
  mode: SourceDiffMode;
  structuralAvailable: boolean;
  onToggleInline: () => void;
  onToggleWordWrap: () => void;
  onToggleExpandAll: () => void;
  onSetMode: (mode: SourceDiffMode) => void;
}) {
  return (
    <>
      <ChromeIconButton
        testId={`${idPrefix}-view-toggle-${wsId}`}
        ariaLabel={inline ? "Switch to split diff" : "Switch to inline diff"}
        tooltip={inline ? "Side-by-side view" : "Inline view"}
        onClick={onToggleInline}
      >
        {inline ? <Columns2 size={13} /> : <Rows3 size={13} />}
      </ChromeIconButton>
      <ChromeIconButton
        testId={`${idPrefix}-wrap-toggle-${wsId}`}
        ariaLabel={wordWrap ? "Disable word wrap" : "Enable word wrap"}
        tooltip={wordWrap ? "No wrap" : "Word wrap"}
        active={wordWrap}
        onClick={onToggleWordWrap}
      >
        <WrapText size={13} />
      </ChromeIconButton>
      <ChromeIconButton
        testId={`${idPrefix}-expand-toggle-${wsId}`}
        ariaLabel={expandAll ? "Collapse unchanged regions" : "Expand all context"}
        tooltip={expandAll ? "Collapse unchanged" : "Expand all"}
        active={expandAll}
        onClick={onToggleExpandAll}
      >
        {expandAll ? <FoldVertical size={13} /> : <UnfoldVertical size={13} />}
      </ChromeIconButton>
      {/* SLW-DV-8b (RD-3): ignore-whitespace and structural are mutually-exclusive
          modes — each button toggles between its mode and the plain line diff. */}
      <ChromeIconButton
        testId={`${idPrefix}-ignore-ws-toggle-${wsId}`}
        ariaLabel={
          mode === "ignoreWhitespace" ? "Show whitespace changes" : "Ignore whitespace changes"
        }
        tooltip={mode === "ignoreWhitespace" ? "Showing whitespace" : "Ignore whitespace"}
        active={mode === "ignoreWhitespace"}
        onClick={() => onSetMode(mode === "ignoreWhitespace" ? "line" : "ignoreWhitespace")}
      >
        <Pilcrow size={13} />
      </ChromeIconButton>
      <ChromeIconButton
        testId={`${idPrefix}-structural-toggle-${wsId}`}
        ariaLabel={mode === "structural" ? "Show line diff" : "Structural (AST) diff"}
        tooltip={
          !structuralAvailable
            ? "Structural diff unavailable for this language"
            : mode === "structural"
              ? "Showing structural diff"
              : "Structural (AST) diff"
        }
        active={mode === "structural"}
        disabled={!structuralAvailable}
        onClick={() => onSetMode(mode === "structural" ? "line" : "structural")}
      >
        <Braces size={13} />
      </ChromeIconButton>
    </>
  );
}

/**
 * SLW-WD-5 AC-6 / RD-4 — the renderer-agnostic diff pane: a clean
 * `{ file, redline }` → render boundary that the master-detail layout drops into
 * its right column. WD-6 makes the renderer a pure `path`→renderer branch
 * (`pickRenderer`): binary → placeholder; markdown → `RedlineProse` (the
 * word-level prose redline reads better for docs — RD-3); everything else → a
 * read-only, syntax-highlighted `CodeDiffReview` (split or inline per the
 * `inline` toggle). Loading and redline-error states render inside this pane —
 * `RedlineProse` shows its own skeleton/error; the code path mirrors that with
 * an in-pane skeleton (before/after still loading) and an inline error row
 * (AC-6) — so a redline failure never blanks the rail beside it.
 */
export function SourceDiffPane({
  filePath,
  binary,
  status,
  badges,
  before,
  after,
  error,
  inline,
  wordWrap,
  expandAll,
  diffMode = "line",
  autoFollow = false,
  navRef,
  onChunkPosition,
}: {
  filePath: string;
  binary: boolean;
  status: PublishFile["status"];
  /** The file's classification badges — a `mode` badge explains an identical
   *  before/after. */
  badges?: PublishFile["badges"];
  before: string | null;
  after: string | null;
  error: string | null;
  inline: boolean;
  wordWrap: boolean;
  expandAll: boolean;
  /** SLW-DV-8b (RD-3): forward the code-diff mode (line / ignore-ws / structural). */
  diffMode?: SourceDiffMode;
  /** SLW-DV-5 (AC-4): forward live follow-the-edit to the code diff. */
  autoFollow?: boolean;
  /** The host's change navigator (n/p) and its position readout. */
  navRef?: React.Ref<DiffNavigator | null>;
  onChunkPosition?: (position: ChunkPosition) => void;
}) {
  // AC-8: the code diff is computed synchronously on the renderer thread, so a
  // very large file is gated behind an explicit "show anyway" — opting one file
  // in is remembered by path (switching files re-gates).
  const [forcedLargePath, setForcedLargePath] = useState<string | null>(null);

  if (binary) {
    // The binary placeholder REUSES the shared `FileWarningPlaceholder` (the WD-2
    // binary-affordance idiom) but OMITS its context-viewer "open current
    // version" action (a true source-baseline image diff is a scoped WD follow-up).
    return (
      <FileWarningPlaceholder
        icon={Binary}
        message={`${STATUS_LABEL[status]} · Binary file — no text diff to show`}
        filename={basename(filePath)}
        testId="source-diff-binary-placeholder"
      />
    );
  }

  // Identical text on both sides: a mode-only change (or an empty file added or
  // removed). Say what changed rather than paint an empty diff.
  if (!error && before != null && after != null && before === after) {
    return (
      <OverlayCenteredState testId="source-diff-no-content-change">
        {badges?.includes("mode")
          ? "Only the file mode changed — the content is identical."
          : status === "added"
            ? "Empty file added."
            : status === "deleted"
              ? "Empty file deleted."
              : "No content change — the file is identical on both sides."}
      </OverlayCenteredState>
    );
  }

  // AC-8 (DW-DV6-1): gate an enormous RESOLVED diff behind an explicit opt-in
  // before EITHER renderer runs — the markdown prose redline is synchronous too,
  // so the guard can't be code-only (a multi-MB doc would freeze the renderer).
  // Only fires once content is loaded (non-null); while loading each renderer
  // shows its own skeleton. Opting one file in is remembered by path.
  if (
    before != null &&
    after != null &&
    before.length + after.length > LARGE_DIFF_CHARS &&
    forcedLargePath !== filePath
  ) {
    return (
      <OverlayCenteredState
        testId="source-diff-large-gate"
        action={
          <button
            type="button"
            data-testid="source-diff-show-anyway"
            onClick={() => setForcedLargePath(filePath)}
            className="rounded border border-border px-2 py-0.5 text-[0.7rem] font-medium text-foreground hover:bg-accent"
          >
            Show diff anyway
          </button>
        }
      >
        This file is large — rendering the diff may be slow.
      </OverlayCenteredState>
    );
  }

  // RD-3/AC-4: markdown keeps the word-level prose redline.
  if (isProseDiffPath(filePath)) {
    return (
      <RedlineProse
        before={before}
        after={after}
        error={error}
        // AC-2: the file path is the no-flash reset key — a file switch resets the
        // renderer, an in-place before/after edit at the SAME key keeps the prior render.
        resetKey={filePath}
        className="prose prose-sm max-w-none px-6 py-5 dark:prose-invert"
      />
    );
  }

  // AC-6: an upstream (fetch) redline error surfaces in place — never a crash.
  if (error) {
    return (
      <div
        role="alert"
        data-testid="source-diff-code-error"
        className="flex h-full items-center justify-center px-8 text-center text-xs text-destructive"
      >
        {error}
      </div>
    );
  }

  // before/after still loading — a quiet skeleton (mirrors RedlineProse's).
  if (before == null || after == null) {
    return (
      <div
        aria-hidden
        data-testid="source-diff-code-skeleton"
        className="h-full w-full animate-pulse rounded bg-muted/40"
      />
    );
  }

  // AC-1/AC-2/AC-3: source code → read-only, syntax-highlighted code diff.
  // `CodeDiffReview` remounts on a file/mode/theme change (its own composite
  // key), so no-flash is preserved without an outer `key` here.
  // SLW-DV-8b (AC-3): both the ignore-whitespace override (synchronous, renderer
  // thread) and structural mode (worker parse) are cost the plain line diff avoids;
  // for a file only shown past the large-diff gate, force the plain line diff so an
  // oversized file never pays either cost.
  const oversized = before.length + after.length > LARGE_DIFF_CHARS;
  return (
    <CodeDiffReview
      before={before}
      after={after}
      path={filePath}
      mode={inline ? "inline" : "split"}
      wordWrap={wordWrap}
      collapseUnchanged={!expandAll}
      diffMode={oversized ? "line" : diffMode}
      autoFollow={autoFollow}
      navRef={navRef}
      onChunkPosition={onChunkPosition}
    />
  );
}

/**
 * SLW-WD-9 (AC-2/AC-8/RD-11): the History-mode right pane — the selected commit's
 * (or the Working node's) changeset as a compact file list + the selected file's
 * redline through the SAME `SourceDiffPane`. Working files are passed in (the
 * overlay already fetched them); a commit's files fetch LAZILY by sha (immutable,
 * so no nonce). Selection resets per commit; the redline is lazy per file.
 */
function CommitDetailPane({
  ws,
  effectiveCwd,
  sha,
  baseRef,
  worktreeNonce,
  inline,
  onToggleInline,
  wordWrap,
  onToggleWordWrap,
  expandAll,
  onToggleExpandAll,
  diffMode,
  onSetDiffMode,
  workingFiles,
  workingLoading,
  workingError,
}: {
  ws: Workspace;
  /** SW-VE-12 R3-B2: the local-tab fetch cwd (`liveCwd ?? ws.cwd`). Threaded
   *  from the parent so `effectiveCwd` is the ONE local-cwd read, and the
   *  History-mode fetches pick up SW-VE-14's revision-bumped liveCwd when
   *  that story lands. Remote tabs ignore this (`workspaceSourceRepo(ws)`
   *  stays source of truth for the remote branch upstream). */
  effectiveCwd: string;
  sha: string | null;
  baseRef: string | undefined;
  worktreeNonce: number;
  inline: boolean;
  onToggleInline: () => void;
  wordWrap: boolean;
  onToggleWordWrap: () => void;
  expandAll: boolean;
  onToggleExpandAll: () => void;
  diffMode: SourceDiffMode;
  onSetDiffMode: (mode: SourceDiffMode) => void;
  workingFiles: PublishFile[] | null;
  workingLoading: boolean;
  workingError: string | null;
}) {
  const commit = useSourceCommitChangeset(effectiveCwd, sha);
  const files = sha === null ? workingFiles : commit.files;
  const loading = sha === null ? workingLoading : commit.loading;
  const error = sha === null ? workingError : commit.error;

  const [picked, setPicked] = useState<string | null>(null);
  // Reset the pick when the selected commit (or the Working node) changes.
  const [seenSha, setSeenSha] = useState<string | null>(sha);
  if (seenSha !== sha) {
    setSeenSha(sha);
    setPicked(null);
  }
  // DERIVE the effective file (default to the first) so the pane is never dead —
  // no setState-in-effect. An explicit pick wins; else fall back to files[0].
  const selectedFile = picked ?? files?.[0]?.path ?? null;

  const selected = selectedFile ? (files?.find((f) => f.path === selectedFile) ?? null) : null;
  const workingRedline = useSourceFileRedline(
    effectiveCwd,
    sha === null ? selectedFile : null,
    selected?.renamedFrom,
    baseRef,
    worktreeNonce,
  );
  const commitRedline = useSourceCommitRedline(
    effectiveCwd,
    sha,
    sha !== null ? selectedFile : null,
    selected?.renamedFrom,
  );
  const redline = sha === null ? workingRedline.redline : commitRedline.redline;
  const redlineError = sha === null ? workingRedline.error : commitRedline.error;

  const label = sha === null ? "Working tree" : sha.slice(0, 7);

  return (
    <>
      <OverlayHeader
        icon={<FileDiff size={13} className="shrink-0 text-muted-foreground" aria-hidden />}
        title={
          <InfoTip label={label} side="bottom">
            <span className="min-w-0 flex-1 truncate font-mono text-[0.7rem] text-muted-foreground">
              {label}
            </span>
          </InfoTip>
        }
        stats={<CommitTotals files={files} />}
        actions={
          redline != null &&
          !redline.binary &&
          // A mode-only change renders a sentence and an error its message:
          // neither has a diff for these controls to change.
          redline.before !== redline.after &&
          redlineError == null &&
          selectedFile &&
          !isProseDiffPath(selectedFile) && (
            <DiffCodeControls
              wsId={ws.id}
              idPrefix="commit-diff"
              inline={inline}
              wordWrap={wordWrap}
              expandAll={expandAll}
              mode={diffMode}
              structuralAvailable={isStructuralLanguage(detectLanguageId(selectedFile))}
              onToggleInline={onToggleInline}
              onToggleWordWrap={onToggleWordWrap}
              onToggleExpandAll={onToggleExpandAll}
              onSetMode={onSetDiffMode}
            />
          )
        }
      />
      <div className="max-h-[38%] shrink-0 overflow-y-auto border-b border-[var(--border)] px-2 py-1">
        {loading ? (
          <div className="space-y-1" aria-hidden>
            {[0, 1].map((i) => (
              <div key={i} className="h-5 animate-pulse rounded bg-muted/50" />
            ))}
          </div>
        ) : error ? (
          <p className="px-1 py-1 text-xs text-destructive">Couldn’t load the changeset.</p>
        ) : files && files.length === 0 ? (
          <p className="px-1 py-1 text-xs text-muted-foreground">No changes in this commit.</p>
        ) : (
          <ul className="flex flex-col gap-0.5">
            {files?.map((f) => (
              <li key={f.path}>
                <button
                  type="button"
                  onClick={() => setPicked(f.path)}
                  aria-pressed={f.path === selectedFile}
                  className={cn(
                    "flex w-full items-center gap-1.5 rounded px-1 py-0.5 text-left text-xs",
                    f.path === selectedFile ? "bg-[var(--muted)]" : "hover:bg-[var(--muted)]/50",
                  )}
                >
                  <StatusChip status={f.status} />
                  <span className="min-w-0 truncate" title={f.path}>
                    {basename(f.path)}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
      <div className="min-h-0 flex-1 overflow-auto p-3" data-source-diff-pane>
        {selectedFile ? (
          <SourceDiffPane
            filePath={selectedFile}
            binary={redline?.binary ?? false}
            status={selected?.status ?? "modified"}
            badges={selected?.badges}
            before={redline?.before ?? null}
            after={redline?.after ?? null}
            inline={inline}
            wordWrap={wordWrap}
            expandAll={expandAll}
            diffMode={diffMode}
            error={redlineError}
          />
        ) : (
          <OverlayCenteredState>Select a file to view its diff.</OverlayCenteredState>
        )}
      </div>
    </>
  );
}

/** A commit's (or the Working node's) total +/−; nothing for an all-binary set. */
function CommitTotals({ files }: { files: PublishFile[] | null }) {
  const totals = diffTotals(files ?? []);
  if (!files || files.length === 0 || !totals.counted) return null;
  return <LineStat added={totals.added} removed={totals.removed} className="mr-0.5" />;
}

/** The file after `file` in `oldOrder` that is still in `newOrder`, else the
 *  nearest one before it, else the first of `newOrder`. */
function nearestSurvivor(
  oldOrder: readonly string[],
  file: string,
  newOrder: readonly string[],
): string | null {
  const alive = new Set(newOrder);
  const at = oldOrder.indexOf(file);
  if (at >= 0) {
    for (let i = at + 1; i < oldOrder.length; i++) if (alive.has(oldOrder[i])) return oldOrder[i];
    for (let i = at - 1; i >= 0; i--) if (alive.has(oldOrder[i])) return oldOrder[i];
  }
  return newOrder[0] ?? null;
}
