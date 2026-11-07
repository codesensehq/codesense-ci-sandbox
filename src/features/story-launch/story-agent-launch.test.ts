// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";

import type { DashboardLaunchDeps } from "@/features/dashboard/components/dashboard-launch";
import { resolveAgentCli, type AgentRow } from "@/tauri/commands";
import { toast } from "sonner";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { makeRealDashboardLaunchDeps } from "@/features/dashboard/components/dashboard-launch-controller";
import { getActiveWorkspaceContext } from "@/shared/hooks/use-active-workspace-context";
import { SHORTCUT_ACTIONS, findConflict } from "@/shared/lib/shortcuts/registry";
import { isReservedKeystroke } from "@/shared/lib/shortcuts/keystroke";
import {
  STORY_LAUNCH_LAUNCHER,
  activeStoryTarget,
  defaultLaunchAgent,
  launchDefaultAgentForActiveStory,
  launchAgentInStoryWorktree,
  missingCliHint,
  openStoryLaunchSheet,
  probeAgentCli,
  orderStoryLaunchAgents,
  groupStoryLaunchMenu,
  storyLaunchPrompt,
  type StoryLaunchAgent,
} from "./story-agent-launch";
import { preferredStarterRuntime, starterDefaultNote } from "@/features/agents/lib/agent-display";
import { useLaunchSheetStore } from "@/features/agents/lib/launch-sheet";

vi.mock("sonner", () => ({ toast: { info: vi.fn(), error: vi.fn(), success: vi.fn() } }));
vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  getActiveWorkspaceContext: vi.fn(() => ({ teamKey: "org:team", workspace: null })),
}));
vi.mock("@/shared/lib/team-key", async (orig) => ({
  ...(await orig<typeof import("@/shared/lib/team-key")>()),
  getActiveTeamKey: () => "org:team",
}));
vi.mock("@/features/dashboard/components/dashboard-launch-controller", () => ({
  makeRealDashboardLaunchDeps: vi.fn(() => {
    throw new Error("tests inject deps");
  }),
}));
// CLIs the dependency cache does not have.
const missingClis = vi.hoisted(() => ({ current: new Set<string>() }));
const listed = vi.hoisted(() => ({ rows: [] as unknown[] }));
vi.mock("@/tauri/commands", async (orig) => ({
  ...(await orig<typeof import("@/tauri/commands")>()),
  agentsList: vi.fn(async () => listed.rows),
  getDependencyStatus: vi.fn(async () => ({
    agents: Object.fromEntries(
      ["claude", "codex"].map((binary) => [
        binary,
        {
          available: !missingClis.current.has(binary),
          path: missingClis.current.has(binary) ? null : `/opt/bin/${binary}`,
          resolving: false,
        },
      ]),
    ),
  })),
  // The backend's launch-time lookup agrees with the cache unless a test says otherwise.
  resolveAgentCli: vi.fn(async (binary: string) =>
    missingClis.current.has(binary) ? null : `/opt/bin/${binary}`,
  ),
}));

function row(over: Partial<AgentRow>): AgentRow {
  return {
    key: "org/team/builder",
    name: "Builder",
    description: "",
    runtime: "claude",
    launch: "foreground",
    scope: "team",
    org: "org",
    team: "team",
    created_by: "you",
    created_by_you: true,
    archived: false,
    revision: 2,
    default_prompt: "",
    avatar: null,
    account: {
      source: "runtime_default",
      bound_account_id: null,
      resolved_source: "system_default",
      account_id: null,
      provider: "claude",
      label: null,
      reason: "optional-system-default",
      availability: "available",
      spawn_eligible: true,
      capture_raw_terminal: false,
    },
    used_by: [],
    routines: [],
    archived_routines: [],
    model: null,
    running_now: 0,
    ...over,
  };
}

const ROWS = [
  row({ key: "other/ops/zed", name: "Zed", org: "other", team: "ops" }),
  row({ key: "_personal/1/personal/mine", name: "Mine", scope: "personal" }),
  row({ key: "org/team/reviewer", name: "Reviewer", runtime: "codex" }),
  row({ key: "org/team/builder", name: "Builder" }),
  row({ key: "org/team/old", name: "Old", archived: true }),
];

/** The two starters as the backend lists them; `codexAccount` gives Codex (only) an active
 *  CLI login. */
