// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { canMergeMark } from "./merge-mark";

// SW-CP-27 AC-2: the format gate must match CP-24's `ConfigFormat::detect`
// EXACTLY (extension-only, lowercased): json, jsonc, json5, yaml, yml, toml.
// A drift where the UI offers a mark CP-24 rejects is the bug to avoid.
describe("canMergeMark", () => {
  it.each(["json", "jsonc", "json5", "yaml", "yml", "toml"])(
    "enables the toggle for a .%s config file",
    (ext) => {
      expect(canMergeMark(`config.${ext}`)).toBe(true);
      expect(canMergeMark(`some/dir/nested.${ext}`)).toBe(true);
    },
  );

  it("matches case-insensitively (CP-24 lowercases the extension)", () => {
    expect(canMergeMark("Config.JSON")).toBe(true);
    expect(canMergeMark("Settings.YAML")).toBe(true);
  });

  it.each([".md", ".png", ".editorconfig", ".env", ".txt", ".toml.bak", ""])(
    "disables the toggle for a non-structured file (%s)",
    (name) => {
      expect(canMergeMark(`file${name}`)).toBe(false);
    },
  );

  it("gates the canonical codesense merge targets ON", () => {
    // The driving cases — a whole-root dotfile and a file nested in a whole-root dir.
    expect(canMergeMark(".mcp.json")).toBe(true);
    expect(canMergeMark(".claude/settings.json")).toBe(true);
    expect(canMergeMark(".claude/settings.local.json")).toBe(true);
  });

  it("does NOT treat a dotfile with no further extension as markable", () => {
    // `.editorconfig` is INI, not structured — CP-24 rejects it. A leading dot is
    // not an extension boundary.
    expect(canMergeMark(".editorconfig")).toBe(false);
    expect(canMergeMark(".gitignore")).toBe(false);
  });
});
