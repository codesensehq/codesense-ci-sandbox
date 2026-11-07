// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { computePipelineScopeUpdate } from "./compute-pipeline-scope-update";

// PTS-2: scope helper anchors on `teamContentPath` (per-team flat content
// dir) instead of composing `${contextRepoPath}/${viewerRoot}`. The third
// arg is now a full absolute path, not a segment name.
describe("computePipelineScopeUpdate", () => {
  const CTX = "/ctx/repo";
  const BASE = "/org/_team/default/content";

  it("returns '' when target equals teamContentPath", () => {
    expect(computePipelineScopeUpdate(BASE, CTX, BASE)).toBe("");
  });

  it("returns '' when target equals teamContentPath with trailing slash", () => {
    expect(computePipelineScopeUpdate(`${BASE}/`, CTX, BASE)).toBe("");
  });

  it("returns relative path when target is a child of teamContentPath", () => {
    expect(computePipelineScopeUpdate(`${BASE}/in-progress`, CTX, BASE)).toBe("in-progress");
  });

  it("returns multi-segment relative path for nested targets", () => {
    expect(
      computePipelineScopeUpdate(`${BASE}/implementation-artifacts/in-progress`, CTX, BASE),
    ).toBe("implementation-artifacts/in-progress");
  });

  it("returns '' (reset to root) when target is the contextRepoPath — back-button case", () => {
    expect(computePipelineScopeUpdate(CTX, CTX, BASE)).toBe("");
  });

  it("returns '' when target is inside contextRepoPath but outside content root", () => {
    expect(computePipelineScopeUpdate(`${CTX}/src`, CTX, BASE)).toBe("");
  });

  it("returns null (pin) when target is completely outside the repo", () => {
    expect(computePipelineScopeUpdate("/other/path", CTX, BASE)).toBeNull();
  });

  it("returns null for empty inputs", () => {
    expect(computePipelineScopeUpdate("", CTX, BASE)).toBeNull();
    expect(computePipelineScopeUpdate(BASE, "", BASE)).toBeNull();
  });

  it("does not treat a prefix match as inside (content vs contentX)", () => {
    // BASE ends in `content`; a sibling `content2` must NOT match as a
    // descendant. Post-PTS-2 the test fixture's CTX and BASE live in
    // disjoint trees (CTX = per-(team, repo) install dir, BASE = per-team
    // flat content), so a target outside BOTH is outside the repo and
    // pins (null) rather than resetting.
    expect(computePipelineScopeUpdate(`${BASE}2/x`, CTX, BASE)).toBeNull();
  });

  it("normalizes scope path via normalizeScopePath", () => {
    expect(computePipelineScopeUpdate(`${BASE}/a/b`, CTX, BASE)).toBe("a/b");
  });
});
