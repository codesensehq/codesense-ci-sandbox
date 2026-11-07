// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";

import { toast } from "sonner";

import { discoverLocalOrgs } from "@/shared/api/org";
import { loadOrg } from "@/shared/api/registry";
import { commitRegistryEdit } from "@/shared/lib/commit-registry-edit";
import { visibleTeams } from "@/shared/lib/user-teams";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { Org, Team } from "@/shared/types/domain";
import {
  ACTIVE_TEAM_SWITCH_EVENT,
  ORG_REGISTRY_CHANGED_EVENT,
  SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT,
  type ActiveTeamSwitchPayload,
  type OrgRegistryChangedPayload,
} from "@/tauri/events";
import {
  ORG_RELOAD_TIMEOUT_MS,
  applyActiveTeamSwitch,
  applySourceAttachmentChange,
  SOURCE_ATTACHMENTS_CHANGED_EVENT,
  applyOrgRegistryChange,
  useOrgRegistrySync,
} from "./use-org-registry-sync";

// Controllable `listen` registry (mirrors use-epic-repos-reassembly.test).
const eventMock = vi.hoisted(() => {
  const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();
  return {
    listeners,
    listen: vi.fn(async (event: string, handler: (e: { payload: unknown }) => void) => {
      let set = listeners.get(event);
      if (!set) {
        set = new Set();
        listeners.set(event, set);
      }
      set.add(handler);
      return () => set!.delete(handler);
    }),
  };
});
vi.mock("@tauri-apps/api/event", () => ({ listen: eventMock.listen }));
vi.mock("@/tauri/commands");
vi.mock("@/shared/lib/local-profile-session", () => ({
  rendererPrincipal: vi.fn(async () => "42"),
}));
vi.mock("@/shared/api/registry", () => ({ loadOrg: vi.fn() }));
vi.mock("@/shared/api/org", () => ({ discoverLocalOrgs: vi.fn(() => Promise.resolve([])) }));
vi.mock("sonner", () => ({ toast: { warning: vi.fn(), error: vi.fn(), success: vi.fn() } }));
vi.mock("@/shared/lib/commit-registry-edit", () => ({
  commitRegistryEdit: vi.fn(() => Promise.resolve()),
}));
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: vi.fn(() => ({
    github_user_id: 42,
    github_username: "dev",
    principal: "42",
  })),
}));

const mockedLoadOrg = vi.mocked(loadOrg);
const mockedDiscover = vi.mocked(discoverLocalOrgs);

type Loaded = Awaited<ReturnType<typeof loadOrg>>;
const loaded = (org: Org | null): Loaded =>
  ({
    slug: org?.slug,
    root: org?.org_root,
    org,
    partial: {},
    file_errors: [],
  }) as unknown as Loaded;

const team = (name: string): Team => ({ name, source_repos: [] });

function makeOrg(slug: string, teams: string[]): Org {
  return {
    slug,
    display_name: slug,
    created_at: "2026-01-01T00:00:00Z",
    org_root: `/orgs/${slug}`,
    remote_url: null,
    config_flags: {},
    teams: teams.map(team),
    people: [],
  };
}

/** The org as the next `loadOrg` reads it from disk. */
function diskHas(org: Org) {
  mockedLoadOrg.mockResolvedValue({
    slug: org.slug,
    root: org.org_root,
    org,
    partial: {},
    file_errors: [],
  } as unknown as Awaited<ReturnType<typeof loadOrg>>);
}

function emit(event: string, payload: unknown) {
  for (const handler of [...(eventMock.listeners.get(event) ?? [])]) handler({ payload });
}

beforeEach(() => {
  eventMock.listeners.clear();
  vi.clearAllMocks();
  useOrgStore.setState({
    orgs: [makeOrg("acme/ctx", ["default", "payments"]), makeOrg("beta/ctx", ["ops"])],
    activeOrgSlug: "acme/ctx",
    activeTeamName: "payments",
    lastTeamByOrgSlug: { "acme/ctx": "payments" },
    shownTeamsByOrgSlug: {},
    suspendedWorkspaceRepos: {},
    recentlyRemovedRepos: {},
    disabledOrgSlugs: [],
    stateLoaded: false,
  });
  useViewerStore.setState({
    openedTeams: ["acme/ctx:payments"],
    maximizedPanel: { "acme/ctx:payments": "weft-board" },
  });
});

