// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";

// Mock the Tauri-touching seams so the hook's OWN logic (mount-branch selection,
// in-flight guard, store writes, gating, picker wiring) is what's under test —
// not the handshake/prime/probe internals (covered by their own suites).
import type { AgentTabOption } from "@/shared/hooks/use-workspace-agent-tabs";

const h = vi.hoisted(() => ({
  awaitPong: vi.fn(async () => true),
  primeAgentChat: vi.fn(async () => {}),
  useWorkspaceAgentTabs: vi.fn(
    (
      _targetWorkspace?: string,
      _agentBasename?: string,
    ): { options: AgentTabOption[]; refresh: () => void } => ({
      options: [],
      refresh: vi.fn(),
    }),
  ),
  toastError: vi.fn(),
  sessionHasLiveAgent: vi.fn(async (_ptyId?: string, _agentName?: string) => true),
  // SW-FX-52: the liveness poller now batches via `sessionsHaveLiveAgent`. Keep
  // the existing `h.sessionHasLiveAgent` control knob by delegating per query, so
  // tests that `mockResolvedValue(false)` still drive a disconnect.
  sessionsHaveLiveAgent: vi.fn(async (queries: { ptyId: string; agentName: string }[]) =>
    Promise.all(
      queries.map(async (q) => ({
        ptyId: q.ptyId,
        alive: await h.sessionHasLiveAgent(q.ptyId, q.agentName),
      })),
    ),
  ),
}));
vi.mock("@/shared/lib/agent-handshake", () => ({ awaitPong: h.awaitPong }));
vi.mock("@/shared/lib/agent-chat-prime", () => ({
  primeAgentChat: h.primeAgentChat,
  newHandshakeToken: () => "cs-ack-test",
}));
vi.mock("@/shared/hooks/use-workspace-agent-tabs", () => ({
  useWorkspaceAgentTabs: (...a: [string, string]) => h.useWorkspaceAgentTabs(...a),
}));
vi.mock("sonner", () => ({ toast: { error: h.toastError } }));
vi.mock("@/tauri/commands", () => ({
  sessionHasLiveAgent: h.sessionHasLiveAgent,
  sessionsHaveLiveAgent: h.sessionsHaveLiveAgent,
}));

import {
  useAgentWorkspaceSession,
  LIVENESS_POLL_MS,
} from "@/shared/hooks/use-agent-workspace-session";
import {
  resetAgentSessionStoreForTests,
  selectBinding,
  useAgentSessionStore,
  buildAgentSurfaceKey,
} from "@/shared/stores/use-agent-session-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { buildTeamKey } from "@/shared/lib/team-key";
import type { AgentWorkspaceConfig } from "@/shared/lib/run-agent-bind";
import { seedTerminalTabs, installTerminalLayoutHarness } from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();

// SW-GS-22: the reopen guard reads the REAL `sessionExists` (→ `selectTabBySessionId`
// over the host layout the store projects). Seed a live tab for a session id so a
// reopen counts it as "still around" and reconnects; leave it unseeded so the reopen
// sees it as GONE and heals to a fresh spawn.
function seedSessionTabs(...sessionIds: string[]): void {
  useViewerStore.setState({
    workspaces: { seed: [{ id: "ws", kind: "generic", cwd: "/ws", status: "ready" }] },
  });
  seedTerminalTabs("seed", "ws", sessionIds);
}

// SW-AW-6: the hook now team-scopes the binding key via the reactive team key.
// Seed a deterministic active team so the composite key is stable across tests.
const TEAM_KEY = buildTeamKey("acme", "default");
const SURFACE_KEY = buildAgentSurfaceKey("story:test-A", TEAM_KEY);

// A NON-git-sync config — AC-9: the generic hook must drive an arbitrary surface
// unchanged (here a story workspace instance with the `codex` agent).
function makeConfig(over: Partial<AgentWorkspaceConfig> = {}): AgentWorkspaceConfig {
  return {
    targetWorkspace: "story:test-A",
    agentBasename: "codex",
    connect: vi.fn(async () => "shell-story-1"),
    reconnect: vi.fn(async () => ({ reattached: true })),
    autoConnect: true,
    ...over,
  };
}

beforeEach(() => {
  resetAgentSessionStoreForTests();
  useViewerStore.setState({ softClosedWorkspaces: {} });
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  vi.clearAllMocks();
  h.awaitPong.mockResolvedValue(true);
  h.useWorkspaceAgentTabs.mockReturnValue({ options: [], refresh: vi.fn() });
  h.sessionHasLiveAgent.mockResolvedValue(true);
});

