// SPDX-License-Identifier: MIT
// @vitest-environment node
import { viewerScript } from "../../../../tests/support/page-shim";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { it, expect, vi } from "vitest";
const source = readFileSync("src-tauri/src/http/routes/live_hub.js", "utf8");
const sdk = viewerScript("LINK_INTERCEPTOR_JS");

it("delivers resolved session faces unchanged through subscribe replies and live deltas", async () => {
  const { resolveAgentFace } = await import("../../viewer/lib/agent-list-view");
  const row = { id: "a", handle: "builder", displayName: "Builder" };
  const image = resolveAgentFace({ ...row, avatar: "builtin-chess:rook" }, { name: "Builder" });
  const mark = resolveAgentFace({ ...row, avatar: "#64748b" }, { name: "Builder" });
  expect(image.kind).toBe("image");
  expect(mark).toEqual({ kind: "mark", text: "BU", color: "#64748b" });
  const s = shim();
  s.invoke.mockResolvedValue({
    topic: "sessions",
    viewKey: "team:a",
    rev: 4,
    snapshot: [{ ...row, face: image }],
  });
  s.show(true);
  const changed = vi.fn();
  await s.codesense.subscribe("sessions", { onChange: changed });
  expect(changed.mock.calls.at(-1)?.[0].snapshot[0].face).toEqual(image);
  s.message({
    type: "codesense:delta",
    topic: "sessions",
    viewKey: "team:a",
    rev: 5,
    upserts: [{ ...row, face: mark }],
    removes: [],
  });
  expect(changed.mock.calls.at(-1)?.[0].snapshot[0].face).toEqual(mark);
});

it("does not resubscribe a folder retired while its initial read was pending", async () => {
  const s = shim();
  let reject!: (error: Error) => void;
  const reply = new Promise((_, fail) => {
    reject = fail;
  });
  s.invoke.mockImplementation((verb) => (verb === "subscribe" ? reply : Promise.resolve(null)));
  s.show(true);
  const changed = vi.fn();
  const options = { scope: "team", dir: "/content/one", onChange: changed };
  const pending = s.codesense.subscribe("files", options);
  const rejected = expect(pending).rejects.toThrow("subscription retired");
  await s.codesense.unsubscribe("files", changed, options);
  reject(new Error("subscription retired"));
  await rejected;
  expect(s.invoke.mock.calls.filter(([verb]) => verb === "subscribe")).toHaveLength(1);
  expect(changed).not.toHaveBeenCalled();
});

it("notifies saved-agent consumers from definition deltas without a session change", async () => {
  const s = shim();
  const definitions = {
    topic: "agentDefinitions",
    viewKey: "agent-definitions:1:frame:a",
    rev: 1,
    snapshot: [{ id: "definition:acme/api/a/unused", key: "acme/api/a/unused", name: "Before" }],
  };
  s.invoke.mockImplementation(async (verb, args) =>
    verb === "sessions"
      ? []
      : args.topic === "agentDefinitions"
        ? definitions
        : { topic: "sessions", viewKey: "team:a", rev: 4, snapshot: [] },
  );
  s.show(true);
  const changed = vi.fn();
  const stop = s.codesense.onAgentsChanged(changed);
  await vi.waitFor(() => expect(changed).toHaveBeenCalledTimes(2));
  changed.mockClear();
  const sessionReads = s.invoke.mock.calls.filter(([verb]) => verb === "sessions").length;
  s.message({
    type: "codesense:delta",
    topic: "agentDefinitions",
    viewKey: "agent-definitions:1:frame:a",
    rev: 2,
    upserts: [{ id: "definition:acme/api/a/unused", key: "acme/api/a/unused", name: "After" }],
    removes: [],
  });
  expect(changed).toHaveBeenCalledTimes(1);
  expect(s.invoke.mock.calls.filter(([verb]) => verb === "sessions")).toHaveLength(sessionReads);
  stop();
});

