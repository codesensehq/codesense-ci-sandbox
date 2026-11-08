// SPDX-License-Identifier: MIT
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  DEFAULT_SCAN,
  checkPaths,
  htmlScripts,
  loadSurface,
  recordSurface,
  scanFile,
  scanScript,
  surfaceOf,
  writeDocs,
  SHARED_MEMBERS,
} from "./check-api-skew.mjs";

const surface = loadSurface();
const members = (text) => scanScript(text, surface).map((f) => f.member);

test("the 0.0.25 snapshot records its source and lacks onTeardown", () => {
  const snapshot = JSON.parse(
    readFileSync(new URL("./api-surface/0.0.25.json", import.meta.url), "utf8"),
  );
  // the recording commit is private history, so the published snapshot names none
  assert.equal(snapshot.source.commit, "v0.0.25 bump (private)");
  assert.doesNotMatch(JSON.stringify(snapshot.source), /\b[0-9a-f]{40}\b/);
  assert.ok(snapshot.source.command.includes("--record <commit> 0.0.25"));
  assert.equal(surface.oldest, "0.0.25");
  for (const m of ["invoke", "inbox", "onInboxChanged", "onVisibility", "kit", "kit.icon"])
    assert.ok(surface.members.has(m), m);
  for (const m of ["onTeardown", "ready", "subscribe", "kit.virtualList"])
    assert.ok(!surface.members.has(m), m);
});

test("a recorded surface names no commit SHA", () => {
  const snapshot = recordSurface("HEAD", "9.9.9");
  assert.equal(snapshot.source.commit, "v9.9.9 bump (private)");
  assert.equal(snapshot.source.command, "node scripts/check-api-skew.mjs --record <commit> 9.9.9");
  assert.ok(snapshot.members.length > 0);
  assert.doesNotMatch(JSON.stringify(snapshot), /\b[0-9a-f]{40}\b/);
});

test("an unguarded cs.onTeardown fails with file and line", () => {
  const html = [
    "<!doctype html>",
    "<script>",
    "  const cs = window.codesense;",
    "  cs.onTeardown(() => stop());",
    "</script>",
  ].join("\n");
  assert.deepEqual(
    scanFile("fixture.html", surface, html).map(({ line, member }) => ({ line, member })),
    [{ line: 4, member: "onTeardown" }],
  );
  assert.deepEqual(members("window.codesense.onTeardown(stop);"), ["onTeardown"]);
  assert.deepEqual(members("codesense.onTeardown(stop);"), ["onTeardown"]);
  assert.deepEqual(members("const cs = window.codesense; cs.kit.virtualList(el);"), [
    "kit.virtualList",
  ]);
  assert.deepEqual(members("const cs = window.codesense; cs.records.list({});"), ["records"]);
  assert.deepEqual(members("const cs = window.codesense; void cs\n  .showMenu({});"), ["showMenu"]);
});

test("guarded forms pass", () => {
  const guarded = [
    "cs.onTeardown?.(stop);",
    'if (typeof cs.onTeardown === "function") cs.onTeardown(stop);',
    'typeof cs.onTeardown === "function" && cs.onTeardown(stop);',
    "if (cs.onTeardown) cs.onTeardown(stop);",
    "if (cs.onTeardown) { cs.onTeardown(stop); }",
    "cs.onTeardown && cs.onTeardown(stop);",
    "const off = cs.onTeardown ? cs.onTeardown(stop) : null;",
    '"onTeardown" in cs && cs.onTeardown(stop);',
    "if (cs.onTeardown != null) cs.onTeardown(stop);",
    'if (typeof cs.onTeardown !== "function") return; else cs.onTeardown(stop);',
    "function f() { if (!cs.onTeardown) return; cs.onTeardown(stop); }",
    'function f() { if (typeof cs.onTeardown !== "function") throw new Error("old"); cs.onTeardown(stop); }',
    'const canTeardown = typeof cs.onTeardown === "function"; if (canTeardown) cs.onTeardown(stop);',
    "cs.records?.list({});",
    "cs.kit.virtualList?.(el);",
    "const { onTeardown = () => {} } = cs; onTeardown(stop);",
    "const t = cs.onTeardown || noop; t(stop);",
    "if (cs.kit.virtualList) cs.kit.virtualList(el);",
    "cs.onTeardown = () => {};",
    "cs.inbox({}); cs.kit.icon('x'); cs.invoke('x');",
  ];
  for (const body of guarded)
    assert.deepEqual(members(`const cs = window.codesense;\n${body}`), [], body);
});

test("guards for another member, or after the call, do not count", () => {
  for (const body of [
    "if (cs.onFlush) cs.onTeardown(stop);",
    "if (!cs.onTeardown) {} cs.onTeardown(stop);",
    "cs.onTeardown(stop); if (!cs.onTeardown) return;",
    "cs.onTeardown || cs.onTeardown(stop);",
  ])
    assert.deepEqual(members(`const cs = window.codesense;\n${body}`), ["onTeardown"], body);
});

test("members a script defines itself are its own", () => {
  const text =
    "var cs = (window.codesense = window.codesense || {}); cs.docs = cs.docs || {}; cs.docs.open = open; cs.docs.open();";
  assert.deepEqual(scanScript(text, surface), []);
});

