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
//       serde alias is reported with the name the schema uses). A nested object or array of
//       objects is checked too when its schema lists its fields. Required fields are not checked.
//   R4  A page verb's object argument (`codesense.<verb>({ … })`,
//       `codesense.invoke('<verb>', { … })`, `kit.call("<verb>", { … })`) passes only the
//       properties its argsSchema lists. A verb whose schema lists none is free-form and
//       skipped, and so is a direct `codesense.<verb>(` call to a verb that is also a Surface
//       member: it calls the Surface method, whose own signature governs (openFile, showMenu); its
//       invoke and kit.call forms are checked. `queryContext` and
//       `subscribeContext` take `query_context`'s parameters. A page member is also called with
//       no more arguments than its signature takes (`onRoutineResult(cb)`, not `(key, cb)`).
//
// In HTML pages, a <cs-try-this> block's `verb` must be a page verb (R1). A dry-run block may
// instead name an MCP tool, to show an agent-side call; its `args` are then checked as R3.
//
// Truth: generated/page-sdk.json (verbs, members, argsSchema), generated/mcp-tools.json
// (params) and the `alias = "…"` attributes in src-tauri/src/**/*.rs. Corpus: the docs content tree
// (.html .md .js .yml .yaml, minus generated/) and the handbook MDX when the site is present (site/,
// or $CODESENSE_SITE_ROOT: lib/site-root.mjs) (a
// tree without it, like the public export, checks only the handbook snapshot in the content
// tree), plus every tool and parameter description in generated/mcp-tools.json, reported by
// tool and parameter, and the examples in generated/primitives.json (the primitives explorer).
// Code is the text of <code> and <pre> (HTML), fenced and inline code (Markdown), or a whole
// description.
// Not examples, so skipped:
// HTML comments, and in HTML pages <script> (live page code), <style>, <template> and
// <textarea>. In HTML and Markdown, an element carrying data-api-lint="off" holds a
// deliberately wrong example and is skipped too.
//
//   node scripts/check-docs-api-examples.mjs [--root <dir> | --root=<dir>]
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseItems } from "./lib/rust.mjs";
import { isMain } from "./lib/is-main.mjs";
import { HANDBOOK_DIR, siteRoot } from "./lib/site-root.mjs";

const DEFAULT_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const CONTENT = "codesense-docs/getting-started/content";
const ALIAS_DIR = "src-tauri/src";
// Page members the docs' own shared scripts define on `codesense`.
const EXTRA_MEMBERS = ["docs"];
// Arguments the page bridge reads that a verb's argsSchema does not list yet (html-bridge.ts).
// The node test keeps each entry a checked verb and each key one its schema lacks.
export const BRIDGE_ARGS = {
  inbox: ["agentId", "kind", "limit", "org", "role", "scope", "sessionId", "team"],
  routine: ["id"],
  openFile: ["org"],
  openWorkspace: ["operationId", "reviewPrompt"],
  routineRefresh: ["variables"],
  // MenuRequest keys (codesense.d.ts).
  showMenu: ["builtinAction", "nativeSurface", "type"],
  showPopover: ["builtinAction", "nativeSurface", "type"],
};
// Page calls whose object argument is an MCP tool's query, checked against its parameters.
const QUERY_ARGS = { queryContext: "query_context", subscribeContext: "query_context" };

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
  const { tools: list } = gen("mcp-tools.json");
  const tools = new Map(list.map((t) => [t.name, t.params]));
  const surface = new Set(sdk.members.filter((m) => m.owner === "Surface").map((m) => m.name));
  const verbArgs = new Map();
  for (const v of sdk.verbs) {
    const listed = Object.keys(v.argsSchema?.properties ?? {});
    if (!listed.length) continue;
    const extra = Object.fromEntries((BRIDGE_ARGS[v.name] ?? []).map((k) => [k, {}]));
    verbArgs.set(v.name, {
      ...v.argsSchema,
      properties: { ...v.argsSchema.properties, ...extra },
      additionalProperties: false,
    });
  }
  // A QUERY_ARGS schema is the Surface method's own argument, so its direct calls are checked.
  for (const [name, tool] of Object.entries(QUERY_ARGS)) {
    surface.delete(name);
    if (tools.has(tool)) verbArgs.set(name, { ...tools.get(tool), additionalProperties: false });
  }
  // Each page member's most parameters over its signatures; a rest parameter takes any number.
  const arity = new Map();
  for (const { name, signature } of sdk.members) {
    const n = paramCount(signature ?? "");
    const prev = arity.get(name);
    arity.set(name, n === null || prev === null ? null : Math.max(prev ?? 0, n));
  }
  // Every description in a tool's params, by parameter path (`input.signIn`).
  const descriptions = [];
  const visit = (node, where) => {
    if (!node || typeof node !== "object") return;
    if (typeof node.description === "string") descriptions.push({ where, text: node.description });
    for (const [name, sub] of Object.entries(node.properties ?? {})) visit(sub, `${where}.${name}`);
    for (const sub of [...(node.anyOf ?? []), ...(node.oneOf ?? []), ...(node.allOf ?? [])])
      visit(sub, where);
    if (node.items) visit(node.items, `${where}[]`);
    if (typeof node.additionalProperties === "object")
      visit(node.additionalProperties, `${where}.*`);
  };
  for (const t of list) {
    descriptions.push({ where: `tool ${t.name}`, text: t.description ?? "" });
    for (const [name, sub] of Object.entries(t.params?.properties ?? {}))
      visit(sub, `tool ${t.name}, parameter ${name}`);
  }
  // Parse only the Rust files that declare an alias.
  const rust = walk(join(root, ALIAS_DIR))
    .filter((p) => p.endsWith(".rs"))
    .map((p) => [relative(root, p), readFileSync(p, "utf8")])
    .filter(([, src]) => /\balias\s*=\s*"/.test(src));
  const aliases = parseAliases(
    rust.map(([, src]) => src),
    rust.map(([p]) => p),
  );
  return { verbs, members, surface, verbArgs, arity, twinOf, tools, aliases, descriptions };
}

