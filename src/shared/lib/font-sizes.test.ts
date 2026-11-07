// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  clampFontPx,
  getEffectiveViewerFontPx,
  getEffectiveTerminalFontPx,
  getEffectiveTreeFontPx,
  getEffectiveCodeEditorFontPx,
  EDITOR_FONT_PX_MAP,
  FONT_PX_MIN,
  FONT_PX_MAX,
} from "./font-sizes";

describe("font-sizes", () => {
  describe("clampFontPx", () => {
    it("clamps values below the minimum", () => {
      expect(clampFontPx(4)).toBe(FONT_PX_MIN);
    });

    it("clamps values above the maximum", () => {
      expect(clampFontPx(9999)).toBe(FONT_PX_MAX);
    });

    it("rounds fractional values", () => {
      expect(clampFontPx(15.7)).toBe(16);
    });

    it("returns 0 for non-finite input", () => {
      expect(clampFontPx(Number.NaN)).toBe(0);
      expect(clampFontPx(Number.POSITIVE_INFINITY)).toBe(0);
    });
  });

  describe("getEffectiveViewerFontPx", () => {
    it("returns the custom px when a positive override is set", () => {
      expect(getEffectiveViewerFontPx("normal", 22)).toBe(22);
    });

    it("falls back to the preset when custom is 0", () => {
      expect(getEffectiveViewerFontPx("large", 0)).toBe(16);
    });

    it("falls back to the preset when custom is undefined", () => {
      expect(getEffectiveViewerFontPx("small", undefined)).toBe(12);
    });
  });

  describe("getEffectiveTerminalFontPx", () => {
    it("returns the custom px when a positive override is set", () => {
      expect(getEffectiveTerminalFontPx("normal", 20)).toBe(20);
    });

    it("falls back to the preset when custom is 0", () => {
      expect(getEffectiveTerminalFontPx("large", 0)).toBe(15);
    });

    it("falls back to the preset when custom is undefined", () => {
      expect(getEffectiveTerminalFontPx("xl", undefined)).toBe(17);
    });
  });

  describe("getEffectiveTreeFontPx", () => {
    it("returns the custom px when a positive override is set", () => {
      expect(getEffectiveTreeFontPx("normal", 18)).toBe(18);
    });

    it("falls back to the preset when custom is 0", () => {
      expect(getEffectiveTreeFontPx("large", 0)).toBe(13);
    });

    it("falls back to the preset when custom is undefined", () => {
      expect(getEffectiveTreeFontPx("small", undefined)).toBe(11);
    });
  });

  describe("getEffectiveCodeEditorFontPx (SS-14 D-13)", () => {
    it("returns the custom px when a positive override is set", () => {
      expect(getEffectiveCodeEditorFontPx("normal", 19)).toBe(19);
    });

    it("falls back to the preset when custom is 0", () => {
      expect(getEffectiveCodeEditorFontPx("large", 0)).toBe(EDITOR_FONT_PX_MAP.large);
    });

    it("falls back to the preset when custom is undefined", () => {
      expect(getEffectiveCodeEditorFontPx("small", undefined)).toBe(EDITOR_FONT_PX_MAP.small);
      expect(getEffectiveCodeEditorFontPx("normal", undefined)).toBe(EDITOR_FONT_PX_MAP.normal);
      expect(getEffectiveCodeEditorFontPx("xl", undefined)).toBe(EDITOR_FONT_PX_MAP.xl);
    });

    it("preset scale matches terminal (closest visual peer)", () => {
      // Documented in D-13: editor preset matches terminal.
      expect(EDITOR_FONT_PX_MAP).toEqual({
        small: 11,
        normal: 13,
        large: 15,
        xl: 17,
      });
    });
  });
});
