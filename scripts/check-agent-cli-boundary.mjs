// SPDX-License-Identifier: MIT
// The agent-CLI boundary, enforced: Rust core never branches on which agent CLI
// it runs. Every CLI fact lives in its adapter
// (`crates/codesense-pty-core/src/agent_cli/`) or in the desktop's extension of it
// (`src/agents/adapter.rs`, `src/agents/adapter/`); core asks the adapter.
//
// Fails when Rust outside those places, test code aside, names
// `AgentCli::Claude`/`AgentCli::Codex`/`AgentCli::OpenCode` (or a login provider's
// `AgentProvider::Claude`/`AgentProvider::Codex`), a snake_case name carrying one
// (`opencode_plugin_url`), brings them into scope bare or under
// an alias, or holds a CLI id as a string literal. Comments
// are not code and are skipped; so are test files and `#[cfg(test)]` items, which name
// real CLIs to drive them.
//
//   node scripts/check-agent-cli-boundary.mjs [--root <dir>] [--list]
//
// `--root` scans another tree (the check's own tests use it on fixtures);
// `--list` prints every hit, allowed or not.
import { lstatSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join, posix, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

// The places a CLI's facts belong. Paths are relative to `src-tauri/`.
const ADAPTER_HOMES = [
  "crates/codesense-pty-core/src/agent_cli/",
  // The enum itself, and its wire spellings.
  "crates/codesense-host-proto/src/agent_cli.rs",
  // The desktop extension: account requirement, runner recipe, starters.
  "src/agents/adapter.rs",
  "src/agents/adapter/",
];

// Code that names a CLI for a reason other than branching on the CLI it runs,
// each with the most hits it may hold, so a new branch in the same file still
// fails. Keep this short; move a CLI fact behind the adapter instead of adding a
// line here.
export const ALLOWLIST = [
  // Login providers: their own ids (their serde spelling, persisted in the account
  // store), and each provider's login flow. A login provider is not a CLI: OpenCode will
  // be one CLI with several.
  { path: "src/install/agent_account_store.rs", max: 8, reason: "login-provider ids" },
  { path: "src/commands/agent_accounts.rs", max: 6, reason: "per-provider login flows" },
  // Claude's own folder pre-trust, reached only through its desktop extension's
  // `pretrust`: it trusts the folder in each Claude login's home.
  { path: "src/agents/workspace_trust.rs", max: 1, reason: "Claude login homes" },
  // AUTH1, the remote agent task harness, pins its CLIs' own recipes; it moves behind
  // the adapter in SW-AN-55-CODEX with its version policy.
  { path: "src/mcp/remote_agent_task/claude.rs", max: 1, reason: "AUTH1 (SW-AN-55-CODEX)" },
  { path: "src/mcp/remote_agent_task/harness.rs", max: 1, reason: "AUTH1 (SW-AN-55-CODEX)" },
  { path: "src/mcp/remote_agent_task/mod.rs", max: 1, reason: "AUTH1 (SW-AN-55-CODEX)" },
  { path: "src/mcp/remote_agent_task/runtime.rs", max: 8, reason: "AUTH1 (SW-AN-55-CODEX)" },
  { path: "src/mcp/local_mcp_relay/native.rs", max: 4, reason: "AUTH1 (SW-AN-55-CODEX)" },
];

const SCAN_ROOTS = ["src", "crates"];

/**
 * Splits Rust source into tokens enough to tell code from comments and strings:
 * `{ kind: "string", text, at }` for a string literal (its source text, quotes
 * included), `{ kind: "punct", text, at }` for `{ } ; #`, and `{ kind: "code",
 * text, at }` for runs of anything else, `at` being the source offset. Comments
 * are dropped.
 */
export function tokenize(source) {
  const tokens = [];
  let i = 0;
  let code = "";
  let codeAt = 0;
  const flush = () => {
    if (code.trim()) tokens.push({ kind: "code", text: code, at: codeAt });
    code = "";
  };
  const push = (kind, text) => {
    flush();
    tokens.push({ kind, text, at: i });
    i += text.length;
  };
  const isIdent = (c) => c !== undefined && /\w/.test(c);
  const rawOpen = /b?r(#*)"/y;
  while (i < source.length) {
    const c = source[i];
    const next = source[i + 1];
    if (c === "/" && next === "/") {
      const end = source.indexOf("\n", i);
      flush();
      i = end < 0 ? source.length : end;
      continue;
    }
    if (c === "/" && next === "*") {
      let depth = 1;
      let j = i + 2;
      while (j < source.length && depth > 0) {
        if (source[j] === "/" && source[j + 1] === "*") ((depth += 1), (j += 2));
        else if (source[j] === "*" && source[j + 1] === "/") ((depth -= 1), (j += 2));
        else j += 1;
      }
      flush();
      i = j;
      continue;
    }
    // Raw strings: r"…", r#"…"#, br#"…"#.
    rawOpen.lastIndex = i;
    const raw = (c === "r" || c === "b") && !isIdent(source[i - 1]) ? rawOpen.exec(source) : null;
    if (raw) {
      const close = `"${raw[1]}`;
      const end = source.indexOf(close, i + raw[0].length);
      push("string", source.slice(i, end < 0 ? source.length : end + close.length));
      continue;
    }
    if (c === '"' || (c === "b" && next === '"' && !isIdent(source[i - 1]))) {
      let j = source.indexOf('"', i) + 1;
      while (j < source.length && source[j] !== '"') j += source[j] === "\\" ? 2 : 1;
      push("string", source.slice(i, j + 1));
      continue;
    }
    // A char literal ('x', '\n', '\u{..}', '"'), not a lifetime ('a).
    if (c === "'") {
      const char = /^'(\\(u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)|[^\\'])'/u.exec(
        source.slice(i, i + 12),
      );
      if (char) {
        flush();
        i += char[0].length;
        continue;
      }
    }
    if (c === "{" || c === "}" || c === ";" || c === "#") {
      push("punct", c);
      continue;
    }
    if (!code) codeAt = i;
    code += c;
    i += 1;
  }
  flush();
  return tokens;
}

