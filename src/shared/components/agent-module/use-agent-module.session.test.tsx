// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";
import { Bot } from "lucide-react";

// Mirror the SW-AW-3 hook harness: mock ONLY the Tauri-touching seams so the REAL
// useAgentWorkspaceSession binding-store dedup (the AC-7 single-session mechanism)
// is exercised, not the handshake/prime/probe internals.
const h = vi.hoisted(() => ({
  awaitPong: vi.fn(async () => true),
  primeAgentChat: vi.fn(async () => {}),
  useWorkspaceAgentTabs: vi.fn((_targetWorkspace?: string, _agentBasename?: string) => ({
    options: [],
    refresh: vi.fn(),
  })),
  toastError: vi.fn(),
  sessionHasLiveAgent: vi.fn(async () => true),
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
vi.mock("@/tauri/commands", () => ({ sessionHasLiveAgent: h.sessionHasLiveAgent }));

import { useAgentModule } from "./use-agent-module";
import type { AgentModuleConfig } from "./agent-module-config";
import { resetAgentSessionStoreForTests } from "@/shared/stores/use-agent-session-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  registerPanel,
  unregisterPanel,
  getPanel,
} from "@/features/shell/right-pane/panel-registry";
import type { AgentWorkspaceConfig } from "@/shared/lib/run-agent-bind";

function makeConfig(
  over: Partial<AgentModuleConfig> = {},
  session: Partial<AgentWorkspaceConfig> = {},
): AgentModuleConfig {
  return {
    panelId: "agent-gs",
    title: "Sync Agent",
    icon: Bot,
    session: {
      targetWorkspace: "gs-workspace",
      agentBasename: "claude",
      connect: vi.fn(async () => "sess-shared"),
      reconnect: vi.fn(async () => ({ reattached: true }) as never),
      autoConnect: true,
      ...session,
    },
    ...over,
  };
}

beforeEach(() => {
  resetAgentSessionStoreForTests();
  // SW-AW-6: the hook now gates auto-connect on a real active team — seed one.
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  vi.clearAllMocks();
  h.awaitPong.mockResolvedValue(true);
  // Reset the right-pane layout to empty + pane closed (deterministic promote).
  useViewerStore.setState({ rightPaneLayout: { sections: [] }, rightPaneOpen: false });
});

// AC-7: one shared warm session per surface, keyed by `targetWorkspace`.
describe("useAgentModule single-shared-session (AC-7)", () => {
  it("two instances with the SAME targetWorkspace share ONE connect + one sessionId (GS-10 + GS-12 shapes)", async () => {
    const connect = vi.fn(async () => "sess-shared");
    const config = makeConfig({}, { connect });

    // Instance A (e.g. GS-10 hook+panel) and instance B (GS-12 headless) — same key.
    const a = renderHook(() => useAgentModule(config));
    const b = renderHook(() => useAgentModule(config));

    await waitFor(() => expect(a.result.current.session.boundSessionId).toBe("sess-shared"));
    expect(b.result.current.session.boundSessionId).toBe("sess-shared");
    // The connection-state gate means only ONE instance drives the spawn.
    expect(connect).toHaveBeenCalledTimes(1);
  });

  it("a DIFFERENT targetWorkspace gets its own session (different key → two connects)", async () => {
    const connectA = vi.fn(async () => "sess-A");
    const connectB = vi.fn(async () => "sess-B");
    const a = renderHook(() =>
      useAgentModule(
        makeConfig({ panelId: "p-a" }, { targetWorkspace: "ws-A", connect: connectA }),
      ),
    );
    const b = renderHook(() =>
      useAgentModule(
        makeConfig({ panelId: "p-b" }, { targetWorkspace: "ws-B", connect: connectB }),
      ),
    );
    await waitFor(() => expect(a.result.current.session.boundSessionId).toBe("sess-A"));
    await waitFor(() => expect(b.result.current.session.boundSessionId).toBe("sess-B"));
    expect(connectA).toHaveBeenCalledTimes(1);
    expect(connectB).toHaveBeenCalledTimes(1);
  });
});

// AC-4b: non-blocking — mounting must not await connect before first paint.
describe("useAgentModule non-blocking spawn (AC-4b)", () => {
  it("first paint returns a null-session handle while connect is still pending", () => {
    let resolveConnect: ((id: string) => void) | undefined;
    const connect = vi.fn(
      () =>
        new Promise<string>((r) => {
          resolveConnect = r;
        }),
    );
    const { result } = renderHook(() => useAgentModule(makeConfig({}, { connect })));
    // connect was invoked (spawn kicked off) but NOT awaited on the render path —
    // the hook returned synchronously with no bound session.
    expect(connect).toHaveBeenCalledTimes(1);
    expect(result.current.session.boundSessionId).toBeNull();
    // Leave connect pending; nothing on the first-paint path blocked on it.
    void resolveConnect;
  });
});

// AC-8: born-minimized headless by default; promotable via the handle; the promote
// path adds NO second spawn and is no-op-safe when the panel isn't registered.
describe("useAgentModule promote/minimize (AC-8)", () => {
  it("promote() is a no-op when the panel id is not registered (no orphan section)", () => {
    const { result } = renderHook(() => useAgentModule(makeConfig()));
    act(() => result.current.promote());
    expect(useViewerStore.getState().rightPaneLayout.sections).toHaveLength(0);
  });

  it("promote() surfaces the registered panel as a top section; minimize() removes it", async () => {
    registerPanel({ id: "agent-gs", title: "Sync Agent", icon: Bot, render: () => null });
    const { result } = renderHook(() => useAgentModule(makeConfig()));

    act(() => result.current.promote());
    const promoted = useViewerStore.getState().rightPaneLayout;
    expect(promoted.sections[0].group.activePanelId).toBe("agent-gs");
    expect(promoted.sections[0].collapsed).toBe(false);
    await waitFor(() => expect(result.current.isMinimized).toBe(false));

    act(() => result.current.minimize());
    const minimized = useViewerStore.getState().rightPaneLayout;
    expect(minimized.sections.some((s) => s.group.panelIds.includes("agent-gs"))).toBe(false);
    await waitFor(() => expect(result.current.isMinimized).toBe(true));
    unregisterPanel("agent-gs");
  });

  it("promote() OPENS the right pane when closed; minimize() closes it if the layout empties", () => {
    registerPanel({ id: "agent-gs", title: "Sync Agent", icon: Bot, render: () => null });
    const { result } = renderHook(() => useAgentModule(makeConfig()));
    expect(useViewerStore.getState().rightPaneOpen).toBe(false);

    act(() => result.current.promote());
    // A surfaced panel is useless behind a closed (display:none) pane — promote opens it.
    expect(useViewerStore.getState().rightPaneOpen).toBe(true);

    act(() => result.current.minimize());
    // Removing the only section empties the layout → close the pane.
    expect(useViewerStore.getState().rightPaneOpen).toBe(false);
    unregisterPanel("agent-gs");
  });

  it("promote() adds NO additional connect (reuses the already-warm session)", async () => {
    registerPanel({ id: "agent-gs", title: "Sync Agent", icon: Bot, render: () => null });
    const connect = vi.fn(async () => "sess-shared");
    const { result } = renderHook(() => useAgentModule(makeConfig({}, { connect })));
    await waitFor(() => expect(result.current.session.boundSessionId).toBe("sess-shared"));
    expect(connect).toHaveBeenCalledTimes(1);

    act(() => result.current.promote());
    // Promoting is a pure layout mutation — no re-spawn.
    expect(connect).toHaveBeenCalledTimes(1);
    expect(getPanel("agent-gs")).toBeDefined();
    unregisterPanel("agent-gs");
  });
});
