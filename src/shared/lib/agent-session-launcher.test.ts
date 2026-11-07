// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import {
  buildAgentLaunchCommand,
  agentLaunchLine,
  connectAgentSession,
  reconnectAgentSession,
  waitForAgentLive,
  agentBasename,
  GUARDED_ENV_STRIP,
  makeRealLaunchDeps,
  type LaunchDeps,
  type RespawnRequest,
  relaunchAgentSession,
  IN_FLIGHT_STALE_MS,
} from "@/shared/lib/agent-session-launcher";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  seedTerminalTabs,
  terminalHost,
  installTerminalLayoutHarness,
} from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();

describe("buildAgentLaunchCommand (AC-2/AC-7 · SW-AR-1)", () => {
  it.each([undefined, null, []])(
    "R6 initial launch inserts flags before operands (allowlist %j)",
    (mcpAllow) => {
      const prompt = "Review 'quotes'\n  and $VARIABLE verbatim";
      expect(
        agentLaunchLine(
          {
            agentPath: "/bin/codex",
            args: ["exec", "--", prompt],
            shell: { cwd: "/wt", envRemove: [], mcpAllow },
          },
          ["-c", "scope=1"],
        ),
      ).toBe(buildAgentLaunchCommand("/bin/codex", ["exec", "-c", "scope=1", "--", prompt]));
    },
  );

  it("never emits -p / --print (must stay an interactive subscription TUI)", () => {
    const line = buildAgentLaunchCommand("/opt/homebrew/bin/claude");
    expect(line).not.toMatch(/\s-p\b/);
    expect(line).not.toContain("--print");
    expect(line).toContain("claude");
  });

  it("SW-AR-1: appends --dangerously-skip-permissions so the agent uses all tools without prompts", () => {
    expect(buildAgentLaunchCommand("/opt/homebrew/bin/claude")).toBe(
      "'/opt/homebrew/bin/claude' '--dangerously-skip-permissions'",
    );
  });

  it("shell-quotes the path so spaces survive", () => {
    expect(buildAgentLaunchCommand("/Apps/My Tools/claude")).toBe(
      "'/Apps/My Tools/claude' '--dangerously-skip-permissions'",
    );
  });

  // SW-AN-7 (AC-1): args are a PARAMETER — the default profile retires the blanket
  // flag; an escalating profile carries it; a model swap rides args.
  it("SW-AN-7: the default profile (empty args) launches plain `claude` — no flag", () => {
    expect(buildAgentLaunchCommand("/opt/homebrew/bin/claude", [])).toBe(
      "'/opt/homebrew/bin/claude'",
    );
  });

  it("SW-AN-7: an explicit skip-permissions profile appends its flag", () => {
    expect(buildAgentLaunchCommand("/bin/claude", ["--dangerously-skip-permissions"])).toBe(
      "'/bin/claude' '--dangerously-skip-permissions'",
    );
  });

  it("SW-AN-7: a model-swap profile appends its args (still no -p)", () => {
    const line = buildAgentLaunchCommand("/bin/claude", ["--model", "claude-opus-4-6"]);
    expect(line).toBe("'/bin/claude' '--model' 'claude-opus-4-6'");
    expect(line).not.toMatch(/\s-p\b/);
  });

  it("SW-AN-7: omitting args KEEPS the permissive default (git-sync/notes/branch unchanged)", () => {
    expect(buildAgentLaunchCommand("/bin/claude")).toBe(
      "'/bin/claude' '--dangerously-skip-permissions'",
    );
  });
});

