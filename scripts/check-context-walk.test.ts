// SPDX-License-Identifier: MIT
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

// @ts-expect-error -- a plain Node ESM script without type declarations.
import { ALLOWLIST, check } from "./check-context-walk.mjs";

const SCRIPT = join(__dirname, "check-context-walk.mjs");

let root: string;
const write = (path: string, text: string) => {
  const full = join(root, path);
  mkdirSync(dirname(full), { recursive: true });
  writeFileSync(full, text);
};
const run = () => spawnSync(process.execPath, [SCRIPT, "--root", root], { encoding: "utf8" });
const violations = (allowlist: unknown[] = []) =>
  check(root, allowlist).violations as { path: string; line: number; text: string }[];

beforeEach(() => {
  root = mkdtempSync(join(tmpdir(), "context-walk-"));
  // A Rust tree the check can scan.
  write("src-tauri/src/lib.rs", "pub fn nothing() {}\n");
});
afterEach(() => rmSync(root, { recursive: true, force: true }));

describe("check-context-walk", () => {
  it("passes on the repository, with today's walkers allowlisted", () => {
    const result = spawnSync(process.execPath, [SCRIPT], { encoding: "utf8" });
    expect(result.stderr).toBe("");
    expect(result.status).toBe(0);
    expect(ALLOWLIST.every((e: { reason: string }) => e.reason.length > 0)).toBe(true);
  });

  it("fails on a new WalkDir::new outside the index", () => {
    write("src-tauri/src/feature.rs", 'fn f() {\n    let _ = walkdir::WalkDir::new("/x");\n}\n');
    const result = run();
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("src/feature.rs:2: walkdir::WalkDir::new");
  });

  it("fails on ignore::WalkBuilder and collect_viewable_files", () => {
    write(
      "src-tauri/src/feature.rs",
      'fn f() {\n    let _ = ignore::WalkBuilder::new("/x");\n    collect_viewable_files();\n}\n',
    );
    expect(violations().map((v) => v.text)).toEqual([
      "ignore::WalkBuilder::new",
      "collect_viewable_files",
    ]);
  });

  it("lets the index and the filesystem core walk", () => {
    write("src-tauri/src/context_query/index.rs", 'fn f() { walkdir::WalkDir::new("/x"); }\n');
    write(
      "src-tauri/crates/codesense-fs-core/src/walk.rs",
      'fn f() { walkdir::WalkDir::new("/x"); }\n',
    );
    expect(violations()).toEqual([]);
  });

  it("skips comments, strings and test code", () => {
    write(
      "src-tauri/src/feature.rs",
      [
        "// walkdir::WalkDir::new(root) was the old way",
        'const NOTE: &str = "WalkDir::new";',
        "#[cfg(test)]",
        "mod tests {",
        '    fn t() { walkdir::WalkDir::new("/x"); }',
        "}",
        "",
      ].join("\n"),
    );
    expect(violations()).toEqual([]);
  });

  it("allows an allowlisted walker up to its max and fails over it", () => {
    write(
      "src-tauri/src/feature.rs",
      'fn f() { walkdir::WalkDir::new("/a"); walkdir::WalkDir::new("/b"); }\n',
    );
    const entry = { path: "src/feature.rs", max: 2, reason: "test" };
    expect(check(root, [entry]).violations).toEqual([]);
    const over = check(root, [{ ...entry, max: 1 }]);
    expect(over.violations).toHaveLength(2);
    expect(over.over).toHaveLength(1);
  });

  it("fails when an allowlist entry is stale", () => {
    write("src-tauri/src/feature.rs", 'fn f() { walkdir::WalkDir::new("/a"); }\n');
    expect(check(root, [{ path: "src/feature.rs", max: 3, reason: "t" }]).stale).toHaveLength(1);
  });

  it("fails on any read_dir in commands/file_index.rs", () => {
    write(
      "src-tauri/src/commands/file_index.rs",
      "fn walk(d: &Path) { for _ in std::fs::read_dir(d).unwrap() {} }\n",
    );
    const found = violations();
    expect(found).toHaveLength(1);
    expect(found[0].path).toBe("src/commands/file_index.rs");
  });

  it("fails on a raw listContextFiles( outside commands.ts and the shared cache", () => {
    write(
      "src/features/thing/thing.tsx",
      'import { listContextFiles } from "@/tauri/commands";\nconst x = await listContextFiles(root);\n',
    );
    write(
      "src/tauri/commands.ts",
      "export const listContextFiles = (r: string) => invoke('list_context_files', { r });\n",
    );
    write(
      "src/shared/lib/pipeline-graph/content-scan-queue.ts",
      "const files = await listContextFiles(root);\n",
    );
    const found = violations();
    expect(found.map((v) => `${v.path}:${v.line}`)).toEqual(["src/features/thing/thing.tsx:2"]);
  });

  it("fails on a raw invoke of list_context_files, but not tests, e2e controls or comments", () => {
    write("src/a.ts", 'await invoke<ContextFile[]>("list_context_files", { rootPath });\n');
    write("src/b.test.ts", "await listContextFiles(root);\n");
    write("src/app/e2e-content-nav-control.ts", "await listContextFiles(root);\n");
    write("src/c.ts", "// listContextFiles(root) used to be called here\n");
    expect(violations().map((v) => v.path)).toEqual(["src/a.ts"]);
  });

  it("rejects unknown arguments", () => {
    const result = spawnSync(process.execPath, [SCRIPT, "--nope"], { encoding: "utf8" });
    expect(result.status).toBe(2);
  });
});
