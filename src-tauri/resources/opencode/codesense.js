// SPDX-License-Identifier: MIT
import { spawn } from "node:child_process";
import { accessSync, constants } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const HELPER_TIMEOUT_MS = 3000;
const MAX_PENDING = 256;
const MAX_ID = 128;

// Ids and names are the only things this plugin forwards. Prompt text, tool
// arguments and results, and permission patterns never leave OpenCode.
function opaque(value) {
  if (typeof value !== "string") return undefined;
  const clean = value.replace(/[\u0000-\u001f\u007f]/g, "").slice(0, MAX_ID);
  return clean || undefined;
}

// The hook `error` reason for an OpenCode error: a name or status code, never its message.
function reason(error) {
  const status = Number(error?.statusCode);
  switch (error?.type) {
    case "ProviderAuthError":
      return "authentication_failed";
    case "MessageOutputLengthError":
      return "max_output_tokens";
    case "APIError":
      if (status === 401 || status === 403) return "authentication_failed";
      if (status === 429) return "rate_limit";
      if (status >= 500) return "server_error";
      return "invalid_request";
    default:
      return "unknown";
  }
}

const LOOKUP_TIMEOUT_MS = 2000;
const USAGE_TIMEOUT_MS = 3000;
const MAX_REPORTED = 4096;
const MAX_OUTBOX = 200;
const MAX_INFLIGHT = 16;
const QUICK_RETRY_MS = 20;
const QUICK_RETRIES = 2;
const RETRY_MIN_MS = 500;
const RETRY_MAX_MS = 30000;

function count(value) {
  const n = Number(value);
  return Number.isFinite(n) && n > 0 ? String(Math.floor(n)) : "0";
}

// One OTLP log record for a finished assistant message: ids, names and numbers only.
function usageRecord(info, session) {
  const tokens = info.tokens ?? {};
  const text = (key, value) => ({ key, value: { stringValue: value } });
  const number = (key, value) => ({ key, value: { intValue: count(value) } });
  const attributes = [
    text("event.name", "opencode.usage"),
    text("event.timestamp", new Date(Number(info.time.completed)).toISOString()),
    text("session.id", session),
    text("message_id", opaque(info.id)),
    text("model", opaque(info.modelID) ?? ""),
    text("provider", opaque(info.providerID) ?? ""),
    number("input", tokens.input),
    number("output", tokens.output),
    number("reasoning", tokens.reasoning),
    number("cache_read", tokens.cache?.read),
    number("cache_write", tokens.cache?.write),
  ];
  // A zero is sent: the receiver decides whether it means free or unpriced.
  const cost = info.cost;
  if (typeof cost === "number" && Number.isFinite(cost) && cost >= 0) attributes.push({ key: "cost", value: { doubleValue: cost } });
  return { attributes };
}

const MAX_NOTICE = 8192;
// The waiter protocol is not written here: the desktop fills this slot when it installs the
// plugin, from the adapter that declares it, so there is one source. `events` are the hook
// events a waiter is armed at, `replace` those of them that take the session's place (the
// others defer to a waiter already waiting), `renewal` the line a waiter prints to renew
// itself, which is not mail, and `confirmation` the line that tells a waiter its notice was
// delivered, which it must hear before it counts the mail told. Without it (an unfilled copy) the plugin never waits.
const WAITER = /* @waiter */ null;
const ARM_EVENTS = new Set(WAITER?.events ?? []);
const ARM_REPLACING = new Set(WAITER?.replace ?? []);
const ARM_IF_ABSENT = (WAITER?.events ?? []).find((event) => !ARM_REPLACING.has(event));
const REARM_LINE = WAITER?.renewal;
const CONFIRMATION_LINE = WAITER?.confirmation;
// OpenCode commands that run no terminal: a waiter there has no one to wake. The
// interactive terminal UI runs from its own worker script, which names none of them.
// A resumed session raises no event until something happens in it, so at boot the plugin
// reads how this OpenCode was started. The server may not be up at once: it asks again.
const RESUME_DELAY_MS = 1000;
const RESUME_TRIES = 5;
const HEADLESS_COMMANDS = new Set(["run", "serve", "web", "acp"]);
// Global options (`opencode --help`) that take a value, which is not the command.
const VALUE_OPTIONS = new Set([
  "--log-level",
  "--port",
  "--hostname",
  "--mdns-domain",
  "--cors",
  "-m",
  "--model",
  "-s",
  "--session",
  "--prompt",
  "--agent",
  "--replay-limit",
]);

