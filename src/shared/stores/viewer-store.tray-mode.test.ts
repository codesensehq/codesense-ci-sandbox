// SPDX-License-Identifier: MIT
import "@/test/prefs-state-fixture";
import { describe, it, expect, beforeEach, vi } from "vitest";

// SW-EW-38 Task 1: per-team `trayMode` ("flat" | "grouped") + the runtime-only
// `selectedEpicDirName` (rail key). `trayMode` is a new additive PERSISTED slice
// (absent → {}, no migration — mirrors SW-FX-8 `navCollapsed`); R3:
// `selectedEpicDirName` is RUNTIME-ONLY (in TEAM_KEYED_SLICES for closeTeam
// cleanup + init {}, but NEVER persisted — follow-focus reconstructs it).

vi.mock("@/tauri/commands", () => ({
  workspaceGroupEdit: vi.fn(),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  saveScratchNote: vi.fn().mockResolvedValue(undefined),
  loadScratchNote: vi.fn().mockResolvedValue(""),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import { useViewerStore } from "./viewer-store";
import * as commands from "@/tauri/commands";

const loadAppStateMock = commands.loadAppState as ReturnType<typeof vi.fn>;
const saveAppStateMock = commands.saveAppState as ReturnType<typeof vi.fn>;

const TEAM = "acme:default";

beforeEach(() => {
  useViewerStore.setState({ trayMode: {}, selectedEpicDirName: {} });
  vi.clearAllMocks();
});

describe("trayMode — setter + default", () => {
  it("defaults to flat when unset (read site applies ?? 'flat')", () => {
    expect(useViewerStore.getState().trayMode["never:set"]).toBeUndefined();
  });

  it("setTrayMode flips the per-team mode", () => {
    useViewerStore.getState().setTrayMode(TEAM, "grouped");
    expect(useViewerStore.getState().trayMode[TEAM]).toBe("grouped");
    useViewerStore.getState().setTrayMode(TEAM, "flat");
    expect(useViewerStore.getState().trayMode[TEAM]).toBe("flat");
  });

  it("is independent per team", () => {
    useViewerStore.getState().setTrayMode("acme:a", "grouped");
    useViewerStore.getState().setTrayMode("acme:b", "flat");
    expect(useViewerStore.getState().trayMode["acme:a"]).toBe("grouped");
    expect(useViewerStore.getState().trayMode["acme:b"]).toBe("flat");
  });
});

describe("selectedEpicDirName — setter + default (rail key)", () => {
  it("defaults to undefined when unset (read site applies ?? '')", () => {
    expect(useViewerStore.getState().selectedEpicDirName["never:set"]).toBeUndefined();
  });

  it("setSelectedEpicGroup stores the resolved rail key verbatim", () => {
    useViewerStore.getState().setSelectedEpicGroup(TEAM, "epic-foo");
    expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBe("epic-foo");
    // sentinels/empty are valid rail keys (roots / Ungrouped)
    useViewerStore.getState().setSelectedEpicGroup(TEAM, "");
    expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBe("");
  });
});

describe("trayMode — persistence (AC-2); selectedEpicDirName runtime-only (R3)", () => {
  it("trayMode round-trips through persist → reload", async () => {
    useViewerStore.getState().setTrayMode(TEAM, "grouped");

    let captured: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      captured = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((r) => setTimeout(r, 400));
    expect(captured).not.toBeNull();

    useViewerStore.setState({ trayMode: {} });
    loadAppStateMock.mockResolvedValue(captured!);
    await useViewerStore.getState().loadPersistedState();

    expect(useViewerStore.getState().trayMode[TEAM]).toBe("grouped");
  });

  it("selectedEpicDirName is NOT persisted (follow-focus rebuilds it on load)", async () => {
    useViewerStore.getState().setTrayMode(TEAM, "grouped");
    useViewerStore.getState().setSelectedEpicGroup(TEAM, "epic-foo");

    let captured: string | null = null;
    saveAppStateMock.mockImplementation(async (payload: string) => {
      captured = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((r) => setTimeout(r, 400));
    expect(captured).not.toBeNull();
    // The serialized blob must not carry the runtime-only selection.
    expect(JSON.parse(captured!).viewer.selectedEpicDirName).toBeUndefined();

    useViewerStore.setState({ trayMode: {}, selectedEpicDirName: {} });
    loadAppStateMock.mockResolvedValue(captured!);
    await useViewerStore.getState().loadPersistedState();

    expect(useViewerStore.getState().trayMode[TEAM]).toBe("grouped");
    expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBeUndefined();
  });

  it("absent trayMode slice loads as {} (backward-compatible, no migration)", async () => {
    loadAppStateMock.mockResolvedValue(JSON.stringify({ viewer: { schemaVersion: 9 } }));
    await useViewerStore.getState().loadPersistedState();
    expect(useViewerStore.getState().trayMode).toEqual({});
  });
});

describe("closeTeam cleanup — both slices are team-keyed", () => {
  it("drops trayMode + selectedEpicDirName for the closed team only", () => {
    useViewerStore.setState({
      trayMode: { [TEAM]: "grouped", "acme:other": "grouped" },
      selectedEpicDirName: { [TEAM]: "epic-foo", "acme:other": "epic-bar" },
    });
    useViewerStore.getState().closeTeam(TEAM);
    expect(useViewerStore.getState().trayMode[TEAM]).toBeUndefined();
    expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBeUndefined();
    // other team survives
    expect(useViewerStore.getState().trayMode["acme:other"]).toBe("grouped");
    expect(useViewerStore.getState().selectedEpicDirName["acme:other"]).toBe("epic-bar");
  });
});

// Persisting is a no-op until the saved state has loaded (SW-FU-B2).
beforeEach(() => {
  useViewerStore.setState({ stateLoaded: true });
});

describe("custom group store actions", () => {
  it("validates before transport and rebuilds the one index and selection after each edit", async () => {
    const a = { id: "story:a", kind: "story" as const, cwd: "/a", status: "ready" as const };
    const b = { ...a, id: "story:b" };
    useViewerStore.setState({
      workspaceGroupVersionsByTeam: {},
      customGroupsByTeam: {},
      workspaces: { [TEAM]: [a, b] },
      softClosedWorkspaces: {},
      railKeyByWorkspaceId: {},
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: "" },
      activeWorkspace: { [TEAM]: a.id },
      foregroundLayoutByRail: {},
    });
    expect((await useViewerStore.getState().createWorkspaceGroup(TEAM, " ")).status).toBe(
      "validation",
    );
    expect(commands.workspaceGroupEdit).not.toHaveBeenCalled();
    const group = { id: "one", name: "Release checks", workspaceIds: [a.id] };
    vi.mocked(commands.workspaceGroupEdit).mockResolvedValueOnce({
      status: "ok",
      teamKey: TEAM,
      groups: [group],
      group,
      revision: 1,
    });
    expect(
      (await useViewerStore.getState().createWorkspaceGroup(TEAM, " Release checks ", [a.id]))
        .status,
    ).toBe("ok");
    expect(useViewerStore.getState().railKeyByWorkspaceId[TEAM][a.id]).toBe(" group:one");
    expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBe(" group:one");
    expect(
      (await useViewerStore.getState().createWorkspaceGroup(TEAM, "release CHECKS")).status,
    ).toBe("validation");
    vi.mocked(commands.workspaceGroupEdit).mockResolvedValueOnce({
      status: "ok",
      teamKey: TEAM,
      groups: [{ ...group, name: "Ship checks" }],
      revision: 2,
    });
    await useViewerStore.getState().renameWorkspaceGroup(TEAM, "one", "Ship checks");
    expect(useViewerStore.getState().customGroupsByTeam[TEAM][0].name).toBe("Ship checks");
    useViewerStore.setState({
      foregroundLayoutByRail: { [TEAM]: { " group:one": { kind: "columns" } as never } },
    });
    vi.mocked(commands.workspaceGroupEdit).mockResolvedValueOnce({
      status: "ok",
      teamKey: TEAM,
      groups: [],
      revision: 3,
    });
    await useViewerStore.getState().deleteWorkspaceGroup(TEAM, "one");
    expect(useViewerStore.getState().foregroundLayoutByRail[TEAM][" group:one"]).toBeUndefined();
    expect(useViewerStore.getState().workspaces[TEAM]).toHaveLength(2);
    expect(useViewerStore.getState().railKeyByWorkspaceId[TEAM][a.id]).toBe("");
  });
  it("persists the names, creation order and closed membership in the profile viewer slice", async () => {
    const groups = [
      { id: "g", name: "Checks", workspaceIds: ["closed-story"] },
      { id: "empty", name: "Empty", workspaceIds: [] },
    ];
    useViewerStore.setState({
      customGroupsByTeam: { [TEAM]: groups },
      workspaceGroupVersionsByTeam: { [TEAM]: 4 },
      stateLoaded: true,
    });
    await useViewerStore.getState().flushPersist();
    const saved = JSON.parse(saveAppStateMock.mock.calls.at(-1)![0]);
    expect(saved.viewer.customGroupsByTeam[TEAM]).toEqual(groups);
    expect(saved.viewer.workspaceGroupVersionsByTeam[TEAM]).toBe(4);
  });
});