describe("useAgentWorkspaceSession (AC-9 — generic plug-in hook, non-git-sync config)", () => {
  it("scopes the picker to the surface's own instance + agent (no git-sync assumption)", () => {
    const config = makeConfig();
    renderHook(() => useAgentWorkspaceSession(config));
    expect(h.useWorkspaceAgentTabs).toHaveBeenCalledWith("story:test-A", "codex");
  });

  it("auto-connects on mount: spawns, handshakes, pong → live + bound + canSend", async () => {
    const config = makeConfig();
    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));
    expect(config.connect).toHaveBeenCalledTimes(1);
    expect(result.current.boundSessionId).toBe("shell-story-1");
    expect(result.current.canSend).toBe(true);
    expect(result.current.recovery).toBe(false);
  });

  it("no pong → stays not-live, recovery surfaced, session still bound (AC-3a/AC-3b)", async () => {
    h.awaitPong.mockResolvedValue(false);
    const config = makeConfig();
    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(result.current.recovery).toBe(true));
    expect(result.current.binding.connection).toBe("disconnected");
    expect(result.current.canSend).toBe(false);
    expect(result.current.boundSessionId).toBe("shell-story-1");
  });

  it("disconnect releases the keyboard (owner→user, agent stays bound) and clears recovery (AC-2)", async () => {
    const config = makeConfig();
    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));
    act(() => result.current.binding.onDisconnect());
    await waitFor(() => expect(result.current.binding.owner).toBe("user"));
    expect(result.current.canSend).toBe(false);
    expect(result.current.recovery).toBe(false);
    expect(result.current.boundSessionId).toBe("shell-story-1"); // agent stays alive
  });

  it("Reconnect routes through the state-aware engine reconnect, not a fresh spawn (AC-1)", async () => {
    const config = makeConfig();
    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));
    (config.connect as ReturnType<typeof vi.fn>).mockClear();
    act(() => result.current.binding.onReconnect());
    await waitFor(() => expect(config.reconnect).toHaveBeenCalledWith("shell-story-1"));
    expect(config.connect).not.toHaveBeenCalled();
  });

  it("picker.connectToTab binds an existing tab via attach — no spawn, no engine reconnect (AC-4)", async () => {
    const config = makeConfig();
    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));
    (config.connect as ReturnType<typeof vi.fn>).mockClear();
    (config.reconnect as ReturnType<typeof vi.fn>).mockClear();
    act(() => result.current.picker.connectToTab("shell-chosen"));
    await waitFor(() => expect(result.current.boundSessionId).toBe("shell-chosen"));
    expect(config.connect).not.toHaveBeenCalled();
    expect(config.reconnect).not.toHaveBeenCalled();
  });

  it("reopen self-heals via state-aware reconnect (D5) — no duplicate spawn", async () => {
    const config = makeConfig();
    const first = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(first.result.current.binding.connection).toBe("live"));
    first.unmount();
    (config.connect as ReturnType<typeof vi.fn>).mockClear();
    (config.reconnect as ReturnType<typeof vi.fn>).mockClear();

    // SW-GS-22: the session SURVIVED the close — its tab is still in the store, so
    // the reopen guard sees it as live and reconnects (no fresh spawn).
    seedSessionTabs("shell-story-1");
    // Remount the same surface — the keyed store still holds boundSessionId.
    renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(config.reconnect).toHaveBeenCalledWith("shell-story-1"));
    expect(config.connect).not.toHaveBeenCalled();
  });

  it("SW-GS-22: reopen with a GONE session heals to a fresh spawn — no reconnect, no error (AC-1/AC-3)", async () => {
    const config = makeConfig();
    const first = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(first.result.current.binding.connection).toBe("live"));
    first.unmount();
    (config.connect as ReturnType<typeof vi.fn>).mockClear();
    (config.reconnect as ReturnType<typeof vi.fn>).mockClear();
    h.toastError.mockClear();

    // The soft-close shadow was cleared while the workspace was closed → the bound
    // session's tab is GONE (viewer store left unseeded). The reopen must NOT drive
    // config.reconnect into the void (which throws the guarded-injection error);
    // it clears the stale binding and takes the fresh-spawn path instead.
    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(config.connect).toHaveBeenCalledTimes(1)); // fresh spawn
    expect(config.reconnect).not.toHaveBeenCalled(); // never reconnects a dead session
    expect(h.toastError).not.toHaveBeenCalled(); // no scary "session is gone" toast
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));
  });

  it("SW-GS-22: the MANUAL Reconnect is NOT gated by sessionExists — in-session loud-throw preserved (AC-4)", async () => {
    // Only the automatic reopen path is made safe. The manual Reconnect button
    // (ownership.onReconnect) must still drive config.reconnect even with no tab in
    // the store, so the genuine in-session "owned tab died under us" case still
    // reaches the guarded-injection throw downstream.
    const config = makeConfig();
    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));
    (config.connect as ReturnType<typeof vi.fn>).mockClear();
    (config.reconnect as ReturnType<typeof vi.fn>).mockClear();

    // No seeded tab → sessionExists is false, yet the manual reconnect is ungated.
    act(() => result.current.binding.onReconnect());
    await waitFor(() => expect(config.reconnect).toHaveBeenCalledWith("shell-story-1"));
    expect(config.connect).not.toHaveBeenCalled();
  });

  it("reopen PRESERVES a prior user takeover — does not auto-rebind (owner stays user)", async () => {
    const config = makeConfig();
    const first = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(first.result.current.binding.connection).toBe("live"));
    act(() => first.result.current.binding.onDisconnect());
    await waitFor(() => expect(first.result.current.binding.owner).toBe("user"));
    first.unmount();
    (config.connect as ReturnType<typeof vi.fn>).mockClear();
    (config.reconnect as ReturnType<typeof vi.fn>).mockClear();

    const second = renderHook(() => useAgentWorkspaceSession(config));
    // Let mount effects settle, then assert no auto-rebind happened.
    await act(async () => {
      await Promise.resolve();
    });
    expect(config.reconnect).not.toHaveBeenCalled();
    expect(config.connect).not.toHaveBeenCalled();
    expect(second.result.current.binding.owner).toBe("user");
  });

  it("autoConnect:false → NO automatic (re)bind on mount OR reopen, even with a persisted binding (M1)", async () => {
    const config = makeConfig({ autoConnect: false });
    // First mount: no auto-connect.
    const first = renderHook(() => useAgentWorkspaceSession(config));
    await act(async () => {
      await Promise.resolve();
    });
    expect(config.connect).not.toHaveBeenCalled();
    // Explicit user action binds it.
    act(() => first.result.current.picker.startNew());
    await waitFor(() => expect(first.result.current.binding.connection).toBe("live"));
    first.unmount();
    (config.connect as ReturnType<typeof vi.fn>).mockClear();
    (config.reconnect as ReturnType<typeof vi.fn>).mockClear();
    // Reopen with a persisted binding must NOT auto-reconnect/relaunch (opt-out honored).
    renderHook(() => useAgentWorkspaceSession(config));
    await act(async () => {
      await Promise.resolve();
    });
    expect(config.reconnect).not.toHaveBeenCalled();
    expect(config.connect).not.toHaveBeenCalled();
  });

  it("resets the verified-connection state on unmount so reopen can't show stale 'live'", async () => {
    const config = makeConfig();
    const { result, unmount } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));
    unmount();
    // boundSessionId persists (the binding), but connection is no longer "live".
    const after = selectBinding(SURFACE_KEY)(useAgentSessionStore.getState());
    expect(after.connection).toBe("disconnected");
    expect(after.boundSessionId).toBe("shell-story-1");
  });

  it("SW-AW-6 (AC-1): writes the binding under the TEAM-SCOPED key, not the raw targetWorkspace", async () => {
    const config = makeConfig();
    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));
    // The binding lives under `${targetWorkspace}::${teamKey}` …
    expect(selectBinding(SURFACE_KEY)(useAgentSessionStore.getState()).boundSessionId).toBe(
      "shell-story-1",
    );
    // … and the raw, un-scoped key holds nothing (no global binding possible).
    expect(
      selectBinding("story:test-A")(useAgentSessionStore.getState()).boundSessionId,
    ).toBeNull();
  });

  it("re-probes tab liveness after a bind (so a de-duped Start-new can't leave a stale ◐)", async () => {
    const refreshMock = vi.fn();
    h.useWorkspaceAgentTabs.mockReturnValue({ options: [], refresh: refreshMock });
    const config = makeConfig();
    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));
    expect(refreshMock).toHaveBeenCalled();
  });

  it("liveness monitor: drops to recovery when the agent process dies while connected", async () => {
    vi.useFakeTimers();
    try {
      const config = makeConfig();
      const { result } = renderHook(() => useAgentWorkspaceSession(config));
      // Flush the connect chain (mocked awaitPong resolves via microtasks).
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(result.current.binding.connection).toBe("live");

      // The user exits claude in the CLI — the next probe reports the agent gone.
      h.sessionHasLiveAgent.mockResolvedValue(false);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(LIVENESS_POLL_MS + 50);
      });
      expect(result.current.binding.connection).toBe("disconnected");
      expect(result.current.recovery).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("flows picker options straight through from the data hook", () => {
    const options = [
      { id: "t1", name: "Tab 1", sessionId: "t1", role: "story:test-A", live: true as const },
    ];
    h.useWorkspaceAgentTabs.mockReturnValue({ options, refresh: vi.fn() });
    const config = makeConfig({ autoConnect: false });
    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    expect(result.current.picker.options).toEqual(options);
  });
});

