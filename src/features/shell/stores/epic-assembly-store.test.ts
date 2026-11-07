// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";

import type { DevEnvRun } from "@/tauri/commands";

import {
  deriveEpicBatchStatus,
  formatEpicAssemblyProgress,
  formatEpicPartialFailure,
  useEpicAssemblyStore,
} from "./epic-assembly-store";

const CWD = "/epic/dir";
const runs: DevEnvRun[] = [
  { owner: "acme", repo: "api", runId: "epic-e-acme-api" },
  { owner: "acme", repo: "web", runId: "epic-e-acme-web" },
];

/** Build + return an entry via the store actions (the internal entry type isn't
 *  exported, so we round-trip through the store to derive against real shapes). */
function entry(cwd = CWD) {
  return useEpicAssemblyStore.getState().byCwd[cwd];
}

beforeEach(() => {
  useEpicAssemblyStore.setState({ byCwd: {}, runIdToCwd: {}, pendingByRunId: {} });
});

describe("epic-assembly-store actions", () => {
  it("beginEpicAssembly creates a fresh un-seeded entry (and RESETS on reopen)", () => {
    const s = useEpicAssemblyStore.getState();
    s.beginEpicAssembly(CWD);
    expect(entry()).toEqual({ installs: {}, createFailures: [], seeded: false });

    // Seed then re-begin → fully reset (close+reopen starts clean).
    s.seedEpicAssembly(CWD, runs, []);
    expect(Object.keys(entry().installs)).toHaveLength(2);
    s.beginEpicAssembly(CWD);
    expect(entry()).toEqual({ installs: {}, createFailures: [], seeded: false });
    expect(useEpicAssemblyStore.getState().runIdToCwd).toEqual({});
  });

  it("seedEpicAssembly records installs (running) + createFailures + run_id routing", () => {
    const s = useEpicAssemblyStore.getState();
    s.beginEpicAssembly(CWD);
    s.seedEpicAssembly(CWD, runs, [{ owner: "acme", repo: "cli", error: "no local clone" }]);
    expect(entry().seeded).toBe(true);
    expect(entry().installs["epic-e-acme-api"]).toEqual({
      owner: "acme",
      repo: "api",
      state: "running",
    });
    expect(entry().createFailures).toEqual([
      { owner: "acme", repo: "cli", error: "no local clone" },
    ]);
    expect(useEpicAssemblyStore.getState().runIdToCwd["epic-e-acme-web"]).toBe(CWD);
  });

  it("markEpicInstall advances state, returns the cwd, and is idempotent", () => {
    const s = useEpicAssemblyStore.getState();
    s.beginEpicAssembly(CWD);
    s.seedEpicAssembly(CWD, runs, []);
    expect(s.markEpicInstall("epic-e-acme-api", true)).toBe(CWD);
    expect(entry().installs["epic-e-acme-api"].state).toBe("done");

    // A duplicate completion for an already-terminal run is a no-op.
    const before = entry();
    s.markEpicInstall("epic-e-acme-api", false);
    expect(entry()).toBe(before);

    // An unknown run_id routes nowhere.
    expect(s.markEpicInstall("not-a-run", true)).toBeNull();
  });

  it("buffers a completion that races ahead of the seed, then drains it on seed", () => {
    const s = useEpicAssemblyStore.getState();
    s.beginEpicAssembly(CWD);
    // Completion arrives BEFORE seed (run_id not yet routed) → buffered, no route.
    expect(s.markEpicInstall("epic-e-acme-api", true)).toBeNull();
    expect(useEpicAssemblyStore.getState().pendingByRunId["epic-e-acme-api"]).toBe(true);
    // Seed drains the buffered completion → that install is already done.
    s.seedEpicAssembly(CWD, runs, []);
    expect(entry().installs["epic-e-acme-api"].state).toBe("done");
    expect(entry().installs["epic-e-acme-web"].state).toBe("running");
    expect(useEpicAssemblyStore.getState().pendingByRunId["epic-e-acme-api"]).toBeUndefined();
  });

  it("drains a buffered FAILURE too", () => {
    const s = useEpicAssemblyStore.getState();
    s.beginEpicAssembly(CWD);
    s.markEpicInstall("epic-e-acme-web", false);
    s.seedEpicAssembly(CWD, runs, []);
    expect(entry().installs["epic-e-acme-web"].state).toBe("failed");
  });

  it("does NOT buffer a non-epic run_id (guards the shared channel)", () => {
    const s = useEpicAssemblyStore.getState();
    expect(s.markEpicInstall("story-run-123", true)).toBeNull();
    expect(useEpicAssemblyStore.getState().pendingByRunId).toEqual({});
  });

  it("review F1: a fresh seed does NOT inherit a stale prior-attempt completion (unique ids)", () => {
    const s = useEpicAssemblyStore.getState();
    s.beginEpicAssembly(CWD);
    // A prior attempt's install (seq 1) finishes late and is buffered.
    s.markEpicInstall("epic-e-acme-api-1", false);
    // The reopen seeds a FRESH run (seq 2) — it must stay "running", NOT inherit
    // the stale "failed" from the prior attempt's id.
    s.seedEpicAssembly(CWD, [{ owner: "acme", repo: "api", runId: "epic-e-acme-api-2" }], []);
    expect(entry().installs["epic-e-acme-api-2"].state).toBe("running");
  });

  it("review F3: beginEpicAssembly prunes a prior batch's ids from the buffer", () => {
    const s = useEpicAssemblyStore.getState();
    s.beginEpicAssembly(CWD);
    s.seedEpicAssembly(CWD, [{ owner: "acme", repo: "api", runId: "epic-e-acme-api-1" }], []);
    // Simulate a stale buffered completion for this batch's id, then re-begin.
    useEpicAssemblyStore.setState({ pendingByRunId: { "epic-e-acme-api-1": true } });
    s.beginEpicAssembly(CWD);
    expect(useEpicAssemblyStore.getState().pendingByRunId["epic-e-acme-api-1"]).toBeUndefined();
  });
});

