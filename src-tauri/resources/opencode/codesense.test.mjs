// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer } from "node:http";
import { afterEach, test } from "node:test";

import { pathToFileURL } from "node:url";

// The waiter protocol (its events, which of them replace a waiter, the line it prints to
// renew itself) is written into the plugin at install from the adapter that declares it.
// The tests install the protocol the way the desktop does.
const SLOT = "/* @waiter */ null";
const PROTOCOL = {
  events: ["SessionStart", "Stop", "StopFailure", "PostToolUse"],
  replace: ["SessionStart", "Stop", "StopFailure"],
  renewal: "Re-arming inbox waiter; no action needed.",
  confirmation: "delivered",
};
const source = readFileSync(new URL("./codesense.js", import.meta.url), "utf8");
assert.ok(source.includes(SLOT), "the plugin has the waiter slot the desktop fills");
const installed = join(mkdtempSync(join(tmpdir(), "codesense-opencode-installed-")), "codesense.js");
writeFileSync(installed, source.replace(SLOT, JSON.stringify(PROTOCOL)));
const { default: installedPlugin } = await import(pathToFileURL(installed).href);
const { default: uninstalledPlugin } = await import("./codesense.js");
const cleanups = [];
// Exercise the native domain registration and AsyncIterable event stream.
async function nativePlugin(definition, { client = {} } = {}) {
  const hooks = {};
  const queue = [];
  let wake;
  const register = (domain) => (name, handler) => { hooks[`${domain}.${name}`] = handler; };
  const cleanup = await definition.setup({
    ...client,
    session: { ...client.session, hook: register("session") },
    tool: { hook: register("tool") }, shell: { hook: register("shell") },
    event: { subscribe: ({ signal }) => ({ async *[Symbol.asyncIterator]() {
      while (!signal.aborted) {
        if (!queue.length) await new Promise((resolve) => {
          wake = resolve;
          signal.addEventListener("abort", resolve, { once: true });
        });
        while (queue.length) yield queue.shift();
      }
    } }) },
  });
  if (cleanup) cleanups.push(cleanup);
  return { ...hooks, event: async (events) => {
    queue.push(...(Array.isArray(events) ? events : [events])); wake?.();
    await new Promise((resolve) => setImmediate(resolve));
  } };
}
const CodeSense = (options) => nativePlugin(installedPlugin, options);
const Uninstalled = (options) => nativePlugin(uninstalledPlugin, options);
afterEach(async () => { while (cleanups.length) await cleanups.pop()(); });

const KEYS = [
  "HOME",
  "XDG_CONFIG_HOME",
  "CODESENSE_ORIGINAL_XDG_CONFIG_HOME",
  "CODESENSE_ACTIVITY_HOST",
  "CODESENSE_USAGE_ENDPOINT",
];
const previous = Object.fromEntries(KEYS.map((key) => [key, process.env[key]]));

afterEach(() => {
  for (const key of KEYS) {
    if (previous[key] === undefined) delete process.env[key];
    else process.env[key] = previous[key];
  }
});

async function shellEnvironment() {
  const plugin = await CodeSense();
  const output = { env: {} };
  await plugin["shell.create.before"](output);
  return output.env;
}

test("restores an explicitly configured XDG config directory", async () => {
  process.env.CODESENSE_ORIGINAL_XDG_CONFIG_HOME = "/tmp/original-xdg";
  assert.equal((await shellEnvironment()).XDG_CONFIG_HOME, "/tmp/original-xdg");
});

test("uses the user's default config directory when original XDG was unset", async () => {
  process.env.HOME = "/tmp/opencode-test-home";
  process.env.CODESENSE_ORIGINAL_XDG_CONFIG_HOME = "";
  process.env.XDG_CONFIG_HOME = "/tmp/managed-xdg";
  assert.equal((await shellEnvironment()).XDG_CONFIG_HOME, "/tmp/opencode-test-home/.config");
});

test("leaves a hand-typed launch's XDG alone", async () => {
  delete process.env.CODESENSE_ORIGINAL_XDG_CONFIG_HOME;
  process.env.XDG_CONFIG_HOME = "/tmp/users-xdg";
  assert.deepEqual(await shellEnvironment(), {});
});

