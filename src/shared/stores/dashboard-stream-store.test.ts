// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { StatusCallback, StatusDelta } from "@/shared/lib/dashboard-stream/types";
import { useDashboardStreamStore } from "./dashboard-stream-store";

/** Last delta batch a spy callback received. */
function lastDeltas(cb: ReturnType<typeof vi.fn>): StatusDelta[] {
  return cb.mock.calls.at(-1)?.[0] ?? [];
}

beforeEach(() => {
  useDashboardStreamStore.setState({
    subs: new Map(),
    fresh: {},
    resolveStorySession: () => null,
  });
});

describe("dashboard-stream-store", () => {
  it("subscribe → unsubscribe is idempotent and removes the entry (AC-3/9)", () => {
    const store = useDashboardStreamStore.getState();
    const off = store.subscribe({ kind: "session", sessionId: "s1" }, vi.fn());
    expect(useDashboardStreamStore.getState().subs.size).toBe(1);
    off();
    expect(useDashboardStreamStore.getState().subs.size).toBe(0);
    expect(() => off()).not.toThrow(); // double-unsubscribe is a no-op
    expect(useDashboardStreamStore.getState().subs.size).toBe(0);
  });

  it("session subs receive session deltas (needs-attention preserved), not doc deltas (AC-5)", () => {
    const cb = vi.fn<StatusCallback>();
    useDashboardStreamStore.getState().subscribe({ kind: "session", sessionId: "s1" }, cb);
    // A matching session delta does — needs-attention carried verbatim.
    useDashboardStreamStore.getState().pushSessionDelta({
      origin: "session",
      kind: "session",
      sessionId: "s1",
      status: "needs-attention",
    });
    expect(cb).toHaveBeenCalledTimes(1);
    const [delta] = lastDeltas(cb);
    expect(delta).toMatchObject({ origin: "session", kind: "session", status: "needs-attention" });
  });

  it("a session delta for a different session is not delivered", () => {
    const cb = vi.fn<StatusCallback>();
    useDashboardStreamStore.getState().subscribe({ kind: "session", sessionId: "s1" }, cb);
    useDashboardStreamStore.getState().pushSessionDelta({
      origin: "session",
      kind: "session",
      sessionId: "s2",
      status: "success",
    });
    expect(cb).not.toHaveBeenCalled();
  });

  it("a stale session sub recovers to fresh on a live session delta (AC-7)", () => {
    const cb = vi.fn<StatusCallback>();
    useDashboardStreamStore.getState().subscribe({ kind: "session", sessionId: "s1" }, cb);
    const subId = [...useDashboardStreamStore.getState().subs.keys()][0];
    // Go stale (webview hidden). Only a session delta restores freshness.
    useDashboardStreamStore.getState().markStale();
    expect(useDashboardStreamStore.getState().getFresh(subId)).toBe(false);
    useDashboardStreamStore.getState().pushSessionDelta({
      origin: "session",
      kind: "session",
      sessionId: "s1",
      status: "success",
    });
    // Delivered fresh, and the sub is no longer latched stale.
    expect(cb.mock.calls[0][1]).toEqual({ fresh: true });
    expect(useDashboardStreamStore.getState().getFresh(subId)).toBe(true);
  });

  it("concurrent subscriptions with different scopes each get only their own (AC-3)", () => {
    const otherCb = vi.fn<StatusCallback>();
    const sessCb = vi.fn<StatusCallback>();
    useDashboardStreamStore.getState().subscribe({ kind: "session", sessionId: "s2" }, otherCb);
    useDashboardStreamStore.getState().subscribe({ kind: "session", sessionId: "s1" }, sessCb);
    useDashboardStreamStore.getState().pushSessionDelta({
      origin: "session",
      kind: "session",
      sessionId: "s1",
      status: "success",
    });
    expect(sessCb).toHaveBeenCalledTimes(1);
    expect(otherCb).not.toHaveBeenCalled();
  });

  // ── SW-GW-3: story-scoped subscription (AC-3/AC-4) ──────────────────────────
  it("story sub receives a session delta for the story's CURRENT bound session (AC-3/4)", () => {
    // The trusted resolver maps GW-3 → its live session s9 (viewer-store, app-root).
    useDashboardStreamStore.setState({
      resolveStorySession: (id) => (id === "GW-3" ? "s9" : null),
    });
    const cb = vi.fn<StatusCallback>();
    useDashboardStreamStore.getState().subscribe({ kind: "story", storyId: "GW-3" }, cb);
    // The terminal `done` delta for that session reaches the story sub — without
    // the board ever supplying a sessionId.
    useDashboardStreamStore.getState().pushSessionDelta({
      origin: "session",
      kind: "session",
      sessionId: "s9",
      status: "success",
    });
    expect(cb).toHaveBeenCalledTimes(1);
    expect(lastDeltas(cb)[0]).toMatchObject({ kind: "session", status: "success" });
  });

  it("story sub receives a SW-GW-4 progress delta for its current session", () => {
    useDashboardStreamStore.setState({
      resolveStorySession: (id) => (id === "GW-3" ? "s9" : null),
    });
    const cb = vi.fn<StatusCallback>();
    useDashboardStreamStore.getState().subscribe({ kind: "story", storyId: "GW-3" }, cb);
    useDashboardStreamStore.getState().pushSessionDelta({
      origin: "session",
      kind: "progress",
      sessionId: "s9",
      phase: "dev",
      step: 2,
      total: 5,
      note: "running tests",
    });
    expect(cb).toHaveBeenCalledTimes(1);
    expect(lastDeltas(cb)[0]).toMatchObject({
      kind: "progress",
      phase: "dev",
      step: 2,
      total: 5,
      note: "running tests",
    });
  });

  it("story sub ignores a progress delta for a different session", () => {
    useDashboardStreamStore.setState({
      resolveStorySession: (id) => (id === "GW-3" ? "s9" : null),
    });
    const cb = vi.fn<StatusCallback>();
    useDashboardStreamStore.getState().subscribe({ kind: "story", storyId: "GW-3" }, cb);
    useDashboardStreamStore.getState().pushSessionDelta({
      origin: "session",
      kind: "progress",
      sessionId: "s-other",
      phase: "review",
    });
    expect(cb).not.toHaveBeenCalled();
  });

  it("story sub ignores a delta for a session that is not the story's current one", () => {
    useDashboardStreamStore.setState({
      resolveStorySession: (id) => (id === "GW-3" ? "s9" : null),
    });
    const cb = vi.fn<StatusCallback>();
    useDashboardStreamStore.getState().subscribe({ kind: "story", storyId: "GW-3" }, cb);
    useDashboardStreamStore.getState().pushSessionDelta({
      origin: "session",
      kind: "session",
      sessionId: "s-other",
      status: "failed",
    });
    expect(cb).not.toHaveBeenCalled();
  });

  it("story sub with no open workspace (resolver → null) never matches", () => {
    // Default resolver returns null (no story open) — a not-started story row.
    const cb = vi.fn<StatusCallback>();
    useDashboardStreamStore.getState().subscribe({ kind: "story", storyId: "GW-3" }, cb);
    useDashboardStreamStore.getState().pushSessionDelta({
      origin: "session",
      kind: "session",
      sessionId: "s9",
      status: "success",
    });
    expect(cb).not.toHaveBeenCalled();
  });

  it("story sub follows session churn — re-resolved at each delta", () => {
    let current: string | null = "s-old";
    useDashboardStreamStore.setState({ resolveStorySession: () => current });
    const cb = vi.fn<StatusCallback>();
    useDashboardStreamStore.getState().subscribe({ kind: "story", storyId: "GW-3" }, cb);
    // First session ends.
    useDashboardStreamStore.getState().pushSessionDelta({
      origin: "session",
      kind: "session",
      sessionId: "s-old",
      status: "success",
    });
    // The story's workspace re-opens under a NEW session; the sub follows it with
    // no re-subscribe (resolution happens at delivery time).
    current = "s-new";
    useDashboardStreamStore.getState().pushSessionDelta({
      origin: "session",
      kind: "session",
      sessionId: "s-new",
      status: "needs-attention",
    });
    expect(cb).toHaveBeenCalledTimes(2);
    expect(lastDeltas(cb)[0]).toMatchObject({ status: "needs-attention" });
  });

  it("a THROWING story resolver is isolated — sibling subs still receive the delta", () => {
    // Review r1 (edge): the resolve must not abort delivery to the subs after it.
    useDashboardStreamStore.setState({
      resolveStorySession: () => {
        throw new Error("resolver blew up");
      },
    });
    const storyCb = vi.fn<StatusCallback>();
    const sessCb = vi.fn<StatusCallback>();
    // Register the throwing story sub FIRST, then a plain session sub after it.
    useDashboardStreamStore.getState().subscribe({ kind: "story", storyId: "GW-3" }, storyCb);
    useDashboardStreamStore.getState().subscribe({ kind: "session", sessionId: "s1" }, sessCb);
    expect(() =>
      useDashboardStreamStore.getState().pushSessionDelta({
        origin: "session",
        kind: "session",
        sessionId: "s1",
        status: "success",
      }),
    ).not.toThrow();
    expect(storyCb).not.toHaveBeenCalled(); // its resolve threw → no match
    expect(sessCb).toHaveBeenCalledTimes(1); // the sibling still got its delta
  });

  it("unsubscribe stops further callbacks", () => {
    const cb = vi.fn<StatusCallback>();
    const off = useDashboardStreamStore
      .getState()
      .subscribe({ kind: "session", sessionId: "s1" }, cb);
    const push = () =>
      useDashboardStreamStore.getState().pushSessionDelta({
        origin: "session",
        kind: "session",
        sessionId: "s1",
        status: "success",
      });
    push();
    expect(cb).toHaveBeenCalledTimes(1);
    off();
    push();
    expect(cb).toHaveBeenCalledTimes(1); // no further calls
  });
});
