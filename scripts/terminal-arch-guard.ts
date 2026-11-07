// SPDX-License-Identifier: MIT
/**
 * The frontend half of the terminal architecture guard: a text scan of the
 * production tree that fails if raw byte replay, a second terminal parser, or
 * an xterm owned outside the terminal manager comes back. The Rust half is
 * `src-tauri/crates/codesense-pty-core/tests/architecture_guard.rs`; both run
 * from `scripts/check-terminal-arch.mjs` (`npm run lint`, `quality:real`).
 *
 * Byte replay INSIDE the manager is not caught here: every display is built
 * from a `TerminalSnapshot` (the manager's construction contract and its
 * restore tests), which a text scan cannot see.
 *
 * Scanned: every `.ts`/`.tsx` under `<root>/src` (code and comments), and the
 * `package.json` files at `<root>` and under `<root>/src`. Excluded: tests
 * (`*.test.*`, `__tests__`), `__test-utils__`, `__fixtures__` and
 * `__guard_fixtures__`. `src-emulator` is outside `src` and builds its own
 * headless terminals by design.
 */
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { pathToFileURL } from "node:url";

export type Clause = "a" | "b" | "c" | "d" | "e";

export interface Violation {
  clause: Clause;
  file: string;
  line: number;
  text: string;
}

/** (a) Symbols of the deleted byte-replay and delta-recovery machinery. */
const DELETED_SYMBOLS =
  /ReplayBuffer|replay_tail|snapshot_delta|SnapshotDelta|[Rr]esnapshot|needs_cold_settle|\bpausedRef\b|dirtyWhileHidden|resumeStrategy|drainResumeDelta|rehydrateFromSnapshot|terminal-freshness|writeTerminalMarker/;
/** (b) Constructing a display terminal. */
const NEW_TERMINAL = /\bnew\s+Terminal\s*\(/;
/** (c) A value import of an xterm package (type-only imports are fine). */
const XTERM_VALUE_IMPORT =
  /^\s*(?:import\s+(?!type\b)[^;]*?from\s*|import\s*|export\s+(?!type\b)[^;]*?from\s*)["']@xterm\/|(?:\bimport|\brequire)\s*\(\s*["']@xterm\//;
/** (d) Writing bytes into a terminal. */
const TERMINAL_WRITE = /\b(?:term|terminal|xterm|xt)\s*[?!]?\.\s*write\s*\(/;
/** (e) Any terminal emulator other than xterm.js: one parser. */
const OTHER_EMULATORS = ["alacritty_terminal", "vt100", "wezterm-term", "vte"];

const MANAGER = "src/shared/terminal/terminal-manager.ts";
const TERMINAL_DIR = "src/shared/terminal/";
const EXCLUDED_DIRS = new Set([
  "node_modules",
  "__tests__",
  "__test-utils__",
  "__fixtures__",
  "__guard_fixtures__",
]);

function sources(dir: string, out: string[]): string[] {
  if (!existsSync(dir)) return out;
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) {
      if (!EXCLUDED_DIRS.has(name)) sources(path, out);
    } else if (/\.tsx?$/.test(name) && !/\.test\./.test(name)) {
      out.push(path);
    }
  }
  return out;
}

function manifests(root: string): string[] {
  const found: string[] = [];
  const walk = (dir: string) => {
    if (!existsSync(dir)) return;
    for (const name of readdirSync(dir)) {
      const path = join(dir, name);
      if (statSync(path).isDirectory()) {
        if (!EXCLUDED_DIRS.has(name)) walk(path);
      } else if (name === "package.json") {
        found.push(path);
      }
    }
  };
  if (existsSync(join(root, "package.json"))) found.push(join(root, "package.json"));
  walk(join(root, "src"));
  return found;
}

/** Every violation under `root`, in file then line order. */
export function scan(root: string): Violation[] {
  const violations: Violation[] = [];
  const posix = (path: string) => relative(root, path).split(sep).join("/");
  for (const path of sources(join(root, "src"), [])) {
    const file = posix(path);
    const lines = readFileSync(path, "utf8").split("\n");
    lines.forEach((text, index) => {
      const at = (clause: Clause) => violations.push({ clause, file, line: index + 1, text });
      if (DELETED_SYMBOLS.test(text)) at("a");
      if (file !== MANAGER && NEW_TERMINAL.test(text)) at("b");
      if (!file.startsWith(TERMINAL_DIR) && XTERM_VALUE_IMPORT.test(text)) at("c");
      if (file !== MANAGER && TERMINAL_WRITE.test(text)) at("d");
    });
  }
  for (const path of manifests(root)) {
    const manifest = JSON.parse(readFileSync(path, "utf8")) as Record<string, unknown>;
    for (const section of [
      "dependencies",
      "devDependencies",
      "optionalDependencies",
      "peerDependencies",
    ]) {
      const deps = (manifest[section] ?? {}) as Record<string, string>;
      for (const name of Object.keys(deps)) {
        if (OTHER_EMULATORS.includes(name)) {
          violations.push({ clause: "e", file: posix(path), line: 0, text: `${section}.${name}` });
        }
      }
    }
  }
  return violations;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const root = process.argv[2] ?? process.cwd();
  const violations = scan(root);
  for (const v of violations) {
    console.error(`terminal-arch (${v.clause}) ${v.file}:${v.line}: ${v.text.trim()}`);
  }
  if (violations.length > 0) process.exit(1);
  console.log("terminal-arch: frontend guard clean");
}
