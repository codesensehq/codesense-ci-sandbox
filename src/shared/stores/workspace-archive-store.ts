// SPDX-License-Identifier: MIT
import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";

import { listWorkspaceArchiveStates, type WorkspaceArchiveState } from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";

/**
 * SW-WC-1 — per-workspace archive-flag mirror of
 * `~/.codesense/workspace-registry.json`.
 *
 * The Rust registry owns the on-disk truth; this store caches whatever the FE
 * needs to render archive-aware affordances (badges, locked chat compose) in
 * one spot so a hundred components don't each invoke the Tauri command.
 *
 * `null` ↔ "active" (either explicitly unarchived or never touched); any
 * numeric value is the epoch-ms the operator archived the workspace. The
 * registry keeps unarchived entries so the FE can tell "I toggled this
 * recently" from "I've never heard of it", but every FE consumer treats
 * `archivedAtMs == null` the same.
 */

type ArchiveMap = Readonly<Record<string, number | null>>;

export interface WorkspaceArchiveStoreState {
  /** Keyed by viewer workspace id (`root:…` / `story:…` / `generic:…`). */
  states: ArchiveMap;
  /** True once the first disk read completes (even if empty). FE guards reads
   *  with this so a cold start never shows a stale "active" look for a
   *  genuinely-archived workspace while the registry is still loading. */
  loaded: boolean;
  /** Replace the whole map (used by `primeFromDisk`). */
  setAll: (next: ArchiveMap) => void;
  /** Merge one workspace's state in. The event payload is a single row. */
  applyChange: (state: WorkspaceArchiveState) => void;
  /** Load the current registry contents and subscribe to change events. The
   *  returned function cancels the subscription. */
  initialize: () => Promise<() => void>;
}

/** Pure merge — exported for tests. `null` is a real "active" value, so we
 *  keep the key around (the registry already does on disk). */
export function mergeArchiveState(current: ArchiveMap, state: WorkspaceArchiveState): ArchiveMap {
  if (current[state.workspaceId] === state.archivedAtMs) return current;
  return { ...current, [state.workspaceId]: state.archivedAtMs };
}

/** Pure "is archived" check — the one place the FE encodes the rule. */
export function isWorkspaceArchived(archivedAtMs: number | null | undefined): boolean {
  return typeof archivedAtMs === "number";
}

export const useWorkspaceArchiveStore = create<WorkspaceArchiveStoreState>((set, get) => ({
  states: Object.freeze({}),
  loaded: false,
  setAll: (next) => set({ states: Object.freeze({ ...next }), loaded: true }),
  applyChange: (state) => set({ states: Object.freeze(mergeArchiveState(get().states, state)) }),
  async initialize() {
    try {
      const rows = await listWorkspaceArchiveStates();
      const next: Record<string, number | null> = {};
      for (const row of rows) {
        next[row.workspaceId] = row.archivedAtMs;
      }
      get().setAll(next);
    } catch (error) {
      // Fail-open: an unreadable registry leaves the FE showing every
      // workspace as active (the pre-SW-WC-1 behaviour). Logged, never a
      // toast — this is background priming, not a user action.
      logger.warn("workspace-archive", `failed to load workspace archive registry: ${error}`);
      set({ loaded: true });
    }
    const unlisten = await listen<WorkspaceArchiveState>(
      "codesense:workspace-archive-changed",
      (event) => {
        if (event.payload) get().applyChange(event.payload);
      },
    );
    return unlisten;
  },
}));

/** Selector helper for the common "is this workspace archived" question. */
export function selectArchivedAtMs(
  state: WorkspaceArchiveStoreState,
  workspaceId: string | undefined,
): number | null {
  if (!workspaceId) return null;
  const value = state.states[workspaceId];
  return typeof value === "number" ? value : null;
}

/** Test seam: reset the store back to its empty, un-initialized state. */
export function resetWorkspaceArchiveStoreForTest(): void {
  useWorkspaceArchiveStore.setState({
    states: Object.freeze({}),
    loaded: false,
  });
}
