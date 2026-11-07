// SPDX-License-Identifier: MIT
import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Org } from "@/shared/types/domain";
import { useReadyResolverContext } from "@/shared/hooks/use-resolver-context";
import { useActiveOrg } from "@/shared/stores/use-active-org";
import { useActiveTeamKey } from "@/shared/lib/team-key";

import { useTeamRepoGlyphMap, useTeamSourceRepoSlugs } from "./use-source-repo-selection";

vi.mock("@/shared/hooks/use-resolver-context", () => ({ useReadyResolverContext: vi.fn() }));
vi.mock("@/shared/stores/use-active-org", () => ({ useActiveOrg: vi.fn() }));
vi.mock("@/shared/lib/team-key", async () => {
  const actual =
    await vi.importActual<typeof import("@/shared/lib/team-key")>("@/shared/lib/team-key");
  return { ...actual, useActiveTeamKey: vi.fn() };
});

const mockCtx = vi.mocked(useReadyResolverContext);
const mockOrg = vi.mocked(useActiveOrg);
const mockActiveTeamKey = vi.mocked(useActiveTeamKey);

const TEAM_KEY = "alderco:default";
const READY_CTX = { orgSlug: "alderco", teamName: "default", codesenseHome: "/h", userId: 1 };

function orgWithRepos(repos: Array<{ owner: string; repo: string }>): Org {
  return {
    slug: "alderco",
    teams: [{ name: "default", source_repos: repos }],
    org_root: "/h/orgs/acme/team-context",
  } as unknown as Org;
}

beforeEach(() => {
  vi.clearAllMocks();
  mockCtx.mockReturnValue(READY_CTX);
  mockActiveTeamKey.mockReturnValue(TEAM_KEY);
  mockOrg.mockReturnValue(orgWithRepos([{ owner: "alderco", repo: "codesense" }]));
  globalThis.localStorage?.clear();
});

describe("useTeamSourceRepoSlugs (SLW-NC-1 Phase 2 — single slug source)", () => {
  it("returns the team's source repos, deduped by slug", () => {
    mockOrg.mockReturnValue(
      orgWithRepos([
        { owner: "alderco", repo: "codesense" },
        { owner: "alderco", repo: "codesense" }, // dupe
        { owner: "acme", repo: "widgets" },
      ]),
    );
    const { result } = renderHook(() => useTeamSourceRepoSlugs(TEAM_KEY));
    expect(result.current.map((r) => `${r.owner}/${r.repo}`)).toEqual([
      "alderco/codesense",
      "acme/widgets",
    ]);
  });

  it("returns [] when the teamKey is not the active team (per-view scoping)", () => {
    mockActiveTeamKey.mockReturnValue("other:team");
    const { result } = renderHook(() => useTeamSourceRepoSlugs(TEAM_KEY));
    expect(result.current).toEqual([]);
  });
});

describe("useTeamRepoGlyphMap", () => {
  it("derives one shared glyph per source repo, in repo order", () => {
    mockOrg.mockReturnValue(
      orgWithRepos([
        { owner: "alderco", repo: "codesense" },
        { owner: "alderco", repo: "website" },
      ]),
    );
    const { result } = renderHook(() => useTeamRepoGlyphMap(TEAM_KEY));
    expect([...result.current.keys()]).toEqual(["alderco/codesense", "alderco/website"]);
  });
});
