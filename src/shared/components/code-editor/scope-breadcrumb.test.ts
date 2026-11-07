// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";
import { markdown } from "@codemirror/lang-markdown";
import { json } from "@codemirror/lang-json";
import type { Tree } from "@lezer/common";

import {
  breadcrumbModeFor,
  resolveScopeChain,
  type ScopeTree,
  type BreadcrumbMode,
} from "./scope-breadcrumb";

/** Parse `code` with a language's Lezer parser and adapt its Tree to `ScopeTree`. */
function parse(
  code: string,
  lang: { language: { parser: { parse(input: string): Tree } } },
): {
  tree: ScopeTree;
  slice: (from: number, to: number) => string;
} {
  const tree = lang.language.parser.parse(code) as unknown as ScopeTree;
  return { tree, slice: (from, to) => code.slice(from, to) };
}

/** Resolve the scope chain at the first occurrence of `marker` in `code`. */
function scopeAt(
  code: string,
  marker: string,
  lang: { language: { parser: { parse(input: string): Tree } } },
  mode: BreadcrumbMode,
): string[] | null {
  const { tree, slice } = parse(code, lang);
  const pos = code.indexOf(marker);
  expect(pos).toBeGreaterThanOrEqual(0);
  return resolveScopeChain(tree, pos, slice, mode);
}

describe("breadcrumbModeFor", () => {
  it("maps parse-tree langs to code/markdown and flat/plain langs to none", () => {
    expect(breadcrumbModeFor("markdown")).toBe("markdown");
    expect(breadcrumbModeFor("typescript")).toBe("code");
    expect(breadcrumbModeFor("javascript")).toBe("code");
    expect(breadcrumbModeFor("python")).toBe("code");
    expect(breadcrumbModeFor("json")).toBe("code");
    expect(breadcrumbModeFor("yaml")).toBe("code");
    expect(breadcrumbModeFor("rust")).toBe("none");
    expect(breadcrumbModeFor("shell")).toBe("none");
    expect(breadcrumbModeFor("xml")).toBe("none");
    expect(breadcrumbModeFor("plain")).toBe("none");
  });
});

describe("resolveScopeChain — code (real grammars)", () => {
  it("JS: reports the enclosing class → method chain", () => {
    const code = `class Widget {\n  render() {\n    return TARGET;\n  }\n}\n`;
    const chain = scopeAt(code, "TARGET", javascript(), "code");
    expect(chain).toEqual(["class Widget", "method render"]);
  });

  it("JS: reports a top-level function", () => {
    const code = `function compute() {\n  return TARGET;\n}\n`;
    expect(scopeAt(code, "TARGET", javascript(), "code")).toEqual(["function compute"]);
  });

  it("TS: resolves through the TypeScript dialect too", () => {
    const code = `class Store {\n  get(): number {\n    return TARGET;\n  }\n}\n`;
    const chain = scopeAt(code, "TARGET", javascript({ typescript: true }), "code");
    expect(chain).toEqual(["class Store", "method get"]);
  });

  it("Python: reports class → function (def) chain", () => {
    const code = `class Model:\n    def train(self):\n        return TARGET\n`;
    const chain = scopeAt(code, "TARGET", python(), "code");
    expect(chain).toEqual(["class Model", "function train"]);
  });

  it("JSON: reports the enclosing key path", () => {
    const code = `{\n  "outer": {\n    "inner": TARGET\n  }\n}\n`;
    const chain = scopeAt(code, "TARGET", json(), "code");
    expect(chain).toEqual(["outer", "inner"]);
  });

  it("returns null at top level (no enclosing scope)", () => {
    const code = `const TARGET = 1;\n`;
    expect(scopeAt(code, "TARGET", javascript(), "code")).toBeNull();
  });
});

describe("resolveScopeChain — markdown (real grammar)", () => {
  it("reports the nearest heading above the position", () => {
    const code = `# Title\n\nintro\n\n## Section Two\n\nTARGET body\n`;
    expect(scopeAt(code, "TARGET", markdown(), "markdown")).toEqual(["Section Two"]);
  });

  it("returns null before the first heading", () => {
    const code = `TARGET before any heading\n\n# Later\n`;
    expect(scopeAt(code, "TARGET", markdown(), "markdown")).toBeNull();
  });

  it("finds a heading nested inside a blockquote (not only top-level)", () => {
    const code = `# Top\n\n> ## Quoted Section\n>\n> TARGET inside the quote\n`;
    expect(scopeAt(code, "TARGET", markdown(), "markdown")).toEqual(["Quoted Section"]);
  });

  it("resolves a Setext heading to just its text (not the underline)", () => {
    const code = `Section Title\n=============\n\nTARGET body\n`;
    expect(scopeAt(code, "TARGET", markdown(), "markdown")).toEqual(["Section Title"]);
  });
});

describe("resolveScopeChain — none mode", () => {
  it("returns null regardless of tree (flat-tree / plain langs)", () => {
    const { tree, slice } = parse("fn main() {}\n", javascript());
    expect(resolveScopeChain(tree, 5, slice, "none")).toBeNull();
  });
});