test("recording reads object literals, copied maps and assignments", () => {
  const found = surfaceOf(
    [
      "window.codesense={version:'2',get firstPaint(){},invoke:function(){},commands:{run:function(){}}};",
      "(function(){var c=window.codesense;var m={routines:function(){}};for(var k in m)c[k]=m[k]})();",
      "(function(){var c=window.codesense;['agents'].forEach(function(n){c[n]=function(){}})})();",
      "var cs=(window.codesense=window.codesense||{});cs.kit=Object.assign({h:h},{icon:icon});",
    ].join("\n"),
  );
  assert.deepEqual([...found].sort(), [
    "agents",
    "commands",
    "commands.run",
    "firstPaint",
    "invoke",
    "kit",
    "kit.h",
    "kit.icon",
    "routines",
    "version",
  ]);
});

test("names bound to the kit are followed", () => {
  for (const text of [
    "const cs = window.codesense, kit = cs.kit; kit.virtualList(el);",
    "const cs = window.codesense; let kit; kit = cs.kit; kit.virtualList(el);",
    "const { kit } = window.codesense; kit.virtualList(el);",
    "const k = window.codesense.kit; k.virtualList(el);",
    "const { kit: { virtualList } } = window.codesense; virtualList(el);",
  ])
    assert.deepEqual(members(text), ["kit.virtualList"], text);
  for (const text of [
    "const { kit } = window.codesense; kit.virtualList?.(el);",
    'const k = window.codesense.kit; if (typeof k.virtualList === "function") k.virtualList(el);',
    "const { kit } = window.codesense; kit.icon('x');",
  ])
    assert.deepEqual(members(text), [], text);
});

test("value reads, string keys and destructuring are uses", () => {
  for (const body of [
    "const fn = cs.onTeardown;",
    "[stop].forEach(cs.onTeardown);",
    'cs["onTeardown"](stop);',
    "const { onTeardown } = cs;",
    "const { onTeardown: td } = cs; td(stop);",
  ])
    assert.deepEqual(members(`const cs = window.codesense;\n${body}`), ["onTeardown"], body);
});

test("a computed member fails unless its line says why it is safe", () => {
  assert.deepEqual(members("const cs = window.codesense; cs[name]();"), ["[computed]"]);
  assert.deepEqual(
    members("const cs = window.codesense; cs[name](); // api-skew-ok: name is inbox or agents"),
    [],
  );
  assert.deepEqual(members("const cs = window.codesense; cs[name] = fn;"), []);
});

test("names and flags resolve by scope", () => {
  const text = [
    "function a() { const cs = window.codesense; cs.onTeardown(stop); }",
    "function b() { const cs = getComputedStyle(el); cs.onTeardown(); }",
    'function c() { const ok = typeof window.codesense.onFlush === "function"; }',
    "function d() { const ok = true; if (ok) window.codesense.onFlush(save); }",
    "function e() { let ok = !!window.codesense.ready; if (ok) window.codesense.ready(); }",
  ].join("\n");
  assert.deepEqual(
    scanScript(text, surface).map(({ line, member }) => [line, member]),
    [
      [1, "onTeardown"],
      [4, "onFlush"],
      [5, "ready"],
    ],
  );
});

test("parameters follow their default, else the names cs, codesense and kit", () => {
  for (const text of [
    "function f(cs = window.codesense) { cs.onTeardown(stop); }",
    "function f({ cs = window.codesense, doc = document } = {}) { cs.onTeardown(stop); }",
    "function f({ cs }) { cs.onTeardown(stop); }",
    "const f = (cs) => cs.onTeardown(stop);",
    "function f(codesense) { codesense.onTeardown(stop); }",
  ])
    assert.deepEqual(members(text), ["onTeardown"], text);
  assert.deepEqual(members("function f({ kit }) { kit.virtualList(el); }"), ["kit.virtualList"]);
  assert.deepEqual(members("function f(kit) { kit.h('div'); }"), []);
  assert.deepEqual(members("function f({ cs }) { cs.onTeardown?.(stop); }"), []);
  assert.deepEqual(members("function f(api) { api.onTeardown(stop); }"), []);
});

test("a member another script defines is not this script's own, unless listed as shared", () => {
  assert.deepEqual(members("const cs = window.codesense; cs.helpers.open();"), ["helpers"]);
  assert.deepEqual(members("const cs = window.codesense; cs.docs.mountPersistentCta(el);"), []);
  assert.ok(SHARED_MEMBERS.get("docs").length > 20);
});

test("the story-trail dashboard is scanned and passes", () => {
  assert.ok(DEFAULT_SCAN.includes("src-tauri/assets/dashboards"));
  const findings = checkPaths(["src-tauri/assets/dashboards/story-trail"], surface);
  assert.deepEqual(findings, []);
  const files = new Set(
    checkPaths(["src-tauri/assets/dashboards/story-trail"], {
      ...surface,
      members: new Set(["kit", "kit.h"]),
    }).map((f) => f.file.split("/").pop()),
  );
  for (const file of ["index.html", "page.mjs", "conversation-view.mjs"])
    assert.ok(files.has(file), file);
});

test("the docs list the supported surface", () => {
  assert.equal(writeDocs({ check: true }), true);
});

test("the current tree passes", () => {
  assert.deepEqual(checkPaths(DEFAULT_SCAN, surface), []);
});

test("inline scripts: data-src and data-type are not src and type; types are normalised", () => {
  const html = [
    '<script data-src="x.js">a()</script>',
    '<script src="x.js"></script>',
    '<script type="text/javascript; charset=utf-8">b()</script>',
    '<script type="text/ecmascript" data-type="module">c()</script>',
    '<script type="application/json">{}</script>',
  ].join("\n");
  assert.deepEqual(
    htmlScripts(html).map(({ text, type }) => ({ text, type })),
    [
      { text: "a()", type: "" },
      { text: "b()", type: "text/javascript" },
      { text: "c()", type: "text/ecmascript" },
    ],
  );
});
