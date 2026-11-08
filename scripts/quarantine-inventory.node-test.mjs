// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { loadEntries } from "./lib/quarantine.mjs";
import { validateInventory, validateRustSources } from "./lib/quarantine-inventory.mjs";

const issue = "https://github.com/example/project/issues/1";
const entry = { id: "cargo:fixture_lib::tests::nested::red", issue };

function fixture(t, source = `#[test]\n#[ignore = "quarantine: ${issue}"]\nfn red() {}`) {
  const root = mkdtempSync(join(tmpdir(), "quarantine-inventory-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const put = (path, text) => {
    const target = join(root, path);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, text);
  };
  put(
    "src-tauri/Cargo.toml",
    '[package]\nname = "fixture"\nversion = "0.0.0"\n[lib]\nname = "fixture_lib"\n',
  );
  put("src-tauri/src/lib.rs", "#[cfg(test)]\nmod tests;");
  put("src-tauri/src/tests.rs", '#[path = "cases/nested.rs"]\nmod nested;');
  put("src-tauri/src/cases/nested.rs", source);
  return { root, put };
}

test("cargo quarantine attributes pair with module paths selected by path attributes", (t) => {
  const { root } = fixture(t);
  validateInventory([entry], root);
});

test("every cargo quarantine attribute needs the same issue in the ledger", (t) => {
  const { root } = fixture(t);
  assert.throws(() => validateInventory([], root), /attribute has no matching ledger issue/);
  assert.throws(
    () => validateInventory([{ ...entry, issue: `${issue}2` }], root),
    /attribute has no matching ledger issue/,
  );
});

test("every cargo ledger entry needs a quarantine attribute", (t) => {
  const { root } = fixture(t, "#[test]\nfn red() {}");
  assert.throws(
    () => validateInventory([entry], root),
    /ledger entry has no matching quarantine attribute/,
  );
});

test("inline modules add their real module name without parsing string fixtures as tests", (t) => {
  const { root } = fixture(
    t,
    `mod inline { #[test] #[ignore = "quarantine: ${issue}"] fn red() {} }
const FIXTURE: &str = r#"mod bogus { fn fake() {} }"#;`,
  );
  validateInventory([{ id: "cargo:fixture_lib::tests::nested::inline::red", issue }], root);
});

test("vitest and playwright ledger IDs must name existing files", (t) => {
  const { root, put } = fixture(t, "");
  for (const runner of ["vitest", "playwright"]) {
    const entry = { id: `${runner}:tests/example.test.ts#suite > example` };
    assert.throws(() => validateInventory([entry], root), /test file does not exist/);
    put("tests/example.test.ts", "");
    validateInventory([entry], root);
    rmSync(join(root, "tests/example.test.ts"));
  }
});

test("an orphan Rust quarantine cannot evade the inventory", (t) => {
  const { root, put } = fixture(t, "");
  put("src-tauri/src/orphan.rs", `#[ignore = "quarantine: ${issue}"] fn red() {}`);
  assert.throws(() => validateInventory([], root), /outside resolved Rust modules/);
});

test("binary-only crates use their package name with underscores", (t) => {
  const { root, put } = fixture(t, "");
  put(
    "src-tauri/crates/worker/Cargo.toml",
    '[package]\nname = "fixture-worker"\nversion = "0.0.0"\n',
  );
  put(
    "src-tauri/crates/worker/src/main.rs",
    `mod tests { #[test] #[ignore = "quarantine: ${issue}"] fn red() {} }`,
  );
  validateInventory([{ id: "cargo:fixture_worker::tests::red", issue }], root);
});

test("quarantine attributes reject raw strings and conditional wrappers", (t) => {
  for (const attr of [
    `#[ignore = r#"quarantine: ${issue}"#]`,
    `#[cfg_attr(test, ignore = "quarantine: ${issue}")]`,
    `#[cfg_attr(test, ignore = r#"quarantine: ${issue}"#)]`,
  ]) {
    const { root } = fixture(t, `#[test]\n${attr}\nfn red() {}`);
    assert.throws(() => validateInventory([entry], root), /must use canonical/);
  }
});

test("every bare ignore needs a reason even without a cargo ledger entry", (t) => {
  for (const attr of ["#[ignore]", "#[cfg_attr(test, ignore)]"]) {
    const { root } = fixture(t, `#[test]\n${attr}\nfn red() {}`);
    assert.throws(() => validateInventory([], root), /ignore attribute needs a reason/);
  }
});

test("reasoned non-quarantine ignores do not need ledger entries", (t) => {
  const { root } = fixture(
    t,
    `
#[test]
#[ignore = "manual performance measurement; run explicitly with --ignored"]
fn red() {}
#[test]
#[cfg_attr(windows, ignore = "requires a Unix PTY")]
fn platform_specific() {}
const EXAMPLE: &str = r#"#[ignore] #[ignore = "quarantine: example"]"#;
`,
  );
  validateInventory([], root);
});

test("a real attribution guard's constructor variable is not an opening delimiter", (t) => {
  const { root } = fixture(
    t,
    `
fn attribution_guard(tokens: &str) {
    for constructor in tokens.split("Command::new(").skip(1) {
        let argument = constructor.split(')').next().unwrap();
        assert!(!argument.contains("git-ai"));
    }
}
#[test]
#[ignore = "quarantine: ${issue}"]
fn red() {}
`,
  );
  validateInventory([entry], root);
});

test("inventory handles real host lifetimes alongside Rust literals and nested comments", (t) => {
  const { root } = fixture(
    t,
    String.raw`
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|error| error.into_inner())
}
type Deadline<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
fn borrowed(label: &'static str) -> &'static str { label }
fn literals() {
    let braces = ['{', '}', '\'', '\\', '\u{7b}', '\x7d', '🦀'];
    let byte_braces = [b'{', b'}', b'\x7b'];
    let text = "braces { and } and an escaped quote \"";
    let bytes = b"[ } \"";
    let raw = r##"{ ] \" # }"##;
    let raw_bytes = br#"{ ] \" }"#;
    /* outer { /* nested ] */ still a comment } */
}
#[test]
#[ignore = "quarantine: ` +
      issue +
      String.raw`"]
fn red() {}
`,
  );
  validateInventory([entry], root);
});

test("real ledger inventory resolves and every Rust source parses without throwing", () => {
  const root = fileURLToPath(new URL("../", import.meta.url));
  const entries = loadEntries();
  assert.doesNotThrow(() => validateInventory(entries, root));
  assert.doesNotThrow(() => validateRustSources(join(root, "src-tauri")));
});
