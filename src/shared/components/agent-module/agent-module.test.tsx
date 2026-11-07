// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, type Mock } from "vitest";
import { render, renderHook, screen, cleanup } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Bot } from "lucide-react";
import { useEffect } from "react";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import { getPanel, usePanelRegistry } from "@/features/shell/right-pane/panel-registry";
import { useAgentWorkspaceSession } from "@/shared/hooks/use-agent-workspace-session";
import type { AgentWorkspaceSessionApi } from "@/shared/hooks/use-agent-workspace-session";
import type { AgentWorkspaceConfig } from "@/shared/lib/run-agent-bind";

import { useOldestPendingAsk } from "@/shared/hooks/use-oldest-pending-ask";
import { answerAskUser } from "@/shared/hooks/use-managed-session-bridge";
import {
  acceptAnswer,
  type AskUserRequest,
  type AskUserResponse,
} from "@/shared/lib/managed-session";

import { useAgentModule } from "./use-agent-module";
import { AgentModulePanel, useRegisterAgentModulePanel } from "./agent-module-panel";
import type { AgentModuleConfig } from "./agent-module-config";

// The reusable-module test is about COMPOSITION (registration, slots, open-terminal,
// the memoized-def contract), not the chrome children — those have their own suites.
// Stub the children + the session hook + the managed store so the module wiring is
// isolated; keep the REAL panel registry + viewer store to prove registration.
vi.mock("@/shared/hooks/use-agent-workspace-session", () => ({
  useAgentWorkspaceSession: vi.fn(),
}));

vi.mock("@/shared/stores/use-managed-session-store", () => {
  const useStore = vi.fn(() => "running") as unknown as Mock & { getState: () => unknown };
  useStore.getState = () => ({ markRunning: vi.fn() });
  return { useManagedSessionStore: useStore, selectManagedState: () => () => "running" };
});

vi.mock("@/shared/components/supervised-agent-session", () => ({
  SupervisedAgentSession: (p: { binding: { connection: string } }) => (
    <div data-testid="stub-supervised">{p.binding.connection}</div>
  ),
}));
vi.mock("@/shared/components/agent-chat-surface", () => ({
  AgentChatSurface: (p: { sessionId: string | null; canSend: boolean }) => (
    <div data-testid="stub-chat">{`${p.sessionId ?? "none"}:${p.canSend}`}</div>
  ),
}));
vi.mock("@/shared/components/agent-session-picker", () => ({
  AgentSessionPicker: () => <div data-testid="stub-picker" />,
}));
vi.mock("@/shared/components/managed-question-panel", () => ({
  ManagedQuestionPanel: (p: { sessionId: string }) => (
    <div data-testid="stub-question">{p.sessionId}</div>
  ),
}));
// SW-AW-7: the module body owns ask-selection via this shared hook (its own suite
// covers the real filter+sort); stub it so slot tests drive the pending-ask picture.
vi.mock("@/shared/hooks/use-oldest-pending-ask", () => ({
  useOldestPendingAsk: vi.fn(() => ({ ask: null, pendingCount: 0 })),
}));
// SW-AW-7: the answer channel is shared with SW-AW-4 (its own suite covers the
// no-wedge resolve); stub it to assert the slot routes through it with the right id.
vi.mock("@/shared/hooks/use-managed-session-bridge", () => ({
  answerAskUser: vi.fn(),
}));

function makeSession(overrides: Partial<AgentWorkspaceSessionApi> = {}): AgentWorkspaceSessionApi {
  return {
    binding: {
      sessionId: null,
      owner: "user",
      connection: "disconnected",
      onDisconnect: vi.fn(),
      onReconnect: vi.fn(),
    },
    boundSessionId: null,
    canSend: false,
    disabledReason: undefined,
    recovery: false,
    picker: { options: [], startNew: vi.fn(), connectToTab: vi.fn(), refresh: vi.fn() },
    ...overrides,
  };
}

const SESSION_CONFIG: AgentWorkspaceConfig = {
  targetWorkspace: "ws-1",
  agentBasename: "claude",
  connect: vi.fn(async () => "sess-1"),
  reconnect: vi.fn(async () => ({ kind: "reattach" }) as never),
  autoConnect: true,
};

