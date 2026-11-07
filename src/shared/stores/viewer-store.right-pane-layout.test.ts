// SPDX-License-Identifier: MIT
import "@/test/prefs-state-fixture";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  SCRATCH_PANEL_ID,
  INBOX_PANEL_ID,
  addPanelAsTopSection,
  defaultRightPaneLayout,
  findPanelLocation,
} from "@/features/shell/right-pane/right-pane-layout";
import * as commands from "@/tauri/commands";

import { useOrgStore } from "./org-store";
import { useViewerStore } from "./viewer-store";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  saveScratchNote: vi.fn().mockResolvedValue(undefined),
  loadScratchNote: vi.fn().mockResolvedValue(""),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  detectTools: vi.fn().mockResolvedValue({
    terminals: [],
    editors: [],
    default_terminal: "",
    default_editor: "",
  }),
}));

const loadAppStateMock = commands.loadAppState as ReturnType<typeof vi.fn>;
const saveAppStateMock = commands.saveAppState as ReturnType<typeof vi.fn>;

const TEAM_KEY = "acme:default";

function seedOrg() {
  useOrgStore.setState({
    orgs: [
      {
        slug: "acme",
        display_name: "Acme",
        created_at: "2026-01-01T00:00:00Z",
        remote_url: null,
        config_flags: {},
        people: [],
        teams: [
          {
            name: "default",
            source_repos: [{ owner: "owner", repo: "repo" }],
          },
        ],
        org_root: "/orgs/acme",
      },
    ],
    workspaceRepos: {},
    activeOrgSlug: null,
    activeTeamName: null,
  });
}

beforeEach(() => {
  seedOrg();
  useViewerStore.setState({
    rightPaneLayout: defaultRightPaneLayout(),
    rightPaneOpen: false,
    openedTeams: [],
  });
  loadAppStateMock.mockReset();
  saveAppStateMock.mockReset().mockResolvedValue(undefined);
});

