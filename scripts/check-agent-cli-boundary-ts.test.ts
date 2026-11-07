// SPDX-License-Identifier: MIT
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

// @ts-expect-error -- a plain Node ESM script without type declarations.
import { check, listLines } from "./check-agent-cli-boundary-ts.mjs";

const SCRIPT = join(__dirname, "check-agent-cli-boundary-ts.mjs");

let root: string;
const write = (path: string, text: string) => {
  const full = join(root, path);
  mkdirSync(dirname(full), { recursive: true });
  writeFileSync(full, text);
};

beforeEach(() => {
  root = mkdtempSync(join(tmpdir(), "agent-cli-boundary-ts-"));
});
afterEach(() => rmSync(root, { recursive: true, force: true }));

describe("check-agent-cli-boundary-ts", () => {
  it("passes on the repository", () => {
    const result = spawnSync(process.execPath, [SCRIPT], { encoding: "utf8" });
    expect(result.stderr).toBe("");
    expect(result.status).toBe(0);
  });

  it("fails on a literal CLI list outside the registry", () => {
    write("src/features/viewer/lib/foo.ts", 'const launches = ["shell", "claude", "codex"];\n');
    const { violations } = check(root, []);
    expect(violations).toEqual([
      { path: "src/features/viewer/lib/foo.ts", line: 1, named: ["claude", "codex"] },
    ]);
  });

  it("includes a registered but unlisted CLI in the boundary", () => {
    write("src/features/launch.ts", 'const runtimes = ["claude", "hermes"];');
    expect(check(root, []).violations).toHaveLength(1);
  });

  it("allows the registry, comments, per-CLI maps and test code", () => {
    write(
      "src/shared/lib/agent-runtimes.ts",
      'export const A = ["claude", "codex", "opencode"];\n',
    );
    write("src/features/x.test.ts", 'const a = ["claude", "codex"];\n');
    write("src/features/y.test.tsx", 'const b = ["claude", "codex", "opencode"];\n');
    write(
      "src/features/per-cli.ts",
      'const install = { claude: "curl …", codex: "npm i -g …" } as const;\n// "claude" "codex" in a comment\n',
    );
    expect(check(root, []).violations).toEqual([]);
  });

  it("holds an allowlisted file to its count", () => {
    write("src/shared/lib/injection.ts", 'const a = ["claude", "codex"];\n');
    expect(check(root, [{ path: "src/shared/lib/injection.ts", max: 1 }]).violations).toEqual([]);
    expect(check(root, [{ path: "src/shared/lib/injection.ts", max: 0 }]).violations).toHaveLength(
      1,
    );
    expect(check(root, [{ path: "src/shared/lib/other.ts", max: 1 }]).stale).toHaveLength(1);
  });

  it("listLines reports source lines and skips comments", () => {
    expect(listLines('const a = ["claude", "codex"];\n')).toEqual([
      { line: 1, named: ["claude", "codex"] },
    ]);
    expect(listLines('// "claude" "codex"\nconst a = 1;\n')).toEqual([]);
    expect(listLines('/* "claude" "codex" */\nconst a = 1;\n')).toEqual([]);
  });
});
