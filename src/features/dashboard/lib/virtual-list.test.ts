// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";

const kit = readFileSync("src-tauri/src/http/routes/dashboard_kit/kit.js", "utf8");
const source = readFileSync("src-tauri/src/http/routes/dashboard_kit/virtual-list.js", "utf8");

describe("kit local rows", () => {
  it("windows mixed heights, keeps unchanged nodes, and never subscribes", () => {
    const subscribe = vi.fn();
    (window as any).codesense = { subscribe };
    window.eval(kit);
    window.eval(source);
    const box = document.createElement("div");
    Object.defineProperty(box, "clientHeight", { value: 100 });
    document.body.append(box);
    const render = vi.fn((row: { id: string; label: string }) => {
      const el = document.createElement("button");
      el.textContent = row.label;
      return el;
    });
    const rows = Array.from({ length: 500 }, (_, i) => ({
      id: String(i),
      label: `Row ${i}`,
      height: i === 0 ? 30 : 52,
    }));
    const list = (window as any).codesense.kit.virtualList({
      container: box,
      rows,
      rowHeight: (r: any) => r.height,
      render,
      overscan: 2,
    });
    expect(subscribe).not.toHaveBeenCalled();
    expect(box.querySelectorAll("button").length).toBeLessThan(8);
    const first = box.querySelector("button");
    const next = rows.map((r, i) => (i === 1 ? { ...r, label: "Changed" } : r));
    list.setRows(next);
    expect(box.querySelector("button")).toBe(first);
    const second = box.querySelectorAll("button")[2];
    list.setRows(next.map((r, i) => (i === 1 ? { ...r, height: 84 } : r)));
    expect(box.querySelectorAll("button")[2]).toBe(second);
    expect(second.style.top).toBe("114px");
    list.setRows(next);
    expect(render.mock.calls.filter(([r]) => r.label === "Changed")).toHaveLength(3);
    expect((box.firstChild as HTMLElement).style.height).toBe(`${30 + 499 * 52}px`);
    list.scrollToIndex(400);
    expect(box.scrollTop).toBe(30 + 400 * 52 - 100);
    list.destroy();
    expect(box.childElementCount).toBe(0);
    box.remove();
  });
});

it("restores the same control when a row gains or loses a fold button", () => {
  (window as any).codesense = {};
  window.eval(kit);
  window.eval(source);
  const box = document.createElement("div");
  Object.defineProperty(box, "clientHeight", { value: 100 });
  document.body.append(box);
  const list = (window as any).codesense.kit.virtualList({
    container: box,
    rows: [{ id: "lead", workers: false }],
    rowHeight: 52,
    render: (row: { workers: boolean }) => {
      const el = document.createElement("div");
      el.innerHTML =
        (row.workers ? '<button data-cs-focus-key="fold">Fold</button>' : "") +
        '<button data-cs-focus-key="main">Agent</button>';
      return el;
    },
  });
  box.querySelector<HTMLButtonElement>('[data-cs-focus-key="main"]')!.focus();
  list.setRows([{ id: "lead", workers: true }]);
  expect(document.activeElement).toBe(box.querySelector('[data-cs-focus-key="main"]'));
  box.querySelector<HTMLButtonElement>('[data-cs-focus-key="fold"]')!.focus();
  list.setRows([{ id: "lead", workers: false }]);
  expect(document.activeElement).toBe(box.querySelector('[data-cs-focus-key="main"]'));
  list.destroy();
  box.remove();
});

it("tree mode shares the window, folds descendants and keeps one roving tab stop", () => {
  (window as any).codesense = {};
  window.eval(kit);
  window.eval(source);
  const box = document.createElement("div");
  Object.defineProperty(box, "clientHeight", { value: 96 });
  document.body.append(box);
  const rows = [
    { id: "folder", name: "Folder", children: [{ id: "child", name: "Child" }] },
    ...Array.from({ length: 5000 }, (_, i) => ({ id: `f${i}`, name: `File ${i}` })),
  ];
  const list = (window as any).codesense.kit.virtualList({
    container: box,
    rows,
    tree: true,
    expanded: ["folder"],
    rowHeight: 24,
    render: (r: any) => {
      const el = document.createElement("div");
      el.textContent = r.name;
      return el;
    },
  });
  expect(box.getAttribute("role")).toBe("tree");
  expect(box.querySelectorAll('[role="treeitem"]').length).toBeLessThan(24);
  expect(box.querySelector('[data-key="child"]')?.getAttribute("aria-level")).toBe("2");
  expect(box.querySelector('[data-key="child"]')?.getAttribute("aria-setsize")).toBe("1");
  expect(box.querySelector('[data-key="child"]')?.getAttribute("aria-posinset")).toBe("1");
  expect(box.querySelector('[data-key="f4999"]')).toBeNull();
  expect(box.querySelector('[data-key="folder"]')?.getAttribute("aria-setsize")).toBe("5001");
  expect(box.querySelectorAll('[tabindex="0"]')).toHaveLength(1);
  (box.querySelector('[data-key="folder"]') as HTMLElement).focus();
  box.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }));
  expect(box.querySelector('[data-key="child"]')).toBeNull();
  expect(box.querySelector('[data-key="folder"]')?.getAttribute("aria-expanded")).toBe("false");
  box.dispatchEvent(new KeyboardEvent("keydown", { key: "End", bubbles: true }));
  expect(document.activeElement?.getAttribute("data-key")).toBe("f4999");
  expect(box.querySelectorAll('[tabindex="0"]')).toHaveLength(1);
  list.setRows(rows.filter((row) => row.id !== "f4999"));
  expect(document.activeElement?.getAttribute("data-key")).toBe("f4998");
  list.destroy();
  box.remove();
});