describe("connectAgentSession orchestration (AC-1/2/3/5/7)", () => {
  function makeDeps(over: Partial<LaunchDeps> = {}): {
    deps: LaunchDeps;
    calls: string[];
    spawnOpts: unknown[];
  } {
    const calls: string[] = [];
    const spawnOpts: unknown[] = [];
    const deps: LaunchDeps = {
      findExisting: vi.fn(async () => undefined),
      provision: vi.fn(async () => {
        calls.push("provision");
      }),
      openTab: vi.fn(async (_team, workspaceId, tab, shell) => {
        calls.push(`openTab:${tab.role}@${workspaceId}`);
        spawnOpts.push(shell);
      }),
      closeTab: vi.fn(() => {
        calls.push("closeTab");
      }),
      launchAgentLine: vi.fn(async () => {
        calls.push("launchAgentLine");
        return { ok: true } as const;
      }),
      projectScopeConfig: vi.fn(async (_sid: string, _cwd: string, allow: string[] | null) => {
        calls.push("projectScopeConfig");
        // SW-AN-43: the command returns the RENDERED flags (raw path — the launch
        // builder shell-quotes each token), not a bare path. With no allowlist this fake
        // adds nothing (a pack with no open mode).
        return {
          extraArgs:
            allow === null
              ? []
              : ["--mcp-config", "/tmp/mcp-scope/sess.json", "--strict-mcp-config"],
          environmentBinding: null,
        };
      }),
      ...over,
    };
    return { deps, calls, spawnOpts };
  }

  const params = {
    teamKey: "acme:default",
    orgRoot: "/ctx/root",
    agentPath: "/bin/claude",
    role: "test-agent",
    workspaceKey: "test-agent",
  } as const;

  it("uses the prepared canonical product root without generic ancestor provisioning", async () => {
    const { deps, spawnOpts } = makeDeps();
    await connectAgentSession(
      {
        ...params,
        prepare: async () => ({
          agentPath: "/bin/claude",
          cwd: "/canonical/product",
          args: [],
          mcpAllow: [],
          extraArgs: [],
          envRemove: [],
        }),
      },
      deps,
    );
    expect(deps.provision).not.toHaveBeenCalled();
    expect(spawnOpts[0]).toMatchObject({ cwd: "/canonical/product" });
  });

  it("does not open a process when named product preparation fails", async () => {
    const { deps } = makeDeps();
    await expect(
      connectAgentSession(
        {
          ...params,
          prepare: async () => {
            throw new Error("product startup preparation failed: conflicting document");
          },
        },
        deps,
      ),
    ).rejects.toThrow("product startup preparation failed");
    expect(deps.openTab).not.toHaveBeenCalled();
    expect(deps.launchAgentLine).not.toHaveBeenCalled();
    expect(deps.provision).not.toHaveBeenCalled();
  });

  it("runs provision → openTab → launch in order", async () => {
    const { deps, calls } = makeDeps();
    await connectAgentSession(params, deps);
    expect(calls).toEqual([
      "provision",
      "projectScopeConfig",
      "openTab:test-agent@test-agent",
      "launchAgentLine",
    ]);
  });

  it("AC-1: when an owned tab already exists, returns it and never spawns a duplicate", async () => {
    const existing = { workspaceId: "test-agent", sessionId: "already" };
    const { deps, calls } = makeDeps({ findExisting: vi.fn(async () => existing) });
    const res = await connectAgentSession(params, deps);
    expect(res).toEqual(existing);
    expect(calls).toEqual([]); // no provision/open/launch
    expect(deps.openTab).not.toHaveBeenCalled();
  });

  it("M3: two CONCURRENT launches share one launch (no duplicate spawn/agent)", async () => {
    const { deps } = makeDeps();
    const [a, b] = await Promise.all([
      connectAgentSession(params, deps),
      connectAgentSession(params, deps),
    ]);
    expect(a).toEqual(b); // same result
    expect(deps.openTab).toHaveBeenCalledTimes(1); // exactly one terminal + agent
  });

  it("a launch stuck in flight never wedges the next start: past its window a new call launches", async () => {
    let now = 1_000;
    const stuck = makeDeps({
      // A prepare/inject that never settles.
      findExisting: vi.fn(() => new Promise<undefined>(() => {})),
    });
    const hung = connectAgentSession({ ...params, role: "stuck" }, stuck.deps, () => now);
    void hung;
    // Inside the window a second start shares the in-flight one.
    expect(connectAgentSession({ ...params, role: "stuck" }, stuck.deps, () => now)).toBe(hung);
    now += IN_FLIGHT_STALE_MS;
    const fresh = makeDeps();
    const res = await connectAgentSession({ ...params, role: "stuck" }, fresh.deps, () => now);
    expect(res.workspaceId).toBe("test-agent");
    expect(fresh.deps.openTab).toHaveBeenCalledTimes(1);
  });

  it("opens at the cwd with ANTHROPIC_API_KEY stripped (AC-7) under the owned workspace key", async () => {
    const { deps, spawnOpts } = makeDeps();
    await connectAgentSession(params, deps);
    expect(deps.openTab).toHaveBeenCalledWith(
      "acme:default",
      "test-agent",
      expect.objectContaining({ role: "test-agent" }),
      expect.objectContaining({ cwd: "/ctx/root", envRemove: GUARDED_ENV_STRIP }),
    );
    expect(spawnOpts[0]).toMatchObject({ envRemove: ["ANTHROPIC_API_KEY"] });
  });

  it("AC-3: the primitive is generic — a NON-git-sync binding connects unchanged", async () => {
    const { deps, calls } = makeDeps();
    // A story-agent binding: only the params differ; the engine is identical.
    const res = await connectAgentSession(
      {
        teamKey: "acme:default",
        orgRoot: "/work/story-x",
        agentPath: "/bin/codex",
        role: "story-x",
        workspaceKey: "story-x",
      },
      deps,
    );
    expect(calls).toEqual([
      "provision",
      "projectScopeConfig",
      "openTab:story-x@story-x",
      "launchAgentLine",
    ]);
    expect(deps.launchAgentLine).toHaveBeenCalledWith(
      res.sessionId,
      "'/bin/codex' '--dangerously-skip-permissions'",
    );
  });

  it("throws loudly when the shell never reaches its prompt to launch the agent", async () => {
    const { deps } = makeDeps({
      launchAgentLine: vi.fn(async () => ({ ok: false, reason: "agent-busy" }) as const),
    });
    await expect(connectAgentSession(params, deps)).rejects.toThrow(/never reached its prompt/i);
  });

  it("ends the owned tab when launch fails, so a retry relaunches (no orphan)", async () => {
    const { deps, calls } = makeDeps({
      openTab: vi.fn(async () => {
        calls.push("openTab");
        throw new Error("open failed");
      }),
    });
    await expect(connectAgentSession(params, deps)).rejects.toThrow(/open failed/);
    // The half-built tab is ended so findExisting can't re-bind a dead orphan.
    expect(deps.closeTab).toHaveBeenCalledTimes(1);
    expect(calls).toEqual(["provision", "projectScopeConfig", "openTab", "closeTab"]);
  });

  it("a launch that never reached the prompt keeps no launch spec", async () => {
    useAgentStateStore.setState({ launchBySession: {} });
    const { deps } = makeDeps({
      launchAgentLine: vi.fn(async () => ({ ok: false, reason: "agent-busy" }) as const),
    });
    await expect(connectAgentSession(params, deps)).rejects.toThrow();
    expect(useAgentStateStore.getState().launchBySession).toEqual({});
  });

  it("keeps the launch failure when opening never placed its session", async () => {
    const { deps } = makeDeps({
      openTab: vi.fn(async () => {
        throw new Error("open failed");
      }),
      closeTab: vi.fn(async (sessionId) => {
        throw `layout: no entry for session ${sessionId}`;
      }),
    });
    await expect(connectAgentSession(params, deps)).rejects.toThrow("open failed");
    expect(deps.closeTab).toHaveBeenCalledTimes(1);
  });

  it("surfaces other cleanup failures even when opening failed", async () => {
    const { deps } = makeDeps({
      openTab: vi.fn(async () => {
        throw new Error("open failed");
      }),
      closeTab: vi.fn(async () => {
        throw new Error("host unreachable");
      }),
    });
    await expect(connectAgentSession(params, deps)).rejects.toThrow("host unreachable");
  });

  it("surfaces an unknown-session cleanup failure after the tab opened", async () => {
    const { deps } = makeDeps({
      launchAgentLine: vi.fn(async () => ({ ok: false, reason: "agent-busy" }) as const),
      closeTab: vi.fn(async (sessionId) => {
        throw `layout: no entry for session ${sessionId}`;
      }),
    });
    await expect(connectAgentSession(params, deps)).rejects.toMatch(
      /^layout: no entry for session /,
    );
  });

  it("a default launch gets its per-session config, projected for the CLI it runs", async () => {
    const { deps } = makeDeps();
    const res = await connectAgentSession(params, deps);
    // `null`: no allowlist; the CLI's basename picks the runtime.
    expect(deps.projectScopeConfig).toHaveBeenCalledWith(
      res.sessionId,
      "/ctx/root",
      null,
      undefined,
      undefined,
      {
        binary: "claude",
        defaultArgs: true,
      },
    );
  });

  it("an unscoped launch with the launcher's default args keeps them before its config", async () => {
    let launchLine = "";
    const { deps } = makeDeps({
      projectScopeConfig: vi.fn(async () => ({
        extraArgs: ["--settings", "/tmp/s/hooks.json"],
        environmentBinding: null,
      })),
      launchAgentLine: vi.fn(async (_sid: string, line: string) => {
        launchLine = line;
        return { ok: true } as const;
      }),
    });
    await connectAgentSession(params, deps);
    expect(launchLine).toBe(
      "'/bin/claude' '--dangerously-skip-permissions' '--settings' '/tmp/s/hooks.json'",
    );
  });

  it("keeps backend-selected argv through the real launch cache and direct relaunch", async () => {
    const execution = {
      argv: ["/bin/hermes", "chat", "--tui", "--query=quotes ' and -- stay literal"],
      nativeLaunch: "hermes" as const,
    };
    const { deps, spawnOpts } = makeDeps();
    const opened = await connectAgentSession(
      {
        ...params,
        role: "native-receipt-agent",
        prepare: async () => ({
          agentPath: "/bin/hermes",
          args: execution.argv.slice(1),
          execution,
          mcpAllow: ["codesense"],
          extraArgs: [],
          envRemove: [],
          environmentBinding: "initial-binding",
        }),
      },
      deps,
    );
    expect(spawnOpts[0]).toEqual(
      expect.objectContaining({ execution, environmentBinding: "initial-binding" }),
    );
    expect(deps.launchAgentLine).not.toHaveBeenCalled();
    const cached = useAgentStateStore.getState().launchBySession[opened.sessionId].spec;
    expect([cached.agentPath, ...(cached.args ?? [])]).toEqual(execution.argv);
    const fresh = { ...execution, argv: ["/fresh/hermes", ...execution.argv.slice(1)] };
    const respawn = vi.fn(async (_request: RespawnRequest) => ({
      extraArgs: ["must-never-be-injected"],
      execution: fresh,
    }));
    const inject = { launchAgentLine: vi.fn(async () => ({ ok: true as const })) };
    await expect(relaunchAgentSession(opened.sessionId, cached, respawn, inject)).resolves.toEqual({
      ok: true,
    });
    expect(respawn.mock.calls[0][0].scope?.launchArgs).toEqual(execution.argv.slice(1));
    expect(inject.launchAgentLine).not.toHaveBeenCalled();
    const relaunched = useAgentStateStore.getState().launchBySession[opened.sessionId].spec;
    expect(relaunched.agentPath).toBe("/fresh/hermes");
    expect(relaunched.args).toEqual(execution.argv.slice(1));
  });

  it("a prepared launch spawns with exactly the backend's env strip, account and binding", async () => {
    let launchLine = "";
    const { deps, calls } = makeDeps({
      launchAgentLine: vi.fn(async (_sid: string, line: string) => {
        launchLine = line;
        return { ok: true } as const;
      }),
    });
    const account = { runtime: "codex" as const, overrideAccountId: "codex-account" };
    await connectAgentSession(
      {
        ...params,
        prepare: async () => ({
          agentPath: "/bin/codex",
          args: ["--model", "m"],
          mcpAllow: ["codesense"],
          ...{ bridgeTools: [] },
          extraArgs: [],
          envRemove: ["OPENAI_API_KEY"],
          account,
          environmentBinding: "selected-codex-scope",
        }),
      },
      deps,
    );
    // The backend already projected the scope for the session: nothing is projected here.
    expect(calls).not.toContain("projectScopeConfig");
    expect(deps.openTab).toHaveBeenCalledWith(
      "acme:default",
      "test-agent",
      expect.anything(),
      expect.objectContaining({
        cwd: "/ctx/root",
        envRemove: ["OPENAI_API_KEY"],
        account,
        environmentBinding: "selected-codex-scope",
      }),
    );
    expect(launchLine).toBe("'/bin/codex' '--model' 'm'");
    expect(Object.values(useAgentStateStore.getState().launchBySession)).toContainEqual(
      expect.objectContaining({
        spec: expect.objectContaining({ shell: expect.objectContaining({ bridgeTools: [] }) }),
      }),
    );
  });

  it("a FAILED launch ends its session, so its per-session config can't linger", async () => {
    // Ending the session is what reaps the launch's 0600 config (keyed by it).
    const { deps } = makeDeps({
      launchAgentLine: vi.fn(async () => ({ ok: false, reason: "agent-busy" }) as const),
    });
    await expect(connectAgentSession(params, deps)).rejects.toThrow();
    const projectedSid = vi.mocked(deps.projectScopeConfig).mock.calls[0][0];
    expect(deps.closeTab).toHaveBeenCalledWith(projectedSid);
  });
});

