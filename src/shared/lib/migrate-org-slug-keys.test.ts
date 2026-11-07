// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import {
  rewriteLegacyOrgSlug,
  rewriteLegacyTeamKey,
  migrateViewerOrgSlugKeys,
  migrateOrgStoreOrgSlug,
  deepRewriteOrgSlugPaths,
  TEAM_KEYED_PERSISTED_SLICES,
} from "./migrate-org-slug-keys";

describe("rewriteLegacyOrgSlug", () => {
  it("rewrites owner--repo to owner/repo", () => {
    expect(rewriteLegacyOrgSlug("acme--team-context")).toBe("acme/team-context");
  });

  it("splits on the FIRST -- when the repo half contains --", () => {
    expect(rewriteLegacyOrgSlug("alderco--my--repo")).toBe("alderco/my--repo");
  });

  it("passes already-nested and non-slug values through unchanged", () => {
    expect(rewriteLegacyOrgSlug("acme/team-context")).toBe("acme/team-context");
    expect(rewriteLegacyOrgSlug("alderco")).toBe("alderco");
    expect(rewriteLegacyOrgSlug("--repo")).toBe("--repo"); // empty owner
    expect(rewriteLegacyOrgSlug("owner--")).toBe("owner--"); // empty repo
  });
});

describe("rewriteLegacyTeamKey", () => {
  it("rewrites only the org-slug prefix, keeping the team suffix", () => {
    expect(rewriteLegacyTeamKey("acme--team-context:default")).toBe("acme/team-context:default");
  });

  it("leaves already-migrated and team-less keys untouched", () => {
    expect(rewriteLegacyTeamKey("acme/team-context:default")).toBe("acme/team-context:default");
    expect(rewriteLegacyTeamKey("__no_team__")).toBe("__no_team__");
  });
});

describe("migrateViewerOrgSlugKeys", () => {
  it("rewrites teamKey outer keys across every persisted team-keyed slice", () => {
    const viewer: Record<string, unknown> = {
      schemaVersion: 4,
      openTabs: { "acme--team-context:default": [{ path: "a.md" }] },
      workspaces: { "acme--team-context:default": [] },
      // already-migrated entry is left as-is (idempotent)
      activeWorkspace: { "acme/repo:main": "root:acme/repo" },
      // non-team-keyed scalar is untouched
      rightPaneOpen: true,
    };

    migrateViewerOrgSlugKeys(viewer);

    expect(viewer.openTabs).toEqual({
      "acme/team-context:default": [{ path: "a.md" }],
    });
    expect(viewer.workspaces).toEqual({ "acme/team-context:default": [] });
    expect(viewer.activeWorkspace).toEqual({ "acme/repo:main": "root:acme/repo" });
    expect(viewer.rightPaneOpen).toBe(true);
  });

  it("rewrites teamKey strings stored as openedTeams array elements", () => {
    const viewer: Record<string, unknown> = {
      openedTeams: [
        "acme--team-context:_none",
        "acme--team-context:default",
        "acme/repo:main", // already-nested → passthrough
      ],
    };

    migrateViewerOrgSlugKeys(viewer);

    expect(viewer.openedTeams).toEqual([
      "acme/team-context:_none",
      "acme/team-context:default",
      "acme/repo:main",
    ]);
  });

  it("dedupes openedTeams when a stale entry collides with a fresh one post-rewrite", () => {
    const viewer: Record<string, unknown> = {
      openedTeams: [
        "acme/team-context:_none",
        "acme--team-context:default", // stale — rewrites to `…/…:default`
        "acme/team-context:default", // already-live duplicate of the above
      ],
    };

    migrateViewerOrgSlugKeys(viewer);

    expect(viewer.openedTeams).toEqual(["acme/team-context:_none", "acme/team-context:default"]);
  });

  it("covers the same slice set the serializer writes", () => {
    // Guard: every team-keyed persisted slice is a distinct string.
    expect(new Set(TEAM_KEYED_PERSISTED_SLICES).size).toBe(TEAM_KEYED_PERSISTED_SLICES.length);
    expect(TEAM_KEYED_PERSISTED_SLICES).toContain("workspaces");
    expect(TEAM_KEYED_PERSISTED_SLICES).toContain("softClosedWorkspaces");
  });
});

