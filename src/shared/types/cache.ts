// SPDX-License-Identifier: MIT
export interface ContextRepoInfo {
  path: string;
  repo_name: string;
  has_remote: boolean;
  remote_url: string | null;
}
