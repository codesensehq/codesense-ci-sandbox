// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, fireEvent } from "@/test-utils";
import userEvent from "@testing-library/user-event";
import { DndContext, PointerSensor, useSensor, useSensors, useDraggable } from "@dnd-kit/core";

import { TerminalTabBar } from "./terminal-tab-bar";
import { sessionsRunningProcesses } from "@/tauri/commands";
import type { TerminalTab } from "@/shared/stores/viewer-store";
import { REGISTERED_AGENT_CLIS } from "@/shared/lib/agent-runtimes";

vi.mock("@/tauri/commands", () => ({ sessionsRunningProcesses: vi.fn() }));

const mockJobs = vi.mocked(sessionsRunningProcesses);

const TAB: TerminalTab = { id: "t1", name: "sh", sessionId: "sess-1" };

const baseProps = {
  tabs: [TAB],
  activeTabId: "t1",
  onSelectTab: vi.fn(),
  onCloseTab: vi.fn(),
  onNewShell: vi.fn(),
  onReorderTab: vi.fn(),
  onRenameTab: vi.fn(),
};

describe("@P1 TerminalTabBar — adaptive close tooltip (PT-2 AC-4)", () => {
  beforeEach(() => vi.clearAllMocks());

  // SW-FX-80: the close affordance's adaptive label moved from a native `title`
  // to a shared `InfoTip` (Radix tooltip). The trigger is the icon-only close
  // span; its content is revealed on hover and read from `role="tooltip"`.
  function closeTrigger(container: HTMLElement): HTMLElement {
    return container.querySelector<HTMLElement>('[data-slot="tooltip-trigger"]')!;
  }

  it("reads Hide before any verdict (the verb that never loses anything)", async () => {
    mockJobs.mockReturnValue(new Promise(() => {})); // never answers
    const { container } = render(<TerminalTabBar {...baseProps} />);
    await userEvent.hover(closeTrigger(container));
    expect(await screen.findByRole("tooltip")).toHaveTextContent("Hide · keeps running");
  });

  it("hovering the × probes and reads End for a shell at its prompt", async () => {
    mockJobs.mockResolvedValue([{ foreground: null, background: [] }]);
    const { container } = render(<TerminalTabBar {...baseProps} />);
    await userEvent.hover(closeTrigger(container));
    expect(mockJobs).toHaveBeenCalledWith(["sess-1"]);
    expect(await screen.findByRole("tooltip")).toHaveTextContent(/^End \((Del|⌫)\)$/);
  });

  it("keyboard focus on the tab probes too, so its close already names the right verb", async () => {
    mockJobs.mockResolvedValue([{ foreground: null, background: [] }]);
    const { container } = render(<TerminalTabBar {...baseProps} />);
    fireEvent.focus(screen.getByText("sh").closest("button")!);
    expect(mockJobs).toHaveBeenCalledWith(["sess-1"]);
    mockJobs.mockReturnValue(new Promise(() => {})); // the hover's probe never answers
    await userEvent.hover(closeTrigger(container));
    expect(await screen.findByRole("tooltip")).toHaveTextContent(/^End /);
  });

  it("stays Hide when a command runs in the foreground, or detection is unavailable", async () => {
    mockJobs.mockResolvedValueOnce([{ foreground: "claude", background: [] }]);
    const { container } = render(<TerminalTabBar {...baseProps} />);
    await userEvent.hover(closeTrigger(container));
    expect(await screen.findByRole("tooltip")).toHaveTextContent("Hide · keeps running");
    mockJobs.mockRejectedValueOnce(new Error("ipc down"));
    fireEvent.focus(screen.getByText("sh").closest("button")!);
    await Promise.resolve();
    expect(screen.getByRole("tooltip")).toHaveTextContent(/^Hide · keeps running/);
  });
});

describe("TerminalTabBar — tab-chrome polish", () => {
  beforeEach(() => vi.clearAllMocks());

  // The close X's clickable area is a 24px square around the ~12px glyph (the
  // pointer-target floor), while staying reveal-on-hover and mouse-only.
  it("close affordance has a 24px hit target, out of the tab order", () => {
    const { container } = render(<TerminalTabBar {...baseProps} />);
    const closeSpan = container.querySelector<HTMLElement>('[data-slot="tooltip-trigger"]')!;
    expect(closeSpan.className).toMatch(/\bh-6\b/);
    expect(closeSpan.className).toMatch(/\bw-6\b/);
    expect(closeSpan).toHaveAttribute("tabindex", "-1");
    expect(closeSpan).toHaveAttribute("aria-hidden");
    // Still hover-revealed (unchanged behavior).
    expect(closeSpan.className).toContain("group-hover:opacity-100");
    // `cursor-pointer` on the close affordance.
    expect(closeSpan.className).toContain("cursor-pointer");
  });

  // The tab name tooltips its full text only when it is actually truncated, so
  // split-grid tabs aren't ambiguous. The rename span is cursor-pointer.
  it("tab name shows a title tooltip only when truncated, and is cursor-pointer", () => {
    render(<TerminalTabBar {...baseProps} />);
    const nameSpan = screen.getByText("sh");
    expect(nameSpan.className).toContain("cursor-pointer");
    // Fits: no tooltip.
    Object.defineProperty(nameSpan, "clientWidth", { configurable: true, value: 40 });
    Object.defineProperty(nameSpan, "scrollWidth", { configurable: true, value: 40 });
    fireEvent.mouseEnter(nameSpan);
    expect(nameSpan.hasAttribute("title")).toBe(false);
    // Cut off: the full name appears.
    Object.defineProperty(nameSpan, "scrollWidth", { configurable: true, value: 90 });
    fireEvent.mouseEnter(nameSpan);
    expect(nameSpan.getAttribute("title")).toBe("sh");
    // Widened again: the stale tooltip is dropped.
    Object.defineProperty(nameSpan, "clientWidth", { configurable: true, value: 120 });
    fireEvent.mouseEnter(nameSpan);
    expect(nameSpan.hasAttribute("title")).toBe(false);
  });

  // A rename request from the context menu enters inline-edit mode
  // (the same editor as double-click). The token is keyed by nonce.
  it("renameRequest enters inline-edit mode for the target tab", () => {
    const { rerender } = render(<TerminalTabBar {...baseProps} renameRequest={null} />);
    // No input yet.
    expect(document.querySelector("input")).toBeNull();
    rerender(<TerminalTabBar {...baseProps} renameRequest={{ tabId: "t1", nonce: 1 }} />);
    const input = document.querySelector<HTMLInputElement>("input")!;
    expect(input).not.toBeNull();
    expect(input.value).toBe("sh");
  });

  it("a repeat rename request for the tab already being edited keeps the typed text", () => {
    const { rerender } = render(
      <TerminalTabBar {...baseProps} renameRequest={{ tabId: "t1", nonce: 1 }} />,
    );
    const input = document.querySelector<HTMLInputElement>("input")!;
    fireEvent.change(input, { target: { value: "half-typed" } });
    rerender(<TerminalTabBar {...baseProps} renameRequest={{ tabId: "t1", nonce: 2 }} />);
    expect(document.querySelector<HTMLInputElement>("input")!.value).toBe("half-typed");
  });
});

