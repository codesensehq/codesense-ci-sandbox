// SPDX-License-Identifier: MIT
// Splices the one shared source of the dashboard patterns (dashboard_kit/kit-core.js) into the two hosts that ship it:
// the in-app kit (dashboard_kit/kit.js) and the hosted toolkit (toolkit/cs-live.js). Same code, same API, one toolkit version.
//
//   node scripts/gen-dashboard-kit-core.mjs          rewrite the block in both files
//   node scripts/gen-dashboard-kit-core.mjs --check  fail if either file is stale
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
export const CORE_JS = join(ROOT, "src-tauri/src/http/routes/dashboard_kit/kit-core.js");
export const HOSTS = [
  join(ROOT, "src-tauri/src/http/routes/dashboard_kit/kit.js"),
  join(ROOT, "src-tauri/src/dashboard_publish/toolkit/cs-live.js"),
];
const START = "/*@kit-core-start*/";
const END = "/*@kit-core-end*/";

export function spliced(src, core) {
  const a = src.indexOf(START);
  const b = src.indexOf(END);
  if (a < 0 || b < a) throw new Error("kit core markers not found");
  return `${src.slice(0, a)}${START}\n${core}  ${END}${src.slice(b + END.length)}`;
}

function main() {
  const check = process.argv.includes("--check");
  const core = readFileSync(CORE_JS, "utf8");
  for (const file of HOSTS) {
    const src = readFileSync(file, "utf8");
    const next = spliced(src, core);
    if (check) {
      if (next !== src) {
        console.error(`${file} is stale: run node scripts/gen-dashboard-kit-core.mjs`);
        process.exit(1);
      }
    } else writeFileSync(file, next);
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) main();
