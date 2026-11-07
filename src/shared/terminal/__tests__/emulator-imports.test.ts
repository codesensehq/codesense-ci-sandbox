// SPDX-License-Identifier: MIT
// @vitest-environment node
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * The emulator worker bundles these files from the app tree. It runs outside
 * the webview, so anything they import beyond `@xterm/*` (React, the DOM, app
 * modules) would either break its build or drag the app into it.
 */
const WORKER_SHARED = ["buffer-dump.ts", "terminal-parser-options.ts"];

function importsOf(source: string): string[] {
  const specifiers: string[] = [];
  const pattern =
    /(?:^|\n)\s*(?:import|export)\b[^"'`]*?from\s*["']([^"']+)["']|import\s*\(\s*["']([^"']+)["']\s*\)|(?:^|\n)\s*import\s*["']([^"']+)["']/g;
  for (const match of source.matchAll(pattern)) {
    specifiers.push(match[1] ?? match[2] ?? match[3]);
  }
  return specifiers;
}

/** The imports `source` may not have. */
function forbiddenImports(source: string): string[] {
  const allowed = (specifier: string) =>
    specifier.startsWith("@xterm/") ||
    WORKER_SHARED.some((file) => specifier === `./${file.replace(/\.ts$/, "")}`);
  return importsOf(source).filter((specifier) => !allowed(specifier));
}

const read = (file: string) => readFileSync(resolve(__dirname, "..", file), "utf8");

describe("files the emulator worker bundles", () => {
  it.each(WORKER_SHARED)("%s imports only @xterm/* and the other shared file", (file) => {
    const source = read(file);
    expect(importsOf(source).length).toBeGreaterThan(0);
    expect(forbiddenImports(source)).toEqual([]);
  });

  // Negative control: the guard catches a React import.
  it("rejects buffer-dump.ts importing a React module", () => {
    const violating = `import { useState } from "react";\n${read("buffer-dump.ts")}`;
    expect(forbiddenImports(violating)).toEqual(["react"]);
  });
});
