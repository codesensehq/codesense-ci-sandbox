// SPDX-License-Identifier: MIT
import { vi, describe, it, expect, beforeEach } from "vitest";
import { useViewerStore } from "./viewer-store";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn(() => Promise.resolve()),
  loadAppState: vi.fn(() => Promise.resolve("{}")),
}));
vi.mock("sonner", () => ({ toast: { info: vi.fn(), error: vi.fn(), success: vi.fn() } }));

const REPO = "/test/repo";

// SW-VE-3: `closeTab` must never silently drop an unsaved deliberate edit.
describe("useViewerStore — SW-VE-3 dirty-edit close guard", () => {
  beforeEach(() => {
    useViewerStore.setState({
      openTabs: {},
      activeTab: {},
      editingDirtyPaths: {},
      navHistory: {},
      navIndex: {},
      openedViewerPaths: {},
    });
  });

  it.each([true, false])(
    "closing an inactive dirty edit (user: %s) retains it and respects selection",
    (user) => {
      const s = useViewerStore.getState();
      s.openFile(REPO, REPO, "a.yaml");
      s.openFile(REPO, REPO, "b.yaml");
      s.setActiveTab(REPO, REPO, "b.yaml");
      s.setEditingDirtyPaths(REPO, ["a.yaml"]);

      s.closeTab(REPO, REPO, "a.yaml", user ? { user: true } : undefined);

      const st = useViewerStore.getState();
      expect(st.openTabs[REPO]?.map((t) => t.path)).toEqual(["a.yaml", "b.yaml"]);
      expect(st.activeTab[REPO]).toBe(user ? "a.yaml" : "b.yaml");
    },
  );

  it("closes normally once the edit is no longer dirty (guard cleared)", () => {
    const s = useViewerStore.getState();
    s.openFile(REPO, REPO, "a.yaml");
    s.openFile(REPO, REPO, "b.yaml");
    s.setEditingDirtyPaths(REPO, ["a.yaml"]);
    s.setEditingDirtyPaths(REPO, []); // saved / discarded

    s.closeTab(REPO, REPO, "a.yaml");

    const st = useViewerStore.getState();
    expect(st.openTabs[REPO]?.map((t) => t.path) ?? []).not.toContain("a.yaml");
  });

  it("closeOtherTabs refuses when another tab holds a dirty edit (no silent drop)", () => {
    const s = useViewerStore.getState();
    s.openFile(REPO, REPO, "a.yaml");
    s.openFile(REPO, REPO, "b.yaml");
    s.setEditingDirtyPaths(REPO, ["a.yaml"]);

    // Keep b.yaml, which would close the dirty a.yaml → refused, nothing closes.
    s.closeOtherTabs(REPO, REPO, "b.yaml");

    const paths = useViewerStore.getState().openTabs[REPO]?.map((t) => t.path) ?? [];
    expect(paths).toContain("a.yaml");
    expect(paths).toContain("b.yaml");
  });

  it("closeOtherTabs proceeds when the kept tab IS the dirty edit", () => {
    const s = useViewerStore.getState();
    s.openFile(REPO, REPO, "a.yaml");
    s.openFile(REPO, REPO, "b.yaml");
    s.setEditingDirtyPaths(REPO, ["a.yaml"]);

    s.closeOtherTabs(REPO, REPO, "a.yaml"); // keeping the dirty edit is fine

    const paths = useViewerStore.getState().openTabs[REPO]?.map((t) => t.path) ?? [];
    expect(paths).toEqual(["a.yaml"]);
  });

  it("closeAllTabs refuses while any dirty edit is open", () => {
    const s = useViewerStore.getState();
    s.openFile(REPO, REPO, "a.yaml");
    s.openFile(REPO, REPO, "b.yaml");
    s.setEditingDirtyPaths(REPO, ["a.yaml"]);

    s.closeAllTabs(REPO);

    const paths = useViewerStore.getState().openTabs[REPO]?.map((t) => t.path) ?? [];
    expect(paths).toContain("a.yaml");
    expect(paths).toContain("b.yaml");
  });

  it("closeAllTabs proceeds normally with no dirty edits", () => {
    const s = useViewerStore.getState();
    s.openFile(REPO, REPO, "a.yaml");
    s.openFile(REPO, REPO, "b.yaml");

    s.closeAllTabs(REPO);

    expect(useViewerStore.getState().openTabs[REPO] ?? []).toHaveLength(0);
  });

  it("guards each of several concurrently-edited tabs", () => {
    const s = useViewerStore.getState();
    s.openFile(REPO, REPO, "a.yaml");
    s.openFile(REPO, REPO, "b.yaml");
    s.openFile(REPO, REPO, "c.yaml");
    s.setEditingDirtyPaths(REPO, ["a.yaml", "b.yaml"]);

    // Both dirty edits are protected; the non-edited tab closes normally.
    s.closeTab(REPO, REPO, "a.yaml");
    s.closeTab(REPO, REPO, "b.yaml");
    s.closeTab(REPO, REPO, "c.yaml");

    const paths = useViewerStore.getState().openTabs[REPO]?.map((t) => t.path) ?? [];
    expect(paths).toContain("a.yaml");
    expect(paths).toContain("b.yaml");
    expect(paths).not.toContain("c.yaml");
  });
});
