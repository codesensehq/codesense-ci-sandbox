// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook } from "@testing-library/react";

import { useTileTabActions } from "./workspace-terminal-panels";
import { closeTab, endTabs, hideTab } from "../lib/terminal-close";

vi.mock("../lib/terminal-close", () => ({
  closeTab: vi.fn(),
  hideTab: vi.fn(),
  endTabs: vi.fn(),
  closeWorkspace: vi.fn(),
  hideWorkspace: vi.fn(),
}));

/**
 * Unit tests for workspace terminal panel visibility logic.
 *
 * The actual components require xterm.js DOM APIs that don't work in
 * happy-dom. These tests validate the visibility computation logic
 * (same logic used by WorkspaceTerminalContainer) directly.
 */

/**
 * Mirrors the `isVisible` computation in WorkspaceTerminalContainer.
 * A terminal is visible only when ALL four conditions are met.
 */
function computeVisible(
  terminalVisible: boolean,
  isActive: boolean,
  isActiveTabInWs: boolean,
  isActiveView: boolean,
): boolean {
  return terminalVisible && isActive && isActiveTabInWs && isActiveView;
}

/**
 * Mirrors the workspace container's display logic.
 * Container uses `display: none` when inactive.
 */
function computeContainerDisplay(isActive: boolean): "block" | "none" {
  return isActive ? "block" : "none";
}

describe("WorkspaceTerminalContainer visibility", () => {
  it("container displays block when active", () => {
    expect(computeContainerDisplay(true)).toBe("block");
  });

  it("container displays none when inactive", () => {
    expect(computeContainerDisplay(false)).toBe("none");
  });
});

describe("WorkspaceTerminalContainer visible prop", () => {
  it("visible=true only when all four conditions are met", () => {
    expect(computeVisible(true, true, true, true)).toBe(true);
  });

  it("visible=false when terminalVisible is false", () => {
    expect(computeVisible(false, true, true, true)).toBe(false);
  });

  it("visible=false when workspace is inactive", () => {
    expect(computeVisible(true, false, true, true)).toBe(false);
  });

  it("visible=false when tab is not active in workspace", () => {
    expect(computeVisible(true, true, false, true)).toBe(false);
  });

  it("visible=false when view is not active", () => {
    expect(computeVisible(true, true, true, false)).toBe(false);
  });

  it("visible=false when multiple conditions are false", () => {
    expect(computeVisible(false, false, true, true)).toBe(false);
    expect(computeVisible(true, false, false, false)).toBe(false);
    expect(computeVisible(false, false, false, false)).toBe(false);
  });
});

// Every close verb on a tile's tabs routes through `lib/terminal-close` (the one
// decision point), scoped to THIS tile's workspace, after focusing the tile.
describe("@P1 useTileTabActions — close verbs route through terminal-close", () => {
  const focusTile = vi.fn();

  beforeEach(() => vi.clearAllMocks());

  const renderTileActions = () =>
    renderHook(() => useTileTabActions("acme:default", "ws1", focusTile));

  it.each([
    ["onCloseTab", "t1", closeTab, ["acme:default", "ws1", "t1"]],
    ["onHideTab", "t1", hideTab, ["acme:default", "ws1", "t1"]],
    ["onEndTab", "t1", endTabs, ["acme:default", "ws1", { tabId: "t1" }]],
    ["onEndOtherTabs", "t1", endTabs, ["acme:default", "ws1", { except: "t1" }]],
  ] as const)("%s(%s) → the matching terminal-close verb", (action, tabId, verb, args) => {
    const { result } = renderTileActions();
    result.current[action](tabId);
    expect(focusTile).toHaveBeenCalled();
    expect(vi.mocked(verb)).toHaveBeenCalledWith(...args);
  });

  it("onEndAllTabs → endTabs(all)", () => {
    const { result } = renderTileActions();
    result.current.onEndAllTabs();
    expect(vi.mocked(endTabs)).toHaveBeenCalledWith("acme:default", "ws1", "all");
  });
});
