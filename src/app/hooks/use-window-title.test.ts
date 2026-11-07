// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";

const setTitle = vi.fn(() => Promise.resolve());
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ setTitle }),
}));

// Import AFTER the mock so the hook resolves against the mocked module.
import { useWindowTitle } from "./use-window-title";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Org } from "@/shared/types/domain";

function makeOrg(slug: string, displayName: string): Org {
  return {
    slug,
    display_name: displayName,
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {},
    people: [],
    teams: [
      {
        name: "default",
        source_repos: [],
      },
    ],
    org_root: `/home/user/.codesense/users/1234/orgs/${slug}`,
  };
}

describe("useWindowTitle", () => {
  beforeEach(() => {
    setTitle.mockClear();
    useOrgStore.setState({
      orgs: [],
      activeOrgSlug: null,
      activeTeamName: null,
    });
  });

  it("sets a base title when no org is active", () => {
    renderHook(() => useWindowTitle());
    expect(setTitle).toHaveBeenCalledWith("CodeSense");
  });

  it("includes org display_name + team in the title once active", () => {
    const org = makeOrg("9north", "9North Studio");
    useOrgStore.setState({ orgs: [org], activeOrgSlug: "9north", activeTeamName: "default" });

    renderHook(() => useWindowTitle());

    expect(setTitle).toHaveBeenLastCalledWith("CodeSense — 9North Studio / default");
  });

  it("updates synchronously when activeTeamName changes", () => {
    const org = makeOrg("9north", "9North Studio");
    useOrgStore.setState({ orgs: [org], activeOrgSlug: "9north", activeTeamName: "default" });

    const { rerender } = renderHook(() => useWindowTitle());

    setTitle.mockClear();
    act(() => {
      useOrgStore.setState({ activeTeamName: "beta" });
    });
    rerender();

    expect(setTitle).toHaveBeenCalledWith("CodeSense — 9North Studio / beta");
  });

  it("falls back to org slug when display_name is missing", () => {
    // display_name = "" → fall back to slug
    const org = makeOrg("acme", "");
    useOrgStore.setState({ orgs: [org], activeOrgSlug: "acme", activeTeamName: "default" });

    renderHook(() => useWindowTitle());

    expect(setTitle).toHaveBeenLastCalledWith("CodeSense — acme / default");
  });

  it("uses 'default' as team label when activeTeamName is null", () => {
    const org = makeOrg("9north", "9North Studio");
    useOrgStore.setState({ orgs: [org], activeOrgSlug: "9north", activeTeamName: null });

    renderHook(() => useWindowTitle());

    expect(setTitle).toHaveBeenLastCalledWith("CodeSense — 9North Studio / default");
  });
});