// The parameters a TypeScript signature (`name(a: A, b?: B): R`) declares, or null when it has a
// rest parameter or none can be read. A comma inside `<…>` counts too, so a generic type can only
// raise the count.
function paramCount(signature) {
  const open = signature.indexOf("(");
  const inner = open < 0 ? null : balanced(signature, open);
  if (!inner || inner.includes("...")) return null;
  return argCount(inner);
}

// The arguments in a bracketed `(…)`: its non-blank top-level entries.
const argCount = (inner) =>
  topLevelEntries(inner).filter(([a, b]) => inner.slice(a, b).trim()).length;

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

// A file's text with non-example HTML blanked first, then elements marked off, so a marker or
// tag inside a comment or a script string can neither hide nor expose anything.
function examplesOnly(path, raw) {
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
  return text;
}

// Code snippets of one file: { text, line } with `line` the 1-based line the text starts on.
export const extractSnippets = (path, raw) => snippetsOf(path, examplesOnly(path, raw));

// The snippets of a file's text once examplesOnly has blanked what is not an example.
function snippetsOf(path, text) {
  const html = path.endsWith(".html");
  const markdown = path.endsWith(".md") || path.endsWith(".mdx");
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

// [start, end) of each comma-separated entry inside a bracketed `{…}` or `[…]`, at depth 0,
// skipping strings and comments.
function topLevelEntries(text) {
  const entries = [];
  let depth = 0;
  let start = 1;
  for (let i = 1; i < text.length - 1; i++) {
    const skip = skipNonCode(text, i);
    const c = text[i];
    if (skip >= 0) i = skip;
    else if ("{[(".includes(c)) depth++;
    else if ("}])".includes(c)) depth--;
    else if (c === "," && depth === 0) {
      entries.push([start, i]);
      start = i + 1;
    }
  }
  entries.push([start, text.length - 1]);
  return entries;
}

// Top-level keys of an object literal (`{ a: 1, 'b': 2, c, ...d }`): each key, its offset in
// `obj` and where its value starts (-1 for shorthand). Spreads and ellipses are skipped, so
// `{ …, agent }` checks only `agent`.
export function topLevelKeys(obj) {
  const keys = [];
  const entry = (from, to) => {
    const raw = obj
      .slice(from, to)
      .replace(/\/\/[^\n]*|\/\*[^]*?\*\//g, (c) => " ".repeat(c.length));
    const m = raw.match(/^\s*(?:(["'])([^"']+)\1|([A-Za-z_$][\w$-]*))\s*(?::\s*|$|\(|\?\s*:\s*)/s);
    if (!m) return;
    const value = m[0].trimEnd().endsWith(":") ? from + m[0].length : -1;
    keys.push({ key: m[2] ?? m[3], at: from + raw.search(/\S/), value });
  };
  for (const [from, to] of topLevelEntries(obj)) entry(from, to);
  return keys;
}

// The fields an object under `schema` takes, by name, or null when they are not known (a
// free-form object, or a branch that is not an object). An object with no fields and
// additionalProperties: false takes none. Every object branch of anyOf, oneOf and allOf
// counts; a field in two branches takes either schema.
export function argNames(schema) {
  if (!schema || typeof schema !== "object") return null;
  const branches = [...(schema.anyOf ?? []), ...(schema.oneOf ?? []), ...(schema.allOf ?? [])];
  if (!schema.properties && !branches.length)
    return schema.additionalProperties === false ? new Map() : null;
  // Other keys allowed too (true, or a schema for them): which ones is not known.
  if (schema.additionalProperties) return null;
  const names = new Map(Object.entries(schema.properties ?? {}));
  for (const branch of branches) {
    if (branch?.type === "null") continue;
    const sub = argNames(branch);
    if (!sub) return null;
    for (const [k, v] of sub) names.set(k, names.has(k) ? { anyOf: [names.get(k), v] } : v);
  }
  return names;
}

// The schema of an array's items, through anyOf/oneOf branches (a nullable array).
const itemsOf = (schema) =>
  schema?.items ??
  [...(schema?.anyOf ?? []), ...(schema?.oneOf ?? [])].find((b) => b?.items)?.items;

// Check `obj` (an object literal at `at` in the snippet) against `schema`, then every nested
// object or array of objects whose schema lists its fields. `path` prefixes nested names.
function checkArgs(tool, schema, obj, at, lineOf, aliases, out, path = "", rule = "R3") {
  const names = argNames(schema ?? { properties: {} });
  if (!names) return;
  for (const { key, at: off, value } of topLevelKeys(obj)) {
    let name = key;
    if (!names.has(key)) {
      const canon = (aliases.get(key) ?? []).find((c) => names.has(c));
      const message = canon
        ? `${tool}: \`${path}${key}\` is an alias; use \`${path}${canon}\``
        : `${tool}: \`${path}${key}\` is not a parameter (has: ${[...names.keys()].join(", ") || "none"})`;
      out.push({ line: lineOf(at + off), rule, message });
      if (!canon) continue;
      name = canon;
    }
    const inner = value < 0 || !"{[".includes(obj[value]) ? null : balanced(obj, value);
    if (!inner) continue;
    const sub = names.get(name);
    if (inner[0] === "{") {
      checkArgs(tool, sub, inner, at + value, lineOf, aliases, out, `${path}${name}.`, rule);
      continue;
    }
    const items = itemsOf(sub);
    if (!items) continue;
    for (const [from, to] of topLevelEntries(inner)) {
      // The item's first code character, past whitespace and comments.
      let lead = from;
      for (let skip; lead < to; lead++) {
        if ((skip = skipNonCode(inner, lead)) >= 0 && inner[lead] === "/") lead = skip;
        else if (!/\s/.test(inner[lead])) break;
      }
      const item = inner[lead] === "{" ? balanced(inner, lead) : null;
      if (item)
        checkArgs(
          tool,
          items,
          item,
          at + value + lead,
          lineOf,
          aliases,
          out,
          `${path}${name}[].`,
          rule,
        );
    }
  }
}

// A call on the page SDK: `codesense.` or `cs.`, also through window, globalThis or self.
const PAGE = String.raw`(?<![\w$.?])(?:(?:window|globalThis|self)\??\.)?(?:codesense|cs)\??\.`;
// `codesense.<member>(`, the `(` not consumed (R2 and the arity check).
const MEMBER_CALL = new RegExp(String.raw`${PAGE}([A-Za-z_$][\w$]*)\s*(?:\?\.)?(?=\()`, "g");
// A page verb called with an object literal (R4): directly, through invoke/act, or kit.call.
const DIRECT_VERB_CALL = new RegExp(
  String.raw`${PAGE}(?<verb>[A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)?)\s*(?:\?\.)?\(\s*(?=\{)`,
  "g",
);
const VERB_CALLS = [
  DIRECT_VERB_CALL,
  /\b(?:codesense|cs)\??\.(?:invoke|act)(?:\?\.)?\(\s*(["'`])(?<verb>[\w.]+)\1\s*,\s*(?=\{)/g,
  /\bkit\.call\(\s*(["'`])(?<verb>[\w.]+)\1\s*,\s*(?=\{)/g,
];
// Serde aliases are MCP-side, so a page verb's arguments have none.
const NO_ALIASES = new Map();

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

  // R2, then R4: a member called with more arguments than its signature takes.
  for (const m of text.matchAll(MEMBER_CALL)) {
    const member = m[1];
    if (!truth.members.has(member)) {
      const message = `\`codesense.${member}\` is not a page member or verb`;
      out.push({ line: lineOf(m.index), rule: "R2", message });
      continue;
    }
    const most = truth.arity?.get(member);
    const args = most == null ? null : balanced(text, m.index + m[0].length);
    const count = args ? argCount(args) : 0;
    if (!args || count <= most) continue;
    const message = `codesense.${member}: takes ${most} argument${most === 1 ? "" : "s"}, not ${count}`;
    out.push({ line: lineOf(m.index), rule: "R4", message });
  }

  // R4: a page verb's object argument against its listed properties (loadTruth: `verbArgs`).
  // A direct call to a Surface member goes to the Surface method, whose own signature governs.
  for (const re of VERB_CALLS) {
    for (const m of text.matchAll(re)) {
      const { verb } = m.groups;
      const schema = truth.verbArgs?.get(verb);
      if (!schema || (re === DIRECT_VERB_CALL && truth.surface?.has(verb))) continue;
      const open = m.index + m[0].length;
      const obj = balanced(text, open);
      if (obj) checkArgs(`codesense.${verb}`, schema, obj, open, lineOf, NO_ALIASES, out, "", "R4");
    }
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
      checkArgs(tool, truth.tools.get(tool), obj, open, lineOf, truth.aliases, out);
      if (tool !== "codesense_call_tool") continue;
      // Where a top-level key's value starts; a repeated key takes its last value, as in JS.
      const keys = topLevelKeys(obj);
      const valueAt = (key) => keys.findLast((e) => e.key === key)?.value ?? -1;
      const nameAt = valueAt("name");
      const name = nameAt < 0 ? null : obj.slice(nameAt).match(/^["'`]([a-z][a-z0-9_]*)["'`]/)?.[1];
      const brace = valueAt("arguments");
      if (!name || !truth.tools.has(name) || obj[brace] !== "{") continue;
      const inner = balanced(obj, brace);
      if (inner)
        checkArgs(name, truth.tools.get(name), inner, open + brace, lineOf, truth.aliases, out);
    }
  }
  return out;
}

// A <cs-try-this> start tag; a quoted attribute value may hold a `>`.
const TRY_THIS_TAG = /<cs-try-this(?=[\s/>])(?:"[^"]*"|'[^']*'|[^'">])*>/gi;

// A start tag's attributes as the browser reads them: names lowercased, values quoted or
// not, read in order so text inside one value is never taken for another attribute.
function tagAttributes(tag) {
  const attrs = new Map();
  const body = tag.replace(/^<[\w-]+/, "").replace(/\/?>$/, "");
  for (const m of body.matchAll(/([^\s"'>/=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+)))?/g)) {
    const name = m[1].toLowerCase();
    if (!attrs.has(name)) attrs.set(name, m[2] ?? m[3] ?? m[4] ?? "");
  }
  return attrs;
}

// <cs-try-this verb="…" args='…' dry-run="true"> blocks of an HTML page (see _shared/try-this.js).
function checkTryThis(text, truth) {
  const out = [];
  const lineOf = lineCounter(text);
  for (const m of text.matchAll(TRY_THIS_TAG)) {
    const attrs = tagAttributes(m[0]);
    const verb = attrs.get("verb");
    const line = lineOf(m.index);
    if (!verb) {
      out.push({ line, rule: "R1", message: "try-this has no verb" });
      continue;
    }
    if (truth.verbs.has(verb)) continue;
    const dryRun = attrs.get("dry-run") === "true";
    const isTool = truth.tools.has(verb);
    if (!dryRun || !isTool) {
      const message = isTool
        ? `try-this \`${verb}\` is an MCP tool, not a page verb: mark the block dry-run="true" or use a page verb`
        : `try-this \`${verb}\` is not a page verb`;
      out.push({ line, rule: "R1", message });
      continue;
    }
    const args = decodeEntities(attrs.get("args") ?? "").trim();
    const obj = args.startsWith("{") ? balanced(args, 0) : null;
    if (obj && obj.length === args.length)
      checkArgs(verb, truth.tools.get(verb), obj, 0, () => line, truth.aliases, out);
    else if (args)
      out.push({ line, rule: "R3", message: `try-this \`${verb}\`: args is not a JSON object` });
  }
  return out;
}

export function checkFile(path, raw, truth) {
  const text = examplesOnly(path, raw);
  return snippetsOf(path, text)
    .flatMap((s) => checkSnippet(s, truth))
    .concat(path.endsWith(".html") ? checkTryThis(text, truth) : [])
    .sort((a, b) => a.line - b.line);
}

// The tool and parameter descriptions agents are served, each checked as one snippet and
// named by tool and parameter (they are written in the Rust source, not at a line here).
export function checkDescriptions(truth) {
  return truth.descriptions.flatMap(({ where, text }) =>
    checkSnippet({ text, line: 1 }, truth).map(({ rule, message }) => ({ where, rule, message })),
  );
}

// The primitives explorer's examples, each checked as one snippet: a verb's example `args` as
// `codesense.invoke("<verb>", <args>)`, and each SDK member's and event's `example` code.
// Named by section and entry; `primitives` is the parsed file.
export function checkPrimitives(primitives, truth) {
  const snippets = [
    ...(primitives.invoke ?? []).flatMap(({ name, examples = [] }) =>
      examples
        .filter(({ args }) => args && typeof args === "object")
        .map(({ args }) => ({
          where: `invoke ${name}`,
          text: `codesense.invoke(${JSON.stringify(name)}, ${JSON.stringify(args)})`,
        })),
    ),
    ...["kit", "events"].flatMap((section) =>
      (primitives[section] ?? [])
        .filter(({ example }) => typeof example === "string")
        .map(({ name, example }) => ({ where: `${section} ${name}`, text: example })),
    ),
  ];
  return snippets.flatMap(({ where, text }) =>
    checkSnippet({ text, line: 1 }, truth).map(({ rule, message }) => ({ where, rule, message })),
  );
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

/** `env` names the site through CODESENSE_SITE_ROOT; main passes none when --root is given. */
export function corpus(root = DEFAULT_ROOT, env = process.env) {
  const base = join(root, CONTENT);
  const content = walk(base).filter(
    (p) => /\.(html|md|js|ya?ml)$/.test(p) && !relative(base, p).startsWith("generated/"),
  );
  const site = siteRoot(root, env);
  const handbook = existsSync(site)
    ? walk(join(site, HANDBOOK_DIR)).filter((p) => p.endsWith(".mdx"))
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
  // An explicit --root wins over CODESENSE_SITE_ROOT: the checkout it names brings its own site.
  for (const path of corpus(root, dir ? {} : process.env)) {
    for (const f of checkFile(path, readFileSync(path, "utf8"), truth)) {
      console.error(`${relative(root, path)}:${f.line}: ${f.rule} ${f.message}`);
      count++;
    }
  }
  for (const f of checkDescriptions(truth)) {
    console.error(`${CONTENT}/generated/mcp-tools.json (${f.where}): ${f.rule} ${f.message}`);
    count++;
  }
  const primitives = join(root, CONTENT, "generated/primitives.json");
  if (existsSync(primitives)) {
    for (const f of checkPrimitives(JSON.parse(readFileSync(primitives, "utf8")), truth)) {
      console.error(`${CONTENT}/generated/primitives.json (${f.where}): ${f.rule} ${f.message}`);
      count++;
    }
  }
  if (count) {
    console.error(
      `\ncheck-docs-api-examples: ${count} problem(s). Fix the example, or (in HTML and Markdown) mark a deliberately ` +
        `wrong one with data-api-lint="off". A tool or parameter description is fixed in its Rust source, then the export is refreshed.`,
    );
    process.exit(1);
  }
  console.log("check-docs-api-examples: ok");
}

if (isMain(import.meta.url)) main(process.argv.slice(2));
