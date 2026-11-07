// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  openWorkspaceTab,
  sendToWorkspaceTab,
  GENERIC_AGENT_ROLE,
  type OpenWorkspaceTabDeps,
  type SendToWorkspaceTabDeps,
} from "./workspace-tab-verbs";

describe("@P1 openWorkspaceTab (SW-GW-2)", () => {
  function makeDeps(overrides?: Partial<OpenWorkspaceTabDeps>): {
    deps: OpenWorkspaceTabDeps;
    opened: Array<{ workspaceId: string; role?: string }>;
    launched: string[];
    closed: Array<{ workspaceId: string; sessionId: string }>;
  } {
    const opened: Array<{ workspaceId: string; role?: string }> = [];
    const launched: string[] = [];
    const closed: Array<{ workspaceId: string; sessionId: string }> = [];
    const deps: OpenWorkspaceTabDeps = {
      workspaceKindOf: () => "generic",
      openTab: (workspaceId, role) => {
        opened.push({ workspaceId, role });
        return "shell-new";
      },
      closeTab: (workspaceId, sessionId) => {
        closed.push({ workspaceId, sessionId });
      },
      launchAgent: async (_workspaceId, sessionId) => {
        launched.push(sessionId);
      },
      ...overrides,
    };
    return { deps, opened, launched, closed };
  }

  it("opens a plain tab in a generic workspace and returns its sessionId", async () => {
    const { deps, opened, launched } = makeDeps();
    const res = await openWorkspaceTab({ workspaceId: "generic:/x" }, deps);
    expect(res).toEqual({ sessionId: "shell-new" });
    expect(opened).toEqual([{ workspaceId: "generic:/x", role: undefined }]);
    expect(launched).toEqual([]); // no agent launched for a plain tab
  });

  it("tags an agent tab and launches the agent when agent:true", async () => {
    const { deps, opened, launched } = makeDeps();
    const res = await openWorkspaceTab({ workspaceId: "generic:/x", agent: true }, deps);
    expect(res).toEqual({ sessionId: "shell-new" });
    expect(opened).toEqual([{ workspaceId: "generic:/x", role: GENERIC_AGENT_ROLE }]);
    expect(launched).toEqual(["shell-new"]); // agent launched into the new tab
  });

  it("rejects a non-generic workspace (never opens a tab)", async () => {
    const { deps, opened } = makeDeps({ workspaceKindOf: () => "story" });
    await expect(openWorkspaceTab({ workspaceId: "story:SW-1" }, deps)).rejects.toThrow(
      /not-allowed: openTab targets generic \(dir\) workspaces only/,
    );
    expect(opened).toEqual([]);
  });

  it("rejects an un-opened workspace with unavailable", async () => {
    const { deps } = makeDeps({ workspaceKindOf: () => null });
    await expect(openWorkspaceTab({ workspaceId: "generic:/gone" }, deps)).rejects.toThrow(
      /unavailable: no open workspace/,
    );
  });

  it("rejects a missing workspaceId", async () => {
    const { deps } = makeDeps();
    await expect(openWorkspaceTab({}, deps)).rejects.toThrow(/must be a non-empty string/);
  });

  it("rolls back the orphan tab when the agent launch fails (no mislabeled tab left)", async () => {
    const { deps, opened, closed } = makeDeps({
      launchAgent: async () => {
        throw new Error("unavailable: agent CLI not found on PATH");
      },
    });
    await expect(
      openWorkspaceTab({ workspaceId: "generic:/x", agent: true }, deps),
    ).rejects.toThrow(/agent CLI not found/);
    // The tab was opened, then rolled back — same workspace + the minted sessionId.
    expect(opened).toEqual([{ workspaceId: "generic:/x", role: GENERIC_AGENT_ROLE }]);
    expect(closed).toEqual([{ workspaceId: "generic:/x", sessionId: "shell-new" }]);
  });

  it("does NOT roll back a plain (non-agent) tab (no launch to fail)", async () => {
    const { deps, closed } = makeDeps();
    await openWorkspaceTab({ workspaceId: "generic:/x" }, deps);
    expect(closed).toEqual([]);
  });
});

