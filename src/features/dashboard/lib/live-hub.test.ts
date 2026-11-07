// SPDX-License-Identifier: MIT
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import { LiveHub, paneViewKey, type HubDelta, type HubRow, type HubSnapshot } from "./live-hub";
import runReviewFixture from "../../../../tests/fixtures/run-review.json";
import configurationFixture from "./configuration-writer-trace.json";
import prefsFixture from "./prefs-writer-trace.json";
const scopedRaceFrames = [
  ...runReviewFixture,
  ...prefsFixture.initial,
  ...prefsFixture.deltas
    .filter(({ event }) => event === "codesense:delta")
    .map(({ payload }) => payload),
  ...configurationFixture.initial,
  ...configurationFixture.deltas
    .filter(({ event }) => event === "codesense:delta")
    .map(({ payload }) => payload),
] as (HubSnapshot | HubDelta)[];

const { viewKey } = vi.hoisted(() => ({ viewKey: vi.fn() }));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  liveHubViewKey: viewKey,
}));
import type { SessionListRow } from "@/tauri/commands";

it("acknowledges folder retirement only after the backend releases its subscription", async () => {
  let release!: () => void;
  const backend = new Set<string>();
  const local = new LiveHub({
    read: async (topic, viewKey, _rev, options) => {
      backend.add(options!.subscriptionId);
      return { topic, viewKey, rev: 1, snapshot: [] };
    },
    unsubscribe: (id) =>
      new Promise<void>((done) => {
        release = () => {
          backend.delete(id);
          done();
        };
      }),
    listen: async () => () => {},
    readUi: async () => ({}),
    writeUi: async () => ({}),
  });
  const frame = local.attach("nav", "team:acme/backend", () => {}, true);
  const options = { viewKey: 'team:acme/backend|files:["team",null,null,null,"/content/one"]' };
  try {
    await frame.subscribe("files", undefined, options);
    let acknowledged = false;
    const retired = Promise.resolve(frame.unsubscribe("files", options)).then(() => {
      acknowledged = true;
    });
    await vi.waitFor(() => expect(release).toBeTypeOf("function"));
    expect(acknowledged).toBe(false);
    release();
    await retired;
    await frame.subscribe("files", undefined, options);
    expect(backend.size).toBe(1);
  } finally {
    local.dispose();
  }
});

it("retires an in-flight folder read before acknowledging a collapse", async () => {
  let finish!: () => void;
  const backend = new Set<string>();
  const unsubscribe = vi.fn(async (id: string) => {
    backend.delete(id);
  });
  const local = new LiveHub({
    read: (topic, viewKey, _rev, options) =>
      new Promise((resolve) => {
        finish = () => {
          backend.add(options!.subscriptionId);
          resolve({ topic, viewKey, rev: 1, snapshot: [] });
        };
      }),
    unsubscribe,
    listen: async () => () => {},
    readUi: async () => ({}),
    writeUi: async () => ({}),
  });
  const frame = local.attach("nav", "team:acme/backend", () => {}, true);
  const options = { viewKey: 'team:acme/backend|files:["team",null,null,null,"/content/one"]' };
  try {
    const initial = expect(frame.subscribe("files", undefined, options)).rejects.toThrow(
      "subscription retired",
    );
    await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
    const retired = frame.unsubscribe("files", options);
    await Promise.resolve();
    expect(unsubscribe).not.toHaveBeenCalled();
    finish();
    await initial;
    await retired;
    expect(backend.size).toBe(0);
  } finally {
    local.dispose();
  }
});

it("keeps a pending native subscription active when revealed before its first reply", async () => {
  let resolve!: (value: HubSnapshot) => void;
  const unsubscribe = vi.fn(async () => {});
  const nativeHub = new LiveHub({
    read: () =>
      new Promise((done) => {
        resolve = done;
      }),
    unsubscribe,
    listen: async () => () => {},
    readUi: async () => ({}),
    writeUi: async () => ({}),
  });
  const frame = nativeHub.attach("run", "run-key", () => {}, true);
  const initial = frame.subscribe("runEvidence", undefined, { viewKey: "run-key" });
  await vi.waitFor(() => expect(resolve).toBeTypeOf("function"));
  frame.visible(false);
  frame.visible(true);
  const resumed = frame.subscribe("runEvidence", undefined, { viewKey: "run-key" });
  resolve({ topic: "runEvidence", viewKey: "run-key", rev: 0, snapshot: [] });
  await Promise.all([initial, resumed]);
  await Promise.resolve();
  expect(unsubscribe).not.toHaveBeenCalled();
  frame.dispose();
  nativeHub.dispose();
});

it.each(["runEvidence", "runEndorsements", "profile", "workspaceConfig", "prefs"])(
  "catches up %s when a scoped delta is discarded while its initial reply is hidden",
  async (topic) => {
    const initialSnapshot = scopedRaceFrames.find(
      (frame) => frame.topic === topic && "snapshot" in frame,
    ) as HubSnapshot;
    const committed = scopedRaceFrames.find(
      (frame) =>
        frame.topic === topic &&
        frame.viewKey === initialSnapshot.viewKey &&
        frame.rev === initialSnapshot.rev + 1 &&
        "upserts" in frame,
    ) as HubDelta;
    const rows = new Map(initialSnapshot.snapshot.map((row) => [row.id, row]));
    committed.removes.forEach((id) => rows.delete(id));
    committed.upserts.forEach((row) => rows.set(row.id, row));
    const latest = { ...initialSnapshot, rev: committed.rev, snapshot: [...rows.values()] };
    let resolve!: (value: HubSnapshot) => void;
    const read = vi
      .fn()
      .mockImplementationOnce(
        () =>
          new Promise<HubSnapshot>((done) => {
            resolve = done;
          }),
      )
      .mockResolvedValue(latest);
    const events = new Map<string, (event: { payload: unknown }) => void>();
    const unsubscribe = vi.fn(async () => {});
    const nativeHub = new LiveHub({
      read,
      unsubscribe,
      listen: async (name, handler) => {
        events.set(name, handler);
        return () => {
          events.delete(name);
        };
      },
      readUi: async () => ({}),
      writeUi: async () => ({}),
    });
    let observed: HubSnapshot | undefined;
    const frame = nativeHub.attach(
      "run",
      initialSnapshot.viewKey,
      (message) => {
        if (message.type === "codesense:resync") {
          void frame
            .subscribe(topic, undefined, { viewKey: initialSnapshot.viewKey })
            .then((value) => {
              observed = value;
            });
        }
      },
      true,
    );
    try {
      const initial = frame.subscribe(topic, undefined, { viewKey: initialSnapshot.viewKey });
      await vi.waitFor(() => expect(resolve).toBeTypeOf("function"));
      frame.visible(false);
      events.get("codesense:subscription-delta")!({
        payload: {
          subscriptionId: read.mock.calls[0][3].subscriptionId,
          baseRev: initialSnapshot.rev,
          delta: committed,
        },
      });
      expect(read).toHaveBeenCalledOnce();
      frame.visible(true);
      const resumed = frame.subscribe(topic, undefined, { viewKey: initialSnapshot.viewKey });
      resolve(initialSnapshot);
      const [, resumedReply] = await Promise.all([initial, resumed]);
      expect(resumedReply.rev).toBe(initialSnapshot.rev);
      await vi.waitFor(() => expect(observed?.rev).toBe(committed.rev));
      expect(observed?.snapshot).toEqual(latest.snapshot);
      expect(read).toHaveBeenCalledTimes(2);
      expect(read.mock.calls[1][2]).toBe(initialSnapshot.rev);
      expect(unsubscribe).not.toHaveBeenCalled();
    } finally {
      frame.dispose();
      nativeHub.dispose();
    }
  },
);

it("retires a native run subscription after a pending initial read finishes", async () => {
  let resolve!: (value: HubSnapshot) => void;
  const unsubscribe = vi.fn(async () => {});
  const nativeHub = new LiveHub({
    read: () =>
      new Promise((done) => {
        resolve = done;
      }),
    unsubscribe,
    listen: async () => () => {},
    readUi: async () => ({}),
    writeUi: async () => ({}),
  });
  const frame = nativeHub.attach("run", "run-key", () => {}, true);
  const initial = frame.subscribe("runEvidence", undefined, { viewKey: "run-key" }).catch(() => {});
  await vi.waitFor(() => expect(resolve).toBeTypeOf("function"));
  frame.dispose();
  expect(unsubscribe).not.toHaveBeenCalled();
  resolve({ topic: "runEvidence", viewKey: "run-key", rev: 0, snapshot: [] });
  await initial;
  await vi.waitFor(() =>
    expect(unsubscribe).toHaveBeenCalledWith(expect.stringContaining("runEvidence")),
  );
  nativeHub.dispose();
});

