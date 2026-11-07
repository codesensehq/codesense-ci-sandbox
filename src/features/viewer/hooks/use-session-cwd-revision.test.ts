// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";

// SW-VE-14: the per-session revision counter — one shared Tauri listener on
// `pty-osc7` bumps a per-session counter so `useSessionCwd(id, revision)`
// re-reads. Covers:
//  - 0 at mount, bumps once per matching event,
//  - mismatched sessionId never bumps,
//  - sibling consumers of the same session share one bump,
//  - an undefined sessionId is inert (no listener, revision stays 0).

const h = vi.hoisted(() => ({
  handlers: new Set<(event: { payload: unknown }) => void>(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, cb: (event: { payload: unknown }) => void) => {
    h.handlers.add(cb);
    return () => h.handlers.delete(cb);
  }),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { warn: vi.fn(), info: vi.fn(), error: vi.fn(), debug: vi.fn() },
}));

import {
  __bumpSessionCwdRevisionForTest,
  resetSessionCwdRevisions,
  useSessionCwdRevision,
} from "./use-session-cwd-revision";

function emitOsc7(sessionId: string): void {
  for (const cb of [...h.handlers]) cb({ payload: { session_id: sessionId, cwd: "/x" } });
}

beforeEach(() => {
  h.handlers.clear();
  resetSessionCwdRevisions();
});
afterEach(() => {
  cleanup();
  resetSessionCwdRevisions();
});

describe("useSessionCwdRevision (SW-VE-14)", () => {
  it("reads 0 at mount and bumps once per matching pty-osc7 event", async () => {
    const { result } = renderHook(() => useSessionCwdRevision("sid-a"));
    // The shared Tauri listener is attached asynchronously (await listen()).
    await act(async () => {
      await Promise.resolve();
    });
    expect(result.current).toBe(0);
    act(() => emitOsc7("sid-a"));
    expect(result.current).toBe(1);
    act(() => emitOsc7("sid-a"));
    expect(result.current).toBe(2);
  });

  it("ignores events for a different session", async () => {
    const { result } = renderHook(() => useSessionCwdRevision("sid-a"));
    await act(async () => {
      await Promise.resolve();
    });
    act(() => emitOsc7("sid-OTHER"));
    expect(result.current).toBe(0);
    act(() => emitOsc7("sid-a"));
    expect(result.current).toBe(1);
  });

  it("fans out to sibling consumers of the same session with one bump each", async () => {
    const a = renderHook(() => useSessionCwdRevision("sid-a"));
    const b = renderHook(() => useSessionCwdRevision("sid-a"));
    await act(async () => {
      await Promise.resolve();
    });
    act(() => emitOsc7("sid-a"));
    expect(a.result.current).toBe(1);
    expect(b.result.current).toBe(1);
  });

  it("stays at 0 for an undefined sessionId and never subscribes", async () => {
    const { result } = renderHook(() => useSessionCwdRevision(undefined));
    await act(async () => {
      await Promise.resolve();
    });
    // Test seam: a direct bump should not reach the hook — the hook with an
    // undefined session never registers a listener for any id.
    __bumpSessionCwdRevisionForTest("sid-a");
    expect(result.current).toBe(0);
  });
});