it("keeps session-only updates in the legacy agent callback through the hub", async () => {
  const s = shim();
  s.show(true);
  const changed = vi.fn();
  const stop = s.codesense.onAgentsChanged(changed);
  await vi.waitFor(() => expect(changed).toHaveBeenCalled());
  changed.mockClear();
  s.message({
    type: "codesense:delta",
    topic: "sessions",
    viewKey: "team:a",
    rev: 5,
    upserts: [{ id: "a", displayName: "A new session title" }],
    removes: [],
  });
  await vi.waitFor(() => expect(changed).toHaveBeenCalled());
  stop();
});

function shim(
  firstPaint?: unknown,
  liveBadge = false,
  resolvedBadge = liveBadge,
  respond?: (verb: string, args: Record<string, unknown>) => unknown,
) {
  let shown = false;
  const listeners: Array<(v: boolean) => void> = [];
  let message!: (e: unknown) => void;
  const parent = {};
  const agentListeners = new Set<() => void>();
  const inboxListeners = new Set<() => void>();
  const invoke = vi.fn(
    async (verb: string, args: Record<string, unknown>): Promise<unknown> =>
      respond
        ? respond(verb, args)
        : verb === "sessions"
          ? [{ id: "a", displayName: "A" }]
          : {
              topic: args.topic,
              viewKey: "team:a",
              rev: 4,
              snapshot: [{ id: "a", displayName: "A" }],
            },
  );
  const codesense = {
    invoke,
    onAgentsChanged: (callback: () => void) => {
      agentListeners.add(callback);
      return () => agentListeners.delete(callback);
    },
    onInboxChanged: (callback: () => void) => {
      inboxListeners.add(callback);
      return () => inboxListeners.delete(callback);
    },
    onVisibility: (cb: (v: boolean) => void) => {
      listeners.push(cb);
      return () => {};
    },
  } as Record<string, any>;
  const window = {
    codesense,
    parent,
    __codesenseFirstPaint: firstPaint,
    __codesenseProxy: { token: "frame-token" },
    addEventListener: (_t: string, cb: typeof message) => {
      message = cb;
    },
  };
  vm.runInNewContext(source, {
    window,
    queueMicrotask,
    document: { querySelector: () => (liveBadge ? {} : null) },
  });
  if (resolvedBadge)
    message({
      source: parent,
      origin: "tauri://localhost",
      data: {
        type: "codesense:slot-context",
        token: "frame-token",
        declaration: { badge: "live" },
      },
    });
  return {
    codesense,
    invoke,
    ping: () => {
      agentListeners.forEach((cb) => cb());
      inboxListeners.forEach((cb) => cb());
    },
    show: (v: boolean) => {
      shown = v;
      listeners.forEach((f) => f(shown));
    },
    message: (data: Record<string, unknown>, origin = "tauri://localhost") =>
      message({ source: parent, origin, data: { token: "frame-token", ...data } }),
  };
}
it("the served shim makes no hidden reads, resyncs on show and applies deltas", async () => {
  const s = shim();
  const changed = vi.fn();
  const pending = s.codesense.subscribe("sessions", { onChange: changed });
  await Promise.resolve();
  expect(s.invoke).not.toHaveBeenCalled();
  s.show(true);
  await pending;
  expect(changed).toHaveBeenCalledTimes(1);
  s.message({
    type: "codesense:delta",
    topic: "sessions",
    viewKey: "team:a",
    rev: 5,
    upserts: [{ id: "a", displayName: "B" }],
    removes: [],
  });
  expect(changed.mock.calls.at(-1)?.[0].snapshot).toEqual([{ id: "a", displayName: "B" }]);
  s.show(false);
  s.message({ type: "codesense:stale" });
  await Promise.resolve();
  expect(s.invoke).toHaveBeenCalledTimes(1);
});
it("accepts a folded baseRev and applies its final rows like sequential deltas", async () => {
  const s = shim({
    topic: "sessions",
    viewKey: "team:a",
    rev: 4,
    snapshot: [{ id: "a", value: 0 }],
  });
  let reads = 0;
  s.invoke.mockImplementation(async (_verb, args) =>
    ++reads === 1
      ? { topic: args.topic, viewKey: "team:a", rev: 4, snapshot: [{ id: "a", value: 0 }] }
      : {
          topic: args.topic,
          viewKey: "team:a",
          rev: 7,
          snapshot: [
            { id: "b", value: 3 },
            { id: "c", value: 2 },
          ],
        },
  );
  s.show(true);
  const changed = vi.fn();
  await s.codesense.subscribe("sessions", { onChange: changed });
  changed.mockClear();
  s.message({
    type: "codesense:delta",
    topic: "sessions",
    viewKey: "team:a",
    baseRev: 4,
    rev: 7,
    upserts: [
      { id: "c", value: 2 },
      { id: "b", value: 3 },
    ],
    removes: ["a", "d"],
  });
  expect(changed).toHaveBeenCalledOnce();
  expect(changed.mock.calls[0][0]).toMatchObject({
    rev: 7,
    snapshot: [
      { id: "b", value: 3 },
      { id: "c", value: 2 },
    ],
  });
  expect(s.invoke).toHaveBeenCalledTimes(1);

  const sequential = shim({
    topic: "sessions",
    viewKey: "team:a",
    rev: 4,
    snapshot: [{ id: "a", value: 0 }],
  });
  sequential.invoke.mockResolvedValue({
    topic: "sessions",
    viewKey: "team:a",
    rev: 4,
    snapshot: [{ id: "a", value: 0 }],
  });
  sequential.show(true);
  const sequentialChanged = vi.fn();
  await sequential.codesense.subscribe("sessions", { onChange: sequentialChanged });
  sequentialChanged.mockClear();
  for (const delta of [
    { rev: 5, upserts: [{ id: "b", value: 1 }], removes: [] },
    {
      rev: 6,
      upserts: [
        { id: "c", value: 2 },
        { id: "d", value: 1 },
      ],
      removes: ["b"],
    },
    { rev: 7, upserts: [{ id: "b", value: 3 }], removes: ["a", "d"] },
  ])
    sequential.message({ type: "codesense:delta", topic: "sessions", viewKey: "team:a", ...delta });
  expect(sequentialChanged.mock.calls.at(-1)?.[0].snapshot).toEqual(
    changed.mock.calls[0][0].snapshot,
  );
});
it("firstPaint subscribes from its revision and keeps one copy of each row", async () => {
  const first = {
    topic: "sessions",
    viewKey: "team:a",
    rev: 4,
    snapshot: [{ id: "a", displayName: "A" }],
  };
  const s = shim(first);
  s.show(true);
  const changed = vi.fn();
  await s.codesense.subscribe("sessions", { onChange: changed });
  expect(s.invoke).toHaveBeenCalledWith("subscribe", { topic: "sessions", sinceRev: 4 });
  expect(changed).toHaveBeenCalledTimes(1);
  expect(changed.mock.calls[0][0]).toEqual(first);
});

