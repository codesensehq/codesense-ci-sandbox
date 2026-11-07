// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { SessionListRow } from "@/tauri/commands";

import {
  instancesByAgent,
  mergeInstanceSessions,
  needsMailBySender,
  runningInstances,
  startedAtByHandle,
} from "./agent-running";

const KEY = "acme/api/triage";

function session(over: Partial<SessionListRow>): SessionListRow {
  return {
    handle: "h1",
    displayName: "Triage 0912",
    state: "running",
    launchedBy: null,
    unread: null,
    oldestUnreadAt: null,
    agent: KEY,
    ...over,
  };
}

describe("runningInstances", () => {
  it("lists only this agent's live sessions", () => {
    const list = runningInstances({
      agentKey: KEY,
      sessions: [
        session({ handle: "a" }),
        session({ handle: "b", agent: "acme/api/other" }),
        session({ handle: "c", state: "exited" }),
        session({ handle: "d", state: "offline" }),
      ],
      startedAt: {},
    });
    expect(list.map((i) => i.handle)).toEqual(["a"]);
  });

  it("puts what waits on you first, then running, then idle", () => {
    const list = runningInstances({
      agentKey: KEY,
      sessions: [
        session({ handle: "idle", state: "idle" }),
        session({ handle: "run", state: "running" }),
        session({ handle: "ask", state: "needs-you" }),
      ],
      startedAt: {},
    });
    expect(list.map((i) => i.handle)).toEqual(["ask", "run", "idle"]);
    expect(list.map((i) => i.state)).toEqual(["needs", "running", "idle"]);
  });

  it("says where it runs: its story, its workspace, else the team folder", () => {
    const where = (over: Partial<SessionListRow>) =>
      runningInstances({ agentKey: KEY, sessions: [session(over)], startedAt: {} })[0].where;
    expect(where({ story: "SV-REL-4" })).toBe("Story SV-REL-4");
    expect(where({ workspace: "feat-x" })).toBe("feat-x");
    expect(where({})).toBe("Team folder");
  });

  it("names a remote machine, and leaves This Mac out", () => {
    const machine = (over: Partial<SessionListRow>) =>
      runningInstances({ agentKey: KEY, sessions: [session(over)], startedAt: {} })[0].machine;
    expect(machine({ machine: "Build box" })).toBe("Build box");
    expect(machine({})).toBeNull();
  });

  it("carries unread mail and the start time only when known", () => {
    const [a, b] = runningInstances({
      agentKey: KEY,
      sessions: [
        session({ handle: "a", unread: 2 }),
        session({ handle: "b", unread: null, state: "idle" }),
      ],
      startedAt: { a: new Date(2026, 9, 24, 9, 12).getTime() },
    });
    expect(a.unread).toBe(2);
    expect(a.since).toBe("09:12");
    expect(b.unread).toBe(0);
    expect(b.since).toBeNull();
  });

  it("counts the waiting ones", () => {
    const list = runningInstances({
      agentKey: KEY,
      sessions: [session({ handle: "a", state: "needs-you" }), session({ handle: "b" })],
      startedAt: {},
    });
    expect(list.filter((i) => i.state === "needs")).toHaveLength(1);
  });
});

describe("mergeInstanceSessions", () => {
  it("takes a listed session that names its agent over the team's own read of it", () => {
    const listed = [session({ handle: "a", displayName: "From the roster", state: "idle" })];
    const known = [session({ handle: "a", displayName: "From the team" })];
    const merged = mergeInstanceSessions(listed, known);
    expect(merged).toHaveLength(1);
    expect(merged[0].displayName).toBe("From the roster");
  });

  it("takes the team's read when the roster row doesn't name an agent", () => {
    const listed = [{ ...session({ handle: "a" }), agent: undefined, state: undefined }] as never;
    const merged = mergeInstanceSessions(listed, [session({ handle: "a", displayName: "Team" })]);
    expect(merged).toHaveLength(1);
    expect(merged[0].displayName).toBe("Team");
  });

  it("adds sessions only the team knows", () => {
    expect(
      mergeInstanceSessions([session({ handle: "a" })], [session({ handle: "b" })]).map(
        (s) => s.handle,
      ),
    ).toEqual(["a", "b"]);
  });
});

describe("needs-you mail and unknown sessions", () => {
  it("counts an idle sender with unread needs-you mail as waiting on you", () => {
    const [a, b] = runningInstances({
      agentKey: KEY,
      sessions: [
        session({ handle: "quiet", state: "idle" }),
        session({ handle: "asked", state: "idle" }),
      ],
      startedAt: {},
      needsBySender: { asked: 2 },
    });
    expect(a.handle).toBe("asked");
    expect(a.state).toBe("needs");
    expect(b.state).toBe("idle");
  });

  it("counts a session once when its terminal and its mail both wait on you", () => {
    const list = runningInstances({
      agentKey: KEY,
      sessions: [session({ handle: "both", state: "needs-you" })],
      startedAt: {},
      needsBySender: { both: 3 },
    });
    expect(list).toHaveLength(1);
    expect(list.filter((i) => i.state === "needs")).toHaveLength(1);
  });

  it("keeps an unknown session unknown until activity is seen", () => {
    const [i] = runningInstances({
      agentKey: KEY,
      sessions: [session({ handle: "u", state: "unknown" })],
      startedAt: {},
    });
    expect(i.state).toBe("unknown");
    expect(i.stateWord).toBe("Not seen yet");
  });

  it("orders needs you, working, not seen, then idle", () => {
    const list = runningInstances({
      agentKey: KEY,
      sessions: [
        session({ handle: "i", state: "idle" }),
        session({ handle: "u", state: "unknown" }),
        session({ handle: "r", state: "running" }),
        session({ handle: "n", state: "needs-you" }),
      ],
      startedAt: {},
    });
    expect(list.map((x) => x.handle)).toEqual(["n", "r", "u", "i"]);
  });
});

describe("instancesByAgent", () => {
  it("groups every live session by the agent it runs", () => {
    const map = instancesByAgent({
      sessions: [
        session({ handle: "a" }),
        session({ handle: "b", agent: "acme/api/other" }),
        session({ handle: "c", agent: "acme/api/other", state: "exited" }),
        { ...session({ handle: "d" }), agent: undefined },
      ],
      startedAt: {},
    });
    expect([...map.keys()].sort()).toEqual(["acme/api/other", KEY]);
    expect(map.get(KEY)!.map((i) => i.handle)).toEqual(["a"]);
    expect(map.get("acme/api/other")!.map((i) => i.handle)).toEqual(["b"]);
  });
});

describe("needsMailBySender and startedAtByHandle", () => {
  it("sums needs-you mail per sender across every team's counts", () => {
    const counts = [{ needsYouBySender: { a: 1, b: 2 } }, { needsYouBySender: { a: 2 } }] as never;
    expect(needsMailBySender(counts)).toEqual({ a: 3, b: 2 });
  });

  it("reads start times from every projected session row, whichever team it is in", () => {
    expect(
      startedAtByHandle([
        { handle: "x", startedAtMs: 5 },
        { handle: "y", startedAtMs: null },
        { handle: "z" },
      ]),
    ).toEqual({ x: 5 });
  });
});
