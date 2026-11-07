// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import type { RepoState } from "./config-store";

const mockSaveAppState = vi.fn();
const mockLoadAppState = vi.fn();
const mockLoadWorkspaceConfig = vi.fn();
const mockSaveWorkspaceConfig = vi.fn();
const mockToastError = vi.fn();
const mockLoggerError = vi.fn();
const mockLoggerWarn = vi.fn();

vi.mock("@/tauri/commands", () => ({
  saveAppState: (...args: unknown[]) => mockSaveAppState(...args),
  loadAppState: () => mockLoadAppState(),
  createContextRepo: vi.fn(),
  loadWorkspaceConfig: (...args: unknown[]) => mockLoadWorkspaceConfig(...args),
  saveWorkspaceConfig: (...args: unknown[]) => mockSaveWorkspaceConfig(...args),
}));

vi.mock("@/shared/lib/codesense-home", () => ({
  getCodesenseHome: () => "/mock/codesense/home",
  deriveOrgRoot: (p: string) => p,
}));

vi.mock("sonner", () => ({
  toast: { error: (...args: unknown[]) => mockToastError(...args) },
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: {
    error: (...args: unknown[]) => mockLoggerError(...args),
    warn: (...args: unknown[]) => mockLoggerWarn(...args),
    info: vi.fn(),
    debug: vi.fn(),
  },
}));

import { useConfigStore, __test__ as configStoreTest } from "./config-store";
import { useOrgStore } from "./org-store";

const SYNTHETIC_KEY = "workspace:acme:default:org/repo";

function makeWorkspaceRepo(overrides: Partial<RepoState> = {}): RepoState {
  return {
    repoPath: SYNTHETIC_KEY,
    repoName: "repo (acme)",
    contextRepoPath: "/mock/codesense/home/users/1/orgs/acme/_team/default/repos/org/repo",
    teamContentPath: "",
    hasRemote: true,
    remoteUrl: null,
    mainDoc: "",
    ...overrides,
  };
}

function makeLegacyRepo(overrides: Partial<RepoState> = {}): RepoState {
  return {
    repoPath: "/legacy/repo",
    repoName: "legacy",
    contextRepoPath: "/ctx/legacy",
    teamContentPath: "",
    hasRemote: false,
    remoteUrl: null,
    mainDoc: "",
    ...overrides,
  };
}

async function flush(times = 10): Promise<void> {
  for (let i = 0; i < times; i++) await Promise.resolve();
}

