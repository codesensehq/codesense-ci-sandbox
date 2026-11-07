// SPDX-License-Identifier: MIT
import { isAgentRuntime } from "@/shared/lib/agent-runtimes";
import { prepareAgentLaunch } from "@/app/mock/handlers/agents-launch";
import { describe, it, expect, vi, beforeEach } from "vitest";

// SW-DX-15: unit-test the epic-target resolution the `openWorkspace` verb's
// `epic` arm relies on. Mock the tauri dir-list + active-team snapshot.
const { readWorkspaceFile, resolveEpicPath, getActiveTeamKey, dashboardProbeSessionsAlive } =
  vi.hoisted(() => ({
    readWorkspaceFile: vi.fn(),
    resolveEpicPath: vi.fn(),
    getActiveTeamKey: vi.fn(),
    dashboardProbeSessionsAlive: vi.fn(),
  }));
vi.mock("@/tauri/commands", async (importOriginal) => {
  const { AGENT_PROVIDERS } = await importOriginal<typeof import("@/tauri/commands")>();
  return { readWorkspaceFile, resolveEpicPath, dashboardProbeSessionsAlive, AGENT_PROVIDERS };
});
vi.mock("@/shared/lib/team-key", () => ({
  getActiveTeamKey: () => getActiveTeamKey(),
  parseTeamKey: (key: string) => {
    const i = key.indexOf(":");
    return i < 0
      ? { orgSlug: key, teamName: null }
      : { orgSlug: key.slice(0, i), teamName: key.slice(i + 1) || null };
  },
}));
// SW-DX-16: mock the shared epic opener so `resolveExplicitEpicTarget` tests the
// context-resolve + trusted `resolveEpicPath` wiring without pulling stores/toast.
const { resolveEpicCommandContext, openEpicWorkspaceFor } = vi.hoisted(() => ({
  resolveEpicCommandContext: vi.fn(),
  openEpicWorkspaceFor: vi.fn(),
}));
vi.mock("@/features/viewer/lib/epic-workspace-opener", () => ({
  resolveEpicCommandContext: (scope: unknown) => resolveEpicCommandContext(scope),
  openEpicWorkspaceFor: (t: unknown) => openEpicWorkspaceFor(t),
}));

import {
  isEpicMainDocName,
  resolveOwnEpicTarget,
  resolveExplicitEpicTarget,
  dispatchOpenWorkspace,
  cancelWorkspaceOpen,
  injectReviewPromptSingleFlight,
  reconcileManagedStory,
  makeRealDashboardLaunchDeps,
  resolveWorkspaceStatusForStory,
  storySessionRole,
  STORY_DEV_ROLE,
  type HeadlessSessionLaunchers,
} from "./dashboard-launch-controller";
import type { DashboardLaunchDeps, BranchLaunchDeps, DirLaunchDeps } from "./dashboard-launch";
import { subscribeReviewOpenPhase } from "./review-open-progress";
import { trackWorktreeEstablish } from "@/shared/lib/worktree/pending-establish";

