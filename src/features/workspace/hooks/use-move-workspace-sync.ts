// SPDX-License-Identifier: MIT
/**
 * SW-WS-1: FE bridge for the Rust-side `move_workspace` pipeline.
 *
 * Durable state lives on the Rust side as a `MoveIntent` file; the FE's
 * `ws.setupState = { phase: "moving", from, to, step }` is a projection of
 * that intent for the UI. Two paths keep the projection fresh:
 *
 *  1. **Pull-on-init** — on hook mount (first render in the viewer), call
 *     `list_move_intents` MCP once to bootstrap projections for every open
 *     intent. This is the Pass-2 Flag 3 answer: a readiness race between
 *     the Rust-side boot reaper emitting `move-workspace-intent-hydrate`
 *     and the FE listener installing is impossible because the FE pulls
 *     authoritative state on init anyway.
 *
 *  2. **Live events** — once mounted, listen for:
 *     - `move-workspace-intent-hydrate` — reaper or in-flight pipeline said
 *       "here's the current intent; set `ws.setupState` and, if step is
 *       `swapping-location`/`cleaning-source`, apply the location swap"
 *     - `move-workspace-intent-cleared` — pipeline completed; drop
 *       `ws.setupState`
 *     - `move-workspace-progress` — a step-1 progress tick; update the
 *       banner's progress line
 */

import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { listMoveIntentsBridge, type MoveIntentView } from "@/tauri/commands";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { logger } from "@/shared/lib/logger";

/** Shape of `move-workspace-intent-hydrate` payload — matches the Rust-side
 *  `MoveIntentView` serde output (camelCase). */
export type MoveIntentHydratePayload = MoveIntentView;

export interface MoveIntentClearedPayload {
  teamKey: string;
  workspaceId: string;
}

export interface MoveProgressPayload {
  teamKey: string;
  workspaceId: string;
  step: "preparing-target" | "swapping-location" | "cleaning-source";
  message?: string | null;
}

const HYDRATE_EVENT = "move-workspace-intent-hydrate";
const CLEARED_EVENT = "move-workspace-intent-cleared";
const PROGRESS_EVENT = "move-workspace-progress";

/** Apply the hydrate payload to the viewer store: set `setupState` to the
 *  "moving" projection, and when the pipeline has passed step 2
 *  (`swapping-location` or beyond) ensure `ws.location` points at the target
 *  (the Rust side is the source of truth about "where the workspace lives
 *  NOW"; the FE just mirrors it). */
function applyHydrate(payload: MoveIntentHydratePayload): void {
  // Only `location` is persisted; `setupState` is runtime-only. R1 nit:
  // avoid firing `flushPersist` when the write touched only `setupState`.
  let locationChanged = false;
  useViewerStore.setState((state) => {
    const rows = state.workspaces[payload.teamKey];
    if (!rows) return state;
    const next = rows.map((w) => {
      if (w.id !== payload.workspaceId) return w;
      const setupState = {
        phase: "moving" as const,
        from: payload.from,
        to: payload.to,
        step: payload.step,
      } as unknown as typeof w.setupState;
      // If the pipeline has advanced past step 2, apply the swap on the
      // FE's `location` record too — idempotent, so re-fired events are
      // free. (`preparing-target` leaves `location` at source.)
      const shouldApplySwap =
        payload.step === "swapping-location" || payload.step === "cleaning-source";
      if (!shouldApplySwap) return { ...w, setupState };
      locationChanged = true;
      // The target may be local or a remote machine.
      if (payload.to.machineId === null) {
        // Target is local — drop the remote location record (if any).
        const { location: _location, ...rest } = w as typeof w & {
          location?: unknown;
        };
        return { ...rest, setupState };
      }
      return {
        ...w,
        setupState,
        location: {
          kind: "machine" as const,
          machineId: payload.to.machineId,
          requestId: payload.to.requestId,
          // handle + cwd will be rehydrated when the next `prepare_remote_workspace`
          // reconcile reads back the receipt; for now the id + request_id are
          // enough for `whereWorkspaceLives` to resolve correctly.
          handle: undefined,
          cwd: undefined,
        },
      };
    });
    return {
      ...state,
      workspaces: { ...state.workspaces, [payload.teamKey]: next },
    };
  });
  if (locationChanged) void useViewerStore.getState().flushPersist();
}

