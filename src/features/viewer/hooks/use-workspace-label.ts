// SPDX-License-Identifier: MIT
import type { Workspace } from "@/shared/stores/viewer-store";
import { getKindCapabilities } from "@/shared/lib/workspace-kind-capabilities";
import { workspaceDisplayLabel, epicCodeFromTitle } from "@/shared/lib/worktree";
import { useStoryDetails } from "@/shared/lib/pipeline-graph/use-story-details";
import { useSummaryHeadings } from "@/shared/hooks/use-summary-headings";

/**
 * The display label for a workspace tab / tile / tray chip.
 *
 * Story + root workspaces resolve synchronously via `workspaceDisplayLabel`
 * (the path/`story_id`-derived short label). An EPIC (integration) workspace
 * shows its compact CODE — the trailing `(CODE)` parenthetical of its epic doc
 * H1 (e.g. `Sub-Epic: Epic Planning Workspace (SW-EW)` → `SW-EW`), which mirrors
 * the prefix of its child story ids so the epic tab stays as compact as a story
 * tab (SW-EW-35). The title is fetched through the SAME `useStoryDetails`
 * source (SW-EW-32) the epic HUD heading uses, so they can't drift. Falls back
 * to the prettified epic-dir leaf when the title is unresolved or carries no
 * trailing `(CODE)` (or when the epic has no doc path, e.g. legacy/test records).
 *
 * The hook is unconditional: for non-epic workspaces `useStoryDetails(undefined)`
 * is a no-op (no IPC, no cache entry), so the extra subscription is cheap on the
 * story/root pills that render in lists.
 */
export function useWorkspaceLabel(
  // `cwd` backs the SW-GW-1 generic-workspace label (its dir basename); include it
  // in the Pick so the label isn't reliant on runtime excess-property passthrough.
  ws: Pick<
    Workspace,
    "kind" | "id" | "storyId" | "epicDirPath" | "epicDocPath" | "cwd" | "contextWorktreeName"
  >,
): string {
  const isEpic = getKindCapabilities(ws.kind).resolvesDocTitle;
  const headings = useSummaryHeadings();
  const details = useStoryDetails(isEpic ? ws.epicDocPath : undefined, headings);
  const fallback = workspaceDisplayLabel(ws);
  if (!isEpic) return fallback;
  // The metadata title (frontmatter `title:` / HTML `cs:title`) wins over the H1.
  return (
    epicCodeFromTitle(details.data?.titleMeta) ?? epicCodeFromTitle(details.data?.title) ?? fallback
  );
}
