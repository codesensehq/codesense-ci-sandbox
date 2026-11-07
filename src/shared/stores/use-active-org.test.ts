// SPDX-License-Identifier: MIT
import { renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@/shared/stores/org-store", () => ({
  useOrgStore: vi.fn(),
}));

import type { Org } from "@/shared/types/domain";
import { useOrgStore } from "@/shared/stores/org-store";
import { useActiveOrg } from "./use-active-org";

const mockUseOrgStore = vi.mocked(useOrgStore);

interface OrgStoreShape {
  orgs: Org[];
  activeOrgSlug: string | null;
}

function org(slug: string): Org {
  return {
    slug,
    display_name: slug,
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {},
    people: [],
    teams: [],
    org_root: `/home/dev/.codesense/users/42/orgs/${slug}`,
  };
}

function stub(state: OrgStoreShape): void {
  mockUseOrgStore.mockImplementation((selector?: unknown) => {
    if (typeof selector === "function") {
      return (selector as (s: OrgStoreShape) => unknown)(state);
    }
    return state as unknown;
  });
}

afterEach(() => {
  vi.clearAllMocks();
});

describe("useActiveOrg", () => {
  it("returns null when no org is active", () => {
    stub({ orgs: [org("acme")], activeOrgSlug: null });
    const { result } = renderHook(() => useActiveOrg());
    expect(result.current).toBeNull();
  });

  it("returns null when the active slug is not in the orgs list", () => {
    stub({ orgs: [org("acme")], activeOrgSlug: "ghost" });
    const { result } = renderHook(() => useActiveOrg());
    expect(result.current).toBeNull();
  });

  it("returns the matching org for the active slug", () => {
    stub({ orgs: [org("acme"), org("beta")], activeOrgSlug: "beta" });
    const { result } = renderHook(() => useActiveOrg());
    expect(result.current?.slug).toBe("beta");
    expect(result.current?.org_root).toBe("/home/dev/.codesense/users/42/orgs/beta");
  });
});
