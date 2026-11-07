// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { openEpicTerminal, type OpenEpicTerminalDeps } from "./open-epic-terminal";
import type {
  EnsureEpicWorkspaceParams,
  AssembleEpicResult,
  EpicWorkspaceHandle,
} from "@/tauri/commands";

const EPIC_FILE = "/ctx/epic-workspaces/SW-EW.md";
const BASE: EnsureEpicWorkspaceParams = {
  codesenseHome: "/home/.codesense",
  githubUserId: 42,
  orgSlug: "acme",
  team: "default",
  epicFilePath: EPIC_FILE,
};
const DIR = "/home/.codesense/_epic/SW-EW";

const emptyResult: AssembleEpicResult = {
  assembled: [],
  failures: [],
  branch: "epic/SW-EW",
  devEnvRuns: [],
};

function makeDeps(over: Partial<OpenEpicTerminalDeps> = {}): OpenEpicTerminalDeps {
  return {
    ensureEpicWorkspace: vi.fn().mockResolvedValue({
      slug: "_epic/SW-EW",
      dirPath: DIR,
      registered: true,
    }),
    assembleEpicWorktrees: vi.fn().mockResolvedValue(emptyResult),
    inFlight: new Set<string>(),
    isAlreadyOpen: vi.fn().mockReturnValue(false),
    focusExisting: vi.fn(),
    armEpicOpen: vi.fn(),
    cancelEpicOpen: vi.fn(),
    // Default: the armed placeholder is still present when the materialize resolves.
    isPlaceholderPresent: vi.fn().mockReturnValue(true),
    openIntegration: vi.fn(),
    onAssemblyFailed: vi.fn(),
    onAssemblyResolved: vi.fn(),
    onOpenError: vi.fn(),
    beginEpicAssembly: vi.fn(),
    seedEpicAssembly: vi.fn(),
    markEpicAssemblyError: vi.fn(),
    ...over,
  };
}

