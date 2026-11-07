// SPDX-License-Identifier: MIT
import type { FrameRecord, Note, NoteMeta } from "@/tauri/commands";
import { summarize } from "@/features/notes/lib/note-summary";
import type { HandlerRegistry } from "../mock-router";
import type { MockEventBus } from "../mock-events";
import { applyMockTextChange } from "../text-change";

type RecordRow = { id: string; fields: Record<string, unknown>; body: string; rev: string };
type Collection = {
  name: string;
  scope: "private" | "team";
  id: "generated" | "slug";
  fields: Record<string, { type?: string }>;
  order: string[];
  options: { humanOnly: string[] };
};
const collections: Collection[] = [
  {
    name: "notes",
    scope: "private",
    id: "generated",
    fields: { pinned: { type: "boolean" } },
    order: ["pinned desc", "updatedAt desc"],
    options: { humanOnly: ["delete"] },
  },
  { name: "care", scope: "team", id: "slug", fields: {}, order: [], options: { humanOnly: [] } },
];

const attribution = new Set(["updatedAt", "updatedBy"]);
// Stable field lines keep independent field and body edits separate in a patch.
function document(record?: RecordRow): string {
  if (!record) return "";
  const fields = Object.entries(record.fields)
    .filter(([key]) => !attribution.has(key))
    .sort(([a], [b]) => a.localeCompare(b))
    .map((entry) => JSON.stringify(entry));
  return `${fields.join("\n")}\n---\n${record.body}`;
}
function readDocument(text: string) {
  const end = text.indexOf("\n---\n");
  const fields = text.slice(0, end);
  return {
    fields: Object.fromEntries(fields ? fields.split("\n").map((line) => JSON.parse(line)) : []),
    body: text.slice(end + 5),
  };
}

