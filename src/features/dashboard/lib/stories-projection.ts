// SPDX-License-Identifier: MIT
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { buildPipelineGraph } from "@/shared/lib/pipeline-graph/pipeline-graph";
import { countDependencies } from "@/shared/lib/pipeline-graph/resolve-nav-readiness";
import { normalizeStatus } from "@/shared/lib/pipeline-graph/pipeline-utils";
import {
  resolveDeclaredStoryTitle,
  storyMetadataTitle,
} from "@/shared/lib/pipeline-graph/story-display-title";
import { computeCountsByAbsPath } from "./nav-ordering";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import { topMark, type StatusMarkKind } from "@/shared/lib/status-mark";

export type StoryProjectionRow = Record<string, unknown> & {
  id: string;
  path: string;
  ownerPath: string;
  kind: string;
};

/** Adapt the native full-team scan and native story marks, without another rule engine. */
export function buildStoriesRows(
  tree: DerivedPipelineTree,
  marks: Readonly<Record<string, StatusMarkKind>>,
  states = useStatusTaxonomyStore.getState().byId,
): StoryProjectionRow[] {
  const graph = buildPipelineGraph(tree, states);
  const counts = computeCountsByAbsPath(tree, states);
  const rows: StoryProjectionRow[] = [];
  function walk(node: DerivedPipelineTree, parent: string): StatusMarkKind | undefined {
    const childMarks = node.subEpics.map((child) => walk(child, node.absoluteDirPath));
    const base = { ownerPath: node.absoluteDirPath };
    for (const story of node.stories) {
      const relativePath = node.dirPath ? `${node.dirPath}/${story.fileName}` : story.fileName;
      const status = story.status?.trim() || normalizeStatus(story.status, states);
      const state = states.get(status);
      rows.push({
        ...base,
        id: story.filePath,
        path: story.filePath,
        relativePath,
        name: story.fileName,
        kind: "story",
        storyId: story.storyId ?? null,
        title:
          resolveDeclaredStoryTitle({
            storyId: story.storyId,
            titleMeta: storyMetadataTitle(story),
          }) ?? null,
        status: story.status,
        role: state?.role ?? null,
        lensRole: states.get(normalizeStatus(story.status, states))?.role ?? null,
        label: state?.label ?? status,
        ...graph.readiness(story, node.dirPath),
        dependencyCount: countDependencies(story),
        mark: marks[story.filePath] ?? null,
        sprint: story.sprint ?? null,
        metadataValid: story.metadataValid !== false,
        metadataInvalidReason: story.metadataInvalidReason ?? null,
      });
    }
    for (const doc of node.docs ?? [])
      rows.push({
        ...base,
        id: doc.filePath,
        path: doc.filePath,
        name: doc.fileName,
        kind: "doc",
        metadataValid: doc.metadataValid !== false,
        metadataInvalidReason: doc.metadataInvalidReason ?? null,
        inheritedFrom: doc.sourceTeam ?? null,
        overrides: doc.overridesTeam ?? null,
        shadows: doc.shadowsTeamFile ?? false,
        installedSourceRepo: doc.installedSourceRepo ?? null,
      });
    if (node.mainDoc)
      rows.push({
        ...base,
        id: node.mainDoc.filePath,
        path: node.mainDoc.filePath,
        relativePath: node.dirPath
          ? `${node.dirPath}/${node.mainDoc.filePath.split("/").pop()}`
          : node.mainDoc.filePath.split("/").pop(),
        kind: "main-doc",
        name: node.mainDoc.filePath.split("/").pop(),
        metadataValid: node.mainDoc.metadataValid !== false,
        metadataInvalidReason: node.mainDoc.metadataInvalidReason ?? null,
      });
    const mark = topMark([...childMarks, ...node.stories.map((story) => marks[story.filePath])]);
    const epic = node.mainDoc;
    const epicStatus = epic?.status?.trim() || null;
    rows.push({
      storyId: epic?.storyId ?? null,
      title:
        resolveDeclaredStoryTitle({
          storyId: epic?.storyId ?? undefined,
          titleMeta: epic?.titleMeta,
        }) ?? null,
      status: epicStatus,
      role: epicStatus ? (states.get(epicStatus)?.role ?? null) : null,
      label: epicStatus ? (states.get(epicStatus)?.label ?? epicStatus) : null,
      sprint: epic?.sprint ?? null,
      metadataValid: epic?.metadataValid !== false,
      metadataInvalidReason: epic?.metadataInvalidReason ?? null,
      ready: null,
      blockedBy: [],
      dependencyCount: 0,
      id: node.absoluteDirPath,
      path: node.absoluteDirPath,
      relativePath: node.dirPath,
      ownerPath: parent,
      kind: "epic",
      name: node.dirName,
      mainDocPath: node.mainDoc?.filePath ?? null,
      ...counts.get(node.absoluteDirPath),
      mark: mark ?? null,
    });
    return mark;
  }
  walk(tree, "");
  return rows;
}