describe("makeRealLaunchDeps — adopting an agent that outlived the app", () => {
  const team = "acme:default";
  const params = {
    teamKey: team,
    orgRoot: "/ctx/root",
    agentPath: "/bin/claude",
    role: "test-agent",
    workspaceKey: "test-agent",
  } as const;

  function realDeps() {
    const provision = vi.fn(async () => {});
    return { provision, deps: makeRealLaunchDeps(provision) };
  }

  it("adopts the host's surviving role tab instead of opening another", async () => {
    seedTerminalTabs(team, "test-agent", [{ session: "survivor", role: "test-agent" }]);
    const { deps, provision } = realDeps();
    const res = await connectAgentSession(params, deps);
    expect(res).toEqual({ workspaceId: "test-agent", sessionId: "survivor" });
    expect(provision).not.toHaveBeenCalled();
    expect(terminalHost().opened).toEqual([]);
  });

  it("waits for the hosts' layout before deciding there is no role tab", async () => {
    useViewerStore.setState({
      terminalLayout: { hydrated: false, admission: {}, members: {}, byKey: {} },
    });
    const { deps } = realDeps();
    let found: unknown = "pending";
    const finding = deps.findExisting(team, "test-agent").then((hit) => (found = hit));
    await Promise.resolve();
    expect(found).toBe("pending");

    terminalHost().seed(
      `${team}|test-agent`,
      { displayName: "test-agent", kind: "generic" },
      { session: "survivor", role: "test-agent" },
    );
    useViewerStore.getState().applyLayout({ kind: "snapshot", replies: [terminalHost().reply()] });
    await finding;
    expect(found).toEqual({ workspaceId: "test-agent", sessionId: "survivor" });
  });
});