it("does not replace a newer delta with an older subscribe reply", async () => {
  const first = {
    topic: "sessions",
    viewKey: "team:a",
    rev: 4,
    snapshot: [{ id: "a", displayName: "Before" }],
  };
  const s = shim(first);
  let reply!: (value: typeof first) => void;
  s.invoke.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        reply = resolve;
      }),
  );
  s.show(true);
  const changed = vi.fn();
  const pending = s.codesense.subscribe("sessions", { onChange: changed });
  s.message({
    type: "codesense:delta",
    topic: "sessions",
    viewKey: "team:a",
    rev: 5,
    upserts: [{ id: "a", displayName: "After" }],
    removes: [],
  });
  reply(first);
  expect((await pending).rev).toBe(5);
  expect(changed.mock.calls.map(([snapshot]) => snapshot.rev)).toEqual([4, 5]);
  s.invoke.mockResolvedValueOnce([{ id: "a", displayName: "After" }]);
  expect(await s.codesense.sessions()).toEqual([{ id: "a", displayName: "After" }]);
});

it("resyncs a delta that arrives before the first subscribe reply", async () => {
  const s = shim();
  let reply!: (value: any) => void;
  s.invoke.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        reply = resolve;
      }),
  );
  const latest = {
    topic: "sessions",
    viewKey: "team:a",
    rev: 5,
    snapshot: [{ id: "a", displayName: "After" }],
  };
  s.invoke.mockResolvedValueOnce(latest);
  s.show(true);
  const changed = vi.fn();
  const pending = s.codesense.subscribe("sessions", { onChange: changed });
  s.message({
    type: "codesense:delta",
    topic: "sessions",
    viewKey: "team:a",
    rev: 5,
    upserts: latest.snapshot,
    removes: [],
  });
  reply({ ...latest, rev: 4 });
  expect(await pending).toEqual(latest);
  expect(s.invoke).toHaveBeenCalledTimes(2);
  expect(changed).toHaveBeenCalledTimes(1);
});