/** The top-level comma-separated terms of a `cfg` predicate list. */
function terms(list) {
  const out = [];
  let depth = 0;
  let term = "";
  for (const c of list) {
    if (c === "," && depth === 0) {
      out.push(term.trim());
      term = "";
      continue;
    }
    depth += c === "(" ? 1 : c === ")" ? -1 : 0;
    term += c;
  }
  if (term.trim()) out.push(term.trim());
  return out;
}

/**
 * Whether an attribute's text (`[cfg(test)]`, after its `#` or `#!`) compiles its item for
 * tests only: `cfg(test)`, or `cfg(all(…))` with a bare `test` among its terms. `not(test)`
 * and `any(test, …)` compile outside tests too.
 */
export function isTestCfg(attr) {
  const m = /^\s*!?\s*\[\s*cfg\s*\(([\s\S]*)\)\s*\]/.exec(attr);
  if (!m) return false;
  const [predicate] = terms(m[1]);
  if (predicate === "test") return true;
  const all = /^all\s*\(([\s\S]*)\)$/.exec(predicate ?? "");
  return Boolean(all) && terms(all[1]).includes("test");
}

/**
 * The attribute after the `#` at `tokens[at]`: its text (string literals in it included),
 * and where it ends, as the token holding its closing `]` and the offset just past it.
 */
function attribute(tokens, at) {
  let depth = 0;
  let text = "";
  for (let j = at + 1; j < tokens.length; j += 1) {
    const u = tokens[j];
    if (u.kind === "string") {
      text += u.text;
      continue;
    }
    if (u.kind !== "code") break;
    for (let k = 0; k < u.text.length; k += 1) {
      const c = u.text[k];
      text += c;
      if (c === "[") depth += 1;
      else if (c === "]" && --depth === 0) return { text, token: j, offset: k + 1 };
    }
  }
  return { text, token: at + 1, offset: tokens[at + 1]?.text.length ?? 0 };
}

/**
 * Drops each item under a test-only `cfg` attribute ([`isTestCfg`]): up to a `;` or `,`
 * outside any brace, bracket or generic list (not the one in `[u8; 3]` or `<A, B>`), the
 * brace that closes the first one it opened, or the bracket or brace that closes what holds
 * it (a gated field, variant or match arm). What follows the item in the same token is
 * kept.
 */
