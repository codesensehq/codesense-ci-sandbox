// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { useWindowVisible } from "@/shared/hooks/use-window-focused";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { ContextRepoStatus } from "@/tauri/commands";
import {
  CONTEXT_GIT_MOVED_EVENT,
  CONTEXT_REPO_STATUS_CHANGED_EVENT,
  type ContextGitMoveKind,
  type ContextGitMovedPayload,
} from "@/tauri/events";

import { useWorktreeRosterStore } from "@/features/viewer/stores/worktree-roster-store";
import { useContextWorktrees } from "./use-context-worktrees";
import { useContextGitStore } from "../store/use-context-git-store";
import { keepUnchangedChanges, toNavChanges, type NavChange } from "./nav-changes";
import { useScheduledRead } from "./refresh-scheduler";
import { readContextRepoStatus } from "./repo-status-source";
import { scopePublishSet, useTeamContentPrefix } from "./scope-publish-set";
import type { ResolvedNavSource } from "@/shared/hooks/use-nav-source";
import { useOrgRoot } from "./use-org-root";
import { usePublishSet } from "./use-publish-set";

/**
 * Read the team's context repo — its changed files (team-scoped) and its branch/sync status —
 * and publish them to {@link useContextGitStore} for the status button, the nav rows' tints,
 * the Changes view and the tab's diff strip. Mounted by the active team's nav only
 * (`enabled`), so backgrounded teams read nothing.
 *
 * Both reads follow the context repo's file changes live: a burst of writes is one read once
 * it settles (at most a couple of seconds behind continuous writes), one read at a time. The
 * status reads whenever the window is on screen; the changed files only while the nav that
 * shows them is open too. Whatever is missed while hidden is read once on coming back. An
 * answer equal to the last one publishes nothing, so a quiet refresh re-renders nothing.
 *
 * What git does inside `.git` (a commit, a push, a fetch, a checkout, a rebase moving on), by
 * anyone, arrives as one `context-git-moved` per settled burst: the status is read at once,
 * and when that read lands the changed files follow a move of HEAD, the index or a ref, and
 * History a move of HEAD, a ref or a fetch. A status read that shows HEAD or the upstream
 * moved does the same, so the slow safety-net poll ({@link STATUS_POLL_MS}) still carries a
 * move no event reported. While a sync is under way the poll is faster
 * ({@link STATUS_POLL_BUSY_MS}): "stopped partway" and a stale "syncing" report are decided
 * by time, and a stalled git writes nothing.
 */
export const STATUS_POLL_MS = 60_000;
export const STATUS_POLL_BUSY_MS = 30_000;

const CHANGES_KINDS: ReadonlySet<ContextGitMoveKind> = new Set(["head", "index", "refs"]);
const HISTORY_KINDS: ReadonlySet<ContextGitMoveKind> = new Set(["head", "refs", "fetch"]);