it("agent changes follow definitions and sessions while inbox changes retain their subscription", async () => {
  const s = shim();
  s.show(true);
  const agents = vi.fn(),
    inbox = vi.fn();
  const stopAgents = s.codesense.onAgentsChanged(agents);
  const stopInbox = s.codesense.onInboxChanged(inbox);
  await vi.waitFor(() => expect(agents).toHaveBeenCalledTimes(2));
  expect(inbox).toHaveBeenCalledOnce();
  agents.mockClear();
  inbox.mockClear();
  s.message({
    type: "codesense:delta",
    topic: "sessions",
    viewKey: "team:a",
    rev: 5,
    upserts: [{ id: "a", state: "needs-you" }],
    removes: [],
  });
  await vi.waitFor(() => expect(inbox).toHaveBeenCalledTimes(1));
  expect(agents).toHaveBeenCalledTimes(1);
  expect(inbox).toHaveBeenCalledTimes(1);
  s.ping();
  expect(agents).toHaveBeenCalledTimes(1);
  expect(inbox).toHaveBeenCalledTimes(2);
  stopAgents();
  stopInbox();
  s.ping();
  s.message({
    type: "codesense:delta",
    topic: "sessions",
    viewKey: "team:a",
    rev: 6,
    upserts: [],
    removes: ["a"],
  });
  expect(agents).toHaveBeenCalledTimes(1);
  expect(inbox).toHaveBeenCalledTimes(2);
  expect(s.invoke).toHaveBeenCalledWith("unsubscribe", { topic: "sessions" });
  expect(s.invoke).toHaveBeenLastCalledWith("unsubscribe", { topic: "sessions" });
});

it("the actual injected SDK getter exposes the hub firstPaint snapshot", () => {
  const first = { topic: "sessions", viewKey: "team:a", rev: 4, snapshot: [{ id: "a" }] };
  const window: Record<string, any> = {
    __codesenseFirstPaint: first,
    parent: { postMessage: () => {} },
    addEventListener: () => {},
  };
  const document = { addEventListener: () => {} };
  vm.runInNewContext(sdk, { window, document });
  expect(window.codesense.firstPaint).toEqual(first);
});

it("keeps authored dashboard data accessible when the host also injects sessions", () => {
  const sessions = { topic: "sessions", viewKey: "team:a", rev: 4, snapshot: [{ id: "a" }] };
  const authored = { sprints: [{ id: "current", state: "active" }], activeSprint: "current" };
  const window: Record<string, any> = {
    __codesenseFirstPaint: sessions,
    parent: { postMessage: () => {} },
    addEventListener: () => {},
  };
  const document = {
    addEventListener: () => {},
    getElementById: (id: string) =>
      id === "codesense-dashboard-data" ? { textContent: JSON.stringify(authored) } : null,
  };
  vm.runInNewContext(sdk, { window, document });
  expect(window.codesense.firstPaint).toEqual(authored);
  expect(window.__codesenseFirstPaint).toEqual(sessions);
});