describe("@P1 sendToWorkspaceTab (SW-GW-2)", () => {
  function makeDeps(overrides?: Partial<SendToWorkspaceTabDeps>): {
    deps: SendToWorkspaceTabDeps;
    commands: Array<{ sessionId: string; text: string }>;
    prompts: Array<{ sessionId: string; text: string }>;
  } {
    const commands: Array<{ sessionId: string; text: string }> = [];
    const prompts: Array<{ sessionId: string; text: string }> = [];
    const deps: SendToWorkspaceTabDeps = {
      tabInfo: () => ({ kind: "generic", isAgentTab: true }),
      sendCommand: async (sessionId, text) => {
        commands.push({ sessionId, text });
        return { submitted: true };
      },
      agentLive: async () => true,
      sendPrompt: async (sessionId, text) => {
        prompts.push({ sessionId, text });
        return { submitted: true };
      },
      ...overrides,
    };
    return { deps, commands, prompts };
  }

  it("routes a command to the shell submit path", async () => {
    const { deps, commands, prompts } = makeDeps();
    const res = await sendToWorkspaceTab(
      { sessionId: "shell-1", mode: "command", text: "ls -la" },
      deps,
    );
    expect(res).toEqual({ submitted: true });
    expect(commands).toEqual([{ sessionId: "shell-1", text: "ls -la" }]);
    expect(prompts).toEqual([]);
  });

  it("routes a prompt to the agent submit path when the tab hosts a LIVE agent", async () => {
    const { deps, commands, prompts } = makeDeps();
    const res = await sendToWorkspaceTab(
      { sessionId: "shell-1", mode: "prompt", text: "/dev-story" },
      deps,
    );
    expect(res).toEqual({ submitted: true });
    expect(prompts).toEqual([{ sessionId: "shell-1", text: "/dev-story" }]);
    expect(commands).toEqual([]);
  });

  it("refuses a prompt to a tab that was never opened as an agent (no-agent-in-tab)", async () => {
    const { deps, prompts } = makeDeps({ tabInfo: () => ({ kind: "generic", isAgentTab: false }) });
    await expect(
      sendToWorkspaceTab({ sessionId: "shell-1", mode: "prompt", text: "hi" }, deps),
    ).rejects.toThrow(/^no-agent-in-tab$/);
    expect(prompts).toEqual([]); // never pasted into a bare shell
  });

  it("refuses a prompt when the agent is tagged but NOT currently live (no-agent-in-tab)", async () => {
    const { deps, prompts } = makeDeps({ agentLive: async () => false });
    await expect(
      sendToWorkspaceTab({ sessionId: "shell-1", mode: "prompt", text: "hi" }, deps),
    ).rejects.toThrow(/^no-agent-in-tab$/);
    expect(prompts).toEqual([]);
  });

  it("refuses a sessionId that is not a tab of any open workspace (trust boundary)", async () => {
    const { deps } = makeDeps({ tabInfo: () => null });
    await expect(
      sendToWorkspaceTab({ sessionId: "shell-foreign", mode: "command", text: "x" }, deps),
    ).rejects.toThrow(/unavailable: no open workspace tab/);
  });

  it("refuses a tab that belongs to a NON-generic workspace (never a story/branch agent)", async () => {
    const { deps } = makeDeps({ tabInfo: () => ({ kind: "story", isAgentTab: true }) });
    await expect(
      sendToWorkspaceTab({ sessionId: "story-sess", mode: "command", text: "x" }, deps),
    ).rejects.toThrow(
      /not-allowed: sendToWorkspaceTab targets generic \(dir\) workspace tabs only/,
    );
  });

  it("rejects an invalid mode and an empty text", async () => {
    const { deps } = makeDeps();
    await expect(
      sendToWorkspaceTab({ sessionId: "s", mode: "nope", text: "x" }, deps),
    ).rejects.toThrow(/mode.*command.*prompt/);
    await expect(
      sendToWorkspaceTab({ sessionId: "s", mode: "command", text: "" }, deps),
    ).rejects.toThrow(/text.*non-empty/);
  });

  it("surfaces a held/dropped submit honestly as { submitted: false }", async () => {
    const { deps } = makeDeps({ sendCommand: async () => ({ submitted: false }) });
    const res = await sendToWorkspaceTab({ sessionId: "s", mode: "command", text: "x" }, deps);
    expect(res).toEqual({ submitted: false });
  });
});
