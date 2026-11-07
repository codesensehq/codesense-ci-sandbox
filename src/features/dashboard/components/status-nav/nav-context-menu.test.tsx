// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { NavContextMenu, type NavSprintMenu } from "./nav-context-menu";
import type { NavContextMenuState } from "./types";
import type { StoryAgentLaunchActions } from "@/features/story-launch/story-agent-launch";
import { StoryAgentLaunchActionsProvider } from "@/features/story-launch/use-story-agent-launch";
import type { NavAgentContext } from "@/features/agent-context/lib/nav-agent-context";

// The sprint section is independent of the file-action / pin
// bundles, so those are stubbed to the minimum the render touches (SW-SB-15).
function stubProps(over: {
  menu: NavContextMenuState;
  sprint: NavSprintMenu;
  mergeMark?: { marked: boolean; onToggle: () => void };
  agentContext?: { context: NavAgentContext; rel: string };
}) {
  return {
    menu: over.menu,
    onClose: vi.fn(),
    actions: {
      menuInherited: false,
      // The hook reads the tree; a dotted-name path stands in for a file row here.
      menuFileGestures: /\.[^/]+$/.test(over.menu.path),
      newFileFromRow: vi.fn(),
      menuScope: null,
      attachCanvas: null,
      canvasAttach: { open: vi.fn() },
      handleCreateCanvas: vi.fn(),
      handleCopyPath: vi.fn(),
      setNewFileDir: vi.fn(),
      setMoveTarget: vi.fn(),
      setRenameTarget: vi.fn(),
      setDeleteTarget: vi.fn(),
    } as any,
    pins: {
      pinnable: false,
      pinKey: null,
      isTeamPinned: false,
      isPersonalPinned: false,
      teamKey: "org:team",
      toggleTeamPin: vi.fn(),
      togglePersonalPin: vi.fn(),
      unpinPinnedRow: vi.fn(),
    },
    parent: {},
    sprint: over.sprint,
    mergeMark: over.mergeMark,
    agentContext: over.agentContext,
  };
}

function sprintBundle(over?: Partial<NavSprintMenu>): NavSprintMenu {
  return {
    targets: [
      { id: "2026-08-A", label: "2026-08-A (active)", active: true },
      { id: "2026-09-A", label: "2026-09-A", active: false },
    ],
    story: null,
    epic: null,
    onSetStory: vi.fn(),
    onRemoveStory: vi.fn(),
    onBulkEpic: vi.fn(),
    ...over,
  };
}

const storyMenu: NavContextMenuState = { x: 0, y: 0, path: "/repo/s.md" };

describe("NavContextMenu — SW-SB-15 sprint items", () => {
  it("an untagged story shows 'Add to {sprint}' for each target and fires onSetStory", () => {
    const sprint = sprintBundle({ story: { filePath: "/repo/s.md", current: null } });
    render(<NavContextMenu {...stubProps({ menu: storyMenu, sprint })} />);

    expect(screen.getByText("Add to 2026-08-A (active)")).toBeInTheDocument();
    expect(screen.getByText("Add to 2026-09-A")).toBeInTheDocument();
    expect(screen.queryByText("Remove from sprint")).toBeNull();

    fireEvent.click(screen.getByText("Add to 2026-08-A (active)"));
    expect(sprint.onSetStory).toHaveBeenCalledWith("/repo/s.md", null, "2026-08-A");
  });

  it("a tagged story shows 'Move to…' (excluding its current) + Remove", () => {
    const sprint = sprintBundle({ story: { filePath: "/repo/s.md", current: "2026-08-A" } });
    render(<NavContextMenu {...stubProps({ menu: storyMenu, sprint })} />);

    // Current sprint is never re-offered; the other target reads "Move to…".
    expect(screen.queryByText("Move to 2026-08-A (active)")).toBeNull();
    expect(screen.getByText("Move to 2026-09-A")).toBeInTheDocument();

    fireEvent.click(screen.getByText("Remove from sprint"));
    expect(sprint.onRemoveStory).toHaveBeenCalledWith("/repo/s.md", "2026-08-A");
  });

  it("an epic dir with an active sprint shows a bulk 'Add to sprint…' that opens the confirm", () => {
    const epicMenu: NavContextMenuState = {
      x: 0,
      y: 0,
      path: "/ctx/epic",
      isEpic: true,
      epicDirPath: "epic",
    };
    const sprint = sprintBundle({ epic: { unassigned: 2, total: 3 } });
    render(<NavContextMenu {...stubProps({ menu: epicMenu, sprint })} />);

    fireEvent.click(screen.getByText("Add to sprint…"));
    expect(sprint.onBulkEpic).toHaveBeenCalledTimes(1);
  });

  it("no epic bulk item when there is no active sprint", () => {
    const epicMenu: NavContextMenuState = {
      x: 0,
      y: 0,
      path: "/ctx/epic",
      isEpic: true,
      epicDirPath: "epic",
    };
    const sprint = sprintBundle({
      targets: [{ id: "2026-09-A", label: "2026-09-A", active: false }],
      epic: { unassigned: 2, total: 3 },
    });
    render(<NavContextMenu {...stubProps({ menu: epicMenu, sprint })} />);
    expect(screen.queryByText("Add to sprint…")).toBeNull();
  });
});

