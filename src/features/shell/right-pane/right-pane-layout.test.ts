// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  resizePairAt,
  BUILTIN_PANEL_IDS,
  MAIN_SECTION_ID,
  SCRATCH_PANEL_ID,
  SCRATCH_SECTION_ID,
  addPanelAsTopSection,
  coerceRightPaneLayout,
  collapseSection,
  defaultRightPaneLayout,
  expandSection,
  expandedSectionCount,
  findPanelLocation,
  isPanelVisible,
  mergePanelIntoSection,
  migrateLegacyRightPaneState,
  reconcileLayoutToBuiltins,
  movePanelWithinSection,
  moveSection,
  removePanel,
  revealPanel,
  selectRenderableLayout,
  setActivePanel,
  setSectionSize,
  splitPanelToOwnSection,
  INBOX_PANEL_ID,
  type RightPaneLayout,
} from "./right-pane-layout";

/** The retired JSON widget panel kind a stale persisted layout may still name. */
const RETIRED_WIDGET_KIND = "widget";

/** Whether `id` is one of the fixed built-in panels. */
const isBuiltinPanelId = (id: string): boolean => BUILTIN_PANEL_IDS.has(id);

/** Deterministic section-id factory for assertions. */
function seqIds(prefix = "s"): () => string {
  let n = 0;
  return () => `${prefix}${++n}`;
}

/** The retired PR review panel's persisted id — no longer a built-in. */
const RETIRED_PRS_PANEL_ID = "git-sync-prs";

/** Neutral panel ids for the pure layout ops (they never consult a registry). */
const PANEL_A = "panel-a";
const PANEL_B = "panel-b";

/** The default layout with a three-tab main group `[A, B, Inbox]` (Inbox active) —
 *  the multi-tab shape the tab/group ops need. */
function multiTabLayout(): RightPaneLayout {
  const layout = defaultRightPaneLayout();
  layout.sections[0].group = {
    panelIds: [PANEL_A, PANEL_B, INBOX_PANEL_ID],
    activePanelId: INBOX_PANEL_ID,
  };
  return layout;
}

describe("defaultRightPaneLayout", () => {
  it("is the Inbox over Notes; landing = Inbox", () => {
    const layout = defaultRightPaneLayout();
    expect(layout.sections).toHaveLength(2);
    const [live, notes] = layout.sections;

    // The main group is first + expanded, so legacy migration + id-addressed ops stay
    // index-stable: the Inbox alone, the daily home.
    expect(live.id).toBe(MAIN_SECTION_ID);
    expect(live.label).toBe("Live");
    expect(live.group.panelIds).toEqual([INBOX_PANEL_ID]);
    expect(live.group.activePanelId).toBe(INBOX_PANEL_ID);
    expect(live.collapsed).toBe(false);

    // Notes below (unlabelled), still `sections[1]`.
    expect(notes.id).toBe(SCRATCH_SECTION_ID);
    expect(notes.label).toBeUndefined();
    expect(notes.group.panelIds).toEqual([SCRATCH_PANEL_ID]);
    expect(notes.collapsed).toBe(false);

    for (const s of layout.sections) expect(s.size).toBeGreaterThan(0);
  });

  it("SW-AV-10 / SW-WA-E14: the seeded default round-trips through coerce (back-compat)", () => {
    // Every panel is a registered viewer built-in, so a fresh default is a valid,
    // reconcile-stable layout — a persisted copy still resolves. The optional group
    // labels survive the clone/coerce round-trip.
    const layout = defaultRightPaneLayout();
    expect(coerceRightPaneLayout(layout)).toEqual(layout);
    expect(reconcileLayoutToBuiltins(layout)).toEqual(layout);
  });
});

describe("isPanelVisible", () => {
  it("true only when the panel is the active panel of an EXPANDED section", () => {
    const ids = seqIds();
    // Promote A to its own top section (collapses the rest).
    const promoted = addPanelAsTopSection(multiTabLayout(), PANEL_A, ids);
    expect(isPanelVisible(promoted, PANEL_A)).toBe(true);
    // B shares no visible slot now (its section was collapsed).
    expect(isPanelVisible(promoted, PANEL_B)).toBe(false);
  });

  it("false for a background tab in a multi-panel group (present but not active)", () => {
    // The main group lands on the Inbox; A/B ride as background tabs.
    const layout = multiTabLayout();
    expect(isPanelVisible(layout, INBOX_PANEL_ID)).toBe(true);
    expect(isPanelVisible(layout, PANEL_A)).toBe(false);
    expect(isPanelVisible(layout, PANEL_B)).toBe(false);
  });

  it("false for a collapsed section and for an absent panel", () => {
    const layout = defaultRightPaneLayout();
    const collapsed = collapseSection(layout, SCRATCH_SECTION_ID); // collapse Notes
    expect(isPanelVisible(collapsed, SCRATCH_PANEL_ID)).toBe(false);
    expect(isPanelVisible(layout, "does-not-exist")).toBe(false);
  });
});

