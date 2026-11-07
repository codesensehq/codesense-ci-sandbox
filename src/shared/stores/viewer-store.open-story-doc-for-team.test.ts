// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn(),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  listNotes: vi.fn().mockResolvedValue([]),
  readNote: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0, content: "" }),
  createNote: vi.fn().mockResolvedValue({
    id: "n2",
    title: "Untitled",
    pinned: false,
    lastOpenedAt: 0,
    content: "",
  }),
  writeNote: vi.fn().mockResolvedValue(undefined),
  updateNoteMeta: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0 }),
  deleteNote: vi.fn().mockResolvedValue([]),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import { useViewerStore } from "./viewer-store";
import { useOrgStore } from "./org-store";
import type { Org } from "@/shared/types/domain";

beforeEach(() => {
  useViewerStore.setState({ openFile: vi.fn() });
  vi.clearAllMocks();
});

describe("openStoryDocForTeam (SW-AM-2)", () => {
  const org: Org = {
    slug: "acme",
    display_name: "Acme",
    org_root: "/ctx",
    created_at: "",
    remote_url: null,
    config_flags: {},
    people: [],
    teams: [{ name: "default", source_repos: [] }],
  };

  it("AC-1/AC-5: resolves the team's content anchor and opens the file", () => {
    const openFile = vi.fn();
    useViewerStore.setState({ openFile });
    useOrgStore.setState({
      orgs: [org],
      stateLoaded: true,
      workspaceRepos: {},
    });

    useViewerStore.getState().openStoryDocForTeam("acme:default", "/ctx/story.md");

    expect(openFile).toHaveBeenCalledWith(
      "acme:default",
      "team:acme:default",
      "/ctx/story.md",
      undefined,
    );
  });

  it("passes keepLayout through, for an open the user did not ask for", () => {
    const openFile = vi.fn();
    useViewerStore.setState({ openFile });
    useOrgStore.setState({ orgs: [org], stateLoaded: true, workspaceRepos: {} });

    useViewerStore
      .getState()
      .openStoryDocForTeam("acme:default", "/ctx/story.md", { keepLayout: true });

    expect(openFile).toHaveBeenCalledWith("acme:default", "team:acme:default", "/ctx/story.md", {
      keepLayout: true,
    });
  });

  it("AC-3: is a true no-op when the team is unavailable — no tab commit", () => {
    const openFile = vi.fn();
    useViewerStore.setState({ openFile });
    useOrgStore.setState({ orgs: [], stateLoaded: true, workspaceRepos: {} });

    useViewerStore.getState().openStoryDocForTeam("acme:default", "/ctx/story.md");

    // No content root → returns before delegating: no phantom tab.
    expect(openFile).not.toHaveBeenCalled();
  });
});
