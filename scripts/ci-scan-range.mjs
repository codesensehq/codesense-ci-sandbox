// SPDX-License-Identifier: MIT
// Shared, fail-closed change-range resolution for public CI scanners.
import fs from "node:fs";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export const revision = (value, label = "revision") => {
  if (!/^(?:[0-9a-f]{40}|[0-9a-f]{64})$/.test(value ?? ""))
    throw new Error(`scan-range: ${label} must be a full lowercase 40- or 64-character revision`);
  return value;
};
export const gitOptions = (cwd) => ({
  cwd,
  maxBuffer: 256 * 1024 * 1024,
  stdio: ["ignore", "pipe", "pipe"],
});
export const git = (args, cwd, failureMessage) => {
  try {
    return execFileSync("git", ["--literal-pathspecs", ...args], {
      ...gitOptions(cwd),
      encoding: "utf8",
    });
  } catch (error) {
    if (failureMessage) throw new Error(failureMessage);
    throw error;
  }
};

export function resolveRange({ cwd, base, head, defaultBranch }) {
  revision(head, "head");
  revision(base, "base");
  git(["rev-parse", "--verify", `${head}^{commit}`], cwd, "scan-range: head commit is unavailable");
  const zero = /^0+$/.test(base);
  if (!zero) {
    const ancestor = spawnSync(
      "git",
      ["--literal-pathspecs", "merge-base", "--is-ancestor", base, head],
      gitOptions(cwd),
    );
    if (ancestor.status === 0) return { base, head, allHistory: false };
  }
  // Initial/rewritten pushes have no usable before-to-after range. Resolve a
  // real common ancestor with the configured default branch, or fail closed.
  if (typeof defaultBranch !== "string" || !defaultBranch)
    throw new Error("scan-range: default branch is required for the fallback");
  const defaultRef = `refs/remotes/origin/${defaultBranch}`;
  git(["check-ref-format", defaultRef], cwd, "scan-range: default branch ref is invalid");
  git(
    ["rev-parse", "--verify", `${defaultRef}^{commit}`],
    cwd,
    "scan-range: default branch commit is unavailable",
  );
  const common = revision(
    git(
      ["merge-base", head, defaultRef],
      cwd,
      "scan-range: no common ancestor with the default branch",
    ).trim(),
    "merge base",
  );
  // On the default branch the fetched tip may already equal after. Inspect its
  // full reachable history rather than silently accepting an empty range.
  return { base: common, head, allHistory: common === head };
}

if (
  process.argv[1] &&
  fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))
) {
  try {
    const range = resolveRange({
      cwd: process.cwd(),
      base: process.env.SCAN_BASE,
      head: process.env.SCAN_HEAD,
      defaultBranch: process.env.DEFAULT_BRANCH,
    });
    console.log(range.allHistory ? range.head : `${range.base}..${range.head}`);
  } catch (err) {
    console.error(err.message);
    process.exitCode = 1;
  }
}