// SLW-FX-10: regression at the REAL assembly. In a split tile the tab bar is
// nested inside the tile's dnd-kit drag handle (see workspace-terminal-panels.tsx
// `useDraggable` listeners spread onto the chrome wrapper). The isolated tab-bar
// test above never wrapped the bar in a DndContext + draggable, so it couldn't
// catch the tile-drag swallowing a tab-reorder press. This block reconstructs the
// nested structure and proves a press ON A TAB stays owned by the tab bar (reorders,
// does not escape to the tile-drag sensor), while a press on the non-tab chrome
// still reaches the drag handle (AC-2).
describe("@P1 TerminalTabBar — split-tile gesture ownership (SLW-FX-10)", () => {
  beforeEach(() => vi.clearAllMocks());

  const TWO_TABS: TerminalTab[] = [
    { id: "t1", name: "one", sessionId: "sess-1" },
    { id: "t2", name: "two", sessionId: "sess-2" },
  ];

  function rectAt(left: number, right: number): DOMRect {
    return {
      left,
      right,
      top: 0,
      bottom: 20,
      width: right - left,
      height: 20,
      x: left,
      y: 0,
      toJSON: () => ({}),
    } as DOMRect;
  }

  /**
   * Mirrors the assembled split tile: DndContext(PointerSensor distance:6) →
   * draggable wrapper (the tile-drag handle, listeners spread) → chrome, where
   * chrome = a non-tab identity region + the TerminalTabBar. `ancestorSpy` sits
   * ABOVE the draggable: it fires only if a pointerdown escapes the tab bar and
   * bubbles into the tile-drag handle — the exact leak SLW-FX-10 fixes.
   */
  function AssembledSplitTile(props: {
    onReorderTab: (from: number, to: number) => void;
    onDragStart: () => void;
    ancestorSpy: (e: React.PointerEvent) => void;
    onNewShell?: () => void;
  }) {
    const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 6 } }));
    return (
      <div onPointerDown={props.ancestorSpy} data-testid="tile-ancestor">
        <DndContext sensors={sensors} onDragStart={props.onDragStart}>
          <TileDragHandle>
            <div data-testid="non-tab-chrome" style={{ width: 40, height: 32 }}>
              id
            </div>
            <TerminalTabBar
              tabs={TWO_TABS}
              activeTabId="t1"
              onSelectTab={vi.fn()}
              onCloseTab={vi.fn()}
              onNewShell={props.onNewShell ?? vi.fn()}
              onReorderTab={props.onReorderTab}
              onRenameTab={vi.fn()}
            />
          </TileDragHandle>
        </DndContext>
      </div>
    );
  }

  function TileDragHandle({ children }: { children: React.ReactNode }) {
    // The real handle: dnd-kit listeners/attributes spread onto the wrapper that
    // contains the chrome (workspace-terminal-panels.tsx:585-589).
    const { attributes, listeners, setNodeRef } = useDraggable({ id: "ws-1" });
    return (
      <div ref={setNodeRef} {...listeners} {...attributes} data-testid="tile-drag-handle">
        {children}
      </div>
    );
  }

  it("AC-1/AC-5: press-drag on a tab reorders it and does NOT start the tile drag", () => {
    const onReorderTab = vi.fn();
    const onDragStart = vi.fn();
    const ancestorSpy = vi.fn();
    render(
      <AssembledSplitTile
        onReorderTab={onReorderTab}
        onDragStart={onDragStart}
        ancestorSpy={ancestorSpy}
      />,
    );

    const tabOne = screen.getByText("one").closest("button")!;
    const tabTwo = screen.getByText("two").closest("button")!;
    // Give the two tab wrappers (the ref'd divs) deterministic horizontal rects so
    // the tab bar's clientX hit-test resolves a drop target under happy-dom.
    tabOne.parentElement!.getBoundingClientRect = () => rectAt(0, 50);
    tabTwo.parentElement!.getBoundingClientRect = () => rectAt(50, 100);

    // Press tab "one", drag past the 6px sensor threshold into tab "two"'s column,
    // release. (pointermove/up bubble to the tab bar's container handlers.)
    fireEvent.pointerDown(tabOne, { button: 0, clientX: 25 });
    fireEvent.pointerMove(tabOne, { clientX: 75 });
    fireEvent.pointerUp(tabOne, { clientX: 75 });

    expect(onReorderTab).toHaveBeenCalledWith(0, 1);
    // `ancestorSpy` is the LOAD-BEARING assertion: it's a plain React onPointerDown
    // above the draggable, so it fires iff the press escapes the tab bar into the
    // tile-drag handle. (Drop the fix's stopPropagation and this is what turns red.)
    expect(ancestorSpy).not.toHaveBeenCalled();
    // Belt-and-suspenders. dnd-kit's PointerSensor doesn't fully arm under
    // happy-dom, so onDragStart wouldn't fire regardless — keep, but don't rely
    // on it alone; ancestorSpy above is the real proof.
    expect(onDragStart).not.toHaveBeenCalled();
  });

  it("SW-CW-2 AC-1: the active tab is weighted (font-medium), inactive tabs are not", () => {
    render(
      <AssembledSplitTile onReorderTab={vi.fn()} onDragStart={vi.fn()} ancestorSpy={vi.fn()} />,
    );
    const activeTab = screen.getByText("one").closest("button")!; // t1 is active
    const inactiveTab = screen.getByText("two").closest("button")!;
    expect(activeTab.className).toContain("font-medium");
    expect(inactiveTab.className).not.toContain("font-medium");
  });

  it("AC-2: press on the non-tab chrome still reaches the tile-drag handle", () => {
    const onReorderTab = vi.fn();
    const onDragStart = vi.fn();
    const ancestorSpy = vi.fn();
    render(
      <AssembledSplitTile
        onReorderTab={onReorderTab}
        onDragStart={onDragStart}
        ancestorSpy={ancestorSpy}
      />,
    );

    // A press on the identity/chrome region (outside the tab strip) must still
    // bubble to the drag handle — we did not globally kill tiling to fix tabs.
    fireEvent.pointerDown(screen.getByTestId("non-tab-chrome"), { button: 0, clientX: 10 });
    expect(ancestorSpy).toHaveBeenCalledTimes(1);
    expect(onReorderTab).not.toHaveBeenCalled();
  });

  // SLW-FX-20: FX-10 guarded the tab BUTTONS, but the rest of the strip — the
  // `+` new-shell button and the empty flex-1 background after the last tab —
  // stayed unguarded, so a press on either bubbled into the tile-drag handle and
  // could start a tile split/rearrange (the reported "reorder breaks the
  // terminal"). The strip-root pointer guard must own the WHOLE subtree now.
  // `ancestorSpy` is a bubble-phase onPointerDown ABOVE the draggable, so it
  // fires iff the press escapes the strip into the tile-drag handle — the
  // load-bearing assertion (delete the fix's stopPropagation and it turns red).
  it("AC-2: pressing the + new-shell button does NOT leak into the tile drag", () => {
    const onDragStart = vi.fn();
    const ancestorSpy = vi.fn();
    render(
      <AssembledSplitTile
        onReorderTab={vi.fn()}
        onDragStart={onDragStart}
        ancestorSpy={ancestorSpy}
      />,
    );

    fireEvent.pointerDown(screen.getByLabelText("New terminal"), { button: 0, clientX: 10 });

    expect(ancestorSpy).not.toHaveBeenCalled();
    // Belt-and-suspenders — the PointerSensor doesn't fully arm under happy-dom,
    // so onDragStart wouldn't fire regardless; ancestorSpy above is the proof.
    expect(onDragStart).not.toHaveBeenCalled();
  });

  it("AC-2: the + guard is stopPropagation not preventDefault — click still opens a shell", () => {
    // The pointerdown leak is proven dead by the sibling test above; here we lock
    // the other half: stopPropagation (not preventDefault) leaves the `+` button's
    // own onClick intact, so pressing + still spawns a shell.
    const onNewShell = vi.fn();
    render(
      <AssembledSplitTile
        onReorderTab={vi.fn()}
        onDragStart={vi.fn()}
        ancestorSpy={vi.fn()}
        onNewShell={onNewShell}
      />,
    );

    fireEvent.click(screen.getByLabelText("New terminal"));

    expect(onNewShell).toHaveBeenCalledTimes(1);
  });

  it("AC-2: pressing the empty tab-strip background does NOT leak into the tile drag", () => {
    const onDragStart = vi.fn();
    const ancestorSpy = vi.fn();
    render(
      <AssembledSplitTile
        onReorderTab={vi.fn()}
        onDragStart={onDragStart}
        ancestorSpy={ancestorSpy}
      />,
    );

    // The `+` button's parent is the TerminalTabBar root `<div>` (the strip
    // container, terminal-tab-bar.tsx:90) — i.e. the empty flex background.
    const stripRoot = screen.getByLabelText("New terminal").parentElement!;
    fireEvent.pointerDown(stripRoot, { button: 0, clientX: 200 });

    expect(ancestorSpy).not.toHaveBeenCalled();
    expect(onDragStart).not.toHaveBeenCalled();
  });
});