describe("useOrgRegistrySync — org-registry-changed", () => {
  it("reloads the changed org from disk and shows a newly created team", async () => {
    useOrgStore.setState({ shownTeamsByOrgSlug: { "acme/ctx": ["default", "payments"] } });
    diskHas(makeOrg("acme/ctx", ["default", "payments", "search"]));
    renderHook(() => useOrgRegistrySync());

    emit(ORG_REGISTRY_CHANGED_EVENT, {
      orgSlug: "acme/ctx",
      principal: "42",
      edit: "CreateTeam",
      team: "search",
    } satisfies OrgRegistryChangedPayload);
    await vi.waitFor(() => expect(useOrgStore.getState().orgs[0].teams).toHaveLength(3));

    expect(mockedLoadOrg).toHaveBeenCalledWith("acme/ctx", "/orgs/acme/ctx", null, "42");
    const state = useOrgStore.getState();
    expect(state.orgs[0].teams.map((t) => t.name)).toEqual(["default", "payments", "search"]);
    // An agent's create doesn't steal focus, but the team must not be hidden.
    expect(state.activeTeamName).toBe("payments");
    expect(state.shownTeamsByOrgSlug["acme/ctx"]).toBeUndefined(); // collapsed to show-all
  });

  it("ignores a completed registry rename from another principal before reading or rekeying", async () => {
    diskHas(makeOrg("acme/ctx", ["default", "billing"]));
    const orgBefore = useOrgStore.getState().orgs;
    const viewerBefore = useViewerStore.getState().openedTeams;
    await applyOrgRegistryChange({
      principal: "41",
      orgSlug: "acme/ctx",
      edit: "RenameTeam",
      team: "billing",
      previousTeam: "payments",
    });
    expect(mockedLoadOrg).not.toHaveBeenCalled();
    expect(useOrgStore.getState().orgs).toBe(orgBefore);
    expect(useOrgStore.getState().activeTeamName).toBe("payments");
    expect(useViewerStore.getState().openedTeams).toBe(viewerBefore);
    expect(useViewerStore.getState().maximizedPanel["acme/ctx:payments"]).toBe("weft-board");
  });

  it("carries a rename of the active team over to team-keyed state", async () => {
    diskHas(makeOrg("acme/ctx", ["default", "billing"]));
    renderHook(() => useOrgRegistrySync());

    emit(ORG_REGISTRY_CHANGED_EVENT, {
      orgSlug: "acme/ctx",
      principal: "42",
      edit: "RenameTeam",
      team: "billing",
      previousTeam: "payments",
    } satisfies OrgRegistryChangedPayload);
    await vi.waitFor(() => expect(useOrgStore.getState().activeTeamName).toBe("billing"));

    const org = useOrgStore.getState();
    expect(org.activeTeamName).toBe("billing");
    expect(org.lastTeamByOrgSlug["acme/ctx"]).toBe("billing");
    const viewer = useViewerStore.getState();
    expect(viewer.openedTeams).toContain("acme/ctx:billing");
    expect(viewer.maximizedPanel["acme/ctx:billing"]).toBe("weft-board");
    expect(viewer.maximizedPanel["acme/ctx:payments"]).toBeUndefined();
  });

  it("falls the active team back when it is deleted, like the UI delete path", async () => {
    diskHas(makeOrg("acme/ctx", ["default"]));
    renderHook(() => useOrgRegistrySync());

    emit(ORG_REGISTRY_CHANGED_EVENT, {
      orgSlug: "acme/ctx",
      principal: "42",
      edit: "DeleteTeam",
      team: "payments",
    } satisfies OrgRegistryChangedPayload);
    await vi.waitFor(() =>
      expect(useViewerStore.getState().maximizedPanel["acme/ctx:payments"]).toBeUndefined(),
    );

    const state = useOrgStore.getState();
    expect(state.orgs[0].teams.map((t) => t.name)).toEqual(["default"]);
    expect(state.activeTeamName).toBe("default");
    expect(state.lastTeamByOrgSlug["acme/ctx"]).not.toBe("payments");
    expect(useViewerStore.getState().maximizedPanel["acme/ctx:payments"]).toBeUndefined();
  });

  it("does not re-apply a rename the UI already applied", async () => {
    const rekey = vi.spyOn(useViewerStore.getState(), "rekeyTeam");
    await useOrgStore.getState().renameTeam("payments", "billing");
    expect(commitRegistryEdit).toHaveBeenCalledTimes(1);
    expect(rekey).toHaveBeenCalledTimes(1);

    diskHas(makeOrg("acme/ctx", ["default", "billing"]));
    await applyOrgRegistryChange({
      orgSlug: "acme/ctx",
      principal: "42",
      edit: "RenameTeam",
      team: "billing",
      previousTeam: "payments",
    });

    expect(rekey).toHaveBeenCalledTimes(1);
    expect(useOrgStore.getState().activeTeamName).toBe("billing");
    rekey.mockRestore();
  });

  it("reconciles a transient rename into active team and viewer state", async () => {
    mockedLoadOrg.mockResolvedValueOnce({
      ...loaded(null),
      file_errors: [{ kind: "TransientReadError", message: "too many open files; retry" }],
    } as Loaded);
    await applyOrgRegistryChange({
      orgSlug: "acme/ctx",
      principal: "42",
      edit: "RenameTeam",
      team: "billing",
      previousTeam: "payments",
    });
    expect(useOrgStore.getState().activeTeamName).toBe("billing");
    expect(useViewerStore.getState().maximizedPanel["acme/ctx:billing"]).toBe("weft-board");
    expect(useViewerStore.getState().openedTeams).toContain("acme/ctx:billing");
  });

  it("cleans deleted team viewer state despite a transient registry read", async () => {
    mockedLoadOrg.mockResolvedValueOnce({
      ...loaded(null),
      file_errors: [{ kind: "TransientReadError", message: "too many open files; retry" }],
    } as Loaded);
    await applyOrgRegistryChange({
      orgSlug: "acme/ctx",
      principal: "42",
      edit: "DeleteTeam",
      team: "payments",
    });
    expect(useViewerStore.getState().maximizedPanel["acme/ctx:payments"]).toBeUndefined();
  });

  it("warns visibly when a transient read prevents a backend team switch", async () => {
    mockedLoadOrg.mockResolvedValueOnce({
      ...loaded(null),
      file_errors: [{ kind: "TransientReadError", message: "too many open files; retry" }],
    } as Loaded);
    await applyActiveTeamSwitch({ orgSlug: "acme/ctx", team: "billing" });
    expect(toast.warning).toHaveBeenCalledWith(expect.stringContaining("acme/ctx"));
  });

  it("keeps the known org on descriptor exhaustion and reloads it on a later event", async () => {
    mockedLoadOrg.mockResolvedValueOnce({
      ...loaded(null),
      file_errors: [{ kind: "TransientReadError", message: "too many open files; retry" }],
    } as Loaded);
    const change = {
      orgSlug: "acme/ctx",
      principal: "42",
      edit: "ArchiveTeam",
      team: "payments",
    } as const;
    await applyOrgRegistryChange(change);
    expect(useOrgStore.getState().orgs[0].teams.map((t) => t.name)).toContain("payments");
    diskHas(makeOrg("acme/ctx", ["default"]));
    await applyOrgRegistryChange(change);
    expect(useOrgStore.getState().orgs[0].teams.map((t) => t.name)).toEqual(["default"]);
  });

  it("still carries a rename over when the reloaded org does not compose", async () => {
    mockedLoadOrg.mockResolvedValue({
      slug: "acme/ctx",
      root: "/orgs/acme/ctx",
      org: null,
      partial: {},
      file_errors: [{ message: "bad teams.yml" }],
    } as unknown as Awaited<ReturnType<typeof loadOrg>>);
    await applyOrgRegistryChange({
      orgSlug: "acme/ctx",
      principal: "42",
      edit: "RenameTeam",
      team: "billing",
      previousTeam: "payments",
    });
    expect(useOrgStore.getState().activeTeamName).toBe("billing");
    expect(useViewerStore.getState().maximizedPanel["acme/ctx:billing"]).toBe("weft-board");
  });

  it("falls the active team back when an agent archives it", async () => {
    diskHas({
      ...makeOrg("acme/ctx", ["default", "payments"]),
      teams: [team("default"), { ...team("payments"), archived: true }],
    });
    await applyOrgRegistryChange({
      orgSlug: "acme/ctx",
      principal: "42",
      edit: "ArchiveTeam",
      team: "payments",
    });
    expect(useOrgStore.getState().activeTeamName).toBe("default");
  });

  it("ignores an org the store does not list", async () => {
    await applyOrgRegistryChange({
      orgSlug: "zeta/ctx",
      principal: "42",
      edit: "CreateTeam",
      team: "x",
    });
    expect(mockedLoadOrg).not.toHaveBeenCalled();
  });
});

