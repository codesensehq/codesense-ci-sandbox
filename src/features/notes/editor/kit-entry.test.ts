// SPDX-License-Identifier: MIT
// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { createEditor } from "./kit-entry";
import { STATUS_MARKS } from "@/shared/lib/status-mark";

const editors: ReturnType<typeof createEditor>[] = [];
afterEach(() => {
  editors.splice(0).forEach((e) => e.destroy());
  document.body.replaceChildren();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});
function mount(options: Parameters<typeof createEditor>[1] = {}) {
  const el = document.createElement("div");
  document.body.append(el);
  const api = createEditor(el, options);
  editors.push(api);
  return { el, api, root: el.querySelector<HTMLElement>(".scratch-note-editor")! };
}
it("loads silently, emits canonical Markdown for edits and toggles editability silently", () => {
  const onChange = vi.fn();
  const onHeadings = vi.fn();
  const { api, root } = mount({ markdown: "# Before", onChange, onHeadings });
  expect(onChange).not.toHaveBeenCalled();
  api.setMarkdown("## After\n\n- [ ] Task", { emit: false });
  expect(api.getHeadings()).toMatchObject([{ text: "After", level: 2, slug: "after" }]);
  expect(root.querySelector(".nt-heading-count")?.textContent).toBe("1");
  expect(onChange).not.toHaveBeenCalled();
  api.setEditable(false);
  expect(root.getAttribute("contenteditable")).toBe("false");
  api.setEditable(true);
  expect(onChange).not.toHaveBeenCalled();
  api.commands.setHeading(1);
  expect(onChange).toHaveBeenLastCalledWith(api.getMarkdown());
  expect(onHeadings).toHaveBeenLastCalledWith(api.getHeadings());
});
it("offers review tints and removed lines from the shared diff", () => {
  const { api, root } = mount({ markdown: "Changed" });
  expect(api.setChangeOverlay({ baseline: "Old", review: true })).toEqual({ removed: ["Old"] });
  expect(root.querySelector(".nt-chg-add")).not.toBeNull();
  expect(api.setChangeOverlay(null)).toEqual({ removed: [] });
  expect(root.querySelector(".nt-chg")).toBeNull();
});
it("renders and refreshes all twelve native chip marks without changing saved bytes", () => {
  let mark = STATUS_MARKS[0];
  const onAgentClick = vi.fn();
  const { api, root } = mount({
    markdown: "For [@Old](cs:agent:dev)",
    agents: {
      lookup: () => ({ name: "Ricky", status: { mark, word: "WORKING" }, avatar: "/face.svg" }),
    },
    onAgentClick,
  });
  for (const value of STATUS_MARKS) {
    mark = value;
    api.refreshAgents();
    expect(root.querySelector(`.cs-mark-${value} svg`)).not.toBeNull();
  }
  expect(root.querySelector(".nt-agent-chip-name")?.textContent).toBe("Ricky");
  expect(root.querySelector(".nt-agent-chip-status")?.textContent).toBe("working");
  root.querySelector<HTMLButtonElement>(".nt-agent-chip")!.click();
  expect(onAgentClick).toHaveBeenCalledWith("dev");
  expect(api.getMarkdown()).toBe("For [@Old](cs:agent:dev)");
});
it("keeps link activation from reaching the frame interceptor", () => {
  const onLinkClick = vi.fn(),
    intercepted = vi.fn();
  const { root } = mount({ markdown: "[Note](cs:note:abc)", onLinkClick });
  document.addEventListener("click", intercepted);
  root.querySelector<HTMLAnchorElement>("a")!.click();
  document.removeEventListener("click", intercepted);
  expect(onLinkClick).toHaveBeenCalledExactlyOnceWith("cs:note:abc");
  expect(intercepted).not.toHaveBeenCalled();
});
it("edits a code language, cancels Escape and sends the code text", () => {
  const onCodeSend = vi.fn();
  const { api, root } = mount({ markdown: "```typescript\nconst x = 1;\n```", onCodeSend });
  const label = () => root.querySelector<HTMLButtonElement>(".scratch-code-block-lang-label")!;
  label().click();
  let input = root.querySelector<HTMLInputElement>("input")!;
  input.value = "rust";
  const escape = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
  input.dispatchEvent(escape);
  expect(escape.defaultPrevented).toBe(true);
  expect(label().textContent).toBe("typescript");
  label().click();
  input = root.querySelector<HTMLInputElement>("input")!;
  input.value = " RUST ";
  input.dispatchEvent(
    new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
  );
  expect(api.getMarkdown()).toContain("```rust");
  root.querySelector<HTMLButtonElement>(".scratch-code-block-send")!.click();
  expect(onCodeSend).toHaveBeenCalledWith(
    "const x = 1;",
    expect.objectContaining({ top: 0, left: 0 }),
  );
});
it("keeps the language field only while the shim holds its text menu", () => {
  vi.useFakeTimers();
  const menu = { textMenuOpen: true };
  vi.stubGlobal("codesense", menu);
  const { root } = mount({ markdown: "```typescript\nconst x = 1;\n```" });
  root.querySelector<HTMLButtonElement>(".scratch-code-block-lang-label")!.click();
  const input = root.querySelector<HTMLInputElement>(".scratch-code-block-lang-input")!;
  menu.textMenuOpen = true;
  input.blur();
  vi.advanceTimersByTime(200);
  expect(input.isConnected).toBe(true);

  menu.textMenuOpen = false;
  input.focus();
  menu.textMenuOpen = true;
  input.blur();
  vi.advanceTimersByTime(50);
  menu.textMenuOpen = false;
  input.focus();
  vi.advanceTimersByTime(200);
  expect(input.isConnected).toBe(true);

  input.blur();
  vi.advanceTimersByTime(151);
  expect(root.querySelector(".scratch-code-block-lang-input")).toBeNull();
});

