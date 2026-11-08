#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Usage: node scripts/check-api-skew.mjs [<file-or-dir> ...]
//        node scripts/check-api-skew.mjs --record <git-ref> <version>
//        node scripts/check-api-skew.mjs --docs [--check]
//
// Installed apps lag behind the content they load: a team pane written today can open in an app that
// postponed its updates. This check fails when shipped pane, dashboard or docs-example code uses a
// `window.codesense` member that an older supported app lacks, without a feature check first.
//
// Each scripts/api-surface/<version>.json records the `window.codesense` members one release
// defines. The supported surface is the members every recorded release has, so deleting a file drops
// support for that release.
//
// A use is any read of a member: a call, a read through it (`cs.kit.icon`), a value read
// (`const fn = cs.m`, `list.forEach(cs.m)`), `cs["m"]` and destructuring (`const { m } = cs`).
// Names bound to the API or to a member are followed by scope (`const kit = cs.kit`, `{ kit } = cs`,
// `window.codesense.kit`). A use of a member outside the supported surface needs a guard:
// `cs.m?.()`, `typeof cs.m === "function"`, `if (cs.m)`, `cs.m && …`, `cs.m ? … : …`, `"m" in cs`,
// a destructuring default (`{ m = noop }`), a const holding one of those, or an early
// `if (!cs.m) return;` in the same block. Testing a member (`typeof`, `!`, a condition, a
// comparison, the left of `&&`/`||`/`??`) is not a use. `cs[name]` with a computed name always
// fails unless its line carries `// api-skew-ok: <reason>`. Members a script defines (`cs.x = …`)
// count only in that script; shared helpers other pages read are listed, with a reason, in
// SHARED_MEMBERS. A function parameter follows its default (`cs = window.codesense`); with no
// provable binding, a parameter named cs, codesense or kit is taken as the API or its kit.
// Limits: a guard is recognised only in those shapes, so a check made in another function, or a
// flag held in a `let` or object field, does not count; restructure, or guard at the use.
//
// --record reads only git objects at <git-ref>: the raw-string JavaScript constants and the
// include_str! JavaScript files of src-tauri/src/http/routes/viewer.rs (the bridge shim) and every
// .js file in src-tauri/src/http/routes/dashboard_kit/ (the kit). It writes the members those
// sources put on `window.codesense` to scripts/api-surface/<version>.json.
// --docs writes the supported surface into the docs page between its api-skew:surface markers.
import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, statSync, writeFileSync, mkdirSync } from "node:fs";
import { extname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";
import { builtInPaneFiles } from "./lib/builtin-panes.mjs";

const ROOT = resolve(fileURLToPath(new URL("..", import.meta.url)));
export const SURFACE_DIR = "scripts/api-surface";
const VIEWER = "src-tauri/src/http/routes/viewer.rs";
const KIT_DIR = "src-tauri/src/http/routes/dashboard_kit";

// Shipped content: the built-in panes and dashboards, and the docs content teams copy from.
export const DEFAULT_SCAN = [
  ...builtInPaneFiles(ROOT),
  "src-tauri/assets/dashboards",
  "codesense-docs/getting-started/content",
];
const SCANNED = new Set([".html", ".htm", ".js", ".mjs"]);
const SKIPPED = /(?:^|\/)(?:node_modules|generated)(?:\/|$)|\.(?:test|spec)\.|(?:^|\/)build\.mjs$/;

function parse(name, text) {
  return ts.createSourceFile(name, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
}

function unwrap(node) {
  while (
    node &&
    (ts.isParenthesizedExpression(node) ||
      ts.isAsExpression?.(node) ||
      ts.isNonNullExpression?.(node))
  )
    node = node.expression;
  return node;
}

function isWindowCodesense(node) {
  node = unwrap(node);
  return (
    node &&
    ts.isPropertyAccessExpression(node) &&
    node.name.text === "codesense" &&
    ts.isIdentifier(node.expression) &&
    node.expression.text === "window"
  );
}

const VIRTUAL = "/api-skew/script.js";
const COMPILER = { allowJs: true, noLib: true, noResolve: true, types: [] };

// One parsed script with a binder, so names resolve by scope: a `cs` or a guard flag declared in one
// function never stands for another of the same name.
function analyze(text) {
  const file = parse(VIRTUAL, text);
  const host = {
    getSourceFile: (n) => (n === VIRTUAL ? file : undefined),
    fileExists: (n) => n === VIRTUAL,
    readFile: () => text,
    getDefaultLibFileName: () => "lib.d.ts",
    writeFile: () => {},
    getCurrentDirectory: () => "/",
    getCanonicalFileName: (n) => n,
    useCaseSensitiveFileNames: () => true,
    getNewLine: () => "\n",
    getDirectories: () => [],
  };
  const checker = ts.createProgram([VIRTUAL], COMPILER, host).getTypeChecker();
  const symbol = (id) => checker.getSymbolAtLocation(id);
  // Local names bound to `window.codesense` ([]) or to one of its members (["kit"]).
  const bound = new Map();
  // The member path an expression reads: [] for the API itself, null when it is not the API.
  const resolve = (node) => {
    node = unwrap(node);
    if (!node) return null;
    if (isWindowCodesense(node)) return [];
    if (ts.isIdentifier(node)) {
      const s = symbol(node);
      return s ? (bound.get(s) ?? null) : node.text === "codesense" ? [] : null;
    }
    if (ts.isPropertyAccessExpression(node)) {
      const base = resolve(node.expression);
      return base && [...base, node.name.text];
    }
    if (ts.isElementAccessExpression(node) && ts.isStringLiteralLike(node.argumentExpression)) {
      const base = resolve(node.expression);
      return base && [...base, node.argumentExpression.text];
    }
    if (ts.isBinaryExpression(node)) {
      const op = node.operatorToken.kind;
      if (op === ts.SyntaxKind.EqualsToken)
        return resolve(node.right) ?? (isWindowCodesense(node.left) ? [] : null);
      if (op === ts.SyntaxKind.BarBarToken || op === ts.SyntaxKind.QuestionQuestionToken)
        return resolve(node.left);
    }
    return null;
  };
  const bind = (name, path) => {
    if (!path) return false;
    if (ts.isIdentifier(name)) {
      const s = symbol(name);
      if (!s || bound.has(s)) return false;
      bound.set(s, path);
      return true;
    }
    let grew = false;
    if (ts.isObjectBindingPattern(name))
      for (const el of name.elements) {
        const key = bindingKey(el);
        if (key !== null) grew = bind(el.name, [...path, key]) || grew;
      }
    return grew;
  };
  const flags = new Map();
  let grew = true;
  while (grew) {
    grew = false;
    const visit = (node) => {
      if (ts.isVariableDeclaration(node) && node.initializer) {
        grew = bind(node.name, resolve(node.initializer)) || grew;
        if (ts.isIdentifier(node.name) && node.parent.flags & ts.NodeFlags.Const) {
          const s = symbol(node.name);
          if (s) flags.set(s, node.initializer);
        }
      }
      if (
        ts.isBinaryExpression(node) &&
        node.operatorToken.kind === ts.SyntaxKind.EqualsToken &&
        ts.isIdentifier(node.left)
      )
        grew = bind(node.left, resolve(node.right)) || grew;
      // A parameter binds through its default (`cs = window.codesense`, `{ cs = window.codesense }`);
      // with no provable binding, a parameter named cs, codesense or kit is taken as the API.
      if (ts.isParameter(node) || (ts.isBindingElement(node) && inParameter(node))) {
        const byDefault = node.initializer ? resolve(node.initializer) : null;
        const byName = ts.isIdentifier(node.name) ? PARAM_NAMES.get(node.name.text) : undefined;
        grew = bind(node.name, byDefault ?? byName ?? null) || grew;
      }
      ts.forEachChild(node, visit);
    };
    visit(file);
  }
  const flag = (id) => {
    const s = symbol(id);
    return s ? flags.get(s) : undefined;
  };
  return { file, resolve, flag, symbol };
}

// Members that shared content scripts define for other pages, with the reason each is safe. A member
// a script defines itself counts only within that script.
export const SHARED_MEMBERS = new Map([
  [
    "docs",
    "codesense-docs/getting-started/content/_shared/create-repo-primer.js defines codesense.docs for the docs pages that load it; it is not app API, so every app version has it on those pages",
  ],
]);

// Parameter names taken as the API when a caller's argument cannot be followed.
const PARAM_NAMES = new Map([
  ["cs", []],
  ["codesense", []],
  ["kit", ["kit"]],
]);

function inParameter(node) {
  for (let at = node.parent; at; at = at.parent) {
    if (ts.isParameter(at)) return true;
    if (!ts.isObjectBindingPattern(at) && !ts.isArrayBindingPattern(at) && !ts.isBindingElement(at))
      return false;
  }
  return false;
}

// The member name a destructuring element reads (`{kit}`, `{onTeardown: td}`), or null.
function bindingKey(el) {
  if (el.dotDotDotToken) return null;
  const key = el.propertyName ?? el.name;
  return ts.isIdentifier(key) || ts.isStringLiteralLike(key) ? key.text : null;
}

const pathKey = (path) => path.join(".");

// --- recording a release's surface

function objectKeys(node) {
  node = unwrap(node);
  const keys = new Map();
  const add = (obj) => {
    for (const prop of obj.properties) {
      const name =
        prop.name && (ts.isIdentifier(prop.name) || ts.isStringLiteral(prop.name))
          ? prop.name.text
          : null;
      if (!name) continue;
      keys.set(name, ts.isPropertyAssignment(prop) ? objectKeys(prop.initializer) : new Map());
    }
  };
  if (node && ts.isObjectLiteralExpression(node)) add(node);
  // Object.assign({…}, {…}) merges literal arguments.
  if (
    node &&
    ts.isCallExpression(node) &&
    ts.isPropertyAccessExpression(node.expression) &&
    node.expression.name.text === "assign"
  )
    for (const arg of node.arguments) for (const [k, v] of objectKeys(arg)) keys.set(k, v);
  return keys;
}

function flatten(keys, prefix, out) {
  for (const [name, children] of keys) {
    const path = prefix ? `${prefix}.${name}` : name;
    out.add(path);
    flatten(children, path, out);
  }
}

// Members one JavaScript source puts on `window.codesense`.
export function surfaceOf(text) {
  return surfaceFrom(analyze(text));
}

function surfaceFrom({ file, resolve, symbol }) {
  const out = new Set();
  const literals = new Map();
  // `cs[k] = …` inside `node`, where cs is the API or one of its members: that member's path.
  const copyTarget = (node) => {
    let target = null;
    const find = (n) => {
      if (
        ts.isBinaryExpression(n) &&
        n.operatorToken.kind === ts.SyntaxKind.EqualsToken &&
        ts.isElementAccessExpression(n.left) &&
        !ts.isStringLiteralLike(n.left.argumentExpression)
      )
        target ??= resolve(n.left.expression);
      ts.forEachChild(n, find);
    };
    find(node);
    return target;
  };
  const visit = (node) => {
    if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.initializer) {
      const s = symbol(node.name);
      if (s) literals.set(s, objectKeys(node.initializer));
    }
    if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.EqualsToken) {
      const left = unwrap(node.left);
      const path = ts.isIdentifier(left) ? null : isWindowCodesense(left) ? [] : resolve(left);
      if (path) {
        if (path.length) out.add(pathKey(path));
        flatten(objectKeys(node.right), pathKey(path), out);
      }
    }
    // Object.assign(cs.kit, {…}) adds to an existing member.
    if (
      ts.isCallExpression(node) &&
      ts.isPropertyAccessExpression(node.expression) &&
      node.expression.name.text === "assign" &&
      node.arguments.length > 1
    ) {
      const path = resolve(node.arguments[0]);
      if (path)
        for (const arg of node.arguments.slice(1)) flatten(objectKeys(arg), pathKey(path), out);
    }
    // for (var k in m) cs[k] = m[k]
    if (ts.isForInStatement(node) && ts.isIdentifier(unwrap(node.expression))) {
      const path = copyTarget(node.statement);
      const s = symbol(unwrap(node.expression));
      if (path && s) flatten(literals.get(s) ?? new Map(), pathKey(path), out);
    }
    // ["a", "b"].forEach(function (n) { … cs[n] = … })
    if (
      ts.isCallExpression(node) &&
      ts.isPropertyAccessExpression(node.expression) &&
      node.expression.name.text === "forEach" &&
      ts.isArrayLiteralExpression(unwrap(node.expression.expression))
    ) {
      const path = node.arguments.map(copyTarget).find(Boolean);
      if (path)
        for (const el of unwrap(node.expression.expression).elements)
          if (ts.isStringLiteralLike(el)) out.add(pathKey([...path, el.text]));
    }
    ts.forEachChild(node, visit);
  };
  visit(file);
  return out;
}

