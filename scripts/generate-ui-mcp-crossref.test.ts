// SPDX-License-Identifier: MIT
// SW-ODC-24: unit coverage for the UI ↔ MCP crossref generator. Drives the
// pure helpers (parsers + pairing) directly from in-memory fixtures so we
// don't depend on the committed `src-tauri/*.rs` layout changing.

import { describe, expect, it } from "vitest";

// @ts-expect-error -- plain Node ESM build script, no type declarations.
import {
  buildCrossref,
  crossrefVerdict,
  parseMcpTools,
  parseTauriCommands,
  parseTauriWrappers,
  parseYamlMapFile,
  parseYamlPairingFile,
} from "./generate-ui-mcp-crossref.mjs";

const sampleLibRs = [
  "pub fn run() {",
  "    tauri::Builder::default()",
  "        .invoke_handler(tauri::generate_handler![",
  "            commands::foo::alpha,",
  "            // comment line",
  "            commands::foo::beta,",
  "            // SW-X: platform-gated below",
  '            #[cfg(target_os = "macos")]',
  "            commands::mac::gamma,",
  "            renderer_watchdog::renderer_heartbeat,",
  "        ])",
  "        .run();",
  "}",
].join("\n");

// Note: the parser's `^    #\[tool` regex requires exactly four-space indent
// to match the real `#[tool_router] impl Handler { … }` layout. Tests respect it.
const sampleHandlerRs = [
  "#[tool_router]",
  "impl Handler {",
  "    #[tool(",
  '        description = "Alpha mirror."',
  "    )]",
  "    async fn alpha(&self) -> Result<CallToolResult, ErrorData> { todo!() }",
  "",
  '    #[tool(description = "Delta — MCP-only.")]',
  "    async fn delta(&self) -> Result<CallToolResult, ErrorData> { todo!() }",
  "",
  '    #[tool(description = "A sync fn tool.")]',
  "    fn health_ping(&self) -> Result<CallToolResult, ErrorData> { todo!() }",
  "}",
  "",
].join("\n");

const sampleCommandsTs = `
export const alpha = () => invoke<void>("alpha", {});
export function beta(x: number) { return invoke<string>("beta", { x }); }
// not a wrapper
const notUsed = "something_else";
`;

describe("parseTauriCommands", () => {
  it(
    "extracts every ident inside the invoke_handler block, strips comments, " +
      "and tolerates cfg attributes without inhaling them as a command",
    () => {
      const rows = parseTauriCommands(sampleLibRs);
      expect(rows.map((r: { name: string }) => r.name)).toEqual([
        "alpha",
        "beta",
        "gamma",
        "renderer_heartbeat",
      ]);
      expect(rows[0]).toMatchObject({
        name: "alpha",
        path: "commands::foo::alpha",
        file: "src-tauri/src/lib.rs",
      });
      expect(rows[3].path).toBe("renderer_watchdog::renderer_heartbeat");
    },
  );

  it("still finds the block when the handler is wrapped by the invoke-rate counter", () => {
    const wrapped = [
      "        .invoke_handler(shared::invoke_stats::counted(tauri::generate_handler![",
      "            commands::foo::alpha,",
      "            commands::foo::beta,",
      "        ]))",
      "        .run();",
    ].join("\n");
    expect(parseTauriCommands(wrapped).map((r: { name: string }) => r.name)).toEqual([
      "alpha",
      "beta",
    ]);
  });
});

describe("parseMcpTools", () => {
  it("records the fn ident of every #[tool(...)] block, including the sync-fn variant", () => {
    const rows = parseMcpTools(sampleHandlerRs);
    expect(rows.map((r: { name: string }) => r.name).sort()).toEqual([
      "alpha",
      "delta",
      "health_ping",
    ]);
    expect(rows[0].file).toBe("src-tauri/src/mcp/handler.rs");
    expect(rows[0].line).toBeGreaterThan(0);
  });
});