/** A host whose helper appends each stdin it is run with, as one line, to a log. */
function fakeHost(wait = "sleep 3") {
  const dir = mkdtempSync(join(tmpdir(), "codesense-opencode-plugin-"));
  const log = join(dir, "log");
  const helper = join(dir, "activity-helper");
  const waits = join(dir, "waits");
  // `wait` is the shell an `--inbox-wait` run executes after it records its arguments;
  // $n is how many waiters ran so far.
  writeFileSync(
    helper,
    `#!/bin/sh
if [ "$1 $2" = "--inbox-wait opencode" ]; then
  echo "$3 $4" >> "${waits}"
  n=$(wc -l < "${waits}" | tr -d ' ')
  dir="${dir}"
  ${wait}
fi
[ "$1 $2" = "--activity-hook opencode" ] || exit 9
cat >> "${log}"
echo >> "${log}"
`,
  );
  chmodSync(helper, 0o755);
  process.env.CODESENSE_ACTIVITY_HOST = dir;
  const lines = () => {
    try {
      return readFileSync(log, "utf8").trim().split("\n").filter(Boolean).map((line) => JSON.parse(line));
    } catch {
      return [];
    }
  };
  const armed = () => {
    try {
      return readFileSync(waits, "utf8").trim().split("\n").filter(Boolean);
    } catch {
      return [];
    }
  };
  return {
    dir,
    lines,
    armed,
    async armedCount(count) {
      const deadline = Date.now() + 5000;
      while (armed().length < count) {
        assert.ok(Date.now() < deadline, `timed out: ${JSON.stringify(armed())}`);
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
      return armed();
    },
    async settled(count) {
      const deadline = Date.now() + 5000;
      while (lines().length < count) {
        assert.ok(Date.now() < deadline, `timed out: ${JSON.stringify(lines())}`);
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
      return lines();
    },
  };
}

const created = (sessionID, parentID) => raised("session.created", { sessionID, parentID });
const raised = (type, data) => ({ type, data, created: Date.now() });
const names = (lines) => lines.map((line) => [line.hook_event_name, line.session_id, line.agent_id]);

test("a turn reports start, prompt, tool use and stop through the helper in order", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  await plugin.event(created("main"));
  await plugin["session.prompt"]({ sessionID: "main" });
  await plugin["tool.execute.before"]({ tool: "bash", sessionID: "main", id: "c1" });
  await plugin["tool.execute.after"]({ tool: "bash", sessionID: "main", id: "c1" });
  await plugin.event(raised("session.execution.started", { sessionID: "main", status: { type: "idle" } }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  const lines = await host.settled(5);
  assert.deepEqual(names(lines), [
    ["SessionStart", "main", undefined],
    ["UserPromptSubmit", "main", undefined],
    ["PreToolUse", "main", undefined],
    ["PostToolUse", "main", undefined],
    ["Stop", "main", undefined],
  ]);
  assert.deepEqual(lines[2], {
    hook_event_name: "PreToolUse",
    session_id: "main",
    tool_name: "bash",
    tool_use_id: "c1",
  });
});

test("child sessions report as subagents of the main session", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  await plugin.event(created("main"));
  await plugin["session.prompt"]({ sessionID: "main" });
  await plugin.event(created("kid-1", "main"));
  await plugin.event(created("kid-2", "main"));
  await plugin["session.prompt"]({ sessionID: "kid-1" });
  await plugin["tool.execute.before"]({ tool: "read", sessionID: "kid-1", id: "k1" });
  await plugin.event(raised("session.execution.succeeded", { sessionID: "kid-1" }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "kid-2" }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  assert.deepEqual(names(await host.settled(7)), [
    ["SessionStart", "main", undefined],
    ["UserPromptSubmit", "main", undefined],
    ["SubagentStart", "main", "kid-1"],
    ["SubagentStart", "main", "kid-2"],
    ["PreToolUse", "main", "kid-1"],
    ["SubagentStop", "main", "kid-1"],
    ["SubagentStop", "main", "kid-2"],
    ["Stop", "main", undefined],
  ]);
});

test("a permission ask is the main turn's even from a child, and its reply clears only its own call", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  await plugin.event(created("main"));
  await plugin.event(created("kid", "main"));
  await plugin.event(
    raised("permission.asked", { id: "p1", sessionID: "kid", permission: "bash", patterns: ["rm *"], source: { id: "c9" } }),
  );
  await plugin.event(raised("permission.replied", { sessionID: "kid", requestID: "p1", reply: "once" }));
  await plugin.event(
    raised("permission.asked", { id: "p2", sessionID: "main", permission: "edit", patterns: ["a"], source: { id: "c10" } }),
  );
  await plugin.event(raised("permission.replied", { sessionID: "main", requestID: "p2", reply: "reject" }));
  const lines = await host.settled(7);
  assert.deepEqual(
    lines.slice(2).map((line) => [line.hook_event_name, line.agent_id, line.tool_use_id]),
    [
      ["PermissionRequest", undefined, "c9"],
      ["PostToolUse", undefined, "c9"],
      ["PreToolUse", undefined, "c9"],
      ["PermissionRequest", undefined, "c10"],
      ["PostToolUse", undefined, "c10"],
    ],
  );
  assert.ok(lines.every((line) => line.tool_name === undefined || line.hook_event_name.includes("Tool")));
});

test("parallel permission asks are told apart by their call", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  await plugin.event(created("main"));
  for (const [id, call] of [["p1", "c1"], ["p2", "c2"]]) {
    await plugin.event(raised("permission.asked", { id, sessionID: "main", permission: "bash", source: { id: call } }));
  }
  await plugin.event(raised("permission.replied", { sessionID: "main", requestID: "p1", reply: "always" }));
  const lines = await host.settled(5);
  assert.deepEqual(
    lines.slice(1).map((line) => [line.hook_event_name, line.tool_use_id]),
    [["PermissionRequest", "c1"], ["PermissionRequest", "c2"], ["PostToolUse", "c1"], ["PreToolUse", "c1"]],
  );
});

test("a permission ask that names no call stays uncorrelated and any allowed progress answers it", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  await plugin.event(created("main"));
  await plugin.event(raised("permission.asked", { id: "p1", sessionID: "main", permission: "bash" }));
  await plugin.event(raised("permission.replied", { sessionID: "main", requestID: "p1", reply: "once" }));
  assert.deepEqual(names(await host.settled(3)).map(([name]) => name), ["SessionStart", "PermissionRequest", "PreToolUse"]);
});

test("a question is a content-free wait cleared by its answer or its rejection", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  await plugin.event(created("main"));
  await plugin["tool.execute.before"]({ tool: "question", sessionID: "main", id: "q-call" });
  await plugin.event(
    raised("form.created", { form: { id: "q1", sessionID: "main", metadata: { kind: "question", tool: { id: "q-call", messageID: "m" } }, fields: [{ title: "SECRET-QUESTION" }] } }),
  );
  await plugin.event(raised("form.replied", { sessionID: "main", id: "q1", answers: [["SECRET-ANSWER"]] }));
  await plugin.event(
    raised("form.created", { form: { id: "q2", sessionID: "main", metadata: { kind: "question", tool: { id: "q-call-2" } }, fields: [] } }),
  );
  await plugin.event(raised("form.cancelled", { sessionID: "main", id: "q2" }));
  const lines = await host.settled(6);
  assert.deepEqual(
    lines.slice(2).map((line) => [line.hook_event_name, line.tool_use_id]),
    [["Question", "q-call"], ["PostToolUse", "q-call"], ["Question", "q-call-2"], ["PostToolUse", "q-call-2"]],
  );
  assert.ok(!JSON.stringify(lines).includes("SECRET"));
});

