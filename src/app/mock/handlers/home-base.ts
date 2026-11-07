// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { ReconcileReport } from "@/tauri/commands";

/** Nothing required is missing on the first-run happy path, so every bucket is
 *  empty — the provider-reconcile panel resolves to "all connected" rather than
 *  a red "couldn't check affected repositories" error. */
const EMPTY_RECONCILE_REPORT: ReconcileReport = {
  gaps: [],
  comingSoon: [],
  unsupported: [],
};

/**
 * Home-base reads that aren't worktree probes. The
 * provider-reconcile panel calls `reconcile_required_providers` on mount to
 * check the pulled team config's required providers against what's connected;
 * the mock reports no gaps so the panel settles cleanly.
 */
export function homeBaseHandlers(): HandlerRegistry {
  return {
    reconcile_required_providers: (): ReconcileReport => EMPTY_RECONCILE_REPORT,

    // Detach a source repo from a team (the Manage repos "Remove" action). The
    // in-memory store update is driven client-side; the mock just acknowledges.
    remove_source_repo_from_team: () => undefined,
  };
}
