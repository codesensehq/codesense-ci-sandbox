// SPDX-License-Identifier: MIT
// A small, deliberately narrow Rust source reader: enough to pull serde
// structs/enums (fields, doc comments, serde attributes), string constants,
// `&[&str]` tables and struct-literal tables out of the app source. It does not
// try to be a Rust parser; anything it can't read makes the generator throw, so
// a shape change in the app shows up as a failed `gen` instead of silent drift.

/** Cut a file at its `#[cfg(test)] mod …` block so test fixtures are ignored. */
export function productionPart(src) {
  const m = src.match(/^#\[cfg\(test\)\]\s*\n\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{/m);
  return m ? src.slice(0, m.index) : src;
}

/** Decode the body of a Rust "…" literal (no surrounding quotes). */
export function decodeRustString(body) {
  let out = "";
  for (let i = 0; i < body.length; i++) {
    const c = body[i];
    if (c !== "\\") {
      out += c;
      continue;
    }
    const n = body[++i];
    if (n === "n") out += "\n";
    else if (n === "t") out += "\t";
    else if (n === "r") out += "\r";
    else if (n === "0") out += "\0";
    else if (n === "\\" || n === '"' || n === "'") out += n;
    else if (n === "\n") {
      // line continuation: skip the newline and the next line's indent
      while (i + 1 < body.length && /\s/.test(body[i + 1])) i++;
    } else if (n === "u") {
      const end = body.indexOf("}", i);
      out += String.fromCodePoint(parseInt(body.slice(i + 2, end), 16));
      i = end;
    } else if (n === "x") {
      out += String.fromCharCode(parseInt(body.slice(i + 1, i + 3), 16));
      i += 2;
    } else throw new Error(`rust: unknown escape \\${n}`);
  }
  return out;
}

/**
 * Read a string literal (plain or raw) starting at `src[i]`.
 * Returns { value, end } where end is the index after the literal, or null.
 */
export function readStringLiteral(src, i) {
  if (src[i] === '"') {
    let j = i + 1;
    while (j < src.length && src[j] !== '"') j += src[j] === "\\" ? 2 : 1;
    return { value: decodeRustString(src.slice(i + 1, j)), end: j + 1 };
  }
  const raw = src.slice(i).match(/^r(#*)"/);
  if (raw) {
    const close = `"${raw[1]}`;
    const start = i + raw[0].length;
    const j = src.indexOf(close, start);
    if (j < 0) throw new Error("rust: unterminated raw string");
    return { value: src.slice(start, j), end: j + close.length };
  }
  return null;
}

/** Index just past the bracket that closes the one at `src[open]`. Skips strings and comments. */
export function matchBracket(src, open) {
  const pairs = { "{": "}", "(": ")", "[": "]" };
  const stack = [pairs[src[open]]];
  let i = open + 1;
  while (i < src.length && stack.length) {
    const c = src[i];
    if (c === "/" && src[i + 1] === "/") {
      i = src.indexOf("\n", i);
      if (i < 0) break;
      continue;
    }
    if (c === "/" && src[i + 1] === "*") {
      const e = src.indexOf("*/", i + 2);
      if (e < 0) throw new Error("rust: unterminated block comment");
      i = e + 2;
      continue;
    }
    if (c === '"' || (c === "r" && /^r#*"/.test(src.slice(i, i + 4)) && !/\w/.test(src[i - 1]))) {
      i = readStringLiteral(src, i).end;
      continue;
    }
    const ch =
      c === "'" &&
      src.slice(i, i + 12).match(/^'(?:\\u\{[0-9a-fA-F]+\}|\\x[0-9a-fA-F]{2}|\\.|[^\\'])'/);
    if (ch) {
      i += ch[0].length;
      continue;
    }
    if (pairs[c]) stack.push(pairs[c]);
    else if (c === stack[stack.length - 1]) stack.pop();
    i++;
  }
  if (stack.length) throw new Error("rust: unbalanced brackets");
  return i;
}

/** Concatenate every string literal in `text` (for `description = "a" "b"` / concat!). */
export function stringLiterals(text) {
  const out = [];
  for (let i = 0; i < text.length; i++) {
    if (text[i] === '"' || (text[i] === "r" && /^r#*"/.test(text.slice(i, i + 4)))) {
      const lit = readStringLiteral(text, i);
      if (lit) {
        out.push(lit.value);
        i = lit.end - 1;
      }
    }
  }
  return out;
}

// ---- serde attributes -------------------------------------------------------

function serdeAttrs(attrs) {
  const out = {};
  for (const a of attrs) {
    const m = a.match(/^#\[serde\(([\s\S]*)\)\]$/);
    if (!m) continue;
    const body = m[1];
    for (const key of ["rename", "rename_all", "tag", "content", "alias", "default"]) {
      const kv = body.match(new RegExp(`(?:^|[,\\s(])${key}\\s*=\\s*"([^"]*)"`));
      if (kv) out[key] = kv[1];
    }
    if (/(?:^|[,\s])default(?:\s*[,)]|\s*$)/.test(body)) out.default = out.default ?? true;
    for (const flag of ["flatten", "skip", "untagged", "deny_unknown_fields", "transparent"]) {
      if (new RegExp(`(?:^|[,\\s])${flag}(?:\\s*[,)]|\\s*$)`).test(body)) out[flag] = true;
    }
    if (/skip_deserializing/.test(body)) out.skip_deserializing = true;
    if (/skip_serializing_if/.test(body)) out.skip_serializing_if = true;
  }
  return out;
}

export function applyRenameAll(name, rule, isVariant) {
  const words = isVariant
    ? name.split(/(?=[A-Z])/).map((w) => w.toLowerCase())
    : name.split("_").filter(Boolean);
  switch (rule) {
    case undefined:
      return name;
    case "snake_case":
      return words.join("_");
    case "kebab-case":
      return words.join("-");
    case "lowercase":
      return words.join("");
    case "UPPERCASE":
      return words.join("").toUpperCase();
    case "SCREAMING_SNAKE_CASE":
      return words.join("_").toUpperCase();
    case "camelCase":
      return words.map((w, i) => (i ? w[0].toUpperCase() + w.slice(1) : w)).join("");
    case "PascalCase":
      return words.map((w) => w[0].toUpperCase() + w.slice(1)).join("");
    default:
      throw new Error(`rust: unsupported rename_all ${rule}`);
  }
}

// ---- items ------------------------------------------------------------------

/** Doc comment + attribute lines directly above index `at`. */
function leadingMeta(src, at) {
  const lines = src.slice(0, at).split("\n");
  lines.pop(); // the partial line the item starts on
  const docs = [];
  const attrs = [];
  let attrBuf = null;
  while (lines.length) {
    const t = lines[lines.length - 1].trim();
    if (attrBuf !== null) {
      attrBuf = `${t}\n${attrBuf}`;
      lines.pop();
      if (t.startsWith("#[")) {
        attrs.unshift(attrBuf.replace(/\s*\n\s*/g, " "));
        attrBuf = null;
      }
      continue;
    }
    if (t.startsWith("///")) docs.unshift(t.replace(/^\/\/\/ ?/, ""));
    else if (t.startsWith("#[") && t.endsWith("]")) attrs.unshift(t);
    else if (t.endsWith(")]") || t.endsWith(",") || t.endsWith("(")) attrBuf = t;
    else if (t.startsWith("//")) {
      /* plain comment */
    } else break;
    lines.pop();
  }
  return { docs, attrs };
}

/** Split an item body into member chunks with their docs/attrs. */
function members(body) {
  const out = [];
  let docs = [];
  let attrs = [];
  let i = 0;
  while (i < body.length) {
    while (i < body.length && /\s/.test(body[i])) i++;
    if (i >= body.length) break;
    if (body.startsWith("///", i)) {
      const e = body.indexOf("\n", i);
      docs.push(body.slice(i, e < 0 ? body.length : e).replace(/^\/\/\/ ?/, ""));
      i = e < 0 ? body.length : e + 1;
      continue;
    }
    if (body.startsWith("//", i)) {
      const e = body.indexOf("\n", i);
      i = e < 0 ? body.length : e + 1;
      continue;
    }
    if (body.startsWith("/*", i)) {
      const e = body.indexOf("*/", i + 2);
      if (e < 0) throw new Error("rust: unterminated block comment");
      i = e + 2;
      continue;
    }
    if (body.startsWith("#[", i)) {
      const e = matchBracket(body, i + 1);
      attrs.push(body.slice(i, e).replace(/\s*\n\s*/g, " "));
      i = e;
      continue;
    }
    // a member runs to the next top-level comma
    let j = i;
    let depth = 0;
    while (j < body.length) {
      const c = body[j];
      if (c === '"') {
        j = readStringLiteral(body, j).end;
        continue;
      }
      if (c === "/" && body[j + 1] === "/") {
        j = body.indexOf("\n", j);
        if (j < 0) j = body.length;
        continue;
      }
      if ("<([{".includes(c)) depth++;
      else if (">)]}".includes(c) && !(c === ">" && body[j - 1] === "-")) depth--;
      else if (c === "," && depth === 0) break;
      j++;
    }
    out.push({ text: body.slice(i, j).trim(), docs, attrs });
    docs = [];
    attrs = [];
    i = j + 1;
  }
  return out;
}

function simplifyType(t) {
  return t
    .replace(/\s+/g, " ")
    .replace(/\s*([<>,])\s*/g, "$1")
    .replace(/,/g, ", ")
    .trim();
}

/**
 * Parse every `struct Name { … }` and `enum Name { … }` in the production part
 * of `src`. Returns a Map name → item.
 */
export function parseItems(src) {
  const prod = productionPart(src);
  const items = new Map();
  const re = /^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(struct|enum)\s+(\w+)(?:<[^>{]*>)?\s*\{/gm;
  let m;
  while ((m = re.exec(prod))) {
    const [, kind, name] = m;
    const open = m.index + m[0].length - 1;
    const close = matchBracket(prod, open);
    const body = prod.slice(open + 1, close - 1);
    const meta = leadingMeta(prod, m.index + m[0].search(/\S/));
    const serde = serdeAttrs(meta.attrs);
    const line = prod.slice(0, m.index).split("\n").length;
    const item = { kind, name, line, docs: meta.docs, attrs: meta.attrs, serde, members: [] };
    for (const mem of members(body)) {
      const ms = serdeAttrs(mem.attrs);
      if (kind === "struct") {
        const fm = mem.text.match(/^(?:pub(?:\([^)]*\))?\s+)?(r#)?(\w+)\s*:\s*([\s\S]+)$/);
        if (!fm) throw new Error(`rust: can't read field "${mem.text}" in ${name}`);
        const rustName = fm[2];
        const type = simplifyType(fm[3]);
        item.members.push({
          rustName,
          name: ms.rename ?? applyRenameAll(rustName, serde.rename_all, false),
          type,
          docs: mem.docs,
          serde: ms,
          optional:
            /^Option</.test(type) || ms.default !== undefined || serde.default !== undefined,
        });
      } else {
        const vm = mem.text.match(/^(\w+)\s*(\{[\s\S]*\}|\([\s\S]*\))?\s*(?:=\s*[^,]+)?$/);
        if (!vm) throw new Error(`rust: can't read variant "${mem.text}" in ${name}`);
        const payloadFields = vm[2]?.startsWith("{")
          ? members(vm[2].slice(1, -1))
              .map((f) => (f.text.match(/^(?:pub(?:\([^)]*\))?\s+)?(\w+)\s*:/) || [])[1])
              .filter(Boolean)
          : [];
        item.members.push({
          rustName: vm[1],
          name: ms.rename ?? applyRenameAll(vm[1], serde.rename_all, true),
          payload: vm[2] ? simplifyType(vm[2]) : null,
          payloadFields,
          docs: mem.docs,
          serde: ms,
          isDefault: mem.attrs.some((a) => a === "#[default]"),
        });
      }
    }
    items.set(name, item);
  }
  return items;
}

/** `const NAME: &[&str] = &[ "a", "b", OTHER_CONST ];` → string[] (consts resolved from src). */
export function strArrayConst(src, name) {
  const m = src.match(
    new RegExp(`const\\s+${name}\\s*:\\s*&\\[&(?:'static\\s+)?str\\]\\s*=\\s*&\\[`),
  );
  if (!m) throw new Error(`rust: const ${name} not found`);
  const open = m.index + m[0].length - 1;
  const body = src.slice(open + 1, matchBracket(src, open) - 1);
  const out = [];
  for (const mem of members(body)) {
    if (!mem.text) continue;
    const lit = readStringLiteral(mem.text, 0);
    out.push(lit ? lit.value : strConst(src, mem.text));
  }
  return out;
}

/** `const NAME: &str = "…";` → string. */
export function strConst(src, name) {
  const m = src.match(new RegExp(`const\\s+${name}\\s*:\\s*&(?:'static\\s+)?str\\s*=\\s*`));
  if (!m) throw new Error(`rust: const ${name} not found`);
  return readStringLiteral(src, m.index + m[0].length).value;
}

/** `const NAME: <int type> = 123;` → number. */
export function numConst(src, name) {
  const m = src.match(
    new RegExp(`const\\s+${name}\\s*:\\s*\\w+\\s*=\\s*([0-9_]+(?:\\s*\\*\\s*[0-9_]+)*)\\s*;`),
  );
  if (!m) throw new Error(`rust: const ${name} not found`);
  return m[1].split("*").reduce((a, n) => a * Number(n.trim().replace(/_/g, "")), 1);
}

/** `const NAME: &[Row] = &[ Row { k: "v", … }, … ];` → array of { k: v } (string fields only). */
export function structTableConst(src, name) {
  const m = src.match(new RegExp(`const\\s+${name}\\s*:\\s*&\\[\\w+\\]\\s*=\\s*&\\[`));
  if (!m) throw new Error(`rust: const ${name} not found`);
  const open = m.index + m[0].length - 1;
  const body = src.slice(open + 1, matchBracket(src, open) - 1);
  return members(body)
    .filter((mem) => mem.text)
    .map((mem) => {
      const o = mem.text.indexOf("{");
      const inner = mem.text.slice(o + 1, matchBracket(mem.text, o) - 1);
      const row = {};
      for (const f of members(inner)) {
        const fm = f.text.match(/^(\w+)\s*:\s*([\s\S]+)$/);
        if (!fm) continue;
        const lit = readStringLiteral(fm[2].trim(), 0);
        row[fm[1]] = lit ? lit.value : fm[2].trim();
      }
      return row;
    });
}

/** Split comma-separated Rust code at the top level (args, list items, fields). */
export function splitTopLevel(text) {
  return members(text)
    .map((m) => m.text)
    .filter(Boolean);
}

/** Render a format!() template: `{name}` from `vars` (else `<name>`), `{{`/`}}` unescaped. */
export function renderFormat(template, vars = {}) {
  return template
    .replace(/\{\{/g, "\u0000")
    .replace(/\}\}/g, "\u0001")
    .replace(/\{(\w+)(?::[^}]*)?\}/g, (_, n) => (n in vars ? String(vars[n]) : `<${n}>`))
    .replace(/\{(?::[^}]*)?\}/g, "<value>")
    .replace(/\u0000/g, "{")
    .replace(/\u0001/g, "}");
}

/** The body text of `fn name(...) ... { … }` (without braces). */
export function fnBody(src, name) {
  const m = src.match(new RegExp(`fn\\s+${name}\\s*(?:<[^>]*>)?\\s*\\(`));
  if (!m) throw new Error(`rust: fn ${name} not found`);
  const paren = m.index + m[0].length - 1;
  const afterParams = matchBracket(src, paren);
  const open = src.indexOf("{", afterParams);
  return src.slice(open + 1, matchBracket(src, open) - 1);
}
