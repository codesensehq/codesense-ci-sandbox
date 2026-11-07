// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { STATUS_CONFIG } from "../src/shared/lib/pipeline-graph/status-config";
import { kitRoles } from "../src/themes/kit";

const kitDir = "src-tauri/src/http/routes/dashboard_kit";
const tokensCss = readFileSync(`${kitDir}/tokens.css`, "utf8");
const kitJs = readFileSync(`${kitDir}/kit.js`, "utf8");
const layout = JSON.parse(readFileSync("src/themes/kit-layout.json", "utf8")) as Record<
  string,
  unknown
>;

function agentStateNames(): string[] {
  const match = kitJs.match(/var AGENT_STATES = \{([\s\S]*?)\n  \};/);
  if (!match) throw new Error("Could not find AGENT_STATES in dashboard kit");
  return [...match[1].matchAll(/^\s*(?:"([^"]+)"|([\w-]+))\s*:/gm)].map(
    ([, quoted, bare]) => quoted ?? bare,
  );
}

function requiredTokenNames(): Set<string> {
  return new Set([
    ...Object.keys(layout),
    ...Object.keys(kitRoles("viewer")).map((name) => `--cs-${name}`),
    ...Object.keys(STATUS_CONFIG).map((id) => `--cs-story-${id}`),
    ...agentStateNames().map((state) => `--cs-state-${state}`),
  ]);
}

function missingTokenNames(css: string): string[] {
  const actual = new Set([...css.matchAll(/^\s*(--[\w-]+):/gm)].map(([, name]) => name));
  return [...requiredTokenNames()].filter((name) => !actual.has(name)).sort();
}

describe("generated dashboard kit theme tokens", () => {
  it("names the missing tokens when a canonical family is removed in memory", () => {
    const withoutStoryFamily = tokensCss.replace(/^\s*--cs-story-[\w-]+:[^\n]*\n/gm, "");
    const missingStoryTokens = Object.keys(STATUS_CONFIG)
      .map((id) => `--cs-story-${id}`)
      .sort();

    expect(missingTokenNames(withoutStoryFamily)).toEqual(missingStoryTokens);
  });

  it("includes every token name required by its canonical sources", () => {
    const missing = missingTokenNames(tokensCss);

    expect(missing, `Missing generated dashboard kit tokens: ${missing.join(", ")}`).toEqual([]);
  });
});
