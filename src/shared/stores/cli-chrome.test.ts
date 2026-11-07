// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  DEFAULT_CLI_CHROME,
  applyCliChromePatch,
  sanitizeCliChrome,
  seedCliChrome,
  type CliChrome,
} from "./cli-chrome";

const shared: CliChrome = { ...DEFAULT_CLI_CHROME, visible: true, layoutMode: "shared" };

describe("seedCliChrome (v15): the per-team chrome becomes one cliChrome", () => {
  it("seeds from the team open at the last close", () => {
    const viewer: Record<string, unknown> = {
      openedTeams: ["a:one", "a:two"],
      terminalVisible: { "a:one": false, "a:two": true },
      layoutMode: { "a:one": "viewer", "a:two": "console" },
      lastTerminalMode: { "a:one": "shared", "a:two": "console" },
      terminalSplitRatio: { "a:one": 0.3, "a:two": 0.7 },
      workspacePanelCollapsed: { "a:two": true },
      workspacePanelWidth: { "a:two": 300 },
    };
    seedCliChrome(viewer, "a:two");
    expect(sanitizeCliChrome(viewer.cliChrome)).toEqual({
      visible: true,
      layoutMode: "console",
      lastTerminalMode: "console",
      splitRatio: 0.7,
      expanded: false,
      agentsDockCollapsed: true,
      agentsDockWidth: 300,
    });
  });

  it("falls back to the first saved team when the last team saved none, then the maps' order", () => {
    const opened: Record<string, unknown> = {
      openedTeams: ["a:none", "a:two", "a:one"],
      layoutMode: { "a:one": "viewer", "a:two": "console" },
    };
    seedCliChrome(opened, "a:gone");
    expect(sanitizeCliChrome(opened.cliChrome).layoutMode).toBe("console");
    const unordered: Record<string, unknown> = {
      layoutMode: { "a:one": "viewer", "a:two": "console" },
    };
    seedCliChrome(unordered, null);
    expect(sanitizeCliChrome(unordered.cliChrome).layoutMode).toBe("viewer");
  });

  it("gives a field the seed team never saved its default", () => {
    const viewer: Record<string, unknown> = {
      openedTeams: ["a:one", "a:two"],
      layoutMode: { "a:one": "shared", "a:two": "viewer" },
      terminalSplitRatio: { "a:two": 0.7 },
    };
    seedCliChrome(viewer, "a:one");
    const chrome = sanitizeCliChrome(viewer.cliChrome);
    expect(chrome.layoutMode).toBe("shared");
    expect(chrome.splitRatio).toBe(DEFAULT_CLI_CHROME.splitRatio);
  });

  it("with no old keys leaves no cliChrome, so it loads as the defaults", () => {
    const viewer: Record<string, unknown> = { openedTeams: ["a:one"] };
    seedCliChrome(viewer, "a:one");
    expect(viewer).not.toHaveProperty("cliChrome");
    expect(sanitizeCliChrome(viewer.cliChrome)).toEqual(DEFAULT_CLI_CHROME);
  });

  it("seeds expanded from the seed team's zoomed layout, flat or an epic's, and drops every zoom", () => {
    const flat: Record<string, unknown> = {
      openedTeams: ["a:one", "a:two"],
      layoutMode: { "a:one": "shared" },
      foregroundLayout: {
        "a:one": { preset: "rows", zoomedTileId: "w1" },
        "a:two": { preset: "grid", zoomedTileId: "w9" },
      },
    };
    seedCliChrome(flat, "a:one");
    expect(sanitizeCliChrome(flat.cliChrome).expanded).toBe(true);
    expect(flat.foregroundLayout).toEqual({
      "a:one": { preset: "rows" },
      "a:two": { preset: "grid" },
    });
    const grouped: Record<string, unknown> = {
      layoutMode: { "a:one": "shared" },
      foregroundLayout: { "a:one": { preset: "rows" } },
      trayMode: { "a:one": "grouped" },
      selectedEpicDirName: { "a:one": "epic-x" },
      foregroundLayoutByRail: { "a:one": { "epic-x": { zoomedTileId: "w2" } } },
    };
    seedCliChrome(grouped, "a:one");
    expect(sanitizeCliChrome(grouped.cliChrome).expanded).toBe(true);
    expect(grouped.foregroundLayoutByRail).toEqual({ "a:one": { "epic-x": {} } });
  });

  it("drops the old keys, the retired ones too, and leaves a saved cliChrome as it is", () => {
    const viewer: Record<string, unknown> = {
      cliChrome: { layoutMode: "console" },
      layoutMode: { "a:one": "viewer" },
      previousSplitRatio: { "a:one": 0.4 },
      terminalReady: { "a:one": true },
    };
    seedCliChrome(viewer, "a:one");
    expect(viewer).toEqual({ cliChrome: { layoutMode: "console" } });
  });
});