test("a resumed Task is counted again when its session goes back to work", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  await plugin.event(created("main"));
  await plugin.event(created("kid", "main"));
  await plugin.event(raised("session.execution.started", { sessionID: "kid", status: { type: "busy" } }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "kid" }));
  await plugin.event(raised("session.execution.started", { sessionID: "kid", status: { type: "busy" } }));
  await plugin.event(raised("session.execution.started", { sessionID: "kid", status: { type: "busy" } }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "kid" }));
  assert.deepEqual(names(await host.settled(5)), [
    ["SessionStart", "main", undefined],
    ["SubagentStart", "main", "kid"],
    ["SubagentStop", "main", "kid"],
    ["SubagentStart", "main", "kid"],
    ["SubagentStop", "main", "kid"],
  ]);
});

test("a saved Task restored after a restart is found to be a child before it is reported", async () => {
  const host = fakeHost();
  const lookups = [];
  const client = {
    session: {
      get: async ({ sessionID }) => {
        lookups.push(sessionID);
        return { id: sessionID, parentID: sessionID === "old-kid" ? "main" : undefined };
      },
    },
  };
  const plugin = await CodeSense({ client });
  await plugin["session.prompt"]({ sessionID: "main" });
  await plugin["tool.execute.before"]({ tool: "task", sessionID: "main", id: "t1" });
  await plugin["session.prompt"]({ sessionID: "old-kid" });
  await plugin.event(raised("session.execution.started", { sessionID: "old-kid", status: { type: "busy" } }));
  await plugin["tool.execute.before"]({ tool: "read", sessionID: "old-kid", id: "k1" });
  await plugin.event(raised("session.execution.succeeded", { sessionID: "old-kid" }));
  assert.deepEqual(names(await host.settled(6)), [
    ["SessionStart", "main", undefined],
    ["UserPromptSubmit", "main", undefined],
    ["PreToolUse", "main", undefined],
    ["SubagentStart", "main", "old-kid"],
    ["PreToolUse", "main", "old-kid"],
    ["SubagentStop", "main", "old-kid"],
  ]);
  assert.deepEqual(lookups.sort(), ["main", "old-kid"]);
});

test("a session whose parent cannot be looked up is not reported until it can be", async () => {
  const host = fakeHost();
  let kidLookups = 0;
  const client = {
    session: {
      get: async ({ sessionID }) => {
        if (sessionID === "kid" && (kidLookups += 1) === 1) throw new Error("lookup failed");
        return { id: sessionID, parentID: sessionID === "kid" ? "main" : undefined };
      },
    },
  };
  const plugin = await CodeSense({ client });
  await plugin["session.prompt"]({ sessionID: "main" });
  // The failed lookup reports nothing, and is tried again at the session's next event.
  await plugin["session.prompt"]({ sessionID: "kid" });
  await plugin["tool.execute.before"]({ tool: "read", sessionID: "kid", id: "k1" });
  await plugin.event(raised("session.execution.started", { sessionID: "kid", status: { type: "busy" } }));
  assert.deepEqual(names(await host.settled(4)), [
    ["SessionStart", "main", undefined],
    ["UserPromptSubmit", "main", undefined],
    ["PreToolUse", "main", "kid"],
    ["SubagentStart", "main", "kid"],
  ]);
  assert.equal(kidLookups, 2);
});

test("a lookup that hangs is given up on and the session is not taken for a main one", async () => {
  const host = fakeHost();
  const client = { session: { get: () => new Promise(() => {}) } };
  const plugin = await CodeSense({ client });
  await plugin["session.prompt"]({ sessionID: "stuck" });
  await new Promise((resolve) => setTimeout(resolve, 2600));
  assert.deepEqual(host.lines(), []);
});

test("a compaction in a session returned to rebinds the tab first", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  for (const id of ["A", "B"]) {
    await plugin.event(created(id));
    await plugin["session.prompt"]({ sessionID: id });
    await plugin.event(raised("session.execution.succeeded", { sessionID: id }));
  }
  await plugin.event(raised("session.execution.started", { sessionID: "A", status: { type: "busy" } }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "A" }));
  assert.deepEqual(names(await host.settled(9)).slice(6), [
    ["SessionStart", "A", undefined],
    ["PreCompact", "A", undefined],
    ["Stop", "A", undefined],
  ]);
});

test("returning to a session announces the Tasks it still runs, and only those", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  await plugin.event(created("A"));
  await plugin["session.prompt"]({ sessionID: "A" });
  await plugin.event(created("running", "A"));
  await plugin.event(created("done", "A"));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "done" }));
  await plugin.event(created("B"));
  await plugin["session.prompt"]({ sessionID: "B" });
  await plugin["session.prompt"]({ sessionID: "A" });
  await plugin.event(raised("session.execution.succeeded", { sessionID: "running" }));
  assert.deepEqual(names(await host.settled(11)).slice(5), [
    ["SessionStart", "B", undefined],
    ["UserPromptSubmit", "B", undefined],
    ["SessionStart", "A", undefined],
    ["SubagentStart", "A", "running"],
    ["UserPromptSubmit", "A", undefined],
    ["SubagentStop", "A", "running"],
  ]);
});

test("returning to an earlier main session starts it again, and a background one does not take the tab", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  await plugin.event(created("A"));
  await plugin["session.prompt"]({ sessionID: "A" });
  await plugin.event(raised("session.execution.succeeded", { sessionID: "A" }));
  await plugin.event(created("B"));
  await plugin["session.prompt"]({ sessionID: "B" });
  await plugin["tool.execute.before"]({ tool: "bash", sessionID: "A", id: "bg" });
  await plugin["session.prompt"]({ sessionID: "A" });
  assert.deepEqual(names(await host.settled(8)), [
    ["SessionStart", "A", undefined],
    ["UserPromptSubmit", "A", undefined],
    ["Stop", "A", undefined],
    ["SessionStart", "B", undefined],
    ["UserPromptSubmit", "B", undefined],
    ["PreToolUse", "A", undefined],
    ["SessionStart", "A", undefined],
    ["UserPromptSubmit", "A", undefined],
  ]);
});

