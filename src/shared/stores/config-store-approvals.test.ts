// SPDX-License-Identifier: MIT
// SW-WT-9: config-store worktree setup-approval slice.
import { describe, it, expect, beforeEach, vi } from "vitest";
import { useConfigStore, setupApprovalKey } from "./config-store";

const mockLoadAppState = vi.fn().mockResolvedValue("{}");
const mockSaveAppState = vi.fn().mockResolvedValue(undefined);

vi.mock("@/tauri/commands", () => ({
  saveAppState: (s: string) => mockSaveAppState(s),
  loadAppState: () => mockLoadAppState(),
  createContextRepo: vi.fn(),
  loadWorkspaceConfig: vi.fn(),
  saveWorkspaceConfig: vi.fn(),
}));

describe("config-store — setup approvals (SW-WT-9)", () => {
  beforeEach(() => {
    mockLoadAppState.mockReset().mockResolvedValue("{}");
    mockSaveAppState.mockReset().mockResolvedValue(undefined);
    useConfigStore.setState({ initialized: false, repos: [], worktreeSetupApprovals: {} });
  });

  it("is not approved before recording", () => {
    expect(useConfigStore.getState().isSetupApproved("/repo", "h1")).toBe(false);
  });

  it("records an approval and reports it approved", async () => {
    await useConfigStore.getState().recordSetupApproval("/repo", "h1");
    expect(useConfigStore.getState().isSetupApproved("/repo", "h1")).toBe(true);
    // A different content hash for the same repo is NOT approved (AC-3).
    expect(useConfigStore.getState().isSetupApproved("/repo", "h2")).toBe(false);
  });

  it("persists approvals via app-state RMW, preserving other keys", async () => {
    mockLoadAppState.mockResolvedValue(JSON.stringify({ viewer: { tab: 1 }, repos: [] }));
    await useConfigStore.getState().recordSetupApproval("/repo", "h1");
    expect(mockSaveAppState).toHaveBeenCalledTimes(1);
    const written = JSON.parse(mockSaveAppState.mock.calls[0][0]);
    expect(written.viewer).toEqual({ tab: 1 }); // untouched
    expect(written.worktreeSetupApprovals[setupApprovalKey("/repo", "h1")].configHash).toBe("h1");
  });

  it("loadState hydrates approvals and drops malformed entries", async () => {
    mockLoadAppState.mockResolvedValue(
      JSON.stringify({
        repos: [],
        worktreeSetupApprovals: {
          good: { configHash: "h1", approvedAtMs: 123 },
          bad: { configHash: 5 },
          alsoBad: "nope",
        },
      }),
    );
    await useConfigStore.getState().loadState();
    const approvals = useConfigStore.getState().worktreeSetupApprovals;
    expect(approvals.good).toEqual({ configHash: "h1", approvedAtMs: 123 });
    expect(approvals.bad).toBeUndefined();
    expect(approvals.alsoBad).toBeUndefined();
  });

  it("is idempotent — re-recording the same hash does not write twice", async () => {
    await useConfigStore.getState().recordSetupApproval("/repo", "h1");
    expect(mockSaveAppState).toHaveBeenCalledTimes(1);
    await useConfigStore.getState().recordSetupApproval("/repo", "h1");
    expect(mockSaveAppState).toHaveBeenCalledTimes(1); // no second write
  });
});
