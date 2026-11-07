// SPDX-License-Identifier: MIT
// @vitest-environment node
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { describe, expect, it } from "vitest";
import { TERMINAL_UNICODE_VERSION } from "../src/shared/terminal/terminal-parser-options";
import { TestWorker } from "./test-worker";

const require = createRequire(import.meta.url);
const root = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));
const installed = (name: string): string => require(`${name}/package.json`).version;
const source = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");

/** Whether a worker session parses with the display's width table. */
function workerUsesDisplayWidths(worker: TestWorker): boolean {
  worker.create(1);
  return worker.host.unicodeVersion(1) === TERMINAL_UNICODE_VERSION;
}

describe("version lock", () => {
  it("the worker's xterm is exactly the display's", () => {
    expect(root.dependencies["@xterm/xterm"]).toBe(root.dependencies["@xterm/headless"]);
    expect(installed("@xterm/headless")).toBe(installed("@xterm/xterm"));
    expect(installed("@xterm/headless")).toBe(root.dependencies["@xterm/headless"]);
  });

  it("both sides load the same, exactly pinned, Unicode 11 addon", () => {
    // One installed copy serves the display and the worker bundle.
    expect(installed("@xterm/addon-unicode11")).toBe(root.dependencies["@xterm/addon-unicode11"]);
  });

  it("the display and the worker both take their parser setup from terminal-parser-options", () => {
    const display = source("../src/shared/terminal/terminal-manager.ts");
    const worker = source("./emulator.ts");
    for (const file of [display, worker]) {
      expect(file).toMatch(/from "[^"]*shared\/terminal\/terminal-parser-options"/);
      expect(file).toContain("activateTerminalParser");
      expect(file).toContain("TERMINAL_PARSER_OPTIONS");
    }
    expect(display).not.toMatch(/activeVersion\s*=/);
  });

  it("a worker session parses with the display's Unicode version", () => {
    expect(workerUsesDisplayWidths(new TestWorker())).toBe(true);
  });

  // Negative control: Unicode 11 activated only on the display side is caught.
  it("fails when the worker does not activate the display's parser setup", () => {
    expect(workerUsesDisplayWidths(new TestWorker({ activate: () => {} }))).toBe(false);
  });
});