function starters({ codexAccount = false } = {}): AgentRow[] {
  const starter = (runtime: "claude" | "codex", name: string, accountId: string | null) => {
    const base = row({});
    return row({
      key: `builtin/${runtime}`,
      name,
      runtime,
      scope: "builtin",
      org: "builtin",
      team: "",
      created_by: "CodeSense",
      created_by_you: false,
      revision: 13,
      account: { ...base.account, provider: runtime, account_id: accountId },
    });
  };
  return [
    starter("claude", "Claude", null),
    starter("codex", "Codex", codexAccount ? "codex-work" : null),
  ];
}

describe("orderStoryLaunchAgents", () => {
  it("lists the starters last, the one for your own CLI login first", () => {
    const ordered = orderStoryLaunchAgents([...starters(), ...ROWS], "org:team");
    expect(ordered.slice(-2).map((a) => [a.agentKey, a.place])).toEqual([
      ["builtin/claude", "built-in"],
      ["builtin/codex", "built-in"],
    ]);
    expect(ordered[0].agentKey).toBe("org/team/builder");
    const codexFirst = orderStoryLaunchAgents(starters({ codexAccount: true }), "org:team");
    expect(codexFirst.map((a) => a.agentKey)).toEqual(["builtin/codex", "builtin/claude"]);
    expect(codexFirst[0].isDefault).toBe(true);
  });

  it("defaults to the Claude starter by key, even when another built-in sorts before it", () => {
    const [claude] = starters();
    const reviewer = { ...claude, key: "builtin/a-reviewer", name: "A Reviewer" };
    const ordered = orderStoryLaunchAgents([reviewer, ...starters()], "org:team");
    expect(ordered.map((a) => a.agentKey)).toEqual([
      "builtin/claude",
      "builtin/a-reviewer",
      "builtin/codex",
    ]);
    expect(ordered[0].isDefault).toBe(true);
  });

  it("lists a team's own agent named controller like any other team agent", () => {
    const teams = row({ key: "org/team/controller", name: "Controller" });
    const keys = orderStoryLaunchAgents([teams, ...starters()], "org:team").map((a) => a.agentKey);
    expect(keys[0]).toBe("org/team/controller");
    expect(keys).toContain("builtin/claude");
  });

  it("prefers Codex only when Codex has an account and Claude has none, and says so", () => {
    const [claude, codex] = starters({ codexAccount: true });
    const withAccount = (r: AgentRow, id: string): AgentRow => ({
      ...r,
      account: { ...r.account, account_id: id },
    });
    expect(preferredStarterRuntime([claude, codex])).toBe("codex");
    expect(preferredStarterRuntime([withAccount(claude, "claude-work"), codex])).toBe("claude");
    expect(preferredStarterRuntime(starters())).toBe("claude");
    expect(preferredStarterRuntime([])).toBe("claude");
    expect(starterDefaultNote([claude, codex])).toContain(
      "start with Codex, since only Codex has an account set up",
    );
    expect(starterDefaultNote(starters())).toContain(
      "start with Claude, or Codex when only Codex has an account set up",
    );
  });

  it("lists the story team's agents, then your personal ones, then other teams', each by name", () => {
    const ordered = orderStoryLaunchAgents(ROWS, "org:team");
    expect(ordered.map((a) => a.agentKey)).toEqual([
      "org/team/builder",
      "org/team/reviewer",
      "_personal/1/personal/mine",
      "other/ops/zed",
    ]);
    expect(ordered.map((a) => a.place)).toEqual([null, null, "personal", "ops"]);
    expect(ordered.filter((a) => a.isDefault).map((a) => a.agentKey)).toEqual(["org/team/builder"]);
  });

  it("runs each agent's own runtime CLI and leaves archived agents out", () => {
    const ordered = orderStoryLaunchAgents(ROWS, "org:team");
    expect(ordered.find((a) => a.agentKey === "org/team/reviewer")).toMatchObject({
      runtime: "codex",
      binary: "codex",
    });
    expect(ordered.some((a) => a.agentKey === "org/team/old")).toBe(false);
  });

  it("offers nothing until the agent list is known", () => {
    expect(orderStoryLaunchAgents(null, "org:team")).toEqual([]);
  });
});