function makeConfig(overrides: Partial<AgentModuleConfig> = {}): AgentModuleConfig {
  return {
    panelId: "agent-test",
    title: "Sync Agent",
    icon: Bot,
    session: SESSION_CONFIG,
    ...overrides,
  };
}

beforeEach(() => {
  cleanup();
  vi.clearAllMocks();
  usePanelRegistry.setState({ panels: {} });
  (useAgentWorkspaceSession as Mock).mockReturnValue(makeSession());
  // Default: no pending ask (overridden per-test; clearAllMocks keeps the impl).
  (useOldestPendingAsk as Mock).mockReturnValue({ ask: null, pendingCount: 0 });
});

// AC-4/AC-4a: the headless hook is usable with NO panel — the GS-12 shape.
describe("useAgentModule (headless hook, AC-4a)", () => {
  it("returns a live session handle without any panel being registered (GS-12 shape)", () => {
    (useAgentWorkspaceSession as Mock).mockReturnValue(makeSession({ boundSessionId: "sess-1" }));
    const { result } = renderHook(() => useAgentModule(makeConfig()));
    expect(result.current.session.boundSessionId).toBe("sess-1");
    expect(typeof result.current.promote).toBe("function");
    // No panel mounted → nothing registered.
    expect(getPanel("agent-test")).toBeUndefined();
  });
});

// AC-4/AC-5: the panel seam registers the chrome and honors the memoized-def contract.
describe("useRegisterAgentModulePanel (panel seam, AC-4/AC-5)", () => {
  function Host({ config }: { config: AgentModuleConfig }) {
    const api = useAgentModule(config);
    useRegisterAgentModulePanel(config, api);
    return null;
  }

  it("registers on mount and unregisters on unmount", () => {
    const { unmount } = render(<Host config={makeConfig()} />);
    const def = getPanel("agent-test");
    expect(def).toBeDefined();
    expect(def!.title).toBe("Sync Agent");
    expect(def!.icon).toBe(Bot);
    unmount();
    expect(getPanel("agent-test")).toBeUndefined();
  });

  it("render() returns the chrome body (transcript + slots) from the bound session", () => {
    (useAgentWorkspaceSession as Mock).mockReturnValue(makeSession({ boundSessionId: "sess-1" }));
    render(<Host config={makeConfig()} />);
    const def = getPanel("agent-test")!;
    render(<>{def.render()}</>);
    expect(screen.getByTestId("agent-module-body")).toBeInTheDocument();
    expect(screen.getByTestId("stub-supervised")).toBeInTheDocument();
    expect(screen.getByTestId("stub-chat")).toHaveTextContent("sess-1:false");
    // Module-owned answerable-prompt card renders for the bound session.
    expect(screen.getByTestId("stub-question")).toHaveTextContent("sess-1");
  });

  it("renders the consumer actionCards slot alongside the answerable-prompt card (AC-5, two slots)", () => {
    (useAgentWorkspaceSession as Mock).mockReturnValue(makeSession({ boundSessionId: "sess-9" }));
    const actionCards = vi.fn(({ boundSessionId }: { boundSessionId: string | null }) => (
      <div data-testid="consumer-cards">{boundSessionId}</div>
    ));
    render(<Host config={makeConfig({ actionCards })} />);
    render(<>{getPanel("agent-test")!.render()}</>);
    expect(screen.getByTestId("consumer-cards")).toHaveTextContent("sess-9");
    expect(screen.getByTestId("stub-question")).toBeInTheDocument();
    expect(actionCards).toHaveBeenCalledWith({ boundSessionId: "sess-9" });
  });

  it("omits headerActions when onOpenTerminal is not supplied; provides it when it is", async () => {
    // No onOpenTerminal → no header actions.
    render(<Host config={makeConfig()} />);
    expect(getPanel("agent-test")!.headerActions).toBeUndefined();
    cleanup();
    usePanelRegistry.setState({ panels: {} });

    // With onOpenTerminal → headerActions returns an element that reveals the tab.
    const onOpenTerminal = vi.fn();
    render(<Host config={makeConfig({ onOpenTerminal })} />);
    const def = getPanel("agent-test")!;
    expect(def.headerActions).toBeDefined();
    render(<>{def.headerActions!()}</>);
    await userEvent.click(screen.getByTestId("agent-module-open-terminal"));
    expect(onOpenTerminal).toHaveBeenCalledTimes(1);
  });

  it("keeps the same panel id registered across a session-state change (gap-free replace)", () => {
    const session = makeSession({ boundSessionId: null });
    (useAgentWorkspaceSession as Mock).mockReturnValue(session);
    function Wrapper() {
      const api = useAgentModule(makeConfig());
      useRegisterAgentModulePanel(makeConfig(), api);
      return null;
    }
    const { rerender } = render(<Wrapper />);
    expect(getPanel("agent-test")).toBeDefined();
    // Flip to a bound session — the def is re-created + re-registered under the same id.
    (useAgentWorkspaceSession as Mock).mockReturnValue(makeSession({ boundSessionId: "sess-2" }));
    rerender(<Wrapper />);
    expect(getPanel("agent-test")).toBeDefined();
    expect(Object.keys(usePanelRegistry.getState().panels)).toEqual(["agent-test"]);
  });
});

