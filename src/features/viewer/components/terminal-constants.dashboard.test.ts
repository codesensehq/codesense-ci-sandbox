// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { TERMINAL_THEMES } from "./terminal-constants";

// The dashboard toolkit (cs-live.js, served to published dashboards) draws its
// terminals with a copy of the app's themes: the copy must not drift.
describe("dashboard toolkit terminal themes", () => {
  it("are the app's TERMINAL_THEMES, each with its chrome colour", () => {
    const src = readFileSync(
      resolve(__dirname, "../../../../src-tauri/src/dashboard_publish/toolkit/cs-live.js"),
      "utf8",
    );
    const block = /\/\/ cs-themes:begin\s*var THEMES = (\{.*\});\s*\/\/ cs-themes:end/s.exec(src);
    expect(block).not.toBeNull();
    const copy = JSON.parse(block![1]) as Record<string, Record<string, string>>;
    const want: Record<string, Record<string, string>> = {};
    for (const [name, config] of Object.entries(TERMINAL_THEMES)) {
      want[name] = { chrome: config.chromeBg, ...(config.theme as Record<string, string>) };
    }
    expect(copy).toEqual(want);
    // Colours only: every value a hex colour, so the copy can never carry CSS of its own.
    for (const theme of Object.values(copy)) {
      for (const value of Object.values(theme))
        expect(value).toMatch(/^#[0-9a-f]{6}([0-9a-f]{2})?$/i);
    }
  });
});
