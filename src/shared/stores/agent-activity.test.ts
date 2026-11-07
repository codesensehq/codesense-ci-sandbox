// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ActivitySnapshot } from "@/tauri/commands";
import {
  admitsAgentUpdate,
  agentInputSeq,
  resetPrunedAgentSessionsForTests,
  useAgentStateStore,
} from "./agent-state-store";
const snapshot = (patch: Partial<ActivitySnapshot> = {}): ActivitySnapshot => ({
  session: "s",
  identity: { hostBootId: "boot", sessionIncarnation: "pty", invocationId: "run" },
  revision: 2,
  turnGeneration: 1,
  provider: "claude",
  state: "needs-permission",
  observed: true,
  limited: true,
  exit: null,
  asks: ["ask"],
  ...patch,
});
beforeEach(() => {
  resetPrunedAgentSessionsForTests();
  useAgentStateStore.setState({
    bySession: {},
    activityBySession: {},
    exitBySession: {},
    outputAtBySession: {},
    completionSeqBySession: {},
  });
});
afterEach(() => vi.useRealTimers());
describe("host activity projection", () => {
  it("retains output without creating a lifecycle record and drops it on reset/prune", () => {
    const store = useAgentStateStore.getState();
    store.recordOutput("headless", 1000);
    expect(useAgentStateStore.getState().bySession.headless).toBeUndefined();
    expect(useAgentStateStore.getState().outputAtBySession.headless).toBe(1000);
    store.recordOutput("headless", 1100);
    expect(useAgentStateStore.getState().outputAtBySession.headless).toBe(1000);
    store.reset("headless");
    expect(useAgentStateStore.getState().outputAtBySession.headless).toBeUndefined();
    store.recordOutput("headless", 2000);
    store.remove(["headless"]);
    store.recordOutput("headless", 3000);
    expect(useAgentStateStore.getState().outputAtBySession.headless).toBeUndefined();
  });

  it.each([undefined, NaN, Infinity, -1, "0", null])(
    "missing or malformed activity age %s never invents freshness",
    (age) => {
      vi.useFakeTimers();
      vi.setSystemTime(10_000);
      const store = useAgentStateStore.getState();
      const old = snapshot({ state: "working", activityAgeMs: age as number });
      store.recordActivity(old, agentInputSeq());
      expect(useAgentStateStore.getState().bySession.s.activityAt).toBeUndefined();
      store.recordOutput("s", 11_000);
      store.recordActivity({ ...old, revision: 3 }, agentInputSeq());
      expect(useAgentStateStore.getState().bySession.s.activityAt).toBe(11_000);
    },
  );

  it("projects age from receipt, bounds underflow, and cannot refresh on old or duplicate replay", () => {
    vi.useFakeTimers();
    vi.setSystemTime(900_000);
    const store = useAgentStateStore.getState();
    const aged = snapshot({ state: "thinking", activityAgeMs: 600_000 });
    store.recordActivity(aged, agentInputSeq(), 800_000);
    expect(useAgentStateStore.getState().bySession.s.activityAt).toBe(200_000);
    vi.setSystemTime(1_200_000);
    store.activityUnavailable(["s"]);
    store.recordActivity({ ...aged, activityAgeMs: 0 }, agentInputSeq());
    expect(store.recordActivity({ ...aged, revision: 1, activityAgeMs: 0 }, agentInputSeq())).toBe(
      false,
    );
    expect(useAgentStateStore.getState().bySession.s.activityAt).toBe(200_000);
    store.recordActivity(
      snapshot({ session: "underflow", activityAgeMs: Number.MAX_VALUE }),
      agentInputSeq(),
    );
    expect(useAgentStateStore.getState().bySession.underflow.activityAt).toBe(0);
  });

  it("accepts first valid equal-revision metadata once and preserves fresher PTY evidence", () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000);
    const store = useAgentStateStore.getState();
    store.recordActivity(snapshot({ state: "working" }), agentInputSeq());
    store.recordOutput("s", 900);
    store.recordActivity(
      snapshot({ state: "working", toolOpen: false, activityAgeMs: 500 }),
      agentInputSeq(),
    );
    expect(useAgentStateStore.getState().bySession.s.activityAt).toBe(900);
    expect(useAgentStateStore.getState().activityBySession.s.snapshot?.toolOpen).toBe(false);
    vi.setSystemTime(3_000);
    store.recordActivity(snapshot({ state: "working", activityAgeMs: 0 }), agentInputSeq());
    expect(useAgentStateStore.getState().bySession.s.activityAt).toBe(900);
    store.recordActivity(snapshot({ session: "first", activityAgeMs: undefined }), agentInputSeq());
    store.recordActivity(snapshot({ session: "first", activityAgeMs: 200 }), agentInputSeq());
    expect(useAgentStateStore.getState().bySession.first.activityAt).toBe(2_800);
  });

  it("retains genuine legacy tool evidence only within the same invocation", () => {
    const store = useAgentStateStore.getState();
    store.recordSignal("s", { tier: 1, signal: { kind: "claude-hook", event: "PreToolUse" } });
    const at = useAgentStateStore.getState().bySession.s.activityAt;
    store.recordActivity(snapshot({ state: "working" }), agentInputSeq());
    expect(useAgentStateStore.getState().bySession.s).toMatchObject({
      openTools: 1,
      activityAt: at,
    });
    store.recordActivity(snapshot({ state: "working", toolOpen: false }), agentInputSeq());
    expect(useAgentStateStore.getState().bySession.s.openTools).toBeUndefined();
    store.recordActivity(snapshot({ state: "working", revision: 3 }), agentInputSeq());
    expect(useAgentStateStore.getState().bySession.s.openTools).toBeUndefined();
    store.recordActivity(
      snapshot({ state: "working", revision: 4, toolOpen: false }),
      agentInputSeq(),
    );
    expect(useAgentStateStore.getState().bySession.s.openTools).toBeUndefined();
    store.recordActivity(
      snapshot({ identity: { ...snapshot().identity, invocationId: "next" }, state: "working" }),
      agentInputSeq(),
    );
    expect(useAgentStateStore.getState().bySession.s.activityAt).toBeUndefined();
    expect(useAgentStateStore.getState().bySession.s.openTools).toBeUndefined();
  });

  it("projects the compatible blocked reason and clears it on the next host hook", () => {
    const store = useAgentStateStore.getState();
    store.recordActivity(snapshot({ reason: "blocked", asks: [] }), agentInputSeq());
    expect(useAgentStateStore.getState().bySession.s.state).toBe("blocked");
    store.recordActivity(
      snapshot({ revision: 3, state: "thinking", reason: null, asks: [] }),
      agentInputSeq(),
    );
    expect(useAgentStateStore.getState().bySession.s.state).toBe("thinking");
    store.recordActivity(snapshot({ revision: 4, reason: null, asks: [] }), agentInputSeq());
    expect(useAgentStateStore.getState().bySession.s.state).toBe("needs-permission");
  });

  it("freshens unchanged-state hooks, host revisions and output, never probes or snapshot polls", () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000);
    const store = useAgentStateStore.getState();
    const hook = () =>
      store.recordSignal("s", {
        tier: 1,
        signal: { kind: "claude-hook", event: "UserPromptSubmit" },
      });
    hook();
    expect(useAgentStateStore.getState().bySession.s.activityAt).toBe(1_000);
    vi.setSystemTime(2_000);
    hook();
    expect(useAgentStateStore.getState().bySession.s.activityAt).toBe(2_000);
    vi.setSystemTime(3_000);
    store.recordProcess("s", { kind: "alive" }, agentInputSeq());
    expect(useAgentStateStore.getState().bySession.s.activityAt).toBe(2_000);
    store.recordActivity(snapshot({ state: "thinking", activityAgeMs: 0 }), agentInputSeq());
    expect(useAgentStateStore.getState().bySession.s.activityAt).toBe(3_000);
    vi.setSystemTime(4_000);
    store.recordActivity(snapshot({ state: "thinking", activityAgeMs: 0 }), agentInputSeq());
    expect(useAgentStateStore.getState().bySession.s.activityAt).toBe(3_000);
    store.recordActivity(
      snapshot({ state: "thinking", revision: 3, activityAgeMs: 0 }),
      agentInputSeq(),
    );
    expect(useAgentStateStore.getState().bySession.s.activityAt).toBe(4_000);
    vi.setSystemTime(5_000);
    store.recordOutput("s");
    expect(useAgentStateStore.getState().bySession.s.activityAt).toBe(5_000);
    store.remove(["s"]);
    store.recordOutput("s", 6_000);
    expect(useAgentStateStore.getState().bySession.s).toBeUndefined();
  });
  it("restores host waiting directly and legacy hooks/probes/exit cannot overwrite", () => {
    const store = useAgentStateStore.getState();
    expect(store.recordActivity(snapshot(), agentInputSeq())).toBe(true);
    store.recordSignal("s", { tier: 1, signal: { kind: "claude-hook", event: "Stop" } });
    store.recordProcess("s", { kind: "exited", code: 1 }, Infinity);
    expect(store.recordPtyExit("s", { code: 1, signal: null, requested: false })).toBeUndefined();
    expect(store.recordCommandEnd("s", 1)).toBeUndefined();
    expect(admitsAgentUpdate("s")).toBe(false);
    expect(useAgentStateStore.getState().bySession.s).toMatchObject({
      state: "needs-permission",
      t1: null,
      t2: null,
    });
  });
  it("unavailability preserves last observation and never invents crash or done", () => {
    const store = useAgentStateStore.getState();
    store.recordActivity(snapshot(), agentInputSeq());
    store.activityUnavailable(["s"]);
    expect(useAgentStateStore.getState().activityBySession.s.freshness).toBe("unavailable");
    expect(useAgentStateStore.getState().bySession.s.state).toBe("needs-permission");
  });
  it("prune rejects pending snapshots and reset rejects retired incarnation", () => {
    const store = useAgentStateStore.getState();
    store.recordActivity(snapshot(), agentInputSeq());
    const sent = agentInputSeq();
    store.remove(["s"]);
    expect(store.recordActivity(snapshot(), sent)).toBe(false);
    store.reset("s");
    expect(store.recordActivity(snapshot(), agentInputSeq())).toBe(false);
    expect(
      store.recordActivity(
        snapshot({
          identity: { hostBootId: "boot", sessionIncarnation: "new", invocationId: "new" },
        }),
        agentInputSeq(),
      ),
    ).toBe(true);
  });
  it("rejects older revision, while same snapshot can restore freshness", () => {
    const store = useAgentStateStore.getState();
    store.recordActivity(snapshot(), agentInputSeq());
    store.activityUnavailable(["s"]);
    expect(store.recordActivity(snapshot({ revision: 1, state: "done" }), agentInputSeq())).toBe(
      false,
    );
    expect(store.recordActivity(snapshot(), agentInputSeq())).toBe(true);
    expect(useAgentStateStore.getState().activityBySession.s.freshness).toBe("current");
  });
  it("an unobserved snapshot does not block the fallbacks, and does not erase what they said", () => {
    const store = useAgentStateStore.getState();
    store.recordActivity(snapshot({ observed: false, state: "idle", asks: [] }), agentInputSeq());
    expect(admitsAgentUpdate("s")).toBe(true);
    store.recordSignal("s", {
      tier: 1,
      signal: { kind: "claude-hook", event: "PermissionRequest" },
    });
    expect(useAgentStateStore.getState().bySession.s.state).toBe("needs-permission");
    // A newer snapshot that still observed nothing keeps the fallback's word.
    store.recordActivity(
      snapshot({ observed: false, state: "idle", asks: [], revision: 3 }),
      agentInputSeq(),
    );
    expect(useAgentStateStore.getState().bySession.s.state).toBe("needs-permission");
    // Once the host observes the agent, it is authoritative again.
    store.recordActivity(snapshot({ revision: 4, state: "thinking", asks: [] }), agentInputSeq());
    expect(admitsAgentUpdate("s")).toBe(false);
    expect(useAgentStateStore.getState().bySession.s.state).toBe("thinking");
  });
  it("a snapshot whose agent exited admits the fallbacks", () => {
    const store = useAgentStateStore.getState();
    store.recordActivity(
      snapshot({ exit: { code: 0, signal: null, requested: false }, state: "done", asks: [] }),
      agentInputSeq(),
    );
    expect(admitsAgentUpdate("s")).toBe(true);
  });
});
