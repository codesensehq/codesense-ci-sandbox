// SPDX-License-Identifier: MIT
import { useState } from "react";
import { describe, it, expect, vi } from "vitest";
import { render, screen, renderHook } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import type { AgentSessionState, ConnectionPhase, SessionOwner } from "./types";

import {
  SupervisedAgentSession,
  agentSessionChipLabel,
  taskStateBadgeLabel,
} from "./SupervisedAgentSession";
import { useAgentSessionOwnership } from "./use-agent-session-ownership";
import { VENDOR_NAME_RE } from "./agent-agnostic";

describe("useAgentSessionOwnership (AC-10/11/14)", () => {
  const cases: Array<{
    owner: SessionOwner;
    connection: ConnectionPhase;
    sessionId: string | null;
    inputLocked: boolean;
    isDriving: boolean;
    hasLiveSession: boolean;
  }> = [
    {
      owner: "agent",
      connection: "live",
      sessionId: "s1",
      inputLocked: true,
      isDriving: true,
      hasLiveSession: true,
    },
    {
      owner: "agent",
      connection: "reconnecting",
      sessionId: "s1",
      inputLocked: true,
      isDriving: false,
      hasLiveSession: false,
    },
    {
      owner: "agent",
      connection: "disconnected",
      sessionId: null,
      inputLocked: true,
      isDriving: false,
      hasLiveSession: false,
    },
    {
      owner: "user",
      connection: "live",
      sessionId: "s1",
      inputLocked: false,
      isDriving: false,
      hasLiveSession: true,
    },
  ];

  it.each(cases)(
    "owner=$owner connection=$connection → locked=$inputLocked driving=$isDriving live=$hasLiveSession",
    ({ owner, connection, sessionId, inputLocked, isDriving, hasLiveSession }) => {
      const { result } = renderHook(() =>
        useAgentSessionOwnership({
          sessionId,
          owner,
          connection,
          onDisconnect: () => {},
          onReconnect: () => {},
        }),
      );
      expect(result.current.inputLocked).toBe(inputLocked);
      expect(result.current.isDriving).toBe(isDriving);
      expect(result.current.hasLiveSession).toBe(hasLiveSession);
    },
  );
});

describe("agentSessionChipLabel (AC-20 — agent-agnostic)", () => {
  it("never emits a vendor string", () => {
    const labels = [
      agentSessionChipLabel("agent", "live"),
      agentSessionChipLabel("agent", "reconnecting"),
      agentSessionChipLabel("agent", "disconnected"),
      agentSessionChipLabel("user", "live"),
    ];
    for (const l of labels) expect(VENDOR_NAME_RE.test(l)).toBe(false);
    expect(labels).toContain("Agent driving");
    expect(labels).toContain("You're in control");
    expect(labels).toContain("Reconnecting…");
  });
});

/** A controlled harness mirroring how the git-sync store drives the primitive:
 *  the consumer owns `{ owner, connection }`; Disconnect/Reconnect mutate it.
 *  Reconnect models the async relaunch as reconnecting → live. */
function Harness({
  initial = { owner: "agent", connection: "live" },
  sessionId = "bound-session",
  onChange,
}: {
  initial?: AgentSessionState;
  sessionId?: string | null;
  onChange?: (s: AgentSessionState) => void;
}): React.JSX.Element {
  const [state, setState] = useState<AgentSessionState>(initial);
  const update = (s: AgentSessionState) => {
    setState(s);
    onChange?.(s);
  };
  return (
    <SupervisedAgentSession
      binding={{
        sessionId,
        owner: state.owner,
        connection: state.connection,
        onDisconnect: () => update({ owner: "user", connection: state.connection }),
        // Models AC-13: relaunch lands live + agent owns again.
        onReconnect: () => update({ owner: "agent", connection: "live" }),
      }}
    >
      {(api) => <div data-testid="lock-state">{String(api.inputLocked)}</div>}
    </SupervisedAgentSession>
  );
}

