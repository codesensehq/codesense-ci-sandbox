// SPDX-License-Identifier: MIT
import { DEFAULT_APP_THEME, type AppTheme } from "@/themes";
import type { ThemeMode } from "@/themes/compiler";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  liveHubSubscribe,
  liveHubPage,
  liveHubWindow,
  liveHubUnsubscribe,
  liveHubViewKey,
  type FrameRecord,
  agentReadSessions,
  type SessionListRow,
} from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";
import { readSharedUiState, writeSharedUiState, watchSharedUiState } from "./shared-ui-state";

// Policy: hidden badge work is bounded independently of visible delivery.
export const LIVE_HIDDEN_THROTTLE_MS = 2000;
export const MAX_LIVE_HIDDEN_FRAMES = 4;
type BadgeDelivery = { badge?: "live"; owner?: string };

const RUN_REVIEW_TOPICS = ["runEndorsements", "runEvidence"] as const;
export type RunReviewTopic = (typeof RUN_REVIEW_TOPICS)[number];
const isRunReviewTopic = (topic: string) =>
  (RUN_REVIEW_TOPICS as readonly string[]).includes(topic);
const isConfigurationTopic = (topic: string) =>
  topic === "profile" || topic === "workspaceConfig" || topic === "prefs";

export type HubRow = Record<string, unknown> & { id: string };
export type HubSnapshot = {
  topic: string;
  viewKey: string;
  rev: number;
  snapshot: HubRow[];
  total?: number;
  cursor?: string;
};
export type HubReply = Omit<HubSnapshot, "snapshot"> & { snapshot?: HubRow[]; deltas?: HubDelta[] };
export type SubscriptionOptions = {
  window?: [number, number];
  viewKey?: string;
  order?: "status";
};
type ScopedSubscription = SubscriptionOptions & {
  id: string;
  frame: Frame;
  topic: string;
  value?: HubSnapshot;
  stale?: boolean;
  retry?: boolean;
  pending?: Array<{ baseRev: number | null; delta: HubDelta }>;
  read?: Promise<HubSnapshot>;
  paused?: Promise<void>;
};
export type HubDelta = {
  topic: string;
  viewKey: string;
  rev: number;
  baseRev?: number;
  upserts: HubRow[];
  removes: string[];
  requestId?: string;
  actor?: unknown;
  cursor?: string | null;
  kind?: string;
  total?: number;
};
type Post = (message: Record<string, unknown>) => void;
type Dependencies = {
  readSessions?: () => Promise<SessionListRow[]>;
  read: (
    topic: string,
    viewKey: string,
    sinceRev?: number,
    options?: SubscriptionOptions & { subscriptionId: string },
  ) => Promise<HubReply>;
  page?: (id: string, cursor: string) => Promise<HubSnapshot>;
  window?: (id: string, window: [number, number]) => Promise<HubDelta>;
  unsubscribe?: (id: string, paused?: boolean) => Promise<void>;
  listen: (event: string, callback: (event: { payload: unknown }) => void) => Promise<UnlistenFn>;
  watchUi?: (changed: (topic: string, viewKey: string) => void) => Promise<UnlistenFn>;
  readUi: (key: string) => Promise<Record<string, unknown>>;
  writeUi: (key: string, patch: Record<string, unknown>) => Promise<Record<string, unknown>>;
};
type Frame = {
  viewKey: string;
  liveBadge: boolean;
  hiddenLive: boolean;
  shownOrder: number;
  hiddenTimer?: ReturnType<typeof setTimeout>;
  disposed: boolean;
  generation: number;
  native: boolean;
  post: Post;
  visible: boolean;
  stale: boolean;
  defaults: Set<string>;
  scoped: Map<string, ScopedSubscription>;
  pending: HubDelta[];
  resync: Set<string>;
  waits: Array<() => void>;
  invalidations: Map<string, Record<string, unknown>>;
  themeChanged?: () => void;
};
const keyOf = (topic: string, viewKey: string) => JSON.stringify([topic, viewKey]);
/** Inbox rows a committed projection pass changed, as `codesense:inbox-changed` carries them. */
type InboxChange = { team: { org: string; team: string }; messageIds: number[] };
const teamScope = (team: InboxChange["team"]) => `team:${team.org}/${team.team}`;
function unionChanges(held: InboxChange[], more: InboxChange[]): InboxChange[] {
  const byTeam = new Map(held.map((c) => [teamScope(c.team), c]));
  for (const change of more) {
    const known = byTeam.get(teamScope(change.team));
    byTeam.set(teamScope(change.team), {
      team: change.team,
      messageIds: [...new Set([...(known?.messageIds ?? []), ...change.messageIds])],
    });
  }
  return [...byTeam.values()];
}
function inboxChangesOf(payload: unknown): InboxChange[] {
  const changes = (payload as { changes?: unknown } | null)?.changes;
  return Array.isArray(changes)
    ? changes.filter((c): c is InboxChange => !!c?.team && Array.isArray(c.messageIds))
    : [];
}
/** What a frame may be told now: an inbox change names only its own team's rows, judged
 *  by the frame's current scope, so held ids never outlive a rekey. */
function deliverable(frame: Pick<Frame, "viewKey">, message: Record<string, unknown>) {
  const { changes, ...rest } = message;
  const visible = (changes as InboxChange[] | undefined)?.filter(
    (c) => !frame.viewKey.startsWith("team:") || frame.viewKey === teamScope(c.team),
  );
  return visible?.length ? { ...rest, changes: visible } : rest;
}
/** Host view state that is one for every team: the dock's folds and arrangement survive a team
 *  switch, whatever view key the frame's grant gives its data topics. */
