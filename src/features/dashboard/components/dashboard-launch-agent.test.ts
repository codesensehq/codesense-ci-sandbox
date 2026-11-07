// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

/**
 * A named agent launched from the app's open-workspace dispatch, through the controller's REAL
 * launch deps (story, branch and dir), with only the IPC and the terminal host faked. The
 * backend's `agent_launch_prepare` is served by the RF-29 mock backend's handler. The seams
 * checked: the shell spawn receives exactly the prepared args and the account override, the
 * prepare call names the spawned session, and the bind records the agent revision.
 */

const {
  invoke,
  bindCalls,
  prepareCalls,
  terminal,
  liveProbes,
  checkouts,
  agentRows,
  running,
  jobs,
} = vi.hoisted(() => ({
  agentRows: { rows: [] as unknown[] },
  /** The agent CLIs running in each session, as the process probe reads them. */
  running: new Map<string, string>(),
  /** The jobs besides its shell each session runs; absent reads as an idle shell. */
  jobs: new Map<string, { foreground: string | null; background: string[] }>(),
  liveProbes: [] as Array<{ sessionId: string; agent: string }>,
  checkouts: [] as Array<Record<string, unknown>>,
  invoke: { router: null as null | ((cmd: string, args?: Record<string, unknown>) => unknown) },
  bindCalls: [] as Array<{ handle: string; sessionId: string; scope: unknown }>,
  prepareCalls: [] as Array<Record<string, unknown>>,
  terminal: {
    projections: [] as Array<{ sessionId: string; mcpAllow: string[] | null }>,
    existing: null as null | { workspaceId: string; sessionId: string },
    shells: [] as Array<{ sessionId: string; name?: string; shell: Record<string, unknown> }>,
    lines: [] as Array<{ sessionId: string; line: string }>,
    seeds: [] as Array<{ sessionId: string; text: string }>,
    shown: [] as Array<{ teamKey: string; workspaceId: string; sessionId: string }>,
  },
}));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  dashboardLaunchBegin: vi.fn(async () => true),
  agentsList: vi.fn(async () => agentRows.rows),
  sessionHasLiveAgent: vi.fn(
    async (sessionId: string, agentName: string) => running.get(sessionId) === agentName,
  ),
  sessionsRunningProcesses: vi.fn(async (ids: string[]) =>
    ids.map((id) => jobs.get(id) ?? { foreground: null, background: [] }),
  ),
  dashboardLaunchAbort: vi.fn(async () => undefined),
  dashboardResolveSession: vi.fn(async () => null),
  dashboardProbeSessionsAlive: vi.fn(async () => []),
  terminalCloseTab: vi.fn(async () => undefined),
  dashboardLaunchBind: vi.fn(
    async (handle: string, sessionId: string, _launcher?: string, scope?: unknown) => {
      bindCalls.push({ handle, sessionId, scope });
    },
  ),
  agentLaunchPrepare: vi.fn(async (p: Record<string, unknown>) => {
    prepareCalls.push(p);
    return invoke.router!("agent_launch_prepare", p);
  }),
  ensureBranchWorkspace: vi.fn(async (p: Record<string, unknown>) => {
    checkouts.push(p);
    return { slug: p.repoSlug, dirPath: "/wt/agent-branch", registered: true };
  }),
}));

vi.mock("@/shared/lib/agent-session-launcher", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/lib/agent-session-launcher")>();
  return {
    ...actual,
    waitForAgentLive: vi.fn(async (sessionId: string, agent: string) => {
      liveProbes.push({ sessionId, agent });
      return true;
    }),
    makeRealLaunchDeps: () => ({
      findExisting: async () => terminal.existing ?? undefined,
      provision: async () => undefined,
      openTab: async (
        _teamKey: string,
        _workspaceId: string,
        tab: { sessionId: string; name?: string },
        shell: Record<string, unknown>,
      ) => {
        terminal.shells.push({ sessionId: tab.sessionId, name: tab.name, shell });
      },
      closeTab: () => undefined,
      launchAgentLine: async (sessionId: string, line: string) => {
        terminal.lines.push({ sessionId, line });
        return { ok: true };
      },
      projectScopeConfig: async (
        sessionId: string,
        _cwd: string,
        mcpAllow: string[] | null,
        _account?: unknown,
        launch?: { binary?: string },
      ) => {
        terminal.projections.push({ sessionId, mcpAllow });
        // Like the backend: a Codex launch with no allowlist adds nothing (its bridge rides
        // its profile args); anything else gets a session config.
        if (launch?.binary === "codex" && mcpAllow === null) {
          return { extraArgs: [], environmentBinding: null };
        }
        return {
          extraArgs: ["--mcp-config", `/scope/${sessionId}.json`],
          environmentBinding: null,
        };
      },
    }),
  };
});