describe("groupStoryLaunchMenu", () => {
  it("lists one row per launchable agent and one install row per missing CLI", () => {
    const menu = groupStoryLaunchMenu(
      orderStoryLaunchAgents(ROWS, "org:team").map((a) => ({
        ...a,
        availability: a.binary === "claude" ? ("missing" as const) : ("available" as const),
      })),
    );
    expect(menu.agents.map((a) => a.agentKey)).toEqual(["org/team/reviewer"]);
    // Three Claude agents, one install row.
    expect(menu.missing).toEqual([
      { binary: "claude", label: "Install Claude Code — `claude` isn't on your PATH" },
    ]);
    // The shortcut runs the first available agent, so it labels that row.
    expect(menu.shortcutAgent?.agentKey).toBe("org/team/reviewer");
  });
});

describe("missingCliHint", () => {
  it("names the agent CLI", () => {
    expect(missingCliHint({ binary: "claude" })).toBe(
      "Install the Claude Code CLI (`claude` isn't on your login-shell PATH)",
    );
    expect(missingCliHint({ binary: "codex" })).toBe(
      "Install the Codex CLI (`codex` isn't on your login-shell PATH)",
    );
  });

  it("names an unknown CLI by its binary", () => {
    expect(missingCliHint({ binary: "aider" })).toBe(
      "Install `aider` (it isn't on your login-shell PATH)",
    );
  });
});

describe("storyLaunchPrompt", () => {
  it("names the story file (the same recipe a headless story run derives)", () => {
    expect(storyLaunchPrompt("/ctx/epic/S-1.md")).toBe(
      "Read the story at /ctx/epic/S-1.md and complete it, satisfying every acceptance criterion.",
    );
  });
});

function makeDeps(over: Partial<DashboardLaunchDeps> = {}) {
  const calls: string[] = [];
  const deps = {
    resolveStory: vi.fn(async () => {
      calls.push("resolveStory");
      return null;
    }),
    begin: vi.fn(async () => {
      calls.push("begin");
      return true;
    }),
    abort: vi.fn(async () => {
      calls.push("abort");
    }),
    killSession: vi.fn(async () => {}),
    ensureWorktree: vi.fn(async () => {
      calls.push("ensureWorktree");
      return "/wt/S-1";
    }),
    awaitEstablished: vi.fn(async () => {
      calls.push("awaitEstablished");
    }),
    connect: vi.fn(async (req: { agent?: { agent: string } }) => {
      calls.push("connect");
      return { sessionId: "sess-1", agentRevision: `${req.agent?.agent}@2` };
    }),
    bind: vi.fn(async () => {
      calls.push("bind");
    }),
    waitLive: vi.fn(async () => {
      calls.push("waitLive");
      return true;
    }),
    focus: vi.fn(() => {
      calls.push("focus");
    }),
    resolveSession: vi.fn(async () => null),
    injectLine: vi.fn(async () => {}),
    sendLine: vi.fn(async () => {
      calls.push("sendLine");
      return { submitted: true };
    }),
    isSessionParked: vi.fn(() => false),
    echoUserTurn: vi.fn(),
    probeAlive: vi.fn(async () => true),
    evictStaleTab: vi.fn(),
    closeByHandle: vi.fn(async () => {}),
    confirmLiveClose: vi.fn(async () => true),
    ...over,
  } as unknown as DashboardLaunchDeps;
  return { deps, calls };
}

function agent(over: Partial<StoryLaunchAgent> = {}): StoryLaunchAgent {
  return {
    agentKey: "org/team/builder",
    name: "Builder",
    runtime: "claude",
    binary: "claude",
    place: null,
    isDefault: true,
    availability: "available",
    ...over,
  };
}

describe("openStoryLaunchSheet (the story's Launch agent menu)", () => {
  it("opens the launch sheet on the story with the agent and the story task prefilled", async () => {
    useLaunchSheetStore.setState({ request: null });
    const read = vi.fn(async () => "---\nstory_id: SW-42\n---\n# Story");
    await openStoryLaunchSheet(
      "org:team",
      { storyFilePath: "/ctx/epic/sw-42-fix.md" },
      agent({ agentKey: "org/team/dev" }),
      read,
    );
    expect(useLaunchSheetStore.getState().request).toEqual({
      teamKey: "org:team",
      agentKey: "org/team/dev",
      storyId: "SW-42",
      task: storyLaunchPrompt("/ctx/epic/sw-42-fix.md"),
    });
  });

  it("refuses an agent whose CLI is missing with the install hint, opening nothing", async () => {
    useLaunchSheetStore.setState({ request: null });
    const missing = agent({ availability: "missing" });
    await expect(
      openStoryLaunchSheet("org:team", { storyFilePath: "/ctx/S-1.md", storyId: "S-1" }, missing),
    ).rejects.toThrow(missingCliHint(missing));
    expect(useLaunchSheetStore.getState().request).toBeNull();
  });
});