test("work with no prompt reads as working until the session goes idle", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  await plugin.event(created("main"));
  await plugin["session.prompt"]({ sessionID: "main" });
  await plugin.event(raised("session.execution.started", { sessionID: "main", status: { type: "busy" } }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  for (let step = 0; step < 3; step += 1) {
    await plugin.event(raised("session.execution.started", { sessionID: "main", status: { type: "busy" } }));
  }
  await plugin.event(raised("session.compacted", { sessionID: "main" }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  assert.deepEqual(names(await host.settled(5)).map(([name]) => name), [
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "PreCompact",
    "Stop",
  ]);
});

test("errors map to a reason, an abort is an interrupt, a child's error ends only the child", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  await plugin.event(created("main"));
  await plugin.event(created("kid", "main"));
  await plugin.event(raised("session.execution.failed", { sessionID: "kid", error: { type: "APIError", statusCode: 500 } }));
  await plugin.event(raised("session.execution.failed", { sessionID: "main", error: { type: "APIError", statusCode: 429 } }));
  await plugin.event(raised("session.execution.interrupted", { sessionID: "main", reason: "user" }));
  await plugin.event(raised("session.execution.failed", { sessionID: "main", error: { type: "Weird" } }));
  const lines = await host.settled(6);
  assert.deepEqual(names(lines).slice(2), [
    ["SubagentStop", "main", "kid"],
    ["StopFailure", "main", undefined],
    ["Interrupt", "main", undefined],
    ["StopFailure", "main", undefined],
  ]);
  assert.deepEqual(lines.slice(3).map((line) => line.error), ["rate_limit", undefined, "unknown"]);
});

test("an error's idle and a repeated idle report the turn's end once", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  await plugin.event(created("main"));
  await plugin["session.prompt"]({ sessionID: "main" });
  await plugin.event(raised("session.execution.failed", { sessionID: "main", error: { type: "APIError", statusCode: 400 } }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  await plugin["session.prompt"]({ sessionID: "main" });
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  await plugin.event(raised("session.execution.started", { sessionID: "main", status: { type: "busy" } }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  // The final busy with no prompt is work the session started on its own.
  assert.deepEqual((await host.settled(7)).map((line) => line.hook_event_name), [
    "SessionStart",
    "UserPromptSubmit",
    "StopFailure",
    "UserPromptSubmit",
    "Stop",
    "PreCompact",
    "Stop",
  ]);
});

test("a retry, an unknown event or a session the plugin never saw start is handled quietly", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  await plugin.event(raised("session.retry.scheduled", { sessionID: "resumed" }));
  await plugin.event(raised("message.updated", { sessionID: "resumed" }));
  await plugin.event({ event: undefined });
  await plugin["session.prompt"]({ sessionID: "resumed" });
  assert.deepEqual(names(await host.settled(2)), [
    ["SessionStart", "resumed", undefined],
    ["UserPromptSubmit", "resumed", undefined],
  ]);
});

test("a new main session starts again so the tab can rebind", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  await plugin.event(created("one"));
  await plugin.event(created("two"));
  await plugin["session.prompt"]({ sessionID: "two" });
  assert.deepEqual(names(await host.settled(3)), [
    ["SessionStart", "one", undefined],
    ["SessionStart", "two", undefined],
    ["UserPromptSubmit", "two", undefined],
  ]);
});

test("no content reaches the helper", async () => {
  const host = fakeHost();
  const plugin = await CodeSense();
  const secret = "SECRET-PROMPT-TEXT";
  await plugin.event(created("main"));
  await plugin["session.prompt"]({ sessionID: "main", messageID: "m", agent: secret, model: { modelID: secret }, variant: secret });
  await plugin["tool.execute.before"]({ tool: "bash", sessionID: "main", id: "c1", args: { command: secret } });
  await plugin["tool.execute.after"]({ tool: "bash", sessionID: "main", id: "c1", output: secret, metadata: { secret }, title: secret });
  await plugin.event(
    raised("permission.asked", {
      id: "p",
      sessionID: "main",
      permission: "bash",
      patterns: [secret],
      metadata: { command: secret },
      source: { id: "c1", messageID: secret },
    }),
  );
  await plugin.event(raised("permission.replied", { sessionID: "main", requestID: "p", reply: "once" }));
  await plugin.event(
    raised("session.execution.failed", { sessionID: "main", error: { type: "APIError", data: { message: secret, statusCode: 400, responseBody: secret } } }),
  );
  await plugin.event(created("kid", "main"));
  const lines = await host.settled(8);
  assert.ok(!JSON.stringify(lines).includes(secret));
  const allowed = new Set(["hook_event_name", "session_id", "agent_id", "tool_name", "tool_use_id", "error"]);
  for (const line of lines) {
    for (const key of Object.keys(line)) assert.ok(allowed.has(key), `leaked field ${key}`);
  }
});

test("a missing helper or host is a no-op and never throws", async () => {
  const dir = mkdtempSync(join(tmpdir(), "codesense-opencode-plugin-"));
  process.env.CODESENSE_ACTIVITY_HOST = dir;
  const plugin = await CodeSense();
  await plugin.event(created("main"));
  await plugin["session.prompt"]({ sessionID: "main" });
  delete process.env.CODESENSE_ACTIVITY_HOST;
  await plugin["session.prompt"]({ sessionID: "main" });
  await new Promise((resolve) => setTimeout(resolve, 100));
});

/** A local endpoint that keeps every OTLP body posted to it. */
async function fakeEndpoint({ failFirst = 0, status = 503 } = {}) {
  const posts = [];
  let attempts = 0;
  const server = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => (body += chunk));
    request.on("end", () => {
      attempts += 1;
      if (attempts <= failFirst) {
        response.statusCode = status;
        response.end("{}");
        return;
      }
      posts.push({ url: request.url, type: request.headers["content-type"], body: JSON.parse(body) });
      response.end("{}");
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  process.env.CODESENSE_USAGE_ENDPOINT = `http://127.0.0.1:${server.address().port}/api/v1/telemetry/tok`;
  const records = () =>
    posts.flatMap((post) =>
      post.body.resourceLogs.flatMap((r) => r.scopeLogs.flatMap((s) => s.logRecords)),
    );
  const attrs = (record) =>
    Object.fromEntries(
      record.attributes.map((a) => [a.key, a.value.stringValue ?? a.value.intValue ?? a.value.doubleValue]),
    );
  return {
    posts,
    records,
    attrs,
    close: () => server.close(),
    async settled(count) {
      const deadline = Date.now() + 5000;
      while (records().length < count) {
        assert.ok(Date.now() < deadline, `timed out: ${JSON.stringify(posts)}`);
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
      return records();
    },
  };
}

const message = (overrides = {}) => {
  const info = { id: "msg_1", sessionID: "ses_1", role: "assistant", modelID: "deepseek-v4-1-flash",
    providerID: "opencode-go", time: { completed: 1761328801500 },
    tokens: { input: 1200, output: 300, reasoning: 50, cache: { read: 9000, write: 0 } }, cost: 0.0123, ...overrides };
  if (info.role !== "assistant" || info.time?.completed === undefined) return [];
  return [raised("session.step.started", { sessionID: info.sessionID, assistantMessageID: info.id,
    model: { id: info.modelID, providerID: info.providerID } }),
    { ...raised("session.step.ended", { sessionID: info.sessionID, assistantMessageID: info.id, tokens: info.tokens, cost: info.cost }), created: info.time.completed }];
};

test("a finished assistant message posts one usage record, once however often it updates", async () => {
  const endpoint = await fakeEndpoint();
  try {
    const plugin = await CodeSense();
    await plugin.event(message({ time: { created: 1761328800000 } }));
    await plugin.event(message());
    await plugin.event(message());
    const records = await endpoint.settled(1);
    assert.equal(records.length, 1);
    assert.equal(endpoint.posts[0].url, "/api/v1/telemetry/tok/v1/logs");
    assert.deepEqual(endpoint.attrs(records[0]), {
      "event.name": "opencode.usage",
      "event.timestamp": "2025-10-24T18:00:01.500Z",
      "session.id": "ses_1",
      message_id: "msg_1",
      model: "deepseek-v4-1-flash",
      provider: "opencode-go",
      input: "1200",
      output: "300",
      reasoning: "50",
      cache_read: "9000",
      cache_write: "0",
      cost: 0.0123,
    });
  } finally {
    endpoint.close();
  }
});

test("user messages, unfinished messages and a missing endpoint post nothing", async () => {
  const endpoint = await fakeEndpoint();
  try {
    const plugin = await CodeSense();
    await plugin.event(message({ id: "u1", role: "user" }));
    await plugin.event(message({ id: "a1", time: { created: 1 } }));
    delete process.env.CODESENSE_USAGE_ENDPOINT;
    await plugin.event(message({ id: "a2" }));
    await new Promise((resolve) => setTimeout(resolve, 200));
    assert.deepEqual(endpoint.posts, []);
  } finally {
    endpoint.close();
  }
});

test("a subagent's message is reported under its main session", async () => {
  const endpoint = await fakeEndpoint();
  try {
    const plugin = await CodeSense();
    await plugin.event(created("main"));
    await plugin.event(created("kid", "main"));
    await plugin.event(message({ id: "kid_msg", sessionID: "kid" }));
    const records = await endpoint.settled(1);
    assert.equal(endpoint.attrs(records[0])["session.id"], "main");
  } finally {
    endpoint.close();
  }
});

test("a record the receiver rejects is retried until it is accepted, once", async () => {
  const endpoint = await fakeEndpoint({ failFirst: 2 });
  try {
    const plugin = await CodeSense();
    await plugin.event(message({ id: "m1" }));
    await plugin.event(message({ id: "m2" }));
    await plugin.event(message({ id: "m3" }));
    const records = await endpoint.settled(3);
    assert.deepEqual(
      records.map((r) => endpoint.attrs(r).message_id).sort(),
      ["m1", "m2", "m3"],
    );
  } finally {
    endpoint.close();
  }
});

test("a receiver that is briefly busy is retried within the turn", async () => {
  const endpoint = await fakeEndpoint({ failFirst: 1 });
  try {
    const plugin = await CodeSense();
    await plugin.event(message({ id: "m1" }));
    const started = Date.now();
    const records = await endpoint.settled(1);
    assert.equal(records.length, 1);
    assert.ok(Date.now() - started < 450, "the quick retry, not the backed-off timer");
  } finally {
    endpoint.close();
  }
});

test("a longer outage waits for the backed-off retry and then drains in order", async () => {
  const endpoint = await fakeEndpoint({ failFirst: 5 });
  try {
    const plugin = await CodeSense();
    await plugin.event(message({ id: "m1" }));
    await plugin.event(message({ id: "m2" }));
    const records = await endpoint.settled(2);
    assert.deepEqual(records.map((r) => endpoint.attrs(r).message_id).sort(), ["m1", "m2"]);
  } finally {
    endpoint.close();
  }
});

test("a record the receiver refuses for good is not retried", async () => {
  const endpoint = await fakeEndpoint({ failFirst: 1, status: 400 });
  try {
    const plugin = await CodeSense();
    await plugin.event(message({ id: "m1" }));
    await plugin.event(message({ id: "m2" }));
    const records = await endpoint.settled(1);
    await new Promise((resolve) => setTimeout(resolve, 1500));
    assert.deepEqual(endpoint.records().map((r) => endpoint.attrs(r).message_id), ["m2"]);
  } finally {
    endpoint.close();
  }
});

test("a reported cost of zero is sent as zero", async () => {
  const endpoint = await fakeEndpoint();
  try {
    const plugin = await CodeSense();
    await plugin.event(message({ id: "free", modelID: "deepseek-v4.1-flash-free", cost: 0 }));
    const [record] = await endpoint.settled(1);
    assert.equal(endpoint.attrs(record).cost, 0);
  } finally {
    endpoint.close();
  }
});

test("a usage record carries no content", async () => {
  const endpoint = await fakeEndpoint();
  try {
    const plugin = await CodeSense();
    const secret = "SECRET-PROMPT-TEXT";
    await plugin.event(message({ summary: secret, system: [secret], path: { cwd: secret }, error: { data: { message: secret } } }));
    await endpoint.settled(1);
    assert.ok(!JSON.stringify(endpoint.posts).includes(secret));
  } finally {
    endpoint.close();
  }
});

/** A client that records the prompts the plugin submits. */
function fakeClient() {
  const prompts = [];
  return {
    prompts,
    client: {
      session: {
        prompt: async (request) => {
          prompts.push(request);
          return { id: "admitted", delivery: "queue" };
        },
      },
    },
  };
}

const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const mail = (text) => `echo '${text}' >&2; exit 2`;

test("the waiter is armed at boot, at a turn's end, at a failed turn and after a tool call", async () => {
  const host = fakeHost();
  const { client } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("main"));
  await host.armedCount(1);
  await plugin["session.prompt"]({ sessionID: "main" });
  await plugin["tool.execute.after"]({ tool: "bash", sessionID: "main", id: "c1" });
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  await host.armedCount(2);
  await plugin["session.prompt"]({ sessionID: "main" });
  await plugin.event(raised("session.execution.failed", { sessionID: "main", error: { type: "APIError", statusCode: 500 } }));
  const armed = await host.armedCount(3);
  assert.deepEqual(armed.map((line) => line.split(" ")[0]), ["SessionStart", "Stop", "StopFailure"]);
  // Its parent is the process that ran the plugin.
  assert.ok(armed.every((line) => line.endsWith(` ${process.pid}`)));
});

test("a tool call arms only when no waiter is waiting; a turn's end replaces the waiter", async () => {
  const host = fakeHost();
  const { client } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("main"));
  await host.armedCount(1);
  await plugin["tool.execute.after"]({ tool: "bash", sessionID: "main", id: "c1" });
  await plugin["tool.execute.after"]({ tool: "bash", sessionID: "main", id: "c2" });
  await pause(200);
  assert.equal(host.armed().length, 1);
  await plugin["session.prompt"]({ sessionID: "main" });
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  assert.deepEqual((await host.armedCount(2)).map((line) => line.split(" ")[0]), ["SessionStart", "Stop"]);
});

test("a tool call re-arms a session whose waiter ended", async () => {
  const host = fakeHost(`[ "$n" = 1 ] && exit 0; sleep 3`);
  const { client } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("main"));
  await host.armedCount(1);
  await pause(300);
  await plugin["tool.execute.after"]({ tool: "bash", sessionID: "main", id: "c1" });
  assert.deepEqual((await host.armedCount(2)).map((line) => line.split(" ")[0]), ["SessionStart", "PostToolUse"]);
});

test("mail submits its notice to the main session once, through the client", async () => {
  const host = fakeHost(`[ "$n" = 1 ] && ${mail("You have 1 new message.")}; sleep 3`);
  const { client, prompts } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("main"));
  await host.armedCount(1);
  await pause(400);
  assert.deepEqual(prompts, [
    { sessionID: "main", text: "You have 1 new message.", delivery: "queue" },
  ]);
  // Nothing re-arms until the next turn event, and the notice is never sent twice.
  await plugin["tool.execute.after"]({ tool: "bash", sessionID: "main", id: "c1" });
  await host.armedCount(2);
  await pause(300);
  assert.equal(prompts.length, 1);
});

test("the renewal line re-arms the waiter and submits nothing", async () => {
  const host = fakeHost(`[ "$n" -lt 3 ] && ${mail("Re-arming inbox waiter; no action needed.")}; sleep 3`);
  const { client, prompts } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("main"));
  const armed = await host.armedCount(3);
  assert.equal(armed.length, 3);
  assert.deepEqual(prompts, []);
});