vi.mock("@/shared/lib/default-agent", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/default-agent")>()),
  resolveAgentCliPath: async (basename = "claude") => `/opt/bin/${basename}`,
}));

vi.mock("@/shared/lib/agent-session-injection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/agent-session-injection")>()),
  injectAgentLine: vi.fn(async (sessionId: string, text: string) => {
    terminal.seeds.push({ sessionId, text });
    return { ok: true };
  }),
}));

vi.mock("@/shared/lib/team-key", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/team-key")>()),
  getActiveTeamKey: () => "acme:backend",
}));

vi.mock("./dashboard-launch-status", () => ({ armLaunchStatus: () => undefined }));
vi.mock("@/features/machines/stores/machines-store", () => ({ useTeamMachines: () => [] }));
vi.mock("@/features/agents/lib/agent-names", () => ({
  useAgentRows: () => ({ rows: [], error: null }),
}));
vi.mock("@/features/story-launch/story-agent-launch", async (original) => ({
  ...(await original<typeof import("@/features/story-launch/story-agent-launch")>()),
  orderStoryLaunchAgents: () => [
    { agentKey: "triage", name: "Slack triage", runtime: "claude", place: null },
  ],
}));
vi.mock("@/shared/lib/get-current-user", async (original) => ({
  ...(await original<typeof import("@/shared/lib/get-current-user")>()),
  useCurrentUser: () => ({ principal: "alice" }),
}));
vi.mock("@/features/viewer/lib/terminal-tab-keys", async (original) => ({
  ...(await original<typeof import("@/features/viewer/lib/terminal-tab-keys")>()),
  showNewTab: async (
    teamKey: string,
    workspaceId: string,
    sessionId: string,
    stillHere: () => boolean,
  ) => {
    terminal.shown.push({ teamKey, workspaceId, sessionId });
    if (stillHere()) document.querySelector<HTMLTextAreaElement>("[data-test-terminal]")?.focus();
  },
}));

import { createInvokeRouter } from "@/app/mock/mock-router";
import { agentLaunchHandlers } from "@/app/mock/handlers/agents-launch";
import {
  dispatchOpenWorkspace,
  makeRealBranchLaunchDeps,
  makeRealDashboardLaunchDeps,
  makeRealHeadlessLaunchers,
  shellLaunchFromPrepared,
  launchAgentInWorkspace,
} from "./dashboard-launch-controller";
import {
  setDashboardLaunchController,
  type BranchLaunchDeps,
  type DashboardLaunchDeps,
  type DirLaunchDeps,
} from "./dashboard-launch";
import { runBridgeCommand } from "./html-bridge";
import { useViewerStore } from "@/shared/stores/viewer-store";

const DOC = "/ctx/boards/agents.html";
const TEAM_KEY = "acme:backend";

function storyDeps(): DashboardLaunchDeps {
  return {
    ...makeRealDashboardLaunchDeps(DOC),
    resolveStory: async () => ({ storyFilePath: "/ctx/stories/SW-1.md", teamKey: TEAM_KEY }),
    ensureWorktree: async () => "/wt/SW-1",
    awaitEstablished: undefined,
    awaitContext: undefined,
    focus: vi.fn(),
  };
}

function branchDeps(): BranchLaunchDeps {
  return {
    ...makeRealBranchLaunchDeps(DOC),
    resolveBranch: async () => ({
      repoPath: "/src/api",
      teamKey: TEAM_KEY,
      codesenseHome: "/home/.codesense",
      githubUserId: 1,
      orgSlug: "acme",
      team: "backend",
    }),
    ensureBranchWorktree: async () => "/wt/review",
    focus: vi.fn(),
  };
}

const dirDeps: DirLaunchDeps = { openGenericWorkspace: vi.fn() };

function open(arg: Parameters<typeof dispatchOpenWorkspace>[0]) {
  return dispatchOpenWorkspace(
    arg,
    DOC,
    storyDeps(),
    branchDeps(),
    dirDeps,
    makeRealHeadlessLaunchers(DOC),
  );
}

beforeEach(() => {
  invoke.router = createInvokeRouter(agentLaunchHandlers());
  bindCalls.length = 0;
  prepareCalls.length = 0;
  terminal.shells.length = 0;
  terminal.projections.length = 0;
  terminal.existing = null;
  terminal.lines.length = 0;
  terminal.seeds.length = 0;
  terminal.shown.length = 0;
  liveProbes.length = 0;
  checkouts.length = 0;
  agentRows.rows = [];
  running.clear();
  jobs.clear();
});

