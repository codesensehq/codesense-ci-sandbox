// SPDX-License-Identifier: MIT
import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  checkFile,
  corpus,
  extractSnippets,
  loadTruth,
  parseAliases,
  topLevelKeys,
} from "./check-docs-api-examples.mjs";

// A small truth: page verbs (one tool has two twins), the Surface members, three tools.
const truth = {
  verbs: new Set(["worktrees", "openFile", "postMessage", "sendAsHuman"]),
  members: new Set(["invoke", "act", "queryContext", "worktrees", "openFile", "docs"]),
  twinOf: new Map([
    ["list_worktrees", ["worktrees"]],
    ["send_message", ["postMessage", "sendAsHuman"]],
  ]),
  tools: new Map([
    ["list_worktrees", new Set(["kind", "team"])],
    ["reply", new Set(["message_id", "text", "session_id"])],
    ["send_message", new Set(["to", "text"])],
    ["codesense_call_tool", new Set(["name", "arguments"])],
  ]),
  aliases: new Map([["messageId", ["message_id", "messageId"]]]),
};
const problems = (path, text) =>
  checkFile(path, text, truth).map((f) => `${f.line} ${f.rule} ${f.message}`);
const html = (code, attrs = "") => `<p>x</p>\n<pre${attrs}><code>${code}</code></pre>\n`;
const fixture = (name) =>
  readFileSync(new URL(`./fixtures/docs-api-examples/${name}`, import.meta.url), "utf8");
const TWIN = "or call the page verb `codesense.worktrees()`";
const REPLY = "(has: message_id, text, session_id)";

// One fixture per corpus type, each with right and wrong examples.
test("fixture: HTML (<code>, bare and upper-case <pre>, tags, entities, escape hatch; non-example HTML skipped)", () => {
  // Style, script, template and textarea bodies give nothing, nor do off elements written in
  // upper case, unquoted, with spaces around = or closed by a lookalike </div-x>. An off marker
  // inside a comment or a script string hides nothing, even when a matching close tag follows,
  // and a lookalike my-data-api-lint attribute is not the marker. Nested off elements leave the
  // following line numbers intact.
  assert.deepEqual(problems("page.html", fixture("page.html")), [
    `7 R1 \`list_worktrees\` is an MCP tool, not a page verb: write \`list_worktrees { … }\` for the agent side, ${TWIN}`,
    "8 R3 reply: `messageId` is an alias; use `message_id`",
    `8 R3 reply: \`body\` is not a parameter ${REPLY}`,
    "11 R2 `codesense.openSource` is not a page member or verb",
    `15 R3 reply: \`to\` is not a parameter ${REPLY}`,
    `23 R3 reply: \`kind\` is not a parameter ${REPLY}`,
    `29 R3 reply: \`kind\` is not a parameter ${REPLY}`,
    `32 R3 reply: \`kind\` is not a parameter ${REPLY}`,
    `33 R3 reply: \`kind\` is not a parameter ${REPLY}`,
    `35 R3 reply: \`kind\` is not a parameter ${REPLY}`,
  ]);
});

test("fixture: Markdown (fences, inline code, `tool` {…} prose, paragraph-bound backticks and comments, escape hatch)", () => {
  // The stray backtick on line 11 must not pair across the blank line with line 13's,
  // which would turn the prose `reply { body }` into code and hide `reply {text, to}`.
  assert.deepEqual(problems("guide.md", fixture("guide.md")), [
    "3 R3 reply: `messageId` is an alias; use `message_id`",
    `4 R3 reply: \`body\` is not a parameter ${REPLY}`,
    "8 R1 `listOrgsTeams` is not a page verb",
    `13 R3 reply: \`to\` is not a parameter ${REPLY}`,
    `19 R3 reply: \`body\` is not a parameter ${REPLY}`,
    `26 R3 reply: \`kind\` is not a parameter ${REPLY}`,
    `44 R3 reply: \`kind\` is not a parameter ${REPLY}`,
  ]);
});

test("fixture: MDX (a fence closed by a longer fence; an off element around a fence)", () => {
  assert.deepEqual(problems("handbook.mdx", fixture("handbook.mdx")), [
    "8 R1 `listOrgsTeams` is not a page verb",
    "21 R1 `alsoNotAVerb` is not a page verb",
  ]);
});