export function withoutTestItems(tokens) {
  const kept = [];
  for (let i = 0; i < tokens.length; i += 1) {
    const t = tokens[i];
    const attr = t.kind === "punct" && t.text === "#" ? attribute(tokens, i) : null;
    if (!attr || !isTestCfg(attr.text)) {
      kept.push(t);
      continue;
    }
    let depth = 0;
    let nest = 0;
    // Generic lists (`impl<A, B>`, `Vec<u8>`): a `<` straight after a name or `::`, never a
    // comparison or shift, which rustfmt spaces.
    let angle = 0;
    let j = attr.token;
    let rest = null;
    scan: for (; j < tokens.length; j += 1) {
      const u = tokens[j];
      if (u.kind === "code") {
        const from = j === attr.token ? attr.offset : 0;
        for (let k = from; k < u.text.length; k += 1) {
          const c = u.text[k];
          if (c === "(" || c === "[") nest += 1;
          else if (c === ")" || c === "]") nest -= 1;
          else if (depth === 0 && c === "<" && /[\w:]/.test(u.text[k - 1] ?? "")) angle += 1;
          else if (depth === 0 && c === ">" && angle > 0 && !/[-=]/.test(u.text[k - 1] ?? ""))
            angle -= 1;
          if (depth === 0 && (nest < 0 || (nest === 0 && angle === 0 && c === ","))) {
            const end = c === "," ? k + 1 : k;
            rest = { kind: "code", text: u.text.slice(end), at: u.at + end };
            break scan;
          }
        }
        continue;
      }
      if (u.kind !== "punct") continue;
      if (u.text === "{") depth += 1;
      else if (u.text === "}") {
        if (depth === 0) {
          j -= 1; // the enclosing item's own brace stays
          break;
        }
        depth -= 1;
        if (depth === 0) break;
      } else if (u.text === ";" && depth === 0 && nest <= 0) break;
    }
    if (rest?.text) kept.push(rest);
    i = j;
  }
  return kept;
}

/**
 * The out-of-line modules `source` declares under `#[cfg(test)]`: `{ name, path }`,
 * `path` being its `#[path]` attribute's value, if it has one.
 */