describe("launchAgentInStoryWorktree (the dashboard openWorkspace story spine)", () => {
  it("ensures the story worktree, waits for it to settle, starts the agent there and seeds the story prompt", async () => {
    const { deps, calls } = makeDeps();
    const result = await launchAgentInStoryWorktree(
      "org:team",
      { storyFilePath: "/ctx/epic/S-1.md", storyId: "S-1" },
      agent(),
      { deps },
    );

    expect(result).toMatchObject({ sessionId: "sess-1", promptDelivered: true });
    // The surface already knows the story's managed path — no frontmatter lookup.
    expect(deps.resolveStory).not.toHaveBeenCalled();
    expect(deps.ensureWorktree).toHaveBeenCalledWith("org:team", "/ctx/epic/S-1.md");
    expect(deps.awaitEstablished).toHaveBeenCalledWith("/wt/S-1");
    expect(deps.connect).toHaveBeenCalledWith(
      expect.objectContaining({
        teamKey: "org:team",
        cwd: "/wt/S-1",
        storyFilePath: "/ctx/epic/S-1.md",
        agent: { agent: "org/team/builder", prompt: storyLaunchPrompt("/ctx/epic/S-1.md") },
      }),
    );
    expect(deps.bind).toHaveBeenCalledWith("S-1", "sess-1", {
      teamKey: "org:team",
      agentRevision: "org/team/builder@2",
    });
    expect(deps.focus).toHaveBeenCalledWith("org:team", "sess-1", "/ctx/epic/S-1.md");
    expect(calls.indexOf("ensureWorktree")).toBeLessThan(calls.indexOf("awaitEstablished"));
    expect(calls.indexOf("awaitEstablished")).toBeLessThan(calls.indexOf("connect"));
  });

  it("names a Codex agent by its key: never a pack default", async () => {
    const { deps } = makeDeps();
    await launchAgentInStoryWorktree(
      "org:team",
      { storyFilePath: "/ctx/epic/S-1.md", storyId: "S-1" },
      agent({ agentKey: "org/team/reviewer", runtime: "codex", binary: "codex" }),
      { deps },
    );
    expect(deps.connect).toHaveBeenCalledWith(
      expect.objectContaining({ agent: expect.objectContaining({ agent: "org/team/reviewer" }) }),
    );
  });

  it("a launch behind a maximized panel says so, with a Show that restores the layout", async () => {
    useViewerStore.setState({ maximizedPanel: { "org:team": "weft-board" } });
    try {
      const { deps } = makeDeps();
      await launchAgentInStoryWorktree(
        "org:team",
        { storyFilePath: "/ctx/epic/TR-2.md", storyId: "TR-2" },
        agent({ name: "Reviewer" }),
        { deps },
      );
      expect(toast.success).toHaveBeenCalledWith(
        "Reviewer is running on TR-2",
        expect.objectContaining({ action: expect.objectContaining({ label: "Show" }) }),
      );
      const opts = vi.mocked(toast.success).mock.calls.at(-1)?.[1] as unknown as {
        action: { onClick: () => void };
      };
      opts.action.onClick();
      expect(useViewerStore.getState().maximizedPanel["org:team"]).toBeUndefined();
    } finally {
      useViewerStore.setState({ maximizedPanel: {} });
      vi.mocked(toast.success).mockClear();
    }
  });

  it("a launch with nothing maximized adds no toast (the terminal is in view)", async () => {
    vi.mocked(toast.success).mockClear();
    const { deps } = makeDeps();
    await launchAgentInStoryWorktree(
      "org:team",
      { storyFilePath: "/ctx/epic/S-1.md", storyId: "S-1" },
      agent(),
      { deps },
    );
    expect(toast.success).not.toHaveBeenCalled();
  });

  it("reads the frontmatter story_id when the surface only knows the path", async () => {
    const { deps } = makeDeps();
    const readFile = vi.fn(async () => "---\nstory_id: S-9\ntitle: x\n---\n# body\n");
    await launchAgentInStoryWorktree(
      "org:team",
      { storyFilePath: "/ctx/epic/S-9-thing.md", storyId: null },
      agent(),
      { deps, readFile },
    );
    expect(readFile).toHaveBeenCalledWith("/ctx/epic/S-9-thing.md");
    expect(deps.bind).toHaveBeenCalledWith("S-9", "sess-1", expect.anything());
  });

  it("falls back to the file stem when the story has no readable frontmatter id", async () => {
    const { deps } = makeDeps();
    await launchAgentInStoryWorktree(
      "org:team",
      { storyFilePath: "/ctx/epic/S-9-thing.md" },
      agent(),
      { deps, readFile: async () => Promise.reject(new Error("gone")) },
    );
    expect(deps.bind).toHaveBeenCalledWith("S-9-thing", "sess-1", expect.anything());
  });

  it("refuses a missing CLI before touching the worktree", async () => {
    const { deps } = makeDeps();
    await expect(
      launchAgentInStoryWorktree(
        "org:team",
        { storyFilePath: "/ctx/epic/S-1.md", storyId: "S-1" },
        agent({ availability: "missing", binary: "codex", runtime: "codex" }),
        { deps },
      ),
    ).rejects.toThrow(/codex/);
    expect(deps.ensureWorktree).not.toHaveBeenCalled();
    expect(deps.begin).not.toHaveBeenCalled();
  });

  it("records the story-launch surface as the session's launcher", () => {
    expect(STORY_LAUNCH_LAUNCHER).toBe("virtual://story-launch");
  });
});

