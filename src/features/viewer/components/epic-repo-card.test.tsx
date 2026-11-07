// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { EpicRepoCard } from "./epic-repo-card";

function setup(overrides: Partial<React.ComponentProps<typeof EpicRepoCard>> = {}) {
  const onReadEpicRepos = vi.fn(async () => ["acme/web"]);
  const onSetEpicRepos = vi.fn(async (_dir: string, repos: string[]) => repos);
  const props: React.ComponentProps<typeof EpicRepoCard> = {
    epicDirPath: "alpha",
    teamSourceRepos: ["acme/web", "acme/api"],
    clonedRepos: new Set(["acme/web", "acme/api"]),
    onReadEpicRepos,
    onSetEpicRepos,
    ...overrides,
  };
  render(<EpicRepoCard {...props} />);
  return { onReadEpicRepos, onSetEpicRepos };
}

describe("EpicRepoCard", () => {
  beforeEach(() => vi.clearAllMocks());

  it("reads the epic's repos on mount and lists them", async () => {
    const { onReadEpicRepos } = setup();
    expect(onReadEpicRepos).toHaveBeenCalledWith("alpha");
    expect(await screen.findByText("acme/web")).toBeInTheDocument();
  });

  it("shows a 'needs clone' marker for repos without a local clone", async () => {
    setup({
      onReadEpicRepos: vi.fn(async () => ["acme/uncloned"]),
      clonedRepos: new Set(),
    });
    expect(await screen.findByTestId("epic-repo-needs-clone-acme/uncloned")).toBeInTheDocument();
  });

  it("the add picker lists only team repos not already on the epic", async () => {
    const user = userEvent.setup();
    setup(); // current = ["acme/web"]; team = ["acme/web","acme/api"]
    await screen.findByText("acme/web");
    await user.click(screen.getByTestId("epic-repo-add-alpha"));
    // acme/api is addable; acme/web is already present (only the list entry).
    expect(screen.getByText("acme/api")).toBeInTheDocument();
  });

  it("add calls onSetEpicRepos with the appended list + assemble:true", async () => {
    const user = userEvent.setup();
    const onSetEpicRepos = vi.fn(async (_d: string, repos: string[]) => repos);
    setup({ onSetEpicRepos });
    await screen.findByText("acme/web");
    await user.click(screen.getByTestId("epic-repo-add-alpha"));
    await user.click(screen.getByText("acme/api"));
    await waitFor(() =>
      expect(onSetEpicRepos).toHaveBeenCalledWith("alpha", ["acme/web", "acme/api"], {
        assemble: true,
      }),
    );
  });

  it("remove calls onSetEpicRepos without the slug + assemble:false", async () => {
    const user = userEvent.setup();
    const onSetEpicRepos = vi.fn(async (_d: string, repos: string[]) => repos);
    setup({ onReadEpicRepos: vi.fn(async () => ["acme/web", "acme/api"]), onSetEpicRepos });
    await screen.findByText("acme/web");
    await user.click(screen.getByRole("button", { name: "Remove acme/web" }));
    await waitFor(() =>
      expect(onSetEpicRepos).toHaveBeenCalledWith("alpha", ["acme/api"], { assemble: false }),
    );
  });

  it("re-renders from the returned list (source of truth)", async () => {
    const user = userEvent.setup();
    // Server drops the add (e.g. validation) — UI must reflect the returned list.
    const onSetEpicRepos = vi.fn(async () => ["acme/web"]);
    setup({ onSetEpicRepos });
    await screen.findByText("acme/web");
    await user.click(screen.getByTestId("epic-repo-add-alpha"));
    await user.click(screen.getByText("acme/api"));
    await waitFor(() => expect(onSetEpicRepos).toHaveBeenCalled());
    // acme/api never lands as a configured row (returned list won).
    await waitFor(() =>
      expect(screen.queryByRole("treeitem", { name: "acme/api" })).not.toBeInTheDocument(),
    );
  });

  it("surfaces a read error", async () => {
    setup({ onReadEpicRepos: vi.fn(async () => Promise.reject(new Error("boom"))) });
    expect(await screen.findByTestId("epic-repo-error-alpha")).toHaveTextContent("boom");
  });

  it("a11y: renders repos as treeitems within a labelled group", async () => {
    setup();
    expect(await screen.findByRole("treeitem", { name: "acme/web" })).toBeInTheDocument();
    expect(screen.getByRole("group", { name: /Repos for alpha/ })).toBeInTheDocument();
  });

  it("a11y: Escape closes the add picker without bubbling to the panel", async () => {
    const user = userEvent.setup();
    const onPanelKeyDown = vi.fn();
    render(
      // Wrap to assert Esc is stopped from bubbling to the panel's Esc handler.
      <div onKeyDown={onPanelKeyDown}>
        <EpicRepoCard
          epicDirPath="alpha"
          teamSourceRepos={["acme/web", "acme/api"]}
          clonedRepos={new Set(["acme/web", "acme/api"])}
          onReadEpicRepos={vi.fn(async () => ["acme/web"])}
          onSetEpicRepos={vi.fn(async (_d, repos) => repos)}
        />
      </div>,
    );
    await screen.findByText("acme/web");
    await user.click(screen.getByTestId("epic-repo-add-alpha"));
    const addOption = screen.getByRole("button", { name: /acme\/api/ });
    expect(addOption).toBeInTheDocument();
    addOption.focus();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("button", { name: /acme\/api/ })).not.toBeInTheDocument();
    expect(onPanelKeyDown).not.toHaveBeenCalled();
  });

  // SLW-FX-15 — design-polish style contracts. happy-dom has no layout, so we
  // assert class/token contracts (SLW-FX-11 pattern), not geometry. Behavior
  // tests above stay verbatim — this is a pure visual/token pass.
  describe("SLW-FX-15 style contracts", () => {
    it("AC-1/2/3: card root is page-scaled with a token surface (no tree indent, no fine-print, no ad-hoc alpha)", async () => {
      setup();
      await screen.findByText("acme/web");
      const cls = screen.getByTestId("epic-repo-card-alpha").className;
      expect(cls).not.toMatch(/\bml-5\b/); // AC-1: left-aligns with header
      expect(cls).toContain("text-sm"); // AC-2: page scale
      expect(cls).not.toMatch(/\btext-xs\b/); // AC-2: not fine-print at root
      expect(cls).not.toContain("bg-card/40"); // AC-3: no diluted alpha
      expect(cls).not.toContain("border-border/60"); // AC-3
      expect(
        /\bcs-workspace-card\b/.test(cls) ||
          (/\bbg-card\b/.test(cls) && /\bborder-border\b/.test(cls)),
      ).toBe(true); // AC-3: raised idiom OR solid token surface
    });

    it("AC-4: needs-clone marker binds a token color, not a raw amber hue", async () => {
      setup({ onReadEpicRepos: vi.fn(async () => ["acme/uncloned"]), clonedRepos: new Set() });
      const marker = await screen.findByTestId("epic-repo-needs-clone-acme/uncloned");
      expect(marker.className).not.toMatch(/amber-\d/);
      expect(marker.className).toMatch(/var\(--cs-|--cs-status|text-\[color:var/);
    });

    it("AC-5: add is a real button (data-slot) and remove is discoverable (not opacity-0 at rest)", async () => {
      setup();
      await screen.findByText("acme/web");
      expect(screen.getByTestId("epic-repo-add-alpha")).toHaveAttribute("data-slot", "button");
      const remove = screen.getByRole("button", { name: "Remove acme/web" });
      expect(remove.className).not.toMatch(/\bopacity-0\b/);
    });

    it("AC-7: error renders inside a token alert container", async () => {
      setup({ onReadEpicRepos: vi.fn(async () => Promise.reject(new Error("boom"))) });
      const err = await screen.findByTestId("epic-repo-error-alpha");
      expect(err).toHaveAttribute("role", "alert");
      expect(err).toHaveTextContent("boom");
      expect(err.className).toMatch(/border-destructive|bg-destructive\/10/);
    });

    it("AC-8: empty state shows text-sm guidance and the add button as the CTA", async () => {
      setup({ onReadEpicRepos: vi.fn(async () => []) });
      const add = await screen.findByTestId("epic-repo-add-alpha");
      expect(add).toHaveAttribute("data-slot", "button");
      expect(screen.getByText(/No repos yet/i).className).toContain("text-sm");
    });
  });
});