let read: ReturnType<typeof vi.fn<() => Promise<HubSnapshot>>>;
let hub: LiveHub;
let events: Map<string, (event: { payload: unknown }) => void>;
const snapshot = (rev = 0): HubSnapshot => ({
  topic: "sessions",
  viewKey: "all",
  rev,
  snapshot: [{ id: "a", handle: "a", displayName: "A" }],
});
beforeEach(() => {
  vi.useFakeTimers();
  read = vi.fn(async () => snapshot());
  events = new Map();
  hub = new LiveHub({
    read,
    listen: vi.fn(async (event, cb) => {
      events.set(event, cb);
      return () => {
        events.delete(event);
      };
    }),
    readUi: async () => ({}),
    writeUi: async (_k, p) => p,
  });
});
afterEach(() => {
  hub.dispose();
  vi.useRealTimers();
});
it("releases machineStats when hidden or disposed and catches up on show", async () => {
  hub.dispose();
  const unsubscribe = vi.fn(async () => {});
  const readStats = vi.fn(async () => ({
    topic: "machineStats",
    viewKey: "all",
    rev: 1,
    snapshot: [{ id: "local", load1: 3.04 }],
  }));
  hub = new LiveHub({
    read: readStats,
    unsubscribe,
    listen: async () => () => {},
    readUi: async () => ({}),
    writeUi: async (_key, state) => state,
  });
  const frame = hub.attach("machines", "all", vi.fn(), true, true);
  await frame.subscribe("machineStats");
  expect(readStats.mock.calls[0]).toEqual(
    expect.arrayContaining([expect.objectContaining({ subscriptionId: expect.any(String) })]),
  );
  frame.visible(false);
  expect(unsubscribe).toHaveBeenCalledOnce();
  frame.visible(true);
  await frame.subscribe("machineStats");
  expect(readStats).toHaveBeenCalledTimes(2);
  frame.dispose();
  expect(unsubscribe).toHaveBeenCalledTimes(2);
});

it("retires a prepared definition view when its frame switches teams during a read", async () => {
  let finish!: (value: HubSnapshot) => void;
  read.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const post = vi.fn();
  const frame = hub.attach("definition-team-switch", "team:acme/a", post, true);
  const pending = frame.subscribe("agentDefinitions", undefined, {
    viewKey: "agentDefinitions:1:frame:a",
  });
  const rejected = expect(pending).rejects.toThrow(/scope changed|retired/);
  await vi.advanceTimersByTimeAsync(1);
  frame.rekey("team:acme/b");
  finish({
    topic: "agentDefinitions",
    viewKey: "agentDefinitions:1:frame:a",
    rev: 1,
    snapshot: [{ id: "private-old-scope" }],
  });
  await rejected;
  expect(
    post.mock.calls.some(([message]) =>
      message.snapshot?.some((r: { id: string }) => r.id === "private-old-scope"),
    ),
  ).toBe(false);
});

it("marks hidden definition consumers stale once and catches up from the real revision", async () => {
  const viewKey = "agentDefinitions:1:frame:a";
  const initial = {
    topic: "agentDefinitions",
    viewKey,
    rev: 1,
    snapshot: [{ id: "definition:a", name: "Before" }],
  };
  read.mockResolvedValue(initial);
  const post = vi.fn();
  const frame = hub.attach("hidden-definitions", "team:acme/a", post, true);
  await frame.subscribe("agentDefinitions", undefined, { viewKey });
  frame.visible(false);
  for (const rev of [2, 3])
    events.get("codesense:subscription-delta")!({
      payload: {
        subscriptionId: "hidden-definitions:" + JSON.stringify(["agentDefinitions", viewKey]),
        baseRev: rev - 1,
        delta: {
          topic: "agentDefinitions",
          viewKey,
          rev,
          upserts: [{ id: "definition:a", name: "After" }],
          removes: [],
        },
      },
    });
  expect(read).toHaveBeenCalledTimes(1);
  expect(post.mock.calls.filter(([message]) => message.type === "codesense:stale")).toHaveLength(1);
  read.mockResolvedValue({ ...initial, rev: 3, snapshot: [{ id: "definition:a", name: "After" }] });
  frame.visible(true);
  expect((await frame.subscribe("agentDefinitions", 1, { viewKey })).snapshot[0].name).toBe(
    "After",
  );
  expect(read.mock.calls.at(-1)).toEqual(["agentDefinitions", viewKey, 1, expect.any(Object)]);
});
it("shares one snapshot across mounted frames and applies payloads without reads", async () => {
  const posts = [vi.fn(), vi.fn(), vi.fn()];
  const frames = posts.map((p, i) => hub.attach(String(i), "all", p, true));
  await Promise.all(frames.map((f) => f.subscribe("sessions")));
  expect(read).toHaveBeenCalledTimes(1);
  hub.receive({
    topic: "sessions",
    viewKey: "all",
    rev: 1,
    upserts: [{ id: "a", displayName: "B" }],
    removes: [],
  });
  await vi.advanceTimersByTimeAsync(50);
  expect(read).toHaveBeenCalledTimes(1);
  for (const p of posts)
    expect(p).toHaveBeenCalledWith(expect.objectContaining({ type: "codesense:delta" }));
});

