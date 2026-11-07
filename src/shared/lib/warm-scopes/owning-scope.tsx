// SPDX-License-Identifier: MIT
import { createContext, useContext, type ReactNode } from "react";
import { createPortal } from "react-dom";
import type { ScopeKey } from "./tiers";
import { useScopeActive } from "./use-scope-tier";

const OwningScopeContext = createContext<ScopeKey | null>(null);

/** Names the scope (team) a subtree belongs to. A retained team's views and CLI grid wrap
 *  themselves in it, so a descendant can ask whether it is on screen without being handed the
 *  team key through every prop. The context crosses React portals, so it also names the owner of
 *  a right-pane or dialog body that renders elsewhere in the DOM. */
export function OwningScopeProvider({
  scope,
  children,
}: {
  scope: ScopeKey | null;
  children: ReactNode;
}) {
  return <OwningScopeContext value={scope}>{children}</OwningScopeContext>;
}

/** The scope this component belongs to, or null outside any retained team. */
export function useOwningScope(): ScopeKey | null {
  return useContext(OwningScopeContext);
}

/** Whether this component's owning team is the one on screen (true outside any team). The hook form
 *  of the hidden-team rule for a component that owns a mount-time focus, a modal or a listener. */
export function useOwnsScope(): boolean {
  return useScopeActive(useOwningScope());
}

/** Renders `children` only while the owning team is on screen. A body portal escapes the team
 *  wrapper's `display:none`, so a hidden team's dialog, menu or overlay would otherwise stay on
 *  screen, take modal focus and duplicate the shown team's. */
export function ScopeGate({ children }: { children: ReactNode }) {
  return useOwnsScope() ? children : null;
}

/** `createPortal` for a component rendered inside a team: nothing while that team is hidden. The
 *  one way to portal from a team subtree (the boundary guard rejects a bare `createPortal`). */
export function ScopedPortal({
  children,
  container,
}: {
  children: ReactNode;
  container: Element | DocumentFragment;
}) {
  return useOwnsScope() ? createPortal(children, container) : null;
}