/** The same record operations and projection contract, with session-local mock storage. */
export function recordsHandlers(events: MockEventBus, fallback: HandlerRegistry): HandlerRegistry {
  const frames = new Map<string, FrameRecord>();
  const stores = new Map<string, Map<string, RecordRow>>();
  const histories = new Map<string, Array<{ record: RecordRow; content: string }>>();
  const receipts = new Map<string, { at: number; value: unknown }>();
  type ProjectionRow = ReturnType<typeof row> & { order: number };
  const views = new Map<
    string,
    {
      store: Map<string, RecordRow>;
      collection: Collection;
      rows: Map<string, ProjectionRow>;
      rev: number;
    }
  >();
  const subscriptions = new Map<
    string,
    {
      key: string;
      window: [number, number];
      rev: number;
      total: number;
      loaded: Map<string, ProjectionRow>;
    }
  >();
  let sequence = 0;
  const viewKey = (collection: Collection, owner: string) =>
    `records:${collection.scope}:${owner}:${collection.name}`;
  function resolve(frameId: unknown, name: unknown, write = false) {
    const frame = frames.get(String(frameId));
    if (!frame) throw new Error("denied: frame released");
    const collection = collections.find((c) => c.name === name);
    if (!collection) throw new Error(`unknown collection: ${String(name)}`);
    const cap = `records:${write ? "write" : "read"}:${collection.scope === "private" ? "own" : "team"}`;
    if (!frame.effectiveGrant?.includes(cap)) throw new Error("denied: grant");
    const org = frame.grant.team?.org ?? "acme/widgets";
    const owner =
      collection.scope === "private" ? org : `${org}/${frame.grant.team?.team ?? "default"}`;
    const key = viewKey(collection, owner);
    if (!stores.has(key)) stores.set(key, new Map());
    return { frame, collection, key, store: stores.get(key)! };
  }
  function row(record: RecordRow) {
    const { snippet, refs } = summarize(record.body);
    return {
      id: record.id,
      title: record.fields.title,
      fields: record.fields,
      preview: snippet,
      refs,
      updatedAt: record.fields.updatedAt,
      updatedBy: record.fields.updatedBy,
    };
  }
  function orderedRows(store: Map<string, RecordRow>, collection: Collection): ProjectionRow[] {
    return [...store.values()]
      .sort((a, b) => compareRecords(a, b, collection))
      .map((record, order) => ({ ...row(record), order }));
  }
  function snapshot(key: string, window?: [number, number]) {
    const view = views.get(key)!;
    const all = orderedRows(view.store, view.collection);
    const [start, end] = window ?? [0, 200];
    return {
      topic: "records",
      viewKey: key,
      rev: view.rev,
      snapshot: all.slice(start, end),
      total: all.length,
      cursor: end < all.length ? String(end) : undefined,
    };
  }
  function change(key: string, kind: string, requestId?: string) {
    const view = views.get(key);
    if (!view) return;
    const all = orderedRows(view.store, view.collection);
    const nextRows = new Map(all.map((row) => [row.id, row]));
    const upserts = all.filter(
      (row) => JSON.stringify(row) !== JSON.stringify(view.rows.get(row.id)),
    );
    const removes = [...view.rows.keys()].filter((id) => !nextRows.has(id));
    view.rows = nextRows;
    if (!upserts.length && !removes.length) return;
    const delta = {
      topic: "records",
      viewKey: key,
      rev: ++view.rev,
      upserts,
      removes,
      actor: { kind: "human", name: "You" },
      kind,
      requestId,
    };
    events.emit("codesense:delta", delta);
    for (const [id, subscription] of subscriptions) {
      if (subscription.key !== key) continue;
      const selected = snapshot(key, subscription.window).snapshot;
      const next = new Map(selected.map((r) => [r.id, r]));
      const upserts = selected.filter(
        (r) => JSON.stringify(r) !== JSON.stringify(subscription.loaded.get(r.id)),
      );
      const removes = [...subscription.loaded.keys()].filter((id) => !next.has(id));
      if (!upserts.length && !removes.length && subscription.total === view.store.size) continue;
      events.emit("codesense:subscription-delta", {
        subscriptionId: id,
        baseRev: subscription.rev,
        delta: {
          ...delta,
          upserts,
          removes,
          total: view.store.size,
          cursor: snapshot(key, subscription.window).cursor ?? null,
        },
      });
      subscription.loaded = next;
      subscription.total = view.store.size;
      subscription.rev = view.rev;
    }
  }
  function prepare(key: string, store: Map<string, RecordRow>, collection: Collection) {
    if (!views.has(key))
      views.set(key, {
        store,
        collection,
        rows: new Map(orderedRows(store, collection).map((row) => [row.id, row])),
        rev: 0,
      });
    return key;
  }
  // The native Notes list's view is the org's private notes, which the mock's notes
  // commands keep: mirror them into the view's store so it projects what they hold.
  const notes = collections.find((c) => c.name === "notes")!;
  const nativeKey = (orgSlug: unknown) => viewKey(notes, String(orgSlug));
  async function mirrorNotes(args: Record<string, unknown>, ids?: string[]) {
    const key = nativeKey(args.orgSlug);
    const store = stores.get(key);
    if (!store || !fallback.list_notes) return;
    const listed = (await fallback.list_notes(args)) as NoteMeta[];
    const present = new Set(listed.map((n) => n.id));
    for (const id of store.keys()) if (!present.has(id)) store.delete(id);
    const due = listed.filter((n) => !ids || ids.includes(n.id) || !store.has(n.id));
    const notes = await Promise.all(
      due.map((n) => fallback.read_note({ ...args, id: n.id }) as Promise<Note>),
    );
    for (const note of notes) {
      store.set(note.id, {
        id: note.id,
        fields: {
          title: note.title,
          pinned: note.pinned,
          ...(note.updatedAt === undefined ? {} : { updatedAt: note.updatedAt }),
          ...(note.updatedBy === undefined ? {} : { updatedBy: note.updatedBy }),
        },
        body: note.content,
        rev: String(note.updatedAt ?? ""),
      });
    }
  }
  const mirrored = (command: string, kind: string) => async (args: Record<string, unknown>) => {
    const result = await fallback[command](args);
    const id = (result as { note?: { id?: string } } | undefined)?.note?.id ?? args.id;
    await mirrorNotes(args, id === undefined ? [] : [String(id)]);
    change(
      nativeKey(args.orgSlug),
      kind,
      typeof args.requestId === "string" ? args.requestId : undefined,
    );
    return result;
  };
  const noteCommands = {
    create_note: "create",
    write_note: "write",
    update_note_meta: "meta",
    delete_note: "delete",
    notes_restore: "restore",
  };
  const writeOps = ["create", "write", "setFields", "delete", "restore"];
  return {
    mint_frame: async (args) => {
      const frame = (await fallback.mint_frame(args)) as FrameRecord;
      frames.set(frame.frameId, frame);
      return frame;
    },
    rebind_frame_team: async (args) => {
      const frame = (await fallback.rebind_frame_team(args)) as FrameRecord;
      frames.set(frame.frameId, frame);
      return frame;
    },
    release_frame: async (args) => {
      frames.delete(String(args.frameId));
      return fallback.release_frame(args);
    },
    records_prepare_view: ({ frameId, collection }) => {
      const { key, store, collection: declared } = resolve(frameId, collection);
      return prepare(key, store, declared);
    },
    records_prepare_native_view: async ({ orgSlug, collection }) => {
      const declared = collections.find((c) => c.name === collection && c.scope === "private");
      if (!declared) throw new Error(`unknown collection: ${String(collection)}`);
      const key = viewKey(declared, String(orgSlug));
      if (!stores.has(key)) {
        stores.set(key, new Map());
        if (declared.name === "notes") await mirrorNotes({ orgSlug, githubUserId: 1 });
      }
      return prepare(key, stores.get(key)!, declared);
    },
    ...Object.fromEntries(
      Object.entries(noteCommands)
        .filter(([command]) => fallback[command])
        .map(([command, kind]) => [command, mirrored(command, kind)]),
    ),
    records_call: ({ frameId, operation, args: raw }) => {
      const args = (raw ?? {}) as Record<string, unknown>;
      if (["org", "team", "profile", "githubUserId"].some((k) => k in args))
        throw new Error("denied: scope is fixed by the caller grant");
      if (operation === "collections") {
        const frame = frames.get(String(frameId));
        if (!frame) throw new Error("denied: frame released");
        return collections.filter((c) =>
          frame.effectiveGrant?.includes(`records:read:${c.scope === "private" ? "own" : "team"}`),
        );
      }
      const { collection, key, store } = resolve(
        frameId,
        args.collection,
        writeOps.includes(String(operation)),
      );
      const id = String(args.id ?? "");
      const previous = store.get(id);
      const request = typeof args.requestId === "string" ? args.requestId : undefined;
      const receiptKey = JSON.stringify([key, id, request]);
      const receipt = request ? receipts.get(receiptKey) : undefined;
      if (receipt && Date.now() - receipt.at < 60_000) return structuredClone(receipt.value);
      if (operation === "create" && previous) throw new Error(`record already exists: ${id}`);
      if (operation === "list") {
        const rows = orderedRows(store, collection),
          window = args.window as [number, number] | undefined;
        return { rows: rows.slice(...(window ?? [0, 200])), total: rows.length };
      }
      if (operation === "search") {
        const query = String(args.query ?? "")
          .trim()
          .toLowerCase();
        return [...store.values()]
          .flatMap((r) => {
            const title = String(r.fields.title ?? ""),
              body = r.body.split("\n").find((s) => s.toLowerCase().includes(query)),
              hit = title.toLowerCase().includes(query);
            return !query || (!hit && !body)
              ? []
              : [
                  {
                    id: r.id,
                    title,
                    matched: hit ? (body ? "both" : "title") : "body",
                    snippet: body ?? title,
                  },
                ];
          })
          .slice(0, Number(args.limit ?? 20));
      }
      if (operation === "history" || operation === "readAt" || operation === "restore") {
        if (collection.scope !== "private")
          throw new Error("unavailable: no history for team collections");
      }
      const versions = histories.get(`${key}:${id}`) ?? [];
      if (operation === "history")
        return versions.map(({ record: v }) => ({
          commit: v.rev,
          actor: { kind: "human", name: "You" },
          change: "write",
          at: v.fields.updatedAt,
          summary: "Edited by you",
          added: 0,
          removed: 0,
        }));
      if (operation === "readAt") {
        const version = versions.find((v) => v.record.rev === args.commit);
        if (!version) throw new Error("revision not found");
        return structuredClone(version.record);
      }
      if (operation !== "create" && operation !== "restore" && !previous)
        throw new Error(`record not found: ${id}`);
      if (operation === "read") return structuredClone(previous);
      let record: RecordRow = previous
        ? structuredClone(previous)
        : { id: id || `record-${++sequence}`, fields: {}, body: "", rev: "" };
      let merged = false;
      let overlaps: Array<{ ours: string; theirs: string; at: number }> = [];
      if (operation === "delete") store.delete(id);
      else if (operation === "restore") {
        const at = versions.findIndex((v) => v.record.rev === args.commit);
        if (at < 0) throw new Error("revision not found");
        if (args.undo) {
          const restored = readDocument(
            applyMockTextChange(
              versions[at + 1]?.content ?? "",
              versions[at].content,
              document(previous),
              true,
            ).body,
          );
          record.body = restored.body;
          record.fields = {
            ...restored.fields,
            ...Object.fromEntries(
              Object.entries(record.fields).filter(([key]) => attribution.has(key)),
            ),
          };
        } else record = structuredClone(versions[at].record);
      } else if (operation === "create") record.body = String(args.body ?? "");
      else if (operation === "write") {
        if (typeof args.body === "string") {
          if (
            typeof args.base === "string" &&
            args.base !== record.body &&
            args.body !== record.body
          ) {
            merged = true;
            ({ body: record.body, overlaps } = applyMockTextChange(
              args.base,
              args.body,
              record.body,
            ));
          } else record.body = args.body;
        } else if (typeof args.append === "string")
          record.body += (record.body && !record.body.endsWith("\n") ? "\n" : "") + args.append;
        else if (args.patch) {
          const patch = args.patch as { find: string; replace: string };
          record.body = record.body.replace(patch.find, patch.replace);
        }
      }
      if (args.fields) {
        for (const [key, value] of Object.entries(args.fields as Record<string, unknown>)) {
          if (value === null) delete record.fields[key];
          else record.fields[key] = value;
        }
      }
      const changed =
        operation === "delete" || !previous || JSON.stringify(record) !== JSON.stringify(previous);
      if (changed) {
        record.rev = `commit-${++sequence}`;
        record.fields = {
          ...record.fields,
          updatedAt: Date.now(),
          updatedBy: { kind: "human", name: "You" },
        };
        if (operation !== "delete") store.set(record.id, structuredClone(record));
        if (collection.scope === "private")
          histories.set(`${key}:${record.id}`, [
            { record: structuredClone(record), content: document(store.get(record.id)) },
            ...versions,
          ]);
        change(key, String(operation), request);
      }
      const result = {
        ...record,
        changed,
        deleted: operation === "delete",
        merged,
        overlaps,
        commit: operation === "restore" ? record.rev : null,
      };
      if (request) receipts.set(receiptKey, { at: Date.now(), value: structuredClone(result) });
      return result;
    },
    live_hub_subscribe: (args) => {
      if (args.topic !== "records") return fallback.live_hub_subscribe(args);
      const key = String(args.viewKey);
      if (!views.has(key)) throw new Error("unavailable: collection view");
      const window = (args.window as [number, number] | undefined) ?? [0, 200];
      const value = snapshot(key, window);
      if (args.subscriptionId)
        subscriptions.set(String(args.subscriptionId), {
          key,
          window,
          rev: value.rev,
          total: value.total,
          loaded: new Map(value.snapshot.map((r) => [r.id, r])),
        });
      return value;
    },
    live_hub_page: (args) => {
      const subscription = subscriptions.get(String(args.subscriptionId));
      if (!subscription) return fallback.live_hub_page(args);
      const start = Number(args.cursor);
      const value = snapshot(subscription.key, [start, start + 200]);
      subscription.window = [subscription.window[0], start + value.snapshot.length];
      for (const row of value.snapshot) subscription.loaded.set(row.id, row);
      return value;
    },
    live_hub_window: (args) => {
      const subscription = subscriptions.get(String(args.subscriptionId));
      if (!subscription) return fallback.live_hub_window(args);
      subscription.window = args.window as [number, number];
      const value = snapshot(subscription.key, subscription.window);
      const next = new Map(value.snapshot.map((r) => [r.id, r]));
      const delta = {
        topic: "records",
        viewKey: subscription.key,
        rev: value.rev,
        upserts: value.snapshot,
        removes: [...subscription.loaded.keys()].filter((id) => !next.has(id)),
        total: value.total,
        cursor: value.cursor ?? null,
      };
      subscription.loaded = next;
      return delta;
    },
    live_hub_unsubscribe: (args) =>
      subscriptions.delete(String(args.subscriptionId))
        ? null
        : fallback.live_hub_unsubscribe(args),
  };
}