test("a waiter that ends with nothing to say, or fails, submits nothing", async () => {
  const host = fakeHost(`[ "$n" = 1 ] && exit 0; [ "$n" = 2 ] && exit 1; [ "$n" = 3 ] && exit 2; sleep 3`);
  const { client, prompts } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("main"));
  await host.armedCount(1);
  for (const id of ["a", "b"]) {
    await pause(200);
    await plugin["session.prompt"]({ sessionID: "main" });
    await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  }
  await host.armedCount(3);
  await pause(300);
  assert.deepEqual(prompts, []);
});

test("a child session never has a waiter", async () => {
  const host = fakeHost();
  const { client } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("main"));
  await host.armedCount(1);
  await plugin.event(created("kid", "main"));
  await plugin["tool.execute.after"]({ tool: "read", sessionID: "kid", id: "k1" });
  await plugin.event(raised("session.execution.succeeded", { sessionID: "kid" }));
  await plugin.event(raised("session.execution.failed", { sessionID: "kid", error: { type: "APIError", statusCode: 500 } }));
  await pause(300);
  assert.equal(host.armed().length, 1);
});

test("without a host binding or a client no waiter runs", async () => {
  const host = fakeHost();
  const { client } = fakeClient();
  const bare = await CodeSense({});
  await bare.event(created("one"));
  delete process.env.CODESENSE_ACTIVITY_HOST;
  const unbound = await CodeSense({ client });
  await unbound.event(created("two"));
  await pause(300);
  assert.deepEqual(host.armed(), []);
});

