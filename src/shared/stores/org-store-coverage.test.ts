// SPDX-License-Identifier: MIT
import { vi, describe, it, expect, beforeEach } from "vitest";
import type { Org } from "@/shared/types/domain";

// Mock the logger so invariant-rejection tests can assert on warnings.
const mockWarn = vi.fn();
vi.mock("@/shared/lib/logger", () => ({
  logger: {
    error: vi.fn(),
    warn: mockWarn,
    info: vi.fn(),
    debug: vi.fn(),
  },
}));

// Mock the Tauri persistence commands — the store uses the same
// saveAppState/loadAppState pipeline as useViewerStore. The permission-
// slice tests also drive `getRepoPermissions` + `parseGitRemote`.
const mockSaveAppState = vi.fn().mockResolvedValue(undefined);
let mockLoadAppStateValue = "";
const mockGetRepoPermissions = vi.fn();
const mockParseGitRemote = vi.fn();
vi.mock("@/tauri/commands", () => ({
  saveAppState: (...args: unknown[]) => mockSaveAppState(...args),
  loadAppState: () => Promise.resolve(mockLoadAppStateValue),
  getRepoPermissions: (...args: unknown[]) => mockGetRepoPermissions(...args),
  parseGitRemote: (...args: unknown[]) => mockParseGitRemote(...args),
}));

function makeOrg(slug: string, teamNames: string[] = []): Org {
  return {
    slug,
    display_name: slug,
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {},
    people: [],
    teams: teamNames.map((name) => ({
      name,
      source_repos: [],
    })),
    org_root: "",
  };
}

// Freshly import the store module — needed for rehydrate test because
// the store is created at module-load time and holds private refs.
async function freshStore() {
  vi.resetModules();
  return (await import("./org-store")).useOrgStore;
}

beforeEach(() => {
  mockWarn.mockClear();
  mockSaveAppState.mockClear();
  mockGetRepoPermissions.mockReset();
  mockParseGitRemote.mockReset();
  mockLoadAppStateValue = "";
});

function makeOrgWithRemote(slug: string, remoteUrl: string): Org {
  return { ...makeOrg(slug), remote_url: remoteUrl };
}

describe("useOrgStore — initial state", () => {
  it("starts with empty orgs, null active selection", async () => {
    const useOrgStore = await freshStore();
    const s = useOrgStore.getState();
    expect(s.orgs).toEqual([]);
    expect(s.activeOrgSlug).toBeNull();
    expect(s.activeTeamName).toBeNull();
    expect(s.stateLoaded).toBe(false);
  });
});

describe("useOrgStore — updatePersonInStore (SW-ID-3)", () => {
  const alice = {
    github_user_id: 1,
    github_username: "alice",
    display_name: "Alice",
    avatar_url: null,
  };
  const bob = {
    github_user_id: 2,
    github_username: "bob",
    display_name: "Bob",
    avatar_url: null,
  };

  it("replaces the matching person by id and leaves others untouched", async () => {
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([{ ...makeOrg("a"), people: [alice, bob] }]);
    const refreshed = { ...alice, display_name: "Alice Cooper", avatar_url: "https://x/a.png" };
    useOrgStore.getState().updatePersonInStore("a", refreshed);
    const people = useOrgStore.getState().orgs[0].people;
    expect(people.find((p) => p.github_user_id === 1)).toEqual(refreshed);
    expect(people.find((p) => p.github_user_id === 2)).toEqual(bob);
  });

  it("is a no-op (same org ref) when the person id is absent", async () => {
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([{ ...makeOrg("a"), people: [alice] }]);
    const before = useOrgStore.getState().orgs[0];
    useOrgStore.getState().updatePersonInStore("a", { ...bob, display_name: "Bobby" });
    expect(useOrgStore.getState().orgs[0].people).toEqual([alice]);
    expect(useOrgStore.getState().orgs[0]).toBe(before);
  });

  it("is a no-op for an unknown org slug", async () => {
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([{ ...makeOrg("a"), people: [alice] }]);
    useOrgStore.getState().updatePersonInStore("nope", { ...alice, display_name: "X" });
    expect(useOrgStore.getState().orgs[0].people).toEqual([alice]);
  });
});