describe("dispatchOpenWorkspace — a named agent, prepared trusted-side for the spawned session", () => {
  it("story: spawns with the prepared args and account, binds the revision, seeds the default prompt", async () => {
    const result = await open({ target: { kind: "story", storyId: "SW-1", agent: "triage" } });

    expect(prepareCalls).toHaveLength(1);
    const [shell] = terminal.shells;
    expect(prepareCalls[0]).toMatchObject({
      agentKey: "triage",
      team: TEAM_KEY,
      cwd: "/wt/SW-1",
      sessionId: shell.sessionId,
      mode: "interactive",
    });
    // The shell runs as the agent runtime's account, with its key stripped.
    expect(shell.shell).toMatchObject({
      cwd: "/wt/SW-1",
      envRemove: ["ANTHROPIC_API_KEY"],
      account: { runtime: "claude" },
    });
    // The launch line is exactly the prepared args plus the projected scope's flags.
    const [{ line }] = terminal.lines;
    expect(line).toContain("'/opt/bin/claude' '--model' 'opus' '--settings'");
    expect(line).toContain(`codesense-launch-settings/${shell.sessionId}--`);
    expect(line).toContain("'--strict-mcp-config'");
    expect(bindCalls).toEqual([
      {
        handle: "SW-1",
        sessionId: shell.sessionId,
        scope: { teamKey: TEAM_KEY, agentRevision: "acme/backend/triage@3" },
      },
    ]);
    // No prompt of its own: the agent starts with its default one.
    expect(terminal.seeds).toEqual([{ sessionId: shell.sessionId, text: "/triage-slack" }]);
    expect(result.sessionId).toBe(shell.sessionId);
    expect(result.warnings).toBeUndefined();
    // The backend projected the scope for the session; nothing is projected here.
    expect(terminal.projections).toEqual([]);
  });

  it("carries prepared role startup through the shell launch and never seeds it again after bind", async () => {
    const router = invoke.router!;
    const task = "/review queued work\n\nYou hold role reviewer; 2 messages are pending.";
    invoke.router = async (cmd, args) => {
      const value = await router(cmd, args);
      if (cmd !== "agent_launch_prepare") return value;
      const prepared = value as import("@/tauri/commands").PreparedAgentLaunch;
      return {
        ...prepared,
        promptInArgv: true,
        defaultPrompt: null,
        launch: {
          ...prepared.launch,
          args: [...prepared.launch.args, ...prepared.scope.extraArgs, "--", task],
        },
        scope: { ...prepared.scope, extraArgs: [] },
      };
    };
    const result = await open({
      target: { kind: "story", storyId: "SW-1", agent: "triage" },
      initialPrompt: "/review queued work",
    });
    expect(terminal.lines[0].line).toContain(`'--' '${task}'`);
    expect(terminal.seeds).toEqual([]);
    expect(result.promptDelivered).toBe(true);
    expect(bindCalls).toHaveLength(1);
  });

  it("story: a prompt the agent does not start with is seeded and warned about, never refused", async () => {
    const result = await open({
      target: { kind: "story", storyId: "SW-1", agent: "triage" },
      initialPrompt: "/groom the backlog",
    });
    expect(prepareCalls[0].prompt).toBe("/groom the backlog");
    expect(terminal.seeds.map((s) => s.text)).toEqual(["/groom the backlog"]);
    expect(result.promptDelivered).toBe(true);
    expect(result.warnings).toEqual([expect.objectContaining({ code: "command_not_in_manifest" })]);
  });

  it("story: an open that names no agent launches the built-in Claude agent", async () => {
    await open({ target: { kind: "story", storyId: "SW-1" }, initialPrompt: "go" });
    expect(prepareCalls[0]).toMatchObject({ agentKey: "builtin/claude", prompt: "go" });
    expect(bindCalls[0].scope).toEqual({ teamKey: TEAM_KEY, agentRevision: "builtin/claude@1" });
    expect(terminal.lines[0].line).toContain("'/opt/bin/claude' '--settings'");
  });

  it("story: an account named for one launch rides the prepare, never the binding", async () => {
    await open({
      target: { kind: "story", storyId: "SW-1", agent: "builtin/codex" },
      account: { userId: 1, id: "acct-work", generation: 2 },
    });
    expect(prepareCalls[0]).toMatchObject({
      agentKey: "builtin/codex",
      accountId: "acct-work",
      accountGeneration: 2,
    });
    expect(terminal.shells[0].shell).toMatchObject({
      account: { runtime: "codex", overrideAccountId: "acct-work", overrideAccountGeneration: 2 },
    });
  });

  it("branch: a Codex agent spawns with its scope binding and records its revision", async () => {
    const result = await open({
      target: { kind: "branch", repo: "acme/api", branch: "feat/x", agent: "reviewer" },
      reviewPrompt: "review it",
    });
    const [shell] = terminal.shells;
    expect(prepareCalls[0]).toMatchObject({
      agentKey: "reviewer",
      cwd: "/wt/review",
      sessionId: shell.sessionId,
      prompt: "review it",
    });
    expect(shell.shell).toMatchObject({
      envRemove: ["OPENAI_API_KEY"],
      account: { runtime: "codex" },
      environmentBinding: `mock-scope-${shell.sessionId}`,
    });
    expect(terminal.lines[0].line).toBe("'/opt/bin/codex' '--model' 'gpt-5'");
    expect(bindCalls[0].scope).toEqual({
      teamKey: TEAM_KEY,
      agentRevision: "acme/backend/reviewer@1",
    });
    expect(result.sessionId).toBe(shell.sessionId);
    // Readiness waits for the agent that actually runs, not the active pack's.
    expect(liveProbes).toEqual([{ sessionId: shell.sessionId, agent: "codex" }]);
    // The review prompt is the one driven in; nothing else is seeded.
    expect(terminal.seeds.map((s) => s.text)).toEqual(["review it"]);
  });

  it.each([
    ["new", "creates"],
    ["existing", "opens"],
  ] as const)(
    "branch: the backend checkout is told the agent launch's mode (%s %s the branch, no review)",
    async (checkout, _verb) => {
      const deps: BranchLaunchDeps = { ...branchDeps() };
      // The real checkout seam, down to the `ensure_branch_workspace` IPC.
      deps.ensureBranchWorktree = makeRealBranchLaunchDeps(DOC).ensureBranchWorktree;
      await dispatchOpenWorkspace(
        {
          target: {
            kind: "branch",
            repo: "acme/api",
            branch: "feat/fresh",
            agent: "reviewer",
            checkout,
          },
          reviewPrompt: "start",
        },
        DOC,
        storyDeps(),
        deps,
        dirDeps,
        makeRealHeadlessLaunchers(DOC),
      );
      expect(checkouts).toEqual([
        expect.objectContaining({
          repoPath: "/src/api",
          repoSlug: "acme/api",
          branch: "feat/fresh",
          checkout,
        }),
      ]);
      expect(prepareCalls[0]).toMatchObject({ cwd: "/wt/agent-branch" });
    },
  );

  it.each([
    [
      "a live session",
      "sess-live",
      "Branch `feat/x` already exists — use Repo + branch to open it.",
    ],
    [
      "a launch still starting",
      null,
      "A launch on `feat/x` is already starting — wait for it, then use Repo + branch to open the branch.",
    ],
  ] as const)(
    "branch: New worktree on a branch with %s refuses instead of joining it",
    async (_case, bound, refusal) => {
      const commands = await import("@/tauri/commands");
      // Another launch owns the handle: bound and alive, or reserved and not yet bound.
      vi.mocked(commands.dashboardLaunchBegin).mockResolvedValueOnce(false);
      // The IPC answers null for an unbound handle, though its wrapper is typed `string`.
      vi.mocked(commands.dashboardResolveSession).mockResolvedValueOnce(bound as string);
      vi.mocked(commands.dashboardProbeSessionsAlive).mockResolvedValueOnce(["sess-live"]);
      // Live means its agent still runs there, not only its shell.
      running.set("sess-live", "claude");
      const deps: BranchLaunchDeps = { ...branchDeps() };
      deps.ensureBranchWorktree = makeRealBranchLaunchDeps(DOC).ensureBranchWorktree;
      await expect(
        dispatchOpenWorkspace(
          {
            target: {
              kind: "branch",
              repo: "acme/api",
              branch: "feat/x",
              agent: "reviewer",
              checkout: "new",
            },
            reviewPrompt: "start",
          },
          DOC,
          storyDeps(),
          deps,
          dirDeps,
          makeRealHeadlessLaunchers(DOC),
        ),
      ).rejects.toThrow(refusal);
      expect(checkouts).toEqual([]);
      expect(terminal.seeds).toEqual([]);
      expect(terminal.shells).toEqual([]);
    },
  );

  it("dir: launches the agent session in the folder (never the bare shell tile) and binds its revision", async () => {
    const result = await open({
      target: { kind: "dir", path: "/wt/existing", agent: "triage" },
      headed: false,
      initialPrompt: "/bmad-help",
    });
    expect(dirDeps.openGenericWorkspace).not.toHaveBeenCalled();
    const [shell] = terminal.shells;
    expect(prepareCalls[0]).toMatchObject({
      agentKey: "triage",
      team: TEAM_KEY,
      cwd: "/wt/existing",
      sessionId: shell.sessionId,
      prompt: "/bmad-help",
    });
    expect(shell.shell).toMatchObject({ account: { runtime: "claude" } });
    expect(terminal.lines[0].line).toContain("'--settings'");
    expect(bindCalls[0].scope).toEqual({
      teamKey: TEAM_KEY,
      agentRevision: "acme/backend/triage@3",
    });
    expect(terminal.seeds.map((s) => s.text)).toEqual(["/bmad-help"]);
    expect(result).toMatchObject({ sessionId: shell.sessionId, promptDelivered: true });
  });

  it("dir: a named session at a context folder writes nothing there and keys its own handle", async () => {
    const commands = await import("@/tauri/commands");
    const result = await open({
      target: {
        kind: "dir",
        path: "/ctx/team/content",
        agent: "triage",
        session: "triage",
      },
      headed: false,
    });
    const [shell] = terminal.shells;
    // Its MCP scope, permissions and hooks ride per-session files outside the folder.
    expect(terminal.lines[0].line).toContain("'--strict-mcp-config'");
    expect(prepareCalls[0]).toMatchObject({ agentKey: "triage", cwd: "/ctx/team/content" });
    // A distinct handle, so it never collides with the folder's plain session.
    expect(vi.mocked(commands.dashboardLaunchBegin)).toHaveBeenCalledWith(
      "/ctx/team/content#triage",
    );
    expect(bindCalls[0]).toMatchObject({
      handle: "/ctx/team/content#triage",
      scope: { agentRevision: "acme/backend/triage@3" },
    });
    expect(result.sessionId).toBe(shell.sessionId);
  });

  it("dir: a headed open is the bare shell tile; a headless one runs the unattended built-in", async () => {
    await open({ target: { kind: "dir", path: "/wt/existing" } });
    expect(dirDeps.openGenericWorkspace).toHaveBeenCalledTimes(1);
    expect(terminal.shells).toEqual([]);

    await open({ target: { kind: "dir", path: "/wt/plain" }, headed: false });
    const [shell] = terminal.shells;
    expect(prepareCalls[0]).toMatchObject({
      agentKey: "builtin/claude-unattended",
      cwd: "/wt/plain",
      mode: "headless",
    });
    expect(terminal.lines[0].line).toContain("--dangerously-skip-permissions");
    // The backend projected its per-session config; nothing is written into the folder.
    expect(terminal.projections).toEqual([]);
    expect(terminal.lines[0].line).toContain(`codesense-launch-settings/${shell.sessionId}--`);
  });

  it("epic: a headless open runs the agent it names in the epic workspace", async () => {
    const commands = await import("@/tauri/commands");
    vi.spyOn(commands, "ensureEpicWorkspace").mockResolvedValueOnce({
      dirPath: "/wt/epic-dir",
    } as never);
    const opener = await import("@/features/viewer/lib/epic-workspace-opener");
    vi.spyOn(opener, "resolveEpicCommandContext").mockReturnValue({
      codesenseHome: "/home/.codesense",
      githubUserId: 1,
      orgSlug: "acme",
      team: "backend",
    } as never);
    vi.spyOn(commands, "resolveEpicPath").mockResolvedValueOnce({
      epicDirName: "e1",
      epicFilePath: "/ctx/e1/README.md",
    } as never);
    await open({
      target: { kind: "epic", team: TEAM_KEY, epic: "e1", agent: "builtin/codex" },
      headed: false,
    });
    expect(prepareCalls[0]).toMatchObject({ agentKey: "builtin/codex", cwd: "/wt/epic-dir" });
    expect(bindCalls[0]).toMatchObject({
      handle: "/ctx/e1/README.md",
      scope: { agentRevision: "builtin/codex@1" },
    });
  });

  it("a background open never asks for interactive permissions; a headed one keeps the normal default; a named agent wins", async () => {
    // Background story and dir opens with no agent: the unattended built-in, headless.
    await open({ target: { kind: "story", storyId: "SW-1" }, headed: false });
    await open({ target: { kind: "dir", path: "/wt/bg" }, headed: false });
    // A background open that names its agent runs that agent, still headless.
    await open({
      target: { kind: "story", storyId: "SW-2", agent: "builtin/codex" },
      headed: false,
    });
    // A headed open with no agent keeps builtin/claude and interactive permissions.
    await open({ target: { kind: "story", storyId: "SW-3" } });
    expect(prepareCalls.map((c) => [c.agentKey, c.mode])).toEqual([
      ["builtin/claude-unattended", "headless"],
      ["builtin/claude-unattended", "headless"],
      ["builtin/codex", "headless"],
      ["builtin/claude", "interactive"],
    ]);
  });

  it("an unknown agent fails the launch as invalid and leaves no terminal behind", async () => {
    await expect(
      open({ target: { kind: "story", storyId: "SW-1", agent: "nobody" } }),
    ).rejects.toThrow('invalid: no agent "nobody" in acme/backend or your private space');
    expect(terminal.lines).toHaveLength(0);
    expect(bindCalls).toHaveLength(0);
  });
});

