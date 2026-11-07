// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";

import { useSprintRegistry } from "./use-sprint-registry";
import { loadSprintRegistry } from "@/tauri/commands";
import { useOrgStore } from "@/shared/stores/org-store";
import type { SprintRegistry } from "@/tauri/commands";

vi.mock("@/tauri/commands", () => ({
  loadSprintRegistry: vi.fn(),
}));

const loadMock = vi.mocked(loadSprintRegistry);

const REGISTRY: SprintRegistry = {
  activeSprint: "2026-08-A",
  sprints: [{ id: "2026-08-A", state: "active", goal: "", focusEpics: [], start: "", end: "" }],
  warnings: [],
};

function seedOrg(orgRoot: string | null = "/orgs/acme") {
  useOrgStore.setState({
    orgs: orgRoot
      ? ([{ slug: "acme", org_root: orgRoot }] as unknown as ReturnType<
          typeof useOrgStore.getState
        >["orgs"])
      : [],
  });
}

beforeEach(() => {
  loadMock.mockReset();
  seedOrg("/orgs/acme");
});

describe("useSprintRegistry", () => {
  it("loads the registry for a resolvable team and settles loading", async () => {
    loadMock.mockResolvedValue(REGISTRY);
    const { result } = renderHook(() => useSprintRegistry("acme:default"));

    expect(result.current.loading).toBe(true); // fires after paint
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.registry).toEqual(REGISTRY);
    expect(loadMock).toHaveBeenCalledWith("/orgs/acme", "default");
  });

  it("skips the load (settled, null) when there is no team name", async () => {
    const { result } = renderHook(() => useSprintRegistry("acme:_none"));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.registry).toBeNull();
    expect(loadMock).not.toHaveBeenCalled();
  });

  it("stays LOADING (does not settle) while a real team's org is still hydrating", async () => {
    seedOrg(null); // org store not hydrated for a real team
    const { result } = renderHook(() => useSprintRegistry("acme:default"));
    // No doomed invoke, and crucially loading stays true so the board holds its
    // spinner rather than flashing a "no active sprint" CTA (round-2 fix).
    await Promise.resolve();
    expect(result.current.loading).toBe(true);
    expect(result.current.registry).toBeNull();
    expect(loadMock).not.toHaveBeenCalled();
  });

  it("retries once the org store hydrates after mount, with no false-settle window", async () => {
    seedOrg(null); // orgs absent at mount
    loadMock.mockResolvedValue(REGISTRY);
    const { result } = renderHook(() => useSprintRegistry("acme:default"));
    expect(result.current.loading).toBe(true);
    expect(loadMock).not.toHaveBeenCalled();

    // Orgs hydrate later — the reactive org-root subscription re-fires the load.
    // loading must remain true across the hydration re-fire (no CTA flash).
    act(() => seedOrg("/orgs/acme"));
    expect(result.current.loading).toBe(true);
    await waitFor(() => expect(loadMock).toHaveBeenCalledWith("/orgs/acme", "default"));
    await waitFor(() => expect(result.current.registry).toEqual(REGISTRY));
    expect(result.current.loading).toBe(false);
  });

  it("degrades to an empty registry on an IPC fault (never throws)", async () => {
    loadMock.mockRejectedValue(new Error("ipc down"));
    const { result } = renderHook(() => useSprintRegistry("acme:default"));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.registry).toEqual({ activeSprint: null, sprints: [], warnings: [] });
  });

  it("re-loads when teamKey changes", async () => {
    loadMock.mockResolvedValue(REGISTRY);
    const { result, rerender } = renderHook(({ tk }) => useSprintRegistry(tk), {
      initialProps: { tk: "acme:default" },
    });
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(loadMock).toHaveBeenCalledWith("/orgs/acme", "default");

    rerender({ tk: "acme:other" });
    await waitFor(() => expect(loadMock).toHaveBeenCalledWith("/orgs/acme", "other"));
  });
});