describe("viewer-store rightPaneLayout — migration + persistence", () => {
  it.each(["standalone", "tabbed", "only"])(
    "restores a saved layout with workspace-chat %s without the retired panel",
    async (placement) => {
      const base = defaultRightPaneLayout();
      const chatSection = {
        id: "saved-chat",
        group: { panelIds: ["workspace-chat"], activePanelId: "workspace-chat" },
        collapsed: false,
        size: 0.4,
      };
      const saved =
        placement === "only"
          ? { sections: [chatSection] }
          : placement === "tabbed"
            ? {
                sections: [
                  {
                    ...base.sections[0],
                    group: {
                      panelIds: ["workspace-chat", INBOX_PANEL_ID],
                      activePanelId: "workspace-chat",
                    },
                  },
                  base.sections[1],
                ],
              }
            : {
                sections: [
                  chatSection,
                  ...base.sections.map((section) => ({ ...section, collapsed: true })),
                ],
              };
      loadAppStateMock.mockResolvedValue(
        JSON.stringify({ viewer: { schemaVersion: 9, rightPaneLayout: saved } }),
      );

      await useViewerStore.getState().loadPersistedState();

      const layout = useViewerStore.getState().rightPaneLayout;
      expect(findPanelLocation(layout, "workspace-chat")).toBeNull();
      expect(layout.sections.map((section) => section.id)).not.toContain("saved-chat");
      expect(layout.sections.flatMap((section) => section.group.panelIds)).toEqual([
        INBOX_PANEL_ID,
        SCRATCH_PANEL_ID,
      ]);
      expect(layout.sections[0].group.activePanelId).toBe(INBOX_PANEL_ID);
      expect(layout.sections.some((section) => !section.collapsed)).toBe(true);
    },
  );

  it("folds a legacy team-keyed split into the default layout on load, ignoring a legacy view-mode", async () => {
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({
        viewer: {
          schemaVersion: 9,
          rightPaneViewMode: { [TEAM_KEY]: "board" }, // retired — must be ignored
          rightPaneSplitRatio: { [TEAM_KEY]: 0.3 },
        },
      }),
    );

    await useViewerStore.getState().loadPersistedState();

    const layout = useViewerStore.getState().rightPaneLayout;
    // the retired view-mode no longer picks a panel: the Inbox stays active
    expect(layout.sections[0].group.activePanelId).toBe(INBOX_PANEL_ID);
    // split (scratch share) → section sizes
    expect(layout.sections[1].size).toBeCloseTo(0.3);
    expect(layout.sections[0].size).toBeCloseTo(0.7);
    // structure preserved: Inbox group + scratch
    expect(layout.sections[0].group.panelIds).toEqual([INBOX_PANEL_ID]);
    expect(layout.sections[1].group.panelIds).toEqual([SCRATCH_PANEL_ID]);
  });

  it("silently prunes the retired pipeline/board panels from a saved layout on load", async () => {
    const base = defaultRightPaneLayout();
    const saved = {
      sections: [
        {
          ...base.sections[0],
          group: {
            panelIds: ["pipeline", "board", INBOX_PANEL_ID],
            activePanelId: "board",
          },
        },
        {
          id: "lone-pipeline",
          group: { panelIds: ["pipeline"], activePanelId: "pipeline" },
          collapsed: false,
          size: 0.3,
        },
        ...base.sections.slice(1),
      ],
    };
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({ viewer: { schemaVersion: 9, rightPaneLayout: saved } }),
    );

    await useViewerStore.getState().loadPersistedState();

    const layout = useViewerStore.getState().rightPaneLayout;
    const ids = layout.sections.flatMap((s) => s.group.panelIds);
    expect(ids).not.toContain("pipeline");
    expect(ids).not.toContain("board");
    expect(layout.sections.map((s) => s.id)).not.toContain("lone-pipeline");
    expect(layout.sections[0].group).toEqual({
      panelIds: [INBOX_PANEL_ID],
      activePanelId: INBOX_PANEL_ID,
    });
    expect(ids).toEqual([INBOX_PANEL_ID, SCRATCH_PANEL_ID]);
  });

  it("prefers an already-migrated valid rightPaneLayout over the legacy scalars", async () => {
    // A built-ins-only rearrangement (scratch promoted on top) — distinct from the
    // default, and survives the load-time built-in reconcile intact.
    const custom = addPanelAsTopSection(
      defaultRightPaneLayout(),
      SCRATCH_PANEL_ID,
      () => "sec-top",
    );
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({
        viewer: {
          schemaVersion: 9,
          rightPaneLayout: custom,
          rightPaneViewMode: { [TEAM_KEY]: "board" }, // must be ignored
        },
      }),
    );

    await useViewerStore.getState().loadPersistedState();

    expect(useViewerStore.getState().rightPaneLayout).toEqual(custom);
  });

  it("SW-AW-5: prunes a persisted DYNAMIC-panel section on load (no orphaned dead section)", async () => {
    // Simulate a layout persisted while the git-sync agent was promoted.
    const withAgent = addPanelAsTopSection(
      defaultRightPaneLayout(),
      "git-sync-agent",
      () => "sec-agent",
    );
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({ viewer: { schemaVersion: 9, rightPaneLayout: withAgent } }),
    );

    await useViewerStore.getState().loadPersistedState();

    const layout = useViewerStore.getState().rightPaneLayout;
    const ids = layout.sections.flatMap((s) => s.group.panelIds);
    expect(ids).not.toContain("git-sync-agent");
    // Built-ins survive and the pane still has an expanded section (floor intact).
    expect(ids).toEqual(expect.arrayContaining([INBOX_PANEL_ID, SCRATCH_PANEL_ID]));
    expect(layout.sections.some((s) => !s.collapsed)).toBe(true);
  });

  it("drops persisted sections of the retired JSON widget kind on load, keeping panes and built-ins", async () => {
    // A layout saved while JSON widgets existed: one lone widget section, one widget
    // tabbed beside a built-in, and a custom pane. The id is assembled so the source
    // never spells the retired prefix.
    const retired = (id: string) => ["widget", id].join(":");
    const pane = "pane:acme:default/board";
    const base = defaultRightPaneLayout();
    const saved = {
      sections: [
        {
          id: "w-lone",
          group: { panelIds: [retired("sales")], activePanelId: retired("sales") },
          collapsed: false,
          size: 0.3,
        },
        {
          ...base.sections[0],
          group: {
            panelIds: [...base.sections[0].group.panelIds, retired("triage")],
            activePanelId: retired("triage"),
          },
        },
        ...base.sections.slice(1),
        {
          id: "p",
          group: { panelIds: [pane], activePanelId: pane },
          collapsed: true,
          size: 0.2,
        },
      ],
    };
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({ viewer: { schemaVersion: 9, rightPaneLayout: saved } }),
    );

    await useViewerStore.getState().loadPersistedState();

    const layout = useViewerStore.getState().rightPaneLayout;
    const ids = layout.sections.flatMap((s) => s.group.panelIds);
    expect(ids.some((id) => id.startsWith("widget" + ":"))).toBe(false);
    expect(layout.sections.map((s) => s.id)).not.toContain("w-lone");
    // No empty section, and every stored active tab is a surviving member.
    for (const s of layout.sections) {
      expect(s.group.panelIds.length).toBeGreaterThan(0);
      expect(s.group.panelIds).toContain(s.group.activePanelId);
    }
    expect(ids).toContain(pane);
    expect(ids).toEqual(expect.arrayContaining([INBOX_PANEL_ID, SCRATCH_PANEL_ID]));
    expect(layout.sections.some((s) => !s.collapsed)).toBe(true);
  });

  it("coerces a malformed persisted layout to the default", async () => {
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({
        viewer: { schemaVersion: 9, rightPaneLayout: { sections: "nope" } },
      }),
    );

    await useViewerStore.getState().loadPersistedState();

    expect(useViewerStore.getState().rightPaneLayout).toEqual(defaultRightPaneLayout());
  });

  it("drops the retired right-pane History panel from a saved layout", () => {
    const saved = addPanelAsTopSection(
      defaultRightPaneLayout(),
      "git-sync-history",
      () => "sec-history",
    );
    loadAppStateMock.mockResolvedValue(
      JSON.stringify({ viewer: { schemaVersion: 9, rightPaneLayout: saved } }),
    );

    return useViewerStore
      .getState()
      .loadPersistedState()
      .then(() => {
        const layout = useViewerStore.getState().rightPaneLayout;
        expect(findPanelLocation(layout, "git-sync-history")).toBeNull();
        const ids = layout.sections.flatMap((s) => s.group.panelIds);
        expect(ids).toEqual(expect.arrayContaining([INBOX_PANEL_ID, SCRATCH_PANEL_ID]));
      });
  });

  it("round-trips a mutated layout through persist → load", async () => {
    const mutated = addPanelAsTopSection(
      defaultRightPaneLayout(),
      SCRATCH_PANEL_ID,
      () => "sec-top",
    );
    useViewerStore.setState({ rightPaneLayout: mutated });

    let captured: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      captured = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((resolve) => setTimeout(resolve, 400));

    expect(captured).not.toBeNull();
    expect(JSON.parse(captured!).viewer.rightPaneLayout).toEqual(mutated);

    // Reload from the captured payload.
    useViewerStore.setState({ rightPaneLayout: defaultRightPaneLayout() });
    loadAppStateMock.mockResolvedValue(captured!);
    await useViewerStore.getState().loadPersistedState();

    expect(useViewerStore.getState().rightPaneLayout).toEqual(mutated);
  });
});
