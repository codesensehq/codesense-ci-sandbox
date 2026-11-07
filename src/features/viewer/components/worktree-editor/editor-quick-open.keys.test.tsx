// SPDX-License-Identifier: MIT
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { EditorQuickOpen } from "./editor-quick-open";

// Keyboard edges of the palette: ↓ before the list loads, and go to line
// outside the open file's range.

function palette(over: Partial<Parameters<typeof EditorQuickOpen>[0]> = {}) {
  const props = {
    initialQuery: "",
    files: null as readonly string[] | null,
    filesError: null,
    truncated: false,
    recent: [] as readonly string[],
    lineCount: 10 as number | null,
    onOpenFile: vi.fn(),
    onGoToLine: vi.fn(),
    onClose: vi.fn(),
    ...over,
  };
  return { props, ...render(<EditorQuickOpen {...props} />) };
}

describe("EditorQuickOpen keys", () => {
  it("still opens the first file after ↓ was pressed on an empty list", () => {
    const { props, rerender } = palette();
    const input = screen.getByTestId("worktree-quick-open-input");
    fireEvent.keyDown(input, { key: "ArrowDown" });
    rerender(<EditorQuickOpen {...props} files={["a.ts", "b.ts"]} />);
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onOpenFile).toHaveBeenCalledWith("a.ts", null, null);
  });

  it("lands go to line inside the file, and does nothing with no file open", () => {
    const { props } = palette({ initialQuery: ":999" });
    expect(screen.getByText("Go to line 10")).toBeTruthy();
    fireEvent.keyDown(screen.getByTestId("worktree-quick-open-input"), { key: "Enter" });
    expect(props.onGoToLine).toHaveBeenCalledWith(10, null);

    const none = palette({ initialQuery: ":5", lineCount: null });
    fireEvent.keyDown(screen.getAllByTestId("worktree-quick-open-input")[1]!, { key: "Enter" });
    expect(none.props.onGoToLine).not.toHaveBeenCalled();
  });

  it("finds an open tab the file list leaves out (an ignored .env)", () => {
    const { props } = palette({ initialQuery: "env", files: ["src/a.ts"], recent: [".env"] });
    fireEvent.keyDown(screen.getByTestId("worktree-quick-open-input"), { key: "Enter" });
    expect(props.onOpenFile).toHaveBeenCalledWith(".env", null, null);
  });
});

it("keeps an arrowed path when ranked files arrive after typing", () => {
  const { props, rerender } = palette({
    initialQuery: "ts",
    recent: ["src/alpha.ts", "src/beta.ts"],
  });
  const input = screen.getByTestId("worktree-quick-open-input");
  fireEvent.keyDown(input, { key: "ArrowDown" });
  const chosen = screen
    .getByRole("option", { selected: true })
    .getAttribute("aria-label")!
    .replace(" (open)", "");
  rerender(<EditorQuickOpen {...props} files={["x.ts"]} />);
  fireEvent.keyDown(input, { key: "Enter" });
  expect(props.onOpenFile).toHaveBeenCalledWith(chosen, null, null);
});

it("uses the new first match before a deliberate move, and resets after query edits", () => {
  const { props, rerender } = palette({
    initialQuery: "ts",
    recent: ["src/alpha.ts", "src/beta.ts"],
  });
  const input = screen.getByTestId("worktree-quick-open-input");
  rerender(<EditorQuickOpen {...props} files={["x.ts"]} />);
  expect(screen.getByRole("option", { selected: true })).toHaveAttribute("aria-label", "x.ts");
  fireEvent.keyDown(input, { key: "ArrowDown" });
  fireEvent.change(input, { target: { value: "alpha" } });
  fireEvent.keyDown(input, { key: "Enter" });
  expect(props.onOpenFile).toHaveBeenCalledWith("src/alpha.ts", null, null);
});

it("falls back to the first remaining file when the deliberate path disappears", () => {
  const { props, rerender } = palette({ files: ["a.ts", "b.ts"] });
  const input = screen.getByTestId("worktree-quick-open-input");
  fireEvent.keyDown(input, { key: "ArrowDown" });
  rerender(<EditorQuickOpen {...props} files={["a.ts"]} />);
  fireEvent.keyDown(input, { key: "Enter" });
  expect(props.onOpenFile).toHaveBeenCalledWith("a.ts", null, null);
});
