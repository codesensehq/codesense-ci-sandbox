// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  detectIndentation,
  detectLineEnding,
  indentationLabel,
  lineEndingLabel,
} from "./text-format";

const FALLBACK = { useTabs: false, size: 2 };

describe("detectLineEnding", () => {
  it("defaults to LF", () => {
    expect(detectLineEnding("")).toBe("\n");
    expect(detectLineEnding("one line")).toBe("\n");
    expect(detectLineEnding("a\nb\n")).toBe("\n");
  });

  it("detects CRLF files and labels them", () => {
    expect(detectLineEnding("a\r\nb\r\nc")).toBe("\r\n");
    expect(lineEndingLabel(detectLineEnding("a\r\nb\r\n"))).toBe("CRLF");
  });

  it("edits a mixed file as LF", () => {
    expect(detectLineEnding("a\r\nb\r\nc\nd")).toBe("\n");
  });
});

describe("detectIndentation", () => {
  it("finds two-space indentation", () => {
    const text = "function f() {\n  if (x) {\n    y();\n  }\n}\n";
    expect(detectIndentation(text, { useTabs: false, size: 4 })).toEqual({
      useTabs: false,
      size: 2,
    });
  });

  it("finds four-space indentation", () => {
    const text = "def f():\n    if x:\n        y()\n    return 1\n";
    expect(detectIndentation(text, FALLBACK)).toEqual({ useTabs: false, size: 4 });
  });

  it("finds tabs", () => {
    const text = "func f() {\n\tif x {\n\t\ty()\n\t}\n}\n";
    expect(detectIndentation(text, FALLBACK)).toEqual({ useTabs: true, size: 2 });
  });

  it("is not fooled by block-comment continuation lines", () => {
    const text = "/**\n * doc\n * more\n */\nfunction f() {\n    body();\n}\n";
    expect(detectIndentation(text, FALLBACK)).toEqual({ useTabs: false, size: 4 });
  });

  it("falls back when nothing is indented", () => {
    expect(detectIndentation("a\nb\nc", FALLBACK)).toBe(FALLBACK);
  });

  it("labels the result", () => {
    expect(indentationLabel({ useTabs: false, size: 4 })).toBe("Spaces: 4");
    expect(indentationLabel({ useTabs: true, size: 4 })).toBe("Tab Size: 4");
  });
});
