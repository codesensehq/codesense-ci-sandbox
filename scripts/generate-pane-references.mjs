// SPDX-License-Identifier: MIT
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { builders, sourceTree } from "./lib/pane-references.mjs";
export const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export function generate(root = ROOT, { check = false, only } = {}) {
  const app = sourceTree(root);
  const outputs = Object.assign(
    {},
    ...Object.entries(builders)
      .filter(([name]) => !only || name === only)
      .map(([, build]) => build(app)),
  );
  for (const [path, text] of Object.entries(outputs)) {
    const dest = resolve(root, path);
    if (check) {
      if (!existsSync(dest) || readFileSync(dest, "utf8") !== text)
        throw new Error(`Stale generated file: ${path}; run pnpm gen:docs`);
    } else {
      mkdirSync(dirname(dest), { recursive: true });
      writeFileSync(dest, text);
    }
  }
  return outputs;
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const arg = (name) => {
    const i = process.argv.indexOf(name);
    return i < 0 ? undefined : process.argv[i + 1];
  };
  generate(arg("--root") ?? ROOT, { check: process.argv.includes("--check"), only: arg("--only") });
  console.log("pane references: " + (process.argv.includes("--check") ? "ok" : "generated"));
}