describe("sanitizeCliChrome", () => {
  it("loads a missing or malformed field as its default and keeps the rest", () => {
    expect(sanitizeCliChrome(undefined)).toEqual(DEFAULT_CLI_CHROME);
    expect(
      sanitizeCliChrome({
        layoutMode: "shared",
        visible: true,
        splitRatio: "bad",
        agentsDockWidth: "x",
      }),
    ).toEqual({ ...DEFAULT_CLI_CHROME, layoutMode: "shared", visible: true });
  });

  it("keeps visibility and the reopen layout paired with a full-screen or folded layout", () => {
    expect(sanitizeCliChrome({ layoutMode: "console", visible: false })).toMatchObject({
      visible: true,
      lastTerminalMode: "console",
    });
    expect(sanitizeCliChrome({ layoutMode: "viewer", visible: true }).visible).toBe(false);
  });
});

describe("applyCliChromePatch: an agent's write is honoured as asked (the agent decides)", () => {
  it("may leave full screen, and the shown layout becomes the reopen layout", () => {
    const fullScreen = {
      ...shared,
      layoutMode: "console" as const,
      lastTerminalMode: "console" as const,
    };
    expect(applyCliChromePatch(fullScreen, { layoutMode: "shared" })).toMatchObject({
      layoutMode: "shared",
      visible: true,
      lastTerminalMode: "shared",
    });
  });

  it("may enter full screen, and a layout brings visibility along", () => {
    const folded = { ...DEFAULT_CLI_CHROME, layoutMode: "viewer" as const };
    expect(applyCliChromePatch(folded, { layoutMode: "console" })).toMatchObject({
      layoutMode: "console",
      visible: true,
      lastTerminalMode: "console",
    });
    expect(applyCliChromePatch(shared, { layoutMode: "viewer" })).toMatchObject({
      layoutMode: "viewer",
      visible: false,
    });
  });

  it("folds the CLI on visible: false, even from full screen, and brings it back in its last layout", () => {
    const fullScreen = {
      ...shared,
      layoutMode: "console" as const,
      lastTerminalMode: "console" as const,
    };
    const folded = applyCliChromePatch(fullScreen, { visible: false });
    expect(folded).toMatchObject({
      layoutMode: "viewer",
      visible: false,
      lastTerminalMode: "console",
    });
    expect(applyCliChromePatch(folded, { visible: true })).toMatchObject({
      layoutMode: "console",
      visible: true,
    });
  });

  it("a hidden pane write keeps the reopen layout, and a named one is kept", () => {
    const fromConsole = { ...shared, lastTerminalMode: "console" as const };
    expect(
      applyCliChromePatch(fromConsole, { layoutMode: "shared", visible: false }),
    ).toMatchObject({ layoutMode: "shared", visible: false, lastTerminalMode: "console" });
    expect(
      applyCliChromePatch(shared, { layoutMode: "shared", lastTerminalMode: "console" })
        .lastTerminalMode,
    ).toBe("console");
  });

  it("merges the other fields one by one and ignores a wrong type or a per-team map", () => {
    const next = applyCliChromePatch(shared, {
      splitRatio: 0.3,
      expanded: true,
      agentsDockCollapsed: true,
      agentsDockWidth: 260,
    });
    expect(next).toEqual({
      ...shared,
      splitRatio: 0.3,
      expanded: true,
      agentsDockCollapsed: true,
      agentsDockWidth: 260,
    });
    expect(
      applyCliChromePatch(shared, { layoutMode: { "a:one": "viewer" }, splitRatio: "x" }),
    ).toEqual(shared);
    expect(applyCliChromePatch(shared, "nope")).toBe(shared);
  });
});

