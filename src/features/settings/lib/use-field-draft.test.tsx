// SPDX-License-Identifier: MIT
import { render, screen, fireEvent, cleanup, waitFor, act } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { useFieldDraft } from "./use-field-draft";
afterEach(cleanup);
function Field({ value, commit }: { value: string; commit: (value: string) => Promise<void> }) {
  const field = useFieldDraft({ committed: value, commit, label: "main doc", syncCommitted: true });
  return (
    <input
      aria-label="Main doc"
      value={field.draft}
      onChange={field.onChange}
      onBlur={field.onBlur}
      onKeyDown={field.onKeyDown}
    />
  );
}
it("updates an untouched source convention without committing the stale value on blur", async () => {
  const commit = vi.fn(async () => {});
  const { rerender } = render(<Field value="Old.md" commit={commit} />);
  const input = screen.getByLabelText("Main doc");
  input.focus();
  rerender(<Field value="New.md" commit={commit} />);
  expect(input).toHaveValue("New.md");
  expect(input).toHaveFocus();
  fireEvent.blur(input);
  expect(commit).not.toHaveBeenCalled();
});
it("preserves a dirty draft and lets Escape restore the newest committed convention", async () => {
  const commit = vi.fn(async () => {});
  const { rerender } = render(<Field value="Old.md" commit={commit} />);
  const input = screen.getByLabelText("Main doc");
  input.focus();
  fireEvent.change(input, { target: { value: "Draft.md" } });
  rerender(<Field value="New.md" commit={commit} />);
  expect(input).toHaveValue("Draft.md");
  fireEvent.keyDown(input, { key: "Escape" });
  await waitFor(() => expect(input).toHaveValue("New.md"));
  expect(commit).not.toHaveBeenCalled();
});

it("marks a normalized successful save clean so subsequent live conventions replace it", async () => {
  let saved!: () => void;
  const commit = vi.fn(
    () =>
      new Promise<void>((resolve) => {
        saved = resolve;
      }),
  );
  const { rerender } = render(<Field value="Old.md" commit={commit} />);
  const input = screen.getByLabelText("Main doc");
  fireEvent.change(input, { target: { value: " Plan.md " } });
  fireEvent.blur(input);
  await waitFor(() => expect(saved).toBeTypeOf("function"));
  rerender(<Field value="Plan.md" commit={commit} />);
  await act(async () => saved());
  expect(input).toHaveValue("Plan.md");
  rerender(<Field value="Outside.md" commit={commit} />);
  expect(input).toHaveValue("Outside.md");
  fireEvent.blur(input);
  expect(commit).toHaveBeenCalledTimes(1);
});

it.each([false, true])(
  "cleans a draft matched by a committed update, with no-op blur=%s",
  async (blur) => {
    const commit = vi.fn(async () => {});
    const { rerender } = render(<Field value="Old.md" commit={commit} />);
    const input = screen.getByLabelText("Main doc");
    fireEvent.change(input, { target: { value: "Pending.md" } });
    rerender(<Field value="Pending.md" commit={commit} />);
    if (blur) fireEvent.blur(input);
    rerender(<Field value="Outside.md" commit={commit} />);
    expect(input).toHaveValue("Outside.md");
    expect(commit).not.toHaveBeenCalled();
  },
);
