// SPDX-License-Identifier: MIT
import { renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@/shared/stores/org-store", () => ({
  useOrgStore: vi.fn(),
}));
vi.mock("@/shared/lib/get-current-user", () => ({
  useCurrentUser: vi.fn(),
}));

import type { Org } from "@/shared/types/domain";
import { useOrgStore } from "@/shared/stores/org-store";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { setCodesenseHomeForTests } from "@/shared/lib/codesense-home";
import { isResolverContext, useResolverContext } from "./use-resolver-context";

const mockUseOrgStore = vi.mocked(useOrgStore);
const mockUseCurrentUser = vi.mocked(useCurrentUser);

interface OrgStoreShape {
  orgs: Org[];
  activeOrgSlug: string | null;
  activeTeamName: string | null;
}

function makeOrg(slug: string, orgRoot: string): Org {
  return {
    slug,
    display_name: slug,
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {},
    people: [],
    teams: [],
    org_root: orgRoot,
  };
}

// `useResolverContext` reads `activeTeamName` directly off the store and the
// active org via `useActiveOrg` (which reads `orgs` + `activeOrgSlug`). The
// real `useActiveOrg` runs against this same mocked store.
function stubOrgStore(state: OrgStoreShape): void {
  mockUseOrgStore.mockImplementation((selector?: unknown) => {
    if (typeof selector === "function") {
      return (selector as (s: OrgStoreShape) => unknown)(state);
    }
    return state as unknown;
  });
}

function stubUser(userId: number | null): void {
  if (userId === null) {
    mockUseCurrentUser.mockReturnValue(null);
  } else {
    mockUseCurrentUser.mockReturnValue({
      github_user_id: userId,
      github_username: "test",
      display_name: "Test",
      avatar_url: null,
    });
  }
}

afterEach(() => {
  vi.clearAllMocks();
});

describe("useResolverContext", () => {
  it("returns null when org is not active", () => {
    stubOrgStore({ orgs: [], activeOrgSlug: null, activeTeamName: "default" });
    stubUser(123);
    const { result } = renderHook(() => useResolverContext());
    expect(result.current).toBeNull();
  });

  it("returns null when team is not active", () => {
    stubOrgStore({
      orgs: [makeOrg("alderco", "/Users/me/.codesense/users/123/orgs/alderco")],
      activeOrgSlug: "alderco",
      activeTeamName: null,
    });
    stubUser(123);
    const { result } = renderHook(() => useResolverContext());
    expect(result.current).toBeNull();
  });

  it("returns null when user is not authenticated", () => {
    stubOrgStore({
      orgs: [makeOrg("alderco", "/Users/me/.codesense/users/123/orgs/alderco")],
      activeOrgSlug: "alderco",
      activeTeamName: "default",
    });
    stubUser(null);
    const { result } = renderHook(() => useResolverContext());
    expect(result.current).toBeNull();
  });

  it("returns null when org_root is not yet populated (mount-timing race)", () => {
    stubOrgStore({
      orgs: [makeOrg("alderco", "")],
      activeOrgSlug: "alderco",
      activeTeamName: "default",
    });
    stubUser(123);
    const { result } = renderHook(() => useResolverContext());
    expect(result.current).toBeNull();
  });

  it("uses the app's CodeSense home, wherever the context repo lives", () => {
    setCodesenseHomeForTests("/Users/me/.codesense");
    // A repo cloned at ~/src/users/bob/orgs/acme once parsed to ~/src.
    stubOrgStore({
      orgs: [makeOrg("alderco", "/Users/me/src/users/bob/orgs/acme")],
      activeOrgSlug: "alderco",
      activeTeamName: "default",
    });
    stubUser(12345);
    const { result } = renderHook(() => useResolverContext());
    expect(result.current).toEqual({
      orgSlug: "alderco",
      teamName: "default",
      codesenseHome: "/Users/me/.codesense",
      userId: 12345,
    });
    expect(isResolverContext(result.current)).toBe(true);
  });

  it("returns the error variant when the CodeSense home could not be read (AC-5)", () => {
    setCodesenseHomeForTests(null);
    stubOrgStore({
      orgs: [makeOrg("alderco", "/Users/me/work/acme-context")],
      activeOrgSlug: "alderco",
      activeTeamName: "default",
    });
    stubUser(12345);
    const { result } = renderHook(() => useResolverContext());
    expect(result.current).toEqual({ error: "unresolved-home" });
    expect(isResolverContext(result.current)).toBe(false);
  });
});
