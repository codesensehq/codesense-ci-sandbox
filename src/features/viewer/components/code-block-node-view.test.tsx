// SPDX-License-Identifier: MIT
import { afterEach, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@/test-utils";
import type { NodeViewProps } from "@tiptap/react";
import type { ReactNode } from "react";
import { holdTextMenuFocus } from "@/shared/lib/text-menu-focus";
import { CodeBlockNodeView } from "./code-block-node-view";

vi.mock("@tiptap/react", () => ({
  NodeViewWrapper: ({ children }: { children: ReactNode }) => <div>{children}</div>,
  NodeViewContent: () => <div />,
}));
afterEach(() => vi.useRealTimers());

it("keeps the language field past its blur timeout while its text menu is open", () => {
  vi.useFakeTimers();
  render(
    <CodeBlockNodeView
      {...({
        node: { attrs: { language: "" } },
        updateAttributes: vi.fn(),
      } as unknown as NodeViewProps)}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "prompt" }));
  const field = screen.getByPlaceholderText("language...");
  const finish = holdTextMenuFocus(field);
  act(() => {
    field.blur();
    vi.advanceTimersByTime(250);
  });
  expect(field).toBeInTheDocument();
  act(() => {
    finish(true);
    vi.advanceTimersByTime(20);
  });
  expect(field).toHaveFocus();
  act(() => {
    field.blur();
    vi.advanceTimersByTime(151);
  });
  expect(screen.queryByPlaceholderText("language...")).toBeNull();
});
