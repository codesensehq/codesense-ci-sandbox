// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { renderHook } from "@testing-library/react";

import { useSyncWaveRescan } from "./use-sync-wave-rescan";
import { SYNC_WAVE_COMPLETED_EVENT } from "@/tauri/events";

// Controllable `listen` registry so the test can emit the event.
const eventMock = vi.hoisted(() => {
  const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();
  return {
    listeners,
    listen: vi.fn(async (event: string, handler: (e: { payload: unknown }) => void) => {
      let set = listeners.get(event);
      if (!set) {
        set = new Set();
        listeners.set(event, set);
      }
      set.add(handler);
      return () => {
        set!.delete(handler);
      };
    }),
  };
});
vi.mock("@tauri-apps/api/event", () => ({ listen: eventMock.listen }));

const bumpMock = vi.hoisted(() => vi.fn());
vi.mock("@/shared/lib/refresh-for-source-repo", () => ({
  bumpFileRefreshForSourceRepo: bumpMock,
}));

// SW-NR-1: the team-nav routing helpers are unit-tested directly in
// reverse-sync-nav-refresh.test.ts; here we assert the hook ROUTES to them for
// the right wave shapes (and not for the forward path).
const teamNavWorktreesMock = vi.hoisted(() => vi.fn());
vi.mock("@/shared/lib/reverse-sync-nav-refresh", () => ({
  bumpTeamNavForWorktrees: teamNavWorktreesMock,
}));

interface CompletedOverrides {
  affected_repos?: string[];
  written?: number;
  reasonKind?: string;
}

function emitCompleted({
  affected_repos = [],
  written = 1,
  reasonKind = "MigrationCutover",
}: CompletedOverrides) {
  const handlers = eventMock.listeners.get(SYNC_WAVE_COMPLETED_EVENT);
  if (!handlers) return;
  for (const h of [...handlers]) {
    h({
      payload: {
        wave_id: 1,
        reason: { kind: reasonKind },
        affected_repos,
        materialize_counts: { written, skipped: 0, unmaterialized: 0 },
        errors: [],
        duration_ms: 0,
        cancelled: false,
      },
    });
  }
}

describe("useSyncWaveRescan", () => {
  beforeEach(() => {
    bumpMock.mockClear();
    teamNavWorktreesMock.mockClear();
    eventMock.listeners.clear();
  });

  it("AC-10: bumps file-refresh for each affected repo when a materialize wave wrote files", () => {
    renderHook(() => useSyncWaveRescan());

    // Broad refresh for each repo.
    emitCompleted({ affected_repos: ["/repos/a", "/repos/b"], written: 3 });

    expect(bumpMock).toHaveBeenCalledTimes(2);
    expect(bumpMock).toHaveBeenCalledWith("/repos/a", undefined);
    expect(bumpMock).toHaveBeenCalledWith("/repos/b", undefined);
  });

  it("SW-XS-4: a wave broad-refreshes each repo and its team nav (no per-path skip)", () => {
    renderHook(() => useSyncWaveRescan());

    emitCompleted({
      affected_repos: ["/wt/feat-x", "/wt/feat-y"],
      written: 4,
      reasonKind: "MigrationCutover",
    });

    expect(bumpMock).toHaveBeenCalledWith("/wt/feat-x", undefined);
    expect(bumpMock).toHaveBeenCalledWith("/wt/feat-y", undefined);
    expect(teamNavWorktreesMock).toHaveBeenCalledTimes(1);
    expect(teamNavWorktreesMock).toHaveBeenCalledWith(["/wt/feat-x", "/wt/feat-y"]);
  });

  it("AC-16: no-op wave (written == 0) does not bump — no rescan storm", () => {
    renderHook(() => useSyncWaveRescan());

    emitCompleted({ affected_repos: ["/repos/a"], written: 0 });

    expect(bumpMock).not.toHaveBeenCalled();
    expect(teamNavWorktreesMock).not.toHaveBeenCalled();
  });

  it("empty affected_repos is a no-op even when files were written", () => {
    renderHook(() => useSyncWaveRescan());

    emitCompleted({ affected_repos: [], written: 5 });

    expect(bumpMock).not.toHaveBeenCalled();
  });

  it("unlistens on unmount (no bump after teardown)", async () => {
    const { unmount } = renderHook(() => useSyncWaveRescan());
    // Let the async `listen(...)` promise resolve so the unlisten fn is wired.
    await Promise.resolve();
    unmount();
    await Promise.resolve();

    emitCompleted({ affected_repos: ["/repos/a"], written: 1 });

    expect(bumpMock).not.toHaveBeenCalled();
  });
});
