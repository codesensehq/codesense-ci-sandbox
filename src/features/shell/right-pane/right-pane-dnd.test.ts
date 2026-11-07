// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  applyRightPaneDrag,
  gapDropId,
  parseGapIndex,
  parseSectionId,
  parseTabId,
  resolveRightPaneDrag,
  sectionDragId,
  tabDragId,
} from "./right-pane-dnd";
import {
  SCRATCH_PANEL_ID,
  INBOX_PANEL_ID,
  defaultRightPaneLayout,
  type RightPaneLayout,
} from "./right-pane-layout";

const MAIN = "rps-main";
const SCRATCH = "rps-scratch";
/** Neutral panel ids — the drag model never consults a registry. */
const PANEL_A = "panel-a";
const PANEL_B = "panel-b";

/** The default layout with a multi-tab main group:
 *  [ main(A, B, Inbox*), scratch ]. */
function tabbedLayout(): RightPaneLayout {
  const layout = defaultRightPaneLayout();
  layout.sections[0].group = {
    panelIds: [PANEL_A, PANEL_B, INBOX_PANEL_ID],
    activePanelId: INBOX_PANEL_ID,
  };
  return layout;
}

describe("id grammar", () => {
  it("round-trips section / tab / gap ids", () => {
    expect(parseSectionId(sectionDragId("rps-main"))).toBe("rps-main");
    expect(parseTabId(tabDragId("panel-b"))).toBe("panel-b");
    expect(parseGapIndex(gapDropId(2))).toBe(2);
  });

  it("rejects foreign / malformed ids (no cross-talk with terminal-drag ids)", () => {
    expect(parseSectionId("tab:panel-b")).toBeNull();
    expect(parseSectionId("tile:ws:left")).toBeNull();
    expect(parseTabId("section:rps-main")).toBeNull();
    expect(parseGapIndex("gap:-1")).toBeNull();
    expect(parseGapIndex("gap:1.5")).toBeNull();
    expect(parseGapIndex("gap:")).toBeNull();
    expect(parseSectionId(null)).toBeNull();
    expect(parseSectionId(42)).toBeNull();
  });
});

describe("resolveRightPaneDrag — reorder (section dragged)", () => {
  it("section over another section → reorder to that section's index", () => {
    const layout = tabbedLayout();
    const op = resolveRightPaneDrag(layout, sectionDragId(MAIN), sectionDragId(SCRATCH));
    expect(op).toEqual({ kind: "reorder", sectionId: MAIN, toIndex: 1 });
  });

  it("section over a gap below itself → reorder (translated index)", () => {
    const layout = tabbedLayout();
    // main is at index 0; gap 2 = below the last section → toIndex 1 after removal
    const op = resolveRightPaneDrag(layout, sectionDragId(MAIN), gapDropId(2));
    expect(op).toEqual({ kind: "reorder", sectionId: MAIN, toIndex: 1 });
  });

  it("section dropped on itself → no-op", () => {
    const layout = tabbedLayout();
    expect(resolveRightPaneDrag(layout, sectionDragId(MAIN), sectionDragId(MAIN))).toBeNull();
  });

  it("section over the gap at its own position → no-op", () => {
    const layout = tabbedLayout();
    // main at index 0; gap 0 is just above it → translated toIndex 0 === from
    expect(resolveRightPaneDrag(layout, sectionDragId(MAIN), gapDropId(0))).toBeNull();
  });

  it("unknown dragged section → no-op", () => {
    const layout = tabbedLayout();
    expect(resolveRightPaneDrag(layout, sectionDragId("ghost"), sectionDragId(SCRATCH))).toBeNull();
  });
});

describe("resolveRightPaneDrag — group (tab dragged onto a section)", () => {
  it("tab over a different section → group into it", () => {
    const layout = tabbedLayout();
    // drag scratch's tab onto the main section
    const op = resolveRightPaneDrag(layout, tabDragId(SCRATCH_PANEL_ID), sectionDragId(MAIN));
    expect(op).toEqual({ kind: "group", panelId: SCRATCH_PANEL_ID, targetSectionId: MAIN });
  });

  it("tab dropped on its own home section → no-op", () => {
    const layout = tabbedLayout();
    // B lives in main; dropping it back onto main does nothing
    expect(resolveRightPaneDrag(layout, tabDragId(PANEL_B), sectionDragId(MAIN))).toBeNull();
  });

  it("unknown dragged panel → no-op", () => {
    const layout = tabbedLayout();
    expect(resolveRightPaneDrag(layout, tabDragId("ghost"), sectionDragId(MAIN))).toBeNull();
  });
});

describe("resolveRightPaneDrag — reorder tab within a group (tab onto a sibling tab)", () => {
  it("tab over a sibling tab in the same group → reorderTab to the sibling's index", () => {
    const layout = tabbedLayout(); // main = [A(0), B(1), Inbox(2)]
    const op = resolveRightPaneDrag(layout, tabDragId(PANEL_B), tabDragId(PANEL_A));
    expect(op).toEqual({ kind: "reorderTab", panelId: PANEL_B, toIndex: 0 });
  });

  it("tab dropped on itself → no-op", () => {
    const layout = tabbedLayout();
    expect(resolveRightPaneDrag(layout, tabDragId(PANEL_B), tabDragId(PANEL_B))).toBeNull();
  });

  it("tab over a tab in ANOTHER section → group into that section", () => {
    const layout = tabbedLayout();
    // drag scratch (its own section) onto the B tab (in main) → group into main
    const op = resolveRightPaneDrag(layout, tabDragId(SCRATCH_PANEL_ID), tabDragId(PANEL_B));
    expect(op).toEqual({ kind: "group", panelId: SCRATCH_PANEL_ID, targetSectionId: MAIN });
  });
});

