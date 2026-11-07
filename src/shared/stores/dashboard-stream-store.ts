// SPDX-License-Identifier: MIT
import { create } from "zustand";

import type {
  StatusScope,
  SessionStatusDelta,
  StatusCallback,
  Unsubscribe,
} from "@/shared/lib/dashboard-stream/types";

/**
 * SW-DX-3 — the in-app agent status hub. The app-root hook
 * (`use-dashboard-stream`) feeds session events in via `pushSessionDelta`; this
 * store owns the subscription registry and the live/stale freshness. Fully
 * unit-testable without React or Tauri.
 *
 * SW-OC-26: doc changes no longer flow through here — in-app doc views (csquery
 * blocks, panes) query the one Rust context engine directly
 * (`subscribeContext`), and HTML dashboards use the SSE stream. This hub carries
 * session/story deltas only.
 *
 * The in-app transport is a DIRECT in-process callback (zero IPC, zero round-
 * trip — never blocks the UI).
 */

type Subscription = {
  id: string;
  scope: StatusScope;
  cb: StatusCallback;
};

/**
 * SW-GW-3: resolve a `storyId` to the sessionId its workspace is bound to RIGHT
 * NOW (or null). Injected from the app root (`use-dashboard-stream`) so this
 * store stays viewer-store-free and unit-testable; the default answers "nothing
 * open" so a story sub simply never matches until the resolver is wired.
 */
export type StorySessionResolver = (storyId: string) => string | null;

interface DashboardStreamState {
  /** Non-reactive registry (add/remove replace the Map so React can observe). */
  subs: Map<string, Subscription>;
  /** Reactive freshness per subscription — a badge component selects this. */
  fresh: Record<string, boolean>;
  /** SW-GW-3: the injected `storyId → current sessionId` resolver (see type). */
  resolveStorySession: StorySessionResolver;

  /** Register a subscription; returns an idempotent unsubscribe (AC-3). */
  subscribe: (scope: StatusScope, cb: StatusCallback) => Unsubscribe;
  /** SW-GW-3: wire the trusted `storyId → sessionId` resolver (app-root only). */
  setStorySessionResolver: (resolver: StorySessionResolver) => void;
  /** Remove a subscription (idempotent). */
  unsubscribe: (id: string) => void;
  /** Current freshness for a sub (`true` if unknown/absent). */
  getFresh: (id: string) => boolean;

  /** Deliver a session (agent) delta to matching `session`/`story` subs (AC-5). */
  pushSessionDelta: (delta: SessionStatusDelta) => void;
  /** Flip every subscription to stale (AC-7) — channel error / webview hidden. */
  markStale: () => void;
}

let subCounter = 0;

export const useDashboardStreamStore = create<DashboardStreamState>((set, get) => ({
  subs: new Map(),
  fresh: {},
  resolveStorySession: () => null,

  setStorySessionResolver: (resolver) => set({ resolveStorySession: resolver }),

  subscribe: (scope, cb) => {
    const id = `sub_${++subCounter}`;
    set((s) => {
      const subs = new Map(s.subs);
      subs.set(id, { id, scope, cb });
      return { subs, fresh: { ...s.fresh, [id]: true } };
    });
    let done = false;
    return () => {
      if (done) return; // idempotent double-unsubscribe (AC-3)
      done = true;
      get().unsubscribe(id);
    };
  },

  unsubscribe: (id) => {
    set((s) => {
      if (!s.subs.has(id)) return s;
      const subs = new Map(s.subs);
      subs.delete(id);
      const fresh = { ...s.fresh };
      delete fresh[id];
      return { subs, fresh };
    });
  },

  getFresh: (id) => get().fresh[id] ?? true,

  pushSessionDelta: (delta) => {
    const matched: string[] = [];
    const resolveStorySession = get().resolveStorySession;
    for (const sub of get().subs.values()) {
      // A session sub matches by its fixed sessionId; a STORY sub (SW-GW-3)
      // matches by resolving the story's CURRENT bound session at delivery time —
      // so it follows the live session even across churn and needs no sessionId.
      // The resolve is wrapped: a throwing resolver must not abort delivery to the
      // sibling subs after it (same isolation guarantee as the `sub.cb` catch).
      let isMatch = false;
      try {
        isMatch =
          (sub.scope.kind === "session" && sub.scope.sessionId === delta.sessionId) ||
          (sub.scope.kind === "story" &&
            resolveStorySession(sub.scope.storyId) === delta.sessionId);
      } catch {
        isMatch = false;
      }
      if (isMatch) {
        matched.push(sub.id);
        try {
          // A session delta arrives via the LIVE agent listener, so it is
          // inherently fresh — deliver `fresh: true` and restore the sub's
          // freshness. Without this a session-scoped sub that went stale on
          // `visibilitychange` would latch stale forever (`recompute` skips
          // session subs, so nothing else clears it). AC-7.
          sub.cb([delta], { fresh: true });
        } catch {
          // isolate subscriber errors
        }
      }
    }
    if (matched.length > 0) {
      set((s) => {
        const fresh = { ...s.fresh };
        for (const id of matched) fresh[id] = true;
        return { fresh };
      });
    }
  },

  markStale: () => {
    set((s) => {
      const fresh = { ...s.fresh };
      for (const id of s.subs.keys()) fresh[id] = false;
      return { fresh };
    });
  },
}));
