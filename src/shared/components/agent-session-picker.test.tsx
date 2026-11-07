// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@/test-utils";

import { AgentSessionPicker } from "@/shared/components/agent-session-picker";
import type { AgentTabOption } from "@/shared/hooks/use-workspace-agent-tabs";

function opt(sessionId: string, name: string, live: boolean | null, role?: string): AgentTabOption {
  return { id: sessionId, name, sessionId, role, live };
}

const baseProps = {
  options: [] as AgentTabOption[],
  onStartNew: vi.fn(),
  onConnectToTab: vi.fn(),
  recovery: false,
  onRebind: vi.fn(),
  boundSessionId: null as string | null,
  connection: "disconnected" as const,
};

describe("AgentSessionPicker (AC-4/AC-6/AC-3a)", () => {
  it("AC-6: Start new session fires onStartNew", () => {
    const onStartNew = vi.fn();
    render(<AgentSessionPicker {...baseProps} onStartNew={onStartNew} />);
    fireEvent.click(screen.getByRole("button", { name: /start new session/i }));
    expect(onStartNew).toHaveBeenCalledTimes(1);
  });

  it("AC-4: lists open tabs and binds the chosen one by sessionId", () => {
    const onConnectToTab = vi.fn();
    render(
      <AgentSessionPicker
        {...baseProps}
        options={[opt("shell-1", "Agent A", true, "test-agent")]}
        onConnectToTab={onConnectToTab}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /connect to agent a/i }));
    expect(onConnectToTab).toHaveBeenCalledWith("shell-1");
  });

  it("AC-4: shows a state hint — ● live vs ◐ not-responding", () => {
    render(
      <AgentSessionPicker
        {...baseProps}
        options={[
          opt("shell-1", "Live One", true),
          opt("shell-2", "Dead One", false, "test-agent"),
        ]}
      />,
    );
    expect(screen.getByTestId("tab-state-shell-1")).toHaveAttribute("data-live", "true");
    expect(screen.getByTestId("tab-state-shell-2")).toHaveAttribute("data-live", "false");
  });

  it("AC-4: a not-responding (◐) tab is NOT connectable — attach would prime a dead shell", () => {
    const onConnectToTab = vi.fn();
    render(
      <AgentSessionPicker
        {...baseProps}
        options={[opt("shell-2", "Dead One", false, "test-agent")]}
        onConnectToTab={onConnectToTab}
      />,
    );
    const btn = screen.getByRole("button", { name: /connect to dead one/i });
    expect(btn).toBeDisabled();
    fireEvent.click(btn);
    expect(onConnectToTab).not.toHaveBeenCalled();
  });

  it("AC-3a: recovery surfaces the 'open its terminal, then Re-bind' affordance, Re-bind reachable", () => {
    const onRebind = vi.fn();
    render(<AgentSessionPicker {...baseProps} recovery onRebind={onRebind} />);
    expect(screen.getByText(/isn't responding/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /re-bind/i }));
    expect(onRebind).toHaveBeenCalledTimes(1);
  });

  it("does not render the recovery affordance when not in recovery", () => {
    render(<AgentSessionPicker {...baseProps} recovery={false} />);
    expect(screen.queryByText(/isn't responding/i)).not.toBeInTheDocument();
  });
});
