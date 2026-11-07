// SPDX-License-Identifier: MIT
import { create } from "zustand";

import type { SessionOwner, ConnectionPhase } from "@/shared/components/supervised-agent-session";
import { useActiveTeamKey } from "@/shared/lib/team-key";

/**
 * SW-GS-2d (AC-9) — the generic, per-surface-KEYED agent binding store.
 *
 * One store holds the binding state (`{ boundSessionId, owner, connection }`) for
 * EVERY agent chat surface, keyed by `surfaceKey` — the workspace-INSTANCE id
 * (`targetWorkspace`), e.g. `"story:<path>"`, `"integration:<…>"`.
 * Keying by instance (not kind) is what keeps two open surfaces of the same kind
 * — story `story:A` and story `story:B` — fully isolated; binding "this story"
 * never leaks into "that story".
 *
 * This REPLACES the bespoke binding slice that lived in `use-git-sync-store`
 * (which now keeps only its `selectedBucketId`). Keyed ≠ a single global binding:
 * it removes the per-feature duplication while still scoping per surface, so a
 * story/epic surface drops in without a new store. `useAgentWorkspaceSession`
 * (the plug-in hook) is the only writer; consumers read via `selectBinding`.
 *
 * Selector discipline: read via `useAgentSessionStore(selectBinding(key))` (or a
 * narrowed field selector); never destructure the whole store.
 */
export interface SurfaceBinding {
  /** The bound agent session's PTY id; null pre-bind. The chat surface targets this. */
  boundSessionId: string | null;
  /** Who holds the keyboard. */
  owner: SessionOwner;
  /** Three-phase connection, orthogonal to `owner`. */
  connection: ConnectionPhase;
  /** AC-3a: the agent didn't ack — show the "open its terminal, then Re-bind"
   *  affordance. Per-surface like the rest of the binding (SW-AW-6): one keyed
   *  home for ALL of a surface's session state, so a single app-level hook
   *  instance driving N teams can't leak one team's recovery banner onto another. */
  recovery: boolean;
}

/** Initial per-surface state: agent owns, no live session yet (mirrors the
 *  pre-2d git-sync default so migrated behavior is identical). */
export const DEFAULT_SURFACE_BINDING: SurfaceBinding = {
  boundSessionId: null,
  owner: "agent",
  connection: "disconnected",
  recovery: false,
};

interface AgentSessionStoreState {
  /** Per-surface binding, keyed by instance `surfaceKey`. */
  bindings: Record<string, SurfaceBinding>;
  setBoundSessionId: (surfaceKey: string, id: string | null) => void;
  setOwner: (surfaceKey: string, owner: SessionOwner) => void;
  setConnection: (surfaceKey: string, connection: ConnectionPhase) => void;
  setRecovery: (surfaceKey: string, recovery: boolean) => void;
  clearSurface: (surfaceKey: string) => void;
}

/** Upsert one field of a surface's binding, defaulting absent surfaces.
 *
 *  No-op writes to an EXISTING surface return the SAME map reference so
 *  subscribers don't re-render — restores the value-dedup the per-surface
 *  recovery map had before `recovery` moved into the store (SW-AW-6), and
 *  extends it to every setter (e.g. the `false→false` recovery clear at the top
 *  of each bind attempt no longer churns a render). A brand-new surface always
 *  materializes so store shape stays predictable (`surfaceKey in bindings`). */
function patch(
  bindings: Record<string, SurfaceBinding>,
  surfaceKey: string,
  next: Partial<SurfaceBinding>,
): Record<string, SurfaceBinding> {
  const prev = bindings[surfaceKey] ?? DEFAULT_SURFACE_BINDING;
  if (surfaceKey in bindings) {
    const unchanged = (Object.keys(next) as (keyof SurfaceBinding)[]).every(
      (k) => prev[k] === next[k],
    );
    if (unchanged) return bindings;
  }
  return { ...bindings, [surfaceKey]: { ...prev, ...next } };
}

export const useAgentSessionStore = create<AgentSessionStoreState>((set) => ({
  bindings: {},
  setBoundSessionId: (surfaceKey, id) =>
    set((s) => ({ bindings: patch(s.bindings, surfaceKey, { boundSessionId: id }) })),
  setOwner: (surfaceKey, owner) =>
    set((s) => ({ bindings: patch(s.bindings, surfaceKey, { owner }) })),
  setConnection: (surfaceKey, connection) =>
    set((s) => ({ bindings: patch(s.bindings, surfaceKey, { connection }) })),
  setRecovery: (surfaceKey, recovery) =>
    set((s) => ({ bindings: patch(s.bindings, surfaceKey, { recovery }) })),
  clearSurface: (surfaceKey) =>
    set((s) => {
      if (!(surfaceKey in s.bindings)) return s;
      const next = { ...s.bindings };
      delete next[surfaceKey];
      return { bindings: next };
    }),
}));

/** Selector: the binding for `surfaceKey`, or the default when none exists yet. */
export function selectBinding(surfaceKey: string) {
  return (state: AgentSessionStoreState): SurfaceBinding =>
    state.bindings[surfaceKey] ?? DEFAULT_SURFACE_BINDING;
}

/**
 * SW-AW-6 (AC-8) — the SINGLE source of truth for an agent binding's
 * `surfaceKey`. Composes the caller's per-instance `targetWorkspace` with the
 * active `teamKey` so every binding is team-scoped BY CONSTRUCTION — a consumer
 * cannot register a global (team-agnostic) binding, and one team's session never
 * leaks into another's. Binding-store readers (`useAgentWorkspaceSession`) MUST
 * route through this — an inline second composition is how the cross-team-stale
 * bug gets re-manufactured.
 *
 * `"::"` is a safe boundary: `buildTeamKey` uses a single `:` internally, so the
 * doubled separator can't collide with a teamKey segment.
 */
export function buildAgentSurfaceKey(targetWorkspace: string, teamKey: string): string {
  return `${targetWorkspace}::${teamKey}`;
}

/**
 * Reactive companion to {@link buildAgentSurfaceKey}: the binding key for
 * `targetWorkspace` under the ACTIVE team. Reads the reactive `useActiveTeamKey()`,
 * so it re-derives on a team switch and the surface's binding re-keys WITHOUT a
 * remount — the app-level agent mounts persist across team switches (SW-AW-5), so
 * a remount can't be relied on (SW-AW-6 AC-1/AC-4).
 *
 * `teamScoped` (default true) governs the composition: a per-team agent gets
 * `${targetWorkspace}::${teamKey}` (own session per team); a USER-GLOBAL agent
 * (`teamScoped: false`, e.g. notes) gets the bare `targetWorkspace`, so a team
 * switch does NOT re-key — one session shared across teams. `useActiveTeamKey`
 * is still called unconditionally (hook rules) but ignored when global.
 */
export function useAgentBindingKey(targetWorkspace: string, teamScoped = true): string {
  const teamKey = useActiveTeamKey();
  return teamScoped ? buildAgentSurfaceKey(targetWorkspace, teamKey) : targetWorkspace;
}

/** Test-only reset. */
export function resetAgentSessionStoreForTests(): void {
  useAgentSessionStore.setState({ bindings: {} });
}
