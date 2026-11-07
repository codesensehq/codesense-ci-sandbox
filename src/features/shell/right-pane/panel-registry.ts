// SPDX-License-Identifier: MIT
/**
 * SW-RP-1: the right-pane panel registry.
 *
 * A panel is `{ id, title, icon, render }` where `render` is a zero-arg closure
 * returning the panel body (the "component"). A closure — not a `FC<Props>` —
 * because the panels do NOT share a prop shape: Inbox and custom panes pull host
 * data from `RightPaneHostContext`, Notes reads its own store, and History closes
 * over its own sync state. The container never hardcodes which panels exist.
 *
 * This zustand store is the single source of truth the stack renderer AND the
 * native View → Panels menu read, so a newly-registered panel appears in both
 * with no per-panel wiring.
 */
import type { LucideIcon } from "lucide-react";
import { useEffect, useMemo, useRef, type ReactNode } from "react";
import { create } from "zustand";
import { warmScopes } from "@/shared/lib/warm-scopes";

export interface PanelDefinition {
  id: string;
  title: string;
  icon: LucideIcon;
  /** Zero-arg render closure for the panel body. */
  render: () => ReactNode;
  /**
   * SW-RP-3: optional controls rendered in the section title bar (right side,
   * before the collapse control) for the panel when it is the ACTIVE panel of
   * an expanded section. A closure — like `render` — so a consumer registering
   * from a living component (e.g. SW-GS-10's agent) closes over its own
   * session/callbacks. A panel registered at module load can only source state
   * a closure can reach from outside React render (store/context/module bus),
   * NOT component-local state (the Tiptap editor / React Flow instance) — those
   * await the portal-authored header slot (deferred). No built-in adopts this
   * yet; the first consumer is the SW-GS-10 Sync Agent panel.
   *
   * Like `render`, this closure is **called conditionally** (only for the active
   * panel of an expanded section), so it must RETURN A COMPONENT ELEMENT and keep
   * any hooks inside that child component — never call hooks in the closure body,
   * or collapsing the section changes the host's hook order and crashes.
   */
  headerActions?: () => ReactNode;
  /**
   * SW-AM-5 (AC-5): an optional presence/count indicator rendered next to the
   * panel's title in the section title bar — both when the panel is a lone
   * section title AND as a tab in a grouped strip — so a count stays visible even
   * when the section is collapsed (the title bar renders regardless of collapse).
   * Same closure contract as `headerActions`: RETURN A COMPONENT ELEMENT, keep any
   * hooks inside that child (the Agents badge reads its count from the app-root
   * roster store — SW-AM-9 — the same source the panel rows read). Returning `null`
   * renders nothing.
   */
  badge?: () => ReactNode;
  /**
   * SW-AU-38 (AC-0): whether this panel is OFFERED in the add-panel (View →
   * Panels) menu. `undefined`/`true` ⇒ offered (the default). `false` ⇒ kept
   * REGISTERED (so a persisted layout referencing it still renders its real body
   * — never a dead ghost section) but hidden from the menu, so a retired surface
   * can't be re-added while its saved instances keep working. Filtered in
   * `listedPanels` (the single basis shared by the menu entries AND the
   * rebuild signature), NOT in placement paths — a programmatic `revealPanel`
   * (e.g. the workflow-editor overlay) still opens a non-offerable panel.
   */
  offerable?: boolean;
  /**
   * SW-WG-9: whether this panel offers the generic in-app maximize (full-viewport
   * overlay). When set, the host section header renders ONE generic maximize button
   * for the active, expanded panel; the panel body wraps itself in
   * `PanelMaximizeBoundary` (which owns the overlay + ✕/Esc restore). `undefined`/
   * `false` ⇒ no maximize control (the default).
   */
  maximizable?: boolean;
}

