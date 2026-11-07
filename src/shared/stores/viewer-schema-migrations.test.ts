// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  RETIRED_FORWARD_SYNC_SETTINGS,
  RETIRED_REVERSE_SYNC_SETTINGS,
  RETIRED_WORKTREE_REAP_SETTINGS,
  VIEWER_SCHEMA_MIGRATIONS,
  VIEWER_SCHEMA_VERSION,
  runViewerSchemaMigrations,
  sanitizeAgentContextSelections,
} from "./viewer-schema-migrations";

function blobWithRetiredSettings(): Record<string, unknown> {
  return {
    settings: {
      forwardSyncDefault: "live",
      forwardSyncOverrides: { "/wt/a": "on" },
      hydrationMode: "manual",
      autoPullEnabled: false,
      reverseSyncDefault: "interval",
      reverseSyncOverrides: { "/wt/a": "on" },
      maxWorktrees: 7,
      idleThresholdMinutes: 30,
      worktreePins: ["/wt/a"],
      autoRemoveMergedWorktrees: false,
      quitOnClose: true,
    },
  };
}

describe("viewer schema migration chain", () => {
  it("versions are strictly increasing and the persisted version is the last step", () => {
    const versions = VIEWER_SCHEMA_MIGRATIONS.map((m) => m.to);
    for (let i = 1; i < versions.length; i++) {
      expect(versions[i]).toBeGreaterThan(versions[i - 1]);
    }
    expect(VIEWER_SCHEMA_VERSION).toBe(versions[versions.length - 1]);
    expect(VIEWER_SCHEMA_VERSION).toBe(15);
  });

  it("v10 → v13 drops the forward-sync, reverse-sync and worktree-reaper settings and keeps the rest", () => {
    const viewer = blobWithRetiredSettings();
    expect(runViewerSchemaMigrations(viewer, 10)).toEqual([11, 12, 13, 14, 15]);
    const settings = viewer.settings as Record<string, unknown>;
    for (const key of RETIRED_FORWARD_SYNC_SETTINGS) expect(settings).not.toHaveProperty(key);
    for (const key of RETIRED_REVERSE_SYNC_SETTINGS) expect(settings).not.toHaveProperty(key);
    for (const key of RETIRED_WORKTREE_REAP_SETTINGS) expect(settings).not.toHaveProperty(key);
    expect(settings).toEqual({ quitOnClose: true });
  });

  it("v11 → v12 (a blob saved by SW-XS-3) drops the reverse-sync settings, then v13 the reaper's", () => {
    const viewer: Record<string, unknown> = {
      settings: {
        reverseSyncDefault: "live",
        reverseSyncOverrides: { "/wt/a": "off" },
        maxWorktrees: 7,
        quitOnClose: true,
      },
    };
    expect(runViewerSchemaMigrations(viewer, 11)).toEqual([12, 13, 14, 15]);
    expect(viewer.settings).toEqual({ quitOnClose: true });
  });

  it("SW-XS-17 v12 → v13 drops only the worktree cap, idle-threshold, pin and merged auto-remove settings", () => {
    const viewer: Record<string, unknown> = {
      settings: {
        maxWorktrees: 20,
        idleThresholdMinutes: 30,
        worktreePins: ["/wt/a"],
        autoRemoveMergedWorktrees: false,
        quitOnClose: true,
      },
    };
    expect(runViewerSchemaMigrations(viewer, 12)).toEqual([13, 14, 15]);
    expect(viewer.settings).toEqual({ quitOnClose: true });
  });

  it("v9 steps through v10, v11, v12, v13, v14 and then v15", () => {
    const viewer = blobWithRetiredSettings();
    expect(runViewerSchemaMigrations(viewer, 9)).toEqual([10, 11, 12, 13, 14, 15]);
    expect(viewer.settings).toEqual({ quitOnClose: true });
  });

  it("a missing version runs the whole chain", () => {
    const viewer = blobWithRetiredSettings();
    expect(runViewerSchemaMigrations(viewer, undefined)).toEqual([8, 10, 11, 12, 13, 14, 15]);
  });

  it("v15 passes through untouched", () => {
    const viewer = blobWithRetiredSettings();
    const before = JSON.stringify(viewer);
    expect(runViewerSchemaMigrations(viewer, 15)).toEqual([]);
    expect(JSON.stringify(viewer)).toBe(before);
  });

  it("tolerates a blob with no settings object", () => {
    const viewer: Record<string, unknown> = { settings: ["not", "an", "object"] };
    expect(() => runViewerSchemaMigrations(viewer, 10)).not.toThrow();
    expect(viewer.settings).toEqual(["not", "an", "object"]);
  });
});

describe("SW-XS-15 v13 → v14: the context pivot maps to an agent", () => {
  it("retires the install-tier pivot's two slots; its picks carry over as Team (everything)", () => {
    const viewer: Record<string, unknown> = {
      schemaVersion: 13,
      selectedSourceRepo: { "alderco:default": "alderco/codesense" },
      selectedContextId: { "alderco:default": "team-repo:alderco:default:alderco/codesense" },
      pipelineScope: { "alderco:default": "" },
    };
    expect(runViewerSchemaMigrations(viewer, 13)).toEqual([14, 15]);
    expect(viewer).not.toHaveProperty("selectedSourceRepo");
    expect(viewer).not.toHaveProperty("selectedContextId");
    expect(viewer.selectedAgentContext).toEqual({});
    expect(viewer.pipelineScope).toEqual({ "alderco:default": "" });
  });

  it("a blob from v8 steps through the whole chain to the agent pivot", () => {
    const viewer: Record<string, unknown> = { selectedSourceRepo: { "a:b": "x/y" } };
    expect(runViewerSchemaMigrations(viewer, 8)).toEqual([10, 11, 12, 13, 14, 15]);
    expect(viewer).not.toHaveProperty("selectedSourceRepo");
    expect(viewer.selectedAgentContext).toEqual({});
  });

  it("keeps only well-formed selections", () => {
    expect(
      sanitizeAgentContextSelections({
        "a:t": { agentKey: "a/t/barb", repoSlug: "alderco/codesense" },
        "a:h": { agentKey: "a/h/barb", repoSlug: "github.com/alderco/codesense" },
        "a:u": { agentKey: "a/u/bubbles", repoSlug: "not a slug" },
        "a:v": { agentKey: "" },
        "a:w": "barb",
        "a:x": { repoSlug: "o/r" },
      }),
    ).toEqual({
      "a:t": { agentKey: "a/t/barb", repoSlug: "alderco/codesense" },
      "a:h": { agentKey: "a/h/barb", repoSlug: "github.com/alderco/codesense" },
      "a:u": { agentKey: "a/u/bubbles" },
    });
    expect(sanitizeAgentContextSelections(["nope"])).toEqual({});
    expect(sanitizeAgentContextSelections(undefined)).toEqual({});
  });
});

describe("D39 v14 → v15: the CLI chrome becomes one global value", () => {
  it("runs as the v15 step, seeding cliChrome from the last team in the migration context", () => {
    const viewer: Record<string, unknown> = {
      openedTeams: ["a:one", "a:two"],
      terminalVisible: { "a:one": false, "a:two": true },
      layoutMode: { "a:one": "viewer", "a:two": "console" },
    };
    expect(runViewerSchemaMigrations(viewer, 14, { lastTeamKey: "a:two" })).toEqual([15]);
    expect(viewer.cliChrome).toEqual({ visible: true, layoutMode: "console", expanded: false });
    expect(viewer).not.toHaveProperty("terminalVisible");
    expect(viewer).not.toHaveProperty("layoutMode");
  });
});