describe("parseTauriWrappers", () => {
  it('records every invoke<T>("name", …) call site, keeping the first occurrence', () => {
    const wrappers = parseTauriWrappers(sampleCommandsTs);
    expect([...wrappers.keys()].sort()).toEqual(["alpha", "beta"]);
    expect(wrappers.get("alpha")).toBeGreaterThan(0);
  });
});

describe("parseYamlPairingFile", () => {
  it("groups entries by top-level ui_only / mcp_only sections with rationales", () => {
    const yaml = [
      "ui_only:",
      "  - name: save_app_state",
      "    rationale: >-",
      "      pure UI housekeeping",
      "  - name: rebuild_menu",
      "    rationale: macOS native menu bar",
      "mcp_only:",
      "  - name: codesense_overview",
      "    rationale: progressive discovery",
    ].join("\n");
    const parsed = parseYamlPairingFile(yaml);
    expect([...parsed.ui_only.keys()]).toEqual(["save_app_state", "rebuild_menu"]);
    expect(parsed.ui_only.get("save_app_state")).toBe("pure UI housekeeping");
    expect(parsed.mcp_only.get("codesense_overview")).toBe("progressive discovery");
  });
});

describe("parseYamlMapFile", () => {
  it("parses the `pairs:` list and drops incomplete rows", () => {
    const yaml = [
      "pairs:",
      "  - ui: foo_cmd",
      "    mcp: foo",
      "    note: Tauri kept the _cmd suffix",
      "  - ui: bar_only",
      "    # mcp missing — dropped",
      "  - ui: baz_cmd",
      "    mcp: baz",
    ].join("\n");
    const pairs = parseYamlMapFile(yaml);
    expect(pairs).toEqual([
      { ui: "foo_cmd", mcp: "foo", note: "Tauri kept the _cmd suffix" },
      { ui: "baz_cmd", mcp: "baz", note: null },
    ]);
  });
});

