// SPDX-License-Identifier: MIT
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentContextView } from "@/tauri/commands";

import type { AgentContextPivot as PivotState } from "../hooks/use-agent-context";
import { AgentContextPivot } from "./agent-context-pivot";

vi.mock("@/shared/hooks/use-avatar-packs", () => ({
  useAvatarPacks: () => ({ faces: undefined }),
}));

const BASIL = { key: "alderco/default/basil", name: "Basil", avatar: null };
const BUCK = { key: "alderco/default/buck", name: "Buck", avatar: null };
const BRUCE = { key: "alderco/default/bruce", name: "Bruce", avatar: null };
const LANCE = { key: "alderco/default/lance", name: "Lance", avatar: null };
const CODESENSE = { owner: "alderco", repo: "codesense" };

function view(): AgentContextView {
  return {
    key: BASIL.key,
    name: "Basil",
    revision: 4,
    context: null,
    runtime_default: [],
    startup: [],
    repo_overrides: [],
    repo: "github.com/alderco/codesense",
    defaulted: true,
    entries: [],
    roots: [],
    files: [],
    truncated: false,
    totals: { files: 1214, bytes: 19_084_000 },
    warnings: [],
  };
}

function pivot(over: Partial<PivotState> = {}): PivotState {
  return {
    agents: [BASIL, BRUCE, BUCK, LANCE],
    repos: [CODESENSE, { owner: "alderco", repo: "website" }],
    agent: BASIL,
    repo: CODESENSE,
    select: vi.fn(),
    view: view(),
    loading: false,
    error: null,
    reload: vi.fn(),
    membership: () => ({ state: "out" }),
    plan: () => null,
    edit: vi.fn(),
    ...over,
  };
}

function renderPivot(p: PivotState) {
  return render(<AgentContextPivot pivot={p} glyphMap={new Map()} isLightTheme />);
}

function open() {
  fireEvent.click(screen.getByTestId("agent-context-trigger"));
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe("AgentContextPivot (SW-XS-15)", () => {
  it("AC-1: the header reads 'Context for Basil · in codesense — 1,214 files · 18.2 MB'", () => {
    renderPivot(pivot());
    const header = screen.getByTestId("agent-context-header");
    expect(header.textContent?.replace(/\s+/g, " ").trim()).toBe(
      "Context for Basil · in codesense — 1,214 files · 18.2 MB",
    );
  });

  it("Team (everything) shows no summary: it implies nothing about delivery", () => {
    renderPivot(pivot({ agent: null, repo: null, view: null }));
    expect(screen.getByTestId("agent-context-label")).toHaveTextContent("Team (everything)");
    expect(screen.queryByTestId("agent-context-summary")).toBeNull();
  });

  it("lists Team (everything) first, then the agents with the selected one first, then the repos", () => {
    renderPivot(pivot());
    open();
    const [whose, where] = screen.getAllByRole("listbox");
    expect(
      within(whose)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual([
      "Team (everything)All of the context repo, not what an agent gets",
      "Basil",
      "Bruce",
      "Buck",
      "Lance",
    ]);
    expect(within(whose).getByRole("option", { name: /^Basil/ })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(
      within(where)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["Team workspaceNo repo: what a team-workspace launch gets", "codesense", "website"]);
  });

  it("picking an agent keeps the repo; picking a repo keeps the agent; Team clears both", () => {
    const p = pivot();
    renderPivot(p);
    open();
    fireEvent.click(screen.getByTestId(`agent-context-option-${BUCK.key}`));
    expect(p.select).toHaveBeenLastCalledWith({
      agentKey: BUCK.key,
      repoSlug: "github.com/alderco/codesense",
    });
    open();
    fireEvent.click(screen.getByTestId("agent-context-repo-alderco/website"));
    expect(p.select).toHaveBeenLastCalledWith({
      agentKey: BASIL.key,
      repoSlug: "github.com/alderco/website",
    });
    open();
    fireEvent.click(screen.getByTestId("agent-context-option-team"));
    expect(p.select).toHaveBeenLastCalledWith(null);
  });

  it("AC-5: the pivot is a listbox with arrow keys and type-ahead, focus starting on the selection", async () => {
    renderPivot(pivot());
    open();
    const whose = screen.getAllByRole("listbox")[0];
    const basil = within(whose).getByRole("option", { name: /^Basil/ });
    await waitFor(() => expect(basil).toHaveFocus());
    expect(basil).toHaveAttribute("tabindex", "0");

    fireEvent.keyDown(basil, { key: "ArrowDown" });
    expect(within(whose).getByRole("option", { name: /^Bruce/ })).toHaveFocus();

    // Type-ahead: "l" jumps to Lance; "bu" finds Buck.
    fireEvent.keyDown(whose, { key: "l" });
    expect(within(whose).getByRole("option", { name: /^Lance/ })).toHaveFocus();
    await new Promise((r) => setTimeout(r, 650));
    fireEvent.keyDown(whose, { key: "b" });
    fireEvent.keyDown(whose, { key: "u" });
    expect(within(whose).getByRole("option", { name: /^Buck/ })).toHaveFocus();

    fireEvent.keyDown(whose, { key: "Home" });
    expect(within(whose).getByRole("option", { name: /^Team \(everything\)/ })).toHaveFocus();
    fireEvent.keyDown(whose, { key: "End" });
    expect(within(whose).getByRole("option", { name: /^Lance/ })).toHaveFocus();
  });

  it("says when the want set cannot be resolved, with Retry", () => {
    const p = pivot({ view: null, error: "no such agent" });
    renderPivot(p);
    expect(screen.getByTestId("agent-context-summary")).toHaveTextContent(
      "Couldn't resolve Basil's context",
    );
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(p.reload).toHaveBeenCalled();
  });

  it("names its warnings without blocking", () => {
    const v = view();
    v.warnings = [{ path: "docs", code: "missing", message: "no tier has docs" }];
    renderPivot(pivot({ view: v }));
    expect(screen.getByTestId("agent-context-warnings")).toHaveTextContent("1 warning");
    expect(screen.getByTestId("agent-context-warnings")).toHaveAttribute(
      "title",
      "no tier has docs",
    );
  });

  it("with no team agents, says how to get one", () => {
    renderPivot(pivot({ agents: [], agent: null, repo: null, view: null }));
    open();
    expect(screen.getByTestId("agent-context-no-agents")).toHaveTextContent(
      "No team agents yet. Create one in Agents.",
    );
  });
});