// AC-4: the panel-only convenience component owns the hook + registration.
describe("AgentModulePanel (convenience component)", () => {
  it("registers the panel on mount and renders nothing itself", () => {
    (useAgentWorkspaceSession as Mock).mockReturnValue(makeSession({ boundSessionId: "sess-1" }));
    const { container, unmount } = render(<AgentModulePanel config={makeConfig()} />);
    expect(container).toBeEmptyDOMElement();
    expect(getPanel("agent-test")).toBeDefined();
    unmount();
    expect(getPanel("agent-test")).toBeUndefined();
  });
});

// SW-AW-7: the optional consumer-supplied answerable-prompt slot.
describe("answerablePrompts slot (SW-AW-7 AC-1/2/3/4/5)", () => {
  type SlotCtx = {
    ask: AskUserRequest;
    answer: (response: AskUserResponse) => void;
    pendingCount: number;
    sessionId: string;
  };
  const pendingAsk: AskUserRequest = {
    correlationId: "ask-user-7",
    sessionId: "sess-1",
    question: "Resolve the README conflict?",
    options: ["Keep mine", "Keep theirs", "Both"],
  };

  function Host({ config }: { config: AgentModuleConfig }) {
    const api = useAgentModule(config);
    useRegisterAgentModulePanel(config, api);
    return null;
  }
  const renderBody = (config: AgentModuleConfig): void => {
    (useAgentWorkspaceSession as Mock).mockReturnValue(makeSession({ boundSessionId: "sess-1" }));
    render(<Host config={config} />);
    render(<>{getPanel("agent-test")!.render()}</>);
  };

  it("AC-2: renders the default ManagedQuestionPanel when the slot is omitted", () => {
    (useOldestPendingAsk as Mock).mockReturnValue({ ask: pendingAsk, pendingCount: 1 });
    renderBody(makeConfig());
    expect(screen.getByTestId("stub-question")).toHaveTextContent("sess-1");
  });

  it("AC-3: renders the consumer slot INSTEAD of the default card when supplied + ask pending", () => {
    (useOldestPendingAsk as Mock).mockReturnValue({ ask: pendingAsk, pendingCount: 3 });
    const answerablePrompts = vi.fn((ctx: SlotCtx) => (
      <div data-testid="consumer-ask">{`${ctx.sessionId}:${ctx.ask.correlationId}:+${ctx.pendingCount}`}</div>
    ));
    renderBody(makeConfig({ answerablePrompts }));

    // Slot gets the non-null ask, bound session id, AND the backlog count (+N parity).
    expect(screen.getByTestId("consumer-ask")).toHaveTextContent("sess-1:ask-user-7:+3");
    // One answerable surface — the default card is NOT also rendered.
    expect(screen.queryByTestId("stub-question")).not.toBeInTheDocument();
    expect(answerablePrompts).toHaveBeenCalledWith(
      expect.objectContaining({ ask: pendingAsk, sessionId: "sess-1", pendingCount: 3 }),
    );
  });

  it("AC-3: a consumer render throw degrades to the default answer card (blocking ask stays answerable), pane survives", () => {
    (useOldestPendingAsk as Mock).mockReturnValue({ ask: pendingAsk, pendingCount: 1 });
    const answerablePrompts = () => {
      throw new Error("consumer redline blew up");
    };
    // The boundary logs the error; silence the expected console noise.
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    renderBody(makeConfig({ answerablePrompts }));

    // The blocking ask stays answerable: the boundary falls back to the module-owned
    // default question card (not an inert message), and the rest of the chrome lives.
    expect(screen.getByTestId("stub-question")).toHaveTextContent("sess-1");
    expect(screen.getByTestId("stub-chat")).toBeInTheDocument();
    expect(screen.getByTestId("stub-supervised")).toBeInTheDocument();
    spy.mockRestore();
  });

  it("AC-3: renders nothing answerable when the slot is supplied but no ask is pending", () => {
    (useOldestPendingAsk as Mock).mockReturnValue({ ask: null, pendingCount: 0 });
    const answerablePrompts = vi.fn(() => <div data-testid="consumer-ask" />);
    renderBody(makeConfig({ answerablePrompts }));

    expect(screen.queryByTestId("consumer-ask")).not.toBeInTheDocument();
    expect(screen.queryByTestId("stub-question")).not.toBeInTheDocument();
    expect(answerablePrompts).not.toHaveBeenCalled();
  });

  it("AC-4: the consumer's answer routes through answerAskUser with the pre-bound correlationId", async () => {
    (useOldestPendingAsk as Mock).mockReturnValue({ ask: pendingAsk, pendingCount: 1 });
    const answerablePrompts = (ctx: SlotCtx) => (
      <button data-testid="keep-mine" onClick={() => ctx.answer(acceptAnswer("Keep mine"))}>
        Keep mine
      </button>
    );
    renderBody(makeConfig({ answerablePrompts }));

    await userEvent.click(screen.getByTestId("keep-mine"));
    expect(answerAskUser).toHaveBeenCalledWith("ask-user-7", {
      action: "accept",
      content: "Keep mine",
    });
  });
});

