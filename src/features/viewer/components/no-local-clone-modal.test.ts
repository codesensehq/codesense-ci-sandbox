// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { parseSyntheticRepoKey } from "@/shared/lib/build-synthetic-repo";

// R2-P9 patch: explicit tests for parseSyntheticRepoKey's strict-parsing
// guards (rejects extra colons / multi-slash / empty segments / non-
// synthetic input). Previously the parser was claimed to be hardened but
// went untested.

describe("parseSyntheticRepoKey", () => {
  it("parses a canonical synthetic key", () => {
    const parsed = parseSyntheticRepoKey("workspace:acme:default:owner/repo");
    expect(parsed).toEqual({
      orgSlug: "acme",
      team: "default",
      owner: "owner",
      repo: "repo",
    });
  });

  it("returns null for legacy real-path repoPath (not synthetic)", () => {
    expect(parseSyntheticRepoKey("/Users/dev/repo")).toBeNull();
  });

  it("returns null for empty string", () => {
    expect(parseSyntheticRepoKey("")).toBeNull();
  });

  it("returns null for input with extra colon segments", () => {
    // "workspace:a:b:c:d" — 4 segments after prefix, parser must reject.
    expect(parseSyntheticRepoKey("workspace:a:b:c:d/extra")).toBeNull();
  });

  it("returns null for input with too few colon segments", () => {
    expect(parseSyntheticRepoKey("workspace:onlyone")).toBeNull();
    expect(parseSyntheticRepoKey("workspace:a:b")).toBeNull();
  });

  it("returns null when owner/repo segment has more than one slash", () => {
    expect(parseSyntheticRepoKey("workspace:acme:default:host/owner/sub/repo")).toBeNull();
  });

  it("returns null when owner/repo segment has no slash", () => {
    expect(parseSyntheticRepoKey("workspace:acme:default:justrepo")).toBeNull();
  });

  it("returns null when owner is empty (key starts with `/repo`)", () => {
    expect(parseSyntheticRepoKey("workspace:acme:default:/repo")).toBeNull();
  });

  it("returns null when repo is empty (key ends with `owner/`)", () => {
    expect(parseSyntheticRepoKey("workspace:acme:default:owner/")).toBeNull();
  });

  it("returns null when any of orgSlug / team is empty", () => {
    expect(parseSyntheticRepoKey("workspace::default:owner/repo")).toBeNull();
    expect(parseSyntheticRepoKey("workspace:acme::owner/repo")).toBeNull();
  });

  it("preserves case in owner/repo (case-sensitive parse)", () => {
    const parsed = parseSyntheticRepoKey("workspace:acme:default:Owner/Repo");
    expect(parsed?.owner).toBe("Owner");
    expect(parsed?.repo).toBe("Repo");
  });
});