describe("NavContextMenu — SW-CP-27 merge-mark toggle", () => {
  const configMenu: NavContextMenuState = { x: 0, y: 0, path: "/repo/.mcp.json" };

  it("shows 'Mark as merge file' for an unmarked config file and fires onToggle (AC-1)", () => {
    const onToggle = vi.fn();
    render(
      <NavContextMenu
        {...stubProps({
          menu: configMenu,
          sprint: sprintBundle(),
          mergeMark: { marked: false, onToggle },
        })}
      />,
    );
    fireEvent.click(screen.getByText("Mark as merge file"));
    expect(onToggle).toHaveBeenCalledTimes(1);
  });

  it("shows 'Unmark merge file' when already marked (AC-3 label)", () => {
    render(
      <NavContextMenu
        {...stubProps({
          menu: configMenu,
          sprint: sprintBundle(),
          mergeMark: { marked: true, onToggle: vi.fn() },
        })}
      />,
    );
    expect(screen.getByText("Unmark merge file")).toBeInTheDocument();
    expect(screen.queryByText("Mark as merge file")).toBeNull();
  });

  it("shows no merge item when the bundle is absent — a non-markable file (AC-2)", () => {
    // The orchestrator gates on CP-24's format list, so a `.md` row carries no
    // mergeMark bundle → no item ever offered.
    const docMenu: NavContextMenuState = { x: 0, y: 0, path: "/repo/notes.md" };
    render(<NavContextMenu {...stubProps({ menu: docMenu, sprint: sprintBundle() })} />);
    expect(screen.queryByText("Mark as merge file")).toBeNull();
    expect(screen.queryByText("Unmark merge file")).toBeNull();
  });
});

describe("NavContextMenu — story authoring items", () => {
  const epicMenu: NavContextMenuState = {
    x: 0,
    y: 0,
    path: "/content/sample-project",
    isEpic: true,
    epicDirPath: "sample-project",
    epicFilePath: "/content/sample-project/README.md",
  };

  it("an epic row offers New story… with the epic's absolute dir, and no sample removal", () => {
    const storyAuthoring = { onNewStory: vi.fn() };
    render(
      <NavContextMenu
        {...stubProps({ menu: epicMenu, sprint: sprintBundle() })}
        storyAuthoring={storyAuthoring}
      />,
    );
    fireEvent.click(screen.getByText("New story…"));
    expect(storyAuthoring.onNewStory).toHaveBeenCalledWith("/content/sample-project");
    expect(screen.queryByText("Remove sample project")).toBeNull();
  });

  it("a story row offers no New story…", () => {
    render(
      <NavContextMenu
        {...stubProps({ menu: storyMenu, sprint: sprintBundle() })}
        storyAuthoring={{ onNewStory: vi.fn() }}
      />,
    );
    expect(screen.queryByText("New story…")).toBeNull();
  });
});