describe("migrateOrgStoreOrgSlug", () => {
  it("rewrites activeOrgSlug and lastTeamByOrgSlug keys", () => {
    const org: Record<string, unknown> = {
      activeOrgSlug: "acme--team-context",
      activeTeamName: "default",
      lastTeamByOrgSlug: {
        "acme--team-context": "default",
        "acme--repo": "main",
      },
    };

    migrateOrgStoreOrgSlug(org);

    expect(org.activeOrgSlug).toBe("acme/team-context");
    expect(org.activeTeamName).toBe("default");
    expect(org.lastTeamByOrgSlug).toEqual({
      "acme/team-context": "default",
      "acme/repo": "main",
    });
  });

  it("tolerates a null activeOrgSlug and a missing map", () => {
    const org: Record<string, unknown> = { activeOrgSlug: null };
    migrateOrgStoreOrgSlug(org);
    expect(org.activeOrgSlug).toBeNull();
  });
});

describe("deepRewriteOrgSlugPaths", () => {
  it("rewrites /orgs/owner--repo/ paths anywhere in the tree", () => {
    const node: Record<string, unknown> = {
      tabPath: "/Users/me/.codesense/users/1/orgs/acme--team-context/_team/default/content/x.md",
      nested: {
        drillRoots: [
          "/Users/me/.codesense/users/1/orgs/acme--team-context/_team/default/content/dir",
          "/Users/me/.codesense/users/1/orgs/acme/repo/_team/main/content/dir", // already nested
        ],
        notAPath: "acme--team-context", // bare slug; no /orgs/ prefix → untouched
      },
    };

    deepRewriteOrgSlugPaths(node);

    expect(node.tabPath).toBe(
      "/Users/me/.codesense/users/1/orgs/acme/team-context/_team/default/content/x.md",
    );
    const drillRoots = (node.nested as { drillRoots: string[] }).drillRoots;
    expect(drillRoots[0]).toBe(
      "/Users/me/.codesense/users/1/orgs/acme/team-context/_team/default/content/dir",
    );
    expect(drillRoots[1]).toBe(
      "/Users/me/.codesense/users/1/orgs/acme/repo/_team/main/content/dir",
    );
    expect((node.nested as { notAPath: string }).notAPath).toBe("acme--team-context");
  });

  it("splits on the FIRST -- so a repo half with -- is preserved", () => {
    const node: Record<string, unknown> = {
      p: "/Users/me/.codesense/users/1/orgs/alderco--my--repo/_team/x",
    };
    deepRewriteOrgSlugPaths(node);
    expect(node.p).toBe("/Users/me/.codesense/users/1/orgs/alderco/my--repo/_team/x");
  });

  it("handles a path that ends exactly at the slug (no trailing slash)", () => {
    const node: Record<string, unknown> = {
      p: "/Users/me/.codesense/users/1/orgs/acme--team-context",
    };
    deepRewriteOrgSlugPaths(node);
    expect(node.p).toBe("/Users/me/.codesense/users/1/orgs/acme/team-context");
  });

  it("is idempotent — a second run rewrites nothing", () => {
    const node: Record<string, unknown> = {
      p: "/x/orgs/acme--team-context/y",
    };
    deepRewriteOrgSlugPaths(node);
    const after = node.p;
    deepRewriteOrgSlugPaths(node);
    expect(node.p).toBe(after);
  });
});

it("rewrites both legacy Agents dock maps before the v15 chrome seed", () => {
  const viewer = {
    workspacePanelCollapsed: { "owner--repo:team": true },
    workspacePanelWidth: { "owner--repo:team": 300 },
  };
  migrateViewerOrgSlugKeys(viewer);
  expect(viewer).toEqual({
    workspacePanelCollapsed: { "owner/repo:team": true },
    workspacePanelWidth: { "owner/repo:team": 300 },
  });
});