it("reads the send target and appends an agent without exposing a bridge", () => {
  const { api } = mount({ markdown: "Send this" });
  expect(api.commands.sendTarget()?.text).toBe("Send this");
  expect(api.commands.appendAgentRef(0, { handle: "dev", name: "Ricky" })).toBe(true);
  expect(api.getMarkdown()).toBe("Send this [@Ricky](cs:agent:dev)");
});
it("places removed lines between their surviving neighbours only during review", () => {
  const { api, root } = mount({ markdown: "First\n\nLast" });
  api.setChangeOverlay({ baseline: "First\n\nRemoved\n\nLast", review: true });
  expect([...root.children].map((el) => el.textContent)).toEqual(["First", "Removed", "Last"]);
  expect(root.querySelector<HTMLElement>(".nt-removed")?.contentEditable).toBe("false");
  expect(api.getMarkdown()).toBe("First\n\nLast");
  api.setChangeOverlay({ baseline: "First\n\nRemoved\n\nLast", review: false });
  expect(root.querySelector(".nt-removed")).toBeNull();
});
it("consumes both send chords even when the page callback returns false", () => {
  const onSend = vi.fn(() => false);
  const { api, root } = mount({ markdown: "Words", onSend });
  for (const altKey of [false, true]) {
    const event = new KeyboardEvent("keydown", {
      key: "Enter",
      metaKey: true,
      keyCode: 13,
      altKey,
      bubbles: true,
      cancelable: true,
    });
    root.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
    expect(api.getMarkdown()).toBe("Words");
  }
  expect(onSend.mock.calls).toEqual([[false], [true]]);
});
it("keeps repeated removed lines in order and includes a removal at the end", () => {
  const { api, root } = mount({ markdown: "First\n\nSame\n\nLast" });
  api.setChangeOverlay({ baseline: "First\n\nSame\n\nSame\n\nLast\n\nTail", review: true });
  expect([...root.children].map((el) => el.textContent)).toEqual([
    "First",
    "Same",
    "Same",
    "Last",
    "Tail",
  ]);
  expect([...root.querySelectorAll(".nt-removed")].map((el) => el.textContent)).toEqual([
    "Same",
    "Tail",
  ]);
  api.setMarkdown("First\n\nNew\n\nSame\n\nLast");
  expect([...root.querySelectorAll(".nt-removed")].map((el) => el.textContent)).toEqual([
    "Same",
    "Tail",
  ]);
});

it("leaves a plain Escape unprevented for the pane and host", () => {
  const { api, root } = mount({ markdown: "Words" });
  api.focus("end");
  const event = new KeyboardEvent("keydown", {
    key: "Escape",
    keyCode: 27,
    bubbles: true,
    cancelable: true,
  });
  root.dispatchEvent(event);
  expect(event.defaultPrevented).toBe(false);
  expect(api.getMarkdown()).toBe("Words");
});

it("lets Escape then either Tab direction leave code, and clears the escape on another key", () => {
  const { api, root } = mount({ markdown: "```plaintext\nx\n```" });
  const key = (key: string, extra: KeyboardEventInit = {}) => {
    const event = new KeyboardEvent("keydown", {
      key,
      bubbles: true,
      cancelable: true,
      ...extra,
    });
    root.dispatchEvent(event);
    return event;
  };
  for (const shiftKey of [false, true]) {
    expect(key("Escape").defaultPrevented).toBe(false);
    if (shiftKey) key("Shift", { shiftKey: true });
    expect(key("Tab", { shiftKey }).defaultPrevented).toBe(false);
    expect(api.getMarkdown()).toBe("```plaintext\nx\n```");
  }
  key("Escape");
  key("ArrowLeft");
  expect(key("Tab").defaultPrevented).toBe(true);
  expect(api.getMarkdown()).toContain("  ");
});

it("relocates a retained send block after editing an earlier paragraph", () => {
  const { api, root } = mount({ markdown: "Before\n\nSend this" });
  api.focus("end");
  const block = api.commands.sendTarget()!.block!;
  api.setMarkdown("An earlier paragraph grew\n\nSend this");
  const pos = api.commands.relocateBlock(block);
  expect(pos).not.toBeNull();
  expect(pos).not.toBe(block.pos);
  api.commands.appendAgentRef(pos!, { handle: "ada", name: "Ada" });
  expect(root.querySelector("p:last-child .nt-agent-chip-name")?.textContent).toBe("Ada");
});
