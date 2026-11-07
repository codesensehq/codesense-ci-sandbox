// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
const { load, save } = vi.hoisted(() => ({ load: vi.fn(), save: vi.fn() }));
vi.mock("@/tauri/commands", () => ({
  loadAppState: load,
  saveAppState: save,
  createContextRepo: vi.fn(),
  loadWorkspaceConfig: vi.fn(),
  saveWorkspaceConfig: vi.fn(),
}));
beforeEach(() => {
  vi.resetModules();
  load.mockReset();
  save.mockReset();
});
it("an in-flight org write cannot overwrite a concurrent config approval at switch drain", async () => {
  const { useOrgStore, flushOrgForProfileSwitch } = await import("./org-store");
  const { useConfigStore, flushConfigForProfileSwitch } = await import("./config-store");
  let disk = '{"viewer":{"tab":"keep"}}';
  load.mockImplementation(async () => disk);
  let release!: () => void;
  save
    .mockImplementationOnce(async (value: string) => {
      await new Promise<void>((resolve) => {
        release = resolve;
      });
      disk = value;
    })
    .mockImplementation(async (value: string) => {
      disk = value;
    });
  useOrgStore.setState({
    stateLoaded: true,
    activeOrgSlug: "owner/context",
    activeTeamName: "main",
  });
  const orgWrite = flushOrgForProfileSwitch();
  await vi.waitFor(() => expect(save).toHaveBeenCalledOnce());
  const approval = useConfigStore.getState().recordSetupApproval("/repo", "hash");
  const drain = flushConfigForProfileSwitch();
  await Promise.resolve();
  await Promise.resolve();
  expect(load).toHaveBeenCalledTimes(1);
  release();
  await orgWrite;
  await approval;
  await drain;
  const result = JSON.parse(disk);
  expect(result.org).toMatchObject({ activeOrgSlug: "owner/context", activeTeamName: "main" });
  expect(result.worktreeSetupApprovals["/repo::hash"].configHash).toBe("hash");
  expect(result.viewer).toEqual({ tab: "keep" });
});