interface PanelRegistryState {
  /** What the active scope shows: the unscoped panels plus the active scope's own. */
  panels: Record<string, PanelDefinition>;
  /** Panels registered without a scope (built-ins, agent modules), seen by every scope. */
  shared: Record<string, PanelDefinition>;
  /** Each team's own panels, kept registered while the team is warm so its hidden pane keeps its
   *  bodies (and so its frames) while another team is on screen. */
  scoped: Record<string, Record<string, PanelDefinition>>;
  activeScope: string | null;
  /** The scope whose panels `panels` shows: the active scope's once it has settled, else the
   *  previous one's, so a switch to a team still listing never empties the registry. */
  viewScope: string | null;
  registerPanel: (def: PanelDefinition, scope?: string) => void;
  unregisterPanel: (id: string, scope?: string) => void;
  setActiveScope: (scope: string | null) => void;
  /** The active scope's panels are all registered: `panels` shows them from now on. */
  settleScope: (scope: string) => void;
  dropScope: (scope: string) => void;
}

type Slice = Pick<PanelRegistryState, "shared" | "scoped" | "viewScope" | "activeScope">;

const view = (s: Slice): Record<string, PanelDefinition> =>
  s.viewScope && s.scoped[s.viewScope] ? { ...s.shared, ...s.scoped[s.viewScope] } : s.shared;

/** The scopes whose panels are dropped when they leave the warm tier (one callback per scope). */
const watched = new Set<string>();

export const usePanelRegistry = create<PanelRegistryState>((set, get) => {
  const apply = (next: Slice) => set({ ...next, panels: view(next) });
  return {
    panels: {},
    shared: {},
    scoped: {},
    activeScope: null,
    viewScope: null,
    registerPanel: (def, scope) => {
      const s = get();
      if (!scope) return apply({ ...s, shared: { ...s.shared, [def.id]: def } });
      if (!watched.has(scope)) {
        watched.add(scope);
        warmScopes.onTier(scope, "warm", () => {
          watched.delete(scope);
          get().dropScope(scope);
        });
      }
      apply({ ...s, scoped: { ...s.scoped, [scope]: { ...s.scoped[scope], [def.id]: def } } });
    },
    unregisterPanel: (id, scope) => {
      const s = get();
      if (!scope) {
        if (!(id in s.shared)) return;
        const shared = { ...s.shared };
        delete shared[id];
        return apply({ ...s, shared });
      }
      if (!s.scoped[scope] || !(id in s.scoped[scope])) return;
      const own = { ...s.scoped[scope] };
      delete own[id];
      apply({ ...s, scoped: { ...s.scoped, [scope]: own } });
    },
    setActiveScope: (scope) => {
      const s = get();
      if (s.activeScope === scope) return;
      // A scope already registered shows at once; one still listing keeps the previous view.
      apply({
        ...s,
        activeScope: scope,
        viewScope: scope === null || s.scoped[scope] ? scope : s.viewScope,
      });
    },
    settleScope: (scope) => {
      const s = get();
      if (s.activeScope === scope && s.viewScope !== scope) apply({ ...s, viewScope: scope });
    },
    dropScope: (scope) => {
      const s = get();
      if (!s.scoped[scope]) return;
      const scoped = { ...s.scoped };
      delete scoped[scope];
      apply({ ...s, scoped });
    },
  };
});

warmScopes.onRekey((from) => {
  watched.delete(from);
  usePanelRegistry.getState().dropScope(from);
});

/** A team's panels as its own right pane sees them. A hidden team keeps the panels it registered
 *  while it was shown; outside any team (no scope) it is what the active scope shows. */
export function usePanelsFor(scope: string | null): Record<string, PanelDefinition> {
  const shown = usePanelRegistry((s) => s.panels);
  const shared = usePanelRegistry((s) => s.shared);
  const own = usePanelRegistry((s) => (scope ? s.scoped[scope] : undefined));
  const isActive = usePanelRegistry((s) => scope === s.activeScope);
  return useMemo(
    () => (!scope || isActive ? shown : own ? { ...shared, ...own } : shared),
    [scope, isActive, shown, shared, own],
  );
}

