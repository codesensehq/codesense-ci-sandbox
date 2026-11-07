#!/usr/bin/env node
// SPDX-License-Identifier: MIT
/**
 * generate-shortcuts.mjs — the keyboard shortcuts reference.
 *
 * Reads the authored shortcuts table in `codesense-docs/getting-started/content/
 * arc-n-reference/shortcuts.manifest.json` and emits
 * `codesense-docs/getting-started/content/generated/shortcuts.json`
 * for runtime consumption by `arc-n-reference/keyboard-shortcuts.html`.
 *
 * The manifest is authored by hand from the app's menu and shortcut registry
 * (`src-tauri/src/menu.rs`, `src/shared/lib/shortcuts/registry.ts`).
 *
 * Usage
 *   node scripts/generate-shortcuts.mjs           # --write
 *   node scripts/generate-shortcuts.mjs --check   # exit non-zero if drift
 */

import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(__dirname, "..");
const MANIFEST = resolve(
  ROOT,
  "codesense-docs/getting-started/content/arc-n-reference/shortcuts.manifest.json",
);
const OUT = resolve(ROOT, "codesense-docs/getting-started/content/generated/shortcuts.json");

function main() {
  const check = process.argv.includes("--check");
  if (!existsSync(MANIFEST)) {
    console.error("[generate-shortcuts] manifest missing:", MANIFEST);
    process.exit(1);
  }
  const manifest = JSON.parse(readFileSync(MANIFEST, "utf8"));

  const total = manifest.groups.reduce((acc, group) => acc + group.shortcuts.length, 0);

  const payload = {
    generatedAt: new Date().toISOString(),
    source: "arc-n-reference/shortcuts.manifest.json",
    totalCount: total,
    groups: manifest.groups,
  };

  const next = JSON.stringify(payload, null, 2) + "\n";

  if (check) {
    const prev = existsSync(OUT) ? readFileSync(OUT, "utf8") : "";
    // Strip generatedAt before compare — the field is time-sensitive by design.
    const prevStable = prev.replace(/"generatedAt":\s*"[^"]*"/, '"generatedAt":"X"');
    const nextStable = next.replace(/"generatedAt":\s*"[^"]*"/, '"generatedAt":"X"');
    if (prevStable !== nextStable) {
      console.error(
        "[generate-shortcuts] drift between manifest and generated/shortcuts.json. Rerun without --check to update.",
      );
      process.exit(1);
    }
    console.log("[generate-shortcuts] OK — no drift (", total, "shortcuts)");
    return;
  }

  writeFileSync(OUT, next);
  console.log("[generate-shortcuts] wrote", OUT, "(", total, "shortcuts)");
}

main();