describe("reconcileLayoutToBuiltins", () => {
  it("drops a persisted dynamic-panel section, keeps the built-ins, re-asserts the floor", () => {
    const ids = seqIds();
    // Simulate a persisted layout where the agent panel was promoted on top and
    // collapsed the built-ins (the exact orphan-on-restart shape).
    const promoted = addPanelAsTopSection(defaultRightPaneLayout(), "git-sync-agent", ids);
    expect(promoted.sections[0].group.panelIds).toEqual(["git-sync-agent"]);

    const reconciled = reconcileLayoutToBuiltins(promoted);
    const survivingIds = reconciled.sections.flatMap((s) => s.group.panelIds);
    expect(survivingIds).not.toContain("git-sync-agent");
    expect(survivingIds).toEqual(expect.arrayContaining([INBOX_PANEL_ID, SCRATCH_PANEL_ID]));
    // Floor invariant restored after dropping the (expanded) agent section.
    expect(expandedSectionCount(reconciled)).toBeGreaterThanOrEqual(1);
  });

  it("is a no-op for a built-ins-only layout", () => {
    const layout = defaultRightPaneLayout();
    expect(reconcileLayoutToBuiltins(layout)).toBe(layout);
  });

  it("KEEPS a persisted pane section under viewer/undefined mode", () => {
    const ids = seqIds();
    // A durable `pane:` section is RETAINED across a reload (unlike the self-re-adding
    // agent module above). The render-time filter — not the prune — hides it while it
    // isn't registered, so full arrangement survives.
    const withPane = addPanelAsTopSection(defaultRightPaneLayout(), "pane:acme:core/sales", ids);
    expect(reconcileLayoutToBuiltins(withPane)).toBe(withPane);
    const reconciled = reconcileLayoutToBuiltins(withPane);
    expect(reconciled.sections.flatMap((s) => s.group.panelIds)).toContain("pane:acme:core/sales");
    expect(reconciled.sections.flatMap((s) => s.group.panelIds)).toEqual(
      expect.arrayContaining([INBOX_PANEL_ID, SCRATCH_PANEL_ID]),
    );
  });

  it("silently drops a persisted section of the retired JSON widget kind — no ghost, no blank", () => {
    // The id is assembled so the source never spells the retired prefix.
    const retired = (id: string) => `${RETIRED_WIDGET_KIND}:${id}`;
    const ids = seqIds();
    const lone = addPanelAsTopSection(defaultRightPaneLayout(), retired("sales"), ids);
    // A second widget tabbed into the built-in LIVE section.
    const grouped = mergePanelIntoSection(
      addPanelAsTopSection(lone, retired("triage"), ids),
      retired("triage"),
      MAIN_SECTION_ID,
    );
    expect(grouped.sections.flatMap((s) => s.group.panelIds)).toContain(retired("triage"));

    {
      const reconciled = reconcileLayoutToBuiltins(grouped);
      const survivingIds = reconciled.sections.flatMap((s) => s.group.panelIds);
      expect(survivingIds.some((id) => id.startsWith(`${RETIRED_WIDGET_KIND}:`))).toBe(false);
      // The widget-only section is gone; the built-in it was tabbed with stays.
      expect(reconciled.sections.every((s) => s.group.panelIds.length > 0)).toBe(true);
      expect(reconciled.sections).toHaveLength(defaultRightPaneLayout().sections.length);
      expect(survivingIds).toEqual(expect.arrayContaining([INBOX_PANEL_ID, SCRATCH_PANEL_ID]));
      expect(expandedSectionCount(reconciled)).toBeGreaterThanOrEqual(1);
    }

    // A layout that held ONLY retired widgets falls back to the default, never empty.
    const only: RightPaneLayout = {
      sections: [
        {
          id: "w",
          group: { panelIds: [retired("sales")], activePanelId: retired("sales") },
          collapsed: false,
          size: 1,
        },
      ],
    };
    expect(reconcileLayoutToBuiltins(only)).toEqual(defaultRightPaneLayout());
  });

  it("SW-AU-56 (AC-3): drops a persisted 'pipeline-config' section — the retired panel leaves no ghost", () => {
    const ids = seqIds();
    // AU-56 removed the pipeline-config right-pane panel (re-homed inline into Settings). A layout
    // persisted before the removal still names the id; since it is no longer a built-in, the generic
    // foreign-id prune drops it — no registration needed, no blank/ghost section.
    const stale = addPanelAsTopSection(defaultRightPaneLayout(), "pipeline-config", ids);
    expect(stale.sections[0].group.panelIds).toEqual(["pipeline-config"]);

    const reconciled = reconcileLayoutToBuiltins(stale);
    const survivingIds = reconciled.sections.flatMap((s) => s.group.panelIds);
    expect(survivingIds).not.toContain("pipeline-config");
    expect(survivingIds).toEqual(expect.arrayContaining([INBOX_PANEL_ID, SCRATCH_PANEL_ID]));
    expect(expandedSectionCount(reconciled)).toBeGreaterThanOrEqual(1);
  });

  it("SW-AV-19 (AC): drops a persisted 'worktrees' section — the folded-in tab leaves no ghost", () => {
    const ids = seqIds();
    // SW-AV-19 folded the standalone Worktrees tab into the Agents fleet (per-pill
    // worktree-inline + the reused-WorktreesPanel lens), so "worktrees" is no longer a
    // built-in. A layout persisted before the fold still names the id; the generic
    // foreign-id prune drops it — no registration, no blank/ghost section.
    const stale = addPanelAsTopSection(defaultRightPaneLayout(), "worktrees", ids);
    expect(stale.sections[0].group.panelIds).toEqual(["worktrees"]);

    const reconciled = reconcileLayoutToBuiltins(stale);
    const survivingIds = reconciled.sections.flatMap((s) => s.group.panelIds);
    expect(survivingIds).not.toContain("worktrees");
    expect(survivingIds).toEqual(expect.arrayContaining([INBOX_PANEL_ID, SCRATCH_PANEL_ID]));
    expect(expandedSectionCount(reconciled)).toBeGreaterThanOrEqual(1);
  });

  it("SW-AV-26 (AC): drops a persisted 'automations' section — the retired tab leaves no ghost", () => {
    const ids = seqIds();
    // SW-AV-26 retired the Automations tab, so "automations" is no longer a built-in. A layout
    // persisted before the retirement still names the id; the generic foreign-id prune drops it —
    // no registration, no blank/ghost section.
    expect(isBuiltinPanelId("automations")).toBe(false);

    const stale = addPanelAsTopSection(defaultRightPaneLayout(), "automations", ids);
    expect(stale.sections[0].group.panelIds).toEqual(["automations"]);

    const reconciled = reconcileLayoutToBuiltins(stale);
    const survivingIds = reconciled.sections.flatMap((s) => s.group.panelIds);
    expect(survivingIds).not.toContain("automations");
    expect(survivingIds).toEqual(expect.arrayContaining([INBOX_PANEL_ID, SCRATCH_PANEL_ID]));
    expect(expandedSectionCount(reconciled)).toBeGreaterThanOrEqual(1);
  });

  it("drops a persisted 'agent-result' section — run output lives in the agent area now", () => {
    const ids = seqIds();
    // Run output moved to the bottom agent area, so "agent-result" is no longer registered in the
    // right pane. A layout saved while it was open still names it; the foreign-id prune drops it.
    expect(isBuiltinPanelId("agent-result")).toBe(false);
    const stale = addPanelAsTopSection(defaultRightPaneLayout(), "agent-result", ids);
    expect(stale.sections[0].group.panelIds).toEqual(["agent-result"]);

    const reconciled = reconcileLayoutToBuiltins(stale);
    const survivingIds = reconciled.sections.flatMap((s) => s.group.panelIds);
    expect(survivingIds).not.toContain("agent-result");
    expect(survivingIds).toEqual(expect.arrayContaining([INBOX_PANEL_ID, SCRATCH_PANEL_ID]));
    expect(expandedSectionCount(reconciled)).toBeGreaterThanOrEqual(1);
  });

  it("falls back to the default layout if pruning empties the pane (defensive)", () => {
    const allForeign: RightPaneLayout = {
      sections: [
        { id: "s1", group: { panelIds: ["foo"], activePanelId: "foo" }, collapsed: false, size: 1 },
      ],
    };
    expect(reconcileLayoutToBuiltins(allForeign)).toEqual(defaultRightPaneLayout());
  });
});