it("gates the served sprint and taxonomy HTTP reads while the frame is hidden", async () => {
  const listeners: Array<(event: unknown) => void> = [];
  const parent = { postMessage: () => {} };
  const window: Record<string, any> = {
    parent,
    __codesenseProxy: { base: "http://localhost", readCap: "test" },
    addEventListener: (type: string, cb: (event: unknown) => void) => {
      if (type === "message") listeners.push(cb);
    },
  };
  const fetch = vi.fn(async () => ({ ok: true, json: async () => ({ rows: [] }) }));
  const document = { addEventListener: () => {} };
  vm.runInNewContext(sdk + source, { window, document, fetch, queueMicrotask });
  const pending = [window.codesense.sprints(), window.codesense.taxonomy()];
  await Promise.resolve();
  expect(fetch).not.toHaveBeenCalled();
  listeners.forEach((callback) =>
    callback({ source: parent, data: { type: "codesense:visibility", visible: true } }),
  );
  await Promise.all(pending);
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("rejects LiveHub messages with the wrong token or a changed pinned parent origin", async () => {
  const s = shim();
  const changed = vi.fn();
  s.show(true);
  await s.codesense.subscribe("sessions", { onChange: changed });
  const delta = {
    type: "codesense:delta",
    topic: "sessions",
    viewKey: "team:a",
    rev: 5,
    upserts: [{ id: "a", displayName: "B" }],
    removes: [],
  };
  s.message({ ...delta, token: "other-frame" });
  expect(changed).toHaveBeenCalledTimes(1);
  s.message(delta);
  expect(changed).toHaveBeenCalledTimes(2);
  s.message({ ...delta, rev: 6 }, "http://untrusted.localhost");
  expect(changed).toHaveBeenCalledTimes(2);
});

it("session callbacks keep owner counts while the inbox window moves", async () => {
  const s = shim();
  let unread = 2;
  s.invoke.mockImplementation(async (verb, args) =>
    verb === "sessions"
      ? [
          {
            id: "a",
            handle: "a",
            team: { org: "acme", team: "a" },
            unread,
            groupUnread: unread ? 1 : 0,
            mailNotice: unread ? "pending" : null,
          },
        ]
      : {
          topic: args.topic,
          viewKey: "team:a",
          rev: 4,
          snapshot:
            args.topic === "sessions"
              ? [{ id: "a", handle: "a" }]
              : [{ id: "older", rowType: "message" }],
        },
  );
  s.show(true);
  const changed = vi.fn();
  const stop = s.codesense.onSessions(changed);
  await vi.waitFor(() => expect(changed).toHaveBeenCalledOnce());
  expect(changed.mock.calls[0][0][0]).toMatchObject({ unread: 2, groupUnread: 1 });
  const render = vi.fn();
  await s.codesense.subscribe("inbox", { window: [50, 100], onChange: render });
  expect((await s.codesense.sessions())[0]).toMatchObject({ unread: 2, groupUnread: 1 });
  unread = 0;
  s.ping();
  await vi.waitFor(() =>
    expect(changed.mock.calls.at(-1)?.[0][0]).toMatchObject({ unread: 0, groupUnread: 0 }),
  );
  stop();
  const calls = changed.mock.calls.length;
  s.ping();
  await Promise.resolve();
  expect(changed).toHaveBeenCalledTimes(calls);
  // A session getter adds no anonymous inbox subscriber.
  s.codesense.unsubscribe("inbox", render);
  expect(s.invoke).toHaveBeenCalledWith("unsubscribe", { topic: "inbox" });
});

it("notifies the renderer when a moved window keeps the same view revision", async () => {
  const s = shim();
  s.show(true);
  s.invoke.mockImplementation(async (_verb, args) => ({
    topic: args.topic,
    viewKey: "team:a",
    rev: 4,
    snapshot: [{ id: String((args.window as number[])[0]) }],
    total: 100,
  }));
  const render = vi.fn();
  await s.codesense.subscribe("inbox", { window: [0, 50], onChange: render });
  render.mockClear();
  await s.codesense.subscribe("inbox", { window: [50, 100], onChange: render });
  expect(render).toHaveBeenCalledOnce();
  expect(render.mock.calls[0][0].snapshot).toEqual([{ id: "50" }]);
});

it("notifies the moved window after a write echo already notified at the same revision", async () => {
  const s = shim();
  s.show(true);
  let reply!: (value: unknown) => void;
  s.invoke.mockImplementationOnce(async (_verb, args) => ({
    topic: args.topic,
    viewKey: "team:a",
    rev: 4,
    snapshot: [{ id: "0" }],
    total: 100,
    cursor: "records:next",
  }));
  const render = vi.fn();
  await s.codesense.subscribe("records", { collection: "c", window: [0, 50], onChange: render });
  s.invoke.mockImplementationOnce(() => new Promise((resolve) => (reply = resolve)));
  const moved = s.codesense.subscribe("records", {
    collection: "c",
    window: [50, 100],
    onChange: render,
  });
  s.message({
    type: "codesense:delta",
    topic: "records",
    viewKey: "team:a",
    rev: 5,
    requestId: "own-write",
    cursor: "records:next",
    upserts: [],
    removes: [],
  });
  reply({
    topic: "records",
    viewKey: "team:a",
    rev: 5,
    snapshot: [{ id: "50" }],
    total: 100,
    cursor: "records:page-2",
  });
  await moved;
  expect(render.mock.calls.some(([, delta]) => delta?.requestId === "own-write")).toBe(true);
  expect(render.mock.calls.at(-1)?.[0]).toMatchObject({
    snapshot: [{ id: "50" }],
    cursor: "records:page-2",
  });
});

it("notifies once when a moved window is retried after a failed subscribe", async () => {
  const s = shim();
  s.show(true);
  const snapshot = (args: Record<string, unknown>) => ({
    topic: args.topic,
    viewKey: "team:a",
    rev: 4,
    snapshot: [{ id: String((args.window as number[])[0]) }],
    total: 100,
    cursor: `inbox:page-${(args.window as number[])[0]}`,
  });
  s.invoke.mockImplementationOnce(async (_verb, args) => snapshot(args));
  const render = vi.fn();
  await s.codesense.subscribe("inbox", { window: [0, 50], onChange: render });
  s.invoke.mockRejectedValueOnce(new Error("transient"));
  await expect(
    s.codesense.subscribe("inbox", { window: [50, 100], onChange: render }),
  ).rejects.toThrow("transient");
  s.invoke.mockImplementation(async (_verb, args) => snapshot(args));
  render.mockClear();
  const result = await s.codesense.subscribe("inbox", { window: [50, 100], onChange: render });
  expect(result.snapshot).toEqual([{ id: "50" }]);
  expect(render).toHaveBeenCalledTimes(1);
  expect(render.mock.calls[0][0]).toMatchObject({
    snapshot: [{ id: "50" }],
    cursor: "inbox:page-50",
  });
});

it("resyncs the latest window requested during the initial SDK read", async () => {
  const s = shim();
  s.show(true);
  let reply!: (value: unknown) => void;
  s.invoke.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        reply = resolve;
      }),
  );
  const render = vi.fn();
  const initial = s.codesense.subscribe("inbox", { window: [0, 50], onChange: render });
  const moved = s.codesense.subscribe("inbox", { window: [50, 100], onChange: render });
  s.invoke.mockResolvedValueOnce({
    topic: "inbox",
    viewKey: "team:a",
    rev: 4,
    snapshot: [{ id: "next" }],
    total: 100,
  });
  reply({ topic: "inbox", viewKey: "team:a", rev: 4, snapshot: [{ id: "first" }], total: 100 });
  await Promise.all([initial, moved]);
  expect(s.invoke).toHaveBeenLastCalledWith("subscribe", {
    topic: "inbox",
    sinceRev: 4,
    window: [50, 100],
  });
  expect(render.mock.calls.at(-1)?.[0].snapshot).toEqual([{ id: "next" }]);
});

