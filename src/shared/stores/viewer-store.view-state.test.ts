// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";

import { useViewerStore } from "./viewer-store";
import { applyFieldPatch } from "@/shared/lib/field-patch";
import * as commands from "@/tauri/commands";
import { navViewKey, fileViewKey } from "@/shared/lib/view-snapshot";

// RF-perf-12: the `viewStateByKey` session slice — capture/read/drop round-trip,
// GC on tab/team close, and the AC-4 invariant that it is NEVER disk-persisted.

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  listNotes: vi.fn().mockResolvedValue([]),
  readNote: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "S", pinned: false, lastOpenedAt: 0, content: "" }),
  createNote: vi
    .fn()
    .mockResolvedValue({ id: "n2", title: "U", pinned: false, lastOpenedAt: 0, content: "" }),
  writeNote: vi.fn().mockResolvedValue(undefined),
  updateNoteMeta: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "S", pinned: false, lastOpenedAt: 0 }),
  deleteNote: vi.fn().mockResolvedValue([]),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  detectTools: vi
    .fn()
    .mockResolvedValue({ terminals: [], editors: [], default_terminal: "", default_editor: "" }),
}));

const saveAppStateMock = commands.saveAppState as ReturnType<typeof vi.fn>;
const TEAM = "acme/web:default";

beforeEach(() => {
  useViewerStore.setState({ viewStateByKey: {}, openedTeams: [] });
  saveAppStateMock.mockReset().mockResolvedValue(undefined);
});

describe("viewStateByKey actions (RF-perf-12 T1)", () => {
  it("captures and reads back a snapshot round-trip", () => {
    const key = fileViewKey(TEAM, "/a.md");
    useViewerStore
      .getState()
      .captureViewState(key, { scrollTop: 120, selection: { anchor: 3, head: 9 } });
    expect(useViewerStore.getState().readViewState(key)).toEqual({
      scrollTop: 120,
      selection: { anchor: 3, head: 9 },
    });
  });

  it("readViewState returns undefined for an unknown key", () => {
    expect(useViewerStore.getState().readViewState("nope")).toBeUndefined();
  });

  it("captureViewState overwrites the same key in place", () => {
    const key = navViewKey(TEAM);
    useViewerStore.getState().captureViewState(key, { scrollTop: 40 });
    useViewerStore.getState().captureViewState(key, { scrollTop: 90 });
    expect(useViewerStore.getState().readViewState(key)).toEqual({ scrollTop: 90 });
    expect(Object.keys(useViewerStore.getState().viewStateByKey)).toEqual([key]);
  });
});

describe("viewStateByKey GC on close (RF-perf-12 T5 / AC-5)", () => {
  it("closeTab drops the closed file's snapshot", () => {
    const file = "/x.md";
    useViewerStore.setState({
      openTabs: { [TEAM]: [{ path: file, name: "x.md" }] },
      activeTab: { [TEAM]: file },
    });
    useViewerStore.getState().captureViewState(fileViewKey(TEAM, file), { scrollTop: 55 });
    useViewerStore.getState().captureViewState(navViewKey(TEAM), { scrollTop: 12 });

    useViewerStore.getState().closeTab(TEAM, "acme/web", file);

    // file snapshot gone, nav snapshot untouched
    expect(useViewerStore.getState().readViewState(fileViewKey(TEAM, file))).toBeUndefined();
    expect(useViewerStore.getState().readViewState(navViewKey(TEAM))).toEqual({ scrollTop: 12 });
  });

  it("closeTeam prunes every snapshot for the team but leaves other teams", () => {
    const other = "acme/api:default";
    useViewerStore.setState({
      openedTeams: [TEAM, other],
      viewStateByKey: {
        [navViewKey(TEAM)]: { scrollTop: 1 },
        [fileViewKey(TEAM, "/a.md")]: { scrollTop: 2 },
        [navViewKey(other)]: { scrollTop: 3 },
      },
    });
    useViewerStore.getState().closeTeam(TEAM);
    const map = useViewerStore.getState().viewStateByKey;
    expect(map[navViewKey(TEAM)]).toBeUndefined();
    expect(map[fileViewKey(TEAM, "/a.md")]).toBeUndefined();
    expect(map[navViewKey(other)]).toEqual({ scrollTop: 3 });
  });
});

describe("viewStateByKey is session-only (RF-perf-12 AC-4)", () => {
  it("is NEVER written into the persisted app state", async () => {
    useViewerStore.getState().captureViewState(fileViewKey(TEAM, "/a.md"), { scrollTop: 99 });
    useViewerStore.setState({ cliExpanded: true });

    let payload: string | null = null;
    saveAppStateMock.mockImplementation(async (p: string) => {
      payload = p;
    });
    useViewerStore.getState().persistState();
    await new Promise((r) => setTimeout(r, 400));

    expect(payload).not.toBeNull();
    const parsed = JSON.parse(payload!);
    expect(parsed.viewer.viewStateByKey).toBeUndefined();
    const committed = applyFieldPatch({ viewer: { schemaVersion: 15 } }, parsed) as {
      viewer: Record<string, unknown>;
    };
    expect(committed.viewer.viewStateByKey).toBeUndefined();
    expect((parsed.viewer.cliChrome as { expanded?: boolean }).expanded).toBe(true);
  });
});

// Persisting is a no-op until the saved state has loaded (SW-FU-B2).
beforeEach(() => {
  useViewerStore.setState({ stateLoaded: true });
});
