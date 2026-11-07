// SPDX-License-Identifier: MIT
/**
 * Vertical resize of stacked right-pane sections, driven through the rendered
 * splitter into the real viewer store (the seam the founder hit): drag, the
 * iframe-proof press shield, collapsed sections in between, double-click
 * equalize, keyboard nudges, and uniform header heights.
 */
import { act, fireEvent, render, screen } from "@testing-library/react";
import { Workflow } from "lucide-react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useViewerStore } from "@/shared/stores/viewer-store";

import { usePanelRegistry } from "./panel-registry";
import { RightPaneHostProvider, type RightPaneHostValue } from "./right-pane-host-context";
import type { RightPaneLayout, RightPaneSection } from "./right-pane-layout";
import { RightPaneStack } from "./right-pane-stack";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  saveScratchNote: vi.fn().mockResolvedValue(undefined),
  loadScratchNote: vi.fn().mockResolvedValue(""),
  checkPathExists: vi.fn().mockResolvedValue(true),
  detectTools: vi
    .fn()
    .mockResolvedValue({ terminals: [], editors: [], default_terminal: "", default_editor: "" }),
}));

const HOST: RightPaneHostValue = { repoPath: "/repo", onClose: vi.fn() };
/** Every section measures 500px tall → a splitter's pair spans 1000px. */
const SECTION_PX = 500;

function stub(id: string, title: string, body?: () => React.ReactNode) {
  return {
    id,
    title,
    icon: Workflow,
    render: body ?? (() => <div data-testid={`body-${id}`}>{title} body</div>),
  };
}

function section(
  id: string,
  panelIds: string[],
  size: number,
  collapsed = false,
): RightPaneSection {
  return { id, group: { panelIds, activePanelId: panelIds[0] }, collapsed, size };
}

function renderStack(layout: RightPaneLayout) {
  useViewerStore.setState({ rightPaneLayout: layout });
  return render(
    <RightPaneHostProvider value={HOST}>
      <RightPaneStack />
    </RightPaneHostProvider>,
  );
}

const sizes = () => useViewerStore.getState().rightPaneLayout.sections.map((s) => s.size);

beforeEach(() => {
  Element.prototype.setPointerCapture = vi.fn();
  Element.prototype.releasePointerCapture = vi.fn();
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    height: SECTION_PX,
    width: 400,
    top: 0,
    left: 0,
    right: 400,
    bottom: SECTION_PX,
    x: 0,
    y: 0,
    toJSON: () => "",
  });
  usePanelRegistry.setState({
    panels: {
      p1: stub("p1", "P1"),
      p2: stub("p2", "P2"),
      p3: stub("p3", "P3"),
      // A custom pane: its HTML lives in an iframe that would swallow pointer events.
      pane: stub("pane", "Pane", () => <iframe data-testid="pane-iframe" title="pane" />),
    },
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  document.documentElement.classList.remove("dragging");
});

/** Run pending rAF visual updates so a drag's moves land. */
async function flushFrames() {
  await act(async () => {
    await new Promise((r) => requestAnimationFrame(() => r(null)));
  });
}

