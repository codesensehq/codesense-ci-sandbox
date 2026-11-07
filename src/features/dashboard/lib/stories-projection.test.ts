// SPDX-License-Identifier: MIT
import { beforeEach, expect, it } from "vitest";
import { buildTree, buildStoryRef } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { buildPipelineGraph } from "@/shared/lib/pipeline-graph/pipeline-graph";
import { computeCountsByAbsPath } from "./nav-ordering";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import { buildStoriesRows } from "./stories-projection";

beforeEach(() => useStatusTaxonomyStore.getState().resetForTest());
it("wraps native readiness, declared titles, marks and recursive epic counts across scopes", () => {
  const dep = { ...buildStoryRef("dependency.md", { status: "done" }), storyId: "DEP-1" };
  const next = {
    ...buildStoryRef("next.md", { status: "todo", dependsOn: ["DEP-1"] }),
    storyId: "NEXT-1",
    openTags: { title: ["NEXT-1 — Declared title"] },
    sprint: "1",
    metadataValid: false,
    metadataInvalidReason: "Unconfigured repository",
  };
  const tree = buildTree("", {
    stories: [next],
    subEpics: [buildTree("", { dirPath: "other", stories: [dep] })],
  });
  const rows = buildStoriesRows(tree, { [next.filePath]: "question" });
  expect(rows.find((r) => r.path === next.filePath)).toMatchObject({
    kind: "story",
    storyId: "NEXT-1",
    title: "Declared title",
    label: "Todo",
    role: "not_started",
    ready: true,
    blockedBy: [],
    dependencyCount: 1,
    mark: "question",
    sprint: "1",
    metadataValid: false,
    metadataInvalidReason: "Unconfigured repository",
  });
  expect(rows.find((r) => r.path === tree.absoluteDirPath)).toMatchObject({
    ...computeCountsByAbsPath(tree).get(tree.absoluteDirPath),
    mark: "question",
  });
  expect(rows.find((r) => r.path === next.filePath)?.ready).toBe(
    buildPipelineGraph(tree).readiness(next, tree.dirPath).ready,
  );
});
it("includes plain docs, main docs and native shadow/override metadata", () => {
  const tree = buildTree("", {});
  tree.mainDoc = {
    filePath: `${tree.absoluteDirPath}/README.md`,
    supportingDocs: null,
    purposeText: null,
  };
  tree.docs = [
    {
      fileName: "guide.md",
      filePath: `${tree.absoluteDirPath}/guide.md`,
      shadowsTeamFile: true,
      overridesTeam: "platform",
      installedSourceRepo: "owner/repo",
    },
  ];
  const rows = buildStoriesRows(tree, {});
  expect(rows.find((r) => r.path === tree.mainDoc!.filePath)).toMatchObject({
    kind: "main-doc",
    ownerPath: tree.absoluteDirPath,
  });
  expect(rows.find((r) => r.path.endsWith("/guide.md"))).toMatchObject({
    kind: "doc",
    shadows: true,
    overrides: "platform",
    installedSourceRepo: "owner/repo",
  });
});

it("preserves native off-taxonomy rails alongside the native normalized lens role", () => {
  const story = buildStoryRef("off.md", { status: "outside-taxonomy" });
  const row = buildStoriesRows(buildTree("", { stories: [story] }), {}).find(
    (r) => r.path === story.filePath,
  );
  expect(row).toMatchObject({
    role: null,
    label: "outside-taxonomy",
    lensRole: "not_started",
    ready: true,
  });
});

it("derives a retained team's readiness, blockers and counts from its own taxonomy after the active one changes", () => {
  const store = useStatusTaxonomyStore.getState();
  const teamA = store.byId;
  const dep = { ...buildStoryRef("dep.md", { status: "done" }), storyId: "A-DEP" };
  const next = {
    ...buildStoryRef("next.md", { status: "todo", dependsOn: ["A-DEP"] }),
    storyId: "A-NEXT",
  };
  const tree = buildTree("", { stories: [dep, next] });
  const project = () => {
    const rows = buildStoriesRows(tree, {}, teamA);
    return {
      next: rows.find((r) => r.storyId === "A-NEXT"),
      epic: rows.find((r) => r.kind === "epic"),
    };
  };
  const before = project();
  // Team B is complete and valid but gives the same ids other roles, plus its own terminal state.
  const done = store.states.find((s) => s.id === "done")!;
  store.setStates([
    ...store.states.map((s) =>
      s.id === "done"
        ? { ...s, role: "review" as const }
        : s.id === "todo"
          ? { ...s, role: "active" as const }
          : s,
    ),
    { ...done, id: "shipped", label: "Shipped", order: store.states.length },
  ]);
  const after = project();
  expect(before.next).toMatchObject({ role: "not_started", ready: true, blockedBy: [] });
  expect(before.epic).toMatchObject({ done: 1, total: 2 });
  expect(after.next).toMatchObject({ role: "not_started", ready: true, blockedBy: [] });
  expect(after.epic).toMatchObject({ done: 1, total: 2 });
});

it("joins inherited story rows to their active-team files row by relativePath", () => {
  const inherited = buildStoryRef("guide.md", {
    absolutePath: "/parent/docs/guide.md",
    storyId: "DOC-1",
  });
  const tree = buildTree("", {
    stories: [],
    subEpics: [
      buildTree("docs", {
        dirPath: "docs",
        absoluteDirPath: "/team/docs",
        stories: [inherited],
      }),
    ],
  });
  expect(buildStoriesRows(tree, {}).find((row) => row.storyId === "DOC-1")).toMatchObject({
    path: "/parent/docs/guide.md",
    relativePath: "docs/guide.md",
  });
});
