// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  type ActivityAlert,
  type AgentAlert,
  type AlertItem,
  alertLine,
  composeNotification,
  type FinishedAlert,
  type TerminalAlert,
  entryKey,
  isBlockingActivityAlert,
  planAlerts,
} from "./attention-alerts";

const item = (id: string, kind: TerminalAlert["kind"] = "permission"): TerminalAlert => ({
  sessionId: `sid-${id}`,
  teamKey: "acme:default",
  workspaceId: "w1",
  tabId: id,
  hidden: false,
  label: "SW-1",
  tabName: `Tab ${id}`,
  kind,
});
const activity = (id: number, summary = `item ${id}`): ActivityAlert => ({
  kind: "activity",
  id,
  summary,
  open: () => {},
});

describe("planAlerts — state entry × setting × re-notify floor", () => {
  const A = item("a");
  const known = (...xs: AlertItem[]) => new Set(xs.map(entryKey));

  it.each<[string, Set<string>, TerminalAlert[], boolean, Set<string>, string[]]>([
    ["new entry, on → fresh", known(), [A], true, new Set(), ["sid-a:permission"]],
    ["new entry, setting off → absorbed", known(), [A], false, new Set(), []],
    ["same entry again (a status tick) → nothing", known(A), [A], true, new Set(), []],
    [
      "re-entry notified too recently (a flapping tab) → absorbed",
      known(),
      [A],
      true,
      known(A),
      [],
    ],
    [
      "same tab, different state → a new entry",
      known(A),
      [item("a", "question")],
      true,
      new Set(),
      ["sid-a:question"],
    ],
  ])("%s", (_label, k, items, enabled, recentlyNotified, fresh) => {
    const out = planAlerts(k, items, { enabled, recentlyNotified });
    expect(out.fresh.map(entryKey)).toEqual(fresh);
    expect([...out.known]).toEqual(items.map(entryKey));
  });

  it("an entry that resolves is forgotten, so re-entering it is considered again", () => {
    const left = planAlerts(known(A), [], { enabled: true });
    expect(left.known.size).toBe(0);
    const back = planAlerts(left.known, [A], { enabled: true });
    expect(back.fresh).toEqual([A]);
  });
});

describe("composeNotification", () => {
  it("describes a self-reported block without requesting an answer or approval", () => {
    expect(composeNotification([item("a", "blocked")])).toEqual({
      title: "SW-1 · Tab a is blocked",
      body: "The agent is blocked.",
    });
    expect(
      composeNotification([{ ...item("a", "blocked"), detail: "Missing credentials" }]),
    ).toEqual({
      title: "SW-1 · Tab a is blocked",
      body: "Blocked: Missing credentials",
    });
  });

  it("one tab: where, and the state in words", () => {
    expect(composeNotification([item("a")])).toEqual({
      title: "SW-1 · Tab a needs your permission",
      body: "The agent is waiting for you to approve a step.",
    });
    expect(composeNotification([{ ...item("a", "question"), detail: "Merge now?" }])).toEqual({
      title: "SW-1 · Tab a has a question for you",
      body: "“Merge now?”",
    });
    expect(composeNotification([{ ...item("a", "crashed"), detail: "exit code 139" }])).toEqual({
      title: "SW-1 · Tab a crashed",
      body: "The agent crashed (exit code 139).",
    });
    expect(composeNotification([activity(1, "Review the plan")])).toEqual({
      title: "Something needs you",
      body: "Review the plan",
    });
  });

  it("an error says the agent hit one, with its reason — never an approval", () => {
    expect(composeNotification([item("a", "failed")])).toEqual({
      title: "SW-1 · Tab a hit an error",
      body: "The agent stopped on an error.",
    });
    expect(composeNotification([{ ...item("a", "failed"), detail: "API error" }])).toEqual({
      title: "SW-1 · Tab a hit an error",
      body: "The agent stopped on an error: API error",
    });
    expect(composeNotification([item("a", "question")]).body).toBe(
      "The agent is waiting for your answer.",
    );
  });

  it("truncates a long question to about 80 characters", () => {
    const { body } = composeNotification([
      { ...item("a", "question"), detail: `${"x".repeat(200)}` },
    ]);
    expect(body).toBe(`“${"x".repeat(79)}…”`);
  });

  it("a burst collapses into one summary", () => {
    const out = composeNotification([
      item("a"),
      item("b", "question"),
      item("c", "crashed"),
      item("d"),
    ]);
    expect(out).toEqual({
      title: "4 agents need you",
      body: [
        "SW-1 · Tab a needs your permission",
        "SW-1 · Tab b has a question for you",
        "SW-1 · Tab c crashed",
        "+1 more",
      ].join("\n"),
    });
    expect(composeNotification([item("a"), activity(2)]).title).toBe("2 items need you");
  });
});

