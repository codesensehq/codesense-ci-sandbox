// SPDX-License-Identifier: MIT
// @vitest-environment jsdom
import { viewerScript } from "../../../../tests/support/page-shim";
import { afterEach, expect, it, vi } from "vitest";

const shim = viewerScript("LINK_INTERCEPTOR_JS");
let dispose: (() => void) | undefined;
afterEach(() => {
  dispose?.();
  vi.restoreAllMocks();
});

function install() {
  // Capture and remove only this shim's listeners after each run.
  const registrations: [
    EventTarget,
    string,
    EventListenerOrEventListenerObject,
    boolean | AddEventListenerOptions | undefined,
  ][] = [];
  for (const target of [window, document]) {
    const add = target.addEventListener.bind(target);
    vi.spyOn(target, "addEventListener").mockImplementation(
      (
        type: string,
        handler: EventListenerOrEventListenerObject,
        opts?: boolean | AddEventListenerOptions,
      ) => {
        registrations.push([target, type, handler as EventListener, opts]);
        add(type, handler as EventListener, opts);
      },
    );
  }
  window.eval(shim);
  hostMessage("codesense:presentation", { shown: true, focusAllowed: true });
  dispose = () =>
    registrations.forEach(([target, type, handler, opts]) =>
      target.removeEventListener(type, handler, opts),
    );
  const post = vi.spyOn(window.parent, "postMessage").mockImplementation(() => {});
  return post;
}
function key(key: string, options: KeyboardEventInit = {}) {
  const e = new KeyboardEvent("keydown", {
    key,
    code: `Key${key.toUpperCase()}`,
    metaKey: true,
    bubbles: true,
    cancelable: true,
    ...options,
  });
  document.dispatchEvent(e);
  return e;
}

it("lets the page keep a chord it prevented and never forwards composing or plain keys", async () => {
  const post = install();
  const page = (e: KeyboardEvent) => {
    if (e.key === "b") e.preventDefault();
  };
  document.addEventListener("keydown", page);
  key("b");
  document.removeEventListener("keydown", page);
  key("i", { isComposing: true });
  key("i", { metaKey: false });
  key("i", { metaKey: false, shiftKey: true });
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(post).not.toHaveBeenCalled();
});
it("lets a later page window handler keep its chord", async () => {
  const post = install();
  window.addEventListener("keydown", (e) => {
    if (e.key === "b") e.preventDefault();
  });
  key("b");
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(post).not.toHaveBeenCalled();
});
it("forwards key descriptions without preventing browser editing defaults", async () => {
  const post = install();
  for (const k of ["i", "b", "c", "v", "x", "z", "a"]) {
    expect(key(k).defaultPrevented).toBe(false);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(post).toHaveBeenLastCalledWith(
      {
        type: "codesense:shortcut",
        key: k,
        code: `Key${k.toUpperCase()}`,
        metaKey: true,
        ctrlKey: false,
        shiftKey: false,
        altKey: false,
        editorTarget: false,
        textFieldTarget: false,
      },
      "*",
    );
  }
  expect(key("w").defaultPrevented).toBe(true);
  expect(post).toHaveBeenLastCalledWith({ type: "codesense:close-tab" }, "*");
  // ⌘P is an ordinary chord the host matches against its registry.
  expect(key("p").defaultPrevented).toBe(false);
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(post).toHaveBeenLastCalledWith(
    expect.objectContaining({ type: "codesense:shortcut", key: "p", metaKey: true }),
    "*",
  );
});

it("forwards Option chords and function keys but leaves Option+W to the page", async () => {
  const post = install();
  key("j", { metaKey: false, altKey: true });
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(post).toHaveBeenLastCalledWith(
    expect.objectContaining({ type: "codesense:shortcut", key: "j", metaKey: false, altKey: true }),
    "*",
  );
  key("F5", { metaKey: false, code: "F5" });
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(post).toHaveBeenLastCalledWith(
    expect.objectContaining({ type: "codesense:shortcut", key: "F5", code: "F5" }),
    "*",
  );
  expect(key("w", { metaKey: false, altKey: true }).defaultPrevented).toBe(false);
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(post).not.toHaveBeenCalledWith({ type: "codesense:close-tab" }, "*");
});

