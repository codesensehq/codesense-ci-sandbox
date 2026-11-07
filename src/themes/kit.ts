// SPDX-License-Identifier: MIT
import type { Surface } from "./compiler";

/** Optional theme token for review and PR status; themes that omit it fall back to the function hue. */
const REVIEW_STATUS_ROLE = "review-status";

/**
 * Surface-free pane names share one role mapping at build time and during live pushes.
 * `defines` reports whether the active theme carries an optional role token.
 */
export function kitRoles(
  surface: Surface = "viewer",
  defines: (role: string) => boolean = () => false,
): Record<string, string> {
  const reviewRole = defines(REVIEW_STATUS_ROLE) ? REVIEW_STATUS_ROLE : "code-function";
  const roles: Record<string, string> = {
    bg: `surface-${surface}-bg`,
    fg: `text-${surface}-primary`,
    "muted-fg": `text-${surface}-muted`,
    border: "border-default",
    "border-control": "border-control",
    "focus-ring": "focus-ring",
    surface: surface === "viewer" ? "surface-viewer-raised" : `surface-${surface}-bg`,
    accent: "accent-default",
    "accent-fg": "accent-fg",
    highlight: "accent-default",
    link: "accent-text",
    danger: `severitySurface-${surface}-danger-fg`,
    success: `severitySurface-${surface}-success-fg`,
    warning: `severitySurface-${surface}-warning-fg`,
    deleted: "git-deleted",
    selected: `surface-${surface}-selected`,
    "git-added": "git-added",
    "git-modified": "git-modified",
    "git-deleted": "git-deleted",
    "git-conflict": "code-function",
    "sidebar-selected": "surface-sidebar-selected",
    "sidebar-drop-ring": "focus-ring",
    "sidebar-bg": "surface-sidebar-bg",
    "sidebar-fg": "text-sidebar-primary",
    "sidebar-text": "text-sidebar-primary",
    "sidebar-selected-text": "text-sidebar-primary",
    "status-needs": `status-${surface}-needs`,
    "status-busy": `status-${surface}-busy`,
    "status-failed": `status-${surface}-failed`,
    "status-idle": `status-${surface}-idle`,
  };
  for (const name of [
    "keyword",
    "string",
    "comment",
    "number",
    "constant",
    "function",
    "variable",
    "type",
    "operator",
    "punctuation",
    "meta",
    "regexp",
  ])
    roles[`code-${name}`] = `code-${name}`;
  for (const [name, role] of Object.entries({
    planning: `text-${surface}-muted`,
    todo: `text-${surface}-muted`,
    "in-progress": `severitySurface-${surface}-info-fg`,
    blocked: `severitySurface-${surface}-danger-fg`,
    review: reviewRole,
    qa: `severitySurface-${surface}-warning-fg`,
    done: `severitySurface-${surface}-success-fg`,
  }))
    roles[`story-${name}`] = role;
  for (const [name, role] of Object.entries({
    failed: "failed",
    running: "busy",
    idle: "idle",
    "needs-you": "needs",
    exited: "idle",
    offline: "idle",
    unknown: "idle",
  }))
    roles[`state-${name}`] = `status-${surface}-${role}`;
  for (const [name, role] of Object.entries({
    planning: `text-${surface}-muted`,
    dev: `severitySurface-${surface}-info-fg`,
    review: reviewRole,
    pr: reviewRole,
    blocked: `severitySurface-${surface}-danger-fg`,
    done: `severitySurface-${surface}-success-fg`,
  }))
    roles[`phase-${name}`] = role;
  return roles;
}
