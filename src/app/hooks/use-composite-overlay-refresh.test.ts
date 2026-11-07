// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { renderHook } from "@testing-library/react";

import {
  shouldRefreshCompositeOverlay,
  useCompositeOverlayRefresh,
} from "./use-composite-overlay-refresh";
import {
  SYNC_WAVE_COMPLETED_EVENT,
  CONTEXT_FILE_CHANGE_EVENT,
  type WaveReasonView,
  type SyncWaveCompletedPayload,
} from "@/tauri/events";
import { useCompositeRefreshStore } from "@/shared/stores/composite-refresh-store";

// Controllable `listen` registry so the test can emit events.
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
      return () => set!.delete(handler);
    }),
  };
});
vi.mock("@tauri-apps/api/event", () => ({ listen: eventMock.listen }));

// Active team is fixed for the listener tests; parseTeamKey stays real.
vi.mock("@/shared/lib/team-key", async () => {
  const actual =
    await vi.importActual<typeof import("@/shared/lib/team-key")>("@/shared/lib/team-key");
  return { ...actual, getActiveTeamKey: vi.fn(() => "alderco:default") };
});

const TEAM_KEY = "alderco:default";
const counts = (written = 0): SyncWaveCompletedPayload["materialize_counts"] => ({
  written,
  skipped: 0,
  unmaterialized: 0,
});

function emit(event: string, payload: unknown) {
  const handlers = eventMock.listeners.get(event);
  if (!handlers) return;
  for (const h of [...handlers]) h({ payload });
}

function emitWave(reason: WaveReasonView, opts: { written?: number; cancelled?: boolean } = {}) {
  emit(SYNC_WAVE_COMPLETED_EVENT, {
    wave_id: 1,
    reason,
    affected_repos: [],
    materialize_counts: counts(opts.written ?? 0),
    errors: [],
    duration_ms: 0,
    cancelled: opts.cancelled ?? false,
  });
}

describe("shouldRefreshCompositeOverlay (SLW-NC-5 AC-1 reason filter)", () => {
  it("MigrationCutover → refresh", () => {
    expect(shouldRefreshCompositeOverlay({ kind: "MigrationCutover" }, counts(), false)).toBe(true);
  });

  it("cancelled wave → skip even when it would otherwise refresh", () => {
    expect(shouldRefreshCompositeOverlay({ kind: "MigrationCutover" }, counts(9), true)).toBe(
      false,
    );
  });

  it("unknown/future reason → refresh iff it wrote to disk (bias to freshness)", () => {
    const unknown = { kind: "SomethingNew" } as unknown as WaveReasonView;
    expect(shouldRefreshCompositeOverlay(unknown, counts(0), false)).toBe(false);
    expect(shouldRefreshCompositeOverlay(unknown, counts(4), false)).toBe(true);
  });
});

describe("useCompositeOverlayRefresh (SLW-NC-5 listener)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    eventMock.listeners.clear();
    useCompositeRefreshStore.setState({ nonces: {} });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("AC-1: a relevant wave bumps the active team's nonce (leading edge — SW-SY-4)", async () => {
    renderHook(() => useCompositeOverlayRefresh());
    await vi.runOnlyPendingTimersAsync(); // let listen() promises resolve

    emitWave({ kind: "MigrationCutover" });
    // SW-SY-4 (AC-2): leading edge — the first event bumps immediately so the
    // nav re-resolves without waiting out the window (was a trailing debounce).
    expect(useCompositeRefreshStore.getState().nonces[TEAM_KEY]).toBe(1);
    await vi.advanceTimersByTimeAsync(150);
    // No pending trailing payload after a lone event → the window closes with no
    // extra bump.
    expect(useCompositeRefreshStore.getState().nonces[TEAM_KEY]).toBe(1);
  });

  it("AC-2: a burst of waves coalesces into a bounded number of bumps (SW-SY-4 leading throttle)", async () => {
    renderHook(() => useCompositeOverlayRefresh());
    await vi.runOnlyPendingTimersAsync();

    for (let i = 0; i < 5; i++) {
      emitWave({ kind: "MigrationCutover" });
    }
    // Leading edge fires the first event synchronously; the remaining 4 coalesce
    // into ONE trailing flush — bounded (2), NOT one-per-event and NOT starved.
    expect(useCompositeRefreshStore.getState().nonces[TEAM_KEY]).toBe(1);
    await vi.advanceTimersByTimeAsync(150);
    expect(useCompositeRefreshStore.getState().nonces[TEAM_KEY]).toBe(2);
    // Quiet after the burst — the window closes, no runaway timer.
    await vi.advanceTimersByTimeAsync(150);
    expect(useCompositeRefreshStore.getState().nonces[TEAM_KEY]).toBe(2);
  });

  it("F3: a count-independent reason (MigrationCutover) bumps even with absent materialize_counts", async () => {
    renderHook(() => useCompositeOverlayRefresh());
    await vi.runOnlyPendingTimersAsync();

    // Emit a relevant wave with NO materialize_counts field at all.
    emit(SYNC_WAVE_COMPLETED_EVENT, {
      wave_id: 2,
      reason: { kind: "MigrationCutover" },
      affected_repos: [],
      errors: [],
      duration_ms: 0,
      cancelled: false,
    });
    await vi.advanceTimersByTimeAsync(150);
    expect(useCompositeRefreshStore.getState().nonces[TEAM_KEY]).toBe(1);
  });

  it("Wholeness: a context-file-change bumps the active team (covers raw/agent writes)", async () => {
    renderHook(() => useCompositeOverlayRefresh());
    await vi.runOnlyPendingTimersAsync();

    emit(CONTEXT_FILE_CHANGE_EVENT, {
      repo_path: "/ws",
      context_repo_path: "/ctx",
      file_path: "/ctx/a.md",
    });
    await vi.advanceTimersByTimeAsync(150);
    expect(useCompositeRefreshStore.getState().nonces[TEAM_KEY]).toBe(1);
  });

  it("unlistens on unmount (no bump after teardown)", async () => {
    const { unmount } = renderHook(() => useCompositeOverlayRefresh());
    await vi.runOnlyPendingTimersAsync();
    unmount();

    emitWave({ kind: "MigrationCutover" });
    await vi.advanceTimersByTimeAsync(150);
    expect(useCompositeRefreshStore.getState().nonces[TEAM_KEY] ?? 0).toBe(0);
  });
});
