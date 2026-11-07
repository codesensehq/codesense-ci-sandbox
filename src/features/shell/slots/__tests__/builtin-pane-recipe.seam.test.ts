// SPDX-License-Identifier: MIT
import { runInNewContext } from "node:vm";
import { describe, expect, it, vi } from "vitest";
import { recipePane } from "../../../../../tests/support/builtin-pane-recipe";

// Execute the real fork's layout closure, keeping its existing row builder and navigation action.
describe("shipped agent-list role recipe at the host-row seam", () => {
  it("keeps every role and stable row/action identity through a session refresh", () => {
    const html = recipePane("agent-list");
    expect(html).toContain('["status", "By roles"]');
    expect(html).toContain('chip("By roles"');
    const stable = html.slice(
      html.indexOf("      function stable("),
      html.indexOf("      function liveOf("),
    );
    const flatten = html.slice(
      html.indexOf("      function flatten("),
      html.indexOf("      const cardMath"),
    );
    const goto = html.slice(
      html.indexOf("      function goto("),
      html.indexOf("      function descendants("),
    );
    const sessions = new Map<string, any>([
      [
        "a",
        {
          id: "a",
          sessionId: "session-a",
          handle: "@a",
          roleHoldings: [{ role: "reviewer" }, { role: "builder" }],
        },
      ],
      ["b", { id: "b", sessionId: "session-b", handle: "@b", roleHoldings: [] }],
      ["c", { id: "c", sessionId: "session-c", handle: "@c", roleHoldings: [{ role: "builder" }] }],
    ]);
    const refs = [...sessions.keys()].map((sessionRef, order) => ({
      id: "ref-" + sessionRef,
      sessionRef,
      teamKey: "org/team",
      order,
    }));
    const gotoAgent = vi.fn(async () => null);
    const context: Record<string, any> = {
      sessions,
      references: refs,
      viewRefs: refs,
      metadata: { teamOrder: ["org/team"] },
      ui: { arrange: "status", selected: "session-a" },
      contextIds: new Set(),
      CRASHED: new Set(),
      STOPPED: new Set(),
      isNeedsYou: () => false,
      subagentsWorking: () => false,
      lines: () => ({}),
      agentHeight: () => 56,
      nameOf: (s: any) => s?.handle,
      managerKey: () => "",
      cs: { gotoAgent },
      report: vi.fn(),
    };
    runInNewContext(
      stable + flatten + goto + "; this.flatten = flatten; this.goto = goto;",
      context,
    );
    const first = context.flatten();
    expect(first.filter((r: any) => r.kind === "heading").map((r: any) => r.label)).toEqual([
      "org/team · builder",
      "org/team · builder, reviewer",
      "org/team · Unassigned",
    ]);
    const rows = first.filter((r: any) => r.kind === "agent");
    expect(rows.map((r: any) => r.id).sort()).toEqual(refs.map((r) => r.id).sort());
    expect(new Set(rows.map((r: any) => r.id)).size).toBe(3);
    expect(rows.find((r: any) => r.id === "ref-a").selected).toBe(true);
    expect(rows.every((r: any) => r.s === sessions.get(r.ref.sessionRef))).toBe(true);
    context.goto(rows.find((r: any) => r.id === "ref-a").s, true);
    expect(gotoAgent).toHaveBeenCalledWith({ agentId: "session-a", preserveFocus: true });
    sessions.get("b").roleHoldings = [{ role: "builder" }];
    const refreshed = context.flatten();
    expect(
      refreshed.filter((r: any) => r.kind === "heading").map((r: any) => r.label),
    ).not.toContain("org/team · Unassigned");
    expect(
      refreshed
        .filter((r: any) => r.kind === "agent")
        .map((r: any) => r.id)
        .sort(),
    ).toEqual(rows.map((r: any) => r.id).sort());
    expect(
      refreshed
        .filter((r: any) => r.kind === "agent")
        .slice(0, 2)
        .map((r: any) => r.id),
    ).toEqual(["ref-b", "ref-c"]);
  });
});
