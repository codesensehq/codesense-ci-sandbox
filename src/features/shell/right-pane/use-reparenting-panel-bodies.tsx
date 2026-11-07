// SPDX-License-Identifier: MIT
/**
 * SW-RP-2: keep every panel body MOUNTED across reorder / group / ungroup /
 * collapse / tab-switch (AC-5).
 *
 * The problem: panel bodies used to render nested inside each section's React
 * subtree, so a group/ungroup that moves a panel to a *different* section
 * unmounted + remounted it — destroying react-flow viewport / Tiptap editor
 * state (the #1 regression SW-RP-1 guards). Reorder was safe (the whole keyed
 * section moves intact) but cross-section moves were not.
 *
 * The fix (dependency-free, mirrors `react-reverse-portal`): render each panel
 * body ONCE into a **stable detached container** via a portal whose target node
 * never changes — so React never remounts it. A layout-effect then imperatively
 * `appendChild`s that container into the active section's slot. Moving a DOM node
 * with `appendChild` does not remount the React components inside it, so the body
 * survives every rearrange.
 *
 * react-flow #004 (mount-into-zero-height) is avoided the same way SW-RP-1 did:
 * a body's portal only renders once its container has been attached to a
 * **visible** (expanded + active) slot at least once, then it sticks.
 */
import { useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

import type { PanelDefinition } from "./panel-registry";
import type { RightPaneLayout } from "./right-pane-layout";

interface PanelPlacement {
  sectionId: string;
  /** Active panel of an expanded section — i.e. should be on screen. */
  visible: boolean;
}

/** Where each panel currently lives + whether it should be visible. */
function computePlacements(layout: RightPaneLayout): Map<string, PanelPlacement> {
  const placements = new Map<string, PanelPlacement>();
  for (const section of layout.sections) {
    for (const panelId of section.group.panelIds) {
      placements.set(panelId, {
        sectionId: section.id,
        visible: !section.collapsed && section.group.activePanelId === panelId,
      });
    }
  }
  return placements;
}

export interface ReparentingPanelBodies {
  /** Ref callback a `SectionView` uses to register its body slot DOM node. */
  registerSlot: (sectionId: string, el: HTMLDivElement | null) => void;
  /** Portal nodes to render once anywhere under the host providers. */
  portals: React.ReactPortal[];
}

export function useReparentingPanelBodies(
  layout: RightPaneLayout,
  panels: Record<string, PanelDefinition>,
): ReparentingPanelBodies {
  // sectionId -> the section's body slot DOM node (populated by ref callbacks;
  // pure ref mutation, never triggers a render).
  const slots = useRef(new Map<string, HTMLDivElement>());
  // panelId -> its stable, reused body container (the portal target that never
  // changes, so the body is never remounted as it is reparented between slots).
  const containers = useRef(new Map<string, HTMLDivElement>());
  // Sticky "has been attached to a visible slot at least once" set — gates the
  // portal so react-flow only ever mounts into a sized, on-screen container.
  const [ready, setReady] = useState<Set<string>>(() => new Set());

  const registerSlot = (sectionId: string, el: HTMLDivElement | null) => {
    if (el) slots.current.set(sectionId, el);
    else slots.current.delete(sectionId);
  };

  // Placement: reparent each panel's container into its section slot + toggle
  // visibility. Runs after every layout change; reads `slots.current` (populated
  // during the same commit). Imperative DOM move — no React remount.
  useLayoutEffect(() => {
    const placements = computePlacements(layout);
    const present = new Set(placements.keys());
    const newlyReady: string[] = [];

    for (const [panelId, { sectionId, visible }] of placements) {
      if (!panels[panelId]) continue; // unregistered → no body to host
      const slot = slots.current.get(sectionId);
      if (!slot) continue; // slot not mounted yet (e.g. just-added section)
      let container = containers.current.get(panelId);
      if (!container) {
        container = document.createElement("div");
        container.className = "flex flex-col flex-1 min-h-0 overflow-hidden";
        containers.current.set(panelId, container);
      }
      if (container.parentElement !== slot) slot.appendChild(container);
      container.style.display = visible ? "flex" : "none";
      if (visible && !ready.has(panelId)) newlyReady.push(panelId);
    }

    // Drop containers for panels no longer in the layout (frees them so a
    // removed+re-added panel re-runs the lazy-into-visible mount).
    for (const [panelId, container] of containers.current) {
      if (!present.has(panelId)) {
        container.remove();
        containers.current.delete(panelId);
      }
    }

    setReady((prev) => {
      const next = new Set<string>();
      let changed = false;
      for (const id of prev) {
        if (present.has(id)) next.add(id);
        else changed = true; // panel left → drop from ready
      }
      for (const id of newlyReady) {
        if (!next.has(id)) {
          next.add(id);
          changed = true;
        }
      }
      return changed ? next : prev;
    });
    // `ready` is a dep so the effect re-runs after a panel becomes ready (to
    // render its portal); the updater is a no-op once converged, so no loop.
  }, [layout, panels, ready]);

  // Safe ref-read during render: a panel only enters `ready` AFTER its container
  // was created + attached in the layout-effect of a prior commit, so the node
  // always exists here. (Same intentional pattern as SW-RP-1's mount set.)
  // eslint-disable-next-line react-hooks/refs
  const portals = [...ready].flatMap((panelId) => {
    const def = panels[panelId];
    const container = containers.current.get(panelId);
    if (!def || !container) return [];
    // A panel's own controls live in a toolbar at the TOP OF ITS BODY — never in the section chrome.
    // The chrome carries only expand + close; putting per-panel actions there reads as clutter once
    // panels are combined into a tab strip (a tab's controls would sit next to another tab's title).
    // Because each panel has its own reparented container, this toolbar rides with the panel across
    // every rearrange and is naturally scoped to it — no shared chrome slot to collide.
    const body = def.headerActions ? (
      <>
        <div className="flex shrink-0 items-center gap-1 border-b px-2 py-1">
          {def.headerActions()}
        </div>
        {def.render()}
      </>
    ) : (
      def.render()
    );
    return [createPortal(body, container, panelId)];
  });

  return { registerSlot, portals };
}