describe("deriveEpicBatchStatus", () => {
  function build(seed: () => void) {
    const s = useEpicAssemblyStore.getState();
    s.beginEpicAssembly(CWD);
    seed();
    return entry();
  }

  it("all installs done without context inheritance → settled, clean", () => {
    const e = build(() => {
      const s = useEpicAssemblyStore.getState();
      s.seedEpicAssembly(CWD, runs, []);
      s.markEpicInstall("epic-e-acme-api", true);
      s.markEpicInstall("epic-e-acme-web", true);
    });
    const st = deriveEpicBatchStatus(e);
    expect(st).toMatchObject({
      total: 2,
      ready: 2,
      allInstallsTerminal: true,
      settled: true,
      totalFailure: false,
      partialFailure: false,
    });
    expect(st.failures).toEqual([]);
  });

  it("settles completed setup without any context wave", () => {
    const e = build(() => {
      const s = useEpicAssemblyStore.getState();
      s.seedEpicAssembly(CWD, runs, []);
      s.markEpicInstall("epic-e-acme-api", true);
      s.markEpicInstall("epic-e-acme-web", true);
    });
    expect(deriveEpicBatchStatus(e).settled).toBe(true);
  });

  it("stays unsettled while an install is running (long npm ci)", () => {
    const e = build(() => {
      const s = useEpicAssemblyStore.getState();
      s.seedEpicAssembly(CWD, runs, []);
      s.markEpicInstall("epic-e-acme-api", true);
      // web still running
    });
    const st = deriveEpicBatchStatus(e);
    expect(st.allInstallsTerminal).toBe(false);
    expect(st.settled).toBe(false);
    expect(st.installingRepos).toEqual(["acme/web"]);
  });

  it("not seeded → never settled", () => {
    const e = build(() => {});
    expect(deriveEpicBatchStatus(e).settled).toBe(false);
  });

  it("partial failure (one install failed) → settled + usable + flagged", () => {
    const e = build(() => {
      const s = useEpicAssemblyStore.getState();
      s.seedEpicAssembly(CWD, runs, []);
      s.markEpicInstall("epic-e-acme-api", true);
      s.markEpicInstall("epic-e-acme-web", false);
    });
    const st = deriveEpicBatchStatus(e);
    expect(st.settled).toBe(true);
    expect(st.totalFailure).toBe(false);
    expect(st.partialFailure).toBe(true);
    expect(st.ready).toBe(1);
    expect(st.failures).toEqual([{ owner: "acme", repo: "web" }]);
  });

  it("create failure folds into the batch failures", () => {
    const e = build(() => {
      const s = useEpicAssemblyStore.getState();
      s.seedEpicAssembly(CWD, runs, [{ owner: "acme", repo: "cli", error: "no local clone" }]);
      s.markEpicInstall("epic-e-acme-api", true);
      s.markEpicInstall("epic-e-acme-web", true);
    });
    const st = deriveEpicBatchStatus(e);
    expect(st.partialFailure).toBe(true);
    expect(st.failures).toContainEqual({ owner: "acme", repo: "cli", error: "no local clone" });
  });

  it("assemble threw → total failure (settled, in-pane error)", () => {
    const e = build(() => {
      useEpicAssemblyStore.getState().markEpicAssemblyError(CWD, "backend blew up");
    });
    const st = deriveEpicBatchStatus(e);
    expect(st.totalFailure).toBe(true);
    expect(st.settled).toBe(true);
    expect(st.assemblyError).toBe("backend blew up");
  });

  it("a late assembly report supersedes the provisional no-report failure", () => {
    const e = build(() => {
      const s = useEpicAssemblyStore.getState();
      s.markEpicAssemblyError(CWD, "never reported");
      s.seedEpicAssembly(CWD, [], []);
    });
    const st = deriveEpicBatchStatus(e);
    expect(st.totalFailure).toBe(false);
    expect(st.assemblyError).toBeUndefined();
    expect(st.settled).toBe(true);
  });

  it("draft epic (no repos) → settled after assembly, no context wave needed", () => {
    const e = build(() => {
      useEpicAssemblyStore.getState().seedEpicAssembly(CWD, [], []);
    });
    const st = deriveEpicBatchStatus(e);
    expect(st.total).toBe(0);
    expect(st.settled).toBe(true);
    expect(st.partialFailure).toBe(false);
  });
});

describe("formatters", () => {
  const base = {
    seeded: true,
    total: 3,
    ready: 1,
    installingRepos: ["acme/web"],
    failures: [],
    allInstallsTerminal: false,

    settled: false,
    totalFailure: false,
    partialFailure: false,
  };

  it("formatEpicAssemblyProgress shows X of M + current install", () => {
    expect(formatEpicAssemblyProgress(base)).toBe(
      "Assembling 3 repos — 1 of 3 ready · installing acme/web…",
    );
    expect(formatEpicAssemblyProgress({ ...base, installingRepos: [] })).toBe(
      "Assembling 3 repos — 1 of 3 ready",
    );
    expect(formatEpicAssemblyProgress({ ...base, total: 0 })).toBe("Setting up epic…");
  });

  it("formatEpicPartialFailure lists the failed repos", () => {
    expect(
      formatEpicPartialFailure({
        ...base,
        ready: 2,
        failures: [{ owner: "acme", repo: "api" }],
      }),
    ).toBe("2 repos ready · acme/api couldn't set up — reopen to retry");
  });
});