describe("injectReviewPromptSingleFlight", () => {
  it("is sticky single-flight per prompt and retries a definite failed delivery", async () => {
    const inject = vi.fn(async () => undefined);
    await Promise.all([
      injectReviewPromptSingleFlight("dedupe-session", "/review", inject),
      injectReviewPromptSingleFlight("dedupe-session", "/review", inject),
    ]);
    expect(inject).toHaveBeenCalledTimes(1);
    await injectReviewPromptSingleFlight("dedupe-session", "/different", inject);
    expect(inject).toHaveBeenCalledTimes(2);
    await injectReviewPromptSingleFlight("dedupe-session", "/review", inject);
    expect(inject).toHaveBeenCalledTimes(2);

    const retry = vi
      .fn()
      .mockRejectedValueOnce(new Error("not ready"))
      .mockResolvedValue(undefined);
    await expect(injectReviewPromptSingleFlight("retry-session", "/review", retry)).rejects.toThrow(
      "not ready",
    );
    await expect(
      injectReviewPromptSingleFlight("retry-session", "/review", retry),
    ).resolves.toBeUndefined();
    expect(retry).toHaveBeenCalledTimes(2);
  });

  it("times out a stalled delivery without submitting a duplicate retry", async () => {
    vi.useFakeTimers();
    try {
      let release!: () => void;
      const stalled = vi.fn(
        () =>
          new Promise<void>((resolve) => {
            release = resolve;
          }),
      );
      const delivery = injectReviewPromptSingleFlight("stalled-session", "/review", stalled);
      const rejection = expect(delivery).rejects.toThrow("review prompt delivery did not complete");
      await vi.advanceTimersByTimeAsync(15_000);
      await rejection;

      const retry = vi.fn(async () => undefined);
      const retried = injectReviewPromptSingleFlight("stalled-session", "/review", retry);
      const retryRejection = expect(retried).rejects.toThrow("delivery outcome is unknown");
      await retryRejection;
      expect(retry).not.toHaveBeenCalled();
      expect(stalled).toHaveBeenCalledTimes(1);

      release();
      await vi.advanceTimersByTimeAsync(0);
      await expect(
        injectReviewPromptSingleFlight("stalled-session", "/review", retry),
      ).resolves.toBeUndefined();
      expect(retry).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("never evicts unknown deliveries to admit a duplicate", async () => {
    vi.useFakeTimers();
    try {
      const releases: Array<() => void> = [];
      const inject = vi.fn(
        () =>
          new Promise<void>((resolve) => {
            releases.push(resolve);
          }),
      );
      const deliveries = Array.from({ length: 256 }, (_, index) =>
        injectReviewPromptSingleFlight(`unknown-${index}`, "/review", inject),
      );
      const rejections = deliveries.map((delivery) =>
        expect(delivery).rejects.toThrow("review prompt delivery did not complete"),
      );
      await vi.advanceTimersByTimeAsync(15_000);
      await Promise.all(rejections);
      await expect(injectReviewPromptSingleFlight("unknown-0", "/review", inject)).rejects.toThrow(
        "delivery outcome is unknown",
      );
      await expect(
        injectReviewPromptSingleFlight("healthy-new", "/review", inject),
      ).rejects.toThrow("too many review prompt deliveries");
      expect(inject).toHaveBeenCalledTimes(256);
      releases.forEach((release) => release());
      await vi.advanceTimersByTimeAsync(0);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("real story launch deps — establish before the spawn", () => {
  it("awaitEstablished blocks on the story worktree's in-flight establish", async () => {
    const deps = makeRealDashboardLaunchDeps("/ctx/dash.md");
    let release!: () => void;
    trackWorktreeEstablish("/wt/story", new Promise<void>((r) => (release = r)));
    let settled = false;
    const waiting = deps.awaitEstablished!("/wt/story").then(() => {
      settled = true;
    });
    await Promise.resolve();
    expect(settled).toBe(false);
    release();
    await waiting;
    expect(settled).toBe(true);
  });
});

describe("real story launch deps — team context before the agent starts", () => {
  it("awaitContext blocks until repository setup completes", async () => {
    const cwd = "/wt/fresh-story";
    useSurfaceSyncStore.setState({ byPath: {}, assemblyFailure: {} });
    useViewerStore.setState({
      workspaces: {
        "org:team": [
          {
            id: "S-1",
            kind: "story",
            cwd,
            status: "ready",
            backgroundSetup: { status: "running", label: "Setup" },
          },
        ] as unknown as Workspace[],
      },
    });
    const deps = makeRealDashboardLaunchDeps("/ctx/dash.md");
    let settled = false;
    const waiting = deps.awaitContext!(cwd).then(() => {
      settled = true;
    });
    useSurfaceSyncStore.getState().markStarted(cwd, 1);
    await new Promise((r) => setTimeout(r, 0));
    expect(settled).toBe(false);
    useViewerStore.setState({
      workspaces: { "org:team": [{ id: "S-1", kind: "story", cwd, status: "ready" }] },
    });
    await waiting;
    expect(settled).toBe(true);
    useViewerStore.setState({ workspaces: {} });
  });
});

describe("reconcileManagedStory", () => {
  it("skips a stale story without writing a worktree link", async () => {
    const establish = vi.fn();
    await expect(
      reconcileManagedStory("SW-OR-8", "/wt", "acme/app", {
        resolve: vi.fn(async () => {
          throw new Error("unavailable: no managed story with id 'SW-OR-8'");
        }),
        establish,
      }),
    ).resolves.toBe(false);
    expect(establish).not.toHaveBeenCalled();
  });

  it("still skips when SW-CR-9 appends a hint on line 2", async () => {
    // SW-CR-9 (R2): the Rust refusal now carries the hint on line 2, so the
    // prefix check must match on the FIRST line, not the full message.
    const establish = vi.fn();
    await expect(
      reconcileManagedStory("SW-OR-8", "/wt", "acme/app", {
        resolve: vi.fn(async () => {
          throw new Error(
            "unavailable: no managed story with id 'SW-OR-8'\nFrontmatter cache + fallback scan both missed; call refresh_frontmatter_cache(path).",
          );
        }),
        establish,
      }),
    ).resolves.toBe(false);
    expect(establish).not.toHaveBeenCalled();
  });

  it("skips when SW-CR-9's fallback found a candidate that still won't index", async () => {
    // SW-CR-9 (R2): a candidate-on-disk refusal is also "nothing to
    // reconcile from this call site" — treat it the same as unknown id.
    const establish = vi.fn();
    await expect(
      reconcileManagedStory("SW-OR-8", "/wt", "acme/app", {
        resolve: vi.fn(async () => {
          throw new Error(
            "unavailable: story file candidate at /ctx/x.md does not carry story_id 'SW-OR-8'\nVerify its frontmatter has type: story, story_id: SW-OR-8, and source_repo.",
          );
        }),
        establish,
      }),
    ).resolves.toBe(false);
    expect(establish).not.toHaveBeenCalled();
  });

  it("propagates operational story-index failures", async () => {
    await expect(
      reconcileManagedStory("SW-OR-8", "/wt", "acme/app", {
        resolve: vi.fn(async () => {
          throw new Error("permission denied");
        }),
        establish: vi.fn(),
      }),
    ).rejects.toThrow("permission denied");
  });
});
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { useSurfaceSyncStore } from "@/features/shell/stores/surface-sync-store";
import { seedTerminalTabs, installTerminalLayoutHarness } from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();

// SW-GW-6: the load-bearing role builder — a NAMED session must carry a
// `#<session>`-suffixed role or the launcher's `(teamKey, role)` dedup aliases it
// onto the MAIN session's tab and never spawns.
describe("storySessionRole (SW-GW-6)", () => {
  it("AC-3: no session → the pre-GW-6 role, byte-identical", () => {
    expect(storySessionRole("SW-GW-6")).toBe(`${STORY_DEV_ROLE}:SW-GW-6`);
    expect(storySessionRole("SW-GW-6")).toBe("story-dev:SW-GW-6");
  });
  it("named session → a DISTINCT `#<session>`-suffixed role", () => {
    expect(storySessionRole("SW-GW-6", "arch-review")).toBe("story-dev:SW-GW-6#arch-review");
  });
  it("distinct labels → distinct roles (no cross-alias)", () => {
    const a = storySessionRole("SW-GW-6", "arch-review");
    const b = storySessionRole("SW-GW-6", "ux-review");
    const main = storySessionRole("SW-GW-6");
    expect(new Set([a, b, main]).size).toBe(3);
  });
});

describe("isEpicMainDocName", () => {
  it("matches README/index case-insensitively", () => {
    expect(isEpicMainDocName("README.md")).toBe(true);
    expect(isEpicMainDocName("readme.md")).toBe(true);
    expect(isEpicMainDocName("index.md")).toBe(true);
    expect(isEpicMainDocName("Index.html")).toBe(true);
  });
  it("rejects non-main docs + extension-less names", () => {
    expect(isEpicMainDocName("SW-DX-15-story.md")).toBe(false);
    expect(isEpicMainDocName("some-board.html")).toBe(false);
    expect(isEpicMainDocName("readme")).toBe(false);
    expect(isEpicMainDocName(".md")).toBe(false);
  });

  it("requires a pipeline extension (md/html/htm) — mirrors Rust detect_format", () => {
    expect(isEpicMainDocName("README.htm")).toBe(true);
    // Non-pipeline extensions must NOT be mistaken for the epic doc.
    expect(isEpicMainDocName("readme.txt")).toBe(false);
    expect(isEpicMainDocName("index.json")).toBe(false);
    expect(isEpicMainDocName("index.yaml")).toBe(false);
  });
});

describe("resolveOwnEpicTarget", () => {
  const DOC = "/ctx/epics/SW-EW/some-board.html";

  beforeEach(() => {
    vi.clearAllMocks();
    getActiveTeamKey.mockReturnValue("acme:default");
    readWorkspaceFile.mockResolvedValue({
      entries: [
        { name: "some-board.html", isDir: false, size: 10 },
        { name: "README.md", isDir: false, size: 20 },
        { name: "sub", isDir: true, size: 0 },
      ],
    });
  });

  it("resolves the board's own epic to its canonical mainDoc target", async () => {
    expect(await resolveOwnEpicTarget(DOC)).toEqual({
      teamKey: "acme:default",
      orgSlug: "acme",
      team: "default",
      epicDirName: "SW-EW",
      epicFilePath: "/ctx/epics/SW-EW/README.md",
    });
  });

  it("picks the mainDoc by the scanner's precedence, not directory order", async () => {
    // Both index.md and readme.md present: the scanner picks readme.md (priority),
    // so the resolver MUST too — else the integration id diverges (double-open).
    readWorkspaceFile.mockResolvedValue({
      entries: [
        { name: "index.md", isDir: false, size: 5 },
        { name: "readme.md", isDir: false, size: 5 },
        { name: "readme.html", isDir: false, size: 5 },
      ],
    });
    const t = await resolveOwnEpicTarget(DOC);
    expect(t?.epicFilePath).toBe("/ctx/epics/SW-EW/readme.md");
  });

  it("returns null when there is no active team", async () => {
    getActiveTeamKey.mockReturnValue(null);
    expect(await resolveOwnEpicTarget(DOC)).toBeNull();
  });

  it("returns null when the dir has no README/index (not an epic dir)", async () => {
    readWorkspaceFile.mockResolvedValue({
      entries: [{ name: "some-board.html", isDir: false, size: 10 }],
    });
    expect(await resolveOwnEpicTarget(DOC)).toBeNull();
  });

  it("returns null when the dir list fails", async () => {
    readWorkspaceFile.mockRejectedValue(new Error("nope"));
    expect(await resolveOwnEpicTarget(DOC)).toBeNull();
  });
});

// SW-DX-16: the EXPLICIT `{team, epic}` resolver — the foreign-epic sibling that
// resolves the mainDoc TRUSTED-SIDE (not the doc-scoped dir list).
describe("resolveExplicitEpicTarget", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    resolveEpicCommandContext.mockReturnValue({
      codesenseHome: "/home/.codesense",
      githubUserId: 42,
      orgSlug: "acme",
      team: "default",
    });
    resolveEpicPath.mockResolvedValue({
      epicFilePath: "/ctx/acme/default/content/epics/x/README.md",
      epicDirName: "x",
    });
  });

  it("resolves {team, epic} to an EpicOpenTarget via the trusted resolver", async () => {
    const t = await resolveExplicitEpicTarget("acme:default", "epics/x");
    expect(resolveEpicCommandContext).toHaveBeenCalledWith({ orgSlug: "acme", team: "default" });
    expect(resolveEpicPath).toHaveBeenCalledWith(
      "/home/.codesense",
      42,
      "acme",
      "default",
      "epics/x",
      undefined,
      undefined,
    );
    expect(t).toEqual({
      teamKey: "acme:default",
      orgSlug: "acme",
      team: "default",
      epicDirName: "x",
      epicFilePath: "/ctx/acme/default/content/epics/x/README.md",
    });
  });

  it("rejects unavailable when the team/org context can't be resolved", async () => {
    resolveEpicCommandContext.mockReturnValue(null);
    await expect(resolveExplicitEpicTarget("nope:team", "epics/x")).rejects.toThrow(
      /unavailable: unknown or unresolved team/,
    );
    expect(resolveEpicPath).not.toHaveBeenCalled();
  });

  it("propagates the trusted resolver's typed reject (traversal/unmanaged/unavailable)", async () => {
    resolveEpicPath.mockRejectedValue(new Error("traversal: 'x' escapes the team content root"));
    await expect(resolveExplicitEpicTarget("acme:default", "../x")).rejects.toThrow(/^traversal:/);
  });
});

// SW-GW-7: the dispatch's headless routing — EVERY kind backgrounds an agent
// session now; the epic/dir arms route to the injected headless launchers, the
// story/branch arms thread `headed` into their (session-based) spines.
describe("dispatchOpenWorkspace — headless routing (SW-GW-7)", () => {
  const DOC = "/ctx/epics/SW-EW/some-board.html";

  // A stub for the injected epic/dir headless launchers that records its calls.
  const stubHeadless = () => {
    const calls: { epic: unknown[]; epicAgents: string[]; dir: string[] } = {
      epic: [],
      epicAgents: [],
      dir: [],
    };
    const launchers: HeadlessSessionLaunchers = {
      openEpicHeadless: async (t, agent) => {
        calls.epic.push(t);
        calls.epicAgents.push(agent);
        return { sessionId: "epic-sess" };
      },
      openDirHeadless: async (p) => {
        calls.dir.push(p);
        return { sessionId: "dir-sess" };
      },
    };
    return { calls, launchers };
  };

  it("routes a headed:false EXPLICIT epic open to the headless launcher with the resolved target", async () => {
    // SW-GW-7: epic headless backgrounds ONE agent session (not the visible tile).
    resolveEpicCommandContext.mockReturnValue({
      codesenseHome: "/home",
      githubUserId: 7,
      orgSlug: "org",
      team: "team",
    });
    resolveEpicPath.mockResolvedValue({
      epicDirName: "SW-EW",
      epicFilePath: "/ctx/epics/SW-EW/README.md",
    });
    const { calls, launchers } = stubHeadless();
    const res = await dispatchOpenWorkspace(
      { target: { kind: "epic", team: "org:team", epic: "epics/SW-EW" }, headed: false },
      DOC,
      {} as DashboardLaunchDeps,
      {} as BranchLaunchDeps,
      {} as DirLaunchDeps,
      launchers,
    );
    expect(res).toEqual({ sessionId: "epic-sess" });
    // The resolved epic target (canonical mainDoc) flowed into the headless launcher,
    // and the visible-tile opener was NEVER called.
    expect(calls.epic).toHaveLength(1);
    expect((calls.epic[0] as { epicFilePath: string }).epicFilePath).toBe(
      "/ctx/epics/SW-EW/README.md",
    );
    // No agent named → the unattended built-in runs the background epic.
    expect(calls.epicAgents[0]).toBe("builtin/claude-unattended");
    expect(openEpicWorkspaceFor).not.toHaveBeenCalled();
  });

  it("routes a headed:false OWN epic open (no explicit team/epic) to the headless launcher", async () => {
    // SW-GW-7 (review r2): the board's OWN containing epic — resolved via the
    // doc-scoped dir list (`resolveOwnEpicTarget`), NOT an explicit {team, epic} —
    // must also route to the headless launcher, not the visible tile.
    getActiveTeamKey.mockReturnValue("acme:default");
    readWorkspaceFile.mockResolvedValue({
      entries: [
        { name: "some-board.html", isDir: false, size: 10 },
        { name: "README.md", isDir: false, size: 20 },
      ],
    });
    const { calls, launchers } = stubHeadless();
    const res = await dispatchOpenWorkspace(
      { target: { kind: "epic" }, headed: false },
      DOC,
      {} as DashboardLaunchDeps,
      {} as BranchLaunchDeps,
      {} as DirLaunchDeps,
      launchers,
    );
    expect(res).toEqual({ sessionId: "epic-sess" });
    // The board's own epic resolved to its canonical mainDoc and flowed to the
    // headless launcher; the visible-tile opener was NEVER called.
    expect(calls.epic).toHaveLength(1);
    expect((calls.epic[0] as { epicFilePath: string }).epicFilePath).toBe(
      "/ctx/epics/SW-EW/README.md",
    );
    // No agent named → the unattended built-in runs the background epic.
    expect(calls.epicAgents[0]).toBe("builtin/claude-unattended");
    expect(openEpicWorkspaceFor).not.toHaveBeenCalled();
  });

  it("threads the agent the open names into the headless epic launch", async () => {
    resolveEpicCommandContext.mockReturnValue({
      codesenseHome: "/home",
      githubUserId: 7,
      orgSlug: "org",
      team: "team",
    });
    resolveEpicPath.mockResolvedValue({
      epicDirName: "SW-EW",
      epicFilePath: "/ctx/epics/SW-EW/README.md",
    });
    const { calls, launchers } = stubHeadless();
    await dispatchOpenWorkspace(
      {
        target: {
          kind: "epic",
          team: "org:team",
          epic: "epics/SW-EW",
          agent: "builtin/codex",
        },
        headed: false,
      },
      DOC,
      {} as DashboardLaunchDeps,
      {} as BranchLaunchDeps,
      {} as DirLaunchDeps,
      launchers,
    );
    expect(calls.epicAgents[0]).toBe("builtin/codex");
  });

  it("routes a headed:false STORY open to the story launch (session spine, focus suppressed)", async () => {
    // Stub deps whose resolveStory returns null → the story path short-circuits with
    // its OWN error, proving dispatch took the story arm.
    const deps = { resolveStory: async () => null } as unknown as DashboardLaunchDeps;
    await expect(
      dispatchOpenWorkspace(
        { target: { kind: "story", storyId: "SW-DX-7" }, headed: false },
        DOC,
        deps,
        {} as BranchLaunchDeps,
        {} as DirLaunchDeps,
        stubHeadless().launchers,
      ),
    ).rejects.toThrow(/cannot resolve story/);
  });

  it("cancels through the active operation registry and releases the id for retry", async () => {
    let releaseCheckout!: (cwd: string) => void;
    const checkout = new Promise<string>((resolve) => {
      releaseCheckout = resolve;
    });
    const branchDeps = {
      resolveBranch: vi.fn(async () => ({ teamKey: "acme:team" })),
      begin: vi.fn(async () => true),
      abort: vi.fn(async () => undefined),
      killSession: vi.fn(async () => undefined),
      resolveSession: vi.fn(async () => null),
      ensureBranchWorktree: vi.fn(() => checkout),
      reconcileStory: vi.fn(async () => true),
      connect: vi.fn(async () => ({ sessionId: "review-session" })),
      bind: vi.fn(async () => undefined),
      waitLive: vi.fn(async () => true),
      focus: vi.fn(),
      isSessionParked: vi.fn(() => false),
      probeAlive: vi.fn(async () => true),
      evictStaleTab: vi.fn(),
    } as unknown as BranchLaunchDeps;
    const dashboardDeps = {
      injectLine: vi.fn(async () => undefined),
      resolveSession: vi.fn(async () => null),
      evictStaleTab: vi.fn(),
      closeByHandle: vi.fn(async () => undefined),
    } as unknown as DashboardLaunchDeps;
    const arg = {
      target: { kind: "branch" as const, repo: "acme/app", branch: "feature/x" },
      headed: false,
      operationId: "review-op",
    };

    const opening = dispatchOpenWorkspace(
      arg,
      DOC,
      dashboardDeps,
      branchDeps,
      {} as DirLaunchDeps,
      stubHeadless().launchers,
    );
    await vi.waitFor(() => expect(branchDeps.ensureBranchWorktree).toHaveBeenCalledOnce());
    cancelWorkspaceOpen("review-op");
    releaseCheckout("/wt");
    await expect(opening).rejects.toThrow(/^canceled:/);
    expect(branchDeps.connect).not.toHaveBeenCalled();

    const retryDeps = {
      ...branchDeps,
      ensureBranchWorktree: vi.fn(async () => "/wt"),
    } as unknown as BranchLaunchDeps;
    await expect(
      dispatchOpenWorkspace(
        arg,
        DOC,
        dashboardDeps,
        retryDeps,
        {} as DirLaunchDeps,
        stubHeadless().launchers,
      ),
    ).resolves.toEqual(expect.objectContaining({ sessionId: "review-session" }));
    // A background review that names no agent runs the unattended built-in, headless.
    expect(retryDeps.connect).toHaveBeenCalledWith(
      expect.objectContaining({
        agent: expect.objectContaining({ agent: "builtin/claude-unattended", headless: true }),
      }),
    );
  });

  it("closes a newly bound session when cancellation wins during readiness", async () => {
    let releaseLive!: (live: boolean) => void;
    const branchDeps = {
      resolveBranch: vi.fn(async () => ({ teamKey: "acme:team" })),
      begin: vi.fn(async () => true),
      abort: vi.fn(async () => undefined),
      killSession: vi.fn(async () => undefined),
      resolveSession: vi.fn(async () => null),
      ensureBranchWorktree: vi.fn(async () => "/wt"),
      reconcileStory: vi.fn(async () => true),
      connect: vi.fn(async () => ({ sessionId: "bound-review" })),
      bind: vi.fn(async () => undefined),
      waitLive: vi.fn(
        () =>
          new Promise<boolean>((resolve) => {
            releaseLive = resolve;
          }),
      ),
      focus: vi.fn(),
      isSessionParked: vi.fn(() => false),
      probeAlive: vi.fn(async () => true),
      evictStaleTab: vi.fn(),
    } as unknown as BranchLaunchDeps;
    const dashboardDeps = {
      injectLine: vi.fn(async () => undefined),
      resolveSession: vi.fn(async () => "bound-review"),
      evictStaleTab: vi.fn(),
      closeByHandle: vi.fn(async () => undefined),
    } as unknown as DashboardLaunchDeps;
    const opening = dispatchOpenWorkspace(
      {
        target: { kind: "branch", repo: "acme/app", branch: "feature/x" },
        headed: false,
        operationId: "bound-cancel-op",
      },
      DOC,
      dashboardDeps,
      branchDeps,
      {} as DirLaunchDeps,
      stubHeadless().launchers,
    );
    await vi.waitFor(() => expect(branchDeps.waitLive).toHaveBeenCalledWith("bound-review"));
    cancelWorkspaceOpen("bound-cancel-op");
    releaseLive(true);
    await expect(opening).rejects.toThrow(/^canceled:/);
    expect(dashboardDeps.resolveSession).toHaveBeenCalledWith("acme/app::feature/x");
    expect(dashboardDeps.evictStaleTab).toHaveBeenCalledWith("bound-review");
    expect(dashboardDeps.closeByHandle).toHaveBeenCalledWith("acme/app::feature/x");
  });

  it("closes a newly bound session when readiness fails naturally", async () => {
    const branchDeps = {
      resolveBranch: vi.fn(async () => ({ teamKey: "acme:team" })),
      begin: vi.fn(async () => true),
      abort: vi.fn(async () => undefined),
      killSession: vi.fn(async () => undefined),
      resolveSession: vi.fn(async () => null),
      ensureBranchWorktree: vi.fn(async () => "/wt"),
      reconcileStory: vi.fn(async () => true),
      connect: vi.fn(async () => ({ sessionId: "unready-review" })),
      bind: vi.fn(async () => undefined),
      waitLive: vi.fn(async () => false),
      focus: vi.fn(),
      isSessionParked: vi.fn(() => false),
      probeAlive: vi.fn(async () => true),
      evictStaleTab: vi.fn(),
    } as unknown as BranchLaunchDeps;
    const dashboardDeps = {
      injectLine: vi.fn(async () => undefined),
      resolveSession: vi.fn(async () => "unready-review"),
      evictStaleTab: vi.fn(),
      closeByHandle: vi.fn(async () => undefined),
    } as unknown as DashboardLaunchDeps;
    await expect(
      dispatchOpenWorkspace(
        {
          target: { kind: "branch", repo: "acme/app", branch: "feature/x" },
          headed: false,
          operationId: "unready-op",
        },
        DOC,
        dashboardDeps,
        branchDeps,
        {} as DirLaunchDeps,
        stubHeadless().launchers,
      ),
    ).rejects.toThrow("review agent did not become ready");
    expect(dashboardDeps.resolveSession).toHaveBeenCalledWith("acme/app::feature/x");
    expect(dashboardDeps.evictStaleTab).toHaveBeenCalledWith("unready-review");
    expect(dashboardDeps.closeByHandle).toHaveBeenCalledWith("acme/app::feature/x");
    expect(dashboardDeps.injectLine).not.toHaveBeenCalled();
  });

  it("keeps cancellation authoritative when pending prompt delivery rejects", async () => {
    let rejectPrompt!: (error: Error) => void;
    const branchDeps = {
      resolveBranch: vi.fn(async () => ({ teamKey: "acme:team" })),
      begin: vi.fn(async () => true),
      abort: vi.fn(async () => undefined),
      killSession: vi.fn(async () => undefined),
      resolveSession: vi.fn(async () => null),
      ensureBranchWorktree: vi.fn(async () => "/wt"),
      reconcileStory: vi.fn(async () => true),
      connect: vi.fn(async () => ({ sessionId: "prompt-review" })),
      bind: vi.fn(async () => undefined),
      waitLive: vi.fn(async () => true),
      focus: vi.fn(),
      isSessionParked: vi.fn(() => false),
      probeAlive: vi.fn(async () => true),
      evictStaleTab: vi.fn(),
    } as unknown as BranchLaunchDeps;
    const dashboardDeps = {
      injectLine: vi.fn(
        () =>
          new Promise<void>((_resolve, reject) => {
            rejectPrompt = reject;
          }),
      ),
      resolveSession: vi.fn(async () => "prompt-review"),
      evictStaleTab: vi.fn(),
      closeByHandle: vi.fn(async () => undefined),
    } as unknown as DashboardLaunchDeps;
    const opening = dispatchOpenWorkspace(
      {
        target: { kind: "branch", repo: "acme/app", branch: "feature/x" },
        headed: false,
        reviewPrompt: "/bmad-code-review",
        operationId: "prompt-cancel-op",
      },
      DOC,
      dashboardDeps,
      branchDeps,
      {} as DirLaunchDeps,
      stubHeadless().launchers,
    );
    await vi.waitFor(() => expect(dashboardDeps.injectLine).toHaveBeenCalledOnce());
    cancelWorkspaceOpen("prompt-cancel-op");
    rejectPrompt(new Error("terminal write failed"));
    await expect(opening).rejects.toThrow(/^canceled:/);
    expect(dashboardDeps.evictStaleTab).toHaveBeenCalledWith("prompt-review");
    expect(dashboardDeps.closeByHandle).toHaveBeenCalledWith("acme/app::feature/x");
  });

  it("closes an owned launch when cancellation races with successful prompt delivery", async () => {
    let resolvePrompt!: () => void;
    const branchDeps = {
      resolveBranch: vi.fn(async () => ({ teamKey: "acme:team" })),
      begin: vi.fn(async () => true),
      abort: vi.fn(async () => undefined),
      killSession: vi.fn(async () => undefined),
      resolveSession: vi.fn(async () => null),
      ensureBranchWorktree: vi.fn(async () => "/wt"),
      reconcileStory: vi.fn(async () => true),
      connect: vi.fn(async () => ({ sessionId: "owned-review" })),
      bind: vi.fn(async () => undefined),
      waitLive: vi.fn(async () => true),
      focus: vi.fn(),
      isSessionParked: vi.fn(() => false),
      probeAlive: vi.fn(async () => true),
      evictStaleTab: vi.fn(),
    } as unknown as BranchLaunchDeps;
    const dashboardDeps = {
      injectLine: vi.fn(() => new Promise<void>((resolve) => (resolvePrompt = resolve))),
      resolveSession: vi.fn(async () => "owned-review"),
      evictStaleTab: vi.fn(),
      closeByHandle: vi.fn(async () => undefined),
    } as unknown as DashboardLaunchDeps;
    const opening = dispatchOpenWorkspace(
      {
        target: { kind: "branch", repo: "acme/app", branch: "feature/x" },
        headed: false,
        reviewPrompt: "/bmad-code-review",
        operationId: "owned-prompt-cancel-op",
      },
      DOC,
      dashboardDeps,
      branchDeps,
      {} as DirLaunchDeps,
      stubHeadless().launchers,
    );
    await vi.waitFor(() => expect(dashboardDeps.injectLine).toHaveBeenCalledOnce());
    cancelWorkspaceOpen("owned-prompt-cancel-op");
    resolvePrompt();
    await expect(opening).rejects.toThrow(/^canceled:/);
    expect(dashboardDeps.closeByHandle).toHaveBeenCalledWith("acme/app::feature/x");
  });

  it("keeps a reused launch reviewing when cancellation follows successful delivery", async () => {
    let resolvePrompt!: () => void;
    const branchDeps = {
      resolveBranch: vi.fn(async () => ({ teamKey: "acme:team" })),
      begin: vi.fn(async () => false),
      abort: vi.fn(async () => undefined),
      killSession: vi.fn(async () => undefined),
      resolveSession: vi.fn(async () => "reused-review"),
      ensureBranchWorktree: vi.fn(async () => "/wt"),
      reconcileStory: vi.fn(async () => true),
      connect: vi.fn(),
      bind: vi.fn(),
      waitLive: vi.fn(async () => true),
      focus: vi.fn(),
      isSessionParked: vi.fn(() => false),
      probeAlive: vi.fn(async () => true),
      evictStaleTab: vi.fn(),
    } as unknown as BranchLaunchDeps;
    const dashboardDeps = {
      injectLine: vi.fn(() => new Promise<void>((resolve) => (resolvePrompt = resolve))),
      resolveSession: vi.fn(async () => "reused-review"),
      evictStaleTab: vi.fn(),
      closeByHandle: vi.fn(async () => undefined),
    } as unknown as DashboardLaunchDeps;
    const phases: string[] = [];
    const unsubscribe = subscribeReviewOpenPhase("reused-prompt-cancel-op", (phase) =>
      phases.push(phase),
    );
    try {
      const opening = dispatchOpenWorkspace(
        {
          target: { kind: "branch", repo: "acme/app", branch: "feature/x" },
          headed: false,
          reviewPrompt: "/bmad-code-review",
          operationId: "reused-prompt-cancel-op",
        },
        DOC,
        dashboardDeps,
        branchDeps,
        {} as DirLaunchDeps,
        stubHeadless().launchers,
      );
      await vi.waitFor(() => expect(dashboardDeps.injectLine).toHaveBeenCalledOnce());
      cancelWorkspaceOpen("reused-prompt-cancel-op");
      resolvePrompt();
      await expect(opening).resolves.toMatchObject({ sessionId: "reused-review" });
      expect(phases.at(-1)).toBe("reviewing");
      expect(dashboardDeps.closeByHandle).not.toHaveBeenCalled();
    } finally {
      unsubscribe();
    }
  });

  it("publishes joining through the reserved-but-unbound dispatch path", async () => {
    const branchDeps = {
      resolveBranch: vi.fn(async () => ({ teamKey: "acme:team" })),
      begin: vi.fn().mockResolvedValueOnce(false).mockResolvedValueOnce(true),
      abort: vi.fn(async () => undefined),
      killSession: vi.fn(async () => undefined),
      resolveSession: vi.fn(async () => null),
      ensureBranchWorktree: vi.fn(async () => "/wt"),
      reconcileStory: vi.fn(async () => true),
      connect: vi.fn(async () => ({ sessionId: "joined-review" })),
      bind: vi.fn(async () => undefined),
      waitLive: vi.fn(async () => true),
      focus: vi.fn(),
      isSessionParked: vi.fn(() => false),
      probeAlive: vi.fn(async () => true),
      evictStaleTab: vi.fn(),
    } as unknown as BranchLaunchDeps;
    const dashboardDeps = {
      injectLine: vi.fn(async () => undefined),
      resolveSession: vi.fn(async () => "joined-review"),
      evictStaleTab: vi.fn(),
      closeByHandle: vi.fn(async () => undefined),
    } as unknown as DashboardLaunchDeps;
    const phases: string[] = [];
    const unsubscribe = subscribeReviewOpenPhase("joining-producer-op", (phase) =>
      phases.push(phase),
    );
    try {
      await dispatchOpenWorkspace(
        {
          target: { kind: "branch", repo: "acme/app", branch: "feature/x" },
          headed: false,
          operationId: "joining-producer-op",
        },
        DOC,
        dashboardDeps,
        branchDeps,
        {} as DirLaunchDeps,
        stubHeadless().launchers,
      );
      expect(phases).toContain("joining");
      expect(phases.at(-1)).toBe("launching");
    } finally {
      unsubscribe();
    }
  });

  it("cleans up after a waiting joiner acquires ownership and is canceled during prompt delivery", async () => {
    let resolvePrompt!: () => void;
    const branchDeps = {
      resolveBranch: vi.fn(async () => ({ teamKey: "acme:team" })),
      begin: vi.fn().mockResolvedValueOnce(false).mockResolvedValueOnce(true),
      abort: vi.fn(async () => undefined),
      killSession: vi.fn(async () => undefined),
      resolveSession: vi.fn(async () => null),
      ensureBranchWorktree: vi.fn(async () => "/wt"),
      reconcileStory: vi.fn(async () => true),
      connect: vi.fn(async () => ({ sessionId: "transferred-review" })),
      bind: vi.fn(async () => undefined),
      waitLive: vi.fn(async () => true),
      focus: vi.fn(),
      isSessionParked: vi.fn(() => false),
      probeAlive: vi.fn(async () => true),
      evictStaleTab: vi.fn(),
    } as unknown as BranchLaunchDeps;
    const dashboardDeps = {
      injectLine: vi.fn(() => new Promise<void>((resolve) => (resolvePrompt = resolve))),
      resolveSession: vi.fn(async () => "transferred-review"),
      evictStaleTab: vi.fn(),
      closeByHandle: vi.fn(async () => undefined),
    } as unknown as DashboardLaunchDeps;
    const opening = dispatchOpenWorkspace(
      {
        target: { kind: "branch", repo: "acme/app", branch: "feature/x" },
        headed: false,
        reviewPrompt: "/bmad-code-review",
        operationId: "transferred-cancel-op",
      },
      DOC,
      dashboardDeps,
      branchDeps,
      {} as DirLaunchDeps,
      stubHeadless().launchers,
    );
    await vi.waitFor(() => expect(dashboardDeps.injectLine).toHaveBeenCalledOnce());
    cancelWorkspaceOpen("transferred-cancel-op");
    resolvePrompt();
    await expect(opening).rejects.toThrow(/^canceled:/);
    expect(dashboardDeps.resolveSession).toHaveBeenCalledWith("acme/app::feature/x");
    expect(dashboardDeps.evictStaleTab).toHaveBeenCalledWith("transferred-review");
    expect(dashboardDeps.closeByHandle).toHaveBeenCalledWith("acme/app::feature/x");
  });
});

// SW-GW-2 / SW-GW-7: the dispatch's `dir` routing — a HEADED open dispatches to
// SW-GW-1's generic-workspace shell wrapper (synchronously); a HEADLESS open
// backgrounds an agent session via the injected launcher (a different path).
describe("dispatchOpenWorkspace — dir target (SW-GW-2 / SW-GW-7)", () => {
  const DOC = "/ctx/epics/SW-GW/some-board.html";

  const stubHeadless = () => {
    const dir: string[] = [];
    const dirAgents: string[] = [];
    const launchers: HeadlessSessionLaunchers = {
      openEpicHeadless: async () => ({ sessionId: "epic-sess" }),
      openDirHeadless: async (p, opts) => {
        dir.push(p);
        dirAgents.push(opts.agent);
        return { sessionId: "dir-sess" };
      },
    };
    return { dir, dirAgents, launchers };
  };

  it("dispatches a headed dir open to the generic-workspace wrapper with the path", async () => {
    const opened: string[] = [];
    const dirDeps: DirLaunchDeps = { openGenericWorkspace: (p) => opened.push(p) };
    const res = await dispatchOpenWorkspace(
      { target: { kind: "dir", path: "/Users/me/proj" } },
      DOC,
      {} as DashboardLaunchDeps,
      {} as BranchLaunchDeps,
      dirDeps,
      stubHeadless().launchers,
    );
    expect(opened).toEqual(["/Users/me/proj"]);
    // No session handle for a HEADED dir open (the shell tile paints synchronously).
    expect(res).toEqual({});
  });

  it("routes a headed:false dir open to the headless agent launcher (not the shell tile)", async () => {
    const opened: string[] = [];
    const dirDeps: DirLaunchDeps = { openGenericWorkspace: (p) => opened.push(p) };
    const { dir, launchers } = stubHeadless();
    const res = await dispatchOpenWorkspace(
      { target: { kind: "dir", path: "/Users/me/proj" }, headed: false },
      DOC,
      {} as DashboardLaunchDeps,
      {} as BranchLaunchDeps,
      dirDeps,
      launchers,
    );
    // The headless launcher ran; the visible shell wrapper NEVER did (no UI tile).
    expect(res).toEqual({ sessionId: "dir-sess" });
    expect(dir).toEqual(["/Users/me/proj"]);
    expect(opened).toEqual([]);
  });

  it("a headless dir open runs the agent it names, else the built-in Claude agent", async () => {
    const dirDeps: DirLaunchDeps = { openGenericWorkspace: () => {} };
    const { dirAgents, launchers } = stubHeadless();
    await dispatchOpenWorkspace(
      { target: { kind: "dir", path: "/Users/me/proj" }, headed: false },
      DOC,
      {} as DashboardLaunchDeps,
      {} as BranchLaunchDeps,
      dirDeps,
      launchers,
    );
    await dispatchOpenWorkspace(
      {
        target: { kind: "dir", path: "/Users/me/proj", agent: "builtin/codex" },
        headed: false,
      },
      DOC,
      {} as DashboardLaunchDeps,
      {} as BranchLaunchDeps,
      dirDeps,
      launchers,
    );
    expect(dirAgents).toEqual(["builtin/claude-unattended", "builtin/codex"]);
  });
});

// ── SW-GW-3: resolveWorkspaceStatusForStory (the OBSERVE-half read verb core) ──
describe("resolveWorkspaceStatusForStory", () => {
  const storyWorkspace = {
    "org:team": [{ id: "ws1", kind: "story", storyId: "GW-3", cwd: "/wt", status: "ready" }],
  };
  /** The viewer's open workspaces, and the host's running terminals in ws1. */
  const seed = (workspaces: unknown, sessions: string[] = []) => {
    useViewerStore.setState({ workspaces: workspaces as never });
    if (sessions.length) seedTerminalTabs("org:team", "ws1", sessions);
  };

  beforeEach(() => {
    dashboardProbeSessionsAlive.mockReset();
    seed({});
  });

  it("returns exists:false for a story with no open workspace (never-started → Launch)", async () => {
    const status = await resolveWorkspaceStatusForStory("GW-3");
    expect(status).toEqual({ exists: false, live: false, sessionId: null });
    expect(dashboardProbeSessionsAlive).not.toHaveBeenCalled(); // no probe when not open
  });

  it("finds a MANUALLY-opened story (viewer open list, not the launch registry) and probes it live", async () => {
    seed(storyWorkspace, ["s1"]);
    dashboardProbeSessionsAlive.mockResolvedValue(["s1"]); // child alive
    const status = await resolveWorkspaceStatusForStory("GW-3");
    expect(dashboardProbeSessionsAlive).toHaveBeenCalledWith(["s1"]);
    expect(status).toEqual({ exists: true, live: true, sessionId: "s1" });
  });

  it("reports live:false when the open story's session child has EXITED (real try_wait, not membership)", async () => {
    seed(storyWorkspace, ["s1"]);
    dashboardProbeSessionsAlive.mockResolvedValue([]); // probe says dead
    expect(await resolveWorkspaceStatusForStory("GW-3")).toEqual({
      exists: true,
      live: false,
      sessionId: "s1",
    });
  });

  it("reports exists:true, live:false, sessionId:null when open but no terminal tab yet (no probe)", async () => {
    seed(storyWorkspace);
    const status = await resolveWorkspaceStatusForStory("GW-3");
    expect(status).toEqual({ exists: true, live: false, sessionId: null });
    expect(dashboardProbeSessionsAlive).not.toHaveBeenCalled();
  });
});

it.each(["acme/backend/hermes-open", "builtin/hermes"])(
  "dashboard preparation follows backend listing for %s",
  (agentKey) => {
    const prepare = () => prepareAgentLaunch({ agentKey, sessionId: "dashboard-listing" });
    if (isAgentRuntime("hermes")) expect(prepare).not.toThrow(/no agent.*hermes/);
    else expect(prepare).toThrow(/no agent.*hermes/);
  },
);
