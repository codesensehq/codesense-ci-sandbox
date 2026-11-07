// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";

const detectToolsMock = vi.fn();
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  detectTools: () => detectToolsMock(),
}));

import { useDetectedEditors, resetDetectedEditorsCache } from "./use-detected-editors";

const EDITORS = [
  { id: "code", label: "VS Code" },
  { id: "cursor", label: "Cursor" },
];

beforeEach(() => {
  detectToolsMock.mockReset();
  detectToolsMock.mockResolvedValue({
    terminals: [],
    editors: EDITORS,
    default_terminal: "",
    default_editor: "",
  });
  resetDetectedEditorsCache();
});

describe("useDetectedEditors (SLW-WDE-5 R2)", () => {
  it("starts empty then populates from the async detect", async () => {
    const { result } = renderHook(() => useDetectedEditors());
    expect(result.current).toEqual([]);
    await waitFor(() => expect(result.current).toEqual(EDITORS));
  });

  it("caches: two mounts detect ONCE per session", async () => {
    const a = renderHook(() => useDetectedEditors());
    await waitFor(() => expect(a.result.current).toEqual(EDITORS));
    renderHook(() => useDetectedEditors());
    await waitFor(() => expect(detectToolsMock).toHaveBeenCalledTimes(1));
  });

  it("enabled=false skips the detect entirely (R1) and returns []", async () => {
    const { result } = renderHook(() => useDetectedEditors(false));
    // give any (wrongly-scheduled) effect a chance to fire
    await Promise.resolve();
    expect(result.current).toEqual([]);
    expect(detectToolsMock).not.toHaveBeenCalled();
  });

  it("reset clears the cache so a later mount detects again", async () => {
    const a = renderHook(() => useDetectedEditors());
    await waitFor(() => expect(a.result.current).toEqual(EDITORS));
    expect(detectToolsMock).toHaveBeenCalledTimes(1);
    resetDetectedEditorsCache();
    renderHook(() => useDetectedEditors());
    await waitFor(() => expect(detectToolsMock).toHaveBeenCalledTimes(2));
  });

  it("refresh=true re-detects on each mount even when the cache is warm (config-surface freshness)", async () => {
    // warm the cache
    const a = renderHook(() => useDetectedEditors());
    await waitFor(() => expect(a.result.current).toEqual(EDITORS));
    expect(detectToolsMock).toHaveBeenCalledTimes(1);
    // a refresh consumer (e.g. Settings) must pick up a newly-installed editor
    // rather than the stale cache → it forces a fresh detect on mount
    const NEXT = [...EDITORS, { id: "zed", label: "Zed" }];
    detectToolsMock.mockResolvedValue({
      terminals: [],
      editors: NEXT,
      default_terminal: "",
      default_editor: "",
    });
    const b = renderHook(() => useDetectedEditors(true, true));
    await waitFor(() => expect(b.result.current).toEqual(NEXT));
    expect(detectToolsMock).toHaveBeenCalledTimes(2);
    // …and it repopulates the shared cache for cheap (non-refresh) consumers
    const c = renderHook(() => useDetectedEditors());
    await waitFor(() => expect(c.result.current).toEqual(NEXT));
    expect(detectToolsMock).toHaveBeenCalledTimes(2);
  });
});
