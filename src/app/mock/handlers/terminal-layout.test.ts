// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { MockEventBus } from "../mock-events";
import { MOCK_ORG_SLUG, MOCK_TEAM_NAME, TEAM_CONTENT_PATH } from "../fixtures";
import { terminalLayoutHandlers } from "./terminal-layout";
import { SESSION_NOT_FOUND, attachTerminal } from "@/features/viewer/lib/terminal-attach";
import type {
  TerminalLayoutChanged,
  RuntimeLayoutReply,
  RuntimeAdmissionBaseline,
  TerminalOpenTabRequest,
  TerminalRuntimeEvent,
  TerminalSubscription,
  TerminalText,
} from "@/tauri/commands";

const TEAM_KEY = `${MOCK_ORG_SLUG}:${MOCK_TEAM_NAME}`;
const CONTENT_KEY = `${TEAM_KEY}|generic:${TEAM_CONTENT_PATH}`;

/** The handlers plus every layout change a `terminal-runtime-event` listener
 *  saw, each checked to carry the mock runtime's admitted target. */
function world(search = "") {
  const events = new MockEventBus();
  const changes: TerminalLayoutChanged[] = [];
  const outputs: unknown[] = [];
  events.bind((_callbackId, message) => {
    const { event: name, payload } = message as { event: string; payload: TerminalRuntimeEvent };
    if (name !== "terminal-runtime-event") {
      outputs.push(payload);
      return;
    }
    expect(payload.target.member).toEqual({ machineId: "local", slot: "inProcess" });
    if (payload.event.LayoutChanged) changes.push(payload.event.LayoutChanged);
  });
  events.handlers()["plugin:event|listen"]({ event: "terminal-runtime-event", handler: 1 });
  const h = terminalLayoutHandlers(search, events);
  const snapshot = () => {
    const reply = (h.terminal_layout_snapshot({}) as RuntimeLayoutReply[])[0];
    if (reply.mode !== "live") throw new Error("expected admitted mock runtime");
    return reply.snapshot;
  };
  return { h, events, changes, outputs, snapshot };
}

it("mock admission and snapshot preserve the same production member authority", () => {
  const { h } = world();
  const row = (h.terminal_layout_admission({}) as RuntimeAdmissionBaseline).rows[0];
  const reply = (h.terminal_layout_snapshot({}) as RuntimeLayoutReply[])[0];
  expect(row.state).toBe("live");
  expect(reply.mode).toBe("live");
  if (row.state !== "live" || reply.mode !== "live") throw new Error("expected live member");
  expect(reply.member).toEqual(row.member);
  expect(reply.admissionRevision).toBe(row.revision);
  expect(reply.namespace).toEqual(row.namespace);
  expect(typeof row.namespace.generation).toBe("string");
});

it("scoped mock commands reject stale and wrong-member targets before changing layout", () => {
  const { h, snapshot } = world();
  h.terminal_open_tab({ request: openRequest("shared") });
  const reply = (h.terminal_layout_snapshot({}) as RuntimeLayoutReply[])[0];
  if (reply.mode !== "live") throw new Error("expected live member");
  const target = {
    member: reply.member,
    namespace: reply.namespace,
    admissionRevision: reply.admissionRevision,
    session: "shared",
  };
  const before = snapshot();
  for (const invalid of [
    { ...target, admissionRevision: "0" },
    { ...target, member: { machineId: "local", slot: "sidecar" } },
    { ...target, namespace: { ...target.namespace, generation: "9007199254740993" } },
    { ...target, namespace: null, mode: "historical" },
  ]) {
    expect(() => h.terminal_rename_tab({ session: invalid, name: "wrong" })).toThrow();
    expect(snapshot()).toEqual(before);
  }
  h.terminal_rename_tab({ session: target, name: "selected" });
  expect(snapshot().workspaces[`${TEAM_KEY}|w1`].tabs[0].name).toBe("selected");
});

