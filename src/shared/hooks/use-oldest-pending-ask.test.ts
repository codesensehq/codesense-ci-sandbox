// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { renderHook } from "@testing-library/react";

import { useOldestPendingAsk } from "./use-oldest-pending-ask";
import {
  useManagedSessionStore,
  resetManagedSessionStoreForTests,
} from "@/shared/stores/use-managed-session-store";
import type { AskUserRequest } from "@/shared/lib/managed-session";

const ask = (o: Partial<AskUserRequest> = {}): AskUserRequest => ({
  correlationId: "ask-user-0",
  sessionId: "s1",
  question: "q?",
  ...o,
});

describe("useOldestPendingAsk (SW-AW-7 shared ask selection)", () => {
  beforeEach(() => resetManagedSessionStoreForTests());

  it("returns null ask + 0 count when nothing is pending for the session", () => {
    const { result } = renderHook(() => useOldestPendingAsk("s1"));
    expect(result.current).toEqual({ ask: null, pendingCount: 0 });
  });

  it("returns the OLDEST ask by monotonic correlationId and the session count", () => {
    const store = useManagedSessionStore.getState();
    // Insert out of order — selection must not depend on insertion order.
    store.addPendingAsk(ask({ correlationId: "ask-user-2", question: "third?" }));
    store.addPendingAsk(ask({ correlationId: "ask-user-0", question: "first?" }));
    store.addPendingAsk(ask({ correlationId: "ask-user-1", question: "second?" }));

    const { result } = renderHook(() => useOldestPendingAsk("s1"));
    expect(result.current.ask?.correlationId).toBe("ask-user-0");
    expect(result.current.ask?.question).toBe("first?");
    expect(result.current.pendingCount).toBe(3);
  });

  it("filters to the requested session only (ignores other sessions' asks)", () => {
    const store = useManagedSessionStore.getState();
    store.addPendingAsk(ask({ correlationId: "ask-user-0", question: "mine" }));
    store.addPendingAsk(ask({ correlationId: "ask-user-5", sessionId: "other" }));

    const { result } = renderHook(() => useOldestPendingAsk("s1"));
    expect(result.current.ask?.question).toBe("mine");
    expect(result.current.pendingCount).toBe(1);
  });

  it("orders numerically, not lexically (ask-user-10 is newer than ask-user-2)", () => {
    const store = useManagedSessionStore.getState();
    store.addPendingAsk(ask({ correlationId: "ask-user-10", question: "tenth?" }));
    store.addPendingAsk(ask({ correlationId: "ask-user-2", question: "second?" }));

    const { result } = renderHook(() => useOldestPendingAsk("s1"));
    expect(result.current.ask?.question).toBe("second?");
  });
});
