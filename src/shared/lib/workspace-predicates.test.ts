// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { isEpicShellWorkspace } from "./workspace-predicates";

describe("isEpicShellWorkspace", () => {
  it("is true for a slug-less workspace anchored to an epic dir", () => {
    expect(isEpicShellWorkspace({ sourceRepoSlug: undefined, epicDirPath: "SW-EW" })).toBe(true);
  });

  it("is false when the workspace carries its own sourceRepoSlug", () => {
    expect(isEpicShellWorkspace({ sourceRepoSlug: "acme/repo", epicDirPath: "SW-EW" })).toBe(false);
  });

  it("is false when there is no epicDirPath", () => {
    expect(isEpicShellWorkspace({ sourceRepoSlug: undefined, epicDirPath: undefined })).toBe(false);
  });

  it("is false for null/undefined", () => {
    expect(isEpicShellWorkspace(null)).toBe(false);
    expect(isEpicShellWorkspace(undefined)).toBe(false);
  });
});