// AC-5: the slot is a render seam only — it must NEVER reconstruct the ask_user
// transport (the Rust command or the inbound listener). Guarded by a source scan.
describe("answerablePrompts channel integrity (SW-AW-7 AC-5)", () => {
  it("no render-seam source imports resolveAskUser or the mcp:ask-user listener", () => {
    const here = dirname(fileURLToPath(import.meta.url));
    // The whole render seam: the agent-module files AND the shared ask-selection hook.
    const seam = [
      join(here, "agent-module-panel.tsx"),
      join(here, "agent-module-config.ts"),
      join(here, "index.ts"),
      join(here, "..", "..", "hooks", "use-oldest-pending-ask.ts"),
    ];
    for (const path of seam) {
      const src = readFileSync(path, "utf8");
      expect(src, `${path} must not touch the raw ask_user command`).not.toMatch(/resolveAskUser/);
      expect(src, `${path} must not re-mount the ask_user listener`).not.toMatch(/mcp:ask-user/);
    }
  });
});

// AC-4b: markRunning arms supervision when the agent binds (in the hook, so a
// headless consumer arms it too).
describe("supervision arming (AC-4b/AC-5)", () => {
  it("does not throw and registers cleanly when the session has no bound id yet", () => {
    (useAgentWorkspaceSession as Mock).mockReturnValue(makeSession({ boundSessionId: null }));
    function Host() {
      const api = useAgentModule(makeConfig());
      useRegisterAgentModulePanel(makeConfig(), api);
      useEffect(() => void api.isMinimized, [api.isMinimized]);
      return null;
    }
    expect(() => render(<Host />)).not.toThrow();
    expect(getPanel("agent-test")).toBeDefined();
  });
});