describe("resolveRightPaneDrag — ungroup (tab dragged into a gap)", () => {
  it("tab from a multi-panel group over a gap → ungroup", () => {
    const layout = tabbedLayout(); // main has [A, B, Inbox]
    const op = resolveRightPaneDrag(layout, tabDragId(PANEL_B), gapDropId(2));
    expect(op).toEqual({ kind: "ungroup", panelId: PANEL_B });
  });

  it("a solo panel over a gap → no-op (nothing to split, AC-3)", () => {
    const layout = tabbedLayout(); // scratch is solo in its section
    expect(resolveRightPaneDrag(layout, tabDragId(SCRATCH_PANEL_ID), gapDropId(1))).toBeNull();
  });
});

describe("resolveRightPaneDrag — universal no-ops", () => {
  it("no over target → no-op", () => {
    const layout = tabbedLayout();
    expect(resolveRightPaneDrag(layout, sectionDragId(MAIN), null)).toBeNull();
    expect(resolveRightPaneDrag(layout, tabDragId(PANEL_B), undefined)).toBeNull();
  });

  it("unrecognized active id → no-op", () => {
    const layout = tabbedLayout();
    expect(resolveRightPaneDrag(layout, "tile:ws:left", sectionDragId(MAIN))).toBeNull();
  });
});

describe("applyRightPaneDrag", () => {
  const ids = (l: RightPaneLayout) => l.sections.map((s) => s.id);

  it("reorder moves the section", () => {
    const layout = tabbedLayout();
    const next = applyRightPaneDrag(layout, { kind: "reorder", sectionId: MAIN, toIndex: 1 });
    expect(ids(next)).toEqual([SCRATCH, MAIN]);
  });

  it("reorderTab reorders the tab within its group", () => {
    const layout = tabbedLayout(); // main = [A, B, Inbox]
    const next = applyRightPaneDrag(layout, {
      kind: "reorderTab",
      panelId: PANEL_B,
      toIndex: 0,
    });
    expect(next.sections[0].group.panelIds).toEqual([PANEL_B, PANEL_A, INBOX_PANEL_ID]);
  });

  it("group merges the panel as a tab and makes it active", () => {
    const layout = tabbedLayout();
    const next = applyRightPaneDrag(layout, {
      kind: "group",
      panelId: SCRATCH_PANEL_ID,
      targetSectionId: MAIN,
    });
    const main = next.sections.find((s) => s.id === MAIN);
    expect(main?.group.panelIds).toContain(SCRATCH_PANEL_ID);
    expect(main?.group.activePanelId).toBe(SCRATCH_PANEL_ID);
    // scratch's old section emptied → dropped
    expect(next.sections.some((s) => s.id === SCRATCH)).toBe(false);
  });

  it("group expands a collapsed target so the merged-in (active) tab is visible", () => {
    const layout: RightPaneLayout = {
      sections: [
        // multi-panel so removing pa does NOT drop the section (keeps a expanded,
        // so the floor never rescues b — isolating the expand-on-group behavior)
        {
          id: "a",
          group: { panelIds: ["pa", "px"], activePanelId: "pa" },
          collapsed: false,
          size: 0.5,
        },
        { id: "b", group: { panelIds: ["pb"], activePanelId: "pb" }, collapsed: true, size: 0.5 },
      ],
    };
    const next = applyRightPaneDrag(layout, { kind: "group", panelId: "pa", targetSectionId: "b" });
    const b = next.sections.find((s) => s.id === "b");
    expect(b?.collapsed).toBe(false); // expanded so the merged tab isn't hidden
    expect(b?.group.activePanelId).toBe("pa");
    expect(b?.group.panelIds).toContain("pa");
  });

  it("ungroup splits the tab into its own section below the source", () => {
    const layout = tabbedLayout();
    const next = applyRightPaneDrag(layout, { kind: "ungroup", panelId: PANEL_B });
    const solo = next.sections.find(
      (s) => s.group.panelIds.length === 1 && s.group.panelIds[0] === PANEL_B,
    );
    expect(solo).toBeDefined();
    // A (+ the Inbox tab) left in the original main section; B split out
    const main = next.sections.find((s) => s.id === MAIN);
    expect(main?.group.panelIds).toEqual([PANEL_A, INBOX_PANEL_ID]);
  });

  it("resolve → apply end-to-end keeps the panel present (no drop)", () => {
    const layout = tabbedLayout();
    const op = resolveRightPaneDrag(layout, tabDragId(PANEL_B), gapDropId(2));
    const next = op ? applyRightPaneDrag(layout, op) : layout;
    const all = next.sections.flatMap((s) => s.group.panelIds);
    expect(all).toContain(PANEL_B);
    expect(all).toContain(PANEL_A);
    expect(all).toContain(SCRATCH_PANEL_ID);
  });
});