test("cancelling a mail-woken turn re-arms the waiter, though the idle after the abort is suppressed", async () => {
  const host = fakeHost(`[ "$n" = 1 ] && ${mail("You have 1 new message.")}; sleep 3`);
  const { client, prompts } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("main"));
  await host.armedCount(1);
  await pause(400);
  assert.equal(prompts.length, 1);
  await plugin["session.prompt"]({ sessionID: "main" });
  await plugin.event(raised("session.execution.interrupted", { sessionID: "main", reason: "user" }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  assert.deepEqual((await host.armedCount(2)).map((line) => line.split(" ")[0]), ["SessionStart", "Stop"]);
  await pause(300);
  assert.equal(host.armed().length, 2, "the suppressed idle does not arm a second one");
});

test("a failed turn's own idle leaves the one waiter its failure armed", async () => {
  const host = fakeHost();
  const { client } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("main"));
  await host.armedCount(1);
  await plugin["session.prompt"]({ sessionID: "main" });
  await plugin.event(raised("session.execution.failed", { sessionID: "main", error: { type: "APIError", statusCode: 500 } }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  await pause(400);
  assert.deepEqual(host.armed().map((line) => line.split(" ")[0]), ["SessionStart", "StopFailure"]);
});


test("a turn's end replacing a waiter that already printed its notice still delivers it, once", async () => {
  const host = fakeHost(
    `[ "$n" = 1 ] && { echo 'You have 1 new message.' >&2; sleep 0.6; exit 2; }; sleep 3`,
  );
  const { client, prompts } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("main"));
  await host.armedCount(1);
  await pause(200);
  // The helper has printed and not yet exited when the turn ends.
  await plugin["session.prompt"]({ sessionID: "main" });
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  await host.armedCount(2);
  await pause(900);
  assert.deepEqual(prompts, [
    { sessionID: "main", text: "You have 1 new message.", delivery: "queue" },
  ]);
  // The replacement waits on; the replaced one does not re-arm another.
  await pause(300);
  assert.equal(host.armed().length, 2);
});

test("a replaced waiter that printed nothing is killed and delivers nothing", async () => {
  const host = fakeHost(`sleep 3; echo 'late' >&2; exit 2`);
  const { client, prompts } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("main"));
  await host.armedCount(1);
  await plugin["session.prompt"]({ sessionID: "main" });
  await plugin.event(raised("session.execution.succeeded", { sessionID: "main" }));
  await host.armedCount(2);
  await pause(500);
  assert.deepEqual(prompts, []);
});


