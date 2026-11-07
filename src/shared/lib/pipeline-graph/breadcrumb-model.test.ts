// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  buildPipelineBreadcrumbs,
  collapsePipelineBreadcrumbs,
  type PipelineCrumb,
} from "./breadcrumb-model";

describe("buildPipelineBreadcrumbs @P1", () => {
  it("returns [] when not drilled in", () => {
    expect(
      buildPipelineBreadcrumbs({
        isDrilledIn: false,
        rootPath: "/root",
        scopeAbsPath: "/root/epic-a",
        rootLabel: "Pipeline",
      }),
    ).toEqual([]);
  });

  it("returns [] when rootPath is missing", () => {
    expect(
      buildPipelineBreadcrumbs({
        isDrilledIn: true,
        rootPath: "",
        scopeAbsPath: "/root/epic-a",
        rootLabel: "Pipeline",
      }),
    ).toEqual([]);
  });

  it("returns [] when the scope is not a descendant of root (defensive)", () => {
    expect(
      buildPipelineBreadcrumbs({
        isDrilledIn: true,
        rootPath: "/root",
        scopeAbsPath: "/elsewhere/epic-a",
        rootLabel: "Pipeline",
      }),
    ).toEqual([]);
  });

  it("builds root + one segment for a single-level drill", () => {
    const crumbs = buildPipelineBreadcrumbs({
      isDrilledIn: true,
      rootPath: "/root",
      scopeAbsPath: "/root/epic-a",
      rootLabel: "Pipeline",
    });
    expect(crumbs).toEqual<PipelineCrumb[]>([
      { label: "Pipeline", absPath: "/root", isCurrent: false },
      { label: "epic-a", absPath: "/root/epic-a", isCurrent: true },
    ]);
  });

  it("reconstructs cumulative absolute paths for a nested drill", () => {
    const crumbs = buildPipelineBreadcrumbs({
      isDrilledIn: true,
      rootPath: "/root",
      scopeAbsPath: "/root/epic-a/sub-epic/leaf",
      rootLabel: "Pipeline",
    });
    expect(crumbs.map((c) => c.absPath)).toEqual([
      "/root",
      "/root/epic-a",
      "/root/epic-a/sub-epic",
      "/root/epic-a/sub-epic/leaf",
    ]);
    expect(crumbs.map((c) => c.label)).toEqual(["Pipeline", "epic-a", "sub-epic", "leaf"]);
    expect(crumbs.filter((c) => c.isCurrent)).toHaveLength(1);
    expect(crumbs[crumbs.length - 1].isCurrent).toBe(true);
  });

  it("tolerates a trailing slash on rootPath", () => {
    const crumbs = buildPipelineBreadcrumbs({
      isDrilledIn: true,
      rootPath: "/root/",
      scopeAbsPath: "/root/epic-a",
      rootLabel: "Pipeline",
    });
    expect(crumbs.map((c) => c.absPath)).toEqual(["/root", "/root/epic-a"]);
  });

  it("marks the root crumb current when scope equals root", () => {
    const crumbs = buildPipelineBreadcrumbs({
      isDrilledIn: true,
      rootPath: "/root",
      scopeAbsPath: "/root",
      rootLabel: "Pipeline",
    });
    expect(crumbs).toEqual<PipelineCrumb[]>([
      { label: "Pipeline", absPath: "/root", isCurrent: true },
    ]);
  });
});

describe("collapsePipelineBreadcrumbs @P1", () => {
  const mk = (n: number): PipelineCrumb[] =>
    Array.from({ length: n }, (_, i) => ({
      label: `c${i}`,
      absPath: `/root/${Array.from({ length: i }, (_, j) => `s${j}`).join("/")}`,
      isCurrent: i === n - 1,
    }));

  it("keeps all crumbs when the trail is three or fewer", () => {
    const crumbs = mk(3);
    const { displayCrumbs, hiddenAncestors } = collapsePipelineBreadcrumbs(crumbs, "/root");
    expect(displayCrumbs).toEqual(crumbs);
    expect(hiddenAncestors).toEqual([]);
  });

  it("collapses the middle to an ellipsis when deeper than three", () => {
    const crumbs = mk(5); // root + 4
    const { displayCrumbs, hiddenAncestors } = collapsePipelineBreadcrumbs(crumbs, "/root");
    expect(displayCrumbs).toEqual([crumbs[0], "ellipsis", crumbs[3], crumbs[4]]);
    // hidden = everything between root and parent (indices 1..len-2 exclusive of parent)
    expect(hiddenAncestors).toEqual(crumbs.slice(1, crumbs.length - 2));
  });

  it("points parentAbsPath at the second-to-last crumb", () => {
    const crumbs = mk(4);
    const { parentAbsPath } = collapsePipelineBreadcrumbs(crumbs, "/root");
    expect(parentAbsPath).toBe(crumbs[crumbs.length - 2].absPath);
  });

  it("falls back to rootPath for parentAbsPath at depth 1", () => {
    const crumbs = mk(1);
    const { parentAbsPath } = collapsePipelineBreadcrumbs(crumbs, "/root");
    expect(parentAbsPath).toBe("/root");
  });
});
