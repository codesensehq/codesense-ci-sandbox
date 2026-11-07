// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { FakeEventBus, FakeTerminalServer } from "./__test-utils__/fake-terminal-server";
import { RESTORE_FAILED, createTerminalManager, type TerminalManager } from "./terminal-manager";
import {
  resetPrunedAgentSessionsForTests,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";

const authority = vi.hoisted(() => ({
  active: "501" as string | null,
  retired: new Set<() => void>(),
}));
vi.mock("@/shared/lib/private-terminal-authority", () => ({
  acceptsPrivateAuthority: (epoch?: string | null) => epoch == null || epoch === authority.active,
  onPrivateAuthorityRetired: (callback: () => void) => {
    authority.retired.add(callback);
    return () => authority.retired.delete(callback);
  },
}));
function retirePrivate() {
  authority.active = "502";
  for (const retire of [...authority.retired]) retire();
}

const fake = vi.hoisted(() => ({
  bus: null as unknown as import("./__test-utils__/fake-terminal-server").FakeEventBus,
  server: null as unknown as import("./__test-utils__/fake-terminal-server").FakeTerminalServer,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) =>
    fake.bus.listen(name, handler),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
const transport = vi.hoisted(() => ({
  reserveTerminalStream: vi.fn(() => fake.server.reserve()),
  subscribeTerminal: vi.fn((session: string, stream?: number) =>
    fake.server.subscribe(session, stream),
  ),
  unsubscribeTerminal: vi.fn((stream: number) => fake.server.unsubscribe(stream)),
  writePty: vi.fn(async () => {}),
  resizePty: vi.fn(async () => {}),
}));
vi.mock(
  "@/features/viewer/lib/cli-workspace/transport/terminal-session-source",
  async (importOriginal) => ({
    ...(await importOriginal<
      typeof import("@/features/viewer/lib/cli-workspace/transport/terminal-session-source")
    >()),
    ...transport,
  }),
);

const OPTS = { fontSizePx: 13 };

/** Run the clock until `pending` settles (a restore waits on xterm's parser). */
async function until<T>(pending: Promise<T>): Promise<T> {
  let done = false;
  const settled = pending.finally(() => (done = true));
  for (let i = 0; i < 500 && !done; i++) await vi.advanceTimersByTimeAsync(10);
  return settled;
}
let manager: TerminalManager;

function privateSubscribe() {
  transport.subscribeTerminal.mockImplementationOnce(async (session, stream) => ({
    ...(await fake.server.subscribe(session, stream)),
    privateAuthorityEpoch: "501",
  }));
}

beforeEach(() => {
  vi.clearAllMocks();
  authority.active = "501";
  fake.bus = new FakeEventBus();
  fake.server = new FakeTerminalServer(fake.bus);
  manager = createTerminalManager({
    createWebgl: () => {
      throw new Error("no WebGL in tests");
    },
  });
  vi.useFakeTimers({
    toFake: ["setTimeout", "clearTimeout", "requestAnimationFrame", "cancelAnimationFrame"],
  });
});

afterEach(() => {
  for (const s of ["private", "local"]) manager.dispose(s);
  document.body.innerHTML = "";
  vi.useRealTimers();
});

describe("terminal output refreshes the shared agent activity clock", () => {
  let now: number;
  beforeEach(() => {
    now = 1_000_000;
    vi.spyOn(Date, "now").mockImplementation(() => now);
    resetPrunedAgentSessionsForTests();
    useAgentStateStore.setState({ bySession: {}, activityBySession: {}, exitBySession: {} });
  });
  afterEach(() => {
    vi.restoreAllMocks();
    useAgentStateStore.setState({ bySession: {}, activityBySession: {}, exitBySession: {} });
  });

  function observe(sessionId: string) {
    useAgentStateStore.getState().recordSignal(sessionId, {
      tier: 1,
      signal: { kind: "claude-hook", event: "UserPromptSubmit" },
    });
  }
  const activityAt = (sessionId: string) =>
    useAgentStateStore.getState().bySession[sessionId]?.activityAt;

  it("refreshes on admitted live bytes, but not snapshot restore, duplicate bytes or a retired stream", async () => {
    fake.server.open("local");
    observe("local");
    const original = now;
    fake.server.produce("local", "historical");
    now += 600_000 - 1_000;
    await until(manager.ensure("local", OPTS));
    expect(activityAt("local")).toBe(original);

    const [stream] = fake.server.boundStreams();
    fake.server.produce("local", "live");
    expect(activityAt("local")).toBe(now);
    const liveAt = now;
    now += 2_000;
    fake.bus.emit(`pty-output-stream-${stream}`, {
      data: "live",
      seq: fake.server.seqOf("local"),
    });
    expect(activityAt("local")).toBe(liveAt);

    manager.dispose("local");
    fake.bus.emit(`pty-output-stream-${stream}`, { data: "stale", seq: 100 });
    expect(activityAt("local")).toBe(liveAt);
    now = liveAt + 600_000;
  });

  it("refuses unstamped and revoked private output without refreshing activity", async () => {
    fake.server.open("private");
    observe("private");
    const original = now;
    privateSubscribe();
    await until(manager.ensure("private", OPTS));
    const [stream] = fake.server.boundStreams();
    now += 600_000 - 1_000;
    fake.bus.emit(`pty-output-stream-${stream}`, { data: "unowned", seq: 7 });
    fake.bus.emit(`pty-output-stream-${stream}`, {
      data: "wrong epoch",
      seq: 11,
      privateAuthorityEpoch: "500",
    });
    expect(activityAt("private")).toBe(original);
    fake.bus.emit(`pty-output-stream-${stream}`, {
      data: "owned",
      seq: 5,
      privateAuthorityEpoch: "501",
    });
    expect(activityAt("private")).toBe(now);
    const liveAt = now;
    now += 2_000;
    retirePrivate();
    fake.bus.emit(`pty-output-stream-${stream}`, {
      data: "revoked",
      seq: 12,
      privateAuthorityEpoch: "501",
    });
    expect(activityAt("private")).toBe(liveAt);
  });
});

const text = (sessionId: string, row = 0) =>
  manager.terminal(sessionId)?.buffer.active.getLine(row)?.translateToString(true);

describe("a private-account session whose authority retires", () => {
  it("drops its queued output and its display; a local session is untouched", async () => {
    fake.server.open("private");
    fake.server.open("local");
    privateSubscribe();
    const retired = vi.fn();
    await until(manager.ensure("private", OPTS));
    manager.listen("private", { retired });
    await until(manager.ensure("local", OPTS));
    const slot = document.createElement("div");
    document.body.appendChild(slot);
    manager.attach("private", slot, { focus: false });
    const [privateStream] = fake.server.boundStreams();
    const element = manager.terminal("private")!.element!;

    fake.bus.emit(`pty-output-stream-${privateStream}`, {
      data: "PRIVATE_QUEUED",
      seq: 14,
      privateAuthorityEpoch: "501",
    });
    retirePrivate();
    await vi.runAllTimersAsync();

    expect(retired).toHaveBeenCalledOnce();
    expect(manager.terminal("private")).toBeNull();
    expect(element.isConnected).toBe(false);
    expect(fake.bus.count(`pty-output-stream-${privateStream}`)).toBe(0);
    expect(document.body.textContent).not.toContain("PRIVATE_QUEUED");

    fake.server.produce("local", "still local");
    await vi.runAllTimersAsync();
    expect(text("local")).toBe("still local");

    // The next show builds a fresh display, with none of the retired bytes.
    await until(manager.ensure("private", OPTS));
    await vi.runAllTimersAsync();
    expect(manager.terminal("private")!.element).not.toBe(element);
    expect(text("private")).not.toContain("PRIVATE_QUEUED");
  });

  it("a retirement while subscribe is pending refuses the late private seed", async () => {
    fake.server.open("private");
    fake.server.holdSubscribes = true;
    privateSubscribe();
    const opening = manager.ensure("private", OPTS);
    await vi.waitFor(() => expect(transport.subscribeTerminal).toHaveBeenCalled());
    retirePrivate();
    fake.server.releaseSubscribes();
    // The pane fails with the reason and offers Retry, as for any refused attach.
    expect((await until(opening))?.outcome).toEqual({ kind: "failed", reason: RESTORE_FAILED });
    expect(fake.server.boundStreams()).toEqual([]);
  });

  it("an unstamped replay on a private stream is refused; later stamped output is written", async () => {
    fake.server.open("private");
    privateSubscribe();
    await until(manager.ensure("private", OPTS));
    const [stream] = fake.server.boundStreams();
    fake.bus.emit(`pty-output-stream-${stream}`, { data: "downgrade", seq: 9 });
    fake.bus.emit(`pty-output-stream-${stream}`, {
      data: "owned",
      seq: 5,
      privateAuthorityEpoch: "501",
    });
    await vi.runAllTimersAsync();
    expect(text("private")).toBe("owned");
  });
});

const remoteAccess = vi.hoisted(() => ({ allowed: true }));
vi.mock("@/features/machines/stores/machine-projection-selectors", () => ({
  machineSessionTarget: (_team: string, target: import("@/tauri/commands").RuntimeSessionTarget) =>
    remoteAccess.allowed ? target : undefined,
}));
import {
  terminalDisplayKey,
  type RemoteTerminalDisplayReference,
} from "./terminal-display-reference";
function remote(machine: string): RemoteTerminalDisplayReference {
  return {
    teamKey: "acme:core",
    workspaceKey: "acme:core|root",
    privateAuthorityEpoch: "501",
    target: {
      member: { machineId: machine, slot: "remote" },
      admissionRevision: "1",
      namespace: { serverId: "same-server", epoch: "epoch", generation: "1" },
      session: "local",
    },
  };
}

describe("exact remote displays", () => {
  beforeEach(() => {
    remoteAccess.allowed = true;
  });
  it("a quick view that borrowed a display it can no longer show parks it on close", async () => {
    const a = remote("a");
    fake.server.open("local");
    transport.subscribeTerminal.mockImplementation(async (_target, stream) => ({
      ...(await fake.server.subscribe("local", stream)),
      privateAuthorityEpoch: "501",
    }));
    await until(manager.ensure(a, OPTS));
    const [tab, popup] = [document.createElement("div"), document.createElement("div")];
    document.body.append(tab, popup);
    manager.attach(a, tab, { focus: false, visible: false });
    manager.attach(a, popup, { focus: false, borrow: true });
    // The machine is no longer reachable from this team: the tab cannot take the display back.
    remoteAccess.allowed = false;
    manager.detach(a, popup);
    expect(manager.holds(a, popup)).toBe(false);
    expect(manager.holds(a, tab)).toBe(false);
    manager.dispose(a);
  });

  it("isolates equal local and two remote IDs, preserves displays across hide, and sends exact input", async () => {
    const a = remote("a"),
      b = remote("b");
    fake.server.open("local");
    transport.subscribeTerminal.mockImplementation(async (target, stream) => ({
      ...(await fake.server.subscribe(typeof target === "string" ? target : "local", stream)),
      ...(typeof target === "string" ? {} : { privateAuthorityEpoch: "501" }),
    }));
    await until(manager.ensure("local", OPTS));
    await until(manager.ensure(a, OPTS));
    await until(manager.ensure(b, OPTS));
    expect(terminalDisplayKey(a)).not.toBe(terminalDisplayKey(b));
    const term = manager.terminal(a)!;
    expect(term).not.toBe(manager.terminal("local"));
    expect(term).not.toBe(manager.terminal(b));
    const slot = document.createElement("div");
    document.body.appendChild(slot);
    manager.attach(a, slot, { focus: false });
    manager.detach(a, slot);
    await until(manager.ensure(a, OPTS));
    manager.attach(a, slot, { focus: false });
    expect(manager.terminal(a)).toBe(term);
    expect(transport.subscribeTerminal).toHaveBeenCalledTimes(3);
    expect(manager.holds(a, slot)).toBe(true);
    expect(manager.holds(b, slot)).toBe(false);
    expect(manager.webglLease(a)).not.toBeNull();
    manager.requestResize(a, 111, 33);
    expect(manager.size(a)).toEqual({ cols: 111, rows: 33 });
    expect(manager.size(b)).toEqual({ cols: 100, rows: 30 });
    expect(transport.resizePty).toHaveBeenCalledExactlyOnceWith(a.target, 111, 33);
    manager.requestResize(a, 111, 33);
    expect(transport.resizePty).toHaveBeenCalledTimes(1);
    remoteAccess.allowed = false;
    manager.requestResize(a, 112, 34);
    expect(transport.resizePty).toHaveBeenCalledTimes(1);
    remoteAccess.allowed = true;
    term.input("only-a", true);
    expect(transport.writePty).toHaveBeenLastCalledWith(a.target, "only-a");
    const remoteStream = fake.server.boundStreams()[1];
    fake.server.loseStreams([remoteStream]);
    for (let i = 0; i < 100 && transport.resizePty.mock.calls.length < 2; i++)
      await vi.advanceTimersByTimeAsync(10);
    expect(transport.resizePty).toHaveBeenCalledTimes(2);
    expect(transport.resizePty).toHaveBeenLastCalledWith(a.target, 111, 33);
    expect(manager.size(a)).toEqual({ cols: 111, rows: 33 });
    manager.dispose(a);
    manager.dispose(b);
  });

  it("refuses missing private epoch in a remote subscribe reply", async () => {
    const ref = remote("a");
    fake.server.open("local");
    transport.subscribeTerminal.mockImplementation(async (_target, stream) =>
      fake.server.subscribe("local", stream),
    );
    const handle = await until(manager.ensure(ref, OPTS));
    expect(handle?.outcome.kind).toBe("failed");
    expect(fake.server.boundStreams()).toEqual([]);
    manager.dispose(ref);
  });

  it("drops a held remote reply after access retirement without touching a colliding local display", async () => {
    const ref = remote("a");
    fake.server.open("local");
    transport.subscribeTerminal.mockImplementation(async (_target, stream) => ({
      ...(await fake.server.subscribe("local", stream)),
      privateAuthorityEpoch: "501",
    }));
    await until(manager.ensure("local", OPTS));
    fake.server.holdSubscribes = true;
    const opening = manager.ensure(ref, OPTS);
    await vi.waitFor(() => expect(transport.subscribeTerminal).toHaveBeenCalledTimes(2));
    remoteAccess.allowed = false;
    fake.server.releaseSubscribes();
    expect(await until(opening)).toBeNull();
    expect(manager.terminal(ref)).toBeNull();
    expect(manager.terminal("local")).not.toBeNull();
    manager.dispose(ref);
  });
});
