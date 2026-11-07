// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useRef } from "react";
import { navModelPublish } from "@/tauri/commands";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { collectAllStories } from "@/shared/lib/pipeline-graph/collect-all-stories";
import { useStoryMarks } from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import { useTeamStatusStates } from "@/features/viewer/hooks/use-status-taxonomy";
import { buildStoriesRows } from "./stories-projection";

/** Publish the existing full-team scan and mark owners into the Rust delivery cache. */
export function useNavModelProjection(
  teamKey: string,
  tree: DerivedPipelineTree | null,
  files: unknown | null,
) {
  const scopes = useMemo(
    () =>
      (tree ? collectAllStories(tree) : []).map((story) => ({
        filePath: story.filePath,
        storyId: story.storyId,
      })),
    [tree],
  );
  const marks = useStoryMarks(teamKey, scopes);
  // The owning team's taxonomy, never the active team's snapshot, so retained teams stay correct.
  const states = useTeamStatusStates(teamKey);
  const rows = useMemo(
    () => (tree && states ? buildStoriesRows(tree, marks, states) : null),
    [tree, marks, states],
  );
  const pending = useRef(Promise.resolve());
  useEffect(() => {
    if (!rows || !files) return;
    // Preserve scan/mark order even when IPC completes after another render.
    pending.current = pending.current
      .catch(() => {})
      .then(() => navModelPublish(teamKey, rows, files));
    void pending.current.catch(console.error);
  }, [teamKey, rows, files]);
}
