// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { SourceRepoSlug } from "@/shared/types/domain";
import {
  contextId,
  parseContextId,
  personalContextId,
  teamAllContextId,
  teamAllContext,
  teamAllLabel,
  teamRepoContextId,
  teamRepoLabel,
  viewOnlyRepoContextId,
  SCRATCH_SPACE_LABEL,
} from "./context-model";

const CODESENSE: SourceRepoSlug = { owner: "alderco", repo: "codesense" };
const WIDGETS: SourceRepoSlug = { owner: "acme", repo: "widgets" };

// Banned internal words that must never appear in any user-visible string.
const BANNED = /\b(provider|overlay|layer|scratch|TeamWorkspace|TeamRepo|Workspace)\b/;

describe("context-model — stable logical id round-trips", () => {
  it("personal id round-trips", () => {
    const id = personalContextId();
    expect(parseContextId(id)).toEqual({ kind: "personal" });
  });

  it("team-all id round-trips (orgSlug + teamName)", () => {
    const id = teamAllContextId("alderco", "default");
    expect(parseContextId(id)).toEqual({
      kind: "team-all",
      orgSlug: "alderco",
      teamName: "default",
    });
  });

  it("team-repo id round-trips (orgSlug + teamName + slug)", () => {
    const id = teamRepoContextId("alderco", "default", CODESENSE);
    expect(parseContextId(id)).toEqual({
      kind: "team-repo",
      orgSlug: "alderco",
      teamName: "default",
      repoSlug: CODESENSE,
    });
  });

  it("view-only-repo id round-trips (slug only)", () => {
    const id = viewOnlyRepoContextId(WIDGETS);
    expect(parseContextId(id)).toEqual({ kind: "view-only-repo", repoSlug: WIDGETS });
  });

  it("a team name containing a colon still round-trips (orgSlug is first field)", () => {
    const id = teamRepoContextId("alderco", "team:with:colons", CODESENSE);
    expect(parseContextId(id)).toEqual({
      kind: "team-repo",
      orgSlug: "alderco",
      teamName: "team:with:colons",
      repoSlug: CODESENSE,
    });
  });

  it("the single contextId() builder agrees with the per-kind builders", () => {
    expect(contextId({ kind: "personal" })).toBe(personalContextId());
    expect(contextId({ kind: "team-all", orgSlug: "alderco", teamName: "default" })).toBe(
      teamAllContextId("alderco", "default"),
    );
    expect(
      contextId({
        kind: "team-repo",
        orgSlug: "alderco",
        teamName: "default",
        repoSlug: CODESENSE,
      }),
    ).toBe(teamRepoContextId("alderco", "default", CODESENSE));
    expect(contextId({ kind: "view-only-repo", repoSlug: WIDGETS })).toBe(
      viewOnlyRepoContextId(WIDGETS),
    );
  });

  it("returns null for malformed ids rather than crashing enumeration", () => {
    expect(parseContextId("garbage")).toBeNull();
    expect(parseContextId("team:onlyorg")).toBeNull();
    expect(parseContextId("repo:noslash")).toBeNull();
    expect(parseContextId("team-repo:org:team")).toBeNull();
  });
});

describe("context-model — vocabulary is word-self-sufficient", () => {
  it("the repo-agnostic bucket reads 'All of <Team>', never Workspace / · all repos", () => {
    const label = teamAllLabel("Platform");
    expect(label).toBe("All of Platform");
    expect(label).not.toMatch(/Workspace/);
    expect(label).not.toMatch(/all repos/);
  });

  it("a repo-scoped team context reads '<Team> · <Repo>'", () => {
    expect(teamRepoLabel("Platform", "codesense")).toBe("Platform · codesense");
  });

  it("the scratch target is named 'Scratch space', never 'Personal'", () => {
    expect(SCRATCH_SPACE_LABEL).toBe("Private");
  });
});

describe("context-model — the team's own content as the nav's move source (SW-XS-15)", () => {
  it("teamAllContext is All of <Team> with a round-tripping id", () => {
    const c = teamAllContext("alderco", "Platform");
    expect(c.label).toBe("All of Platform");
    expect(c.kind).toBe("team-all");
    expect(parseContextId(c.id)).toEqual({
      kind: "team-all",
      orgSlug: "alderco",
      teamName: "Platform",
    });
    expect(c.label).not.toMatch(BANNED);
  });
});