it("closes only on the exact ⌘W and forwards other W chords and modified arrows", async () => {
  const post = install();
  expect(key("w", { shiftKey: true, altKey: true }).defaultPrevented).toBe(false);
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(post).toHaveBeenLastCalledWith(
    expect.objectContaining({ type: "codesense:shortcut", key: "w", shiftKey: true, altKey: true }),
    "*",
  );
  key("ArrowDown", { code: "ArrowDown" });
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(post).toHaveBeenLastCalledWith(
    expect.objectContaining({ type: "codesense:shortcut", key: "ArrowDown", metaKey: true }),
    "*",
  );
  expect(post).not.toHaveBeenCalledWith({ type: "codesense:close-tab" }, "*");
});

it("delivers host commands to onCommand listeners and registers through the bridge", () => {
  const post = install();
  const seen: unknown[] = [];
  const off = (
    window as unknown as { codesense: { onCommand: (cb: (c: unknown) => void) => () => void } }
  ).codesense.onCommand((c) => seen.push(c));
  window.dispatchEvent(
    new MessageEvent("message", {
      source: window.parent,
      data: { type: "codesense:command", id: "archive", args: { n: 1 }, dispatchId: "d1" },
    }),
  );
  expect(seen).toEqual([{ id: "archive", args: { n: 1 }, dispatchId: "d1" }]);
  off();
  void (
    window as unknown as {
      codesense: { commands: { register: (ids: string[]) => Promise<unknown> } };
    }
  ).codesense.commands.register(["archive"]);
  expect(post).toHaveBeenLastCalledWith(
    expect.objectContaining({
      type: "codesense:invoke",
      command: "commands.register",
      args: { ids: ["archive"] },
    }),
    "*",
  );
});

it("preserves editor focus in forwarded chords even without preventDefault", async () => {
  const post = install();
  const editor = document.createElement("div");
  editor.setAttribute("contenteditable", "true");
  const text = document.createElement("span");
  editor.append(text);
  document.body.append(editor);
  text.dispatchEvent(
    new KeyboardEvent("keydown", {
      key: "[",
      code: "BracketLeft",
      metaKey: true,
      bubbles: true,
      cancelable: true,
    }),
  );
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(post).toHaveBeenLastCalledWith(
    expect.objectContaining({
      type: "codesense:shortcut",
      key: "[",
      editorTarget: true,
    }),
    "*",
  );
  editor.remove();
});

it.each(["input", "textarea", "select"])("reports focus in a plain %s", async (tag) => {
  const post = install();
  const field = document.createElement(tag);
  document.body.append(field);
  field.dispatchEvent(
    new KeyboardEvent("keydown", {
      key: "d",
      code: "KeyD",
      metaKey: true,
      bubbles: true,
      cancelable: true,
    }),
  );
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(post).toHaveBeenLastCalledWith(
    expect.objectContaining({
      type: "codesense:shortcut",
      key: "d",
      editorTarget: false,
      textFieldTarget: true,
    }),
    "*",
  );
  field.remove();
});

it("forwards Ctrl chords, modified arrows and unhandled Esc; the page keeps what it prevents", async () => {
  const post = install();
  key("b", { metaKey: false, ctrlKey: true });
  key("ArrowRight", { code: "ArrowRight" });
  key("ArrowLeft", { metaKey: false, ctrlKey: true, code: "ArrowLeft" });
  key("ArrowUp", { metaKey: false, code: "ArrowUp" });
  const keep = (e: KeyboardEvent) => e.key === "ArrowDown" && e.preventDefault();
  document.addEventListener("keydown", keep);
  key("ArrowDown", { code: "ArrowDown" });
  document.removeEventListener("keydown", keep);
  key("Escape", { metaKey: false, code: "Escape" });
  key("Escape", { metaKey: false, isComposing: true });
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(post.mock.calls.map(([m]) => [m.type, m.key])).toEqual([
    ["codesense:shortcut", "b"],
    ["codesense:shortcut", "ArrowRight"],
    ["codesense:shortcut", "ArrowLeft"],
    ["codesense:key-escape", undefined],
  ]);
  expect(post.mock.calls[0][0]).toMatchObject({ ctrlKey: true, metaKey: false });
});