describe("reconcileLayoutToBuiltins keeps the built-ins", () => {
  it("drops the retired right-pane History panel from a saved layout", () => {
    const layout = addPanelAsTopSection(defaultRightPaneLayout(), "git-sync-history", seqIds());
    const reconciled = reconcileLayoutToBuiltins(layout);
    expect(findPanelLocation(reconciled, "git-sync-history")).toBeNull();
    expect(BUILTIN_PANEL_IDS.has("git-sync-history")).toBe(false);
  });

  it("leaves the default untouched (no over-pruning)", () => {
    const layout = defaultRightPaneLayout();
    expect(reconcileLayoutToBuiltins(layout)).toBe(layout);
  });

  it("retains a durable pane section", () => {
    const layout = addPanelAsTopSection(defaultRightPaneLayout(), "pane:acme:core/sales", seqIds());
    const surviving = reconcileLayoutToBuiltins(layout).sections.flatMap((s) => s.group.panelIds);
    expect(surviving).toContain("pane:acme:core/sales");
  });
});

describe("selectRenderableLayout (render-time dead-section filter)", () => {
  const reg = (...ids: string[]) => {
    const set = new Set(ids);
    return (id: string) => set.has(id);
  };

  it("returns the SAME layout ref when every panel is registered (no churn)", () => {
    const layout = defaultRightPaneLayout();
    expect(selectRenderableLayout(layout, reg(INBOX_PANEL_ID, SCRATCH_PANEL_ID))).toBe(layout);
  });

  it("hides an unregistered member but keeps registered siblings in the group", () => {
    const layout: RightPaneLayout = {
      sections: [
        {
          id: "s1",
          group: {
            panelIds: [INBOX_PANEL_ID, "pane:acme:core/sales"],
            activePanelId: INBOX_PANEL_ID,
          },
          collapsed: false,
          size: 1,
        },
      ],
    };
    const out = selectRenderableLayout(layout, reg(INBOX_PANEL_ID)); // the pane is absent
    expect(out.sections[0].group.panelIds).toEqual([INBOX_PANEL_ID]);
    expect(out.sections[0].group.activePanelId).toBe(INBOX_PANEL_ID);
  });

  it("drops a section whose entire group is unregistered", () => {
    const layout: RightPaneLayout = {
      sections: [
        {
          id: "keep",
          group: { panelIds: [PANEL_A], activePanelId: PANEL_A },
          collapsed: false,
          size: 1,
        },
        {
          id: "drop",
          group: { panelIds: ["pane:acme:core/gone"], activePanelId: "pane:acme:core/gone" },
          collapsed: true,
          size: 1,
        },
      ],
    };
    const out = selectRenderableLayout(layout, reg(PANEL_A));
    expect(out.sections.map((s) => s.id)).toEqual(["keep"]);
  });

  it("falls the DISPLAYED active tab back to the first renderable member — without mutating the source", () => {
    const layout: RightPaneLayout = {
      sections: [
        {
          id: "s1",
          group: {
            panelIds: ["pane:acme:core/absent", PANEL_A],
            activePanelId: "pane:acme:core/absent",
          },
          collapsed: false,
          size: 1,
        },
      ],
    };
    const out = selectRenderableLayout(layout, reg(PANEL_A));
    expect(out.sections[0].group.activePanelId).toBe(PANEL_A);
    // Source layout's stored activePanelId is UNTOUCHED (absent ≠ mutated).
    expect(layout.sections[0].group.activePanelId).toBe("pane:acme:core/absent");
    expect(layout.sections[0].group.panelIds).toEqual(["pane:acme:core/absent", PANEL_A]);
  });

  it("re-asserts the expanded floor when the only expanded section is dropped", () => {
    const layout: RightPaneLayout = {
      sections: [
        {
          id: "gone",
          group: { panelIds: ["pane:acme:core/x"], activePanelId: "pane:acme:core/x" },
          collapsed: false, // the only expanded section…
          size: 1,
        },
        {
          id: "survivor",
          group: { panelIds: [SCRATCH_PANEL_ID], activePanelId: SCRATCH_PANEL_ID },
          collapsed: true, // …everything else collapsed
          size: 1,
        },
      ],
    };
    const out = selectRenderableLayout(layout, reg(SCRATCH_PANEL_ID));
    expect(out.sections.map((s) => s.id)).toEqual(["survivor"]);
    expect(expandedSectionCount(out)).toBeGreaterThanOrEqual(1);
    // Source untouched.
    expect(layout.sections[1].collapsed).toBe(true);
  });
});

