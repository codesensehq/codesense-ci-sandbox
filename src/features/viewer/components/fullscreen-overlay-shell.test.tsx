// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, afterEach } from "vitest";
import { useEffect, useState } from "react";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { FullscreenOverlayShell } from "./fullscreen-overlay-shell";

afterEach(cleanup);

function renderShell(
  opts: {
    isFullscreen?: boolean;
    onExit?: () => void;
    testId?: string;
    header?: React.ReactNode;
    body?: React.ReactNode;
  } = {},
) {
  return render(
    <FullscreenOverlayShell
      isFullscreen={opts.isFullscreen ?? true}
      onExit={opts.onExit ?? (() => {})}
      testId={opts.testId ?? "test-shell"}
      header={opts.header}
    >
      {opts.body ?? <div data-testid="body">body</div>}
    </FullscreenOverlayShell>,
  );
}

describe("FullscreenOverlayShell", () => {
  it("docked: no fixed overlay, header not rendered, data-fullscreen=false", () => {
    renderShell({ isFullscreen: false, header: <div data-testid="hdr" /> });
    const shell = screen.getByTestId("test-shell");
    expect(shell.dataset.fullscreen).toBe("false");
    expect(shell.style.position).toBe("");
    expect(screen.queryByTestId("hdr")).toBeNull();
    // Body always renders.
    expect(screen.getByTestId("body")).toBeTruthy();
  });

  it("maximized: fixed overlay + header rendered + data-fullscreen=true", () => {
    renderShell({ isFullscreen: true, header: <div data-testid="hdr" /> });
    const shell = screen.getByTestId("test-shell");
    expect(shell.dataset.fullscreen).toBe("true");
    expect(shell.style.position).toBe("fixed");
    expect(shell.style.zIndex).toBe("40");
    expect(screen.getByTestId("hdr")).toBeTruthy();
  });

  it("honors the testId prop (per-view test addressing)", () => {
    renderShell({ testId: "board-fullscreen-shell" });
    expect(screen.getByTestId("board-fullscreen-shell")).toBeTruthy();
  });

  it("Escape exits while maximized", () => {
    const onExit = vi.fn();
    renderShell({ isFullscreen: true, onExit });
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onExit).toHaveBeenCalledTimes(1);
  });

  it("Escape does NOT exit while docked (listener unmounted)", () => {
    const onExit = vi.fn();
    renderShell({ isFullscreen: false, onExit });
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onExit).not.toHaveBeenCalled();
  });

  it("bails when the Escape event was already handled (defaultPrevented) — AC-2 precedence", () => {
    const onExit = vi.fn();
    renderShell({ isFullscreen: true, onExit });
    // Simulate a Radix DismissableLayer that consumed Escape in the capture phase
    // (closing a context menu / dropdown) and called preventDefault.
    document.addEventListener("keydown", (e) => e.preventDefault(), { capture: true, once: true });
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onExit).not.toHaveBeenCalled();
  });

  it("ignores Escape while typing in an input / textarea / contenteditable", () => {
    const onExit = vi.fn();
    renderShell({
      isFullscreen: true,
      onExit,
      body: (
        <div>
          <input data-testid="in" />
          <textarea data-testid="ta" />
          <div data-testid="ce" contentEditable />
        </div>
      ),
    });
    for (const id of ["in", "ta", "ce"]) {
      fireEvent.keyDown(screen.getByTestId(id), { key: "Escape" });
    }
    expect(onExit).not.toHaveBeenCalled();
  });

  it("ignores Escape originating inside a terminal (.xterm keeps its own)", () => {
    const onExit = vi.fn();
    renderShell({
      isFullscreen: true,
      onExit,
      body: (
        <div className="xterm">
          <span data-testid="term-child">x</span>
        </div>
      ),
    });
    fireEvent.keyDown(screen.getByTestId("term-child"), { key: "Escape" });
    expect(onExit).not.toHaveBeenCalled();
  });

  // SW-AU-72 (AC-4): dock/maximize/back must preserve the selected entity and keyboard focus. The shell
  // keeps its children in ONE always-mounted container (a CSS flip, never a remount), so any panel's
  // selection state (e.g. the Triggers axis' selected trigger) survives the whole cycle. Guard the
  // mount-preservation property directly: a stateful child selected while docked stays selected after
  // maximize → restore, and never remounts.
  it("SW-AU-72 (AC-4): the same child instance persists across docked↔maximized↔docked (selection survives)", () => {
    let mounts = 0;
    function StatefulRow() {
      const [selected, setSelected] = useState(false);
      useEffect(() => {
        mounts += 1;
      }, []);
      return (
        <button data-testid="row" aria-pressed={selected} onClick={() => setSelected(true)}>
          trigger row
        </button>
      );
    }
    const view = (isFullscreen: boolean) => (
      <FullscreenOverlayShell isFullscreen={isFullscreen} onExit={() => {}} testId="test-shell">
        <StatefulRow />
      </FullscreenOverlayShell>
    );

    const { rerender } = render(view(false));
    const row = screen.getByTestId("row");
    fireEvent.click(row); // select while docked
    row.focus(); // give it keyboard focus
    expect(row.getAttribute("aria-pressed")).toBe("true");
    expect(document.activeElement).toBe(row);

    rerender(view(true)); // maximize
    rerender(view(false)); // back to docked

    expect(mounts).toBe(1); // never remounted across the cycle
    expect(screen.getByTestId("row").getAttribute("aria-pressed")).toBe("true"); // selection intact
    expect(document.activeElement).toBe(screen.getByTestId("row")); // keyboard focus preserved (AC-4)
  });

  it("restores focus to the previously-focused element on exit", () => {
    const trigger = document.createElement("button");
    document.body.appendChild(trigger);
    trigger.focus();
    expect(document.activeElement).toBe(trigger);

    const { rerender } = renderShell({ isFullscreen: true });
    // Move focus elsewhere while maximized.
    (screen.getByTestId("body") as HTMLElement).tabIndex = -1;
    (screen.getByTestId("body") as HTMLElement).focus();

    rerender(
      <FullscreenOverlayShell isFullscreen={false} onExit={() => {}} testId="test-shell">
        <div data-testid="body">body</div>
      </FullscreenOverlayShell>,
    );
    expect(document.activeElement).toBe(trigger);
    trigger.remove();
  });
});

describe("FullscreenOverlayShell — pane scope (SW-LN-B40)", () => {
  it("covers only the right pane's box, never the whole app", () => {
    const rect = {
      top: 40,
      left: 900,
      width: 500,
      height: 700,
      right: 1400,
      bottom: 740,
      x: 900,
      y: 40,
    };
    const spy = vi
      .spyOn(HTMLElement.prototype, "getBoundingClientRect")
      .mockImplementation(function (this: HTMLElement) {
        return (
          this.hasAttribute("data-right-pane-root") ? rect : { ...rect, width: 0, height: 0 }
        ) as DOMRect;
      });
    try {
      render(
        <div data-right-pane-root>
          <FullscreenOverlayShell isFullscreen scope="pane" onExit={() => {}} testId="pane-shell">
            <div>body</div>
          </FullscreenOverlayShell>
        </div>,
      );
      const shell = screen.getByTestId("pane-shell");
      expect(shell.dataset.maximizeScope).toBe("pane");
      expect(shell.style.left).toBe("900px");
      expect(shell.style.width).toBe("500px");
      expect(shell.style.top).toBe("40px");
    } finally {
      spy.mockRestore();
    }
  });
});
