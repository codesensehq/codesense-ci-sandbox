// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { test } from "node:test";

const SOURCE = readFileSync(new URL("./codesense.js", import.meta.url), "utf8");
const SLOT = "/* @supported */ null";
const RANGE = { min: [2, 1, 4], beforeMajor: 3 };
const WAITER_SLOT = "/* @waiter */ null";
const PROTOCOL = {
  events: ["SessionStart", "Stop", "StopFailure", "PostToolUse"],
  replace: ["SessionStart", "Stop", "StopFailure"],
  renewal: "Re-arming inbox waiter; no action needed.",
  confirmation: "delivered",
};
let installs = 0;

// The plugin as the desktop installs it: its range and its waiter protocol written into
// their slots.
async function installed(range) {
  assert.ok(SOURCE.includes(SLOT), "the plugin has a slot for its supported range");
  const dir = mkdtempSync(join(tmpdir(), "codesense-opencode-guard-"));
  const file = join(dir, "codesense.js");
  assert.ok(SOURCE.includes(WAITER_SLOT), "the plugin has a slot for its waiter protocol");
  writeFileSync(
    file,
    SOURCE.replace(SLOT, `/* @supported */ ${JSON.stringify(range)}`).replace(
      WAITER_SLOT,
      JSON.stringify(PROTOCOL),
    ),
  );
  installs += 1;
  const definition = (await import(`${pathToFileURL(file).href}?install=${installs}`)).default;
  return async ({ client } = {}) => {
    const hooks = {};
    const original = { error: console.error, warn: console.warn };
    console.error = (message) => { try { client.logs.push({ service: "codesense", level: "info", message }); } catch {} };
    console.warn = (message) => { try { client?.logs.push({ service: "codesense", level: "warn", message }); } catch {} };
    try {
      const register = (domain) => (name, handler) => { hooks[`${domain}.${name}`] = handler; };
      await definition.setup({ ...client,
        session: { ...client?.session, hook: register("session") },
        tool: { hook: register("tool") }, shell: { hook: register("shell") },
        event: { subscribe: () => ({ async *[Symbol.asyncIterator]() {} }) },
      });
      return hooks;
    } finally { Object.assign(console, original); }
  };
}

// An OpenCode client that answers `/global/health` with `version`, or fails.
function host(version) {
  const logs = [];
  const asked = [];
  const client = { app: { version }, logs, session: {} };
  return { client, logs, asked };
}

test("an OpenCode in the supported range runs the plugin", async () => {
  const CodeSense = await installed(RANGE);
  for (const version of ["2.1.4", "2.1.21", "2.99.3", "2.1.5-beta.2"]) {
    const { client, logs, asked } = host(version);
    const hooks = await CodeSense({ client });
    assert.equal(typeof hooks["tool.execute.after"], "function", version);
    assert.equal(typeof hooks["session.prompt"], "function", version);
    // Its one log line says it started, once its hooks are registered.
    assert.deepEqual(
      logs.map((log) => [log.level, log.message]),
      [["info", "CodeSense plugin started"]],
      version,
    );
    assert.deepEqual(asked, [], version);
  }
});

test("an unknown major disables every hook with one log line", async () => {
  const CodeSense = await installed(RANGE);
  const { client, logs } = host("3.0.1");
  const hooks = await CodeSense({ client });
  assert.deepEqual(Object.keys(hooks), []);
  assert.equal(logs.length, 1);
  assert.equal(logs[0].service, "codesense");
  assert.equal(logs[0].level, "warn");
  assert.match(logs[0].message, /3\.0\.1/);
  assert.match(logs[0].message, /disabled/);
});

test("a release below the minimum, or only a prerelease of it, is disabled", async () => {
  const CodeSense = await installed(RANGE);
  for (const version of ["2.1.3", "2.1.4-rc.1", "1.19.4", "banana", ""]) {
    const { client, logs } = host(version);
    assert.deepEqual(Object.keys(await CodeSense({ client })), [], version);
    assert.equal(logs.length, 1, version);
  }
});