test("fixture: JS (whole file; window., globalThis., self. and ?. receivers; dynamic names skipped; no escape hatch)", () => {
  assert.deepEqual(problems("shared.js", fixture("shared.js")), [
    "4 R2 `codesense.nope` is not a page member or verb",
    "6 R2 `codesense.alsoNope` is not a page member or verb",
    "7 R2 `codesense.bogus` is not a page member or verb",
    "8 R2 `codesense.bogus` is not a page member or verb",
    `9 R1 \`list_worktrees\` is an MCP tool, not a page verb: write \`list_worktrees { … }\` for the agent side, ${TWIN}`,
    "10 R1 `listOrgsTeams` is not a page verb",
    "12 R2 `codesense.nope3` is not a page member or verb",
  ]);
});

test("fixture: YAML (whole file, .yml and .yaml)", () => {
  for (const name of ["agent.yml", "role.yaml"])
    assert.deepEqual(problems(name, fixture(name)), [
      "3 R3 list_worktrees: `repo` is not a parameter (has: kind, team)",
    ]);
});

test("corpus: docs content (.yaml included, generated/ excluded) and the handbook MDX", () => {
  const files = corpus();
  assert.ok(files.some((p) => p.endsWith(".yaml")));
  // A tree without site/ (the public export) checks only the snapshot in the content tree.
  const site = existsSync(fileURLToPath(new URL("../site", import.meta.url)));
  assert.equal(
    files.some((p) => /site\/src\/content\/handbook\/.+\.mdx$/.test(p)),
    site,
  );
  assert.ok(files.some((p) => /\/content\/handbook\/.+\.md$/.test(p)));
  assert.ok(!files.some((p) => p.includes("/content/generated/")));
});

test("CLI: --root <dir> and --root=<dir> run; a malformed --root is a usage error", () => {
  const script = fileURLToPath(new URL("./check-docs-api-examples.mjs", import.meta.url));
  const root = fileURLToPath(new URL("..", import.meta.url));
  for (const args of [["--root", root], [`--root=${root}`]])
    assert.equal(spawnSync(process.execPath, [script, ...args]).status, 0, args.join(" "));
  for (const args of [["--root"], ["--root="], ["--root", "--x"], ["--nope"]])
    assert.equal(spawnSync(process.execPath, [script, ...args]).status, 2, args.join(" "));
});

test("R1 names every page twin of an MCP tool", () => {
  assert.deepEqual(problems("a.html", html("codesense.invoke('send_message', {})")), [
    "2 R1 `send_message` is an MCP tool, not a page verb: write `send_message { … }` for the agent side, or call the page verb `codesense.postMessage()` or `codesense.sendAsHuman()`",
  ]);
});

test("R2: members and verbs pass; a longer receiver is not the page SDK", () => {
  assert.deepEqual(
    problems("a.html", html("codesense.queryContext({})\ncodesense.docs()\nmy.cs.thing()")),
    [],
  );
});

test("R3: tool({…}), mcp.call and codesense_call_tool arguments", () => {
  assert.deepEqual(problems("a.html", html("reply({ message_id: 1, text })")), []);
  assert.deepEqual(problems("a.html", html("reply({ body })")), [
    `2 R3 reply: \`body\` is not a parameter ${REPLY}`,
  ]);
  assert.deepEqual(problems("a.html", html('mcp.call("reply", { to: "x" })')), [
    `2 R3 reply: \`to\` is not a parameter ${REPLY}`,
  ]);
  const call =
    "codesense_call_tool {\n  name: 'reply',\n  arguments: { message_id: 1, body: 'x' }\n}";
  assert.deepEqual(problems("a.html", html(call)), [
    `4 R3 reply: \`body\` is not a parameter ${REPLY}`,
  ]);
  // `arguments` held in a variable: nothing to check, and no sibling object is mistaken for it.
  const indirect = "codesense_call_tool { name: 'reply', arguments: args, extra: { body: 1 } }";
  assert.deepEqual(problems("a.html", html(indirect)), [
    "2 R3 codesense_call_tool: `extra` is not a parameter (has: name, arguments)",
  ]);
  // A repeated key takes its last value, as in JS.
  const repeated =
    "codesense_call_tool { name: 'list_worktrees', name: 'reply', arguments: { body: 1 } }";
  assert.deepEqual(problems("a.html", html(repeated)), [
    `2 R3 reply: \`body\` is not a parameter ${REPLY}`,
  ]);
  // The tool is named by the top-level `name`, not a nested one.
  const nested =
    "codesense_call_tool { arguments: { name: 'x', kind: 1 }, name: 'list_worktrees' }";
  assert.deepEqual(problems("a.html", html(nested)), [
    "2 R3 list_worktrees: `name` is not a parameter (has: kind, team)",
  ]);
});