it("merges a pending SDK page without duplicating or overwriting live rows", async () => {
  const s = shim();
  s.show(true);
  s.invoke.mockResolvedValueOnce({
    topic: "inbox",
    viewKey: "team:a",
    rev: 0,
    snapshot: [{ id: "newest", messageId: 2 }],
    total: 2,
    cursor: "older",
  });
  const render = vi.fn();
  await s.codesense.subscribe("inbox", { onChange: render });
  let reply!: (value: unknown) => void;
  s.invoke.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        reply = resolve;
      }),
  );
  const page = s.codesense.page("inbox", "older");
  s.message({
    type: "codesense:delta",
    topic: "inbox",
    viewKey: "team:a",
    rev: 1,
    upserts: [{ id: "older", messageId: 1, unread: false }],
    removes: [],
    total: 3,
  });
  reply({
    topic: "inbox",
    viewKey: "team:a",
    rev: 0,
    snapshot: [{ id: "older", messageId: 1, unread: true }],
    total: 2,
  });
  await page;
  expect(render.mock.calls.at(-1)?.[0]).toMatchObject({
    rev: 1,
    total: 3,
    snapshot: [
      { id: "newest", messageId: 2 },
      { id: "older", messageId: 1, unread: false },
    ],
  });
});

it("orders agentList reference deltas by the server position", async () => {
  const s = shim();
  s.show(true);
  s.invoke.mockResolvedValue({
    topic: "agentList",
    viewKey: "team:a",
    rev: 0,
    snapshot: [
      { id: "lead", order: 0 },
      { id: "worker", order: 1 },
    ],
  });
  const changed = vi.fn();
  await s.codesense.subscribe("agentList", { onChange: changed });
  s.message({
    type: "codesense:delta",
    topic: "agentList",
    viewKey: "team:a",
    rev: 1,
    upserts: [{ id: "worker", order: -1 }],
    removes: [],
  });
  expect(changed.mock.lastCall?.[0].snapshot.map((row: { id: string }) => row.id)).toEqual([
    "worker",
    "lead",
  ]);
});
it("sends final record and preference writes while hidden, while reads wait for show", async () => {
  const s = shim();
  s.show(true);
  s.show(false);
  const write = s.codesense.records.write({ collection: "notes", id: "a", body: "last key" });
  const prefs = s.codesense.invoke("setPrefs", { mirror: "last key" });
  const read = s.codesense.records.read({ collection: "notes", id: "a" });
  await Promise.resolve();
  expect(s.invoke.mock.calls.map(([verb]) => verb)).toEqual(["records.write", "setPrefs"]);
  await Promise.all([write, prefs]);
  s.show(true);
  await read;
  expect(s.invoke.mock.calls.at(-1)?.[0]).toBe("records.read");
});

