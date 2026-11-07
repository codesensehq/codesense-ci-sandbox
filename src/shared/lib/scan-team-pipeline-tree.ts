// SPDX-License-Identifier: MIT
import { CONTENT_SCAN_TIMEOUT_MS, requestContentScan } from "./pipeline-graph/content-scan-queue";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { resolveContentSource } from "@/shared/lib/resolve-content-source";
import { resolveTeamPipelineRepoPath } from "@/shared/hooks/use-team-pipeline-repo-path";
import { useOrgStore } from "@/shared/stores/org-store";
import { useConfigStore } from "@/shared/stores/config-store";

/**
 * Resolve a team's FULL pipeline tree the same way the UI does — resolve the
 * team's nav repo → its content dir → the shipped `scan_pipeline_tree` (scope
 * `""` = full team scan). Async → off the JS main thread, never blocks the UI.
 * Returns `null` when the team has no resolvable content repo or the scan fails
 * (a cheap no-op for the caller). The ONE place this resolve→scan chain lives, so
 * the live-status hub (`use-dashboard-stream`) and the durable story→file-path
 * resolve (`ensureStoryWorkspaceOpen`) can't drift.
 */
export async function scanTeamPipelineTree(teamKey: string): Promise<DerivedPipelineTree | null> {
  const navRepoPath = resolveTeamPipelineRepoPath(teamKey, useOrgStore.getState().orgs);
  if (!navRepoPath) return null;
  // An imperative scan may be the first consumer of an unopened team. Load
  // its same persisted convention; keep preference waiting inside the scan budget.
  const started = Date.now();
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      useConfigStore
        .getState()
        .ensureTeamMainDocPreference(teamKey)
        .catch(() => undefined),
      new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error("Loading team preferences timed out")),
          CONTENT_SCAN_TIMEOUT_MS,
        );
      }),
    ]);
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
  const repo = resolveContentSource(navRepoPath);
  if (!repo?.teamContentPath) return null;
  try {
    return await requestContentScan(
      [repo.teamContentPath, "", "", repo.mainDoc ?? ""],
      useFileRefreshStore.getState().fileRefreshKeys[navRepoPath] ?? 0,
      CONTENT_SCAN_TIMEOUT_MS - (Date.now() - started),
    );
  } catch {
    return null;
  }
}
