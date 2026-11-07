// SPDX-License-Identifier: MIT
import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { readPendingJoinUrl, setPendingJoinUrl } from "@/shared/lib/pending-join";
import { resetFirstRunStoreForTest, useFirstRunStore } from "@/shared/stores/first-run-store";

const { mockOpenWorkspaces } = vi.hoisted(() => ({ mockOpenWorkspaces: vi.fn() }));
vi.mock("@/features/org/lib/open-workspaces", () => ({
  openWorkspaces: mockOpenWorkspaces,
}));

import { usePendingJoinLanding } from "./use-pending-join-landing";

beforeEach(() => {
  globalThis.localStorage?.clear();
  resetFirstRunStoreForTest();
  mockOpenWorkspaces.mockClear();
});

describe("usePendingJoinLanding (SW-FU-B6)", () => {
  it("opens the join form once the app is ready after sign-in, keeping the URL", () => {
    useFirstRunStore.setState({ guest: false, joinPending: true });
    setPendingJoinUrl("https://github.com/acme/team");
    const { rerender } = renderHook(({ ready }) => usePendingJoinLanding(ready), {
      initialProps: { ready: false },
    });
    // Still signing in or discovering: nothing to host the form yet.
    expect(mockOpenWorkspaces).not.toHaveBeenCalled();

    rerender({ ready: true });
    expect(mockOpenWorkspaces).toHaveBeenCalledTimes(1);
    // A landing the user did not ask for keeps full-screen CLI on.
    expect(mockOpenWorkspaces).toHaveBeenCalledWith("join", { keepLayout: true });
    expect(useFirstRunStore.getState().joinPending).toBe(false);
    expect(readPendingJoinUrl()).toBe("https://github.com/acme/team");

    // It opens once, not again on later renders.
    rerender({ ready: false });
    rerender({ ready: true });
    expect(mockOpenWorkspaces).toHaveBeenCalledTimes(1);
  });

  it("does nothing without a pending join, and drops a URL left from a closed app", () => {
    setPendingJoinUrl("https://github.com/acme/team");
    renderHook(() => usePendingJoinLanding(true));
    expect(mockOpenWorkspaces).not.toHaveBeenCalled();
    expect(readPendingJoinUrl()).toBeNull();
  });
});