describe("TerminalTabBar — an exited tab", () => {
  it("marks only the exited tab, with the ended bar's words (a signal death crashed)", () => {
    const tabs: TerminalTab[] = [
      TAB,
      { id: "t2", name: "npm test", sessionId: "sess-2", exit: { code: 1, signal: null } },
      { id: "t3", name: "vite", sessionId: "sess-3", exit: { code: null, signal: "SIGTERM" } },
    ];
    render(<TerminalTabBar {...baseProps} tabs={tabs} />);
    expect(screen.getByTestId("tab-t2-ai-status")).toHaveAttribute("data-mark", "ended");
    expect(screen.getByTestId("tab-t2-ai-status")).toHaveAccessibleName("Ended · exit 1");
    expect(screen.getByTestId("tab-t3-ai-status")).toHaveAttribute("data-mark", "crashed");
    expect(screen.getByTestId("tab-t3-ai-status")).toHaveAccessibleName("Crashed · SIGTERM");
    expect(screen.queryByTestId("tab-t1-ai-status")).not.toBeInTheDocument();
  });
});

describe("TerminalTabBar — opening the tab menu from the keyboard", () => {
  const pill = () => screen.getByText("sh").closest("button") as HTMLButtonElement;

  it.each([
    ["Shift+F10", "{Shift>}{F10}{/Shift}"],
    ["the context-menu key", "{ContextMenu}"],
  ])("%s on a focused pill opens the menu anchored under the pill", async (_label, keys) => {
    const onContextMenu = vi.fn();
    const user = userEvent.setup();
    render(<TerminalTabBar {...baseProps} onContextMenu={onContextMenu} />);
    vi.spyOn(pill(), "getBoundingClientRect").mockReturnValue({
      left: 40,
      bottom: 28,
      top: 0,
      right: 120,
      width: 80,
      height: 28,
      x: 40,
      y: 0,
      toJSON: () => ({}),
    } as DOMRect);
    pill().focus();
    await user.keyboard(keys);
    expect(onContextMenu).toHaveBeenCalledTimes(1);
    expect(onContextMenu).toHaveBeenCalledWith("t1", 40, 28);
  });

  it.each([
    ["F10 alone", "{F10}"],
    ["⌘⇧F10", "{Meta>}{Shift>}{F10}{/Shift}{/Meta}"],
    ["⌃⇧F10", "{Control>}{Shift>}{F10}{/Shift}{/Control}"],
  ])("%s does not open it", async (_label, keys) => {
    const onContextMenu = vi.fn();
    const user = userEvent.setup();
    render(<TerminalTabBar {...baseProps} onContextMenu={onContextMenu} />);
    pill().focus();
    await user.keyboard(keys);
    expect(onContextMenu).not.toHaveBeenCalled();
  });

  it("a bar with no tab menu doesn't claim a popup on its pills", () => {
    render(<TerminalTabBar {...baseProps} />);
    expect(pill()).not.toHaveAttribute("aria-haspopup");
    expect(pill()).not.toHaveAttribute("aria-expanded");
  });

  it("the keys are only claimed from a pill — not from elsewhere, and not while renaming", async () => {
    const onContextMenu = vi.fn();
    const user = userEvent.setup();
    render(
      <>
        <textarea aria-label="terminal" />
        <TerminalTabBar {...baseProps} onContextMenu={onContextMenu} />
      </>,
    );
    screen.getByLabelText("terminal").focus();
    await user.keyboard("{Shift>}{F10}{/Shift}{ContextMenu}");
    expect(onContextMenu).not.toHaveBeenCalled();

    // Inside the inline rename editor the keys are typing, not a menu.
    fireEvent.doubleClick(screen.getByText("sh"));
    const input = document.querySelector("input") as HTMLInputElement;
    input.focus();
    await user.keyboard("{Shift>}{F10}{/Shift}{ContextMenu}");
    expect(onContextMenu).not.toHaveBeenCalled();
  });
});