describe("an HTML doc's openWorkspace launches the agent it names", () => {
  /** The doc's bridge, forwarding to this test's controller over the real launch deps. */
  function mountDoc() {
    setDashboardLaunchController({
      openWorkspace: (arg: Parameters<typeof open>[0]) => open(arg),
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: () => null,
      getDocDir: () => null,
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    } as never);
  }

  it("a doc naming builtin/codex reaches agent_launch_prepare and spawns Codex as prepared", async () => {
    mountDoc();
    try {
      const result = (await runBridgeCommand("openWorkspace", {
        target: { kind: "story", storyId: "SW-1", agent: "builtin/codex" },
        initialPrompt: "go",
      })) as { sessionId: string };
      const [shell] = terminal.shells;
      expect(prepareCalls).toEqual([
        expect.objectContaining({
          agentKey: "builtin/codex",
          team: TEAM_KEY,
          cwd: "/wt/SW-1",
          sessionId: shell.sessionId,
          prompt: "go",
        }),
      ]);
      expect(shell.shell).toMatchObject({
        envRemove: ["OPENAI_API_KEY"],
        account: { runtime: "codex" },
      });
      expect(terminal.lines[0].line).toBe("'/opt/bin/codex'");
      expect(bindCalls[0]).toMatchObject({
        handle: "SW-1",
        scope: { teamKey: TEAM_KEY, agentRevision: "builtin/codex@1" },
      });
      expect(result.sessionId).toBe(shell.sessionId);
    } finally {
      setDashboardLaunchController(null);
    }
  });

  it("an unknown agent comes back to the doc as a typed invalid rejection, nothing spawned", async () => {
    mountDoc();
    try {
      await expect(
        runBridgeCommand("openWorkspace", {
          target: { kind: "story", storyId: "SW-1", agent: "nobody" },
        }),
      ).rejects.toThrow(/^invalid: no agent "nobody"/);
      expect(terminal.lines).toEqual([]);
      expect(bindCalls).toEqual([]);
    } finally {
      setDashboardLaunchController(null);
    }
  });

  it("a doc passing a launch profile is refused before anything is prepared", async () => {
    mountDoc();
    try {
      await expect(
        runBridgeCommand("openWorkspace", {
          target: { kind: "story", storyId: "SW-1", profile: "skip-permissions" },
        }),
      ).rejects.toThrow(/^invalid: openWorkspace launches an agent/);
      expect(prepareCalls).toEqual([]);
      expect(terminal.shells).toEqual([]);
    } finally {
      setDashboardLaunchController(null);
    }
  });
});

