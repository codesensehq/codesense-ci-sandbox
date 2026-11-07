// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { commitRegistryEdit } from "@/shared/lib/commit-registry-edit";
import type { Org } from "@/shared/types/domain";
import { useOrgStore } from "./org-store";
import { useViewerStore } from "./viewer-store";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn(() => Promise.resolve()),
  loadAppState: vi.fn(() => Promise.resolve("{}")),
  getRepoPermissions: vi.fn(),
  parseGitRemote: vi.fn(),
}));
vi.mock("@/shared/lib/commit-registry-edit", () => ({
  commitRegistryEdit: vi.fn(() => Promise.resolve()),
}));
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: vi.fn(() => ({ github_user_id: 42, github_username: "creator" })),
}));

function org(slug: string, teams: string[]): Org {
  return {
    slug,
    display_name: slug,
    created_at: "2026-01-01T00:00:00Z",
    org_root: `/fake/${slug}`,
    remote_url: null,
    config_flags: {},
    teams: teams.map((name) => ({ name, source_repos: [] })),
    people: [],
  };
}

const ALL = ["acme:a", "acme:b", "acme:c", "acme:d", "zed:x", "zed:y", "acme:b2"];

beforeEach(() => {
  for (const k of ALL) warmScopes.forget(k);
  warmScopes.activate(null);
  useViewerStore.setState({ openedTeams: [] });
  useOrgStore.setState({
    orgs: [org("acme", ["a", "b", "c", "d"]), org("zed", ["x", "y"])],
    activeOrgSlug: "acme",
    activeTeamName: null,
    lastTeamByOrgSlug: { zed: "y" },
    shownTeamsByOrgSlug: {},
    hasUserIntent: true,
    stateLoaded: true,
  });
});

describe("the active team is the one writer of the warm scopes", () => {
  it("setActiveTeam activates the scope and opens its team in the same tick", () => {
    useOrgStore.getState().setActiveTeam("a");
    expect(warmScopes.tierOf("acme:a")).toBe("active");
    expect(useViewerStore.getState().openedTeams).toContain("acme:a");
  });

  it("the previous team stays warm", () => {
    useOrgStore.getState().setActiveTeam("a");
    useOrgStore.getState().setActiveTeam("b");
    expect(warmScopes.tierOf("acme:a")).toBe("warm");
    expect(warmScopes.tierOf("acme:b")).toBe("active");
  });

  it("setActiveOrg activates the team it restores", () => {
    useOrgStore.getState().setActiveOrg("zed");
    expect(useOrgStore.getState().activeTeamName).toBe("y");
    expect(warmScopes.tierOf("zed:y")).toBe("active");
    expect(useViewerStore.getState().openedTeams).toContain("zed:y");
  });

  it("setActiveTeamIn crosses orgs in one write without warming the remembered team", () => {
    useOrgStore.getState().setActiveTeam("a");
    const seen: Array<string | null> = [];
    const stop = useOrgStore.subscribe((s) => seen.push(`${s.activeOrgSlug}:${s.activeTeamName}`));
    useOrgStore.getState().setActiveTeamIn("zed", "x");
    stop();
    expect(seen).toEqual(["zed:x"]);
    expect(warmScopes.tierOf("zed:x")).toBe("active");
    expect(warmScopes.tierOf("zed:y")).toBe("gone");
    expect(useOrgStore.getState().lastTeamByOrgSlug.zed).toBe("x");
    expect(useViewerStore.getState().openedTeams).not.toContain("zed:y");
  });

  it("setActiveTeamIn refuses an unknown team", () => {
    useOrgStore.getState().setActiveTeam("a");
    useOrgStore.getState().setActiveTeamIn("zed", "nope");
    expect(useOrgStore.getState().activeOrgSlug).toBe("acme");
    expect(warmScopes.tierOf("acme:a")).toBe("active");
  });

  it("a plain state write of the active team also activates (other writers, e2e controls)", () => {
    useOrgStore.setState({ activeOrgSlug: "zed", activeTeamName: "x" });
    expect(warmScopes.tierOf("zed:x")).toBe("active");
    expect(useViewerStore.getState().openedTeams).toContain("zed:x");
  });

  it("clearing the active team leaves the last one warm", () => {
    useOrgStore.getState().setActiveTeam("a");
    useOrgStore.getState().setActiveTeam(null);
    expect(warmScopes.tierOf("acme:a")).toBe("warm");
  });

  it("deleting a team forgets its scope and activates the fallback", async () => {
    useOrgStore.getState().setActiveTeam("a");
    useOrgStore.getState().setActiveTeam("b");
    const dispose = vi.fn();
    warmScopes.hold("acme:b", "watch", "k", dispose);
    await useOrgStore.getState().deleteTeam("b");
    expect(dispose).toHaveBeenCalledTimes(1);
    expect(warmScopes.tierOf("acme:b")).toBe("gone");
    expect(useOrgStore.getState().activeTeamName).not.toBe("b");
    const now = useOrgStore.getState().activeTeamName;
    expect(warmScopes.tierOf(`acme:${now}`)).toBe("active");
    expect(vi.mocked(commitRegistryEdit)).toHaveBeenCalled();
  });

  it("renaming the active team moves its scope instead of warming a ghost", async () => {
    useOrgStore.getState().setActiveTeam("a");
    useOrgStore.getState().setActiveTeam("b");
    const dispose = vi.fn();
    warmScopes.hold("acme:b", "watch", "k", dispose);
    await useOrgStore.getState().renameTeam("b", "b2");
    expect(warmScopes.tierOf("acme:b2")).toBe("active");
    expect(warmScopes.tierOf("acme:b")).toBe("gone");
    expect(dispose).not.toHaveBeenCalled();
    expect(warmScopes.debugCounts().held["acme:b2"]).toEqual({ watch: 1 });
  });
});
