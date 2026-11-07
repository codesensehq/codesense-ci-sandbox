// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@/test-utils";

import { SettingsSidebar, SECTION_NAV_ITEMS } from "./settings-sidebar";

// Stub Tooltip to render children directly — avoids Radix portal issues in happy-dom.
vi.mock("@/components/ui/tooltip", () => ({
  Tooltip: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  TooltipTrigger: ({ children, asChild }: { children: React.ReactNode; asChild?: boolean }) =>
    asChild ? <>{children}</> : <span>{children}</span>,
  TooltipContent: ({ children }: { children: React.ReactNode }) => <span>{children}</span>,
  TooltipProvider: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  InfoTip: ({ children }: { children: React.ReactNode }) => <>{children}</>,
}));

const defaultProps = {
  activeSection: "general" as const,
  onSectionChange: vi.fn(),
  query: "",
  collapsed: false,
};

beforeEach(() => {
  defaultProps.onSectionChange.mockReset();
});

describe("SettingsSidebar — rendering", () => {
  it("renders all 5 nav items with correct labels", () => {
    render(<SettingsSidebar {...defaultProps} />);
    for (const item of SECTION_NAV_ITEMS) {
      expect(screen.getByTestId(`settings-nav-${item.key}`)).toBeInTheDocument();
      expect(screen.getByTestId(`settings-nav-${item.key}`)).toHaveTextContent(item.label);
    }
  });

  it("active section has aria-selected='true', others 'false'", () => {
    render(<SettingsSidebar {...defaultProps} activeSection="repo" />);
    expect(screen.getByTestId("settings-nav-repo")).toHaveAttribute("aria-selected", "true");
    expect(screen.getByTestId("settings-nav-general")).toHaveAttribute("aria-selected", "false");
    expect(screen.getByTestId("settings-nav-account")).toHaveAttribute("aria-selected", "false");
  });

  it("clicking a nav item calls onSectionChange with the correct key", () => {
    render(<SettingsSidebar {...defaultProps} />);
    fireEvent.click(screen.getByTestId("settings-nav-workspace"));
    expect(defaultProps.onSectionChange).toHaveBeenCalledWith("workspace");
  });

  it("has role='tablist' with vertical orientation", () => {
    render(<SettingsSidebar {...defaultProps} />);
    const tablist = screen.getByRole("tablist");
    expect(tablist).toHaveAttribute("aria-orientation", "vertical");
    expect(tablist).toHaveAttribute("aria-label", "Settings sections");
  });
});

describe("SettingsSidebar — keyboard navigation", () => {
  it("ArrowDown moves focus to next item", () => {
    render(<SettingsSidebar {...defaultProps} />);
    const general = screen.getByTestId("settings-nav-general");
    general.focus();
    fireEvent.keyDown(general.parentElement!, { key: "ArrowDown" });
    expect(defaultProps.onSectionChange).toHaveBeenCalledWith("appearance");
  });

  it("ArrowUp wraps from first to last", () => {
    render(<SettingsSidebar {...defaultProps} />);
    const general = screen.getByTestId("settings-nav-general");
    general.focus();
    fireEvent.keyDown(general.parentElement!, { key: "ArrowUp" });
    expect(defaultProps.onSectionChange).toHaveBeenCalledWith("account");
  });

  it("Home jumps to first item", () => {
    render(<SettingsSidebar {...defaultProps} activeSection="account" />);
    const tablist = screen.getByRole("tablist");
    fireEvent.keyDown(tablist, { key: "Home" });
    expect(defaultProps.onSectionChange).toHaveBeenCalledWith("general");
  });

  it("End jumps to last item", () => {
    render(<SettingsSidebar {...defaultProps} />);
    const tablist = screen.getByRole("tablist");
    fireEvent.keyDown(tablist, { key: "End" });
    expect(defaultProps.onSectionChange).toHaveBeenCalledWith("account");
  });

  it("skips dimmed items during ArrowDown navigation", () => {
    // Query "reveal" only matches Repo section (via REPO_SECTION_SPEC keywords).
    // SS-9: previously "verify"; that keyword retired with RepoHealthSection.
    render(<SettingsSidebar {...defaultProps} activeSection="repo" query="reveal" />);
    const tablist = screen.getByRole("tablist");
    fireEvent.keyDown(tablist, { key: "ArrowDown" });
    // Wraps around to the only enabled item
    expect(defaultProps.onSectionChange).toHaveBeenCalledWith("repo");
  });
});

describe("SettingsSidebar — search filtering", () => {
  it("non-matching items get opacity-40 and pointer-events-none", () => {
    render(<SettingsSidebar {...defaultProps} query="repo" />);
    const general = screen.getByTestId("settings-nav-general");
    expect(general.className).toContain("opacity-40");
    expect(general.className).toContain("pointer-events-none");
    const repo = screen.getByTestId("settings-nav-repo");
    expect(repo.className).not.toContain("opacity-40");
  });

  it("all items enabled when query is empty", () => {
    render(<SettingsSidebar {...defaultProps} query="" />);
    for (const item of SECTION_NAV_ITEMS) {
      const el = screen.getByTestId(`settings-nav-${item.key}`);
      expect(el.className).not.toContain("opacity-40");
    }
  });
});

// SS-9: health-dot tests retired. The `repoStatus` prop and the
// per-Repo-section dot were the install-health surface; both are gone.
// The aria-label is now always plain "Repo".
describe("SettingsSidebar — Repo tab (SS-9: no status dot)", () => {
  it("never renders the settings-nav-repo-dot", () => {
    render(<SettingsSidebar {...defaultProps} />);
    expect(screen.queryByTestId("settings-nav-repo-dot")).toBeNull();
  });

  it("Repo tab aria-label is plain (no status suffix)", () => {
    render(<SettingsSidebar {...defaultProps} />);
    expect(screen.getByTestId("settings-nav-repo")).toHaveAttribute("aria-label", "Repo");
  });
});

describe("SettingsSidebar — collapsed mode", () => {
  it("labels are sr-only when collapsed", () => {
    render(<SettingsSidebar {...defaultProps} collapsed />);
    const label = screen.getByTestId("settings-nav-general").querySelector("span");
    expect(label?.className).toContain("sr-only");
  });

  it("labels are visible when not collapsed", () => {
    render(<SettingsSidebar {...defaultProps} collapsed={false} />);
    const label = screen.getByTestId("settings-nav-general").querySelector("span");
    expect(label?.className).not.toContain("sr-only");
  });

  it("sidebar narrows to w-[48px] when collapsed", () => {
    render(<SettingsSidebar {...defaultProps} collapsed />);
    const tablist = screen.getByRole("tablist");
    expect(tablist.className).toContain("w-[48px]");
  });

  it("sidebar is w-[150px] when not collapsed", () => {
    render(<SettingsSidebar {...defaultProps} collapsed={false} />);
    const tablist = screen.getByRole("tablist");
    expect(tablist.className).toContain("w-[150px]");
  });
});
