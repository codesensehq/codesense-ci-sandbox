#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// The docs' API examples, checked against the real API. A reader who copies an example from
// the in-app docs or the handbook should not get an error, so every code example must name
// real page verbs and real MCP arguments:
//
//   R1  `codesense.invoke('<n>'` / `cs.act('<n>'` names a page verb. An MCP tool there is an
//       agent-side call written as page code: write `tool { … }` or call its page verb.
//   R2  `codesense.<m>(` names a Surface member or a page verb.
//   R3  `tool { … }`, `tool({ … })`, `mcp.call("tool", { … })` and the `arguments` of
//       `codesense_call_tool` pass only that tool's parameters, by their canonical names (a
//       serde alias is reported with the name the schema uses). Required fields are not checked.
//
// Truth: generated/page-sdk.json (verbs, members), generated/mcp-tools.json (params) and the
// `alias = "…"` attributes in src-tauri/src/**/*.rs. Corpus: the docs content tree
// (.html .md .js .yml .yaml, minus generated/) and the handbook MDX when site/ is present (a
// tree without it, like the public export, checks only the handbook snapshot in the content
// tree). Code is the text of <code> and <pre> (HTML) or fenced and inline code (Markdown). Not
// examples, so skipped:
// HTML comments, and in HTML pages <script> (live page code), <style>, <template> and
// <textarea>. In HTML and Markdown, an element carrying data-api-lint="off" holds a
// deliberately wrong example and is skipped too.
//
//   node scripts/check-docs-api-examples.mjs [--root <dir> | --root=<dir>]
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseItems } from "./lib/rust.mjs";

const DEFAULT_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const CONTENT = "codesense-docs/getting-started/content";
const HANDBOOK = "site/src/content/handbook";
const ALIAS_DIR = "src-tauri/src";
// Page members the docs' own shared scripts define on `codesense`.
const EXTRA_MEMBERS = ["docs"];

// Every struct field's serde `alias` mapped to the field's serde names (its rename, or its
// identifier under the struct's rename_all). The caller keeps the one the tool's schema lists.
export function parseAliases(rustSources, names = []) {
  const aliases = new Map();
  for (const [i, src] of rustSources.entries()) {
    let items;
    try {
      items = parseItems(src);
    } catch (e) {
      throw new Error(`${names[i] ?? `source ${i}`}: ${e.message}`);
    }
    for (const item of items.values()) {
      if (item.kind !== "struct") continue;
      for (const { serde, name } of item.members) {
        if (!serde.alias) continue;
        const canon = aliases.get(serde.alias) ?? [];
        if (!canon.includes(name)) aliases.set(serde.alias, [...canon, name]);
      }
    }
  }
  return aliases;
}

export function loadTruth(root = DEFAULT_ROOT) {
  const gen = (name) => JSON.parse(readFileSync(join(root, CONTENT, "generated", name), "utf8"));
  const sdk = gen("page-sdk.json");
  const verbs = new Set(sdk.verbs.map((v) => v.name));
  const members = new Set([
    ...sdk.members.filter((m) => m.owner !== "Kit").map((m) => m.name),
    ...verbs,
    ...EXTRA_MEMBERS,
  ]);
  const twinOf = new Map();
  for (const v of sdk.verbs) {
    if (!v.mcpTwin) continue;
    if (!twinOf.has(v.mcpTwin)) twinOf.set(v.mcpTwin, []);
    twinOf.get(v.mcpTwin).push(v.name);
  }
  const tools = new Map(
    gen("mcp-tools.json").tools.map((t) => [
      t.name,
      new Set(Object.keys(t.params?.properties ?? {})),
    ]),
  );
  // Parse only the Rust files that declare an alias.
  const rust = walk(join(root, ALIAS_DIR))
    .filter((p) => p.endsWith(".rs"))
    .map((p) => [relative(root, p), readFileSync(p, "utf8")])
    .filter(([, src]) => /\balias\s*=\s*"/.test(src));
  const aliases = parseAliases(
    rust.map(([, src]) => src),
    rust.map(([p]) => p),
  );
  return { verbs, members, twinOf, tools, aliases };
}

const decodeEntities = (s) =>
  s
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&apos;/g, "'")
    .replace(/&nbsp;/g, " ")
    .replace(/&hellip;/g, "…")
    .replace(/&rarr;/g, "→")
    .replace(/&mdash;/g, "—")
    .replace(/&#(\d+);|&#x([0-9a-f]+);/gi, (e, d, h) => {
      const n = d ? Number(d) : parseInt(h, 16);
      return n <= 0x10ffff ? String.fromCodePoint(n) : e;
    })
    .replace(/&amp;/g, "&");

