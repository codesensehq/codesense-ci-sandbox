#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// The frontend agent-CLI boundary, enforced: no TypeScript outside the one registry may
// enumerate the agent CLIs as a literal list. The list lives only in
// `src/shared/lib/agent-runtimes.ts` (mirroring the backend `AgentCli::ALL`); every other
// place reads it. A new `["claude", "codex"]`, `"claude" | "codex"` or
// `x === "claude" || x === "codex"` outside that file fails, so adding a CLI is exactly
// a registry entry plus its adapter, never a hunt through the UI.
//
//   node scripts/check-agent-cli-boundary-ts.mjs [--root <dir>] [--list]
//
// `--list` prints every hit, allowed or not. Test files and mock/fixture trees are skipped
// (they name real CLIs to drive them). The check is heuristic: it flags any non-test line
// that carries two or more agent-CLI string literals, which is the shape of a list.
import { lstatSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const REGISTRY = "src/shared/lib/agent-runtimes.ts";
const CLI = ["claude", "codex", "opencode", "hermes"];

// Places that name two or more CLIs for a reason other than enumerating the set, each with
// the most hits it may hold, so a new list in the same file still fails. Keep this short.
const ALLOWLIST = [
  {
    path: "src/tauri/commands.ts",
    max: 32,
    reason: "the hand-written wire/type mirror of the Rust agent-CLI types",
  },
  {
    path: "src/features/agents/lib/agent-display.ts",
    max: 1,
    reason: "starter account preference (only Claude/Codex have logins)",
  },
  {
    path: "src/features/viewer/components/pending-remote-opens.tsx",
    max: 1,
    reason: "per-CLI install command for the remotely-launchable CLIs",
  },
  {
    path: "src/shared/lib/agent-session-injection.ts",
    max: 2,
    reason: "Claude/Codex shell injection functions (OpenCode has none)",
  },
];

const isTestPath = (path) =>
  /\.(?:test|seam\.test)\.tsx?$/.test(path) ||
  path.includes("/__tests__/") ||
  path.includes("/__fixtures__/") ||
  path.includes("/mock/") ||
  path.includes("/mocks/");

/** Every line that names two or more distinct agent CLIs as string literals, outside a
 *  comment. Line numbers are the source's own. */
export function listLines(source) {
  const found = [];
  let block = false;
  const lines = source.split("\n");
  for (let i = 0; i < lines.length; i += 1) {
    let line = lines[i];
    // A block comment that starts and ends on one line drops only its own text.
    let next = line.replace(/\/\*.*?\*\//g, "");
    if (block) {
      const end = line.indexOf("*/");
      if (end < 0) continue;
      next = line.slice(end + 2);
      block = false;
    }
    next = next.replace(/\/\*[\s\S]*$/, ""); // an unterminated block starts here
    if (line.includes("/*") && !/\/\*.*?\*\//.test(line) && next === "") block = true;
    next = next.replace(/\/\/.*$/, "");
    const named = CLI.filter((cli) => new RegExp(`["']${cli}["']`).test(next));
    if (named.length >= 2) found.push({ line: i + 1, named });
  }
  return found;
}

function* tsFiles(dir) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (lstatSync(path).isDirectory()) {
      if (!name.startsWith(".") && name !== "node_modules") yield* tsFiles(path);
    } else if (/\.tsx?$/.test(name)) {
      yield path;
    }
  }
}

/** The check over `root`: `{ violations, allowed, over, stale, counts }`. */
export function check(root, allowlist = ALLOWLIST) {
  const counts = new Map();
  const allowed = [];
  const violations = [];
  for (const file of tsFiles(join(root, "src"))) {
    const path = relative(root, file).split(sep).join("/");
    if (path === REGISTRY || isTestPath(path)) continue;
    const source = readFileSync(file, "utf8");
    const found = listLines(source);
    if (!found.length) continue;
    counts.set(path, found.length);
    const entry = allowlist.find((a) => a.path === path);
    const out = entry && found.length <= entry.max ? allowed : violations;
    for (const hit of found) out.push({ path, line: hit.line, named: hit.named });
  }
  const over = allowlist.filter((a) => (counts.get(a.path) ?? 0) > a.max);
  const stale = allowlist.filter((a) => !counts.has(a.path));
  return { violations, allowed, over, stale, counts };
}

const isMain = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const args = process.argv.slice(2);
  const rootAt = args.indexOf("--root");
  const root =
    rootAt >= 0
      ? resolve(args[rootAt + 1])
      : resolve(dirname(fileURLToPath(import.meta.url)), "..");
  const { violations, allowed, over, stale } = check(root);
  if (args.includes("--list")) {
    for (const v of [...allowed, ...violations])
      console.log(`${v.path}:${v.line}: ${v.named.join(", ")}`);
  }
  for (const v of violations) {
    console.error(
      `${v.path}:${v.line}: names a literal agent-CLI list (${v.named.join(", ")}) outside ${REGISTRY}`,
    );
  }
  for (const a of over) {
    console.error(`${a.path}: allowlisted for ${a.max} CLI lists but holds ${counts.get(a.path)}`);
  }
  for (const a of stale) {
    console.error(`${a.path}: allowlisted but names no CLI list (or is gone); remove its entry`);
  }
  if (violations.length || over.length || stale.length) {
    console.error(
      "agent-cli-boundary-ts: FAILED — read src/shared/lib/agent-runtimes.ts instead of naming the CLIs; see scripts/check-agent-cli-boundary-ts.mjs",
    );
    process.exit(1);
  }
}