describe("default-launch shortcut", () => {
  it("is a registered, team-scoped, conflict-free shortcut", () => {
    const action = SHORTCUT_ACTIONS.find((a) => a.id === "story.launch-agent");
    expect(action).toMatchObject({ scope: "team", defaultKeystroke: "CmdOrCtrl+Shift+A" });
    expect(isReservedKeystroke("CmdOrCtrl+Shift+A")).toBeNull();
    expect(findConflict("CmdOrCtrl+Shift+A", {}, "story.launch-agent")).toBeNull();
  });

  it("targets the focused story workspace", async () => {
    vi.mocked(getActiveWorkspaceContext).mockReturnValueOnce({
      teamKey: "org:team",
      workspace: { kind: "story", storyFilePath: "/ctx/e/S-2.md", storyId: "S-2" },
    } as never);
    await expect(activeStoryTarget("org:team")).resolves.toEqual({
      storyFilePath: "/ctx/e/S-2.md",
      storyId: "S-2",
    });
  });

  it("else targets the open tab only when it is a story file with a story_id", async () => {
    useViewerStore.setState({ activeTab: { "org:team": "/ctx/e/S-3.md" } });
    await expect(
      activeStoryTarget("org:team", async () => "---\nstory_id: S-3\n---\n"),
    ).resolves.toEqual({ storyFilePath: "/ctx/e/S-3.md", storyId: "S-3" });
    await expect(activeStoryTarget("org:team", async () => "# notes\n")).resolves.toBeNull();

    useViewerStore.setState({ activeTab: { "org:team": "/ctx/e/README.md" } });
    await expect(
      activeStoryTarget("org:team", async () => "---\nstory_id: S-3\n---\n"),
    ).resolves.toBeNull();
  });

  it("tells the user to open a story when nothing launchable is in focus", async () => {
    useViewerStore.setState({ activeTab: {} });
    await launchDefaultAgentForActiveStory();
    expect(toast.info).toHaveBeenCalledWith("Open a story to launch an agent in its worktree");
  });
});