describe("buildCrossref — end-to-end from in-memory fixtures", () => {
  it(
    "pairs by exact name, honours the explicit map, and partitions residuals " +
      "with acknowledged/unacknowledged flags from the allowlist",
    () => {
      const mapYaml = ["pairs:", "  - ui: beta_cmd", "    mcp: beta"].join("\n");
      const allowYaml = [
        "ui_only:",
        "  - name: renderer_heartbeat",
        "    rationale: liveness ack",
        "mcp_only:",
        "  - name: codesense_overview",
        "    rationale: meta-tool",
      ].join("\n");

      const libRs = [
        "    .invoke_handler(tauri::generate_handler![",
        "        commands::a::alpha,",
        "        commands::b::beta_cmd,",
        "        commands::c::ui_lonely,",
        "        renderer_watchdog::renderer_heartbeat,",
        "    ])",
      ].join("\n");
      const handlerRs = [
        "impl Handler {",
        '    #[tool(description = "...")]',
        "    async fn alpha(&self) -> R { todo!() }",
        '    #[tool(description = "...")]',
        "    async fn beta(&self) -> R { todo!() }",
        '    #[tool(description = "...")]',
        "    async fn mcp_lonely(&self) -> R { todo!() }",
        '    #[tool(description = "...")]',
        "    async fn codesense_overview(&self) -> R { todo!() }",
        "}",
      ].join("\n");
      const commandsTs = `
        invoke<void>("alpha", {});
        invoke<void>("beta_cmd", {});
      `;

      const { doc, warnings } = buildCrossref({
        libRs,
        handlerRs,
        commandsTs,
        mapYaml,
        allowYaml,
      });

      expect(warnings).toEqual([]);
      expect(doc.counts).toMatchObject({
        paired: 2,
        uiOnly: 2,
        mcpOnly: 2,
        uiOnlyAcknowledged: 1,
        mcpOnlyAcknowledged: 1,
      });

      const pairedNames = doc.paired.map(
        (r: { name: string; via: string }) => `${r.name}:${r.via}`,
      );
      expect(pairedNames.sort()).toEqual(["alpha:exact", "beta_cmd:map"]);

      // Paired "alpha" also carries the TS wrapper annotation.
      const alpha = doc.paired.find((r: { name: string }) => r.name === "alpha");
      expect(alpha.ui.wrapperFile).toBe("src/tauri/commands.ts");

      // Acknowledged residuals have their rationale copied through.
      const uiHeartbeat = doc.uiOnly.find((r: { name: string }) => r.name === "renderer_heartbeat");
      expect(uiHeartbeat).toMatchObject({ acknowledged: true, rationale: "liveness ack" });

      const uiLonely = doc.uiOnly.find((r: { name: string }) => r.name === "ui_lonely");
      expect(uiLonely).toMatchObject({ acknowledged: false, rationale: null });

      const mcpOverview = doc.mcpOnly.find(
        (r: { name: string }) => r.name === "codesense_overview",
      );
      expect(mcpOverview).toMatchObject({ acknowledged: true, rationale: "meta-tool" });
    },
  );

  it("warns on an unresolved explicit map entry rather than silently dropping it", () => {
    const libRs = [
      "    .invoke_handler(tauri::generate_handler![",
      "        commands::a::alpha,",
      "    ])",
    ].join("\n");
    const handlerRs = [
      "impl Handler {",
      '    #[tool(description = "x")]',
      "    async fn alpha(&self) -> R { todo!() }",
      "}",
    ].join("\n");
    const { warnings } = buildCrossref({
      libRs,
      handlerRs,
      commandsTs: "",
      mapYaml: "pairs:\n  - ui: alpha\n    mcp: nonexistent_tool",
      allowYaml: "",
    });
    expect(warnings.join("\n")).toContain("does not resolve");
  });

  describe("PR gate tolerance for line-only drift", () => {
    const libRs = [
      "pub fn run() {",
      "    .invoke_handler(tauri::generate_handler![",
      "        commands::foo::alpha,",
      "        commands::foo::beta,",
      "    ])",
      "}",
    ].join("\n");
    const handlerRs = (pad: number) =>
      [
        "impl Handler {",
        ...Array(pad).fill("    // pad"),
        '    #[tool(description = "x")]',
        "    async fn alpha(&self) -> R { todo!() }",
        "}",
      ].join("\n");
    const render = (pad: number, lib = libRs) =>
      buildCrossref({
        libRs: lib,
        handlerRs: handlerRs(pad),
        commandsTs: 'invoke<void>("alpha", {});',
        mapYaml: "",
        allowYaml: "",
      }).doc;
    const text = (doc: unknown) => `${JSON.stringify(doc, null, 2)}\n`;

    it("line-only drift is a notice under the PR switch and stale when strict", () => {
      const committed = text(render(0));
      const fresh = render(3);
      expect(crossrefVerdict(committed, fresh, true)).toBe("notice");
      expect(crossrefVerdict(committed, fresh, false)).toBe("stale");
      expect(crossrefVerdict(text(fresh), fresh, false)).toBe("ok");
    });

    it("a renamed, removed or re-paired entry still fails the PR gate", () => {
      const committed = text(render(0));
      const renamed = libRs.replace("commands::foo::beta", "commands::foo::gamma");
      expect(crossrefVerdict(committed, render(3, renamed), true)).toBe("stale");
      const dropped = libRs.replace("        commands::foo::beta,\n", "");
      expect(crossrefVerdict(committed, render(3, dropped), true)).toBe("stale");
      const moved = libRs.replace("commands::foo::alpha", "commands::bar::alpha");
      expect(crossrefVerdict(committed, render(3, moved), true)).toBe("stale");
      expect(crossrefVerdict("not json", render(0), true)).toBe("stale");
    });
  });
});