test("R3: braces inside strings and comments do not end the object", () => {
  assert.deepEqual(problems("a.html", html("reply({ text: '}', /* } */ body })")), [
    `2 R3 reply: \`body\` is not a parameter ${REPLY}`,
  ]);
});

test("R3: spreads, ellipses and shorthand pass; required fields are not enforced", () => {
  assert.deepEqual(
    problems("a.html", html("list_worktrees { kind: 'story', team, ...rest, … }\nreply {}")),
    [],
  );
});

test("R3: a tool name counts only on the same line as its {", () => {
  assert.deepEqual(problems("a.html", html("// then call reply\n{ anything: 1 }")), []);
});

test('data-api-lint="off" covers its element and nothing past it', () => {
  const wrong = "reply { body }";
  assert.deepEqual(problems("a.html", html(wrong, ' data-api-lint="off"')), []);
  const nested = `<div data-api-lint="off">\n<div><code>${wrong}</code></div>\n</div>\n<code>${wrong}</code>`;
  assert.deepEqual(problems("a.html", nested), [
    `4 R3 reply: \`body\` is not a parameter ${REPLY}`,
  ]);
  // Self-closing or never closed: the rest of the file is still checked.
  for (const open of ['<br data-api-lint="off"/>', '<div data-api-lint="off">'])
    assert.deepEqual(problems("a.html", `${open}\n<code>${wrong}</code>`), [
      `2 R3 reply: \`body\` is not a parameter ${REPLY}`,
    ]);
});

test("entities decode, out-of-range ones stay as written; CRLF keeps line numbers", () => {
  assert.deepEqual(
    extractSnippets("a.html", "<code>a &lt;b&gt; &amp;&#39;c&#x27; &hellip; &#99999999;</code>"),
    [{ text: "a <b> &'c' … &#99999999;", line: 1 }],
  );
  assert.deepEqual(problems("a.md", "x\r\n```\r\nreply { body }\r\n```\r\n"), [
    `3 R3 reply: \`body\` is not a parameter ${REPLY}`,
  ]);
  assert.deepEqual(extractSnippets("a.md", "```\r\nx\r\n```\r\n"), [{ text: "x\n", line: 2 }]);
});

test("topLevelKeys reads keys, shorthand and quoted keys; skips spreads, nesting and comments", () => {
  const keys = topLevelKeys(
    "{ a: 1, 'b': { x: 1 }, c, ...d, …, /* e: 1, g */ f: [g, h], // i: 2\n j }",
  );
  assert.deepEqual(
    keys.map((k) => k.key),
    ["a", "b", "c", "f", "j"],
  );
});

test("parseAliases maps a struct field's alias to its serde name (rename or rename_all)", () => {
  const src = [
    "pub struct P {",
    '    #[serde(default, alias = "storyId")]',
    "    pub story_id: Option<String>,",
    "    /// doc",
    '    #[serde(\n        default,\n        rename = "createWorktree",\n        alias = "create_worktree"\n    )]',
    "    pub create_worktree: Option<bool>,",
    "}",
    '#[serde(rename_all = "camelCase")]',
    "pub struct Q {",
    '    #[serde(alias = "machine")]',
    "    pub machine_id: String,",
    "}",
  ].join("\n");
  const aliases = parseAliases([src]);
  assert.deepEqual(aliases.get("storyId"), ["story_id"]);
  assert.deepEqual(aliases.get("machine"), ["machineId"]);
  // A file the Rust reader cannot parse is named in the error.
  assert.throws(
    () => parseAliases(['pub struct Bad { #[serde(alias = "x")] ??? }'], ["bad.rs"]),
    /^Error: bad\.rs: /,
  );
  assert.deepEqual(aliases.get("create_worktree"), ["createWorktree"]);
});

test("the real truth: verbs, twins, and aliases from inside and outside src-tauri/src/mcp", () => {
  const real = loadTruth();
  assert.deepEqual(real.twinOf.get("open_workspace"), ["openWorkspace"]);
  const messages = (code) => checkFile("a.html", html(code), real).map((f) => f.message);
  assert.deepEqual(messages("open_workspace { storyId: 'DEMO-1' }"), [
    "open_workspace: `storyId` is an alias; use `story_id`",
  ]);
  // schedule_wakeup's params live in src-tauri/src/delivery, and take `sessionId`.
  assert.deepEqual(messages("schedule_wakeup { session_id: 'x' }"), [
    "schedule_wakeup: `session_id` is an alias; use `sessionId`",
  ]);
});
