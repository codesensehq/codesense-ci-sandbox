// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";

import {
  copyIntoTeam,
  parentMoveDir,
  pathInTeamContent,
  teamContentRoot,
  teamCopyPath,
} from "./inherited-file";

const FILE = "/u/orgs/acme/platform/content/rules/deep/r.md";
const ACTIVE = "/u/orgs/acme/default/content";

describe("inherited-file paths (SW-XS-20)", () => {
  it("finds the parent's content root in the active team's org, and the file's place in it", () => {
    expect(teamContentRoot(`${ACTIVE}/`, "platform")).toBe("/u/orgs/acme/platform/content");
    expect(pathInTeamContent(FILE, "platform", ACTIVE)).toBe("rules/deep/r.md");
    expect(pathInTeamContent(FILE, "other", ACTIVE)).toBeNull();
    // A folder that happens to be called `platform/content` deeper down is not the team.
    expect(
      pathInTeamContent("/u/orgs/acme/default/content/x/platform/content/a.md", "platform", ACTIVE),
    ).toBeNull();
  });

  it("puts a team copy at the same path in the active team", () => {
    expect(teamCopyPath(FILE, "platform", `${ACTIVE}/`)).toBe(`${ACTIVE}/rules/deep/r.md`);
    expect(teamCopyPath(FILE, "platform", "")).toBeNull();
  });

  it("maps a move destination to the matching folder in the parent", () => {
    expect(parentMoveDir(FILE, "platform", "notes/a", ACTIVE)).toBe(
      "/u/orgs/acme/platform/content/notes/a",
    );
    expect(parentMoveDir(FILE, "platform", "", ACTIVE)).toBe("/u/orgs/acme/platform/content");
    expect(parentMoveDir(FILE, "other", "", ACTIVE)).toBeNull();
  });
});

describe("copyIntoTeam (SW-XS-20)", () => {
  it("copies the parent's file to the same path in the active team, creating folders", async () => {
    const ingest = vi.fn(async (_src: string, dir: string, name: string) => ({
      ok: true as const,
      path: `${dir}/${name}`,
    }));
    const r = await copyIntoTeam(FILE, "platform", ACTIVE, "default", ingest);
    expect(ingest).toHaveBeenCalledWith(FILE, `${ACTIVE}/rules/deep`, "r.md");
    expect(r).toEqual({ ok: true, path: `${ACTIVE}/rules/deep/r.md` });
  });

  it("refuses when the active team already has that path", async () => {
    const ingest = vi.fn(async () => ({
      ok: false as const,
      error: "File already exists",
      collision: true,
    }));
    const r = await copyIntoTeam(FILE, "platform", ACTIVE, "default", ingest as never);
    expect(r).toEqual({
      ok: false,
      exists: true,
      error: "default already has rules/deep/r.md. Open that file instead.",
    });
  });
});