// The subcommand an OpenCode was started with: the first operand after the options and
// their values. None for the terminal UI, whose `[project]` operand is not a command.
function subcommand(argv) {
  for (let at = 2; at < argv.length; at += 1) {
    const arg = argv[at];
    if (arg === "--") return undefined;
    if (!arg.startsWith("-")) return arg;
    if (!arg.includes("=") && VALUE_OPTIONS.has(arg)) at += 1;
  }
  return undefined;
}

// The command line of this process, as `ps` shows it.
function commandLine(pid = process.pid) {
  return new Promise((resolve, reject) => {
    const child = spawn("ps", ["-o", "args=", "-p", String(pid)], {
      stdio: ["ignore", "pipe", "ignore"],
    });
    let out = "";
    const timer = setTimeout(() => child.kill("SIGKILL"), LOOKUP_TIMEOUT_MS);
    child.stdout.on("data", (chunk) => {
      if (out.length < MAX_NOTICE) out += chunk;
    });
    child.once("error", reject);
    child.once("close", (code) => {
      clearTimeout(timer);
      if (code === 0) resolve(out.trim());
      else reject(new Error("ps"));
    });
  });
}

// The OpenCode releases this plugin's events, hooks and client calls were qualified on:
// `{ min: [major, minor, patch], beforeMajor }`, written at install from the same release
// range the desktop checks versions with. Unfilled (running from source), nothing is checked.
const SUPPORTED = /* @supported */ null;

// Whether `version` is in `range`: at least `min` (a prerelease of `min` itself is not it,
// a prerelease of a later version is) and below `beforeMajor`.
function inRange(version, range) {
  const match = /^(\d+)\.(\d+)\.(\d+)(-[^+]*)?(\+.*)?$/.exec(String(version));
  if (!match) return false;
  const found = match.slice(1, 4).map(Number);
  const cmp = found.reduce((diff, part, at) => diff || part - range.min[at], 0);
  if (cmp < 0 || (cmp === 0 && (match[4] || match[5]))) return false;
  return range.beforeMajor == null || found[0] < range.beforeMajor;
}

// Why this plugin must not run in its host, or undefined when it may. The host's version
// is asked of OpenCode itself, so a hand-typed launch is covered too; a host that cannot
// say is treated as unknown, which is not supported.
function unsupportedHost(client, range) {
  const version = client?.app?.version;
  if (typeof version !== "string") return "could not read the OpenCode version";
  if (inRange(version, range)) return undefined;
  const upper = range.beforeMajor == null ? "" : ` and before ${range.beforeMajor}.0.0`;
  return `OpenCode ${version} is outside the supported range (${range.min.join(".")} or newer${upper})`;
}

