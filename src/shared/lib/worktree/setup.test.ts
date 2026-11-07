// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  MAX_SETUP_DIAGNOSTIC_STREAM_CHARS,
  buildSetupFailureDiagnostics,
  setupWorktree,
  createWorktree,
  runWorktreeSetupPhase,
} from "./setup";
import type { WorktreeProgressEvent } from "./types";

vi.mock("@/tauri/commands", () => ({
  fetchAndCreateWorktree: vi.fn(),
  removeWorktree: vi.fn().mockResolvedValue(undefined),
  listWorktrees: vi.fn().mockResolvedValue([]),
  runWorktreeSetup: vi.fn(),
  cancelWorktreeRun: vi.fn().mockResolvedValue(undefined),
  getWorktreeConfig: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

import {
  fetchAndCreateWorktree,
  removeWorktree,
  listWorktrees,
  runWorktreeSetup,
  cancelWorktreeRun,
  getWorktreeConfig,
} from "@/tauri/commands";
import { listen } from "@tauri-apps/api/event";

const listenMock = listen as unknown as ReturnType<typeof vi.fn>;

const fetchAndCreateMock = fetchAndCreateWorktree as unknown as ReturnType<typeof vi.fn>;
const removeWorktreeMock = removeWorktree as unknown as ReturnType<typeof vi.fn>;
const listWorktreesMock = listWorktrees as unknown as ReturnType<typeof vi.fn>;
const runWorktreeSetupMock = runWorktreeSetup as unknown as ReturnType<typeof vi.fn>;
const cancelWorktreeRunMock = cancelWorktreeRun as unknown as ReturnType<typeof vi.fn>;
const getWorktreeConfigMock = getWorktreeConfig as unknown as ReturnType<typeof vi.fn>;

const baseConfig = {
  version: 1 as const,
  setup: ["npm install"],
};

const okResult = {
  results: [
    {
      command: "npm install",
      passed: true,
      stdout: "",
      stderr: "",
      exit_code: 0,
      phase: "setup" as const,
      step_index: 0,
    },
  ],
  exit_code: 0,
};

describe("buildSetupFailureDiagnostics", () => {
  it("selects the failed result and preserves exit-code ownership", () => {
    const failed = {
      command: "pnpm install --frozen-lockfile",
      passed: false,
      stdout: "out\n",
      stderr: "ERR_PNPM_NO_LOCKFILE\n",
      exit_code: 1,
      phase: "setup" as const,
      step_index: 2,
    };
    expect(buildSetupFailureDiagnostics([okResult.results[0], failed], 17)).toEqual({
      overallExitCode: 17,
      failedResult: failed,
      streams: {
        stdout: { originalLength: 4, truncated: false },
        stderr: { originalLength: 21, truncated: false },
      },
    });
  });

  it("bounds oversized streams with deterministic surrogate-safe head and tail", () => {
    const stdout = `😀${"a".repeat(MAX_SETUP_DIAGNOSTIC_STREAM_CHARS)}TAIL😀`;
    const diagnostics = buildSetupFailureDiagnostics(
      [
        {
          command: "big-output",
          passed: false,
          stdout,
          stderr: stdout,
          exit_code: null,
          phase: "setup",
          step_index: 0,
        },
      ],
      -1,
    );
    expect(diagnostics?.failedResult.stdout.length).toBeLessThanOrEqual(
      MAX_SETUP_DIAGNOSTIC_STREAM_CHARS,
    );
    expect(diagnostics?.failedResult.stdout).toContain("output truncated");
    expect(diagnostics?.failedResult.stdout.endsWith("TAIL😀")).toBe(true);
    expect(diagnostics?.streams.stdout).toEqual({
      originalLength: stdout.length,
      truncated: true,
    });
    expect(diagnostics?.failedResult.exit_code).toBeNull();
  });

  it("preserves exact head/tail allocation without splitting surrogate pairs", () => {
    const marker = "\n… output truncated …\n";
    const contentBudget = MAX_SETUP_DIAGNOSTIC_STREAM_CHARS - marker.length;
    const headLength = Math.floor(contentBudget / 4);
    const tailLength = contentBudget - headLength;
    const stdoutLength = MAX_SETUP_DIAGNOSTIC_STREAM_CHARS + 100;
    const tailStart = stdoutLength - tailLength;
    const stdoutCodeUnits = new Array<string>(stdoutLength).fill("m");
    stdoutCodeUnits[headLength - 1] = "\uD83D";
    stdoutCodeUnits[headLength] = "\uDE00";
    stdoutCodeUnits[tailStart - 1] = "\uD83D";
    stdoutCodeUnits[tailStart] = "\uDE00";
    const stdout = stdoutCodeUnits.join("");
    const diagnostics = buildSetupFailureDiagnostics(
      [
        {
          command: "boundary-output",
          passed: false,
          stdout,
          stderr: stdout,
          exit_code: 1,
          phase: "setup",
          step_index: 0,
        },
      ],
      1,
    );
    expect(diagnostics?.failedResult.stdout).toBe(
      `${"m".repeat(headLength - 1)}${marker}${"m".repeat(tailLength - 1)}`,
    );
    expect(diagnostics?.failedResult.stderr).toBe(diagnostics?.failedResult.stdout);
    expect(diagnostics?.failedResult.stdout).not.toMatch(/[\uD800-\uDFFF]/);
    expect(diagnostics?.failedResult.stderr).not.toMatch(/[\uD800-\uDFFF]/);
    expect(diagnostics?.streams.stdout).toEqual({
      originalLength: stdout.length,
      truncated: true,
    });
    expect(diagnostics?.streams.stderr).toEqual(diagnostics?.streams.stdout);
  });

  it("preserves deterministic empty-stream metadata", () => {
    const diagnostics = buildSetupFailureDiagnostics(
      [
        {
          command: "quiet-failure",
          passed: false,
          stdout: "",
          stderr: "",
          exit_code: null,
          phase: "setup",
          step_index: 0,
        },
      ],
      -1,
    );
    expect(diagnostics?.streams).toEqual({
      stdout: { originalLength: 0, truncated: false },
      stderr: { originalLength: 0, truncated: false },
    });
  });

  it("returns undefined when the runner supplies no failed result", () => {
    expect(buildSetupFailureDiagnostics(okResult.results, 1)).toBeUndefined();
  });
});

beforeEach(() => {
  fetchAndCreateMock.mockReset();
  removeWorktreeMock.mockReset().mockResolvedValue(undefined);
  listWorktreesMock.mockReset().mockResolvedValue([]);
  runWorktreeSetupMock.mockReset();
  cancelWorktreeRunMock.mockReset().mockResolvedValue(undefined);
  getWorktreeConfigMock.mockReset();
});

describe("setupWorktree — happy path", () => {
  it("creates the worktree, runs setup, resolves with results", async () => {
    fetchAndCreateMock.mockResolvedValue({ worktree_path: "/wt/30-10", from_remote: false });
    getWorktreeConfigMock.mockResolvedValue(baseConfig);
    runWorktreeSetupMock.mockResolvedValue(okResult);

    const events: WorktreeProgressEvent[] = [];
    const result = await setupWorktree({
      repoPath: "/repo",
      orgRoot: "",
      branchName: "feat/30-10",
      worktreePath: "/wt/30-10",
      startPoint: "main",
      runId: "run-1",
      onProgress: (e) => events.push(e),
    });

    expect(fetchAndCreateMock).toHaveBeenCalledWith(
      "/repo",
      "feat/30-10",
      "/wt/30-10",
      "main",
      null,
    );
    // SW-WT-1: worktree projection moved off this command onto the
    // WorktreeDerive wave, so `team` / `stalenessThreshold` are no longer
    // passed — orgRoot then the trailing approved-config-hash (undefined when
    // no gate hooks are supplied, as here).
    expect(runWorktreeSetupMock).toHaveBeenCalledWith(
      "/wt/30-10",
      "/repo",
      baseConfig,
      "run-1",
      "",
      undefined,
    );
    expect(result).toEqual({
      worktreePath: "/wt/30-10",
      results: okResult.results,
    });
    // Streamed events are output-only — no synthesized completion event.
    expect(events.every((e) => e.phase !== undefined)).toBe(true);
  });
});

describe("setupWorktree — rehydrate", () => {
  it("returns an empty-results result when an existing worktree at the path matches the branch", async () => {
    listWorktreesMock.mockResolvedValue([
      { path: "/wt/30-10", head: "abc", branch: "refs/heads/feat/30-10" },
    ]);

    const result = await setupWorktree({
      repoPath: "/repo",
      orgRoot: "",
      branchName: "feat/30-10",
      worktreePath: "/wt/30-10",
      startPoint: "main",
      runId: "run-2",
    });

    expect(result).toEqual({ worktreePath: "/wt/30-10", results: [] });
    expect(fetchAndCreateMock).not.toHaveBeenCalled();
    expect(runWorktreeSetupMock).not.toHaveBeenCalled();
  });

  it("throws an 'orphan' error when an existing worktree tracks a different branch", async () => {
    listWorktreesMock.mockResolvedValue([
      { path: "/wt/30-10", head: "abc", branch: "refs/heads/feat/wrong" },
    ]);

    await expect(
      setupWorktree({
        repoPath: "/repo",
        orgRoot: "",
        branchName: "feat/30-10",
        worktreePath: "/wt/30-10",
        startPoint: "main",
        runId: "run-3",
      }),
    ).rejects.toMatchObject({ failedStep: "orphan", recoverable: true });
  });
});

describe("setupWorktree — failure paths", () => {
  it("rolls back the worktree when setup fails non-zero", async () => {
    fetchAndCreateMock.mockResolvedValue({ worktree_path: "/wt/30-10", from_remote: false });
    getWorktreeConfigMock.mockResolvedValue(baseConfig);
    runWorktreeSetupMock.mockResolvedValue({
      results: [
        {
          command: "npm install",
          passed: false,
          stdout: "",
          stderr: "boom",
          exit_code: 1,
          phase: "setup" as const,
          step_index: 0,
        },
      ],
      exit_code: 1,
    });

    await expect(
      setupWorktree({
        repoPath: "/repo",
        orgRoot: "",
        branchName: "feat/30-10",
        worktreePath: "/wt/30-10",
        startPoint: "main",
        runId: "run-4",
      }),
    ).rejects.toMatchObject({
      failedStep: "setup",
      recoverable: true,
      diagnostics: {
        overallExitCode: 1,
        failedResult: expect.objectContaining({ command: "npm install", stderr: "boom" }),
      },
    });

    expect(removeWorktreeMock).toHaveBeenCalledWith("/repo", "/wt/30-10", false);
  });

  it("SW-WT-1: keeps the worktree and skips setup when worktree.yaml is missing (no rollback, no throw)", async () => {
    fetchAndCreateMock.mockResolvedValue({ worktree_path: "/wt/30-10", from_remote: false });
    getWorktreeConfigMock.mockResolvedValue(null);
    runWorktreeSetupMock.mockResolvedValue({ results: [], exit_code: 0 });

    const result = await setupWorktree({
      repoPath: "/repo",
      orgRoot: "",
      branchName: "feat/30-10",
      worktreePath: "/wt/30-10",
      startPoint: "main",
      runId: "run-5",
    });

    // AC-6: a missing config is a clean, non-blocking state — the worktree
    // stays (materialization rides the WorktreeDerive wave), setup is a no-op.
    expect(result).toEqual({ worktreePath: "/wt/30-10", results: [], setupSkipped: true });
    expect(removeWorktreeMock).not.toHaveBeenCalled();
    expect(runWorktreeSetupMock).toHaveBeenCalledWith(
      "/wt/30-10",
      "/repo",
      { version: 1, setup: [] },
      "run-5",
      "",
      undefined,
    );
  });

  it("classifies create failures as 'create' with no rollback (nothing to undo)", async () => {
    fetchAndCreateMock.mockRejectedValue("fatal: branch exists");

    await expect(
      setupWorktree({
        repoPath: "/repo",
        orgRoot: "",
        branchName: "feat/30-10",
        worktreePath: "/wt/30-10",
        startPoint: "main",
        runId: "run-6",
      }),
    ).rejects.toMatchObject({ failedStep: "create", recoverable: false });

    expect(removeWorktreeMock).not.toHaveBeenCalled();
  });
});

describe("setupWorktree — cancellation", () => {
  it("rejects with 'cancelled' before doing anything when the signal is already aborted", async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(
      setupWorktree({
        repoPath: "/repo",
        orgRoot: "",
        branchName: "feat/30-10",
        worktreePath: "/wt/30-10",
        startPoint: "main",
        runId: "run-7",
        signal: controller.signal,
      }),
    ).rejects.toMatchObject({ failedStep: "cancelled", recoverable: true });
  });

  it("signals cancelWorktreeRun on abort during setup, rolls back the worktree", async () => {
    fetchAndCreateMock.mockResolvedValue({ worktree_path: "/wt/30-10", from_remote: false });
    getWorktreeConfigMock.mockResolvedValue(baseConfig);

    const controller = new AbortController();
    runWorktreeSetupMock.mockImplementation(async () => {
      controller.abort();
      return {
        results: [
          {
            command: "npm install",
            passed: false,
            stdout: "",
            stderr: "cancelled",
            exit_code: 130,
            phase: "setup" as const,
            step_index: 0,
          },
        ],
        exit_code: 130,
      };
    });

    await expect(
      setupWorktree({
        repoPath: "/repo",
        orgRoot: "",
        branchName: "feat/30-10",
        worktreePath: "/wt/30-10",
        startPoint: "main",
        runId: "run-8",
        signal: controller.signal,
      }),
    ).rejects.toMatchObject({ failedStep: "cancelled", recoverable: true });

    expect(cancelWorktreeRunMock).toHaveBeenCalledWith("run-8");
    expect(removeWorktreeMock).toHaveBeenCalledWith("/repo", "/wt/30-10", false);
  });
});

// RF-perf-14 decouple — the two phases setupWorktree now composes.

// #2840: a story opened with no configured base sends no start point, so the backend
// starts the new branch from the repo's default branch (origin/HEAD) rather than an
// assumed `main`, and its refusal reaches the user unchanged. A configured base goes to
// the backend as `defaultStartPoint`, which prefers its cached `origin/` ref (as the MCP
// open does), never as a verbatim start point.
describe("createWorktree — the repo's default branch (#2840)", () => {
  it("#2840: with no configured base, sends no start point and no default", async () => {
    fetchAndCreateMock.mockResolvedValue({ worktree_path: "/wt/zz", from_remote: false });
    const result = await createWorktree({
      repoPath: "/repo",
      branchName: "zz-open-probe",
      worktreePath: "/wt/zz",
      startPoint: null,
    });
    expect(result).toEqual({ worktreePath: "/wt/zz", rehydrated: false });
    expect(fetchAndCreateMock).toHaveBeenCalledWith("/repo", "zz-open-probe", "/wt/zz", null, null);
  });

  it.each(["develop", "master"])(
    "#2840: a configured default of %s goes to the backend to resolve, not as the start point",
    async (configured) => {
      fetchAndCreateMock.mockResolvedValue({ worktree_path: "/wt/zz", from_remote: false });
      await createWorktree({
        repoPath: "/repo",
        branchName: "zz-open-probe",
        worktreePath: "/wt/zz",
        startPoint: null,
        defaultStartPoint: configured,
      });
      expect(fetchAndCreateMock).toHaveBeenCalledWith(
        "/repo",
        "zz-open-probe",
        "/wt/zz",
        null,
        configured,
      );
    },
  );

  it("#2840: the backend's refusal is surfaced unchanged", async () => {
    const refusal =
      "unavailable: can't start a new story branch in /repo: no default branch found. Tried refs/remotes/origin/HEAD (not set), refs/remotes/origin/main, refs/heads/main.";
    fetchAndCreateMock.mockRejectedValue(refusal);
    const error = await createWorktree({
      repoPath: "/repo",
      branchName: "zz-open-probe",
      worktreePath: "/wt/zz",
      startPoint: null,
    }).catch((e: unknown) => e);
    expect(error).toMatchObject({ failedStep: "create", message: refusal });
  });

  it("an explicit start point (a dependency branch or override) reaches the backend verbatim", async () => {
    fetchAndCreateMock.mockResolvedValue({ worktree_path: "/wt/zz", from_remote: false });
    await createWorktree({
      repoPath: "/repo",
      branchName: "zz-open-probe",
      worktreePath: "/wt/zz",
      startPoint: "feat/SV-1-dep",
      defaultStartPoint: "develop",
    });
    expect(fetchAndCreateMock).toHaveBeenCalledWith(
      "/repo",
      "zz-open-probe",
      "/wt/zz",
      "feat/SV-1-dep",
      "develop",
    );
  });
});

describe("createWorktree (RF-perf-14 Phase A)", () => {
  it("creates the worktree and returns rehydrated:false (no setup run)", async () => {
    fetchAndCreateMock.mockResolvedValue({ worktree_path: "/wt/a", from_remote: false });
    const result = await createWorktree({
      repoPath: "/repo",
      branchName: "feat/a",
      worktreePath: "/wt/a",
      startPoint: "main",
    });
    expect(result).toEqual({ worktreePath: "/wt/a", rehydrated: false });
    expect(fetchAndCreateMock).toHaveBeenCalledWith("/repo", "feat/a", "/wt/a", "main", null);
    // Phase A NEVER runs setup commands.
    expect(runWorktreeSetupMock).not.toHaveBeenCalled();
  });

  it("returns rehydrated:true (no git create) when a matching worktree already exists", async () => {
    listWorktreesMock.mockResolvedValue([
      { path: "/wt/a", head: "abc", branch: "refs/heads/feat/a" },
    ]);
    const result = await createWorktree({
      repoPath: "/repo",
      branchName: "feat/a",
      worktreePath: "/wt/a",
      startPoint: "main",
    });
    expect(result).toEqual({ worktreePath: "/wt/a", rehydrated: true });
    expect(fetchAndCreateMock).not.toHaveBeenCalled();
  });

  it("throws 'orphan' when an existing worktree tracks a different branch", async () => {
    listWorktreesMock.mockResolvedValue([
      { path: "/wt/a", head: "abc", branch: "refs/heads/feat/wrong" },
    ]);
    await expect(
      createWorktree({
        repoPath: "/repo",
        branchName: "feat/a",
        worktreePath: "/wt/a",
        startPoint: "main",
      }),
    ).rejects.toMatchObject({ failedStep: "orphan", recoverable: true });
  });

  it("classifies a git failure as 'create' with no rollback", async () => {
    fetchAndCreateMock.mockRejectedValue("fatal: branch exists");
    await expect(
      createWorktree({
        repoPath: "/repo",
        branchName: "feat/a",
        worktreePath: "/wt/a",
        startPoint: "main",
      }),
    ).rejects.toMatchObject({ failedStep: "create", recoverable: false });
    expect(removeWorktreeMock).not.toHaveBeenCalled();
  });

  it("rejects 'cancelled' before doing anything when already aborted", async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(
      createWorktree({
        repoPath: "/repo",
        branchName: "feat/a",
        worktreePath: "/wt/a",
        startPoint: "main",
        signal: controller.signal,
      }),
    ).rejects.toMatchObject({ failedStep: "cancelled", recoverable: true });
    expect(fetchAndCreateMock).not.toHaveBeenCalled();
  });

  it("removes the fresh dir + rejects 'cancelled' when aborted during git create", async () => {
    const controller = new AbortController();
    fetchAndCreateMock.mockImplementation(async () => {
      controller.abort();
      return { worktree_path: "/wt/a", from_remote: false };
    });
    await expect(
      createWorktree({
        repoPath: "/repo",
        branchName: "feat/a",
        worktreePath: "/wt/a",
        startPoint: "main",
        signal: controller.signal,
      }),
    ).rejects.toMatchObject({ failedStep: "cancelled", recoverable: true });
    expect(removeWorktreeMock).toHaveBeenCalledWith("/repo", "/wt/a", false);
  });
});

describe("runWorktreeSetupPhase (RF-perf-14 Phase B) — keepWorktreeOnFailure", () => {
  it("does NOT create the worktree (assumes Phase A already did)", async () => {
    getWorktreeConfigMock.mockResolvedValue(baseConfig);
    runWorktreeSetupMock.mockResolvedValue(okResult);
    await runWorktreeSetupPhase({
      repoPath: "/repo",
      orgRoot: "",
      branchName: "feat/a",
      worktreePath: "/wt/a",
      startPoint: "main",
      runId: "run-b1",
    });
    expect(fetchAndCreateMock).not.toHaveBeenCalled();
    expect(runWorktreeSetupMock).toHaveBeenCalled();
  });

  it("keepWorktreeOnFailure=true: a setup failure throws but does NOT roll the worktree back", async () => {
    getWorktreeConfigMock.mockResolvedValue(baseConfig);
    runWorktreeSetupMock.mockResolvedValue({
      results: [
        {
          command: "npm install",
          passed: false,
          stdout: "",
          stderr: "boom",
          exit_code: 1,
          phase: "setup" as const,
          step_index: 0,
        },
      ],
      exit_code: 1,
    });
    await expect(
      runWorktreeSetupPhase({
        repoPath: "/repo",
        orgRoot: "",
        branchName: "feat/a",
        worktreePath: "/wt/a",
        startPoint: "main",
        runId: "run-b2",
        keepWorktreeOnFailure: true,
      }),
    ).rejects.toMatchObject({ failedStep: "setup", recoverable: true });
    // The decouple contract: a backgrounded setup failure leaves the usable worktree alone.
    expect(removeWorktreeMock).not.toHaveBeenCalled();
  });

  it("keepWorktreeOnFailure=true: a mid-setup cancel keeps the worktree", async () => {
    getWorktreeConfigMock.mockResolvedValue(baseConfig);
    const controller = new AbortController();
    runWorktreeSetupMock.mockImplementation(async () => {
      controller.abort();
      return {
        results: [
          {
            command: "npm install",
            passed: false,
            stdout: "",
            stderr: "cancelled",
            exit_code: 130,
            phase: "setup" as const,
            step_index: 0,
          },
        ],
        exit_code: 130,
      };
    });
    await expect(
      runWorktreeSetupPhase({
        repoPath: "/repo",
        orgRoot: "",
        branchName: "feat/a",
        worktreePath: "/wt/a",
        startPoint: "main",
        runId: "run-b3",
        signal: controller.signal,
        keepWorktreeOnFailure: true,
      }),
    ).rejects.toMatchObject({ failedStep: "cancelled", recoverable: true });
    expect(removeWorktreeMock).not.toHaveBeenCalled();
  });

  it("bails 'cancelled' WITHOUT spawning setup when the signal is already aborted (closed while queued)", async () => {
    getWorktreeConfigMock.mockResolvedValue(baseConfig);
    const controller = new AbortController();
    controller.abort(); // e.g. the workspace closed while this setup waited behind the gate
    await expect(
      runWorktreeSetupPhase({
        repoPath: "/repo",
        orgRoot: "",
        branchName: "feat/a",
        worktreePath: "/wt/a",
        startPoint: "main",
        runId: "run-b5",
        signal: controller.signal,
        keepWorktreeOnFailure: true,
      }),
    ).rejects.toMatchObject({ failedStep: "cancelled", recoverable: true });
    // The setup subprocess never spawned, and the usable worktree is kept.
    expect(runWorktreeSetupMock).not.toHaveBeenCalled();
    expect(removeWorktreeMock).not.toHaveBeenCalled();
  });

  it("default (keepWorktreeOnFailure=false): a setup failure DOES roll the worktree back (parity)", async () => {
    getWorktreeConfigMock.mockResolvedValue(baseConfig);
    runWorktreeSetupMock.mockResolvedValue({
      results: [
        {
          command: "npm install",
          passed: false,
          stdout: "",
          stderr: "boom",
          exit_code: 1,
          phase: "setup" as const,
          step_index: 0,
        },
      ],
      exit_code: 1,
    });
    await expect(
      runWorktreeSetupPhase({
        repoPath: "/repo",
        orgRoot: "",
        branchName: "feat/a",
        worktreePath: "/wt/a",
        startPoint: "main",
        runId: "run-b4",
      }),
    ).rejects.toMatchObject({ failedStep: "setup", recoverable: true });
    expect(removeWorktreeMock).toHaveBeenCalledWith("/repo", "/wt/a", false);
  });
});

describe("runWorktreeSetupPhase — SW-WT-12 step events (S2)", () => {
  it("routes step-started/step-finished to onStep and lines to onProgress", async () => {
    let handler: ((e: { payload: unknown }) => void) | null = null;
    listenMock
      .mockReset()
      .mockImplementation((_channel: string, h: (e: { payload: unknown }) => void) => {
        handler = h;
        return Promise.resolve(() => {});
      });
    getWorktreeConfigMock.mockResolvedValue(baseConfig);
    runWorktreeSetupMock.mockResolvedValue(okResult);

    const steps: unknown[] = [];
    const lines: WorktreeProgressEvent[] = [];
    await runWorktreeSetupPhase({
      repoPath: "/repo",
      orgRoot: "",
      branchName: "feat/x",
      worktreePath: "/wt/x",
      startPoint: "main",
      runId: "run-steps",
      onProgress: (e) => lines.push(e),
      onStep: (e) => steps.push(e),
    });

    expect(handler).not.toBeNull();
    // A step-started payload maps to a `started` step event with the command.
    handler!({
      payload: { event: "step-started", phase: "setup", step_index: 0, command: "npm install" },
    });
    // A step-finished payload maps to a `finished` step with passed + exit code.
    handler!({
      payload: {
        event: "step-finished",
        phase: "setup",
        step_index: 0,
        passed: true,
        exit_code: 0,
      },
    });
    // A plain line payload (no `event`) still routes to onProgress, not onStep.
    handler!({
      payload: { stream: "stdout", line: "installing…", phase: "setup", step_index: 0, ts_ms: 1 },
    });

    expect(steps).toEqual([
      {
        kind: "started",
        phase: "setup",
        stepIndex: 0,
        command: "npm install",
        passed: undefined,
        // Normalized absent → null on ingest (matches a signal-killed child's None).
        exitCode: null,
      },
      {
        kind: "finished",
        phase: "setup",
        stepIndex: 0,
        command: undefined,
        passed: true,
        exitCode: 0,
      },
    ]);
    expect(lines).toEqual([
      { phase: "setup", stepIndex: 0, stream: "stdout", line: "installing…", tsMs: 1 },
    ]);
  });
});

describe("setupWorktree — SW-WT-12 onCreated hook (S1)", () => {
  it("fires onCreated after create and BEFORE setup (derive-before-setup ordering)", async () => {
    fetchAndCreateMock.mockResolvedValue({ worktree_path: "/wt/oc", from_remote: false });
    getWorktreeConfigMock.mockResolvedValue(baseConfig);
    const order: string[] = [];
    runWorktreeSetupMock.mockImplementation(async () => {
      order.push("setup");
      return okResult;
    });

    await setupWorktree({
      repoPath: "/repo",
      orgRoot: "",
      branchName: "feat/oc",
      worktreePath: "/wt/oc",
      startPoint: "main",
      runId: "run-oc",
      onCreated: () => {
        order.push("created");
      },
    });

    expect(order).toEqual(["created", "setup"]);
  });

  it("fires onCreated even on rehydrate (materialize regardless), skipping setup", async () => {
    listWorktreesMock.mockResolvedValue([{ path: "/wt/rh", branch: "feat/rh" }]);
    const order: string[] = [];
    runWorktreeSetupMock.mockImplementation(async () => {
      order.push("setup");
      return okResult;
    });
    const onCreated = vi.fn(() => {
      order.push("created");
    });

    await setupWorktree({
      repoPath: "/repo",
      orgRoot: "",
      branchName: "feat/rh",
      worktreePath: "/wt/rh",
      startPoint: "main",
      runId: "run-rh",
      onCreated,
    });

    expect(onCreated).toHaveBeenCalledTimes(1);
    expect(order).toEqual(["created"]); // rehydrate → setup replay skipped
  });
});

describe("explicit story setup continuation", () => {
  it("awaits registration and reruns incomplete adopted setup", async () => {
    listWorktreesMock.mockResolvedValue([{ path: "/wt/retry", branch: "refs/heads/feat/retry" }]);
    getWorktreeConfigMock.mockResolvedValue(baseConfig);
    runWorktreeSetupMock.mockResolvedValue(okResult);
    let register!: (complete: boolean) => void;
    const run = setupWorktree({
      repoPath: "/repo",
      orgRoot: "",
      branchName: "feat/retry",
      worktreePath: "/wt/retry",
      startPoint: "main",
      runId: "retry",
      onCreated: () =>
        new Promise<boolean>((resolve) => {
          register = resolve;
        }),
    });
    await vi.waitFor(() => expect(register).toBeDefined());
    expect(runWorktreeSetupMock).not.toHaveBeenCalled();
    register(false);
    await run;
    expect(runWorktreeSetupMock).toHaveBeenCalledOnce();
  });
});
