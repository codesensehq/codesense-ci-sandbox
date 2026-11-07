// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  TEAM_CONTENT_DIR,
  LAYER_MARKER_DIRS,
  pathToTeamContentDirSync,
  teamContentPathFromEpicDocPath,
} from "./team-content-path";

describe("team-content-path", () => {
  it("exposes constants matching Rust context_repo/path.rs", () => {
    expect(TEAM_CONTENT_DIR).toBe("content");
    // SLW-IN-7: mirror of Rust `LAYER_MARKER_DIRS`.
    expect([...LAYER_MARKER_DIRS]).toEqual(["install", "content", "repos", "libraries"]);
  });

  it("matches Rust path_to_team_content_dir doc-comment fixture (flat, SLW-IN-7)", () => {
    // Mirror of context_repo/path.rs example:
    //   org_root = /home/dev/.codesense/users/42/orgs/acme/api
    //   team     = "default"
    //   returns  = /home/dev/.codesense/users/42/orgs/acme/api/default/content
    const result = pathToTeamContentDirSync(
      "/home/dev/.codesense/users/42/orgs/acme/api",
      "default",
    );
    expect(result).toBe("/home/dev/.codesense/users/42/orgs/acme/api/default/content");
  });

  it("handles team names with safe identifier characters", () => {
    expect(pathToTeamContentDirSync("/root", "team-name_2")).toBe("/root/team-name_2/content");
  });
});

// SLW-FX-14: recover org_root / team / teamContentPath synchronously from an
// epic workspace's own persisted `epicDocPath` so the epic's pipeline tree can
// resolve its scan root on cold open — before org-discovery populates the
// (unpersisted) org store. Shape (flat per SLW-IN-7):
//   {org_root}/{team}/content/_bmad-output/…/epic.md
describe("teamContentPathFromEpicDocPath (SLW-FX-14)", () => {
  it("recovers orgRoot, team, and teamContentPath from a real epic doc path", () => {
    const epicDocPath =
      "/Users/x/.codesense/users/1/orgs/o/codesense-context/default/content/_bmad-output/epic-workspaces/epic.md";
    expect(teamContentPathFromEpicDocPath(epicDocPath)).toEqual({
      orgRoot: "/Users/x/.codesense/users/1/orgs/o/codesense-context",
      team: "default",
      teamContentPath: "/Users/x/.codesense/users/1/orgs/o/codesense-context/default/content",
    });
  });

  it("teamContentPath equals pathToTeamContentDirSync for the recovered parts (parity with org-minted RepoState)", () => {
    const orgRoot = "/home/dev/.codesense/users/42/orgs/acme/api";
    const team = "team-name_2";
    const epicDocPath = `${orgRoot}/${team}/content/_bmad-output/x/epic.md`;
    const result = teamContentPathFromEpicDocPath(epicDocPath);
    expect(result?.teamContentPath).toBe(pathToTeamContentDirSync(orgRoot, team));
  });

  it("returns null for a path with no content marker (no-anchor degrade)", () => {
    expect(teamContentPathFromEpicDocPath("/Users/x/some/other/file.md")).toBeNull();
    // a per-source-repo context path's first marker is `repos`, not `content`
    expect(teamContentPathFromEpicDocPath("/root/default/repos/acme/web/story.md")).toBeNull();
  });

  it("returns null when the marker sits at the start (no orgRoot prefix)", () => {
    expect(teamContentPathFromEpicDocPath("/content/epic.md")).toBeNull();
    // team present but no org root before it → null
    expect(teamContentPathFromEpicDocPath("/default/content/epic.md")).toBeNull();
  });

  it("anchors on the FIRST content marker after the org slug (SLW-IN-7)", () => {
    // The team is the segment immediately before the first `content` marker;
    // deeper `content/` dirs under it don't re-anchor.
    const epicDocPath = "/x/orgs/o/codesense-context/real/content/_bmad-output/e/content/epic.md";
    expect(teamContentPathFromEpicDocPath(epicDocPath)).toEqual({
      orgRoot: "/x/orgs/o/codesense-context",
      team: "real",
      teamContentPath: "/x/orgs/o/codesense-context/real/content",
    });
  });

  it("returns null for empty / nullish input", () => {
    expect(teamContentPathFromEpicDocPath("")).toBeNull();
    expect(teamContentPathFromEpicDocPath(undefined)).toBeNull();
    expect(teamContentPathFromEpicDocPath(null)).toBeNull();
  });
});
