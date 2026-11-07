// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it, vi } from "vitest";
import { commitTextEditOnBlur, holdTextMenuFocus, textMenuWithin } from "./text-menu-focus";

afterEach(() => {
  document.body.replaceChildren();
  vi.useRealTimers();
});

describe("text menu focus boundary", () => {
  it("commits ordinary blur immediately", () => {
    const field = document.createElement("input");
    const commit = vi.fn();
    commitTextEditOnBlur(field, commit);
    expect(commit).toHaveBeenCalledOnce();
  });

  it("returns focus without committing and commits the latest text only when focus leaves", () => {
    vi.useFakeTimers();
    const field = document.createElement("input");
    const outside = document.createElement("button");
    document.body.append(field, outside);
    field.focus();
    const finish = holdTextMenuFocus(field);
    const commit = vi.fn();
    outside.focus();
    commitTextEditOnBlur(field, () => commit(field.value));
    expect(textMenuWithin(document.body)).toBe(true);
    expect(commit).not.toHaveBeenCalled();
    field.value = "Pasted text";
    finish(true);
    vi.advanceTimersByTime(20);
    expect(field).toHaveFocus();
    expect(commit).not.toHaveBeenCalled();
    expect(textMenuWithin(document.body)).toBe(false);
    outside.focus();
    commitTextEditOnBlur(field, () => commit(field.value));
    expect(commit).toHaveBeenCalledExactlyOnceWith("Pasted text");
  });

  it("outside dismissal commits the current value, while removed fields do not commit", () => {
    vi.useFakeTimers();
    const field = document.createElement("input");
    document.body.append(field);
    const finish = holdTextMenuFocus(field);
    const commit = vi.fn();
    commitTextEditOnBlur(field, () => commit(field.value));
    field.value = "Pasted text";
    finish(false);
    vi.advanceTimersByTime(20);
    expect(commit).toHaveBeenCalledExactlyOnceWith("Pasted text");
    const finishRemoved = holdTextMenuFocus(field);
    commitTextEditOnBlur(field, commit);
    field.remove();
    finishRemoved(false);
    vi.advanceTimersByTime(20);
    expect(commit).toHaveBeenCalledOnce();
  });
});