async function pluginRunAs(argv, client) {
  const saved = process.argv;
  process.argv = argv;
  try {
    return await CodeSense({ client });
  } finally {
    process.argv = saved;
  }
}

test("a headless OpenCode run never spawns a waiter, an interactive one does", async () => {
  const host = fakeHost();
  const { client } = fakeClient();
  for (const argv of [
    ["bun", "/$bunfs/root/src/index.js", "run", "hi"],
    ["bun", "/$bunfs/root/src/index.js", "--print-logs", "run", "hi"],
    ["bun", "/$bunfs/root/src/index.js", "serve"],
  ]) {
    const plugin = await pluginRunAs(argv, client);
    await plugin.event(created("headless"));
    await plugin["session.prompt"]({ sessionID: "headless" });
    await plugin["tool.execute.after"]({ tool: "bash", sessionID: "headless", id: "c1" });
    await plugin.event(raised("session.execution.succeeded", { sessionID: "headless" }));
    // One run's hooks drain before the next starts, so their log lines do not interleave.
    await pause(400);
  }
  assert.deepEqual(host.armed(), []);
  // The activity hooks still report.
  const reported = readFileSync(join(host.dir, "log"), "utf8").match(/hook_event_name/g) ?? [];
  assert.ok(reported.length >= 9, `${reported.length} hooks`);
  const tui = await pluginRunAs(["bun", "/$bunfs/root/src/cli/tui/worker.js"], client);
  await tui.event(created("tab"));
  assert.equal((await host.armedCount(1)).length, 1);
});

/** A `ps` that reports the OpenCode being run as `command`, first on PATH. */
function fakePs(command) {
  const dir = mkdtempSync(join(tmpdir(), "codesense-opencode-ps-"));
  writeFileSync(join(dir, "ps"), `#!/bin/sh\necho '${command}'\n`);
  chmodSync(join(dir, "ps"), 0o755);
  process.env.PATH = `${dir}:${previousPath}`;
}
const previousPath = process.env.PATH;
afterEach(() => {
  process.env.PATH = previousPath;
});

const listing = {
  session: {
    prompt: async () => {},
    list: async () => ({
      data: [
        { id: "older", time: { updated: 1 } },
        { id: "latest", time: { updated: 5 } },
        { id: "child", parentID: "older", time: { updated: 9 } },
      ],
    }),
  },
};

test("a resumed idle session is armed at boot, though no event names it", async () => {
  const host = fakeHost();
  fakePs("/Users/me/.opencode/bin/opencode --continue");
  await CodeSense({ client: listing });
  const armed = await host.armedCount(1);
  assert.equal(armed[0].split(" ")[0], "SessionStart");
  assert.deepEqual(names(await host.settled(1)), [["SessionStart", "latest", undefined]]);
});

test("a named session is armed at boot, a fresh launch is not", async () => {
  const host = fakeHost();
  fakePs("opencode -s ses_named");
  await CodeSense({ client: listing });
  assert.deepEqual(names(await host.settled(1)), [["SessionStart", "ses_named", undefined]]);
  assert.equal(host.armed().length, 1);
  const fresh = fakeHost();
  fakePs("opencode");
  await CodeSense({ client: listing });
  await pause(1600);
  assert.deepEqual(fresh.armed(), []);
});

