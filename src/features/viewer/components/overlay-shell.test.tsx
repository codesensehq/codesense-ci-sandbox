// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@/test-utils";

import {
  OverlayShell,
  OverlayHeader,
  OverlayCenteredState,
  useOverlayEscape,
} from "./overlay-shell";

// The single token set every overlay's chrome must carry (AC-1): all colors are
// CSS variables — no hardcoded hexes, no `--border` vs `var(--border)` drift.
const SHELL_TOKENS = [
  "border-[var(--border)]",
  "bg-[var(--popover)]",
  "text-[var(--popover-foreground)]",
];

describe("OverlayShell — chrome (AC-1)", () => {
  it("the inset size is the expanded popover chrome, token-driven", () => {
    render(
      <OverlayShell size="inset" testId="shell" ariaLabel="Test overlay">
        <div>body</div>
      </OverlayShell>,
    );
    const shell = screen.getByTestId("shell");
    expect(shell).toHaveAttribute("role", "dialog");
    expect(shell).toHaveAttribute("aria-label", "Test overlay");
    for (const token of SHELL_TOKENS) expect(shell.className).toContain(token);
    expect(shell.className).toContain("inset-3");
    expect(shell.className).toContain("shadow-xl");
    // A flex ROW that stays text-selectable — the rail + section layout depends
    // on both, so guard against a silent drop.
    expect(shell.className).toContain("select-text");
    expect(shell.className).toMatch(/\bflex\b/);
    // No raw hex color anywhere in the chrome class string.
    expect(shell.className).not.toMatch(/#[0-9a-fA-F]{3,6}/);
  });

  it("the card size is the compact corner card, same token set", () => {
    render(
      <OverlayShell size="card" testId="shell" ariaLabel="Card overlay" className="w-[300px]">
        <div>body</div>
      </OverlayShell>,
    );
    const shell = screen.getByTestId("shell");
    for (const token of SHELL_TOKENS) expect(shell.className).toContain(token);
    expect(shell.className).toContain("right-2");
    expect(shell.className).toContain("shadow-md");
    // A flex COLUMN — the card stacks header over body.
    expect(shell.className).toContain("flex-col");
    // Caller-supplied sizing merges in.
    expect(shell.className).toContain("w-[300px]");
  });

  it("forwards tabIndex + onKeyDown to the surface", () => {
    const onKeyDown = vi.fn();
    render(
      <OverlayShell testId="shell" ariaLabel="x" tabIndex={0} onKeyDown={onKeyDown}>
        <div>body</div>
      </OverlayShell>,
    );
    const shell = screen.getByTestId("shell");
    expect(shell).toHaveAttribute("tabindex", "0");
    fireEvent.keyDown(shell, { key: "Escape" });
    expect(onKeyDown).toHaveBeenCalled();
  });
});

describe("OverlayHeader — header frame (AC-1 / RD-3)", () => {
  it("renders icon, title, the stats slot, and the actions area in order", () => {
    render(
      <OverlayHeader
        icon={<span data-testid="icon" />}
        title={<span data-testid="title">Title</span>}
        stats={<span data-testid="stats" />}
        actions={<button data-testid="action">x</button>}
      />,
    );
    // All four slots render; the border-b chrome is on the row.
    for (const id of ["icon", "title", "stats", "action"]) {
      expect(screen.getByTestId(id)).toBeInTheDocument();
    }
  });
});

describe("OverlayCenteredState — shared body states (AC-1 / RD-2)", () => {
  it("renders a muted message by default", () => {
    render(<OverlayCenteredState testId="empty">No source changes</OverlayCenteredState>);
    const el = screen.getByTestId("empty");
    expect(el).toHaveTextContent("No source changes");
    expect(el.className).toContain("text-muted-foreground");
  });

  it("renders the destructive tone with an alert role and an action", () => {
    render(
      <OverlayCenteredState
        testId="err"
        tone="destructive"
        role="alert"
        action={<button>Retry</button>}
      >
        Couldn’t load
      </OverlayCenteredState>,
    );
    const el = screen.getByTestId("err");
    expect(el).toHaveAttribute("role", "alert");
    expect(el.className).toContain("text-destructive");
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
  });
});

describe("useOverlayEscape — shared guarded dismiss (AC-5)", () => {
  function Harness({
    enabled = true,
    zoomActive = false,
    onDismiss,
  }: {
    enabled?: boolean;
    zoomActive?: boolean;
    onDismiss: () => void;
  }) {
    // The mock identity is already stable across this harness's rerenders, so it
    // can drive the hook directly (no useCallback needed).
    useOverlayEscape({ enabled, zoomActive, onDismiss });
    return (
      <div>
        <input data-testid="field" />
        <div className="xterm">
          <textarea data-testid="term" />
        </div>
      </div>
    );
  }

  it("Escape dismisses when enabled", () => {
    const onDismiss = vi.fn();
    render(<Harness onDismiss={onDismiss} />);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("does not dismiss while disabled or zoomed", () => {
    const onDismiss = vi.fn();
    const { rerender } = render(<Harness enabled={false} onDismiss={onDismiss} />);
    fireEvent.keyDown(document, { key: "Escape" });
    rerender(<Harness zoomActive onDismiss={onDismiss} />);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onDismiss).not.toHaveBeenCalled();
  });

  it("leaves Escape to text fields and the terminal", () => {
    const onDismiss = vi.fn();
    render(<Harness onDismiss={onDismiss} />);
    fireEvent.keyDown(screen.getByTestId("field"), { key: "Escape" });
    fireEvent.keyDown(screen.getByTestId("term"), { key: "Escape" });
    expect(onDismiss).not.toHaveBeenCalled();
  });

  it("SW-FX-140: an owning overlay takes Escape from the terminal and consumes it", () => {
    const onDismiss = vi.fn();
    const onTermKey = vi.fn();
    function Owned() {
      useOverlayEscape({
        enabled: true,
        zoomActive: false,
        onDismiss,
        onTerminalEscape: onDismiss,
      });
      return (
        <div className="xterm">
          <textarea data-testid="term" onKeyDown={onTermKey} />
        </div>
      );
    }
    render(<Owned />);
    fireEvent.keyDown(screen.getByTestId("term"), { key: "Escape" });
    expect(onDismiss).toHaveBeenCalledTimes(1);
    expect(onTermKey).not.toHaveBeenCalled();
  });

  it("uses the latest onDismiss WITHOUT re-subscribing — an inline closure needs no memoization", () => {
    const addSpy = vi.spyOn(document, "addEventListener");
    const removeSpy = vi.spyOn(document, "removeEventListener");
    const first = vi.fn();
    const second = vi.fn();
    const { rerender } = render(<Harness onDismiss={first} />);
    const addsAfterMount = addSpy.mock.calls.filter(([type]) => type === "keydown").length;

    // A new inline onDismiss on every render must NOT tear down + re-add the
    // document listener — that is the whole point of the ref (deps are only
    // [enabled, zoomActive]).
    rerender(<Harness onDismiss={second} />);
    expect(addSpy.mock.calls.filter(([type]) => type === "keydown").length).toBe(addsAfterMount);
    expect(removeSpy.mock.calls.filter(([type]) => type === "keydown").length).toBe(0);

    // …and the still-subscribed listener fires the LATEST closure.
    fireEvent.keyDown(document, { key: "Escape" });
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);

    addSpy.mockRestore();
    removeSpy.mockRestore();
  });
});