describe("reconnectAgentSession (AC-1/AC-2)", () => {
  // Default: agent is DEAD (probeLive → false) so the relaunch path is exercised
  // unless a test overrides probeLive to simulate a still-live agent.
  function reconnectDeps(over = {}) {
    return {
      probeLive: vi.fn(async () => false),
      relaunch: vi.fn(async (_s: string, _l: string) => ({ ok: true }) as const),
      waitLive: vi.fn(async () => true),
      sendReorient: vi.fn(async (_s: string, _l: string) => ({ ok: true }) as const),
      projectScopeConfig: vi.fn(async () => ({
        extraArgs: ["--mcp-config", "/tmp/s/fresh.json"],
        environmentBinding: null,
      })),
      ...over,
    };
  }

  it("AC-1 (dead, nothing recorded): relaunches with a fresh per-session config for its dir", async () => {
    const deps = reconnectDeps();
    await reconnectAgentSession(
      {
        sessionId: "s-after-restart",
        agentPath: "/bin/claude",
        reorientCommand: "/cs-status",
        cwd: "/notes",
      },
      deps,
    );
    expect(deps.projectScopeConfig).toHaveBeenCalledWith(
      "s-after-restart",
      "/notes",
      null,
      undefined,
      [],
      { binary: "claude", defaultArgs: true },
    );
    expect(deps.relaunch).toHaveBeenCalledWith(
      "s-after-restart",
      "'/bin/claude' '--dangerously-skip-permissions' '--mcp-config' '/tmp/s/fresh.json'",
    );
  });

  it("AC-1 (dead): relaunches a FRESH context (no --resume), waits for liveness, then re-orients", async () => {
    const deps = reconnectDeps();
    const res = await reconnectAgentSession(
      { sessionId: "s1", agentPath: "/bin/claude", reorientCommand: "/cs-status" },
      deps,
    );
    const relaunch = deps.relaunch.mock.calls[0][1];
    expect(relaunch).not.toContain("--resume");
    expect(relaunch).toBe("'/bin/claude' '--dangerously-skip-permissions'");
    // re-orient only AFTER the liveness probe confirms the fresh agent is up
    expect(deps.waitLive).toHaveBeenCalledWith("s1", "claude");
    expect(deps.sendReorient).toHaveBeenCalledWith("s1", "/cs-status");
    expect(res).toEqual({ reattached: false });
  });

  it("AC-1 (dead): relaunches with the line it was launched with, its session config included", async () => {
    const line =
      "'/bin/claude' '--mcp-config' '/tmp/s/bridge.json' '--settings' '/tmp/s/hooks.json'";
    useAgentStateStore.getState().recordLaunch("s-recorded", {
      line,
      agent: "claude",
      spec: { agentPath: "/bin/claude" },
    });
    const deps = reconnectDeps();
    await reconnectAgentSession(
      { sessionId: "s-recorded", agentPath: "/bin/claude", reorientCommand: "/cs-status" },
      deps,
    );
    expect(deps.relaunch).toHaveBeenCalledWith("s-recorded", line);
  });

  it("AC-1 (live): re-attaches — re-orients the RUNNING agent, NO relaunch, NO shell gate", async () => {
    const deps = reconnectDeps({ probeLive: vi.fn(async () => true) });
    const res = await reconnectAgentSession(
      { sessionId: "s1", agentPath: "/bin/claude", reorientCommand: "/cs-status" },
      deps,
    );
    expect(deps.probeLive).toHaveBeenCalledWith("s1", "claude");
    // The reported "shell never reached its prompt" bug came from an unconditional
    // relaunch — a live agent must NOT relaunch and must NOT wait on liveness.
    expect(deps.relaunch).not.toHaveBeenCalled();
    expect(deps.waitLive).not.toHaveBeenCalled();
    expect(deps.sendReorient).toHaveBeenCalledWith("s1", "/cs-status");
    expect(res).toEqual({ reattached: true });
  });

  it("AC-1: a probe FAILURE is treated as not-live → falls through to relaunch (never aborts)", async () => {
    const deps = reconnectDeps({
      probeLive: vi.fn(async () => {
        throw new Error("probe IPC failed");
      }),
    });
    const res = await reconnectAgentSession(
      { sessionId: "s1", agentPath: "/bin/claude", reorientCommand: "/cs-status" },
      deps,
    );
    expect(deps.relaunch).toHaveBeenCalledTimes(1); // safe relaunch, not an abort
    expect(deps.sendReorient).toHaveBeenCalledWith("s1", "/cs-status");
    expect(res).toEqual({ reattached: false });
  });

  it("AC-1 (dead): does NOT re-orient if the agent never comes up (no boot-gap injection)", async () => {
    const deps = reconnectDeps({ waitLive: vi.fn(async () => false) });
    await expect(
      reconnectAgentSession(
        { sessionId: "s1", agentPath: "/bin/claude", reorientCommand: "/cs-status" },
        deps,
      ),
    ).rejects.toThrow(/did not come up/i);
    expect(deps.sendReorient).not.toHaveBeenCalled();
  });
});

