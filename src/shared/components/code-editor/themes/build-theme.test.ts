// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";

import type { CmModule } from "../cm-loader";
import { getCodeMirror, _resetCmLoaderForTests } from "../cm-loader";
import { buildCodesenseTheme } from "./build-theme";
import { lightTheme } from "./light";
import { darkTheme } from "./dark";
import { codeSyntaxSpec, buildCodesenseHighlightStyle } from "./highlight";
import { BUNDLED_THEMES, themeTokens } from "@/themes";
import { compositeOver, contrastRatio, CONTRAST_THRESHOLDS } from "@/shared/lib/color-contrast";

// Fake Lezer `tags`: any property is a distinct token; `.function(t)` is a
// modifier. Shared by the spy-cm and the codeSyntaxSpec test.
function makeTagsProxy(): CmModule["tags"] {
  return new Proxy(
    {},
    {
      get(_t, prop) {
        if (prop === "function") return (inner: unknown) => ({ mod: "function", inner });
        return { tag: String(prop) };
      },
    },
  ) as unknown as CmModule["tags"];
}

// A minimal spy CmModule that captures how the theme wires syntax highlighting.
// `defaultHighlightStyle` is a getter that flips a flag if anything reads it —
// so we can prove SLW-WDE-7 replaced the stock highlight (RD-1/AC-1).
function makeSpyCm(): {
  cm: CmModule;
  themeSpy: ReturnType<typeof vi.fn>;
  syntaxHighlightingSpy: ReturnType<typeof vi.fn>;
  defineSpy: ReturnType<typeof vi.fn>;
  wasDefaultHighlightRead: () => boolean;
} {
  const themeSpy = vi.fn((spec: unknown, opts: unknown) => ({ __theme: spec, opts }));
  const syntaxHighlightingSpy = vi.fn((style: unknown) => ({ __syntaxHighlighting: style }));
  const defineSpy = vi.fn((spec: unknown) => ({ __highlightStyle: spec }));

  let defaultRead = false;
  const tags = makeTagsProxy();

  const cm = {
    EditorView: { theme: themeSpy },
    syntaxHighlighting: syntaxHighlightingSpy,
    HighlightStyle: { define: defineSpy },
    tags,
  } as unknown as CmModule;
  Object.defineProperty(cm, "defaultHighlightStyle", {
    get() {
      defaultRead = true;
      return { __default: true };
    },
  });

  return {
    cm,
    themeSpy,
    syntaxHighlightingSpy,
    defineSpy,
    wasDefaultHighlightRead: () => defaultRead,
  };
}

describe("buildCodesenseTheme — custom highlight wiring (AC-1/RD-1)", () => {
  it("keeps every syntax role readable on the actual active line and gutter", () => {
    for (const id of Object.keys(BUNDLED_THEMES) as Array<keyof typeof BUNDLED_THEMES>)
      for (const mode of ["light", "dark"] as const) {
        const { cm, themeSpy } = makeSpyCm();
        buildCodesenseTheme(cm, mode === "dark");
        const spec = themeSpy.mock.calls[0][0] as Record<string, Record<string, string>>;
        const background = spec[".cm-activeLine"].backgroundColor;
        expect(spec[".cm-activeLineGutter"].backgroundColor).toBe(background);
        const variable = /^var\((--theme-[\w-]+)\)$/.exec(background)![1];
        const tokens = themeTokens(id, mode);
        const ground = compositeOver(tokens[variable], tokens["--theme-surface-viewer-bg"]);
        for (const [role, ink] of Object.entries(tokens).filter(([name]) =>
          name.startsWith("--theme-code-"),
        ))
          expect(contrastRatio(ink, ground), `${id}/${mode}/${role}`).toBeGreaterThanOrEqual(
            CONTRAST_THRESHOLDS.text,
          );
      }
  });
  it("wires the codesense HighlightStyle, never cm.defaultHighlightStyle", () => {
    const { cm, syntaxHighlightingSpy, defineSpy, wasDefaultHighlightRead } = makeSpyCm();

    buildCodesenseTheme(cm, false);

    // The custom HighlightStyle is built and fed to syntaxHighlighting…
    expect(defineSpy).toHaveBeenCalledTimes(1);
    expect(syntaxHighlightingSpy).toHaveBeenCalledTimes(1);
    expect(syntaxHighlightingSpy.mock.calls[0][0]).toBe(defineSpy.mock.results[0]!.value);
    // …and the stock default highlight is never touched.
    expect(wasDefaultHighlightRead()).toBe(false);
  });

  it("both editor + merge-view entry points (lightTheme/darkTheme) wire the same custom highlight", () => {
    for (const wrapper of [lightTheme, darkTheme]) {
      const { cm, syntaxHighlightingSpy, defineSpy, wasDefaultHighlightRead } = makeSpyCm();
      wrapper(cm);
      expect(defineSpy).toHaveBeenCalledTimes(1);
      expect(syntaxHighlightingSpy.mock.calls[0][0]).toBe(defineSpy.mock.results[0]!.value);
      expect(wasDefaultHighlightRead()).toBe(false);
    }
  });
});

