// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
const { invoke, save, load, reload, flushOrg, flushViewer } = vi.hoisted(() => ({
  invoke: vi.fn(),
  save: vi.fn(),
  load: vi.fn(),
  reload: vi.fn(),
  flushOrg: vi.fn(),
  flushViewer: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@/tauri/commands", () => ({
  saveAppState: save,
  loadAppState: load,
  createContextRepo: vi.fn(),
  loadWorkspaceConfig: vi.fn(),
  saveWorkspaceConfig: vi.fn(),
}));
vi.mock("@/shared/lib/private-terminal-authority", () => ({
  listenForPrivateAuthorityRetirement: async () => {},
  observePrivateAuthority: vi.fn(),
  onPrivateAuthorityRetired: () => () => {},
}));
vi.mock("@/shared/stores/viewer-store", () => ({
  flushViewerForProfileSwitch: flushViewer,
  // shortcuts-store's registry reads the viewer's state while resolving bindings.
  useViewerStore: { getState: () => ({}), subscribe: () => () => {} },
}));
vi.mock("@/shared/stores/org-store", () => ({
  flushOrgForProfileSwitch: flushOrg,
  useOrgStore: { getState: () => ({ workspaceRepos: {} }) },
}));
const profile = { principal: "42", displayName: "Work", machineOwner: null };
beforeEach(() => {
  vi.resetModules();
  invoke
    .mockReset()
    .mockResolvedValue({ active: profile, profiles: [profile], privateAuthority: null });
  save.mockReset().mockResolvedValue(undefined);
  load.mockReset().mockResolvedValue('{"viewer":{"selected":"old"}}');
  reload.mockReset();
  flushOrg.mockReset().mockResolvedValue(undefined);
  flushViewer.mockReset().mockResolvedValue(undefined);
});
it("holds selection until a pending config approval and repo snapshot reach the old profile", async () => {
  const { useConfigStore } = await import("./config-store");
  const { useLocalProfileStore } = await import("./local-profile-store");
  await useLocalProfileStore.getState().initialize();
  useConfigStore.setState({
    initialized: true,
    repos: [
      {
        repoPath: "/old/repo",
        repoName: "repo",
        contextRepoPath: "/old/context",
        teamContentPath: "",
        hasRemote: false,
        remoteUrl: null,
        mainDoc: "",
      },
    ],
  });
  let saved!: () => void;
  save.mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        saved = resolve;
      }),
  );
  const approval = useConfigStore.getState().recordSetupApproval("/old/repo", "hash");
  await vi.waitFor(() => expect(save).toHaveBeenCalledOnce());
  vi.stubGlobal("window", { location: { reload } });
  try {
    const selecting = useLocalProfileStore.getState().select("84");
    await Promise.resolve();
    expect(invoke).not.toHaveBeenCalledWith("select_local_profile", expect.anything());
    expect(flushOrg).not.toHaveBeenCalled();
    saved();
    await approval;
    await selecting;
    const persisted = JSON.parse(save.mock.calls[0][0] as string);
    expect(persisted.repos[0].repoPath).toBe("/old/repo");
    expect(persisted.worktreeSetupApprovals["/old/repo::hash"].configHash).toBe("hash");
    expect(persisted.viewer).toEqual({ selected: "old" });
    expect(invoke).toHaveBeenCalledWith("select_local_profile", {
      principal: "84",
      displayName: undefined,
    });
    expect(reload).toHaveBeenCalledOnce();
  } finally {
    vi.unstubAllGlobals();
  }
});
it("aborts profile retirement on a pending config failure and retains the rollback", async () => {
  const { useConfigStore } = await import("./config-store");
  const { useLocalProfileStore } = await import("./local-profile-store");
  await useLocalProfileStore.getState().initialize();
  let reject!: (error: Error) => void;
  save.mockImplementationOnce(
    () =>
      new Promise<void>((_, fail) => {
        reject = fail;
      }),
  );
  const approval = useConfigStore
    .getState()
    .recordSetupApproval("/old/repo", "hash")
    .catch((error) => error);
  await vi.waitFor(() => expect(save).toHaveBeenCalledOnce());
  vi.stubGlobal("window", { location: { reload } });
  try {
    const selecting = useLocalProfileStore.getState().select("84");
    reject(new Error("config disk full"));
    await approval;
    await selecting;
    expect(useLocalProfileStore.getState().error).toContain("config disk full");
    expect(useLocalProfileStore.getState().status?.active?.principal).toBe("42");
    expect(useConfigStore.getState().isSetupApproved("/old/repo", "hash")).toBe(false);
    expect(invoke).not.toHaveBeenCalledWith("select_local_profile", expect.anything());
    expect(reload).not.toHaveBeenCalled();
    expect(flushOrg).not.toHaveBeenCalled();
  } finally {
    vi.unstubAllGlobals();
  }
});

it("a failed legacy mutation cannot be discarded by simply retrying profile selection", async () => {
  const { useConfigStore } = await import("./config-store");
  const { useLocalProfileStore } = await import("./local-profile-store");
  await useLocalProfileStore.getState().initialize();
  useConfigStore.setState({
    initialized: true,
    repos: [
      {
        repoPath: "/old/repo",
        repoName: "repo",
        contextRepoPath: "/old/context",
        teamContentPath: "",
        hasRemote: false,
        remoteUrl: null,
        mainDoc: "",
      },
    ],
  });
  save.mockRejectedValueOnce(new Error("disk full"));
  await useConfigStore.getState().updateRepoRemote("/old/repo", true, "ssh://example/repo");
  vi.stubGlobal("window", { location: { reload } });
  try {
    await useLocalProfileStore.getState().select("84");
    expect(invoke).not.toHaveBeenCalledWith("select_local_profile", expect.anything());
    let saved!: () => void;
    save.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          saved = resolve;
        }),
    );
    const retry = useLocalProfileStore.getState().select("84");
    await vi.waitFor(() => expect(save).toHaveBeenCalledTimes(2));
    expect(invoke).not.toHaveBeenCalledWith("select_local_profile", expect.anything());
    expect(JSON.parse(save.mock.calls[1][0] as string).repos[0].remoteUrl).toBe(
      "ssh://example/repo",
    );
    saved();
    await retry;
    expect(invoke).toHaveBeenCalledWith("select_local_profile", expect.anything());
  } finally {
    vi.unstubAllGlobals();
  }
});
