// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";

import { buildTeamKey } from "@/shared/lib/team-key";
import * as commands from "@/tauri/commands";

import { useViewerStore } from "./viewer-store";

// SW-WG-9: the generic team-keyed `maximizedPanel` slice + its `setMaximized` /
// `toggleMaximized` actions, plus its ride on the TEAM_KEYED_SLICES rename/close
// sweep (replacing the four per-panel `*FullscreenTeamKey` scalars).

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  saveScratchNote: vi.fn().mockResolvedValue(undefined),
  loadScratchNote: vi.fn().mockResolvedValue(""),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
}));

const saveAppStateMock = commands.saveAppState as ReturnType<typeof vi.fn>;

const ORG = "acme";
const TEAM_A = buildTeamKey(ORG, "team-a");
const TEAM_B = buildTeamKey(ORG, "team-b");

beforeEach(() => {
  useViewerStore.setState({ maximizedPanel: {} });
});

describe("maximizedPanel slice (SW-WG-9)", () => {
  it("setMaximized sets a team's maximized panel and overwrites (one per team)", () => {
    const { setMaximized } = useViewerStore.getState();
    setMaximized(TEAM_A, "pipeline");
    expect(useViewerStore.getState().maximizedPanel).toEqual({ [TEAM_A]: "pipeline" });
    // Only one panel maximized per team — a second set replaces the first.
    setMaximized(TEAM_A, "board");
    expect(useViewerStore.getState().maximizedPanel).toEqual({ [TEAM_A]: "board" });
  });

  it("setMaximized(null) clears the team's entry", () => {
    const { setMaximized } = useViewerStore.getState();
    setMaximized(TEAM_A, "pipeline");
    setMaximized(TEAM_A, null);
    expect(useViewerStore.getState().maximizedPanel).toEqual({});
  });

  it("setMaximized(null) on an absent team is an Object.is no-op", () => {
    const before = useViewerStore.getState().maximizedPanel;
    useViewerStore.getState().setMaximized(TEAM_A, null);
    expect(useViewerStore.getState().maximizedPanel).toBe(before);
  });

  it("scopes independently per team", () => {
    const { setMaximized } = useViewerStore.getState();
    setMaximized(TEAM_A, "pipeline");
    setMaximized(TEAM_B, "automations");
    expect(useViewerStore.getState().maximizedPanel).toEqual({
      [TEAM_A]: "pipeline",
      [TEAM_B]: "automations",
    });
  });

  it("toggleMaximized maximizes when not set, exits when it is", () => {
    const { toggleMaximized } = useViewerStore.getState();
    toggleMaximized(TEAM_A, "pipeline");
    expect(useViewerStore.getState().maximizedPanel[TEAM_A]).toBe("pipeline");
    // Toggling the SAME panel exits.
    toggleMaximized(TEAM_A, "pipeline");
    expect(useViewerStore.getState().maximizedPanel[TEAM_A]).toBeUndefined();
  });

  it("toggleMaximized to a different panel switches (still one per team)", () => {
    const { toggleMaximized } = useViewerStore.getState();
    toggleMaximized(TEAM_A, "pipeline");
    toggleMaximized(TEAM_A, "board");
    expect(useViewerStore.getState().maximizedPanel).toEqual({ [TEAM_A]: "board" });
  });

  it("closeTeam clears the closing team's maximize but leaves others (TEAM_KEYED_SLICES sweep)", () => {
    useViewerStore.setState({ maximizedPanel: { [TEAM_A]: "pipeline", [TEAM_B]: "board" } });
    useViewerStore.getState().closeTeam(TEAM_A);
    expect(useViewerStore.getState().maximizedPanel).toEqual({ [TEAM_B]: "board" });
  });

  it("rekeyTeam moves the maximize entry to the new key (TEAM_KEYED_SLICES sweep)", () => {
    const oldKey = buildTeamKey(ORG, "old-name");
    const newKey = buildTeamKey(ORG, "new-name");
    useViewerStore.setState({ maximizedPanel: { [oldKey]: "automations" } });
    useViewerStore.getState().rekeyTeam(ORG, "old-name", "new-name");
    expect(useViewerStore.getState().maximizedPanel).toEqual({ [newKey]: "automations" });
  });

  it("runtime-only — excluded from the persisted snapshot (never survives a reload)", async () => {
    useViewerStore.setState({ maximizedPanel: { [TEAM_A]: "pipeline" } });
    let written = "";
    saveAppStateMock.mockReset().mockImplementation(async (payload: string) => {
      written = payload;
    });
    useViewerStore.getState().persistState();
    await new Promise((r) => setTimeout(r, 400));
    const saved = JSON.parse(written).viewer;
    expect(saved.maximizedPanel).toBeUndefined();
  });
});

// Persisting is a no-op until the saved state has loaded (SW-FU-B2).
beforeEach(() => {
  useViewerStore.setState({ stateLoaded: true });
});
