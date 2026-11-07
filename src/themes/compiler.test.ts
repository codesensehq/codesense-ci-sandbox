// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { compileTheme, validateTheme } from "./compiler";
import codesense from "./codesense.tokens.json";

describe("typed theme compiler", () => {
  it("compiles both modes deterministically", () => {
    expect(compileTheme(codesense, "light")["--theme-surface-canvas-bg"]).toBe("#ffffff");
    expect(compileTheme(codesense, "dark")["--theme-border-control"]).toBe("#6e7681");
    expect(compileTheme(codesense, "light")).toMatchSnapshot();
    expect(compileTheme(codesense, "dark")).toMatchSnapshot();
  });
  it.each(["evil;}body{}", "url(https://example.com)"])("refuses font %s", (font) => {
    const t = structuredClone(codesense) as any;
    t.light.font = { $type: "fontFamily", $value: font };
    expect(() => compileTheme(t, "light")).toThrow(/light.font/);
  });
  it("refuses unknown types, nested aliases, missing modes and extensions with paths", () => {
    const t = structuredClone(codesense) as any;
    t.light.a = { $type: "color", $value: "{light.border.control}" };
    t.light.b = { $type: "color", $value: "{light.a}" };
    expect(() => compileTheme(t, "light")).toThrow(/light.b/);
    delete t.light.a;
    delete t.light.b;
    t.light.bad = { $type: "script", $value: "anything" };
    expect(() => compileTheme(t, "light")).toThrow(/light.bad/);
    delete t.light.bad;
    delete t.dark;
    expect(() => compileTheme(t, "light")).toThrow(/dark/);
    t.dark = codesense.dark;
    t.light.$extensions = {};
    expect(() => compileTheme(t, "light")).toThrow(/light.\$extensions/);
  });
  it("strips control and bidi characters from display names", () => {
    const t = { ...codesense, name: "Code\u202esense\u0000" };
    expect(validateTheme(t).name).toBe("Codesense");
  });
});