function openRequest(session: string, workspace = `${TEAM_KEY}|w1`): TerminalOpenTabRequest {
  return {
    machineId: "local",
    workspace,
    session,
    descriptor: { displayName: "w1", kind: "generic" },
    placement: "tab",
    activate: true,
    spawn: { kind: "warm", targetCwd: "/w1", size: { cols: 80, rows: 24 } },
  };
}

describe("terminalLayoutHandlers — the mock's terminal host", () => {
  it("starts empty: no workspaces, and load_app_state is left to the default world", () => {
    const { h, snapshot } = world();
    expect(snapshot().workspaces).toEqual({});
    expect(h.load_app_state).toBeUndefined();
  });

  it("each verb changes the host's layout and announces it on terminal-layout-changed", () => {
    const { h, changes, snapshot } = world();
    h.terminal_open_tab({ request: openRequest("a") });
    h.terminal_open_tab({ request: openRequest("b") });
    h.terminal_rename_tab({ session: "a", name: "build" });
    h.terminal_move_tab({ session: "b", toIndex: 0 });
    h.terminal_set_active({ workspace: `${TEAM_KEY}|w1`, session: "a" });

    const tabs = snapshot().workspaces[`${TEAM_KEY}|w1`].tabs;
    expect(tabs.map((t) => [t.session, t.name])).toEqual([
      ["b", "Terminal 2"],
      ["a", "build"],
    ]);
    expect(changes).toHaveLength(5);
    expect(changes.every((c) => c.workspace === `${TEAM_KEY}|w1`)).toBe(true);
    expect(changes.map((c) => c.rev)).toEqual([1, 2, 3, 4, 5]);
    expect(changes.at(-1)?.layout?.tabs.map((t) => t.session)).toEqual(["b", "a"]);

    h.terminal_close_tab({ session: "a", mode: "kill" });
    h.terminal_close_workspace({ workspace: `${TEAM_KEY}|w1` });
    expect(snapshot().workspaces).toEqual({});
    expect(changes.at(-1)).toMatchObject({ workspace: `${TEAM_KEY}|w1`, layout: null });
  });

  it("serves a terminal only for a session the host holds", async () => {
    const { h } = world();
    expect(() => h.subscribe_terminal({ ptyId: "ghost" })).toThrow(SESSION_NOT_FOUND);
    // The pane reads the refusal the way it reads the real host's: an ended session.
    const ghost = await attachTerminal("ghost", {
      open: async (id) => {
        const subscription = h.subscribe_terminal({ ptyId: id }) as TerminalSubscription;
        return { streamId: subscription.stream_id, subscription, begin: () => {}, close: () => {} };
      },
      isAborted: () => false,
    });
    expect(ghost).toEqual({ attached: false, ended: true });
    h.terminal_open_tab({ request: openRequest("a") });
    expect(h.subscribe_terminal({ ptyId: "a" })).toMatchObject({ stream_id: 1 });
  });

  it("binds a reserved stream once, and refuses one it never reserved", () => {
    const { h } = world();
    h.terminal_open_tab({ request: openRequest("a") });
    const stream = h.reserve_terminal_stream({}) as number;
    expect(h.subscribe_terminal({ ptyId: "a", stream })).toMatchObject({ stream_id: stream });
    expect(() => h.subscribe_terminal({ ptyId: "a", stream })).toThrow("not reserved");
    const released = h.reserve_terminal_stream({}) as number;
    h.unsubscribe_terminal({ streamId: released });
    expect(() => h.subscribe_terminal({ ptyId: "a", stream: released })).toThrow("not reserved");
  });
});