describe("absent ≠ closed (team-switch round-trip) + idempotent", () => {
  const reg =
    (...ids: string[]) =>
    (id: string) =>
      new Set(ids).has(id);

  it("AC-6: a write on the RAW layout preserves a hidden (team-absent) pane — absent ≠ closed", () => {
    const raw: RightPaneLayout = {
      sections: [
        {
          id: "b",
          group: { panelIds: [INBOX_PANEL_ID], activePanelId: INBOX_PANEL_ID },
          collapsed: false,
          size: 0.5,
        },
        {
          id: "w",
          group: { panelIds: ["pane:acme:core/sales"], activePanelId: "pane:acme:core/sales" },
          collapsed: true,
          size: 0.5,
        },
      ],
    };
    // In a team where the pane isn't registered it's hidden from RENDER…
    expect(
      findPanelLocation(selectRenderableLayout(raw, reg(INBOX_PANEL_ID)), "pane:acme:core/sales"),
    ).toBeNull();
    // …but a write op runs on the RAW layout and leaves the section intact — only
    // an explicit removePanel (user close) deletes it.
    expect(findPanelLocation(setSectionSize(raw, "b", 0.8), "pane:acme:core/sales")).not.toBeNull();
  });

  it("AC-5/AC-6: re-registering restores exact arrangement; an absent tab-group sibling degrades to standalone", () => {
    const arranged: RightPaneLayout = {
      sections: [
        {
          id: "b",
          group: { panelIds: [INBOX_PANEL_ID], activePanelId: INBOX_PANEL_ID },
          collapsed: false,
          size: 0.7,
        },
        {
          id: "w",
          group: {
            panelIds: [SCRATCH_PANEL_ID, "pane:acme:core/sales"],
            activePanelId: "pane:acme:core/sales",
          },
          collapsed: true,
          size: 0.3,
        },
      ],
    };
    const inA = reg(INBOX_PANEL_ID, SCRATCH_PANEL_ID, "pane:acme:core/sales");
    // Team A (all registered): arrangement untouched — same ref, zero churn.
    expect(selectRenderableLayout(arranged, inA)).toBe(arranged);
    // Team B (pane absent): the pane tab is hidden and its group-sibling Notes
    // stands alone in the same slot — no ghost tab.
    const inB = selectRenderableLayout(arranged, reg(INBOX_PANEL_ID, SCRATCH_PANEL_ID));
    expect(inB.sections[1].group.panelIds).toEqual([SCRATCH_PANEL_ID]);
    expect(inB.sections[1].collapsed).toBe(true); // size/collapse of the slot preserved
    // Back in Team A: full arrangement (order, size, collapse, tab-group) restored.
    expect(selectRenderableLayout(arranged, inA)).toBe(arranged);
  });

  it("AC-8: reconcile is idempotent for a pane layout (launch stacks nothing)", () => {
    const withPane = addPanelAsTopSection(
      defaultRightPaneLayout(),
      "pane:acme:core/sales",
      seqIds(),
    );
    const once = reconcileLayoutToBuiltins(withPane);
    expect(reconcileLayoutToBuiltins(once)).toBe(once);
  });

  it("AC-4/AC-13: full arrangement survives a persist→hydrate round-trip (order/collapse/size/tab-group)", () => {
    // A pane the user arranged: second in the stack, tab-grouped with a built-in
    // (pane the active tab), collapsed, custom size.
    const arranged: RightPaneLayout = {
      sections: [
        {
          id: "top",
          group: { panelIds: [INBOX_PANEL_ID], activePanelId: INBOX_PANEL_ID },
          collapsed: false,
          size: 0.7,
        },
        {
          id: "panegroup",
          group: {
            panelIds: [SCRATCH_PANEL_ID, "pane:acme:core/sales"],
            activePanelId: "pane:acme:core/sales",
          },
          collapsed: true,
          size: 0.3,
        },
      ],
    };
    // Persist = JSON round-trip; hydrate = coerce then reconcile(viewer) — the exact
    // load path (`viewer-store` rehydrate). The durable pane
    // arrangement rides the persisted layout and comes back intact.
    const persisted = JSON.parse(JSON.stringify(arranged));
    const hydrated = reconcileLayoutToBuiltins(coerceRightPaneLayout(persisted)!);

    expect(hydrated.sections).toHaveLength(2);
    expect(hydrated.sections[0].group.panelIds).toEqual([INBOX_PANEL_ID]); // order preserved
    const w = hydrated.sections[1]; // pane still second in the stack
    expect(w.group.panelIds).toEqual([SCRATCH_PANEL_ID, "pane:acme:core/sales"]); // tab-group intact
    expect(w.group.activePanelId).toBe("pane:acme:core/sales"); // active tab intact
    expect(w.collapsed).toBe(true); // collapse intact
    expect(w.size).toBeCloseTo(0.3); // size intact
  });
});

describe("migrateLegacyRightPaneState", () => {
  it("maps an empty legacy state to the default layout", () => {
    const layout = migrateLegacyRightPaneState({});
    expect(layout).toEqual(defaultRightPaneLayout());
  });

  it("maps team-keyed rightPaneSplitRatio → section sizes (split = scratch share)", () => {
    const layout = migrateLegacyRightPaneState({
      rightPaneSplitRatio: { "org:teamA": 0.25 },
    });
    const [top, bottom] = layout.sections;
    // bottom (scratch) gets the split share; top gets the complement
    expect(bottom.size).toBeCloseTo(0.25);
    expect(top.size).toBeCloseTo(0.75);
  });

  it("prefers an already-migrated valid layout when present", () => {
    const existing = addPanelAsTopSection(defaultRightPaneLayout(), SCRATCH_PANEL_ID, seqIds());
    const layout = migrateLegacyRightPaneState({ rightPaneLayout: existing });
    expect(layout).toEqual(existing);
  });

  it("coerces a malformed persisted layout to the default rather than crashing", () => {
    const layout = migrateLegacyRightPaneState({
      rightPaneLayout: { sections: "not-an-array" } as unknown as RightPaneLayout,
    });
    expect(layout).toEqual(defaultRightPaneLayout());
  });
});