it("clamps saved and MCP geometry to the actual divider limits", () => {
  expect(sanitizeCliChrome({ splitRatio: 0.01, agentsDockWidth: 1 })).toMatchObject({
    splitRatio: 0.1,
    agentsDockWidth: 236,
  });
  expect(applyCliChromePatch(shared, { splitRatio: 4, agentsDockWidth: 999 })).toMatchObject({
    splitRatio: 0.9,
    agentsDockWidth: 400,
  });
});
it("a visibility-only reveal pairs the shown pane with its reopen mode", () => {
  expect(
    applyCliChromePatch(
      { ...shared, visible: false, lastTerminalMode: "console" },
      { visible: true },
    ),
  ).toMatchObject({ lastTerminalMode: "shared" });
});
it("seeds a by-rail-only zoom from the active grouped rail, skipping malformed team keys", () => {
  const viewer: Record<string, unknown> = {
    openedTeams: ["bad", "a:one"],
    layoutMode: { bad: "console" },
    trayMode: { "a:one": "grouped" },
    foregroundLayoutByRail: { "a:one": { "": { zoomedTileId: "w" } } },
    cliTeamKey: "bad",
  };
  seedCliChrome(viewer, "bad");
  expect(sanitizeCliChrome(viewer.cliChrome)).toMatchObject({ layoutMode: null, expanded: true });
  expect(viewer).not.toHaveProperty("cliTeamKey");
});
it("does not seed expansion from an inactive rail or a grouped layout while flat", () => {
  const viewer: Record<string, unknown> = {
    layoutMode: { "a:one": "shared" },
    foregroundLayoutByRail: { "a:one": { other: { zoomedTileId: "w" } } },
  };
  seedCliChrome(viewer, "a:one");
  expect(sanitizeCliChrome(viewer.cliChrome).expanded).toBe(false);
});

it("recovers a real named-epic legacy zoom from the saved focused workspace", () => {
  const viewer: Record<string, unknown> = {
    trayMode: { "a:one": "grouped" },
    activeWorkspace: { "a:one": "w" },
    foregroundLayoutByRail: { "a:one": { "epic-a": { zoomedTileId: "w" } } },
  };
  seedCliChrome(viewer, "a:one");
  expect(sanitizeCliChrome(viewer.cliChrome).expanded).toBe(true);
  expect(JSON.stringify(viewer)).not.toContain("zoomedTileId");
});

it("an ordinary preset does not outrank a team that saved chrome", () => {
  const viewer: Record<string, unknown> = {
    openedTeams: ["a:one", "a:two"],
    foregroundLayout: { "a:one": { preset: "rows" } },
    layoutMode: { "a:two": "console" },
  };
  seedCliChrome(viewer, "a:one");
  expect(sanitizeCliChrome(viewer.cliChrome).layoutMode).toBe("console");
});

it("keeps an agent's explicit reopen mode while entering console", () => {
  expect(
    applyCliChromePatch(shared, {
      layoutMode: "console",
      lastTerminalMode: "shared",
    }),
  ).toMatchObject({ layoutMode: "console", lastTerminalMode: "shared", visible: true });
});
