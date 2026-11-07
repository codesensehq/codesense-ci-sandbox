// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { shimSource } from "../../../../tests/support/page-shim";
import type { IncomingMessage, ServerResponse } from "node:http";
import { Readable } from "node:stream";
import vm from "node:vm";
import type { ViteDevServer } from "vite";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { runBridgeCommand } from "./html-bridge";
import { LiveHub } from "../lib/live-hub";
import { compareRecords, recordsHandlers } from "@/app/mock/handlers/records";
import { createInvokeRouter } from "@/app/mock/mock-router";
import { MockEventBus } from "@/app/mock/mock-events";
import { paneHandlers } from "@/app/mock/handlers/panes";
import { mockPanes } from "../../../../scripts/vite-plugin-mock-panes";
import { SLOT_GRANT_CEILINGS } from "../lib/verb-registry.generated";
import {
  liveHubSubscribe,
  liveHubPage,
  liveHubWindow,
  liveHubUnsubscribe,
  mintFrame,
  recordsPrepareNativeView,
} from "@/tauri/commands";
import { useViewerStore } from "@/shared/stores/viewer-store";

const shim = shimSource();
let bus: MockEventBus;
let hub: LiveHub;
let counter = 0;
let dropSubscriptionDelta = false;
let callbacks: Map<number, (event: { payload: unknown }) => void>;
beforeEach(async () => {
  bus = new MockEventBus();
  callbacks = new Map();
  dropSubscriptionDelta = false;
  bus.bind((id, event) => {
    if (
      dropSubscriptionDelta &&
      (event as { payload?: { subscriptionId?: string } }).payload?.subscriptionId
    ) {
      dropSubscriptionDelta = false;
      return;
    }
    callbacks.get(id)?.(event as { payload: unknown });
  });
  // Route the real pane handler's fetch through the Vite mock middleware.
  const plugin = mockPanes();
  let middleware!: (req: IncomingMessage, res: ServerResponse, next: () => void) => unknown;
  if (typeof plugin.configureServer !== "function") throw new Error("Expected mock server hook");
  await plugin.configureServer.call(
    {} as never,
    {
      config: { server: { port: 1420 } },
      middlewares: { use: (handler: typeof middleware) => (middleware = handler) },
    } as unknown as ViteDevServer,
  );
  vi.stubGlobal(
    "fetch",
    (url: string, init?: RequestInit) =>
      new Promise<Response>((resolve, reject) => {
        const req = Object.assign(Readable.from([init?.body ?? ""]), {
          url,
          method: init?.method ?? "GET",
        });
        const res = { setHeader() {}, end: (body: string) => resolve(new Response(body)) };
        void Promise.resolve(
          middleware(req as IncomingMessage, res as unknown as ServerResponse, () =>
            reject(new Error(`Unhandled mock route: ${url}`)),
          ),
        ).catch(reject);
      }),
  );
  const fallback = {
    ...paneHandlers(bus),
    live_hub_subscribe: () => ({ topic: "sessions", viewKey: "all", rev: 0, snapshot: [] }),
    live_hub_page: () => null,
    live_hub_unsubscribe: () => null,
  };
  invoke.mockImplementation(createInvokeRouter(recordsHandlers(bus, fallback)));
  hub = new LiveHub({
    read: liveHubSubscribe,
    page: liveHubPage,
    unsubscribe: liveHubUnsubscribe,
    window: liveHubWindow,
    readSessions: async () => [],
    listen: async (event, callback) => {
      const id = ++counter;
      callbacks.set(id, callback);
      const eventId = bus.handlers()["plugin:event|listen"]({ event, handler: id });
      return () => {
        bus.handlers()["plugin:event|unlisten"]({ event, eventId });
        callbacks.delete(id);
      };
    },
    readUi: async () => ({}),
    writeUi: async () => ({}),
  });
});
afterEach(() => {
  hub.dispose();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

const ids = (rows: Array<{ id: string }>) => rows.map((row) => row.id);

async function pane(scope: "private" | "team" = "private") {
  const frame = await mintFrame({
    documentPath: `/mock-ctx/${scope}/board.html`,
    contentRoot: `/mock-ctx/${scope}/content`,
    slot: "right-pane",
  });
  const listeners: Array<(event: unknown) => void> = [];
  const deliver = (data: Record<string, unknown>) =>
    listeners.forEach((listener) =>
      listener({
        source: parent,
        origin: "tauri://localhost",
        data: { token: frame.token, ...data },
      }),
    );
  const parent = {
    postMessage: (message: Record<string, unknown>) => {
      if (message.type !== "codesense:invoke") return;
      void runBridgeCommand(message.command, message.args, {
        frame,
        frameGrant: frame.grant,
        liveFrame: async () => live,
      }).then(
        (data) => deliver({ type: "codesense:invoke-result", id: message.id, ok: true, data }),
        (error) =>
          deliver({
            type: "codesense:invoke-result",
            id: message.id,
            ok: false,
            error: String(error.message),
          }),
      );
    },
  };
  const window = {
    parent,
    __codesenseProxy: { token: frame.token },
    addEventListener: (type: string, listener: (event: unknown) => void) => {
      if (type === "message") listeners.push(listener);
    },
  };
  const context: Record<string, any> = vm.createContext({
    window,
    document,
    queueMicrotask,
    setTimeout,
    clearTimeout,
    requestAnimationFrame,
    cancelAnimationFrame,
    URL,
    URLSearchParams,
    Map,
    Set,
  });
  vm.runInContext(shim, context);
  const live = hub.attach(frame.frameId, "all", deliver, true);
  deliver({ type: "codesense:visibility", visible: true });
  const api = (window as typeof window & { codesense: any }).codesense;
  return { api, frame, deliver, live };
}

it.each(["private", "team"] as const)(
  "an open %s mock frame has the full grant and discovers collections",
  async (scope) => {
    const { api, frame } = await pane(scope);
    expect(frame.kind).toBe(scope);
    expect(frame.grant.scope).toBe("open");
    expect(frame.effectiveGrant).toEqual(SLOT_GRANT_CEILINGS["right-pane"]);
    expect(
      (await api.records.collections()).map((collection: { name: string }) => collection.name),
    ).toEqual(["notes", "care"]);
  },
);

it("merges independent stale-base edits through the real mock path", async () => {
  const { api } = await pane();
  const base = "first\nmiddle\nlast\n";
  const note = await api.records.create({ collection: "notes", body: base });
  await api.records.write({
    collection: "notes",
    id: note.id,
    body: "their first\nmiddle\nlast\n",
  });
  expect(
    await api.records.write({
      collection: "notes",
      id: note.id,
      base,
      body: "first\nmiddle\nour last\n",
    }),
  ).toMatchObject({ body: "their first\nmiddle\nour last\n", merged: true, overlaps: [] });
});

it("undoes only the chosen change and refuses later overlapping edits through the real mock path", async () => {
  const { api } = await pane();
  const note = await api.records.create({ collection: "notes", body: "first\nmiddle\nlast\n" });
  const chosen = await api.records.write({
    collection: "notes",
    id: note.id,
    body: "chosen first\nmiddle\nlast\n",
  });
  await api.records.write({
    collection: "notes",
    id: note.id,
    body: "chosen first\nmiddle\nlater last\n",
  });
  expect(
    await api.records.restore({ collection: "notes", id: note.id, commit: chosen.rev, undo: true }),
  ).toMatchObject({ body: "first\nmiddle\nlater last\n" });
  const later = await api.records.write({
    collection: "notes",
    id: note.id,
    body: "later first\nmiddle\nlater last\n",
  });
  await expect(
    api.records.restore({ collection: "notes", id: note.id, commit: chosen.rev, undo: true }),
  ).rejects.toThrow("later changes overlap this revision");
  expect(await api.records.read({ collection: "notes", id: note.id })).toMatchObject({
    body: later.body,
    rev: later.rev,
  });
});

it.each([
  {
    fields: { pinned: true },
    later: { title: "Later title" },
    expected: { pinned: false, title: "Later title" },
  },
  {
    fields: { title: "Renamed" },
    later: { pinned: true },
    expected: { pinned: true, title: "Original" },
  },
])(
  "undoes a field change $fields while retaining later fields and body edits",
  async ({ fields, later, expected }) => {
    const { api } = await pane();
    const note = await api.records.create({
      collection: "notes",
      fields: { title: "Original", pinned: false },
      body: "Original body\n",
    });
    const chosen = await api.records.setFields({ collection: "notes", id: note.id, fields });
    await api.records.write({ collection: "notes", id: note.id, body: "Later body\n" });
    await api.records.setFields({ collection: "notes", id: note.id, fields: later });
    const restored = await api.records.restore({
      collection: "notes",
      id: note.id,
      commit: chosen.rev,
      undo: true,
    });
    expect(restored).toMatchObject({ changed: true, fields: expected, body: "Later body\n" });
    expect(await api.records.read({ collection: "notes", id: note.id })).toMatchObject({
      fields: expected,
      body: "Later body\n",
    });
  },
);

it("orders Notes lists, snapshots and windowed deltas by pinned then recent", async () => {
  const { api } = await pane();
  let at = Date.now() - 1000;
  const clock = vi.spyOn(Date, "now").mockImplementation(() => ++at);
  try {
    const create = (title: string) =>
      api.records.create({ collection: "notes", fields: { title, pinned: false }, body: title });
    const oldest = await create("Oldest");
    const middle = await create("Middle");
    const newest = await create("Newest");
    const changes = vi.fn();
    const initial = await api.subscribe("records", { collection: "notes", onChange: changes });
    expect(ids(initial.snapshot)).toEqual([newest.id, middle.id, oldest.id]);
    expect(initial.snapshot.map((row: { order: number }) => row.order)).toEqual([0, 1, 2]);
    expect(ids((await api.records.list({ collection: "notes" })).rows)).toEqual(
      ids(initial.snapshot),
    );
    const { api: other } = await pane();
    const topChanges = vi.fn();
    expect(
      ids(
        (
          await other.subscribe("records", {
            collection: "notes",
            window: [0, 1],
            onChange: topChanges,
          })
        ).snapshot,
      ),
    ).toEqual([newest.id]);
    await api.records.setFields({ collection: "notes", id: oldest.id, fields: { pinned: true } });
    await vi.waitFor(() => {
      expect(ids(changes.mock.calls.at(-1)?.[0].snapshot ?? [])).toEqual([
        oldest.id,
        newest.id,
        middle.id,
      ]);
      expect(ids(topChanges.mock.calls.at(-1)?.[0].snapshot ?? [])).toEqual([oldest.id]);
    });
    const pinDelta = changes.mock.calls.at(-1)![1];
    expect(
      pinDelta.upserts.map((row: { id: string; order: number }) => [row.id, row.order]),
    ).toEqual([
      [oldest.id, 0],
      [newest.id, 1],
      [middle.id, 2],
    ]);
    expect(pinDelta.upserts.every((row: object) => !("body" in row))).toBe(true);
    expect(topChanges.mock.calls.at(-1)![1].removes).toContain(newest.id);
    expect(ids((await api.records.list({ collection: "notes" })).rows)).toEqual([
      oldest.id,
      newest.id,
      middle.id,
    ]);
    await api.records.write({ collection: "notes", id: middle.id, body: "Recently edited" });
    await vi.waitFor(() =>
      expect(ids(changes.mock.calls.at(-1)?.[0].snapshot ?? [])).toEqual([
        oldest.id,
        middle.id,
        newest.id,
      ]),
    );
    const listed = (await api.records.list({ collection: "notes" })).rows;
    expect(ids(listed)).toEqual([oldest.id, middle.id, newest.id]);
    expect(listed.map((row: { order: number }) => row.order)).toEqual([0, 1, 2]);
    expect(
      changes.mock.calls.at(-1)![0].snapshot.map((row: { order: number }) => row.order),
    ).toEqual([0, 1, 2]);
  } finally {
    clock.mockRestore();
  }
});

it("orders Notes without pinned like unpinned ones by recency", async () => {
  const { api } = await pane();
  let at = Date.now() - 1000;
  const clock = vi.spyOn(Date, "now").mockImplementation(() => ++at);
  try {
    const create = (fields: Record<string, unknown>) =>
      api.records.create({ collection: "notes", fields, body: "" });
    const pinned = await create({ title: "Pinned", pinned: true });
    const unpinned = await create({ title: "Unpinned", pinned: false });
    const fromPage = await create({ title: "From a page" });
    const rows = (await api.records.list({ collection: "notes" })).rows;
    expect(rows.map((row: { id: string }) => row.id)).toEqual([
      pinned.id,
      fromPage.id,
      unpinned.id,
    ]);
  } finally {
    clock.mockRestore();
  }
});

it("clears pinned with a null patch and lists the note below still-pinned Notes", async () => {
  const { api } = await pane();
  const pinned = await api.records.create({
    collection: "notes",
    fields: { title: "Still pinned", pinned: true },
    body: "",
  });
  const toClear = await api.records.create({
    collection: "notes",
    fields: { title: "Clear pin", pinned: true },
    body: "",
  });
  const cleared = await api.records.setFields({
    collection: "notes",
    id: toClear.id,
    fields: { pinned: null },
  });
  const rows = (await api.records.list({ collection: "notes" })).rows;
  expect(rows.map((row: { id: string }) => row.id)).toEqual([pinned.id, toClear.id]);
  expect(cleared.fields).not.toHaveProperty("pinned");
  expect(rows[1].fields).not.toHaveProperty("pinned");
});

it("a private pane lists, creates, merges with overlaps, restores, pins and deletes Notes through the real shim and bridge", async () => {
  const { api } = await pane();
  const created = await api.records.create({
    collection: "notes",
    fields: { title: "Plan" },
    body: "base",
  });
  expect(created.id).toBeTruthy();
  expect((await api.records.list({ collection: "notes" })).rows).toHaveLength(1);
  await api.records.write({ collection: "notes", id: created.id, body: "other edit" });
  const merged = await api.records.write({
    collection: "notes",
    id: created.id,
    body: "my edit",
    base: "base",
    requestId: "merge",
  });
  expect(merged.merged).toBe(true);
  expect(merged.overlaps).toHaveLength(1);
  expect(merged.body).toContain("other edit");
  expect(merged.body).toContain("my edit");
  const revisions = await api.records.history({ collection: "notes", id: created.id });
  const original = revisions.at(-1);
  expect(
    (await api.records.readAt({ collection: "notes", id: created.id, commit: original.commit }))
      .body,
  ).toBe("base");
  expect(
    (await api.records.restore({ collection: "notes", id: created.id, commit: original.commit }))
      .body,
  ).toBe("base");
  let at = Date.now() + 1000;
  const clock = vi.spyOn(Date, "now").mockImplementation(() => ++at);
  const listed = async () => ids((await api.records.list({ collection: "notes" })).rows);
  try {
    const newer = await api.records.create({
      collection: "notes",
      fields: { pinned: false },
      body: "newer",
    });
    expect(await listed()).toEqual([newer.id, created.id]);
    await api.records.setFields({ collection: "notes", id: created.id, fields: { pinned: true } });
    await api.records.write({ collection: "notes", id: newer.id, body: "newer still" });
    expect(await listed()).toEqual([created.id, newer.id]);
    await api.records.setFields({ collection: "notes", id: created.id, fields: { pinned: false } });
    await api.records.write({ collection: "notes", id: newer.id, body: "newest" });
    expect(await listed()).toEqual([newer.id, created.id]);
    await api.records.delete({ collection: "notes", id: created.id });
    expect(await listed()).toEqual([newer.id]);
  } finally {
    clock.mockRestore();
  }
});

it("a team pane receives one writer delta for Care and no duplicate for a request retry", async () => {
  const { api } = await pane("team");
  const changed = vi.fn();
  const initial = await api.subscribe("records", { collection: "care", onChange: changed });
  expect(initial.viewKey).toBe("records:team:acme/widgets/default:care");
  const args = {
    collection: "care",
    id: "filter",
    fields: { title: "Filter" },
    body: "Instructions",
    requestId: "one",
  };
  await api.records.create(args);
  await vi.waitFor(() => expect(changed.mock.calls.at(-1)?.[0].snapshot).toHaveLength(1));
  const delta = changed.mock.calls.find(([, delta]) => delta)?.[1];
  expect(delta).toMatchObject({ kind: "create", actor: { kind: "human" }, requestId: "one" });
  expect(delta.upserts[0]).not.toHaveProperty("body");
  const count = changed.mock.calls.length;
  await api.records.create(args);
  expect(changed).toHaveBeenCalledTimes(count);
  await api.records.setFields({
    collection: "care",
    id: "filter",
    fields: { lastDone: "2026-10-21" },
  });
  await vi.waitFor(() =>
    expect(changed.mock.calls.at(-1)?.[0].snapshot[0].fields.lastDone).toBe("2026-10-21"),
  );
});

it("a revision gap resyncs the bound records view and a hidden frame emits one stale marker", async () => {
  const { api, live } = await pane("team");
  await api.subscribe("records", { collection: "care", onChange: () => {} });
  const before = invoke.mock.calls.filter(([command]) => command === "live_hub_subscribe").length;
  dropSubscriptionDelta = true;
  await api.records.create({ collection: "care", id: "lost", body: "First change" });
  await api.records.create({ collection: "care", id: "received", body: "Second change" });
  await vi.waitFor(() =>
    expect(
      invoke.mock.calls.filter(([command]) => command === "live_hub_subscribe").length,
    ).toBeGreaterThan(before),
  );
  const hidden = hub.attach("hidden", "all", vi.fn(), true);
  // The visible-to-hidden transition is the same host path used by panes.
  const posts: unknown[] = [];
  const other = hub.attach("hidden-records", "all", (message) => posts.push(message), true);
  await other.subscribe("records", undefined, {
    viewKey: "records:team:acme/widgets/default:care",
  });
  other.visible(false);
  other.visible(false);
  expect(posts.filter((message: any) => message.type === "codesense:stale")).toHaveLength(1);
  live.dispose();
  other.dispose();
  hidden.dispose();
});

it("records windows stay bounded and can move to another row", async () => {
  const { api } = await pane("team");
  const changes = vi.fn();
  await api.subscribe("records", { collection: "care", window: [0, 1], onChange: changes });
  await api.records.create({ collection: "care", id: "first", body: "One" });
  await api.records.create({ collection: "care", id: "second", body: "Two" });
  await vi.waitFor(() => expect(changes.mock.calls.at(-1)?.[0].total).toBe(2));
  expect(changes.mock.calls.at(-1)?.[0].snapshot).toHaveLength(1);
  const moved = await api.subscribe("records", { collection: "care", window: [1, 2] });
  expect(ids(moved.snapshot)).toEqual(["second"]);
});

it("keeps simultaneous collection subscriptions isolated", async () => {
  const { api } = await pane();
  const notes = vi.fn(),
    care = vi.fn();
  await api.subscribe("records", { collection: "notes", onChange: notes });
  await api.subscribe("records", { collection: "care", onChange: care });
  const note = await api.records.create({ collection: "notes", body: "Private" });
  await api.records.create({ collection: "care", id: "filter", body: "Shared" });
  await vi.waitFor(() => {
    expect(ids(notes.mock.calls.at(-1)?.[0].snapshot ?? [])).toEqual([note.id]);
    expect(ids(care.mock.calls.at(-1)?.[0].snapshot ?? [])).toEqual(["filter"]);
  });
  api.unsubscribe("records", notes);
  const count = notes.mock.calls.length;
  await api.records.write({ collection: "care", id: "filter", body: "Changed" });
  await vi.waitFor(() => expect(care.mock.calls.at(-1)?.[0].snapshot[0].preview).toBe("Changed"));
  expect(notes).toHaveBeenCalledTimes(count);
});

it("updates a missing cursor and keeps loaded pages after hiding and showing", async () => {
  const { api, live, deliver } = await pane("team");
  const changes = vi.fn();
  const first = await api.subscribe("records", {
    collection: "care",
    window: [0, 1],
    onChange: changes,
  });
  expect(first.cursor).toBeUndefined();
  await api.records.create({ collection: "care", id: "one", body: "One" });
  await api.records.create({ collection: "care", id: "two", body: "Two" });
  await vi.waitFor(() => expect(changes.mock.calls.at(-1)?.[0].cursor).toBeTruthy());
  await api.page("records", changes.mock.calls.at(-1)![0].cursor, { collection: "care" });
  expect(changes.mock.calls.at(-1)?.[0].snapshot).toHaveLength(2);
  deliver({ type: "codesense:visibility", visible: false });
  live.visible(false);
  deliver({ type: "codesense:visibility", visible: true });
  live.visible(true);
  await vi.waitFor(() => expect(changes.mock.calls.at(-1)?.[0].snapshot).toHaveLength(2));
  const moved = await api.subscribe("records", { collection: "care", window: [0, 1] });
  expect(moved.cursor).toBeTruthy();
  expect(moved.snapshot).toHaveLength(1);
});

it.each([false, true])(
  "restores a deleted record (undo=%s) through the real mock path",
  async (undo) => {
    const { api } = await pane();
    const content = {
      fields: { title: "Saved plan", pinned: true, tags: ["follow-up"] },
      body: "Original body\nSecond line\n",
    };
    const original = await api.records.create({ collection: "notes", ...content });
    const [revision] = await api.records.history({ collection: "notes", id: original.id });
    const deleted = await api.records.delete({ collection: "notes", id: original.id });
    await expect(api.records.read({ collection: "notes", id: original.id })).rejects.toThrow(
      "record not found",
    );
    expect(ids((await api.records.list({ collection: "notes" })).rows)).toEqual([]);
    const restored = await api.records.restore({
      collection: "notes",
      id: original.id,
      commit: undo ? deleted.rev : revision.commit,
      undo,
    });
    const expected = { id: original.id, ...content };
    expect(restored).toMatchObject({ ...expected, changed: true, deleted: false });
    expect(await api.records.read({ collection: "notes", id: original.id })).toMatchObject({
      ...expected,
      rev: restored.rev,
    });
    expect(ids((await api.records.list({ collection: "notes" })).rows)).toEqual([original.id]);
  },
);

it.each([
  { name: "creation after a later field edit", body: "", later: "fields" },
  { name: "delete after restoring the same content", body: "Original body\n", later: null },
  { name: "delete after editing restored content", body: "Original body\n", later: "body" },
])("handles undo of $name without corrupting the record", async ({ body, later }) => {
  const { api } = await pane();
  const content = { fields: { title: "Plan" }, body };
  const original = await api.records.create({ collection: "notes", ...content });
  let commit = original.rev;
  if (later === "fields") {
    await api.records.setFields({ collection: "notes", id: original.id, fields: { pinned: true } });
  } else {
    const deleted = await api.records.delete({ collection: "notes", id: original.id });
    commit = deleted.rev;
    await api.records.restore({ collection: "notes", id: original.id, commit: original.rev });
    if (later === "body")
      await api.records.write({ collection: "notes", id: original.id, body: "Later body\n" });
  }
  const before = await api.records.read({ collection: "notes", id: original.id });
  const restore = api.records.restore({ collection: "notes", id: original.id, commit, undo: true });
  if (later) {
    await expect(restore).rejects.toThrow("later changes overlap this revision");
    expect(await api.records.read({ collection: "notes", id: original.id })).toEqual(before);
  } else {
    expect(await restore).toMatchObject({ id: original.id, ...content });
    expect(await api.records.read({ collection: "notes", id: original.id })).toMatchObject({
      id: original.id,
      ...content,
    });
  }
  expect(ids((await api.records.list({ collection: "notes" })).rows)).toEqual([original.id]);
});

it("rejects duplicate creates without changing the original, while request replay still succeeds", async () => {
  const { api } = await pane("team");
  const changes = vi.fn();
  await api.subscribe("records", { collection: "care", onChange: changes });
  const args = {
    collection: "care",
    id: "filter",
    fields: { title: "Original" },
    body: "Original body",
    requestId: "first-create",
  };
  const original = await api.records.create(args);
  const stored = await api.records.read({ collection: "care", id: args.id });
  await vi.waitFor(() => expect(changes.mock.calls.at(-1)?.[0].snapshot).toHaveLength(1));
  const notifications = changes.mock.calls.length;
  expect(await api.records.create({ ...args, body: "Retried body" })).toEqual(original);
  for (const requestId of ["another-create", undefined]) {
    await expect(
      api.records.create({
        ...args,
        body: "Replacement",
        fields: { title: "Replacement" },
        requestId,
      }),
    ).rejects.toThrow("record already exists: filter");
  }
  expect(await api.records.read({ collection: "care", id: args.id })).toEqual(stored);
  expect((await api.records.list({ collection: "care" })).rows).toHaveLength(1);
  expect(changes).toHaveBeenCalledTimes(notifications);
});

it("a Notes pane and the native Notes list show the same rows after a write", async () => {
  const { api } = await pane();
  const first = await api.records.create({
    collection: "notes",
    fields: { title: "Plan", pinned: false },
    body: "# Plan\n- [ ] Call the bank",
  });
  const changes = vi.fn();
  await api.subscribe("records", { collection: "notes", onChange: changes });
  const scope = { githubUserId: 1, orgSlug: "acme/widgets" };
  useViewerStore.setState({ notes: [], activeNoteId: null, notesLoadedScope: scope });
  const viewKey = await recordsPrepareNativeView("acme/widgets", "notes");
  expect(viewKey).toBe("records:private:acme/widgets:notes");
  const stop = hub.watch(
    "records",
    viewKey,
    (view, change) => useViewerStore.getState().applyNotesProjection(scope, view.snapshot, change),
    { window: [0, 100_000] },
  );
  try {
    await vi.waitFor(() => expect(useViewerStore.getState().notes).toHaveLength(1));
    // The write an agent's notes_write makes, through the same records store.
    await api.records.write({
      collection: "notes",
      id: first.id,
      body: "Handed off [@Jimmy](cs:agent:jimmy)\nsecond line",
    });
    const second = await api.records.create({
      collection: "notes",
      fields: { title: "Later", pinned: true },
      body: "Later",
    });
    const shown = (rows: Array<Record<string, unknown>>) =>
      rows
        .map(({ id, title, preview, refs }) => ({ id, title, preview, refs }))
        .sort((a, b) => String(a.id).localeCompare(String(b.id)));
    await vi.waitFor(() => {
      const paneRows = changes.mock.calls.at(-1)?.[0].snapshot ?? [];
      expect(paneRows).toHaveLength(2);
      expect(shown(useViewerStore.getState().notes as never)).toEqual(shown(paneRows));
    });
    expect(useViewerStore.getState().notes.find((n) => n.id === first.id)).toMatchObject({
      preview: "Handed off · second line",
      refs: [{ handle: "jimmy", name: "Jimmy" }],
    });
    expect(useViewerStore.getState().notes.find((n) => n.id === second.id)?.pinned).toBe(true);
  } finally {
    stop();
  }
});

const orderingCases = JSON.parse(
  readFileSync("tests/fixtures/records/ordering.json", "utf8"),
) as Array<{
  name: string;
  fields: Record<string, { type?: string }>;
  order: string[];
  rows: Array<{ id: string; fields: Record<string, unknown> }>;
  expected: string[];
}>;
it.each(orderingCases)("shared record ordering: $name", ({ fields, order, rows, expected }) => {
  const compare = (a: (typeof rows)[number], b: (typeof rows)[number]) =>
    compareRecords(a, b, { fields, order });
  expect([...rows].sort(compare).map((row) => row.id)).toEqual(expected);
  // Every pair must agree with the same total order, regardless of sort's comparisons.
  for (const a of rows) {
    for (const b of rows) {
      expect(Math.sign(compare(a, b))).toBe(
        Math.sign(expected.indexOf(a.id) - expected.indexOf(b.id)),
      );
    }
  }
});