// --- Non-hook accessors (for use outside React, e.g. the native menu rebuild) -

/** Register a panel (idempotent replace by id). With a scope it belongs to that team. */
export function registerPanel(def: PanelDefinition, scope?: string): void {
  usePanelRegistry.getState().registerPanel(def, scope);
}

/** Remove a panel definition by id. */
export function unregisterPanel(id: string, scope?: string): void {
  usePanelRegistry.getState().unregisterPanel(id, scope);
}

/** Look up a panel definition the active scope shows, or undefined if not registered. */
export function getPanel(id: string): PanelDefinition | undefined {
  return usePanelRegistry.getState().panels[id];
}

/** A panel definition registered for `scope` (warm or active), or undefined. */
export function getScopedPanel(scope: string, id: string): PanelDefinition | undefined {
  return usePanelRegistry.getState().scoped[scope]?.[id];
}

/** The panels registered for `scope`. */
export function listScopedPanels(scope: string): PanelDefinition[] {
  return Object.values(usePanelRegistry.getState().scoped[scope] ?? {});
}

/** All panel definitions the active scope shows (registration order). */
export function listPanels(): PanelDefinition[] {
  return Object.values(usePanelRegistry.getState().panels);
}

/**
 * Whether a panel is OFFERED for management — offerable, or already present (a
 * retired panel that landed can still be removed). The basis the native
 * View → Panels menu (`listedPanels`) filters on. `present` is passed in so this
 * stays layout-agnostic (the caller computes membership).
 */
export function isPanelListable(def: PanelDefinition, present: boolean): boolean {
  return def.offerable !== false || present;
}

/**
 * SW-RP-3: register a panel for the lifetime of the calling component —
 * registers on mount, unregisters on unmount, and re-registers when `def`
 * identity changes. This is how a DYNAMIC consumer (e.g. SW-GS-10's agent
 * panel) adds itself from its own component scope, so `def.render` /
 * `def.headerActions` close over that component's session/callbacks.
 *
 * The built-in panels do NOT use this — they register once at module load
 * (`registerBuiltinPanels`) since they never unmount.
 *
 * CONTRACT: pass a STABLE/memoized `def` (`useMemo`, or a module constant). A
 * fresh object literal every render re-runs the register effect — wasteful, and
 * the registry's React subscribers re-render. The hook is hardened against the
 * three lifecycle hazards a naive `register/unregister` would hit:
 *  - **Same-id replace is gap-free.** A `def` update that keeps the same id
 *    REPLACES in place (no unregister→register window) so the portaled, kept-
 *    mounted panel body is never torn down + remounted (which would destroy
 *    react-flow / Tiptap state — the regression SW-RP-1/2 exist to prevent).
 *  - **An id change drops the stale id** (no leftover entry in the registry).
 *  - **Cleanup only removes OUR entry.** If another component registered the
 *    same id after us (last-writer-wins), our unmount won't clobber its live
 *    registration — we no-op unless the stored def is still the one we wrote.
 */
export function useRegisterPanel(def: PanelDefinition): void {
  // The def we actually wrote to the registry — drives gap-free same-id
  // replacement, stale-id cleanup on an id change, and owner-guarded teardown.
  const registered = useRef<PanelDefinition | null>(null);
  useEffect(() => {
    const prev = registered.current;
    // id changed since our last write → drop the stale id (only if still ours).
    if (prev && prev.id !== def.id && getPanel(prev.id) === prev) {
      unregisterPanel(prev.id);
    }
    // Same-id update is a plain replace — no intervening unregister, so the
    // body is never remounted.
    registerPanel(def);
    registered.current = def;
  }, [def]);
  // Unmount-only teardown, owner-guarded so it can't remove a same-id panel a
  // later-mounted component is still relying on.
  useEffect(
    () => () => {
      const ours = registered.current;
      if (ours && getPanel(ours.id) === ours) unregisterPanel(ours.id);
    },
    [],
  );
}
