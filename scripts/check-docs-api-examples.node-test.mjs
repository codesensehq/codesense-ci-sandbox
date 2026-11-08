// SPDX-License-Identifier: MIT
import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  BRIDGE_ARGS,
  argNames,
  checkDescriptions,
  checkFile,
  checkPrimitives,
  corpus,
  extractSnippets,
  loadTruth,
  parseAliases,
  topLevelKeys,
} from "./check-docs-api-examples.mjs";

// A tool's params schema with these fields, each free-form unless given a schema.
const params = (...fields) => ({
  properties: Object.fromEntries(fields.map((f) => (typeof f === "string" ? [f, {}] : f))),
});
// A small truth: page verbs (one tool has two twins), the Surface members, five tools.
const truth = {
  verbs: new Set(["worktrees", "openFile", "postMessage", "sendAsHuman"]),
  members: new Set(["invoke", "act", "queryContext", "worktrees", "openFile", "docs"]),
  twinOf: new Map([
    ["list_worktrees", ["worktrees"]],
    ["send_message", ["postMessage", "sendAsHuman"]],
  ]),
  tools: new Map([
    ["list_worktrees", params("kind", "team")],
    ["reply", params("message_id", "text", "session_id")],
    ["send_message", params("to", "text")],
    ["codesense_call_tool", params("name", ["arguments", { type: ["object", "null"] }])],
    [
      "save_machine",
      params(
        ["input", params("host", ["signIn", params("method", "keyPath")])],
        [
          "share",
          {
            oneOf: [params("kind"), params("kind", "orgSlug", "team")],
          },
        ],
        [
          "targets",
          { anyOf: [{ type: "array", items: params("kind", "path") }, { type: "null" }] },
        ],
      ),
    ],
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

test("descriptions: each tool and parameter description is checked and named by tool and parameter", () => {
  const descriptions = [
    { where: "tool a", text: "Next: answer one with reply {messageId}." },
    { where: "tool b, parameter x.y", text: "Pass reply { message_id }\nthen reply {body}." },
    { where: "tool c", text: "See list_worktrees." },
  ];
  assert.deepEqual(
    checkDescriptions({ ...truth, descriptions }).map((f) => `${f.where}: ${f.rule} ${f.message}`),
    [
      "tool a: R3 reply: `messageId` is an alias; use `message_id`",
      `tool b, parameter x.y: R3 reply: \`body\` is not a parameter ${REPLY}`,
    ],
  );
  // The real truth collects nested parameter descriptions by path.
  const real = loadTruth().descriptions.map((d) => d.where);
  assert.ok(real.includes("tool list_machines"));
  assert.ok(
    real.some((w) => /^tool save_machine, parameter input\.\w+$/.test(w)),
    "nested",
  );
});

test("CLI: a wrong argument in a served description fails the run and names the tool", () => {
  const root = mkdtempSync(join(tmpdir(), "api-lint-"));
  const gen = join(root, "codesense-docs/getting-started/content/generated");
  mkdirSync(gen, { recursive: true });
  mkdirSync(join(root, "src-tauri/src"), { recursive: true });
  writeFileSync(join(gen, "page-sdk.json"), JSON.stringify({ verbs: [], members: [] }));
  const tools = [
    { name: "reply", description: "Reply.", params: params("message_id") },
    { name: "inbox_list", description: "Answer one with reply {messageId}.", params: params() },
  ];
  writeFileSync(join(gen, "mcp-tools.json"), JSON.stringify({ tools }));
  const script = fileURLToPath(new URL("./check-docs-api-examples.mjs", import.meta.url));
  const run = spawnSync(process.execPath, [script, "--root", root], { encoding: "utf8" });
  assert.equal(run.status, 1);
  assert.match(
    run.stderr,
    /generated\/mcp-tools\.json \(tool inbox_list\): R3 reply: `messageId` is not a parameter/,
  );
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

test("R3: nested objects and arrays of objects with a schema; free-form ones pass", () => {
  const code = [
    "save_machine {",
    "  input: { host: 'h', user: 'x', signIn: { method: 'keyFile', file: 'k' } },",
    "  share: { kind: 'team', orgSlug: 'o', team: 't', role: 'r' },",
    "  targets: [{ kind: 'a', path: 'p' }, { kind: 'b', mode: 1 }],",
    "}",
  ].join("\n");
  assert.deepEqual(problems("a.html", html(code)), [
    "3 R3 save_machine: `input.user` is not a parameter (has: host, signIn)",
    "3 R3 save_machine: `input.signIn.file` is not a parameter (has: method, keyPath)",
    "4 R3 save_machine: `share.role` is not a parameter (has: kind, orgSlug, team)",
    "5 R3 save_machine: `targets[].mode` is not a parameter (has: kind, path)",
  ]);
  // An array item behind a comment is still checked.
  assert.deepEqual(
    problems("a.html", html("save_machine { targets: [ /* one */ { mode: 1 } ] }")),
    ["2 R3 save_machine: `targets[].mode` is not a parameter (has: kind, path)"],
  );
  // `arguments` is free-form here; codesense_call_tool checks it against the named tool.
  assert.deepEqual(problems("a.html", html("list_worktrees { team: { any: 1 } }")), []);
  assert.deepEqual(argNames({ type: "object" }), null);
  // No fields and no other properties allowed: it takes none.
  assert.deepEqual(argNames({ type: "object", additionalProperties: false }), new Map());
  // Known fields plus any others: which ones is not known.
  assert.equal(argNames({ properties: { a: {} }, additionalProperties: true }), null);
  assert.equal(argNames({ properties: { a: {} }, additionalProperties: { type: "string" } }), null);
  assert.deepEqual(argNames({ anyOf: [{ type: "string" }, params("a")] }), null);
});

test("R4: a page verb's object argument passes only its listed properties", () => {
  const closed = (...keys) => ({ ...params(...keys), additionalProperties: false });
  const withVerbs = {
    ...truth,
    verbs: new Set([...truth.verbs, "routineResult", "openTab"]),
    members: new Set([...truth.members, "routineResult", "openTab"]),
    verbArgs: new Map([
      ["routineResult", closed("key")],
      ["openTab", closed("workspaceId", ["target", closed("kind")])],
    ]),
  };
  const r4 = (code) =>
    checkFile("a.html", html(code), withVerbs).map((f) => `${f.line} ${f.rule} ${f.message}`);
  const ID = "R4 codesense.routineResult: `id` is not a parameter (has: key)";
  assert.deepEqual(
    r4("codesense.routineResult({ key: 'rt' })\ncodesense.routineResult({ key })"),
    [],
  );
  assert.deepEqual(r4("codesense.routineResult({ id: 'rt' })"), [`2 ${ID}`]);
  assert.deepEqual(r4("cs?.routineResult({ id })\nwindow.codesense.routineResult({ id })"), [
    `2 ${ID}`,
    `3 ${ID}`,
  ]);
  assert.deepEqual(
    r4("codesense.invoke('routineResult', { id })\nkit.call(\"routineResult\", { id })"),
    [`2 ${ID}`, `3 ${ID}`],
  );
  // Nested objects with listed fields; a variable argument or a verb not in verbArgs passes.
  assert.deepEqual(r4("codesense.openTab({ workspaceId, target: { kind, path } })"), [
    "2 R4 codesense.openTab: `target.path` is not a parameter (has: kind)",
  ]);
  assert.deepEqual(r4("codesense.routineResult(args)\ncodesense.worktrees({ any: 1 })"), []);
  // Serde aliases are MCP-side: a page verb never reports one.
  const aliased = { ...withVerbs, aliases: new Map([["id", ["key"]]]) };
  assert.deepEqual(
    checkFile("a.html", html("codesense.routineResult({ id })"), aliased).map((f) => f.message),
    ["codesense.routineResult: `id` is not a parameter (has: key)"],
  );
});

test("R4, real truth: { id } fails on the routine result verbs; free-form, Surface and bridge-read args pass", () => {
  const real = loadTruth();
  const messages = (code) => checkFile("a.html", html(code), real).map((f) => f.message);
  for (const verb of ["routineResult", "routineStatus", "routineRefresh", "routineCancel"]) {
    assert.match(messages(`codesense.${verb}({ id: "rt" })`)[0] ?? "", /`id` is not a parameter/);
    assert.deepEqual(messages(`codesense.${verb}({ key: "rt" })`), []);
  }
  assert.deepEqual(
    messages(
      [
        'codesense.routineRuns({ id: "rt", limit: 5 })',
        'codesense.routine({ id: "rt" })',
        'codesense.routineRefresh({ key: "rt", variables: { a: "b" } })',
        "codesense.inbox({ scope: 'all', limit: 20 })",
        "codesense.showMenu({ anchorRect, items: [{ builtinAction: 'x' }] })",
        "codesense.openFile({ path: 'a.md', org: 'o', team: 't' })",
      ].join("\n"),
    ),
    [],
  );
});

test("R4: a page member takes no more arguments than its signature; rest parameters take any", () => {
  const withArity = {
    ...truth,
    members: new Set([...truth.members, "onRoutineResult", "invoke"]),
    arity: new Map([
      ["onRoutineResult", 1],
      ["invoke", null],
    ]),
  };
  const r4 = (code) => checkFile("a.html", html(code), withArity).map((f) => f.message);
  assert.deepEqual(
    r4("codesense.onRoutineResult(paint)\ncodesense.onRoutineResult(() => f(a, b))"),
    [],
  );
  assert.deepEqual(r4("codesense.onRoutineResult(key, paint)"), [
    "codesense.onRoutineResult: takes 1 argument, not 2",
  ]);
  assert.deepEqual(r4("codesense.invoke('worktrees', {}, 1, 2)\ncodesense.queryContext(1, 2)"), []);
});

test("R4, real truth: listener arity, query_context keys and the bridge-args list", () => {
  const real = loadTruth();
  const messages = (code) => checkFile("a.html", html(code), real).map((f) => f.message);
  assert.deepEqual(messages("codesense.onRoutineResult({ id: 'x' }, cb)"), [
    "codesense.onRoutineResult: takes 1 argument, not 2",
  ]);
  assert.deepEqual(messages("codesense.subscribeContext({ topic: 'routines.runs' }, cb)"), [
    "codesense.subscribeContext: `topic` is not a parameter (has: cursor, filters, groupBy, limit, match, relationships, select, sort, text, where)",
  ]);
  assert.deepEqual(messages("codesense.queryContext({ where: {}, text: 'x' })"), []);
  // A Surface verb: its direct call follows the Surface signature; invoke reaches the verb.
  assert.deepEqual(messages("codesense.showMenu({ anchorRect, label: 'x' })"), []);
  assert.match(
    messages("codesense.invoke('showMenu', { anchorRect, label: 'x' })")[0] ?? "",
    /^codesense\.showMenu: `label` is not a parameter/,
  );
  assert.deepEqual(
    messages("codesense.invoke('showMenu', { anchorRect, nativeSurface: 'x' })"),
    [],
  );
  // Each entry is a verb R4 checks, and each key one its argsSchema does not list.
  const sdk = JSON.parse(
    readFileSync(
      new URL("../codesense-docs/getting-started/content/generated/page-sdk.json", import.meta.url),
      "utf8",
    ),
  );
  for (const [verb, keys] of Object.entries(BRIDGE_ARGS)) {
    assert.ok(real.verbArgs.has(verb), `BRIDGE_ARGS.${verb} is not a checked page verb`);
    const listed = sdk.verbs.find((v) => v.name === verb).argsSchema.properties;
    for (const key of keys)
      assert.ok(!(key in listed), `BRIDGE_ARGS.${verb}.${key} is in its argsSchema now; drop it`);
  }
});

test("primitives: verb example args (R4) and member and event example code are checked", () => {
  const withVerbs = {
    ...truth,
    verbs: new Set([...truth.verbs, "routineResult"]),
    members: new Set([...truth.members, "routineResult"]),
    verbArgs: new Map([
      ["routineResult", { properties: { key: {} }, additionalProperties: false }],
    ]),
  };
  const primitives = {
    invoke: [
      { name: "routineResult", examples: [{ args: { id: "rt" } }, { args: { key: "rt" } }] },
      { name: "worktrees", examples: [{ args: { any: 1 } }] },
    ],
    kit: [{ name: "codesense.x", example: "codesense.routineResult({ id })" }],
    events: [{ name: "e", example: "codesense.openSource()" }],
  };
  assert.deepEqual(
    checkPrimitives(primitives, withVerbs).map((f) => `${f.where}: ${f.rule} ${f.message}`),
    [
      "invoke routineResult: R4 codesense.routineResult: `id` is not a parameter (has: key)",
      "kit codesense.x: R4 codesense.routineResult: `id` is not a parameter (has: key)",
      "events e: R2 `codesense.openSource` is not a page member or verb",
    ],
  );
});

test("cs-try-this: live blocks need a page verb; a dry-run block may name a tool, its args checked", () => {
  const block = (attrs) => `<p>x</p>\n<cs-try-this ${attrs} label="x"></cs-try-this>\n`;
  assert.deepEqual(problems("a.html", block(`verb="worktrees" args='{"any": 1}'`)), []);
  assert.deepEqual(problems("a.html", block(`verb="invoke" dry-run="true"`)), [
    "2 R1 try-this `invoke` is not a page verb",
  ]);
  assert.deepEqual(problems("a.html", block(`verb="reply" args='{"message_id": 1}'`)), [
    '2 R1 try-this `reply` is an MCP tool, not a page verb: mark the block dry-run="true" or use a page verb',
  ]);
  assert.deepEqual(
    problems(
      "a.html",
      block(`verb="save_machine" dry-run="true" args='{"input": {"hots": "&quot;x&quot;"}}'`),
    ),
    ["2 R3 save_machine: `input.hots` is not a parameter (has: host, signIn)"],
  );
  // A `>` inside a quoted value does not end the tag; args that are not an object are reported.
  assert.deepEqual(
    problems("a.html", block(`verb="reply" args='{"text": "a -> b", "body": 1}' dry-run="true"`)),
    [`2 R3 reply: \`body\` is not a parameter ${REPLY}`],
  );
  assert.deepEqual(problems("a.html", block(`verb="reply" dry-run="true" args='[1]'`)), [
    "2 R3 try-this `reply`: args is not a JSON object",
  ]);
  // Attributes read as the browser reads them: any case, unquoted, never from inside a value.
  assert.deepEqual(problems("a.html", block(`VERB="reply" DRY-RUN=true args='{"body": 1}'`)), [
    `2 R3 reply: \`body\` is not a parameter ${REPLY}`,
  ]);
  assert.deepEqual(problems("a.html", block(`verb="reply" title="dry-run='true'"`)), [
    '2 R1 try-this `reply` is an MCP tool, not a page verb: mark the block dry-run="true" or use a page verb',
  ]);
  assert.deepEqual(problems("a.html", block(`verb="reply" dry-run="true" args='{"text": 1} x'`)), [
    "2 R3 try-this `reply`: args is not a JSON object",
  ]);
  assert.deepEqual(problems("a.html", block("")), ["2 R1 try-this has no verb"]);
  assert.deepEqual(problems("a.html", block('verb="" dry-run="true"')), [
    "2 R1 try-this has no verb",
  ]);
  // try-this.js reads only `dry-run`, with the exact value "true", in either quote style.
  const live =
    '2 R1 try-this `reply` is an MCP tool, not a page verb: mark the block dry-run="true" or use a page verb';
  for (const attrs of ['dry-run="TRUE"', 'data-dry-run="true"'])
    assert.deepEqual(problems("a.html", block(`verb="reply" ${attrs}`)), [live], attrs);
  assert.deepEqual(problems("a.html", block(`verb="reply" dry-run='true'`)), []);
  // In a comment or an off element, it is not checked.
  assert.deepEqual(problems("a.html", `<!-- ${block('verb="nope"')} -->`), []);
  assert.deepEqual(problems("a.md", block('verb="nope"')), []);
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
  // Nested arguments and no-argument tools, against the generated schemas.
  const [nested, ...rest] = messages("save_machine { input: { hots: 'x' } }");
  assert.match(nested, /^save_machine: `input\.hots` is not a parameter \(has: /);
  assert.deepEqual(rest, []);
  assert.deepEqual(messages("terminal_server_status { team: 'x' }"), [
    "terminal_server_status: `team` is not a parameter (has: none)",
  ]);
  // schedule_wakeup's params live in src-tauri/src/delivery, and take `sessionId`.
  assert.deepEqual(messages("schedule_wakeup { session_id: 'x' }"), [
    "schedule_wakeup: `session_id` is an alias; use `sessionId`",
  ]);
});

test("corpus: CODESENSE_SITE_ROOT supplies the handbook to a checkout without site/", (t) => {
  const base = mkdtempSync(join(tmpdir(), "docs-api-site-"));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const root = join(base, "checkout");
  mkdirSync(join(root, "codesense-docs/getting-started/content"), { recursive: true });
  writeFileSync(join(root, "codesense-docs/getting-started/content/page.md"), "# Page\n");
  const site = join(base, "site");
  mkdirSync(join(site, "src/content/handbook/guides"), { recursive: true });
  writeFileSync(join(site, "src/content/handbook/guides/x.mdx"), "# X\n");
  const before = process.env.CODESENSE_SITE_ROOT;
  t.after(() =>
    before === undefined
      ? delete process.env.CODESENSE_SITE_ROOT
      : (process.env.CODESENSE_SITE_ROOT = before),
  );
  delete process.env.CODESENSE_SITE_ROOT;
  assert.deepEqual(corpus(root), [join(root, "codesense-docs/getting-started/content/page.md")]);
  process.env.CODESENSE_SITE_ROOT = site;
  assert.deepEqual(corpus(root), [
    join(root, "codesense-docs/getting-started/content/page.md"),
    join(site, "src/content/handbook/guides/x.mdx"),
  ]);
  // An explicit root brings its own site: main passes no env with --root, and so does this.
  assert.deepEqual(corpus(root, {}), [
    join(root, "codesense-docs/getting-started/content/page.md"),
  ]);
  process.env.CODESENSE_SITE_ROOT = "relative/site";
  assert.throws(() => corpus(root), /must be an absolute path/);
});

test("siteInside: by real path, and `..x` is a name, not a parent", async (t) => {
  const { siteInside } = await import("./lib/site-root.mjs");
  const { symlinkSync, realpathSync } = await import("node:fs");
  const base = realpathSync(mkdtempSync(join(tmpdir(), "site-inside-")));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const root = join(base, "checkout");
  mkdirSync(join(root, "site"), { recursive: true });
  mkdirSync(join(root, "..x"), { recursive: true });
  mkdirSync(join(base, "elsewhere"));
  assert.equal(siteInside(root, join(root, "site")), true);
  assert.equal(siteInside(root, join(root, "..x")), true);
  assert.equal(siteInside(root, join(base, "elsewhere")), false);
  symlinkSync(root, join(base, "alias"));
  assert.equal(siteInside(join(base, "alias"), join(root, "site")), true);
});
