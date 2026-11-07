// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

import {
  resolveTabSourceRepo,
  resolveTabSourceRepoRemote,
  type TabSourceFolder,
  type TabSourceRepo,
  type TabSourceResolution,
} from "@/tauri/commands";
import { getKindCapabilities } from "@/shared/lib/workspace-kind-capabilities";
import { isRemoteWorkspace } from "@/shared/lib/workspace-location";
import type { Workspace } from "@/shared/stores/viewer-store";
import { logger } from "@/shared/lib/logger";

export type { TabSourceFolder, TabSourceRepo, TabSourceResolution };

/**
 * The per-tab-primitives source + diff gate (DESIGN-per-tab-primitives.md §3.4
 * / AD-3). Resolves TWO independent signals for the toolbar:
 *
 *   - `folder`: `{kind:"folder", cwd}` whenever the kind is `sourceCapable`
 *     and the tab has a cwd (local) or a minted handle (remote). Available
 *     immediately — the Source icon mounts without a backend round-trip.
 *   - `repo`: the git working tree the cwd (or remote handle) resolves to.
 *     Tri-state — `undefined` while the resolver call is in flight, `null`
 *     when resolved to a non-git dir, a `TabSourceRepo` otherwise. The Diff
 *     icon waits for `repo != null` so a non-git tab never flashes it.
 *
 * The `story` kind short-circuits the repo signal — its cwd is a worktree by
 * construction, no round-trip needed. Kinds that aren't `diffCapable` (`epic`)
 * skip the resolver entirely and report `repo: null`.
 *
 * DESIGN-remote-source.md §3.4 — remote branch. When
 * `workspace.location.kind === "machine"`, both signals route through the
 * remote: the folder is a virtual `remote://<machineId>/<handle>` root; the
 * repo is `resolveTabSourceRepoRemote(machineId, handle)` (also reporting a
 * virtual root). A remote story with no handle yet reports a NULL folder (we
 * can't address the remote dir without a handle) and `undefined` repo.
 */
export function useTabSourceRepo(
  workspace: Workspace,
  // SW-VE-14: optional local cwd override. Callers pass
  // `useSessionCwd(sessionId, useSessionCwdRevision(sessionId))`; a
  // non-null reading overrides `workspace.cwd` for the LOCAL path only —
  // remote tabs stay anchored to their `location.handle`, which is the
  // machine-side address and not something a local cwd resolver can speak
  // to. The effect below re-runs whenever `cwd = liveCwd ?? ws.cwd`
  // changes, so an OSC 7 bump that mutates liveCwd retargets the resolver
  // through for free.
  liveCwd?: string | null,
): TabSourceResolution {
  const caps = getKindCapabilities(workspace.kind);
  const canSource = caps.sourceCapable;
  const canDiff = caps.diffCapable;
  const remote = isRemoteWorkspace(workspace) ? workspace.location : null;
  const cwd = remote ? workspace.cwd : (liveCwd ?? workspace.cwd);
  const machineId = remote?.machineId ?? null;
  const handle = remote?.handle ?? null;

  const folder: TabSourceFolder | null = (() => {
    if (!canSource) return null;
    if (remote) {
      if (!handle) return null;
      return { kind: "folder", cwd: `remote://${remote.machineId}/${handle}` };
    }
    return cwd ? { kind: "folder", cwd } : null;
  })();

  const [repo, setRepo] = useState<TabSourceRepo | null | undefined>(() => {
    if (!canDiff) return null;
    if (workspace.kind === "story") {
      if (remote && handle) {
        return { root: `remote://${remote.machineId}/${handle}`, isRoot: true };
      }
      return { root: cwd, isRoot: true };
    }
    return undefined;
  });

  useEffect(() => {
    if (!canDiff) {
      setRepo(null);
      return;
    }
    if (remote) {
      if (!handle) {
        setRepo(undefined);
        return;
      }
      if (workspace.kind === "story") {
        setRepo({ root: `remote://${remote.machineId}/${handle}`, isRoot: true });
        return;
      }
      let cancelled = false;
      try {
        resolveTabSourceRepoRemote(remote.machineId, handle)
          .then((res) => {
            if (cancelled) return;
            setRepo(res ?? null);
          })
          .catch((err) => {
            if (cancelled) return;
            logger.warn(
              "useTabSourceRepo",
              `remote resolve failed for ${remote.machineId}/${handle} — ${err}`,
            );
            setRepo(null);
          });
      } catch (err) {
        logger.warn(
          "useTabSourceRepo",
          `remote resolve threw for ${remote.machineId}/${handle} — ${err}`,
        );
        setRepo(null);
      }
      return () => {
        cancelled = true;
      };
    }

    if (!cwd) {
      setRepo(undefined);
      return;
    }
    let cancelled = false;
    try {
      resolveTabSourceRepo(cwd)
        .then((res) => {
          if (cancelled) return;
          setRepo(res ?? null);
        })
        .catch((err) => {
          if (cancelled) return;
          logger.warn("useTabSourceRepo", `resolve failed for ${cwd} — ${err}`);
          setRepo(null);
        });
    } catch (err) {
      logger.warn("useTabSourceRepo", `resolve threw for ${cwd} — ${err}`);
      setRepo(null);
    }
    return () => {
      cancelled = true;
    };
  }, [canDiff, cwd, workspace.kind, machineId, handle, remote]);

  return { folder, repo };
}

/** Thin wrapper for callers that need only "is this a git working tree, and
 *  where?" — the pre-split `TabSourceRepo | null | undefined` shape. */
export function useTabSourceRepoOnly(workspace: Workspace): TabSourceRepo | null | undefined {
  return useTabSourceRepo(workspace).repo;
}
