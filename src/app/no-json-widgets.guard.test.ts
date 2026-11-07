// SPDX-License-Identifier: MIT
import { lstatSync, readdirSync, readFileSync } from "node:fs";
import { join, relative, resolve } from "node:path";

import { describe, expect, it } from "vitest";

/**
 * Custom right panes are HTML files; the JSON widget system is gone. This guard
 * keeps it gone: no source under `src/` or `src-tauri/src/` may name a widget
 * panel id, the widget spec, the widget registration tool, the widget grant
 * ledger or the old `config/widgets` home.
 */

const REPO = resolve(__dirname, "../..");
const ROOTS = ["src", "src-tauri/src"];
const SELF = relative(REPO, __filename);
const SKIP_DIRS = new Set(["node_modules", "target", "target-ci", "dist"]);
const TEXT = /\.(ts|tsx|js|mjs|cjs|rs|json|html|css|md|toml|yaml|yml|sh)$/;

const BANNED: Array<[string, RegExp]> = [
  ["a `widget:` panel id", /["'`]widget:/],
  ["WidgetSpec", /WidgetSpec/],
  ["register_widget", /register_widget/],
  ["registerWidget", /registerWidget/],
  ["widget_grant", /widget_grant/],
  ["widgetGrant", /widgetGrant/],
  ["config/widgets", /config\/widgets/],
];

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    if (SKIP_DIRS.has(name)) continue;
    const abs = join(dir, name);
    // lstat: a symlink is never followed, so a dangling or cyclic link can't break the scan.
    const st = lstatSync(abs);
    if (st.isSymbolicLink()) continue;
    if (st.isDirectory()) out.push(...sourceFiles(abs));
    else if (TEXT.test(name)) out.push(abs);
  }
  return out;
}

describe("the JSON widget system stays deleted", () => {
  it("no source names a widget panel id, spec, registration, grant or config home", () => {
    const hits: string[] = [];
    for (const root of ROOTS) {
      for (const abs of sourceFiles(join(REPO, root))) {
        const rel = relative(REPO, abs);
        if (rel === SELF) continue;
        const lines = readFileSync(abs, "utf8").split("\n");
        lines.forEach((line, i) => {
          for (const [label, re] of BANNED) {
            if (re.test(line)) hits.push(`${rel}:${i + 1} names ${label}`);
          }
        });
      }
    }
    expect(hits).toEqual([]);
  });
});
