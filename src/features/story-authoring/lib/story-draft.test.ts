// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import type { DerivedPipelineTree, StoryRef } from "@/shared/lib/pipeline-graph/types";
import {
  allStoryIds,
  defaultSourceRepo,
  epicStoryIdPrefix,
  findEpicNode,
  listEpics,
  mintStoryId,
  prefixFromEpicDirName,
  proposeStoryId,
  repoChoices,
} from "./story-draft";

function story(storyId: string): StoryRef {
  return {
    fileName: `${storyId}.md`,
    filePath: `/c/${storyId}.md`,
    dependsOn: null,
    status: "planning",
    documentType: "story",
    supportingDocs: null,
    storyId,
    sourceRepo: "acme/api",
    team: null,
  };
}

function dir(
  name: string,
  opts: { stories?: string[]; subEpics?: DerivedPipelineTree[]; epic?: boolean } = {},
): DerivedPipelineTree {
  const abs = `/c/${name}`;
  return {
    dirPath: name,
    dirName: name,
    absoluteDirPath: abs,
    stories: (opts.stories ?? []).map(story),
    subEpics: opts.subEpics ?? [],
    orderError: null,
    mainDoc:
      opts.epic === false
        ? null
        : { filePath: `${abs}/README.md`, supportingDocs: null, purposeText: null },
  };
}

const tree: DerivedPipelineTree = {
  ...dir("", { epic: false }),
  absoluteDirPath: "/c",
  subEpics: [
    dir("14-public-launch", { stories: ["SW-PL-3", "SW-PL-12", "SW-PL-7"] }),
    dir("empty-epic"),
    dir("notes", { epic: false, subEpics: [dir("nested", { stories: ["NX-2"] })] }),
  ],
};

describe("listEpics", () => {
  it("lists every dir with a main doc, including nested ones", () => {
    expect(listEpics(tree).map((e) => e.dirPath)).toEqual([
      "14-public-launch",
      "empty-epic",
      "nested",
    ]);
    expect(listEpics(tree)[0].mainDocPath).toBe("/c/14-public-launch/README.md");
  });
});

describe("story ids", () => {
  it("collects every story id, naturally sorted", () => {
    expect(allStoryIds(tree)).toEqual(["NX-2", "SW-PL-3", "SW-PL-7", "SW-PL-12"]);
  });

  it("derives the epic prefix from its stories' ids", () => {
    expect(epicStoryIdPrefix(["SW-PL-3", "SW-PL-12", "OTHER-1"])).toBe("SW-PL");
    expect(epicStoryIdPrefix(["no-number", ""])).toBeNull();
  });

  it("mints one past the highest number for the prefix across the org", () => {
    expect(mintStoryId("SW-PL", ["SW-PL-3", "SW-PL-12", "SW-PLX-99"])).toBe("SW-PL-13");
    expect(mintStoryId("NEW", [])).toBe("NEW-1");
  });

  it("falls back to dir-name initials for an epic with no stories", () => {
    expect(prefixFromEpicDirName("14-public-launch-codesense-ai")).toBe("PLCA");
    expect(prefixFromEpicDirName("42")).toBe("STORY");
  });

  it("proposes an id minted from the epic prefix", () => {
    expect(proposeStoryId(tree, "/c/14-public-launch")).toBe("SW-PL-13");
    expect(proposeStoryId(tree, "/c/empty-epic")).toBe("EE-1");
    expect(findEpicNode(tree, "/c/nested")?.dirName).toBe("nested");
  });
});

describe("defaultSourceRepo", () => {
  it("preselects only a single choice: the epic's one repo, else the team's one repo", () => {
    expect(defaultSourceRepo(["acme/web"], ["acme/api", "acme/web"])).toBe("acme/web");
    expect(defaultSourceRepo(["acme/web", "acme/api"], ["acme/api"])).toBe("");
    expect(defaultSourceRepo([], ["acme/api"])).toBe("acme/api");
    expect(defaultSourceRepo([], ["acme/api", "acme/web"])).toBe("");
  });
});

describe("repoChoices", () => {
  it("offers the epic's repos when it names any, else the team's", () => {
    expect(repoChoices(["acme/web"], ["acme/api", "acme/web"])).toEqual(["acme/web"]);
    expect(repoChoices([], ["acme/api", "acme/web"])).toEqual(["acme/api", "acme/web"]);
    expect(repoChoices(["acme/web", "acme/web"], [])).toEqual(["acme/web"]);
    expect(defaultSourceRepo(["acme/web", "acme/web"], [])).toBe("acme/web");
  });
});
