// SPDX-License-Identifier: MIT
import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const addTerminalTab = vi.fn(() => Promise.resolve());
const expandTerminalOnOpen = vi.fn();
const focusActiveWorkspaceTerminal = vi.fn();

vi.mock("@/shared/stores/viewer-store", () => ({
  createTerminalTab: (title?: string) => ({ sessionId: "shell-fresh-1", name: title }),
  useViewerStore: { getState: () => ({ addTerminalTab, expandTerminalOnOpen }) },
}));
vi.mock("@/features/viewer/lib/focus-cascade", () => ({
  focusActiveWorkspaceTerminal: (...a: unknown[]) => focusActiveWorkspaceTerminal(...(a as [])),
}));

import { TerminalGhostHint } from "@/features/viewer/components/terminal-ghost-hint";
import { useTerminalGhostHintStore } from "@/features/viewer/stores/terminal-ghost-hint-store";

import { OPEN_AGENT_GHOST_HINT, openFreshAgentTerminal } from "./open-fresh-agent";

afterEach(() => {
  useTerminalGhostHintStore.setState({ hints: {} });
  addTerminalTab.mockClear();
  expandTerminalOnOpen.mockClear();
  focusActiveWorkspaceTerminal.mockClear();
});

describe("openFreshAgentTerminal — new work is a terminal, not a form (§8 rule 11)", () => {
  it("opens a fresh terminal, reveals + focuses it, and registers the ghost hint", () => {
    const sessionId = openFreshAgentTerminal("org:team");
    expect(addTerminalTab).toHaveBeenCalledWith("org:team", expect.objectContaining({ sessionId }));
    expect(expandTerminalOnOpen).toHaveBeenCalledWith("org:team");
    expect(focusActiveWorkspaceTerminal).toHaveBeenCalledWith("org:team");
    expect(useTerminalGhostHintStore.getState().hints[sessionId]).toBe(OPEN_AGENT_GHOST_HINT);
  });

  it("uses the constant invitation copy, never a blank prompt", () => {
    expect(OPEN_AGENT_GHOST_HINT).toBe("Tell the agent what to do…");
  });
});

describe("TerminalGhostHint overlay", () => {
  it("renders nothing for an ordinary tab with no registered hint", () => {
    render(<TerminalGhostHint sessionId="ordinary" />);
    expect(screen.queryByTestId("terminal-ghost-hint")).not.toBeInTheDocument();
  });

  it("paints the ghosted placeholder for a fresh agent terminal", () => {
    useTerminalGhostHintStore.getState().set("shell-fresh-1", OPEN_AGENT_GHOST_HINT);
    render(<TerminalGhostHint sessionId="shell-fresh-1" />);
    const ghost = screen.getByTestId("terminal-ghost-hint");
    expect(ghost).toHaveTextContent("Tell the agent what to do…");
    // Decorative + non-blocking so it never intercepts focus/typing.
    expect(ghost).toHaveAttribute("aria-hidden");
    expect(ghost.className).toContain("pointer-events-none");
  });

  it("disappears once the hint is cleared (the first keystroke)", () => {
    useTerminalGhostHintStore.getState().set("shell-fresh-1", OPEN_AGENT_GHOST_HINT);
    const { rerender } = render(<TerminalGhostHint sessionId="shell-fresh-1" />);
    expect(screen.getByTestId("terminal-ghost-hint")).toBeInTheDocument();
    useTerminalGhostHintStore.getState().clear("shell-fresh-1");
    rerender(<TerminalGhostHint sessionId="shell-fresh-1" />);
    expect(screen.queryByTestId("terminal-ghost-hint")).not.toBeInTheDocument();
  });
});
