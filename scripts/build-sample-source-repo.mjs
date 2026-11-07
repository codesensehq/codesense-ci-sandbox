#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Builds the sample source repo bundled with the CodeSense docs (SW-ODC-11).
//
// The output is a gzipped tar of a real git repository's `.git` directory: a 5-commit
// chain on `main` whose trees are built from `codesense-docs/sample-source-repo/commits/`
// in natural order, plus an `origin` remote. The app places it under the CodeSense home
// on launch (via `ensure_sample_repo`) and checks the newest commit out with libgit2, so
// the placed folder is an ordinary clone agents in the sample demo can inspect.
//
// Deterministic: author + base date come from `codesense-docs/sample-source-repo/bundle.json`,
// each commit's timestamp is `committedAtBase + (index * committedAtStep)` so the same
// sources always produce the same commit ids and the same archive bytes. No git binary
// is needed; objects are written directly.
//
// Usage: node scripts/build-sample-source-repo.mjs [output.tar.gz]
// Default output: src-tauri/resources/sample-source-repo.tar.gz
//                 (only rewritten when it changes).

import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { deflateSync, gzipSync } from "node:zlib";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const TEMPLATE = join(ROOT, "codesense-docs", "sample-source-repo");
const COMMITS_DIR = join(TEMPLATE, "commits");

const bundle = JSON.parse(readFileSync(join(TEMPLATE, "bundle.json"), "utf8"));

/** The commit manifests, sorted by their natural (numeric-prefix) order. */
function listCommits() {
  const entries = readdirSync(COMMITS_DIR)
    .filter((name) => name.endsWith(".json"))
    .sort();
  if (entries.length === 0) {
    throw new Error(`No commit manifests under ${COMMITS_DIR}. Expected at least one.`);
  }
  return entries.map((name) => ({
    name,
    data: JSON.parse(readFileSync(join(COMMITS_DIR, name), "utf8")),
  }));
}

/** Each commit's files, keyed by POSIX path, with per-commit overlay semantics:
 *  commit N's `files` is a sparse delta; the full tree at N is every earlier
 *  overlay plus N's own, keyed by path (later writes override earlier ones). */
export function buildCommitTrees(commits) {
  const trees = [];
  const running = new Map();
  for (const commit of commits) {
    if (!commit.data.files || typeof commit.data.files !== "object") {
      throw new Error(`Commit manifest ${commit.name} is missing a 'files' object.`);
    }
    for (const [path, contents] of Object.entries(commit.data.files)) {
      if (typeof contents !== "string") {
        throw new Error(
          `Commit ${commit.name}: file ${path} content must be a string (got ${typeof contents}).`,
        );
      }
      running.set(path, Buffer.from(contents, "utf8"));
    }
    trees.push(new Map(running));
  }
  return trees;
}

// ---------------------------------------------------------------------------
// Git objects
// ---------------------------------------------------------------------------

function gitObject(objects, type, payload) {
  const body = Buffer.concat([Buffer.from(`${type} ${payload.length}\0`), payload]);
  const id = createHash("sha1").update(body).digest("hex");
  if (!objects.has(id)) objects.set(id, deflateSync(body));
  return id;
}

/** Writes the tree for `files` (path → bytes) and returns its id. */
function writeTree(objects, files) {
  const root = new Map();
  for (const [path, data] of files) {
    const parts = path.split("/");
    let node = root;
    for (const part of parts.slice(0, -1)) {
      if (!node.has(part)) node.set(part, new Map());
      node = node.get(part);
    }
    node.set(parts.at(-1), data);
  }
  const write = (node) => {
    const entries = [...node].map(([name, value]) =>
      value instanceof Map
        ? { name, key: `${name}/`, mode: "40000", id: write(value) }
        : { name, key: name, mode: "100644", id: gitObject(objects, "blob", value) },
    );
    // Git orders entries byte-wise, comparing a directory as if its name ended in "/".
    entries.sort((a, b) => Buffer.compare(Buffer.from(a.key), Buffer.from(b.key)));
    const payload = Buffer.concat(
      entries.flatMap((entry) => [
        Buffer.from(`${entry.mode} ${entry.name}\0`),
        Buffer.from(entry.id, "hex"),
      ]),
    );
    return gitObject(objects, "tree", payload);
  };
  return write(root);
}

