// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";

const reconfigureWarmPool = vi.fn().mockResolvedValue(undefined);
const setTerminalShellOverride = vi.fn().mockResolvedValue(undefined);
vi.mock("@/tauri/commands", () => ({
  reconfigureWarmPool: (...args: unknown[]) => reconfigureWarmPool(...args),
  setTerminalShellOverride: (...args: unknown[]) => setTerminalShellOverride(...args),
  // viewer-store module-load deps:
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
  detectTools: vi.fn().mockResolvedValue({
    terminals: [],
    editors: [],
    default_terminal: "",
    default_editor: "",
  }),
}));

// Import AFTER the mock so the hook resolves against it.
import { useWarmPool } from "./use-warm-pool";
import { useViewerStore } from "@/shared/stores/viewer-store";

describe("useWarmPool", () => {
  beforeEach(() => {
    reconfigureWarmPool.mockClear();
    setTerminalShellOverride.mockClear();
    // Known baseline: default pool size 2, compact preset, integration on.
    act(() => {
      useViewerStore.getState().updateSettings({
        terminalWarmPoolSize: 2,
        starshipPreset: "compact",
        shellIntegration: true,
        terminalShell: undefined,
      });
    });
  });

  it("fills the pool at launch with the current settings (AC-1)", async () => {
    renderHook(() => useWarmPool());
    await waitFor(() => expect(reconfigureWarmPool).toHaveBeenCalled());
    expect(reconfigureWarmPool).toHaveBeenLastCalledWith(2, "compact", true);
  });

  it("re-fires when the pool size changes (AC-15)", async () => {
    renderHook(() => useWarmPool());
    await waitFor(() => expect(reconfigureWarmPool).toHaveBeenCalledTimes(1));

    act(() => {
      useViewerStore.getState().updateSettings({ terminalWarmPoolSize: 0 });
    });
    await waitFor(() => expect(reconfigureWarmPool).toHaveBeenCalledTimes(2));
    expect(reconfigureWarmPool).toHaveBeenLastCalledWith(0, "compact", true);
  });

  it("re-fires when the starship preset changes (AC-15)", async () => {
    renderHook(() => useWarmPool());
    await waitFor(() => expect(reconfigureWarmPool).toHaveBeenCalledTimes(1));

    act(() => {
      useViewerStore.getState().updateSettings({ starshipPreset: "plain" });
    });
    await waitFor(() => expect(reconfigureWarmPool).toHaveBeenCalledTimes(2));
    expect(reconfigureWarmPool).toHaveBeenLastCalledWith(2, "plain", true);
  });

  it("defaults to size 2 when the setting is unset", async () => {
    act(() => {
      useViewerStore.getState().updateSettings({ terminalWarmPoolSize: undefined });
    });
    reconfigureWarmPool.mockClear();
    renderHook(() => useWarmPool());
    await waitFor(() => expect(reconfigureWarmPool).toHaveBeenCalled());
    expect(reconfigureWarmPool).toHaveBeenLastCalledWith(2, "compact", true);
  });

  // SW-LN-35: the override reaches Rust BEFORE the pool refills, so the new
  // warm members run the chosen shell.
  it("pushes the shell override, then reconfigures the pool", async () => {
    renderHook(() => useWarmPool());
    await waitFor(() => expect(reconfigureWarmPool).toHaveBeenCalledTimes(1));
    expect(setTerminalShellOverride).toHaveBeenLastCalledWith(undefined);

    act(() => {
      useViewerStore.getState().updateSettings({ terminalShell: "/bin/bash" });
    });
    await waitFor(() => expect(reconfigureWarmPool).toHaveBeenCalledTimes(2));
    expect(setTerminalShellOverride).toHaveBeenLastCalledWith("/bin/bash");
    const overrideOrder = setTerminalShellOverride.mock.invocationCallOrder.at(-1)!;
    const reconfigureOrder = reconfigureWarmPool.mock.invocationCallOrder.at(-1)!;
    expect(overrideOrder).toBeLessThan(reconfigureOrder);
  });

  it("still fills the pool when the override call fails", async () => {
    setTerminalShellOverride.mockRejectedValueOnce(new Error("no tauri"));
    renderHook(() => useWarmPool());
    await waitFor(() => expect(reconfigureWarmPool).toHaveBeenCalledTimes(1));
  });
});
