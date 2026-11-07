// SPDX-License-Identifier: MIT
// The terminal architecture guard, enforced: the frontend scan
// (scripts/terminal-arch-guard.ts) and the Rust scan
// (`cargo test -p codesense-pty-core --test architecture_guard`). Exits
// non-zero when either half fails. Run by `npm run lint` and by the
// `quality:real` preflight.
//
//   node scripts/check-terminal-arch.mjs [--root <dir>] [--skip-rust]
//
// `--root` points both halves at another tree (the guard's own tests use it on
// a violating fixture); `--skip-rust` runs the frontend half only.
import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const rootAt = args.indexOf("--root");
const root = rootAt >= 0 ? resolve(args[rootAt + 1]) : repo;
const skipRust = args.includes("--skip-rust");

const run = (command, commandArgs, options) =>
  spawnSync(command, commandArgs, { stdio: "inherit", ...options }).status ?? 1;

let failed = false;
if (
  run(process.execPath, ["--import", "tsx", join(repo, "scripts/terminal-arch-guard.ts"), root], {
    cwd: repo,
  }) !== 0
) {
  failed = true;
}
if (!skipRust) {
  const env = { ...process.env };
  if (root !== repo) env.TERMINAL_ARCH_GUARD_ROOT = join(root, "src-tauri");
  if (
    run("cargo", ["test", "-q", "-p", "codesense-pty-core", "--test", "architecture_guard"], {
      cwd: join(repo, "src-tauri"),
      env,
    }) !== 0
  ) {
    failed = true;
  }
}
if (failed) {
  console.error("terminal-arch: FAILED — see docs in scripts/terminal-arch-guard.ts");
  process.exit(1);
}
