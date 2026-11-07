// SPDX-License-Identifier: MIT
import { cleanup, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { FlatNavItem } from "@/features/dashboard/lib/flatten-nav-tree";
import type { AgentContextView } from "@/tauri/commands";

import { buildContextMembership } from "../lib/agent-context-model";
import { navRelResolver, teamRelResolver, type NavAgentContext } from "../lib/nav-agent-context";
import { withAgentContext } from "./agent-context-row";

const ROOT = "/ctx/default/content";
const REPO_TIER = "/ctx/default/repos/github.com/alderco/codesense/content";

const view = {
  roots: [
    {
      tier: {
        kind: "repo",
        org: "alderco",
        team: "default",
        repo: "github.com/alderco/codesense",
      },
      path: REPO_TIER,
    },
    { tier: { kind: "team", org: "alderco", team: "default" }, path: ROOT },
  ],
  files: [
    {
      destination: ".claude/skills/review/SKILL.md",
      logical: ".claude/skills/review/SKILL.md",
      bytes: 1,
      root: 1,
    },
    { destination: "docs/repo-adr.md", logical: "docs/repo-adr.md", bytes: 1, root: 0 },
  ],
  truncated: false,
  entries: [
    { path: ".claude/skills", source: "default", files: 1, bytes: 1 },
    { path: "docs", source: "repo_add", files: 1, bytes: 1 },
  ],
  startup: [],
  repo: "github.com/alderco/codesense",
  repo_overrides: [],
} as unknown as AgentContextView;

const ctx: NavAgentContext = {
  agentName: "Barb",
  repoName: "codesense",
  relOf: navRelResolver(ROOT, view),
  editRelOf: teamRelResolver(ROOT),
  defaulted: true,
  membership: buildContextMembership(view),
  plan: () => null,
  edit: vi.fn(),
};

function folder(abs: string): FlatNavItem {
  return {
    type: "folder-header",
    node: { absoluteDirPath: abs } as never,
    depth: 0,
    expanded: false,
    total: 0,
    done: 0,
  };
}

function doc(abs: string): FlatNavItem {
  return { type: "doc", filePath: abs, label: abs, depth: 0, installed: false } as FlatNavItem;
}

function renderRow(item: FlatNavItem) {
  render(<>{withAgentContext(item, <div data-testid="row">row</div>, ctx)}</>);
  return screen.getByTestId("row").parentElement!.parentElement!;
}

describe("withAgentContext — the nav overlay (SW-XS-15 AC-1)", () => {
  it("mutes a folder outside the want set: _bmad-output", () => {
    const wrap = renderRow(folder(`${ROOT}/_bmad-output`));
    expect(wrap).toHaveAttribute("data-agent-context", "out");
    expect(wrap).toHaveClass("cs-agent-ctx-out");
    // No tag word, but the slot keeps the row actions aligned and tells screen readers.
    expect(wrap.querySelector(".cs-agent-ctx-tag")).toHaveTextContent("not in Barb's context");
  });

  it("emphasizes an in-set row with a quiet source tag", () => {
    const wrap = renderRow(folder(`${ROOT}/.claude/skills`));
    expect(wrap).toHaveAttribute("data-agent-context", "in");
    expect(wrap).not.toHaveClass("cs-agent-ctx-out");
    expect(wrap.querySelector('.cs-agent-ctx-tag [aria-hidden="true"]')).toHaveTextContent("team");
    expect(wrap.querySelector(".cs-agent-ctx-tag .sr-only")).toHaveTextContent(
      "Barb gets this from the team",
    );
  });

  it("tags a repo-tier file (drawn from repos/{o}/{r}/content/) as repo", () => {
    const wrap = renderRow(doc(`${REPO_TIER}/docs/repo-adr.md`));
    expect(wrap.querySelector('.cs-agent-ctx-tag [aria-hidden="true"]')).toHaveTextContent("repo");
    expect(wrap.querySelector(".cs-agent-ctx-tag")).toHaveAttribute(
      "title",
      "Barb gets this from codesense's own content",
    );
    // The synthetic folder the repo tier is drawn in resolves too.
    cleanup();
    const dir = renderRow(folder("installed://alderco/codesense/codesense content/docs"));
    expect(dir.querySelector('.cs-agent-ctx-tag [aria-hidden="true"]')).toHaveTextContent("repo");
  });

  it("a folder with only some files in the set says so in words ('some'), not colour alone", () => {
    const wrap = renderRow(folder(`${ROOT}/.claude`));
    expect(wrap).toHaveAttribute("data-agent-context", "partial");
    expect(wrap).not.toHaveClass("cs-agent-ctx-out");
    expect(wrap.querySelector('.cs-agent-ctx-tag [aria-hidden="true"]')).toHaveTextContent("some");
    expect(wrap.querySelector(".cs-agent-ctx-tag .sr-only")).toHaveTextContent(
      "partly in Barb's context",
    );
  });

  it("repo-tier rows are browse-only: no edit path, though they carry the overlay", () => {
    expect(ctx.editRelOf(`${REPO_TIER}/docs/repo-adr.md`)).toBeNull();
    expect(ctx.editRelOf("installed://alderco/codesense/codesense content/docs")).toBeNull();
    expect(ctx.editRelOf(`${ROOT}/docs/architecture`)).toBe("docs/architecture");
  });

  it("leaves rows untouched with no agent picked, or with no content path", () => {
    render(
      <>
        {withAgentContext(
          folder(`${ROOT}/_bmad-output`),
          <div data-testid="bare">x</div>,
          undefined,
        )}
      </>,
    );
    expect(screen.getByTestId("bare").parentElement).not.toHaveAttribute("data-agent-context");
    const pinned = { type: "pinned-header", depth: 0, count: 1, collapsed: false } as FlatNavItem;
    render(<>{withAgentContext(pinned, <div data-testid="pin">p</div>, ctx)}</>);
    expect(screen.getByTestId("pin").parentElement).not.toHaveAttribute("data-agent-context");
  });
});
