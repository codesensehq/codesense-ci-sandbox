// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import {
  getCodeMirror,
  getCodeMirrorMerge,
  decodeUtf8,
  _resetCmLoaderForTests,
  _resetCmMergeLoaderForTests,
} from "./cm-loader";

describe("decodeUtf8", () => {
  it("returns empty string for empty byte array", () => {
    expect(decodeUtf8([])).toBe("");
  });

  it("decodes ASCII", () => {
    expect(decodeUtf8([72, 101, 108, 108, 111])).toBe("Hello");
  });

  it("decodes 2-byte UTF-8 (non-Latin1)", () => {
    // "é" is U+00E9 → 0xC3 0xA9
    expect(decodeUtf8([0xc3, 0xa9])).toBe("é");
  });

  it("decodes 4-byte UTF-8 (emoji)", () => {
    // "😀" is U+1F600 → 0xF0 0x9F 0x98 0x80
    expect(decodeUtf8([0xf0, 0x9f, 0x98, 0x80])).toBe("😀");
  });

  it("decodes multi-line ASCII content", () => {
    // "a\nb\nc" → [97, 10, 98, 10, 99]
    expect(decodeUtf8([97, 10, 98, 10, 99])).toBe("a\nb\nc");
  });
});

describe("getCodeMirror", () => {
  beforeEach(() => {
    _resetCmLoaderForTests();
  });

  it("returns the same promise on repeat calls (singleton)", () => {
    const p1 = getCodeMirror();
    const p2 = getCodeMirror();
    expect(p1).toBe(p2);
  });

  it("resolves to a module exposing the documented surface", async () => {
    const cm = await getCodeMirror();
    expect(cm.EditorState).toBeTypeOf("function");
    expect(cm.EditorView).toBeTypeOf("function");
    expect(cm.Compartment).toBeTypeOf("function");
    expect(cm.keymap).toBeTypeOf("object"); // Facet instance
    expect(cm.placeholder).toBeTypeOf("function");
    expect(cm.lineNumbers).toBeTypeOf("function");
    expect(cm.history).toBeTypeOf("function");
    expect(cm.defaultKeymap).toBeInstanceOf(Array);
    expect(cm.historyKeymap).toBeInstanceOf(Array);
    expect(cm.syntaxHighlighting).toBeTypeOf("function");
    expect(cm.defaultHighlightStyle).toBeTypeOf("object");
    expect(cm.StreamLanguage).toBeTypeOf("function");
    // SLW-WDE-7: HighlightStyle (build the codesense palette) + Lezer tags.
    expect(cm.HighlightStyle).toBeTypeOf("function");
    expect(cm.HighlightStyle.define).toBeTypeOf("function");
    expect(cm.tags).toBeTypeOf("object");
    expect(cm.tags.keyword).toBeDefined();
    expect(cm.tags.string).toBeDefined();
    expect(cm.tags.comment).toBeDefined();
  });

  it("exposes every language factory", async () => {
    const cm = await getCodeMirror();
    expect(cm.languages.markdown()).toBeDefined();
    expect(cm.languages.typescript()).toBeDefined();
    expect(cm.languages.javascript()).toBeDefined();
    expect(cm.languages.json()).toBeDefined();
    expect(cm.languages.yaml()).toBeDefined();
    expect(cm.languages.python()).toBeDefined();
    expect(cm.languages.shell()).toBeDefined();
    expect(cm.languages.rust()).toBeDefined();
    expect(cm.languages.xml()).toBeDefined(); // SW-VE-2
  });

  it("caches the resolved module across calls", async () => {
    const cm1 = await getCodeMirror();
    const cm2 = await getCodeMirror();
    expect(cm1).toBe(cm2);
  });
});

describe("getCodeMirrorMerge", () => {
  beforeEach(() => {
    _resetCmMergeLoaderForTests();
  });

  it("returns the same promise on repeat calls (singleton)", () => {
    const p1 = getCodeMirrorMerge();
    const p2 = getCodeMirrorMerge();
    expect(p1).toBe(p2);
  });

  it("resolves to a module exposing MergeView and chunk-nav commands", async () => {
    const merge = await getCodeMirrorMerge();
    expect(merge.MergeView).toBeTypeOf("function");
    expect(merge.goToNextChunk).toBeTypeOf("function");
    expect(merge.goToPreviousChunk).toBeTypeOf("function");
  });

  it("caches the resolved module across calls", async () => {
    const m1 = await getCodeMirrorMerge();
    const m2 = await getCodeMirrorMerge();
    expect(m1).toBe(m2);
  });

  it("is independent from getCodeMirror's singleton (two-loader split, D-3)", async () => {
    const cm = await getCodeMirror();
    const merge = await getCodeMirrorMerge();
    // Two-loader split: callers that don't open the merge view never pay
    // its load cost. The two facades MUST be distinct objects.
    expect(merge).not.toBe(cm as unknown);
    expect("MergeView" in cm).toBe(false);
    expect("EditorState" in merge).toBe(false);
  });

  it("retries on rejection by clearing the singleton (mirrors SS-14 D-12)", async () => {
    // Sanity-only: the contract is that a failed load is not cached. We
    // exercise it by calling the public reset helper (the production
    // singleton's catch handler invokes the same clearing logic).
    const p1 = getCodeMirrorMerge();
    _resetCmMergeLoaderForTests();
    const p2 = getCodeMirrorMerge();
    expect(p1).not.toBe(p2);
    // Both eventually resolve to the same SHAPE (they're independent
    // imports of the same module).
    const [m1, m2] = await Promise.all([p1, p2]);
    expect(m1.MergeView).toBeTypeOf("function");
    expect(m2.MergeView).toBeTypeOf("function");
  });
});
