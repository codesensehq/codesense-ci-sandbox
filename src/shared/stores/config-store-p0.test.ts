// SPDX-License-Identifier: MIT
import { vi, describe, it, expect, beforeEach } from "vitest";
import { createRepoState } from "../../../tests/support/factories/repo-factory";

// Mock Tauri commands used by config-store
const mockSaveAppState = vi.fn();
const mockLoadAppState = vi.fn();
const mockCreateContextRepo = vi.fn();

vi.mock("@/tauri/commands", () => ({
  saveAppState: (...args: unknown[]) => mockSaveAppState(...args),
  loadAppState: (...args: unknown[]) => mockLoadAppState(...args),
  createContextRepo: (...args: unknown[]) => mockCreateContextRepo(...args),
}));

import { useConfigStore } from "./config-store";

describe("useConfigStore @P0", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useConfigStore.setState({
      initialized: false,
      repos: [],
    });

    // Default happy-path mocks
    mockSaveAppState.mockResolvedValue(undefined);
    mockLoadAppState.mockResolvedValue(JSON.stringify({ repos: [] }));
  });

  // ---------------------------------------------------------------------------
  // P0-012: Config store — add repo creates context repo
  // ---------------------------------------------------------------------------
  describe("P0-012: Config store — add repo creates context repo", () => {
    it("calls createContextRepo and adds the resulting RepoState to the store", async () => {
      // Given: createContextRepo returns context repo info
      const repoPath = "/Users/dev/my-project";
      mockCreateContextRepo.mockResolvedValue({
        path: "/app/contexts/my-project",
        repo_name: "my-project",
        has_remote: false,
        remote_url: null,
      });

      // When: addRepo is called
      await useConfigStore.getState().addRepo(repoPath);

      // Then: createContextRepo was invoked with the repo path
      expect(mockCreateContextRepo).toHaveBeenCalledWith(repoPath);

      // Then: the repo is added to the store with correct fields
      const repos = useConfigStore.getState().repos;
      expect(repos).toHaveLength(1);
      expect(repos[0]).toEqual({
        repoPath: "/Users/dev/my-project",
        repoName: "my-project",
        contextRepoPath: "/app/contexts/my-project",
        teamContentPath: "",
        hasRemote: false,
        remoteUrl: null,
        mainDoc: "",
      });

      // Then: state is persisted
      expect(mockSaveAppState).toHaveBeenCalledOnce();
      const persisted = JSON.parse(mockSaveAppState.mock.calls[0][0]);
      expect(persisted.repos).toHaveLength(1);
      expect(persisted.repos[0].repoPath).toBe(repoPath);
    });

    it("propagates remote info from createContextRepo", async () => {
      // Given: the repo has a remote
      mockCreateContextRepo.mockResolvedValue({
        path: "/app/contexts/my-project",
        repo_name: "my-project",
        has_remote: true,
        remote_url: "https://github.com/org/my-project-context.git",
      });

      // When: addRepo is called
      await useConfigStore.getState().addRepo("/Users/dev/my-project");

      // Then: hasRemote and remoteUrl are stored
      const repo = useConfigStore.getState().repos[0];
      expect(repo.hasRemote).toBe(true);
      expect(repo.remoteUrl).toBe("https://github.com/org/my-project-context.git");
    });

    it("throws when createContextRepo fails", async () => {
      // Given: createContextRepo rejects
      mockCreateContextRepo.mockRejectedValue("permission denied");

      // When/Then: addRepo propagates the error
      await expect(useConfigStore.getState().addRepo("/Users/dev/my-project")).rejects.toThrow(
        "Failed to create context repo",
      );

      // Then: no repo is added to the store
      expect(useConfigStore.getState().repos).toHaveLength(0);

      // Then: persist is not called
      expect(mockSaveAppState).not.toHaveBeenCalled();
    });
  });

  // ---------------------------------------------------------------------------
  // P0-013: Config store — persist and reload state
  // ---------------------------------------------------------------------------
  describe("P0-013: Config store — persist and reload state", () => {
    it("loadState restores repos from persisted JSON", async () => {
      const repo = createRepoState({
        repoPath: "/Users/dev/acme",
        repoName: "acme",
        contextRepoPath: "/app/contexts/acme",
      });
      mockLoadAppState.mockResolvedValue(JSON.stringify({ repos: [repo] }));

      await useConfigStore.getState().loadState();

      const state = useConfigStore.getState();
      expect(state.initialized).toBe(true);
      expect(state.repos).toHaveLength(1);
      expect(state.repos[0].repoPath).toBe("/Users/dev/acme");
      expect(state.repos[0].contextRepoPath).toBe("/app/contexts/acme");
    });

    it("loadState sets initialized even when loadAppState fails", async () => {
      mockLoadAppState.mockRejectedValue(new Error("file not found"));

      await useConfigStore.getState().loadState();

      const state = useConfigStore.getState();
      expect(state.initialized).toBe(true);
      expect(state.repos).toEqual([]);
    });

    it("addRepo followed by removeRepo persists correctly", async () => {
      // Given: a repo has been added
      mockCreateContextRepo.mockResolvedValue({
        path: "/app/contexts/my-project",
        repo_name: "my-project",
        has_remote: false,
        remote_url: null,
      });
      await useConfigStore.getState().addRepo("/Users/dev/my-project");
      expect(useConfigStore.getState().repos).toHaveLength(1);

      // When: the repo is removed
      await useConfigStore.getState().removeRepo("/Users/dev/my-project");

      // Then: repos list is empty
      expect(useConfigStore.getState().repos).toHaveLength(0);

      // Then: persist was called twice (once for add, once for remove)
      expect(mockSaveAppState).toHaveBeenCalledTimes(2);
      const lastPersisted = JSON.parse(mockSaveAppState.mock.calls[1][0]);
      expect(lastPersisted.repos).toEqual([]);
    });

    it("loadState migrates old string-path repo format", async () => {
      // Given: persisted state uses old string[] format for repos
      mockLoadAppState.mockResolvedValue(
        JSON.stringify({
          repos: ["/Users/dev/legacy-project"],
        }),
      );

      // When: loadState is called
      await useConfigStore.getState().loadState();

      // Then: string paths are migrated to RepoState objects
      const repos = useConfigStore.getState().repos;
      expect(repos).toHaveLength(1);
      expect(repos[0].repoPath).toBe("/Users/dev/legacy-project");
      expect(repos[0].repoName).toBe("legacy-project");
      expect(repos[0].contextRepoPath).toBe("");
      expect(repos[0].hasRemote).toBe(false);
    });
  });
});
