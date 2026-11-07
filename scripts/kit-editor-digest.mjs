// SPDX-License-Identifier: MIT
import { readFileSync, existsSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, resolve, relative } from "node:path";
import ts from "typescript";

const EXTRA_INPUTS = [
  "scripts/build-kit-editor.mjs",
  "scripts/kit-editor-digest.mjs",
  "src/styles/scratch-note.css",
  "src/styles/status-mark.css",
];

function packageName(specifier) {
  const parts = specifier.split("/");
  return specifier.startsWith("@") ? parts.slice(0, 2).join("/") : parts[0];
}

/** Module specifiers the bundle keeps; type-only declarations are erased, so they are skipped. */
function runtimeSpecifiers(file) {
  const source = ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest);
  const specifiers = [];
  for (const node of source.statements) {
    if (ts.isImportDeclaration(node)) {
      if (!node.importClause?.isTypeOnly) specifiers.push(node.moduleSpecifier.text);
    } else if (ts.isExportDeclaration(node)) {
      if (!node.isTypeOnly && node.moduleSpecifier) specifiers.push(node.moduleSpecifier.text);
    } else if (
      ts.isImportEqualsDeclaration(node) &&
      !node.isTypeOnly &&
      ts.isExternalModuleReference(node.moduleReference) &&
      ts.isStringLiteral(node.moduleReference.expression)
    ) {
      specifiers.push(node.moduleReference.expression.text);
    }
  }
  return specifiers;
}

/** Local files and bare npm package names reachable from the entry. */
export function collectGraph(root, entry) {
  const files = new Set();
  const packages = new Set();
  const visit = (file) => {
    if (files.has(file)) return;
    files.add(file);
    for (const name of runtimeSpecifiers(file)) {
      const base = name.startsWith("@/")
        ? resolve(root, "src", name.slice(2))
        : name.startsWith(".")
          ? resolve(dirname(file), name)
          : null;
      if (!base) {
        if (!name.startsWith("node:")) packages.add(packageName(name));
        continue;
      }
      const dep = [base, `${base}.ts`, `${base}.tsx`, `${base}/index.ts`].find((p) =>
        existsSync(p),
      );
      if (dep) visit(dep);
    }
  };
  visit(entry);
  return { files, packages };
}

const unquote = (text) => text.replace(/^'(.*)'$/, "$1");
const withoutPeers = (key) => key.split("(")[0];
// A lockfile reference is a bare version, or `real-name@version` for an npm alias.
const edgeKey = (name, ref) => {
  const value = unquote(ref);
  return /^@?[^@(\s]+@/.test(value) ? value : `${name}@${value}`;
};

function lockSections(lock) {
  const lines = lock.split("\n");
  const rootImporter = new Map();
  const snapshots = new Map();
  let section = "";
  let importer = "";
  let entryName = "";
  let snapshot = null;
  for (const line of lines) {
    if (/^\S/.test(line)) {
      section = line.replace(":", "");
      continue;
    }
    if (section === "importers") {
      const head = line.match(/^ {2}(\S.*):$/);
      if (head) importer = head[1];
      if (importer !== ".") continue;
      const name = line.match(/^ {6}(\S.*):$/);
      if (name) entryName = unquote(name[1]);
      const version = line.match(/^ {8}version: (.+)$/);
      if (version) rootImporter.set(entryName, version[1]);
    } else if (section === "snapshots") {
      const head = line.match(/^ {2}(\S.*?):(?: \{\})?$/);
      if (head) {
        snapshot = [];
        snapshots.set(unquote(head[1]), snapshot);
        continue;
      }
      const dep = line.match(/^ {6}(\S.*?): (.+)$/);
      if (dep && snapshot) snapshot.push(edgeKey(unquote(dep[1]), dep[2]));
    }
  }
  return { rootImporter, snapshots };
}

/** Resolved `name@version` of the packages and transitive deps the bundle can include. */
export function bundledPackages(lock, packages) {
  const { rootImporter, snapshots } = lockSections(lock);
  const seen = new Set();
  const queue = [];
  for (const name of packages) {
    const version = rootImporter.get(name);
    if (version) queue.push(edgeKey(name, version));
  }
  while (queue.length) {
    const key = queue.pop();
    if (seen.has(key)) continue;
    seen.add(key);
    queue.push(...(snapshots.get(key) ?? []));
  }
  return [...new Set([...seen].map(withoutPeers))].sort();
}

export function tiptapVersion(lock) {
  return lock.match(/'@tiptap\/core@([^(']+)[(']/)?.[1];
}

export function editorDigest(root, entry) {
  const { files, packages } = collectGraph(root, entry);
  // Vite, Rollup and esbuild generate and minify the bundle, so their versions are inputs too.
  const builder = collectGraph(root, resolve(root, "scripts/build-kit-editor.mjs"));
  for (const name of builder.packages) packages.add(name);
  for (const file of EXTRA_INPUTS) files.add(resolve(root, file));
  const lock = readFileSync(resolve(root, "pnpm-lock.yaml"), "utf8");
  const hash = createHash("sha256");
  for (const file of [...files].sort())
    hash.update(relative(root, file)).update("\0").update(readFileSync(file));
  hash.update("packages\0").update(bundledPackages(lock, packages).join("\n"));
  return { digest: hash.digest("hex").slice(0, 20), tiptap: tiptapVersion(lock) };
}