describe("default-launch shortcut — the launch itself", () => {
  it("launches the menu's first agent, from a fresh read of the list", async () => {
    listed.rows = ROWS;
    await expect(defaultLaunchAgent("org:team")).resolves.toMatchObject({
      agentKey: "org/team/builder",
      availability: "available",
    });
    listed.rows = [row({ key: "_personal/1/personal/mine", name: "Mine", scope: "personal" })];
    await expect(defaultLaunchAgent("org:team")).resolves.toMatchObject({
      agentKey: "_personal/1/personal/mine",
    });
  });

  it("with no agents of your own, launches the starter for your CLI login, else Claude", async () => {
    listed.rows = starters();
    await expect(defaultLaunchAgent("org:team")).resolves.toMatchObject({
      agentKey: "builtin/claude",
      binary: "claude",
      availability: "available",
    });
    listed.rows = starters({ codexAccount: true });
    await expect(defaultLaunchAgent("org:team")).resolves.toMatchObject({
      agentKey: "builtin/codex",
      binary: "codex",
    });
    // Its CLI missing, the other starter's is used.
    missingClis.current = new Set(["codex"]);
    try {
      await expect(defaultLaunchAgent("org:team")).resolves.toMatchObject({
        agentKey: "builtin/claude",
      });
    } finally {
      missingClis.current = new Set();
    }
  });

  it("launches the default agent on the focused story workspace", async () => {
    listed.rows = ROWS;
    vi.mocked(getActiveWorkspaceContext).mockReturnValueOnce({
      teamKey: "org:team",
      workspace: { kind: "story", storyFilePath: "/ctx/e/S-2.md", storyId: "S-2" },
    } as never);
    const { deps } = makeDeps();
    vi.mocked(makeRealDashboardLaunchDeps).mockReturnValueOnce(deps);
    vi.mocked(toast.error).mockClear();

    await launchDefaultAgentForActiveStory();

    expect(deps.ensureWorktree).toHaveBeenCalledWith("org:team", "/ctx/e/S-2.md");
    expect(deps.bind).toHaveBeenCalledWith("S-2", "sess-1", {
      teamKey: "org:team",
      agentRevision: "org/team/builder@2",
    });
    expect(toast.error).not.toHaveBeenCalled();
  });

  it("a missing CLI never wins over an available one", async () => {
    listed.rows = ROWS;
    missingClis.current = new Set(["claude"]);
    await expect(defaultLaunchAgent("org:team")).resolves.toMatchObject({
      agentKey: "org/team/reviewer",
      availability: "available",
    });
    missingClis.current = new Set();
  });

  it("with no agent CLI on PATH, the launch says what to install", async () => {
    listed.rows = ROWS;
    missingClis.current = new Set(["claude", "codex"]);
    await expect(defaultLaunchAgent("org:team")).resolves.toMatchObject({
      agentKey: "org/team/builder",
      availability: "missing",
    });
    vi.mocked(getActiveWorkspaceContext).mockReturnValueOnce({
      teamKey: "org:team",
      workspace: { kind: "story", storyFilePath: "/ctx/e/S-2.md", storyId: "S-2" },
    } as never);
    vi.mocked(toast.error).mockClear();
    await launchDefaultAgentForActiveStory();
    expect(toast.error).toHaveBeenCalledWith(
      "Couldn't launch agent: Install the Claude Code CLI (`claude` isn't on your login-shell PATH)",
    );
    missingClis.current = new Set();
  });
});

describe("probeAgentCli", () => {
  it("takes a cached available CLI without asking the backend", async () => {
    missingClis.current = new Set();
    vi.mocked(resolveAgentCli).mockClear();
    expect(await probeAgentCli({ binary: "claude" })).toBe("available");
    expect(resolveAgentCli).not.toHaveBeenCalled();
  });

  it("asks the backend about a cached unavailable CLI and trusts a found path", async () => {
    missingClis.current = new Set(["claude"]);
    vi.mocked(resolveAgentCli).mockClear();
    vi.mocked(resolveAgentCli).mockResolvedValueOnce("/opt/bin/claude");
    try {
      expect(await probeAgentCli({ binary: "claude" })).toBe("available");
      expect(resolveAgentCli).toHaveBeenCalledTimes(1);
      expect(resolveAgentCli).toHaveBeenCalledWith("claude");
    } finally {
      missingClis.current = new Set();
    }
  });

  it("is missing when the backend finds no CLI either", async () => {
    missingClis.current = new Set(["claude"]);
    vi.mocked(resolveAgentCli).mockClear();
    vi.mocked(resolveAgentCli).mockResolvedValueOnce(null);
    try {
      expect(await probeAgentCli({ binary: "claude" })).toBe("missing");
      expect(resolveAgentCli).toHaveBeenCalledTimes(1);
    } finally {
      missingClis.current = new Set();
    }
  });

  it("is missing when the backend lookup fails", async () => {
    missingClis.current = new Set(["claude"]);
    vi.mocked(resolveAgentCli).mockRejectedValueOnce(new Error("ipc down"));
    try {
      expect(await probeAgentCli({ binary: "claude" })).toBe("missing");
    } finally {
      missingClis.current = new Set();
    }
  });
});
