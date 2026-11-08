// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import { declaresSlot, revealSlot, resolveSlot } from "./reveal-slot";
import { paneDefinitions } from "../right-pane/panes/use-register-pane-panels";
import { useOrgStore } from "@/shared/stores/org-store";
import { usePaneStore } from "../right-pane/panes/pane-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  setSurfaceDashboard,
  useDashboardProfileStore,
} from "@/features/dashboard/lib/dashboard-profile";
import {
  defaultRightPaneLayout,
  INBOX_PANEL_ID,
  SCRATCH_PANEL_ID,
} from "../right-pane/right-pane-layout";
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  readDashboardProfile: vi.fn(async () => null),
  writeDashboardProfile: vi.fn(async (_org, profile) => profile),
}));
const ref = { scope: "team" as const, org: "acme", team: "dev", id: "inbox" };
beforeEach(() => {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "dev" });
  usePaneStore.setState({
    teamKey: "acme:dev",
    listing: {
      contentRoot: "/ctx",
      chain: ["dev"],
      panes: [
        {
          id: "inbox",
          title: "Triage",
          scope: "team",
          team: "dev",
          surface: "human-inbox",
          path: "/ctx/panes/inbox.html",
          panesDir: "/ctx/panes",
          panelId: "pane:acme:dev/inbox",
          dashboardRef: ref,
          status: "rejected",
          rejectedReason: "unknown capability",
        },
      ],
    },
    pendingIntents: {},
    knownEntries: {},
  });
  useDashboardProfileStore.setState({
    org: "acme",
    profile: {
      id: "default",
      name: "Default",
      posture: "",
      slots: { "right-pane": [ref] },
      cliLayout: "default",
      chrome: {},
    },
  });
  useViewerStore.setState({ rightPaneOpen: false, rightPaneLayout: defaultRightPaneLayout() });
});
it("reveals the chosen rejected dashboard and delivers the intent so its fallback can use it", () => {
  revealSlot("right-pane", { surface: "inbox", intent: { conversation: "#qa" } });
  expect(useViewerStore.getState().rightPaneOpen).toBe(true);
  expect(usePaneStore.getState().pendingIntents[INBOX_PANEL_ID]).toEqual({
    conversation: "#qa",
    focusComposer: true,
    focusList: false,
  });
});
it("opens the listed Inbox for a legacy choice and keeps native Notes", () => {
  useDashboardProfileStore.setState({
    profile: { ...useDashboardProfileStore.getState().profile!, slots: { "right-pane": "native" } },
  });
  revealSlot("right-pane", { surface: "inbox" });
  expect(useViewerStore.getState().rightPaneOpen).toBe(true);
  revealSlot("right-pane", { surface: "inbox" });
  expect(useViewerStore.getState().rightPaneOpen).toBe(false);
  revealSlot("right-pane", { surface: "notes" });
  expect(
    useViewerStore
      .getState()
      .rightPaneLayout.sections.some((s) => s.group.activePanelId === SCRATCH_PANEL_ID),
  ).toBe(true);
});
it("explicit dashboard wins over the profile and stale team listings cannot select a dashboard", () => {
  expect(
    resolveSlot("right-pane", { dashboard: "pane:acme:dev/other", surface: "inbox" }).panelId,
  ).toBe("pane:acme:dev/other");
  usePaneStore.setState({ teamKey: "acme:old" });
  expect(resolveSlot("right-pane", { surface: "inbox" }).dashboard).toBeUndefined();
});

it("keeps a selected arbitrary-id dashboard and its surface after a normal deletion", () => {
  const current = usePaneStore.getState().listing!;
  const pane = {
    ...current.panes[0],
    id: "triage",
    status: "ready" as const,
    panelId: "pane:acme:dev/triage",
    dashboardRef: { ...ref, id: "triage" },
  };
  usePaneStore.getState().setListing("acme:dev", { ...current, panes: [pane] });
  useDashboardProfileStore.setState({
    profile: {
      ...useDashboardProfileStore.getState().profile!,
      slots: { "right-pane": [pane.dashboardRef] },
    },
  });
  usePaneStore.getState().setListing("acme:dev", { ...current, panes: [] });
  expect(resolveSlot("right-pane", { surface: "inbox" }).dashboard?.panelId).toBe(pane.panelId);
  revealSlot("right-pane", { surface: "inbox" });
  expect(
    useViewerStore
      .getState()
      .rightPaneLayout.sections.some((s) => s.group.activePanelId === INBOX_PANEL_ID),
  ).toBe(true);
});