test("a finished turn of a session the tab left does not take the waiter, and its mail goes to the one the tab follows", async () => {
  const host = fakeHost(`[ "$n" = 2 ] && { sleep 0.7; ${mail("You have 1 new message.")}; }; sleep 3`);
  const { client, prompts } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("A"));
  await host.armedCount(1);
  await plugin["session.prompt"]({ sessionID: "A" });
  await plugin.event(created("B"));
  await plugin["session.prompt"]({ sessionID: "B" });
  await host.armedCount(2);
  // A, in the background, finishes: it is not the session the tab follows.
  await plugin.event(raised("session.execution.succeeded", { sessionID: "A" }));
  await plugin["tool.execute.after"]({ tool: "bash", sessionID: "A", id: "c1" });
  await pause(1200);
  assert.deepEqual(host.armed().map((line) => line.split(" ")[0]), ["SessionStart", "SessionStart"]);
  assert.deepEqual(prompts, [
    { sessionID: "B", text: "You have 1 new message.", delivery: "queue" },
  ]);
});

test("moving the tab back to a session arms it again", async () => {
  const host = fakeHost();
  const { client } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("A"));
  await plugin.event(created("B"));
  await plugin["session.prompt"]({ sessionID: "A" });
  await host.armedCount(3);
  assert.deepEqual(host.armed().map((line) => line.split(" ")[0]), ["SessionStart", "SessionStart", "SessionStart"]);
});

test("the headless command is found past global options and their values", async () => {
  const host = fakeHost();
  const { client } = fakeClient();
  const index = ["bun", "/$bunfs/root/src/index.js"];
  for (const args of [
    ["--log-level", "ERROR", "run", "hi"],
    ["--print-logs", "run", "hi"],
    ["--port", "0", "serve"],
    ["-m", "fake/fake-model", "run", "x"],
    ["--log-level=ERROR", "web"],
  ]) {
    const plugin = await pluginRunAs([...index, ...args], client);
    await plugin.event(created("headless"));
  }
  await pause(300);
  assert.deepEqual(host.armed(), []);
  // An interactive launch whose option value or prompt reads like a command still arms.
  for (const args of [["--prompt", "run this"], ["--prompt", "run"], ["--log-level", "ERROR"], ["myproject"]]) {
    const plugin = await pluginRunAs([...index, ...args], client);
    await plugin.event(created(`tui-${args.join("-")}`));
  }
  assert.equal((await host.armedCount(4)).length, 4);
});

const deleted = (sessionID) => raised("session.deleted", { sessionID, info: { id: sessionID } });

/** A waiter that prints a notice, then says whether it was confirmed to it, in `dir`. */
const printsThenListens = `echo 'You have 1 new message.' >&2; if read -t 3 c; then echo "$c" > "$dir/confirmed"; else echo eof > "$dir/unconfirmed"; fi; exit 2`;
const exists = (host, name) => {
  try {
    return readFileSync(join(host.dir, name), "utf8").trim();
  } catch {
    return undefined;
  }
};

test("a delivered notice is confirmed to its waiter", async () => {
  const host = fakeHost(`[ "$n" = 1 ] && { ${printsThenListens}; }; sleep 3`);
  const { client, prompts } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("main"));
  await host.armedCount(1);
  await pause(500);
  assert.equal(prompts.length, 1);
  assert.equal(exists(host, "confirmed"), "delivered");
  assert.equal(exists(host, "unconfirmed"), undefined);
});

test("a notice with no conversation to receive it is not confirmed", async () => {
  // This waiter ignores the signal that retires it, and prints after the conversation went.
  const host = fakeHost(`[ "$n" = 1 ] && { trap "" TERM; sleep 0.5; ${printsThenListens}; }; sleep 3`);
  const { client, prompts } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("B"));
  await host.armedCount(1);
  await plugin.event(deleted("B"));
  await pause(1500);
  assert.deepEqual(prompts, [], "never submitted to the deleted conversation");
  assert.equal(exists(host, "confirmed"), undefined);
  assert.equal(exists(host, "unconfirmed"), "eof");
});

test("a notice OpenCode refuses is not confirmed", async () => {
  const host = fakeHost(`[ "$n" = 1 ] && { ${printsThenListens}; }; sleep 3`);
  const refusing = { session: { prompt: async () => ({ error: { type: "NotFoundError" } }) } };
  const plugin = await CodeSense({ client: refusing });
  await plugin.event(created("main"));
  await host.armedCount(1);
  await pause(1500);
  assert.equal(exists(host, "confirmed"), undefined);
  assert.equal(exists(host, "unconfirmed"), "eof");
  const thrown = { session: { prompt: async () => Promise.reject(new Error("gone")) } };
  const other = fakeHost(`[ "$n" = 1 ] && { ${printsThenListens}; }; sleep 3`);
  const second = await CodeSense({ client: thrown });
  await second.event(created("main"));
  await other.armedCount(1);
  await pause(1500);
  assert.equal(exists(other, "confirmed"), undefined);
  assert.equal(exists(other, "unconfirmed"), "eof");
});

test("deleting the followed conversation retires its quiet waiter, so mail is left unread", async () => {
  const host = fakeHost(`sleep 0.8; echo 'You have 1 new message.' >&2; exit 2`);
  const { client, prompts } = fakeClient();
  const plugin = await CodeSense({ client });
  await plugin.event(created("B"));
  await host.armedCount(1);
  await plugin.event(deleted("B"));
  await pause(1300);
  assert.deepEqual(prompts, []);
  // Late events of the deleted conversation do not make the tab follow it again.
  await plugin.event(raised("session.execution.started", { sessionID: "B", status: { type: "busy" } }));
  await plugin.event(raised("session.execution.succeeded", { sessionID: "B" }));
  await pause(300);
  assert.equal(host.armed().length, 1);
  await plugin.event(created("C"));
  assert.deepEqual((await host.armedCount(2)).map((line) => line.split(" ")[0]), ["SessionStart", "SessionStart"]);
});

test("a plugin the desktop has not filled the waiter slot of never waits", async () => {
  const host = fakeHost();
  const { client } = fakeClient();
  const plugin = await Uninstalled({ client });
  await plugin.event(created("main"));
  await pause(300);
  assert.deepEqual(host.armed(), []);
  assert.equal((await host.settled(1)).length, 1, "its activity still reports");
});