describe("useOrgStore — setOrgs / setActiveOrg / setActiveTeam", () => {
  it("setOrgs populates the list", async () => {
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([makeOrg("a"), makeOrg("b")]);
    const s = useOrgStore.getState();
    expect(s.orgs.length).toBe(2);
    expect(s.activeOrgSlug).toBeNull();
    expect(s.activeTeamName).toBeNull();
  });

  it("SW-OB-7: setActiveOrg with a known slug lands on the org's default team", async () => {
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([makeOrg("a", ["backend"])]);
    useOrgStore.getState().setActiveOrg("a");
    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBe("a");
    // No "default" team → first team; never null while the org has teams.
    expect(s.activeTeamName).toBe("backend");
  });

  it("setActiveOrg with an unknown slug is a no-op and logs a warning", async () => {
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([makeOrg("a")]);
    useOrgStore.getState().setActiveOrg("does-not-exist");
    expect(useOrgStore.getState().activeOrgSlug).toBeNull();
    expect(mockWarn).toHaveBeenCalledWith("org-store", expect.stringContaining("does-not-exist"));
  });

  it("setActiveOrg with the currently-active slug is a no-op (preserves active team)", async () => {
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([makeOrg("a", ["backend"])]);
    useOrgStore.getState().setActiveOrg("a");
    useOrgStore.getState().setActiveTeam("backend");
    // Re-selecting the already-active org must NOT clear the active team.
    useOrgStore.getState().setActiveOrg("a");
    expect(useOrgStore.getState().activeTeamName).toBe("backend");
  });

  it("setActiveTeam with a known team sets it", async () => {
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([makeOrg("a", ["backend", "frontend"])]);
    useOrgStore.getState().setActiveOrg("a");
    useOrgStore.getState().setActiveTeam("backend");
    expect(useOrgStore.getState().activeTeamName).toBe("backend");
  });

  it("setActiveTeam with an unknown team is a no-op and logs a warning", async () => {
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([makeOrg("a", ["backend"])]);
    useOrgStore.getState().setActiveOrg("a"); // SW-OB-7: lands on "backend"
    useOrgStore.getState().setActiveTeam("ghost");
    // No-op leaves the already-active team untouched (not null).
    expect(useOrgStore.getState().activeTeamName).toBe("backend");
    expect(mockWarn).toHaveBeenCalledWith("org-store", expect.stringContaining("ghost"));
  });
});

describe("useOrgStore — setOrgs cascades", () => {
  it("clears active when new list does not contain active org", async () => {
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([makeOrg("a", ["backend"])]);
    useOrgStore.getState().setActiveOrg("a");
    useOrgStore.getState().setActiveTeam("backend");
    useOrgStore.getState().setOrgs([makeOrg("b")]);
    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBeNull();
    expect(s.activeTeamName).toBeNull();
  });

  it("empty list clears active", async () => {
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([makeOrg("a")]);
    useOrgStore.getState().setActiveOrg("a");
    useOrgStore.getState().setOrgs([]);
    expect(useOrgStore.getState().activeOrgSlug).toBeNull();
  });

  it("SW-OB-7: preserves active org and lands on the default team when the active team was removed", async () => {
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([makeOrg("a", ["backend", "frontend"])]);
    useOrgStore.getState().setActiveOrg("a");
    useOrgStore.getState().setActiveTeam("backend");
    // Replacement org with same slug but backend removed.
    useOrgStore.getState().setOrgs([makeOrg("a", ["frontend"])]);
    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBe("a");
    // backend gone, no memory → land on the remaining team (never null-limbo).
    expect(s.activeTeamName).toBe("frontend");
  });
});

describe("useOrgStore — clearActive helper", () => {
  it("zeros out active selection without touching orgs", async () => {
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([makeOrg("a", ["backend"])]);
    useOrgStore.getState().setActiveOrg("a");
    useOrgStore.getState().setActiveTeam("backend");
    useOrgStore.getState().clearActive();
    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBeNull();
    expect(s.activeTeamName).toBeNull();
    expect(s.orgs.length).toBe(1);
  });
});