function hostMessage(type: string, data = {}) {
  window.dispatchEvent(
    new MessageEvent("message", { source: window.parent, data: { type, ...data } }),
  );
}
it("acks flush after async handlers settle, including a rejection, and answers pings", async () => {
  const post = install();
  const cs = (window as any).codesense;
  let finish!: () => void;
  cs.onFlush(
    () =>
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
  );
  cs.onFlush(() => {
    throw new Error("save failed");
  });
  hostMessage("codesense:flush", { id: 1 });
  await Promise.resolve();
  expect(post).not.toHaveBeenCalled();
  finish();
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(post).toHaveBeenLastCalledWith({ type: "codesense:flush-ack", id: 1 }, "*");
  hostMessage("codesense:ping");
  expect(post).toHaveBeenLastCalledWith({ type: "codesense:pong" }, "*");
});
it("delivers slot context and its nav alias, teardown, and default/custom focus-in", async () => {
  install();
  const cs = (window as any).codesense;
  const nav = vi.fn(),
    slot = vi.fn(),
    teardown = vi.fn();
  cs.onSlotContext(slot);
  cs.onNavSelectionChange(nav);
  cs.onTeardown(teardown);
  hostMessage("codesense:slot-context", {
    context: { slotId: "centre", selection: { file: "a" } },
  });
  expect(slot).toHaveBeenCalledWith({ slotId: "centre", selection: { file: "a" } });
  expect(nav).toHaveBeenCalledWith({ file: "a" });
  expect(await cs.getNavSelection()).toEqual({ file: "a" });
  const input = document.createElement("input");
  document.body.append(input);
  vi.spyOn(input, "getClientRects").mockReturnValue({ length: 1 } as DOMRectList);
  vi.spyOn(window, "getComputedStyle").mockReturnValue({
    visibility: "visible",
  } as CSSStyleDeclaration);
  hostMessage("codesense:focus-in");
  expect(document.activeElement).not.toBe(input);
  const focus = vi.fn();
  const off = cs.onFocusIn(focus);
  hostMessage("codesense:focus-in");
  expect(focus).toHaveBeenCalledOnce();
  expect(document.activeElement).not.toBe(input);
  off();
  hostMessage("codesense:teardown");
  expect(teardown).toHaveBeenCalledOnce();
  input.remove();
});

it("keeps an early nav read pending until slot context has a resolved selection", async () => {
  install();
  const cs = (window as any).codesense;
  const result = vi.fn();
  const pending = cs.getNavSelection().then(result);
  hostMessage("codesense:slot-context", { context: { slotId: "centre" } });
  await Promise.resolve();
  expect(result).not.toHaveBeenCalled();
  hostMessage("codesense:slot-context", {
    context: { slotId: "centre", selection: { file: "cold" } },
  });
  await pending;
  expect(result).toHaveBeenCalledExactlyOnceWith({ file: "cold" });
});

