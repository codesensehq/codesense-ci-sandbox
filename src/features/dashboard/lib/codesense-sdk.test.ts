// SPDX-License-Identifier: MIT
/**
 * `/kit/v1/codesense.d.ts` against the shim a live doc really gets: every member of
 * `window.codesense` is declared, and every declared member exists.
 */
import { join } from "node:path";

import ts from "typescript";
import { describe, expect, it } from "vitest";

import { loadShim } from "../../../../tests/support/page-shim";
import { PAGE_SDK_FEATURES } from "./page-sdk";
import { VERB_REGISTRY } from "./verb-registry.generated";

const DTS = join(process.cwd(), "src-tauri/src/http/routes/dashboard_kit/codesense.d.ts");

function declared(usage: string) {
  const options: ts.CompilerOptions = {
    strict: true,
    noEmit: true,
    lib: ["lib.es2022.d.ts", "lib.dom.d.ts"],
  };
  const host = ts.createCompilerHost(options);
  const usagePath = join(process.cwd(), "codesense-sdk-usage.ts");
  const getSourceFile = host.getSourceFile;
  host.getSourceFile = (path, ...args) =>
    path === usagePath
      ? ts.createSourceFile(path, usage, ts.ScriptTarget.Latest)
      : getSourceFile(path, ...args);
  const program = ts.createProgram([DTS, usagePath], options, host);
  const checker = program.getTypeChecker();
  const file = program.getSourceFile(DTS)!;
  const sdk = checker
    .getSymbolsInScope(file, ts.SymbolFlags.Namespace)
    .find((s) => s.name === "CodesenseSdk")!;
  const codesense = checker.getExportsOfModule(sdk).find((s) => s.name === "Codesense")!;
  const type = checker.getDeclaredTypeOfSymbol(codesense);
  const diagnostics = ts.getPreEmitDiagnostics(program);
  return {
    diagnostics: diagnostics
      .filter((d) => d.file?.fileName !== usagePath)
      .map((d) => ts.flattenDiagnosticMessageText(d.messageText, "\n")),
    usageDiagnostics: diagnostics
      .filter((d) => d.file?.fileName === usagePath)
      .map((d) => ts.flattenDiagnosticMessageText(d.messageText, "\n")),
    members: checker.getPropertiesOfType(type).map((p) => p.name),
    nestedMembers: Object.fromEntries(
      checker
        .getPropertiesOfType(type)
        .map((p) => [
          p.name,
          checker
            .getPropertiesOfType(
              checker.getNonNullableType(checker.getTypeOfSymbolAtLocation(p, file)),
            )
            .map((member) => member.name),
        ]),
    ),
  };
}

describe("codesense.d.ts", () => {
  const { diagnostics, usageDiagnostics, members, nestedMembers } = declared(`
    codesense.kit!.virtualList?.({
      container: document.createElement("div"),
      rows: [{ id: "a", label: "Agent", unread: 0 }],
      rowHeight: 24,
      layoutKey: row => row.label,
      render: () => document.createElement("button"),
      update: (el, row, index) => { el.textContent = row.unread + ":" + index; }
    });
    codesense.kit!.keyed(document.createElement("div"), [{id: "a", layout: 1, unread: 0}], {
      key: row => row.id,
      version: row => row.layout,
      render: () => document.createElement("button"),
      update: (el, row) => { el.textContent = String(row.unread); }
    });
    codesense.showPopover({
      kind: "agent-quick-send",
      anchorRect: { x: 0, y: 0, width: 100, height: 30 },
      row: { kind: "agent", sessionId: "session-1" },
    });
    const navKit = codesense.kit!;
    const statusMark: HTMLSpanElement = navKit.statusMark("question", { size: 6, still: true });
    const glyph: Element = navKit.navGlyph("changes");
    const fileKind: string = navKit.fileKindOf("dashboards/overview.htm");
    const stripped: string = navKit.stripPipelineExtension(navKit.stripViewerExtension("a.md"));
    const story = { role: "active" as const, mark: "question", ready: false };
    const shown: boolean = navKit.inLens("needs", story);
    const counts: number = navKit.countLenses([story]).done;
    const group: string = navKit.stageGroupOf(story);
    const order: number = navKit.compareNavNames("F-2", "F-10");
    const statusGlyph: CodesenseSdk.MenuGlyph = { kind: "statusMark", mark: "question", size: 8, still: true };
    codesense.showMenu({ builtinAction: "builtin:new-file", anchorRect: { x: 0, y: 0, width: 0, height: 0 } });
    codesense.showMenu({
      type: "native",
      nativeSurface: "nav-filter",
      row: { kind: "nav-filter" },
      anchorRect: { x: 0, y: 0, width: 0, height: 0 },
    });
    void [statusMark, glyph, fileKind, stripped, shown, counts, group, order, statusGlyph];
  `);
  const { codesense } = loadShim({ kit: true });
  const exposed = Object.getOwnPropertyNames(codesense);

  it("type-checks on its own", () => {
    expect(diagnostics).toEqual([]);
  });

  it("type-checks quick view and stable live row callbacks", () => {
    expect(usageDiagnostics).toEqual([]);
  });

  it("declares the native nav helpers the kit exposes", () => {
    const kit = codesense.kit as Record<string, unknown>;
    const helpers = [
      "navGlyph",
      "fileKindOf",
      "stripViewerExtension",
      "stripPipelineExtension",
      "inLens",
      "countLenses",
      "stageGroupOf",
      "compareNavNames",
    ];
    for (const name of helpers) {
      expect(typeof kit[name], name).toBe("function");
      expect(nestedMembers.kit, name).toContain(name);
    }
  });

  it("declares every member the shim exposes", () => {
    expect(exposed.filter((name) => !members.includes(name))).toEqual([]);
  });

  it("declares nothing the shim does not expose", () => {
    expect(members.filter((name) => !exposed.includes(name))).toEqual([]);
  });

  it("declares every method inside the shim's verb namespaces", () => {
    for (const name of ["commands", "records"]) {
      const namespace = codesense[name];
      if (!namespace || typeof namespace !== "object") continue;
      for (const [method, value] of Object.entries(namespace)) {
        if (typeof value === "function") {
          expect(nestedMembers[name], `${name}.${method}`).toContain(method);
        }
      }
    }
  });

  it("has a method for every registry verb", () => {
    for (const { name } of VERB_REGISTRY) {
      expect(members).toContain(name);
      expect(typeof codesense[name], name).toBe("function");
    }
  });

  it("names no verb as a feature, so a verb's has() follows the grant", () => {
    const verbs = new Set<string>(VERB_REGISTRY.map((verb) => verb.name));
    expect(PAGE_SDK_FEATURES.filter((name) => verbs.has(name))).toEqual([]);
  });
});