describe("TerminalTabBar — agent tabs", () => {
  const AGENT_TAB: TerminalTab = { id: "t2", name: "Login timeout lead", sessionId: "sess-2" };
  const props = { ...baseProps, tabs: [TAB, AGENT_TAB], activeTabId: "t2" };

  beforeEach(async () => {
    const { useAgentListProjectionStore } =
      await import("@/features/viewer/lib/agent-list-projection");
    useAgentListProjectionStore.setState({
      sessions: [
        {
          handle: "BUG-142#lead",
          sessionId: "sess-2",
          kind: "dashboard-launched",
          alive: true,
          backgrounded: false,
          startedWith: null,
          startedAtMs: 1,
          launcher: null,
          lastOutputMs: null,
          displayName: "Login timeout lead",
        },
      ],
      error: null,
    });
  });

  it("shows the agent's face and its whole name, truncated at the end; a shell keeps its icon", () => {
    render(<TerminalTabBar {...props} />);
    const agent = screen.getByTestId("terminal-tab-t2");
    expect(agent.querySelector(".ag-face img")).not.toBeNull();
    expect(agent).toHaveAccessibleName(/^Login timeout lead/);
    expect(screen.getByText("Login timeout lead")).toHaveClass("truncate");
    expect(screen.getByTestId("terminal-tab-t1").querySelector(".ag-face")).toBeNull();
  });

  it("updates projected progress titles without moving tabs and caps the label with full identity on hover", async () => {
    const { useAgentListProjectionStore } =
      await import("@/features/viewer/lib/agent-list-projection");
    const setNote = (primary: string) =>
      useAgentListProjectionStore.setState((state) => ({
        sessions: state.sessions!.map((row) => ({
          ...row,
          avatar: "#00aa00",
          rowNames: {
            primary,
            secondary: "dev-worker",
            full: `${primary} · dev-worker`,
            source: "progress" as const,
          },
        })),
      }));
    setNote("Review");
    const onRenameTab = vi.fn();
    const view = render(<TerminalTabBar {...props} onRenameTab={onRenameTab} />);
    const tab = screen.getByTestId("terminal-tab-t2");
    const strip = screen.getByTestId("terminal-tab-strip");
    strip.scrollLeft = 50;
    const before = [...view.container.querySelectorAll('[role="tab"]')];
    expect(screen.getByText("Review")).toHaveStyle({
      maxWidth: "24ch",
      width: "24ch",
    });
    act(() => setNote("Review round two against the full specification"));
    const label = screen.getByText("Review round two against the full specification");
    expect(label).toHaveClass("truncate");
    expect(label).toHaveStyle({ width: "24ch", maxWidth: "24ch" });
    expect(label).toHaveAttribute(
      "title",
      "Review round two against the full specification · dev-worker",
    );
    expect(tab).toHaveAccessibleName(/Review round two.*dev-worker/);
    expect(screen.getByTestId("terminal-tab-t2")).toBe(tab);
    expect([...view.container.querySelectorAll('[role="tab"]')]).toEqual(before);
    expect(strip.scrollLeft).toBe(50);
    expect(tab.querySelector(".ag-face")).toHaveTextContent("DW");
    fireEvent.doubleClick(label);
    const editor = screen.getByRole("textbox", { name: "Tab name" });
    expect(editor).toHaveValue("dev-worker");
    fireEvent.blur(editor);
    expect(onRenameTab).not.toHaveBeenCalled();
    fireEvent.doubleClick(screen.getByText("Review round two against the full specification"));
    fireEvent.change(screen.getByRole("textbox", { name: "Tab name" }), {
      target: { value: "My worker" },
    });
    fireEvent.keyDown(screen.getByRole("textbox", { name: "Tab name" }), { key: "Enter" });
    expect(onRenameTab).toHaveBeenCalledWith("t2", "My worker");
  });

  it("keeps rename, keyboard close and the close button on an agent tab", () => {
    const onCloseTab = vi.fn();
    const onRenameTab = vi.fn();
    render(<TerminalTabBar {...props} onCloseTab={onCloseTab} onRenameTab={onRenameTab} />);
    fireEvent.keyDown(screen.getByTestId("terminal-tab-t2"), { key: "Delete" });
    expect(onCloseTab).toHaveBeenCalledWith("t2");
    fireEvent.click(screen.getByTestId("tab-t2-close"));
    expect(onCloseTab).toHaveBeenCalledTimes(2);
    fireEvent.doubleClick(screen.getByText("Login timeout lead"));
    const input = screen.getByRole("textbox", { name: "Tab name" });
    fireEvent.change(input, { target: { value: "Lead" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onRenameTab).toHaveBeenCalledWith("t2", "Lead");
  });

  it("draws every face at 20px in a fixed 22px box; the active one ringed in the team colour", () => {
    const { rerender } = render(<TerminalTabBar {...props} teamHue={180} />);
    const tab = screen.getByTestId("terminal-tab-t2");
    const box = tab.querySelector<HTMLElement>(".ag-tab-face")!;
    expect(box).toHaveStyle({ width: "22px", height: "22px" });
    expect(box).toHaveAttribute("data-active");
    expect(box.querySelector(".ag-face")).toHaveStyle({ width: "20px", height: "20px" });
    expect(tab.style.getPropertyValue("--ag-tab-ring")).toBe("oklch(78% 0.06 180)");

    rerender(<TerminalTabBar {...props} teamHue={180} activeTabId="t1" />);
    const inactive = screen
      .getByTestId("terminal-tab-t2")
      .querySelector<HTMLElement>(".ag-tab-face")!;
    expect(inactive).toHaveStyle({ width: "22px", height: "22px" });
    expect(inactive).not.toHaveAttribute("data-active");
    expect(inactive.querySelector(".ag-face")).toHaveStyle({ width: "20px", height: "20px" });
  });

  it("rings the active face in grey when the workspace has no team colour", () => {
    render(<TerminalTabBar {...props} />);
    expect(screen.getByTestId("terminal-tab-t2").style.getPropertyValue("--ag-tab-ring")).toBe(
      "#6e7681",
    );
  });

  it("wears no role piece in a tab", () => {
    render(<TerminalTabBar {...props} />);
    expect(screen.getByTestId("terminal-tab-t2").querySelector(".ag-piece")).toBeNull();
  });

  it("puts the status on the face's corner, cut out by the tab's own background", () => {
    const tabs = [TAB, { ...AGENT_TAB, exit: { code: 1, signal: null } }];
    render(<TerminalTabBar {...props} tabs={tabs} activeTabBg="#000001" chromeBg="#000002" />);
    const status = screen.getByTestId("tab-t2-ai-status");
    expect(status.closest(".ag-tab-face-status")?.parentElement).toHaveClass("ag-tab-face");
    expect(status).not.toHaveClass("absolute");
    expect(screen.getByTestId("terminal-tab-t2").style.getPropertyValue("--ag-tab-cut")).toBe(
      "#000001",
    );
    expect(screen.getByTestId("terminal-tab-t1").style.getPropertyValue("--ag-tab-cut")).toBe(
      "#000002",
    );
  });

  it("draws a crashed agent's marker on its face in the status palette's red", () => {
    const tabs = [TAB, { ...AGENT_TAB, exit: { code: null, signal: "SIGKILL" } }];
    render(<TerminalTabBar {...props} tabs={tabs} />);
    const marker = screen.getByTestId("tab-t2-ai-status");
    expect(marker).toHaveAttribute("data-mark", "crashed");
    expect(marker.closest(".ag-tab-face-status")).not.toBeNull();
    expect(marker).toHaveClass("cs-mark-crashed");
    // Its colour is a token the themes define, so the marker is never drawn in no colour.
    const css = readFileSync(resolve(__dirname, "../../../styles/status-mark.css"), "utf-8");
    expect(css).toMatch(/\.cs-mark-crashed \{\s*color: var\(--cs-status-failed\);/);
    const themes = readFileSync(resolve(__dirname, "../../../styles/theme-tokens.css"), "utf-8");
    expect(themes).toContain("--cs-status-failed:");
  });

  it("keeps a readable truncated name beside the face through narrow width and width recovery", () => {
    const client = vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(10);
    const scroll = vi.spyOn(HTMLElement.prototype, "scrollWidth", "get").mockReturnValue(120);
    try {
      const view = render(<TerminalTabBar {...props} />);
      const tab = screen.getByTestId("terminal-tab-t2");
      expect(screen.getByText("Login timeout lead")).not.toHaveClass("sr-only");
      expect(tab.parentElement).toHaveStyle({ minWidth: "98px" });
      expect(screen.getByTestId("terminal-tab-strip")).toHaveClass("overflow-x-auto");
      client.mockReturnValue(180);
      view.rerender(<TerminalTabBar {...props} />);
      expect(screen.getByText("Login timeout lead")).not.toHaveClass("sr-only");
      expect(tab).toHaveAccessibleName(/^Login timeout lead/);
    } finally {
      client.mockRestore();
      scroll.mockRestore();
    }
  });

  it("wears the face a person chose for the agent", async () => {
    const { useViewerStore } = await import("@/shared/stores/viewer-store");
    useViewerStore.setState({
      agentIdentity: { "team-x": { "BUG-142#lead": { face: "builtin-funny:yaml-wizard" } } },
    });
    render(<TerminalTabBar {...props} />);
    const face = screen.getByTestId("terminal-tab-t2").querySelector(".ag-face");
    expect(face).toHaveAttribute("data-face", "builtin-funny:yaml-wizard");
    useViewerStore.setState({ agentIdentity: {} });
  });
});

describe("TerminalTabBar — agents started by hand", () => {
  // Each test closes its tabs (tombstoning their sessions); the next test reuses
  // the same session ids, so it starts from a clean tombstone set.
  beforeEach(async () => {
    const { resetPrunedAgentSessionsForTests } = await import("@/shared/stores/agent-state-store");
    resetPrunedAgentSessionsForTests();
  });
  const HAND: TerminalTab = { id: "t3", name: "remote codesensemcp", sessionId: "pty-hand" };
  const SHELL: TerminalTab = { id: "t4", name: "zsh", sessionId: "pty-zsh" };
  const bare = (sessionId: string) => ({
    handle: sessionId,
    sessionId,
    kind: "live-pty",
    alive: true,
    backgrounded: false,
    startedWith: null,
    startedAtMs: null,
    launcher: null,
    lastOutputMs: null,
  });

  it("faces a tab whose agent the hooks detected, while a bare live shell keeps its icon", async () => {
    const { useAgentListProjectionStore } =
      await import("@/features/viewer/lib/agent-list-projection");
    const { useAgentStateStore } = await import("@/shared/stores/agent-state-store");
    useAgentListProjectionStore.setState({
      sessions: [{ ...bare("pty-hand"), kind: "hand-started" }, bare("pty-zsh")],
      error: null,
    });
    useAgentStateStore
      .getState()
      .recordSignal("pty-hand", { tier: 1, signal: { kind: "claude-hook", event: "PreToolUse" } });
    render(<TerminalTabBar {...baseProps} tabs={[HAND, SHELL]} activeTabId="t3" />);
    expect(screen.getByTestId("terminal-tab-t3").querySelector(".ag-face img")).not.toBeNull();
    expect(screen.getByTestId("terminal-tab-t4").querySelector(".ag-face")).toBeNull();
    useAgentStateStore.getState().remove(["pty-hand"]);
  });

  it("keeps the terminal icon on a registered shell until an agent runs in it, and after it exits", async () => {
    const { useAgentListProjectionStore } =
      await import("@/features/viewer/lib/agent-list-projection");
    const opened = { ...bare("pty-zsh"), kind: "registered", agentRevision: null };
    const project = (row: typeof opened & { detected?: { cli: string; dot: null } }) =>
      act(() => useAgentListProjectionStore.setState({ sessions: [row], error: null }));
    project(opened);
    render(<TerminalTabBar {...baseProps} tabs={[SHELL]} activeTabId="t4" />);
    const tab = () => screen.getByTestId("terminal-tab-t4");
    expect(tab().querySelector(".ag-face")).toBeNull();
    expect(tab()).toHaveAccessibleName(/zsh/);
    project({ ...opened, detected: { cli: "claude", dot: null } });
    expect(tab().querySelector(".ag-face img")).not.toBeNull();
    project(opened);
    expect(tab().querySelector(".ag-face")).toBeNull();
  });

  it("faces a reattached Claude and a Codex found by their running processes, with no hook", async () => {
    const { useAgentListProjectionStore } =
      await import("@/features/viewer/lib/agent-list-projection");
    const { useAgentStateStore } = await import("@/shared/stores/agent-state-store");
    const CODEX: TerminalTab = { id: "t5", name: "codex", sessionId: "pty-codex" };
    useAgentListProjectionStore.setState({
      sessions: [
        { ...bare("pty-hand"), kind: "hand-started" },
        bare("pty-zsh"),
        { ...bare("pty-codex"), kind: "hand-started" },
      ],
      error: null,
    });
    useAgentStateStore.getState().setProcessAgent("pty-hand", "claude");
    useAgentStateStore.getState().setProcessAgent("pty-codex", "codex");
    const { rerender } = render(
      <TerminalTabBar {...baseProps} tabs={[HAND, SHELL, CODEX]} activeTabId="t3" />,
    );
    expect(screen.getByTestId("terminal-tab-t3").querySelector(".ag-face img")).not.toBeNull();
    expect(screen.getByTestId("terminal-tab-t5").querySelector(".ag-face img")).not.toBeNull();
    expect(screen.getByTestId("terminal-tab-t4").querySelector(".ag-face")).toBeNull();
    // The agent exits: its tab is a plain shell again.
    act(() => {
      useAgentStateStore.getState().setProcessAgent("pty-codex", null);
      useAgentListProjectionStore.setState({
        sessions: [
          { ...bare("pty-hand"), kind: "hand-started" },
          bare("pty-zsh"),
          bare("pty-codex"),
        ],
      });
    });
    rerender(<TerminalTabBar {...baseProps} tabs={[HAND, SHELL, CODEX]} activeTabId="t3" />);
    expect(screen.getByTestId("terminal-tab-t5").querySelector(".ag-face")).toBeNull();
    useAgentStateStore.getState().remove(["pty-hand", "pty-codex"]);
  });

  it("numbers two agent tabs wearing the same face, and not a tab whose face is its own", async () => {
    const { useAgentListProjectionStore } =
      await import("@/features/viewer/lib/agent-list-projection");
    const A: TerminalTab = { id: "ta", name: "a", sessionId: "pty-a" };
    const B: TerminalTab = { id: "tb", name: "b", sessionId: "pty-b" };
    const C: TerminalTab = { id: "tc", name: "c", sessionId: "pty-c" };
    const agent = (sid: string, startedAtMs: number, avatar: string) => ({
      ...bare(sid),
      kind: "dashboard-launched",
      startedAtMs,
      avatar,
    });
    useAgentListProjectionStore.setState({
      sessions: [
        agent("pty-b", 2, "builtin-funny:tie-cat"),
        agent("pty-a", 1, "builtin-funny:tie-cat"),
        agent("pty-c", 3, "builtin-funny:fine-dog"),
      ],
      error: null,
    });
    render(<TerminalTabBar {...baseProps} tabs={[A, B, C]} activeTabId="ta" />);
    const tagOf = (id: string) =>
      screen.getByTestId(`terminal-tab-${id}`).querySelector("[data-testid=agent-face-tag]")
        ?.textContent ?? null;
    expect(tagOf("ta")).toBe("1");
    expect(tagOf("tb")).toBe("2");
    expect(tagOf("tc")).toBeNull();
  });

  it("wears the face a person picked for an agent named controller, like any agent", async () => {
    const { useAgentListProjectionStore } =
      await import("@/features/viewer/lib/agent-list-projection");
    const { useViewerStore } = await import("@/shared/stores/viewer-store");
    const CTL_HANDLE = "/orgs/acme/ctx/team-x/content#controller";
    const CTL: TerminalTab = { id: "t6", name: "Controller", sessionId: "pty-ctl" };
    useAgentListProjectionStore.setState({
      sessions: [
        {
          ...bare("pty-ctl"),
          handle: CTL_HANDLE,
          kind: "dashboard-launched",
          agentRevision: "acme/team-x/controller@1",
        },
      ],
      error: null,
    });
    act(() =>
      useViewerStore.setState({
        agentIdentity: { "team-x": { [CTL_HANDLE]: { face: "builtin-funny:tie-cat" } } },
      }),
    );
    render(<TerminalTabBar {...baseProps} tabs={[CTL]} activeTabId="t6" />);
    expect(screen.getByTestId("terminal-tab-t6").querySelector(".ag-face")).toHaveAttribute(
      "data-face",
      "builtin-funny:tie-cat",
    );
    useViewerStore.setState({ agentIdentity: {} });
  });
});

it.each(["Fix labels", "$bmad-code-review Fix labels\nFocus on: navigation"])(
  "agent tabs follow live titles (%s), preserve a personal rename and expose the saved identity",
  async (live) => {
    const { useAllAgentRowsStore } = await import("@/features/agents/lib/agent-names");
    const { useAgentListProjectionStore } =
      await import("@/features/viewer/lib/agent-list-projection");
    const { useViewerStore } = await import("@/shared/stores/viewer-store");
    useAllAgentRowsStore.setState({
      rows: [
        {
          key: "acme/team/dev",
          name: "Developer",
          runtime: "claude",
        } as import("@/tauri/commands").AgentRow,
      ],
    });
    const sessions = ["one", "two"].map((id) => ({
      handle: id,
      sessionId: id,
      kind: "dashboard-launched",
      alive: true,
      backgrounded: false,
      startedWith: null,
      startedAtMs: 1,
      launcher: null,
      lastOutputMs: null,
      agentRevision: "acme/team/dev@1",
      displayName: "Developer",
      rowNames: {
        primary: id === "one" ? "Fix labels" : "Fix menus",
        secondary: "Developer",
        full: `${id === "one" ? live.replace(/\s+/g, " ") : "Fix menus"} · Developer`,
      },
    }));
    useAgentListProjectionStore.setState({ sessions });
    const tabs = [
      { id: "one", sessionId: "one", name: live },
      { id: "two", sessionId: "two", name: "Fix menus" },
    ];
    try {
      const view = render(<TerminalTabBar {...baseProps} tabs={tabs} />);
      const full = `${live.replace(/\s+/g, " ")} · Developer`;
      expect(screen.getByTestId("terminal-tab-one")).toHaveAccessibleName(
        new RegExp(full.replace(/\$/g, "\\$")),
      );
      expect(screen.getByTestId("terminal-tab-two")).toHaveAccessibleName(/Fix menus · Developer/);
      const label = screen.getByText("Fix labels");
      expect(label).toHaveAttribute("title", full);
      expect(label).toHaveClass("truncate");
      expect(screen.getByTestId("terminal-tab-one").querySelector(".ag-tab-face")).toHaveAttribute(
        "title",
        full,
      );
      act(() => {
        useViewerStore.setState({ agentIdentity: { "team-x": { one: { name: "My worker" } } } });
        useAgentListProjectionStore.setState({
          sessions: sessions.map((row) =>
            row.handle === "one"
              ? {
                  ...row,
                  rowNames: {
                    primary: "My worker",
                    secondary: "Developer",
                    full: "My worker · Developer",
                  },
                }
              : row,
          ),
        });
      });
      view.rerender(
        <TerminalTabBar
          {...baseProps}
          tabs={[{ ...tabs[0]!, name: "New live title" }, tabs[1]!]}
        />,
      );
      expect(screen.getByTestId("terminal-tab-one")).toHaveAccessibleName(/My worker · Developer/);
      fireEvent.doubleClick(screen.getByText("My worker"));
      expect(screen.getByRole("textbox", { name: "Tab name" })).toHaveValue("My worker");
      expect(screen.getByTestId("terminal-tab-two")).toHaveAccessibleName(/Fix menus · Developer/);
    } finally {
      useAllAgentRowsStore.setState({ rows: null });
      useViewerStore.setState({ agentIdentity: {} });
    }
  },
);

describe("TerminalTabBar — a session typed by hand or on an unsupported release", () => {
  const TAB_ROW = {
    alive: true,
    backgrounded: false,
    startedWith: null,
    startedAtMs: 1,
    launcher: null,
    lastOutputMs: null,
  };
  const SHELL3: TerminalTab = { id: "t9", name: "shell-3", sessionId: "pty-typed" };
  beforeEach(async () => {
    const { resetPrunedAgentSessionsForTests } = await import("@/shared/stores/agent-state-store");
    resetPrunedAgentSessionsForTests();
  });

  describe.each(REGISTERED_AGENT_CLIS.map((cli) => cli.runtime))("%s", (runtime) => {
    const typed = async (inboxWaiter: boolean) => {
      const { useAgentListProjectionStore } =
        await import("@/features/viewer/lib/agent-list-projection");
      const { useAgentStateStore } = await import("@/shared/stores/agent-state-store");
      useAgentListProjectionStore.setState({
        sessions: [
          {
            ...TAB_ROW,
            handle: "pty-typed",
            sessionId: "pty-typed",
            kind: "hand-started",
            inboxWaiter,
          },
        ],
        error: null,
      });
      useAgentStateStore.getState().setProcessAgent("pty-typed", runtime);
    };

    it("a tab typed by hand wears the mail-off glyph, named by its tooltip's words", async () => {
      await typed(false);
      render(<TerminalTabBar {...baseProps} tabs={[SHELL3]} activeTabId="t9" />);
      const tab = screen.getByTestId("terminal-tab-t9");
      const glyph = screen.getByRole("img", {
        name: "Typed by hand · Mail waits; CodeSense can't wake it",
      });
      expect(tab.contains(glyph)).toBe(true);
      expect(tab).toHaveAccessibleName(/Typed by hand · Mail waits; CodeSense can't wake it/);
      await userEvent.hover(glyph);
      expect(await screen.findByRole("tooltip")).toHaveTextContent(
        "Typed by hand · Mail waits; CodeSense can't wake it",
      );
    });

    it("a tab typed by hand whose waiter is armed wears no glyph", async () => {
      await typed(true);
      render(<TerminalTabBar {...baseProps} tabs={[SHELL3]} activeTabId="t9" />);
      expect(screen.queryByRole("img", { name: /Typed by hand/ })).toBeNull();
    });

    it("a session on an unsupported release says so after its status", async () => {
      const { useAgentListProjectionStore } =
        await import("@/features/viewer/lib/agent-list-projection");
      const { useAgentStateStore } = await import("@/shared/stores/agent-state-store");
      useAgentListProjectionStore.setState({
        sessions: [
          {
            ...TAB_ROW,
            handle: "reviewer-2",
            sessionId: "pty-old",
            kind: "dashboard-launched",
            displayName: "reviewer-2",
            releaseQualified: false,
            releaseNote: "This CLI version (0.1.0) isn't supported yet.",
          },
        ],
        error: null,
      });
      useAgentStateStore.getState().setProcessAgent("pty-old", runtime);
      useAgentStateStore
        .getState()
        .recordSignal("pty-old", { tier: 1, signal: { kind: "claude-hook", event: "PreToolUse" } });
      const OLD: TerminalTab = { id: "t8", name: "reviewer-2", sessionId: "pty-old" };
      render(<TerminalTabBar {...baseProps} tabs={[OLD]} activeTabId="t8" />);
      expect(screen.getByTestId("tab-t8-ai-status")).toHaveAccessibleName(
        / · Release not supported$/,
      );
      expect(screen.getByTestId("terminal-tab-t8")).toHaveAccessibleName(/Release not supported/);
      useAgentStateStore.getState().remove(["pty-old"]);
    });
  });
});
