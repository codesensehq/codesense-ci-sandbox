// SPDX-License-Identifier: MIT
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";

vi.mock("@/tauri/commands", () => ({
  listWorkspaceArchiveStates: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

import { listWorkspaceArchiveStates } from "@/tauri/commands";
import { listen } from "@tauri-apps/api/event";

import {
  isWorkspaceArchived,
  mergeArchiveState,
  resetWorkspaceArchiveStoreForTest,
  selectArchivedAtMs,
  useWorkspaceArchiveStore,
} from "./workspace-archive-store";

beforeEach(() => {
  resetWorkspaceArchiveStoreForTest();
  vi.clearAllMocks();
});

afterEach(() => {
  resetWorkspaceArchiveStoreForTest();
});

describe("mergeArchiveState", () => {
  it("returns the same reference when the value is unchanged", () => {
    const base = { "story:A": 100 };
    const result = mergeArchiveState(base, { workspaceId: "story:A", archivedAtMs: 100 });
    expect(result).toBe(base);
  });

  it("adds a new entry without mutating the input", () => {
    const base = { "story:A": 100 };
    const result = mergeArchiveState(base, { workspaceId: "story:B", archivedAtMs: null });
    expect(result).not.toBe(base);
    expect(result).toEqual({ "story:A": 100, "story:B": null });
    // Input unmutated.
    expect(base).toEqual({ "story:A": 100 });
  });

  it("replaces an existing entry when the value changes", () => {
    const base = { "story:A": null };
    const result = mergeArchiveState(base, { workspaceId: "story:A", archivedAtMs: 500 });
    expect(result).toEqual({ "story:A": 500 });
  });
});

describe("isWorkspaceArchived", () => {
  it("treats a number as archived", () => {
    expect(isWorkspaceArchived(123)).toBe(true);
    expect(isWorkspaceArchived(0)).toBe(true);
  });

  it("treats null / undefined as active", () => {
    expect(isWorkspaceArchived(null)).toBe(false);
    expect(isWorkspaceArchived(undefined)).toBe(false);
  });
});

describe("selectArchivedAtMs", () => {
  it("returns null for an unknown workspace id", () => {
    const state = useWorkspaceArchiveStore.getState();
    expect(selectArchivedAtMs(state, "story:missing")).toBeNull();
  });

  it("returns null when the id itself is undefined", () => {
    const state = useWorkspaceArchiveStore.getState();
    expect(selectArchivedAtMs(state, undefined)).toBeNull();
  });

  it("returns the stored value when present", () => {
    useWorkspaceArchiveStore.getState().setAll({ "story:A": 42 });
    const state = useWorkspaceArchiveStore.getState();
    expect(selectArchivedAtMs(state, "story:A")).toBe(42);
  });
});

describe("useWorkspaceArchiveStore.initialize", () => {
  it("primes the map from listWorkspaceArchiveStates and subscribes to the event", async () => {
    vi.mocked(listWorkspaceArchiveStates).mockResolvedValue([
      { workspaceId: "story:A", archivedAtMs: 100 },
      { workspaceId: "root:/x", archivedAtMs: null },
    ]);
    const unlistenFn = vi.fn();
    vi.mocked(listen).mockResolvedValue(unlistenFn as never);

    const cleanup = await useWorkspaceArchiveStore.getState().initialize();

    const state = useWorkspaceArchiveStore.getState();
    expect(state.loaded).toBe(true);
    expect(state.states).toEqual({
      "story:A": 100,
      "root:/x": null,
    });
    expect(listen).toHaveBeenCalledWith(
      "codesense:workspace-archive-changed",
      expect.any(Function),
    );
    expect(typeof cleanup).toBe("function");
  });

  it("marks loaded even when the initial read fails (fail-open)", async () => {
    vi.mocked(listWorkspaceArchiveStates).mockRejectedValue(new Error("boom"));
    const unlistenFn = vi.fn();
    vi.mocked(listen).mockResolvedValue(unlistenFn as never);

    await useWorkspaceArchiveStore.getState().initialize();

    const state = useWorkspaceArchiveStore.getState();
    expect(state.loaded).toBe(true);
    expect(state.states).toEqual({});
  });

  it("applies a change event to the store", async () => {
    vi.mocked(listWorkspaceArchiveStates).mockResolvedValue([]);
    let captured: ((event: { payload: unknown }) => void) | null = null;
    vi.mocked(listen).mockImplementation(((_name: string, handler: unknown) => {
      captured = handler as (event: { payload: unknown }) => void;
      return Promise.resolve(() => undefined);
    }) as never);

    await useWorkspaceArchiveStore.getState().initialize();
    expect(captured).not.toBeNull();

    captured!({ payload: { workspaceId: "story:A", archivedAtMs: 999 } });
    expect(useWorkspaceArchiveStore.getState().states["story:A"]).toBe(999);

    captured!({ payload: { workspaceId: "story:A", archivedAtMs: null } });
    expect(useWorkspaceArchiveStore.getState().states["story:A"]).toBeNull();
  });
});
