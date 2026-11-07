// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import type { Heading } from "../lib/note-headings";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { HeadingJumpMount } from "./heading-jump-mount";

function h(level: number, text: string, pos: number, slug: string): Heading {
  return { level, text, pos, slug };
}

describe("HeadingJumpMount (SW-SN-6)", () => {
  let revealSpy: Mock<() => void>;
  let requestJumpSpy: Mock<(slug: string) => void>;

  beforeEach(() => {
    revealSpy = vi.fn<() => void>();
    requestJumpSpy = vi.fn<(slug: string) => void>();
    useViewerStore.setState({
      activeNoteHeadings: [
        h(1, "Intro", 0, "intro"),
        h(2, "TLS handshake timing", 10, "tls-handshake-timing"),
        h(2, "Teardown", 20, "teardown"),
      ],
      revealPanelById: revealSpy,
      requestHeadingJump: requestJumpSpy,
    });
    useUiStore.setState({ headingJumpOpen: false });
  });

  it("does not render the dialog when the view is inactive (gated off)", () => {
    useUiStore.setState({ headingJumpOpen: true });
    render(<HeadingJumpMount isActiveView={false} />);
    expect(screen.queryByPlaceholderText("Go to heading…")).toBeNull();
  });

  it("lists the active note's headings in document order when open + active", () => {
    useUiStore.setState({ headingJumpOpen: true });
    render(<HeadingJumpMount isActiveView={true} />);
    const rows = screen.getAllByRole("option");
    expect(rows[0]).toHaveTextContent("Intro");
    expect(rows[1]).toHaveTextContent("TLS handshake timing");
    expect(rows[2]).toHaveTextContent("Teardown");
  });

  it("on select: reveals the notes surface FIRST, then requests the jump by slug, and closes", async () => {
    useUiStore.setState({ headingJumpOpen: true });
    render(<HeadingJumpMount isActiveView={true} />);
    await userEvent.click(screen.getByText("Teardown"));
    // AC-3: reveal must happen so the scroll lands on a visible editor even from
    // a closed pane — assert it ran, and (via call order) before the jump signal.
    expect(revealSpy).toHaveBeenCalledOnce();
    expect(requestJumpSpy).toHaveBeenCalledWith("teardown");
    expect(revealSpy.mock.invocationCallOrder[0]).toBeLessThan(
      requestJumpSpy.mock.invocationCallOrder[0],
    );
    expect(useUiStore.getState().headingJumpOpen).toBe(false);
  });

  it("typing fuzzy-filters by heading text (AC-1)", async () => {
    useUiStore.setState({ headingJumpOpen: true });
    render(<HeadingJumpMount isActiveView={true} />);
    await userEvent.type(screen.getByPlaceholderText("Go to heading…"), "tls");
    expect(screen.getByText("TLS handshake timing")).toBeInTheDocument();
    expect(screen.queryByText("Intro")).toBeNull();
  });

  it("Enter jumps to the highlighted heading (AC-1 keyboard path)", async () => {
    useUiStore.setState({ headingJumpOpen: true });
    render(<HeadingJumpMount isActiveView={true} />);
    await userEvent.type(screen.getByPlaceholderText("Go to heading…"), "tear{Enter}");
    expect(requestJumpSpy).toHaveBeenCalledWith("teardown");
    expect(useUiStore.getState().headingJumpOpen).toBe(false);
  });

  it("Escape closes without revealing the pane or requesting a jump (AC-3)", async () => {
    useUiStore.setState({ headingJumpOpen: true });
    render(<HeadingJumpMount isActiveView={true} />);
    await userEvent.type(screen.getByPlaceholderText("Go to heading…"), "{Escape}");
    expect(requestJumpSpy).not.toHaveBeenCalled();
    expect(revealSpy).not.toHaveBeenCalled();
    expect(useUiStore.getState().headingJumpOpen).toBe(false);
  });

  it("shows the empty-state label when the note has no headings", () => {
    useViewerStore.setState({ activeNoteHeadings: [] });
    useUiStore.setState({ headingJumpOpen: true });
    render(<HeadingJumpMount isActiveView={true} />);
    expect(screen.getByText("No headings in this note")).toBeInTheDocument();
  });
});
