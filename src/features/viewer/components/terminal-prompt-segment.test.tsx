// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@/test-utils";
import type { GitPromptStatus } from "../hooks/use-session-prompt";
import {
  GitPromptSegment,
  PromptPathLabel,
  gitPromptCounts,
  gitPromptRef,
  gitPromptSummary,
} from "./terminal-prompt-segment";
import { abbreviatePromptPath } from "../lib/prompt-path";

const clean: GitPromptStatus = {
  branch: "main",
  detached: false,
  sha: "0123456789abcdef0123456789abcdef01234567",
  upstream: "origin/main",
  staged: 0,
  modified: 0,
  untracked: 0,
  conflicted: 0,
  ahead: 0,
  behind: 0,
};

describe("GitPromptSegment", () => {
  it("shows the branch alone for a clean checkout", () => {
    render(<GitPromptSegment status={clean} testId="git" />);
    expect(screen.getByTestId("prompt-git-ref").textContent).toBe("main");
    expect(screen.queryByTestId("prompt-git-+")).toBeNull();
    expect(screen.getByTestId("git").getAttribute("aria-label")).toBe("main, clean");
  });

  it("shows compact counts in prompt order, zeros left out", () => {
    const status = { ...clean, staged: 3, modified: 2, untracked: 1, ahead: 4, behind: 1 };
    render(<GitPromptSegment status={status} testId="git" />);
    expect(screen.getByTestId("prompt-git-+").textContent).toBe("+3");
    expect(screen.getByTestId("prompt-git-~").textContent).toBe("~2");
    expect(screen.getByTestId("prompt-git-?").textContent).toBe("?1");
    expect(screen.getByTestId("prompt-git-⇡").textContent).toBe("⇡4");
    expect(screen.getByTestId("prompt-git-⇣").textContent).toBe("⇣1");
    expect(screen.queryByTestId("prompt-git-!")).toBeNull();
    expect(gitPromptCounts(status).map((c) => c.symbol)).toEqual(["+", "~", "?", "⇡", "⇣"]);
  });

  it("colours counts from the status tokens", () => {
    render(<GitPromptSegment status={{ ...clean, staged: 1, modified: 1, conflicted: 1 }} />);
    expect(screen.getByTestId("prompt-git-+").className).toContain("--cs-status-added");
    expect(screen.getByTestId("prompt-git-~").className).toContain("--cs-status-modified");
    expect(screen.getByTestId("prompt-git-!").className).toContain("--cs-status-deleted");
  });

  it("names each count for its tooltip and the whole for assistive tech", () => {
    const status = { ...clean, conflicted: 2, staged: 1, ahead: 1 };
    expect(gitPromptCounts(status).map((c) => c.tip)).toEqual([
      "2 conflicted files",
      "1 staged change",
      "1 commit ahead of origin/main",
    ]);
    expect(gitPromptSummary(status)).toBe(
      "main, 2 conflicted files, 1 staged change, 1 commit ahead of origin/main",
    );
  });

  it("shows the short SHA on a detached HEAD", () => {
    const detached = { ...clean, branch: null, detached: true, upstream: null };
    expect(gitPromptRef(detached)).toBe("0123456");
    render(<GitPromptSegment status={detached} testId="git" />);
    expect(screen.getByTestId("prompt-git-ref").textContent).toBe("0123456");
    expect(gitPromptSummary(detached)).toBe("detached at 0123456, clean");
  });

  it("is a button that opens the diff only when the tab has one", () => {
    const onOpen = vi.fn();
    const { rerender } = render(<GitPromptSegment status={clean} testId="git" />);
    expect(screen.getByTestId("git").tagName).toBe("SPAN");
    rerender(<GitPromptSegment status={clean} testId="git" onOpen={onOpen} />);
    const button = screen.getByTestId("git");
    expect(button.tagName).toBe("BUTTON");
    fireEvent.click(button);
    expect(onOpen).toHaveBeenCalledTimes(1);
    // Already open: still the same button, aria-disabled and inert.
    rerender(<GitPromptSegment status={clean} testId="git" onOpen={onOpen} openDisabled />);
    expect(screen.getByTestId("git")).toBe(button);
    expect(button.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(button);
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("keeps its own press and Enter / Space from reaching the caption", () => {
    const onMouseDown = vi.fn();
    const onKeyDown = vi.fn();
    render(
      <div onMouseDown={onMouseDown} onKeyDown={onKeyDown}>
        <GitPromptSegment status={clean} testId="git" onOpen={vi.fn()} />
      </div>,
    );
    const button = screen.getByTestId("git");
    fireEvent.mouseDown(button);
    fireEvent.keyDown(button, { key: "Enter" });
    fireEvent.keyDown(button, { key: " " });
    expect(onMouseDown).not.toHaveBeenCalled();
    expect(onKeyDown).not.toHaveBeenCalled();
  });
});

describe("PromptPathLabel", () => {
  it("mutes the head and parent and emphasizes the current directory", () => {
    const path = abbreviatePromptPath("/Users/me/a/b/c", "/Users/me");
    render(<PromptPathLabel path={path} emphasis="medium" />);
    expect(screen.getByTestId("prompt-path-parent").textContent).toBe("~/…/b/");
    expect(screen.getByTestId("prompt-path-parent").className).toContain("opacity-60");
    expect(screen.getByTestId("prompt-path-leaf").textContent).toBe("c");
    expect(screen.getByTestId("prompt-path-leaf").className).toContain("font-medium");
    const label = screen.getByTestId("prompt-path");
    expect(label.getAttribute("aria-label")).toBe("~/…/b/c");
    // Never wraps; gives way from the left.
    expect(label.className).toContain("whitespace-nowrap");
    expect(label.className).toContain("[direction:rtl]");
  });

  it("renders the bare root with no muted part", () => {
    render(<PromptPathLabel path={abbreviatePromptPath("/")} emphasis="semibold" />);
    expect(screen.queryByTestId("prompt-path-parent")).toBeNull();
    expect(screen.getByTestId("prompt-path-leaf").textContent).toBe("/");
    expect(screen.getByTestId("prompt-path-leaf").className).toContain("font-semibold");
  });
});
