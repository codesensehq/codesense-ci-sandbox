// SPDX-License-Identifier: MIT
import { projectSessionStatus } from "@/test/session-projection-fixture";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
beforeEach(() => useAgentListProjectionStore.setState({ sessions: [] }));
import { beforeEach, describe, it, expect } from "vitest";
import {
  selectScopeMarkKey,
  selectSessionMark,
  type ScopeMarkFacts,
} from "./terminal-tab-selectors";

const NO_AGENT = { bySession: {}, exitBySession: {} };

type Agent = Parameters<typeof selectScopeMarkKey>[2];

/** The roll-up facts over `ids`, from stored statuses and agent-state sources. */
function facts(
  stored: Record<string, string>,
  agent: Partial<Agent>,
  ids: string[],
  pendingAsks: Record<string, { sessionId: string }> = {},
): ScopeMarkFacts {
  for (const [id, status] of Object.entries(stored))
    projectSessionStatus(
      id,
      (
        { done: "idle", error: "failed", "needs-permission": "paused", running: "unknown" } as const
      )[status as "done"] ?? "unknown",
      { statusSinceMs: agent.stateSince?.[id] },
    );
  return JSON.parse(
    selectScopeMarkKey(
      { aiStatusBySession: stored } as Parameters<typeof selectScopeMarkKey>[0],
      {},
      { stateSince: {}, ...NO_AGENT, ...agent } as Agent,
      ids.map((sessionId) => ({ sessionId })),
      pendingAsks,
    ),
  ) as ScopeMarkFacts;
}

describe("selectScopeMarkKey — a finished turn is ready", () => {
  it.each([
    ["failed", "error"],
    ["needs-attention", "question"],
    ["done", "ended"],
  ] as const)("projects managed-only %s without a coarse dot", (outcome, mark) => {
    projectSessionStatus(
      "s",
      ({ failed: "failed", "needs-attention": "dropped", done: "completed" } as const)[outcome],
    );
    const result = selectSessionMark(
      { aiStatusBySession: {} },
      {},
      { ...NO_AGENT, stateSince: {} },
      { sessionId: "s" },
      {},
      { s: outcome },
    );
    expect(result?.mark).toBe(mark);
    if (outcome === "needs-attention") expect(result?.words).toBe("Needs you");
  });

  it("a done reads ready, looked at or not", () => {
    expect(facts({ a: "done" }, { stateSince: { a: 10 } }, ["a"]).mark).toBe("ready");
  });

  it("a roll-up over finished turns is ready", () => {
    const f = facts({ a: "done", b: "done" }, { stateSince: { a: 10, b: 10 } }, ["a", "b"]);
    expect(f.mark).toBe("ready");
    expect(f.counts).toEqual({ ready: 2 });
  });

  it("an error outranks a finished turn", () => {
    expect(
      facts({ a: "done", b: "error" }, { stateSince: { a: 10, b: 10 } }, ["a", "b"]).mark,
    ).toBe("error");
  });
});

describe("selectScopeMarkKey — the words' facts over a scope", () => {
  it("nothing when no member shows a status", () => {
    expect(facts({}, {}, ["a"])).toEqual({ counts: {} });
  });

  it("a single member carries its own facts: an end says how, once, with no age", () => {
    const f = facts(
      { a: "running" },
      {
        stateSince: { a: 5 },
        exitBySession: {
          b: { code: 137, signal: null, requested: false, outcome: "crashed", at: 7 },
        },
      },
      ["b"],
    );
    expect(f.single).toEqual({ mark: "crashed", since: null, exit: "exit 137" });
  });

  it("a clean exit CodeSense did not ask for reads ended; one it asked for, nothing", () => {
    const exit = (requested: boolean) => ({
      exitBySession: { a: { code: 0, signal: null, requested, outcome: "done" as const } },
    });
    expect(facts({}, exit(false), ["a"]).mark).toBe("ended");
    expect(facts({}, exit(true), ["a"]).mark).toBeUndefined();
  });

  it("a pending question reads the question mark, over what its status says", () => {
    const f = facts({ a: "running" }, {}, ["a"], { q: { sessionId: "a" } });
    // a pending ask gets the specific `question` mark, not the
    // generic `needs` bucket.
    expect(f.mark).toBe("question");
    expect(f.single?.asking).toBe(true);
  });

  it("several members are counted by mark, most urgent on top", () => {
    const f = facts({ a: "running", b: "running", c: "needs-permission" }, {}, ["a", "b", "c"]);
    // finer vocabulary — `needs-permission` dot projects to the
    // specific `permission` mark; a process-only running dot is unknown.
    expect(f.mark).toBe("permission");
    expect(f.counts).toEqual({ unknown: 2, permission: 1 });
    expect(f.single).toBeUndefined();
  });
});