it.each([undefined, 70])(
  "discards an old pending view reply and both cached/initial revisions (initial=%s)",
  async (sinceRev) => {
    const s = shim(
      sinceRev === undefined
        ? {
            topic: "agentList",
            viewKey: "team:a|status",
            rev: 8,
            snapshot: [{ id: "old" }],
          }
        : undefined,
    );
    s.show(true);
    let finish!: (value: unknown) => void;
    s.invoke.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    s.invoke.mockResolvedValue({
      topic: "agentList",
      viewKey: "team:b|status",
      rev: 2,
      snapshot: [{ id: "new" }],
    });
    const changed = vi.fn();
    const pending = s.codesense.subscribe("agentList", {
      order: "status",
      sinceRev,
      onChange: changed,
    });
    changed.mockClear();
    s.message({ type: "codesense:resync", topic: "agentList", viewKey: "team:b|status" });
    finish({ topic: "agentList", viewKey: "team:a|status", rev: 9, snapshot: [{ id: "old" }] });
    expect((await pending).viewKey).toBe("team:b|status");
    expect(s.invoke.mock.calls.slice(1).every(([, args]) => args.sinceRev === undefined)).toBe(
      true,
    );
    expect(changed.mock.calls.every(([snapshot]) => snapshot.viewKey === "team:b|status")).toBe(
      true,
    );
  },
);

