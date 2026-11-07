// SPDX-License-Identifier: MIT
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { runBuildJob } from "./lib/build-queue.mjs";
import {
  REFRESH_COMMAND,
  allowStaleExport as switchedOn,
  diskGate,
  exportsUntouchedSinceMain,
  inheritedNotice,
} from "./lib/sealed-export.mjs";
export const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const output = "codesense-docs/getting-started/content/generated/live-data-contract.json";
const manifest = "codesense-docs/getting-started/content/generated/live-data-contract.inputs.json";
const hash = (text) => createHash("sha256").update(text).digest("hex");
function walk(root, dir) {
  return readdirSync(resolve(root, dir), { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? walk(root, `${dir}/${e.name}`) : [`${dir}/${e.name}`],
  );
}
export function contractInputs(root = ROOT) {
  // local_contract_payload's static inputs: its module, embedded starter, SDK constants,
  // query version and the complete verb/policy registry. The canonical contract includes
  // the hosted section, which is compiled only with dashboard-publish; its sources are the
  // query/stream/action catalog, the toolkit helper and version constants in create.rs, and the
  // inbox and agents modules the catalog's entries run (a catalog edit changes the payload).
  // records/binding.rs feeds the records verb grants into the verb policy the payload serializes.
  const paths = [
    "src-tauri/Cargo.toml",
    "Cargo.lock",
    "src-tauri/src/mcp/live_data_authoring.rs",
    "src-tauri/src/mcp/live-data-starter.html",
    "src-tauri/src/records/binding.rs",
    "src-tauri/src/http/routes/viewer.rs",
    "src-tauri/src/context_query/model.rs",
    "src-tauri/src/dashboard_publish/agents.rs",
    "src-tauri/src/dashboard_publish/catalog.rs",
    "src-tauri/src/dashboard_publish/create.rs",
    "src-tauri/src/dashboard_publish/inbox.rs",
    "src-tauri/src/dashboard_publish/toolkit/cs-live.js",
    ...walk(root, "src-tauri/src/dashboard_publish/inbox"),
    ...walk(root, "src-tauri/src/verbs"),
  ];
  return Object.fromEntries(
    paths.sort().map((path) => [path, hash(readFileSync(resolve(root, path)))]),
  );
}
export function sealContract(root = ROOT) {
  const payload = readFileSync(resolve(root, output));
  JSON.parse(payload);
  mkdirSync(dirname(resolve(root, manifest)), { recursive: true });
  writeFileSync(
    resolve(root, manifest),
    JSON.stringify(
      {
        schema: 1,
        features: ["dashboard-publish"],
        inputs: contractInputs(root),
        outputSha256: hash(payload),
      },
      null,
      2,
    ) + "\n",
  );
}
export function checkContract(root = ROOT, { allowStaleExport = false, warn = console.warn } = {}) {
  const message = (what) =>
    `Stale generated file: ${output} (${what}); refresh during the next docs wave with ${REFRESH_COMMAND}`;
  const missing = !existsSync(resolve(root, manifest)) || !existsSync(resolve(root, output));
  const stored = missing ? null : JSON.parse(readFileSync(resolve(root, manifest), "utf8"));
  const payloadDrift = missing || stored.outputSha256 !== hash(readFileSync(resolve(root, output)));
  const inputDrift =
    !missing && JSON.stringify(stored.inputs) !== JSON.stringify(contractInputs(root));
  if (!payloadDrift && !inputDrift) return;
  const what = missing
    ? "missing sealed export"
    : payloadDrift
      ? "payload changed"
      : "source inputs changed";
  const untouched = allowStaleExport && exportsUntouchedSinceMain(root, [manifest, output]);
  if (untouched)
    return warn(payloadDrift ? inheritedNotice(root, message(what)) : `NOTICE: ${message(what)}`);
  throw new Error(message(what));
}
export function exportContract() {
  diskGate(ROOT);
  const result = runBuildJob(
    "live-data contract export",
    [process.execPath, fileURLToPath(import.meta.url), "--queued-export"],
    { cwd: ROOT, stdio: "inherit" },
  );
  if (result.status !== 0) throw new Error(`Rust contract export failed (${result.status})`);
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv.includes("--queued-export")) {
    diskGate(ROOT);
    // Cargo builds into the worktree's shared `target/` with the dev profile.
    const result = spawnSync(
      "cargo",
      [
        "test",
        "--manifest-path",
        "src-tauri/Cargo.toml",
        "--lib",
        "mcp::live_data_authoring::tests",
        "--features",
        "dashboard-publish",
        "-j",
        "4",
      ],
      {
        cwd: ROOT,
        stdio: "inherit",
        env: { ...process.env, LIVE_DATA_CONTRACT_OUTPUT: resolve(ROOT, output) },
      },
    );
    if (result.status !== 0) process.exitCode = result.status ?? 1;
    else sealContract();
  } else if (process.argv.includes("--export")) exportContract();
  else checkContract(ROOT, { allowStaleExport: switchedOn() });
}
