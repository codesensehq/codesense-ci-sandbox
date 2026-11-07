// SPDX-License-Identifier: MIT
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

import type { StoryDraft, StoryValidationReport } from "@/tauri/commands";
import type { DerivedPipelineTree, StoryRef } from "@/shared/lib/pipeline-graph/types";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import { NewStoryDialog } from "./new-story-dialog";

function story(storyId: string): StoryRef {
  return {
    fileName: `${storyId}.md`,
    filePath: `/c/checkout/${storyId}.md`,
    dependsOn: null,
    status: "todo",
    documentType: "story",
    supportingDocs: null,
    storyId,
    sourceRepo: "acme/api",
    team: null,
  };
}

const tree: DerivedPipelineTree = {
  dirPath: "",
  dirName: "",
  absoluteDirPath: "/c",
  stories: [],
  orderError: null,
  mainDoc: null,
  subEpics: [
    {
      dirPath: "checkout",
      dirName: "checkout",
      absoluteDirPath: "/c/checkout",
      stories: [story("CO-1"), story("CO-4")],
      subEpics: [],
      orderError: null,
      mainDoc: { filePath: "/c/checkout/README.md", supportingDocs: null, purposeText: null },
    },
    {
      dirPath: "empty-epic",
      dirName: "empty-epic",
      absoluteDirPath: "/c/empty-epic",
      stories: [],
      subEpics: [],
      orderError: null,
      mainDoc: {
        filePath: "/c/empty-epic/README.md",
        supportingDocs: null,
        purposeText: null,
      },
    },
  ],
};

const clean: StoryValidationReport = {
  detectedKind: "pipeline_story",
  format: "markdown",
  valid: true,
  findings: [],
};

let previewReport: StoryValidationReport = clean;
let epicRepos = ["acme/web", "acme/api"];
const drafts: StoryDraft[] = [];

beforeEach(() => {
  drafts.length = 0;
  previewReport = clean;
  epicRepos = ["acme/web", "acme/api"];
  useStatusTaxonomyStore.getState().resetForTest();
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string, args: { draft?: StoryDraft }) => {
    if (cmd === "story_repo_choices") return epicRepos;
    if (cmd === "preview_new_story" || cmd === "create_story") {
      drafts.push(args.draft!);
      return { path: "/c/checkout/CO-5-x.md", content: "", validation: previewReport };
    }
    return undefined;
  });
});

function renderDialog(extra: Partial<Parameters<typeof NewStoryDialog>[0]> = {}) {
  const onCreated = vi.fn();
  const onOpenChange = vi.fn();
  render(
    <NewStoryDialog
      open
      onOpenChange={onOpenChange}
      tree={tree}
      repoPath="workspace:acme:core:acme/api"
      initialEpicDir="/c/checkout"
      onCreated={onCreated}
      {...extra}
    />,
  );
  return { onCreated, onOpenChange };
}

describe("NewStoryDialog", () => {
  it("proposes a minted id, the epic's only repo and the taxonomy's first status", async () => {
    epicRepos = ["acme/web"];
    renderDialog();
    expect(screen.getByLabelText("Story id")).toHaveValue("CO-5");
    await waitFor(() =>
      expect(screen.getByRole("combobox", { name: "Repo" })).toHaveTextContent("acme/web"),
    );
    expect(screen.getByRole("combobox", { name: "Status" })).toHaveTextContent("Planning");
  });

  it("requires a repo choice when the epic lists several, offering only the epic's repos", async () => {
    const user = userEvent.setup();
    renderDialog();
    await user.type(screen.getByLabelText("Title"), "Refunds");
    await screen.findByTestId("story-findings-clean");
    const repo = screen.getByRole("combobox", { name: "Repo" });
    expect(repo).toHaveTextContent("Choose a repo…");
    expect(screen.getByRole("button", { name: "Create story" })).toBeDisabled();

    await user.click(repo);
    const options = screen.getAllByRole("option").map((o) => o.textContent);
    expect(options).toEqual(["acme/web", "acme/api"]);
    await user.click(screen.getByRole("option", { name: "acme/api" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Create story" })).toBeEnabled());
  });

  it("shows validate_story findings inline and blocks Create on a blocking one", async () => {
    previewReport = {
      ...clean,
      valid: false,
      findings: [
        {
          code: "UNKNOWN_SOURCE_REPO",
          field: "source_repo",
          severity: "blocking",
          message: "`source_repo` is not a configured repo.",
          hint: "Use one of: acme/api",
        },
      ],
    };
    const user = userEvent.setup();
    renderDialog();
    await user.type(screen.getByLabelText("Title"), "Refunds: retry");
    const finding = await screen.findByText(/is not a configured repo/);
    expect(finding.closest("li")).toHaveAttribute("data-severity", "blocking");
    expect(screen.getByRole("button", { name: "Create story" })).toBeDisabled();
  });

  it("creates the story through create_story and refreshes the tree", async () => {
    const user = userEvent.setup();
    const bump = vi.spyOn(useFileRefreshStore.getState(), "bumpFileRefresh");
    const { onCreated, onOpenChange } = renderDialog();

    await user.type(screen.getByLabelText("Title"), "Refunds: retry");
    await user.click(screen.getByRole("combobox", { name: "Repo" }));
    await user.click(await screen.findByRole("option", { name: "acme/web" }));
    await user.click(screen.getByRole("combobox", { name: "Depends on" }));
    await user.click(screen.getByRole("option", { name: "CO-4" }));
    await screen.findByTestId("story-findings-clean");
    const create = screen.getByRole("button", { name: "Create story" });
    await waitFor(() => expect(create).toBeEnabled());
    await user.click(create);

    await waitFor(() => expect(onCreated).toHaveBeenCalledWith("/c/checkout/CO-5-x.md"));
    const createCall = invokeMock.mock.calls.find(([cmd]) => cmd === "create_story");
    expect(createCall?.[1]).toEqual({
      draft: {
        epicDir: "/c/checkout",
        title: "Refunds: retry",
        storyId: "CO-5",
        status: "planning",
        sourceRepo: "acme/web",
        dependsOn: ["CO-4"],
      },
    });
    expect(bump).toHaveBeenCalledWith("workspace:acme:core:acme/api");
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("checks nothing (and shows no error) until a title is typed", async () => {
    renderDialog();
    await new Promise((r) => setTimeout(r, 400));
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "preview_new_story")).toBe(false);
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("button", { name: "Create story" })).toBeDisabled();
  });

  it("an empty epic proposes an id from its directory's initials", () => {
    renderDialog({ initialEpicDir: "/c/empty-epic" });
    expect(screen.getByLabelText("Story id")).toHaveValue("EE-1");
  });

  it("an epic that already numbers its stories keeps its own prefix", () => {
    renderDialog({ initialEpicDir: "/c/checkout" });
    expect(screen.getByLabelText("Story id")).toHaveValue("CO-5");
  });

  it("has no story body field", () => {
    renderDialog();
    expect(screen.queryByLabelText("Story")).toBeNull();
  });

  it("renders nothing when closed", () => {
    renderDialog({ open: false });
    expect(screen.queryByTestId("new-story-dialog")).toBeNull();
  });
});
