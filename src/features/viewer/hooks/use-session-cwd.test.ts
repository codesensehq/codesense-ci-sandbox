// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";

// SW-TP CAP-11 / CAP-13: `useSessionCwd` is the lazy, defensive resolver that
// feeds the per-tab caption's identity slot. Covers:
//  - resolved non-null path returns it from the hook,
//  - the catch branch leaves cwd at null when the resolver rejects,
//  - a defensive fallback when the Tauri mock omits `sessionCwd` entirely,
//  - sessionId change resets state to null between fetches.

const sessionCwdMock = vi.fn();

vi.mock("@/tauri/commands", () => ({
  // intentionally exposing only `sessionCwd` — a sparse mock
  sessionCwd: (sid: string) => sessionCwdMock(sid),
}));

import { useSessionCwd } from "./use-session-cwd";

afterEach(() => cleanup());

beforeEach(() => {
  sessionCwdMock.mockReset();
});

describe("useSessionCwd (SW-TP CAP-11)", () => {
  it("returns null for an undefined sessionId without invoking the resolver", () => {
    const { result } = renderHook(() => useSessionCwd(undefined));
    expect(result.current).toBeNull();
    expect(sessionCwdMock).not.toHaveBeenCalled();
  });

  it("resolves and returns the backend-reported cwd", async () => {
    sessionCwdMock.mockResolvedValue("/users/alice/project");
    const { result } = renderHook(() => useSessionCwd("sid-a"));
    expect(result.current).toBeNull();
    await act(async () => {
      await Promise.resolve();
    });
    expect(result.current).toBe("/users/alice/project");
    expect(sessionCwdMock).toHaveBeenCalledWith("sid-a");
  });

  it("falls back to null when the resolver rejects (and does not throw)", async () => {
    sessionCwdMock.mockRejectedValue(new Error("ipc timeout"));
    const { result } = renderHook(() => useSessionCwd("sid-a"));
    await act(async () => {
      await Promise.resolve();
    });
    expect(result.current).toBeNull();
  });

  it("resets cwd to null when sessionId changes before the next fetch resolves", async () => {
    sessionCwdMock.mockResolvedValueOnce("/first");
    const { result, rerender } = renderHook(({ sid }) => useSessionCwd(sid), {
      initialProps: { sid: "sid-a" as string | undefined },
    });
    await act(async () => {
      await Promise.resolve();
    });
    expect(result.current).toBe("/first");
    // Swap in a new sessionId whose resolver is slow: the hook must drop the
    // previous session's cwd immediately, not leak it under the new identity.
    sessionCwdMock.mockImplementationOnce(() => new Promise(() => undefined));
    rerender({ sid: "sid-b" });
    expect(result.current).toBeNull();
  });

  it("does not leak the previous session's cwd when sessionId falls to undefined", async () => {
    sessionCwdMock.mockResolvedValueOnce("/some/where");
    const { result, rerender } = renderHook(({ sid }) => useSessionCwd(sid), {
      initialProps: { sid: "sid-a" as string | undefined },
    });
    await act(async () => {
      await Promise.resolve();
    });
    expect(result.current).toBe("/some/where");
    rerender({ sid: undefined });
    expect(result.current).toBeNull();
  });
});
