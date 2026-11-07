// SPDX-License-Identifier: MIT
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { Org } from "@/shared/types/domain";

import { useTeamLandingPage } from "./use-team-landing-page";

const openTeamLandingPage = vi.fn().mockResolvedValue("opened");
const invalidateLandingPages = vi.fn();
vi.mock("@/features/viewer/lib/team-landing-page", () => ({
  openTeamLandingPage: (...args: unknown[]) => openTeamLandingPage(...args),
  invalidateLandingPages: (...args: unknown[]) => invalidateLandingPages(...args),
}));

type Handler = (e: { payload: { file_path?: string } }) => void;
const fileChangeHandlers: Handler[] = [];
const registryHandlers: Handler[] = [];
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, cb: Handler) => {
    (event === "org-registry-changed" ? registryHandlers : fileChangeHandlers).push(cb);
    return () => {};
  }),
}));

const ROOT = "/home/u/codesense-docs";
const DOCS: Org = {
  slug: "codesense/docs",
  display_name: "CodeSense Docs",
  created_at: "2026-01-01T00:00:00Z",
  remote_url: null,
  config_flags: {},
  people: [],
  teams: [
    { name: "getting-started", source_repos: [], landing_page: "README.html" },
    { name: "other", source_repos: [] },
  ],
  org_root: ROOT,
};
const KEY = "codesense/docs:getting-started";
const TOUR = `${ROOT}/getting-started/content/tour.html`;
const NOTES = `${ROOT}/getting-started/content/notes.md`;

const docsEntry = {
  org: { slug: "codesense/docs", org_root: ROOT },
  team: { name: "getting-started", landing_page: "README.html" },
  fresh: true,
};
const docsClose = (justClosed: string[]) => ({
  org: { slug: "codesense/docs", org_root: ROOT },
  team: { name: "getting-started", landing_page: "README.html" },
  justClosed,
});

beforeEach(() => {
  openTeamLandingPage.mockClear();
  invalidateLandingPages.mockClear();
  fileChangeHandlers.length = 0;
  registryHandlers.length = 0;
  useOrgStore.setState({
    orgs: [DOCS],
    activeOrgSlug: "codesense/docs",
    activeTeamName: "getting-started",
    stateLoaded: true,
  });
  useViewerStore.setState({
    stateLoaded: true,
    persistedStateRestored: true,
    openTabs: {},
    activeTab: {},
    openedTeams: [],
  });
});

describe("useTeamLandingPage", () => {
  it("entering a team asks for its landing page", () => {
    renderHook(() => useTeamLandingPage());
    expect(openTeamLandingPage).toHaveBeenCalledWith(docsEntry);
  });

  it("switching team asks again for the new team", () => {
    renderHook(() => useTeamLandingPage());
    act(() => useOrgStore.setState({ activeTeamName: "other" }));
    expect(openTeamLandingPage).toHaveBeenLastCalledWith({
      org: { slug: "codesense/docs", org_root: ROOT },
      team: { name: "other", landing_page: null },
      fresh: true,
    });
  });

  it("closing the team's last tab asks again, naming the tabs just closed", () => {
    renderHook(() => useTeamLandingPage());
    act(() =>
      useViewerStore.setState({
        openTabs: {
          [KEY]: [
            { name: "tour.html", path: TOUR },
            { name: "notes.md", path: NOTES },
          ],
        },
      }),
    );
    openTeamLandingPage.mockClear();
    // Closing one of two tabs is not "nothing open".
    act(() =>
      useViewerStore.setState({ openTabs: { [KEY]: [{ name: "notes.md", path: NOTES }] } }),
    );
    expect(openTeamLandingPage).not.toHaveBeenCalled();
    act(() => useViewerStore.setState({ openTabs: { [KEY]: [] } }));
    expect(openTeamLandingPage).toHaveBeenCalledWith(docsClose([NOTES]));
  });

  it("a team teardown (closeTeam drops it from openedTeams) is not a close", () => {
    useViewerStore.setState({ openedTeams: [KEY] });
    renderHook(() => useTeamLandingPage());
    act(() => useViewerStore.setState({ openTabs: { [KEY]: [{ name: "t", path: TOUR }] } }));
    openTeamLandingPage.mockClear();
    act(() => useViewerStore.setState({ openTabs: { [KEY]: [] }, openedTeams: [] }));
    expect(openTeamLandingPage).not.toHaveBeenCalled();
  });

  it("a re-run for the same team (e.g. a landing_page edit) is not a new entry", () => {
    renderHook(() => useTeamLandingPage());
    expect(openTeamLandingPage).toHaveBeenCalledTimes(1);
    act(() =>
      useOrgStore.setState({
        orgs: [
          {
            ...DOCS,
            teams: [
              { name: "getting-started", source_repos: [], landing_page: "tour.html" },
              { name: "other", source_repos: [] },
            ],
          },
        ],
      }),
    );
    expect(openTeamLandingPage).toHaveBeenCalledTimes(1);
    // …but leaving and coming back is.
    act(() => useOrgStore.setState({ activeTeamName: "other" }));
    act(() => useOrgStore.setState({ activeTeamName: "getting-started" }));
    expect(openTeamLandingPage).toHaveBeenCalledTimes(3);
  });

  it("another team's tabs emptying does not trigger this team's page", () => {
    renderHook(() => useTeamLandingPage());
    act(() => useViewerStore.setState({ openTabs: { "acme:web": [{ name: "a", path: "/a" }] } }));
    openTeamLandingPage.mockClear();
    act(() => useViewerStore.setState({ openTabs: { "acme:web": [] } }));
    expect(openTeamLandingPage).not.toHaveBeenCalled();
  });

  it("waits for the real restore of the saved viewer state", () => {
    useViewerStore.setState({ persistedStateRestored: false });
    renderHook(() => useTeamLandingPage());
    expect(openTeamLandingPage).not.toHaveBeenCalled();
    act(() => useViewerStore.setState({ persistedStateRestored: true }));
    expect(openTeamLandingPage).toHaveBeenCalledTimes(1);
  });

  it("waits until the org selection has been restored", () => {
    useOrgStore.setState({ stateLoaded: false });
    renderHook(() => useTeamLandingPage());
    expect(openTeamLandingPage).not.toHaveBeenCalled();
    act(() => useOrgStore.setState({ stateLoaded: true }));
    expect(openTeamLandingPage).toHaveBeenCalledTimes(1);
  });

  it("does nothing with no active team", () => {
    useOrgStore.setState({ activeTeamName: null });
    renderHook(() => useTeamLandingPage());
    expect(openTeamLandingPage).not.toHaveBeenCalled();
  });

  it("a content file change drops the memoised landing page", async () => {
    renderHook(() => useTeamLandingPage());
    await act(async () => {});
    fileChangeHandlers[0]?.({ payload: { file_path: TOUR } });
    expect(invalidateLandingPages).toHaveBeenCalledWith(TOUR);
  });

  it("a registry edit (a starter page set or cleared) drops every memoised page", async () => {
    renderHook(() => useTeamLandingPage());
    await act(async () => {});
    registryHandlers[0]?.({ payload: {} });
    expect(invalidateLandingPages).toHaveBeenCalledWith();
  });
});
