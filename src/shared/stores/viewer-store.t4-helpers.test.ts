// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { getActiveTeamKey, deleteFromKeyedSlices, rekeyKeyedSlices } from "./viewer-store";
import { useOrgStore } from "./org-store";
import { NO_TEAM_SENTINEL, buildTeamKey } from "@/shared/lib/team-key";
import type { Workspace } from "./viewer-store";

/**
 * T.4 Phase 2 helpers — building blocks for later phases.
 *
 * `getActiveTeamKey` is the bridge used by setter actions whose signature
 * is still `repoPath`-first until Phase 5 lands the signature flip. It
 * lets the action body bucket writes by the active team while the
 * outer-arg contract stays unchanged.
 *
 * `deleteFromKeyedSlices` is the DRY used by `closeTeam` (Phase 3) to
 * clear every team-keyed slice for a given teamKey in one `set()` call.
 * Without it, `closeTeam` would accumulate ~24 destructures.
 */

describe("T.4 — getActiveTeamKey", () => {
  beforeEach(() => {
    useOrgStore.setState({
      activeOrgSlug: null,
      activeTeamName: null,
    });
  });

  it("returns the sentinel bucket when no org is active", () => {
    expect(getActiveTeamKey()).toBe(NO_TEAM_SENTINEL);
  });

  it("returns the sentinel bucket when org is null but team is set (shouldn't happen, defensive)", () => {
    useOrgStore.setState({
      activeOrgSlug: null,
      activeTeamName: "default",
    });
    // buildTeamKey("", "default") — empty org slug — falls through to sentinel.
    expect(getActiveTeamKey()).toBe(NO_TEAM_SENTINEL);
  });

  it("builds a real team key when both org and team are active", () => {
    useOrgStore.setState({
      activeOrgSlug: "acme",
      activeTeamName: "default",
    });
    expect(getActiveTeamKey()).toBe(buildTeamKey("acme", "default"));
  });

  it("re-reads useOrgStore on every call (does not cache)", () => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    const first = getActiveTeamKey();
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "design" });
    const second = getActiveTeamKey();
    expect(first).not.toBe(second);
    expect(second).toBe(buildTeamKey("acme", "design"));
  });
});

describe("T.4 — deleteFromKeyedSlices", () => {
  // Minimal ViewerState-shaped fixture with just the keys this helper
  // touches. The cast at the call site narrows back to `ViewerState`.
  const baseFixture = {
    openTabs: {
      "acme:default": [{ path: "/a.md", name: "a" }],
      "acme:design": [{ path: "/b.md", name: "b" }],
    },
    activeTab: {
      "acme:default": "/a.md",
      "acme:design": "/b.md",
    },
    drillPaths: {
      "acme:default": "/drill-default",
      "acme:design": "/drill-design",
    },
    workspaces: {
      "acme:default": [
        {
          id: "root",
          kind: "root" as const,
          cwd: "/",
          status: "ready" as const,
        } satisfies Workspace,
      ],
    },
    // A non-Record slice (string scalar) to verify the helper skips it.
    selectedRepoPath: "/some/path",
    // A boolean scalar — also skipped.
    scopeRecoveryComplete: true,
    // An array slice — skipped (Array.isArray short-circuit).
    openedRepos: ["x", "y"],
  };

  it("deletes the teamKey entry from each named Record slice", () => {
    const result = deleteFromKeyedSlices(baseFixture as never, "acme:default", [
      "openTabs",
      "activeTab",
      "drillPaths",
      "workspaces",
    ]);

    expect(result.openTabs).toEqual({
      "acme:design": [{ path: "/b.md", name: "b" }],
    });
    expect(result.activeTab).toEqual({
      "acme:design": "/b.md",
    });
    expect(result.drillPaths).toEqual({
      "acme:design": "/drill-design",
    });
    expect(result.workspaces).toEqual({});
  });

  it("returns an empty partial when no slice names are passed", () => {
    const result = deleteFromKeyedSlices(baseFixture as never, "acme:default", []);
    expect(result).toEqual({});
  });

  it("is a no-op for a teamKey absent from every named slice", () => {
    const result = deleteFromKeyedSlices(baseFixture as never, "ghost-team", [
      "openTabs",
      "activeTab",
    ]);
    // Whole sub-objects round-tripped unchanged (since the spread had
    // nothing to drop).
    expect(result.openTabs).toEqual(baseFixture.openTabs);
    expect(result.activeTab).toEqual(baseFixture.activeTab);
  });

  it("skips slices whose value is not a plain Record (scalars, arrays)", () => {
    // T.6: `selectedRepoPath` + `viewMode` + `openedRepos` retired in Task 10.
    // Test now exercises the scalar/array skip path against the remaining
    // non-Record fields on `ViewerState`.
    const result = deleteFromKeyedSlices(baseFixture as never, "acme:default", [
      "stateLoaded",
      "scopeRecoveryComplete",
      "openedTeams",
    ]);
    expect(result.stateLoaded).toBeUndefined();
    expect(result.scopeRecoveryComplete).toBeUndefined();
    expect(result.openedTeams).toBeUndefined();
  });

  it("does not mutate the input state", () => {
    const snapshot = JSON.parse(JSON.stringify(baseFixture));
    deleteFromKeyedSlices(baseFixture as never, "acme:default", [
      "openTabs",
      "activeTab",
      "drillPaths",
    ]);
    expect(baseFixture).toEqual(snapshot);
  });
});

describe("SW-HB-8 — rekeyKeyedSlices", () => {
  const baseFixture = {
    openTabs: {
      "acme:payments": [{ path: "/a.md", name: "a" }],
      "acme:keep": [{ path: "/b.md", name: "b" }],
    },
    activeTab: { "acme:payments": "/a.md", "acme:keep": "/b.md" },
    drillPaths: { "acme:payments": "/drill" },
    // scalar + array slices to verify they're skipped.
    scopeRecoveryComplete: true,
    openedTeams: ["acme:payments", "acme:keep"],
  };

  it("MOVES the old teamKey entry to the new key in each named slice", () => {
    const result = rekeyKeyedSlices(baseFixture as never, "acme:payments", "acme:billing", [
      "openTabs",
      "activeTab",
      "drillPaths",
    ]);
    expect(result.openTabs).toEqual({
      "acme:keep": [{ path: "/b.md", name: "b" }],
      "acme:billing": [{ path: "/a.md", name: "a" }],
    });
    expect(result.activeTab).toEqual({ "acme:keep": "/b.md", "acme:billing": "/a.md" });
    expect(result.drillPaths).toEqual({ "acme:billing": "/drill" });
  });

  it("emits NOTHING for slices that don't hold the old key (Object.is-stable)", () => {
    const result = rekeyKeyedSlices(baseFixture as never, "acme:payments", "acme:billing", [
      "activeTab",
      "openedTeams", // array — skipped
    ]);
    // activeTab held the key → present; openedTeams is an array → skipped.
    expect(result.activeTab).toBeDefined();
    expect(result.openedTeams).toBeUndefined();
  });

  it("is a no-op partial when old === new", () => {
    expect(
      rekeyKeyedSlices(baseFixture as never, "acme:payments", "acme:payments", ["openTabs"]),
    ).toEqual({});
  });

  it("does not mutate the input state", () => {
    const snapshot = JSON.parse(JSON.stringify(baseFixture));
    rekeyKeyedSlices(baseFixture as never, "acme:payments", "acme:billing", [
      "openTabs",
      "activeTab",
    ]);
    expect(baseFixture).toEqual(snapshot);
  });
});
