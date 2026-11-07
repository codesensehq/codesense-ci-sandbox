// SPDX-License-Identifier: MIT
import { dashboardSelected } from "@/features/shell/slots/reveal-slot";
import { beforeEach, expect, it, vi } from "vitest";

const identity = vi.hoisted(() => ({ principal: "42" }));
vi.mock("@/shared/lib/get-current-user", async (original) => ({
  ...(await original<typeof import("@/shared/lib/get-current-user")>()),
  getCurrentUser: () => ({ principal: identity.principal }),
}));
const api = vi.hoisted(() => ({ read: vi.fn(), write: vi.fn() }));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  writeDashboardProfile: api.write,
}));
import {
  setSurfaceDashboard,
  setSlotDashboard,
  useDashboardProfileStore,
} from "./dashboard-profile";

import { serializeAppStateWrite } from "@/shared/lib/app-state-write";
import { useOrgStore } from "@/shared/stores/org-store";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";

const org = "acme/repo";
const inbox = { scope: "team" as const, org, team: "dev", id: "inbox" };
function listing(team = "dev") {
  return {
    contentRoot: "/ctx",
    chain: [team],
    panes: ["inbox", "notes"].map((id) => ({
      id,
      title: id,
      scope: "team" as const,
      team,
      path: `/ctx/${team}/${id}.html`,
      panesDir: "/ctx",
      panelId: `${team}/${id}`,
      surface: id,
      dashboardRef: { ...inbox, team, id },
    })),
  };
}
beforeEach(() => {
  vi.clearAllMocks();
  identity.principal = "42";
  usePaneStore.setState({
    teamKey: `${org}:dev`,
    listing: listing(),
    knownEntries: {},
    refresh: null,
  });
  useOrgStore.setState({ activeOrgSlug: org, activeTeamName: "dev" });
  useDashboardProfileStore.setState({ org: null, profile: null });
  api.read.mockResolvedValue(null);
  api.write.mockImplementation(async (_org, profile) => profile);
});

it("does not let a late local write response replace a newer org view", async () => {
  let finish!: (profile: null) => void;
  api.write.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const saving = setSlotDashboard(org, "dock", "native");
  await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
  useDashboardProfileStore.setState({ org: "other/repo", profile: null });
  finish(null);
  await saving;
  expect(useDashboardProfileStore.getState().org).toBe("other/repo");
  expect(useDashboardProfileStore.getState().profile).toBeNull();
});

it.each([true, false])("keeps the implicit Inbox choice when Notes enabled=%s", async (enabled) => {
  useOrgStore.setState({ activeOrgSlug: org, activeTeamName: "dev" });
  const notes = { ...inbox, id: "journal" };
  usePaneStore.setState({
    teamKey: `${org}:dev`,
    listing: {
      contentRoot: "/ctx",
      chain: [],
      panes: [inbox, notes].map((ref, i) => ({
        id: ref.id,
        title: ref.id,
        scope: ref.scope,
        team: "dev",
        path: `/ctx/${ref.id}.html`,
        panesDir: "/ctx",
        panelId: ref.id,
        dashboardRef: ref,
        surface: i ? "notes" : "human-inbox",
      })),
    },
  });
  expect(dashboardSelected(inbox)).toBe(true);
  expect(dashboardSelected(notes)).toBe(false);
  await setSurfaceDashboard(org, "notes", notes, enabled);
  expect(dashboardSelected(inbox)).toBe(true);
  expect(dashboardSelected(notes)).toBe(enabled);
  await setSurfaceDashboard(org, "notes", notes, !enabled);
  expect(dashboardSelected(inbox)).toBe(true);
  expect(dashboardSelected(notes)).toBe(!enabled);
});

it("keeps each team's implicit Inbox after a Notes choice and a team switch", async () => {
  const notes = { ...inbox, id: "notes" };
  useOrgStore.setState({ activeOrgSlug: org, activeTeamName: "dev" });
  usePaneStore.setState({ teamKey: `${org}:dev`, listing: listing("dev") });
  await setSurfaceDashboard(org, "notes", notes, true);
  useOrgStore.setState({ activeTeamName: "other" });
  usePaneStore.setState({ teamKey: `${org}:other`, listing: listing("other") });
  expect(dashboardSelected({ ...inbox, team: "other" })).toBe(true);
  expect(useDashboardProfileStore.getState().profile?.slots["right-pane"]).toBeUndefined();
});

it.each(["team", "org"])(
  "persists an accepted surface choice for its captured owner after a %s switch",
  async (change) => {
    let release!: () => void;
    const blocking = serializeAppStateWrite(
      () =>
        new Promise<void>((resolve) => {
          release = resolve;
        }),
    );
    await Promise.resolve();
    const write = setSurfaceDashboard(org, "notes", { ...inbox, id: "notes" }, true);
    useOrgStore.setState({
      activeTeamName: "other",
      ...(change === "org" ? { activeOrgSlug: "other/repo" } : {}),
    });
    if (change === "org") useDashboardProfileStore.setState({ org: "other/repo", profile: null });
    release();
    await blocking;
    await write;
    expect(api.write).toHaveBeenCalledWith(
      org,
      { rightPaneSurfaces: { notes: { ...inbox, id: "notes" } } },
      "42",
    );
    if (change === "org") expect(useDashboardProfileStore.getState().org).toBe("other/repo");
  },
);

it("publishes a saved surface choice even if the optional pane refresh rejects", async () => {
  const notes = { ...inbox, id: "notes" };
  const refresh = vi.fn().mockRejectedValue(new Error("Cannot refresh"));
  const log = vi.spyOn(console, "error").mockImplementation(() => {});
  usePaneStore.setState({ refresh });
  try {
    await setSurfaceDashboard(org, "notes", notes, true);
    expect(refresh).toHaveBeenCalledOnce();
    expect(useDashboardProfileStore.getState().profile?.rightPaneSurfaces?.notes).toEqual(notes);
  } finally {
    log.mockRestore();
    usePaneStore.setState({ refresh: null });
  }
});

it("keeps the Inbox assignment while switching the dock to its generic native default", async () => {
  await setSlotDashboard(org, "right-pane", inbox);
  await setSlotDashboard(org, "dock", "native");
  expect(useDashboardProfileStore.getState().profile?.slots).toEqual({
    "right-pane": inbox,
    dock: "native",
  });
});

it.each(["org", "principal"])(
  "keeps a queued slot edit owned across a %s switch",
  async (boundary) => {
    let release!: () => void;
    const blocking = serializeAppStateWrite(
      () =>
        new Promise<void>((resolve) => {
          release = resolve;
        }),
    );
    await Promise.resolve();
    const saving = setSlotDashboard(org, "dock", "native");
    useOrgStore.setState({ activeOrgSlug: "other/repo" });
    useDashboardProfileStore.setState({ org: "other/repo", profile: null });
    if (boundary === "principal") identity.principal = "99";
    release();
    await blocking;
    await saving;
    if (boundary === "org")
      expect(api.write).toHaveBeenCalledWith(org, { slots: { dock: "native" } }, "42");
    else expect(api.write).not.toHaveBeenCalled();
    expect(useDashboardProfileStore.getState()).toEqual({
      org: "other/repo",
      profile: null,
      warm: {},
    });
  },
);

it("rejects a surface choice from a stale team listing before enqueueing", async () => {
  usePaneStore.setState({ teamKey: `${org}:other` });
  await setSurfaceDashboard(org, "notes", { ...inbox, id: "notes" }, true);
  expect(api.write).not.toHaveBeenCalled();
});