describe("NavContextMenu — Launch agent", () => {
  const actions: StoryAgentLaunchActions = {
    agents: [
      {
        agentKey: "acme/core/builder",
        name: "Builder",
        runtime: "claude",
        binary: "claude",
        place: null,
        isDefault: true,
        availability: "available",
      },
      {
        agentKey: "acme/core/reviewer",
        name: "Reviewer",
        runtime: "codex",
        binary: "codex",
        place: null,
        isDefault: false,
        availability: "missing",
      },
    ],
    launch: vi.fn(),
    shortcutLabel: "⌘⇧A",
  };

  function renderNav(value: StoryAgentLaunchActions | null, isStory = true) {
    const props = stubProps({ menu: storyMenu, sprint: sprintBundle() });
    render(
      <StoryAgentLaunchActionsProvider value={value}>
        <NavContextMenu {...props} parent={{ isStoryFilePath: () => isStory }} />
      </StoryAgentLaunchActionsProvider>,
    );
    return props;
  }

  it("a story row lists the agents, the default first with its shortcut", () => {
    const props = renderNav(actions);
    const claude = screen.getByTestId("launch-agent-acme/core/builder");
    expect(claude).toHaveTextContent("Builder");
    expect(claude).toHaveTextContent("⌘⇧A");

    fireEvent.click(claude);
    expect(actions.launch).toHaveBeenCalledWith(
      { storyFilePath: "/repo/s.md" },
      expect.objectContaining({ agentKey: "acme/core/builder" }),
    );
    expect(props.onClose).toHaveBeenCalled();
  });

  it("a missing CLI is one install row and is not launchable", () => {
    renderNav(actions);
    const codex = screen.getByTestId("launch-agent-missing-codex");
    expect(codex).toHaveTextContent("Install Codex — `codex` isn't on your PATH");
    expect(codex).toBeDisabled();
    expect(screen.queryByTestId("launch-agent-acme/core/reviewer")).toBeNull();
  });

  it("the launch rows are items of the keyboard menu; a missing CLI is skipped", () => {
    renderNav(actions);
    const claude = screen.getByTestId("launch-agent-acme/core/builder");
    const codex = screen.getByTestId("launch-agent-missing-codex");
    expect(claude).toHaveAttribute("role", "menuitem");
    expect(claude).toHaveAttribute("tabindex", "-1");
    expect(codex).toHaveAttribute("role", "menuitem");
    // Every row of the menu is reachable by the arrows (End lands on the last
    // item), and the disabled install row is never a stop.
    const menu = screen.getByRole("menu");
    const items = screen.getAllByRole("menuitem").filter((el) => !el.hasAttribute("disabled"));
    expect(items).toContain(claude);
    claude.focus();
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(document.activeElement).not.toBe(codex);
    expect(document.activeElement).toBe(items[items.indexOf(claude) + 1]);
  });

  it("no launch items on a non-story row", () => {
    renderNav(actions, false);
    expect(screen.queryByTestId("nav-launch-agent")).toBeNull();
  });

  it("no launch items on a story row before agents load (null provider)", () => {
    renderNav(null);
    expect(screen.queryByTestId("nav-launch-agent")).toBeNull();
  });
});

describe("NavContextMenu — Open CLI on a story row", () => {
  const isStoryFilePath = (p: string) => p === "/repo/s.md";

  it("opens the story's workspace the plain way when the host offers no choice", () => {
    const onOpenStoryWorkspace = vi.fn();
    const props = stubProps({ menu: storyMenu, sprint: sprintBundle() });
    render(<NavContextMenu {...props} parent={{ onOpenStoryWorkspace, isStoryFilePath }} />);
    fireEvent.click(screen.getByRole("menuitem", { name: "Open CLI" }));
    expect(onOpenStoryWorkspace).toHaveBeenCalledWith("/repo/s.md");
    expect(props.onClose).toHaveBeenCalled();
  });

  it("renders the host's Open CLI item (with its machine choice) when given", () => {
    const renderOpenCli = vi.fn(() => <div data-testid="host-open-cli">Open CLI ▸</div>);
    const props = stubProps({ menu: storyMenu, sprint: sprintBundle() });
    render(
      <NavContextMenu
        {...props}
        parent={{ onOpenStoryWorkspace: vi.fn(), renderOpenCli, isStoryFilePath }}
      />,
    );
    expect(screen.getByTestId("host-open-cli")).toBeInTheDocument();
    expect(renderOpenCli).toHaveBeenCalledWith("/repo/s.md", props.onClose);
    expect(screen.queryByText("Open story workspace")).toBeNull();
  });
});