// The 1-based line of an index into `text`: line starts are found once, then binary-searched.
function lineCounter(text) {
  const starts = [0];
  for (let i = text.indexOf("\n"); i >= 0; i = text.indexOf("\n", i + 1)) starts.push(i + 1);
  return (index) => {
    let lo = 0;
    let hi = starts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (starts[mid] <= index) lo = mid;
      else hi = mid - 1;
    }
    return lo + 1;
  };
}

// [start, end) ranges of elements that carry data-api-lint="off", found by tag depth.
function offRanges(text) {
  const ranges = [];
  const open =
    /<([A-Za-z][\w-]*)(?=[\s/>])[^>]*(?<=\s)data-api-lint\s*=\s*(?:["']off["']|off(?=[\s/>]))[^>]*>/gi;
  for (const m of text.matchAll(open)) {
    const tag = m[1];
    if (m[0].endsWith("/>")) continue;
    const re = new RegExp(`<(/?)${tag}(?=[\\s/>])[^>]*>`, "gi");
    re.lastIndex = m.index + m[0].length;
    let depth = 1;
    // An element never closed covers only its own tag, never the rest of the file.
    let end = m.index + m[0].length;
    for (let t; depth && (t = re.exec(text)); ) {
      depth += t[1] ? -1 : t[0].endsWith("/>") ? 0 : 1;
      if (!depth) end = t.index + t[0].length;
    }
    ranges.push([m.index, end]);
  }
  return ranges;
}

// `text` with every [start, end) range blanked to spaces, its newlines kept so line numbers
// stay true.
function blank(text, ranges) {
  if (!ranges.length) return text;
  let out = "";
  let at = 0;
  for (const [a, b] of [...ranges].sort((x, y) => x[0] - y[0])) {
    if (b <= at) continue;
    const from = Math.max(a, at);
    out += text.slice(at, from) + text.slice(from, b).replace(/[^\n]/g, " ");
    at = b;
  }
  return out + text.slice(at);
}

// HTML that is never example code: comments, and in HTML pages also live page code
// (<script>), styles and inert containers (<template>, <textarea>). In Markdown a comment is
// taken to end within its paragraph and never inside a fence (a trade-off: CommonMark lets one
// run on, but a `<!--` written as code must not swallow the examples after it; the same holds
// for an off marker written as code).
const MD_FENCE = /^ {0,3}((`|~)\2{2,})[^\n]*\n([^]*?)^ {0,3}\1\2*[ \t]*$/gm;
const MD_COMMENT = /<!--(?:(?!\n\s*\n)[^])*?-->/g;
const HTML_NON_EXAMPLE = /<!--[^]*?-->|<(script|style|template|textarea)(?=[\s>])[^]*?<\/\1\s*>/gi;

// Code snippets of one file: { text, line } with `line` the 1-based line the text starts on.
// Non-example HTML is blanked first, then elements marked off, so a marker or tag inside a
// comment or a script string can neither hide nor expose anything.
export function extractSnippets(path, raw) {
  let text = raw.replace(/\r\n?/g, "\n");
  const html = path.endsWith(".html");
  const markdown = path.endsWith(".md") || path.endsWith(".mdx");
  if (html || markdown) {
    const fences = markdown
      ? [...text.matchAll(MD_FENCE)].map((m) => [m.index, m.index + m[0].length])
      : [];
    // In Markdown, a comment or an off marker written as code (in a fence, or after an odd
    // number of backticks on its line) is an example of one, not one.
    const asCode = (a) =>
      fences.some(([fa, fb]) => a >= fa && a < fb) ||
      (text.slice(text.lastIndexOf("\n", a) + 1, a).split("`").length - 1) % 2 === 1;
    const re = html ? HTML_NON_EXAMPLE : MD_COMMENT;
    const skipped = [...text.matchAll(re)].map((m) => [m.index, m.index + m[0].length]);
    text = blank(
      text,
      skipped.filter(([a]) => !(markdown && asCode(a))),
    );
    text = blank(
      text,
      offRanges(text).filter(([a]) => !(markdown && asCode(a))),
    );
  }
  const out = [];
  const lineOf = lineCounter(text);
  const push = (index, body) => out.push({ text: body, line: lineOf(index) });
  if (html) {
    // A <pre> is one snippet (its <code>, if any, included); a <code> outside <pre> is another.
    const re = /<pre(?=[\s>])[^>]*>([^]*?)<\/pre>|<code(?=[\s>])[^>]*>([^]*?)<\/code>/gi;
    for (const m of text.matchAll(re)) {
      const inner = (m[1] ?? m[2]).replace(/<[^>]*>/g, (tag) =>
        "\n".repeat(tag.split("\n").length - 1),
      );
      push(m.index + m[0].indexOf(">") + 1, decodeEntities(inner));
    }
  } else if (markdown) {
    const fences = [];
    for (const m of text.matchAll(MD_FENCE)) {
      push(m.index + m[0].indexOf("\n") + 1, m[3]);
      fences.push([m.index, m.index + m[0].length]);
    }
    const prose = blank(text, fences);
    // Inline code stays within one paragraph, so a stray backtick cannot pair across them.
    for (const m of prose.matchAll(/(`+)([^`\n](?:[^`\n]|\n(?!\s*\n))*?)\1(?!`)/g)) {
      // `tool` { … } in prose: the object sits after the inline code.
      const rest = prose.slice(m.index + m[0].length);
      const body = /^[ \t]*\{/.test(rest) ? m[2] + rest.split("\n")[0] : m[2];
      push(m.index + m[1].length, body);
    }
  } else {
    push(0, text);
  }
  return out;
}

// If a string or comment starts at `i`, the index of its last character (the text's length
// when it never closes); otherwise -1.
function skipNonCode(text, i) {
  const c = text[i];
  if (c === '"' || c === "'" || c === "`") {
    for (i++; i < text.length && text[i] !== c; i++) if (text[i] === "\\") i++;
    return i;
  }
  if (c === "/" && text[i + 1] === "/") {
    while (i < text.length && text[i] !== "\n") i++;
    return i;
  }
  if (c === "/" && text[i + 1] === "*") {
    const close = text.indexOf("*/", i + 2);
    return close < 0 ? text.length : close + 1;
  }
  return -1;
}

// The text from `open` (a `{`) to its matching `}`, skipping strings and comments.
function balanced(text, open) {
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    const skip = skipNonCode(text, i);
    if (skip >= 0) i = skip;
    else if ("{[(".includes(text[i])) depth++;
    else if ("}])".includes(text[i]) && --depth === 0) return text.slice(open, i + 1);
  }
  return null;
}

// Top-level keys of an object literal (`{ a: 1, 'b': 2, c, ...d }`): each key, its offset in
// `obj` and where its value starts (-1 for shorthand). Spreads and ellipses are skipped, so
// `{ …, agent }` checks only `agent`.
export function topLevelKeys(obj) {
  const keys = [];
  let depth = 0;
  let start = 1;
  const entry = (from, to) => {
    const raw = obj
      .slice(from, to)
      .replace(/\/\/[^\n]*|\/\*[^]*?\*\//g, (c) => " ".repeat(c.length));
    const m = raw.match(/^\s*(?:(["'])([^"']+)\1|([A-Za-z_$][\w$-]*))\s*(?::\s*|$|\(|\?\s*:\s*)/s);
    if (!m) return;
    const value = m[0].trimEnd().endsWith(":") ? from + m[0].length : -1;
    keys.push({ key: m[2] ?? m[3], at: from + raw.search(/\S/), value });
  };
  for (let i = 1; i < obj.length - 1; i++) {
    const skip = skipNonCode(obj, i);
    const c = obj[i];
    if (skip >= 0) i = skip;
    else if ("{[(".includes(c)) depth++;
    else if ("}])".includes(c)) depth--;
    else if (c === "," && depth === 0) {
      entry(start, i);
      start = i + 1;
    }
  }
  entry(start, obj.length - 1);
  return keys;
}

function checkArgs(tool, keys, at, lineOf, truth, out) {
  const props = truth.tools.get(tool);
  for (const { key, at: off } of keys) {
    if (props.has(key)) continue;
    const canon = (truth.aliases.get(key) ?? []).find((c) => props.has(c));
    const message = canon
      ? `${tool}: \`${key}\` is an alias; use \`${canon}\``
      : `${tool}: \`${key}\` is not a parameter (has: ${[...props].join(", ") || "none"})`;
    out.push({ line: lineOf(at + off), rule: "R3", message });
  }
}

function checkSnippet(snippet, truth) {
  const { text } = snippet;
  const out = [];
  const lineIn = lineCounter(text);
  const lineOf = (i) => snippet.line + lineIn(i) - 1;

  for (const m of text.matchAll(
    /\b(?:codesense|cs)\??\.(?:invoke|act)(?:\?\.)?\(\s*(["'`])([^"'`]+)\1/g,
  )) {
    const name = m[2];
    if (truth.verbs.has(name) || name.includes("${")) continue;
    const twin = truth.twinOf
      .get(name)
      ?.map((v) => `\`codesense.${v}()\``)
      .join(" or ");
    const message = truth.tools.has(name)
      ? `\`${name}\` is an MCP tool, not a page verb: write \`${name} { … }\` for the agent side` +
        (twin ? `, or call the page verb ${twin}` : "")
      : `\`${name}\` is not a page verb`;
    out.push({ line: lineOf(m.index), rule: "R1", message });
  }

  const sdk =
    /(?<![\w$.?])(?:(?:window|globalThis|self)\??\.)?(?:codesense|cs)\??\.([A-Za-z_$][\w$]*)\s*(?:\?\.)?\(/g;
  for (const m of text.matchAll(sdk)) {
    if (truth.members.has(m[1])) continue;
    out.push({
      line: lineOf(m.index),
      rule: "R2",
      message: `\`codesense.${m[1]}\` is not a page member or verb`,
    });
  }

  // tool { … } / tool({ … }) with the tool name on the `{` line; mcp.call("tool", { … }).
  const calls = [
    /(?<![\w$.'"`-])([a-z][a-z0-9_]*)[ \t]*\(?[ \t]*(?=\{)/g,
    /\bmcp\.call\(\s*["'`]([a-z][a-z0-9_]*)["'`]\s*,\s*(?=\{)/g,
  ];
  for (const re of calls) {
    for (const m of text.matchAll(re)) {
      const tool = m[1];
      if (!truth.tools.has(tool)) continue;
      const open = m.index + m[0].length;
      const obj = balanced(text, open);
      if (!obj) continue;
      const keys = topLevelKeys(obj);
      checkArgs(tool, keys, open, lineOf, truth, out);
      if (tool !== "codesense_call_tool") continue;
      // Where a top-level key's value starts; a repeated key takes its last value, as in JS.
      const valueAt = (key) => keys.findLast((e) => e.key === key)?.value ?? -1;
      const nameAt = valueAt("name");
      const name = nameAt < 0 ? null : obj.slice(nameAt).match(/^["'`]([a-z][a-z0-9_]*)["'`]/)?.[1];
      const brace = valueAt("arguments");
      if (!name || !truth.tools.has(name) || obj[brace] !== "{") continue;
      const inner = balanced(obj, brace);
      if (inner) checkArgs(name, topLevelKeys(inner), open + brace, lineOf, truth, out);
    }
  }
  return out;
}

export function checkFile(path, text, truth) {
  return extractSnippets(path, text)
    .flatMap((s) => checkSnippet(s, truth))
    .sort((a, b) => a.line - b.line);
}

// Every file under `dir`, not following symlinks.
function walk(dir, out = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, entry.name);
    if (entry.isDirectory()) walk(p, out);
    else if (entry.isFile()) out.push(p);
  }
  return out;
}

export function corpus(root = DEFAULT_ROOT) {
  const base = join(root, CONTENT);
  const content = walk(base).filter(
    (p) => /\.(html|md|js|ya?ml)$/.test(p) && !relative(base, p).startsWith("generated/"),
  );
  const handbook = existsSync(join(root, "site"))
    ? walk(join(root, HANDBOOK)).filter((p) => p.endsWith(".mdx"))
    : [];
  return [...content, ...handbook].sort();
}

function main(argv) {
  const args = argv.flatMap((a) => (a.startsWith("--root=") ? ["--root", a.slice(7)] : [a]));
  const dir = args[0] === "--root" ? args[1] : undefined;
  if (args.length && (args.length !== 2 || !dir || dir.startsWith("-"))) {
    console.error("usage: node scripts/check-docs-api-examples.mjs [--root <dir> | --root=<dir>]");
    process.exit(2);
  }
  const root = dir ? resolve(dir) : DEFAULT_ROOT;
  const truth = loadTruth(root);
  let count = 0;
  for (const path of corpus(root)) {
    for (const f of checkFile(path, readFileSync(path, "utf8"), truth)) {
      console.error(`${relative(root, path)}:${f.line}: ${f.rule} ${f.message}`);
      count++;
    }
  }
  if (count) {
    console.error(
      `\ncheck-docs-api-examples: ${count} problem(s). Fix the example, or (in HTML and Markdown) mark a deliberately ` +
        `wrong one with data-api-lint="off".`,
    );
    process.exit(1);
  }
  console.log("check-docs-api-examples: ok");
}

if (process.argv[1] === fileURLToPath(import.meta.url)) main(process.argv.slice(2));