it("waits for the delta listener before taking a snapshot and rechecks visibility", async () => {
  hub.dispose();
  let registered!: () => void;
  hub = new LiveHub({
    read,
    listen: (event, callback) =>
      event === "codesense:delta"
        ? new Promise((resolve) => {
            registered = () => {
              events.set(event, callback);
              resolve(() => {});
            };
          })
        : Promise.resolve(() => {}),
    readUi: async () => ({}),
    writeUi: async (_key, state) => state,
  });
  const frame = hub.attach("startup", "all", vi.fn(), true);
  const pending = frame.subscribe("sessions");
  await Promise.resolve();
  expect(read).not.toHaveBeenCalled();
  frame.visible(false);
  registered();
  await vi.advanceTimersByTimeAsync(1);
  expect(read).not.toHaveBeenCalled();
  frame.visible(true);
  await pending;
  expect(events.has("codesense:delta")).toBe(true);
  expect(read).toHaveBeenCalledOnce();
});
it("resyncs a gap", async () => {
  const post = vi.fn();
  const f = hub.attach("a", "all", post, true);
  await f.subscribe("sessions");
  hub.receive({ topic: "sessions", viewKey: "all", rev: 3, upserts: [], removes: [] });
  await vi.advanceTimersByTimeAsync(50);
  expect(post).toHaveBeenCalledWith(expect.objectContaining({ type: "codesense:resync" }));
});
it("folds a visible burst into one delta without resyncing", async () => {
  const post = vi.fn();
  const f = hub.attach("visible-burst", "all", post, true);
  await f.subscribe("sessions");
  hub.receive({ topic: "sessions", viewKey: "all", rev: 1, upserts: [{ id: "b" }], removes: [] });
  hub.receive({
    topic: "sessions",
    viewKey: "all",
    rev: 2,
    upserts: [{ id: "a" }],
    removes: ["b"],
  });
  hub.receive({
    topic: "sessions",
    viewKey: "all",
    rev: 3,
    upserts: [{ id: "c" }],
    removes: ["a"],
  });
  await vi.advanceTimersByTimeAsync(1);
  expect(post.mock.calls.map(([message]) => message.type)).toEqual(["codesense:delta"]);
  expect(post.mock.calls.at(-1)?.[0]).toMatchObject({
    type: "codesense:delta",
    baseRev: 0,
    rev: 3,
    upserts: [{ id: "c" }],
    removes: ["b", "a"],
  });
});
it("delivers a request echo on its own between folded runs", async () => {
  const post = vi.fn();
  const f = hub.attach("echo-break", "all", post, true);
  await f.subscribe("sessions");
  hub.receive({
    topic: "sessions",
    viewKey: "all",
    rev: 1,
    upserts: [{ id: "a", v: 1 }],
    removes: [],
  });
  hub.receive({
    topic: "sessions",
    viewKey: "all",
    rev: 2,
    upserts: [{ id: "a", v: 2 }],
    removes: [],
  });
  hub.receive({
    topic: "sessions",
    viewKey: "all",
    rev: 3,
    upserts: [{ id: "a", v: 3 }],
    removes: [],
    requestId: "own-write",
  });
  hub.receive({
    topic: "sessions",
    viewKey: "all",
    rev: 4,
    upserts: [{ id: "a", v: 4 }],
    removes: [],
  });
  hub.receive({
    topic: "sessions",
    viewKey: "all",
    rev: 5,
    upserts: [{ id: "a", v: 5 }],
    removes: [],
  });
  await vi.advanceTimersByTimeAsync(1);
  expect(
    post.mock.calls
      .filter(([message]) => message.type === "codesense:delta")
      .map(([message]) => message),
  ).toEqual([
    expect.objectContaining({ baseRev: 0, rev: 2 }),
    expect.objectContaining({ requestId: "own-write", rev: 3 }),
    expect.objectContaining({ baseRev: 3, rev: 5 }),
  ]);
});
let burstPost = vi.fn();
const scopedBurst = async (
  deltas: Array<{
    rev: number;
    id: string;
    total?: number;
    cursor?: string | null;
    baseRev?: number;
  }>,
  topic = "agentList",
) => {
  const viewKey = "team:acme/a|status";
  read.mockResolvedValueOnce({
    topic,
    viewKey,
    rev: 0,
    total: 1,
    snapshot: [{ id: "a", order: 0 }],
  });
  const post = (burstPost = vi.fn());
  const frame = hub.attach("scoped-burst", "team:acme/a", post, true);
  await frame.subscribe(topic, undefined, { order: "status" });
  post.mockClear();
  const subscriptionId = "scoped-burst:" + JSON.stringify([topic, viewKey]);
  for (const { rev, id, total, cursor, baseRev } of deltas)
    events.get("codesense:subscription-delta")!({
      payload: {
        subscriptionId,
        baseRev: baseRev ?? rev - 1,
        delta: { topic, viewKey, rev, upserts: [{ id, order: rev }], removes: [], total, cursor },
      },
    });
  await vi.advanceTimersByTimeAsync(1);
  return post.mock.calls
    .map(([message]) => message)
    .filter((message) => message.type === "codesense:delta");
};
it("folds a visible burst for an independent scoped view", async () => {
  const deltas = await scopedBurst([
    { rev: 1, id: "a", total: 1 },
    { rev: 2, id: "b", total: 1 },
    { rev: 3, id: "c", total: 1 },
  ]);
  expect(deltas).toHaveLength(1);
  expect(deltas[0]).toMatchObject({ baseRev: 0, rev: 3, total: 1 });
});
it("splits a scoped burst at the delta whose total changes", async () => {
  const deltas = await scopedBurst([
    { rev: 1, id: "a", total: 1 },
    { rev: 2, id: "b", total: 1 },
    { rev: 3, id: "c", total: 2 },
    { rev: 4, id: "d", total: 2 },
  ]);
  expect(deltas).toEqual([
    expect.objectContaining({ baseRev: 0, rev: 2, total: 1 }),
    expect.objectContaining({ rev: 3, total: 2 }),
    expect.objectContaining({ rev: 4, total: 2 }),
  ]);
});
it("folds a scoped burst across a revision the host suppressed", async () => {
  const deltas = await scopedBurst([
    { rev: 1, id: "a", total: 1 },
    { rev: 3, id: "b", total: 1, baseRev: 1 },
    { rev: 4, id: "c", total: 1 },
  ]);
  expect(deltas).toHaveLength(1);
  expect(deltas[0]).toMatchObject({ baseRev: 0, rev: 4 });
  expect(burstPost.mock.calls.some(([m]) => m.type === "codesense:resync")).toBe(false);
});
it("keeps the envelope base on a lone delta that follows a suppressed revision", async () => {
  const deltas = await scopedBurst([{ rev: 3, id: "a", total: 1, baseRev: 0 }]);
  expect(deltas).toEqual([expect.objectContaining({ rev: 3, baseRev: 0 })]);
});
it("still resyncs a scoped burst whose base does not match the previous revision", async () => {
  const deltas = await scopedBurst([
    { rev: 1, id: "a", total: 1 },
    { rev: 4, id: "b", total: 1, baseRev: 2 },
  ]);
  expect(deltas).toEqual([]);
  expect(burstPost.mock.calls.some(([m]) => m.type === "codesense:resync")).toBe(true);
});
it("folds a scoped burst whose exhausted cursor stays null", async () => {
  const deltas = await scopedBurst(
    [
      { rev: 1, id: "a", total: 1, cursor: null },
      { rev: 2, id: "b", total: 1, cursor: null },
    ],
    "records",
  );
  expect(deltas).toHaveLength(1);
  expect(deltas[0]).toMatchObject({ baseRev: 0, rev: 2 });
});
it("keeps a scoped cursor change standalone", async () => {
  const deltas = await scopedBurst(
    [
      { rev: 1, id: "a", total: 1, cursor: "c0" },
      { rev: 2, id: "b", total: 1, cursor: "c1" },
      { rev: 3, id: "c", total: 1, cursor: "c1" },
    ],
    "records",
  );
  expect(deltas.map((d) => [d.rev, d.cursor])).toEqual([
    [1, "c0"],
    [2, "c1"],
    [3, "c1"],
  ]);
  expect(deltas.every((d) => d.baseRev === undefined)).toBe(true);
});
it("resyncs a native pane source view without losing a delta during its cached read", async () => {
  const source = (rev: number): HubSnapshot => ({
    topic: "paneSources",
    viewKey: "native",
    rev,
    snapshot: [{ id: "dashboard:team:acme:a/people", sourceRevision: rev }],
  });
  read.mockResolvedValueOnce(source(0));
  const frame = hub.attach("native-pane-source", "native", vi.fn(), true);
  await frame.subscribe("paneSources");
  let finish!: (value: HubSnapshot) => void;
  read.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const pending = hub.resync("paneSources", "native");
  await vi.advanceTimersByTimeAsync(1);
  events.get("codesense:delta")!({
    payload: {
      topic: "paneSources",
      viewKey: "native",
      rev: 2,
      upserts: [{ id: "dashboard:team:acme:a/people", sourceRevision: 2 }],
      removes: [],
    },
  });
  finish(source(1));
  expect((await pending).snapshot[0].sourceRevision).toBe(2);
  expect(read).toHaveBeenCalledTimes(2);
});
it("bounds pending work at 256", async () => {
  const post = vi.fn();
  const f = hub.attach("a", "all", post, true);
  await f.subscribe("sessions");
  for (let rev = 1; rev <= 257; rev++)
    hub.receive({
      topic: "sessions",
      viewKey: "all",
      rev,
      upserts: [{ id: "a", rev }],
      removes: [],
    });
  await vi.advanceTimersByTimeAsync(50);
  expect(post.mock.calls.filter(([p]) => p.type === "codesense:resync")).toHaveLength(1);
});
it("sends one stale marker and makes no reads for a hidden frame", async () => {
  const post = vi.fn();
  const f = hub.attach("a", "all", post, false);
  const pending = f.subscribe("sessions");
  hub.receive({ topic: "sessions", viewKey: "all", rev: 1, upserts: [], removes: [] });
  hub.receive({ topic: "sessions", viewKey: "all", rev: 2, upserts: [], removes: [] });
  await vi.advanceTimersByTimeAsync(100);
  expect(read).not.toHaveBeenCalled();
  expect(post.mock.calls.filter(([p]) => p.type === "codesense:stale")).toHaveLength(1);
  f.visible(true);
  await pending;
  expect(read).toHaveBeenCalledTimes(1);
});

it("replays all affected context scopes after hidden changes in different orgs", async () => {
  const post = vi.fn();
  const frame = hub.attach("context", "team:a/design", post, false);
  events.get("context-index-changed")!({
    payload: { orgs: ["a"], allOrgs: false, truncated: false },
  });
  events.get("context-index-changed")!({
    payload: { orgs: ["b"], allOrgs: false, truncated: false },
  });
  expect(post.mock.calls.map(([message]) => message.type)).toEqual(["codesense:stale"]);
  expect(read).not.toHaveBeenCalled();
  frame.visible(true);
  expect(post).toHaveBeenCalledWith(
    expect.objectContaining({ type: "codesense:context-changed", allOrgs: true }),
  );
  expect(
    post.mock.calls.filter(([message]) => message.type === "codesense:context-changed"),
  ).toHaveLength(1);
});
it("uses firstPaint without duplicate rows", async () => {
  hub.seed(snapshot(4));
  const f = hub.attach("a", "all", vi.fn(), true);
  const first = await f.subscribe("sessions", 4);
  expect(first.snapshot).toHaveLength(1);
  expect(read).not.toHaveBeenCalled();
  hub.receive({ topic: "sessions", viewKey: "all", rev: 4, upserts: [{ id: "a" }], removes: [] });
  expect((await hub.snapshot("sessions", "all")).rev).toBe(4);
});
it("shares UI writes with native and frames and echoes requestId", async () => {
  const post = vi.fn();
  const f = hub.attach("a", "all", post, true);
  await f.subscribe("ui:right");
  const nativeWrite = hub.writeUi("ui:right", "all", { selected: "a" }, "native-write");
  await vi.advanceTimersByTimeAsync(51);
  await nativeWrite;
  expect(post).toHaveBeenCalledWith(
    expect.objectContaining({ type: "codesense:delta", requestId: "native-write" }),
  );
  const frameWrite = f.writeUi("ui:right", { selected: "b" }, "frame-write");
  await vi.advanceTimersByTimeAsync(51);
  await frameWrite;
  expect((await hub.snapshot("ui:right", "all")).snapshot[0]).toMatchObject({ selected: "b" });
});

it("echoes both request IDs for rapid native and page writes", async () => {
  const post = vi.fn();
  const frame = hub.attach("writer", "all", post, true);
  await frame.subscribe("ui:right");
  const one = hub.writeUi("ui:right", "all", { selected: "a" }, "one");
  const two = frame.writeUi("ui:right", { selected: "b" }, "two");
  await vi.advanceTimersByTimeAsync(102);
  await Promise.all([one, two]);
  expect(
    post.mock.calls.filter(([p]) => p.type === "codesense:delta").map(([p]) => p.requestId),
  ).toEqual(["one", "two"]);
});