describe("shellLaunchFromPrepared", () => {
  it("forwards the backend's direct execution, and nothing when it asks for none", async () => {
    const launch = (await createInvokeRouter(agentLaunchHandlers())("agent_launch_prepare", {
      agentKey: "acme/backend/triage",
      team: TEAM_KEY,
      cwd: "/wt",
      sessionId: "s1",
    })) as Parameters<typeof shellLaunchFromPrepared>[0];
    const execution = { argv: ["/bin/hermes", "chat", "--tui"], nativeLaunch: "hermes" as const };
    expect(
      shellLaunchFromPrepared({ ...launch, scope: { ...launch.scope, execution } }).execution,
    ).toEqual(execution);
    expect(shellLaunchFromPrepared(launch).execution).toBeUndefined();
  });

  it("passes the binding as the explicit override and keeps every prepared field", () => {
    const prepared = createInvokeRouter(agentLaunchHandlers());
    return prepared("agent_launch_prepare", {
      agentKey: "acme/backend/triage",
      team: TEAM_KEY,
      cwd: "/wt",
      sessionId: "s1",
    }).then((value) => {
      const launch = value as Parameters<typeof shellLaunchFromPrepared>[0];
      const bound = {
        ...launch,
        account: { ...launch.account, overrideAccountId: "acct-1", overrideAccountGeneration: 4 },
      };
      expect(shellLaunchFromPrepared(bound)).toEqual({
        agentPath: "/opt/bin/claude",
        args: launch.launch.args,
        mcpAllow: ["slack"],
        bridgeTools: null,
        extraArgs: launch.scope.extraArgs,
        envRemove: ["ANTHROPIC_API_KEY"],
        account: {
          runtime: "claude",
          overrideAccountId: "acct-1",
          overrideAccountGeneration: 4,
        },
        environmentBinding: undefined,
        title: "Slack triage",
      });
      for (const bridgeTools of [undefined, [], ["who_am_i"]]) {
        const captured = shellLaunchFromPrepared({
          ...bound,
          launch: {
            ...bound.launch,
            bridge_tools: bridgeTools,
            effective_bridge_tools: bridgeTools ?? [],
          },
        });
        expect(captured).toHaveProperty("bridgeTools", bridgeTools ?? null);
      }
      expect(
        shellLaunchFromPrepared({
          ...bound,
          launch: {
            ...bound.launch,
            bridge_tools: ["who_am_i", "not_registered"],
            effective_bridge_tools: ["who_am_i"],
          },
        }),
      ).toHaveProperty("bridgeTools", ["who_am_i"]);
      // A saved agent's env goes with the launch, for a relaunch to set again.
      const env = { CLAUDE_CODE_DISABLE_AUTO_MEMORY: "1" };
      expect(shellLaunchFromPrepared({ ...bound, launch: { ...launch.launch, env } }).env).toEqual(
        env,
      );
    });
  });
});