describe("agentBasename (AC-12 liveness needle)", () => {
  it("derives the process basename from a resolved agent path", () => {
    expect(agentBasename("/opt/homebrew/bin/claude")).toBe("claude");
    expect(agentBasename("claude")).toBe("claude");
  });
});

describe("waitForAgentLive (AC-11/AC-12 — hook-independent readiness)", () => {
  it("polls the liveness probe until the agent is up", async () => {
    const liveSeq = [false, false, true];
    let i = 0;
    const isLive = vi.fn(async () => liveSeq[i++] ?? true);
    const sleep = vi.fn(async () => {});
    const ok = await waitForAgentLive("s1", "claude", { sleepMs: 0, sleep, isLive });
    expect(ok).toBe(true);
    expect(isLive).toHaveBeenCalledWith("s1", "claude");
    expect(sleep).toHaveBeenCalledTimes(2);
  });

  it("returns false if the agent never comes up within the attempt budget", async () => {
    const ok = await waitForAgentLive("s1", "claude", {
      attempts: 3,
      sleepMs: 0,
      sleep: async () => {},
      isLive: async () => false,
    });
    expect(ok).toBe(false);
  });
});

// ── SW-GW-6: title threading into the owned tab ──────────────────────────────
describe("SW-GW-6 connectAgentSession — tab title", () => {
  function makeTitleDeps(): {
    deps: LaunchDeps;
    tabs: { name: string | undefined; role?: string }[];
  } {
    const tabs: { name: string | undefined; role?: string }[] = [];
    const deps: LaunchDeps = {
      findExisting: vi.fn(async () => undefined),
      provision: vi.fn(async () => {}),
      openTab: vi.fn(async (_team, _workspaceId, tab) => {
        tabs.push({ name: tab.name, role: tab.role });
      }),
      closeTab: vi.fn(),
      launchAgentLine: vi.fn(async () => ({ ok: true }) as const),
      projectScopeConfig: vi.fn(async () => ({
        extraArgs: ["--mcp-config", "/tmp/scope.json", "--strict-mcp-config"],
        environmentBinding: null,
      })),
    };
    return { deps, tabs };
  }

  it("names the owned tab from `title` when provided", async () => {
    const { deps, tabs } = makeTitleDeps();
    await connectAgentSession(
      {
        teamKey: "acme:default",
        orgRoot: "/ctx",
        agentPath: "/bin/claude",
        role: "story-dev:SW-GW-6#arch-review",
        workspaceKey: "SW-GW-6",
        title: "Architecture Review",
      },
      deps,
    );
    expect(tabs[0].name).toBe("Architecture Review");
  });

  const prepared = (title: string) => async () => ({
    agentPath: "/bin/claude",
    args: [],
    mcpAllow: [],
    extraArgs: [],
    envRemove: [],
    title,
  });

  it("names a prepared agent's tab with the agent's name when no title is given", async () => {
    const { deps, tabs } = makeTitleDeps();
    await connectAgentSession(
      {
        teamKey: "acme:default",
        orgRoot: "/ctx",
        agentPath: "",
        role: "dir-agent:/ctx",
        workspaceKey: "/ctx",
        prepare: prepared("Ricky"),
      },
      deps,
    );
    expect(tabs[0].name).toBe("Ricky");
  });

  it("an explicit title wins over the prepared agent's name", async () => {
    const { deps, tabs } = makeTitleDeps();
    await connectAgentSession(
      {
        teamKey: "acme:default",
        orgRoot: "/ctx",
        agentPath: "",
        role: "story-dev:SW-GW-6#arch-review",
        workspaceKey: "SW-GW-6",
        title: "Architecture Review",
        prepare: prepared("Ricky"),
      },
      deps,
    );
    expect(tabs[0].name).toBe("Architecture Review");
  });

  it("leaves the name to the host's `Terminal N` default when no title is given", async () => {
    const { deps, tabs } = makeTitleDeps();
    await connectAgentSession(
      {
        teamKey: "acme:default",
        orgRoot: "/ctx",
        agentPath: "/bin/claude",
        role: "test-agent",
        workspaceKey: "test-agent",
      },
      deps,
    );
    expect(tabs[0].name).toBeUndefined();
  });
});

