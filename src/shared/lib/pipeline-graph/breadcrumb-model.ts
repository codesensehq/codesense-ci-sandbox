// SPDX-License-Identifier: MIT
/**
 * Shared, pure breadcrumb model for the drilled pipeline scope.
 *
 * Extracted from `pipeline-status-nav.tsx` (SW-EW-36) so both the status-nav
 * sidebar breadcrumb and the SW-PP-2 pipeline-panel header toolbar breadcrumb
 * consume the same crumb math instead of reimplementing it. Rendering stays
 * per-site (the two surfaces have different chrome); only the *model* is shared.
 *
 * Each ancestor's absolute path is reconstructed from `rootPath` + the leading
 * segments of the drilled scope path — no new prop, no IPC. A crumb click is
 * expected to route through the same drill action the folder-drill affordance
 * uses, so the two surfaces can never disagree on the resulting scope.
 */

export interface PipelineCrumb {
  /** Display label (root label for the first crumb; dir basename otherwise). */
  label: string;
  /** Absolute dir path this crumb drills to (the root crumb's is `rootPath`). */
  absPath: string;
  /** True for the deepest (current) crumb — rendered non-interactive. */
  isCurrent: boolean;
}

/**
 * Build the breadcrumb trail for a drilled scope. Returns `[]` when not drilled
 * (or when `scopeAbsPath` isn't a descendant of `rootPath` — defensive), so the
 * caller can fall back to a bare title.
 */
export function buildPipelineBreadcrumbs(params: {
  isDrilledIn: boolean;
  rootPath: string | null | undefined;
  scopeAbsPath: string;
  rootLabel: string;
}): PipelineCrumb[] {
  const { isDrilledIn, rootPath, scopeAbsPath, rootLabel } = params;
  if (!isDrilledIn || !rootPath) return [];
  const root = rootPath.replace(/\/+$/, "");
  const abs = scopeAbsPath;
  if (abs !== root && !abs.startsWith(root + "/")) return [];
  const segs = abs.slice(root.length).split("/").filter(Boolean);
  const crumbs: PipelineCrumb[] = [
    { label: rootLabel, absPath: root, isCurrent: segs.length === 0 },
  ];
  segs.forEach((seg, i) => {
    crumbs.push({
      label: seg,
      absPath: root + "/" + segs.slice(0, i + 1).join("/"),
      isCurrent: i === segs.length - 1,
    });
  });
  return crumbs;
}

export interface CollapsedBreadcrumbs {
  /**
   * Crumbs to render, with the middle collapsed to an `"ellipsis"` sentinel
   * when the trail is deeper than three (root + parent + current stay visible).
   */
  displayCrumbs: (PipelineCrumb | "ellipsis")[];
  /** The ancestors hidden behind the ellipsis (for its tooltip). */
  hiddenAncestors: PipelineCrumb[];
  /** "Up one level" target: the parent (second-to-last) crumb, else root. */
  parentAbsPath: string;
}

/**
 * Collapse a deep trail so a narrow container can always show root + parent +
 * current one-click. Keeps every crumb when the trail is three or fewer.
 */
export function collapsePipelineBreadcrumbs(
  breadcrumbs: PipelineCrumb[],
  rootPath: string | null | undefined,
): CollapsedBreadcrumbs {
  const parentAbsPath =
    breadcrumbs.length >= 2 ? breadcrumbs[breadcrumbs.length - 2].absPath : (rootPath ?? "");
  const collapsed = breadcrumbs.length > 3;
  const hiddenAncestors = collapsed ? breadcrumbs.slice(1, breadcrumbs.length - 2) : [];
  const displayCrumbs: (PipelineCrumb | "ellipsis")[] = collapsed
    ? [
        breadcrumbs[0],
        "ellipsis",
        breadcrumbs[breadcrumbs.length - 2],
        breadcrumbs[breadcrumbs.length - 1],
      ]
    : breadcrumbs;
  return { displayCrumbs, hiddenAncestors, parentAbsPath };
}
