#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Render the exact MCP/YAML examples consumed by the real Rust create seam tests.
import { readFileSync, writeFileSync } from "node:fs";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const CONTENT = join(ROOT, "codesense-docs/getting-started/content");
const esc = (text) =>
  String(text).replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
export function yaml(value, indent = 0) {
  const pad = " ".repeat(indent);
  if (Array.isArray(value))
    return value
      .map((item) =>
        typeof item === "object" && item !== null
          ? `${pad}-\n${yaml(item, indent + 2)}`
          : `${pad}- ${JSON.stringify(item)}`,
      )
      .join("\n");
  return Object.entries(value)
    .map(([key, item]) =>
      typeof item === "object" && item !== null
        ? `${pad}${key}:\n${yaml(item, indent + 2)}`
        : `${pad}${key}: ${JSON.stringify(item)}`,
    )
    .join("\n");
}
export const guidance = {
  schedule:
    "<p>Runs weekdays at 09:00 UTC. The disabled draft has an empty <code>data.next_fires</code>; check its schedule wording, then enable and verify the next three weekday fires in that zone. Schedules use five-field cron and may not fire more often than every five minutes. After saving and enabling, wait for the next listed fire, then inspect its run.</p>",
  metadata_change:
    "<p>A metadata trigger watches story status entering review from in-progress. Selection uses the same query model as <code>query_context</code>; target and previous currently use <code>op: eq</code>. Omit previous for any transition into the target value. First discovery and app restart baseline existing matches; they do not replay them. Enable before making a new transition, keep <code>runs_on: one_mac</code>, then inspect the run trigger to identify the document. The dry run validates the condition, not an observed transition.</p>",
  inbox_idle:
    "<p>With this fresh-mode example, idle sessions with unread mail in reach of the team fire it; <code>{{trigger.sessions}}</code> identifies them. For a wake routine, only the owning agent’s own session with unread mail fires and receives it. To test, send ordinary inbox mail to an idle disposable agent session, then inspect the run. After three wakes that leave the same mail unread, the runner stops repeating that mail and records the reason in routine run history. Use <code>routine_runs</code> then <code>routine_run</code> to inspect it; write the task to read the mail.</p>",
  agent_done:
    "<p><code>from_agent</code> filters to the saved agent named default; omit it to watch all agents in reach. After enabling, finish a disposable run of that agent and inspect the trigger’s agent, status and summary. This observes an agent completion; use <code>after</code> to watch a specific routine outcome.</p>",
  role_pending:
    '<p>Use <code>{kind:"role_pending"}</code> when a dispatcher needs every role queue in reach. Use <code>{kind:"role_pending",role:"reviewer"}</code> when this routine should run only for reviewer mail. The example below uses that filter; omit <code>role</code> for the wildcard. A team routine watches its own team; a private routine watches teams in its org. Empty or whitespace-only role names fail validation; role names need no registry entry. The run receives <code>{{trigger.role}}</code>, <code>{{trigger.message_id}}</code>, <code>{{trigger.org}}</code> and <code>{{trigger.team}}</code>. Notification does not claim mail: use <code>register_role</code>, <code>claim_role_messages</code>, <code>accept_role_message</code> before work and <code>ack_role_message</code> after work. After enabling, enqueue a disposable message with <code>send_role_message</code>, then inspect both role_state and the routine run. A defaultHandler is configuration only and does not automatically launch an agent.</p>',
  after:
    '<p>Create the predecessor <code>review-command</code> from the <a href="./command-routines.html#chain">command chain</a> before this dry run. A successful predecessor run fires this agent routine with <code>{{trigger.result_text}}</code>. <code>on: finished</code> continues after <code>succeeded</code>, <code>failed</code>, <code>timed_out</code>, <code>needs_you</code> or <code>ended</code>; <code>cancelled</code> does not fire a chain. Trigger the upstream command after enabling both; compare the downstream trigger run_id to the upstream run. Dry-run success establishes the saved dependency, not a completed chain.</p>',
};
function blocks(example) {
  return `<h3>MCP: routine_create dry run</h3>\n<pre><code class="language-json">${esc(JSON.stringify(example.mcp, null, 2))}</code></pre>\n<h3>Complete routine.yml</h3>\n<pre><code class="language-yaml">${esc(yaml(example.yaml))}</code></pre>`;
}
export function renderExamples(examples) {
  const cards = examples
    .filter((e) => e.kind !== "command")
    .map(
      (example) =>
        `<section class="trigger-card" data-search="${example.kind === "metadata_change" ? "metadata trigger" : example.kind} routine_create dry run YAML" id="${example.kind}"><h2>${esc(example.title)} · <code>${example.kind}</code></h2>\n${guidance[example.kind]}\n${blocks(example)}</section>`,
    )
    .join("\n");
  const chain = ["command", "after"]
    .map((kind) => {
      const example = examples.find((e) => e.kind === kind);
      return `<h3>${esc(example.title)}</h3>\n${blocks(example)}`;
    })
    .join("\n");
  return { cards, chain };
}
export function generate(check = false) {
  const { examples } = JSON.parse(
    readFileSync(join(CONTENT, "examples/routines/cookbook.json"), "utf8"),
  );
  const { cards, chain } = renderExamples(examples);
  for (const [name, marker, body] of [
    ["trigger-cookbook", "cookbook", cards],
    ["command-routines", "chain", chain],
  ]) {
    const path = join(CONTENT, "arc-g-routines", `${name}.html`);
    const current = readFileSync(path, "utf8");
    const pattern = new RegExp(`<!-- ${marker}:start -->[\\s\\S]*?<!-- ${marker}:end -->`);
    if (!pattern.test(current)) throw new Error(`Missing ${marker} markers in ${path}`);
    const next = current.replace(
      pattern,
      `<!-- ${marker}:start -->\n${body}\n<!-- ${marker}:end -->`,
    );
    if (check) {
      if (current !== next) throw new Error(`${path}: examples drifted; pnpm gen:docs`);
    } else writeFileSync(path, next);
  }
  console.log(`generate-routine-cookbook: ${check ? "ok" : "rendered shared examples"}`);
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url))
  generate(process.argv.includes("--check"));
