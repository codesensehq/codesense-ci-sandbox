// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useViewerStore } from "./viewer-store";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));

// SW-VE-6: the open tab follows a renamed/moved context file rather than
// stranding on the stale path.
describe("remapTabPath (SW-VE-6)", () => {
  beforeEach(() => {
    useViewerStore.setState({ openTabs: {}, activeTab: {}, navHistory: {}, navIndex: {} });
    vi.clearAllMocks();
  });

  it("repoints an open tab + active pointer + name from old → new path", () => {
    useViewerStore.setState({
      openTabs: { t1: [{ path: "/team/impl/old.md", name: "old" }] },
      activeTab: { t1: "/team/impl/old.md" },
    });
    useViewerStore.getState().remapTabPath("t1", "/team/impl/old.md", "/team/impl/new.md");
    expect(useViewerStore.getState().openTabs.t1).toEqual([
      { path: "/team/impl/new.md", name: "new" },
    ]);
    expect(useViewerStore.getState().activeTab.t1).toBe("/team/impl/new.md");
  });

  it("preserves other tab fields (preview) and leaves untouched tabs alone", () => {
    useViewerStore.setState({
      openTabs: {
        t1: [
          { path: "/a.md", name: "a", preview: true },
          { path: "/b.md", name: "b" },
        ],
      },
      activeTab: { t1: "/b.md" },
    });
    useViewerStore.getState().remapTabPath("t1", "/a.md", "/renamed.md");
    expect(useViewerStore.getState().openTabs.t1).toEqual([
      { path: "/renamed.md", name: "renamed", preview: true },
      { path: "/b.md", name: "b" },
    ]);
    // active pointer untouched — it referenced a different tab
    expect(useViewerStore.getState().activeTab.t1).toBe("/b.md");
  });

  it("remaps a cross-scope move destination (arbitrary new dir)", () => {
    useViewerStore.setState({
      openTabs: { t1: [{ path: "/team-repo/impl/x.md", name: "x" }] },
      activeTab: { t1: "/team-repo/impl/x.md" },
    });
    useViewerStore
      .getState()
      .remapTabPath("t1", "/team-repo/impl/x.md", "/team-workspace/shared/x.md");
    expect(useViewerStore.getState().activeTab.t1).toBe("/team-workspace/shared/x.md");
  });

  it("rewrites nav history so back-nav can't resurrect the stale path", () => {
    useViewerStore.setState({
      openTabs: { t1: [{ path: "/old.md", name: "old" }] },
      activeTab: { t1: "/old.md" },
      navHistory: { t1: ["/prev.md", "/old.md"] },
    });
    useViewerStore.getState().remapTabPath("t1", "/old.md", "/new.md");
    expect(useViewerStore.getState().navHistory.t1).toEqual(["/prev.md", "/new.md"]);
  });

  it("rewrites the keep-alive / recent-files MRU (no stale recent entry)", () => {
    useViewerStore.setState({
      openTabs: { t1: [{ path: "/old.md", name: "old" }] },
      activeTab: { t1: "/old.md" },
      openedViewerPaths: { t1: ["/other.md", "/old.md"] },
    });
    useViewerStore.getState().remapTabPath("t1", "/old.md", "/new.md");
    expect(useViewerStore.getState().openedViewerPaths.t1).toEqual(["/other.md", "/new.md"]);
  });

  it("is a no-op when the file isn't referenced anywhere (never spawns a tab)", () => {
    useViewerStore.setState({ openTabs: { t1: [{ path: "/a.md", name: "a" }] }, activeTab: {} });
    useViewerStore.getState().remapTabPath("t1", "/not-open.md", "/whatever.md");
    expect(useViewerStore.getState().openTabs.t1).toEqual([{ path: "/a.md", name: "a" }]);
  });

  it("cleans a stale MRU entry even when the file is NOT an open tab", () => {
    // file was opened then its tab closed → lingers only in openedViewerPaths
    useViewerStore.setState({
      openTabs: { t1: [{ path: "/b.md", name: "b" }] },
      activeTab: { t1: "/b.md" },
      openedViewerPaths: { t1: ["/old.md", "/b.md"] },
    });
    useViewerStore.getState().remapTabPath("t1", "/old.md", "/new.md");
    // no tab for old.md, but its MRU entry is still repointed
    expect(useViewerStore.getState().openedViewerPaths.t1).toEqual(["/new.md", "/b.md"]);
    expect(useViewerStore.getState().openTabs.t1).toEqual([{ path: "/b.md", name: "b" }]);
  });

  it("is a no-op when old === new", () => {
    useViewerStore.setState({ openTabs: { t1: [{ path: "/a.md", name: "a" }] } });
    useViewerStore.getState().remapTabPath("t1", "/a.md", "/a.md");
    expect(useViewerStore.getState().openTabs.t1).toEqual([{ path: "/a.md", name: "a" }]);
  });
});