const GLOBAL_UI_TOPICS: ReadonlySet<string> = new Set(["ui:dock", "theme"]);
const frameKey = (frame: { viewKey: string }, topic: string) =>
  GLOBAL_UI_TOPICS.has(topic) ? "all" : frame.viewKey;
const subscriptionKey = (topic: string, viewKey?: string) =>
  viewKey === undefined ? topic : keyOf(topic, viewKey);
type ViewSelector = Pick<SubscriptionOptions, "viewKey" | "order"> | string;
/** Resolve selectors only against the attached frame's current grant. */
function subscriptionViewKey(frame: Frame, topic: string, selector?: ViewSelector) {
  if (typeof selector === "string") return selector;
  return selector?.order ? `${frameKey(frame, topic)}|${selector.order}` : selector?.viewKey;
}
/** Architecture: delivery-time scope check. Grant-derived views follow the current
 * frame; independently prepared records keys name their own Rust-authorized scope,
 * so they survive a rekey while registered to this frame. Native watchers are internal. */
function canDeliver(frame: Frame, message: Record<string, unknown>) {
  if (frame.disposed) return false;
  if (frame.native || typeof message.viewKey !== "string" || typeof message.topic !== "string")
    return true;
  const base = message.viewKey.split("|")[0];
  if (base === frameKey(frame, message.topic).split("|")[0]) return true;
  if (
    message.topic === "files" &&
    frame.viewKey === "all" &&
    [...frame.scoped.values()].some((s) => s.topic === "files" && s.viewKey === message.viewKey)
  )
    return true;
  if (base === "all" || base.startsWith("team:")) return false;
  return [...frame.scoped.values()].some(
    (sub) => sub.topic === message.topic && !sub.order && sub.viewKey === message.viewKey,
  );
}
const rowId = (row: HubRow) => row.id ?? String(row.handle);

export function orderedRows(topic: string, rows: HubRow[]): HubRow[] {
  if (topic === "files")
    return rows.sort(
      (a, b) =>
        Number(b.kind === "dir") - Number(a.kind === "dir") ||
        String(a.name).localeCompare(String(b.name), undefined, { numeric: true }),
    );
  if (topic === "agentList" || topic === "records")
    return rows.sort(
      (a, b) => Number(a.order ?? 0) - Number(b.order ?? 0) || a.id.localeCompare(b.id),
    );
  if (topic !== "inbox") return rows.sort((a, b) => a.id.localeCompare(b.id));
  const at = (row: HubRow) =>
    Number(
      row.createdAt ??
        (row.message as { createdAtMs?: number } | undefined)?.createdAtMs ??
        row.messageId ??
        0,
    );
  return rows.sort(
    (a, b) =>
      Number(b.rowType === "counts") - Number(a.rowType === "counts") ||
      at(b) - at(a) ||
      Number(b.messageId ?? 0) - Number(a.messageId ?? 0) ||
      a.id.localeCompare(b.id),
  );
}

export async function paneViewKey(frame: Pick<FrameRecord, "origin" | "token">): Promise<string> {
  return liveHubViewKey(frame.origin, frame.token);
}