describe("?terminals=demo — a world the host kept running", () => {
  it("seeds the content workspace's live shell and its exited `npm test`", () => {
    const { snapshot } = world("?terminals=demo");
    const { workspaces } = snapshot();
    expect(Object.keys(workspaces)).toEqual([CONTENT_KEY]);

    const [shell, tests] = workspaces[CONTENT_KEY].tabs;
    expect(shell).toMatchObject({ session: "demo-shell", state: { kind: "live" } });
    expect(tests).toMatchObject({
      session: "demo-tests",
      name: "npm test",
      state: { kind: "exited", exitCode: 1 },
    });
  });

  it("boots the viewer onto the content workspace whose terminals the host runs", () => {
    const { h } = world("?terminals=demo");
    const state = JSON.parse(h.load_app_state({}) as string) as {
      viewer: { workspaces: Record<string, { cwd: string }[]>; activeWorkspace: object };
    };
    expect(state.viewer.workspaces[TEAM_KEY].map((w) => w.cwd)).toContain(TEAM_CONTENT_PATH);
    expect(state.viewer.activeWorkspace).toEqual({
      [TEAM_KEY]: `generic:${TEAM_CONTENT_PATH}`,
    });
  });

  it("each seeded session shows what it would have printed", () => {
    const { h } = world("?terminals=demo");
    const tests = h.subscribe_terminal({ ptyId: "demo-tests" }) as TerminalSubscription;
    expect(tests.snapshot.vt).toContain("npm test");
    expect(tests.snapshot.vt).toContain("1 failed");
    expect(tests.snapshot).toMatchObject({ cols: 80, rows: 24, resize_gen: 0 });
    expect(tests.snapshot.seq).toBe(tests.snapshot.vt.length);
  });

  it("a size change rides the stream as the next resize; a later subscribe holds it", () => {
    const { h, events, outputs } = world("?terminals=demo");
    const stream = (h.subscribe_terminal({ ptyId: "demo-shell" }) as TerminalSubscription)
      .stream_id;
    events.handlers()["plugin:event|listen"]({ event: `pty-output-stream-${stream}`, handler: 1 });
    outputs.length = 0;
    h.resize_pty({ sessionId: "demo-shell", cols: 120, rows: 40 });
    h.resize_pty({ sessionId: "demo-shell", cols: 120, rows: 40 });
    expect(outputs).toEqual([
      { resized: { at_seq: expect.any(Number), resize_gen: 1, cols: 120, rows: 40 } },
    ]);
    const again = h.subscribe_terminal({ ptyId: "demo-shell" }) as TerminalSubscription;
    expect(again.snapshot).toMatchObject({ cols: 120, rows: 40, resize_gen: 1 });
  });

  it("reads a session's terminal as text", () => {
    const { h } = world("?terminals=demo");
    const text = h.read_terminal_text({
      ptyId: "demo-tests",
      lines: 3,
      mode: "normal",
    }) as TerminalText;
    expect(text.text.split("\n")).toHaveLength(3);
    expect(text.text).toContain("1 failed");
    expect(text.text).not.toContain("\x1b");
    expect(() => h.read_terminal_text({ ptyId: "ghost", lines: 3, mode: "normal" })).toThrow(
      SESSION_NOT_FOUND,
    );
  });
});

describe("send_prompt_to_agent (mock)", () => {
  it("submits for a named agent, resolves a missing session by handle, and refuses missing handle or text", () => {
    const { h } = world();
    const send = h.send_prompt_to_agent!;
    expect(send({ handle: "barb#1", sessionId: "sess-1", text: "go" })).toMatchObject({
      status: "submitted",
      submitted: true,
    });
    expect(() => send({ handle: " ", sessionId: "sess-1", text: "go" })).toThrow(
      "handle is required",
    );
    // Like Rust, a null sessionId resolves the agent's running session by handle.
    expect(send({ handle: "barb#1", sessionId: null, text: "go" })).toMatchObject({
      status: "submitted",
      submitted: true,
    });
    expect(() => send({ handle: "barb#1", sessionId: "sess-1", text: "  " })).toThrow(
      "nothing to send",
    );
  });
});
