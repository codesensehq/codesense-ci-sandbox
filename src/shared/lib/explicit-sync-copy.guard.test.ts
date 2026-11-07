// SPDX-License-Identifier: MIT
/**
 * SW-XS-11: no automatic sync anywhere. Content reaches a workspace only when a
 * worktree is created, when an agent launch prepares it, or by an explicit MCP
 * call — so no user-facing string in the app may promise projection,
 * materialization, delivery to "every worktree", or keeping things "in sync".
 *
 * Scans every string literal, template literal and JSX text in `src/` (tests,
 * mocks and fixtures excluded). Strings without whitespace are identifiers
 * (event fields, test ids), not copy, and are skipped.
 */
import { readdirSync, readFileSync, statSync } from "fs";
import { join, relative, resolve } from "path";
import ts from "typescript";
import { describe, expect, it } from "vitest";

const ROOT = resolve(__dirname, "../../..");
const BANNED =
  /projected|materializ|propagat|every worktree|every workspace|fan(s|ned|ning)?[ -]?out|\bkeeps?\b[^.]{0,40}\bin sync|kept in sync/i;

function sourceFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) {
      return name === "__tests__" || name === "mock" || name === "fixtures"
        ? []
        : sourceFiles(path);
    }
    return /\.tsx?$/.test(name) && !/\.(test|spec)\.tsx?$/.test(name) && !name.endsWith(".d.ts")
      ? [path]
      : [];
  });
}

function copyStrings(path: string): Array<{ line: number; text: string }> {
  const source = ts.createSourceFile(
    path,
    readFileSync(path, "utf8"),
    ts.ScriptTarget.Latest,
    true,
    path.endsWith("x") ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
  );
  const found: Array<{ line: number; text: string }> = [];
  const visit = (node: ts.Node) => {
    let text: string | null = null;
    if (
      ts.isStringLiteral(node) ||
      ts.isNoSubstitutionTemplateLiteral(node) ||
      ts.isJsxText(node)
    ) {
      text = node.text;
    } else if (ts.isTemplateExpression(node)) {
      text = [node.head.text, ...node.templateSpans.map((s) => s.literal.text)].join(" ");
    }
    if (text !== null && /\s/.test(text.trim())) {
      found.push({ line: source.getLineAndCharacterOfPosition(node.getStart()).line + 1, text });
    }
    ts.forEachChild(node, visit);
  };
  visit(source);
  return found;
}

describe("app copy promises no automatic sync (SW-XS-11)", () => {
  it("no user-facing string says projected, materialize, every worktree or in sync", () => {
    const hits = sourceFiles(join(ROOT, "src")).flatMap((path) =>
      copyStrings(path)
        .filter(({ text }) => BANNED.test(text))
        .map(({ line, text }) => `${relative(ROOT, path)}:${line}: ${text.replace(/\s+/g, " ")}`),
    );
    expect(hits).toEqual([]);
  });

  it("the scan sees strings (it would catch a regression)", () => {
    const strings = copyStrings(join(ROOT, "src/shared/lib/onboarding-copy.ts"));
    expect(strings.some(({ text }) => text.includes("their definition declares"))).toBe(true);
    expect(BANNED.test("codesense keeps its context in sync")).toBe(true);
    expect(BANNED.test("propagate the deletion to every projected copy")).toBe(true);
    expect(BANNED.test("it fans out to every workspace")).toBe(true);
  });
});
