// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { NavContextMenu } from "./nav-context-menu";

// The left nav's file menu carries the starter page items for the active team's files.

vi.mock("@/features/viewer/hooks/use-team-starter-page", () => ({
  useActiveTeamStarterPage: () => ({
    org: { slug: "acme/ctx", org_root: "/orgs/acme" },
    team: { name: "web", source_repos: [], landing_page: "guide/start.md" },
    contentRoot: "/orgs/acme/web/content",
    effectivePath: "/orgs/acme/web/content/guide/start.md",
  }),
}));

function props(path: string, menuNodeKind: "file" | "dir" | null) {
  return {
    menu: { x: 0, y: 0, path },
    onClose: vi.fn(),
    actions: {
      menuInherited: false,
      menuFileGestures: menuNodeKind === "file",
      menuNodeKind,
      newFileFromRow: vi.fn(),
      menuScope: null,
      attachCanvas: null,
      canvasAttach: { open: vi.fn() },
      handleCreateCanvas: vi.fn(),
      handleCopyPath: vi.fn(),
      setMoveTarget: vi.fn(),
      setRenameTarget: vi.fn(),
      setDeleteTarget: vi.fn(),
    } as any,
    pins: {
      pinnable: false,
      pinKey: null,
      isTeamPinned: false,
      isPersonalPinned: false,
      teamKey: "acme/ctx:web",
      toggleTeamPin: vi.fn(),
      togglePersonalPin: vi.fn(),
      unpinPinnedRow: vi.fn(),
    },
    parent: {},
  };
}

describe("NavContextMenu — team starter page", () => {
  it("offers Set on a team file (an extensionless one too, by its real kind)", () => {
    render(<NavContextMenu {...props("/orgs/acme/web/content/LICENSE", "file")} />);
    expect(screen.getByRole("menuitem", { name: "Set as team starter page" })).toBeInTheDocument();
  });

  it("shows the set page as current with Clear", () => {
    render(<NavContextMenu {...props("/orgs/acme/web/content/guide/start.md", "file")} />);
    expect(screen.getByTestId("starter-page-current")).toBeInTheDocument();
    expect(screen.getByTestId("starter-page-clear")).toBeInTheDocument();
  });

  it("offers nothing on a folder, even a dotted one", () => {
    render(<NavContextMenu {...props("/orgs/acme/web/content/v1.2", "dir")} />);
    expect(screen.queryByTestId("starter-page-menu")).toBeNull();
  });
});
