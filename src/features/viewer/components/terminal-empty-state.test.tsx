// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { TerminalEmptyState } from "./terminal-empty-state";

describe("TerminalEmptyState — SW-EW-18 AC-8", () => {
  it("renders the empty state (no spawnable bare terminal)", () => {
    render(<TerminalEmptyState onOpenWorkspace={vi.fn()} />);
    expect(screen.getByTestId("terminal-empty-state")).toBeInTheDocument();
    expect(screen.getByText(/No workspace open/i)).toBeInTheDocument();
  });

  it("the affordance opens a workspace (not a terminal)", () => {
    const onOpenWorkspace = vi.fn();
    render(<TerminalEmptyState onOpenWorkspace={onOpenWorkspace} />);
    fireEvent.click(screen.getByTestId("terminal-empty-state-open"));
    expect(onOpenWorkspace).toHaveBeenCalledTimes(1);
  });

  it("the CTA reads 'Open workspace' (SW-EW-30 relabel)", () => {
    render(<TerminalEmptyState onOpenWorkspace={vi.fn()} />);
    expect(screen.getByTestId("terminal-empty-state-open")).toHaveTextContent("Open workspace");
  });
});

describe("TerminalEmptyState — background-aware copy (SW-EW-30)", () => {
  it("with no background workspaces, nudges opening the first one", () => {
    render(<TerminalEmptyState onOpenWorkspace={vi.fn()} backgroundCount={0} />);
    expect(
      screen.getByText(/Open a story's workspace to start a terminal here/i),
    ).toBeInTheDocument();
    // SW-LN-B37: plain copy, no CLI jargon.
    expect(screen.queryByText(/epic-integration|The CLI runs/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/running in the background/i)).not.toBeInTheDocument();
  });

  it("acknowledges background workspaces (plural) when present", () => {
    render(<TerminalEmptyState onOpenWorkspace={vi.fn()} backgroundCount={3} />);
    expect(screen.getByText(/3 workspaces still running in the background/i)).toBeInTheDocument();
  });

  it("uses the singular form for exactly one background workspace", () => {
    render(<TerminalEmptyState onOpenWorkspace={vi.fn()} backgroundCount={1} />);
    expect(screen.getByText(/1 workspace still running in the background/i)).toBeInTheDocument();
    // not the plural "workspaces"
    expect(screen.queryByText(/1 workspaces /i)).not.toBeInTheDocument();
  });

  it("still offers 'Open workspace' even with background workspaces present", () => {
    const onOpenWorkspace = vi.fn();
    render(<TerminalEmptyState onOpenWorkspace={onOpenWorkspace} backgroundCount={2} />);
    fireEvent.click(screen.getByTestId("terminal-empty-state-open"));
    expect(onOpenWorkspace).toHaveBeenCalledTimes(1);
  });
});

describe("TerminalEmptyState — Start an agent (G6)", () => {
  it("offers Start an agent first, beside Open workspace, and runs it", () => {
    const onStartAgent = vi.fn();
    render(<TerminalEmptyState onOpenWorkspace={vi.fn()} onStartAgent={onStartAgent} />);
    const start = screen.getByTestId("terminal-empty-state-start-agent");
    expect(start).toHaveTextContent("Start an agent");
    expect(screen.getByTestId("terminal-empty-state-open")).toBeInTheDocument();
    expect(
      screen.getByText(/Start an agent here, or open a story's workspace/i),
    ).toBeInTheDocument();
    fireEvent.click(start);
    expect(onStartAgent).toHaveBeenCalledTimes(1);
  });

  it("without a way to start one, offers only Open workspace", () => {
    render(<TerminalEmptyState onOpenWorkspace={vi.fn()} />);
    expect(screen.queryByTestId("terminal-empty-state-start-agent")).toBeNull();
  });
});

describe("TerminalEmptyState — the hide line (SV-GHI-2)", () => {
  afterEach(() => useShortcutsStore.setState({ shortcuts: {} }));

  it("says the current binding hides the terminal", () => {
    render(<TerminalEmptyState onOpenWorkspace={vi.fn()} />);
    const line = screen.getByTestId("terminal-empty-state-hide-hint");
    expect(line).toHaveTextContent(`${formatKeystrokeDisplay("CmdOrCtrl+\\")} hides the terminal`);
    expect(line.querySelector("kbd")).toHaveTextContent(formatKeystrokeDisplay("CmdOrCtrl+\\"));
  });

  it("follows a rebind", () => {
    render(<TerminalEmptyState onOpenWorkspace={vi.fn()} />);
    act(() =>
      useShortcutsStore.setState({ shortcuts: { "viewer.toggle-terminal": "CmdOrCtrl+Alt+J" } }),
    );
    expect(screen.getByTestId("terminal-empty-state-hide-hint")).toHaveTextContent(
      `${formatKeystrokeDisplay("CmdOrCtrl+Alt+J")} hides the terminal`,
    );
  });

  it("is absent when the action is unbound", () => {
    useShortcutsStore.setState({ shortcuts: { "viewer.toggle-terminal": "" } });
    render(<TerminalEmptyState onOpenWorkspace={vi.fn()} />);
    expect(screen.queryByTestId("terminal-empty-state-hide-hint")).toBeNull();
    expect(screen.queryByText(/hides the terminal/)).toBeNull();
  });

  it("is plain text, not a control or a tab stop", () => {
    render(<TerminalEmptyState onOpenWorkspace={vi.fn()} />);
    const line = screen.getByTestId("terminal-empty-state-hide-hint");
    expect(line.tagName).toBe("P");
    expect(line.querySelector("button, a, [tabindex], [role]")).toBeNull();
    expect(line.closest("button, a")).toBeNull();
    expect(line).not.toHaveAttribute("tabindex");
  });
});