export function compareRecords(
  a: Pick<RecordRow, "id" | "fields">,
  b: Pick<RecordRow, "id" | "fields">,
  collection: Pick<Collection, "fields" | "order">,
): number {
  for (const spec of collection.order) {
    const [key, direction] = spec.split(" ");
    const boolean = ["boolean", "yes-no"].includes(collection.fields[key]?.type ?? "");
    const av = normalizeSortValue(a.fields[key], boolean);
    const bv = normalizeSortValue(b.fields[key], boolean);
    const cmp = compareValues(av, bv);
    if (cmp) return direction === "desc" ? -cmp : cmp;
  }
  return compareValues(a.id, b.id);
}

function normalizeSortValue(value: unknown, boolean: boolean): unknown {
  return value === undefined && boolean ? false : value;
}

// This order is specified by the shared ordering cases used by Rust and the browser mock.
function compareValues(a: unknown, b: unknown): number {
  const rank = (value: unknown): number => {
    if (value === undefined) return 0;
    if (typeof value === "boolean") return 1;
    if (typeof value === "number") return 2;
    if (typeof value === "string") return 3;
    if (value === null) return 4;
    return Array.isArray(value) ? 5 : 6;
  };
  const cmp = rank(a) - rank(b);
  if (cmp) return cmp;
  if (a === b) return 0;
  if (typeof a === "boolean" && typeof b === "boolean") return a ? 1 : -1;
  if (typeof a === "number" && typeof b === "number") return a < b ? -1 : 1;
  if (typeof a === "string" && typeof b === "string") return a < b ? -1 : 1;
  const entries = (value: unknown): unknown[] =>
    Array.isArray(value)
      ? value
      : Object.entries(value as Record<string, unknown>).sort(([a], [b]) =>
          a === b ? 0 : a < b ? -1 : 1,
        );
  const left = entries(a),
    right = entries(b);
  for (let i = 0; i < Math.min(left.length, right.length); i++) {
    const cmp = compareValues(left[i], right[i]);
    if (cmp) return cmp;
  }
  return left.length - right.length;
}
