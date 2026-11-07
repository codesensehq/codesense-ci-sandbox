// SPDX-License-Identifier: MIT
/**
 * A custom pane's panel chrome: the icon its `<meta name="codesense:icon">`
 * names and the badge it sets with `codesense.setBadge`.
 *
 * Any Lucide icon may be named, so icons load on demand (one small module per
 * named icon, only when a pane uses it) rather than bundling the whole set. A
 * name Lucide doesn't have shows a visible "?" glyph, never a silent blank.
 */
import { AppWindow, CircleHelp, type LucideIcon, type LucideProps } from "lucide-react";
import { DynamicIcon, iconNames, type IconName } from "lucide-react/dynamic";
import { createElement, forwardRef } from "react";

import type { DocBadgeSeverity } from "@/features/dashboard/components/html-viewer";

import { usePaneStore } from "./pane-store";

const KNOWN = new Set<string>(iconNames);

/** `GitPullRequest` / `git-pull-request` / ` Git_Pull_Request ` → `git-pull-request`;
 *  `Building2` / `BarChart3` → `building-2` / `bar-chart-3`. */
export function lucideIconName(raw: string): string {
  const kebab = raw
    .trim()
    .replace(/([a-z0-9])([A-Z])/g, "$1-$2")
    .replace(/([A-Z])([A-Z][a-z])/g, "$1-$2")
    .replace(/[\s_]+/g, "-")
    .toLowerCase();
  if (KNOWN.has(kebab)) return kebab;
  // Lucide puts a hyphen before a trailing number.
  const numbered = kebab.replace(/([a-z])(\d)/g, "$1-$2");
  return KNOWN.has(numbered) ? numbered : kebab;
}

const byName = new Map<string, LucideIcon>();

function namedIcon(name: IconName): LucideIcon {
  const icon = forwardRef<SVGSVGElement, LucideProps>((props, ref) =>
    createElement(DynamicIcon, {
      ...props,
      ref,
      name,
      "data-pane-icon": name,
      // Holds the slot while the icon's module loads, so the title never shifts.
      fallback: () =>
        createElement("span", {
          style: { display: "inline-block", width: props.size ?? 24, height: props.size ?? 24 },
        }),
    } as LucideProps & { name: IconName }),
  );
  icon.displayName = `PaneIcon(${name})`;
  return icon as LucideIcon;
}

function unknownIcon(raw: string): LucideIcon {
  const icon = forwardRef<SVGSVGElement, LucideProps>((props, ref) =>
    createElement(CircleHelp, {
      ...props,
      ref,
      "data-pane-icon": "unknown",
      "aria-label": `Unknown icon "${raw}"`,
    } as LucideProps),
  );
  icon.displayName = `PaneIcon(unknown ${raw})`;
  return icon as LucideIcon;
}

/** The panel icon for a pane's icon meta: the generic pane icon when it names
 *  none, the "?" glyph when Lucide has no such icon. One component per name, so
 *  re-registering a pane never remounts its tab. */
export function paneIcon(raw: string | undefined): LucideIcon {
  if (!raw?.trim()) return AppWindow;
  const name = lucideIconName(raw);
  const key = KNOWN.has(name) ? name : `?${raw}`;
  let icon = byName.get(key);
  if (!icon) {
    icon = KNOWN.has(name) ? namedIcon(name as IconName) : unknownIcon(raw);
    byName.set(key, icon);
  }
  return icon;
}

const SEVERITY_CLASS: Record<DocBadgeSeverity, string> = {
  info: "bg-muted text-muted-foreground",
  warning: "bg-amber-500/20 text-amber-600 dark:text-amber-400",
  error: "bg-destructive/20 text-destructive",
};

/** The badge a pane set, beside its title (also while its section is collapsed). */
export function PaneBadge({ panelId }: { panelId: string }) {
  const badge = usePaneStore((s) => s.badges[panelId]);
  if (!badge) return null;
  const label =
    badge.severity === "info" ? `${badge.count} items` : `${badge.count} ${badge.severity} items`;
  return (
    <span
      data-testid="pane-badge"
      data-severity={badge.severity}
      aria-label={label}
      className={`ml-1 rounded-full px-1.5 text-[0.65rem] font-medium ${SEVERITY_CLASS[badge.severity]}`}
    >
      {badge.count > 99 ? "99+" : badge.count}
    </span>
  );
}
