// SPDX-License-Identifier: MIT
// Security specs must fail, never skip, when their fixture is missing.
import console from "node:console";
import { existsSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import process from "node:process";
import {
  loadEntries,
  quarantineId,
  validEntries,
  quarantineReason,
} from "../../../scripts/lib/quarantine.mjs";

export const BUILD_HINT =
  "Build it with `cargo test --manifest-path src-tauri/Cargo.toml --lib --no-run`, " +
  "or point CODESENSE_FRAME_TEST_BINARY at the compiled codesense_lib test binary.";

// Returns { path } for the Rust viewer test binary, or { error } saying why it is missing.
export function resolveFrameFixtureBinary({ env = process.env } = {}) {
  const deps = resolve(env.CARGO_TARGET_DIR ?? "target", "debug/deps");
  const override = env.CODESENSE_FRAME_TEST_BINARY;
  if (override) {
    const path = resolve(deps, override);
    if (existsSync(path)) return { path };
    return {
      error: `Frame fixture binary not found: CODESENSE_FRAME_TEST_BINARY=${path}. ${BUILD_HINT}`,
    };
  }
  let names = [];
  try {
    names = readdirSync(deps);
  } catch {
    /* A missing deps directory is the same as a missing binary. */
  }
  const name = names.find((n) => /^codesense_lib-[a-f0-9]+$/.test(n));
  if (name) return { path: resolve(deps, name) };
  return { error: `Frame fixture binary not found in ${deps}. ${BUILD_HINT}` };
}

const quarantines = loadEntries();

// Only an explicit fixme from the quarantine adapter may bypass the skip guard.
// Setup failures and missing fixtures never acquire that annotation.
export function unexecutedTests(tests, entries = quarantines, now = Date.now()) {
  const valid = validEntries(entries, now);
  return tests.filter((t) => {
    if (t.outcome() !== "skipped") return false;
    if (!t.location?.file) return true;
    const id = quarantineId("playwright", t.location.file, t.titlePath().slice(3));
    const entry = valid.find((entry) => entry.id === id);
    if (!entry) return true;
    const reason = quarantineReason(entry);
    return !t.annotations?.some((a) => a.type === "fixme" && a.description === reason);
  });
}

// Playwright reporter: unlisted skips still fail a security run.
export default class RequireEveryTestRan {
  onBegin(_config, suite) {
    this.suite = suite;
  }
  // Only a run that would otherwise pass is turned red; interrupted and timedout keep their status.
  onEnd(result) {
    if (result.status !== "passed") return;
    const missed = unexecutedTests(this.suite?.allTests() ?? []);
    if (missed.length === 0) return;
    console.error(`\n${missed.length} frame-security test(s) did not run; failing the run:`);
    for (const t of missed) console.error(`  - ${t.titlePath().filter(Boolean).join(" › ")}`);
    return { status: "failed" };
  }
  printsToStdio() {
    return false;
  }
}
