// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { resolveRepo, findRepoBySourcePath } from "./resolve-repo";
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

describe("resolveRepo", () => {
  beforeEach(() => {
    useConfigStore.setState({ repos: [] });
    useOrgStore.setState({ workspaceRepos: {} });
  });

  it("returns undefined when repo not found in either store", () => {
    expect(resolveRepo("/nonexistent")).toBeUndefined();
  });

  it("finds a legacy repo in config-store", () => {
    const repo = makeRepo({ repoPath: "/legacy/repo" });
    useConfigStore.setState({ repos: [repo] });

    expect(resolveRepo("/legacy/repo")).toBe(repo);
  });

  it("finds a synthetic repo in org-store.workspaceRepos", () => {
    const repo = makeRepo({ repoPath: "workspace:9north:default:acme/api" });
    useOrgStore.setState({ workspaceRepos: { "workspace:9north:default:acme/api": repo } });

    expect(resolveRepo("workspace:9north:default:acme/api")).toBe(repo);
  });

  it("prefers workspaceRepos over config-store when both contain the same repoPath", () => {
    const legacyRepo = makeRepo({ repoPath: "/shared", repoName: "legacy" });
    const workspaceRepo = makeRepo({ repoPath: "/shared", repoName: "workspace" });
    useConfigStore.setState({ repos: [legacyRepo] });
    useOrgStore.setState({ workspaceRepos: { "/shared": workspaceRepo } });

    expect(resolveRepo("/shared")).toBe(workspaceRepo);
  });
});

describe("findRepoBySourcePath", () => {
  beforeEach(() => {
    useConfigStore.setState({ repos: [] });
    useOrgStore.setState({ workspaceRepos: {}, suspendedWorkspaceRepos: {} });
  });

  it("returns undefined when no repo matches the source path", () => {
    expect(findRepoBySourcePath("/nonexistent")).toBeUndefined();
  });

  it("finds a legacy repo whose repoPath matches the source path", () => {
    const repo = makeRepo({ repoPath: "/clones/api" });
    useConfigStore.setState({ repos: [repo] });

    expect(findRepoBySourcePath("/clones/api")).toBe(repo);
  });

  it("finds an active workspace repo by localPath", () => {
    const repo = makeRepo({
      repoPath: "workspace:9north:default:acme/api",
      localPath: "/clones/acme-api",
    });
    useOrgStore.setState({ workspaceRepos: { [repo.repoPath]: repo } });

    expect(findRepoBySourcePath("/clones/acme-api")).toBe(repo);
  });

  it("finds a suspended workspace repo by localPath", () => {
    const repo = makeRepo({
      repoPath: "workspace:9north:default:acme/api",
      localPath: "/clones/acme-api",
    });
    useOrgStore.setState({
      suspendedWorkspaceRepos: { "9north:default": { [repo.repoPath]: repo } },
    });

    expect(findRepoBySourcePath("/clones/acme-api")).toBe(repo);
  });

  it("prefers workspaceRepos over config-store when both match the source path", () => {
    const legacy = makeRepo({ repoPath: "/shared/path", repoName: "legacy" });
    const workspace = makeRepo({
      repoPath: "workspace:9north:default:org/repo",
      localPath: "/shared/path",
      repoName: "workspace",
    });
    useConfigStore.setState({ repos: [legacy] });
    useOrgStore.setState({ workspaceRepos: { [workspace.repoPath]: workspace } });

    expect(findRepoBySourcePath("/shared/path")).toBe(workspace);
  });

  it("ignores workspace repos with null/empty localPath", () => {
    const unattached = makeRepo({
      repoPath: "workspace:9north:default:org/repo",
      localPath: null,
    });
    useOrgStore.setState({ workspaceRepos: { [unattached.repoPath]: unattached } });

    expect(findRepoBySourcePath("")).toBeUndefined();
    expect(findRepoBySourcePath("/anything")).toBeUndefined();
  });
});
