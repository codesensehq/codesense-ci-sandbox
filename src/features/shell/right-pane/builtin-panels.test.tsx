// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";

// Keep importing builtin-panels cheap — stub the heavy editor body so this test
// only exercises the registration metadata (title/id), not the real editor module.
vi.mock("@/features/viewer/components/scratch-note-panel", () => ({
  ScratchNotePanel: () => null,
}));
vi.mock("@/tauri/commands", () => ({
  rebuildMenu: vi.fn().mockResolvedValue(undefined),
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

// Pin the active team key so the menu's mode resolution is deterministic.
const activeTeamKeyRef = vi.hoisted(() => ({ current: "acme:default" }));
vi.mock("@/shared/lib/team-key", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/lib/team-key")>();
  return {
    ...actual,
    useActiveTeamKey: () => activeTeamKeyRef.current,
    getActiveTeamKey: () => activeTeamKeyRef.current,
  };
});

import { registerBuiltinPanels } from "./builtin-panels";
import { buildPanelMenuEntries } from "./panel-menu";
import { getPanel } from "./panel-registry";
import { SCRATCH_PANEL_ID } from "./right-pane-layout";

describe("registerBuiltinPanels (SW-RP-3 rename)", () => {
  it("registers the notes panel under display title 'Notes' with the id still 'scratch'", () => {
    registerBuiltinPanels();

    // AC-5: display title is "Notes"; the persisted id is unchanged.
    expect(SCRATCH_PANEL_ID).toBe("scratch");
    expect(getPanel(SCRATCH_PANEL_ID)?.title).toBe("Notes");

    // The other built-in is untouched.
    expect(getPanel("weft-board")).toBeUndefined();

    // The View → Panels menu derives its label from the title → reads "Notes"
    // for the scratch id, with no menu-code change.
    const entries = buildPanelMenuEntries();
    expect(entries.find((e) => e.id === SCRATCH_PANEL_ID)?.label).toBe("Notes");

    // Notes carries no header actions.
    expect(getPanel(SCRATCH_PANEL_ID)?.headerActions).toBeUndefined();
  });

  it("the retired Pipeline and Board panels are not registered and not in the menu", () => {
    registerBuiltinPanels();

    // A stale saved layout drops these ids via reconcileLayoutToBuiltins (right-pane-layout test).
    for (const id of ["pipeline", "board"]) {
      expect(getPanel(id)).toBeUndefined();
    }
    const entries = buildPanelMenuEntries();
    expect(entries.map((e) => e.id)).not.toContain("pipeline");
    expect(entries.map((e) => e.id)).not.toContain("board");
    for (const label of ["Pipeline", "Board"]) {
      expect(entries.map((e) => e.label)).not.toContain(label);
    }
  });

  it("the right-pane History panel is gone: History lives in the left nav", () => {
    registerBuiltinPanels();
    expect(getPanel("git-sync-history")).toBeUndefined();
    const entries = buildPanelMenuEntries();
    expect(entries.map((e) => e.id)).not.toContain("git-sync-history");
    expect(entries.map((e) => e.label)).not.toContain("History");
  });

  it("SW-AU-56: the pipeline-config panel is fully retired — not registered at all", () => {
    registerBuiltinPanels();

    // AU-56 re-homed the pipeline editor inline into Settings → Pipeline & phases, so the
    // right-pane panel is gone entirely (not merely hidden): no registration, no menu entry, no
    // ghost. A stale saved layout drops the id via reconcileLayoutToBuiltins (right-pane-layout test).
    expect(getPanel("pipeline-config")).toBeUndefined();
    expect(buildPanelMenuEntries().map((e) => e.id)).not.toContain("pipeline-config");
  });

  it("SW-AV-19: the standalone Worktrees tab is fully retired — not registered, not in the menu", () => {
    registerBuiltinPanels();

    // The Worktrees lifecycle folded into the Agents fleet (per-pill worktree-inline +
    // the reused-WorktreesPanel lens), so the standalone tab is gone entirely: no
    // registration, no menu entry, no ghost. A stale saved layout drops the "worktrees"
    // id via reconcileLayoutToBuiltins (covered in the right-pane-layout test).
    expect(getPanel("worktrees")).toBeUndefined();
    expect(buildPanelMenuEntries().map((e) => e.id)).not.toContain("worktrees");
  });

  it("SW-AV-26: the Automations tab is fully retired — not registered, not in the menu", () => {
    registerBuiltinPanels();

    // A stale saved layout drops the "automations" id via reconcileLayoutToBuiltins
    // (covered in the right-pane-layout test).
    expect(getPanel("automations")).toBeUndefined();
    expect(buildPanelMenuEntries().map((e) => e.id)).not.toContain("automations");
  });

  // The right pane is for the user's own surfaces: agents live in the bottom CLI workspace
  // and routines replaced automations, so the Agents, Workflows and Triggers panels are gone.
  it("registers no Agents, Workflows or Triggers panel, and the menu offers none", () => {
    registerBuiltinPanels();

    for (const id of ["running-agents", "workflow-library", "triggers"]) {
      expect(getPanel(id)).toBeUndefined();
    }
    const labels = buildPanelMenuEntries().map((e) => e.label);
    for (const label of ["Agents", "Workflows", "Triggers"]) {
      expect(labels).not.toContain(label);
    }
  });

  it("offers exactly the user surfaces in the viewer-mode menu", () => {
    registerBuiltinPanels();

    const ids = buildPanelMenuEntries().map((e) => e.id);
    expect(ids).toEqual(expect.arrayContaining([SCRATCH_PANEL_ID]));
  });

  // SW-WV-F1 (AC-2 Test A): the Activity board registers as a viewer-mode builtin, appears
  // in the View → Panels menu, and its modes are exactly `["viewer"]`.

  // Service connections are retired: no Connectors / Derived value / PRs panel is registered.
  it("registers no retired service-connection or PR panels", () => {
    registerBuiltinPanels();

    for (const id of ["connectors", "connector-derived", "git-sync-prs"]) {
      expect(getPanel(id)).toBeUndefined();
    }
    const labels = buildPanelMenuEntries().map((e) => e.label);
    expect(labels).not.toContain("Connectors");
  });

  // Agents moved to their own tab: the right pane no longer registers an Agent Profiles panel.
  it("registers no Agent Profiles panel", () => {
    registerBuiltinPanels();

    expect(getPanel("agent-profiles")).toBeUndefined();
    const labels = buildPanelMenuEntries().map((e) => e.label);
    expect(labels).not.toContain("Agent Profiles");
  });
});
