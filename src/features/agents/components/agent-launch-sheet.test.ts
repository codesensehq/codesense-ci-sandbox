// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { NO_WORKSPACE_ID } from "@/shared/lib/workspace-key";
import type { AgentSessionRow } from "@/tauri/commands";

import { launchedRowIn, launchKeepsArrow, launchLandsIn } from "./agent-launch-sheet";

const none = { placed: null, storyWorkspaceId: null, storyWorkspaceIdToBe: null };

describe("the card a launched agent lands in", () => {
  it("is the workspace the pane places its session in, wherever it was launched", () => {
    expect(launchLandsIn("team-workspace", { ...none, placed: "generic:/w" })).toBe("generic:/w");
    expect(launchLandsIn("story", { ...none, placed: "w2", storyWorkspaceId: "SW-1-x" })).toBe(
      "w2",
    );
  });

  it("is the no-workspace card for its team folder until the pane places it", () => {
    expect(launchLandsIn("team-workspace", none)).toBe(NO_WORKSPACE_ID);
  });

  it("is its story's workspace, else the one its story will open as, else none yet", () => {
    expect(launchLandsIn("story", { ...none, storyWorkspaceId: "SW-1-x" })).toBe("SW-1-x");
    expect(launchLandsIn("story", { ...none, storyWorkspaceIdToBe: "SW-8-new" })).toBe("SW-8-new");
    expect(launchLandsIn("story", none)).toBeNull();
  });
});

describe("the arrow a launch keeps", () => {
  const T = "acme:core";
  it("is only an arrow on the card it lands in, a story's card to be included", () => {
    expect(launchKeepsArrow({ teamKey: T, workspaceId: "SW-8-new" }, T, "SW-8-new")).toBe(true);
    expect(launchKeepsArrow({ teamKey: T, workspaceId: NO_WORKSPACE_ID }, T, NO_WORKSPACE_ID)).toBe(
      true,
    );
    expect(launchKeepsArrow({ teamKey: T, workspaceId: "other" }, T, "SW-8-new")).toBe(false);
    expect(launchKeepsArrow({ teamKey: "acme:ops", workspaceId: "SW-8-new" }, T, "SW-8-new")).toBe(
      false,
    );
    expect(launchKeepsArrow({ teamKey: T, workspaceId: "SW-8-new" }, T, null)).toBe(false);
  });
});

describe("the launched agent's row", () => {
  const row = (handle: string, sessionId: string | null) =>
    ({ handle, sessionId, alive: true }) as AgentSessionRow;
  const tabs: Record<string, string> = { "s-ops": "acme:ops", "s-core": "acme:core" };
  const tabTeamOf = (sid: string) => tabs[sid] ?? null;

  it("is the one of its handle whose tab is in the team it was launched into", () => {
    const rows = [row("docs", "s-ops"), row("docs", "s-core")];
    expect(launchedRowIn(rows, "docs", "acme:core", tabTeamOf)?.sessionId).toBe("s-core");
  });

  it("is one with no tab yet, never one whose tab is in another team", () => {
    expect(launchedRowIn([row("docs", "s-ops")], "docs", "acme:core", tabTeamOf)).toBeUndefined();
    const rows = [row("docs", "s-ops"), row("docs", "s-new")];
    expect(launchedRowIn(rows, "docs", "acme:core", tabTeamOf)?.sessionId).toBe("s-new");
    expect(launchedRowIn(null, "docs", "acme:core", tabTeamOf)).toBeUndefined();
  });
});