describe("right-pane section splitter", () => {
  it("dragging resizes the two sections and persists the new sizes to the layout", async () => {
    renderStack({ sections: [section("s1", ["p1"], 0.5), section("s2", ["p2"], 0.5)] });
    const splitter = screen.getByRole("separator", { name: "Resize sections" });

    fireEvent.pointerDown(splitter, { pointerId: 1, button: 0, clientY: 500 });
    // Moves and the release arrive at the document, as they do once the pointer
    // leaves the thin splitter (or travels over the press shield).
    fireEvent.pointerMove(document, { pointerId: 1, clientY: 450 });
    fireEvent.pointerMove(document, { pointerId: 1, clientY: 400 });
    await flushFrames();
    expect(sizes()).toEqual([0.5, 0.5]); // visual-follow only, no commit mid-drag
    fireEvent.pointerUp(document, { pointerId: 1, clientY: 400 });

    // Moved up 100px of a 1000px pair: above 0.5 → 0.4, below 0.5 → 0.6.
    const [above, below] = sizes();
    expect(above).toBeCloseTo(0.4, 5);
    expect(below).toBeCloseTo(0.6, 5);
  });

  it("holds a full-window shield over an iframe pane for the whole press, then drops it", async () => {
    renderStack({ sections: [section("s1", ["pane"], 0.5), section("s2", ["p2"], 0.5)] });
    expect(screen.getByTestId("pane-iframe")).toBeInTheDocument();
    const splitter = screen.getByRole("separator", { name: "Resize sections" });
    expect(screen.queryByTestId("rps-resize-shield")).toBeNull();

    fireEvent.pointerDown(splitter, { pointerId: 1, button: 0, clientY: 500 });
    const shield = screen.getByTestId("rps-resize-shield");
    // Portaled to <body> so it covers every iframe, not just this pane.
    expect(shield.parentElement).toBe(document.body);
    expect(splitter).toHaveAttribute("data-active");

    // The drag is fed entirely by events that hit the shield (never the iframe).
    fireEvent.pointerMove(shield, { pointerId: 1, clientY: 560 });
    fireEvent.pointerMove(shield, { pointerId: 1, clientY: 650 });
    await flushFrames();
    fireEvent.pointerUp(shield, { pointerId: 1, clientY: 650 });

    expect(screen.queryByTestId("rps-resize-shield")).toBeNull();
    expect(splitter).not.toHaveAttribute("data-active");
    const [above, below] = sizes();
    expect(above).toBeCloseTo(0.65, 5);
    expect(below).toBeCloseTo(0.35, 5);
  });

  it("clamps so neither section shrinks below its minimum height", async () => {
    renderStack({ sections: [section("s1", ["p1"], 0.5), section("s2", ["p2"], 0.5)] });
    const splitter = screen.getByRole("separator", { name: "Resize sections" });
    fireEvent.pointerDown(splitter, { pointerId: 1, button: 0, clientY: 500 });
    fireEvent.pointerMove(document, { pointerId: 1, clientY: -400 });
    await flushFrames();
    fireEvent.pointerUp(document, { pointerId: 1, clientY: -400 });
    // 80px floor of a 1000px pair.
    expect(sizes()[0]).toBeCloseTo(0.08, 5);
    expect(sizes()[1]).toBeCloseTo(0.92, 5);
  });

  it("a collapsed section between two expanded ones does not remove the splitter", async () => {
    renderStack({
      sections: [
        section("s1", ["p1"], 0.6),
        section("s2", ["p2"], 0.3, true),
        section("s3", ["p3"], 0.4),
      ],
    });
    // One splitter on each side of the collapsed header; both trade s1 ↔ s3.
    const splitters = screen.getAllByRole("separator", { name: "Resize sections" });
    expect(splitters).toHaveLength(2);

    fireEvent.pointerDown(splitters[1], { pointerId: 1, button: 0, clientY: 500 });
    fireEvent.pointerMove(document, { pointerId: 1, clientY: 400 });
    await flushFrames();
    fireEvent.pointerUp(document, { pointerId: 1, clientY: 400 });

    const [s1, s2, s3] = sizes();
    expect(s2).toBe(0.3); // the collapsed section's stored size is untouched
    // Pair s1+s3 sums to 1.0 over 1000px; up 100px moves 0.1 from s1 to s3.
    expect(s1).toBeCloseTo(0.5, 5);
    expect(s3).toBeCloseTo(0.5, 5);
  });

  it("double-click splits the pair evenly", () => {
    renderStack({ sections: [section("s1", ["p1"], 0.8), section("s2", ["p2"], 0.2)] });
    const splitter = screen.getByRole("separator", { name: "Resize sections" });
    for (let i = 0; i < 2; i++) {
      fireEvent.pointerDown(splitter, { pointerId: 1, button: 0, clientY: 500 });
      fireEvent.pointerUp(document, { pointerId: 1, clientY: 500 });
    }
    expect(sizes()[0]).toBeCloseTo(0.5, 5);
    expect(sizes()[1]).toBeCloseTo(0.5, 5);
  });

  it("a single tap does not change sizes", () => {
    renderStack({ sections: [section("s1", ["p1"], 0.8), section("s2", ["p2"], 0.2)] });
    const splitter = screen.getByRole("separator", { name: "Resize sections" });
    fireEvent.pointerDown(splitter, { pointerId: 1, button: 0, clientY: 500 });
    fireEvent.pointerUp(document, { pointerId: 1, clientY: 500 });
    expect(sizes()).toEqual([0.8, 0.2]);
  });

  it("↑/↓ nudge the splitter when focused (Shift = coarse)", () => {
    renderStack({ sections: [section("s1", ["p1"], 0.5), section("s2", ["p2"], 0.5)] });
    const splitter = screen.getByRole("separator", { name: "Resize sections" });
    expect(splitter).toHaveAttribute("tabindex", "0");
    expect(splitter).toHaveAttribute("aria-valuenow", "50");

    fireEvent.keyDown(splitter, { key: "ArrowUp" }); // 16px of 1000 up
    expect(sizes()[0]).toBeCloseTo(0.484, 5);
    fireEvent.keyDown(splitter, { key: "ArrowDown", shiftKey: true }); // 64px down
    expect(sizes()[0]).toBeCloseTo(0.548, 5);
    expect(sizes()[1]).toBeCloseTo(0.452, 5);
  });
});

describe("right-pane section headers", () => {
  it("a grouped (tabbed) section's header is the same fixed height as a single-panel one", () => {
    renderStack({
      sections: [section("s1", ["p1", "p2"], 0.5), section("s2", ["p3"], 0.5)],
    });
    const rows = Array.from(document.querySelectorAll<HTMLElement>("[data-panel-header-row]"));
    expect(rows).toHaveLength(2);
    // One fixed height for every header, and no vertical padding that a taller
    // child (tab buttons, the maximize control) could add to.
    for (const row of rows) {
      expect(row.classList.contains("h-8")).toBe(true);
      expect([...row.classList].some((c) => /^py-|^pt-|^pb-/.test(c))).toBe(false);
    }
    expect(rows[0].className).toBe(rows[1].className);
    // The grouped row really is the tab-strip variant.
    expect(rows[0].querySelectorAll("button[aria-pressed]")).toHaveLength(2);
  });
});
