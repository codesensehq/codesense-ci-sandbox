// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";

import type { RepoState } from "@/shared/stores/config-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { Workspace } from "@/shared/stores/viewer-store";

const bumpMock = vi.hoisted(() => vi.fn());
vi.mock("@/shared/lib/refresh-for-source-repo", () => ({
  bumpFileRefreshForSourceRepo: bumpMock,
}));

import { bumpTeamNavForWorktrees } from "./reverse-sync-nav-refresh";

function makeRepoState(orgSlug: string, teamName: string, owner: string, repo: string): RepoState {
  return {
    repoPath: `workspace:${orgSlug}:${teamName}:${owner}/${repo}`,
    repoName: `${repo} (${orgSlug})`,
    contextRepoPath: `/home/user/.codesense/users/1234/orgs/${orgSlug}/${teamName}/repos/${owner}/${repo}`,
    teamContentPath: `/home/user/.codesense/users/1234/orgs/${orgSlug}/${teamName}/content`,
    hasRemote: true,
    remoteUrl: `https://github.com/${orgSlug}/context`,
    mainDoc: "",
    localPath: null,
  };
}

/** Minimal workspace — the resolvers only read `cwd`. */
function ws(cwd: string): Workspace {
  return { cwd } as unknown as Workspace;
}

describe("bumpTeamNavForWorktrees (SW-NR-1)", () => {
  beforeEach(() => {
    bumpMock.mockClear();
    useOrgStore.setState({ workspaceRepos: {} });
    useViewerStore.setState({ workspaces: {} });
  });

  it("resolves a worktree to its team nav synthetic and counter-only bumps", () => {
    const repo = makeRepoState("9north", "default", "acme", "api");
    useOrgStore.setState({ workspaceRepos: { [repo.repoPath]: repo } });
    useViewerStore.setState({
      workspaces: { "9north:default": [ws("/wt/feat-x"), ws("/wt/feat-y")] },
    });

    bumpTeamNavForWorktrees(["/wt/feat-x"]);

    expect(bumpMock).toHaveBeenCalledTimes(1);
    // counterOnly=true: nav re-scans WITHOUT clobbering a concurrent reverse
    // wave's precise per-file Set.
    expect(bumpMock).toHaveBeenCalledWith("workspace:9north:default:acme/api", undefined, true);
  });

  it("dedupes worktrees resolving to the same team", () => {
    const repo = makeRepoState("9north", "default", "acme", "api");
    useOrgStore.setState({ workspaceRepos: { [repo.repoPath]: repo } });
    useViewerStore.setState({
      workspaces: { "9north:default": [ws("/wt/feat-x"), ws("/wt/feat-y")] },
    });

    bumpTeamNavForWorktrees(["/wt/feat-x", "/wt/feat-y"]);

    expect(bumpMock).toHaveBeenCalledTimes(1);
  });

  it("skips a worktree not registered in any open workspace", () => {
    const repo = makeRepoState("9north", "default", "acme", "api");
    useOrgStore.setState({ workspaceRepos: { [repo.repoPath]: repo } });
    useViewerStore.setState({ workspaces: { "9north:default": [ws("/wt/feat-x")] } });

    bumpTeamNavForWorktrees(["/wt/unknown"]);

    expect(bumpMock).not.toHaveBeenCalled();
  });

  it("ignores an empty-string worktree path (never false-matches a cwd)", () => {
    const repo = makeRepoState("9north", "default", "acme", "api");
    useOrgStore.setState({ workspaceRepos: { [repo.repoPath]: repo } });
    // A workspace with an empty cwd must not be reached by an empty input.
    useViewerStore.setState({ workspaces: { "9north:default": [ws("")] } });

    bumpTeamNavForWorktrees([""]);

    expect(bumpMock).not.toHaveBeenCalled();
  });

  it("skips when the team has no registered synthetic repo", () => {
    // Worktree resolves to a teamKey, but workspaceRepos has no synthetic for it.
    useViewerStore.setState({ workspaces: { "9north:ghost": [ws("/wt/feat-x")] } });

    bumpTeamNavForWorktrees(["/wt/feat-x"]);

    expect(bumpMock).not.toHaveBeenCalled();
  });
});
