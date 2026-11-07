// SPDX-License-Identifier: MIT
import { expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileKindOf, FILE_KIND_EXTENSIONS } from "@/shared/lib/icons/file-kind";
import { stripViewerExtension } from "@/shared/lib/viewer-file-type";
import { stripPipelineExtension } from "@/shared/lib/pipeline-graph/pipeline-utils";
import { compareNavNames } from "./nav-ordering";
import { countLenses, inLens, stageGroupOf } from "@/shared/lib/nav-lens";
import { STATUS_MARKS } from "@/shared/lib/status-mark";

type Rules = {
  fileKindOf(path: string): string;
  stripViewerExtension(name: string): string;
  stripPipelineExtension(name: string): string;
  compareNavNames(a: string, b: string): number;
  inLens(lens: Parameters<typeof inLens>[0], story: Parameters<typeof inLens>[1]): boolean;
  countLenses(stories: Iterable<Parameters<typeof inLens>[1]>): ReturnType<typeof countLenses>;
  stageGroupOf(story: Parameters<typeof stageGroupOf>[0]): ReturnType<typeof stageGroupOf>;
};

const scope = { codesense: {} };
new Function(
  "window",
  "document",
  readFileSync("src-tauri/src/http/routes/dashboard_kit/kit.js", "utf8"),
)(scope, document);
const kit = (scope.codesense as { kit: Rules }).kit;

it("generated kit rules stay equivalent to their TypeScript sources", () => {
  for (const ext of FILE_KIND_EXTENSIONS)
    expect(kit.fileKindOf(`file.${ext}`)).toBe(fileKindOf(`file.${ext}`));
  for (const path of [
    "README.md",
    "dashboards/overview.html",
    "dashboards/overview.htm",
    "dashboards/OVERVIEW.HTM",
    "docs/overview.htm",
    "skills/x/SKILL.md",
    ".claude/agents/helper.md",
    "config.yaml",
    "data.json",
    "photo.png",
    "archive.zip",
  ])
    expect(kit.fileKindOf(path)).toBe(fileKindOf(path));
  for (const name of ["README.MD", "guide.html", "screenshot.png", "config.yaml", "Makefile"])
    expect(kit.stripViewerExtension(name)).toBe(stripViewerExtension(name));
  for (const name of ["story.md", "story.HTML", "data.json", "my.md.file.md"])
    expect(kit.stripPipelineExtension(name)).toBe(stripPipelineExtension(name));
  for (const [a, b] of [
    ["F-2", "F-10"],
    ["README.md", "readme.MD"],
    ["a", "z"],
  ])
    expect(Math.sign(kit.compareNavNames(a, b))).toBe(Math.sign(compareNavNames(a, b)));
  const roles = [undefined, "not_started", "active", "review", "blocked", "done"] as const;
  const marks = [undefined, ...STATUS_MARKS] as const;
  const stories = roles.flatMap((role) =>
    marks.flatMap((mark) => [false, true].map((ready) => ({ role, mark, ready }))),
  );
  for (const lens of [
    "everything",
    "needs",
    "agents",
    "ready",
    "active",
    "review",
    "blocked",
    "not_started",
    "done",
  ] as const)
    for (const story of stories) expect(kit.inLens(lens, story)).toBe(inLens(lens, story));
  expect(kit.countLenses(stories)).toEqual(countLenses(stories));
  for (const story of stories) expect(kit.stageGroupOf(story)).toBe(stageGroupOf(story));
});