it("keeps surface defaults with unresolved choices and warns once for missing Notes", () => {
  const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
  const choices = ["missing-inbox", "missing-notes"].map((id) => ({ ...ref, id }));
  useDashboardProfileStore.setState({
    profile: { ...useDashboardProfileStore.getState().profile!, slots: { "right-pane": choices } },
  });
  expect(resolveSlot("right-pane", { surface: "inbox" }).dashboard?.dashboardRef).toEqual(ref);
  expect(resolveSlot("right-pane", { surface: "notes" }).dashboard).toBeUndefined();
  resolveSlot("right-pane", { surface: "notes" });
  expect(warn).toHaveBeenCalledTimes(1);
  expect(warn.mock.calls[0][0]).toContain("missing-inbox");
  expect(warn.mock.calls[0][0]).toContain("missing-notes");
  warn.mockRestore();
});

it("defaults to the listed inbox only when the right-pane profile has no choice", () => {
  useDashboardProfileStore.setState({ profile: null });
  expect(resolveSlot("right-pane", { surface: "inbox" }).dashboard?.panelId).toBe(
    "pane:acme:dev/inbox",
  );
  useDashboardProfileStore.setState({
    profile: {
      id: "default",
      name: "Default",
      posture: "",
      slots: { "right-pane": "native" },
      cliLayout: "default",
      chrome: {},
    },
  });
  expect(resolveSlot("right-pane", { surface: "inbox" }).dashboard?.panelId).toBe(
    "pane:acme:dev/inbox",
  );
});
it("reveals the dock like the tray's Agents control and focuses the chosen page", async () => {
  const { registerFrame, markFrameReady } =
    await import("@/features/dashboard/lib/frame-lifecycle");
  const revealTerminalInPlace = vi.fn(() => useViewerStore.setState({ terminalVisible: true }));
  useViewerStore.setState({
    layoutMode: "split",
    terminalVisible: false,
    workspacePanelCollapsed: true,
    revealTerminalInPlace,
  } as never);
  revealSlot("dock");
  expect(revealTerminalInPlace).toHaveBeenCalled();
  expect(useViewerStore.getState().workspacePanelCollapsed).toBe(false);
  // A chosen dock dashboard takes focus once its page is ready.
  const agents = { scope: "team" as const, org: "acme", team: "dev", id: "agents" };
  useDashboardProfileStore.setState({
    profile: { ...useDashboardProfileStore.getState().profile!, slots: { dock: agents } },
  });
  const element = document.createElement("iframe");
  const focus = vi.spyOn(element, "focus");
  const unregister = registerFrame({
    frameId: "dock-frame",
    dashboard: "agents",
    panelId: "slot:dock",
    slotId: "dock",
    element,
    post: () => {},
    hide: () => {},
  });
  try {
    revealSlot("dock", { focus: true });
    expect(focus).not.toHaveBeenCalled();
    markFrameReady("dock-frame");
    expect(focus).toHaveBeenCalled();
  } finally {
    unregister();
  }
});
it("focuses the built-in Agents list when no dock dashboard is chosen", () => {
  expect(resolveSlot("dock", { surface: "agent-list" })).toMatchObject({
    dashboard: undefined,
    panelId: null,
    native: { entry: "component", label: "Agents list" },
  });
  useViewerStore.setState({
    layoutMode: "split",
    terminalVisible: true,
    workspacePanelCollapsed: false,
  } as never);
  const list = document.createElement("div");
  list.id = "agents-list-acme_dev";
  list.innerHTML = `<div class="ag-scroll"><button>worker</button></div>`;
  document.body.append(list);
  try {
    revealSlot("dock", { focus: true });
    expect(document.activeElement).toBe(list.querySelector("button"));
  } finally {
    list.remove();
  }
});

const choices = ["implicit", "native", "inbox", "notes", "both"] as const;
it.each(
  choices.flatMap((prior) =>
    ["inbox", "notes"].flatMap((surface) =>
      [true, false].map((enabled) => ({ prior, surface, enabled })),
    ),
  ),
)(
  "keeps surfaces independent through $prior legacy $surface changes enabled=$enabled",
  async ({ prior, surface, enabled }) => {
    const notes = { ...ref, id: "journal" };
    const initial = usePaneStore.getState().listing!;
    usePaneStore.setState({
      listing: {
        ...initial,
        panes: [
          { ...initial.panes[0], status: "ready" },
          {
            ...initial.panes[0],
            id: "journal",
            surface: "notes",
            panelId: "pane:acme:dev/journal",
            dashboardRef: notes,
            status: "ready",
          },
        ],
      },
    });
    const choice =
      prior === "implicit"
        ? undefined
        : prior === "native"
          ? "native"
          : prior === "both"
            ? [ref, notes]
            : prior === "inbox"
              ? [ref]
              : [notes];
    useDashboardProfileStore.setState({
      profile: {
        ...useDashboardProfileStore.getState().profile!,
        slots: choice === undefined ? {} : { "right-pane": choice },
      },
    });
    const other = surface === "inbox" ? "notes" : "inbox";
    const before = resolveSlot("right-pane", { surface: other }).panelId;
    await setSurfaceDashboard("acme", surface, surface === "inbox" ? ref : notes, enabled);
    expect(resolveSlot("right-pane", { surface: other }).panelId).toBe(before);
    expect(!!resolveSlot("right-pane", { surface }).dashboard).toBe(surface === "inbox" || enabled);
    await setSurfaceDashboard("acme", surface, surface === "inbox" ? ref : notes, !enabled);
    expect(resolveSlot("right-pane", { surface: other }).panelId).toBe(before);
    expect(!!resolveSlot("right-pane", { surface }).dashboard).toBe(
      surface === "inbox" || !enabled,
    );
  },
);

