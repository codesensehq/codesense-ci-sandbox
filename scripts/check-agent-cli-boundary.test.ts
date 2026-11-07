// SPDX-License-Identifier: MIT
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

// @ts-expect-error -- a plain Node ESM script without type declarations.
import { check, hits, testModules } from "./check-agent-cli-boundary.mjs";

const SCRIPT = join(__dirname, "check-agent-cli-boundary.mjs");

let root: string;
const write = (path: string, text: string) => {
  const full = join(root, "src-tauri", path);
  mkdirSync(dirname(full), { recursive: true });
  writeFileSync(full, text);
};
const run = () => spawnSync(process.execPath, [SCRIPT, "--root", root], { encoding: "utf8" });

beforeEach(() => {
  root = mkdtempSync(join(tmpdir(), "agent-cli-boundary-"));
});
afterEach(() => rmSync(root, { recursive: true, force: true }));

describe("check-agent-cli-boundary", () => {
  it("passes on the repository", () => {
    const result = spawnSync(process.execPath, [SCRIPT], { encoding: "utf8" });
    expect(result.stderr).toBe("");
    expect(result.status).toBe(0);
  });

  it("fails on core that branches on a CLI", () => {
    write(
      "src/core.rs",
      "pub fn f(cli: AgentCli) -> bool {\n    if cli == AgentCli::Claude {\n        return true;\n    }\n    false\n}\n",
    );
    const result = run();
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("src/core.rs:2: names AgentCli::Claude");
  });

  it("keeps OpenCode variants and its id inside the adapter", () => {
    write(
      "src/core.rs",
      'fn f(cli: AgentCli) { let _ = cli == AgentCli::OpenCode; let _ = "opencode"; }',
    );
    expect(
      check(join(root, "src-tauri"), []).violations.map((v: { path: string }) => v.path),
    ).toEqual(["src/core.rs", "src/core.rs"]);
    rmSync(join(root, "src-tauri/src/core.rs"));
    write(
      "src/agents/adapter/opencode.rs",
      'fn f(cli: AgentCli) { let _ = cli == AgentCli::OpenCode; let _ = "opencode"; }',
    );
    expect(check(join(root, "src-tauri"), []).violations).toEqual([]);
  });

  it("keeps the registered Hermes variant and id inside the adapter", () => {
    write(
      "src/core.rs",
      'fn f(cli: AgentCli) { let _ = cli == AgentCli::Hermes; let _ = "hermes"; }',
    );
    expect(check(join(root, "src-tauri"), []).violations).toHaveLength(2);
    rmSync(join(root, "src-tauri/src/core.rs"));
    write(
      "src/agents/adapter/hermes.rs",
      'fn f(cli: AgentCli) { let _ = cli == AgentCli::Hermes; let _ = "hermes"; }',
    );
    expect(check(join(root, "src-tauri"), []).violations).toEqual([]);
  });

  it("keeps OpenCode-named functions inside the adapter", () => {
    write("src/core.rs", "fn f() { let _ = crate::agents::adapter::opencode_plugin_url(); }\n");
    const found = check(join(root, "src-tauri"), []).violations;
    expect(found.map((v: { text: string }) => v.text)).toEqual(["opencode_plugin_url"]);
    // A test item, a comment and a string that mention one are not code that branches.
    write(
      "src/core.rs",
      '// opencode_plugin_url\nfn f() { let _ = "opencode_plugin_url"; }\n#[cfg(test)]\nfn opencode_scope() {}\n',
    );
    expect(check(join(root, "src-tauri"), []).violations).toEqual([]);
    write("src/agents/adapter/opencode.rs", "pub fn opencode_plugin_url() {}\n");
    expect(check(join(root, "src-tauri"), []).violations).toEqual([]);
  });

  it("fails on a CLI's name as a string literal and on a bare import", () => {
    write("src/a.rs", 'fn f(s: &str) -> bool { s == "codex" }\n');
    write("src/b.rs", "use codesense_pty_core::agent_cli::AgentCli::{self, Codex};\n");
    write("src/c.rs", "use AgentCli::*;\n");
    write("src/d.rs", 'const A: &[u8] = b"claude"; const B: &str = r#"codex"#;\n');
    write("src/e.rs", "use codesense_pty_core::agent_cli::AgentCli as Cli;\n");
    // A `;` inside the brackets of a test item's signature does not end the skip early.
    write(
      "src/f.rs",
      '#[cfg(test)]\nfn t(x: [u8; 3]) { let _ = "claude"; }\nconst C: &str = "codex";\n',
    );
    const { violations } = check(join(root, "src-tauri"), []);
    expect(violations.map((v: { path: string }) => v.path)).toEqual([
      "src/a.rs",
      "src/b.rs",
      "src/c.rs",
      "src/d.rs",
      "src/d.rs",
      "src/e.rs",
      "src/f.rs",
    ]);
  });

  it("allows the adapter, comments, larger strings and test code", () => {
    write(
      "crates/codesense-pty-core/src/agent_cli/claude.rs",
      'const B: &str = "claude"; fn f() -> AgentCli { AgentCli::Claude }\n',
    );
    write("src/agents/adapter/codex.rs", 'const B: &str = "codex";\n');
    write(
      "src/core.rs",
      [
        "// AgentCli::Claude in a comment",
        '/* "claude" in a block /* nested */ comment */',
        'const LABEL: &str = "Claude Code"; const CMD: &str = "claude --help";',
        "const C: char = '\"'; fn lifetimes<'a>(x: &'a str) -> &'a str { x }",
        'const RAW: &str = r#"say "hi""#;',
        "#[cfg(test)]",
        'mod tests { fn t() { let _ = ("claude", AgentCli::Codex); } }',
        "#[cfg(all(unix, test))]",
        'fn unix_test() { let _ = "codex"; }',
        "#[cfg(test)]",
        "mod helpers;",
        "#[cfg(test)]",
        '#[path = "elsewhere_tests.rs"]',
        "mod elsewhere;",
        "",
      ].join("\n"),
    );
    write("src/core/helpers.rs", 'const X: &str = "claude";\n');
    write(
      "src/generic.rs",
      '#[cfg(test)]\nfn f<A, B>() -> &\'static str { "claude" }\n#[cfg(all(test, target_os = "linux"))]\nimpl<A, B> T for X<A, B> { fn g() -> bool { AgentCli::Codex == x } }\n',
    );
    write("src/whole.rs", '#![cfg(test)]\nconst X: &str = "claude";\n');
    write("src/elsewhere_tests.rs", 'include!("included.rs");\n');
    write("src/included.rs", 'const X: &str = "codex";\n');
    write("crates/codesense-session-server/tests/live.rs", 'const X: &str = "codex";\n');
    expect(check(join(root, "src-tauri"), []).violations).toEqual([]);
  });

  it("holds an allowlisted file to its count, and flags a count that can be lowered", () => {
    write("src/login.rs", 'const A: &str = "claude"; const B: &str = "codex";\n');
    const tauri = join(root, "src-tauri");
    expect(check(tauri, [{ path: "src/login.rs", max: 2 }]).violations).toEqual([]);
    expect(check(tauri, [{ path: "src/login.rs", max: 1 }]).violations).toHaveLength(2);
    expect(check(tauri, [{ path: "src/login.rs", max: 3 }]).stale).toHaveLength(1);
  });

  it("reports lines and finds out-of-line test modules", () => {
    expect(hits('\n// x\nfn f() {}\n/* y */ const A: &str = "claude";\n')).toEqual([
      { line: 4, text: '"claude"' },
    ]);
    expect(
      testModules(
        '#[cfg(test)]\n#[path = "t.rs"]\npub(crate) mod t;\nmod live;\n#[cfg(all(test, unix))] mod u;',
      ),
    ).toEqual([
      { name: "t", path: "t.rs" },
      { name: "u", path: undefined },
    ]);
  });
  it("ends a gated field or arm at its own end, and scans code that is not test-only", () => {
    write(
      "src/g.rs",
      [
        "struct S {",
        "    #[cfg(test)]",
        "    a: u8,",
        "    b: u8,",
        "}",
        'fn after() -> &\'static str { "codex" }',
        "",
      ].join("\n"),
    );
    write("src/h.rs", '#[cfg(all(not(test), unix))]\nfn f() -> &\'static str { "claude" }\n');
    write("src/i.rs", '#[cfg(any(test, feature = "x"))]\nfn f() -> &\'static str { "claude" }\n');
    write("src/j.rs", 'mod inner {\n    #![cfg(test)]\n}\nconst X: &str = "codex";\n');
    // A comparison or shift in a gated item is no generic list.
    write(
      "src/o.rs",
      '#[cfg(test)]\nconst OK: bool = 1 < 2;\nconst M: u32 = 1 << 4;\nfn real() -> &\'static str { "claude" }\n',
    );
    write(
      "src/p.rs",
      "fn f(n: u8, cli: AgentCli) -> u8 {\n    match n {\n        #[cfg(test)]\n        n if n < 3 => 0,\n        _ if cli == AgentCli::Claude => 1,\n        _ => 2,\n    }\n}\n",
    );
    write("src/k.rs", "type Cli = AgentCli;\n");
    write("src/l.rs", "use x::AgentCli::{self as A};\n");
    write("src/m.rs", "impl Tr for AgentCli { fn f(self) -> bool { self == Self::Claude } }\n");
    // A qualified path names a trait of the enum, not an alias.
    write("src/n.rs", "fn p(s: &str) { let _ = <AgentCli as FromStr>::from_str(s); }\n");
    const { violations } = check(join(root, "src-tauri"), []);
    expect([...new Set(violations.map((v: { path: string }) => v.path))]).toEqual([
      "src/g.rs",
      "src/h.rs",
      "src/i.rs",
      "src/j.rs",
      "src/k.rs",
      "src/l.rs",
      "src/m.rs",
      "src/o.rs",
      "src/p.rs",
    ]);
  });

  it("refuses a --root with no directory, and an unknown argument", () => {
    for (const args of [["--root"], ["--root", "--list"], ["--root=x"]]) {
      const result = spawnSync(process.execPath, [SCRIPT, ...args], { encoding: "utf8" });
      expect(result.status).toBe(2);
      expect(result.stderr).toContain("usage:");
    }
  });
});
