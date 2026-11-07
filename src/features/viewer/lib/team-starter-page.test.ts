// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { isStarterPagePath, looksLikeFile, starterPageChoice } from "./team-starter-page";

const ORG = { slug: "acme/ctx", org_root: "/orgs/acme" };
const TEAM = { name: "web", landing_page: null };
const CONTENT = "/orgs/acme/web/content";

const choice = (
  path: string,
  team: { name: string; landing_page?: string | null } = TEAM,
  effectivePath: string | null = null,
  org = ORG,
) => starterPageChoice({ org, team, path, effectivePath });

describe("starterPageChoice", () => {
  it("offers Set on a file of the active team's content", () => {
    expect(choice(`${CONTENT}/guide/start.md`)).toEqual({
      kind: "settable",
      rel: "guide/start.md",
    });
  });

  it("marks the declared page current (compared content-relative)", () => {
    const team = { name: "web", landing_page: " guide/start.md " };
    expect(choice("/orgs/acme//web/content/guide/start.md", team)).toEqual({
      kind: "current",
      rel: "guide/start.md",
      declared: true,
    });
    expect(choice(`${CONTENT}/README.md`, team, `${CONTENT}/guide/start.md`).kind).toBe("settable");
  });

  it("marks the README fallback current but not declared", () => {
    expect(choice(`${CONTENT}/README.md`, TEAM, `${CONTENT}/README.md`)).toEqual({
      kind: "current",
      rel: "README.md",
      declared: false,
    });
  });

  it("says why a hidden file, a parent team's file or a read-only org can't be the page", () => {
    expect(choice(`${CONTENT}/.claude/notes.md`)).toMatchObject({ kind: "blocked" });
    expect(choice(`${CONTENT}/.env`)).toEqual({
      kind: "blocked",
      reason: "Hidden files can't be a starter page.",
    });
    expect(choice("/orgs/acme/platform/content/guide.md")).toEqual({
      kind: "blocked",
      reason: "This file is platform's. A starter page must be one of web's own files.",
    });
    const docs = { slug: "codesense/docs", org_root: "/u/codesense-docs" };
    expect(
      choice(
        "/u/codesense-docs/getting-started/content/tour.html",
        { name: "getting-started" },
        null,
        docs,
      ),
    ).toEqual({ kind: "blocked", reason: "CodeSense Docs are read-only." });
  });

  it("uses the row's real kind when known: an extensionless file, a dotted folder", () => {
    const at = (path: string, isFile: boolean) =>
      starterPageChoice({ org: ORG, team: TEAM, path, effectivePath: null, isFile }).kind;
    expect(at(`${CONTENT}/LICENSE`, true)).toBe("settable");
    expect(at(`${CONTENT}/v1.2`, false)).toBe("hidden");
  });

  it("says a CodeSense Docs dotfile is read-only before calling it hidden", () => {
    const docs = { slug: "codesense/docs", org_root: "/u/codesense-docs" };
    expect(
      choice(
        "/u/codesense-docs/getting-started/content/.env",
        { name: "getting-started" },
        null,
        docs,
      ),
    ).toEqual({ kind: "blocked", reason: "CodeSense Docs are read-only." });
  });

  it("offers nothing outside any team's content or on a folder", () => {
    expect(choice("/src/app/README.md").kind).toBe("hidden");
    expect(choice("virtual://manage-teams").kind).toBe("hidden");
    expect(choice(`${CONTENT}/guide`).kind).toBe("hidden");
    expect(choice("/orgs/acme/teams.yml").kind).toBe("hidden");
  });
});

describe("looksLikeFile / isStarterPagePath", () => {
  it("treats a basename with an extension as a file", () => {
    expect(looksLikeFile("/a/README.md")).toBe(true);
    expect(looksLikeFile("/a/guide")).toBe(false);
    expect(looksLikeFile("/a/.env")).toBe(false);
  });

  it("matches the effective page by content-relative path", () => {
    expect(isStarterPagePath(`${CONTENT}/README.md`, `${CONTENT}/README.md`, CONTENT)).toBe(true);
    expect(isStarterPagePath(`${CONTENT}/a.md`, `${CONTENT}/README.md`, CONTENT)).toBe(false);
    expect(isStarterPagePath(`${CONTENT}/README.md`, null, CONTENT)).toBe(false);
  });
});
