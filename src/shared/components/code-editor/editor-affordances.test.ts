// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  resolvePerTypeTriState,
  resolveEditorFolding,
  DEFAULT_EDITOR_FOLDING,
  coerceTabSize,
  DEFAULT_EDITOR_TAB_SIZE,
  EDITOR_TAB_SIZES,
  resolveEditorFontFamily,
  MONO_FONT_STACK,
  PROSE_FONT_STACK,
  resolveEditorAffordances,
  type EditorAffordanceSettings,
} from "./editor-affordances";
import { resolveEditorLineWrap } from "./line-wrap";

describe("resolvePerTypeTriState", () => {
  it("forces on/off regardless of type", () => {
    for (const isCode of [true, false]) {
      for (const autoOn of ["code", "prose"] as const) {
        expect(resolvePerTypeTriState("on", isCode, autoOn)).toBe(true);
        expect(resolvePerTypeTriState("off", isCode, autoOn)).toBe(false);
      }
    }
  });

  it("auto derives per-type by polarity", () => {
    // autoOn:"code" → on for code, off for prose
    expect(resolvePerTypeTriState("auto", true, "code")).toBe(true);
    expect(resolvePerTypeTriState("auto", false, "code")).toBe(false);
    // autoOn:"prose" → on for prose, off for code
    expect(resolvePerTypeTriState("auto", true, "prose")).toBe(false);
    expect(resolvePerTypeTriState("auto", false, "prose")).toBe(true);
  });

  it("is TOTAL — an unexpected value falls back to the auto default", () => {
    // @ts-expect-error deliberately passing an invalid setting
    expect(resolvePerTypeTriState("garbage", true, "code")).toBe(true);
    // @ts-expect-error deliberately passing an invalid setting
    expect(resolvePerTypeTriState(undefined, false, "prose")).toBe(true);
  });
});

describe("resolveEditorLineWrap (delegates to the shared core, prose-auto)", () => {
  it("auto wraps prose, not code", () => {
    expect(resolveEditorLineWrap("auto", false)).toBe(true); // prose
    expect(resolveEditorLineWrap("auto", true)).toBe(false); // code
  });
  it("on/off force", () => {
    expect(resolveEditorLineWrap("on", true)).toBe(true);
    expect(resolveEditorLineWrap("off", false)).toBe(false);
  });
});

describe("resolveEditorFolding (code-auto polarity)", () => {
  it("default is auto", () => {
    expect(DEFAULT_EDITOR_FOLDING).toBe("auto");
  });
  it("auto folds code, not prose", () => {
    expect(resolveEditorFolding("auto", true)).toBe(true); // code
    expect(resolveEditorFolding("auto", false)).toBe(false); // prose
  });
  it("on/off force", () => {
    expect(resolveEditorFolding("on", false)).toBe(true);
    expect(resolveEditorFolding("off", true)).toBe(false);
  });
});

describe("coerceTabSize", () => {
  it("keeps valid sizes", () => {
    for (const n of EDITOR_TAB_SIZES) expect(coerceTabSize(n)).toBe(n);
  });
  it("falls back to default for anything else", () => {
    for (const bad of [0, 1, 3, 5, 16, -2, "4", null, undefined, NaN]) {
      expect(coerceTabSize(bad)).toBe(DEFAULT_EDITOR_TAB_SIZE);
    }
  });
});

describe("resolveEditorFontFamily", () => {
  it("prose font only applies to prose files, and only when enabled", () => {
    expect(resolveEditorFontFamily(true, false)).toBe(PROSE_FONT_STACK); // prose + on
    expect(resolveEditorFontFamily(true, true)).toBe(MONO_FONT_STACK); // code stays mono
    expect(resolveEditorFontFamily(false, false)).toBe(MONO_FONT_STACK); // off
    expect(resolveEditorFontFamily(false, true)).toBe(MONO_FONT_STACK);
  });
});

describe("resolveEditorAffordances", () => {
  const base: EditorAffordanceSettings = {
    editorLineWrap: "auto",
    editorFolding: "auto",
    editorShowWhitespace: false,
    editorTabSize: 2,
    editorProseFont: false,
  };

  it("resolves code-file defaults (gutter on, no wrap, fold on, mono)", () => {
    const r = resolveEditorAffordances(base, "src/main.rs");
    expect(r).toEqual({
      gutters: true,
      lineWrap: false,
      folding: true,
      showWhitespace: false,
      tabSize: 2,
      fontFamily: MONO_FONT_STACK,
    });
  });

  it("resolves prose-file defaults (gutter off, wrap on, no fold)", () => {
    const r = resolveEditorAffordances(base, "docs/notes.md");
    expect(r.gutters).toBe(false);
    expect(r.lineWrap).toBe(true);
    expect(r.folding).toBe(false);
  });

  it("prose font applies to markdown when enabled, code stays mono", () => {
    const on = { ...base, editorProseFont: true };
    expect(resolveEditorAffordances(on, "docs/notes.md").fontFamily).toBe(PROSE_FONT_STACK);
    expect(resolveEditorAffordances(on, "src/main.ts").fontFamily).toBe(MONO_FONT_STACK);
  });

  it("threads whitespace + tab size straight through", () => {
    const r = resolveEditorAffordances(
      { ...base, editorShowWhitespace: true, editorTabSize: 8 },
      "app.py",
    );
    expect(r.showWhitespace).toBe(true);
    expect(r.tabSize).toBe(8);
  });

  it("is TOTAL for tab size — an out-of-range value (e.g. from an uncoerced MCP write) is clamped", () => {
    // A direct settings write (MCP set_viewer_state) bypasses load-time
    // coercion; the resolver must still return a safe size so the editor can't
    // crash at `" ".repeat(tabSize)`.
    for (const bad of [-3, 0, 3, 1e9, NaN]) {
      const r = resolveEditorAffordances({ ...base, editorTabSize: bad as unknown as 2 }, "app.ts");
      expect(r.tabSize).toBe(2);
    }
  });
});