describe("useOrgRegistrySync — mcp:set-active-team", () => {
  it("moves the UI to the team an agent switched to, across orgs", async () => {
    diskHas(makeOrg("beta/ctx", ["ops"]));
    renderHook(() => useOrgRegistrySync());

    emit(ACTIVE_TEAM_SWITCH_EVENT, {
      orgSlug: "beta/ctx",
      team: "ops",
    } satisfies ActiveTeamSwitchPayload);
    await vi.waitFor(() => expect(useOrgStore.getState().activeOrgSlug).toBe("beta/ctx"));

    expect(useOrgStore.getState().activeTeamName).toBe("ops");
    // A listed team needs no reload.
    expect(mockedLoadOrg).not.toHaveBeenCalled();
  });

  it("makes a hidden team visible so the shown-set reconcile keeps it active", async () => {
    useOrgStore.setState({ shownTeamsByOrgSlug: { "acme/ctx": ["payments"] } });
    await applyActiveTeamSwitch({ orgSlug: "acme/ctx", team: "default" });

    const state = useOrgStore.getState();
    expect(state.activeTeamName).toBe("default");
    expect(state.shownTeamsByOrgSlug["acme/ctx"]).toBeUndefined(); // every team shown
  });

  it("re-enables a soft-removed org it switches into", async () => {
    useOrgStore.setState({ disabledOrgSlugs: ["beta/ctx"] });
    await applyActiveTeamSwitch({ orgSlug: "beta/ctx", team: "ops" });
    const state = useOrgStore.getState();
    expect(state.disabledOrgSlugs).not.toContain("beta/ctx");
    expect(state.activeOrgSlug).toBe("beta/ctx");
    expect(state.activeTeamName).toBe("ops");
  });

  it("reloads the org first when the team was just created", async () => {
    diskHas(makeOrg("acme/ctx", ["default", "payments", "search"]));
    await applyActiveTeamSwitch({ orgSlug: "acme/ctx", team: "search" });

    expect(mockedLoadOrg).toHaveBeenCalledTimes(1);
    expect(useOrgStore.getState().activeTeamName).toBe("search");
  });
});

