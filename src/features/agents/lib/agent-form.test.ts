// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { AgentDefinition } from "@/tauri/commands";

import {
  MAX_CONTEXT_ENTRIES,
  checkContextDest,
  checkContextPath,
  changedFields,
  checkStartupPath,
  draftFromDefinition,
  editorSectionOf,
  emptyDraft,
  formFromDraft,
  isChanged,
  revisionDiff,
  startupConflict,
} from "./agent-form";
import { joinArgs, splitArgs } from "@/shared/lib/tokenize-args";
import ENTRY_CASES from "../../../../src-tauri/src/agents/testdata/context_entry_cases.json";

const DEF: AgentDefinition = {
  id: "triage",
  name: "Triage",
  created_by: "1",
  runtime: "claude",
  args: ["--model", "opus"],
  startup: ["CLAUDE.md", ".claude/skills/x"],
  permissions: {
    claude: { allow: ["Read"], additional_directories: ["../shared"] },
    codex: { sandbox_mode: "read-only" },
  },
  revision: 2,
};

describe("agent form", () => {
  it("round-trips a definition, keeping both runtimes' blocks and keys it does not show", () => {
    const draft = draftFromDefinition(DEF);
    const form = formFromDraft({ ...draft, runtime: "codex" }, { key: "k" });
    expect(form.args).toEqual(["--model", "opus"]);
    expect(form.permissions).toEqual(DEF.permissions);
    expect(isChanged(draft, draftFromDefinition(DEF))).toBe(false);
    expect(isChanged(draft, { ...draft, args: "--model  opus " })).toBe(false);
    expect(isChanged(draft, { ...draft, args: "--model sonnet" })).toBe(true);
    const cleared = formFromDraft(
      {
        ...draft,
        claude: { ...draft.claude, allow: [], rest: {} },
        codex: { ...draft.codex, sandboxMode: "" },
      },
      { placement: { scope: "personal" } },
    );
    expect(cleared.permissions).toBeNull();
    expect(cleared.scope).toBe("personal");
  });

  it("round-trips OpenCode permission rules without changing other CLI blocks", () => {
    const definition: AgentDefinition = {
      ...DEF,
      runtime: "opencode",
      permissions: {
        ...DEF.permissions,
        opencode: { permission: { "*": "ask", codesense_who_am_i: "allow" }, custom: true },
      },
    };
    const draft = draftFromDefinition(definition);
    expect(formFromDraft(draft, { key: "k" }).permissions).toEqual(definition.permissions);
    expect(isChanged(draft, draftFromDefinition(definition))).toBe(false);
    expect(
      formFromDraft(
        {
          ...draft,
          opencode: { ...draft.opencode, permission: [{ pattern: "*", decision: "deny" }] },
        },
        { key: "k" },
      ).permissions?.opencode?.permission,
    ).toEqual({ "*": "deny" });
  });

  it("rename preserves the Hermes permission block byte-for-byte across runtime switches", () => {
    const definition: AgentDefinition = {
      ...DEF,
      runtime: "hermes",
      permissions: {
        ...DEF.permissions,
        hermes: {
          deny: ["^write_file$", "^mcp__other__delete_.*"],
          approval: "ask",
          toolsets: ["terminal", "web"],
        },
      },
    };
    const before = JSON.stringify(definition.permissions?.hermes);
    const draft = draftFromDefinition(definition);
    for (const runtime of ["hermes", "claude", "codex", "opencode"] as const) {
      const form = formFromDraft({ ...draft, name: "Renamed Hermes", runtime }, { key: "k" });
      expect(form.name).toBe("Renamed Hermes");
      expect(JSON.stringify(form.permissions?.hermes)).toBe(before);
      expect(form.permissions?.claude).toEqual(definition.permissions?.claude);
      expect(form.permissions?.codex).toEqual(definition.permissions?.codex);
    }
    expect(isChanged(draft, draftFromDefinition(definition))).toBe(false);
    const empty = draftFromDefinition({ ...definition, permissions: { hermes: {} } });
    expect(formFromDraft({ ...empty, name: "Renamed empty" }, { key: "k" }).permissions).toEqual({
      hermes: {},
    });
  });

  it.each([undefined, null, [], ["who_am_i"]].map((bridgeTools) => ({ bridgeTools })))(
    "keeps bridge policy %j in both preview and save forms, including runtime changes",
    ({ bridgeTools }) => {
      const definition = { ...DEF, runtime: "codex" as const, mcp: { bridge_tools: bridgeTools } };
      const draft = draftFromDefinition(definition);
      expect(formFromDraft(draft, { key: "k" }).mcpBridgeTools).toEqual(bridgeTools ?? null);
      expect(formFromDraft({ ...draft, runtime: "claude" }, { key: "k" }).mcpBridgeTools).toEqual(
        bridgeTools ?? null,
      );
      expect(isChanged(draft, draftFromDefinition(definition))).toBe(false);
    },
  );

  it("SW-AGR-19 PR-6: carries resumeAfterRestart, off by default", () => {
    expect(draftFromDefinition(DEF).resumeAfterRestart).toBe(false);
    expect(formFromDraft(draftFromDefinition(DEF), { key: "k" }).resumeAfterRestart).toBe(false);
    const on = draftFromDefinition({ ...DEF, resumeAfterRestart: true });
    expect(on.resumeAfterRestart).toBe(true);
    expect(formFromDraft(on, { key: "k" }).resumeAfterRestart).toBe(true);
    expect(isChanged(draftFromDefinition(DEF), on)).toBe(true);
  });

  it("keeps an arg with a space in it, before and after the args are edited", () => {
    const draft = draftFromDefinition({ ...DEF, args: ["--append-system-prompt", "Be terse"] });
    expect(draft.args).toBe("--append-system-prompt 'Be terse'");
    expect(formFromDraft({ ...draft, name: "Other" }, { key: "k" }).args).toEqual([
      "--append-system-prompt",
      "Be terse",
    ]);
    expect(formFromDraft({ ...draft, args: `${draft.args} --verbose` }, { key: "k" }).args).toEqual(
      ["--append-system-prompt", "Be terse", "--verbose"],
    );
  });
});

