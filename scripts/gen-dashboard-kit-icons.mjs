// SPDX-License-Identifier: MIT
// Regenerates the inline Lucide icon set the dashboard kit ships to every live
// doc (`window.codesense.kit.icon(name)`). The icons come from the app's own
// `lucide-react` (ISC), so a doc draws the same glyphs the built-in panes do.
//
//   node scripts/gen-dashboard-kit-icons.mjs          rewrite the block in kit.js
//   node scripts/gen-dashboard-kit-icons.mjs --check  fail if kit.js is stale
//
// To add an icon: append its kebab-case Lucide name to KIT_ICONS and rerun.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
export const KIT_JS = join(ROOT, "src-tauri/src/http/routes/dashboard_kit/kit.js");
const START = "/*@icons-start*/";
const END = "/*@icons-end*/";

/** The curated set: the glyphs the built-in panes use for agents, stories,
 *  routines, machines and common chrome. */
export const KIT_ICONS = [
  "activity",
  "alarm-clock",
  "archive",
  "arrow-up-right",
  "ban",
  "bot",
  "calendar",
  "calendar-clock",
  "chart-network",
  "check",
  "chevron-down",
  "chevron-right",
  "circle",
  "circle-alert",
  "circle-check-big",
  "circle-dashed",
  "circle-dot",
  "circle-help",
  "circle-play",
  "circle-x",
  "clock",
  "crosshair",
  "crown",
  "external-link",
  "eye",
  "eye-off",
  "file-text",
  "file-code",
  "file",
  "image",
  "folder-open",
  "filter",
  "folder",
  "git-branch",
  "git-commit-horizontal",
  "git-fork",
  "git-pull-request",
  "history",
  "inbox",
  "info",
  "laptop",
  "layout-list",
  "link",
  "list",
  "list-filter",
  "lock",
  "layers",
  "list-todo",
  "list-tree",
  "loader-circle",
  "mail",
  "maximize",
  "message-circle-question",
  "minus",
  "monitor",
  "network",
  "pause",
  "pin",
  "play",
  "plus",
  "refresh-cw",
  "repeat",
  "rotate-ccw",
  "scroll-text",
  "search",
  "send",
  "server",
  "settings",
  "shield-check",
  "square",
  "square-kanban",
  "terminal",
  "timer",
  "triangle-alert",
  "user",
  "users",
  "wifi-off",
  "workflow",
  "x",
  "corner-left-up",
  "arrow-left",
  "zap",
];

const escapeAttr = (v) =>
  String(v).replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;");

/** One icon's inner SVG markup, from lucide-react's `__iconNode`. */
export function iconMarkup(iconNode) {
  return iconNode
    .map(([tag, attrs]) => {
      const a = Object.entries(attrs)
        .filter(([k]) => k !== "key")
        .map(([k, v]) => ` ${k}="${escapeAttr(v)}"`)
        .join("");
      return `<${tag}${a}/>`;
    })
    .join("");
}

/** `{name: markup}` for every kit icon, read from the installed lucide-react. */
export async function loadIcons() {
  const out = {};
  for (const name of KIT_ICONS) {
    const dir = join(ROOT, "node_modules/lucide-react/dist/esm/icons");
    let file = join(dir, `${name}.js`);
    // A renamed icon's old name is a re-export of the new one.
    const alias = /export \{ default \} from '\.\/([a-z0-9-]+)\.js'/.exec(
      readFileSync(file, "utf8"),
    );
    if (alias) file = join(dir, `${alias[1]}.js`);
    const mod = await import(pathToFileURL(file).href);
    if (!mod.__iconNode) throw new Error(`lucide icon ${name} has no __iconNode`);
    out[name] = iconMarkup(mod.__iconNode);
  }
  return out;
}

/** The icon object literal as it appears between the markers in kit.js. */
export function renderBlock(icons) {
  const lines = Object.keys(icons)
    .sort()
    .map((name) => `    ${JSON.stringify(name)}: ${JSON.stringify(icons[name])},`);
  return `${START}{\n${lines.join("\n")}\n  }${END}`;
}

export function replaceBlock(src, block) {
  const a = src.indexOf(START);
  const b = src.indexOf(END);
  if (a < 0 || b < a) throw new Error("kit.js: icon markers not found");
  return src.slice(0, a) + block + src.slice(b + END.length);
}

async function main() {
  const check = process.argv.includes("--check");
  const src = readFileSync(KIT_JS, "utf8");
  const next = replaceBlock(src, renderBlock(await loadIcons()));
  if (check) {
    if (next !== src) {
      console.error("kit.js icons are stale: run node scripts/gen-dashboard-kit-icons.mjs");
      process.exit(1);
    }
    return;
  }
  writeFileSync(KIT_JS, next);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  await main();
}