test("a host version that cannot be read disables the plugin", async () => {
  const CodeSense = await installed(RANGE);
  const failing = host(undefined);
  assert.deepEqual(Object.keys(await CodeSense({ client: failing.client })), []);
  assert.equal(failing.logs.length, 1);
  assert.match(failing.logs[0].message, /could not read/);
  // No client at all is the same unknown.
  assert.deepEqual(Object.keys(await CodeSense({})), []);
});

test("a range with no upper major only bounds the minimum", async () => {
  const CodeSense = await installed({ min: [2, 1, 141], beforeMajor: null });
  const { client, logs } = host("3.4.0");
  assert.equal(typeof (await CodeSense({ client }))["tool.execute.after"], "function");
  assert.deepEqual(
    logs.map((log) => log.message),
    ["CodeSense plugin started"],
  );
});

test("a log that fails never breaks the plugin load", async () => {
  const CodeSense = await installed(RANGE);
  const { client } = host("2.0.0");
  client.logs.push = () => {
    throw new Error("log down");
  };
  assert.deepEqual(Object.keys(await CodeSense({ client })), []);
});

// A stand-in host whose helper records each way the plugin runs it, and a usage receiver that
// counts what it is sent.
async function standIns() {
  const dir = mkdtempSync(join(tmpdir(), "codesense-opencode-guard-host-"));
  const runs = join(dir, "runs");
  const helper = join(dir, "activity-helper");
  writeFileSync(
    helper,
    `#!/bin/sh
echo "$1" >> "${runs}"
[ "$1" = "--inbox-wait" ] && exec sleep 1
cat > /dev/null
`,
  );
  chmodSync(helper, 0o755);
  const posts = [];
  const receiver = createServer((req, res) => {
    posts.push(req.url);
    res.end();
  });
  await new Promise((resolve) => receiver.listen(0, "127.0.0.1", resolve));
  process.env.CODESENSE_ACTIVITY_HOST = dir;
  process.env.CODESENSE_USAGE_ENDPOINT = `http://127.0.0.1:${receiver.address().port}`;
  const ran = () => {
    try {
      return readFileSync(runs, "utf8").trim().split("\n").filter(Boolean);
    } catch {
      return [];
    }
  };
  const close = () => {
    delete process.env.CODESENSE_ACTIVITY_HOST;
    delete process.env.CODESENSE_USAGE_ENDPOINT;
    receiver.close();
  };
  return { ran, posts, close };
}

const mainSession = { get: async () => ({ id: "s1" }), prompt: async () => ({ id: "accepted", delivery: "queue" }) };

test("outside the range nothing runs: no activity, no waiter, no usage, one log line", async () => {
  const stand = await standIns();
  try {
    const CodeSense = await installed(RANGE);
    const { client, logs } = host("3.0.1");
    client.session = mainSession;
    const hooks = await CodeSense({ client });
    assert.deepEqual(Object.keys(hooks), []);
    await new Promise((resolve) => setTimeout(resolve, 300));
    assert.deepEqual(stand.ran(), []);
    assert.deepEqual(stand.posts, []);
    assert.equal(logs.length, 1);
  } finally {
    stand.close();
  }
});

test("inside the range the same host sees activity and a waiter", async () => {
  const stand = await standIns();
  try {
    const CodeSense = await installed(RANGE);
    const { client } = host("2.1.4");
    client.session = mainSession;
    const hooks = await CodeSense({ client });
    await hooks["session.prompt"]({ sessionID: "s1" });
    const deadline = Date.now() + 5000;
    while (!stand.ran().includes("--inbox-wait") && Date.now() < deadline)
      await new Promise((resolve) => setTimeout(resolve, 50));
    assert.ok(stand.ran().includes("--activity-hook"), JSON.stringify(stand.ran()));
    assert.ok(stand.ran().includes("--inbox-wait"), JSON.stringify(stand.ran()));
  } finally {
    stand.close();
  }
});
