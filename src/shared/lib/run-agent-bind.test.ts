// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import {
  runAgentBind,
  resolveMountBind,
  type BindDeps,
  type AgentWorkspaceConfig,
} from "@/shared/lib/run-agent-bind";
import type { SurfaceBinding } from "@/shared/stores/use-agent-session-store";

function binding(over: Partial<SurfaceBinding> = {}): SurfaceBinding {
  return {
    boundSessionId: null,
    owner: "agent",
    connection: "disconnected",
    recovery: false,
    ...over,
  };
}

function makeConfig(over: Partial<AgentWorkspaceConfig> = {}): AgentWorkspaceConfig {
  return {
    targetWorkspace: "test-agent",
    agentBasename: "claude",
    connect: vi.fn(async () => "shell-new"),
    reconnect: vi.fn(async () => ({ reattached: true })),
    ...over,
  };
}

function makeDeps(over: Partial<BindDeps> = {}): { deps: BindDeps; calls: string[] } {
  const calls: string[] = [];
  const deps: BindDeps = {
    setOwner: vi.fn((o) => calls.push(`owner:${o}`)),
    setConnection: vi.fn((c) => calls.push(`conn:${c}`)),
    setBoundSessionId: vi.fn((id) => calls.push(`bound:${id}`)),
    setRecovery: vi.fn((r) => calls.push(`recovery:${r}`)),
    primeAndHandshake: vi.fn(async (sid) => {
      calls.push(`handshake:${sid}`);
      return true;
    }),
    isMounted: () => true,
    onError: vi.fn((e) => calls.push(`error:${e}`)),
    ...over,
  };
  return { deps, calls };
}

describe("runAgentBind (AC-9/AC-0/AC-3b orchestration)", () => {
  it("connect: spawns via config.connect, primes+handshakes, pong → live + bound + no recovery", async () => {
    const config = makeConfig();
    const { deps, calls } = makeDeps();
    await runAgentBind(config, { kind: "connect" }, deps);
    expect(config.connect).toHaveBeenCalledTimes(1);
    expect(config.reconnect).not.toHaveBeenCalled();
    expect(calls).toEqual([
      "owner:agent",
      "conn:reconnecting",
      "handshake:shell-new",
      "bound:shell-new",
      "conn:live",
      "recovery:false",
    ]);
  });

  it("connect: no pong → stays not-live + recovery surfaced, but session is still bound (AC-3a/AC-3b)", async () => {
    const config = makeConfig();
    const { deps } = makeDeps({ primeAndHandshake: vi.fn(async () => false) });
    await runAgentBind(config, { kind: "connect" }, deps);
    expect(deps.setBoundSessionId).toHaveBeenCalledWith("shell-new");
    expect(deps.setConnection).toHaveBeenLastCalledWith("disconnected");
    expect(deps.setRecovery).toHaveBeenCalledWith(true);
  });

  it("reconnect: re-attaches via config.reconnect (NOT config.connect), keeps the bound sid", async () => {
    const config = makeConfig();
    const { deps } = makeDeps();
    await runAgentBind(config, { kind: "reconnect", sessionId: "shell-live" }, deps);
    expect(config.reconnect).toHaveBeenCalledWith("shell-live");
    expect(config.connect).not.toHaveBeenCalled();
    expect(deps.primeAndHandshake).toHaveBeenCalledWith("shell-live");
    expect(deps.setBoundSessionId).toHaveBeenCalledWith("shell-live");
  });

  it("attach (click-select / reopen re-handshake): no spawn, no engine reconnect — just prime+handshake", async () => {
    const config = makeConfig();
    const { deps } = makeDeps();
    await runAgentBind(config, { kind: "attach", sessionId: "shell-chosen" }, deps);
    expect(config.connect).not.toHaveBeenCalled();
    expect(config.reconnect).not.toHaveBeenCalled();
    expect(deps.primeAndHandshake).toHaveBeenCalledWith("shell-chosen");
    expect(deps.setBoundSessionId).toHaveBeenCalledWith("shell-chosen");
  });

  it("error during connect → settle disconnected + onError, no false 'live'", async () => {
    const config = makeConfig({
      connect: vi.fn(async () => {
        throw new Error("boom");
      }),
    });
    const { deps } = makeDeps();
    await runAgentBind(config, { kind: "connect" }, deps);
    expect(deps.setConnection).toHaveBeenLastCalledWith("disconnected");
    expect(deps.onError).toHaveBeenCalledTimes(1);
    expect(deps.setBoundSessionId).not.toHaveBeenCalled();
  });

  it("unmounted after handshake → no store writes committed (G2 safety)", async () => {
    const config = makeConfig();
    const { deps } = makeDeps({ isMounted: () => false });
    await runAgentBind(config, { kind: "connect" }, deps);
    expect(deps.setBoundSessionId).not.toHaveBeenCalled();
    // owner/connection 'reconnecting' were set before the await; the post-await
    // commit is skipped because the surface unmounted.
    expect(deps.setConnection).not.toHaveBeenCalledWith("live");
  });
});

describe("resolveMountBind (SW-AW-6 refactor B — pure mount-decision)", () => {
  it("preserves a user takeover — returns null regardless of everything else", () => {
    expect(
      resolveMountBind(
        binding({ owner: "user", boundSessionId: "s1", connection: "disconnected" }),
        {
          autoConnect: true,
          hasActiveTeam: true,
        },
      ),
    ).toBeNull();
  });

  it("null when autoConnect is off (opt-out consumer)", () => {
    expect(resolveMountBind(binding(), { autoConnect: false, hasActiveTeam: true })).toBeNull();
    expect(resolveMountBind(binding(), { autoConnect: undefined, hasActiveTeam: true })).toBeNull();
  });

  it("null when no real team is active (no phantom spawn in a no-team window)", () => {
    expect(resolveMountBind(binding(), { autoConnect: true, hasActiveTeam: false })).toBeNull();
  });

  it("reconnect (D5 reopen) when a session is already bound", () => {
    expect(
      resolveMountBind(binding({ boundSessionId: "s1" }), {
        autoConnect: true,
        hasActiveTeam: true,
      }),
    ).toEqual({ kind: "reconnect", sessionId: "s1" });
  });

  it("connect when nothing is bound and the surface is disconnected", () => {
    expect(resolveMountBind(binding(), { autoConnect: true, hasActiveTeam: true })).toEqual({
      kind: "connect",
    });
  });

  it("null when nothing is bound but the surface is not disconnected (nothing to do)", () => {
    expect(
      resolveMountBind(binding({ connection: "live" }), { autoConnect: true, hasActiveTeam: true }),
    ).toBeNull();
  });
});
