// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { gitSyncHandlers } from "./git-sync";
import { installMockBackend } from "../install";
import {
  FIXTURE_ROOTS,
  allSyncViewerFiles as allFiles,
  type SyncViewerPublishSet,
} from "../git-sync-fixtures";

const h = gitSyncHandlers();

function getSet(repoPath: string): SyncViewerPublishSet {
  return h.get_publish_set({ repoPath }) as SyncViewerPublishSet;
}

describe("gitSyncHandlers — get_publish_set", () => {
  it("keys distinct sets by repo path (team vs personal, one command)", () => {
    const team = getSet(FIXTURE_ROOTS.team);
    const personal = getSet(FIXTURE_ROOTS.personal);
    expect(team).not.toEqual(personal);
    expect(allFiles(team).every((f) => f.scope === "team" || f.scope === "loose")).toBe(true);
    expect(allFiles(personal).every((f) => f.scope === "personal")).toBe(true);
  });

  it("returns the real bucketed shape, not a flat list", () => {
    const team = getSet(FIXTURE_ROOTS.team);
    expect(Array.isArray(team.buckets)).toBe(true);
    expect(team.buckets.length).toBeGreaterThan(0);
    expect(team.general).toBeDefined();
    for (const b of team.buckets) {
      expect(b.count).toBe(b.files.length);
      // Workspace buckets are story/epic; the synthetic general bucket rides
      // set.general, never a buckets[] entry.
      expect(b.workspaceRef.kind).toMatch(/^(story|epic)$/);
    }
  });

  it("carries scope + contentRole on every file", () => {
    for (const f of allFiles(getSet(FIXTURE_ROOTS.team))) {
      expect(f.scope).toBeDefined();
      expect(f.contentRole).toBeDefined();
    }
  });

  it("models a rename as renamedFrom + a rename badge (never an R status)", () => {
    const renamed = allFiles(getSet(FIXTURE_ROOTS.team)).find((f) => f.renamedFrom);
    expect(renamed).toBeDefined();
    expect(renamed?.badges).toContain("rename");
    expect(renamed?.status).toMatch(/^(added|modified|deleted)$/);
  });

  it("seeds a deleted entry", () => {
    const files = allFiles(getSet(FIXTURE_ROOTS.team));
    expect(files.some((f) => f.status === "deleted")).toBe(true);
  });

  it("computes a counts roll-up consistent with the rows", () => {
    const team = getSet(FIXTURE_ROOTS.team);
    const files = allFiles(team);
    expect(team.counts.team + team.counts.personal + team.counts.loose).toBe(files.length);
    const roleSum = Object.values(team.counts.byRole).reduce((a, b) => a + b, 0);
    expect(roleSum).toBe(files.length);
  });

  it("seeds the noise badges (eol-only, mode) the changed-content-only lens hides", () => {
    const badges = new Set(allFiles(getSet(FIXTURE_ROOTS.team)).flatMap((f) => f.badges ?? []));
    expect(badges).toEqual(new Set(["rename", "eol-only", "mode"]));
  });

  it("partitions the personal set by all three personalSurfaces", () => {
    const personal = getSet(FIXTURE_ROOTS.personal);
    const surfaces = new Set(allFiles(personal).map((f) => f.personalSurface));
    expect(surfaces).toEqual(new Set(["notes", "dashboards", "content-overlay"]));
  });

  it("leaves personalSurface undefined on team + loose files (personal-only facet)", () => {
    expect(allFiles(getSet(FIXTURE_ROOTS.team)).every((f) => f.personalSurface === undefined)).toBe(
      true,
    );
  });

  it("ignores the PR/commit source axes (non-goal), keying on repoPath only", () => {
    const plain = getSet(FIXTURE_ROOTS.team);
    const withAxes = h.get_publish_set({
      repoPath: FIXTURE_ROOTS.team,
      prNumber: 42,
      commitSha: "deadbeef",
    }) as SyncViewerPublishSet;
    expect(withAxes).toEqual(plain);
  });

  it("seeds the pathological cases (baseline-3, large-3000, zero-change, all-loose)", () => {
    expect(allFiles(getSet(FIXTURE_ROOTS.baseline3))).toHaveLength(3);

    const large = getSet(FIXTURE_ROOTS.large3000);
    expect(allFiles(large)).toHaveLength(3000);
    expect(large.counts.team).toBe(3000);

    const zero = getSet(FIXTURE_ROOTS.zeroChange);
    expect(allFiles(zero)).toHaveLength(0);
    expect(zero.counts).toMatchObject({ team: 0, personal: 0, loose: 0 });

    const loose = getSet(FIXTURE_ROOTS.allLoose);
    expect(allFiles(loose).every((f) => f.scope === "loose")).toBe(true);
    expect(loose.counts).toMatchObject({ team: 0, personal: 0 });
  });

  it("empty-team / full-personal is a sourcing pair: team side empty, personal side full", () => {
    // Sourcing both roots yields "nothing heading to the team; N land in personal".
    expect(getSet(FIXTURE_ROOTS.emptyTeam).counts.team).toBe(0);
    expect(getSet(FIXTURE_ROOTS.personal).counts.personal).toBeGreaterThan(0);
  });

  it("returns the empty set for an unknown repo path (honest nothing-pending)", () => {
    expect(allFiles(getSet("/mock/not/a/real/root"))).toHaveLength(0);
  });
});

