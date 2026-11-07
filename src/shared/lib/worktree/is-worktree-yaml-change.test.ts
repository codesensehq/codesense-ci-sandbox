// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { isWorktreeYamlChangeEvent } from "./is-worktree-yaml-change";

describe("isWorktreeYamlChangeEvent", () => {
  it("matches legacy <source>/.codesense/worktree.yaml paths", () => {
    expect(isWorktreeYamlChangeEvent("/repos/x/.codesense/worktree.yaml")).toBe(true);
  });

  it("matches SS.8 {orgRoot}/_codesense/worktree/{owner}/{repo}.yaml paths", () => {
    expect(
      isWorktreeYamlChangeEvent(
        "/home/dev/.codesense/users/42/orgs/acme/_codesense/worktree/alderco/codesense.yaml",
      ),
    ).toBe(true);
  });

  it("rejects unrelated source-repo paths", () => {
    expect(isWorktreeYamlChangeEvent("/repos/x/src/main.ts")).toBe(false);
    expect(isWorktreeYamlChangeEvent("/repos/x/.gitignore")).toBe(false);
  });

  it("rejects empty string", () => {
    expect(isWorktreeYamlChangeEvent("")).toBe(false);
  });

  it("rejects a sibling file under .codesense that isn't worktree.yaml", () => {
    // The settings panel only refreshes for worktree.yaml — other
    // .codesense/* writes (e.g. pts-migration.json) should be ignored.
    expect(isWorktreeYamlChangeEvent("/org-root/.codesense/pts-migration.json")).toBe(false);
  });

  it("rejects a non-worktree subdir under _codesense (forward compat)", () => {
    // Only `_codesense/worktree/` is current; future inhabitants of
    // `_codesense/` should not piggyback on this filter.
    expect(isWorktreeYamlChangeEvent("/org-root/_codesense/runtime/cache.json")).toBe(false);
  });

  it("rejects editor backup + temp file suffixes on the legacy path", () => {
    // Code-review patch #2: substring-only match used to trip on
    // every `.swp` / `.lock` / `.tmp` write the editor made. The
    // watcher already fires on the final atomic rename; the temps
    // don't need their own refresh.
    expect(isWorktreeYamlChangeEvent("/repos/x/.codesense/worktree.yaml.swp")).toBe(false);
    expect(isWorktreeYamlChangeEvent("/repos/x/.codesense/worktree.yaml.lock")).toBe(false);
    expect(isWorktreeYamlChangeEvent("/repos/x/.codesense/worktree.yaml.tmp")).toBe(false);
    expect(isWorktreeYamlChangeEvent("/repos/x/.codesense/worktree.yaml~")).toBe(false);
  });

  it("rejects non-yaml files under _codesense/worktree/", () => {
    // A future README, lockfile, or any sibling under
    // `_codesense/worktree/` must not trigger the refresh.
    expect(isWorktreeYamlChangeEvent("/org-root/_codesense/worktree/README.md")).toBe(false);
    expect(
      isWorktreeYamlChangeEvent("/org-root/_codesense/worktree/alderco/codesense.yaml.swp"),
    ).toBe(false);
    expect(isWorktreeYamlChangeEvent("/org-root/_codesense/worktree/alderco/.lock")).toBe(false);
  });
});
