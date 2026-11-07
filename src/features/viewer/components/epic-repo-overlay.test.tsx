// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { EpicRepoOverlay } from "./epic-repo-overlay";

function setup(overrides: Partial<React.ComponentProps<typeof EpicRepoOverlay>> = {}) {
  const onClose = vi.fn();
  const onReadEpicRepos = vi.fn(async () => ["acme/web"]);
  const onSetEpicRepos = vi.fn(async (_dir: string, repos: string[]) => repos);
  const props: React.ComponentProps<typeof EpicRepoOverlay> = {
    epicDirPath: "alpha",
    teamSourceRepos: ["acme/web", "acme/api"],
    clonedRepos: new Set(["acme/web", "acme/api"]),
    onReadEpicRepos,
    onSetEpicRepos,
    onClose,
    ...overrides,
  };
  render(<EpicRepoOverlay {...props} />);
  return { onClose, onReadEpicRepos, onSetEpicRepos };
}

describe("EpicRepoOverlay (SW-EW-15)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("hosts the reused EpicRepoCard (reads the epic's repos)", async () => {
    const { onReadEpicRepos } = setup();
    expect(onReadEpicRepos).toHaveBeenCalledWith("alpha");
    expect(await screen.findByText("acme/web")).toBeInTheDocument();
  });

  it("the close button dismisses the overlay", async () => {
    const user = userEvent.setup();
    const { onClose } = setup();
    await user.click(screen.getByTestId("epic-repo-overlay-close"));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("Escape closes the overlay AND stops propagation (ancestor handlers must not fire)", () => {
    const onClose = vi.fn();
    // A React ancestor onKeyDown proves the synthetic event is stopped — the
    // terminal itself is a focused-overlay sibling, so it never receives the
    // keystroke structurally; this guards ancestor-level shortcut handlers.
    const ancestorKeydown = vi.fn();
    render(
      <div onKeyDown={ancestorKeydown}>
        <EpicRepoOverlay
          epicDirPath="alpha"
          teamSourceRepos={["acme/web"]}
          clonedRepos={new Set(["acme/web"])}
          onReadEpicRepos={vi.fn(async () => ["acme/web"])}
          onSetEpicRepos={vi.fn(async (_d: string, repos: string[]) => repos)}
          onClose={onClose}
        />
      </div>,
    );
    fireEvent.keyDown(screen.getByTestId("epic-repo-overlay"), { key: "Escape" });
    expect(onClose).toHaveBeenCalledOnce();
    expect(ancestorKeydown).not.toHaveBeenCalled();
  });

  it("grabs focus on mount (pulls the keyboard off the terminal)", () => {
    setup();
    expect(screen.getByTestId("epic-repo-overlay")).toHaveFocus();
  });

  // SLW-FX-15 — the pane reads as a focused dialog surface, not the app bg.
  it("AC-6: overlay pane uses the bg-popover surface token", () => {
    setup();
    expect(screen.getByTestId("epic-repo-overlay").className).toContain("bg-popover");
  });
});
