// SPDX-License-Identifier: MIT
/**
 * Public surface of the pipeline-graph module.
 *
 * Anything not re-exported here is module-private — internal consumers
 * import from `./<file>` directly and external consumers go through
 * this barrel. Keeping the barrel narrow makes the dependency graph
 * cheap to reason about.
 */

export type {
  PipelineStatus,
  StoryNodeData,
  GroupNodeData,
  DerivedPipelineTree,
  ResolvedSupportingDoc,
} from "./types";

export { derivedTreeToReactFlow } from "./derived-tree-to-react-flow";
export { usePipelineTree } from "./use-pipeline-tree";
export { mergePipelineTrees } from "./merge-pipeline-trees";
// SLW-NC-1: installed-layer content as a second nav tree source.
export {
  wantSetToRepoScan,
  mergeInstalledIntoNav,
  isInstalledNavPath,
  installedRepoSlugFromNavPath,
} from "./composite-to-repo-scan";
// SLW-NC-1: the team's shared source-repo slug list + its provenance glyph map.
export { useTeamSourceRepoSlugs, useTeamRepoGlyphMap } from "./use-source-repo-selection";
// PTS-2: `buildViewerRootAbsolute` retired — see paths.ts for rationale.
