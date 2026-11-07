// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";

// Spy the setting-push wrapper; the same mock also satisfies viewer-store's
// module-load deps (Tauri commands are absent in the test shell).
vi.mock("@/tauri/commands", () => ({
  setQuitOnClose: vi.fn().mockResolvedValue(undefined),
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

import { setQuitOnClose } from "@/tauri/commands";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useQuitOnCloseSetting } from "./use-quit-on-close-setting";

const setPref = vi.mocked(setQuitOnClose);

describe("useQuitOnCloseSetting (SW-AU-34)", () => {
  beforeEach(() => {
    setPref.mockClear();
    // Start each test from the default-off (background-on-close) state.
    act(() => {
      useViewerStore.getState().updateSettings({ quitOnClose: false });
    });
  });

  it("pushes the setting to Rust on mount (false → false)", async () => {
    renderHook(() => useQuitOnCloseSetting());
    await waitFor(() => expect(setPref).toHaveBeenCalled());
    expect(setPref).toHaveBeenLastCalledWith(false);
  });

  it("re-pushes live when the setting changes (false → true)", async () => {
    renderHook(() => useQuitOnCloseSetting());
    await waitFor(() => expect(setPref).toHaveBeenCalledTimes(1));

    act(() => {
      useViewerStore.getState().updateSettings({ quitOnClose: true });
    });
    await waitFor(() => expect(setPref).toHaveBeenCalledTimes(2));
    expect(setPref).toHaveBeenLastCalledWith(true);
  });

  it("resolves an undefined setting to false (fail-safe to background-on-close)", async () => {
    act(() => {
      // An unset/corrupt read must gate to the shipped default (background).
      useViewerStore.getState().updateSettings({ quitOnClose: undefined });
    });
    setPref.mockClear();
    renderHook(() => useQuitOnCloseSetting());
    await waitFor(() => expect(setPref).toHaveBeenCalled());
    expect(setPref).toHaveBeenLastCalledWith(false);
  });

  it("does NOT re-push when an unrelated setting changes", async () => {
    renderHook(() => useQuitOnCloseSetting());
    await waitFor(() => expect(setPref).toHaveBeenCalledTimes(1));

    act(() => {
      useViewerStore.getState().updateSettings({ starshipPreset: "plain" });
    });
    // Give any stray effect a chance to fire, then assert it did NOT.
    await Promise.resolve();
    expect(setPref).toHaveBeenCalledTimes(1);
  });
});