export class LiveHub {
  private themeValue = { themeId: DEFAULT_APP_THEME, mode: "light" as ThemeMode, rev: 0 };
  publishTheme(themeId: AppTheme, mode: ThemeMode, force = false) {
    if (!force && themeId === this.themeValue.themeId && mode === this.themeValue.mode) return;
    this.seed({
      topic: "theme",
      viewKey: "all",
      rev: this.themeValue.rev,
      snapshot: [{ id: "theme", ...this.themeValue }],
    });
    this.themeValue = { themeId, mode, rev: this.themeValue.rev + 1 };
    this.receive({
      topic: "theme",
      viewKey: "all",
      rev: this.themeValue.rev,
      upserts: [{ id: "theme", ...this.themeValue }],
      removes: [],
    });
  }
  private frames = new Map<string, Frame>();
  private shownOrder = 0;
  private scoped = new Map<string, ScopedSubscription>();
  private views = new Map<string, HubSnapshot>();
  private pendingReads = new Map<string, HubDelta[]>();
  /** What a scoped view held just before each delta, plus the envelope base the host sent: the host
   *  may suppress revisions, and unchanged `total`/`cursor` can fold. */
  private envelopes = new WeakMap<
    HubDelta,
    { baseRev: number | null; total?: number; cursor?: string }
  >();
  private reads = new Map<string, Promise<HubSnapshot>>();
  private unlisteners: Promise<UnlistenFn>[] = [];
  private ready: Promise<void> | null = null;
  private joinedSessionsRead?: Promise<SessionListRow[]>;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private uiTimers = new Map<string, ReturnType<typeof setTimeout>>();
  private uiWrites = new Map<string, Promise<unknown>>();
  constructor(private deps: Dependencies) {}
  private listen(event: string, callback: (event: { payload: unknown }) => void) {
    this.unlisteners.push(
      this.deps.listen(event, callback).catch((error) => {
        logger.info("live-hub", `Cannot listen for ${event}: ${String(error)}`);
        return () => {};
      }),
    );
  }
  /** A native write updates the same cached UI view that dashboard subscribers use. */
  refreshUi(topic: string, viewKey: string): void {
    const current = this.views.get(keyOf(topic, viewKey));
    if (!current) return;
    void (this.uiWrites.get(keyOf(topic, viewKey)) ?? Promise.resolve())
      .catch(() => {})
      .then(() => this.deps.readUi(keyOf(topic, viewKey)))
      .then((state) => {
        const latest = this.views.get(keyOf(topic, viewKey));
        if (
          latest &&
          JSON.stringify(latest.snapshot[0]) !== JSON.stringify({ ...state, id: topic })
        )
          this.receive({
            topic,
            viewKey,
            rev: latest.rev + 1,
            upserts: [{ ...state, id: topic }],
            removes: [],
          });
      })
      .catch((error) => logger.info("live-hub", `Cannot refresh ${topic}: ${String(error)}`));
  }
  private start() {
    if (this.ready) return this.ready;
    if (this.deps.watchUi)
      this.unlisteners.push(this.deps.watchUi((topic, key) => this.refreshUi(topic, key)));
    this.listen("codesense:delta", (e) => this.receive(e.payload as HubDelta));
    this.listen("codesense:subscription-delta", ({ payload }) => {
      const { subscriptionId, baseRev, delta } = payload as {
        subscriptionId: string;
        baseRev: number | null;
        delta: HubDelta;
      };
      const subscription = this.scoped.get(subscriptionId);
      if (!subscription) return;
      if (!this.delivering(subscription.frame)) {
        // An old in-flight reply must not clear a discarded scoped delta's invalidation.
        if (subscription.read) subscription.retry = true;
        this.stale(subscription.frame);
        return;
      }
      if (subscription.read) {
        const pending = subscription.pending ?? (subscription.pending = []);
        if (pending.length < 256) pending.push({ baseRev, delta });
        else {
          pending.length = 0;
          subscription.retry = true;
        }
        return;
      }
      if (!subscription.value || subscription.value.rev !== baseRev) {
        subscription.stale = true;
        subscription.frame.resync.add(keyOf(delta.topic, delta.viewKey));
        this.schedule();
        return;
      }
      this.envelopes.set(delta, {
        baseRev,
        total: subscription.value.total,
        cursor: subscription.value.cursor,
      });
      this.applyScoped(subscription, delta);
      this.queueDelta(subscription.frame, delta);
      this.schedule();
    });
    for (const [event, type] of [
      ["agent-inbox-changed", "inbox-changed"],
      ["routines:changed", "routines-changed"],
      ["machines-changed", "machines-changed"],
      ["worktree-changed", "worktrees-changed"],
      ["context-index-changed", "context-changed"],
      ["mcp:agent-message", "agent-activity-changed"],
      ["mcp:agent-progress", "agent-activity-changed"],
    ])
      this.listen(event, ({ payload }) => {
        if (type === "inbox-changed") this.joinedSessionsRead = undefined;
        for (const frame of this.frames.values()) {
          const message = {
            type: `codesense:${type}`,
            ...(type === "context-changed"
              ? { orgs: [], allOrgs: true, truncated: true, ...(payload as object) }
              : {}),
          };
          const changes = type === "inbox-changed" ? inboxChangesOf(payload) : [];
          if (!frame.visible) {
            // An invalidation can change data outside the delta stream, so consumers must reread.
            const held = frame.invalidations.get(type);
            frame.invalidations.set(
              type,
              type === "context-changed" && held
                ? { ...message, allOrgs: true }
                : type === "inbox-changed"
                  ? {
                      ...message,
                      ...(changes.length || held?.changes
                        ? {
                            changes: unionChanges(
                              (held?.changes as InboxChange[] | undefined) ?? [],
                              changes,
                            ),
                          }
                        : {}),
                    }
                  : message,
            );
            if (frame.hiddenLive) this.schedule();
            else this.stale(frame);
            continue;
          }
          frame.post(
            type === "inbox-changed"
              ? deliverable(frame, changes.length ? { ...message, changes } : message)
              : message,
          );
        }
      });
    this.ready = Promise.all(this.unlisteners).then(() => {});
    return this.ready;
  }
  seed(snapshot: HubSnapshot) {
    const key = keyOf(snapshot.topic, snapshot.viewKey);
    if ((this.views.get(key)?.rev ?? -1) <= snapshot.rev) this.views.set(key, snapshot);
  }
  async joinSessions(rows: HubRow[]): Promise<Array<HubRow | SessionListRow>> {
    if (!this.joinedSessionsRead) {
      const read = this.deps.readSessions!().catch((error) => {
        if (this.joinedSessionsRead === read) this.joinedSessionsRead = undefined;
        throw error;
      });
      this.joinedSessionsRead = read;
    }
    const joined = new Map((await this.joinedSessionsRead).map((row) => [row.handle, row]));
    return rows.map((row) => joined.get(String(row.handle ?? row.id)) ?? row);
  }
  /** Recover one cached view through the same read/delta ordering as a subscription. */
  resync(topic: string, viewKey: string): Promise<HubSnapshot> {
    this.views.delete(keyOf(topic, viewKey));
    return this.snapshot(topic, viewKey);
  }
  async snapshot(topic: string, viewKey: string, sinceRev?: number): Promise<HubSnapshot> {
    await this.start();
    const key = keyOf(topic, viewKey);
    if (topic === "theme")
      return {
        topic,
        viewKey: "all",
        rev: this.themeValue.rev,
        snapshot: [{ id: "theme", ...this.themeValue }],
      };
    const cached = this.views.get(key);
    if (cached) return cached;
    let read = this.reads.get(key);
    if (!read) {
      read = (
        topic.startsWith("ui:")
          ? this.deps.readUi(key).then(
              (state): HubSnapshot => ({
                topic,
                viewKey,
                rev: 0,
                snapshot: [{ ...state, id: topic }],
              }),
            )
          : this.deps.read(topic, viewKey, sinceRev)
      )
        .then((result) => {
          if (result.topic !== topic || result.viewKey !== viewKey)
            throw new Error("Unexpected projection reply");
          if (!result.snapshot) {
            this.reads.delete(key);
            return this.snapshot(topic, viewKey);
          }
          this.seed(result as HubSnapshot);
          this.reads.delete(key);
          const pending = this.pendingReads.get(key) ?? [];
          this.pendingReads.delete(key);
          for (const delta of pending) this.receive(delta);
          if (!this.views.has(key)) return this.snapshot(topic, viewKey, result.rev);
          return this.views.get(key)!;
        })
        .finally(() => this.reads.delete(key));
      this.reads.set(key, read);
    }
    return read;
  }
  attach(
    id: string,
    viewKey: string,
    post: Post,
    visible: boolean,
    native = false,
    themeChanged?: () => void,
    delivery: BadgeDelivery = {},
  ) {
    this.start();
    const subscriptionId = (key: string) =>
      `${delivery.owner ? delivery.owner + ":" : ""}${id}:${key}`;
    const frame: Frame = {
      viewKey,
      liveBadge: delivery.badge === "live",
      hiddenLive: false,
      shownOrder: ++this.shownOrder,
      disposed: false,
      generation: 0,
      native,
      post: (message) => {
        if (canDeliver(frame, message)) post(message);
      },
      visible,
      stale: false,
      defaults: new Set(themeChanged ? ["theme"] : []),
      themeChanged,
      scoped: new Map(),
      pending: [],
      resync: new Set(),
      waits: [],
      invalidations: new Map(),
    };
    this.frames.set(id, frame);
    this.balanceHidden();
    /** Resync each subscribed view independently. */
    const resyncAll = () => {
      // A view rebind or window move changes the dataset boundary and cannot be folded safely.
      for (const topic of frame.defaults) {
        if (topic === "theme" && frame.themeChanged) frame.themeChanged();
        else frame.post({ type: "codesense:resync", topic, viewKey: frameKey(frame, topic) });
      }
      for (const sub of frame.scoped.values())
        if (sub.viewKey !== undefined)
          frame.post({ type: "codesense:resync", topic: sub.topic, viewKey: sub.viewKey });
    };
    const subscribe = async (topic: string, sinceRev?: number, options?: SubscriptionOptions) => {
      const started = frame.generation;
      if (options?.viewKey === undefined && !options?.order) frame.defaults.add(topic);
      await this.start();
      if (frame.disposed) throw new Error("frame disposed");
      while (!this.delivering(frame) && !frame.disposed) {
        this.stale(frame);
        await new Promise<void>((resolve) => frame.waits.push(resolve));
      }
      if (frame.disposed) throw new Error("frame disposed");
      const generation = frame.generation;
      if (started !== generation && topic === "agentDefinitions" && options?.viewKey)
        throw new Error("unavailable: subscription scope changed");
      if (started !== generation) sinceRev = undefined;
      const viewKey = subscriptionViewKey(frame, topic, options);
      let result: HubSnapshot;
      let moved = false;
      try {
        if (
          topic === "inbox" ||
          topic === "records" ||
          topic === "stories" ||
          topic === "machineStats" ||
          isRunReviewTopic(topic) ||
          options?.window ||
          viewKey !== undefined
        ) {
          const scopeKey = subscriptionKey(topic, viewKey);
          let subscription = frame.scoped.get(scopeKey);
          if (!subscription) {
            subscription = {
              id: subscriptionId(scopeKey),
              frame,
              topic,
              window:
                options?.window ?? (["records", "stories"].includes(topic) ? [0, 200] : undefined),
              viewKey,
              order: options?.order,
            };
            frame.scoped.set(scopeKey, subscription);
            this.scoped.set(subscription.id, subscription);
          }
          if (
            options?.window &&
            JSON.stringify(subscription.window) !== JSON.stringify(options.window)
          ) {
            subscription.window = options.window;
            moved = true;
            if (subscription.read) await subscription.read;
            if (frame.disposed) throw new Error("frame disposed");
            if (generation !== frame.generation) return subscribe(topic, undefined, options);
            if (this.deps.window && subscription.value) {
              const delta = await this.deps.window(subscription.id, options.window);
              if (generation !== frame.generation) return subscribe(topic, undefined, options);
              if (canDeliver(frame, delta)) this.applyScoped(subscription, delta);
            }
          }
          result = await this.readScoped(subscription, sinceRev);
          if (moved) {
            // Queued deltas up to the moved window's revision describe rows the new window dropped;
            // folding them with newer deltas would put those rows back. A write echo still has to
            // reach the page that is waiting on its requestId, so it goes out without its rows.
            const key = keyOf(topic, subscription.viewKey ?? frameKey(frame, topic));
            const dropped: HubDelta[] = [];
            frame.pending = frame.pending.filter((queued) => {
              const drop = keyOf(queued.topic, queued.viewKey) === key && queued.rev <= result.rev;
              if (drop) dropped.push(queued);
              return !drop;
            });
            for (const queued of dropped)
              if (
                queued.requestId !== undefined ||
                queued.actor !== undefined ||
                queued.kind !== undefined
              )
                frame.post({
                  type: "codesense:delta",
                  ...queued,
                  baseRev: this.envelopes.get(queued)?.baseRev ?? queued.rev - 1,
                  upserts: [],
                  removes: [],
                });
          }
        } else result = await this.snapshot(topic, frameKey(frame, topic), sinceRev);
      } catch (error) {
        if (topic === "agentDefinitions" && generation !== frame.generation) throw error;
        if (!frame.disposed && generation !== frame.generation)
          return subscribe(topic, undefined, options);
        throw error;
      }
      if (!frame.disposed && generation !== frame.generation && topic === "agentDefinitions")
        throw new Error("unavailable: subscription scope changed");
      if (!frame.disposed && generation !== frame.generation)
        return subscribe(topic, undefined, options);
      if (!canDeliver(frame, result)) throw new Error("unavailable: subscription scope changed");
      return result;
    };
    return {
      subscribe,
      unsubscribe: async (topic: string, selector?: ViewSelector) => {
        const scopedViewKey = subscriptionViewKey(frame, topic, selector);
        const scopeKey = subscriptionKey(topic, scopedViewKey);
        const subscription = frame.scoped.get(scopeKey);
        let retired: Promise<void> | undefined;
        if (subscription) {
          this.scoped.delete(subscription.id);
          frame.scoped.delete(scopeKey);
          retired = this.releaseAfterRead(subscription);
        }
        if (scopedViewKey === undefined) frame.defaults.delete(topic);
        frame.resync.delete(keyOf(topic, scopedViewKey ?? frameKey(frame, topic)));
        frame.pending = frame.pending.filter(
          (d) => d.topic !== topic || (scopedViewKey && d.viewKey !== scopedViewKey),
        );
        await retired;
      },
      page: async (
        topic: string,
        cursor: string,
        selector?: ViewSelector,
      ): Promise<HubSnapshot> => {
        while (!this.delivering(frame) && !frame.disposed) {
          this.stale(frame);
          await new Promise<void>((resolve) => frame.waits.push(resolve));
        }
        if (frame.disposed) throw new Error("frame disposed");
        const scopedViewKey = subscriptionViewKey(frame, topic, selector);
        const subscription = frame.scoped.get(subscriptionKey(topic, scopedViewKey));
        if (!subscription || !this.deps.page) throw new Error("subscription unavailable");
        const page = await this.deps.page(subscription.id, cursor);
        if (this.scoped.get(subscription.id) !== subscription || !canDeliver(frame, page))
          throw new Error("unavailable: subscription scope changed");
        if (
          (topic === "records" || topic === "files" || topic === "stories") &&
          subscription.window
        )
          subscription.window = [
            subscription.window[0],
            subscription.window[1] + page.snapshot.length,
          ];
        if (subscription.value) {
          const current = subscription.value;
          subscription.value = {
            ...(page.rev > current.rev ? page : current),
            cursor: page.cursor,
            snapshot: orderedRows(topic, [
              ...new Map(
                [...page.snapshot, ...current.snapshot].map((row) => [row.id, row]),
              ).values(),
            ]),
          };
        }
        return page;
      },
      visible: (shown: boolean) => {
        if (frame.visible === shown) return;
        frame.visible = shown;
        if (shown) frame.shownOrder = ++this.shownOrder;
        this.balanceHidden();
        if (!shown) {
          if (!frame.hiddenLive) this.pause(frame);
          return;
        }
        if (frame.hiddenTimer) clearTimeout(frame.hiddenTimer);
        frame.hiddenTimer = undefined;
        frame.pending = [];
        frame.stale = false;
        for (const message of frame.invalidations.values()) frame.post(deliverable(frame, message));
        frame.invalidations.clear();
        resyncAll();
        frame.waits.splice(0).forEach((resolve) => resolve());
      },
      writeUi: async (topic: string, patch: Record<string, unknown>, requestId?: string) => {
        const value = await this.writeUi(topic, frameKey(frame, topic), patch, requestId);
        if (!canDeliver(frame, value)) throw new Error("unavailable: subscription scope changed");
        return value;
      },
      /** Moves the frame's subscriptions to a new view key (a re-bound grant) and asks the
       *  page to resync every topic it reads. The synchronous handoff rotates the page's
       *  token after keys advance and before any new-scope notifications are posted. */
      rekey: (next: string, beforeResync?: () => void) => {
        if (frame.disposed) return;
        frame.generation++;
        frame.viewKey = next;
        frame.resync.clear();
        frame.pending = [];
        const filesToResync: string[] = [];
        // Prepared records views stay bound; file views follow the new team.
        for (const [key, subscription] of [...frame.scoped]) {
          if (
            subscription.viewKey &&
            !subscription.order &&
            subscription.topic !== "files" &&
            subscription.topic !== "agentDefinitions"
          )
            continue;
          if (subscription.topic === "files" && subscription.viewKey)
            filesToResync.push(
              next + subscription.viewKey.slice(subscription.viewKey.indexOf("|")),
            );
          this.scoped.delete(subscription.id);
          frame.scoped.delete(key);
          void this.deps.unsubscribe?.(subscription.id).catch(() => {});
          if (subscription.topic === "agentDefinitions") frame.defaults.add(subscription.topic);
          if (subscription.order) {
            const viewKey = subscriptionViewKey(frame, subscription.topic, subscription);
            const nextKey = subscriptionKey(subscription.topic, viewKey);
            const rebound: ScopedSubscription = {
              id: subscriptionId(nextKey),
              frame,
              topic: subscription.topic,
              window: subscription.window,
              viewKey,
              order: subscription.order,
            };
            frame.scoped.set(nextKey, rebound);
            this.scoped.set(rebound.id, rebound);
          }
        }
        beforeResync?.();
        if (frame.visible) {
          resyncAll();
          for (const viewKey of filesToResync)
            frame.post({ type: "codesense:resync", topic: "files", viewKey });
        } else if (frame.hiddenLive) {
          this.queueResync(frame);
          for (const viewKey of filesToResync) frame.resync.add(keyOf("files", viewKey));
          this.schedule();
        } else this.stale(frame);
      },
      dispose: () => {
        frame.generation++;
        frame.disposed = true;
        if (frame.hiddenTimer) clearTimeout(frame.hiddenTimer);
        frame.hiddenTimer = undefined;
        frame.waits.splice(0).forEach((resolve) => resolve());
        if (this.frames.get(id) === frame) this.frames.delete(id);
        for (const subscription of frame.scoped.values()) {
          this.scoped.delete(subscription.id);
          if (isRunReviewTopic(subscription.topic)) {
            void this.releaseAfterRead(subscription);
          } else void this.deps.unsubscribe?.(subscription.id).catch(() => {});
        }
        frame.scoped.clear();
        if (!this.frames.size && this.timer) {
          clearTimeout(this.timer);
          this.timer = null;
        }
        this.balanceHidden();
      },
    };
  }
  private applyScoped(subscription: ScopedSubscription, delta: HubDelta) {
    const rows = new Map((subscription.value?.snapshot ?? []).map((row) => [row.id, row]));
    for (const id of delta.removes) rows.delete(id);
    for (const row of delta.upserts) rows.set(row.id, row);
    subscription.value = {
      topic: delta.topic,
      viewKey: delta.viewKey,
      rev: delta.rev,
      total: delta.total ?? subscription.value?.total,
      cursor:
        delta.topic === "records" || delta.topic === "files" || delta.topic === "stories"
          ? (delta.cursor ?? undefined)
          : subscription.value?.cursor,
      snapshot: orderedRows(delta.topic, [...rows.values()]),
    };
  }
  private releaseAfterRead(subscription: ScopedSubscription): Promise<void> {
    return Promise.resolve(subscription.read)
      .catch(() => {})
      .then(() => this.deps.unsubscribe?.(subscription.id))
      .catch(() => {});
  }
  private readScoped(subscription: ScopedSubscription, sinceRev?: number): Promise<HubSnapshot> {
    if (subscription.read) return subscription.read;
    if (subscription.value && !subscription.stale) return Promise.resolve(subscription.value);
    subscription.read = Promise.resolve(subscription.paused)
      .then(() =>
        this.deps.read(
          subscription.topic,
          subscription.viewKey ?? subscription.frame.viewKey,
          sinceRev ??
            (isRunReviewTopic(subscription.topic) || isConfigurationTopic(subscription.topic)
              ? subscription.value?.rev
              : undefined),
          {
            subscriptionId: subscription.id,
            window: subscription.window,
          },
        ),
      )
      .then(async (reply) => {
        if (subscription.frame.disposed || this.scoped.get(subscription.id) !== subscription) {
          await this.deps.unsubscribe?.(subscription.id).catch(() => {});
          throw new Error("subscription retired");
        }
        if (!canDeliver(subscription.frame, reply))
          throw new Error("unavailable: subscription scope changed");
        if (reply.snapshot) subscription.value = { ...reply, snapshot: reply.snapshot };
        else for (const delta of reply.deltas ?? []) this.applyScoped(subscription, delta);
        if (!subscription.value) throw new Error("snapshot unavailable");
        subscription.stale = !this.delivering(subscription.frame) || (subscription.retry ?? false);
        subscription.retry = false;
        for (const { baseRev, delta } of subscription.pending?.splice(0) ?? []) {
          if (baseRev === null) {
            subscription.stale = true;
            break;
          }
          if (delta.rev <= subscription.value.rev) continue;
          if (baseRev !== subscription.value.rev) {
            subscription.stale = true;
            break;
          }
          this.applyScoped(subscription, delta);
        }
        return subscription.value;
      })
      .finally(() => {
        subscription.read = undefined;
        if (
          subscription.stale &&
          this.delivering(subscription.frame) &&
          !subscription.frame.disposed
        ) {
          const frame = subscription.frame;
          const viewKey = subscription.viewKey ?? frame.viewKey;
          if (frame.visible)
            frame.post({ type: "codesense:resync", topic: subscription.topic, viewKey });
          else {
            // Architecture: read recovery shares the hidden frame's throttled delivery path.
            frame.resync.add(keyOf(subscription.topic, viewKey));
            this.schedule();
          }
        }
      });
    return subscription.read;
  }
  private stale(frame: Frame) {
    if (frame.stale) return;
    frame.stale = true;
    frame.post({ type: "codesense:stale" });
  }
  receive(delta: HubDelta) {
    if (delta.topic === "sessions") this.joinedSessionsRead = undefined;
    const key = keyOf(delta.topic, delta.viewKey);
    const current = this.views.get(key);
    if (!current && this.reads.has(key)) {
      const pending = this.pendingReads.get(key) ?? [];
      if (pending.length < 256) pending.push(delta);
      else pending.splice(0, pending.length, delta);
      this.pendingReads.set(key, pending);
      return;
    }
    if (current && delta.rev <= current.rev) return;
    const gap = !current || delta.rev !== current.rev + 1;
    if (gap) this.views.delete(key);
    else {
      const rows = new Map(current.snapshot.map((row) => [rowId(row), row]));
      for (const id of delta.removes) rows.delete(id);
      for (const row of delta.upserts) rows.set(rowId(row), row);
      this.views.set(key, {
        topic: delta.topic,
        viewKey: delta.viewKey,
        rev: delta.rev,
        snapshot: orderedRows(delta.topic, [...rows.values()]),
      });
    }
    for (const frame of this.frames.values()) {
      if (
        frameKey(frame, delta.topic) !== delta.viewKey ||
        !frame.defaults.has(delta.topic) ||
        frame.scoped.has(delta.topic)
      )
        continue;
      if (!this.delivering(frame)) {
        this.stale(frame);
        continue;
      }
      if (delta.topic === "theme" && frame.themeChanged) {
        frame.themeChanged();
        continue;
      }
      this.queueDelta(frame, delta, gap);
    }
    this.schedule();
  }
  private queueDelta(frame: Frame, delta: HubDelta, gap = false) {
    const key = keyOf(delta.topic, delta.viewKey);
    if (gap || frame.pending.length >= 256) {
      // A gap loses rows; overflow discards the batch. In either case invalidate every queued view.
      for (const queued of frame.pending) frame.resync.add(keyOf(queued.topic, queued.viewKey));
      frame.pending = [];
      frame.resync.add(key);
    } else if (!frame.resync.has(key)) frame.pending.push(delta);
  }
  private delivering(frame: Frame) {
    return frame.visible || frame.hiddenLive;
  }
  private pause(frame: Frame) {
    for (const subscription of frame.scoped.values()) {
      subscription.stale = true;
      if (subscription.topic === "machineStats")
        void this.deps.unsubscribe?.(subscription.id).catch(() => {});
      if (isRunReviewTopic(subscription.topic) || isConfigurationTopic(subscription.topic)) {
        subscription.paused = Promise.resolve(subscription.read)
          .catch(() => {})
          .then(async () => {
            if (!this.delivering(frame))
              await this.deps.unsubscribe?.(subscription.id, isRunReviewTopic(subscription.topic));
          });
      }
    }
    frame.pending = [];
    this.stale(frame);
  }
  private queueResync(frame: Frame) {
    for (const topic of frame.defaults) frame.resync.add(keyOf(topic, frameKey(frame, topic)));
    for (const sub of frame.scoped.values())
      frame.resync.add(keyOf(sub.topic, sub.viewKey ?? frameKey(frame, sub.topic)));
  }
  private balanceHidden() {
    // Policy: retain the four most recently shown frames; evict the oldest-shown first.
    const admitted = new Set(
      [...this.frames.values()]
        .filter((frame) => !frame.visible && frame.liveBadge && !frame.disposed)
        .sort((a, b) => b.shownOrder - a.shownOrder)
        .slice(0, MAX_LIVE_HIDDEN_FRAMES),
    );
    for (const frame of this.frames.values()) {
      const wasLive = frame.hiddenLive;
      frame.hiddenLive = admitted.has(frame);
      if (wasLive && !frame.hiddenLive) {
        if (frame.hiddenTimer) clearTimeout(frame.hiddenTimer);
        frame.hiddenTimer = undefined;
        if (!frame.visible) this.pause(frame);
      }
      if (!wasLive && frame.hiddenLive) {
        if (frame.stale) {
          this.queueResync(frame);
          frame.stale = false;
        }
        frame.waits.splice(0).forEach((resolve) => resolve());
      }
    }
    this.schedule();
  }
  private flush(frame: Frame) {
    const topics = new Map<string, HubDelta[]>();
    for (const d of frame.pending.splice(0)) {
      const key = keyOf(d.topic, d.viewKey);
      topics.set(key, [...(topics.get(key) ?? []), d]);
    }
    for (const [topic, deltas] of topics) {
      if (frame.resync.has(topic)) continue;
      let run: HubDelta[] = [];
      let last: HubDelta | undefined;
      const baseOf = (delta: HubDelta) => this.envelopes.get(delta)?.baseRev ?? delta.rev - 1;
      const postDelta = (delta: HubDelta) =>
        frame.post({
          type: "codesense:delta",
          ...delta,
          ...(baseOf(delta) !== delta.rev - 1 && { baseRev: baseOf(delta) }),
        });
      const flushRun = () => {
        if (!run.length) return;
        if (run.length === 1) postDelta(run[0]);
        else {
          // A run must be contiguous so the receiver can prove continuity from baseRev.
          const upserts = new Map<string, HubRow>();
          const removes = new Set<string>();
          for (const delta of run) {
            for (const id of delta.removes) {
              upserts.delete(id);
              removes.add(id);
            }
            for (const row of delta.upserts) {
              removes.delete(rowId(row));
              upserts.delete(rowId(row));
              upserts.set(rowId(row), row);
            }
          }
          frame.post({
            type: "codesense:delta",
            ...run.at(-1)!,
            baseRev: baseOf(run[0]),
            upserts: [...upserts.values()],
            removes: [...removes],
          });
        }
        run = [];
      };
      for (const delta of deltas) {
        // The host may suppress revisions, so continuity is the envelope base meeting the last rev.
        if (last && baseOf(delta) !== last.rev) {
          // A revision gap means intermediate rows are missing; request a fresh snapshot.
          flushRun();
          frame.resync.add(topic);
          break;
        }
        last = delta;
        const prior = this.envelopes.get(delta);
        const metadataChanged =
          delta.requestId !== undefined ||
          delta.actor !== undefined ||
          delta.kind !== undefined ||
          (delta.cursor !== undefined &&
            (!prior || (delta.cursor ?? undefined) !== (prior.cursor ?? undefined))) ||
          (delta.total !== undefined && (!prior || delta.total !== prior.total));
        if (metadataChanged) {
          flushRun();
          postDelta(delta);
        } else run.push(delta);
      }
      if (!frame.resync.has(topic)) flushRun();
    }
    for (const key of frame.resync) {
      const [topic, viewKey] = JSON.parse(key) as [string, string];
      frame.post({ type: "codesense:resync", topic, viewKey });
    }
    frame.resync.clear();
    for (const message of frame.invalidations.values()) frame.post(deliverable(frame, message));
    frame.invalidations.clear();
  }
  private schedule() {
    for (const frame of this.frames.values()) {
      if (!frame.pending.length && !frame.resync.size && !frame.invalidations.size) continue;
      if (frame.hiddenLive) {
        if (frame.hiddenTimer) continue;
        frame.hiddenTimer = setTimeout(() => {
          frame.hiddenTimer = undefined;
          if (!frame.disposed && frame.hiddenLive) this.flush(frame);
        }, LIVE_HIDDEN_THROTTLE_MS);
      } else if (frame.visible && !this.timer) {
        this.timer = setTimeout(() => {
          this.timer = null;
          for (const visible of this.frames.values()) if (visible.visible) this.flush(visible);
        }, 0);
      }
    }
  }
  writeUi(
    topic: string,
    viewKey: string,
    patch: Record<string, unknown>,
    requestId?: string,
  ): Promise<HubSnapshot> {
    if (!topic.startsWith("ui:"))
      return Promise.reject(new Error("writeUi needs a ui:<slotId> topic"));
    const key = keyOf(topic, viewKey);
    const prior = this.uiWrites.get(key) ?? Promise.resolve();
    const next = prior
      .catch(() => {})
      .then(async () => {
        const before = await this.snapshot(topic, viewKey);
        const state = await this.deps.writeUi(key, patch);
        await new Promise<void>((resolve) => {
          this.uiTimers.set(
            key,
            setTimeout(() => {
              this.uiTimers.delete(key);
              this.receive({
                topic,
                viewKey,
                rev: before.rev + 1,
                upserts: [{ ...state, id: topic }],
                removes: [],
                requestId,
              });
              resolve();
            }, 50),
          );
        });
        const result = this.views.get(key)!;
        return result;
      });
    this.uiWrites.set(key, next);
    return next;
  }
  /** A native subscription: `callback` gets the view on subscribe and after each
   *  change, with the delta that changed it when one did (not on a resync). */
  watch(
    topic: string,
    viewKey: string,
    callback: (snapshot: HubSnapshot, change?: HubDelta) => void,
    options?: SubscriptionOptions,
  ): () => void {
    const id = `native:${crypto.randomUUID()}`;
    const frame = this.attach(
      id,
      viewKey,
      (message) => {
        if (message.topic !== topic) return;
        const change = message.type === "codesense:delta" ? (message as HubDelta) : undefined;
        void frame
          .subscribe(topic, undefined, options)
          .then((snapshot) => callback(snapshot, change))
          .catch((error) => {
            logger.info("live-hub", `Cannot resync ${topic}: ${String(error)}`);
          });
      },
      true,
      true,
    );
    void frame
      .subscribe(topic, undefined, options)
      .then((snapshot) => callback(snapshot))
      .catch(() => {});
    return frame.dispose;
  }
  dispose() {
    for (const off of this.unlisteners) void off.then((fn) => fn()).catch(() => {});
    if (this.timer) clearTimeout(this.timer);
    for (const timer of this.uiTimers.values()) clearTimeout(timer);
    for (const frame of this.frames.values()) {
      if (frame.hiddenTimer) clearTimeout(frame.hiddenTimer);
      frame.disposed = true;
      frame.waits.splice(0).forEach((resolve) => resolve());
    }
    for (const subscription of this.scoped.values())
      void this.deps.unsubscribe?.(subscription.id).catch(() => {});
    this.frames.clear();
    this.scoped.clear();
    this.unlisteners = [];
    this.ready = null;
    this.joinedSessionsRead = undefined;
    this.views.clear();
    this.reads.clear();
    this.pendingReads.clear();
    this.uiWrites.clear();
    this.uiTimers.clear();
    this.timer = null;
  }
}
export const liveHub = new LiveHub({
  readSessions: async () => agentReadSessions(),
  read: async (...args) => liveHubSubscribe(...args),
  page: async (...args) => liveHubPage(...args),
  window: async (...args) => liveHubWindow(...args),
  unsubscribe: async (...args) => {
    await liveHubUnsubscribe(...args);
  },
  listen,
  readUi: readSharedUiState,
  writeUi: writeSharedUiState,
  watchUi: watchSharedUiState,
});
export type HubFrame = ReturnType<LiveHub["attach"]>;