describe("gitSyncHandlers — scratch space", () => {
  it("lists the open org's scratch items and the all-orgs folder", () => {
    const listing = h.list_scratch_space({ orgSlug: "acme/ctx" }) as {
      exists: boolean;
      files: unknown[];
      allOrgs: Array<{ kind: string | null }>;
    };
    expect(listing.exists).toBe(true);
    expect(listing.files).toHaveLength(2);
    expect(listing.allOrgs.map((f) => f.kind)).toEqual(["skill", null]);
  });

  it("with no org open, only the all-orgs folder lists", () => {
    const listing = h.list_scratch_space({ orgSlug: null }) as {
      exists: boolean;
      files: unknown[];
    };
    expect(listing.exists).toBe(false);
    expect(listing.files).toEqual([]);
  });
});

// Prove the fixtures reach the FE through the SAME door the app uses.
describe("sync-viewer fixtures (seam via __TAURI_INTERNALS__)", () => {
  beforeEach(() => {
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    installMockBackend();
  });

  function invoke(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
    return (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__.invoke(cmd, args);
  }

  it("resolves both roots to distinct sets with the target field set", async () => {
    const team = (await invoke("get_publish_set", {
      repoPath: FIXTURE_ROOTS.team,
    })) as SyncViewerPublishSet;
    const personal = (await invoke("get_publish_set", {
      repoPath: FIXTURE_ROOTS.personal,
    })) as SyncViewerPublishSet;

    expect(team).not.toEqual(personal);
    expect(team.counts).toBeDefined();
    expect(team.buckets[0].files[0]).toEqual(
      expect.objectContaining({ scope: expect.any(String), contentRole: expect.any(String) }),
    );
    // The personal-plane facet resolves through the same door.
    expect(new Set(personal.general.files.map((f) => f.personalSurface))).toEqual(
      new Set(["notes", "dashboards", "content-overlay"]),
    );
  });
});

describe("gitSyncHandlers — changed_path_count", () => {
  it("counts the fixture publish set for the repo, 0 without one", () => {
    expect(h.changed_path_count({ repoPath: FIXTURE_ROOTS.team })).toBe(
      allFiles(getSet(FIXTURE_ROOTS.team)).length,
    );
    expect(h.changed_path_count({ repoPath: "/nowhere" })).toBe(0);
  });

  it("pins the count from ?changes= so the badge states can be walked", () => {
    expect(gitSyncHandlers("?changes=87").changed_path_count({ repoPath: "/nowhere" })).toBe(87);
    expect(gitSyncHandlers("?changes=0").changed_path_count({ repoPath: FIXTURE_ROOTS.team })).toBe(
      0,
    );
  });
});
