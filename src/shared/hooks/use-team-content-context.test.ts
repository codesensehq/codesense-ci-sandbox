// SPDX-License-Identifier: MIT
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { useOrgStore } from "@/shared/stores/org-store";
import { useTeamContentContext } from "./use-team-content-context";
import type { Org } from "@/shared/types/domain";
const org: Org = {
  slug: "acme",
  display_name: "Acme",
  org_root: "/plans",
  created_at: "",
  remote_url: null,
  config_flags: {},
  people: [],
  teams: [{ name: "planning", source_repos: [] }],
};
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});
it("waits for org discovery even when persisted selection already loaded", () => {
  useOrgStore.setState({ orgs: [], stateLoaded: true, discoveryComplete: false });
  expect(renderHook(() => useTeamContentContext("acme:planning")).result.current.kind).toBe(
    "loading",
  );
});
it("latches a ten-second failure through late discovery and resets on an explicit discovery attempt", () => {
  vi.useFakeTimers();
  useOrgStore.setState({ orgs: [], stateLoaded: true, discoveryComplete: false });
  const { result } = renderHook(() => useTeamContentContext("acme:planning"));
  act(() => {
    vi.advanceTimersByTime(10000);
  });
  expect(result.current.kind).toBe("unavailable");
  act(() => useOrgStore.setState({ orgs: [org], discoveryComplete: true }));
  expect(result.current.kind).toBe("unavailable");
  act(() => useOrgStore.getState().beginDiscovery());
  expect(result.current.kind).toBe("ready");
});