describe("SupervisedAgentSession chip + lock (AC-10/11/12/13)", () => {
  it("agent-driving shows 'Agent driving' + Disconnect; children are input-locked", () => {
    render(<Harness />);
    expect(screen.getByText("Agent driving")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Disconnect/ })).toBeInTheDocument();
    expect(screen.getByTestId("lock-state")).toHaveTextContent("true");
  });

  it("Disconnect → 'You're in control', children unlocked, Reconnect appears", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(screen.getByRole("button", { name: /Disconnect/ }));
    expect(screen.getByText("You're in control")).toBeInTheDocument();
    expect(screen.getByTestId("lock-state")).toHaveTextContent("false");
    expect(screen.getByRole("button", { name: /Reconnect/ })).toBeInTheDocument();
  });

  it("Reconnect → flips back to 'Agent driving' and re-locks", async () => {
    const user = userEvent.setup();
    render(<Harness initial={{ owner: "user", connection: "live" }} />);
    await user.click(screen.getByRole("button", { name: /Reconnect/ }));
    expect(screen.getByText("Agent driving")).toBeInTheDocument();
    expect(screen.getByTestId("lock-state")).toHaveTextContent("true");
  });

  it("agent + disconnected shows 'Disconnected' + Reconnect, never 'Agent driving' (AC-14)", () => {
    render(<Harness initial={{ owner: "agent", connection: "disconnected" }} sessionId={null} />);
    expect(screen.getByText("Disconnected")).toBeInTheDocument();
    expect(screen.queryByText("Agent driving")).not.toBeInTheDocument();
    // No live session to disconnect from → the action is Reconnect.
    expect(screen.getByRole("button", { name: /Reconnect/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Disconnect/ })).not.toBeInTheDocument();
  });

  it("reconnecting renders the transitional chip with Disconnect disabled (never claims driving)", () => {
    render(<Harness initial={{ owner: "agent", connection: "reconnecting" }} />);
    expect(screen.getByText("Reconnecting…")).toBeInTheDocument();
    expect(screen.queryByText("Agent driving")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Disconnect/ })).toBeDisabled();
    // Still input-locked while the agent owns, even mid-reconnect.
    expect(screen.getByTestId("lock-state")).toHaveTextContent("true");
  });

  it("works with a non-git-sync binding (AC-17 reusable primitive)", async () => {
    // A SW-DR-style binding: different session id, same primitive, full toggle.
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<Harness sessionId="doc-review-session" onChange={onChange} />);
    await user.click(screen.getByRole("button", { name: /Disconnect/ }));
    expect(onChange).toHaveBeenCalledWith({ owner: "user", connection: "live" });
    expect(screen.getByText("You're in control")).toBeInTheDocument();
  });
});

describe("SupervisedAgentSession task-state badge (SW-AW-4 AC-7/10)", () => {
  const binding = {
    sessionId: "s1",
    owner: "agent" as SessionOwner,
    connection: "live" as ConnectionPhase,
    onDisconnect: () => {},
    onReconnect: () => {},
  };

  it("taskStateBadgeLabel is null while running, labelled otherwise", () => {
    expect(taskStateBadgeLabel("running")).toBeNull();
    expect(taskStateBadgeLabel("awaiting-answer")).toBe("Needs answer");
    expect(taskStateBadgeLabel("needs-attention")).toBe("Needs you");
    expect(taskStateBadgeLabel("failed")).toBe("Error");
    expect(taskStateBadgeLabel("done")).toBe("Ended");
  });

  it("renders NO task badge for a plain (unmanaged) session", () => {
    render(<SupervisedAgentSession binding={binding} />);
    expect(screen.queryByRole("status", { name: /Task status/ })).not.toBeInTheDocument();
  });

  it("renders NO task badge while running (ownership chip suffices)", () => {
    render(<SupervisedAgentSession binding={binding} taskState="running" />);
    expect(screen.queryByRole("status", { name: /Task status/ })).not.toBeInTheDocument();
  });

  it.each(["awaiting-answer", "needs-attention", "failed", "done"] as const)(
    "renders the %s badge orthogonally to the ownership chip",
    (taskState) => {
      render(<SupervisedAgentSession binding={binding} taskState={taskState} />);
      const badge = screen.getByRole("status", { name: /Task status/ });
      expect(badge).toHaveAttribute("data-task-state", taskState);
      // Ownership chip is unaffected — still shows the agent-driving label.
      expect(screen.getByText("Agent driving")).toBeInTheDocument();
    },
  );

  it("renders an accessible Error badge with the shared error mark", () => {
    render(<SupervisedAgentSession binding={binding} taskState="failed" />);
    const badge = screen.getByRole("status", { name: "Task status: Error" });
    expect(badge).toHaveAttribute("data-task-state", "failed");
    expect(badge.querySelector("[data-mark]")).toHaveAttribute("data-mark", "error");
  });
});