describe("a Start racing a routine's launch of the same agent (SW-AGR-2)", () => {
  it("reads the backend's already-starting refusal as the session to show", async () => {
    const { alreadyStarting } = await import("./dashboard-launch-controller");
    expect(alreadyStarting(new Error("already-starting: shell-wake-1"))).toBe("shell-wake-1");
    expect(alreadyStarting("already-starting: ")).toBe("");
    expect(alreadyStarting(new Error("invalid: no agent"))).toBeUndefined();
  });

  it("shows the session a routine is starting instead of starting a second agent", async () => {
    const route = invoke.router!;
    invoke.router = (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "agent_launch_prepare") {
        return Promise.reject(new Error("already-starting: shell-wake-1"));
      }
      return route(cmd, args);
    };
    await open({
      target: { kind: "dir", path: "/wt/existing", agent: "triage", session: "triage" },
      headed: false,
    });
    expect(prepareCalls).toHaveLength(1);
    expect(terminal.lines).toHaveLength(0);
    expect(bindCalls.map((b) => b.sessionId)).toEqual(["shell-wake-1"]);
  });
});

describe("adopting an existing tab for a named agent", () => {
  it("records no agent: an adopted tab's launch record went with the old process", async () => {
    const commands = await import("@/tauri/commands");
    vi.mocked(commands.agentsList).mockClear();
    terminal.existing = { workspaceId: "ws-x", sessionId: "shell-restored-triage" };
    running.set("shell-restored-triage", "claude");
    await open({
      target: { kind: "dir", path: "/wt/existing", agent: "triage", session: "triage" },
      headed: false,
    });
    await open({ target: { kind: "story", storyId: "SW-1", agent: "builtin/codex" } });
    expect(prepareCalls).toHaveLength(0);
    expect(bindCalls.map((b) => (b.scope as { agentRevision?: string }).agentRevision)).toEqual([
      undefined,
      undefined,
    ]);
    expect(vi.mocked(commands.agentsList)).not.toHaveBeenCalled();
  });

  it("treats an agent named controller like any other: a restored tab is adopted, recording no agent", async () => {
    const commands = await import("@/tauri/commands");
    vi.mocked(commands.agentsList).mockClear();
    const content = "/ctx/acme/backend/content";
    terminal.existing = { workspaceId: "ws-content", sessionId: "shell-restored-controller" };
    running.set("shell-restored-controller", "claude");
    await open({
      target: { kind: "dir", path: content, agent: "controller", session: "controller" },
      headed: false,
    });
    expect(prepareCalls).toHaveLength(0);
    expect(bindCalls[0]).toMatchObject({ sessionId: "shell-restored-controller" });
    expect((bindCalls[0].scope as { agentRevision?: string }).agentRevision).toBeUndefined();
    expect(vi.mocked(commands.agentsList)).not.toHaveBeenCalled();
  });

  it("never hands back a shell its agent quit: start-or-show closes it and starts the agent fresh", async () => {
    const store = useViewerStore.getState();
    const evicted: string[] = [];
    useViewerStore.setState({
      removeTerminalTabBySession: async (sessionId: string) => {
        evicted.push(sessionId);
        if (terminal.existing?.sessionId === sessionId) terminal.existing = null;
      },
      applyLayout: () => undefined,
    } as never);
    try {
      // The role tab is still there, but its agent quit: a bare, idle shell.
      terminal.existing = { workspaceId: "ws-x", sessionId: "shell-quit-triage" };
      await open({
        target: { kind: "dir", path: "/wt/existing", agent: "triage", session: "triage" },
        headed: false,
      });
    } finally {
      useViewerStore.setState({
        removeTerminalTabBySession: store.removeTerminalTabBySession,
        applyLayout: store.applyLayout,
      } as never);
    }
    expect(evicted).toEqual(["shell-quit-triage"]);
    expect(prepareCalls).toHaveLength(1);
    const [shell] = terminal.shells;
    expect(bindCalls[0].sessionId).toBe(shell.sessionId);
    expect(shell.sessionId).not.toBe("shell-quit-triage");
  });

  it("keeps a role tab running a person's own job: never closed out from under them", async () => {
    terminal.existing = { workspaceId: "ws-x", sessionId: "shell-busy-triage" };
    jobs.set("shell-busy-triage", { foreground: "npm", background: [] });
    await open({
      target: { kind: "dir", path: "/wt/existing", agent: "triage", session: "triage" },
      headed: false,
    });
    expect(prepareCalls).toHaveLength(0);
    expect(bindCalls[0].sessionId).toBe("shell-busy-triage");
  });
});