it("the served shim delivers own subscription data and permits paging for a declared live badge while hidden", async () => {
  const s = shim(undefined, true);
  s.show(true);
  const changed = vi.fn();
  await s.codesense.subscribe("inbox", { onChange: changed });
  s.show(false);
  changed.mockClear();
  s.message({
    type: "codesense:delta",
    topic: "inbox",
    viewKey: "team:a",
    rev: 5,
    upserts: [{ id: "a", unread: 3 }],
    removes: [],
  });
  expect(changed).toHaveBeenCalledOnce();
  expect(changed.mock.calls[0][0].snapshot).toEqual([{ id: "a", unread: 3 }]);
  await s.codesense.page("inbox", "older");
  expect(s.invoke).toHaveBeenCalledWith(
    "page",
    expect.objectContaining({ topic: "inbox", cursor: "older" }),
  );
});

it.each([true, false])(
  "uses the resolved declaration for hidden delivery with DOM badge=%s",
  async (domBadge) => {
    const s = shim(undefined, domBadge, !domBadge);
    s.show(true);
    const changed = vi.fn();
    await s.codesense.subscribe("inbox", { onChange: changed });
    s.show(false);
    changed.mockClear();
    s.message({
      type: "codesense:delta",
      topic: "inbox",
      viewKey: "team:a",
      rev: 5,
      upserts: [{ id: "a", unread: 7 }],
      removes: [],
    });
    expect(changed).toHaveBeenCalledTimes(domBadge ? 0 : 1);
    if (!domBadge) {
      await s.codesense.page("inbox", "older");
      expect(s.invoke).toHaveBeenCalledWith("page", expect.objectContaining({ cursor: "older" }));
    }
  },
);

const storyRows = (count: number, from = 0) =>
  Array.from({ length: count }, (_, i) => ({ id: `s${String(from + i).padStart(4, "0")}` }));

it("keeps the stories page window across hide and show", async () => {
  const s = shim(undefined, false, false, (verb, args) =>
    verb === "page"
      ? { snapshot: storyRows(3, 200), cursor: undefined, rev: 4, total: 203 }
      : {
          topic: args.topic,
          viewKey: "team:a|stories",
          rev: 4,
          snapshot: storyRows(200),
          total: 203,
          cursor: "c1",
        },
  );
  s.show(true);
  await s.codesense.subscribe("stories", { onChange: vi.fn() });
  await s.codesense.page("stories", "c1");
  s.show(false);
  s.show(true);
  await vi.waitFor(() =>
    expect(s.invoke.mock.calls.filter(([v]) => v === "subscribe")).toHaveLength(2),
  );
  const resumed = s.invoke.mock.calls.filter(([v]) => v === "subscribe").at(-1)?.[1] as {
    window: number[];
  };
  expect(resumed.window).toEqual([0, 203]);
});

it("applies the stories delta cursor on both sides of the page boundary", async () => {
  const s = shim(undefined, false, false, (_verb, args) => ({
    topic: args.topic,
    viewKey: "team:a|stories",
    rev: 4,
    snapshot: storyRows(200),
    total: 200,
  }));
  s.show(true);
  const changed = vi.fn();
  await s.codesense.subscribe("stories", { onChange: changed });
  s.message({
    type: "codesense:delta",
    topic: "stories",
    viewKey: "team:a|stories",
    rev: 5,
    upserts: [{ id: "s9999" }],
    removes: [],
    total: 201,
    cursor: "stories:next",
  });
  expect(changed.mock.calls.at(-1)?.[0].cursor).toBe("stories:next");
  s.message({
    type: "codesense:delta",
    topic: "stories",
    viewKey: "team:a|stories",
    rev: 6,
    upserts: [],
    removes: ["s9999"],
    total: 200,
  });
  expect(changed.mock.calls.at(-1)?.[0].cursor).toBeUndefined();
});
