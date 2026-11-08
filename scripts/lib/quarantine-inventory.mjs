// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { basename, dirname, join, relative, resolve } from "node:path";

function filesUnder(root, extension) {
  if (!existsSync(root)) return [];
  return readdirSync(root, { withFileTypes: true }).flatMap((entry) => {
    if (["target", "node_modules", ".git"].includes(entry.name)) return [];
    const path = join(root, entry.name);
    return entry.isDirectory()
      ? filesUnder(path, extension)
      : entry.isFile() && path.endsWith(extension)
        ? [path]
        : [];
  });
}

// Token positions let attributes retain their string values while strings/comments
// elsewhere cannot manufacture a module, test function or quarantine marker.
function tokens(source) {
  const result = [];
  for (let i = 0; i < source.length; ) {
    if (/\s/.test(source[i])) {
      i++;
      continue;
    }
    if (source.startsWith("//", i)) {
      const end = source.indexOf("\n", i);
      i = end < 0 ? source.length : end + 1;
      continue;
    }
    if (source.startsWith("/*", i)) {
      let depth = 1;
      i += 2;
      while (depth && i < source.length) {
        if (source.startsWith("/*", i)) {
          depth++;
          i += 2;
        } else if (source.startsWith("*/", i)) {
          depth--;
          i += 2;
        } else i++;
      }
      continue;
    }
    if (source[i] === '"') {
      i++;
      while (i < source.length && source[i] !== '"') i += source[i] === "\\" ? 2 : 1;
      i++;
      continue;
    }
    const raw = source.slice(i).match(/^(?:b|c)?r(#*)"/);
    if (raw) {
      const end = source.indexOf(`"${raw[1]}`, i + raw[0].length);
      assert.ok(end >= 0, "unterminated Rust raw string");
      i = end + raw[1].length + 1;
      continue;
    }
    const character = source
      .slice(i)
      .match(/^'(?:\\u\{[0-9a-fA-F]+\}|\\x[0-9a-fA-F]{2}|\\.|[^\\'])'/u);
    if (character) {
      i += character[0].length;
      continue;
    }
    const word = source.slice(i).match(/^[A-Za-z_][A-Za-z_0-9]*/)?.[0];
    result.push({ text: word ?? source[i], at: i });
    i += word?.length ?? 1;
  }
  return result;
}

function closing(words, start) {
  const pairs = { "[": "]", "(": ")", "{": "}" };
  const stack = [pairs[words[start].text]];
  for (let index = start + 1; index < words.length; index++) {
    const value = words[index].text;
    if (Object.hasOwn(pairs, value)) stack.push(pairs[value]);
    else if (value === stack.at(-1)) {
      stack.pop();
      if (!stack.length) return index;
    }
  }
  throw new Error("unbalanced Rust item delimiters");
}

function items(source) {
  const words = tokens(source);
  const parsed = [];
  let attrs = [];
  for (let i = 0; i < words.length; i++) {
    const token = words[i];
    if (token.text === "#" && words[i + 1]?.text === "[") {
      const end = closing(words, i + 1);
      attrs.push(source.slice(token.at, words[end].at + 1));
      i = end;
      continue;
    }
    if (token.text === "mod" && words[i + 1]) {
      const name = words[++i].text;
      const next = words[++i];
      if (next?.text === "{") {
        const end = closing(words, i);
        parsed.push({
          kind: "inline",
          name,
          attrs,
          body: source.slice(next.at + 1, words[end].at),
        });
        i = end;
      } else if (next?.text === ";") parsed.push({ kind: "external", name, attrs });
      attrs = [];
      continue;
    }
    if (token.text === "fn" && words[i + 1]) {
      parsed.push({ kind: "fn", name: words[++i].text, attrs, at: token.at });
      attrs = [];
      // Test functions are free items; nested function bodies cannot declare a
      // second test in the surrounding module.
      while (words[i + 1] && !["{", ";"].includes(words[i + 1].text)) i++;
      if (words[i + 1]?.text === "{") {
        i = closing(words, i + 1);
      }
      continue;
    }
    if (["{", "(", "["].includes(token.text)) {
      i = closing(words, i);
      // Visibility modifiers may have parentheses before the item keyword.
      if (token.text === "{") attrs = [];
    } else if (token.text === ";") attrs = [];
  }
  return parsed;
}

/** Parse every Rust source, including files outside the active module graph. */
export function validateRustSources(root = join(process.cwd(), "src-tauri")) {
  for (const file of filesUnder(root, ".rs")) {
    try {
      items(readFileSync(file, "utf8"));
    } catch (error) {
      throw new Error(`${file}: ${error.message}`, { cause: error });
    }
  }
}

const CANONICAL_QUARANTINE =
  /^#\[\s*ignore\s*=\s*"quarantine: (https:\/\/github\.com\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/issues\/[1-9][0-9]*)"\s*\]$/;

function quarantineCount(source, file) {
  const words = tokens(source);
  let count = 0;
  for (let i = 0; i < words.length; i++) {
    if (words[i].text !== "#" || words[i + 1]?.text !== "[") continue;
    const end = closing(words, i + 1);
    const attr = source.slice(words[i].at, words[end].at + 1);
    const ignorePositions = words
      .slice(i + 2, end)
      .flatMap((word, index) =>
        word.text === "ignore" && ["[", ","].includes(words[i + 1 + index]?.text)
          ? [i + 2 + index]
          : [],
      );
    for (const position of ignorePositions) {
      assert.equal(words[position + 1]?.text, "=", `${file}: ignore attribute needs a reason`);
    }
    if (ignorePositions.length && attr.includes("quarantine:")) {
      assert.match(
        attr,
        CANONICAL_QUARANTINE,
        `${file}: quarantine ignore must use canonical #[ignore = "quarantine: <issue-url>"] form`,
      );
      count++;
    }
    i = end;
  }
  return count;
}

function field(section, key) {
  return section?.match(new RegExp(`^\\s*${key}\\s*=\\s*"([^"]+)"`, "m"))?.[1];
}

function crateRoots(manifest) {
  const source = readFileSync(manifest, "utf8");
  const sections = [...source.matchAll(/^\[(\[?[^\]\n]+\]?)\]\s*\n([^]*?)(?=^\[|$(?![^]))/gm)];
  const packageName = field(sections.find((s) => s[1] === "package")?.[2], "name");
  if (!packageName) return [];
  const root = dirname(manifest);
  const library = sections.find((s) => s[1] === "lib")?.[2];
  const roots = [];
  const add = (path, name) => {
    if (existsSync(path)) roots.push({ path, name: name.replaceAll("-", "_") });
  };
  add(join(root, field(library, "path") ?? "src/lib.rs"), field(library, "name") ?? packageName);
  add(join(root, "src/main.rs"), packageName);
  for (const section of sections.filter((s) => s[1] === "[bin]" || s[1] === "[test]")) {
    const path = field(section[2], "path");
    if (path) add(join(root, path), field(section[2], "name") ?? packageName);
  }
  for (const directory of ["tests", "src/bin"]) {
    const path = join(root, directory);
    if (!existsSync(path)) continue;
    for (const entry of readdirSync(path, { withFileTypes: true })) {
      if (entry.isFile() && entry.name.endsWith(".rs"))
        add(join(path, entry.name), basename(entry.name, ".rs"));
      else if (entry.isDirectory() && directory === "src/bin")
        add(join(path, entry.name, "main.rs"), entry.name);
    }
  }
  return roots;
}

export function validateInventory(entries, root = process.cwd()) {
  for (const entry of entries) {
    if (!/^(vitest|playwright):/.test(entry.id)) continue;
    const file = entry.id.slice(entry.id.indexOf(":") + 1).split("#")[0];
    const path = resolve(root, file);
    assert.ok(existsSync(path) && statSync(path).isFile(), `${entry.id}: test file does not exist`);
  }
  const rustRoot = join(root, "src-tauri");
  const rustFiles = filesUnder(rustRoot, ".rs");
  const quarantinedFiles = new Map(
    rustFiles
      .map((path) => [path, quarantineCount(readFileSync(path, "utf8"), path)])
      .filter(([, count]) => count),
  );
  const cargo = entries.filter((entry) => entry.id.startsWith("cargo:"));
  if (quarantinedFiles.size === 0) {
    assert.equal(
      cargo.length,
      0,
      `${cargo[0]?.id}: ledger entry has no matching quarantine attribute`,
    );
    return;
  }
  const resolvedMarkers = new Map();
  const attributes = new Map();
  const visited = new Set();
  function visit(path, modules, crate) {
    const key = `${crate}:${path}:${modules.join("::")}`;
    if (visited.has(key)) return;
    visited.add(key);
    const base = ["lib.rs", "main.rs", "mod.rs"].includes(basename(path))
      ? dirname(path)
      : join(dirname(path), basename(path, ".rs"));
    scan(readFileSync(path, "utf8"), modules, base, dirname(path), crate, path);
  }
  function scan(source, modules, moduleDir, attributeDir, crate, file) {
    for (const item of items(source)) {
      const names = [...modules, item.name];
      if (item.kind === "fn") {
        const issue = item.attrs.map((attr) => attr.match(CANONICAL_QUARANTINE)?.[1]).find(Boolean);
        if (issue) {
          attributes.set(`cargo:${[crate, ...names].join("::")}`, issue);
          if (!resolvedMarkers.has(file)) resolvedMarkers.set(file, new Set());
          resolvedMarkers.get(file).add(`${modules.join("::")}:${item.name}:${item.at}`);
        }
      } else if (item.kind === "inline") {
        const nestedDir = join(moduleDir, item.name);
        scan(item.body, names, nestedDir, nestedDir, crate, file);
      } else {
        const override = item.attrs
          .map((attr) => attr.match(/^#\[\s*path\s*=\s*"([^"]+)"\s*\]$/)?.[1])
          .find(Boolean);
        const candidates = override
          ? [join(attributeDir, override)]
          : [join(moduleDir, `${item.name}.rs`), join(moduleDir, item.name, "mod.rs")];
        const child = candidates.find(existsSync);
        if (child) visit(child, names, crate);
      }
    }
  }
  for (const manifest of filesUnder(rustRoot, "Cargo.toml")) {
    for (const crate of crateRoots(manifest)) visit(crate.path, [], crate.name);
  }
  const unresolved = [...quarantinedFiles].filter(
    ([file, count]) => (resolvedMarkers.get(file)?.size ?? 0) < count,
  );
  assert.equal(
    unresolved.length,
    0,
    `quarantine attributes outside resolved Rust modules: ${unresolved.map(([p]) => relative(root, p)).join(", ")}`,
  );
  for (const [id, issue] of attributes) {
    assert.ok(
      cargo.some((entry) => entry.id === id && entry.issue === issue),
      `${id}: quarantine attribute has no matching ledger issue`,
    );
  }
  for (const entry of cargo) {
    assert.equal(
      attributes.get(entry.id),
      entry.issue,
      `${entry.id}: ledger entry has no matching quarantine attribute`,
    );
  }
}