// SW-XS-5 (X4, SPEC §8.3): directory ownership is gone. A top-level folder's
// menu offers no ownership item, and the remaining items close the gap — no
// orphan separators.
describe("NavContextMenu — SW-XS-5 no directory ownership", () => {
  const topLevelDir: NavContextMenuState = { x: 0, y: 0, path: "/repo/docs", epicDirPath: "docs" };

  function renderDirMenu() {
    return render(<NavContextMenu {...stubProps({ menu: topLevelDir, sprint: sprintBundle() })} />);
  }

  it("a top-level folder shows no ownership items (role query)", () => {
    renderDirMenu();
    const names = screen.getAllByRole("menuitem").map((el) => el.textContent?.trim());
    expect(names).toEqual(["Copy path", "New file…"]);
    for (const label of [
      /mark as .*context/i,
      /stop owning/i,
      /move to (team|this repo's) context/i,
      /\bown(s|ing)?\b/i,
    ]) {
      expect(screen.queryByRole("menuitem", { name: label })).toBeNull();
    }
  });

  // A realistic top-level EPIC folder: pinnable, an epic workspace, story
  // authoring and a sprint bulk-add — every
  // group that can sit around where the ownership group used to be.
  function renderRealisticEpicDirMenu(extra: Record<string, unknown> = {}) {
    const menu: NavContextMenuState = {
      x: 0,
      y: 0,
      path: "/ctx/plans",
      isEpic: true,
      epicDirPath: "plans",
      epicFilePath: "/ctx/plans/README.md",
    };
    const props = stubProps({
      menu,
      sprint: sprintBundle({ epic: { unassigned: 2, total: 3 } }),
    });
    return render(
      <NavContextMenu
        {...props}
        pins={{ ...props.pins, pinnable: true, pinKey: "plans" }}
        parent={{ onOpenEpicTerminal: vi.fn() }}
        storyAuthoring={{ onNewStory: vi.fn() }}
        {...(extra as object)}
      />,
    );
  }

  it("a realistic folder menu: full item list, no ownership, no orphan separators (DOM snapshot)", () => {
    renderRealisticEpicDirMenu();
    const menu = screen.getByRole("menu");
    const kids = Array.from(menu.querySelectorAll('[role="menuitem"], [role="separator"], hr')).map(
      (el) =>
        el.getAttribute("role") === "menuitem" ? `item: ${el.textContent?.trim()}` : "separator",
    );
    expect(kids).toMatchInlineSnapshot(`
      [
        "item: Pin to top (team)",
        "item: Pin for me only",
        "item: Open epic workspace",
        "item: New story…",
        "item: Add to sprint…",
        "item: Copy path",
        "item: New file…",
      ]
    `);
    expect(kids.some((k) => /\bown(s|ing)?\b|context/i.test(k))).toBe(false);
    expect(kids[0]).not.toBe("separator");
    expect(kids[kids.length - 1]).not.toBe("separator");
    kids.forEach((k, i) => {
      if (i > 0) expect(k === "separator" && kids[i - 1] === "separator").toBe(false);
    });
  });
});

describe("NavContextMenu — SW-XS-15 agent context items", () => {
  function agentCtx(over: Partial<NavAgentContext> = {}): NavAgentContext {
    return {
      agentName: "Barb",
      repoName: "codesense",
      relOf: () => null,
      editRelOf: () => null,
      defaulted: false,
      membership: () => ({ state: "out" }),
      plan: () => ({ kind: "edit", optimistic: "in", makesExplicit: false }),
      edit: vi.fn(),
      ...over,
    };
  }
  const dirMenu: NavContextMenuState = { x: 0, y: 0, path: "/ctx/content/docs/architecture" };

  it("AC-2/AC-5: a row outside the set offers Add (and 'for codesense only') as reachable menu items", () => {
    const context = agentCtx();
    const props = stubProps({
      menu: dirMenu,
      sprint: sprintBundle(),
      agentContext: { context, rel: "docs/architecture" },
    });
    render(<NavContextMenu {...props} />);
    const items = screen.getAllByRole("menuitem").map((el) => el.textContent);
    expect(items).toContain("Add to Barb's context");
    expect(items).toContain("Add to Barb's context for codesense only");
    // Arrow keys reach them like every other item.
    const menu = screen.getByRole("menu");
    const add = screen.getByTestId("nav-agent-context-add");
    add.focus();
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(screen.getByTestId("nav-agent-context-add-repo")).toHaveFocus();
    fireEvent.keyDown(menu, { key: "Enter" });
    expect(context.edit).toHaveBeenCalledWith("docs/architecture", "add", true);
    expect(props.onClose).toHaveBeenCalled();
  });

  it("a row in the set offers Remove; without a repo there is no 'for … only' item", () => {
    render(
      <NavContextMenu
        {...stubProps({
          menu: dirMenu,
          sprint: sprintBundle(),
          agentContext: {
            context: agentCtx({
              repoName: null,
              membership: () => ({ state: "in", tier: "team" }),
            }),
            rel: "docs",
          },
        })}
      />,
    );
    expect(screen.getByText("Remove from Barb's context")).toBeInTheDocument();
    expect(screen.queryByText(/for .* only/)).toBeNull();
    expect(screen.queryByText("Add to Barb's context")).toBeNull();
  });

  it("an edit that cannot be made here stays listed, marked unavailable, with the reason", () => {
    render(
      <NavContextMenu
        {...stubProps({
          menu: dirMenu,
          sprint: sprintBundle(),
          agentContext: {
            context: agentCtx({
              membership: () => ({ state: "in" }),
              plan: (_rel, _a, repoOnly) =>
                repoOnly
                  ? { kind: "edit", optimistic: "out", makesExplicit: false }
                  : { kind: "blocked", reason: "Included through .claude/skills." },
            }),
            rel: ".claude/skills/review",
          },
        })}
      />,
    );
    const teamWide = screen.getByTestId("nav-agent-context-remove");
    expect(teamWide).toHaveAttribute("aria-disabled", "true");
    expect(teamWide).toHaveTextContent("Included through .claude/skills.");
    expect(screen.getByTestId("nav-agent-context-remove-repo")).not.toHaveAttribute(
      "aria-disabled",
    );
  });

  it("a partly included folder offers both Add and Remove", () => {
    render(
      <NavContextMenu
        {...stubProps({
          menu: dirMenu,
          sprint: sprintBundle(),
          agentContext: {
            context: agentCtx({ repoName: null, membership: () => ({ state: "partial" }) }),
            rel: ".claude",
          },
        })}
      />,
    );
    expect(screen.getByText("Add to Barb's context")).toBeInTheDocument();
    expect(screen.getByText("Remove from Barb's context")).toBeInTheDocument();
  });

  it("a first team-wide edit says it makes the context explicit for every launch", () => {
    render(
      <NavContextMenu
        {...stubProps({
          menu: dirMenu,
          sprint: sprintBundle(),
          agentContext: {
            context: agentCtx({
              plan: (_rel, _a, repoOnly) => ({
                kind: "edit",
                optimistic: "in",
                makesExplicit: !repoOnly,
              }),
            }),
            rel: "docs",
          },
        })}
      />,
    );
    expect(screen.getByTestId("nav-agent-context-add")).toHaveTextContent(
      "Makes Barb's context explicit for every launch.",
    );
    expect(screen.getByTestId("nav-agent-context-add-repo")).not.toHaveTextContent("explicit");
  });

  it("no agent picked (Team (everything)) → no agent context items", () => {
    render(<NavContextMenu {...stubProps({ menu: dirMenu, sprint: sprintBundle() })} />);
    expect(screen.queryByTestId("nav-agent-context")).toBeNull();
  });
});

describe("NavContextMenu — SW-XS-11 no empty groups, no orphan separators", () => {
  const launch: StoryAgentLaunchActions = {
    agents: [
      {
        agentKey: "acme/core/builder",
        name: "Builder",
        runtime: "claude",
        binary: "claude",
        place: null,
        isDefault: true,
        availability: "available",
      },
    ],
    launch: vi.fn(),
    shortcutLabel: "⌘⇧A",
  };
  const barb: NavAgentContext = {
    agentName: "Barb",
    repoName: "codesense",
    relOf: () => null,
    editRelOf: () => null,
    defaulted: false,
    membership: () => ({ state: "in" }),
    plan: () => ({ kind: "edit", optimistic: "out", makesExplicit: false }),
    edit: vi.fn(),
  };
  const folder: NavContextMenuState = { x: 0, y: 0, path: "/ctx/content/docs" };
  const story: NavContextMenuState = { x: 0, y: 0, path: "/ctx/content/docs/s.md" };

  // The menu draws no separators: groups carry a label instead. So "no orphan
  // separator" means none at all, and "no empty group" means every child of the
  // menu holds a reachable item (an empty wrapper would leave a visible gap).
  function expectWellFormed() {
    const menu = screen.getByRole("menu");
    expect(menu.querySelectorAll('[role="separator"], hr')).toHaveLength(0);
    for (const group of Array.from(menu.querySelectorAll('[role="group"]'))) {
      expect(group.querySelectorAll('[role="menuitem"]').length).toBeGreaterThan(0);
    }
    for (const child of Array.from(menu.children)) {
      const holdsItem =
        child.getAttribute("role") === "menuitem" ||
        child.querySelector('[role="menuitem"]') !== null ||
        (child.textContent ?? "").trim() !== "";
      expect(holdsItem, `empty menu child: ${child.outerHTML}`).toBe(true);
    }
  }

  it.each([
    ["a plain folder", folder, null, false],
    ["a folder with an agent picked", folder, null, true],
    ["a story with launchable agents and an agent picked", story, launch, true],
    ["a story before agents load", story, null, false],
    ["a story with no agents", story, { ...launch, agents: [] }, false],
  ] as const)("%s", (_name, menu, actions, withAgent) => {
    const props = stubProps({
      menu,
      sprint: sprintBundle(),
      ...(withAgent ? { agentContext: { context: barb, rel: "docs" } } : {}),
    });
    render(
      <StoryAgentLaunchActionsProvider value={actions as StoryAgentLaunchActions | null}>
        <NavContextMenu {...props} parent={{ isStoryFilePath: (p: string) => p.endsWith(".md") }} />
      </StoryAgentLaunchActionsProvider>,
    );
    expectWellFormed();
    // Ownership is gone for good: nothing offers to own or mark a folder.
    expect(
      screen.queryByRole("menuitem", { name: /\bown(s|ing)?\b|mark as .*context/i }),
    ).toBeNull();
  });
});

describe("Story trail entry", () => {
  it("offers Trail on a story and passes the exact row path", () => {
    const props = stubProps({ menu: storyMenu, sprint: sprintBundle() }),
      onOpenTrail = vi.fn();
    render(
      <NavContextMenu
        {...props}
        parent={{ isStoryFilePath: (path) => path === storyMenu.path, onOpenTrail }}
      />,
    );
    fireEvent.click(screen.getByRole("menuitem", { name: "Trail" }));
    expect(onOpenTrail).toHaveBeenCalledWith(storyMenu.path);
    expect(props.onClose).toHaveBeenCalled();
  });
  it("omits Trail on a non-story", () => {
    render(
      <NavContextMenu
        {...stubProps({ menu: storyMenu, sprint: sprintBundle() })}
        parent={{ isStoryFilePath: () => false, onOpenTrail: vi.fn() }}
      />,
    );
    expect(screen.queryByRole("menuitem", { name: "Trail" })).toBeNull();
  });
});
