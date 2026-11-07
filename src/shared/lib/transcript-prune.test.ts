// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

import {
  scheduleTranscriptPruneOnDeath,
  beginViewingSession,
  endViewingSession,
  resetTranscriptPruneForTests,
  TRANSCRIPT_PRUNE_GRACE_MS_FOR_TESTS,
} from "./transcript-prune";
import {
  useAgentChatStore,
  resetAgentChatStoreForTests,
} from "@/shared/stores/use-agent-chat-store";
import {
  useManagedSessionStore,
  resetManagedSessionStoreForTests,
} from "@/shared/stores/use-managed-session-store";

const GRACE = TRANSCRIPT_PRUNE_GRACE_MS_FOR_TESTS;

/** A live, running session with a transcript. */
function seedLive(sessionId: string): void {
  useAgentChatStore.getState().appendMessage(sessionId, "agent", "hello");
  useManagedSessionStore.getState().markRunning(sessionId);
}
/** Simulate the real `pty-exit` sequence: handleSessionDeath (running →
 *  needs-attention) then arm the deferred prune — mirrors `receiveSessionDeath`. */
function die(sessionId: string): void {
  useManagedSessionStore.getState().handleSessionDeath(sessionId);
  scheduleTranscriptPruneOnDeath(sessionId);
}
const hasTranscript = (s: string) => s in useAgentChatStore.getState().transcripts;
const hasState = (s: string) => s in useManagedSessionStore.getState().states;

describe("RF-perf-15 (F-5/OQ-2): deferred transcript prune", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetTranscriptPruneForTests();
    resetAgentChatStoreForTests();
    resetManagedSessionStoreForTests();
  });
  afterEach(() => vi.useRealTimers());

  it("prunes transcript + managed state after the grace window on death", () => {
    seedLive("s1");
    die("s1");

    // Retained inside the grace window — a user may still be reading it.
    vi.advanceTimersByTime(GRACE - 1);
    expect(hasTranscript("s1")).toBe(true);
    expect(hasState("s1")).toBe(true);

    // Reclaimed once the window elapses.
    vi.advanceTimersByTime(1);
    expect(hasTranscript("s1")).toBe(false);
    expect(hasState("s1")).toBe(false);
  });

  it("suppresses the prune while a surface is viewing the session", () => {
    seedLive("s1");
    die("s1");
    beginViewingSession("s1"); // surface mounted (bring-to-foreground)

    vi.advanceTimersByTime(GRACE * 2);
    expect(hasTranscript("s1")).toBe(true);
    expect(hasState("s1")).toBe(true);
  });

  it("arms the prune when the last viewer of a dead session leaves", () => {
    seedLive("s1");
    die("s1");
    beginViewingSession("s1"); // viewing
    endViewingSession("s1"); // closed the view

    vi.advanceTimersByTime(GRACE);
    expect(hasTranscript("s1")).toBe(false);
    expect(hasState("s1")).toBe(false);
  });

  it("does NOT prune when a session dies WHILE its surface is open, until the viewer leaves", () => {
    seedLive("s1");
    beginViewingSession("s1"); // user opens the live session's chat
    die("s1"); // session dies while the surface is still mounted

    // Still visible — never yanked out from under the reader.
    vi.advanceTimersByTime(GRACE * 2);
    expect(hasTranscript("s1")).toBe(true);
    expect(hasState("s1")).toBe(true);

    // Only once the user closes the view does the grace window start.
    endViewingSession("s1");
    vi.advanceTimersByTime(GRACE);
    expect(hasTranscript("s1")).toBe(false);
    expect(hasState("s1")).toBe(false);
  });

  it("ref-counts viewers — prune waits for the LAST surface to close", () => {
    seedLive("s1");
    die("s1");
    beginViewingSession("s1");
    beginViewingSession("s1"); // two surfaces on the same session

    endViewingSession("s1"); // one closes — still viewed
    vi.advanceTimersByTime(GRACE);
    expect(hasTranscript("s1")).toBe(true);

    endViewingSession("s1"); // last closes — now armed
    vi.advanceTimersByTime(GRACE);
    expect(hasTranscript("s1")).toBe(false);
  });

  it("ignores an unbalanced endViewing (no negative count, no spurious arming)", () => {
    seedLive("s1");
    die("s1"); // dead, no viewer → armed on death
    // A spurious end with no matching begin (e.g. double cleanup) must be a no-op:
    // it must not cancel the armed prune nor push the count negative.
    endViewingSession("s1");

    vi.advanceTimersByTime(GRACE);
    expect(hasTranscript("s1")).toBe(false); // still pruned as scheduled on death
  });

  it("never prunes a LIVE session (end-viewing is a no-op without a death)", () => {
    seedLive("s1");
    beginViewingSession("s1");
    // No death — the surface for a live agent just tabs away.
    endViewingSession("s1");

    vi.advanceTimersByTime(GRACE * 2);
    expect(hasTranscript("s1")).toBe(true);
    expect(hasState("s1")).toBe(true);
  });

  it("self-heals the dead-mark on fire (no re-prune after)", () => {
    seedLive("s1");
    die("s1");
    vi.advanceTimersByTime(GRACE);
    expect(hasTranscript("s1")).toBe(false);

    // Dead-mark cleared on fire → a later end-viewing for the same (now-gone) id
    // is a no-op; a late-flush transcript is NOT pruned by a stale mark.
    useAgentChatStore.getState().appendMessage("s1", "agent", "late flush");
    endViewingSession("s1");
    vi.advanceTimersByTime(GRACE);
    expect(hasTranscript("s1")).toBe(true);
  });

  it("prunes only the target session, not its neighbors", () => {
    seedLive("s1");
    seedLive("s2");
    die("s1");

    vi.advanceTimersByTime(GRACE);
    expect(hasTranscript("s1")).toBe(false);
    expect(hasTranscript("s2")).toBe(true);
    expect(hasState("s2")).toBe(true);
  });
});