describe("buildCodesenseTheme — merge diff decorations from status tokens (AC-3/RD-3)", () => {
  it("styles the @codemirror/merge classes from --cs-status-added/deleted", () => {
    const { cm, themeSpy } = makeSpyCm();
    buildCodesenseTheme(cm, false);

    const spec = themeSpy.mock.calls[0][0] as Record<string, Record<string, string>>;
    const keys = Object.keys(spec).join(" ");
    // Added/changed + deleted merge classes are present…
    expect(keys).toContain("cm-changedLine");
    expect(keys).toContain("cm-changedText");
    expect(keys).toContain("cm-deletedLine");
    expect(keys).toContain("cm-deletedText");

    const serialized = JSON.stringify(spec);
    // …and reference the git-status tokens, not @codemirror/merge's stock colors.
    expect(serialized).toContain("--cs-status-added");
    expect(serialized).toContain("--cs-status-deleted");
  });

  it("keeps syntax colors legible on diff tints: changed lines and words pull each token toward the foreground", () => {
    const { cm, themeSpy } = makeSpyCm();
    buildCodesenseTheme(cm, false);
    const spec = themeSpy.mock.calls[0][0] as Record<string, Record<string, string>>;
    const scopeOf = (cls: string) =>
      Object.entries(spec).find(
        ([sel, rules]) => sel.includes(cls) && "--cs-diff-code-keyword" in rules,
      )?.[1];
    const line = scopeOf("cm-changedLine");
    const word = scopeOf("cm-changedText");
    expect(line?.["--cs-diff-code-comment"]).toMatch(
      /color-mix\(in oklab, var\(--cs-code-comment\) \d+%, var\(--foreground\)\)/,
    );
    expect(word?.["--cs-diff-code-comment"]).toMatch(/var\(--foreground\)/);
    // The word scope mixes harder than the line (its tint is stronger).
    const pct = (v?: string) => Number(/(\d+)%/.exec(v ?? "")?.[1]);
    expect(pct(word?.["--cs-diff-code-keyword"])).toBeLessThan(
      pct(line?.["--cs-diff-code-keyword"]),
    );
    expect(scopeOf("cm-deletedText")).toBeDefined();
    expect(scopeOf("cm-deletedChunk")).toBeDefined();
  });

  it("split view: each side's own selectors carry the direction colour (matching the library's side-scoped rules)", () => {
    const { cm, themeSpy } = makeSpyCm();
    buildCodesenseTheme(cm, false);
    const spec = themeSpy.mock.calls[0][0] as Record<string, Record<string, string>>;
    const ruleFor = (part: string) =>
      Object.entries(spec).find(([sel]) => sel.split(",").some((p) => p.trim() === part))?.[1];
    const bg = (r?: Record<string, string>) => r?.backgroundColor ?? r?.background ?? "";
    expect(bg(ruleFor("&.cm-merge-b .cm-changedLine"))).toContain("--cs-status-added");
    expect(bg(ruleFor("&.cm-merge-b.cm-merge-b .cm-changedLineGutter"))).toContain(
      "--cs-status-added",
    );
    expect(bg(ruleFor("&.cm-merge-b.cm-merge-b .cm-changedText"))).toContain("--cs-status-added");
    expect(bg(ruleFor("&.cm-merge-a .cm-changedLine"))).toContain("--cs-status-deleted");
    expect(bg(ruleFor("&.cm-merge-a.cm-merge-a .cm-changedText"))).toContain("--cs-status-deleted");
    expect(bg(ruleFor("&.cm-merge-a .cm-changedLineGutter"))).toContain("--cs-status-deleted");
  });

  it("styles the shell-polish states from app tokens (AC-4)", () => {
    const { cm, themeSpy } = makeSpyCm();
    buildCodesenseTheme(cm, false);
    const keys = Object.keys(themeSpy.mock.calls[0][0] as object).join(" ");
    expect(keys).toContain("cm-matchingBracket");
    expect(keys).toContain("cm-searchMatch");
    expect(keys).toContain("cm-selectionMatch");
  });
});

describe("codeSyntaxSpec — palette maps to CSS custom properties (AC-1/AC-2)", () => {
  it("maps every tag rule to a var(--cs-code-*) (or --destructive for invalid)", () => {
    const spec = codeSyntaxSpec(makeTagsProxy());
    expect(spec.length).toBeGreaterThanOrEqual(12);
    for (const rule of spec) {
      expect(rule.color).toMatch(
        /^var\(--cs-diff-code-([a-z]+), var\(--cs-code-\1\)\)$|^var\(--destructive/,
      );
    }
    // The common token families are all covered.
    const colors = spec.map((r) => r.color).join(" ");
    for (const token of [
      "keyword",
      "string",
      "comment",
      "number",
      "function",
      "variable",
      "type",
      "operator",
      "punctuation",
    ]) {
      expect(colors).toContain(`--cs-code-${token}`);
    }
  });
});

describe("buildCodesenseHighlightStyle — real Lezer tags are all valid", () => {
  it("builds without throwing against the real @lezer/highlight tags", async () => {
    _resetCmLoaderForTests();
    const cm = await getCodeMirror();
    // Every tag referenced in codeSyntaxSpec must exist on the real tags object,
    // else HighlightStyle.define throws — this is the guard for that.
    expect(() => buildCodesenseHighlightStyle(cm)).not.toThrow();
  });
});