describe("an agent with no tab, and a finished agent", () => {
  const remote: AgentAlert = {
    kind: "permission",
    sessionId: "sid-r",
    label: "Reviewer",
    agent: {
      handle: "reviewer",
      sessionId: "sid-r",
      kind: "agent",
      alive: true,
      backgrounded: false,
      startedWith: null,
      startedAtMs: null,
      launcher: null,
      lastOutputMs: null,
    },
  };
  const finished = (over: Partial<FinishedAlert> = {}): FinishedAlert => ({
    kind: "finished",
    sessionId: "sid-a",
    turnStartedAt: 5,
    durationMs: 4 * 60_000,
    label: "SW-1",
    tabName: "Tab a",
    ...over,
  });

  it("names a tabless agent by itself, and shares its entry with its tab", () => {
    expect(alertLine(remote)).toBe("Reviewer needs your permission");
    expect(entryKey(remote)).toBe(entryKey({ ...item("r"), sessionId: "sid-r" }));
  });

  it("words a startup screen as waiting, with the agent named", () => {
    expect(alertLine({ ...remote, kind: "startup" })).toBe(
      "Reviewer is waiting on a startup screen",
    );
  });

  it("a finished agent: its place, and its summary or how long it worked", () => {
    expect(composeNotification([finished()])).toEqual({
      title: "SW-1 · Tab a finished",
      body: "Worked for 4 minutes.",
    });
    expect(composeNotification([finished({ detail: "Opened PR #12." })]).body).toBe(
      "Opened PR #12.",
    );
    expect(composeNotification([finished({ durationMs: 35_000 })]).body).toBe(
      "Worked for 35 seconds.",
    );
  });

  it("each turn is its own entry", () => {
    expect(entryKey(finished())).not.toBe(entryKey(finished({ turnStartedAt: 9 })));
  });

  it("a burst counts what needs you; finished agents are listed, not counted as needing you", () => {
    expect(
      composeNotification([item("a"), finished({ sessionId: "sid-b", label: "SW-2" })]),
    ).toEqual({
      title: "1 agent needs you",
      body: ["SW-1 · Tab a needs your permission", "SW-2 · Tab a finished"].join("\n"),
    });
    expect(composeNotification([finished(), finished({ sessionId: "sid-b" })]).title).toBe(
      "2 agents finished",
    );
  });
});

describe("isBlockingActivityAlert", () => {
  const row = (need: ActivityAlert["need"]): ActivityAlert => ({
    kind: "activity",
    id: 1,
    summary: "x",
    need,
    open: () => {},
  });
  it("flags the three BLOCKING needs so the controller never swallows them", () => {
    expect(isBlockingActivityAlert(row("awaiting-answer"))).toBe(true);
    expect(isBlockingActivityAlert(row("needs-approval"))).toBe(true);
    expect(isBlockingActivityAlert(row("awaiting-validation"))).toBe(true);
  });
  it("does NOT flag ambient or absent needs — those stay subject to the opt-in", () => {
    expect(isBlockingActivityAlert(row("needs-attention"))).toBe(false);
    expect(isBlockingActivityAlert(row("done"))).toBe(false);
    expect(isBlockingActivityAlert(row(null))).toBe(false);
    expect(isBlockingActivityAlert(row(undefined))).toBe(false);
  });
  it("never flags a TerminalAlert (terminal rows already notify at the agent floor)", () => {
    const t: TerminalAlert = {
      sessionId: "sid-a",
      teamKey: "acme:default",
      workspaceId: "w1",
      tabId: "a",
      hidden: false,
      label: "SW-1",
      tabName: "Tab a",
      kind: "question",
    };
    expect(isBlockingActivityAlert(t)).toBe(false);
  });
});
