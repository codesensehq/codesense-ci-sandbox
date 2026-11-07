// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { useEffectiveRefreshKey } from "@/shared/hooks/use-effective-refresh-key";
import { useWindowVisible } from "@/shared/hooks/use-window-focused";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { privateFile } from "@/shared/lib/private-file";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { CONTEXT_GIT_MOVED_EVENT, type ContextGitMovedPayload } from "@/tauri/events";

import { useWorktreeRosterStore } from "@/features/viewer/stores/worktree-roster-store";
import { useContextGitStore } from "../store/use-context-git-store";
import { getFileRedline, stripOrgRoot } from "./file-redline-source";
import { useScheduledRead } from "./refresh-scheduler";
import type { FileRedline } from "./types";

export type RedlineRead = { redline: FileRedline } | { error: string };

export interface FileRedlineArgs {
  orgRoot: string;
  absPath: string;
  /** The repo key the file-change counter is kept under. */
  navRepoPath: string;
  oldPath?: string;
  prNumber?: number;
  commitSha?: string;
  /** Whether the tab is the one on screen; a hidden tab re-reads when it is shown again. */
  active?: boolean;
}

function sameRedline(a: FileRedline, b: FileRedline): boolean {
  return (
    a.status === b.status &&
    a.binary === b.binary &&
    a.before === b.before &&
    a.after === b.after &&
    a.beforeLabel === b.beforeLabel &&
    a.afterLabel === b.afterLabel &&
    a.baselineSha === b.baselineSha
  );
}

/**
 * One file's before/after for its Changes tab. Against GitHub it follows the file live: a
 * write to THIS file (never another one) re-reads it once the burst settles, while the tab
 * is shown and the window is on screen, catching up once when they are again. A PR's or a
 * commit's version of a file never changes, so it is read once. `null` until the first read
 * lands; after that the last answer stays until a different one arrives.
 */
export function useFileRedline({
  orgRoot,
  absPath,
  navRepoPath,
  oldPath,
  prNumber,
  commitSha,
  active = true,
}: FileRedlineArgs): { relPath: string | null; read: RedlineRead | null } {
  const contextWorktree = useWorktreeRosterStore((s) =>
    s.rows?.find((row) => row.kind === "context" && absPath.startsWith(`${row.path}/`)),
  );
  const fixed = prNumber != null || commitSha != null;
  const worktree = !fixed && contextWorktree?.kind === "context" ? contextWorktree.id : undefined;
  const root = contextWorktree?.path ?? orgRoot;
  const relPath = stripOrgRoot(absPath, root);
  const changeRevision = useContextGitStore(
    (s) =>
      Object.values(s.byTeam).find((snapshot) => snapshot.status?.worktree?.id === worktree)
        ?.changeRevision ?? 0,
  );
  const privateRoot = privateFile(absPath)?.repoRoot;
  const [baselineMoves, setBaselineMoves] = useState(0);
  useEffect(() => {
    if (fixed || !privateRoot) return;
    const group = createListenerGroup();
    group.track(
      listen<ContextGitMovedPayload>(CONTEXT_GIT_MOVED_EVENT, ({ payload }) => {
        if (
          payload.repoPath === privateRoot &&
          payload.kinds.some((kind) => kind === "head" || kind === "refs")
        )
          setBaselineMoves((n) => n + 1);
      }),
    );
    return () => group.dispose();
  }, [fixed, privateRoot]);
  const fileChanges = useFileRefreshStore((s) => s.fileRefreshKeys[navRepoPath] ?? 0);
  const changedPaths = useFileRefreshStore(
    (s): Set<string> | null => s.lastChangedContextFile[navRepoPath] ?? null,
  );
  const thisFileChanged = useEffectiveRefreshKey(fileChanges, changedPaths, absPath);
  const windowVisible = useWindowVisible();

  const sourceKey =
    relPath == null
      ? null
      : [root, worktree ?? "", relPath, oldPath ?? "", prNumber ?? "", commitSha ?? ""].join("|");
  const [read, setRead] = useState<RedlineRead | null>(null);
  // A different file starts from its skeleton; the same file keeps its last answer.
  const [seenSource, setSeenSource] = useState(sourceKey);
  if (seenSource !== sourceKey) {
    setSeenSource(sourceKey);
    setRead(null);
  }

  const doRead = useCallback(
    (isCurrent: () => boolean) =>
      relPath == null
        ? Promise.resolve()
        : (worktree
            ? getFileRedline(root, relPath, oldPath, prNumber, commitSha, worktree)
            : getFileRedline(root, relPath, oldPath, prNumber, commitSha)
          )
            .then((redline) => {
              if (!isCurrent()) return;
              setRead((prev) =>
                prev && "redline" in prev && sameRedline(prev.redline, redline)
                  ? prev
                  : { redline },
              );
            })
            .catch((e: unknown) => {
              if (!isCurrent()) return;
              // A read that fails mid-write (or while git holds the index) keeps the last good
              // diff; only a file that never loaded shows the error.
              const error = e instanceof Error ? e.message : String(e);
              setRead((prev) => (prev && "redline" in prev ? prev : { error }));
            }),
    [root, worktree, relPath, oldPath, prNumber, commitSha],
  );

  useScheduledRead(sourceKey, doRead, {
    active: windowVisible && active,
    trigger: fixed ? 0 : worktree ? `${thisFileChanged}|${changeRevision}` : thisFileChanged,
    urgentTrigger: fixed ? 0 : baselineMoves,
  });

  return { relPath, read };
}
