// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import ts from "typescript";
import { expect, it } from "vitest";
// @ts-expect-error -- the generator is a plain Node ESM script.
import { KIT_ICONS } from "../../../../scripts/gen-dashboard-kit-icons.mjs";

const panes = [
  "src/features/shell/slots/__fixtures__/agent-list-pane.html",
  "src/features/shell/slots/builtin/files-tree.html",
  ...["inbox", "notes", "parity"].map(
    (name) => `src/features/shell/right-pane/__fixtures__/${name}-pane.html`,
  ),
];

function missingKitIcons(html: string) {
  const names = new Set<string>();
  for (const meta of html.matchAll(
    /<meta\b[^>]*name=["']codesense:icon["'][^>]*content=["']([^"']+)["']/g,
  ))
    names.add(meta[1]);
  for (const script of html.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)) {
    const source = ts.createSourceFile(
      "pane.js",
      script[1],
      ts.ScriptTarget.Latest,
      true,
      ts.ScriptKind.JS,
    );
    // Inbox and Notes also have private SVG helpers; those names belong to their own catalogs.
    const kitHelper =
      /(?:function icon\([^)]*\)|const icon\s*=\s*\([^)]*\)\s*=>)\s*\{[^}]*\bkit\.icon\(/.test(
        script[1],
      );
    const literals = (node: ts.Node) => {
      if (ts.isStringLiteral(node)) names.add(node.text);
      else if (ts.isConditionalExpression(node)) {
        literals(node.whenTrue);
        literals(node.whenFalse);
      }
    };
    const visit = (node: ts.Node) => {
      if (ts.isCallExpression(node)) {
        const callee = node.expression.getText(source);
        if (
          callee === "kit.icon" ||
          callee === "kit.iconMarkup" ||
          callee === "codesense.kit.icon" ||
          callee === "codesense.kit.iconMarkup" ||
          (kitHelper && callee === "icon")
        ) {
          if (node.arguments[0]) literals(node.arguments[0]);
        }
        if ((callee === "kit.empty" || callee === "codesense.kit.empty") && node.arguments[1])
          literals(node.arguments[1]);
      }
      if (
        ts.isPropertyAssignment(node) &&
        node.name.getText(source).replace(/["']/g, "") === "icon"
      ) {
        if (kitHelper) literals(node.initializer);
      }
      ts.forEachChild(node, visit);
    };
    visit(source);
  }
  return [...names].filter((name) => !KIT_ICONS.includes(name));
}

it.each([
  ["function declaration", "function icon(name) { return kit.icon(name); }"],
  [
    "Files arrow wrapper",
    `const icon = (name, size = 14) => {
    const el = kit.icon(name, { size });
    el.classList.add("i");
    return el;
  };`,
  ],
])("rejects unknown icon literals through a %s", (_form, wrapper) => {
  expect(
    missingKitIcons(`<script>${wrapper}
    icon("search");
    icon("missing-wrapper-icon");
    icon(true ? "folder" : "missing-conditional-icon");
    const status = { icon: "missing-table-icon" };
  </script>`),
  ).toEqual(["missing-wrapper-icon", "missing-conditional-icon", "missing-table-icon"]);
});

for (const path of panes) {
  it(`${path} uses existing kit icons`, () => {
    expect(missingKitIcons(readFileSync(path, "utf8"))).toEqual([]);
  });
}