function applyCleared(payload: MoveIntentClearedPayload): void {
  // Clears only `setupState`, which is runtime-only — skip `flushPersist`.
  useViewerStore.setState((state) => {
    const rows = state.workspaces[payload.teamKey];
    if (!rows) return state;
    const next = rows.map((w) => {
      if (w.id !== payload.workspaceId) return w;
      if (w.setupState === undefined) return w;
      const asMoving = w.setupState as unknown as { phase?: string };
      if (asMoving?.phase !== "moving") return w;
      const { setupState: _setupState, ...rest } = w;
      return rest as typeof w;
    });
    return {
      ...state,
      workspaces: { ...state.workspaces, [payload.teamKey]: next },
    };
  });
}

function applyProgress(payload: MoveProgressPayload): void {
  useViewerStore.setState((state) => {
    const rows = state.workspaces[payload.teamKey];
    if (!rows) return state;
    const next = rows.map((w) => {
      if (w.id !== payload.workspaceId) return w;
      const current = w.setupState as unknown as {
        phase?: string;
        step?: string;
        message?: string;
      };
      if (current?.phase !== "moving") return w;
      return {
        ...w,
        setupState: {
          ...(w.setupState as object),
          step: payload.step,
          message: payload.message ?? undefined,
        } as unknown as typeof w.setupState,
      };
    });
    return {
      ...state,
      workspaces: { ...state.workspaces, [payload.teamKey]: next },
    };
  });
}

/** R1 A1 — races against a late-landing `cleared` event:
 *  bootstrap reads the disk snapshot, then applies hydrate for each row —
 *  but a `cleared` event that fires BETWEEN the snapshot read and the
 *  hydrate apply would otherwise be masked (listener's `applyCleared`
 *  fires first with no `setupState` present yet; awaited bootstrap then
 *  reinstalls stale "moving" projection; nothing clears it until a new
 *  move). Fix: listeners are installed FIRST and record every cleared
 *  `(teamKey, workspaceId)` into `clearedDuringBootstrap`; bootstrap
 *  consults that set and SKIPS keys that cleared during the race window. */
let clearedDuringBootstrap: Set<string> | null = null;

function keyOf(teamKey: string, workspaceId: string): string {
  return `${teamKey}\0${workspaceId}`;
}

/** Pull-on-init: call `list_move_intents` MCP and hydrate every open intent.
 *  Fire-and-forget; a transient failure logs and gives up (the user can
 *  re-open the dashboard to retry). */
async function bootstrapFromIntents(): Promise<void> {
  try {
    const intents = await listMoveIntentsBridge().catch(() => []);
    const cleared = clearedDuringBootstrap ?? new Set<string>();
    for (const intent of intents) {
      if (cleared.has(keyOf(intent.teamKey, intent.workspaceId))) {
        // Reaper or in-flight pipeline cleared this intent between the
        // snapshot read and now; the on-disk snapshot is stale for this
        // key. Skip the hydrate — the listener-side `applyCleared` was a
        // no-op (no setupState yet) and leaves `ws.setupState` cleanly
        // undefined. Any subsequent HYDRATE for the same key still goes
        // through the live listener.
        continue;
      }
      applyHydrate(intent);
    }
  } catch (error) {
    logger.warn("viewer", `move-workspace bootstrap failed: ${String(error)}`);
  } finally {
    // Drop the race-window set; a subsequent store load would reinstall
    // its own set.
    clearedDuringBootstrap = null;
  }
}

export function useMoveWorkspaceSync(): void {
  useEffect(() => {
    // R1 A1: register listeners FIRST so a cleared event that fires during
    // the bootstrap round-trip is captured in `clearedDuringBootstrap`.
    clearedDuringBootstrap = new Set<string>();
    const group = createListenerGroup();
    group.track(
      listen<MoveIntentHydratePayload>(HYDRATE_EVENT, ({ payload }) => {
        if (payload?.teamKey && payload.workspaceId) applyHydrate(payload);
      }),
    );
    group.track(
      listen<MoveIntentClearedPayload>(CLEARED_EVENT, ({ payload }) => {
        if (!payload?.teamKey || !payload.workspaceId) return;
        if (clearedDuringBootstrap !== null) {
          clearedDuringBootstrap.add(keyOf(payload.teamKey, payload.workspaceId));
        }
        applyCleared(payload);
      }),
    );
    group.track(
      listen<MoveProgressPayload>(PROGRESS_EVENT, ({ payload }) => {
        if (payload?.teamKey && payload.workspaceId) applyProgress(payload);
      }),
    );
    // Listeners are armed; now pull the disk snapshot. Any cleared event
    // that fires between here and `bootstrapFromIntents` resolution is
    // recorded in the set above and masked at apply time.
    void bootstrapFromIntents();
    return () => group.dispose();
  }, []);
}
