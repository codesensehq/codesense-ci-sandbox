// SPDX-License-Identifier: MIT
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invalidateLandingPages } from "@/features/viewer/lib/team-landing-page";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Org } from "@/shared/types/domain";

import { useActiveTeamStarterPage } from "./use-team-starter-page";

const resolveTeamLandingPage = vi.fn();
vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  resolveTeamLandingPage: (...args: unknown[]) => resolveTeamLandingPage(...args),
}));

const CONTENT = "/orgs/acme/web/content";
const ORG: Org = {
  slug: "acme/ctx",
  display_name: "Acme",
  created_at: "2026-01-01T00:00:00Z",
  remote_url: null,
  config_flags: {},
  people: [],
  teams: [{ name: "web", source_repos: [] }],
  org_root: "/orgs/acme",
};

beforeEach(() => {
  invalidateLandingPages();
  resolveTeamLandingPage.mockReset();
  useOrgStore.setState({ orgs: [ORG], activeOrgSlug: "acme/ctx", activeTeamName: "web" });
});

describe("useActiveTeamStarterPage", () => {
  it("is pending, then the resolved page; a content change looks it up again", async () => {
    resolveTeamLandingPage.mockResolvedValue({ kind: "found", path: `${CONTENT}/README.md` });
    const { result } = renderHook(() => useActiveTeamStarterPage());
    expect(result.current?.effectivePath).toBeUndefined();
    await act(async () => {});
    expect(result.current?.effectivePath).toBe(`${CONTENT}/README.md`);

    resolveTeamLandingPage.mockResolvedValue({ kind: "found", path: `${CONTENT}/index.html` });
    await act(async () => {
      invalidateLandingPages(`${CONTENT}/README.md`);
    });
    expect(result.current?.effectivePath).toBe(`${CONTENT}/index.html`);
    expect(resolveTeamLandingPage).toHaveBeenCalledTimes(2);
  });

  it("content arriving after an empty first look (nothing memoised) is noticed", async () => {
    resolveTeamLandingPage.mockResolvedValue({ kind: "unavailable" });
    const { result } = renderHook(() => useActiveTeamStarterPage());
    await act(async () => {});
    expect(result.current?.effectivePath).toBeNull();

    resolveTeamLandingPage.mockResolvedValue({ kind: "found", path: `${CONTENT}/README.md` });
    await act(async () => {
      invalidateLandingPages(`${CONTENT}/README.md`);
    });
    expect(result.current?.effectivePath).toBe(`${CONTENT}/README.md`);
  });

  it("a second user reads an already-settled page at once", async () => {
    resolveTeamLandingPage.mockResolvedValue({ kind: "found", path: `${CONTENT}/README.md` });
    renderHook(() => useActiveTeamStarterPage());
    await act(async () => {});
    const { result } = renderHook(() => useActiveTeamStarterPage());
    expect(result.current?.effectivePath).toBe(`${CONTENT}/README.md`);
  });
});

it("keeps an owning team's starter page when another team becomes active", async () => {
  useOrgStore.setState({
    orgs: [{ ...ORG, teams: [...ORG.teams, { name: "other", source_repos: [] }] }],
  });
  resolveTeamLandingPage.mockImplementation(async (contentRoot: string) => ({
    kind: "found",
    path: `${contentRoot}/README.md`,
  }));
  const { result } = renderHook(() => useActiveTeamStarterPage("acme/ctx:web"));
  await act(async () => {});
  expect(result.current?.team.name).toBe("web");
  await act(async () => {
    useOrgStore.setState({ activeTeamName: "other" });
  });
  expect(result.current?.team.name).toBe("web");
  expect(result.current?.contentRoot).toBe(CONTENT);
  expect(result.current?.effectivePath).toBe(`${CONTENT}/README.md`);
});