it("typeahead uses visible labels and skeletons are excluded from tree semantics", () => {
  (window as any).codesense = {};
  window.eval(kit);
  window.eval(source);
  const box = document.createElement("div");
  Object.defineProperty(box, "clientHeight", { value: 100 });
  document.body.append(box);
  const list = (window as any).codesense.kit.virtualList({
    container: box,
    tree: true,
    rowHeight: 24,
    rows: [
      { id: "file", name: "a.md" },
      { id: "story", name: "unrelated.md", label: "F-2 Nested work" },
      { id: "loading", ariaHidden: true },
    ],
    render: (row: any) => {
      const el = document.createElement("div");
      if (row.ariaHidden) el.setAttribute("aria-hidden", "true");
      return el;
    },
  });
  expect(box.querySelectorAll('[role="treeitem"]')).toHaveLength(2);
  expect(box.querySelector('[data-key="story"]')?.getAttribute("aria-posinset")).toBe("2");
  expect(box.querySelector('[data-key="story"]')?.getAttribute("aria-setsize")).toBe("2");
  list.focus("file");
  box.dispatchEvent(new KeyboardEvent("keydown", { key: "f", bubbles: true }));
  expect(document.activeElement?.getAttribute("data-key")).toBe("story");
  list.destroy();
  box.remove();
});

function mountList(options: Record<string, unknown> = {}, cs: Record<string, unknown> = {}) {
  (window as any).codesense = cs;
  window.eval(kit);
  window.eval(source);
  const box = document.createElement("div");
  Object.defineProperty(box, "clientHeight", { value: 240 });
  document.body.append(box);
  const render = vi.fn((row: any) => {
    const el = document.createElement("button");
    el.textContent = row.label || row.id;
    return el;
  });
  const api = (window as any).codesense.kit;
  const list = api.virtualList({ container: box, rows: [], rowHeight: 24, render, ...options });
  return {
    box,
    render,
    list,
    api,
    node: (id: string) => box.querySelector<HTMLElement>(`[data-key="${id}"]`)!,
  };
}

it("inserting and removing rows retains every other node without an explicit layoutKey", () => {
  const a = { id: "a" },
    b = { id: "b" },
    c = { id: "c" };
  const { list, node, render, box } = mountList({ rows: [a, b, c] });
  const before = [node("a"), node("b"), node("c")];
  list.setRows([{ id: "new" }, a, b, c]);
  ["a", "b", "c"].forEach((id, i) => expect(node(id)).toBe(before[i]));
  list.setRows([a, c]);
  expect(node("a")).toBe(before[0]);
  expect(node("c")).toBe(before[2]);
  expect(render).toHaveBeenCalledTimes(4);
  expect(node("c").style.top).toBe("24px");
  list.destroy();
  box.remove();
});

it("patches reused rows each paint with the current row and index, replacing only changed layouts", () => {
  const update = vi.fn((el: HTMLElement, row: any, i: number) => {
    el.textContent = `${row.label}:${row.unread}:${i}`;
  });
  const a = { id: "a", label: "A", unread: 0 },
    b = { id: "b", label: "B", unread: 0 };
  const { list, node, render, box } = mountList({
    rows: [a, b],
    layoutKey: (r: any) => r.label,
    update,
  });
  const first = node("a"),
    second = node("b");
  first.focus();
  list.setRows([{ id: "new", label: "New" }, { ...a, unread: 2 }, b]);
  expect(node("a")).toBe(first);
  expect(node("b")).toBe(second);
  expect(document.activeElement).toBe(first);
  expect(first.textContent).toBe("A:2:1");
  expect(update).toHaveBeenCalledWith(second, b, 2);
  list.setRows([{ ...a, label: "New layout" }, b]);
  expect(node("a")).not.toBe(first);
  expect(node("b")).toBe(second);
  expect(render).toHaveBeenCalledTimes(4);
  list.destroy();
  box.remove();
});