afterEach(cleanup);

it("local New terminal uses the real launch spine to bind, seed its saved task once and show its tab", async () => {
  const { NewTerminalChooser } = await import("@/features/viewer/components/new-terminal-chooser");
  const { NEW_WORKSPACE_TERMINAL_EVENT } =
    await import("@/features/viewer/lib/new-workspace-terminal");
  useViewerStore.setState({
    workspaces: {
      [TEAM_KEY]: [{ id: "existing-workspace", cwd: "/wt/existing", kind: "generic" } as never],
    },
    activeWorkspace: { [TEAM_KEY]: "existing-workspace" },
  });
  render(
    createElement(NewTerminalChooser, { teamKey: TEAM_KEY, workspaceId: "existing-workspace" }),
  );
  act(() =>
    window.dispatchEvent(
      new CustomEvent(NEW_WORKSPACE_TERMINAL_EVENT, {
        detail: { teamKey: TEAM_KEY, workspaceId: "existing-workspace" },
      }),
    ),
  );
  fireEvent.change(screen.getByLabelText("Saved Agent"), { target: { value: "triage" } });
  fireEvent.click(screen.getByTestId("new-terminal-where-local"));
  await waitFor(() => expect(terminal.shells).toHaveLength(1));
  const { sessionId } = terminal.shells[0];
  expect(prepareCalls[0]).toMatchObject({
    agentKey: "triage",
    cwd: "/wt/existing",
    team: TEAM_KEY,
    newInstance: true,
  });
  await waitFor(() => expect(bindCalls).toHaveLength(1));
  expect(bindCalls[0]).toMatchObject({
    sessionId,
    scope: { teamKey: TEAM_KEY, agentRevision: "acme/backend/triage@3" },
  });
  await waitFor(() => expect(terminal.seeds).toEqual([{ sessionId, text: "/triage-slack" }]));
  await waitFor(() =>
    expect(terminal.shown).toEqual([
      { teamKey: TEAM_KEY, workspaceId: "existing-workspace", sessionId },
    ]),
  );

  act(() =>
    window.dispatchEvent(
      new CustomEvent(NEW_WORKSPACE_TERMINAL_EVENT, {
        detail: { teamKey: TEAM_KEY, workspaceId: "existing-workspace" },
      }),
    ),
  );
  fireEvent.change(screen.getByLabelText("Saved Agent"), { target: { value: "triage" } });
  fireEvent.click(screen.getByTestId("new-terminal-where-local"));
  await waitFor(() => expect(terminal.shown).toHaveLength(2));
  expect(terminal.shells).toHaveLength(2);
  expect(terminal.shells[1].sessionId).not.toBe(sessionId);
  expect(prepareCalls.map((call) => call.newInstance)).toEqual([true, true]);
});