export function testModules(source, tokens = tokenize(source)) {
  const text = tokens.map((t) => t.text).join(" ");
  const found = [];
  const decl = /((?:#\s*\[[^\]]*\]\s*)+)(?:pub\s*(?:\([^)]*\))?\s*)?mod\s+(\w+)\s*;/g;
  for (const m of text.matchAll(decl)) {
    if (!m[1].split("#").some((attr) => isTestCfg(attr))) continue;
    const path = /#\s*\[\s*path\s*=\s*"([^"]*)"/.exec(m[1])?.[1];
    found.push({ name: m[2], path });
  }
  return found;
}

// A variant by its path (`AgentCli::Claude`, `Self::Claude` in an `impl` of the enum), or
// brought into scope bare (`use AgentCli::{Claude}`, `use AgentCli::*`), where a later
// `Claude` names it unseen. An alias (`AgentCli as Cli`, `type Cli = AgentCli`,
// `AgentCli::{self as Cli}`) hides every later use, so it is flagged too; a qualified
// path (`<AgentCli as FromStr>`) is not an alias.
const VARIANT = new RegExp(
  [
    String.raw`\bAgentCli\s*::\s*(?:Claude\b|Codex\b|OpenCode\b|Hermes\b|\*|\{[^}]*\b(?:Claude|Codex|OpenCode|Hermes|self\s+as)\b[^}]*\})`,
    String.raw`\bAgentProvider\s*::\s*(?:Claude\b|Codex\b|\*|\{[^}]*\b(?:Claude|Codex|self\s+as)\b[^}]*\})`,
    String.raw`(?<!<\s*)\b(?:AgentCli|AgentProvider)\s+as\s+\w+`,
    String.raw`\btype\s+\w+\s*=\s*(?:\w+::)*(?:AgentCli|AgentProvider)\b`,
    String.raw`\bSelf\s*::\s*(?:Claude|Codex|OpenCode|Hermes)\b`,
  ].join("|"),
  "g",
);
const LITERALS = new Set(["claude", "codex", "opencode", "hermes"]);
// A snake_case name that carries OpenCode's (`opencode_plugin_url`): the same branch as the
// variant, spelled as a function, hiding the CLI's fact in shared code. Only OpenCode is
// held to it: Claude's and Codex's older names (`claude_dir`, `claude_session`) are their
// own infrastructure and predate the boundary.
const IDENTIFIER = /\b(?:opencode_[a-z0-9_]+|[a-z0-9_]+_opencode)\b/g;

/** A string literal's text without its quotes and prefix (`b"…"`, `r#"…"#`). */
const unquoted = (literal) => literal.replace(/^b?r?#*"/, "").replace(/"#*$/, "");

/** Whether `source` opens with an inner `#![cfg(test)]`: the whole file is test code. */
export function testOnlyFile(source, tokens = tokenize(source)) {
  let i = 0;
  while (tokens[i]?.text === "#") {
    const attr = attribute(tokens, i);
    if (!attr.text.trimStart().startsWith("!")) return false;
    if (isTestCfg(attr.text)) return true;
    // Only inner attributes may come before it.
    if (tokens[attr.token].text.slice(attr.offset).trim()) return false;
    i = attr.token + 1;
  }
  return false;
}

/** Every place `source` names a CLI in non-test code: `{ line, text }`. */
export function hits(source, tokens = tokenize(source)) {
  const starts = [0];
  for (let at = source.indexOf("\n"); at >= 0; at = source.indexOf("\n", at + 1))
    starts.push(at + 1);
  const lineOf = (at) => {
    let lo = 0;
    let hi = starts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (starts[mid] <= at) lo = mid;
      else hi = mid - 1;
    }
    return lo + 1;
  };
  const found = [];
  // The kept code alone, each piece at its own offset (everything else blanked, its
  // newlines kept), so a match may span tokens.
  const blank = (text) => text.replace(/[^\n]/g, " ");
  const parts = [];
  let end = 0;
  for (const t of withoutTestItems(tokens)) {
    parts.push(blank(source.slice(end, t.at)));
    if (t.kind === "string") {
      if (LITERALS.has(unquoted(t.text))) found.push({ line: lineOf(t.at), text: t.text });
      parts.push(blank(t.text));
    } else {
      parts.push(t.text);
    }
    end = t.at + t.text.length;
  }
  const code = parts.join("");
  for (const m of code.matchAll(VARIANT)) {
    found.push({ line: lineOf(m.index), text: m[0].replace(/\s+/g, " ") });
  }
  for (const m of code.matchAll(IDENTIFIER)) {
    found.push({ line: lineOf(m.index), text: m[0] });
  }
  return found.sort((x, y) => x.line - y.line);
}

function* rustFiles(dir) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    // Symlinks are not followed: a link cycle would never end.
    if (lstatSync(path).isDirectory()) {
      if (!name.startsWith("target") && name !== "node_modules") yield* rustFiles(path);
    } else if (name.endsWith(".rs")) yield path;
  }
}

/** The file a module `name` declared in `parent` lives in, and its submodules' dir. */
function moduleHome(parent, { name, path }) {
  const dir = parent.slice(0, parent.lastIndexOf("/") + 1);
  if (path)
    return {
      file: posix.normalize(dir + path),
      dir: posix.normalize(dir + path).replace(/(\/mod)?\.rs$/, "/"),
    };
  const stem = parent.slice(dir.length).replace(/\.rs$/, "");
  const base = ["mod", "lib", "main"].includes(stem) ? dir : `${dir}${stem}/`;
  return { file: `${base}${name}.rs`, alt: `${base}${name}/mod.rs`, dir: `${base}${name}/` };
}

/**
 * The check over `tauri` (a `src-tauri` dir): `{ violations, allowed, stale, counts }`.
 * Code is test code when it sits under `#[cfg(test)]`, in a module declared under it
 * (and that module's own submodules), or in a crate's `tests/`, `benches/` or
 * `examples/`.
 */