it.each([undefined, "native" as const])("keeps Notes classic with choice %s in CP1", (choice) => {
  const initial = usePaneStore.getState().listing!;
  usePaneStore.setState({
    listing: {
      ...initial,
      panes: [
        ...initial.panes,
        {
          ...initial.panes[0],
          id: "notes",
          surface: "notes",
          panelId: "pane:acme:dev/notes",
          dashboardRef: { ...ref, id: "notes" },
        },
      ],
    },
  });
  useDashboardProfileStore.setState({
    profile: {
      ...useDashboardProfileStore.getState().profile!,
      slots: choice ? { "right-pane": choice } : {},
    },
  });
  expect(resolveSlot("right-pane", { surface: "notes" }).panelId).toBe(SCRATCH_PANEL_ID);
});

it("reveals file-nav through its own collapse state without opening the right pane", () => {
  useViewerStore.setState({ navCollapsed: { "acme:dev": true }, rightPaneOpen: false });
  revealSlot("file-nav");
  expect(useViewerStore.getState().navCollapsed["acme:dev"]).toBe(false);
  expect(useViewerStore.getState().rightPaneOpen).toBe(false);
});
it("offers a right-pane panel only for panes that declare the right pane (or declare nothing)", () => {
  const pane = (id: string, slots?: string[]) =>
    ({
      id,
      title: id,
      scope: "team",
      team: "dev",
      path: `/p/${id}.html`,
      panesDir: "/p",
      panelId: `pane:acme:dev/${id}`,
      declaration: slots ? { slots, capabilities: [] } : undefined,
    }) as never;
  const defs = paneDefinitions(
    "acme",
    "dev",
    {
      contentRoot: "/ctx",
      chain: [],
      panes: [pane("board", ["right-pane", "dock"]), pane("agent-list", ["dock"]), pane("plain")],
    },
    defaultRightPaneLayout(),
  );
  expect(defs.map((d) => d.id)).toEqual(["pane:acme:dev/board", "pane:acme:dev/plain"]);
});

it("never offers a Notes pane beside classic Notes: Settings chooses between them", () => {
  const notes = {
    id: "notes",
    title: "Notes",
    scope: "private",
    path: "/builtin/notes.html",
    panesDir: "/builtin",
    panelId: "pane:private:acme/notes",
    surface: "notes",
    dashboardRef: { scope: "built-in", org: "acme", id: "notes" },
    declaration: { slots: ["right-pane"], surface: "notes", capabilities: [] },
  } as never;
  const defs = paneDefinitions(
    "acme",
    "dev",
    { contentRoot: "/ctx", chain: [], panes: [notes] },
    defaultRightPaneLayout(),
  );
  expect(defs).toEqual([
    expect.objectContaining({ id: "pane:private:acme/notes", offerable: false }),
  ]);
});

it.each(["right-pane", "dock", "file-nav"] as const)(
  "tab-only built-ins are ineligible for %s, including saved placements",
  (slot) => {
    const pane = {
      id: "tab-only",
      title: "Tab only",
      scope: "private",
      path: "/builtin/tab-only.html",
      panesDir: "/builtin",
      panelId: "pane:private:acme/tab-only",
      dashboardRef: { scope: "built-in", org: "acme", id: "tab-only" },
      declaration: { slots: [], capabilities: [] },
    } as const;
    expect(declaresSlot(pane as never, slot)).toBe(false);
    const layout = {
      sections: [
        {
          id: "saved",
          group: { panelIds: [pane.panelId], activePanelId: pane.panelId },
          collapsed: false,
          size: 1,
        },
      ],
    };
    usePaneStore.setState({ knownEntries: { [pane.panelId]: pane as never } });
    expect(
      paneDefinitions(
        "acme",
        "dev",
        { contentRoot: "/ctx", chain: [], panes: [pane as never] },
        layout,
      ).map((row) => row.id),
    ).not.toContain(pane.panelId);
  },
);
