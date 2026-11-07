// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { deepRewriteFlattenTeamPaths } from "./migrate-flatten-team-paths";

const ORG = "/Users/me/.codesense/users/1/orgs/acme/team-context";

describe("deepRewriteFlattenTeamPaths", () => {
  it("rewrites _team/{team}/{marker} paths across the WHOLE blob incl. Workspace fields", () => {
    // A persisted viewer blob whose stale `_team/…` paths live in the
    // Workspace-level fields (`storyFilePath`, `epicDocPath`) the reconnection
    // + tray-grouping bugs key on — NOT just in tabs. This is the field the
    // rename-shaped `rewriteTeamContentPaths` misses (AC-3).
    const viewer: Record<string, unknown> = {
      workspaces: {
        "acme/team-context:default": [
          {
            kind: "story",
            storyFilePath: `${ORG}/_team/default/content/impl/SW-X-1.md`,
            epicDocPath: `${ORG}/_team/default/content/impl/epic-24/README.md`,
            epicDirPath: `${ORG}/_team/default/content/impl/epic-24`,
          },
        ],
      },
      openTabs: {
        "acme/team-context:default": [
          { path: `${ORG}/_team/default/content/impl/SW-X-1.md`, renamedFrom: undefined },
        ],
      },
      // A repos-layer path (install marker too) proves all markers anchor.
      drillPaths: {
        "acme/team-context:backend": `${ORG}/_team/backend/repos/acme/api`,
      },
    };

    deepRewriteFlattenTeamPaths(viewer);

    const ws = (viewer.workspaces as Record<string, Array<Record<string, string>>>)[
      "acme/team-context:default"
    ][0];
    expect(ws.storyFilePath).toBe(`${ORG}/default/content/impl/SW-X-1.md`);
    expect(ws.epicDocPath).toBe(`${ORG}/default/content/impl/epic-24/README.md`);
    expect(ws.epicDirPath).toBe(`${ORG}/default/content/impl/epic-24`);
    const tab = (viewer.openTabs as Record<string, Array<Record<string, string>>>)[
      "acme/team-context:default"
    ][0];
    expect(tab.path).toBe(`${ORG}/default/content/impl/SW-X-1.md`);
    expect((viewer.drillPaths as Record<string, string>)["acme/team-context:backend"]).toBe(
      `${ORG}/backend/repos/acme/api`,
    );
    // No `_team/` segment survives anywhere.
    expect(JSON.stringify(viewer)).not.toContain("/_team/");
  });

  it("leaves a coincidental non-marker /_team/ occurrence untouched", () => {
    // `_team/default/notes` — `notes` is not a layer marker, so the anchor
    // (marker immediately after the team) does not match. Left as-is.
    const viewer: Record<string, unknown> = {
      p: `${ORG}/_team/default/notes/scratch.md`,
    };
    deepRewriteFlattenTeamPaths(viewer);
    expect(viewer.p).toBe(`${ORG}/_team/default/notes/scratch.md`);
  });

  it("is a structural no-op on already-flat paths (idempotent)", () => {
    const viewer: Record<string, unknown> = {
      workspaces: {
        "acme/team-context:default": [{ storyFilePath: `${ORG}/default/content/impl/SW-X-1.md` }],
      },
    };
    const before = JSON.stringify(viewer);
    deepRewriteFlattenTeamPaths(viewer);
    expect(JSON.stringify(viewer)).toBe(before);
    // Second run over the once-rewritten blob also changes nothing.
    deepRewriteFlattenTeamPaths(viewer);
    expect(JSON.stringify(viewer)).toBe(before);
  });

  it("handles a team path that ends exactly at the marker (no trailing slash)", () => {
    const viewer: Record<string, unknown> = { p: `${ORG}/_team/default/content` };
    deepRewriteFlattenTeamPaths(viewer);
    expect(viewer.p).toBe(`${ORG}/default/content`);
  });

  it("rewrites BOTH boundary-sharing occurrences in one string (non-consuming lookahead)", () => {
    // Pathological but proves the global pass doesn't drop the second match when
    // the two share the boundary `/`. Mirrors the twin ORGS_PATH_LEGACY_RE.
    const viewer: Record<string, unknown> = {
      p: `${ORG}/_team/default/content/_team/backend/repos/acme/api`,
    };
    deepRewriteFlattenTeamPaths(viewer);
    expect(viewer.p).toBe(`${ORG}/default/content/backend/repos/acme/api`);
  });
});