describe("addPanelAsTopSection", () => {
  it("inserts the panel as the top section and collapses the others", () => {
    const layout = addPanelAsTopSection(defaultRightPaneLayout(), "agent", seqIds());
    expect(layout.sections[0].group.panelIds).toEqual(["agent"]);
    expect(layout.sections[0].collapsed).toBe(false);
    // every other section collapsed
    expect(layout.sections.slice(1).every((s) => s.collapsed)).toBe(true);
  });

  it("relocates a panel that already exists rather than duplicating it", () => {
    const layout = addPanelAsTopSection(multiTabLayout(), PANEL_B, seqIds());
    const locations = layout.sections.filter((s) => s.group.panelIds.includes(PANEL_B));
    expect(locations).toHaveLength(1);
    expect(layout.sections[0].group.panelIds).toEqual([PANEL_B]);
    // B left A's group
    const groupA = layout.sections.find((s) => s.group.panelIds.includes(PANEL_A));
    expect(groupA?.group.panelIds).not.toContain(PANEL_B);
  });
});

describe("revealPanel (shared reveal seam)", () => {
  it("adds the panel as a new top section when absent", () => {
    // A pane isn't in the default layout, so revealing it must add a section.
    const added = revealPanel(defaultRightPaneLayout(), "pane:acme:core/sales");
    expect(isPanelVisible(added, "pane:acme:core/sales")).toBe(true);
    expect(added.sections[0].group.panelIds).toContain("pane:acme:core/sales");
  });

  it("activates and expands an existing (collapsed / background-tab) panel", () => {
    // Put B behind A in the main group, then collapse that section.
    let layout = setActivePanel(multiTabLayout(), MAIN_SECTION_ID, PANEL_A);
    layout = collapseSection(layout, MAIN_SECTION_ID);
    const revealed = revealPanel(layout, PANEL_B);
    expect(isPanelVisible(revealed, PANEL_B)).toBe(true);
  });

  it("is a no-op on an already-visible active panel", () => {
    const layout = setActivePanel(multiTabLayout(), MAIN_SECTION_ID, PANEL_A);
    expect(revealPanel(layout, PANEL_A)).toEqual(layout);
  });
});

describe("collapse / expand with the at-least-one-expanded floor", () => {
  it("collapses a section when another stays expanded", () => {
    const def = defaultRightPaneLayout();
    const collapsed = collapseSection(def, def.sections[0].id);
    expect(collapsed.sections[0].collapsed).toBe(true);
    expect(expandedSectionCount(collapsed)).toBe(1);
  });

  it("refuses to collapse the LAST expanded section (floor invariant)", () => {
    const def = defaultRightPaneLayout();
    const oneCollapsed = collapseSection(def, def.sections[0].id);
    // now only section[1] is expanded; collapsing it must be a no-op
    const attempt = collapseSection(oneCollapsed, oneCollapsed.sections[1].id);
    expect(attempt.sections[1].collapsed).toBe(false);
    expect(expandedSectionCount(attempt)).toBe(1);
  });

  it("expands a collapsed section", () => {
    const def = defaultRightPaneLayout();
    const collapsed = collapseSection(def, def.sections[0].id);
    const expanded = expandSection(collapsed, collapsed.sections[0].id);
    expect(expanded.sections[0].collapsed).toBe(false);
  });
});

describe("splitPanelToOwnSection / mergePanelIntoSection round-trip", () => {
  it("pulls a tab out of its group into its own section, then merges it back", () => {
    const def = multiTabLayout();
    const split = splitPanelToOwnSection(def, PANEL_B, seqIds());
    const sectionB = split.sections.find(
      (s) => s.group.panelIds.length === 1 && s.group.panelIds[0] === PANEL_B,
    );
    expect(sectionB).toBeDefined();
    // A's group no longer has B (the Inbox tab stays with A)
    const groupA = split.sections.find((s) => s.group.panelIds.includes(PANEL_A));
    expect(groupA?.group.panelIds).toEqual([PANEL_A, INBOX_PANEL_ID]);

    // merge B back into A's section
    const merged = mergePanelIntoSection(split, PANEL_B, groupA!.id);
    const target = merged.sections.find((s) => s.id === groupA!.id);
    expect(target?.group.panelIds).toContain(PANEL_B);
    // the standalone B section is gone
    expect(merged.sections.some((s) => s.id === sectionB!.id)).toBe(false);
  });

  it("splitting an already-solo panel is a no-op", () => {
    const def = defaultRightPaneLayout();
    const before = splitPanelToOwnSection(def, SCRATCH_PANEL_ID, seqIds());
    expect(before).toEqual(def);
  });

  it("mints a section id that does NOT collide with an existing one (reset-counter guard)", () => {
    const def = multiTabLayout(); // contains MAIN_SECTION_ID + SCRATCH_SECTION_ID
    // A factory whose first value collides with a persisted section id (what a
    // reload-reset counter would do), then yields a free one.
    let n = 0;
    const factory = () => (n++ === 0 ? MAIN_SECTION_ID : "rps-fresh");
    const split = splitPanelToOwnSection(def, PANEL_B, factory);
    const ids = split.sections.map((s) => s.id);
    expect(new Set(ids).size).toBe(ids.length); // all unique — no duplicate key
    expect(ids).toContain("rps-fresh");
  });

  it("survives a pathological factory that always collides (bounded, no hang)", () => {
    const def = multiTabLayout();
    const alwaysCollides = () => MAIN_SECTION_ID; // never yields a free id
    const split = splitPanelToOwnSection(def, PANEL_B, alwaysCollides);
    const ids = split.sections.map((s) => s.id);
    expect(new Set(ids).size).toBe(ids.length); // last-resort suffix kept it unique
  });
});

describe("removePanel", () => {
  it("removes a panel and drops the section when its group empties", () => {
    const def = defaultRightPaneLayout();
    const removed = removePanel(def, SCRATCH_PANEL_ID);
    expect(removed.sections.some((s) => s.group.panelIds.includes(SCRATCH_PANEL_ID))).toBe(false);
    // scratch was the only panel in its section → section dropped (the main group remains)
    expect(removed.sections).toHaveLength(1);
  });

  it("reassigns the active panel when the active one is removed from a multi-panel group", () => {
    // The main group is [A, B, Inbox(active)].
    const def = multiTabLayout();
    const removed = removePanel(def, INBOX_PANEL_ID);
    const top = removed.sections[0];
    expect(top.group.panelIds).toEqual([PANEL_A, PANEL_B]);
    // the clamped neighbor at the removed index (2 → 1) takes over as active
    expect(top.group.activePanelId).toBe(PANEL_B);
  });
});

