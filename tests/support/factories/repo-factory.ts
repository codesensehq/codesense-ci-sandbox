// SPDX-License-Identifier: MIT
/**
 * Test data factory for repository-related test objects.
 *
 * Usage:
 *   import { createRepoState } from "../support/factories/repo-factory";
 *
 *   const repo = createRepoState({ repoName: "my-project" });
 *
 * SS-9: `createValidateResult` + `createLockEntry` + `resetFactory` retired
 * with the install-health test suites that consumed them (sync-store-p0,
 * operation-store-*).
 */

interface RepoState {
  repoPath: string;
  repoName: string;
  contextRepoPath: string;
  teamContentPath: string;
  hasRemote: boolean;
  remoteUrl: string | null;
  viewerRoot: string;
  pipelineRoot: string;
  mainDoc: string;
}

let counter = 0;
function uniqueId() {
  return `test-${++counter}-${Date.now()}`;
}

export function createRepoState(overrides: Partial<RepoState> = {}): RepoState {
  const id = uniqueId();
  return {
    repoPath: `/tmp/test-repos/${id}`,
    repoName: `test-repo-${id}`,
    contextRepoPath: `/tmp/test-contexts/${id}`,
    teamContentPath: "",
    hasRemote: false,
    remoteUrl: null,
    viewerRoot: "_bmad-output",
    pipelineRoot: "implementation-artifacts",
    mainDoc: "",
    ...overrides,
  };
}