describe("config-store setter DRY (R-11)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockSaveAppState.mockResolvedValue(undefined);
    mockLoadAppState.mockResolvedValue("{}");
    mockLoadWorkspaceConfig.mockResolvedValue({ repos: {} });
    mockSaveWorkspaceConfig.mockResolvedValue(undefined);
    useConfigStore.setState({ initialized: true, repos: [] });
    useOrgStore.setState({ workspaceRepos: { [SYNTHETIC_KEY]: makeWorkspaceRepo() } });
  });

  describe("optimistic updates", () => {
    it("synthetic: in-memory update is visible before disk save resolves", async () => {
      let resolveSave: () => void = () => {};
      mockSaveWorkspaceConfig.mockReturnValueOnce(
        new Promise<void>((r) => {
          resolveSave = r;
        }),
      );
      const promise = useConfigStore.getState().setMainDoc(SYNTHETIC_KEY, "_specs");
      await flush();
      expect(useOrgStore.getState().workspaceRepos[SYNTHETIC_KEY]?.mainDoc).toBe("_specs");
      expect(mockSaveWorkspaceConfig).toHaveBeenCalledTimes(1);
      resolveSave();
      await promise;
    });

    it("legacy: in-memory update is visible before disk save resolves", async () => {
      useConfigStore.setState({ initialized: true, repos: [makeLegacyRepo()] });
      let resolveSave: () => void = () => {};
      mockSaveAppState.mockReturnValueOnce(
        new Promise<void>((r) => {
          resolveSave = r;
        }),
      );
      const promise = useConfigStore.getState().setMainDoc("/legacy/repo", "_specs");
      await flush();
      expect(useConfigStore.getState().repos[0].mainDoc).toBe("_specs");
      resolveSave();
      await promise;
    });
  });

  describe("persistence failure reverts in-memory state", () => {
    it("synthetic: revert + rejects when saveWorkspaceConfig fails", async () => {
      mockSaveWorkspaceConfig.mockRejectedValueOnce(new Error("disk full"));
      await expect(useConfigStore.getState().setMainDoc(SYNTHETIC_KEY, "_specs")).rejects.toThrow();
      // SW-FX-56: setter-DRY machinery now exercised via setMainDoc (the dead
      // setPipelineRoot was deleted). Revert restores mainDoc to its original "".
      expect(useOrgStore.getState().workspaceRepos[SYNTHETIC_KEY]?.mainDoc).toBe("");
      expect(mockLoggerError).toHaveBeenCalled();
    });

    it("legacy: revert + rejects when saveAppState fails", async () => {
      useConfigStore.setState({ initialized: true, repos: [makeLegacyRepo()] });
      mockSaveAppState.mockRejectedValueOnce(new Error("disk full"));
      await expect(
        useConfigStore.getState().setMainDoc("/legacy/repo", "_specs"),
      ).rejects.toThrow();
      expect(useConfigStore.getState().repos[0].mainDoc).toBe("");
      expect(mockLoggerError).toHaveBeenCalled();
    });
  });

  describe("snapshot-compare guard skips revert when newer write owns the slot", () => {
    it("synthetic: A's failure does NOT revert when B has already overwritten the value", async () => {
      let rejectA: (e: unknown) => void = () => {};
      let resolveB: () => void = () => {};
      mockSaveWorkspaceConfig
        .mockImplementationOnce(
          () =>
            new Promise<void>((_, reject) => {
              rejectA = reject;
            }),
        )
        .mockImplementationOnce(
          () =>
            new Promise<void>((resolve) => {
              resolveB = resolve;
            }),
        );

      const a = useConfigStore.getState().setMainDoc(SYNTHETIC_KEY, "valueA");
      await flush();
      const b = useConfigStore.getState().setMainDoc(SYNTHETIC_KEY, "valueB");
      await flush();
      expect(useOrgStore.getState().workspaceRepos[SYNTHETIC_KEY]?.mainDoc).toBe("valueB");

      rejectA(new Error("A failed"));
      await expect(a).rejects.toThrow();
      expect(useOrgStore.getState().workspaceRepos[SYNTHETIC_KEY]?.mainDoc).toBe("valueB");

      resolveB();
      await b;
      expect(useOrgStore.getState().workspaceRepos[SYNTHETIC_KEY]?.mainDoc).toBe("valueB");
    });

    it("legacy: A's failure does NOT revert when B has already overwritten the value", async () => {
      useConfigStore.setState({ initialized: true, repos: [makeLegacyRepo()] });
      let rejectA: (e: unknown) => void = () => {};
      mockSaveAppState
        .mockImplementationOnce(
          () =>
            new Promise<void>((_, reject) => {
              rejectA = reject;
            }),
        )
        .mockResolvedValueOnce(undefined);

      const a = useConfigStore.getState().setMainDoc("/legacy/repo", "valueA");
      await flush();
      const b = useConfigStore.getState().setMainDoc("/legacy/repo", "valueB");
      await flush();
      expect(useConfigStore.getState().repos[0].mainDoc).toBe("valueB");

      rejectA(new Error("A failed"));
      await expect(a).rejects.toThrow();
      expect(useConfigStore.getState().repos[0].mainDoc).toBe("valueB");

      await b;
      expect(useConfigStore.getState().repos[0].mainDoc).toBe("valueB");
    });
  });

  describe("buildRevertPartial — snapshot-compare semantics", () => {
    // Exercise `buildRevertPartial` directly. SW-XS-18: `mainDoc` is the only
    // workspace-override key left, so the per-key ownership logic reduces to
    // "revert iff the current value still equals the optimistic one".

    const { buildRevertPartial } = configStoreTest;

    const PREVIOUS = Object.freeze({ mainDoc: "orig1" });
    const OPTIMISTIC = Object.freeze({ mainDoc: "a1" });

    it("reverts the key when current still equals optimistic", () => {
      const current = { mainDoc: "a1" };
      expect(buildRevertPartial(current, OPTIMISTIC, PREVIOUS)).toEqual({ mainDoc: "orig1" });
    });

    it("returns null when the key has been overwritten by a newer write", () => {
      const current = { mainDoc: "b1" };
      expect(buildRevertPartial(current, OPTIMISTIC, PREVIOUS)).toBeNull();
    });

    it("returns null when current is undefined (repo has been removed)", () => {
      expect(buildRevertPartial(undefined, OPTIMISTIC, PREVIOUS)).toBeNull();
    });
  });

  describe("pre-flight guard for unknown synthetic key", () => {
    it("does not mutate state, save, throw, or call loadWorkspaceConfig", async () => {
      useOrgStore.setState({ workspaceRepos: {} });
      await useConfigStore.getState().setMainDoc(SYNTHETIC_KEY, "_specs");
      expect(mockLoadWorkspaceConfig).not.toHaveBeenCalled();
      expect(mockSaveWorkspaceConfig).not.toHaveBeenCalled();
      expect(mockLoggerWarn).toHaveBeenCalled();
      expect(useOrgStore.getState().workspaceRepos[SYNTHETIC_KEY]).toBeUndefined();
    });
  });

  describe("store does not toast", () => {
    it("synthetic failure does not invoke toast.error from store-internal paths", async () => {
      mockSaveWorkspaceConfig.mockRejectedValueOnce(new Error("boom"));
      await expect(useConfigStore.getState().setMainDoc(SYNTHETIC_KEY, "_specs")).rejects.toThrow();
      expect(mockToastError).not.toHaveBeenCalled();
    });

    it("legacy failure does not invoke toast.error from store-internal paths", async () => {
      useConfigStore.setState({ initialized: true, repos: [makeLegacyRepo()] });
      mockSaveAppState.mockRejectedValueOnce(new Error("boom"));
      await expect(
        useConfigStore.getState().setMainDoc("/legacy/repo", "_specs"),
      ).rejects.toThrow();
      expect(mockToastError).not.toHaveBeenCalled();
    });
  });
});