describe("setSectionSize", () => {
  it("updates a section's size and clamps negatives to 0", () => {
    const def = defaultRightPaneLayout();
    const resized = setSectionSize(def, def.sections[0].id, 0.8);
    expect(resized.sections[0].size).toBe(0.8);
    expect(setSectionSize(def, def.sections[0].id, -5).sections[0].size).toBe(0);
  });
});

describe("removePanel re-asserts the expanded floor", () => {
  it("expands a survivor when removal drops the only expanded section", () => {
    // [A expanded, B collapsed]; remove A's only panel → B must become expanded
    const layout: RightPaneLayout = {
      sections: [
        { id: "a", group: { panelIds: ["pa"], activePanelId: "pa" }, collapsed: false, size: 0.5 },
        { id: "b", group: { panelIds: ["pb"], activePanelId: "pb" }, collapsed: true, size: 0.5 },
      ],
    };
    const after = removePanel(layout, "pa");
    expect(after.sections).toHaveLength(1);
    expect(after.sections[0].id).toBe("b");
    expect(after.sections[0].collapsed).toBe(false);
    expect(expandedSectionCount(after)).toBe(1);
  });
});

describe("coerceRightPaneLayout hardening", () => {
  it("rejects an empty panelIds group", () => {
    expect(
      coerceRightPaneLayout({
        sections: [
          { id: "s", group: { panelIds: [], activePanelId: "x" }, collapsed: false, size: 1 },
        ],
      }),
    ).toBeNull();
  });

  it("rejects activePanelId not in panelIds", () => {
    expect(
      coerceRightPaneLayout({
        sections: [
          { id: "s", group: { panelIds: ["a"], activePanelId: "b" }, collapsed: false, size: 1 },
        ],
      }),
    ).toBeNull();
  });

  it("rejects a duplicate section id", () => {
    expect(
      coerceRightPaneLayout({
        sections: [
          { id: "dup", group: { panelIds: ["a"], activePanelId: "a" }, collapsed: false, size: 1 },
          { id: "dup", group: { panelIds: ["b"], activePanelId: "b" }, collapsed: false, size: 1 },
        ],
      }),
    ).toBeNull();
  });

  it("rejects a panel id duplicated across sections", () => {
    expect(
      coerceRightPaneLayout({
        sections: [
          {
            id: "s1",
            group: { panelIds: ["dup"], activePanelId: "dup" },
            collapsed: false,
            size: 1,
          },
          {
            id: "s2",
            group: { panelIds: ["dup"], activePanelId: "dup" },
            collapsed: false,
            size: 1,
          },
        ],
      }),
    ).toBeNull();
  });

  it("rejects a non-finite or negative size", () => {
    const mk = (size: number) => ({
      sections: [
        { id: "s", group: { panelIds: ["a"], activePanelId: "a" }, collapsed: false, size },
      ],
    });
    expect(coerceRightPaneLayout(mk(Number.NaN))).toBeNull();
    expect(coerceRightPaneLayout(mk(Number.POSITIVE_INFINITY))).toBeNull();
    expect(coerceRightPaneLayout(mk(-1))).toBeNull();
    expect(coerceRightPaneLayout(mk(0.5))).not.toBeNull();
  });

  it("repairs an all-collapsed layout by expanding the first section (floor)", () => {
    const coerced = coerceRightPaneLayout({
      sections: [
        { id: "s1", group: { panelIds: ["a"], activePanelId: "a" }, collapsed: true, size: 1 },
        { id: "s2", group: { panelIds: ["b"], activePanelId: "b" }, collapsed: true, size: 1 },
      ],
    });
    expect(coerced).not.toBeNull();
    expect(coerced!.sections[0].collapsed).toBe(false);
  });
});

describe("migrate legacy main-minimized", () => {
  it("collapses the main section when the legacy split was maximized", () => {
    const layout = migrateLegacyRightPaneState({ rightPaneSplitRatio: { "org:team": 0.95 } });
    expect(layout.sections[0].collapsed).toBe(true); // main collapsed
    expect(layout.sections[1].collapsed).toBe(false); // scratch expanded (floor)
  });
});

describe("moveSection", () => {
  // A 3-section layout with distinguishable sizes / collapsed flags so a reorder
  // can be asserted to carry every per-section field along with the move.
  const threeSections = (): RightPaneLayout => ({
    sections: [
      { id: "a", group: { panelIds: ["pa"], activePanelId: "pa" }, collapsed: false, size: 0.5 },
      {
        id: "b",
        group: { panelIds: ["pb1", "pb2"], activePanelId: "pb2" },
        collapsed: true,
        size: 0.3,
      },
      { id: "c", group: { panelIds: ["pc"], activePanelId: "pc" }, collapsed: false, size: 0.2 },
    ],
  });

  it("reorders a section to a new index", () => {
    const moved = moveSection(threeSections(), "a", 2);
    expect(moved.sections.map((s) => s.id)).toEqual(["b", "c", "a"]);
  });

  it("moves a section toward the front", () => {
    const moved = moveSection(threeSections(), "c", 0);
    expect(moved.sections.map((s) => s.id)).toEqual(["c", "a", "b"]);
  });

  it("preserves each moved section's collapsed / size / group", () => {
    const moved = moveSection(threeSections(), "b", 0);
    const b = moved.sections[0];
    expect(b.id).toBe("b");
    expect(b.collapsed).toBe(true);
    expect(b.size).toBeCloseTo(0.3);
    expect(b.group.panelIds).toEqual(["pb1", "pb2"]);
    expect(b.group.activePanelId).toBe("pb2");
  });

  it("clamps an out-of-range index to the end", () => {
    const moved = moveSection(threeSections(), "a", 99);
    expect(moved.sections.map((s) => s.id)).toEqual(["b", "c", "a"]);
  });

  it("clamps a negative index to the front", () => {
    const moved = moveSection(threeSections(), "c", -5);
    expect(moved.sections.map((s) => s.id)).toEqual(["c", "a", "b"]);
  });

  it("leaves the expanded count unchanged (floor invariant holds for free)", () => {
    const before = threeSections();
    const moved = moveSection(before, "a", 2);
    expect(expandedSectionCount(moved)).toBe(expandedSectionCount(before));
  });

  it("is a no-op when toIndex equals the current index", () => {
    const before = threeSections();
    const moved = moveSection(before, "b", 1);
    expect(moved.sections.map((s) => s.id)).toEqual(["a", "b", "c"]);
  });

  it("is a no-op for an unknown section id", () => {
    const before = threeSections();
    const moved = moveSection(before, "nope", 0);
    expect(moved.sections.map((s) => s.id)).toEqual(["a", "b", "c"]);
  });

  it("returns a new layout (does not mutate the input)", () => {
    const before = threeSections();
    moveSection(before, "a", 2);
    expect(before.sections.map((s) => s.id)).toEqual(["a", "b", "c"]);
  });

  it("round-trips back to the original order", () => {
    const before = threeSections();
    const there = moveSection(before, "a", 2); // [b, c, a]
    const back = moveSection(there, "a", 0); // [a, b, c]
    expect(back.sections.map((s) => s.id)).toEqual(["a", "b", "c"]);
  });
});

