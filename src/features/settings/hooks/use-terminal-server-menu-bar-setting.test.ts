// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";

// Spy the setting-push wrapper; the same mock also satisfies viewer-store's
// module-load deps (Tauri commands are absent in the test shell).
vi.mock("@/tauri/commands", () => ({
  setTerminalServerMenuBar: vi.fn().mockResolvedValue(undefined),
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

import { setTerminalServerMenuBar } from "@/tauri/commands";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useTerminalServerMenuBarSetting } from "./use-terminal-server-menu-bar-setting";

const push = vi.mocked(setTerminalServerMenuBar);

describe("useTerminalServerMenuBarSetting (SV-8)", () => {
  beforeEach(() => {
    push.mockClear();
    act(() => {
      useViewerStore.getState().updateSettings({ terminalServerMenuBar: false });
    });
  });

  it("is off by default and pushes that on mount", async () => {
    renderHook(() => useTerminalServerMenuBarSetting());
    await waitFor(() => expect(push).toHaveBeenCalled());
    expect(push).toHaveBeenLastCalledWith(false);
  });

  it("pushes at once when the setting is turned on, and again when turned off", async () => {
    renderHook(() => useTerminalServerMenuBarSetting());
    await waitFor(() => expect(push).toHaveBeenCalledTimes(1));

    act(() => {
      useViewerStore.getState().updateSettings({ terminalServerMenuBar: true });
    });
    await waitFor(() => expect(push).toHaveBeenCalledTimes(2));
    expect(push).toHaveBeenLastCalledWith(true);

    act(() => {
      useViewerStore.getState().updateSettings({ terminalServerMenuBar: false });
    });
    await waitFor(() => expect(push).toHaveBeenCalledTimes(3));
    expect(push).toHaveBeenLastCalledWith(false);
  });

  it("resolves an undefined setting to off", async () => {
    act(() => {
      useViewerStore.getState().updateSettings({ terminalServerMenuBar: undefined });
    });
    push.mockClear();
    renderHook(() => useTerminalServerMenuBarSetting());
    await waitFor(() => expect(push).toHaveBeenCalled());
    expect(push).toHaveBeenLastCalledWith(false);
  });

  it("does not re-push when an unrelated setting changes", async () => {
    renderHook(() => useTerminalServerMenuBarSetting());
    await waitFor(() => expect(push).toHaveBeenCalledTimes(1));
    act(() => {
      useViewerStore.getState().updateSettings({ starshipPreset: "plain" });
    });
    await Promise.resolve();
    expect(push).toHaveBeenCalledTimes(1);
  });
});