describe("useAgentWorkspaceSession — team switch (SW-AW-6 AC-4/AC-5/AC-9)", () => {
  const keyFor = (team: string) => buildAgentSurfaceKey("story:test-A", buildTeamKey("acme", team));

  beforeEach(() => {
    resetAgentSessionStoreForTests();
    useViewerStore.setState({ softClosedWorkspaces: {} });
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "teamA" });
    vi.clearAllMocks();
    h.awaitPong.mockResolvedValue(true);
    h.useWorkspaceAgentTabs.mockReturnValue({ options: [], refresh: vi.fn() });
    h.sessionHasLiveAgent.mockResolvedValue(true);
  });

  it("AC-4/AC-5: switching teams re-drives the binding; A→B→A spawns once per team", async () => {
    let n = 0;
    const connect = vi.fn(async () => `shell-${++n}`);
    const reconnect = vi.fn(async () => ({ reattached: true }));
    const config = makeConfig({ connect, reconnect });

    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));
    expect(connect).toHaveBeenCalledTimes(1); // team A spawned once

    // Switch to team B → surfaceKey re-keys → auto-connect B (it has no binding).
    act(() => useOrgStore.setState({ activeTeamName: "teamB" }));
    await waitFor(() => expect(connect).toHaveBeenCalledTimes(2)); // team B spawned once
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));

    const idA = selectBinding(keyFor("teamA"))(useAgentSessionStore.getState()).boundSessionId;
    const idB = selectBinding(keyFor("teamB"))(useAgentSessionStore.getState()).boundSessionId;
    expect(idA).toBe("shell-1");
    expect(idB).toBe("shell-2");
    expect(idA).not.toBe(idB); // distinct sessions per team — no cross-team leak

    // Switch BACK to A → A already has a binding → RE-ATTACH (reconnect), no new spawn.
    // SW-GS-22: both agents' sessions survived the switch (soft-closed, PTY alive),
    // so seed their tabs — the reopen guard then reconnects A rather than re-spawning.
    seedSessionTabs("shell-1", "shell-2");
    reconnect.mockClear();
    act(() => useOrgStore.setState({ activeTeamName: "teamA" }));
    await waitFor(() => expect(reconnect).toHaveBeenCalledWith("shell-1"));
    expect(connect).toHaveBeenCalledTimes(2); // A→B→A: still one spawn for A (AC-5)
  });

  it("SW-AW-6: a USER-GLOBAL agent (teamScoped:false) binds ONCE and does NOT re-drive on team switch", async () => {
    let n = 0;
    const connect = vi.fn(async () => `notes-${++n}`);
    const config = makeConfig({ connect, targetWorkspace: "notes", teamScoped: false });
    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));
    expect(connect).toHaveBeenCalledTimes(1);
    // The global binding lives under the bare key, not a team-scoped one.
    expect(selectBinding("notes")(useAgentSessionStore.getState()).boundSessionId).toBe("notes-1");

    // Switch teams — a global agent must NOT re-key or spawn a second session.
    act(() => useOrgStore.setState({ activeTeamName: "teamB" }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(connect).toHaveBeenCalledTimes(1); // still one notes session across teams
    expect(result.current.boundSessionId).toBe("notes-1");
  });

  it("SW-AW-6: a USER-GLOBAL agent mounted in a no-team window auto-connects once a team resolves", async () => {
    // Global agents have a CONSTANT surfaceKey (no re-key on team change), so the
    // mount effect must also depend on hasActiveTeam or it would skip auto-connect
    // forever if it first mounted before a team was active.
    useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
    let n = 0;
    const connect = vi.fn(async () => `notes-${++n}`);
    const config = makeConfig({ connect, targetWorkspace: "notes", teamScoped: false });
    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await act(async () => {
      await Promise.resolve();
    });
    expect(connect).not.toHaveBeenCalled(); // no team yet → gated

    // A team resolves → hasActiveTeam flips → the global agent connects (once).
    act(() => useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "teamA" }));
    await waitFor(() => expect(connect).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));
  });

  it("AC-9: an in-flight bind on team A does NOT block team B (per-surface guard)", async () => {
    // Team A's handshake hangs → its bind stays in-flight (guard holds team A's key).
    let releaseA!: (v: boolean) => void;
    const aPending = new Promise<boolean>((res) => {
      releaseA = res;
    });
    h.awaitPong.mockReturnValueOnce(aPending); // first handshake (team A) never settles

    let n = 0;
    const connect = vi.fn(async () => `shell-${++n}`);
    const config = makeConfig({ connect });

    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(connect).toHaveBeenCalledTimes(1)); // A connected, handshaking

    // Switch to B while A is still mid-handshake. A single shared boolean guard
    // would drop B's bind here; the per-surface guard lets it through.
    act(() => useOrgStore.setState({ activeTeamName: "teamB" }));
    await waitFor(() => expect(connect).toHaveBeenCalledTimes(2)); // B bound despite A in-flight
    await waitFor(() => expect(result.current.binding.connection).toBe("live")); // B live

    releaseA(true); // let A's pending handshake settle cleanly (no unhandled rejection)
  });

  it("AC-9: switching teams clears the prior team's recovery banner (no cross-team leak)", async () => {
    h.awaitPong.mockResolvedValueOnce(false); // team A handshake fails → recovery banner
    const config = makeConfig();

    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(result.current.recovery).toBe(true)); // A in recovery

    // Switch to B (subsequent handshakes succeed) → recovery must NOT carry over.
    act(() => useOrgStore.setState({ activeTeamName: "teamB" }));
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));
    expect(result.current.recovery).toBe(false); // no leak from team A
  });

  it("SW-AW-6: switching into a no-team window does NOT auto-connect (no phantom spawn / no toast)", async () => {
    let n = 0;
    const connect = vi.fn(async () => `shell-${++n}`);
    const config = makeConfig({ connect });
    const { result } = renderHook(() => useAgentWorkspaceSession(config));
    await waitFor(() => expect(result.current.binding.connection).toBe("live"));
    expect(connect).toHaveBeenCalledTimes(1);

    // Org stays selected but team cleared → teamKey `acme:_none` (a no-team window,
    // NOT the NO_TEAM_SENTINEL). Must not auto-spawn under the unnavigable bucket.
    act(() => useOrgStore.setState({ activeTeamName: null }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(connect).toHaveBeenCalledTimes(1); // no phantom spawn
    expect(h.toastError).not.toHaveBeenCalled();

    // Fully clear the org → NO_TEAM_SENTINEL window; still inert.
    act(() => useOrgStore.setState({ activeOrgSlug: null }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(connect).toHaveBeenCalledTimes(1);
    expect(h.toastError).not.toHaveBeenCalled();
  });

  it("R1: recovery is PER-SURFACE — A's banner survives an A→B→A round-trip, never shows on B", async () => {
    h.awaitPong.mockResolvedValue(false); // any handshake fails → recovery
    // autoConnect:false so a team switch does NOT auto-rebind (which would re-derive
    // recovery and mask the per-surface persistence we're proving).
    const config = makeConfig({ autoConnect: false });
    const { result } = renderHook(() => useAgentWorkspaceSession(config));

    // Manually bind on team A → handshake fails → team A enters recovery.
    act(() => result.current.picker.startNew());
    await waitFor(() => expect(result.current.recovery).toBe(true));

    // Switch to B: its own (untouched) recovery flag is false — no leak from A.
    act(() => useOrgStore.setState({ activeTeamName: "teamB" }));
    expect(result.current.recovery).toBe(false);

    // Back to A: A's recovery flag is preserved (per-surface), not reset.
    act(() => useOrgStore.setState({ activeTeamName: "teamA" }));
    expect(result.current.recovery).toBe(true);
  });
});