// SLW-CO-6 (AC-5/AC-6): the epic path is unified onto the background-materialize
// model — no setup toast. Readiness rides the SW-TS-3 chip (EpicInheritance
// wave); only per-repo assembly FAILURES surface explicitly (onto the chip via
// `onAssemblyFailed`), and pre-open hard failures via `onOpenError`.
describe("openEpicTerminal", () => {
  beforeEach(() => vi.clearAllMocks());

  it("happy path: ensures, opens the terminal (born ready), assembles, no failure surfaced, releases guard", async () => {
    const deps = makeDeps();
    await openEpicTerminal(BASE, deps);

    expect(deps.ensureEpicWorkspace).toHaveBeenCalledWith(BASE);
    // SW-EW-15: the epic doc path is threaded onto the integration record so
    // repo read/write resolves from the record (not the empty nav tree).
    expect(deps.openIntegration).toHaveBeenCalledWith(DIR, EPIC_FILE);
    expect(deps.assembleEpicWorktrees).toHaveBeenCalledWith(BASE);
    expect(deps.onAssemblyFailed).not.toHaveBeenCalled();
    // Clean assembly clears any stale chip failure from a prior open.
    expect(deps.onAssemblyResolved).toHaveBeenCalledWith(DIR);
    expect(deps.onOpenError).not.toHaveBeenCalled();
    expect(deps.inFlight.has(BASE.epicFilePath)).toBe(false);
    // SLW-CO-8: the batch begins (overlay up) then seeds with the result.
    expect(deps.beginEpicAssembly).toHaveBeenCalledWith(DIR);
    expect(deps.seedEpicAssembly).toHaveBeenCalledWith(DIR, emptyResult);
    expect(deps.markEpicAssemblyError).not.toHaveBeenCalled();
  });

  it("SLW-CO-8: begins the batch BEFORE assembling, seeds AFTER the result", async () => {
    const order: string[] = [];
    const deps = makeDeps({
      beginEpicAssembly: vi.fn(() => order.push("begin")),
      assembleEpicWorktrees: vi.fn(() => {
        order.push("assemble");
        return Promise.resolve(emptyResult);
      }),
      seedEpicAssembly: vi.fn(() => order.push("seed")),
    });
    await openEpicTerminal(BASE, deps);
    expect(order).toEqual(["begin", "assemble", "seed"]);
  });

  it("opens the terminal BEFORE assembly (non-blocking — terminal usable immediately)", async () => {
    const order: string[] = [];
    const deps = makeDeps({
      openIntegration: vi.fn(() => order.push("open")),
      assembleEpicWorktrees: vi.fn(() => {
        order.push("assemble");
        return Promise.resolve(emptyResult);
      }),
    });
    await openEpicTerminal(BASE, deps);
    expect(order).toEqual(["open", "assemble"]);
  });

  // SLW-FX-27 Part B (AC-6/AC-7): the epic setup overlay must be armed
  // SYNCHRONOUSLY at open — BEFORE the (~1-2s) `ensureEpicWorkspace` materialize
  // resolves — so an epic shows its "setting up…" affordance immediately, exactly
  // like a story workspace. Before this story, arming happened only AFTER the
  // await (via `openIntegration`/`beginEpicAssembly`), leaving a bare shell.
  it("SLW-FX-27 Part B: arms the setup overlay BEFORE ensureEpicWorkspace resolves", async () => {
    let resolveEnsure!: (h: EpicWorkspaceHandle) => void;
    const ensureEpicWorkspace = vi.fn(
      () =>
        new Promise<EpicWorkspaceHandle>((res) => {
          resolveEnsure = res;
        }),
    );
    const deps = makeDeps({ ensureEpicWorkspace });
    const p = openEpicTerminal(BASE, deps);
    // Flush microtasks up to the pending `await ensureEpicWorkspace`.
    await Promise.resolve();
    await Promise.resolve();
    // Armed already — the overlay is up while the materialize is still pending.
    expect(deps.armEpicOpen).toHaveBeenCalledOnce();
    // The REAL (ready) open still waits for the resolved dir.
    expect(deps.openIntegration).not.toHaveBeenCalled();
    expect(deps.cancelEpicOpen).not.toHaveBeenCalled();

    resolveEnsure({ slug: "_epic/SW-EW", dirPath: DIR, registered: true });
    await p;
    // Now the placeholder reconciles to the ready row at the resolved dir.
    expect(deps.openIntegration).toHaveBeenCalledWith(DIR, EPIC_FILE);
    expect(deps.cancelEpicOpen).not.toHaveBeenCalled();
  });

  it("SLW-FX-27 Part B: arm fires before ensure, before open", async () => {
    const order: string[] = [];
    const deps = makeDeps({
      armEpicOpen: vi.fn(() => order.push("arm")),
      ensureEpicWorkspace: vi.fn(() => {
        order.push("ensure");
        return Promise.resolve({ slug: "_epic/SW-EW", dirPath: DIR, registered: true });
      }),
      openIntegration: vi.fn(() => order.push("open")),
    });
    await openEpicTerminal(BASE, deps);
    expect(order).toEqual(["arm", "ensure", "open"]);
  });

  it("already open: focuses only, skips ensure + assemble + arm", async () => {
    const deps = makeDeps({ isAlreadyOpen: vi.fn().mockReturnValue(true) });
    await openEpicTerminal(BASE, deps);

    expect(deps.focusExisting).toHaveBeenCalledOnce();
    expect(deps.ensureEpicWorkspace).not.toHaveBeenCalled();
    expect(deps.assembleEpicWorktrees).not.toHaveBeenCalled();
    expect(deps.openIntegration).not.toHaveBeenCalled();
    // No placeholder created on the focus-only path → nothing to arm or cancel.
    expect(deps.armEpicOpen).not.toHaveBeenCalled();
    expect(deps.cancelEpicOpen).not.toHaveBeenCalled();
  });

  it("SLW-FX-27 Part B: concurrent re-fire does not re-arm (guarded)", async () => {
    const inFlight = new Set<string>([BASE.epicFilePath]);
    const deps = makeDeps({ inFlight });
    await openEpicTerminal(BASE, deps);
    expect(deps.armEpicOpen).not.toHaveBeenCalled();
  });

  // SLW-FX-27 Part B (code-review R1): the user can close the "creating"
  // placeholder while the materialize is in flight. On resolve, the open must NOT
  // resurrect the dismissed tile (the phantom-write race closeWorkspace warns of).
  it("SLW-FX-27 Part B: placeholder closed mid-materialize ⇒ does not resurrect", async () => {
    const deps = makeDeps({ isPlaceholderPresent: vi.fn().mockReturnValue(false) });
    await openEpicTerminal(BASE, deps);
    // Armed, then the user closed it → resolved open is abandoned.
    expect(deps.armEpicOpen).toHaveBeenCalledOnce();
    expect(deps.openIntegration).not.toHaveBeenCalled();
    expect(deps.assembleEpicWorktrees).not.toHaveBeenCalled();
    expect(deps.beginEpicAssembly).not.toHaveBeenCalled();
    // The row is already gone — nothing to cancel, no error toast.
    expect(deps.cancelEpicOpen).not.toHaveBeenCalled();
    expect(deps.onOpenError).not.toHaveBeenCalled();
    // Guard still released for a later re-open.
    expect(deps.inFlight.has(BASE.epicFilePath)).toBe(false);
  });

  it("concurrent re-fire (same epic in flight): no-op", async () => {
    const inFlight = new Set<string>([BASE.epicFilePath]);
    const deps = makeDeps({ inFlight });
    await openEpicTerminal(BASE, deps);

    expect(deps.ensureEpicWorkspace).not.toHaveBeenCalled();
    // The pre-existing flag is left for the in-flight call to clear.
    expect(inFlight.has(BASE.epicFilePath)).toBe(true);
  });

  it("empty dirPath: surfaces an open error, opens nothing, never assembles, releases guard", async () => {
    const deps = makeDeps({
      ensureEpicWorkspace: vi.fn().mockResolvedValue({ slug: "x", dirPath: "", registered: false }),
    });
    await openEpicTerminal(BASE, deps);

    expect(deps.onOpenError).toHaveBeenCalledWith(
      "Couldn't open epic terminal: workspace dir unavailable.",
    );
    expect(deps.openIntegration).not.toHaveBeenCalled();
    expect(deps.assembleEpicWorktrees).not.toHaveBeenCalled();
    expect(deps.onAssemblyFailed).not.toHaveBeenCalled();
    expect(deps.inFlight.has(BASE.epicFilePath)).toBe(false);
    // SLW-FX-27 Part B: the early-armed placeholder must be torn down — a hard
    // pre-open failure must never strand a stuck "creating" epic tile.
    expect(deps.cancelEpicOpen).toHaveBeenCalledOnce();
  });

  it("draft epic (empty repos): opens, assembles a clean no-op, resolves (clears any stale failure)", async () => {
    const deps = makeDeps();
    await openEpicTerminal(BASE, deps);
    expect(deps.openIntegration).toHaveBeenCalledOnce();
    expect(deps.onAssemblyFailed).not.toHaveBeenCalled();
    expect(deps.onAssemblyResolved).toHaveBeenCalledWith(DIR);
    expect(deps.onOpenError).not.toHaveBeenCalled();
  });

  it("partial assembly failure: surfaces owner/repo labels on the chip, terminal already opened", async () => {
    const deps = makeDeps({
      assembleEpicWorktrees: vi.fn().mockResolvedValue({
        assembled: [
          {
            owner: "acme",
            repo: "core",
            path: "/p",
            branch: "epic/SW-EW",
            createdBranch: false,
            alreadyExisted: false,
            warnings: [],
          },
        ],
        failures: [
          { owner: "acme", repo: "web", error: "boom" },
          { owner: "acme", repo: "api", error: "boom" },
        ],
        branch: "epic/SW-EW",
        devEnvRuns: [],
      } satisfies AssembleEpicResult),
    });
    await openEpicTerminal(BASE, deps);

    expect(deps.openIntegration).toHaveBeenCalledOnce();
    expect(deps.onAssemblyFailed).toHaveBeenCalledWith(
      DIR,
      "2 repos couldn't set up: acme/web, acme/api",
    );
    // A failure must NOT also clear (that would race the failure green).
    expect(deps.onAssemblyResolved).not.toHaveBeenCalled();
    expect(deps.onOpenError).not.toHaveBeenCalled();
    expect(deps.inFlight.has(BASE.epicFilePath)).toBe(false);
  });

  it("single assembly failure: singular copy", async () => {
    const deps = makeDeps({
      assembleEpicWorktrees: vi.fn().mockResolvedValue({
        assembled: [],
        failures: [{ owner: "acme", repo: "web", error: "boom" }],
        branch: "epic/SW-EW",
        devEnvRuns: [],
      } satisfies AssembleEpicResult),
    });
    await openEpicTerminal(BASE, deps);
    expect(deps.onAssemblyFailed).toHaveBeenCalledWith(DIR, "1 repo couldn't set up: acme/web");
  });

  it("assembly throws: terminal stays open, failure surfaced on the chip, guard released", async () => {
    const deps = makeDeps({
      assembleEpicWorktrees: vi.fn().mockRejectedValue(new Error("nope")),
    });
    await openEpicTerminal(BASE, deps);

    expect(deps.openIntegration).toHaveBeenCalledOnce();
    expect(deps.onAssemblyFailed).toHaveBeenCalledWith(DIR, "Repos couldn't set up: nope");
    expect(deps.onAssemblyResolved).not.toHaveBeenCalled();
    expect(deps.inFlight.has(BASE.epicFilePath)).toBe(false);
    // SLW-CO-8: a total assembly throw marks the batch a total failure (in-pane
    // error), and never seeds (no result).
    expect(deps.markEpicAssemblyError).toHaveBeenCalledWith(DIR, "nope");
    expect(deps.seedEpicAssembly).not.toHaveBeenCalled();
  });

  it("ensure throws: open error surfaced, nothing opened, guard released", async () => {
    const deps = makeDeps({
      ensureEpicWorkspace: vi.fn().mockRejectedValue(new Error("offline")),
    });
    await openEpicTerminal(BASE, deps);

    expect(deps.onOpenError).toHaveBeenCalledWith("Couldn't open epic terminal: offline");
    expect(deps.openIntegration).not.toHaveBeenCalled();
    expect(deps.assembleEpicWorktrees).not.toHaveBeenCalled();
    expect(deps.inFlight.has(BASE.epicFilePath)).toBe(false);
    // SLW-FX-27 Part B: ensure-threw must tear down the early-armed placeholder.
    expect(deps.cancelEpicOpen).toHaveBeenCalledOnce();
  });

  it("guard is released so a later open of the same epic proceeds", async () => {
    const deps = makeDeps();
    await openEpicTerminal(BASE, deps);
    await openEpicTerminal(BASE, deps);
    expect(deps.ensureEpicWorkspace).toHaveBeenCalledTimes(2);
  });
});