it("keeps a single stable UI row when persisted state and patches contain id", async () => {
  hub.dispose();
  hub = new LiveHub({
    read,
    listen: async () => () => {},
    readUi: async () => ({ id: "saved" }),
    writeUi: async (_key, state) => state,
  });
  const frame = hub.attach("ui", "all", vi.fn(), true);
  expect((await frame.subscribe("ui:right")).snapshot).toEqual([{ id: "ui:right" }]);
  for (const id of ["first", "second"]) {
    const write = frame.writeUi("ui:right", { id, selected: id });
    await vi.advanceTimersByTimeAsync(51);
    await write;
    expect((await hub.snapshot("ui:right", "all")).snapshot).toEqual([
      { id: "ui:right", selected: id },
    ]);
  }
});

it("native watchers use their topic deltas without reloading for legacy pings", async () => {
  const changed = vi.fn();
  const stop = hub.watch("sessions", "all", changed);
  await vi.advanceTimersByTimeAsync(1);
  expect(changed).toHaveBeenCalledOnce();
  events.get("agent-inbox-changed")!({ payload: null });
  expect(events.has("agents-changed")).toBe(false);
  await vi.advanceTimersByTimeAsync(1);
  expect(changed).toHaveBeenCalledOnce();
  expect(read).toHaveBeenCalledOnce();
  events.get("codesense:delta")!({
    payload: {
      topic: "sessions",
      viewKey: "all",
      rev: 1,
      upserts: [{ id: "a", displayName: "B" }],
      removes: [],
    },
  });
  await vi.advanceTimersByTimeAsync(1);
  expect(changed).toHaveBeenCalledTimes(2);
  expect(changed.mock.calls[1][0].snapshot[0].displayName).toBe("B");
  expect(read).toHaveBeenCalledOnce();
  stop();
});

it("resolves shared view keys from the authenticated frame identity", async () => {
  viewKey.mockResolvedValue("team:org/team");
  const frame = { origin: "http://frame.localhost", token: "frame-token" };
  await expect(paneViewKey(frame)).resolves.toBe("team:org/team");
  expect(viewKey).toHaveBeenCalledWith(frame.origin, frame.token);
});

it("passes each frame's window and keeps resync reads bounded", async () => {
  read.mockResolvedValue({ ...snapshot(), topic: "inbox", viewKey: "team:a" });
  const frame = hub.attach("windowed", "team:a", vi.fn(), true);
  await frame.subscribe("inbox", undefined, { window: [10, 30] });
  expect(read).toHaveBeenCalledWith(
    "inbox",
    "team:a",
    undefined,
    expect.objectContaining({ window: [10, 30] }),
  );
});

it("applies a filtered revision jump without a read and resyncs only its window", async () => {
  read.mockResolvedValue({ ...snapshot(), topic: "inbox", viewKey: "team:a", total: 5000 });
  const post = vi.fn();
  const frame = hub.attach("windowed", "team:a", post, true);
  await frame.subscribe("inbox", undefined, { window: [10, 30] });
  events.get("codesense:subscription-delta")!({
    payload: {
      subscriptionId: "windowed:inbox",
      baseRev: 0,
      delta: {
        topic: "inbox",
        viewKey: "team:a",
        rev: 4,
        upserts: [{ id: "b" }],
        removes: ["a"],
        total: 5000,
      },
    },
  });
  await vi.advanceTimersByTimeAsync(1);
  expect((await frame.subscribe("inbox")).snapshot).toEqual([{ id: "b" }]);
  expect(read).toHaveBeenCalledTimes(1);
  frame.visible(false);
  events.get("codesense:subscription-delta")!({
    payload: {
      subscriptionId: "windowed:inbox",
      baseRev: 4,
      delta: { topic: "inbox", viewKey: "team:a", rev: 5, upserts: [], removes: [] },
    },
  });
  expect(read).toHaveBeenCalledTimes(1);
  frame.visible(true);
  read.mockResolvedValue({ ...snapshot(5), topic: "inbox", viewKey: "team:a" });
  await frame.subscribe("inbox", 4);
  expect(read).toHaveBeenLastCalledWith(
    "inbox",
    "team:a",
    4,
    expect.objectContaining({ window: [10, 30] }),
  );
});

