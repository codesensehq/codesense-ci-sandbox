// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { teamHasSatisfiedClone, isCloneAttached } from "./team-clone-gate";
import type { RepoState } from "@/shared/stores/config-store";

/** Minimal RepoState factory — only `localPath` matters for the gate. */
function makeRepo(localPath: string | null | undefined): RepoState {
  return {
    repoPath: "workspace:acme:default:acme/api",
    repoName: "api (Acme)",
    contextRepoPath: "/ctx",
    teamContentPath: "/ctx/team",
    hasRemote: true,
    remoteUrl: "https://github.com/acme/api",
    mainDoc: "README.md",
    localPath,
  };
}

describe("isCloneAttached @P1", () => {
  it("is true for a non-null localPath, false for null/undefined", () => {
    expect(isCloneAttached(makeRepo("/home/me/api"))).toBe(true);
    expect(isCloneAttached(makeRepo(null))).toBe(false);
    expect(isCloneAttached(makeRepo(undefined))).toBe(false);
  });
});

describe("teamHasSatisfiedClone @P1", () => {
  it("returns false for an empty team (zero tracked repos)", () => {
    expect(teamHasSatisfiedClone([])).toBe(false);
  });

  it("returns false when every repo has a nullish localPath (null or undefined)", () => {
    expect(teamHasSatisfiedClone([makeRepo(null), makeRepo(undefined)])).toBe(false);
  });

  it("returns true when at least one repo has a non-null localPath (attached)", () => {
    expect(teamHasSatisfiedClone([makeRepo("/home/me/api")])).toBe(true);
  });

  it("returns true on a partial team — some attached, some not", () => {
    expect(teamHasSatisfiedClone([makeRepo(null), makeRepo("/home/me/api")])).toBe(true);
  });
});
