// SPDX-License-Identifier: MIT
import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { reattachWorkspaceWatchesWithNewContext } from "@/app/lib/workspace-watch-bridge";
import { loadOrg } from "@/shared/api/registry";
import { useOrgStore } from "@/shared/stores/org-store";

import type { RepoState } from "@/shared/stores/config-store";
import type { Org } from "@/shared/types/domain";
import {
  missingResolvedRepositoryPaths,
  syntheticReposWithStaleContextPath,
  useResolvedRepositoryPathsSync,
} from "./use-resolved-repository-paths-sync";

vi.mock("@/shared/api/registry", () => ({ loadOrg: vi.fn() }));
vi.mock("@/app/lib/workspace-watch-bridge", () => ({
  reattachWorkspaceWatchesWithNewContext: vi.fn(),
}));
vi.mock("@/shared/lib/get-current-user", () => ({
  useCurrentUser: () => ({ principal: "1", github_user_id: 1 }),
}));

const local = (repo: string) => ({ owner: "local", repo, provider: "local", host: "local" });

function org(resolved: Record<string, string> | undefined): Org {
  return {
    slug: "local/e2e",
    org_root: "/h/.codesense/users/1/orgs/local/e2e",
    remote_url: null,
    teams: [{ name: "general", source_repos: [local("fixture-app"), local("fixture-api")] }],
    resolved_repository_paths: resolved,
  } as unknown as Org;
}

const APP_PATH = "/h/.codesense/users/1/orgs/local/e2e/general/repos/local/fixture-app/";
const API_PATH = "/h/.codesense/users/1/orgs/local/e2e/general/repos/local/fixture-api/";

describe("SW-LN-B51 resolved repository paths", () => {
  it("names the tracked repos Rust has not resolved a path for (a repo added mid-session)", () => {
    expect(
      missingResolvedRepositoryPaths(org({ "general\nlocal/local/fixture-app": APP_PATH })),
    ).toEqual(["general\nlocal/local/fixture-api"]);
    expect(
      missingResolvedRepositoryPaths(
        org({
          "general\nlocal/local/fixture-app": APP_PATH,
          "general\nlocal/local/fixture-api": API_PATH,
        }),
      ),
    ).toEqual([]);
  });

  it("patches a workspace repo built with an empty context path once the path resolves", () => {
    const key = "workspace:local/e2e:general:local/local/fixture-api";
    const repos = {
      [key]: { repoPath: key, contextRepoPath: "" } as RepoState,
    };
    expect(syntheticReposWithStaleContextPath([org(undefined)], repos)).toEqual([]);
    const resolved = org({ "general\nlocal/local/fixture-api": API_PATH });
    expect(syntheticReposWithStaleContextPath([resolved], repos)).toEqual([
      { repoPath: key, contextRepoPath: API_PATH },
    ]);
    expect(
      syntheticReposWithStaleContextPath([resolved], {
        [key]: { repoPath: key, contextRepoPath: API_PATH } as RepoState,
      }),
    ).toEqual([]);
  });
});

describe("useResolvedRepositoryPathsSync (SW-LN-B51)", () => {
  const key = "workspace:local/e2e:general:local/local/fixture-api";
  beforeEach(() => {
    vi.mocked(loadOrg).mockReset();
    vi.mocked(reattachWorkspaceWatchesWithNewContext).mockClear();
  });

  it("reloads an org whose added repo has no path, stores it, patches the repo and re-attaches", async () => {
    const resolved = {
      "general\nlocal/local/fixture-app": APP_PATH,
      "general\nlocal/local/fixture-api": API_PATH,
    };
    vi.mocked(loadOrg).mockResolvedValue({ org: org(resolved) } as never);
    useOrgStore.setState({
      orgs: [org({ "general\nlocal/local/fixture-app": APP_PATH })],
      workspaceRepos: { [key]: { repoPath: key, contextRepoPath: "" } as RepoState },
    } as never);

    renderHook(() => useResolvedRepositoryPathsSync());

    await vi.waitFor(() =>
      expect(useOrgStore.getState().workspaceRepos[key]?.contextRepoPath).toBe(API_PATH),
    );
    expect(loadOrg).toHaveBeenCalledTimes(1);
    expect(loadOrg).toHaveBeenCalledWith("local/e2e", expect.any(String), null, "1");
    expect(useOrgStore.getState().orgs[0].resolved_repository_paths).toEqual(resolved);
    expect(reattachWorkspaceWatchesWithNewContext).toHaveBeenCalled();
  });

  it("retries after a failed reload", async () => {
    vi.mocked(loadOrg).mockRejectedValueOnce(new Error("boom"));
    vi.mocked(loadOrg).mockResolvedValue({ org: org(undefined) } as never);
    useOrgStore.setState({ orgs: [org(undefined)], workspaceRepos: {} } as never);
    const { rerender } = renderHook(() => useResolvedRepositoryPathsSync());
    await vi.waitFor(() => expect(loadOrg).toHaveBeenCalledTimes(1));
    await Promise.resolve();
    useOrgStore.setState({ orgs: [org(undefined)] } as never);
    rerender();
    await vi.waitFor(() => expect(loadOrg).toHaveBeenCalledTimes(2));
  });
});
