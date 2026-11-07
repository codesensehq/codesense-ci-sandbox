// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";

import { useViewerStore, resetViewerPrefsPersistence } from "./viewer-store";
import * as commands from "@/tauri/commands";
import { applyFieldPatch } from "@/shared/lib/field-patch";
import {
  registerVirtualTab,
  __resetVirtualTabsForTest,
} from "@/shared/lib/virtual-viewer-registry";

// A virtual viewer tab is a synthetic entry in the open-tabs list (its path
// uses the `virtual://` scheme) rather than a disk-backed doc. These tests pin
// the two store-level invariants:
//   - AC-11a: virtual tabs are NEVER persisted (a restored sentinel would route
//     to the markdown viewer and fail readFile → a broken tab every launch).
//   - AC-12: opening is a singleton (re-open focuses, never duplicates) and the
//     tab shows its registered title, not a path-derived basename.

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  saveScratchNote: vi.fn().mockResolvedValue(undefined),
  loadScratchNote: vi.fn().mockResolvedValue(""),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
}));

const saveAppStateMock = commands.saveAppState as ReturnType<typeof vi.fn>;

const TEAM = "acme:default";
const VIRTUAL = "virtual://notes";

describe("viewer-store virtual tabs @P1", () => {
  beforeEach(() => {
    __resetVirtualTabsForTest();
    saveAppStateMock.mockReset().mockResolvedValue(undefined);
    useViewerStore.setState({ openTabs: {}, activeTab: {} });
  });

  it("AC-11a: virtual tabs are excluded from persistence", async () => {
    useViewerStore.setState({
      openTabs: {
        [TEAM]: [
          { path: "/team/real.md", name: "real" },
          { path: VIRTUAL, name: "Notes" },
        ],
      },
      activeTab: { [TEAM]: VIRTUAL },
    });

    let written = "";
    saveAppStateMock.mockImplementation(async (payload: string) => {
      written = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((r) => setTimeout(r, 400));

    const saved = JSON.parse(written).viewer;
    expect(saved.openTabs[TEAM]).toEqual([{ path: "/team/real.md", name: "real" }]);
    // activeTab pointed at the virtual path → falls back to the first surviving
    // real tab so the team doesn't reopen blank (not dropped entirely).
    expect(saved.activeTab[TEAM]).toBe("/team/real.md");
  });

  it("AC-11a: a lone virtual tab persists nothing and leaves no active pointer", async () => {
    const writes: string[] = [];
    saveAppStateMock.mockImplementation(async (payload: string) => {
      writes.push(payload);
    });
    const flush = async () => {
      useViewerStore.getState().persistState();
      await new Promise((r) => setTimeout(r, 400));
    };

    // Baseline: a real tab is on disk, so the lone-virtual save has something to remove.
    // Reset the module-level persisted snapshot so the first write is the full
    // state rather than an empty diff against the previous test's leftovers.
    resetViewerPrefsPersistence();
    useViewerStore.setState({
      openTabs: { [TEAM]: [{ path: "/team/real.md", name: "real" }] },
      activeTab: { [TEAM]: "/team/real.md" },
    });
    await flush();
    const baseline = applyFieldPatch({}, JSON.parse(writes[0]!)) as {
      viewer: { openTabs: Record<string, unknown>; activeTab: Record<string, unknown> };
    };
    expect(baseline.viewer.openTabs[TEAM]).toEqual([{ path: "/team/real.md", name: "real" }]);
    expect(baseline.viewer.activeTab[TEAM]).toBe("/team/real.md");

    useViewerStore.setState({
      openTabs: { [TEAM]: [{ path: VIRTUAL, name: "Notes" }] },
      activeTab: { [TEAM]: VIRTUAL },
    });
    await flush();
    expect(writes).toHaveLength(2);

    // The save is a field patch (null removes a key); merge it as the backend does.
    const saved = (
      applyFieldPatch(baseline, JSON.parse(writes[1]!)) as {
        viewer: { openTabs: Record<string, unknown>; activeTab: Record<string, unknown> };
      }
    ).viewer;
    expect(saved.openTabs[TEAM]).toBeUndefined();
    expect(saved.activeTab[TEAM]).toBeUndefined();
  });

  it("AC-12: opening the virtual tab is a singleton with its registered title", () => {
    registerVirtualTab(VIRTUAL, "Notes", () => null as never);
    const store = useViewerStore.getState();

    store.openFile(TEAM, "repo", VIRTUAL);
    store.openFile(TEAM, "repo", VIRTUAL); // re-open must focus, not duplicate

    const tabs = useViewerStore.getState().openTabs[TEAM] ?? [];
    const virtualTabs = tabs.filter((t) => t.path === VIRTUAL);
    expect(virtualTabs).toHaveLength(1);
    expect(virtualTabs[0]?.name).toBe("Notes");
    expect(useViewerStore.getState().activeTab[TEAM]).toBe(VIRTUAL);
  });
});

// Persisting is a no-op until the saved state has loaded (SW-FU-B2).
beforeEach(() => {
  useViewerStore.setState({ stateLoaded: true });
});