it("uses correlated presentation RPCs for menus and popovers", async () => {
  const post = install();
  const cs = (window as any).codesense;
  const args = {
    anchorId: "more",
    anchorRect: { x: 1, y: 2, width: 3, height: 4 },
    row: { kind: "agent", sessionId: "s" },
    items: [{ id: "inspect", label: "Inspect" }],
  };
  const pending = cs.showMenu(args);
  const call = post.mock.calls.at(-1)![0];
  expect(call).toMatchObject({ type: "codesense:invoke", command: "showMenu", args });
  hostMessage("codesense:invoke-result", {
    id: call.id,
    ok: true,
    data: { itemId: "inspect", row: args.row },
  });
  await expect(pending).resolves.toEqual({ itemId: "inspect", row: args.row });
  const popover = cs.showPopover({ kind: "agent-quick-send", ...args });
  const pop = post.mock.calls.at(-1)![0];
  expect(pop.command).toBe("showPopover");
  hostMessage("codesense:invoke-result", { id: pop.id, ok: true, data: { closed: true } });
  await expect(popover).resolves.toEqual({ closed: true });
});
it("returns focus to its anchor, or to the document (never a control) when that anchor leaves", () => {
  install();
  const cs = (window as any).codesense;
  const input = document.createElement("input"),
    anchor = document.createElement("button");
  anchor.id = "more";
  document.body.append(input, anchor);
  vi.spyOn(input, "getClientRects").mockReturnValue({ length: 1 } as DOMRectList);
  vi.spyOn(window, "getComputedStyle").mockReturnValue({
    visibility: "visible",
  } as CSSStyleDeclaration);
  hostMessage("codesense:focus-return", { anchorId: "more" });
  expect(document.activeElement).toBe(anchor);
  anchor.remove();
  hostMessage("codesense:focus-return", { anchorId: "more" });
  expect(document.activeElement).not.toBe(input);
  const focus = vi.fn();
  const off = cs.onFocusReturn(focus);
  hostMessage("codesense:focus-return", { anchorId: "more" });
  expect(focus).toHaveBeenCalledExactlyOnceWith({ anchorId: "more" });
  expect(document.activeElement).not.toBe(input);
  off();
  hostMessage("codesense:focus-return", { anchorId: "more" });
  expect(document.activeElement).not.toBe(input);
  input.remove();
});
it("gets a default menu row from the nearest declared ancestor", async () => {
  const post = install();
  const row = document.createElement("div");
  row.id = "row-anchor";
  row.dataset.codesenseRow = JSON.stringify({ kind: "agent", sessionId: "s" });
  row.innerHTML = '<button id="nested">More</button>';
  document.body.append(row);
  row.firstElementChild!.dispatchEvent(
    new MouseEvent("contextmenu", { clientX: 12, clientY: 34, bubbles: true, cancelable: true }),
  );
  await new Promise((done) => setTimeout(done, 0));
  expect(post).toHaveBeenLastCalledWith(
    expect.objectContaining({
      type: "codesense:contextmenu",
      row: { kind: "agent", sessionId: "s" },
      anchorId: "row-anchor",
      anchorRect: { x: 12, y: 34, width: 0, height: 0 },
    }),
    "*",
  );
  row.remove();
});
it("lets a page own its context menu and routes undeclared space to the host", async () => {
  const post = install(),
    cs = (window as any).codesense;
  const row = document.createElement("button");
  row.dataset.codesenseRow = JSON.stringify({ kind: "agent", sessionId: "s" });
  document.body.append(row);
  const handled = (event: Event) => event.preventDefault();
  window.addEventListener("contextmenu", handled);
  row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
  await new Promise((done) => setTimeout(done, 0));
  expect(post).not.toHaveBeenCalled();
  window.removeEventListener("contextmenu", handled);
  const declared = () => {
    void cs.showMenu({ items: [] });
  };
  row.addEventListener("contextmenu", declared);
  row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
  await new Promise((done) => setTimeout(done, 0));
  expect(post.mock.calls.filter(([m]) => m.type === "codesense:contextmenu")).toHaveLength(0);
  row.remove();
  const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
  document.body.dispatchEvent(event);
  expect(event.defaultPrevented).toBe(true);
  await new Promise((done) => setTimeout(done, 0));
  expect(post.mock.calls.filter(([m]) => m.type === "codesense:contextmenu")).toEqual([
    [expect.objectContaining({ type: "codesense:contextmenu", target: "frame" }), "*"],
  ]);
});