describe("args line", () => {
  it.each([
    ["--model opus", ["--model", "opus"]],
    ["  --a   --b\t", ["--a", "--b"]],
    ['--x="a b" c', ["--x=a b", "c"]],
    ["--x='a b'", ["--x=a b"]],
    ["'it'\\''s'", ["it's"]],
    ['"say \\"hi\\""', ['say "hi"']],
    ['"a\\\\b" "c\\d"', ["a\\b", "c\\d"]],
    ["C:\\dir\\file --k=v=w", ["C:\\dir\\file", "--k=v=w"]],
    ["a\\ b", ["a b"]],
    ["'' x", ["", "x"]],
    ["'unclosed rest", ["unclosed rest"]],
    ["", []],
  ])("splits %j", (line, words) => {
    expect(splitArgs(line)).toEqual(words);
  });

  it("reads back exactly the words it joined, for any words", () => {
    // A seeded generator over the characters that matter to quoting, so every run checks the
    // same thousand argvs.
    let seed = 7;
    const next = () => {
      seed = (seed * 48271) % 2147483647;
      return seed;
    };
    const alphabet = ["a", "Z", "0", "-", "=", " ", "\t", "\n", "'", '"', "\\", "$", "*", "é", ","];
    for (let run = 0; run < 1000; run += 1) {
      const words = Array.from({ length: next() % 5 }, () =>
        Array.from({ length: next() % 7 }, () => alphabet[next() % alphabet.length]).join(""),
      );
      expect(splitArgs(joinArgs(words))).toEqual(words);
    }
  });
});

describe("agent form paths and history", () => {
  it("checks typed paths by the backend's rules and normalizes them", () => {
    expect(checkStartupPath("./docs//runbooks/")).toEqual({ path: "docs/runbooks" });
    for (const bad of ["/etc/passwd", "a/../b", "docs/*.md", "a\\b", "", ".git/config", "C:/x"]) {
      expect(checkStartupPath(bad)).toHaveProperty("error");
    }
    expect(checkStartupPath("docs/target/x.md")).toEqual({ path: "docs/target/x.md" });
    expect(startupConflict(["docs"], "docs/a.md")).toContain("overlaps docs");
    expect(startupConflict(["docs"], "docs")).toContain("already listed");
    expect(startupConflict(["docs"], "docsx")).toBeNull();
  });

  it("diffs revisions field by field, startup paths as added and removed", () => {
    const next: AgentDefinition = {
      ...DEF,
      startup: ["CLAUDE.md", "docs/guide.md"],
      permissions: { claude: { allow: ["Read"], deny: ["WebFetch"] } },
    };
    const changes = revisionDiff(DEF, next);
    expect(changes).toContainEqual({
      field: "Startup",
      before: "removed .claude/skills/x",
      after: "added docs/guide.md",
    });
    expect(changes).toContainEqual({
      field: "Claude permissions",
      before: "allow Read",
      after: "allow Read; deny WebFetch",
    });
    expect(changes.map((c) => c.field)).toContain("Codex permissions");
    expect(revisionDiff(DEF, DEF)).toEqual([]);
  });

  it("shows an env change in the revision diff, and a form save never sends env", () => {
    const withEnv: AgentDefinition = { ...DEF, env: { B: "2", A: "1" } };
    expect(revisionDiff(DEF, withEnv)).toContainEqual({
      field: "Env",
      before: "",
      after: "A=1, B=2",
    });
    expect(revisionDiff(withEnv, { ...withEnv, env: { A: "1", B: "2" } })).toEqual([]);
    const form = formFromDraft(draftFromDefinition(withEnv), { key: "k" });
    expect(form).not.toHaveProperty("env");
  });
});