it("a delayed local New terminal places its tab without taking keyboard focus from the editor", async () => {
  const { agentLaunchPrepare } = await import("@/tauri/commands");
  const prepare = vi.mocked(agentLaunchPrepare).getMockImplementation()!;
  let release!: () => void;
  vi.mocked(agentLaunchPrepare).mockImplementationOnce(async (p) => {
    await new Promise<void>((resolve) => {
      release = resolve;
    });
    return prepare(p);
  });
  useViewerStore.setState({
    workspaces: {
      [TEAM_KEY]: [{ id: "existing-workspace", cwd: "/wt/existing", kind: "generic" } as never],
    },
    activeWorkspace: { [TEAM_KEY]: "existing-workspace" },
  });
  render(
    createElement(
      "div",
      null,
      createElement("textarea", { "aria-label": "Editor" }),
      createElement("textarea", { "aria-label": "Terminal", "data-test-terminal": true }),
    ),
  );
  const launching = launchAgentInWorkspace({
    teamKey: TEAM_KEY,
    workspaceKey: "existing-workspace",
    agent: "triage",
  });
  await waitFor(() => expect(release).toBeTypeOf("function"));
  screen.getByLabelText("Editor").focus();
  release();
  await launching;
  expect(terminal.shown).toHaveLength(1);
  expect(document.activeElement).toBe(screen.getByLabelText("Editor"));
});
