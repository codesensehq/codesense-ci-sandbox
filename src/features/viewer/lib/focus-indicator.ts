// SPDX-License-Identifier: MIT
import { useSyncExternalStore } from "react";

/**
 * The terminal focus indicator (the blue line), derived from REAL DOM focus —
 * never from store selection.
 *
 * Two levels, one indicator each: a tile frame carries `data-focus-tile`, and
 * each terminal body inside it carries `data-focus-pane`. The element holding
 * keyboard focus names its tile and pane by `closest()` — exactly the
 * `:focus-within` relation — and those become the indicated tile and pane.
 *
 * The indicator is "on" while focus is inside it and the window is focused. When
 * focus leaves for app chrome or another window it goes "dim" rather than
 * disappearing, so the user can still see where typing would resume. Focus
 * landing in another tile moves both levels; focus landing outside the terminal
 * area only dims.
 *
 * The state is published as a `data-focus-indicator="on" | "dim"` attribute, and
 * the stylesheet (`styles/focus-indicator.css`) keys only on that attribute.
 */

export type FocusIndicator = "on" | "dim";

export const FOCUS_TILE_ATTR = "data-focus-tile";
export const FOCUS_PANE_ATTR = "data-focus-pane";

interface FocusState {
  /** The last tile that held focus (sticky), and whether it holds it now. */
  readonly tile: string | null;
  readonly tileLive: boolean;
  /** The last pane that held focus (sticky), the tile it sits in, and whether it holds it now. */
  readonly pane: string | null;
  readonly paneTile: string | null;
  readonly paneLive: boolean;
  readonly windowFocused: boolean;
}

const EMPTY: FocusState = {
  tile: null,
  tileLive: false,
  pane: null,
  paneTile: null,
  paneLive: false,
  windowFocused: true,
};

let state: FocusState = EMPTY;
const listeners = new Set<() => void>();

function publish(next: FocusState): void {
  const same = (Object.keys(next) as (keyof FocusState)[]).every((k) => next[k] === state[k]);
  if (same) return;
  state = next;
  for (const l of listeners) l();
}

/** Recompute from the element that now holds focus (null: nothing in the page does). */
function focusMovedTo(target: EventTarget | null): void {
  const el = target instanceof Element ? target : null;
  const tile = el?.closest(`[${FOCUS_TILE_ATTR}]`)?.getAttribute(FOCUS_TILE_ATTR) ?? null;
  const pane = el?.closest(`[${FOCUS_PANE_ATTR}]`)?.getAttribute(FOCUS_PANE_ATTR) ?? null;
  if (tile === null) {
    // Outside every tile (app chrome, body, another window): both levels keep
    // their last holder and dim.
    publish({ ...state, tileLive: false, paneLive: false });
    return;
  }
  if (pane !== null) {
    publish({ ...state, tile, tileLive: true, pane, paneTile: tile, paneLive: true });
    return;
  }
  // In a tile but not in one of its panes (its chrome, an overlay): the tile is
  // live; its last pane stays dim, and a pane from another tile is dropped.
  const keepPane = state.paneTile === tile;
  publish({
    ...state,
    tile,
    tileLive: true,
    pane: keepPane ? state.pane : null,
    paneTile: keepPane ? state.paneTile : null,
    paneLive: false,
  });
}

const onFocusIn = (e: FocusEvent) => focusMovedTo(e.target);
// A focusout with a relatedTarget is followed by that target's focusin; one
// without is focus leaving for nowhere (the body, or another window).
const onFocusOut = (e: FocusEvent) => {
  if (e.relatedTarget === null) focusMovedTo(null);
};
const onWindowBlur = () => publish({ ...state, windowFocused: false });
const onWindowFocus = () => {
  publish({ ...state, windowFocused: true });
  focusMovedTo(document.activeElement);
};

function subscribe(listener: () => void): () => void {
  if (listeners.size === 0) {
    document.addEventListener("focusin", onFocusIn);
    document.addEventListener("focusout", onFocusOut);
    window.addEventListener("blur", onWindowBlur);
    window.addEventListener("focus", onWindowFocus);
    state = { ...EMPTY, windowFocused: document.hasFocus() };
    focusMovedTo(document.activeElement);
  }
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) {
      document.removeEventListener("focusin", onFocusIn);
      document.removeEventListener("focusout", onFocusOut);
      window.removeEventListener("blur", onWindowBlur);
      window.removeEventListener("focus", onWindowFocus);
      state = EMPTY;
    }
  };
}

/** The key a tile frame publishes as `data-focus-tile`. Team-scoped: workspace
 *  ids repeat across teams, and every team's terminals stay mounted. */
export function focusTileKey(teamKey: string, workspaceId: string): string {
  return `${teamKey}␟${workspaceId}`;
}

function tileIndicator(s: FocusState, tileKey: string): FocusIndicator | undefined {
  if (s.tile !== tileKey) return undefined;
  return s.tileLive && s.windowFocused ? "on" : "dim";
}

/** `"on:<paneId>"` / `"dim:<paneId>"` for the indicated pane inside `tileKey`,
 *  else null — a primitive, so a tile re-renders only when its own pane changes. */
function paneIndicator(s: FocusState, tileKey: string): string | null {
  if (s.pane === null || s.paneTile !== tileKey) return null;
  return `${s.paneLive && s.windowFocused ? "on" : "dim"}:${s.pane}`;
}

/** The indicator for one tile: on, dim, or none. */
export function useTileFocusIndicator(tileKey: string): FocusIndicator | undefined {
  return useSyncExternalStore(subscribe, () => tileIndicator(state, tileKey));
}

/** The indicated pane inside one tile, as a lookup by pane id. */
export function usePaneFocusIndicator(
  tileKey: string,
): (paneId: string) => FocusIndicator | undefined {
  const entry = useSyncExternalStore(subscribe, () => paneIndicator(state, tileKey));
  return (paneId) => {
    if (entry === null) return undefined;
    const sep = entry.indexOf(":");
    return entry.slice(sep + 1) === paneId ? (entry.slice(0, sep) as FocusIndicator) : undefined;
  };
}
