// SPDX-License-Identifier: MIT
import { exportedCatalog as readCatalog } from "./mcp-reference-fixture";
import { describe, expect, it, vi } from "vitest";
vi.mock("node:child_process", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:child_process")>();
  const spawnSync = vi.fn(() => {
    throw new Error("Node checks must not invoke Cargo or mq");
  });
  return { ...actual, spawnSync, default: { ...actual, spawnSync } };
});
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { buildManifest, META, validateInputReferences } from "./generate-mcp-reference.mjs";
import { REFRESH_COMMAND } from "./lib/sealed-export.mjs";
const GENERATED = "codesense-docs/getting-started/content/generated";
const SEAL_PATH = `${GENERATED}/mcp-tools.inputs.json`;
const PAYLOAD_PATHS = ["default", "dashboard-publish"].map(
  (p) => `${GENERATED}/mcp-tools.${p}.json`,
);
const reference = () =>
  JSON.parse(
    readFileSync(
      "codesense-docs/getting-started/content/generated/mcp-tools.dashboard-publish.json",
      "utf8",
    ),
  );
describe("served MCP reference", () => {
  it("has real contracts, long descriptions and explicit absent output schemas", () => {
    const doc = buildManifest(undefined, META, readCatalog());
    for (const tool of reference().tools) {
      const entry = doc.tools.find((t: { name: string }) => t.name === tool.name)!;
      expect(entry.params, tool.name).toEqual(tool.inputSchema);
      expect(entry.returns, tool.name).toEqual(tool.outputSchema ?? null);
      expect(entry.description, tool.name).toEqual(tool.description);
    }
    expect(
      doc.tools.find((t: { name: string }) => t.name === "open_workspace")!.params.properties
        .story_id,
    ).toBeDefined();
    expect(
      doc.tools.find((t: { name: string }) => t.name === "codesense_overview")!.description.length,
    ).toBeGreaterThan(500);
    expect(doc.tools.find((t: { name: string }) => t.name === "publish_dashboard")!.feature).toBe(
      "dashboard-publish",
    );
  });
  it("rejects unclassified tools, missing examples and wrong example inputs", () => {
    const metadata = { ...META };
    delete metadata.role_save;
    expect(() => buildManifest(undefined, metadata, readCatalog())).toThrow(/role_save.*META/);
    const catalog = readCatalog();
    catalog.families[0].calls = [];
    expect(() => buildManifest(undefined, META, catalog)).toThrow(/example/);
    const invalid = readCatalog();
    invalid.families.find((f: { id: string }) => f.id === "discovery")!.calls[0].arguments = {
      query: 5,
    };
    expect(() => buildManifest(undefined, META, invalid)).toThrow(/invalid family example/);
  });
  it("compares explicit input markers without confusing nested inputs or outputs", () => {
    const tools = reference().tools;
    for (const [name, alias] of [
      ["open_workspace", "storyId"],
      ["open_workspace", "initialPrompt"],
      ["list_sessions", "launchedBy"],
      ["send_message", "sessionId"],
      ["who_am_i", "sessionId"],
    ]) {
      const tool = tools.find((t: { name: string }) => t.name === name);
      expect(() =>
        validateInputReferences({ ...tool, description: `Args: {${alias}}. Returns {sessionId}` }),
      ).toThrow(/schema field/);
    }
    for (const name of ["signal_done", "list_sessions", "who_am_i"])
      expect(() =>
        validateInputReferences(tools.find((t: { name: string }) => t.name === name)),
      ).not.toThrow();
    const open = tools.find((t: { name: string }) => t.name === "open_workspace");
    expect(() =>
      validateInputReferences({
        ...open,
        description: "Args: {story_id}. Returns {handle}\n\nLAUNCH AN AGENT: pass {initialPrompt}.",
      }),
    ).toThrow(/schema field/);
    const sessions = tools.find((t: { name: string }) => t.name === "list_sessions");
    expect(() =>
      validateInputReferences({
        ...sessions,
        description: "Returns {launchedBy}. Optional {launchedBy} filters sessions.",
      }),
    ).toThrow(/schema field/);
    expect(() =>
      validateInputReferences({
        ...open,
        description: "Args: {story_id}. Returns {storyId, sessionId}",
      }),
    ).not.toThrow();
  });
});

