// SPDX-License-Identifier: MIT
/**
 * Drift guard: `STORY_CHANGES_ONLY` (what the bridge accepts for `storyChanges({only})`) must be
 * exactly the Rust `story_changes::Only` enum. Parses the Rust source; no runtime Tauri needed.
 */
import { describe, it, expect } from "vitest";
import { readFileSync } from "fs";
import { resolve } from "path";
import { STORY_CHANGES_ONLY } from "./commands";

function rustOnlyValues(): string[] {
  const source = readFileSync(
    resolve(__dirname, "../../src-tauri/src/session_usage/story_changes.rs"),
    "utf-8",
  );
  const body = /pub enum Only \{([\s\S]*?)\n\}/.exec(source)?.[1];
  if (!body) throw new Error("story_changes.rs has no `pub enum Only`");
  return (
    body
      .split("\n")
      .filter((line) => !line.trim().startsWith("//") && !line.trim().startsWith("#"))
      .map((line) => /^\s*([A-Z][A-Za-z]*),\s*$/.exec(line)?.[1])
      .filter((name): name is string => name !== undefined)
      // `#[serde(rename_all = "camelCase")]`: the wire name is the variant with a lower-case first letter.
      .map((name) => name[0].toLowerCase() + name.slice(1))
  );
}

/** The wire name of a variant is its name with a lower-case first letter only while the enum says so. */
function rustOnlyRenameAll(): string | undefined {
  const source = readFileSync(
    resolve(__dirname, "../../src-tauri/src/session_usage/story_changes.rs"),
    "utf-8",
  );
  return /#\[serde\(rename_all = "(\w+)"\)\]\s*(?:#\[[^\]]*\]\s*)*pub enum Only\b/.exec(
    source,
  )?.[1];
}

describe("storyChanges only", () => {
  it("the Rust Only enum is camelCase on the wire, which the name mapping above assumes", () => {
    expect(rustOnlyRenameAll()).toBe("camelCase");
  });

  it("STORY_CHANGES_ONLY matches the Rust Only enum exactly", () => {
    expect([...STORY_CHANGES_ONLY].sort()).toEqual(rustOnlyValues().sort());
  });
});
