// SPDX-License-Identifier: MIT
import type { ReactElement } from "react";

import type { CommonViewerProps } from "@/shared/lib/viewer-file-type";

/**
 * SW-GS-2b (C): a generalized "virtual viewer tab" mechanism — a non-file
 * React surface that opens in the content area as a tab (like a story file),
 * backed by a sentinel path instead of disk content.
 *
 * The Git Sync Workbench is the first consumer (replacing its Radix `Dialog`
 * overlay): opening it as a tab lets the user switch to/from it while the live
 * agent terminal stays watchable in the bottom tray. The mechanism is generic
 * — Settings and other current popovers can register later — so it carries no
 * git-sync literals; consumers register a component under a virtual path
 * prefix and `renderViewer` short-circuits to it before extension dispatch.
 *
 * A virtual component reads from a store and MUST NOT call `readFile` — a
 * sentinel path has no disk backing, so it opens instantly (zero awaits).
 */

/** Scheme prefix that marks a viewer tab path as synthetic (no disk backing). */
export const VIRTUAL_TAB_SCHEME = "virtual://";

/**
 * A synthetic viewer surface. Receives the same `CommonViewerProps` contract
 * as a real file viewer (so it slots into `renderViewer` unchanged) but is
 * expected to ignore the disk-oriented fields and render from a store.
 */
export type VirtualTabComponent = (props: CommonViewerProps & { filePath: string }) => ReactElement;

/** A tab label: fixed per prefix, or derived from the full path (a remote file
 *  tab labels itself with its machine + basename). */
export type VirtualTabTitle = string | ((path: string) => string);

interface VirtualTabRegistration {
  /** Display label shown on the viewer tab (e.g. "Git Sync"). */
  title: VirtualTabTitle;
  component: VirtualTabComponent;
  /** Optional hover tooltip for the tab, derived from the full path. */
  tooltip?: (path: string) => string;
}

const registry = new Map<string, VirtualTabRegistration>();

/**
 * Register a synthetic surface under a virtual path prefix
 * (e.g. `"virtual://git-sync"`) with the label shown on its tab. Idempotent —
 * last registration wins.
 */
export function registerVirtualTab(
  prefix: string,
  title: VirtualTabTitle,
  component: VirtualTabComponent,
  tooltip?: (path: string) => string,
): void {
  registry.set(prefix, { title, component, tooltip });
}

/** True when a tab path is synthetic (starts with the virtual scheme). */
export function isVirtualTabPath(path: string | null | undefined): boolean {
  return typeof path === "string" && path.startsWith(VIRTUAL_TAB_SCHEME);
}

/**
 * Longest-prefix match for a virtual path, or `null` when the path is not
 * virtual or nothing is registered. A more specific registration
 * (`virtual://git-sync/conflict`) takes precedence over a broader one.
 */
function matchVirtualTab(path: string | null | undefined): VirtualTabRegistration | null {
  if (!isVirtualTabPath(path)) return null;
  const p = path as string;
  let best: { prefix: string; reg: VirtualTabRegistration } | null = null;
  for (const [prefix, reg] of registry) {
    // Segment-aware: exact match or a `/`-delimited descendant — so a prefix
    // `virtual://git-sync` does NOT match a sibling `virtual://git-syncer`.
    const matches = p === prefix || p.startsWith(`${prefix}/`);
    if (matches && (!best || prefix.length > best.prefix.length)) {
      best = { prefix, reg };
    }
  }
  return best?.reg ?? null;
}

/** Resolve the registered component for a virtual path, or `null`. */
export function resolveVirtualTab(path: string | null | undefined): VirtualTabComponent | null {
  return matchVirtualTab(path)?.component ?? null;
}

/** Resolve the registered tab label for a virtual path, or `null`. */
export function resolveVirtualTabTitle(path: string | null | undefined): string | null {
  const title = matchVirtualTab(path)?.title;
  if (title === undefined) return null;
  return typeof title === "function" ? title(path as string) : title;
}

/** Resolve the registered tab tooltip for a virtual path, or `null` (real files
 *  and virtual tabs registered without one carry no tooltip). */
export function resolveVirtualTabTooltip(path: string | null | undefined): string | null {
  const reg = matchVirtualTab(path);
  return reg?.tooltip ? reg.tooltip(path as string) : null;
}

/** Test-only: drop all registrations so suites don't leak into each other. */
export function __resetVirtualTabsForTest(): void {
  registry.clear();
}