it("advertises menu callbacks and acknowledges the unread hold after delivery", () => {
  const post = install();
  const callback = vi.fn();
  const cs = (window as any).codesense;
  const unsubscribe = cs.onMenuAction(callback);
  expect(post).toHaveBeenLastCalledWith(
    expect.objectContaining({ type: "codesense:menu-actions", enabled: true }),
    "*",
  );
  const row = { kind: "message", messageId: 10 };
  window.dispatchEvent(
    new MessageEvent("message", {
      source: window.parent,
      data: { type: "codesense:menu-action", id: "hold", itemId: "mark-unread", row },
    }),
  );
  expect(callback).toHaveBeenCalledWith({ itemId: "mark-unread", row });
  expect(post).toHaveBeenLastCalledWith(
    expect.objectContaining({ type: "codesense:menu-action-ack", id: "hold" }),
    "*",
  );
  expect(callback.mock.invocationCallOrder[0]).toBeLessThan(post.mock.invocationCallOrder.at(-1)!);
  unsubscribe();
  expect(post).toHaveBeenLastCalledWith(
    expect.objectContaining({ type: "codesense:menu-actions", enabled: false }),
    "*",
  );
});

it("frozen input keeps navigation and app shortcuts while blocking edits and submissions", async () => {
  const post = install();
  hostMessage("codesense:freeze", { frozen: true });
  for (const nav of ["Tab", "Escape"]) {
    expect(key(nav, { metaKey: false }).defaultPrevented).toBe(false);
  }
  expect(key("Tab", { metaKey: false, shiftKey: true }).defaultPrevented).toBe(false);
  expect(key("p").defaultPrevented).toBe(true);
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(post).toHaveBeenCalledWith(
    expect.objectContaining({ type: "codesense:shortcut", key: "p" }),
    "*",
  );
  expect(post).toHaveBeenCalledWith(expect.objectContaining({ type: "codesense:key-escape" }), "*");
  for (const edit of ["a", "Enter", "Backspace", "Delete"]) {
    expect(key(edit, { metaKey: false }).defaultPrevented).toBe(true);
  }
  for (const edit of ["x", "v", "z", "y"]) expect(key(edit).defaultPrevented).toBe(true);
  for (const type of [
    "beforeinput",
    "cut",
    "paste",
    "compositionstart",
    "compositionupdate",
    "compositionend",
    "submit",
  ]) {
    const event = new Event(type, { bubbles: true, cancelable: true });
    document.dispatchEvent(event);
    expect(event.defaultPrevented, type).toBe(true);
  }
});

it("frozen arrows cannot reach native controls or page editing handlers; app chords still reach the host", async () => {
  const post = install();
  hostMessage("codesense:freeze", { frozen: true });
  const edit = vi.fn();
  document.addEventListener("keydown", edit);
  for (const k of ["ArrowRight", "Home", "End", "PageUp", "PageDown"]) {
    expect(key(k, { metaKey: false }).defaultPrevented).toBe(true);
  }
  key("ArrowRight", { metaKey: false, altKey: true });
  key("p");
  await new Promise((resolve) => setTimeout(resolve, 0));
  document.removeEventListener("keydown", edit);
  expect(edit).not.toHaveBeenCalled();
  expect(post).toHaveBeenCalledWith(
    expect.objectContaining({ type: "codesense:shortcut", key: "p" }),
    "*",
  );
});

// jsdom does not scroll or move real focus between windows; the scrollTop and Enter
// assertions live in tests/frame-security/html-focus-return.spec.ts (WebKit).
it("focus-in and focus-return never focus the doc's first control; the anchor is restored with preventScroll", () => {
  install();
  const first = document.createElement("input");
  const anchor = document.createElement("button");
  anchor.id = "back";
  document.body.append(first, anchor);
  const focus = vi.spyOn(HTMLElement.prototype, "focus");
  const windowFocus = vi.spyOn(window, "focus").mockImplementation(() => {});
  hostMessage("codesense:focus-in");
  expect(document.activeElement).not.toBe(first);
  expect(windowFocus).toHaveBeenCalledTimes(1);
  hostMessage("codesense:focus-return", { anchorId: "missing" });
  expect(document.activeElement).not.toBe(first);
  expect(windowFocus).toHaveBeenCalledTimes(2);
  hostMessage("codesense:focus-return", { anchorId: "back" });
  expect(document.activeElement).toBe(anchor);
  expect(focus).toHaveBeenCalledWith({ preventScroll: true });
  expect(focus.mock.instances).not.toContain(first);
  first.remove();
  anchor.remove();
});
