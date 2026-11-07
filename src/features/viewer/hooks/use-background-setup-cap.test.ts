// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";

// Spy the cap setter but keep the REAL resolver (integration through the tiers).
vi.mock("@/shared/lib/worktree/background-setup-fanout", async (importActual) => {
  const actual =
    await importActual<typeof import("@/shared/lib/worktree/background-setup-fanout")>();
  return { ...actual, setBackgroundSetupCap: vi.fn() };
});

// viewer-store module-load deps (Tauri commands are absent in the test shell).
vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  listNotes: vi.fn().mockResolvedValue([]),
  readNote: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0, content: "" }),
  createNote: vi.fn().mockResolvedValue({
    id: "n2",
    title: "Untitled",
    pinned: false,
    lastOpenedAt: 0,
    content: "",
  }),
  writeNote: vi.fn().mockResolvedValue(undefined),
  updateNoteMeta: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0 }),
  deleteNote: vi.fn().mockResolvedValue([]),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  detectTools: vi
    .fn()
    .mockResolvedValue({ terminals: [], editors: [], default_terminal: "", default_editor: "" }),
}));

import { setBackgroundSetupCap } from "@/shared/lib/worktree/background-setup-fanout";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useBackgroundSetupCap } from "./use-background-setup-cap";

const setCap = vi.mocked(setBackgroundSetupCap);

/** Force a known core count so the resolver is deterministic (8 cores → auto=3). */
function stubCores(n: number | undefined) {
  Object.defineProperty(navigator, "hardwareConcurrency", {
    configurable: true,
    value: n,
  });
}

describe("useBackgroundSetupCap (RF-perf-14d Prong A2)", () => {
  beforeEach(() => {
    setCap.mockClear();
    stubCores(8);
    act(() => {
      useViewerStore.getState().updateSettings({ worktreeSetupParallelism: "auto" });
    });
  });

  afterEach(() => {
    stubCores(undefined);
  });

  it("sizes the gate to the machine at launch under Auto (8 cores → 6, RF-perf-23 raised tiers)", async () => {
    renderHook(() => useBackgroundSetupCap());
    await waitFor(() => expect(setCap).toHaveBeenCalled());
    expect(setCap).toHaveBeenLastCalledWith(6);
  });

  it("Auto scales up on a strong machine (a 14-core Mac reports ~12 to the webview → 8, was 3 pre-RF-perf-23)", async () => {
    // navigator.hardwareConcurrency under-reports on WebKit; a 14-core box
    // feeds the resolver ~12 and used to resolve to 3. Raised tiers now give 8.
    stubCores(12);
    setCap.mockClear();
    renderHook(() => useBackgroundSetupCap());
    await waitFor(() => expect(setCap).toHaveBeenCalled());
    expect(setCap).toHaveBeenLastCalledWith(8);
  });

  it('applies the unbounded sentinel live when the setting is "off" (RF-perf-23 AC-1)', async () => {
    renderHook(() => useBackgroundSetupCap());
    await waitFor(() => expect(setCap).toHaveBeenCalledTimes(1));

    act(() => {
      useViewerStore.getState().updateSettings({ worktreeSetupParallelism: "off" });
    });
    await waitFor(() => expect(setCap).toHaveBeenCalledTimes(2));
    expect(setCap).toHaveBeenLastCalledWith(Number.POSITIVE_INFINITY);
  });

  it("applies a large custom value live (12 → 12)", async () => {
    renderHook(() => useBackgroundSetupCap());
    await waitFor(() => expect(setCap).toHaveBeenCalledTimes(1));

    act(() => {
      useViewerStore.getState().updateSettings({ worktreeSetupParallelism: 12 });
    });
    await waitFor(() => expect(setCap).toHaveBeenCalledTimes(2));
    expect(setCap).toHaveBeenLastCalledWith(12);
  });

  it("re-applies live when the setting changes (Auto → explicit 2)", async () => {
    renderHook(() => useBackgroundSetupCap());
    await waitFor(() => expect(setCap).toHaveBeenCalledTimes(1));

    act(() => {
      useViewerStore.getState().updateSettings({ worktreeSetupParallelism: 2 });
    });
    await waitFor(() => expect(setCap).toHaveBeenCalledTimes(2));
    expect(setCap).toHaveBeenLastCalledWith(2);
  });

  it("does NOT re-apply when an unrelated setting changes", async () => {
    renderHook(() => useBackgroundSetupCap());
    await waitFor(() => expect(setCap).toHaveBeenCalledTimes(1));

    act(() => {
      useViewerStore.getState().updateSettings({ starshipPreset: "plain" });
    });
    // Give any stray effect a chance to fire, then assert it did NOT.
    await Promise.resolve();
    expect(setCap).toHaveBeenCalledTimes(1);
  });

  it("falls back to the shipped default (3) when hardwareConcurrency is unknown", async () => {
    stubCores(undefined);
    setCap.mockClear();
    renderHook(() => useBackgroundSetupCap());
    await waitFor(() => expect(setCap).toHaveBeenCalled());
    expect(setCap).toHaveBeenLastCalledWith(3);
  });
});