it("buffers a scoped delta until the pending snapshot arrives", async () => {
  let resolve!: (value: HubSnapshot) => void;
  read.mockImplementation(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  const frame = hub.attach("pending", "all", vi.fn(), true);
  const pending = frame.subscribe("inbox", undefined, { window: [0, 2] });
  await vi.advanceTimersByTimeAsync(0);
  events.get("codesense:subscription-delta")!({
    payload: {
      subscriptionId: "pending:inbox",
      baseRev: 0,
      delta: {
        topic: "inbox",
        viewKey: "all",
        rev: 1,
        upserts: [{ id: "new", messageId: 2 }],
        removes: ["old"],
        total: 2,
      },
    },
  });
  resolve({
    topic: "inbox",
    viewKey: "all",
    rev: 0,
    snapshot: [{ id: "old", messageId: 1 }],
    total: 2,
  });
  await pending;
  expect((await frame.subscribe("inbox")).snapshot).toEqual([{ id: "new", messageId: 2 }]);
  expect(read).toHaveBeenCalledTimes(1);
});

it("resumes a fresh window when its initial read completed while hidden", async () => {
  let resolve!: (value: HubSnapshot) => void;
  read.mockImplementationOnce(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  const frame = hub.attach("hide-read", "all", vi.fn(), true);
  const pending = frame.subscribe("inbox", undefined, { window: [0, 2] });
  await vi.advanceTimersByTimeAsync(0);
  frame.visible(false);
  events.get("codesense:subscription-delta")!({
    payload: {
      subscriptionId: "hide-read:inbox",
      baseRev: 0,
      delta: { topic: "inbox", viewKey: "all", rev: 1, upserts: [{ id: "new" }], removes: ["old"] },
    },
  });
  resolve({ topic: "inbox", viewKey: "all", rev: 0, snapshot: [{ id: "old" }] });
  await pending;
  expect(read).toHaveBeenCalledTimes(1);
  read.mockResolvedValue({ topic: "inbox", viewKey: "all", rev: 1, snapshot: [{ id: "new" }] });
  frame.visible(true);
  expect((await frame.subscribe("inbox", 0)).snapshot).toEqual([{ id: "new" }]);
  expect(read).toHaveBeenCalledTimes(2);
});

it("does not subscribe an inbox frame closed while listeners are starting", async () => {
  hub.dispose();
  const unsubscribe = vi.fn(async () => {});
  const listeners: Array<() => void> = [];
  hub = new LiveHub({
    read,
    unsubscribe,
    listen: () => new Promise((resolve) => listeners.push(() => resolve(() => {}))),
    readUi: async () => ({}),
    writeUi: async (_key, state) => state,
  });
  const frame = hub.attach("closed", "all", vi.fn(), true);
  const pending = frame.subscribe("inbox");
  const rejected = expect(pending).rejects.toThrow("disposed");
  frame.dispose();
  listeners.forEach((resolve) => resolve());
  await rejected;
  expect(read).not.toHaveBeenCalled();
});

it("releases active inbox subscriptions when the hub is disposed", async () => {
  hub.dispose();
  const unsubscribe = vi.fn(async () => {});
  hub = new LiveHub({
    read: async () => ({ ...snapshot(), topic: "inbox" }),
    unsubscribe,
    listen: async () => () => {},
    readUi: async () => ({}),
    writeUi: async (_key, state) => state,
  });
  await hub.attach("active", "all", vi.fn(), true).subscribe("inbox");
  hub.dispose();
  expect(unsubscribe).toHaveBeenCalledWith("active:inbox");
});

it("honors a window move while the initial inbox read is pending", async () => {
  hub.dispose();
  let resolve!: (value: HubSnapshot) => void;
  const move = vi.fn(async () => ({
    topic: "inbox",
    viewKey: "all",
    rev: 0,
    upserts: [{ id: "next" }],
    removes: ["first"],
    total: 100,
  }));
  hub = new LiveHub({
    read: () =>
      new Promise((done) => {
        resolve = done;
      }),
    window: move,
    listen: async () => () => {},
    readUi: async () => ({}),
    writeUi: async (_key, state) => state,
  });
  const frame = hub.attach("scroll", "all", vi.fn(), true);
  const initial = frame.subscribe("inbox", undefined, { window: [0, 50] });
  await vi.advanceTimersByTimeAsync(0);
  const moved = frame.subscribe("inbox", undefined, { window: [50, 100] });
  await vi.advanceTimersByTimeAsync(0);
  resolve({ topic: "inbox", viewKey: "all", rev: 0, snapshot: [{ id: "first" }] });
  await initial;
  expect((await moved).snapshot).toEqual([{ id: "next" }]);
  expect(move).toHaveBeenCalledWith("scroll:inbox", [50, 100]);
});

it("drops queued deltas of the old window when the window moves", async () => {
  hub.dispose();
  const post = vi.fn();
  hub = new LiveHub({
    read: async () => ({
      topic: "inbox",
      viewKey: "all",
      rev: 0,
      snapshot: [{ id: "old" }],
      total: 3,
    }),
    window: async () => ({
      topic: "inbox",
      viewKey: "all",
      rev: 1,
      upserts: [{ id: "new" }],
      removes: ["old"],
      total: 3,
    }),
    listen: async (event, callback) => {
      events.set(event, callback);
      return () => {};
    },
    readUi: async () => ({}),
    writeUi: async (_key, state) => state,
  });
  const frame = hub.attach("scroll", "all", post, true);
  await frame.subscribe("inbox", undefined, { window: [0, 1] });
  const send = (rev: number, id: string) =>
    events.get("codesense:subscription-delta")!({
      payload: {
        subscriptionId: "scroll:inbox",
        baseRev: rev - 1,
        delta: { topic: "inbox", viewKey: "all", rev, upserts: [{ id }], removes: [] },
      },
    });
  send(1, "old");
  await frame.subscribe("inbox", undefined, { window: [1, 2] });
  send(2, "new");
  await vi.advanceTimersByTimeAsync(1);
  const upserts = post.mock.calls
    .filter(([message]) => message.type === "codesense:delta")
    .flatMap(([message]) => message.upserts.map((row: HubRow) => row.id));
  expect(upserts).toEqual(["new"]);
});

it("keeps a queued write echo as an empty delta when the window moves", async () => {
  hub.dispose();
  const post = vi.fn();
  hub = new LiveHub({
    read: async () => ({
      topic: "inbox",
      viewKey: "all",
      rev: 0,
      snapshot: [{ id: "old" }],
      total: 3,
    }),
    window: async () => ({
      topic: "inbox",
      viewKey: "all",
      rev: 1,
      upserts: [{ id: "new" }],
      removes: ["old"],
      total: 3,
    }),
    listen: async (event, callback) => {
      events.set(event, callback);
      return () => {};
    },
    readUi: async () => ({}),
    writeUi: async (_key, state) => state,
  });
  const frame = hub.attach("scroll", "all", post, true);
  await frame.subscribe("inbox", undefined, { window: [0, 1] });
  const send = (rev: number, id: string, requestId?: string) =>
    events.get("codesense:subscription-delta")!({
      payload: {
        subscriptionId: "scroll:inbox",
        baseRev: rev - 1,
        delta: {
          topic: "inbox",
          viewKey: "all",
          rev,
          upserts: [{ id }],
          removes: [],
          requestId,
        },
      },
    });
  send(1, "old", "own-write");
  await frame.subscribe("inbox", undefined, { window: [1, 2] });
  send(2, "new");
  await vi.advanceTimersByTimeAsync(1);
  const deltas = post.mock.calls
    .map(([message]) => message)
    .filter((message) => message.type === "codesense:delta");
  expect(deltas).toEqual([
    expect.objectContaining({ rev: 1, requestId: "own-write", upserts: [], removes: [] }),
    expect.objectContaining({ rev: 2, upserts: [{ id: "new" }] }),
  ]);
});

it("merges an older page with rows updated live while the page was pending", async () => {
  hub.dispose();
  let resolve!: (value: HubSnapshot) => void;
  hub = new LiveHub({
    read: async () => ({
      topic: "inbox",
      viewKey: "all",
      rev: 0,
      snapshot: [{ id: "newest", messageId: 2 }],
      total: 2,
      cursor: "older",
    }),
    page: () =>
      new Promise((done) => {
        resolve = done;
      }),
    listen: async (event, callback) => {
      events.set(event, callback);
      return () => {};
    },
    readUi: async () => ({}),
    writeUi: async (_key, state) => state,
  });
  const frame = hub.attach("paging", "all", vi.fn(), true);
  await frame.subscribe("inbox");
  const page = frame.page("inbox", "older");
  events.get("codesense:subscription-delta")!({
    payload: {
      subscriptionId: "paging:inbox",
      baseRev: 0,
      delta: {
        topic: "inbox",
        viewKey: "all",
        rev: 1,
        total: 3,
        upserts: [{ id: "older", messageId: 1, unread: false }],
        removes: [],
      },
    },
  });
  resolve({
    topic: "inbox",
    viewKey: "all",
    rev: 0,
    snapshot: [{ id: "older", messageId: 1, unread: true }],
    total: 2,
  });
  await page;
  const current = await frame.subscribe("inbox");
  expect(current).toMatchObject({ rev: 1, total: 3 });
  expect(current.snapshot).toEqual([
    { id: "newest", messageId: 2 },
    { id: "older", messageId: 1, unread: false },
  ]);
});

it("refetches the requested window after a buffered history resync", async () => {
  let resolve!: (value: HubSnapshot) => void;
  read.mockImplementationOnce(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  const post = vi.fn();
  const frame = hub.attach("hydrate", "all", post, true);
  const initial = frame.subscribe("inbox", undefined, { window: [10, 30] });
  await vi.advanceTimersByTimeAsync(0);
  events.get("codesense:subscription-delta")!({
    payload: {
      subscriptionId: "hydrate:inbox",
      baseRev: null,
      delta: { topic: "inbox", viewKey: "all", rev: 0, upserts: [], removes: [], total: 5000 },
    },
  });
  resolve({ topic: "inbox", viewKey: "all", rev: 0, snapshot: [] });
  await initial;
  expect(post).toHaveBeenCalledWith({ type: "codesense:resync", topic: "inbox", viewKey: "all" });
  read.mockResolvedValueOnce({
    topic: "inbox",
    viewKey: "all",
    rev: 1,
    snapshot: [{ id: "current" }],
  });
  expect((await frame.subscribe("inbox", 0)).snapshot).toEqual([{ id: "current" }]);
  expect(read).toHaveBeenLastCalledWith(
    "inbox",
    "all",
    0,
    expect.objectContaining({ window: [10, 30] }),
  );
});

it("shares joined session reads across frames until the owner changes", async () => {
  hub.dispose();
  const readSessions = vi.fn(async () => [{ handle: "a", unread: 2 }] as SessionListRow[]);
  hub = new LiveHub({
    read,
    readSessions,
    listen: async (event, callback) => {
      events.set(event, callback);
      return () => {};
    },
    readUi: async () => ({}),
    writeUi: async (_key, state) => state,
  });
  const rows = (await hub.snapshot("sessions", "all")).snapshot;
  for (let i = 0; i < 4; i++)
    expect(await hub.joinSessions(rows)).toEqual([{ handle: "a", unread: 2 }]);
  expect(readSessions).toHaveBeenCalledOnce();
  events.get("agent-inbox-changed")!({ payload: null });
  await hub.joinSessions(rows);
  expect(readSessions).toHaveBeenCalledTimes(2);
  hub.receive({ topic: "sessions", viewKey: "all", rev: 1, upserts: [], removes: [] });
  await hub.joinSessions(rows);
  expect(readSessions).toHaveBeenCalledTimes(3);
});

it("follows agentList positions after an order-only delta", async () => {
  read.mockResolvedValue({
    topic: "agentList",
    viewKey: "all",
    rev: 0,
    snapshot: [
      { id: "lead", order: 0 },
      { id: "worker", order: 1 },
    ],
  });
  await hub.snapshot("agentList", "all");
  hub.receive({
    topic: "agentList",
    viewKey: "all",
    rev: 1,
    upserts: [{ id: "worker", order: -1 }],
    removes: [],
  });
  expect((await hub.snapshot("agentList", "all")).snapshot.map((row) => row.id)).toEqual([
    "worker",
    "lead",
  ]);
});

it("keeps the dock's view state under one key for every team, across a re-key and a hub restart", async () => {
  // Stands in for app-state.json, which the shared UI state is saved to.
  const saved = new Map<string, Record<string, unknown>>();
  const deps = {
    read: vi.fn(async (_topic: string, key: string) => ({ ...snapshot(), viewKey: key })),
    listen: vi.fn(async () => () => {}),
    readUi: async (key: string) => ({ ...saved.get(key) }),
    writeUi: async (key: string, patch: Record<string, unknown>) => {
      const next = { ...saved.get(key), ...patch };
      saved.set(key, next);
      return next;
    },
  };
  hub.dispose();
  hub = new LiveHub(deps);
  const post = vi.fn();
  // A team-safe dock frame: its data topics are team-scoped, its dock state is not.
  const frame = hub.attach("dock", "team:acme/a", post, true);
  await frame.subscribe("ui:dock");
  await frame.subscribe("sessions");
  const fold = "#manager-fold:acme:a|lead";
  const write = hub.writeUi("ui:dock", "all", { folds: { [fold]: true } });
  await vi.advanceTimersByTimeAsync(60);
  await write;
  expect(post).toHaveBeenCalledWith(
    expect.objectContaining({ type: "codesense:delta", topic: "ui:dock", viewKey: "all" }),
  );
  // The frame's own write lands in the same key the built-in list reads.
  const own = frame.writeUi("ui:dock", { arrange: "manager" });
  await vi.advanceTimersByTimeAsync(60);
  await own;
  expect((await hub.snapshot("ui:dock", "all")).snapshot[0]).toMatchObject({
    arrange: "manager",
    folds: { [fold]: true },
  });
  // A re-bound grant moves the data topics and asks the page to resync each.
  post.mockClear();
  frame.rekey("team:acme/b");
  expect(post).toHaveBeenCalledWith({
    type: "codesense:resync",
    topic: "sessions",
    viewKey: "team:acme/b",
  });
  expect(post).toHaveBeenCalledWith({ type: "codesense:resync", topic: "ui:dock", viewKey: "all" });
  // A new hub (an app restart) reads the saved state back.
  hub.dispose();
  hub = new LiveHub(deps);
  expect((await hub.snapshot("ui:dock", "all")).snapshot[0]).toMatchObject({
    arrange: "manager",
    folds: { [fold]: true },
  });
});

it("rebinds a Status subscription independently of its old team's pending read", async () => {
  let finish!: (value: HubSnapshot) => void;
  read.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  read.mockResolvedValueOnce({
    topic: "agentList",
    viewKey: "team:acme/b|status",
    rev: 0,
    snapshot: [{ id: "new-team", order: 0 }],
  });
  const post = vi.fn();
  const frame = hub.attach("switching", "team:acme/a", post, true);
  const initial = frame.subscribe("agentList", undefined, {
    order: "status",
  });
  await vi.advanceTimersByTimeAsync(0);
  frame.rekey("team:acme/b");
  const moved = frame.subscribe("agentList", undefined, {
    order: "status",
  });
  await vi.advanceTimersByTimeAsync(0);
  expect(read).toHaveBeenCalledTimes(2);
  finish({
    topic: "agentList",
    viewKey: "team:acme/a|status",
    rev: 0,
    snapshot: [{ id: "old-team", order: 0 }],
  });
  await initial;
  expect((await moved).snapshot).toEqual([{ id: "new-team", order: 0 }]);
  expect(
    (
      await frame.subscribe("agentList", undefined, {
        order: "status",
      })
    ).viewKey,
  ).toBe("team:acme/b|status");
  const subscriptionId = "switching:" + JSON.stringify(["agentList", "team:acme/b|status"]);
  events.get("codesense:subscription-delta")!({
    payload: {
      subscriptionId,
      baseRev: 0,
      delta: {
        topic: "agentList",
        viewKey: "team:acme/b|status",
        rev: 1,
        upserts: [{ id: "new-team", label: "live", order: 0 }],
        removes: [],
      },
    },
  });
  await vi.advanceTimersByTimeAsync(1);
  expect(post).toHaveBeenCalledWith(
    expect.objectContaining({ type: "codesense:delta", viewKey: "team:acme/b|status", rev: 1 }),
  );
});

it.each(["sessions", "agentList"])(
  "never delivers a pending old-team %s snapshot after rebind",
  async (topic) => {
    let finish!: (value: HubSnapshot) => void;
    read.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    read.mockResolvedValueOnce({
      topic,
      viewKey: "team:acme/b" + (topic === "agentList" ? "|status" : ""),
      rev: 0,
      snapshot: [{ id: "new-team" }],
    });
    const posted = vi.fn();
    const frame = hub.attach("delivery", "team:acme/a", posted, true);
    const options = topic === "agentList" ? { order: "status" as const } : undefined;
    const pending = frame.subscribe(topic, undefined, options);
    await vi.advanceTimersByTimeAsync(0);
    frame.rekey("team:acme/b");
    posted.mockClear();
    finish({
      topic,
      viewKey: "team:acme/a" + (topic === "agentList" ? "|status" : ""),
      rev: 0,
      snapshot: [{ id: "old-team" }],
    });
    const value = await pending;
    expect(value.snapshot).toEqual([{ id: "new-team" }]);
    expect(
      posted.mock.calls.every(([message]) => !String(message.viewKey).startsWith("team:acme/a")),
    ).toBe(true);
  },
);

it("drops stale grant-view posts while a prepared records subscription keeps flowing after rebind", async () => {
  hub.dispose();
  const send = vi.fn();
  let finishPage!: (page: HubSnapshot) => void;
  hub = new LiveHub({
    read: async (topic, viewKey) => ({ topic, viewKey, rev: 0, snapshot: [{ id: topic }] }),
    page: () =>
      new Promise((resolve) => {
        finishPage = resolve;
      }),
    listen: async (event, callback) => {
      events.set(event, callback);
      return () => {};
    },
    readUi: async () => ({}),
    writeUi: async (_, state) => state,
  });
  const frame = hub.attach("scope-wall", "team:acme/a", send, true);
  const recordKey = "records:private:acme:notes";
  await frame.subscribe("agentList", undefined, { viewKey: "team:acme/a|status" });
  await frame.subscribe("records", undefined, { viewKey: recordKey });
  const pendingPage = frame.page("records", "older", recordKey);
  send.mockClear();
  frame.rekey("team:acme/b");
  const push = (topic: string, key: string) =>
    events.get("codesense:subscription-delta")!({
      payload: {
        subscriptionId: "scope-wall:" + JSON.stringify([topic, key]),
        baseRev: 0,
        delta: {
          topic,
          viewKey: key,
          rev: 1,
          upserts: [{ id: topic, label: "updated" }],
          removes: [],
        },
      },
    });
  push("agentList", "team:acme/a|status");
  push("records", recordKey);
  await vi.advanceTimersByTimeAsync(1);
  finishPage({ topic: "records", viewKey: recordKey, rev: 0, snapshot: [{ id: "older" }] });
  expect((await pendingPage).snapshot).toEqual([{ id: "older" }]);
  expect(send.mock.calls.some(([message]) => message.viewKey === "team:acme/a|status")).toBe(false);
  expect(send).toHaveBeenCalledWith(
    expect.objectContaining({ type: "codesense:delta", topic: "records", viewKey: recordKey }),
  );
  expect(
    (await frame.subscribe("records", undefined, { viewKey: recordKey })).snapshot.find(
      (row) => row.id === "records",
    )?.label,
  ).toBe("updated");
  await expect(
    frame.subscribe("agentList", undefined, { viewKey: "team:acme/a|status" }),
  ).rejects.toThrow("scope changed");
});

it("publishes one global theme revision and resolves hidden consumers only on show", async () => {
  const visible = vi.fn(),
    hidden = vi.fn();
  const a = hub.attach("native-a", "team-a", vi.fn(), true, true, visible);
  const b = hub.attach("native-b", "team-b", vi.fn(), false, true, hidden);
  const page = hub.attach("page", "team-c", vi.fn(), true);
  await page.subscribe("theme");
  hub.publishTheme("nord", "dark");
  hub.publishTheme("nord", "dark");
  expect(visible).toHaveBeenCalledTimes(1);
  expect(hidden).not.toHaveBeenCalled();
  expect(await page.subscribe("theme")).toMatchObject({
    viewKey: "all",
    rev: 1,
    snapshot: [{ themeId: "nord", mode: "dark", rev: 1 }],
  });
  b.visible(true);
  expect(hidden).toHaveBeenCalledTimes(1);
  expect(read).not.toHaveBeenCalled();
  a.dispose();
  b.dispose();
  page.dispose();
});

it("delivers a files rename only to its granted team and discards the old view after rekey", async () => {
  read.mockImplementation((async (topic: string, viewKey: string) => ({
    topic,
    viewKey,
    rev: 0,
    snapshot: [],
  })) as any);
  const a = vi.fn(),
    b = vi.fn();
  const fa = hub.attach("files-a", "team:acme/a", a, true);
  const fb = hub.attach("files-b", "team:acme/b", b, true);
  const key = 'team:acme/a|files:["team",null,null,null,null]';
  await fa.subscribe("files", undefined, { viewKey: key, window: [0, 200] });
  a.mockClear();
  b.mockClear();
  events.get("codesense:subscription-delta")!({
    payload: {
      subscriptionId: "files-a:" + JSON.stringify(["files", key]),
      baseRev: 0,
      delta: {
        topic: "files",
        viewKey: key,
        rev: 1,
        upserts: [{ id: "/new.md", name: "new.md" }],
        removes: ["/old.md"],
      },
    },
  });
  await vi.advanceTimersByTimeAsync(1);
  expect(a).toHaveBeenCalledWith(expect.objectContaining({ topic: "files", removes: ["/old.md"] }));
  expect(b).not.toHaveBeenCalled();
  fa.rekey("team:acme/b");
  a.mockClear();
  events.get("codesense:subscription-delta")!({
    payload: {
      subscriptionId: "files-a:" + JSON.stringify(["files", key]),
      baseRev: 1,
      delta: { topic: "files", viewKey: key, rev: 2, upserts: [{ id: "/other.md" }], removes: [] },
    },
  });
  await vi.advanceTimersByTimeAsync(1);
  expect(a).not.toHaveBeenCalled();
  fa.dispose();
  fb.dispose();
});

it("serializes native refresh after combined dashboard UI writes", async () => {
  hub.dispose();
  let state: Record<string, unknown> = { selected: "old" };
  hub = new LiveHub({
    read,
    listen: async () => () => {},
    readUi: async () => ({ ...state }),
    writeUi: async (_key, patch) => {
      state = { ...state, expanded: patch.expanded };
      hub.refreshUi("ui:file-nav", "team:acme/a");
      await Promise.resolve();
      state = { ...state, ...patch };
      return state;
    },
  });
  const frame = hub.attach("files", "team:acme/a", vi.fn(), true);
  await frame.subscribe("ui:file-nav");
  const write = hub.writeUi("ui:file-nav", "team:acme/a", {
    expanded: ["folder"],
    selected: "new",
    scope: "private",
  });
  await vi.advanceTimersByTimeAsync(100);
  expect((await write).snapshot[0]).toMatchObject({
    expanded: ["folder"],
    selected: "new",
    scope: "private",
  });
  expect((await frame.subscribe("ui:file-nav")).snapshot[0]).toMatchObject({
    selected: "new",
    scope: "private",
  });
});

it("retains live files cursor changes when resubscribing after hide/show", async () => {
  const key = "team:acme/a|files:team";
  read.mockResolvedValue({
    topic: "files",
    viewKey: key,
    rev: 0,
    total: 200,
    snapshot: [{ id: "a" }],
  });
  const frame = hub.attach("cursor-files", "team:acme/a", vi.fn(), true);
  const options = { viewKey: key, window: [0, 200] as [number, number] };
  await frame.subscribe("files", undefined, options);
  events.get("codesense:subscription-delta")!({
    payload: {
      subscriptionId: "cursor-files:" + JSON.stringify(["files", key]),
      baseRev: 0,
      delta: {
        topic: "files",
        viewKey: key,
        rev: 1,
        total: 201,
        cursor: "files:next",
        upserts: [{ id: "new" }],
        removes: [],
      },
    },
  });
  // A since-revision refresh may have no changes; the host must retain its delta metadata.
  read.mockResolvedValue({ topic: "files", viewKey: key, rev: 1, total: 201, deltas: [] } as any);
  frame.visible(false);
  frame.visible(true);
  const restored = await frame.subscribe("files", undefined, options);
  expect(restored.cursor).toBe("files:next");
});

it("keeps an expanded files page window through hide/show", async () => {
  hub.dispose();
  const key = "team:acme/a|files:team";
  const rows = Array.from({ length: 250 }, (_, i) => ({ id: `file-${i}`, name: `${i}.md` }));
  const readFiles = vi.fn(async (_topic: string, _key: string, _since?: number, options?: any) => ({
    topic: "files",
    viewKey: key,
    rev: 0,
    total: 250,
    snapshot: rows.slice(0, options?.window?.[1] ?? 200),
    cursor: options?.window?.[1] === 250 ? undefined : "files:next",
  }));
  hub = new LiveHub({
    read: readFiles,
    page: async () => ({
      topic: "files",
      viewKey: key,
      rev: 0,
      total: 250,
      snapshot: rows.slice(200),
    }),
    listen: async () => () => {},
    readUi: async () => ({}),
    writeUi: async (_, patch) => patch,
  });
  const frame = hub.attach("files-page", "team:acme/a", vi.fn(), true);
  const options = { viewKey: key, window: [0, 200] as [number, number] };
  await frame.subscribe("files", undefined, options);
  await frame.page("files", "files:next", key);
  frame.visible(false);
  frame.visible(true);
  const restored = await frame.subscribe("files", undefined, { viewKey: key });
  expect(readFiles.mock.calls.at(-1)?.[3]?.window).toEqual([0, 250]);
  expect(restored.snapshot).toHaveLength(250);
  expect(restored.cursor).toBeUndefined();
});

it("delivers an opted-in hidden frame's own data and coalesced invalidations after 2 seconds", async () => {
  const post = vi.fn();
  const frame = hub.attach("inbox", "all", post, true, false, undefined, {
    badge: "live",
    owner: "pane:inbox",
  });
  await frame.subscribe("sessions");
  frame.visible(false);
  post.mockClear();
  for (let rev = 1; rev <= 50; rev++) {
    hub.receive({
      topic: "sessions",
      viewKey: "all",
      rev,
      upserts: [{ id: "a", unread: rev }],
      removes: [],
    });
    events.get("agent-inbox-changed")!({ payload: {} });
  }
  await vi.advanceTimersByTimeAsync(1999);
  expect(post).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(1);
  expect(post.mock.calls.map(([m]) => m.type)).toEqual([
    "codesense:delta",
    "codesense:inbox-changed",
  ]);
  expect(post.mock.calls[0][0]).toMatchObject({ rev: 50, upserts: [{ id: "a", unread: 50 }] });
  expect(read).toHaveBeenCalledOnce();
  frame.dispose();
  expect(vi.getTimerCount()).toBe(0);
});

it("keeps default-off frames stale without hidden data or invalidation delivery", async () => {
  const post = vi.fn();
  const frame = hub.attach("ordinary", "all", post, true);
  await frame.subscribe("sessions");
  frame.visible(false);
  post.mockClear();
  hub.receive({ topic: "sessions", viewKey: "all", rev: 1, upserts: [], removes: [] });
  events.get("agent-inbox-changed")!({ payload: {} });
  await vi.advanceTimersByTimeAsync(4000);
  expect(post).not.toHaveBeenCalled();
});

it("allows hidden opted-in subscribe and page, retaining the pane as subscription owner", async () => {
  hub.dispose();
  const page = vi.fn(async () => ({ topic: "inbox", viewKey: "all", rev: 1, snapshot: [] }));
  const scopedRead = vi.fn(
    async (
      _topic: string,
      _view: string,
      _since?: number,
      _options?: { subscriptionId: string },
    ) => ({ topic: "inbox", viewKey: "all", rev: 0, snapshot: [] }),
  );
  hub = new LiveHub({
    read: scopedRead,
    page,
    listen: async () => () => {},
    readUi: async () => ({}),
    writeUi: async () => ({}),
  });
  const frame = hub.attach("frame-id", "all", vi.fn(), false, false, undefined, {
    badge: "live",
    owner: "pane:private:acme/inbox",
  });
  let completed = false;
  const result = frame
    .subscribe("inbox")
    .then(() => frame.page("inbox", "older"))
    .then(() => {
      completed = true;
    });
  await vi.advanceTimersByTimeAsync(1);
  expect(completed).toBe(true);
  expect(scopedRead.mock.calls[0][3]!.subscriptionId).toContain("pane:private:acme/inbox");
  await result;
});

it("keeps the four most recently shown hidden frames live and evicts the oldest", async () => {
  const posts = Array.from({ length: 5 }, () => vi.fn());
  const frames = posts.map((post, i) =>
    hub.attach(`pane-${i}`, "all", post, true, false, undefined, { badge: "live" }),
  );
  for (const frame of frames) await frame.subscribe("sessions");
  frames.forEach((frame) => frame.visible(false));
  posts.forEach((post) => post.mockClear());
  hub.receive({ topic: "sessions", viewKey: "all", rev: 1, upserts: [], removes: [] });
  events.get("agent-inbox-changed")!({ payload: {} });
  await vi.advanceTimersByTimeAsync(2000);
  posts.slice(1).forEach((post) => expect(post).toHaveBeenCalled());
  expect(posts[0]).not.toHaveBeenCalled();
  frames[4].dispose();
  await vi.advanceTimersByTimeAsync(2000);
  expect(posts[0]).toHaveBeenCalled();
  posts.forEach((post) => post.mockClear());
  frames[0].visible(true);
  frames[0].visible(false);
  const newest = hub.attach("newest", "all", vi.fn(), false, false, undefined, { badge: "live" });
  posts.forEach((post) => post.mockClear());
  events.get("agent-inbox-changed")!({ payload: {} });
  await vi.advanceTimersByTimeAsync(2000);
  expect(posts[0]).toHaveBeenCalled();
  expect(posts[1]).not.toHaveBeenCalled();
  newest.dispose();
});

it("cancels a pending hidden flush on frame or hub disposal", async () => {
  const post = vi.fn();
  const frame = hub.attach("hidden", "all", post, false, false, undefined, { badge: "live" });
  await frame.subscribe("sessions");
  events.get("agent-inbox-changed")!({ payload: {} });
  expect(vi.getTimerCount()).toBeGreaterThan(0);
  post.mockClear();
  frame.dispose();
  expect(vi.getTimerCount()).toBe(0);
  await vi.advanceTimersByTimeAsync(4000);
  expect(post).not.toHaveBeenCalled();
  const other = hub.attach("other", "all", post, false, false, undefined, { badge: "live" });
  await other.subscribe("sessions");
  events.get("agent-inbox-changed")!({ payload: {} });
  hub.dispose();
  expect(vi.getTimerCount()).toBe(0);
});

it("coalesces only a hidden frame's scoped subscription and throttles the next flush too", async () => {
  read.mockResolvedValue({ topic: "inbox", viewKey: "all", rev: 0, snapshot: [] });
  const post = vi.fn();
  const other = vi.fn();
  const frame = hub.attach("mail", "all", post, true, false, undefined, { badge: "live" });
  const sibling = hub.attach("sibling", "all", other, false, false, undefined, { badge: "live" });
  await frame.subscribe("inbox");
  frame.visible(false);
  const send = (baseRev: number, upserts: HubDelta["upserts"], removes: string[] = []) =>
    events.get("codesense:subscription-delta")!({
      payload: {
        subscriptionId: "mail:inbox",
        baseRev,
        delta: { topic: "inbox", viewKey: "all", rev: baseRev + 1, upserts, removes },
      },
    });
  send(0, [{ id: "arrived", unread: true }, { id: "removed" }]);
  send(1, [{ id: "arrived", unread: false }], ["removed"]);
  await vi.advanceTimersByTimeAsync(2000);
  expect(post).toHaveBeenCalledOnce();
  expect(post.mock.calls[0][0]).toMatchObject({
    type: "codesense:delta",
    rev: 2,
    upserts: [{ id: "arrived", unread: false }],
    removes: ["removed"],
  });
  expect(other).not.toHaveBeenCalled();
  post.mockClear();
  send(2, [{ id: "next" }]);
  await vi.advanceTimersByTimeAsync(1999);
  expect(post).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(1);
  expect(post).toHaveBeenCalledOnce();
  frame.visible(true);
  post.mockClear();
  await vi.advanceTimersByTimeAsync(4000);
  expect(post).not.toHaveBeenCalled();
  frame.dispose();
  sibling.dispose();
});

it.each(["scoped overflow", "shared overflow", "shared gap"])(
  "resyncs every queued view before discarding pending data on %s",
  async (path) => {
    read.mockImplementation(async (...args: unknown[]) => ({
      topic: String(args[0]),
      viewKey: String(args[1]),
      rev: 0,
      snapshot: [],
    }));
    const scoped = path === "scoped overflow";
    const topics = scoped ? ["inbox", "records"] : ["sessions", "agentList"];
    const post = vi.fn();
    const frame = hub.attach("overflow", "all", post, false, false, undefined, { badge: "live" });
    for (const topic of topics) await frame.subscribe(topic);
    const send = (topic: string, rev: number) => {
      const delta = { topic, viewKey: "all", rev, upserts: [{ id: topic, rev }], removes: [] };
      if (scoped)
        events.get("codesense:subscription-delta")!({
          payload: { subscriptionId: `overflow:${topic}`, baseRev: rev - 1, delta },
        });
      else hub.receive(delta);
    };
    send(topics[0], 1);
    if (path === "shared gap") send(topics[1], 9);
    else for (let rev = 1; rev <= 256; rev++) send(topics[1], rev);
    expect(post).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(2000);
    expect(
      post.mock.calls.map(([message]) => [message.type, message.topic, message.viewKey]),
    ).toEqual(topics.map((topic) => ["codesense:resync", topic, "all"]));
  },
);

it.each([false, true])(
  "routes read recovery through hidden throttling only when visible=%s",
  async (visible) => {
    let finish!: (value: HubSnapshot) => void;
    read.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const post = vi.fn();
    const frame = hub.attach("recover", "all", post, visible, false, undefined, { badge: "live" });
    const pending = frame.subscribe("inbox");
    await vi.advanceTimersByTimeAsync(0);
    events.get("codesense:subscription-delta")!({
      payload: {
        subscriptionId: "recover:inbox",
        baseRev: null,
        delta: { topic: "inbox", viewKey: "all", rev: 1, upserts: [], removes: [] },
      },
    });
    finish({ topic: "inbox", viewKey: "all", rev: 0, snapshot: [] });
    await pending;
    if (!visible) {
      expect(post).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(1999);
      expect(post).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(1);
    }
    expect(post).toHaveBeenCalledExactlyOnceWith({
      type: "codesense:resync",
      topic: "inbox",
      viewKey: "all",
    });
  },
);

it("retains the pane owner in ordered subscription ids after a team rebind", async () => {
  read.mockImplementation(async (...args: unknown[]) => ({
    topic: String(args[0]),
    viewKey: String(args[1]),
    rev: 0,
    snapshot: [],
  }));
  const frame = hub.attach("ordered", "team:acme/a", vi.fn(), true, false, undefined, {
    badge: "live",
    owner: "pane:agents",
  });
  await frame.subscribe("agentList", undefined, { order: "status" });
  frame.rekey("team:acme/b");
  await frame.subscribe("agentList", undefined, { order: "status" });
  const calls = read.mock.calls as unknown[][];
  expect(calls.map((call) => (call[3] as { subscriptionId: string }).subscriptionId)).toEqual([
    'pane:agents:ordered:["agentList","team:acme/a|status"]',
    'pane:agents:ordered:["agentList","team:acme/b|status"]',
  ]);
});

it.each(["profile", "workspaceConfig", "prefs"])(
  "releases the native %s subscription while paused and resumes from its revision",
  async (topic) => {
    const unsubscribe = vi.fn(async () => {});
    const nativeRead = vi.fn(async (_topic: string, _key: string, _since?: number) => ({
      topic,
      viewKey: "owned",
      rev: 7,
      snapshot: [],
    }));
    const nativeHub = new LiveHub({
      read: nativeRead,
      unsubscribe,
      listen: async () => () => {},
      readUi: async () => ({}),
      writeUi: async () => ({}),
    });
    const frame = nativeHub.attach("configuration", "owned", () => {}, true);
    try {
      await frame.subscribe(topic, undefined, { viewKey: "owned" });
      frame.visible(false);
      await vi.waitFor(() =>
        expect(unsubscribe).toHaveBeenCalledWith(expect.stringContaining(topic), false),
      );
      expect(nativeRead).toHaveBeenCalledTimes(1);
      frame.visible(true);
      await frame.subscribe(topic, undefined, { viewKey: "owned" });
      expect(nativeRead).toHaveBeenCalledTimes(2);
      expect(nativeRead.mock.calls[1][2]).toBe(7);
    } finally {
      frame.dispose();
      nativeHub.dispose();
    }
  },
);

it("windows stories in the grant-derived team and rebinds without delivering the old team's rows", async () => {
  read.mockImplementation((async (topic: string, viewKey: string) => ({
    topic,
    viewKey,
    rev: 0,
    snapshot: [],
    total: 405,
    cursor: "stories:next",
  })) as any);
  const post = vi.fn();
  const frame = hub.attach("story-frame", "team:acme/a", post, true);
  const first = await frame.subscribe("stories");
  expect(first.viewKey).toBe("team:acme/a");
  expect(read).toHaveBeenLastCalledWith(
    "stories",
    "team:acme/a",
    undefined,
    expect.objectContaining({ window: [0, 200] }),
  );
  frame.rekey("team:acme/b");
  expect(post).toHaveBeenCalledWith(
    expect.objectContaining({ type: "codesense:resync", topic: "stories", viewKey: "team:acme/b" }),
  );
  post.mockClear();
  events.get("codesense:subscription-delta")!({
    payload: {
      subscriptionId: "story-frame:stories",
      baseRev: 0,
      delta: {
        topic: "stories",
        viewKey: "team:acme/a",
        rev: 1,
        upserts: [{ id: "old-team" }],
        removes: [],
      },
    },
  });
  expect(post).not.toHaveBeenCalled();
  frame.dispose();
});

const inboxChanges = (post: ReturnType<typeof vi.fn>) =>
  post.mock.calls.map(([m]) => m).filter((m) => m.type === "codesense:inbox-changed");

it("carries the committed inbox change's message ids only to frames that can see that team", async () => {
  const post = vi.fn();
  const other = vi.fn();
  hub.attach("inbox", "all", post, true);
  hub.attach("ops", "team:acme/ops", other, true);
  await vi.advanceTimersByTimeAsync(0);
  const team = { org: "acme", team: "backend" };
  events.get("agent-inbox-changed")!({ payload: { changes: [{ team, messageIds: [5] }] } });
  expect(inboxChanges(post)).toEqual([
    { type: "codesense:inbox-changed", changes: [{ team, messageIds: [5] }] },
  ]);
  expect(inboxChanges(other)).toEqual([{ type: "codesense:inbox-changed" }]);
  events.get("agent-inbox-changed")!({ payload: null });
  expect(inboxChanges(post).at(-1)).toEqual({ type: "codesense:inbox-changed" });
});

it("unions the committed ids per team while a frame is hidden and delivers them when it shows", async () => {
  const post = vi.fn();
  const frame = hub.attach("inbox", "all", post, true);
  await vi.advanceTimersByTimeAsync(0);
  const team = { org: "acme", team: "backend" };
  const ops = { org: "acme", team: "ops" };
  frame.visible(false);
  post.mockClear();
  const committed = events.get("agent-inbox-changed")!;
  committed({ payload: { changes: [{ team, messageIds: [5, 6] }] } });
  committed({ payload: null });
  committed({
    payload: {
      changes: [
        { team, messageIds: [7] },
        { team: ops, messageIds: [2] },
      ],
    },
  });
  expect(inboxChanges(post)).toEqual([]);
  frame.visible(true);
  expect(inboxChanges(post)).toEqual([
    {
      type: "codesense:inbox-changed",
      changes: [
        { team, messageIds: [5, 6, 7] },
        { team: ops, messageIds: [2] },
      ],
    },
  ]);
});

it("drops held ids for another team when the frame is rekeyed before it shows", async () => {
  const post = vi.fn();
  const frame = hub.attach("inbox", "team:acme/backend", post, true);
  await vi.advanceTimersByTimeAsync(0);
  const backend = { org: "acme", team: "backend" };
  const ops = { org: "acme", team: "ops" };
  frame.visible(false);
  post.mockClear();
  events.get("agent-inbox-changed")!({
    payload: {
      changes: [
        { team: backend, messageIds: [5] },
        { team: ops, messageIds: [9] },
      ],
    },
  });
  frame.rekey("team:acme/ops");
  frame.visible(true);
  const delivered = inboxChanges(post).flatMap((m) => m.changes ?? []);
  expect(delivered).toEqual([{ team: ops, messageIds: [9] }]);
});
