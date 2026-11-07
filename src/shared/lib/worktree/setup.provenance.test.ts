// SPDX-License-Identifier: MIT
// SW-WT-9: the team-sync provenance gate inside setupWorktree.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { setupWorktree } from "./setup";
import type { WorktreeSetupApprovalHooks } from "./types";

vi.mock("@/tauri/commands", () => ({
  fetchAndCreateWorktree: vi.fn(),
  removeWorktree: vi.fn().mockResolvedValue(undefined),
  listWorktrees: vi.fn().mockResolvedValue([]),
  runWorktreeSetup: vi.fn(),
  cancelWorktreeRun: vi.fn().mockResolvedValue(undefined),
  getWorktreeConfig: vi.fn(),
  evaluateSetupProvenance: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

import {
  fetchAndCreateWorktree,
  removeWorktree,
  runWorktreeSetup,
  getWorktreeConfig,
  evaluateSetupProvenance,
} from "@/tauri/commands";

const fetchAndCreateMock = fetchAndCreateWorktree as unknown as ReturnType<typeof vi.fn>;
const removeWorktreeMock = removeWorktree as unknown as ReturnType<typeof vi.fn>;
const runWorktreeSetupMock = runWorktreeSetup as unknown as ReturnType<typeof vi.fn>;
const getWorktreeConfigMock = getWorktreeConfig as unknown as ReturnType<typeof vi.fn>;
const evaluateProvenanceMock = evaluateSetupProvenance as unknown as ReturnType<typeof vi.fn>;

const config = {
  version: 1 as const,
  setup: ["npm ci", "cargo build && ./scripts/bootstrap.sh"],
};

const okResult = { results: [], exit_code: 0 };
const HASH = "deadbeef";

function makeApproval(overrides: Partial<WorktreeSetupApprovalHooks> = {}): {
  hooks: WorktreeSetupApprovalHooks;
  prompt: ReturnType<typeof vi.fn>;
  record: ReturnType<typeof vi.fn>;
  isApproved: ReturnType<typeof vi.fn>;
} {
  const prompt = vi.fn().mockResolvedValue(true);
  const record = vi.fn().mockResolvedValue(undefined);
  const isApproved = vi.fn().mockReturnValue(false);
  return {
    hooks: { prompt, record, isApproved, ...overrides },
    prompt,
    record,
    isApproved,
  };
}

function baseOptions(setupApproval?: WorktreeSetupApprovalHooks) {
  return {
    repoPath: "/repo",
    orgRoot: "/org",
    branchName: "feat/x",
    worktreePath: "/wt/x",
    startPoint: "main",
    runId: "run-1",
    setupApproval,
  };
}

beforeEach(() => {
  fetchAndCreateMock.mockReset().mockResolvedValue({ worktree_path: "/wt/x", from_remote: false });
  removeWorktreeMock.mockReset().mockResolvedValue(undefined);
  runWorktreeSetupMock.mockReset().mockResolvedValue(okResult);
  getWorktreeConfigMock.mockReset().mockResolvedValue(config);
  evaluateProvenanceMock.mockReset();
});

describe("setupWorktree — provenance gate", () => {
  it("team-synced + unapproved: prompts, records, runs setup with the approved hash", async () => {
    evaluateProvenanceMock.mockResolvedValue({ provenance: "team_synced", config_hash: HASH });
    const { hooks, prompt, record } = makeApproval();

    const result = await setupWorktree(baseOptions(hooks));

    expect(prompt).toHaveBeenCalledWith({ steps: config.setup, configHash: HASH });
    expect(record).toHaveBeenCalledWith("/repo", HASH);
    // 6th arg is the approved hash (team/stalenessThreshold removed — SW-WT-1).
    expect(runWorktreeSetupMock).toHaveBeenCalledWith(
      "/wt/x",
      "/repo",
      config,
      "run-1",
      "/org",
      HASH,
    );
    expect(result.setupSkipped).toBeUndefined();
  });

  it("team-synced + declined: keeps setup retryable and the worktree intact", async () => {
    evaluateProvenanceMock.mockResolvedValue({ provenance: "team_synced", config_hash: HASH });
    const { hooks, record } = makeApproval({ prompt: vi.fn().mockResolvedValue(false) });

    await expect(setupWorktree(baseOptions(hooks))).rejects.toThrow("Setup approval is required");
    expect(runWorktreeSetupMock).not.toHaveBeenCalled();
    expect(removeWorktreeMock).not.toHaveBeenCalled();
    expect(record).not.toHaveBeenCalled();
  });

  it("decline then approved retry completes actual setup", async () => {
    evaluateProvenanceMock.mockResolvedValue({ provenance: "team_synced", config_hash: HASH });
    const prompt = vi.fn().mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    const { hooks } = makeApproval({ prompt });
    await expect(setupWorktree(baseOptions(hooks))).rejects.toMatchObject({
      failedStep: "setup",
      recoverable: true,
    });
    expect(runWorktreeSetupMock).not.toHaveBeenCalled();
    await expect(setupWorktree(baseOptions(hooks))).resolves.toMatchObject({
      worktreePath: "/wt/x",
    });
    expect(prompt).toHaveBeenCalledTimes(2);
    expect(runWorktreeSetupMock).toHaveBeenCalledTimes(1);
    expect(removeWorktreeMock).not.toHaveBeenCalled();
  });

  it("team-synced + already approved: runs without prompting", async () => {
    evaluateProvenanceMock.mockResolvedValue({ provenance: "team_synced", config_hash: HASH });
    const { hooks, prompt, record } = makeApproval({ isApproved: vi.fn().mockReturnValue(true) });

    await setupWorktree(baseOptions(hooks));

    expect(prompt).not.toHaveBeenCalled();
    expect(record).not.toHaveBeenCalled();
    expect(runWorktreeSetupMock).toHaveBeenCalledWith(
      "/wt/x",
      "/repo",
      config,
      "run-1",
      "/org",
      HASH,
    );
  });

  it("local provenance: no prompt, runs with no approved hash", async () => {
    evaluateProvenanceMock.mockResolvedValue({ provenance: "local", config_hash: null });
    const { hooks, prompt } = makeApproval();

    await setupWorktree(baseOptions(hooks));

    expect(prompt).not.toHaveBeenCalled();
    const call = runWorktreeSetupMock.mock.calls[0];
    expect(call[5]).toBeUndefined();
  });

  it("provenance read failure: proceeds ungated rather than blocking", async () => {
    evaluateProvenanceMock.mockRejectedValue(new Error("git boom"));
    const { hooks, prompt } = makeApproval();

    await setupWorktree(baseOptions(hooks));

    expect(prompt).not.toHaveBeenCalled();
    expect(runWorktreeSetupMock).toHaveBeenCalled();
  });

  it("no approval hooks: gate is inert (back-compat), backend remains the backstop", async () => {
    evaluateProvenanceMock.mockResolvedValue({ provenance: "team_synced", config_hash: HASH });

    await setupWorktree(baseOptions(undefined));

    expect(evaluateProvenanceMock).not.toHaveBeenCalled();
    expect(runWorktreeSetupMock).toHaveBeenCalled();
  });

  it("backend NEEDS_APPROVAL re-fetches the live config, re-prompts with the live hash, retries", async () => {
    // FE gated as local (no initial prompt), but the backend backstop refuses
    // with a live hash — the config changed, so re-fetch it and preview the
    // CURRENT commands (AC-2), then re-prompt and retry.
    evaluateProvenanceMock.mockResolvedValue({ provenance: "local", config_hash: null });
    const liveConfig = { ...config, setup: ["npm ci", "echo updated-by-teammate"] };
    // 1st read = initial load (stale); 2nd read = backstop re-fetch (live).
    getWorktreeConfigMock.mockReset().mockResolvedValueOnce(config).mockResolvedValue(liveConfig);
    runWorktreeSetupMock
      .mockReset()
      .mockRejectedValueOnce("NEEDS_APPROVAL:h2")
      .mockResolvedValue(okResult);
    const { hooks, prompt, record } = makeApproval();

    const result = await setupWorktree(baseOptions(hooks));

    // Preview shows the LIVE steps, not the stale ones captured at start.
    expect(prompt).toHaveBeenCalledWith({ steps: liveConfig.setup, configHash: "h2" });
    expect(record).toHaveBeenCalledWith("/repo", "h2");
    expect(runWorktreeSetupMock).toHaveBeenCalledTimes(2);
    expect(runWorktreeSetupMock.mock.calls[1][2]).toEqual(liveConfig);
    expect(runWorktreeSetupMock.mock.calls[1][5]).toBe("h2");
    expect(result.setupSkipped).toBeUndefined();
    expect(removeWorktreeMock).not.toHaveBeenCalled();
  });

  it("backend NEEDS_APPROVAL + decline keeps setup retryable and the worktree intact", async () => {
    evaluateProvenanceMock.mockResolvedValue({ provenance: "local", config_hash: null });
    runWorktreeSetupMock.mockReset().mockRejectedValue("NEEDS_APPROVAL:h2");
    const { hooks } = makeApproval({ prompt: vi.fn().mockResolvedValue(false) });

    await expect(setupWorktree(baseOptions(hooks))).rejects.toThrow("Setup approval is required");
    expect(removeWorktreeMock).not.toHaveBeenCalled();
  });

  it("NEEDS_APPROVAL without approval hooks surfaces the refusal (does not silently run)", async () => {
    runWorktreeSetupMock.mockReset().mockRejectedValue("NEEDS_APPROVAL:h2");
    await expect(setupWorktree(baseOptions(undefined))).rejects.toBe("NEEDS_APPROVAL:h2");
  });

  it("malformed NEEDS_APPROVAL with no hash surfaces the error, never re-prompts with an empty hash", async () => {
    evaluateProvenanceMock.mockResolvedValue({ provenance: "local", config_hash: null });
    runWorktreeSetupMock.mockReset().mockRejectedValue("NEEDS_APPROVAL:");
    const { hooks, prompt } = makeApproval();
    await expect(setupWorktree(baseOptions(hooks))).rejects.toBe("NEEDS_APPROVAL:");
    expect(prompt).not.toHaveBeenCalled();
  });
});
