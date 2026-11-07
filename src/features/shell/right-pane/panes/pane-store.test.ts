// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { PaneEntry } from "@/tauri/commands";

import { panePanelId, panePathLabel, parsePanePanelId } from "./pane-store";
import { panePanelTitle } from "./use-register-pane-panels";

const entry = (scope: "team" | "private", team: string | undefined, id: string): PaneEntry => ({
  id,
  scope,
  ...(team ? { team } : {}),
  title: id,
  path: `/x/${id}.html`,
  panesDir: "/x",
  panelId: panePanelId(scope, "acme/api", team, id),
});

describe("pane panel ids", () => {
  it("round-trip team and private ids, org slugs with a slash included", () => {
    expect(panePanelId("team", "acme/api", "backend", "board")).toBe("pane:acme/api:backend/board");
    expect(parsePanePanelId("pane:acme/api:backend/board")).toEqual({
      scope: "team",
      teamKey: "acme/api:backend",
      id: "board",
    });
    expect(parsePanePanelId("pane:private:acme/api/board")).toEqual({
      scope: "private",
      org: "acme/api",
      id: "board",
    });
    expect(parsePanePanelId("notes:x")).toBeNull();
    expect(parsePanePanelId("pane:private:acme/api/")).toBeNull();
    expect(parsePanePanelId("pane:private:/board")).toBeNull();
    expect(panePathLabel({ scope: "team", teamKey: "acme/api:backend", id: "board" })).toBe(
      "backend/panes/board.html",
    );
  });

  it("titles carry a layer cue unless the pane is the active team's own", () => {
    expect(panePanelTitle(entry("team", "backend", "board"), "backend")).toBe("board");
    expect(panePanelTitle(entry("team", "platform", "board"), "backend")).toBe("board · platform");
    expect(panePanelTitle(entry("private", undefined, "board"), "backend")).toBe("board · Private");
  });
});
