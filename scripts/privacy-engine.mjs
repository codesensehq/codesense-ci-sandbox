// SPDX-License-Identifier: MIT
// Shared scan engine. Rules never control filesystem traversal or allow accounting.
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { IMAGE_EXT, findImageMetadata, imageKind } from "./public-export-images.mjs";
import { GENERIC_RULES } from "./privacy-rules.mjs";
const UUID = "\\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\\b";
const NEAR = 200;
const UUID_ANY = new RegExp(UUID, "gi");

export const BAD_NAME = /\.(p8|p12|key|pem|mobileprovision)$|(^|\/)\.env(\.|$)/i;
// never in an export, at any depth (the scan does not descend into them)
const FORBIDDEN_DIRS = new Set([".git", "node_modules"]);
const DOC_EXT = /\.(md|mdx|txt|html|rst)$/i;
// the shipped docs trees: everything in them counts as docs for the 'docs' rules, whatever the extension
const DOCS_TREE = /^(?:codesense-docs|site\/src\/content)\//;
export const CHUNK = 20000;
// context each window sees on either side of the range it owns: at least the longest match any rule can make
export const OVERLAP = 1000;

export function isDoc(rel) {
  return DOC_EXT.test(rel) || /(^|\/)(README|CHANGELOG|CONTRIBUTING|SECURITY)/i.test(rel);
}

// 'text' rules: error in docs, warn elsewhere. 'docs' rules: error in docs and the shipped docs trees, off elsewhere.
function sevFor(r, rel, doc) {
  if (r.sev === "text") return doc ? "error" : "warn";
  if (r.sev === "docs") return doc || DOCS_TREE.test(rel) ? "error" : null;
  return r.sev;
}

// Stable across moved lines; changes when the line around the match is edited. Long lines use the
// match plus 80 chars either side, so one edit elsewhere in a minified file does not stale every entry.
export function fingerprint(rule, rel, lineText, at, match) {
  const ctx =
    lineText.length > 1000
      ? lineText.slice(Math.max(0, at - 80), at + match.length + 80)
      : lineText.trim();
  return createHash("sha256")
    .update(`${rule}\0${rel}\0${ctx}\0${match}`)
    .digest("hex")
    .slice(0, 20);
}

export function loadAllow(file, ruleIds = RULE_IDS) {
  if (!file || !fs.existsSync(file)) return new Map();
  return parseAllow(fs.readFileSync(file, "utf8"), ruleIds);
}

export function parseAllow(text, ruleIds = RULE_IDS) {
  const allow = new Map();
  text.split("\n").forEach((raw, i) => {
    const l = raw.trim();
    if (!l || l.startsWith("#")) return;
    const [rule, file, loc, ...rest] = l.split("|").map((s) => s.trim());
    const reason = rest.join("|").trim();
    const where = `allow file line ${i + 1}`;
    if (!rule || !file || !loc || !reason)
      throw new Error(`${where}: needs "RULE | path | fp:<20 hex>[ xN] | reason"`);
    if (!ruleIds.has(rule))
      throw new Error(`${where}: unknown rule "${rule}" (an entry names one allowable rule)`);
    if (/[*?[\]{}]/.test(file) || file.startsWith("/") || file.split("/").includes(".."))
      throw new Error(`${where}: "${file}" must be a literal relative path`);
    const m = /^(fp:[0-9a-f]{20})(?:\s+x([1-9]\d*))?$/.exec(loc);
    if (!m) throw new Error(`${where}: location must be fp:<20 hex>, optionally followed by xN`);
    const key = `${rule}\0${file}\0${m[1]}`;
    if (allow.has(key)) throw new Error(`${where}: duplicate entry (use xN for repeats)`);
    allow.set(key, { rule, file, loc: m[1], n: Number(m[2] ?? 1), reason, used: 0 });
  });
  return allow;
}

// Reviewed exemptions for one rule in one exact generated file, where an allow entry per match would go
// stale on every regeneration. Every other rule still fires in that file.
export const RULE_EXEMPT = [
  // licence-scan.mjs copies each dependency's licence text, which names its authors' addresses
  {
    rule: "email",
    file: "THIRD_PARTY_NOTICES.md",
    reason: "THIRD-PARTY: generated licence texts name the third-party authors",
  },
];
const exempt = (rule, rel) => RULE_EXEMPT.find((e) => e.rule === rule && e.file === rel);

function take(allow, rule, rel, fp) {
  const a = allow.get(`${rule}\0${rel}\0fp:${fp}`);
  if (!a || a.used >= a.n) return false;
  a.used++;
  return true;
}

// Every file under dir. Each path no file's path names goes to `bare`: an empty folder, and a link
// or other entry that is not a file (so a folder holding only links is named by theirs).
function* walk(dir, forbidden, bare = [], base = dir, skipRoot = []) {
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  if (entries.length === 0 && dir !== base)
    bare.push(path.relative(base, dir).split(path.sep).join("/"));
  for (const e of entries.sort((a, b) => (a.name < b.name ? -1 : 1))) {
    if (dir === base && skipRoot.includes(e.name)) continue;
    const p = path.join(dir, e.name);
    const rel = path.relative(base, p).split(path.sep).join("/");
    if (FORBIDDEN_DIRS.has(e.name)) forbidden.push(rel);
    else if (e.isDirectory()) yield* walk(p, forbidden, bare, base, skipRoot);
    else if (e.isFile()) yield rel;
    else bare.push(rel);
  }
}