describe("movePanelWithinSection", () => {
  it("reorders a tab within its group (A|B → B|A)", () => {
    const def = multiTabLayout(); // main group [A, B, Inbox]
    // move B (index 1) to the front
    const moved = movePanelWithinSection(def, PANEL_B, 0);
    expect(moved.sections[0].group.panelIds).toEqual([PANEL_B, PANEL_A, INBOX_PANEL_ID]);
  });

  it("leaves the active panel unchanged", () => {
    const def = multiTabLayout(); // active = Inbox
    const moved = movePanelWithinSection(def, PANEL_B, 0);
    expect(moved.sections[0].group.activePanelId).toBe(INBOX_PANEL_ID);
  });

  it("clamps an out-of-range index", () => {
    const def = multiTabLayout();
    const moved = movePanelWithinSection(def, PANEL_A, 99);
    expect(moved.sections[0].group.panelIds).toEqual([PANEL_B, INBOX_PANEL_ID, PANEL_A]);
  });

  it("is a no-op for an unknown panel and does not mutate the input", () => {
    const def = multiTabLayout();
    const before = def.sections[0].group.panelIds.slice();
    const moved = movePanelWithinSection(def, "ghost", 0);
    expect(moved.sections[0].group.panelIds).toEqual(before);
    expect(def.sections[0].group.panelIds).toEqual(before);
  });
});

describe("setActivePanel", () => {
  it("switches the active panel of a group", () => {
    const def = multiTabLayout();
    const switched = setActivePanel(def, def.sections[0].id, PANEL_B);
    expect(switched.sections[0].group.activePanelId).toBe(PANEL_B);
  });
});

describe("findPanelLocation / coerceRightPaneLayout", () => {
  it("locates a panel by section + panel index", () => {
    const def = multiTabLayout();
    const loc = findPanelLocation(def, PANEL_B);
    expect(loc).toEqual({ sectionIndex: 0, panelIndex: 1 });
  });

  it("returns null for an unknown panel", () => {
    expect(findPanelLocation(defaultRightPaneLayout(), "nope")).toBeNull();
  });

  it("coerces a valid layout and rejects garbage", () => {
    const def = defaultRightPaneLayout();
    expect(coerceRightPaneLayout(def)).toEqual(def);
    expect(coerceRightPaneLayout(null)).toBeNull();
    expect(coerceRightPaneLayout({ sections: [{ id: "x" }] })).toBeNull();
  });

  it("isBuiltinPanelId is true for built-ins, false for panes/dynamic panels", () => {
    expect(isBuiltinPanelId(SCRATCH_PANEL_ID)).toBe(true);
    expect(isBuiltinPanelId("git-sync-history")).toBe(false);
    expect(isBuiltinPanelId(INBOX_PANEL_ID)).toBe(true);
    expect(isBuiltinPanelId("pane:acme:core/sales")).toBe(false);
    expect(isBuiltinPanelId("agent:xyz")).toBe(false);
    // SW-AV-19: the retired standalone Worktrees tab is no longer a built-in.
    expect(isBuiltinPanelId("worktrees")).toBe(false);
  });
});

// --- SW-AV-10: two-group IA ---------------------------------------------------

