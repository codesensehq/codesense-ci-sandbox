// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { selectActiveTerminalTab, useViewerStore } from "./viewer-store";
import { seedTerminalTabs, installTerminalLayoutHarness } from "@/test/terminal-layout-harness";

// Mock Tauri commands used by the store
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
  detectTools: vi.fn().mockResolvedValue({
    terminals: [],
    editors: [],
    default_terminal: "",
    default_editor: "",
  }),
}));

installTerminalLayoutHarness();

describe("cycleTab", () => {
  const repo = "/test/repo";

  beforeEach(() => {
    useViewerStore.setState({
      openTabs: {},
      activeTab: {},
      workspaces: {},
      activeWorkspace: {},
    });
  });

  describe("file tabs", () => {
    it("advances to next file tab", () => {
      useViewerStore.setState({
        openTabs: {
          [repo]: [
            { path: "/a.md", name: "a.md" },
            { path: "/b.md", name: "b.md" },
            { path: "/c.md", name: "c.md" },
          ],
        },
        activeTab: { [repo]: "/a.md" },
      });

      useViewerStore.getState().cycleTab(repo, "file", 1);
      expect(useViewerStore.getState().activeTab[repo]).toBe("/b.md");
    });

    it("goes to previous file tab", () => {
      useViewerStore.setState({
        openTabs: {
          [repo]: [
            { path: "/a.md", name: "a.md" },
            { path: "/b.md", name: "b.md" },
            { path: "/c.md", name: "c.md" },
          ],
        },
        activeTab: { [repo]: "/b.md" },
      });

      useViewerStore.getState().cycleTab(repo, "file", -1);
      expect(useViewerStore.getState().activeTab[repo]).toBe("/a.md");
    });

    it("wraps forward at last tab", () => {
      useViewerStore.setState({
        openTabs: {
          [repo]: [
            { path: "/a.md", name: "a.md" },
            { path: "/b.md", name: "b.md" },
          ],
        },
        activeTab: { [repo]: "/b.md" },
      });

      useViewerStore.getState().cycleTab(repo, "file", 1);
      expect(useViewerStore.getState().activeTab[repo]).toBe("/a.md");
    });

    it("wraps backward at first tab", () => {
      useViewerStore.setState({
        openTabs: {
          [repo]: [
            { path: "/a.md", name: "a.md" },
            { path: "/b.md", name: "b.md" },
          ],
        },
        activeTab: { [repo]: "/a.md" },
      });

      useViewerStore.getState().cycleTab(repo, "file", -1);
      expect(useViewerStore.getState().activeTab[repo]).toBe("/b.md");
    });

    it("no-ops with 0 tabs", () => {
      useViewerStore.setState({
        openTabs: { [repo]: [] },
        activeTab: { [repo]: "" },
      });

      useViewerStore.getState().cycleTab(repo, "file", 1);
      expect(useViewerStore.getState().activeTab[repo]).toBe("");
    });

    it("no-ops with 1 tab", () => {
      useViewerStore.setState({
        openTabs: { [repo]: [{ path: "/a.md", name: "a.md" }] },
        activeTab: { [repo]: "/a.md" },
      });

      useViewerStore.getState().cycleTab(repo, "file", 1);
      expect(useViewerStore.getState().activeTab[repo]).toBe("/a.md");
    });
  });

  describe("terminal tabs", () => {
    // T.2: the active workspace must be explicitly set to a valid root id;
    // `cycleTab` reads via `s.activeWorkspace[teamKey] ?? firstRootIdForTeam(...)`,
    // and `firstRootIdForTeam` returns a sentinel when workspaces is empty.
    // Seed `workspaces` with a root carrying this id so the fallback also
    // works, and set `activeWorkspace[repo]` to the same id for clarity.
    const ROOT = "root:test/repo";
    const seedRoot = () => ({
      workspaces: {
        [repo]: [{ id: ROOT, kind: "root" as const, cwd: "", status: "ready" as const }],
      },
      activeWorkspace: { [repo]: ROOT },
    });
    const wsActiveTab = () => {
      const s = useViewerStore.getState();
      return selectActiveTerminalTab(s, repo, s.activeWorkspace[repo] ?? ROOT);
    };

    it("advances to next terminal tab", () => {
      useViewerStore.setState(seedRoot());
      seedTerminalTabs(repo, ROOT, ["t1", "t2"], { active: "t1" });

      useViewerStore.getState().cycleTab(repo, "terminal", 1);
      expect(wsActiveTab()).toBe("t2");
    });

    it("wraps terminal tabs", () => {
      useViewerStore.setState(seedRoot());
      seedTerminalTabs(repo, ROOT, ["t1", "t2"], { active: "t2" });

      useViewerStore.getState().cycleTab(repo, "terminal", 1);
      expect(wsActiveTab()).toBe("t1");
    });

    it("no-ops with 1 terminal tab", () => {
      useViewerStore.setState(seedRoot());
      seedTerminalTabs(repo, ROOT, ["t1"], { active: "t1" });

      useViewerStore.getState().cycleTab(repo, "terminal", 1);
      expect(wsActiveTab()).toBe("t1");
    });
  });
});
