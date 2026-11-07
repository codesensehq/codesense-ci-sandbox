// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import type { RepoState } from "./config-store";

const mockSaveAppState = vi.fn().mockResolvedValue(undefined);
const mockLoadAppState = vi.fn().mockResolvedValue("{}");
const mockLoadWorkspaceConfig = vi.fn();
const mockSaveWorkspaceConfig = vi.fn();
const mockGetCodesenseHome = vi.fn((): string | null => "/mock/codesense/home");

vi.mock("@/tauri/commands", () => ({
  saveAppState: (...args: unknown[]) => mockSaveAppState(...args),
  loadAppState: () => mockLoadAppState(),
  createContextRepo: vi.fn(),
  loadWorkspaceConfig: (...args: unknown[]) => mockLoadWorkspaceConfig(...args),
  saveWorkspaceConfig: (...args: unknown[]) => mockSaveWorkspaceConfig(...args),
}));

vi.mock("@/shared/lib/codesense-home", () => ({
  getCodesenseHome: () => mockGetCodesenseHome(),
  deriveOrgRoot: (p: string) => p,
}));

import { useConfigStore } from "./config-store";
import { useOrgStore } from "./org-store";

const SYNTHETIC_KEY = "workspace:acme:default:org/repo";

function makeWorkspaceRepo(): RepoState {
  return {
    repoPath: SYNTHETIC_KEY,
    repoName: "repo (acme)",
    contextRepoPath: "/mock/codesense/home/users/1/orgs/acme/_team/default/repos/org/repo",
    teamContentPath: "",
    hasRemote: true,
    remoteUrl: null,
    mainDoc: "",
  };
}

describe("config-store workspace routing (R-10)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useConfigStore.setState({ initialized: true, repos: [] });
    useOrgStore.setState({ workspaceRepos: { [SYNTHETIC_KEY]: makeWorkspaceRepo() } });
    mockLoadWorkspaceConfig.mockResolvedValue({ repos: {} });
    mockSaveWorkspaceConfig.mockResolvedValue(undefined);
  });

  // SW-FX-56: the two `setPipelineRoot routes synthetic key` tests were removed
  // (dead setter). Synthetic→workspace-config routing is covered by the
  // `setMainDoc` routing test below (same
  // `updateRepo` → `persistWorkspaceOverride` path).
  it("setMainDoc routes synthetic key to workspace config", async () => {
    await useConfigStore.getState().setMainDoc(SYNTHETIC_KEY, "Epic.md");

    expect(mockSaveWorkspaceConfig).toHaveBeenCalledWith(
      expect.any(String),
      expect.objectContaining({
        repos: expect.objectContaining({
          [SYNTHETIC_KEY]: expect.objectContaining({ mainDoc: "Epic.md" }),
        }),
      }),
    );
  });

  it("updates org-store workspaceRepos in-memory after save", async () => {
    await useConfigStore.getState().setMainDoc(SYNTHETIC_KEY, "Epic.md");

    const repo = useOrgStore.getState().workspaceRepos[SYNTHETIC_KEY];
    expect(repo?.mainDoc).toBe("Epic.md");
  });

  it("legacy key still routes to config-store", async () => {
    const legacyRepo: RepoState = {
      repoPath: "/legacy/repo",
      repoName: "legacy",
      contextRepoPath: "/ctx/legacy",
      teamContentPath: "",
      hasRemote: false,
      remoteUrl: null,
      mainDoc: "",
    };
    useConfigStore.setState({ repos: [legacyRepo] });

    await useConfigStore.getState().setMainDoc("/legacy/repo", "New.md");

    expect(mockSaveAppState).toHaveBeenCalled();
    expect(mockLoadWorkspaceConfig).not.toHaveBeenCalled();
    expect(useConfigStore.getState().repos[0].mainDoc).toBe("New.md");
  });

  it("no-ops gracefully when synthetic repo not found in org-store", async () => {
    useOrgStore.setState({ workspaceRepos: {} });

    await useConfigStore.getState().setMainDoc(SYNTHETIC_KEY, "Epic.md");

    expect(mockLoadWorkspaceConfig).not.toHaveBeenCalled();
    expect(mockSaveWorkspaceConfig).not.toHaveBeenCalled();
  });

  it("reverts the optimistic write and throws when the CodeSense home is unavailable", async () => {
    // The OS home could not be read. The persist must fail loud (throw) so updateRepo reverts the optimistic
    // in-memory write — never leave the UI showing an unsaved override.
    mockGetCodesenseHome.mockReturnValueOnce(null);

    await expect(useConfigStore.getState().setMainDoc(SYNTHETIC_KEY, "Epic.md")).rejects.toThrow(
      /the CodeSense home is unavailable/,
    );

    // Optimistic write was reverted to the prior value, and nothing was saved.
    expect(useOrgStore.getState().workspaceRepos[SYNTHETIC_KEY]?.mainDoc).toBe("");
    expect(mockSaveWorkspaceConfig).not.toHaveBeenCalled();
  });
});
