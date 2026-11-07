// SPDX-License-Identifier: MIT
export {
  branchRefMatches,
  epicCodeFromTitle,
  extractFrontmatterBlock,
  isStoryFile,
  normalizeStoryFilePath,
  parseBranchNameFromFrontmatter,
  parseDependsOnFromFrontmatter,
  parseSourceRepoFromFrontmatter,
  parseStoryIdFromFrontmatter,
  parseStoryTitle,
  prettifyDirLeaf,
  resolveBranchName,
  resolveWorktreeParams,
  resolveWorktreePath,
  storyDisplayLabel,
  storyIdFromPath,
  workspaceDisplayLabel,
} from "./resolve";
export {
  DEFAULT_CREATION_FANOUT_CAP,
  setCreationFanoutCap,
  withCreationSlot,
} from "./creation-fanout";
export { findWorktreeOnDisk } from "./find-on-disk";
export { isWorktreeYamlChangeEvent } from "./is-worktree-yaml-change";
export { setupWorktree, createWorktree, runWorktreeSetupPhase } from "./setup";
export {
  DEFAULT_BACKGROUND_SETUP_CAP,
  setBackgroundSetupCap,
  withBackgroundSetupSlot,
} from "./background-setup-fanout";
export type {
  WorktreeConfig,
  WorktreePhase,
  WorktreeProgressEvent,
  WorktreeSetupError,
  WorktreeSetupOptions,
  WorktreeSetupResult,
} from "./types";
