// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { createFixtureForegroundLayout } from "./e2e-cli-workspace-control";
import {
  TERMINAL_WEBGL_PANE_COUNT,
  TERMINAL_WEBGL_WORKSPACE_IDS,
  createTerminalWebglFixturePlan,
} from "./e2e-terminal-webgl-control";

const cwds = Array.from(
  { length: TERMINAL_WEBGL_PANE_COUNT },
  (_, i) => `/isolated/qh6/webgl/${i}`,
);

describe("WebDriver terminal-webgl fixture control", () => {
  it("plans exactly six same-config panes so they share one WebGL atlas (SW-FX-82 repro)", () => {
    const plan = createTerminalWebglFixturePlan({ root: "/isolated/qh6/webgl", cwds });
    expect(plan.teamKey).toBe("qh6:webgl");
    expect(plan.workspaces).toHaveLength(6);
    // Same-config = one shared parentEpic ⇒ one shared xterm glyph atlas, which
    // is the precondition SW-FX-82's sibling-corruption bug needs.
    const epics = new Set(plan.workspaces.map((w) => w.parentEpic));
    expect(epics.size).toBe(1);
    expect(plan.workspaces.map((w) => w.id)).toEqual([...TERMINAL_WEBGL_WORKSPACE_IDS]);
    expect(plan.workspaces.map((w) => w.cwd)).toEqual(cwds);
    expect(JSON.stringify(plan)).not.toMatch(/command|output|credential|token/i);
  });

  it("rejects a cwd list that is not exactly six", () => {
    expect(() => createTerminalWebglFixturePlan({ root: "/r", cwds: cwds.slice(0, 5) })).toThrow();
  });

  it("materializes one deterministic foreground mosaic leaf per pane", () => {
    const layout = createFixtureForegroundLayout(TERMINAL_WEBGL_WORKSPACE_IDS);
    const leaves = (node: NonNullable<typeof layout.root>): string[] =>
      node.kind === "leaf" ? [node.workspaceId] : node.children.flatMap(leaves);
    expect(layout.root).not.toBeNull();
    expect(leaves(layout.root!)).toEqual([...TERMINAL_WEBGL_WORKSPACE_IDS]);
  });
});
