// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { DEFAULT_NAV_LENS, normalizeNavLens } from "./nav-lens";
import { resolveNavSource } from "./nav-source";

describe("nav source", () => {
  it("restores older lenses with the clone source", () => {
    expect(normalizeNavLens({ view: "changes" }).source).toEqual({ kind: "clone" });
    expect(DEFAULT_NAV_LENS.source).toEqual({ kind: "clone" });
  });
  it("uses backend private paths for both the tree and git", () => {
    const clone = {
      repoPath: "team://a",
      repoName: "A",
      contextRepoPath: "/clone",
      teamContentPath: "/clone/team/content",
      mainDoc: "",
    };
    const privateSource = {
      repoRoot: "/scratch",
      contentPath: "/scratch/orgs/a/b/content",
      prefix: "orgs/a/b/content",
      baselineStarted: false,
      baselineError: null,
    };
    expect(resolveNavSource({ kind: "clone" }, clone, privateSource)).toEqual({
      content: clone,
      orgRoot: "/clone",
      prefix: "team/content",
    });
    expect(resolveNavSource({ kind: "private" }, clone, privateSource)).toMatchObject({
      content: { teamContentPath: privateSource.contentPath },
      orgRoot: "/scratch",
      prefix: privateSource.prefix,
    });
    expect(resolveNavSource({ kind: "private" }, clone, null).orgRoot).toBeNull();
  });
});