describe("useOrgRegistrySync — review regressions", () => {
  it("a rename in another org never retargets the active team", async () => {
    useOrgStore.setState({
      orgs: [makeOrg("acme/ctx", ["default", "payments"]), makeOrg("beta/ctx", ["default"])],
      activeOrgSlug: "acme/ctx",
      activeTeamName: "default",
      lastTeamByOrgSlug: { "acme/ctx": "default", "beta/ctx": "default" },
    });
    // The reload fails, so nothing re-validates the active team afterwards.
    mockedLoadOrg.mockResolvedValueOnce(loaded(null));

    await applyOrgRegistryChange({
      orgSlug: "beta/ctx",
      principal: "42",
      edit: "RenameTeam",
      team: "core",
      previousTeam: "default",
    });

    const state = useOrgStore.getState();
    expect(state.activeOrgSlug).toBe("acme/ctx");
    expect(state.activeTeamName).toBe("default");
    expect(state.lastTeamByOrgSlug["beta/ctx"]).toBe("core");
    expect(state.orgs.find((o) => o.slug === "beta/ctx")!.teams.map((t) => t.name)).toEqual([
      "core",
    ]);
  });

  it("an earlier switch still waiting on its reload never overrides a later one", async () => {
    let release!: (value: Loaded) => void;
    mockedLoadOrg.mockReturnValueOnce(new Promise<Loaded>((r) => (release = r)));

    const first = applyActiveTeamSwitch({ orgSlug: "acme/ctx", team: "search" });
    await applyActiveTeamSwitch({ orgSlug: "beta/ctx", team: "ops" });
    expect(useOrgStore.getState().activeOrgSlug).toBe("beta/ctx");

    release(loaded(makeOrg("acme/ctx", ["default", "payments", "search"])));
    await first;

    const state = useOrgStore.getState();
    expect(state.activeOrgSlug).toBe("beta/ctx");
    expect(state.activeTeamName).toBe("ops");
  });

  it("discovers an org the store doesn't list yet, then switches into it", async () => {
    mockedDiscover.mockResolvedValueOnce([
      {
        slug: "gamma/ctx",
        display_name: "gamma",
        has_remote: false,
        remote_url: null,
        org_root: "/orgs/gamma/ctx",
      },
    ] as Awaited<ReturnType<typeof discoverLocalOrgs>>);
    diskHas(makeOrg("gamma/ctx", ["main"]));

    await applyActiveTeamSwitch({ orgSlug: "gamma/ctx", team: "main" });

    const state = useOrgStore.getState();
    expect(state.orgs.map((o) => o.slug)).toContain("gamma/ctx");
    expect(state.activeOrgSlug).toBe("gamma/ctx");
    expect(state.activeTeamName).toBe("main");
  });

  it("says so when the UI can't show the team the backend switched to", async () => {
    mockedDiscover.mockResolvedValueOnce([]);

    await applyActiveTeamSwitch({ orgSlug: "zeta/ctx", team: "x" });

    expect(toast.warning).toHaveBeenCalledWith(expect.stringContaining("zeta/ctx"));
    expect(useOrgStore.getState().activeOrgSlug).toBe("acme/ctx");
  });

  it("a hung org load times out instead of wedging that org's later changes", async () => {
    vi.useFakeTimers();
    try {
      mockedLoadOrg.mockReturnValueOnce(new Promise<Loaded>(() => {}));
      const hung = applyOrgRegistryChange({
        orgSlug: "acme/ctx",
        principal: "42",
        edit: "CreateTeam",
        team: "x",
      });
      diskHas(makeOrg("acme/ctx", ["default", "payments", "search"]));
      const next = applyOrgRegistryChange({
        orgSlug: "acme/ctx",
        principal: "42",
        edit: "CreateTeam",
        team: "search",
      });

      await vi.advanceTimersByTimeAsync(ORG_RELOAD_TIMEOUT_MS + 1);
      await hung;
      await next;

      expect(useOrgStore.getState().orgs[0].teams.map((t) => t.name)).toContain("search");
    } finally {
      vi.useRealTimers();
    }
  });
});