// SW-XS-16: the Context section checks a typed entry by the rules a save applies
// (`agents/context.rs` normalize_entries), so a refusal shows before the round trip.
describe("context entry rules (match agents/context.rs)", () => {
  it("normalizes a context path as the backend does (NFC, ./ and trailing / dropped)", () => {
    expect(checkContextPath("./docs/architecture/")).toEqual({ path: "docs/architecture" });
    expect(checkContextPath("docs/cafe\u0301")).toEqual({ path: "docs/caf\u00e9" });
  });

  it("refuses what normalize_entries refuses, worded for a context path", () => {
    for (const raw of ["", ".", "/abs", "C:/x", "a/../b", "docs/*.md", "a\\b", "node_modules/x"]) {
      const checked = checkContextPath(raw);
      expect("error" in checked, raw).toBe(true);
      if ("error" in checked) expect(checked.error).toMatch(/^context path /);
    }
    // A VCS segment at any depth, in any case.
    expect(checkContextPath("docs/.GIT/config")).toEqual({
      error: 'context path "docs/.GIT/config": .GIT is version-control metadata',
    });
  });

  it("checks a dest: blank is no remap, an unsafe workspace path is refused", () => {
    expect(checkContextDest("")).toEqual({ dest: null });
    expect(checkContextDest("  ")).toEqual({ dest: null });
    expect(checkContextDest("plans/")).toEqual({ dest: "plans" });
    for (const raw of [
      "x/.svn/y",
      ".claude/settings.json",
      ".claude/settings.local.json",
      ".mcp.json",
      ".codex/config.toml/inner",
      "a/wvbuild-lock",
      "../up",
    ]) {
      const checked = checkContextDest(raw);
      expect("error" in checked, raw).toBe(true);
    }
  });

  it("caps a context list at the backend's limit", () => {
    expect(MAX_CONTEXT_ENTRIES).toBe(32);
  });
});

// The same cases `agents/context.rs` `entry_rules_match_the_shared_fixture` pins the server's
// `normalize_entries` to, so a typed entry the editor lets through is one a save accepts.
describe("context entry rules · shared fixture with agents/context.rs", () => {
  it.each(ENTRY_CASES as Array<{ path: string; dest?: string; ok: boolean }>)(
    "$path -> $dest: ok=$ok",
    ({ path, dest, ok }) => {
      const p = checkContextPath(path);
      const d = dest === undefined ? { dest: null } : checkContextDest(dest);
      expect(!("error" in p) && !("error" in d)).toBe(ok);
    },
  );
});

describe("changedFields", () => {
  const base = emptyDraft();

  it("is empty for an untouched draft", () => {
    expect(changedFields(base, { ...base })).toEqual([]);
  });

  it("names each edited field once, with the section it belongs to", () => {
    const draft = { ...base, name: "Mira", args: "--model opus", mcpServers: ["slack"] };
    expect(changedFields(base, draft)).toEqual(["name", "args", "mcpServers"]);
    expect(editorSectionOf("name")).toBe("basics");
    expect(editorSectionOf("args")).toBe("runs-on");
    expect(editorSectionOf("mcpServers")).toBe("tools");
    expect(editorSectionOf("startup")).toBe("starts-with");
    expect(editorSectionOf("claude")).toBe("permissions");
  });

  it("counts a permission block as one field however many rules changed", () => {
    const draft = { ...base, claude: { ...base.claude, allow: ["a", "b"], deny: ["c"] } };
    expect(changedFields(base, draft)).toEqual(["claude"]);
  });

  it("is empty again when an edit is undone", () => {
    expect(changedFields(base, { ...base, name: "x" })).toHaveLength(1);
    expect(changedFields(base, { ...base, name: "" })).toEqual([]);
  });
});