describe("useOrgStore — loadPersistedState", () => {
  it("rehydrates activeOrgSlug/activeTeamName from app state JSON", async () => {
    mockLoadAppStateValue = JSON.stringify({
      org: {
        activeOrgSlug: "persisted-org",
        activeTeamName: "persisted-team",
      },
    });
    const useOrgStore = await freshStore();
    await useOrgStore.getState().loadPersistedState();
    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBe("persisted-org");
    expect(s.activeTeamName).toBe("persisted-team");
    expect(s.orgs).toEqual([]);
    expect(s.stateLoaded).toBe(true);
  });

  it("ignores stale `currentUser` key in persisted JSON (F-25 cleanup)", async () => {
    // F-25 removed currentUser from persistence. Old persisted blocks
    // may still carry the field; rehydrate must read activeOrgSlug /
    // activeTeamName as usual and silently drop the stale field.
    mockLoadAppStateValue = JSON.stringify({
      org: {
        activeOrgSlug: "persisted-org",
        activeTeamName: "persisted-team",
        currentUser: { github_user_id: 42, github_username: "octo" },
      },
    });
    const useOrgStore = await freshStore();
    await useOrgStore.getState().loadPersistedState();
    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBe("persisted-org");
    expect(s.activeTeamName).toBe("persisted-team");
    expect(s.stateLoaded).toBe(true);
  });

  it("handles missing `org` key by setting stateLoaded without touching other state", async () => {
    mockLoadAppStateValue = JSON.stringify({ viewer: { foo: "bar" } });
    const useOrgStore = await freshStore();
    await useOrgStore.getState().loadPersistedState();
    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBeNull();
    expect(s.stateLoaded).toBe(true);
  });

  it("handles empty app state string", async () => {
    mockLoadAppStateValue = "";
    const useOrgStore = await freshStore();
    await useOrgStore.getState().loadPersistedState();
    expect(useOrgStore.getState().stateLoaded).toBe(true);
  });

  it("all-or-nothing rehydrate: any setter firing before load drops the persisted block", async () => {
    // Race scenario: App init fires `loadPersistedState()` as fire-and-
    // forget. If any user-intent setter (setActiveOrg, setActiveTeam,
    // clearActive) fires before the async load resolves, the persisted
    // block must be dropped entirely — per-field merge would leak a
    // stale team into a live org (or vice versa), violating the cross-
    // field invariant that a team must belong to its active org.
    mockLoadAppStateValue = JSON.stringify({
      org: {
        activeOrgSlug: "stale-org",
        activeTeamName: "stale-team",
      },
    });
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([makeOrg("live-org", ["live-team"])]);
    useOrgStore.getState().setActiveOrg("live-org");
    await useOrgStore.getState().loadPersistedState();

    const s = useOrgStore.getState();
    // The stale persisted block is dropped — live state wins. SW-OB-7:
    // setActiveOrg landed on the live org's only team rather than null.
    expect(s.activeOrgSlug).toBe("live-org");
    expect(s.activeTeamName).toBe("live-team");
    expect(s.stateLoaded).toBe(true);
    expect(s.hasUserIntent).toBe(true);
  });

  it("setOrgs does NOT flip hasUserIntent — disk-driven refreshes must not trip the rehydrate guard", async () => {
    // `setOrgs` is a system-driven refresh, not user intent. Future
    // stories will call `setOrgs([...fromDisk])` during App init in
    // parallel with `loadPersistedState()`. If `setOrgs` flipped the
    // flag, rehydrate would see `hasUserIntent: true` and silently
    // drop the persisted session. Lock the invariant with an
    // explicit test.
    const useOrgStore = await freshStore();
    expect(useOrgStore.getState().hasUserIntent).toBe(false);

    useOrgStore.getState().setOrgs([makeOrg("a", ["backend"])]);
    expect(useOrgStore.getState().hasUserIntent).toBe(false);

    // Even with a cascade (active org removed), no flag flip.
    useOrgStore.getState().setOrgs([]);
    expect(useOrgStore.getState().hasUserIntent).toBe(false);
  });

  it("all-or-nothing rehydrate: setOrgs pre-load does NOT prevent rehydrate from restoring persisted state", async () => {
    // Regression guard: a disk-driven `setOrgs([...fromDisk])`
    // firing before `loadPersistedState()` resolves must NOT cause
    // rehydrate to drop the persisted `activeOrgSlug`/`activeTeamName`.
    // `setOrgs` is system-driven; it deliberately does not flip
    // `hasUserIntent`.
    mockLoadAppStateValue = JSON.stringify({
      org: {
        activeOrgSlug: "persisted-org",
        activeTeamName: "persisted-team",
      },
    });
    const useOrgStore = await freshStore();

    // Simulate the future A.3+ flow: disk-load populates orgs first.
    useOrgStore.getState().setOrgs([makeOrg("persisted-org", ["persisted-team"])]);
    await useOrgStore.getState().loadPersistedState();

    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBe("persisted-org");
    expect(s.activeTeamName).toBe("persisted-team");
  });

  it("SW-OB-7: loadPersistedState lands on a default team when orgs are pre-populated and the persisted team is null", async () => {
    // Blank-screen repro via the LOAD path (not setOrgs/setActiveOrg): discovery
    // populated `orgs` before rehydrate AND the persisted active org still matches,
    // so no later reconciler fires. A persisted null team would be written straight
    // through → `${org}:_none` ≠ the viewer's restored opened team → blank. The load
    // path must default the team itself.
    mockLoadAppStateValue = JSON.stringify({
      org: { activeOrgSlug: "a", activeTeamName: null },
    });
    const useOrgStore = await freshStore();
    // Raw setState (not setOrgs) so `hasUserIntent` stays false → rehydrate applies.
    useOrgStore.setState({ orgs: [makeOrg("a", ["default", "backend"])] });

    await useOrgStore.getState().loadPersistedState();

    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBe("a");
    expect(s.activeTeamName).toBe("default");
  });

  it("SW-OB-7: loadPersistedState defaults the team when the persisted team was invalidated (no longer in the org)", async () => {
    mockLoadAppStateValue = JSON.stringify({
      org: { activeOrgSlug: "a", activeTeamName: "ghost-team" },
    });
    const useOrgStore = await freshStore();
    useOrgStore.setState({ orgs: [makeOrg("a", ["backend"])] });

    await useOrgStore.getState().loadPersistedState();

    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBe("a");
    // ghost-team invalidated → land on the only team (no "default" here).
    expect(s.activeTeamName).toBe("backend");
  });

  it("all-or-nothing rehydrate: clearActive() pre-load is NOT undone", async () => {
    mockLoadAppStateValue = JSON.stringify({
      org: {
        activeOrgSlug: "stale-org",
        activeTeamName: "stale-team",
      },
    });
    const useOrgStore = await freshStore();
    useOrgStore.getState().clearActive();
    await useOrgStore.getState().loadPersistedState();

    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBeNull();
    expect(s.activeTeamName).toBeNull();
  });

  it("all-or-nothing rehydrate: setActiveOrg pre-load prevents stale activeTeamName leaking into live org", async () => {
    // Cross-field invariant: a persisted team belongs to a persisted
    // org. If the user selects a different org pre-rehydrate, the
    // persisted team must NOT bleed into that live org.
    mockLoadAppStateValue = JSON.stringify({
      org: {
        activeOrgSlug: "persisted-org",
        activeTeamName: "persisted-team",
      },
    });
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([makeOrg("live-org", ["live-team"])]);
    useOrgStore.getState().setActiveOrg("live-org");
    await useOrgStore.getState().loadPersistedState();

    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBe("live-org");
    // SW-OB-7: lands on the LIVE org's own team — the stale "persisted-team"
    // still did NOT leak (that's the invariant under test).
    expect(s.activeTeamName).toBe("live-team");
  });
});

