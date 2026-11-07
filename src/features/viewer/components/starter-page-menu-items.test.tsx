// SPDX-License-Identifier: MIT
import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invalidateLandingPages } from "@/features/viewer/lib/team-landing-page";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Org, Team } from "@/shared/types/domain";

import {
  PositionedContextMenu,
  PositionedMenuItem,
} from "@/shared/components/positioned-context-menu";

import { StarterPageMenuItems } from "./starter-page-menu-items";

const resolveTeamLandingPage = vi.fn();
vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  resolveTeamLandingPage: (...args: unknown[]) => resolveTeamLandingPage(...args),
}));
const setTeamLandingPage = vi.fn().mockResolvedValue(undefined);
vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));

const CONTENT = "/orgs/acme/web/content";

function org(team: Team): Org {
  return {
    slug: "acme/ctx",
    display_name: "Acme",
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {},
    people: [],
    teams: [team],
    org_root: "/orgs/acme",
  };
}

async function show(path: string, team: Team = { name: "web", source_repos: [] }) {
  useOrgStore.setState({ orgs: [org(team)], activeOrgSlug: "acme/ctx", activeTeamName: "web" });
  const onClose = vi.fn();
  render(<StarterPageMenuItems path={path} onClose={onClose} />);
  await act(async () => {});
  return onClose;
}

beforeEach(() => {
  invalidateLandingPages();
  setTeamLandingPage.mockClear();
  useOrgStore.setState({ setTeamLandingPage });
  resolveTeamLandingPage.mockResolvedValue({ kind: "found", path: `${CONTENT}/README.md` });
});

describe("StarterPageMenuItems", () => {
  it("sets a team file as the starter page", async () => {
    const onClose = await show(`${CONTENT}/guide/start.md`);
    fireEvent.click(screen.getByRole("menuitem", { name: "Set as team starter page" }));
    expect(onClose).toHaveBeenCalled();
    expect(setTeamLandingPage).toHaveBeenCalledWith("web", "guide/start.md", "acme/ctx");
  });

  it("shows the set page as current, with Clear", async () => {
    resolveTeamLandingPage.mockResolvedValue({ kind: "found", path: `${CONTENT}/guide/start.md` });
    await show(`${CONTENT}/guide/start.md`, {
      name: "web",
      source_repos: [],
      landing_page: "guide/start.md",
    });
    expect(screen.getByTestId("starter-page-current")).toHaveAttribute("aria-disabled", "true");
    expect(screen.getByTestId("starter-page-current")).toHaveTextContent("Team starter page");
    fireEvent.click(screen.getByTestId("starter-page-clear"));
    expect(setTeamLandingPage).toHaveBeenCalledWith("web", null, "acme/ctx");
  });

  it("shows the README default as current, without Clear", async () => {
    await show(`${CONTENT}/README.md`);
    expect(screen.getByTestId("starter-page-current")).toHaveTextContent(
      "Team starter pageOpens by default when web has nothing open.",
    );
    expect(screen.queryByTestId("starter-page-clear")).toBeNull();
  });

  it("keeps the item for a hidden-folder file, unavailable, with why", async () => {
    await show(`${CONTENT}/.claude/notes.md`);
    const item = screen.getByTestId("starter-page-set");
    expect(item).toHaveAttribute("aria-disabled", "true");
    expect(item).toHaveTextContent("hidden folders");
    fireEvent.click(item);
    expect(setTeamLandingPage).not.toHaveBeenCalled();
  });

  it("the keyboard reaches an unavailable item and the current state, and Enter does nothing", async () => {
    resolveTeamLandingPage.mockResolvedValue({ kind: "found", path: `${CONTENT}/guide/start.md` });
    useOrgStore.setState({
      orgs: [org({ name: "web", source_repos: [], landing_page: "guide/start.md" })],
      activeOrgSlug: "acme/ctx",
      activeTeamName: "web",
    });
    const onClose = vi.fn();
    render(
      <PositionedContextMenu position={{ x: 0, y: 0 }} onClose={onClose} label="File actions">
        <PositionedMenuItem>Copy path</PositionedMenuItem>
        <StarterPageMenuItems path={`${CONTENT}/guide/start.md`} onClose={onClose} />
      </PositionedContextMenu>,
    );
    await act(async () => {});
    const menu = screen.getByRole("menu");
    const current = screen.getByRole("menuitemcheckbox", { name: /Team starter page/ });
    expect(current).toHaveAttribute("aria-checked", "true");
    screen.getByRole("menuitem", { name: "Copy path" }).focus();
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(document.activeElement).toBe(current);
    fireEvent.keyDown(menu, { key: "Enter" });
    expect(onClose).not.toHaveBeenCalled();
    expect(setTeamLandingPage).not.toHaveBeenCalled();
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(document.activeElement).toBe(screen.getByTestId("starter-page-clear"));
  });

  it("offers nothing for a file outside the team's content", async () => {
    await show("/src/app/README.md");
    expect(screen.queryByTestId("starter-page-menu")).toBeNull();
  });
});