describe("relaunchAgentSession — a fresh per-session config every time", () => {
  const ok = { launchAgentLine: vi.fn(async () => ({ ok: true }) as const) };

  it("a launch with no allowlist re-projects its config, keyed by its CLI", async () => {
    const respawn = vi.fn(async () => ({ extraArgs: ["--settings", "/tmp/s/hooks.json"] }));
    const result = await relaunchAgentSession(
      "sess-1",
      {
        agentPath: "/bin/claude",
        args: [],
        shell: { cwd: "/wt/a", envRemove: ["ANTHROPIC_API_KEY"], mcpAllow: null },
      },
      respawn,
      ok,
    );
    expect(result).toEqual({ ok: true });
    expect(respawn).toHaveBeenCalledWith({
      shell: { cwd: "/wt/a", envRemove: ["ANTHROPIC_API_KEY"], account: undefined },
      scope: {
        cwd: "/wt/a",
        mcpAllow: null,
        bridgeTools: null,
        account: undefined,
        binary: "claude",
        launchArgs: [],
      },
    });
    expect(ok.launchAgentLine).toHaveBeenLastCalledWith(
      "sess-1",
      "'/bin/claude' '--settings' '/tmp/s/hooks.json'",
    );
  });

  it("an absent edit appends fresh flags and keeps the original recorded recipe", async () => {
    const spec = {
      agentPath: "/bin/codex",
      args: ["--old"],
      shell: { cwd: "/wt/b", envRemove: [], mcpAllow: [], agent: true },
    };
    await relaunchAgentSession("absent-edit", spec, async () => ({ extraArgs: ["--scope"] }), ok);
    expect(ok.launchAgentLine).toHaveBeenLastCalledWith(
      "absent-edit",
      "'/bin/codex' '--old' '--scope'",
    );
    expect(useAgentStateStore.getState().launchBySession["absent-edit"].spec).toEqual(spec);
  });

  it("a named agent's relaunch asks for its scope alone", async () => {
    const respawn = vi.fn(async () => ({ extraArgs: [] as string[] }));
    await relaunchAgentSession(
      "sess-2",
      {
        agentPath: "/bin/claude",
        args: ["--settings", "/tmp/agent.json"],
        shell: { cwd: "/wt/b", envRemove: [], mcpAllow: [], agent: true },
      },
      respawn,
      ok,
    );
    expect(respawn).toHaveBeenCalledWith(
      expect.objectContaining({
        scope: expect.objectContaining({ mcpAllow: [], agent: true, binary: "claude" }),
      }),
    );
  });

  it.each([undefined, [], ["who_am_i"]].map((bridgeTools) => ({ bridgeTools })))(
    "a named Codex relaunch keeps captured bridge policy %j",
    async ({ bridgeTools }) => {
      const respawn = vi.fn(async () => ({ extraArgs: [] as string[] }));
      await relaunchAgentSession(
        "bridge-relaunch",
        {
          agentPath: "/bin/codex",
          args: [],
          shell: { cwd: "/wt", envRemove: [], mcpAllow: [], agent: true, ...{ bridgeTools } },
        },
        respawn,
        ok,
      );
      expect(respawn).toHaveBeenCalledWith(
        expect.objectContaining({
          scope: expect.objectContaining({ bridgeTools: bridgeTools ?? null }),
        }),
      );
    },
  );

  it("a named agent's relaunch sets its own env again", async () => {
    const respawn = vi.fn(async () => ({ extraArgs: [] as string[] }));
    const env = { CLAUDE_CODE_DISABLE_AUTO_MEMORY: "1" };
    await relaunchAgentSession(
      "sess-3",
      {
        agentPath: "/bin/claude",
        args: [],
        shell: { cwd: "/wt/c", envRemove: [], mcpAllow: [], agent: true, env },
      },
      respawn,
      ok,
    );
    expect(respawn).toHaveBeenCalledWith(
      expect.objectContaining({ scope: expect.objectContaining({ agent: true, agentEnv: env }) }),
    );
  });
});
