// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

/** Local ESM dependencies, including the inventory helper itself. */
export function scriptInputs(url, seen = new Set()) {
  const path = fileURLToPath(url);
  if (seen.has(path)) return [];
  seen.add(path);
  const source = readFileSync(path, "utf8");
  for (const [, specifier] of source.matchAll(/(?:from\s*|import\s*)["'](\.[^"']+)["']/g)) {
    scriptInputs(new URL(specifier, url), seen);
  }
  return [...seen];
}

export function listInputs(url, inputs) {
  const root = resolve(dirname(fileURLToPath(url)), "..");
  return [
    ...new Set([
      ...scriptInputs(url),
      resolve(root, "package.json"),
      resolve(root, "pnpm-lock.yaml"),
      ...inputs,
    ]),
  ].sort();
}