describe("Node-only freshness and resolving teaching links", () => {
  it("rejects missing pages and missing anchors", () => {
    for (const pointer of ["missing-page.html", "mcp-explorer.html#missing-anchor"]) {
      const catalog = readCatalog();
      catalog.families[0].docs = pointer;
      expect(() => buildManifest(undefined, META, catalog)).toThrow(/unresolved Docs/);
    }
  });
  it("rejects source or payload drift without invoking the compiler or queue", async () => {
    const { mkdtempSync, mkdirSync, copyFileSync, readFileSync, writeFileSync, rmSync } =
      await import("node:fs");
    const { tmpdir } = await import("node:os");
    const { dirname, join } = await import("node:path");
    const { ROOT, CONTENT, CATALOG, OUTPUT, sealReference, generateReference } =
      await import("./generate-mcp-reference.mjs");
    const root = mkdtempSync(join(tmpdir(), "mcp-reference-check-"));
    const copy = (path: string) => {
      mkdirSync(dirname(join(root, path)), { recursive: true });
      copyFileSync(join(ROOT, path), join(root, path));
    };
    try {
      mkdirSync(join(root, "src-tauri/crates"), { recursive: true });
      for (const path of [
        "Cargo.lock",
        "src-tauri/Cargo.toml",
        "src-tauri/build.rs",
        "src-tauri/crates/codesense-pty-core/src/agent_cli/codex.rs",
        "src-tauri/crates/codesense-pty-core/src/agent_cli/claude.rs",
        "src-tauri/src/mcp/handler.rs",
        CATALOG,
        `${CONTENT}/generated/mcp-tools.default.json`,
        `${CONTENT}/generated/mcp-tools.dashboard-publish.json`,
      ])
        copy(path);
      for (const f of readCatalog().families) copy(`${CONTENT}/${f.docs.split("#")[0]}`);
      const nested = join(root, "src-tauri/src/nested.rs");
      writeFileSync(nested, "// JsonSchema nested DTO source\n");
      const unrelated = join(root, "src-tauri/src/unrelated.rs");
      writeFileSync(unrelated, "// unrelated backend source\n");
      writeFileSync(join(root, CATALOG), JSON.stringify(readCatalog(), null, 2) + "\n");
      sealReference(root);
      generateReference(root);
      // The tolerant gate only trusts a tree whose exports match main's.
      const git = (...args: string[]) => execFileSync("git", args, { cwd: root, stdio: "pipe" });
      git("init", "-q", "-b", "main");
      // Only the export files matter to the gate; adding just those keeps the setup cheap.
      git("add", "--", ...[SEAL_PATH, OUTPUT, ...PAYLOAD_PATHS]);
      git("-c", "user.email=t@example.com", "-c", "user.name=t", "commit", "-q", "-m", "sealed");
      git("update-ref", "refs/remotes/origin/main", "HEAD");
      expect(() => generateReference(root, { check: true })).not.toThrow();
      writeFileSync(unrelated, "// unrelated backend edit\n");
      expect(() => generateReference(root, { check: true })).not.toThrow();
      writeFileSync(nested, "// JsonSchema nested DTO changed\n");
      expect(() => generateReference(root, { check: true })).toThrow(
        /Stale MCP registry export.*changed inputs: src-tauri\/src\/nested\.rs/,
      );
      const outputPath = join(root, OUTPUT);
      const outputBeforeWarning = readFileSync(outputPath, "utf8");
      const stdout: string[] = [];
      const stderr: string[] = [];
      expect(
        generateReference(root, {
          buildScript: true,
          stdout: (line) => stdout.push(line),
          warn: (line) => stderr.push(line),
        }),
      ).toBeNull();
      expect(stdout.join("\n")).toContain("cargo:warning=MCP REFERENCE EXPORT NEEDED");
      expect(stdout.join("\n")).toContain("src-tauri/src/nested.rs");
      expect(stdout.join("\n")).toContain("node scripts/generate-mcp-reference.mjs --export");
      expect(() => generateReference(root, { check: true })).toThrow(REFRESH_COMMAND);
      expect(stdout.join("\n")).toContain("pnpm gen:docs");
      expect(stderr).toEqual([]);
      expect(readFileSync(outputPath, "utf8")).toBe(outputBeforeWarning);
      const notices: string[] = [];
      expect(
        generateReference(root, {
          check: true,
          allowStaleExport: true,
          warn: (line) => notices.push(line),
        }),
      ).not.toBeNull();
      expect(notices.join("\n")).toContain("NOTICE:");
      expect(notices.join("\n")).toContain("src-tauri/src/nested.rs");
      expect(() => generateReference(root, { check: true })).toThrow(/src-tauri\/src\/nested\.rs/);
      expect(() =>
        generateReference(root, { check: true, buildScript: true, warn: () => {} }),
      ).toThrow(/src-tauri\/src\/nested\.rs/);
      expect(() => generateReference(root)).toThrow(/src-tauri\/src\/nested\.rs/);
      expect(readFileSync(outputPath, "utf8")).toBe(outputBeforeWarning);
      writeFileSync(outputPath, `${outputBeforeWarning}\n`);
      expect(() =>
        generateReference(root, { check: true, allowStaleExport: true, warn: () => {} }),
      ).toThrow(/Stale MCP registry export/);
      writeFileSync(outputPath, outputBeforeWarning);
      writeFileSync(nested, "// JsonSchema nested DTO source\n");
      const source = join(root, "src-tauri/src/mcp/handler.rs");
      const before = readFileSync(source, "utf8");
      writeFileSync(source, before + "\n// changed input\n");
      expect(() => generateReference(root, { check: true })).toThrow(/Stale MCP registry export/);
      expect(
        generateReference(root, { check: true, allowStaleExport: true, warn: () => {} }),
      ).not.toBeNull();
      const tolerate = () =>
        generateReference(root, { check: true, allowStaleExport: true, warn: () => {} });
      const lines = before.split("\n");
      writeFileSync(source, `// prepended comment\n${before}`);
      expect(tolerate()).not.toBeNull();
      expect(() => generateReference(root, { check: true })).toThrow(/Stale MCP registry export/);
      lines.splice(Math.floor(lines.length / 2), 0, "// inserted mid-file");
      writeFileSync(source, lines.join("\n"));
      expect(tolerate()).not.toBeNull();
      writeFileSync(source, before);
      const manifest = readFileSync(outputPath, "utf8");
      for (const edit of ["description", "params"] as const) {
        const edited = JSON.parse(manifest);
        const tool = edited.tools.find((t: { name: string }) => t.name === "accept_role_message");
        if (edit === "description") tool.description += " edited";
        else tool.params = { ...tool.params, title: "edited" };
        writeFileSync(outputPath, JSON.stringify(edited, null, 2) + "\n");
        writeFileSync(source, `// prepended comment\n${before}`);
        // The edited manifest differs from main's, so the tolerant gate is strict.
        expect(tolerate, edit).toThrow(/Stale MCP registry export/);
      }
      writeFileSync(outputPath, manifest);
      writeFileSync(source, before);
      const payload = join(root, `${CONTENT}/generated/mcp-tools.default.json`);
      writeFileSync(payload, readFileSync(payload, "utf8") + "\n");
      expect(() => generateReference(root, { check: true })).toThrow(/Stale MCP registry export/);
      expect(() =>
        generateReference(root, { check: true, allowStaleExport: true, warn: () => {} }),
      ).toThrow(/changed payloads:/);
      rmSync(join(root, `${CONTENT}/generated/mcp-tools.inputs.json`));
      expect(() =>
        generateReference(root, { check: true, allowStaleExport: true, warn: () => {} }),
      ).toThrow(/no sealed export/);
      expect(readFileSync(join(root, OUTPUT), "utf8")).toContain("pane_write");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
