// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { loadShim } from "../../../tests/support/page-shim";

const { codesense, posted, deliver } = loadShim();
beforeEach(() => {
  posted.length = 0;
});
afterEach(() => {
  document.body.innerHTML = "";
  vi.useRealTimers();
});
it("suppresses the browser menu on untagged content and classifies row kinds", async () => {
  vi.useFakeTimers();
  for (const kind of ["story", "epic", "file", "agent", "unknown"]) {
    document.body.innerHTML = `<button data-codesense-row='${JSON.stringify({ kind, path: "story.md", sessionId: "s" })}'>Row</button>`;
    const event = new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      clientX: 20,
      clientY: 30,
    });
    document.querySelector("button")!.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
    await vi.runAllTimersAsync();
    expect(posted.at(-1)).toMatchObject({
      type: "codesense:contextmenu",
      target: kind === "unknown" ? "frame" : "row",
    });
  }
  document.body.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
  await vi.runAllTimersAsync();
  expect(posted.at(-1)).toMatchObject({ target: "frame" });
});
it("remembers the clicked field without an id and sends only text capabilities", async () => {
  vi.useFakeTimers();
  document.body.innerHTML = '<input value="first"><textarea>second</textarea>';
  const field = document.querySelector("textarea")!;
  field.focus();
  field.setSelectionRange(1, 4);
  field.dispatchEvent(
    new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 20, clientY: 30 }),
  );
  await vi.runAllTimersAsync();
  expect(posted.at(-1)).toMatchObject({
    target: "text",
    text: { editable: true, hasSelection: true, hasText: true, password: false },
  });
  expect(JSON.stringify(posted.at(-1))).not.toContain("second");
});

it("keeps the clicked field and its selection when focus-in follows a pointer entry", () => {
  document.body.innerHTML = '<input value="first"><textarea>second</textarea>';
  const field = document.querySelector("textarea")!;
  field.focus();
  field.setSelectionRange(1, 4);
  deliver({ type: "codesense:focus-in" });
  expect(document.activeElement).toBe(field);
  expect([field.selectionStart, field.selectionEnd]).toEqual([1, 4]);
});
it("discards a pointer selection when the page owns the context event", async () => {
  vi.useFakeTimers();
  document.body.innerHTML = "<textarea>second</textarea>";
  const field = document.querySelector("textarea")!;
  field.focus();
  field.setSelectionRange(1, 4);
  field.addEventListener("contextmenu", (event) => event.preventDefault(), { once: true });
  field.dispatchEvent(new PointerEvent("pointerdown", { button: 2, bubbles: true }));
  field.dispatchEvent(
    new MouseEvent("contextmenu", {
      button: 2,
      bubbles: true,
      cancelable: true,
      clientX: 20,
      clientY: 10,
    }),
  );
  field.setSelectionRange(0, 0);
  field.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
  await vi.runAllTimersAsync();
  expect(posted.at(-1)).toMatchObject({ target: "text", text: { hasSelection: false } });
});

it("exposes read-only text-menu state until the matching host menu settles", async () => {
  vi.useFakeTimers();
  document.body.innerHTML = '<input value="draft">';
  const input = document.querySelector("input")!;
  const open = async () => {
    input.focus();
    input.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    await vi.runAllTimersAsync();
    return posted.at(-1)!.textId;
  };
  const first = await open();
  expect(codesense.textMenuOpen).toBe(true);
  expect(Object.getOwnPropertyDescriptor(codesense, "textMenuOpen")?.set).toBeUndefined();
  const second = await open();
  deliver({ type: "codesense:text-menu-closed", textId: first });
  expect(codesense.textMenuOpen).toBe(true);
  deliver({ type: "codesense:text-menu-closed", textId: second });
  expect(codesense.textMenuOpen).toBe(false);
});
