// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
const storage = vi.hoisted(() => ({ raw: "{}" }));
vi.mock("@/tauri/commands", () => ({
  loadAppState: async () => storage.raw,
  saveAppState: async (raw: string) => {
    storage.raw = raw;
  },
}));
import {
  readDashboardPrefs,
  writeDashboardPrefs,
  DASHBOARD_PREFS_MAX_BYTES,
} from "./dashboard-prefs";
import { readSharedUiState, writeSharedUiState } from "./shared-ui-state";
beforeEach(() => {
  storage.raw = "{}";
});
it("persists shared UI state separately from private page prefs with the same byte limit", async () => {
  await Promise.all([
    writeSharedUiState("right", { selected: "a" }),
    writeDashboardPrefs("right", { selected: "private" }),
  ]);
  expect(await readSharedUiState("right")).toEqual({ selected: "a" });
  expect(await readDashboardPrefs("right")).toEqual({ selected: "private" });
  await expect(
    writeSharedUiState("right", { large: "x".repeat(DASHBOARD_PREFS_MAX_BYTES) }),
  ).rejects.toThrow("too-large");
});

it("uses the native expansion map as file-nav's sole backing store, with team isolation", async () => {
  const { useViewerStore } = await import("@/shared/stores/viewer-store");
  const a = JSON.stringify(["ui:file-nav", "team:acme/a"]);
  const b = JSON.stringify(["ui:file-nav", "team:acme/b"]);
  useViewerStore.setState({
    statusNavExpandedPaths: { "acme:a": ["/a/one"], "acme:b": ["/b/two"] },
  });
  expect((await readSharedUiState(a)).expanded).toEqual(["/a/one"]);
  await writeSharedUiState(a, { expanded: ["/a/three"], scope: "story:S-1" });
  expect(useViewerStore.getState().statusNavExpandedPaths["acme:a"]).toEqual(["/a/three"]);
  expect((await readSharedUiState(b)).expanded).toEqual(["/b/two"]);
  const stored = JSON.parse(storage.raw).liveHubUi[a].data;
  expect(stored.expanded).toBeUndefined();
  expect(stored.scope).toBe("story:S-1");
  useViewerStore.getState().toggleStatusNavExpanded("acme:a", "/a/four");
  expect((await readSharedUiState(a)).expanded).toEqual(["/a/three", "/a/four"]);
});

it("shares native source changes and dashboard scope writes in both directions", async () => {
  const { useViewerStore } = await import("@/shared/stores/viewer-store");
  const key = JSON.stringify(["ui:file-nav", "team:acme/a"]);
  useViewerStore.getState().setNavLens("acme:a", { source: { kind: "private" } });
  expect((await readSharedUiState(key)).scope).toBe("private");
  useViewerStore.getState().setNavLens("acme:a", { source: { kind: "clone" } });
  expect((await readSharedUiState(key)).scope).toBe("team");
  await writeSharedUiState(key, { scope: "private" });
  expect(useViewerStore.getState().navLens["acme:a"].source.kind).toBe("private");
  await writeSharedUiState(key, { scope: "story:S-1" });
  expect(useViewerStore.getState().navLens["acme:a"].source.kind).toBe("clone");
  expect((await readSharedUiState(key)).scope).toBe("story:S-1");
  // Native Source actions write the same topic, replacing a specialized pane scope.
  await writeSharedUiState(key, { scope: "private" });
  await writeSharedUiState(key, { scope: "team" });
  expect((await readSharedUiState(key)).scope).toBe("team");
});

it("mirrors every native lens field with no persisted second copy", async () => {
  const { useViewerStore } = await import("@/shared/stores/viewer-store");
  const key = JSON.stringify(["ui:file-nav", "team:acme/a"]);
  const lens = {
    source: { kind: "worktree" as const, id: "wt-1" },
    view: "history" as const,
    lens: "ready" as const,
    arrange: "stage" as const,
    storiesOnly: true,
    doneOpen: true,
    historyBranches: { kind: "main" as const },
  };
  useViewerStore.getState().setNavLens("acme:a", { source: lens.source });
  await writeSharedUiState(key, lens);
  expect(useViewerStore.getState().navLens["acme:a"]).toEqual(lens);
  expect(await readSharedUiState(key)).toMatchObject(lens);
  const saved = JSON.parse(storage.raw).liveHubUi[key]?.data ?? {};
  for (const field of Object.keys(lens)) expect(saved[field]).toBeUndefined();
  useViewerStore.getState().setNavLens("acme:a", { lens: "needs", doneOpen: false });
  expect(await readSharedUiState(key)).toMatchObject({ ...lens, lens: "needs", doneOpen: false });
});

it("notifies live views for native lens writes and clears a specialized pane scope on source selection", async () => {
  const { watchSharedUiState } = await import("./shared-ui-state");
  const { useViewerStore } = await import("@/shared/stores/viewer-store");
  const key = JSON.stringify(["ui:file-nav", "team:acme/a"]);
  const changed = vi.fn();
  const stop = await watchSharedUiState(changed);
  try {
    await writeSharedUiState(key, { scope: "story:S-1" });
    expect((await readSharedUiState(key)).scope).toBe("story:S-1");
    changed.mockClear();
    useViewerStore.getState().setNavLens("acme:a", { source: { kind: "clone" }, lens: "needs" });
    await vi.waitFor(() => expect(changed).toHaveBeenCalledWith("ui:file-nav", "team:acme/a"));
    expect(await readSharedUiState(key)).toMatchObject({ scope: "team", lens: "needs" });
  } finally {
    stop();
  }
});