// Window k owns match starts in [k*CHUNK, (k+1)*CHUNK) and sees OVERLAP chars either side, so a match
// is reported once, whole, and with the same \b context as an unsplit scan.
function* windows(ln) {
  if (ln.length <= CHUNK) {
    yield { base: 0, text: ln, ownStart: 0, ownEnd: Infinity };
    return;
  }
  for (let s = 0; s < ln.length; s += CHUNK) {
    const base = Math.max(0, s - OVERLAP);
    yield {
      base,
      text: ln.slice(base, s + CHUNK + OVERLAP),
      ownStart: s - base,
      ownEnd: s - base + CHUNK,
    };
  }
}

function* matches(re, ln) {
  for (const w of windows(ln)) {
    re.lastIndex = 0;
    let m;
    while ((m = re.exec(w.text))) {
      if (m.index >= w.ownEnd) break;
      if (m.index >= w.ownStart) yield { m, at: w.base + m.index };
      if (m[0] === "") re.lastIndex++;
    }
  }
}

// Printable runs of 8+ bytes, as `strings -n 8` and `strings -e l -n 8` would find them.
export function binaryStrings(buf) {
  const t = buf.toString("latin1");
  const out = [];
  for (const m of t.matchAll(/[\x20-\x7e\t]{8,}/g)) out.push({ offset: m.index, text: m[0] });
  for (const m of t.matchAll(/(?:[\x20-\x7e\t]\x00){8,}/g))
    out.push({ offset: m.index, text: m[0].replace(/\x00/g, "") });
  return out;
}

// Text = valid UTF-8 with no NUL. Anything else gets the strings pass.
const UTF8 = new TextDecoder("utf-8", { fatal: true });
function asText(buf) {
  if (buf.includes(0)) return null;
  try {
    return UTF8.decode(buf);
  } catch {
    return null;
  }
}

function newFinding(rule, sev, rel, lineNo, offsetBase, shown, fp) {
  const f = { rule, sev, file: rel, line: lineNo, shown, fp, allowed: false };
  if (offsetBase !== null) f.offset = offsetBase; // byte offset of the printable string that holds the match
  return f;
}

// `around` is the text of the lines either side, for rules whose word may sit on an adjacent line.
function scanLine(st, rel, ln, lineNo, offsetBase, sevOf, around = "") {
  const flaggedAt = new Set(); // positions of UUIDs a session-id rule reported on this line
  for (const r of st.rules) {
    const sev = sevOf(r);
    if (!sev) continue;
    const nearby = r.adjacent && r.line.test(around);
    if (r.line && !nearby && !r.line.test(ln)) continue;
    for (const { m, at } of matches(r.re, ln)) {
      if (
        r.line &&
        !nearby &&
        !r.line.test(ln.slice(Math.max(0, at - NEAR), at + m[0].length + NEAR))
      )
        continue;
      if (r.ok && r.ok(m[0], m)) continue;
      if (r.uuidAt) {
        const u = r.uuidAt(m[0]);
        if (u >= 0) {
          if (flaggedAt.has(at + u)) continue;
          flaggedAt.add(at + u);
          st.flagged.add(m[0].slice(u, u + 36).toLowerCase());
        }
      }
      const f = newFinding(
        r.id,
        sev,
        rel,
        lineNo,
        offsetBase,
        r.kind === "show" ? m[0] : null,
        fingerprint(r.id, rel, ln, at, m[0]),
      );
      if (sev === "error") {
        const e = exempt(r.id, rel);
        if (e) f.exempt = e.reason;
        f.allowed = Boolean(e) || take(st.allow, r.id, rel, f.fp);
      }
      st.findings.push(f);
    }
  }
  // every other UUID, in case a session-id rule flags it anywhere in the tree
  for (const { m, at } of matches(UUID_ANY, ln)) {
    if (!flaggedAt.has(at))
      st.uuids.push({
        uuid: m[0].toLowerCase(),
        f: newFinding(
          "session-id",
          "error",
          rel,
          lineNo,
          offsetBase,
          m[0],
          fingerprint("session-id", rel, ln, at, m[0]),
        ),
      });
  }
}

function fileFinding(st, rule, rel, what, offset = null, allowable = true) {
  const f = newFinding(rule, "error", rel, 0, offset, what, fingerprint(rule, rel, "", 0, what));
  if (allowable) f.allowed = take(st.allow, rule, rel, f.fp);
  st.findings.push(f);
}

