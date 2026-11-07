// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useResolvedRepo } from "./use-resolved-repo";
import { useConfigStore, type RepoState } from "@/shared/stores/config-store";
import { useOrgStore } from "@/shared/stores/org-store";

function makeRepo(overrides: Partial<RepoState> = {}): RepoState {
  return {
    repoPath: "/legacy/repo",
    repoName: "legacy-repo",
    contextRepoPath: "/ctx/legacy",
    teamContentPath: "",
    hasRemote: false,
    remoteUrl: null,
    mainDoc: "",
    ...overrides,
  };
}

describe("useResolvedRepo", () => {
  beforeEach(() => {
    useConfigStore.setState({ repos: [] });
    useOrgStore.setState({ workspaceRepos: {} });
  });

  it("returns undefined for null repoPath", () => {
    const { result } = renderHook(() => useResolvedRepo(null));
    expect(result.current).toBeUndefined();
  });

  it("returns undefined when repo not found", () => {
    const { result } = renderHook(() => useResolvedRepo("/nonexistent"));
    expect(result.current).toBeUndefined();
  });

  it("finds a legacy repo from config-store", () => {
    const repo = makeRepo({ repoPath: "/legacy/repo" });
    useConfigStore.setState({ repos: [repo] });

    const { result } = renderHook(() => useResolvedRepo("/legacy/repo"));
    expect(result.current).toEqual(repo);
  });

  it("finds a synthetic repo from org-store.workspaceRepos", () => {
    const repo = makeRepo({ repoPath: "workspace:9north:default:acme/api" });
    useOrgStore.setState({ workspaceRepos: { "workspace:9north:default:acme/api": repo } });

    const { result } = renderHook(() => useResolvedRepo("workspace:9north:default:acme/api"));
    expect(result.current).toEqual(repo);
  });

  it("reacts to workspace repo additions", () => {
    const { result } = renderHook(() => useResolvedRepo("workspace:9north:default:acme/api"));
    expect(result.current).toBeUndefined();

    const repo = makeRepo({ repoPath: "workspace:9north:default:acme/api" });
    act(() => {
      useOrgStore.setState({ workspaceRepos: { "workspace:9north:default:acme/api": repo } });
    });
    expect(result.current).toEqual(repo);
  });

  it("prefers workspaceRepos over config-store", () => {
    const legacyRepo = makeRepo({ repoPath: "/shared", repoName: "legacy" });
    const workspaceRepo = makeRepo({ repoPath: "/shared", repoName: "workspace" });
    useConfigStore.setState({ repos: [legacyRepo] });
    useOrgStore.setState({ workspaceRepos: { "/shared": workspaceRepo } });

    const { result } = renderHook(() => useResolvedRepo("/shared"));
    expect(result.current).toEqual(workspaceRepo);
  });

  // F-20: suspended workspace fallback tests
  it("F-20: finds a repo in suspendedWorkspaceRepos when not in workspaceRepos", () => {
    const repo = makeRepo({ repoPath: "workspace:9north:default:acme/api" });
    useOrgStore.setState({
      workspaceRepos: {},
      suspendedWorkspaceRepos: {
        "9north:_none": { "workspace:9north:default:acme/api": repo },
      },
    });

    const { result } = renderHook(() => useResolvedRepo("workspace:9north:default:acme/api"));
    expect(result.current).toEqual(repo);
  });

  it("F-20: prefers workspaceRepos over suspendedWorkspaceRepos", () => {
    const activeRepo = makeRepo({
      repoPath: "workspace:9north:default:acme/api",
      repoName: "active",
    });
    const suspendedRepo = makeRepo({
      repoPath: "workspace:9north:default:acme/api",
      repoName: "suspended",
    });
    useOrgStore.setState({
      workspaceRepos: { "workspace:9north:default:acme/api": activeRepo },
      suspendedWorkspaceRepos: {
        "9north:_none": { "workspace:9north:default:acme/api": suspendedRepo },
      },
    });

    const { result } = renderHook(() => useResolvedRepo("workspace:9north:default:acme/api"));
    expect(result.current).toEqual(activeRepo);
  });

  it("F-20: stays defined when repo moves from workspaceRepos to suspendedWorkspaceRepos", () => {
    const repo = makeRepo({ repoPath: "workspace:9north:default:acme/api" });
    useOrgStore.setState({
      workspaceRepos: { "workspace:9north:default:acme/api": repo },
      suspendedWorkspaceRepos: {},
    });

    const { result } = renderHook(() => useResolvedRepo("workspace:9north:default:acme/api"));
    expect(result.current).toEqual(repo);

    // Simulate suspend: move to suspendedWorkspaceRepos, clear workspaceRepos
    act(() => {
      useOrgStore.setState({
        workspaceRepos: {},
        suspendedWorkspaceRepos: {
          "9north:_none": { "workspace:9north:default:acme/api": repo },
        },
      });
    });

    // Must stay defined — this is the critical invariant preventing unmount
    expect(result.current).toEqual(repo);
  });

  it("F-20: returns undefined after eviction removes repo from suspendedWorkspaceRepos", () => {
    const repo = makeRepo({ repoPath: "workspace:9north:default:acme/api" });
    useOrgStore.setState({
      workspaceRepos: {},
      suspendedWorkspaceRepos: {
        "9north:_none": { "workspace:9north:default:acme/api": repo },
      },
    });

    const { result } = renderHook(() => useResolvedRepo("workspace:9north:default:acme/api"));
    expect(result.current).toEqual(repo);

    // Simulate eviction: remove from suspendedWorkspaceRepos
    act(() => {
      useOrgStore.setState({
        workspaceRepos: {},
        suspendedWorkspaceRepos: {},
      });
    });

    expect(result.current).toBeUndefined();
  });
});