function gitShow(ref, path) {
  return execFileSync("git", ["show", `${ref}:${path}`], {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 1 << 28,
  });
}

// The JavaScript sources that define `window.codesense` at <ref>.
export function surfaceSources(ref) {
  const sources = [];
  const viewer = gitShow(ref, VIEWER);
  for (const m of viewer.matchAll(/^const [A-Z_]+: &str = r(#+)"([\s\S]*?)"\1;/gm))
    if (m[2].includes("codesense")) sources.push({ path: VIEWER, text: m[2] });
  for (const m of viewer.matchAll(/^const [A-Z_]+: &str = include_str!\("([^"]+\.js)"\);/gm)) {
    const path = `src-tauri/src/http/routes/${m[1]}`;
    sources.push({ path, text: gitShow(ref, path) });
  }
  const kit = execFileSync("git", ["ls-tree", "--name-only", `${ref}:${KIT_DIR}`], {
    cwd: ROOT,
    encoding: "utf8",
  });
  for (const name of kit.split("\n").filter((n) => n.endsWith(".js")))
    sources.push({ path: `${KIT_DIR}/${name}`, text: gitShow(ref, `${KIT_DIR}/${name}`) });
  return sources;
}

export function recordSurface(ref, version) {
  const sha = execFileSync("git", ["rev-parse", `${ref}^{commit}`], {
    cwd: ROOT,
    encoding: "utf8",
  }).trim();
  const sources = surfaceSources(sha);
  const members = new Set();
  for (const { path, text } of sources) for (const m of surfaceOf(text)) members.add(m);
  return {
    version,
    // the snapshot is exported, and the recording commit is private history: never write its SHA
    source: {
      commit: `v${version} bump (private)`,
      files: [...new Set(sources.map((s) => s.path))],
      command: `node scripts/check-api-skew.mjs --record <commit> ${version}`,
    },
    members: [...members].sort(),
  };
}

// --- checking content

export function loadSurface(dir = join(ROOT, SURFACE_DIR)) {
  const releases = readdirSync(dir)
    .filter((n) => n.endsWith(".json"))
    .map((n) => JSON.parse(readFileSync(join(dir, n), "utf8")));
  if (!releases.length) throw new Error(`no API surface snapshots in ${dir}`);
  const [first, ...rest] = releases;
  const members = new Set(first.members.filter((m) => rest.every((r) => r.members.includes(m))));
  const versions = releases
    .map((r) => r.version)
    .sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
  return { members, oldest: versions[0], versions };
}

function hasChildren(members, prefix) {
  const p = `${prefix}.`;
  for (const m of members) if (m.startsWith(p)) return true;
  return false;
}

const TYPEOF_PRESENT = new Set(["function", "object"]);

// Whether `test` holding true (or false, when `negated`) proves member `key` is present.
function proves(test, key, an, negated = false) {
  test = unwrap(test);
  if (!test) return false;
  if (ts.isPrefixUnaryExpression(test) && test.operator === ts.SyntaxKind.ExclamationToken)
    return proves(test.operand, key, an, !negated);
  if (ts.isBinaryExpression(test)) {
    const op = test.operatorToken.kind;
    const and = op === ts.SyntaxKind.AmpersandAmpersandToken;
    const or = op === ts.SyntaxKind.BarBarToken;
    if (and || or) {
      // a && b true ⇒ both true; a || b false ⇒ both false.
      const either = negated ? or : and;
      const l = proves(test.left, key, an, negated);
      const r = proves(test.right, key, an, negated);
      return either ? l || r : l && r;
    }
    const eq =
      op === ts.SyntaxKind.EqualsEqualsEqualsToken || op === ts.SyntaxKind.EqualsEqualsToken;
    const ne =
      op === ts.SyntaxKind.ExclamationEqualsEqualsToken ||
      op === ts.SyntaxKind.ExclamationEqualsToken;
    if (eq || ne) {
      const sides = [unwrap(test.left), unwrap(test.right)];
      for (const [a, b] of [sides, [...sides].reverse()]) {
        if (
          ts.isTypeOfExpression(a) &&
          ts.isStringLiteralLike(b) &&
          covers(a.expression, key, an)
        ) {
          // typeof x === "function" ⇒ present; typeof x !== "undefined" ⇒ present.
          const present = TYPEOF_PRESENT.has(b.text) ? eq : b.text === "undefined" ? ne : false;
          const absent = TYPEOF_PRESENT.has(b.text) ? ne : b.text === "undefined" ? eq : false;
          return negated ? absent : present;
        }
        const nullish =
          b.kind === ts.SyntaxKind.NullKeyword || (ts.isIdentifier(b) && b.text === "undefined");
        if (nullish && covers(a, key, an)) return negated ? eq : ne;
      }
      return false;
    }
    // "m" in cs
    if (op === ts.SyntaxKind.InKeyword && !negated && ts.isStringLiteralLike(unwrap(test.left))) {
      const base = an.resolve(test.right);
      return !!base && pathKey([...base, unwrap(test.left).text]) === key;
    }
    return false;
  }
  if (ts.isIdentifier(test)) {
    const init = an.flag(test);
    if (init && proves(init, key, an, negated)) return true;
  }
  return !negated && covers(test, key, an);
}

// Whether `node` reads member `key` (or a member under it, whose presence implies `key`).
function covers(node, key, an) {
  const path = an.resolve(node);
  if (!path?.length) return false;
  const p = pathKey(path);
  return p === key || p.startsWith(`${key}.`);
}

function exits(stmt) {
  if (!stmt) return false;
  if (ts.isBlock(stmt)) return stmt.statements.length > 0 && exits(stmt.statements.at(-1));
  return (
    ts.isReturnStatement(stmt) ||
    ts.isThrowStatement(stmt) ||
    ts.isContinueStatement(stmt) ||
    ts.isBreakStatement(stmt)
  );
}

function guarded(node, key, an) {
  let child = node;
  for (let at = node.parent; at; child = at, at = at.parent) {
    if (ts.isIfStatement(at)) {
      if (child === at.thenStatement && proves(at.expression, key, an)) return true;
      if (child === at.elseStatement && proves(at.expression, key, an, true)) return true;
    } else if (ts.isConditionalExpression(at)) {
      if (child === at.whenTrue && proves(at.condition, key, an)) return true;
      if (child === at.whenFalse && proves(at.condition, key, an, true)) return true;
    } else if (ts.isBinaryExpression(at) && child === at.right) {
      const op = at.operatorToken.kind;
      if (op === ts.SyntaxKind.AmpersandAmpersandToken && proves(at.left, key, an)) return true;
      if (op === ts.SyntaxKind.BarBarToken && proves(at.left, key, an, true)) return true;
    } else if (
      ts.isBlock(at) ||
      ts.isSourceFile(at) ||
      ts.isCaseClause(at) ||
      ts.isDefaultClause(at)
    ) {
      // An earlier `if (!cs.m) return;` in the same block.
      for (const stmt of at.statements) {
        if (stmt === child) break;
        if (
          ts.isIfStatement(stmt) &&
          !stmt.elseStatement &&
          exits(stmt.thenStatement) &&
          proves(stmt.expression, key, an, true)
        )
          return true;
      }
    }
  }
  return false;
}

const ASSIGN = new Set([
  ts.SyntaxKind.EqualsToken,
  ts.SyntaxKind.BarBarEqualsToken,
  ts.SyntaxKind.AmpersandAmpersandEqualsToken,
  ts.SyntaxKind.QuestionQuestionEqualsToken,
]);
const COMPARE = new Set([
  ts.SyntaxKind.EqualsEqualsEqualsToken,
  ts.SyntaxKind.EqualsEqualsToken,
  ts.SyntaxKind.ExclamationEqualsEqualsToken,
  ts.SyntaxKind.ExclamationEqualsToken,
]);
const FALLBACK = new Set([
  ts.SyntaxKind.AmpersandAmpersandToken,
  ts.SyntaxKind.BarBarToken,
  ts.SyntaxKind.QuestionQuestionToken,
]);

function isWrite(node) {
  const p = node.parent;
  return (
    (ts.isBinaryExpression(p) && p.left === node && ASSIGN.has(p.operatorToken.kind)) ||
    ts.isDeleteExpression(p)
  );
}

// Whether reading `node` uses the member: anything but a write, an optional chain on it, or a test
// of whether it exists (`typeof`, `!`, a condition, a comparison, the left of `&&`/`||`/`??`).
function isUse(node) {
  if (isWrite(node)) return false;
  let n = node;
  while (ts.isParenthesizedExpression(n.parent)) n = n.parent;
  const p = n.parent;
  if (
    (ts.isPropertyAccessExpression(p) ||
      ts.isElementAccessExpression(p) ||
      ts.isCallExpression(p)) &&
    p.expression === n
  )
    return !p.questionDotToken;
  if (ts.isTypeOfExpression(p)) return false;
  if (ts.isPrefixUnaryExpression(p) && p.operator === ts.SyntaxKind.ExclamationToken) return false;
  if ((ts.isIfStatement(p) || ts.isWhileStatement(p) || ts.isDoStatement(p)) && p.expression === n)
    return false;
  if (ts.isForStatement(p) && p.condition === n) return false;
  if (ts.isConditionalExpression(p) && p.condition === n) return false;
  if (ts.isBinaryExpression(p)) {
    const op = p.operatorToken.kind;
    if (COMPARE.has(op)) return false;
    if (FALLBACK.has(op) && p.left === n) return false;
  }
  if (
    ts.isCallExpression(p) &&
    ts.isIdentifier(p.expression) &&
    p.expression.text === "Boolean" &&
    p.arguments[0] === n
  )
    return false;
  return true;
}

// Findings in one JavaScript source: [{ line, member, end }], line 1-based within `text`.
export function scanScript(text, surface) {
  const an = analyze(text);
  const { file, resolve } = an;
  // Members the script puts on window.codesense itself (cs.docs = …) are its own.
  const own = surfaceFrom(an);
  const lines = text.split("\n");
  const findings = [];
  const report = (at, member, end = at.end) => {
    const { line } = file.getLineAndCharacterOfPosition(at.getStart(file));
    findings.push({ line: line + 1, member, end });
  };
  const known = (key) => surface.members.has(key) || own.has(key) || SHARED_MEMBERS.has(key);
  // Whether member `path` is one an older app can lack. A member under one the surface does not
  // break down (cs.invoke.call) or under the script's own is not checked; one under a missing
  // member is reported where that member is read.
  const missing = (path) => {
    const key = pathKey(path);
    if (known(key)) return false;
    if (path.length === 1) return true;
    const parent = pathKey(path.slice(0, -1));
    return surface.members.has(parent) && hasChildren(surface.members, parent);
  };
  const visit = (node) => {
    if (ts.isPropertyAccessExpression(node) || ts.isElementAccessExpression(node)) {
      const base = resolve(node.expression);
      if (base) {
        const path = resolve(node);
        if (!path) {
          // cs[name]: which member is unknowable here.
          const { line } = file.getLineAndCharacterOfPosition(node.getStart(file));
          if (!isWrite(node) && !/\/\/\s*api-skew-ok:\s*\S/.test(lines[line] ?? ""))
            report(node.argumentExpression, "[computed]");
        } else if (missing(path) && isUse(node) && !guarded(node, pathKey(path), an)) {
          report(
            ts.isPropertyAccessExpression(node) ? node.name : node.argumentExpression,
            pathKey(path),
            node.end,
          );
        }
      }
    }
    // const { onTeardown, kit: { virtualList } } = cs
    if (
      ts.isVariableDeclaration(node) &&
      node.initializer &&
      ts.isObjectBindingPattern(node.name)
    ) {
      const pattern = (names, base) => {
        for (const el of names.elements) {
          const key = bindingKey(el);
          if (key === null) continue;
          const path = [...base, key];
          if (missing(path)) {
            if (!el.initializer && !guarded(el, pathKey(path), an)) report(el, pathKey(path));
          } else if (ts.isObjectBindingPattern(el.name)) pattern(el.name, path);
        }
      };
      const base = resolve(node.initializer);
      if (base) pattern(node.name, base);
    }
    ts.forEachChild(node, visit);
  };
  visit(file);
  return findings;
}

const JS_TYPES = /^(?:|module|(?:text|application)\/(?:java|ecma)script|text\/babel)$/i;

// Inline <script> blocks of an HTML file, with the line each starts on.
export function htmlScripts(html) {
  const out = [];
  for (const m of html.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script\s*>/gi)) {
    const attrs = m[1];
    // `(?<![\w-])` keeps data-src and data-type from counting as src and type.
    if (/(?<![\w-])src\s*=/i.test(attrs)) continue;
    const type = (/(?<![\w-])type\s*=\s*["']?([^"'\s>]*)/i.exec(attrs)?.[1] ?? "")
      .split(";")[0]
      .trim()
      .toLowerCase();
    if (!JS_TYPES.test(type)) continue;
    const start = m.index + m[0].indexOf(">") + 1;
    out.push({ text: m[2], line: html.slice(0, start).split("\n").length, offset: start, type });
  }
  return out;
}

function scriptBlocks(path, text) {
  return /\.html?$/i.test(path) ? htmlScripts(text) : [{ text, line: 1, offset: 0 }];
}

export function scanFile(path, surface, text = readFileSync(path, "utf8")) {
  return scriptBlocks(path, text).flatMap((b) =>
    scanScript(b.text, surface).map((f) => ({
      ...f,
      line: f.line + b.line - 1,
      end: f.end + b.offset,
    })),
  );
}

function walk(path, out) {
  if (SKIPPED.test(path)) return;
  const st = statSync(path);
  if (st.isDirectory()) for (const name of readdirSync(path).sort()) walk(join(path, name), out);
  else if (SCANNED.has(extname(path).toLowerCase())) out.push(path);
}

export function checkPaths(paths, surface = loadSurface()) {
  const files = [];
  for (const p of paths) walk(resolve(ROOT, p), files);
  return [...new Set(files)].flatMap((file) =>
    scanFile(file, surface).map((f) => ({ file: relative(ROOT, file), ...f })),
  );
}

const DOCS_PAGE =
  "codesense-docs/getting-started/content/arc-i-dashboard-primitives/calling-codesense.html";
const DOCS_START =
  "<!-- api-skew:surface start (generated by node scripts/check-api-skew.mjs --docs) -->";
const DOCS_END = "<!-- api-skew:surface end -->";

// The docs block listing the supported surface, so authors see exactly what needs no check.
export function surfaceDocs(surface = loadSurface()) {
  const code = (list) => list.map((m) => `<code>${m}</code>`).join(", ");
  const all = [...surface.members].sort();
  const top = all.filter((m) => !m.includes("."));
  const nested = all.filter((m) => m.includes("."));
  return [
    DOCS_START,
    `        <p>`,
    `          The oldest supported app is CodeSense ${surface.oldest}. Every supported app has these`,
    `          members, so they need no check: ${code(top)}.`,
    `        </p>`,
    `        <p>Nested members (the kit) every supported app has: ${code(nested)}.</p>`,
    `        ${DOCS_END}`,
  ].join("\n");
}

export function writeDocs({ check = false } = {}) {
  const path = join(ROOT, DOCS_PAGE);
  const html = readFileSync(path, "utf8");
  const a = html.indexOf(DOCS_START);
  const b = html.indexOf(DOCS_END);
  if (a < 0 || b < a) throw new Error(`${DOCS_PAGE}: missing the api-skew:surface markers`);
  const next = html.slice(0, a) + surfaceDocs() + html.slice(b + DOCS_END.length);
  if (next === html) return true;
  if (check) return false;
  writeFileSync(path, next);
  return true;
}

function main(argv) {
  if (argv[0] === "--record") {
    const [, ref, version] = argv;
    if (!ref || !version) throw new Error("usage: check-api-skew.mjs --record <git-ref> <version>");
    const snapshot = recordSurface(ref, version);
    mkdirSync(join(ROOT, SURFACE_DIR), { recursive: true });
    const out = join(ROOT, SURFACE_DIR, `${version}.json`);
    writeFileSync(out, `${JSON.stringify(snapshot, null, 2)}\n`);
    console.log(
      `recorded ${snapshot.members.length} members of ${version} at ${ref} → ${relative(ROOT, out)}`,
    );
    return 0;
  }
  if (argv[0] === "--docs") {
    const check = argv.includes("--check");
    if (writeDocs({ check })) {
      console.log(`api-skew docs: ${check ? "ok" : "generated"}`);
      return 0;
    }
    console.error(
      `api-skew docs: ${DOCS_PAGE} is stale; run node scripts/check-api-skew.mjs --docs`,
    );
    return 1;
  }
  const surface = loadSurface();
  const findings = checkPaths(argv.length ? argv : DEFAULT_SCAN, surface);
  for (const f of findings)
    console.error(
      f.member === "[computed]"
        ? `${f.file}:${f.line}: window.codesense[…] with a computed name can reach a member ${surface.oldest} lacks; name the member, or add \`// api-skew-ok: <reason>\` on this line`
        : `${f.file}:${f.line}: window.codesense.${f.member} is missing in ${surface.oldest} (oldest supported); feature-check it first (\`typeof cs.${f.member} === "function"\` or \`cs.${f.member}?.(…)\`)`,
    );
  if (findings.length) {
    console.error(
      `check-api-skew: ${findings.length} unguarded use(s) of members newer than ${surface.oldest}`,
    );
    return 1;
  }
  console.log(`check-api-skew: ok (supported: ${surface.versions.join(", ")})`);
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url))
  process.exitCode = main(process.argv.slice(2));