it("refreshes source attachment views only for this renderer's principal", async () => {
  const start = useOrgStore.getState().attachmentsVersion;
  await applySourceAttachmentChange({ principal: "other" });
  expect(useOrgStore.getState().attachmentsVersion).toBe(start);
  await applySourceAttachmentChange({ principal: "42" });
  expect(useOrgStore.getState().attachmentsVersion).toBe(start + 1);
});

it("consumes source attachment events through the mounted production listener", async () => {
  const start = useOrgStore.getState().attachmentsVersion;
  renderHook(() => useOrgRegistrySync());
  await act(async () => {
    for (const listener of eventMock.listeners.get(SOURCE_ATTACHMENTS_CHANGED_EVENT) ?? [])
      listener({ payload: { principal: "42" } });
  });
  expect(useOrgStore.getState().attachmentsVersion).toBe(start + 1);
});

// The exact event sequences the team MCP tools emit, replayed through the
// listener: each registry edit, then (for `create_team activate:true`) the switch.
describe("useOrgRegistrySync — team tool event sequences", () => {
  function snapshot() {
    const org = useOrgStore.getState();
    return {
      teams: org.orgs[0].teams.map((t) => t.name),
      activeTeamName: org.activeTeamName,
      lastTeamByOrgSlug: org.lastTeamByOrgSlug,
      shownTeamsByOrgSlug: org.shownTeamsByOrgSlug,
      openedTeams: useViewerStore.getState().openedTeams,
      maximizedPanel: useViewerStore.getState().maximizedPanel,
    };
  }

  it("shows an MCP-created, nested, activated team in the switcher without a reload", async () => {
    useOrgStore.setState({ shownTeamsByOrgSlug: { "acme/ctx": ["default", "payments"] } });
    const created = makeOrg("acme/ctx", ["default", "payments", "search"]);
    created.teams[2] = { ...created.teams[2], parent_team: "payments" };
    // Disk reads resolve late, so the switch arrives while the reloads are in flight.
    mockedLoadOrg.mockImplementation(
      () => new Promise<Loaded>((resolve) => setTimeout(() => resolve(loaded(created)), 5)),
    );
    renderHook(() => useOrgRegistrySync());

    emit(ORG_REGISTRY_CHANGED_EVENT, {
      principal: "42",
      orgSlug: "acme/ctx",
      edit: "CreateTeam",
      team: "search",
    });
    emit(ORG_REGISTRY_CHANGED_EVENT, {
      principal: "42",
      orgSlug: "acme/ctx",
      edit: "SetTeamParent",
      team: "search",
    });
    emit(ACTIVE_TEAM_SWITCH_EVENT, { principal: "42", orgSlug: "acme/ctx", team: "search" });
    await vi.waitFor(() => expect(useOrgStore.getState().activeTeamName).toBe("search"));

    const state = useOrgStore.getState();
    const shown = visibleTeams(state.orgs[0], state.shownTeamsByOrgSlug);
    expect(shown.map((t) => t.name)).toContain("search");
    expect(shown.find((t) => t.name === "search")?.parent_team).toBe("payments");
  });

  it("falls an MCP-deleted active team back to exactly where the UI delete lands", async () => {
    const initial = {
      org: useOrgStore.getState(),
      viewer: useViewerStore.getState(),
    };
    const remaining = makeOrg("acme/ctx", ["default"]);

    await useOrgStore.getState().deleteTeam("payments");
    // The UI's own commit is followed by the same registry event; replaying it is a no-op.
    diskHas(remaining);
    await applyOrgRegistryChange({
      principal: "42",
      orgSlug: "acme/ctx",
      edit: "DeleteTeam",
      team: "payments",
    });
    const viaUi = snapshot();

    useOrgStore.setState(initial.org, true);
    useViewerStore.setState(initial.viewer, true);
    diskHas(remaining);
    renderHook(() => useOrgRegistrySync());
    emit(ORG_REGISTRY_CHANGED_EVENT, {
      principal: "42",
      orgSlug: "acme/ctx",
      edit: "DeleteTeam",
      team: "payments",
    });
    await vi.waitFor(() => expect(useOrgStore.getState().orgs[0].teams).toHaveLength(1));

    expect(snapshot()).toEqual(viaUi);
    expect(viaUi.activeTeamName).toBe("default");
  });

  it("shows an MCP workspace rename", async () => {
    diskHas({ ...makeOrg("acme/ctx", ["default", "payments"]), display_name: "Acme Corp" });
    await applyOrgRegistryChange({ principal: "42", orgSlug: "acme/ctx", edit: "RenameWorkspace" });
    expect(useOrgStore.getState().orgs[0].display_name).toBe("Acme Corp");
    expect(useOrgStore.getState().activeTeamName).toBe("payments");
  });
});

describe("useOrgRegistrySync — source-repo-attachments-changed", () => {
  it("re-reads the attachments when a clone is bound or unbound elsewhere", async () => {
    renderHook(() => useOrgRegistrySync());
    await vi.waitFor(() =>
      expect(eventMock.listeners.get(SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT)?.size).toBe(1),
    );
    const before = useOrgStore.getState().attachmentsVersion;
    emit(SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT, { principal: "42" });
    await vi.waitFor(() => expect(useOrgStore.getState().attachmentsVersion).toBe(before + 1));
  });
});
