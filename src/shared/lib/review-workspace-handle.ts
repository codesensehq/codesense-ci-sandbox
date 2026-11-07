// SPDX-License-Identifier: MIT
export interface ReviewWorkspaceIdentity {
  repo: string;
  branch: string;
  baseRepo?: string;
  pullNumber?: number;
}

/** Canonical registry/UI handle for a review workspace. */
export function reviewWorkspaceHandle(target: ReviewWorkspaceIdentity): string {
  return target.baseRepo && target.baseRepo !== target.repo && target.pullNumber
    ? `${target.baseRepo}::pull/${target.pullNumber}`
    : `${target.repo}::${target.branch}`;
}