export function buildRepository(commits, trees) {
  if (commits.length !== trees.length) {
    throw new Error("commit/tree arity mismatch");
  }
  const objects = new Map();
  let previous = null;
  let tip = null;
  for (let i = 0; i < commits.length; i += 1) {
    const commit = commits[i];
    const tree = writeTree(objects, trees[i]);
    const committedAt = bundle.committedAtBase + i * bundle.committedAtStep;
    const author = commit.data.author || bundle.author;
    const signature = `${author} ${committedAt} +0000`;
    const parent = previous ? `parent ${previous}\n` : "";
    const message = commit.data.message || `commit ${i + 1}`;
    const payload = Buffer.from(
      `tree ${tree}\n${parent}author ${signature}\ncommitter ${signature}\n\n${message}\n`,
      "utf8",
    );
    const id = gitObject(objects, "commit", payload);
    previous = id;
    tip = id;
  }
  const config = [
    "[core]",
    "\trepositoryformatversion = 0",
    "\tfilemode = true",
    "\tbare = false",
    "\tlogallrefupdates = true",
    '[remote "origin"]',
    `\turl = ${bundle.remoteUrl}`,
    "\tfetch = +refs/heads/*:refs/remotes/origin/*",
    `[branch "${bundle.branch}"]`,
    "\tremote = origin",
    `\tmerge = refs/heads/${bundle.branch}`,
    "",
  ].join("\n");
  /** @type {Array<[string, Buffer]>} */
  const entries = [
    [".git/HEAD", Buffer.from(`ref: refs/heads/${bundle.branch}\n`)],
    [".git/config", Buffer.from(config)],
    [`.git/refs/heads/${bundle.branch}`, Buffer.from(`${tip}\n`)],
    [`.git/refs/remotes/origin/${bundle.branch}`, Buffer.from(`${tip}\n`)],
    ...[...objects]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([id, data]) => [`.git/objects/${id.slice(0, 2)}/${id.slice(2)}`, data]),
  ];
  return { commit: tip, entries };
}

// ---------------------------------------------------------------------------
// Deterministic tar.gz (same shape as build-codesense-docs.mjs)
// ---------------------------------------------------------------------------

function tarHeader(name, size, mtime) {
  const header = Buffer.alloc(512);
  const put = (value, offset, length) => header.write(value, offset, length, "utf8");
  const octal = (value, length) => value.toString(8).padStart(length - 1, "0");
  if (Buffer.byteLength(name) > 100) throw new Error(`tar path too long: ${name}`);
  put(name, 0, 100);
  put(octal(0o644, 8), 100, 8);
  put(octal(0, 8), 108, 8);
  put(octal(0, 8), 116, 8);
  put(octal(size, 12), 124, 12);
  put(octal(mtime, 12), 136, 12);
  put("        ", 148, 8);
  put("0", 156, 1);
  put("ustar\0", 257, 6);
  put("00", 263, 2);
  let sum = 0;
  for (const byte of header) sum += byte;
  put(`${octal(sum, 7)}\0`, 148, 8);
  return header;
}

export function tarGz(entries, mtime) {
  const parts = [];
  for (const [name, data] of entries) {
    parts.push(tarHeader(name, data.length, mtime), data);
    const pad = (512 - (data.length % 512)) % 512;
    if (pad) parts.push(Buffer.alloc(pad));
  }
  parts.push(Buffer.alloc(1024));
  return gzipSync(Buffer.concat(parts), { level: 9 });
}

/** Every input the archive is built from, for `cargo:rerun-if-changed`. */
export function buildInputs() {
  return [fileURLToPath(import.meta.url), TEMPLATE];
}

function main() {
  if (process.argv[2] === "--list-inputs") {
    for (const input of buildInputs()) console.log(input);
    return;
  }
  const output = resolve(
    process.argv[2] ?? join(ROOT, "src-tauri", "resources", "sample-source-repo.tar.gz"),
  );
  const commits = listCommits();
  const trees = buildCommitTrees(commits);
  const { commit, entries } = buildRepository(commits, trees);
  const finalCommittedAt = bundle.committedAtBase + (commits.length - 1) * bundle.committedAtStep;
  const archive = tarGz(entries, finalCommittedAt);
  if (existsSync(output) && readFileSync(output).equals(archive)) {
    console.log(`sample-source-repo: ${commit} unchanged (${output})`);
    return;
  }
  mkdirSync(dirname(output), { recursive: true });
  const temp = `${output}.${process.pid}.${Date.now()}.tmp`;
  writeFileSync(temp, archive);
  renameSync(temp, output);
  console.log(`sample-source-repo: ${commit} → ${output} (${archive.length} bytes)`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