export function check(tauri, allowlist = ALLOWLIST) {
  const sources = new Map();
  for (const top of SCAN_ROOTS) {
    let files;
    try {
      files = [...rustFiles(join(tauri, top))];
    } catch {
      continue;
    }
    for (const file of files) {
      const source = readFileSync(file, "utf8");
      sources.set(relative(tauri, file).split(sep).join("/"), { source, tokens: tokenize(source) });
    }
  }
  const testFiles = new Set();
  const testDirs = [/^crates\/[^/]+\/(tests|benches|examples)\//];
  for (const [path, { source, tokens }] of sources) {
    for (const module of testModules(source, tokens)) {
      const home = moduleHome(path, module);
      testFiles.add(home.file);
      if (home.alt) testFiles.add(home.alt);
      testDirs.push(home.dir);
    }
  }
  const isTest = (path) =>
    testFiles.has(path) ||
    testDirs.some((d) => (typeof d === "string" ? path.startsWith(d) : d.test(path)));
  // A file a test file `include!`s is test code too.
  for (let grew = true; grew; ) {
    grew = false;
    for (const [path, { source }] of sources) {
      if (!isTest(path)) continue;
      const dir = path.slice(0, path.lastIndexOf("/") + 1);
      for (const m of source.matchAll(/\binclude!\s*\(\s*"([^"]+)"\s*\)/g)) {
        const file = posix.normalize(dir + m[1]);
        if (!testFiles.has(file)) (testFiles.add(file), (grew = true));
      }
    }
  }

  const violations = [];
  const allowed = [];
  const counts = new Map();
  for (const [path, { source, tokens }] of sources) {
    if (ADAPTER_HOMES.some((home) => path === home || path.startsWith(home))) continue;
    if (isTest(path) || testOnlyFile(source, tokens)) continue;
    const found = hits(source, tokens);
    if (!found.length) continue;
    const entry = allowlist.find((e) => e.path === path);
    counts.set(path, found.length);
    const out = entry && found.length <= entry.max ? allowed : violations;
    for (const h of found) out.push({ path, ...h });
  }
  if (!sources.size) throw new Error(`no Rust sources under ${tauri}`);
  const held = (e) => counts.get(e.path) ?? 0;
  const over = allowlist.filter((e) => held(e) > e.max);
  const stale = allowlist.filter((e) => held(e) < e.max);
  return { violations, allowed, over, stale, counts };
}

const isMain = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
  const args = process.argv.slice(2);
  const rootAt = args.indexOf("--root");
  const known = new Set(["--root", "--list"]);
  if (
    (rootAt >= 0 && (!args[rootAt + 1] || args[rootAt + 1].startsWith("--"))) ||
    args.some((arg, at) => !known.has(arg) && (rootAt < 0 || at !== rootAt + 1))
  ) {
    console.error("usage: check-agent-cli-boundary.mjs [--root <dir>] [--list]");
    process.exit(2);
  }
  const root = rootAt >= 0 ? resolve(args[rootAt + 1]) : repo;
  const { violations, allowed, over, stale, counts } = check(join(root, "src-tauri"));
  if (args.includes("--list")) {
    for (const h of [...allowed, ...violations]) console.log(`${h.path}:${h.line}: ${h.text}`);
  }
  for (const h of violations) {
    console.error(`${h.path}:${h.line}: names ${h.text} outside the agent-CLI adapter`);
  }
  for (const e of over) {
    console.error(`${e.path}: allowlisted for ${e.max} CLI names but holds ${counts.get(e.path)}`);
  }
  for (const e of stale) {
    const held = counts.get(e.path) ?? 0;
    console.error(
      held
        ? `${e.path}: allowlisted for ${e.max} CLI names but holds ${held}; lower its max`
        : `${e.path}: allowlisted but names no CLI (or is gone); remove its entry`,
    );
  }
  if (violations.length || stale.length) {
    console.error(
      "agent-cli-boundary: FAILED — ask the CLI's adapter (agent_cli::adapter) instead of naming the CLI; see scripts/check-agent-cli-boundary.mjs",
    );
    process.exit(1);
  }
}
