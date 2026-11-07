// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { useConfigStore } from "./config-store";
import type { RepoState } from "./config-store";

const mockLoadAppState = vi.fn().mockResolvedValue("{}");

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: () => mockLoadAppState(),
  createContextRepo: vi.fn(),
}));

function makeRepo(overrides: Partial<RepoState> = {}): RepoState {
  return {
    repoPath: "/test/repo",
    repoName: "repo",
    contextRepoPath: "/app/contexts/repo",
    teamContentPath: "",
    hasRemote: false,
    remoteUrl: null,
    mainDoc: "",
    ...overrides,
  };
}

describe("useConfigStore", () => {
  beforeEach(() => {
    useConfigStore.setState({
      initialized: false,
      repos: [],
    });
  });

  it("initializes with empty state", () => {
    const state = useConfigStore.getState();
    expect(state.repos).toEqual([]);
    expect(state.initialized).toBe(false);
  });

  it("stores repo state objects", () => {
    useConfigStore.setState({ repos: [makeRepo()] });
    expect(useConfigStore.getState().repos).toHaveLength(1);
    expect(useConfigStore.getState().repos[0].repoName).toBe("repo");
    expect(useConfigStore.getState().repos[0].contextRepoPath).toBe("/app/contexts/repo");
  });

  // SW-FX-56: `setPipelineRoot` deleted (dead setter — no UI, the override-glob
  // prefix is immutable-after-load). Its normalize / traversal / empty coverage
  // is mirrored by the `setMainDoc` block below (same `updateRepo` + `normalize`
  // path). The one unique case — multi-repo isolation — is preserved here via a
  // surviving setter.
  describe("updateRepo repo isolation", () => {
    it("leaves other repos untouched", async () => {
      useConfigStore.setState({
        repos: [makeRepo({ repoPath: "/a" }), makeRepo({ repoPath: "/b", mainDoc: "custom.md" })],
      });
      await useConfigStore.getState().setMainDoc("/a", "new.md");
      const [a, b] = useConfigStore.getState().repos;
      expect(a.mainDoc).toBe("new.md");
      expect(b.mainDoc).toBe("custom.md");
    });

    it("SW-FX-56 AC-3: exposes no override-glob-prefix mutator (immutable-after-load)", () => {
      // The dead `setPipelineRoot` was deleted so the prefix cannot change
      // mid-session — closing the "overrides silently stop matching" hazard by
      // construction. Runtime lock complementing the compile-time removal.
      const store = useConfigStore.getState() as unknown as Record<string, unknown>;
      expect(store.setPipelineRoot).toBeUndefined();
      expect(store.setOverrideGlobPrefix).toBeUndefined();
    });
  });

  describe("setMainDoc", () => {
    it("updates mainDoc for the matching repo", async () => {
      useConfigStore.setState({ repos: [makeRepo()] });
      await useConfigStore.getState().setMainDoc("/test/repo", "Epic.md");
      expect(useConfigStore.getState().repos[0].mainDoc).toBe("Epic.md");
    });

    it("normalizes the input (strips whitespace)", async () => {
      useConfigStore.setState({ repos: [makeRepo()] });
      await useConfigStore.getState().setMainDoc("/test/repo", "  Epic.md  ");
      expect(useConfigStore.getState().repos[0].mainDoc).toBe("Epic.md");
    });

    it("throws on path traversal (..)", async () => {
      useConfigStore.setState({ repos: [makeRepo()] });
      await expect(
        useConfigStore.getState().setMainDoc("/test/repo", "../escape.md"),
      ).rejects.toThrow(/mainDoc.*\.\./);
    });

    it("accepts empty string as 'use default convention'", async () => {
      useConfigStore.setState({ repos: [makeRepo({ mainDoc: "Epic.md" })] });
      await useConfigStore.getState().setMainDoc("/test/repo", "");
      expect(useConfigStore.getState().repos[0].mainDoc).toBe("");
    });
  });

  describe("migrateRepos", () => {
    it("SW-XS-18: drops a legacy persisted `pipelineRoot` prefix key on load", async () => {
      mockLoadAppState.mockResolvedValueOnce(
        JSON.stringify({
          repos: [
            {
              repoPath: "/legacy",
              repoName: "legacy",
              contextRepoPath: "/ctx/legacy",
              hasRemote: false,
              remoteUrl: null,
              // The retired staleness-override glob prefix (old key name).
              pipelineRoot: "_bmad-output/impl-artifacts",
            },
          ],
          installBundleReady: false,
        }),
      );
      await useConfigStore.getState().loadState();
      expect(useConfigStore.getState().repos[0]).toEqual({
        repoPath: "/legacy",
        repoName: "legacy",
        contextRepoPath: "/ctx/legacy",
        teamContentPath: "",
        hasRemote: false,
        remoteUrl: null,
        mainDoc: "",
      });
    });

    it("coerces wrong-typed fields to defaults instead of crashing on corrupted state", async () => {
      // Hand-edited / downgraded / corrupted state files can land
      // arbitrary non-string values on what should be string fields.
      // Migration must degrade gracefully — keep what's coercible,
      // drop or default the rest — rather than letting bad types
      // propagate into the store and crash a consumer far from here.
      mockLoadAppState.mockResolvedValueOnce(
        JSON.stringify({
          repos: [
            {
              repoPath: "/garbled",
              repoName: 42, // wrong type — should fall back to derived name
              contextRepoPath: null, // wrong type — should fall back to ""
              hasRemote: "yes", // wrong type — should fall back to false
              remoteUrl: 0, // wrong type — should fall back to null
            },
            {
              // Entry missing `repoPath` entirely should be dropped.
              repoName: "no-path",
              contextRepoPath: "/ctx/x",
            },
          ],
          installBundleReady: false,
        }),
      );
      await useConfigStore.getState().loadState();
      const repos = useConfigStore.getState().repos;
      expect(repos).toHaveLength(1);
      const repo = repos[0];
      expect(repo.repoPath).toBe("/garbled");
      // repoName falls back to the derived name from the path.
      expect(repo.repoName).toBe("garbled");
      expect(repo.contextRepoPath).toBe("");
      expect(repo.hasRemote).toBe(false);
      expect(repo.remoteUrl).toBeNull();
    });
  });
});
