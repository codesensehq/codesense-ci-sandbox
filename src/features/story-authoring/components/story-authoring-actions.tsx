// SPDX-License-Identifier: MIT
import { useState, type ReactNode } from "react";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { NewStoryDialog } from "./new-story-dialog";

export interface StoryAuthoringActions {
  /** Open the New-story form, optionally preselecting an epic directory. */
  openNewStory: (epicDir?: string | null) => void;
  /** The dialogs these actions open — render once next to the entry point. */
  dialogs: ReactNode;
}

/**
 * The story-authoring entry point of the left nav: New story…. The host renders
 * `dialogs` once.
 */
export function useStoryAuthoringActions({
  tree,
  repoPath,
  onCreated,
}: {
  tree: DerivedPipelineTree | null | undefined;
  repoPath: string;
  onCreated?: (path: string) => void;
}): StoryAuthoringActions {
  // `undefined` = closed; `null` = open with no preselected epic.
  const [newStoryEpic, setNewStoryEpic] = useState<string | null | undefined>(undefined);

  const dialogs = tree ? (
    <NewStoryDialog
      open={newStoryEpic !== undefined}
      onOpenChange={(open) => {
        if (!open) setNewStoryEpic(undefined);
      }}
      tree={tree}
      repoPath={repoPath}
      initialEpicDir={newStoryEpic}
      onCreated={onCreated}
    />
  ) : null;

  return {
    openNewStory: (epicDir) => setNewStoryEpic(epicDir ?? null),
    dialogs,
  };
}
