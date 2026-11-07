// SPDX-License-Identifier: MIT
import type { WorkspaceRepoOverrides, WorkspaceConfig } from "@/tauri/commands";

export type { WorkspaceRepoOverrides, WorkspaceConfig };

export function getRepoOverrides(
  config: WorkspaceConfig,
  repoPath: string,
): WorkspaceRepoOverrides {
  return config.repos[repoPath] ?? {};
}

export function setRepoOverrides(
  config: WorkspaceConfig,
  repoPath: string,
  partial: Partial<WorkspaceRepoOverrides>,
): WorkspaceConfig {
  return {
    ...config,
    repos: {
      ...config.repos,
      [repoPath]: {
        ...config.repos[repoPath],
        ...partial,
      },
    },
  };
}
