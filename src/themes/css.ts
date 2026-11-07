// SPDX-License-Identifier: MIT
import { SURFACES } from "./compiler";
export const EDITOR_ACTIVE_LINE_ROLE = "surface.viewer.hover";

/** Existing public variables remain aliases; consumers do not need palette knowledge. */
export const APP_ALIASES: Record<string, string> = {
  background: "surface-canvas-bg",
  foreground: "text-canvas-primary",
  card: "surface-sidebar-bg",
  "card-foreground": "text-canvas-primary",
  popover: "surface-overlay-bg",
  "popover-foreground": "text-overlay-primary",
  primary: "text-canvas-primary",
  "primary-foreground": "surface-canvas-bg",
  secondary: "surface-canvas-selected",
  "secondary-foreground": "text-canvas-primary",
  muted: "surface-canvas-selected",
  "muted-foreground": "text-canvas-muted",
  accent: "accent-default",
  "accent-foreground": "accent-fg",
  destructive: "severity-danger-fg",
  border: "border-default",
  input: "border-control",
  ring: "focus-ring",
  sidebar: "surface-sidebar-bg",
  "sidebar-foreground": "text-sidebar-primary",
  "sidebar-primary": "accent-default",
  "sidebar-primary-foreground": "accent-fg",
  "sidebar-accent": "surface-sidebar-selected",
  "sidebar-accent-foreground": "text-sidebar-primary",
  "sidebar-border": "border-default",
  "sidebar-ring": "focus-ring",
  "cs-sidebar-bg": "surface-sidebar-bg",
  "cs-sidebar-text": "text-sidebar-primary",
  "cs-sidebar-text-muted": "text-sidebar-muted",
  "cs-sidebar-icon": "text-sidebar-muted",
  "cs-sidebar-border": "border-default",
  "cs-sidebar-hover": "surface-sidebar-hover",
  "cs-sidebar-selected": "surface-sidebar-selected",
  "cs-sidebar-selected-text": "text-sidebar-primary",
  "cs-viewer-bg": "surface-viewer-bg",
  "cs-viewer-accent": "accent-text",
  "cs-doc-bg": "surface-viewer-raised",
  "cs-workspace-card-bg": "surface-viewer-raised",
  "cs-tab-bg": "surface-sidebar-bg",
  "cs-tab-active-bg": "surface-sidebar-selected",
  "cs-tab-active-text": "text-sidebar-primary",
  "cs-tab-text": "text-sidebar-muted",
  "cs-tab-hover": "surface-sidebar-hover",
  "cs-tab-border": "border-default",
  "cs-app-warning-text": "severity-warning-fg",
  "cs-warning-text": "severity-warning-fg",
  "cs-warning-bg": "severity-warning-bg",
  "cs-warning-border": "severity-warning-border",
  "cs-runtime-claude-text": "severity-warning-fg",
  "cs-runtime-claude-bg": "severity-warning-bg",
  "cs-runtime-codex-text": "severity-info-fg",
  "cs-runtime-codex-bg": "severity-info-bg",
  "cs-runtime-opencode-text": "text-canvas-muted",
  "cs-runtime-opencode-bg": "surface-canvas-selected",
  "cs-status-added": "git-added",
  "cs-status-modified": "git-modified",
  "cs-status-deleted": "git-deleted",
  "cs-status-renamed": "git-renamed",
  "cs-agent-ctx-out": "text-sidebar-muted",
  "cs-status-needs": "severity-warning-fg",
  "cs-status-failed": "severity-danger-fg",
  "cs-status-busy": "severity-info-fg",
  "cs-status-done": "severity-success-fg",
  "cs-status-idle": "text-sidebar-muted",
  "cs-status-text": "text-sidebar-primary",
  "cs-git-added": "git-added",
  "cs-git-modified": "git-modified",
  "cs-git-deleted": "git-deleted",
  "cs-git-conflict": "code-function",
  "cs-tag-text": "text-canvas-muted",
  "cs-tag-select": "accent-text",
  "cs-tag-multi-select": "code-function",
  "cs-tag-ordered": "code-variable",
  "cs-tag-status": "git-added",
  "cs-tag-date": "git-modified",
  "cs-tag-relation": "code-number",
  "chart-1": "accent-default",
  "chart-2": "text-canvas-muted",
  "chart-3": "text-sidebar-muted",
  "chart-4": "border-default",
  "chart-5": "surface-canvas-selected",
};
for (const k of [
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
  APP_ALIASES[`cs-code-${k}`] = `code-${k}`;
for (const k of ["body", "headings", "bold", "quotes", "pre-code"])
  APP_ALIASES[`tw-prose-${k}`] = "text-viewer-primary";
for (const k of ["links", "code"])
  APP_ALIASES[`tw-prose-${k}`] = k === "links" ? "accent-text" : "code-string";
for (const k of ["counters", "bullets", "captions"])
  APP_ALIASES[`tw-prose-${k}`] = "text-viewer-muted";
for (const k of ["hr", "quote-borders", "th-borders", "td-borders"])
  APP_ALIASES[`tw-prose-${k}`] = "border-default";
APP_ALIASES["tw-prose-pre-bg"] = "surface-viewer-inset";
for (const name of ["main", "1", "2", "3"])
  APP_ALIASES[`cs-lane-${name}`] = name === "main" ? "text-viewer-primary" : "text-viewer-muted";
const extraRoles = {
  raise: "surface-canvas-selected",
  lifted: "surface-viewer-raised",
  hair: "border-default",
  fg2: "text-canvas-primary",
  faint: "text-canvas-muted",
  disabled: "text-canvas-muted",
  role: "code-function",
  "role-ink": "code-function",
  "role-on": "accent-fg",
  term: "surface-viewer-inset",
  "term-fg": "text-viewer-primary",
  "term-line": "border-default",
  prompt: "git-added",
  mtag: "code-number",
  "out-bg": "surface-sidebar-selected",
  "out-line": "border-default",
  lifeline: "border-default",
  shadow: "text-canvas-primary",
  "accent-wash": "severity-info-bg",
  "accent-line": "severity-info-border",
  "role-wash": "severity-info-bg",
  "role-line": "severity-info-border",
};
for (const [key, role] of Object.entries(extraRoles)) APP_ALIASES["ibx-" + key] = role;
for (const key of Object.keys(APP_ALIASES)) APP_ALIASES[key] = "compat-" + key;
APP_ALIASES["cs-runtime-hermes-text"] = "runtime-hermes-canvas-text";
APP_ALIASES["cs-runtime-hermes-bg"] = "runtime-hermes-canvas-bg";
APP_ALIASES["cs-dark-foreground"] = "compat-cs-dark-foreground";
APP_ALIASES["cs-dark-muted-foreground"] = "compat-cs-dark-muted-foreground";
APP_ALIASES["cs-dark-accent"] = "compat-cs-dark-accent";
APP_ALIASES["cs-dark-border"] = "compat-cs-dark-border";
APP_ALIASES["cs-dark-popover"] = "compat-cs-dark-popover";
APP_ALIASES["cs-terminal-tabbar-ink"] = "compat-cs-terminal-tabbar-ink";
for (const level of [1, 2, 3])
  APP_ALIASES["scratch-h" + level + "-color"] = "compat-tw-prose-headings";
const declarations = (map: Record<string, string>) =>
  Object.entries(map)
    .map(([k, v]) => `  --${k}: var(--theme-${v});`)
    .join("\n");

const TERMINAL_ALIASES: Record<string, string> = {
  popover: "surface-terminal-bg",
  accent: "severitySurface-terminal-info-fg",
  "accent-foreground": "surface-terminal-bg",
  ring: "severitySurface-terminal-info-fg",
};

export function surfaceCss(): string {
  return (
    SURFACES.map((s) => {
      const aliases: Record<string, string> = {
        // App-painted children must shed inherited terminal interaction colours.
        ...Object.fromEntries(
          Object.entries(TERMINAL_ALIASES).map(([name, role]) => [
            name,
            s === "terminal" ? role : APP_ALIASES[name],
          ]),
        ),
        foreground: `text-${s}-primary`,
        "muted-foreground": `text-${s}-muted`,
        background: `surface-${s}-bg`,
        muted: `surface-${s}-selected`,
        "secondary-foreground": `text-${s}-primary`,
        "cs-warning-text": `severitySurface-${s}-warning-fg`,
        "cs-warning-bg": `severitySurface-${s}-warning-bg`,
        "cs-runtime-claude-text": `severitySurface-${s}-warning-fg`,
        "cs-runtime-claude-bg": `severitySurface-${s}-warning-bg`,
        "cs-runtime-codex-text": `severitySurface-${s}-info-fg`,
        "cs-runtime-codex-bg": `severitySurface-${s}-info-bg`,
        "cs-runtime-opencode-text": `text-${s}-muted`,
        "cs-runtime-opencode-bg": `surface-${s}-selected`,
        "cs-runtime-hermes-text": `runtime-hermes-${s}-text`,
        "cs-runtime-hermes-bg": `runtime-hermes-${s}-bg`,
        destructive: `severitySurface-${s}-danger-fg`,
        "cs-danger-bg": `severitySurface-${s}-danger-bg`,
        "cs-danger-border": `severitySurface-${s}-danger-border`,
      };
      for (const k of Object.keys({
        needs: "warning",
        failed: "danger",
        busy: "info",
        done: "success",
      }))
        aliases[`cs-status-${k}`] = `status-${s}-${k}`;
      aliases["cs-status-idle"] = `status-${s}-idle`;
      aliases["cs-status-text"] = `status-${s}-text`;
      return `[data-surface="${s}"] {\n${declarations(aliases)}\n  --cs-outline-text: var(--foreground);\n  --cs-outline-bg: var(--background);\n  --cs-outline-hover: var(--theme-surface-${s}-hover);\n  --cs-outline-expanded: var(--theme-surface-${s}-selected);\n}`;
    }).join("\n\n") + "\n\n@layer base {\n  [data-surface] { color: var(--foreground); }\n}"
  );
}
export function aliasCss() {
  return `:root, .dark {\n${declarations(APP_ALIASES)}\n  --cs-app-outline-text: var(--foreground);\n  --cs-app-outline-bg: var(--background);\n  --cs-app-outline-hover: var(--muted);\n  --cs-app-outline-expanded: var(--muted);\n}\n\n.prose {\n${declarations(Object.fromEntries(Object.entries(APP_ALIASES).filter(([key]) => key.startsWith("tw-prose-"))))}\n}\n\n${surfaceCss()}\n\n[data-surface] .cs-outline-button { color: var(--cs-outline-text); background: var(--cs-outline-bg); border-color: var(--input); }\n[data-surface] .cs-outline-button:hover { background: var(--cs-outline-hover); }\n[data-surface] .cs-outline-button[aria-expanded="true"] { background: var(--cs-outline-expanded); }\n[data-surface] [data-severity="warning"] { color: var(--cs-warning-text); background: var(--cs-warning-bg); }\n[data-surface] [data-severity="error"] { color: var(--destructive); background: var(--cs-danger-bg); border-color: var(--cs-danger-border); }\n:focus-visible { outline: 2px solid var(--ring); outline-offset: 2px; }\n`;
}