export function useContextGitSource(
  teamKey: string,
  navRepoPath: string,
  enabled: boolean,
  resolved?: ResolvedNavSource,
) {
  const resolvedRoot = useOrgRoot(navRepoPath);
  const orgRoot = enabled ? (resolved ? resolved.orgRoot : resolvedRoot) : null;
  const source = useViewerStore((s) => s.navLens?.[teamKey]?.source);
  const worktrees = useContextWorktrees(teamKey);
  const selected =
    source?.kind === "worktree" ? worktrees.find((w) => w.id === source.id) : undefined;
  const [lastWorktree, setLastWorktree] = useState<{ id: string; name: string } | null>(null);
  const [sourceNote, setSourceNote] = useState<string | null>(null);
  if (selected && (lastWorktree?.id !== selected.id || lastWorktree.name !== selected.name))
    setLastWorktree({ id: selected.id, name: selected.name });
  if (selected && sourceNote !== null) setSourceNote(null);
  useEffect(() => {
    if (source?.kind !== "worktree" || selected) return;
    let current = true;
    // The event-driven roster may still predate an agent's explicit choice.
    void useWorktreeRosterStore
      .getState()
      .reload()
      .then((rows) => {
        if (!current || !rows || rows.some((row) => row.kind === "context" && row.id === source.id))
          return;
        setSourceNote(
          `${lastWorktree?.id === source.id ? lastWorktree.name : source.id} no longer exists; showing your clone.`,
        );
        useViewerStore.getState().setNavLens(teamKey, { source: { kind: "clone" } });
      });
    return () => {
      current = false;
    };
  }, [source, selected, teamKey, lastWorktree]);
  const sourcePending = source?.kind === "worktree" && !selected;
  const selectedRoot = sourcePending ? null : (selected?.path ?? orgRoot);
  const clonePrefix = useTeamContentPrefix(navRepoPath);
  const prefix = resolved ? resolved.prefix : clonePrefix;
  const refreshPath =
    resolved?.source.kind === "private" ? (resolved.content?.teamContentPath ?? "") : navRepoPath;
  const fileChanges = useFileRefreshStore((s) => s.fileRefreshKeys[refreshPath] ?? 0);
  const windowVisible = useWindowVisible();
  const navOpen = useViewerStore((s) => !(s.navCollapsed[teamKey] ?? false));

  const [cloneStatus, setCloneStatus] = useState<ContextRepoStatus | null>(null);
  const [readError, setReadError] = useState<string | null>(null);
  const [status, setStatus] = useState<ContextRepoStatus | null>(null);
  // Bumped when git moved in a way the changed files (or History) follow; each bump is one
  // re-read, straight away.
  const [changesMoves, setChangesMoves] = useState(0);
  const [historyMoves, setHistoryMoves] = useState(0);
  const lastStatus = useRef<{ key: string | null; status: ContextRepoStatus } | null>(null);
  // Kinds reported since the last status read started; that read's landing applies them.
  const pendingKinds = useRef(new Set<ContextGitMoveKind>());

  const { set: cloneSet, loading: cloneLoading } = usePublishSet(orgRoot, prefix, {
    active: windowVisible && navOpen,
    trigger: fileChanges,
    urgentTrigger: changesMoves,
    liveCliPath: navRepoPath,
  });

  const { set: worktreeSet, loading: worktreeLoading } = usePublishSet(
    selected?.path ?? null,
    prefix,
    {
      active: enabled && windowVisible && navOpen && !!selected,
      trigger: fileChanges,
      urgentTrigger: changesMoves,
      liveCliPath: selected?.path ?? navRepoPath,
      worktree: selected?.id,
    },
  );
  const set = selected ? worktreeSet : cloneSet;
  const loading = sourcePending || (selected ? worktreeLoading : cloneLoading);
  const sourceKey = sourcePending ? null : selected ? `${selected.id}|${selected.path}` : orgRoot;
  const [seenSource, setSeenSource] = useState(sourceKey);
  if (seenSource !== sourceKey) {
    setSeenSource(sourceKey);
    setStatus(null);
  }

  const readStatus = useCallback(
    (isCurrent: () => boolean) => {
      if (!orgRoot || sourcePending) return Promise.resolve();
      const kinds = [...pendingKinds.current];
      pendingKinds.current.clear();
      let commitsMoved = false;
      return (
        selected
          ? Promise.all([
              readContextRepoStatus(orgRoot),
              readContextRepoStatus(orgRoot, selected.id),
            ]).then(([clone, chosen]) => {
              if (isCurrent()) setCloneStatus(clone);
              return chosen;
            })
          : readContextRepoStatus(orgRoot)
      )
        .then((s) => {
          if (!isCurrent()) return;
          setReadError(null);
          // Same answer, same object: the button doesn't churn.
          const prev = lastStatus.current?.key === sourceKey ? lastStatus.current.status : null;
          if (prev && sameStatus(prev, s)) {
            setStatus(prev);
            return;
          }
          lastStatus.current = { key: sourceKey, status: s };
          setStatus(s);
          commitsMoved = prev !== null && commitsKey(prev) !== commitsKey(s);
        })
        .catch((error: unknown) => {
          if (isCurrent()) setReadError(String(error));
          // A failed read keeps the last status: the button never blanks on a blip.
        })
        .finally(() => {
          if (!isCurrent()) return;
          if (selected || commitsMoved || kinds.some((k) => CHANGES_KINDS.has(k)))
            setChangesMoves((n) => n + 1);
          if (commitsMoved || kinds.some((k) => HISTORY_KINDS.has(k)))
            setHistoryMoves((n) => n + 1);
        });
    },
    [orgRoot, selected, sourcePending, sourceKey],
  );
  const {
    refresh: refreshStatus,
    markDirty: statusChanged,
    changedNow: gitMoved,
  } = useScheduledRead(sourceKey, readStatus, {
    active: windowVisible,
    trigger: fileChanges,
    refreshOnFocus: true,
    pollMs: isBusy(status) ? STATUS_POLL_BUSY_MS : STATUS_POLL_MS,
  });

  useEffect(() => {
    lastStatus.current = null;
    setStatus(null);
    setReadError(null);
  }, [orgRoot]);

  // Git moved in this repo: the backend already let the burst settle, so read now.
  useEffect(() => {
    if (!enabled || !orgRoot) return;
    const group = createListenerGroup();
    group.track(
      listen<ContextGitMovedPayload>(CONTEXT_GIT_MOVED_EVENT, (event) => {
        if (
          !samePath(event.payload.repoPath, orgRoot) &&
          (!selectedRoot || !samePath(event.payload.repoPath, selectedRoot))
        )
          return;
        for (const kind of event.payload.kinds) pendingKinds.current.add(kind);
        gitMoved();
      }),
    );
    return () => group.dispose();
  }, [enabled, orgRoot, selectedRoot, gitMoved]);

  // Going offline or coming back, and the syncing agent's reports, are status changes the
  // file watcher never sees.
  useEffect(() => {
    if (!enabled) return;
    window.addEventListener("online", statusChanged);
    window.addEventListener("offline", statusChanged);
    const group = createListenerGroup();
    group.track(listen(CONTEXT_REPO_STATUS_CHANGED_EVENT, () => statusChanged()));
    return () => {
      window.removeEventListener("online", statusChanged);
      window.removeEventListener("offline", statusChanged);
      group.dispose();
    };
  }, [enabled, statusChanged]);

  const read = useMemo(() => {
    if (!orgRoot || sourcePending) return [];
    const conflicts = (status?.conflicts ?? []).filter(
      (c) => !prefix || c.path.startsWith(`${prefix}/`),
    );
    return toNavChanges(scopePublishSet(set, prefix), selectedRoot ?? orgRoot, conflicts);
  }, [orgRoot, sourcePending, selectedRoot, set, prefix, status]);
  // Laid over the last answer, so rows already listed keep their place and their objects.
  const [changes, setChanges] = useState<NavChange[]>(read);
  const [seenRead, setSeenRead] = useState(read);
  if (seenRead !== read) {
    setSeenRead(read);
    setChanges(keepUnchangedChanges(changes, read));
  }

  const put = useContextGitStore((s) => s.put);
  useEffect(() => {
    if (!enabled) return;
    put(teamKey, {
      orgRoot: selectedRoot,
      cloneRoot: resolved?.cloneRoot ?? resolvedRoot,
      cloneStatus: selected ? cloneStatus : status,
      sourceNote,
      changeRevision: changesMoves,
      prefix,
      source: source ?? { kind: "clone" },
      error: readError,
      changes,
      byPath: new Map(
        [
          ...toNavChanges(
            scopePublishSet(cloneSet, prefix),
            orgRoot ?? "",
            selected ? (cloneStatus?.conflicts ?? []) : (status?.conflicts ?? []),
          ),
          ...changes,
        ].map((c) => [c.absPath, c]),
      ),
      status,
      loading,
      historyMoves,
    });
  }, [
    enabled,
    put,
    teamKey,
    orgRoot,
    changes,
    status,
    loading,
    historyMoves,
    prefix,
    source,
    resolved?.cloneRoot,
    resolvedRoot,
    selectedRoot,
    sourceNote,
    changesMoves,
    cloneSet,
    cloneStatus,
    selected,
    readError,
  ]);

  return { refreshStatus };
}

function isBusy(status: ContextRepoStatus | null): boolean {
  const state = status?.sync.state;
  return state === "syncing" || state === "retrying";
}

/** Where the clone's commits stand: changes when a commit, a push, a fetch or a rebase lands. */
function commitsKey(s: ContextRepoStatus): string {
  return [s.branch, s.headSha, s.upstream, s.ahead, s.behind].join("|");
}

function samePath(a: string, b: string): boolean {
  return a.replace(/\/+$/, "") === b.replace(/\/+$/, "");
}

function sameStatus(a: ContextRepoStatus, b: ContextRepoStatus): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}
