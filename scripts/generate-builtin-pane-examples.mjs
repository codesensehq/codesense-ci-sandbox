// SPDX-License-Identifier: MIT
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { BUILTIN_PANE_EXAMPLES } from "./lib/builtin-panes.mjs";
import { isMain } from "./lib/is-main.mjs";

export const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

export function exampleBytes(root, pane) {
  return Buffer.concat([
    Buffer.from(
      `<!-- Generated from ${pane.source}. Fork me into your own pane; do not edit this read-only example. -->\n`,
    ),
    readFileSync(resolve(root, pane.source)),
  ]);
}

export function generate(root = ROOT, { check = false } = {}) {
  const outputs = new Map();
  for (const pane of BUILTIN_PANE_EXAMPLES) {
    const bytes = exampleBytes(root, pane);
    const path = resolve(root, pane.example);
    if (check) {
      if (!existsSync(path) || !readFileSync(path).equals(bytes))
        throw new Error(`Stale generated file: ${pane.example}; run pnpm gen:docs`);
    } else {
      mkdirSync(dirname(path), { recursive: true });
      writeFileSync(path, bytes);
    }
    outputs.set(pane.example, bytes);
  }
  return outputs;
}

if (isMain(import.meta.url)) {
  const rootAt = process.argv.indexOf("--root");
  generate(rootAt < 0 ? ROOT : process.argv[rootAt + 1], {
    check: process.argv.includes("--check"),
  });
  console.log("built-in pane examples: " + (process.argv.includes("--check") ? "ok" : "generated"));
}