describe("retired panels in a saved layout", () => {
  it("the retired Connectors panel is no longer a built-in (pruned from a saved layout)", () => {
    expect(isBuiltinPanelId("connectors")).toBe(false);
    const saved = revealPanel(defaultRightPaneLayout(), "connectors");
    expect(findPanelLocation(reconcileLayoutToBuiltins(saved), "connectors")).toBeNull();
  });

  it.each(["running-agents", "workflow-library", "triggers", "agent-profiles"])(
    "%s is no longer a built-in",
    (id) => {
      expect(isBuiltinPanelId(id)).toBe(false);
    },
  );

  it("a saved layout with the old Agents tab and Setup (Triggers) group loads as the user surfaces", () => {
    // A layout persisted before the right pane became user-surfaces-only: the Agents panel
    // is a tab in the main group (and active), Workflows sits with Notes, and the collapsed
    // Setup group holds Triggers.
    const saved = coerceRightPaneLayout({
      sections: [
        {
          id: MAIN_SECTION_ID,
          label: "Live",
          group: {
            panelIds: ["pipeline", "running-agents", "board", INBOX_PANEL_ID],
            activePanelId: "running-agents",
          },
          collapsed: false,
          size: 0.55,
        },
        {
          id: SCRATCH_SECTION_ID,
          group: {
            panelIds: [SCRATCH_PANEL_ID, "workflow-library"],
            activePanelId: SCRATCH_PANEL_ID,
          },
          collapsed: false,
          size: 0.45,
        },
        {
          id: "rps-setup",
          label: "Setup",
          group: { panelIds: ["triggers"], activePanelId: "triggers" },
          collapsed: true,
          size: 0.4,
        },
      ],
    });
    expect(saved).not.toBeNull();
    const restored = reconcileLayoutToBuiltins(saved!);

    // No ghost tab and no leftover Setup section.
    for (const id of ["running-agents", "workflow-library", "triggers"]) {
      expect(findPanelLocation(restored, id)).toBeNull();
    }
    expect(restored.sections.map((s) => s.id)).toEqual([MAIN_SECTION_ID, SCRATCH_SECTION_ID]);
    // The removed active tab (and the retired Pipeline/Board tabs) hand off to the surviving Inbox.
    const main = restored.sections[0];
    expect(main.group.panelIds).toEqual([INBOX_PANEL_ID]);
    expect(main.group.activePanelId).toBe(INBOX_PANEL_ID);
    expect(restored.sections[1].group.panelIds).toEqual([SCRATCH_PANEL_ID]);
    expect(expandedSectionCount(restored)).toBeGreaterThanOrEqual(1);
  });

  it("a saved layout that still names the retired Agent Profiles panel loads without it", () => {
    const lone = coerceRightPaneLayout({
      sections: [
        ...defaultRightPaneLayout().sections,
        {
          id: "rps-lone",
          group: { panelIds: ["agent-profiles"], activePanelId: "agent-profiles" },
          collapsed: false,
          size: 0.2,
        },
      ],
    });
    expect(reconcileLayoutToBuiltins(lone!)).toEqual(
      reconcileLayoutToBuiltins(coerceRightPaneLayout(defaultRightPaneLayout())!),
    );
  });

  it.each(["pipeline", "board"])("the retired %s panel is no longer a built-in", (id) => {
    expect(isBuiltinPanelId(id)).toBe(false);
  });

  it("a saved layout naming the retired Pipeline and Board panels is pruned silently on load", () => {
    // A pre-retirement layout: Pipeline split into its own section, Board tabbed beside
    // the Inbox (Board active), Notes below.
    const [main, notes] = defaultRightPaneLayout().sections;
    const saved = coerceRightPaneLayout({
      sections: [
        {
          id: "rps-lone",
          group: { panelIds: ["pipeline"], activePanelId: "pipeline" },
          collapsed: false,
          size: 0.3,
        },
        {
          ...main,
          group: { panelIds: ["board", INBOX_PANEL_ID], activePanelId: "board" },
        },
        notes,
      ],
    });
    expect(saved).not.toBeNull();
    {
      const restored = reconcileLayoutToBuiltins(saved!);
      expect(findPanelLocation(restored, "pipeline")).toBeNull();
      expect(findPanelLocation(restored, "board")).toBeNull();
      // No ghost section; the Inbox takes over as the main group's active tab.
      expect(restored).toEqual(defaultRightPaneLayout());
    }
  });

  it("the default's only group label is Live (Notes unlabelled)", () => {
    const labels = defaultRightPaneLayout().sections.map((s) => s.label);
    expect(labels).toEqual(["Live", undefined]);
  });
});

describe("first run", () => {
  it("a fresh profile's load path lands on Activity", () => {
    // Nothing persisted: the boot load folds an empty legacy state into the default.
    const layout = migrateLegacyRightPaneState({});
    expect(layout).toEqual(defaultRightPaneLayout());
    expect(isPanelVisible(layout, INBOX_PANEL_ID)).toBe(true);
  });
});

it("a layout holding only a retired PR panel falls back to the default", () => {
  const layout: RightPaneLayout = {
    sections: [
      {
        id: "rps-old",
        group: { panelIds: [RETIRED_PRS_PANEL_ID], activePanelId: RETIRED_PRS_PANEL_ID },
        collapsed: false,
        size: 1,
      },
    ],
  };
  expect(reconcileLayoutToBuiltins(layout)).toEqual(defaultRightPaneLayout());
});

describe("resizePairAt", () => {
  const sec = (id: string, collapsed: boolean) => ({
    id,
    group: { panelIds: [id], activePanelId: id },
    collapsed,
    size: 0.5,
  });

  it("pairs adjacent expanded sections", () => {
    const s = [sec("a", false), sec("b", false)];
    expect(resizePairAt(s, 0)).toEqual({ above: s[0], below: s[1] });
    expect(resizePairAt(s, 1)).toBeNull();
  });

  it("reaches across collapsed sections to the nearest expanded neighbours", () => {
    const s = [sec("a", false), sec("b", true), sec("c", true), sec("d", false)];
    for (const i of [0, 1, 2]) {
      expect(resizePairAt(s, i)).toEqual({ above: s[0], below: s[3] });
    }
  });

  it("is null when one side has no expanded section", () => {
    const s = [sec("a", true), sec("b", false), sec("c", true)];
    expect(resizePairAt(s, 0)).toBeNull();
    expect(resizePairAt(s, 1)).toBeNull();
  });
});

it("rewrites a saved classic Inbox before pruning, preserving its section and tab state", () => {
  const saved: RightPaneLayout = {
    sections: [
      {
        id: "notes-first",
        group: { panelIds: [SCRATCH_PANEL_ID], activePanelId: SCRATCH_PANEL_ID },
        size: 0.28,
        collapsed: false,
      },
      {
        id: "mail",
        label: "Messages",
        group: { panelIds: ["pane:acme:dev/board", "weft-board"], activePanelId: "weft-board" },
        size: 0.72,
        collapsed: true,
      },
    ],
  };
  const migrated = reconcileLayoutToBuiltins(
    migrateLegacyRightPaneState({ rightPaneLayout: saved }),
  );
  expect(migrated.sections[1]).toEqual({
    ...saved.sections[1],
    group: {
      panelIds: ["pane:acme:dev/board", INBOX_PANEL_ID],
      activePanelId: INBOX_PANEL_ID,
    },
  });
  expect(BUILTIN_PANEL_IDS.has("weft-board")).toBe(false);
});
