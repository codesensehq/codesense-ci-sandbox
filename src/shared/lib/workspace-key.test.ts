// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { readdirSync, readFileSync, statSync } from "fs";
import { join, relative, resolve } from "path";

import {
  parseWorkspaceKey,
  teamFoldKey,
  NO_WORKSPACE_ID,
  teamOfFoldKey,
  workspaceFoldKey,
  workspaceGroupKey,
  workspaceKey,
  WORKSPACE_KEY_SEPARATOR,
} from "./workspace-key";

const SRC = resolve(__dirname, "../..");

function sourceFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return sourceFiles(path);
    return /\.(ts|tsx)$/.test(name) && !/\.test\./.test(name) ? [path] : [];
  });
}

describe("workspaceKey — the terminal host's team-scoped key", () => {
  it("carries the team, so same-named workspaces of two teams never share a layout", () => {
    expect(workspaceKey("acme/app:core", { id: "test-agent", kind: "generic" })).toBe(
      "acme/app:core|test-agent",
    );
    expect(workspaceKey("acme/app:core", { id: "root:acme/app" })).not.toBe(
      workspaceKey("acme/app:infra", { id: "root:acme/app" }),
    );
  });

  it("keys a story by its frontmatter story_id (as MCP `open_workspace` does), else its id", () => {
    const story = {
      id: "SW-RT-7a-server-owned-terminal-layout",
      kind: "story",
      storyId: "SW-RT-7a",
    };
    expect(workspaceKey("acme/app:core", story)).toBe("acme/app:core|SW-RT-7a");
    expect(workspaceKey("acme/app:core", { ...story, storyId: undefined })).toBe(
      "acme/app:core|SW-RT-7a-server-owned-terminal-layout",
    );
  });

  it("keys a team's no-workspace bucket apart from every workspace's group", () => {
    const none = workspaceGroupKey("acme/app:core", "");
    expect(none).not.toBe(workspaceKey("acme/app:core", { id: "" }));
    expect(none).not.toContain(WORKSPACE_KEY_SEPARATOR);
    expect(none).not.toBe(workspaceGroupKey("acme/app:ops", ""));
    expect(workspaceGroupKey("acme/app:core", "w1")).toBe("acme/app:core|w1");
  });

  it("splits back into its team and workspace form", () => {
    expect(parseWorkspaceKey("acme/app:core|generic:/w|x")).toEqual({
      teamKey: "acme/app:core",
      form: "generic:/w|x",
    });
    expect(parseWorkspaceKey("no-separator")).toBeNull();
  });

  it("reads the team back out of every fold key it mints, a workspace id ending like the bucket's included", () => {
    expect(teamOfFoldKey(teamFoldKey("acme/app:core"))).toBe("acme/app:core");
    expect(teamOfFoldKey(workspaceFoldKey("acme/app:core", "w1"))).toBe("acme/app:core");
    expect(teamOfFoldKey(workspaceFoldKey("acme/app:core", ""))).toBe("acme/app:core");
    // A workspace whose id ends with the bucket's suffix is still that team's workspace card.
    expect(teamOfFoldKey(workspaceFoldKey("acme/app:core", "x#no-workspace"))).toBe(
      "acme/app:core",
    );
    expect(teamOfFoldKey(workspaceFoldKey("acme/app:core", "a|b#no-workspace"))).toBe(
      "acme/app:core",
    );
    // Keys this app did not mint.
    expect(teamOfFoldKey("acme/app:core|w1")).toBeNull();
    expect(teamOfFoldKey("#ws-fold:acme/app:core")).toBeNull();
    expect(teamOfFoldKey("#ws-fold:#no-workspace")).toBeNull();
    expect(teamOfFoldKey("#ws-fold:|w1")).toBeNull();
    expect(teamOfFoldKey("#team-fold:")).toBeNull();
  });

  it("reads the team back from the edges of the fold keys", () => {
    // The no-workspace bucket is named by its own id.
    expect(workspaceGroupKey("acme/app:core", NO_WORKSPACE_ID)).toBe(
      workspaceGroupKey("acme/app:core", ""),
    );
    expect(teamOfFoldKey(workspaceFoldKey("acme/app:core", NO_WORKSPACE_ID))).toBe("acme/app:core");
    // A team key holding the fold prefixes' marks, or the bucket's suffix, is still read whole.
    expect(teamOfFoldKey(teamFoldKey("a#ws-fold:b"))).toBe("a#ws-fold:b");
    expect(teamOfFoldKey(workspaceFoldKey("a#no-workspace", NO_WORKSPACE_ID))).toBe(
      "a#no-workspace",
    );
    expect(teamOfFoldKey(workspaceFoldKey("a#no-workspace", "w1"))).toBe("a#no-workspace");
    // A workspace id that is itself a fold key, or holds the separator, stays in its card's team.
    expect(teamOfFoldKey(workspaceFoldKey("t", teamFoldKey("other")))).toBe("t");
    expect(teamOfFoldKey(workspaceFoldKey("t", "generic:/a|b"))).toBe("t");
    // Neither prefix, or a prefix with nothing after it.
    expect(teamOfFoldKey("")).toBeNull();
    expect(teamOfFoldKey("#ws-fold:")).toBeNull();
  });

  it("is minted in ONE place: no other source file joins a team and a workspace into a key", () => {
    const minting = sourceFiles(SRC)
      .filter((path) => {
        const code = readFileSync(path, "utf-8").replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
        return (
          /\$\{[^}]*team[^}]*\}\|\$\{[^}]*(workspace|ws|form|story)[^}]*\}/i.test(code) ||
          /WORKSPACE_KEY_SEPARATOR/.test(code) ||
          /\bworkspaceKey\s*=\s*\(/.test(code)
        );
      })
      .map((path) => relative(SRC, path));
    expect(minting).toEqual(["shared/lib/workspace-key.ts"]);
  });

  it("agrees with the Rust minting site (`pty_layout::workspace_key`)", () => {
    const rust = readFileSync(resolve(SRC, "../src-tauri/src/commands/pty_layout.rs"), "utf-8");
    const separator = rust.match(/WORKSPACE_KEY_SEPARATOR: char = '(.)'/)?.[1];
    expect(separator).toBe(WORKSPACE_KEY_SEPARATOR);
    expect(rust).toContain('format!("{org_slug}:{}", team_name.unwrap_or(NO_TEAM))');
    expect(rust).toContain('const NO_TEAM: &str = "_none";');
  });
});