describe("useOrgStore — persistState writes through saveAppState", () => {
  it("setActiveOrg triggers a debounced saveAppState that writes the org key", async () => {
    vi.useFakeTimers();
    try {
      const useOrgStore = await freshStore();
      // Pretend rehydrate already landed so persistState is not gated.
      useOrgStore.setState({ stateLoaded: true });
      useOrgStore.getState().setOrgs([makeOrg("a", ["backend"])]);
      useOrgStore.getState().setActiveOrg("a");
      // Advance past the 300ms debounce.
      await vi.advanceTimersByTimeAsync(350);
      expect(mockSaveAppState).toHaveBeenCalled();
      const saved = JSON.parse(
        mockSaveAppState.mock.calls[mockSaveAppState.mock.calls.length - 1][0] as string,
      );
      expect(saved.org.activeOrgSlug).toBe("a");
      expect(saved.org.activeTeamName).toBe("backend"); // SW-OB-7: default landing
      // F-25: currentUser is no longer part of the persisted payload.
      expect(saved.org).not.toHaveProperty("currentUser");
    } finally {
      vi.useRealTimers();
    }
  });

  it("setter pre-rehydrate does NOT schedule a save (guarded by stateLoaded)", async () => {
    // Regression guard: if `setOrgs` could trigger a debounced save
    // pre-rehydrate, the save's 300ms timer could race the async
    // `loadAppState()` IPC and write in-memory defaults back to disk
    // before rehydrate reads it, clobbering the persisted block.
    vi.useFakeTimers();
    try {
      const useOrgStore = await freshStore();
      expect(useOrgStore.getState().stateLoaded).toBe(false);

      useOrgStore.getState().setOrgs([makeOrg("a")]);
      await vi.advanceTimersByTimeAsync(500);
      expect(mockSaveAppState).toHaveBeenCalledTimes(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("loadPersistedState early-return path does NOT flush — persisted disk block is preserved", async () => {
    // Regression guard: `loadPersistedState` must NOT unconditionally
    // trigger a `debouncedPersist` flush — including on the
    // `hasUserIntent` early-return path. If a pre-rehydrate setter
    // left other fields at null defaults, a flush would write those
    // null defaults to disk, destroying the persisted activeOrg/
    // activeTeam the all-or-nothing design deliberately kept out of
    // live state.
    vi.useFakeTimers();
    try {
      mockLoadAppStateValue = JSON.stringify({
        org: {
          activeOrgSlug: "persisted-org",
          activeTeamName: "persisted-team",
        },
      });
      const useOrgStore = await freshStore();

      // Pre-rehydrate user-intent setter: clearActive() flips
      // hasUserIntent without touching the active* fields meaningfully
      // (both are already null on cold start).
      useOrgStore.getState().clearActive();
      expect(mockSaveAppState).toHaveBeenCalledTimes(0); // gated

      await useOrgStore.getState().loadPersistedState();
      // The early-return path must NOT schedule a flush.
      await vi.advanceTimersByTimeAsync(500);
      expect(mockSaveAppState).toHaveBeenCalledTimes(0);

      // Live state cleared; persisted block dropped.
      const s = useOrgStore.getState();
      expect(s.activeOrgSlug).toBeNull();
      expect(s.activeTeamName).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("loadPersistedState clears orphan team when persisted slug is null but team is non-null", async () => {
    // Regression guard: an incoherent persisted block (from an
    // interrupted write or prior bug) with `activeOrgSlug: null`
    // and `activeTeamName: "x"` must NOT restore the orphan team —
    // a team is meaningless without an org.
    mockLoadAppStateValue = JSON.stringify({
      org: {
        activeOrgSlug: null,
        activeTeamName: "orphan-team",
      },
    });
    const useOrgStore = await freshStore();
    await useOrgStore.getState().loadPersistedState();

    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBeNull();
    expect(s.activeTeamName).toBeNull();
  });

  it("loadPersistedState validates persisted slug against live orgs list and clears phantom references", async () => {
    // Regression guard: if a disk-driven loader calls
    // `setOrgs([orgA, orgB])` pre-rehydrate, and the persisted block
    // has `activeOrgSlug: "orgC"` (not in the list), rehydrate must
    // clear it instead of creating a phantom reference.
    mockLoadAppStateValue = JSON.stringify({
      org: {
        activeOrgSlug: "persisted-ghost",
        activeTeamName: "ghost-team",
      },
    });
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([makeOrg("live-org", ["live-team"])]);
    await useOrgStore.getState().loadPersistedState();

    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBeNull();
    expect(s.activeTeamName).toBeNull();
    expect(s.orgs).toEqual([makeOrg("live-org", ["live-team"])]);
  });

  it("loadPersistedState validates persisted team against the matched org's teams (SW-OB-7: lands on a valid team)", async () => {
    // If the slug matches but the team no longer exists in that org, drop the
    // stale team but preserve the active org — and, under open access, land on
    // a valid team rather than null (a null team would re-blank the screen).
    mockLoadAppStateValue = JSON.stringify({
      org: {
        activeOrgSlug: "live-org",
        activeTeamName: "removed-team",
      },
    });
    const useOrgStore = await freshStore();
    useOrgStore.getState().setOrgs([makeOrg("live-org", ["other-team"])]);
    await useOrgStore.getState().loadPersistedState();

    const s = useOrgStore.getState();
    expect(s.activeOrgSlug).toBe("live-org");
    expect(s.activeTeamName).toBe("other-team");
  });

  it("merges under an existing app state instead of replacing it", async () => {
    vi.useFakeTimers();
    try {
      mockLoadAppStateValue = JSON.stringify({ viewer: { foo: "bar" } });
      const useOrgStore = await freshStore();
      useOrgStore.setState({ stateLoaded: true });
      useOrgStore.getState().setOrgs([makeOrg("a", ["backend"])]);
      useOrgStore.getState().setActiveOrg("a");
      await vi.advanceTimersByTimeAsync(350);
      const saved = JSON.parse(
        mockSaveAppState.mock.calls[mockSaveAppState.mock.calls.length - 1][0] as string,
      );
      // viewer block is preserved; org block is added.
      expect(saved.viewer).toEqual({ foo: "bar" });
      expect(saved.org.activeOrgSlug).toBe("a");
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("useOrgStore — workspace permission slice (SW-OB-8: remote fetch retired)", () => {
  it("currentUserPermission initializes to null (full control); the fetch action + remote fields are gone", async () => {
    const useOrgStore = await freshStore();
    const s = useOrgStore.getState();
    // Pinned null = full control (open access). This is the surface
    // SW-OB-7 removes when it deletes the gate.
    expect(s.currentUserPermission).toBeNull();
    // SW-OB-8 removed the remote permission fetch + its derived fields.
    expect("refreshWorkspacePermission" in s).toBe(false);
    expect("currentWorkspaceDefaultBranch" in s).toBe(false);
    expect("permissionFetchState" in s).toBe(false);
  });

  it("switching into a remote (cloned) workspace performs NO GitHub permission fetch", async () => {
    const useOrgStore = await freshStore();
    useOrgStore
      .getState()
      .setOrgs([makeOrgWithRemote("acme", "https://github.com/acme/context.git")]);
    useOrgStore.getState().setActiveOrg("acme");
    // No fetch is wired to activeOrgSlug changes anymore — clone-on-join
    // is the only origin read (AC-5).
    expect(mockGetRepoPermissions).not.toHaveBeenCalled();
    expect(mockParseGitRemote).not.toHaveBeenCalled();
    // Permission stays null (full control) even for a remote/cloned org.
    expect(useOrgStore.getState().currentUserPermission).toBeNull();
  });

  it("currentUserPermission is NOT part of the persisted payload", async () => {
    vi.useFakeTimers();
    try {
      const useOrgStore = await freshStore();
      useOrgStore.setState({ stateLoaded: true });
      useOrgStore.setState({ currentUserPermission: "admin" });
      useOrgStore.getState().setOrgs([makeOrg("a", ["backend"])]);
      useOrgStore.getState().setActiveOrg("a");
      await vi.advanceTimersByTimeAsync(350);
      const saved = JSON.parse(
        mockSaveAppState.mock.calls[mockSaveAppState.mock.calls.length - 1][0] as string,
      );
      expect(saved.org).not.toHaveProperty("currentUserPermission");
    } finally {
      vi.useRealTimers();
    }
  });
});

it("profile switching surfaces an org selection save failure and can retry", async () => {
  const store = await freshStore();
  store.setState({ stateLoaded: true, activeOrgSlug: "owner/context", activeTeamName: "main" });
  const { flushOrgForProfileSwitch } = await import("./org-store");
  mockSaveAppState.mockRejectedValueOnce(new Error("profile disk full"));
  await expect(flushOrgForProfileSwitch()).rejects.toThrow("profile disk full");
  await expect(flushOrgForProfileSwitch()).resolves.toBeUndefined();
  const saved = JSON.parse(mockSaveAppState.mock.lastCall?.[0] as string);
  expect(saved.org).toMatchObject({ activeOrgSlug: "owner/context", activeTeamName: "main" });
});
