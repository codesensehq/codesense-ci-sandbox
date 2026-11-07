// SPDX-License-Identifier: MIT
import { create } from "zustand";
import type { StatusRole, StatusState } from "@/tauri/commands";
import { DEFAULT_STATUS_STATES } from "@/shared/lib/pipeline-graph/status-config";

/**
 * SW-GT-3 · The reactive status-taxonomy snapshot (SW-GT-1's `{id,label,color,
 * icon,order,role}` states) that the switchers and board derive from.
 *
 * Seeded **synchronously at import** with `DEFAULT_STATUS_STATES` (today's 7
 * states), so the very first paint is instant and identical to the pre-taxonomy
 * behavior — never empty, no spinner, nothing on the render path awaits. The
 * async per-team hydration (`use-status-taxonomy.ts`) later calls `setStates`
 * with the loaded taxonomy; a `status-taxonomy-refresh-store` nonce forces an
 * in-session re-load (GT-6's editor).
 *
 * The store keeps the states ordered by `order` and memoizes a `Map<id, state>`
 * rebuilt ONLY on hydration, so every read — `getStatusState`, `roleOf` — is
 * O(1). Pure, non-React status functions read through the module-level getters
 * (`getState()`); React components subscribe via `useStatusTaxonomyStore`.
 */

function sortByOrder(states: StatusState[]): StatusState[] {
  // Stable sort by `order`; the loader already assigns concrete orders, but the
  // seed + defensiveness make the board's column order independent of input order.
  return [...states].sort((a, b) => a.order - b.order);
}

function indexById(states: StatusState[]): Map<string, StatusState> {
  return new Map(states.map((s) => [s.id, s]));
}

interface StatusTaxonomyState {
  /** The active taxonomy's states, ordered by `order`. */
  states: StatusState[];
  /** O(1) id → state lookup, rebuilt only when `states` changes. */
  byId: Map<string, StatusState>;
  /** Replace the taxonomy (async hydration / refresh). Re-sorts + re-indexes once. */
  setStates: (states: StatusState[]) => void;
  /** Reset to the shipped default seed — AC-8a test isolation so a suite that
   *  hydrates a custom taxonomy can't leak into later suites. */
  resetForTest: () => void;
}

export const useStatusTaxonomyStore = create<StatusTaxonomyState>((set) => ({
  states: sortByOrder(DEFAULT_STATUS_STATES),
  byId: indexById(DEFAULT_STATUS_STATES),
  setStates: (states) => {
    // A taxonomy is never legitimately empty — the loader falls back to defaults
    // (and the hook guards its load path). Defend here too so a stray empty write
    // can't strand every consumer on an undefined first-state / empty-string
    // status (`normalizeStatus` → "" matches no column → a card would vanish).
    const next = states.length > 0 ? states : DEFAULT_STATUS_STATES;
    const ordered = sortByOrder(next);
    set({ states: ordered, byId: indexById(ordered) });
  },
  resetForTest: () => {
    const ordered = sortByOrder(DEFAULT_STATUS_STATES);
    set({ states: ordered, byId: indexById(ordered) });
  },
}));

// ---------------------------------------------------------------------------
// React hooks — the subscribing seam every component reads the live taxonomy
// through, so a hydration/edit repaints. Non-React callers use the pure getters
// below instead.
// ---------------------------------------------------------------------------

/** Subscribe to the state with id `id` (or `undefined` when off-taxonomy). The
 *  one selector every status-rendering component shares, so the
 *  `byId.get(...)` boilerplate lives in one place. Pass `undefined`/`""` for a
 *  statusless surface and get `undefined` back. */
export function useStatusState(id: string | null | undefined): StatusState | undefined {
  return useStatusTaxonomyStore((s) => (id ? s.byId.get(id) : undefined));
}

/** An id → state index: the active store's, or a retained team's own taxonomy. */
export type StatusIndex = ReadonlyMap<string, StatusState>;

function currentIndex(): StatusIndex {
  return useStatusTaxonomyStore.getState().byId;
}

// ---------------------------------------------------------------------------
// Pure accessors — the seam pure (non-React) status functions read through, so
// they reflect the LIVE taxonomy without capturing a module-level const.
// ---------------------------------------------------------------------------

/** The state with id `id`, or `undefined` when off-taxonomy (soft drift). */
export function getStatusState(id: string): StatusState | undefined {
  return useStatusTaxonomyStore.getState().byId.get(id);
}

/** The role of state `id`, or `undefined` when off-taxonomy. Control flow keys
 *  off this (never a literal id), so it survives an arbitrary state set. */
export function roleOf(id: string, byId: StatusIndex = currentIndex()): StatusRole | undefined {
  return byId.get(id)?.role;
}

/** True iff `id` is a real state in the active taxonomy (not soft-drift). */
export function isKnownStatusId(id: string, byId: StatusIndex = currentIndex()): boolean {
  return byId.has(id);
}

/** The first state by `order` — the taxonomy-derived default `normalizeStatus`
 *  falls back to for null/empty/unknown input (AC-7), replacing hardcoded `todo`.
 *  `undefined` only for a (never-shipped) empty taxonomy. */
export function firstStatusId(byId?: StatusIndex): string | undefined {
  if (!byId) return useStatusTaxonomyStore.getState().states[0]?.id;
  let first: StatusState | undefined;
  for (const state of byId.values()) if (!first || state.order < first.order) first = state;
  return first?.id;
}

/** The first state (by `order`) whose role is `role` — the representative id for a
 *  role-level outcome (e.g. an epic status rollup that means "blocked"/"done"/
 *  "active"). `undefined` when the taxonomy carries no state of that role. */
export function firstStatusIdOfRole(role: StatusRole): string | undefined {
  return useStatusTaxonomyStore.getState().states.find((s) => s.role === role)?.id;
}
