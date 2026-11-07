// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import { listPanes } from "@/tauri/commands";
import { loadSavedInboxPanelIds } from "./inbox-layout-migration";
import {
  INBOX_PANEL_ID,
  migrateLegacyRightPaneState,
  reconcileLayoutToBuiltins,
} from "./right-pane-layout";

vi.mock("@/tauri/commands", () => ({ listPanes: vi.fn() }));
vi.mock("@/shared/stores/org-store", () => ({
  useOrgStore: { getState: () => ({ orgs: [{ slug: "acme", teams: [{ name: "dev" }] }] }) },
}));
beforeEach(() => {
  vi.mocked(listPanes).mockReset();
});
function saved(panelId: string) {
  return {
    sections: [
      {
        id: "notes",
        group: { panelIds: ["scratch"], activePanelId: "scratch" },
        size: 0.23,
        collapsed: false,
      },
      {
        id: "mail",
        label: "Messages",
        group: { panelIds: ["pane:acme:dev/board", panelId], activePanelId: panelId },
        size: 0.77,
        collapsed: true,
      },
    ],
  };
}
it.each(["weft-board", "pane:inbox", "pane:private:acme/inbox", "pane:acme:dev/inbox"])(
  "migrates %s before reconcile without changing section metadata",
  (id) => {
    const layout = saved(id);
    expect(
      reconcileLayoutToBuiltins(migrateLegacyRightPaneState({ rightPaneLayout: layout })),
    ).toEqual({
      sections: [
        layout.sections[0],
        {
          ...layout.sections[1],
          group: {
            panelIds: ["pane:acme:dev/board", INBOX_PANEL_ID],
            activePanelId: INBOX_PANEL_ID,
          },
        },
      ],
    });
  },
);
it("discovers renamed installed Inbox copies while leaving surface-only panes independent", async () => {
  const id = "pane:private:acme/my-copy";
  const layout = saved(id);
  layout.sections[0].group.panelIds.push("pane:private:acme/triage");
  vi.mocked(listPanes).mockResolvedValue({
    contentRoot: "/ctx",
    chain: ["dev"],
    panes: [
      {
        id: "my-copy",
        panelId: id,
        scope: "private",
        title: "Copy",
        path: "/copy.html",
        panesDir: "/",
        dashboardRef: {
          scope: "private",
          org: "acme",
          id: "my-copy",
          installedFrom: { scope: "built-in", org: "acme", id: "inbox" },
        },
      },
      {
        id: "triage",
        panelId: "pane:private:acme/triage",
        scope: "private",
        title: "Triage",
        path: "/triage.html",
        panesDir: "/",
        surface: "human-inbox",
      },
    ],
  });
  const inboxPanelIds = await loadSavedInboxPanelIds(layout);
  const migrated = reconcileLayoutToBuiltins(
    migrateLegacyRightPaneState({ rightPaneLayout: layout, inboxPanelIds }),
  );
  expect(migrated.sections[0]).toEqual(layout.sections[0]);
  expect(migrated.sections[1]).toEqual({
    ...layout.sections[1],
    group: { panelIds: ["pane:acme:dev/board", INBOX_PANEL_ID], activePanelId: INBOX_PANEL_ID },
  });
});
it("keeps the real saved section when an old transient alias was also persisted", () => {
  const layout = saved("pane:acme:dev/inbox");
  layout.sections.unshift({
    id: "alias",
    group: { panelIds: ["pane:inbox"], activePanelId: "pane:inbox" },
    size: 0.4,
    collapsed: false,
  });
  const migrated = migrateLegacyRightPaneState({ rightPaneLayout: layout });
  expect(migrated.sections.map((section) => section.id)).toEqual(["notes", "mail"]);
  expect(migrated.sections[1]).toMatchObject({
    id: "mail",
    label: "Messages",
    size: 0.77,
    collapsed: true,
  });
});
it("retains an opaque saved pane when its installation metadata cannot be read", async () => {
  vi.mocked(listPanes).mockRejectedValue(new Error("offline"));
  const layout = saved("pane:private:acme/my-copy");
  const inboxPanelIds = await loadSavedInboxPanelIds(layout);
  expect(migrateLegacyRightPaneState({ rightPaneLayout: layout, inboxPanelIds })).toEqual(layout);
});
