// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { getRepoOverrides, setRepoOverrides } from "./workspace-config";
import type { WorkspaceConfig } from "./workspace-config";

const EMPTY: WorkspaceConfig = { repos: {} };
const KEY = "workspace:acme:default:org/repo";

describe("getRepoOverrides", () => {
  it("returns empty object for unknown repo", () => {
    expect(getRepoOverrides(EMPTY, KEY)).toEqual({});
  });

  it("returns stored overrides", () => {
    const config: WorkspaceConfig = {
      repos: { [KEY]: { mainDoc: "Epic.md" } },
    };
    expect(getRepoOverrides(config, KEY)).toEqual({ mainDoc: "Epic.md" });
  });
});

describe("setRepoOverrides", () => {
  it("adds overrides to empty config", () => {
    const result = setRepoOverrides(EMPTY, KEY, { mainDoc: "Spec.md" });
    expect(result.repos[KEY]?.mainDoc).toBe("Spec.md");
  });

  it("merges with existing overrides", () => {
    const config: WorkspaceConfig = {
      repos: { [KEY]: { mainDoc: "Spec.md" } },
    };
    const result = setRepoOverrides(config, KEY, { mainDoc: "Impl.md" });
    expect(result.repos[KEY]).toEqual({
      mainDoc: "Impl.md",
    });
  });

  it("overwrites existing keys", () => {
    const config: WorkspaceConfig = {
      repos: { [KEY]: { mainDoc: "Old.md" } },
    };
    const result = setRepoOverrides(config, KEY, { mainDoc: "New.md" });
    expect(result.repos[KEY]?.mainDoc).toBe("New.md");
  });

  it("does not mutate the original config", () => {
    const config: WorkspaceConfig = {
      repos: { [KEY]: { mainDoc: "Spec.md" } },
    };
    setRepoOverrides(config, KEY, { mainDoc: "Changed.md" });
    expect(config.repos[KEY]?.mainDoc).toBe("Spec.md");
  });

  it("preserves other repos", () => {
    const other = "workspace:acme:default:other/repo";
    const config: WorkspaceConfig = {
      repos: { [other]: { mainDoc: "README.md" } },
    };
    const result = setRepoOverrides(config, KEY, { mainDoc: "Spec.md" });
    expect(result.repos[other]).toEqual({ mainDoc: "README.md" });
    expect(result.repos[KEY]?.mainDoc).toBe("Spec.md");
  });
});