it("kit.keyed updates only same-key same-version nodes", () => {
  const { api, list, box } = mountList();
  const target = document.createElement("div");
  const render = vi.fn(() => document.createElement("button"));
  const update = vi.fn((el: HTMLElement, row: any) => {
    el.textContent = row.live;
  });
  const opts = { key: (r: any) => r.id, version: (r: any) => r.layout, render, update };
  api.keyed(target, [{ id: "a", layout: 1, live: "one" }], opts);
  const original = target.firstChild;
  expect(update).not.toHaveBeenCalled();
  const next = { id: "a", layout: 1, live: "two" };
  api.keyed(target, [next], opts);
  expect(target.firstChild).toBe(original);
  expect(update).toHaveBeenCalledExactlyOnceWith(original, next);
  api.keyed(target, [{ ...next, layout: 2 }], opts);
  expect(target.firstChild).not.toBe(original);
  expect(render).toHaveBeenCalledTimes(2);
  expect(update).toHaveBeenCalledTimes(1);
  list.destroy();
  box.remove();
});

it("reapplies tree semantics on reused nodes after inserts, nesting and expansion changes", () => {
  const a = { id: "a", children: [] },
    b = { id: "b", children: [] };
  const { list, node, box } = mountList({ rows: [a, b], tree: true, layoutKey: (r: any) => r.id });
  const first = node("a"),
    second = node("b");
  second.focus();
  const addListener = vi.spyOn(second, "addEventListener");
  list.setRows([{ id: "new" }, a, b]);
  expect(node("b")).toBe(second);
  expect(second.getAttribute("aria-posinset")).toBe("3");
  expect(second.getAttribute("aria-setsize")).toBe("3");
  list.setExpanded(["a", "b"]);
  list.setRows([{ ...a, children: [b] }]);
  expect(node("a")).toBe(first);
  expect(node("b")).toBe(second);
  expect(second.getAttribute("role")).toBe("treeitem");
  expect(second.getAttribute("aria-level")).toBe("2");
  expect(second.getAttribute("aria-posinset")).toBe("1");
  expect(second.getAttribute("aria-setsize")).toBe("1");
  expect(second.getAttribute("aria-expanded")).toBe("true");
  expect(second.style.paddingInlineStart).toBe("18px");
  expect(second.tabIndex).toBe(0);
  list.setRows([{ id: "a", children: [{ id: "b" }] }]);
  expect(second.hasAttribute("aria-expanded")).toBe(false);
  expect(addListener).not.toHaveBeenCalled();
  expect(box.querySelectorAll('[tabindex="0"]')).toHaveLength(1);
  addListener.mockRestore();
  list.destroy();
  box.remove();
});

it("topic windows patch live data and shifted indexes without replacing reused nodes", () => {
  let paint: (snapshot: unknown) => void = () => {};
  const subscribe = vi.fn((_topic: string, opts: any) => {
    paint = opts.onChange;
    return Promise.resolve();
  });
  const update = vi.fn((el: HTMLElement, r: any, i: number) => {
    el.textContent = `${r.live}:${i}`;
  });
  const { list, node, box } = mountList(
    { rows: undefined, topic: "sessions", layoutKey: (r: any) => r.id, update },
    { subscribe, unsubscribe: vi.fn() },
  );
  paint({
    snapshot: [
      { id: "a", live: 0 },
      { id: "b", live: 0 },
    ],
    total: 2,
  });
  const second = node("b");
  paint({ snapshot: [{ id: "new" }, { id: "a", live: 1 }, { id: "b", live: 2 }], total: 3 });
  expect(node("b")).toBe(second);
  expect(second.textContent).toBe("2:2");
  expect(second.style.top).toBe("48px");
  list.destroy();
  box.remove();
});

it("unknown icons log once per name in this document and retain the circle-help fallback", () => {
  const { api, list, box } = mountList();
  const error = vi.spyOn(console, "error").mockImplementation(() => {});
  try {
    const missing = api.icon("missing-one");
    api.iconMarkup("missing-one");
    api.icon("missing-two");
    api.icon("missing-two");
    expect(error).toHaveBeenCalledTimes(2);
    expect(error.mock.calls[0].join(" ")).toContain("missing-one");
    expect(error.mock.calls[1].join(" ")).toContain("missing-two");
    expect(missing.getAttribute("data-missing")).toBe("missing-one");
    expect(missing.innerHTML).toBe(api.icon("circle-help").innerHTML);
    expect(error).toHaveBeenCalledTimes(2);
  } finally {
    error.mockRestore();
    list.destroy();
    box.remove();
  }
});