function scanContent(st, rel, buf, strict, lineNumbers = null) {
  if (imageKind(buf) || IMAGE_EXT.test(rel)) {
    try {
      for (const x of findImageMetadata(buf))
        fileFinding(st, "image-metadata", rel, x.what, x.offset);
    } catch (e) {
      fileFinding(st, "image-metadata", rel, e.message);
    }
  }
  const text = asText(buf);
  if (text === null) {
    for (const s of binaryStrings(buf))
      scanLine(st, rel, s.text, 0, s.offset, (r) =>
        r.sev === "text" || r.sev === "docs"
          ? strict
            ? "error"
            : r.sev === "text"
              ? "warn"
              : null
          : r.sev,
      );
    return;
  }
  const doc = strict || isDoc(rel);
  const lines = text.split("\n");
  for (let i = 0; i < lines.length; i++) {
    if (lineNumbers && !lineNumbers.has(i + 1)) continue;
    scanLine(
      st,
      rel,
      lines[i],
      i + 1,
      null,
      (r) => sevFor(r, rel, doc),
      `${(lines[i - 1] ?? "").slice(-NEAR)}\n${(lines[i + 1] ?? "").slice(0, NEAR)}`,
    );
  }
}

function finish(st, checkAllow = true) {
  const { allow } = st;
  // a UUID that a session-id rule flagged anywhere is flagged at every other occurrence too
  for (const { uuid, f } of st.uuids) {
    if (!st.flagged.has(uuid)) continue;
    f.allowed = take(allow, "session-id", f.file, f.fp);
    st.findings.push(f);
  }
  // an entry that no longer matches exactly its N findings fails: it is stale, or the file now has fewer copies
  for (const a of checkAllow ? allow.values() : []) {
    if (a.used !== a.n)
      st.findings.push({
        rule: "allow-stale",
        stale: true,
        sev: "error",
        file: a.file,
        line: 0,
        shown: `${a.rule} | ${a.loc} used ${a.used} of ${a.n}`,
        allowed: false,
      });
  }
  return st.findings;
}

// Scan additions without writing historical paths into the filesystem. Only selected
// text lines are inspected, with their real line numbers and adjacent context.
// A partial scan cannot stale unused allow entries; exact match budgets still apply.
export function scanAddedFiles(files, allow = new Map(), { rules = GENERIC_RULES } = {}) {
  const st = { findings: [], allow, rules, flagged: new Set(), uuids: [] };
  for (const { file: rel, data, lineNumbers } of files) {
    if (rel.split("/").some((part) => FORBIDDEN_DIRS.has(part))) {
      fileFinding(st, "forbidden-path", rel, rel, null, false);
      continue;
    }
    if (BAD_NAME.test(rel)) fileFinding(st, "key-file", rel, rel);
    const before = st.findings.length;
    scanLine(st, rel, rel, 0, null, (r) => sevFor(r, rel, isDoc(rel)));
    for (const finding of st.findings.slice(before)) finding.path = true;
    scanContent(st, rel, data, false, lineNumbers);
  }
  return finish(st, false);
}

// strict: every file is judged as a shipped doc, and a binary's strings as strictly as text
// (the showcase seed, whose every file can reach the screen). paths: each file's path is scanned
// as a line of its own too (a file name shows in a tree), and so is each empty folder's and each
// link's, with findings marked `path: true`.
export function scan(
  dir,
  allow = new Map(),
  { strict = false, paths = false, rules = GENERIC_RULES, skipRoot = [] } = {},
) {
  const st = { findings: [], allow, rules, flagged: new Set(), uuids: [] };
  const forbidden = [];
  const bare = [];
  const scanPath = (rel) => {
    const before = st.findings.length;
    scanLine(st, rel, rel, 0, null, (r) => sevFor(r, rel, strict || isDoc(rel)));
    for (const f of st.findings.slice(before)) f.path = true;
  };
  for (const rel of walk(dir, forbidden, bare, dir, skipRoot)) {
    if (BAD_NAME.test(rel)) fileFinding(st, "key-file", rel, rel);
    if (paths) scanPath(rel);
    scanContent(st, rel, fs.readFileSync(path.join(dir, rel)), strict);
  }
  // an empty folder or a link shows in a tree too: its path is judged like a file's
  for (const rel of bare) {
    if (BAD_NAME.test(rel)) fileFinding(st, "key-file", rel, rel);
    if (paths) scanPath(rel);
  }
  // a nested .git or node_modules is never exported and cannot be allowlisted
  for (const rel of forbidden) fileFinding(st, "forbidden-path", rel, rel, null, false);
  return finish(st);
}

export function where(x) {
  return x.offset !== undefined ? `${x.file}@${x.offset}` : `${x.file}:${x.line}`;
}

// Allow lines for the given findings, grouped (identical rule/file/fp counted into xN).
export function allowLines(findings, reasonOf = () => "TODO: reason") {
  const groups = new Map();
  for (const x of findings) {
    const k = `${x.rule} | ${x.file} | fp:${x.fp}`;
    const g = groups.get(k) ?? { n: 0, x };
    g.n++;
    groups.set(k, g);
  }
  return [...groups].map(([k, g]) => `${k}${g.n > 1 ? ` x${g.n}` : ""} | ${reasonOf(g.x)}`);
}

const RULE_IDS = new Set([...GENERIC_RULES.map((r) => r.id), "key-file", "image-metadata"]); // forbidden-path is never allowable
