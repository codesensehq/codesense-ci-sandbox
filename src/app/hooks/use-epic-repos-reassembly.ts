// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";

import { assembleEpicWorktrees } from "@/tauri/commands";
import { EPIC_REPOS_CHANGED_EVENT, type EpicReposChangedPayload } from "@/tauri/events";
import {
  deriveEpicScopeFromPath,
  resolveEpicCommandContext,
} from "@/features/viewer/lib/epic-workspace-opener";
import { withErrorToast } from "@/shared/lib/with-error-toast";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { logger } from "@/shared/lib/logger";

/**
 * In-flight guard keyed by `epicFilePath` (mirrors the shipped `epicOpenInFlight`
 * set). Unlike the human-rate-limited UI add-path, an agent can fire
 * `set_epic_repos` in a tight loop (e.g. clear-then-set), so coalesce overlapping
 * re-assemblies of the SAME epic rather than racing two git-worktree builds.
 */
const reassemblyInFlight = new Set<string>();

/**
 * SW-SR-9: app-level bridge that re-assembles an epic's integration worktrees
 * after an agent edits its `repos:` via the `set_epic_repos` MCP tool.
 *
 * The MCP handler emits {@link EPIC_REPOS_CHANGED_EVENT} and returns immediately;
 * this listener runs the SAME fire-and-forget `assembleEpicWorktrees` the UI
 * add-path uses, so the heavy git-worktree work never blocks the tool call (and
 * never spins the agent's terminal). Mounted once at the app root — the emitting
 * agent may have no epic view focused, so this can't live on the team view.
 */
export function useEpicReposReassembly(): void {
  useEffect(() => {
    const group = createListenerGroup();

    const pending = listen<EpicReposChangedPayload>(EPIC_REPOS_CHANGED_EVENT, (event) => {
      const epicFilePath = event.payload?.epicFilePath;
      if (!epicFilePath) return;

      const scope = deriveEpicScopeFromPath(epicFilePath);
      const ctx = scope ? resolveEpicCommandContext(scope) : null;
      if (!ctx) {
        // The write already succeeded (the epic doc is the source of truth);
        // only re-assembly is skipped because the org/team/user isn't resolved
        // for this path. Log rather than swallow — the next epic open assembles.
        logger.warn(
          "epic-repos",
          `skipped assemble for "${epicFilePath}" — epic command context unresolved`,
        );
        return;
      }

      // Coalesce overlapping re-assemblies of the same epic (agents can burst).
      if (reassemblyInFlight.has(epicFilePath)) return;
      reassemblyInFlight.add(epicFilePath);

      // Non-blocking, exactly the SW-EW-9 UI add-path: never awaited; the reused
      // worktree/sync toast reports per-repo outcome (an uncloned repo is a
      // non-fatal "needs clone").
      const assembly = assembleEpicWorktrees({ ...ctx, epicFilePath }).finally(() =>
        reassemblyInFlight.delete(epicFilePath),
      );
      withErrorToast(assembly, "Couldn't assemble worktrees: ");
    });
    group.track(pending);

    return () => group.dispose();
  }, []);
}
