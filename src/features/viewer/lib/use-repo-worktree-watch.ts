// SPDX-License-Identifier: MIT
import type { ScopeKey } from "@/shared/lib/warm-scopes";
import type { WorkspaceSourceRepo } from "@/shared/lib/workspace-location";

import { useWorktreeWatch } from "./use-worktree-watch";
import { useRemoteWorktreeWatch } from "./use-remote-worktree-watch";
import { useRemoteCadence } from "./use-remote-cadence";

/**
 * DESIGN-remote-source.md §3.3 — the repo-agnostic worktree watcher. Routes to
 * the local ref-counted watcher (`use-worktree-watch`) for a `{kind:"local"}`
 * repo and the remote one (`use-remote-worktree-watch`) for `{kind:"remote"}`.
 *
 * The two sub-hooks are called UNCONDITIONALLY (per Rules of Hooks); the one
 * that doesn't apply receives `enabled: false` and is a no-op. This matters:
 * a conditional `if (repo.kind === "remote") useX() else useY()` would be a
 * hook-order violation on a tab that migrates (e.g. a soft-close + reopen on
 * a different machine). The current store never does that today, but writing
 * the hook defensively keeps it safe if it ever does.
 *
 * DESIGN-remote-phase-2.md B-2 — threads the resolved remote cadence's
 * `watcherMs` into the remote attach so the Rust polling task runs at the §0
 * remote-small (5 s) / remote-large (10 s) rate instead of the local-native
 * 2 s default. The cadence hook is always called (stable sentinel on the
 * local path) to satisfy Rules of Hooks.
 */
export function useRepoWorktreeWatch(
  wsId: string,
  repo: WorkspaceSourceRepo | null,
  enabled: boolean,
  scope?: ScopeKey | null,
): void {
  const isLocal = enabled && repo?.kind === "local";
  const isRemote = enabled && repo?.kind === "remote";
  const cadenceRepo = repo ?? ({ kind: "local", cwd: "" } as const);
  const cadence = useRemoteCadence(cadenceRepo);
  useWorktreeWatch(wsId, isLocal && repo ? repo.cwd : "", isLocal, scope);
  useRemoteWorktreeWatch(
    isRemote && repo ? repo.machineId : null,
    isRemote && repo ? repo.handle : null,
    isRemote,
    isRemote ? cadence.watcherMs : undefined,
  );
}
