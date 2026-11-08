#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Public-safe scanner. Private rules are deliberately unavailable in this entry point.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { scan, loadAllow, where } from "./privacy-engine.mjs";
export { scan, loadAllow } from "./privacy-engine.mjs";
export const DEFAULT_ALLOW = fileURLToPath(new URL("./privacy-scan.allow", import.meta.url));

// Redact by default: even a filename can contain private data. --show is local opt-in.
export function report(findings, { show = false } = {}) {
  if (show) {
    for (const finding of findings.filter((f) => f.sev === "error" && !f.allowed)) {
      console.log(
        `ERROR ${finding.rule} ${where(finding)}${finding.shown ? " " + finding.shown : ""}`,
      );
    }
  }
  const errors = findings.filter((f) => f.sev === "error" && !f.allowed).length;
  const warnings = findings.filter((f) => f.sev === "warn" && !f.allowed).length;
  console.log(`privacy-scan: unresolved errors=${errors}, warnings=${warnings}`);
  return errors === 0;
}

if (
  process.argv[1] &&
  fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))
) {
  try {
    const raw = process.argv.slice(2);
    const args = raw.filter((arg) => arg !== "--show");
    const show = raw.includes("--show");
    if (args.length !== 1 || raw.length > 2) throw new Error();
    process.exitCode = report(
      scan(path.resolve(args[0]), loadAllow(DEFAULT_ALLOW), {
        paths: true,
        skipRoot: [".git"],
      }),
      { show },
    )
      ? 0
      : 1;
  } catch {
    console.error("privacy-scan: unable to complete scan");
    process.exitCode = 1;
  }
}