async function setup(client = {}) {
  if (SUPPORTED) {
    const unsupported = unsupportedHost(client, SUPPORTED);
    if (unsupported) {
      console.warn(`CodeSense plugin disabled: ${unsupported}`);
      return;
    }
  }
  const abort = new AbortController();
  const timers = new Set();
  let disposed = false;
  const models = new Map();
  // Session -> its parent, or null for a main session. Learned from `session.created` and
  // `session.updated`, else asked of OpenCode the first time a session is heard from, since
  // a session restored from an earlier run is never created again.
  const parents = new Map();
  // Sessions working on a turn. A session goes idle once per turn, but OpenCode follows an
  // error with its idle and repeats idle, which would hide a failure as Done.
  const active = new Set();
  const asks = new Map();
  // The main session the tab follows: a prompt or a new session in another one moves it.
  let current;
  // Assistant messages whose usage was already posted: OpenCode updates each one three times.
  const reported = new Set();
  // Finished messages not yet accepted by the receiver, oldest first. A refusal that may pass
  // (the receiver busy, unreachable, or asking for a retry) keeps its record here for a
  // backed-off retry; the receiver also drops a repeated message id.
  const outbox = [];
  let draining = false;
  let inflight = 0;
  let retry;
  let backoff = RETRY_MIN_MS;
  let lost = 0;
  // A standalone V2 server is a leased child of the actual terminal/run process.
  // Its own argv says `serve --stdio`, so classify and restore its owning client.
  const ownerPID = subcommand(process.argv) === "serve" && process.argv.includes("--stdio") ? process.ppid : process.pid;
  const clientArgs = ownerPID === process.pid ? process.argv : ["", ...(await commandLine(ownerPID)).split(/\s+/)];
  const headless = HEADLESS_COMMANDS.has(subcommand(clientArgs));
  // Main session -> the inbox waiter helper running for it. At most one each.
  const waiters = new Map();
  // Waiters that have written to stderr: a notice, or the renewal line.
  const printing = new WeakSet();
  // Conversations that were deleted: late events of theirs do not make the tab follow them.
  const gone = new Set();
  let tail = Promise.resolve();
  let pending = 0;

  // A session's parent: a string, null for a main session, undefined when it cannot be
  // told. An unknown session is never taken for a main one nor remembered as one: a child
  // reported as a main session would rebind the tab to it.
  async function lookup(id) {
    if (typeof client?.session?.get !== "function") return null;
    let timer;
    try {
      const timeout = new Promise((resolve) => {
        timer = setTimeout(resolve, LOOKUP_TIMEOUT_MS);
      });
      const got = await Promise.race([client.session.get({ sessionID: id }), timeout]);
      if (!got) return undefined;
      return opaque(got.parentID) ?? null;
    } catch {
      return undefined;
    } finally {
      clearTimeout(timer);
    }
  }

  async function parentOf(id) {
    if (parents.has(id)) return parents.get(id);
    const parent = await lookup(id);
    if (parent !== undefined) parents.set(id, parent);
    return parent;
  }

  async function rootOf(id) {
    for (let hops = 0; hops < 16; hops += 1) {
      const parent = await parentOf(id);
      if (parent === undefined) return undefined;
      if (!parent) return id;
      id = parent;
    }
    return id;
  }

  // The running children of `main`, from what is already known of them.
  function runningChildren(main) {
    const found = [];
    for (const id of active) {
      let up = id;
      for (let hops = 0; parents.get(up) && hops < 16; hops += 1) up = parents.get(up);
      if (parents.get(id) && up === main) found.push(id);
    }
    return found;
  }

  // Synchronous, so an arm decides and spawns with nothing else interleaved.
  function helperPath() {
    const host = process.env.CODESENSE_ACTIVITY_HOST;
    if (!host) return undefined;
    const helper = join(host, "activity-helper");
    try {
      accessSync(helper, constants.X_OK);
    } catch {
      return undefined;
    }
    return helper;
  }

  async function deliver(payload) {
    const helper = helperPath();
    if (!helper) return;
    await new Promise((resolve) => {
      const child = spawn(helper, ["--activity-hook", "opencode"], {
        stdio: ["pipe", "ignore", "ignore"],
      });
      const timer = setTimeout(() => child.kill("SIGKILL"), HELPER_TIMEOUT_MS);
      const done = () => {
        clearTimeout(timer);
        resolve();
      };
      child.once("error", done);
      child.once("close", done);
      child.stdin.on("error", () => {});
      child.stdin.end(JSON.stringify(payload));
    });
  }

  // A waiter that ends with mail is the notice on stderr and exit status 2, the contract
  // of every CodeSense inbox waiter. The notice starts (or, mid-turn, queues) a turn of
  // the main session in this process; nothing is typed into the terminal.
  // The tab has one waiter place, for the session it follows: a turn that ends in another
  // main session must not take it.
  function arm(session, event) {
    if (disposed || headless || !client?.session?.prompt) return;
    if (current !== undefined && session !== current) return;
    if (!ARM_EVENTS.has(event)) return;
    const replacing = ARM_REPLACING.has(event);
    const waiting = waiters.get(session);
    if (waiting && !replacing) return;
    // A waiter that has begun to print holds mail the host may already count as told: it
    // is left to finish and deliver, and only a quiet one is killed.
    if (waiting && !printing.has(waiting)) waiting.kill("SIGTERM");
    waiters.delete(session);
    const helper = helperPath();
    if (!helper) return;
    const child = spawn(helper, ["--inbox-wait", "opencode", event, String(ownerPID)], {
      stdio: ["pipe", "ignore", "pipe"],
    });
    waiters.set(session, child);
    child.stdin.on("error", () => {});
    let heard = "";
    let handled = false;
    let renewing = false;
    // The waiter prints a notice and then waits to be told it was delivered. Only a
    // delivered notice is confirmed; any other end leaves the mail with the host, which
    // offers it to the next waiter.
    child.stderr.on("data", (chunk) => {
      printing.add(child);
      if (heard.length < MAX_NOTICE) heard += chunk;
      const end = heard.indexOf("\n");
      if (end < 0 || handled || renewing) return;
      const line = heard.slice(0, end).trim();
      if (!line) return;
      if (line === REARM_LINE) {
        renewing = true;
        return;
      }
      handled = true;
      deliverNotice(line).then((delivered) => {
        if (delivered) child.stdin.write(`${CONFIRMATION_LINE}\n`);
        child.stdin.end();
      });
    });
    child.stderr.on("error", () => {});
    const release = () => waiters.get(session) === child && waiters.delete(session);
    child.once("error", release);
    child.once("close", (code) => {
      const mine = release();
      if (renewing && code === 2 && mine) arm(session, ARM_IF_ABSENT);
    });
  }

  // Hands a notice to the conversation the tab follows: whether OpenCode took it. With no
  // conversation to receive it, or a refusal, the mail stays with the host.
  async function deliverNotice(text) {
    if (current === undefined) return false;
    try {
      const answer = await client.session.prompt({ sessionID: current, text, delivery: "queue" });
      return typeof answer?.id === "string" && answer.delivery === "queue";
    } catch {
      return false;
    }
  }

  // The tab moved to another session: the waiters of the ones it left give up their place.
  function retireWaiters() {
    for (const [id, waiting] of waiters) {
      if (id === current) continue;
      if (!printing.has(waiting)) waiting.kill("SIGTERM");
      waiters.delete(id);
    }
  }

  // Everything runs one task at a time, in the order OpenCode raised it, so a lookup never
  // lets a later event overtake an earlier one and the owner hears them in order.
  function enqueue(task) {
    if (pending >= MAX_PENDING) return;
    pending += 1;
    tail = tail
      .then(task)
      .catch(() => {})
      .finally(() => {
        pending -= 1;
      });
  }

  // `asMain` reports a child's event as the main turn's: a permission prompt blocks the
  // whole tab whichever session raised it. `bind` is for what the user started in a main
  // session (a prompt, a new session, a compaction): it moves the tab there, announcing the
  // Tasks the session still runs, which the owner forgot when it left. Anything else a
  // session other than the tab's reports is sent as it is, and the owner refuses it.
  async function emit(name, sessionID, extra = {}, { asMain = false, bind = false } = {}) {
    const session = opaque(sessionID);
    if (!session || gone.has(session)) return;
    const parent = await parentOf(session);
    if (parent === undefined) return;
    const main = parent ? await rootOf(session) : session;
    if (main === undefined || gone.has(main)) return;
    if (current === undefined || (bind && !parent && current !== main)) {
      current = main;
      await deliver({ hook_event_name: "SessionStart", session_id: main });
      if (!parent) {
        retireWaiters();
        arm(main, "SessionStart");
      }
      for (const child of runningChildren(main)) {
        if (child !== session) {
          await deliver({ hook_event_name: "SubagentStart", session_id: main, agent_id: child });
        }
      }
    }
    if (name === "SessionStart") return;
    const payload = { hook_event_name: name, session_id: main };
    if (parent && !asMain) payload.agent_id = session;
    for (const [key, value] of Object.entries(extra)) {
      const clean = opaque(value);
      if (clean) payload[key] = clean;
    }
    await deliver(payload);
    // An interrupted turn ends like any other: its waiter is re-armed.
    if (!parent) arm(main, name === "Interrupt" ? "Stop" : name);
  }

  // A prompt or a permission/question reply that settles a call the owner may be waiting on.
  async function answered(requestID, sessionID, rejected) {
    const call = asks.get(requestID);
    asks.delete(requestID);
    const extra = { tool_use_id: call };
    if (rejected) return emit("PostToolUse", sessionID, extra, { asMain: true });
    // A correlated wait ends with its call, which then goes on running.
    if (call) await emit("PostToolUse", sessionID, extra, { asMain: true });
    return emit("PreToolUse", sessionID, extra, { asMain: true });
  }

  async function ask(name, { id, sessionID, source }) {
    const call = opaque(source?.id);
    if (opaque(id) && call) asks.set(opaque(id), call);
    await emit(name, sessionID, call ? { tool_use_id: call } : {}, { asMain: true });
  }

  // Work the plugin saw no prompt for (a compaction, a resumed Task) starts here.
  async function busy(sessionID) {
    if (active.has(sessionID)) return;
    const parent = await parentOf(sessionID);
    if (parent === undefined) return;
    active.add(sessionID);
    if (parent) await emit("SubagentStart", sessionID);
    else await emit("PreCompact", sessionID, {}, { bind: true });
  }

  function warn(message) {
    console.warn(message);
  }

  // 0: accepted. 1: refused for good, or no endpoint: drop it. 2: try again later.
  async function post(record) {
    const base = process.env.CODESENSE_USAGE_ENDPOINT;
    if (!base) return 1;
    try {
      const response = await fetch(`${base}/v1/logs`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(record),
        signal: AbortSignal.timeout(USAGE_TIMEOUT_MS),
      });
      if (response.ok) return 0;
      return response.status >= 500 || response.status === 429 ? 2 : 1;
    } catch {
      return 2;
    }
  }

  function refused() {
    lost += 1;
    warn(`CodeSense usage: a usage record was refused (${lost} lost so far)`);
  }

  // Backed-off retries, one record at a time, oldest first, until the receiver takes them.
  function schedule() {
    if (disposed || retry || draining) return;
    retry = setTimeout(async () => {
      retry = undefined;
      draining = true;
      try {
        while (outbox.length) {
          const record = outbox[0];
          const result = await post(record);
          if (result === 2) {
            backoff = Math.min(backoff * 2, RETRY_MAX_MS);
            return;
          }
          if (result === 1) refused();
          backoff = RETRY_MIN_MS;
          const at = outbox.indexOf(record);
          if (at >= 0) outbox.splice(at, 1);
        }
      } finally {
        draining = false;
        if (outbox.length) schedule();
      }
    }, backoff);
    retry.unref?.();
  }

  function defer(record) {
    if (outbox.length >= MAX_OUTBOX) {
      outbox.shift();
      lost += 1;
      warn(`CodeSense usage: the retry queue is full, a usage record was dropped (${lost} lost so far)`);
    }
    outbox.push(record);
    schedule();
  }

  // Each record goes out on its own, off the activity queue, so a slow receiver never delays a
  // tab's status. A receiver that is only briefly busy usually answers a moment later, which
  // also matters when OpenCode exits right after its last message; a longer outage waits in
  // the outbox for the backed-off retries.
  async function deliverUsage(record) {
    inflight += 1;
    try {
      let result = await post(record);
      for (let tries = 0; result === 2 && tries < QUICK_RETRIES; tries += 1) {
        await new Promise((resolve) => setTimeout(resolve, QUICK_RETRY_MS * 3 ** tries));
        result = await post(record);
      }
      if (result === 2) defer(record);
      else if (result === 1) refused();
    } finally {
      inflight -= 1;
    }
  }

  // A subagent's message is reported under its main session, like its activity.
  async function reportUsage(info) {
    const id = opaque(info?.id);
    const session = opaque(info?.sessionID);
    if (!process.env.CODESENSE_USAGE_ENDPOINT || !id || !session) return;
    if (info.role !== "assistant" || !Number.isFinite(Number(info.time?.completed))) return;
    if (reported.has(id)) return;
    if (reported.size >= MAX_REPORTED) reported.delete(reported.values().next().value);
    reported.add(id);
    const record = { resourceLogs: [{ scopeLogs: [{ logRecords: [usageRecord(info, await rootOf(session))] }] }] };
    if (inflight >= MAX_INFLIGHT) defer(record);
    else void deliverUsage(record);
  }
  // The main session a `--continue` or `--session` launch restores, if it was launched so.
  async function resumedSession() {
    const args = (await commandLine(ownerPID)).split(/\s+/);
    if (args.includes("--fork")) return undefined;
    const at = args.findIndex((arg) => arg === "--session" || arg === "-s");
    const named = at >= 0 ? args[at + 1] : args.find((arg) => arg.startsWith("--session="))?.slice(10);
    if (named) return opaque(named);
    if (!args.some((arg) => arg === "--continue" || arg === "-c")) return undefined;
    const listed = await client.session.list();
    const roots = (listed?.data ?? []).filter((info) => info && !info.parentID);
    roots.sort((a, b) => (b.time?.updated ?? 0) - (a.time?.updated ?? 0));
    return opaque(roots[0]?.id);
  }

  function watchResume(tries = 1) {
    const timer = setTimeout(async () => {
      timers.delete(timer);
      if (disposed) return;
      try {
        const id = await resumedSession();
        // A session already followed was not the restored one.
        if (id) enqueue(() => (current === undefined ? emit("SessionStart", id, {}, { bind: true }) : undefined));
      } catch {
        if (tries < RESUME_TRIES) watchResume(tries + 1);
      }
    }, RESUME_DELAY_MS);
    timers.add(timer);
    timer.unref();
  }
  if (!headless && client?.session?.list && client?.session?.prompt) watchResume();

  const events = {
    "session.step.started": ({ assistantMessageID, model }) => {
      if (models.size >= MAX_REPORTED) models.delete(models.keys().next().value);
      models.set(assistantMessageID, model);
    },
    "session.step.ended": async ({ assistantMessageID, sessionID, tokens, cost }, envelope) => {
      const model = models.get(assistantMessageID);
      models.delete(assistantMessageID);
      if (!model) return;
      await reportUsage({ id: assistantMessageID, sessionID, role: "assistant", tokens, cost,
        modelID: model.id, providerID: model.providerID, time: { completed: envelope.created } });
    },
    "session.created": async ({ sessionID, parentID }) => {
      const id = opaque(sessionID);
      if (!id) return;
      const parent = opaque(parentID) ?? null;
      parents.set(id, parent);
      if (parent) {
        active.add(id);
        await emit("SubagentStart", id);
      } else await emit("SessionStart", id, {}, { bind: true });
    },
    "session.deleted": async ({ sessionID }) => {
      const id = opaque(sessionID);
      if (!id) return;
      gone.add(id);
      parents.delete(id);
      active.delete(id);
      if (id !== current) return;
      current = undefined;
      const waiting = waiters.get(id);
      if (waiting && !printing.has(waiting)) waiting.kill("SIGTERM");
      waiters.delete(id);
    },
    "session.execution.started": ({ sessionID }) => busy(sessionID),
    "session.execution.succeeded": async ({ sessionID }) => {
      if (!active.delete(sessionID)) {
        if ((await parentOf(sessionID)) === null) arm(sessionID, ARM_IF_ABSENT);
        return;
      }
      const parent = await parentOf(sessionID);
      if (parent !== undefined) await emit(parent ? "SubagentStop" : "Stop", sessionID);
    },
    "session.execution.failed": async ({ sessionID, error }) => {
      active.delete(sessionID);
      if (await parentOf(sessionID)) await emit("SubagentStop", sessionID);
      else await emit("StopFailure", sessionID, { error: reason(error) });
    },
    "session.execution.interrupted": async ({ sessionID }) => {
      active.delete(sessionID);
      if (await parentOf(sessionID)) await emit("SubagentStop", sessionID);
      else await emit("Interrupt", sessionID);
    },
    "permission.asked": (data) => ask("PermissionRequest", data),
    "permission.replied": ({ sessionID, requestID, reply }) => answered(requestID, sessionID, reply === "reject"),
    "form.created": ({ form }) => {
      if (form.metadata?.kind !== "question") return;
      return ask("Question", { ...form, source: form.metadata.tool });
    },
    "form.replied": ({ id, sessionID }) => asks.has(id) ? answered(id, sessionID, true) : undefined,
    "form.cancelled": ({ id, sessionID }) => asks.has(id) ? answered(id, sessionID, true) : undefined,
  };

  await client.session?.hook?.("prompt", async ({ sessionID }) => {
    enqueue(async () => {
      if ((await parentOf(sessionID)) !== null) return;
      active.add(sessionID);
      await emit("UserPromptSubmit", sessionID, {}, { bind: true });
    });
  });
  await client.tool?.hook?.("execute.before", async ({ tool, sessionID, id }) => {
    enqueue(() => emit("PreToolUse", sessionID, { tool_name: tool, tool_use_id: id }));
  });
  await client.tool?.hook?.("execute.after", async ({ tool, sessionID, id }) => {
    enqueue(() => emit("PostToolUse", sessionID, { tool_name: tool, tool_use_id: id }));
  });
  await client.shell?.hook?.("create.before", async ({ env }) => {
    const original = process.env.CODESENSE_ORIGINAL_XDG_CONFIG_HOME;
    if (original === undefined) return;
    env.XDG_CONFIG_HOME = original || join(homedir(), ".config");
    delete env.CODESENSE_ORIGINAL_XDG_CONFIG_HOME;
  });
  const subscription = (async () => {
    try {
      if (!client.event?.subscribe) return;
      for await (const event of client.event.subscribe({ signal: abort.signal })) {
        if (disposed) break;
        const handle = events[event.type];
        if (handle) enqueue(() => handle(event.data ?? {}, event));
      }
    } catch {
      if (!disposed) warn("CodeSense event subscription ended");
    }
  })();
  console.error("CodeSense plugin started");
  return async () => {
    disposed = true;
    abort.abort();
    clearTimeout(retry);
    for (const timer of timers) clearTimeout(timer);
    for (const waiter of waiters.values()) waiter.kill("SIGTERM");
    waiters.clear();
    await subscription;
    await tail;
  };
}

export default { id: "codesense", setup };
