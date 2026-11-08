// SPDX-License-Identifier: MIT
// SV-ENG-61: the create_dashboard starter script (APP_JS in dashboard_publish/create.rs) shows the
// hosted `runs`, `sessions`, `messages` and `rows` lists as a table, with a note when cut. Every
// other shape renders exactly as it did before (BASE_RENDER, the merge-base text of render()).
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { beforeEach, describe, expect, it, vi } from "vitest";

const source = readFileSync(
  resolve(__dirname, "../../../../src-tauri/src/dashboard_publish/create.rs"),
  "utf8",
);
const appJs = /const APP_JS: &str = r#"([\s\S]*?)"#;/.exec(source)?.[1] ?? "";

/** render() and its helpers as they were before the hosted list answers. */
const BASE_RENDER = `
  function el(tag, text, cls) {
    var n = document.createElement(tag);
    if (text !== undefined) n.textContent = text;
    if (cls) n.className = cls;
    return n;
  }
  function cell(v) {
    if (v === null || v === undefined) return "";
    return typeof v === "object" ? JSON.stringify(v) : String(v);
  }
  function render(box, value) {
    var rows = Array.isArray(value) ? value : value && Array.isArray(value.items) ? value.items : null;
    if (value && value.generated_at) box.appendChild(el("p", "Generated " + value.generated_at, "muted"));
    if (value && value.note) box.appendChild(el("p", String(value.note), "muted"));
    var objects = rows && rows.every(function (r) { return r && typeof r === "object" && !Array.isArray(r); });
    if (rows && rows.length && objects) {
      var cols = [];
      rows.forEach(function (r) { Object.keys(r).forEach(function (k) { if (cols.indexOf(k) < 0) cols.push(k); }); });
      var table = el("table");
      var head = el("tr");
      cols.forEach(function (c) { head.appendChild(el("th", c)); });
      table.appendChild(head);
      rows.forEach(function (r) {
        var tr = el("tr");
        cols.forEach(function (c) { tr.appendChild(el("td", cell(r[c]))); });
        table.appendChild(tr);
      });
      box.appendChild(table);
    } else if (rows && !rows.length) {
      box.appendChild(el("p", "No rows yet.", "muted"));
    } else {
      box.appendChild(el("pre", JSON.stringify(value, null, 2)));
    }
  }
  render(box, value);
`;

/** What the merge-base starter renders into a box for `value`. */
function baseHtml(value: unknown): string {
  const box = document.createElement("div");
  new Function("box", "value", BASE_RENDER)(box, value);
  return box.innerHTML;
}

/** Runs the starter on a listed file holding `value` (the `data-src` flow) and returns the box. */
async function loaded(value: unknown): Promise<HTMLElement> {
  document.body.innerHTML = '<div id="file" data-src="data/x.json"></div>';
  vi.stubGlobal("fetch", () => Promise.resolve({ ok: true, json: () => Promise.resolve(value) }));
  new Function(appJs)();
  await new Promise((done) => setTimeout(done, 0));
  vi.unstubAllGlobals();
  return document.getElementById("file") as HTMLElement;
}

/** Runs the starter against a live box that asked `query`, answers it, and returns the box. */
async function answered(query: string, answer: unknown): Promise<HTMLElement> {
  document.body.innerHTML = `<div id="live" data-query="${query}"></div>`;
  const sent: { id: string }[] = [];
  vi.spyOn(window, "postMessage").mockImplementation(((m: { id: string }) => {
    sent.push(m);
  }) as typeof window.postMessage);
  new Function(appJs)();
  const reply = new MessageEvent("message", {
    data: { cs: "live", v: 1, id: sent[0].id, ok: true, source: "live", answered_at: 1, answer },
  });
  // The event's source is the viewer's frame, the page's parent.
  Object.defineProperty(reply, "source", { value: parent });
  globalThis.dispatchEvent(reply);
  await new Promise((done) => setTimeout(done, 0));
  return document.getElementById("live") as HTMLElement;
}

const row = { id: "a", n: 1 };

describe("starter hosted list answers", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("has the script", () => {
    expect(appJs).toContain("function render(");
  });

  it.each([
    ["runs", { runs: [row] }],
    ["sessions", { sessions: [row] }],
    ["inbox", { messages: [row] }],
    ["queryContext", { rows: [row] }],
    ["runs", { runs: [row], truncated: true, returned: 1, total: 3 }],
  ])("shows the %s list as a table, not raw JSON", async (query, answer) => {
    const box = await answered(query, answer);
    expect(box.querySelector("table")).not.toBeNull();
    expect(box.querySelector("pre")).toBeNull();
  });

  it("says no rows for an empty list", async () => {
    const box = await answered("runs", { runs: [] });
    expect(box.textContent).toContain("No rows yet.");
    expect(box.querySelector("pre")).toBeNull();
  });

  it("says how many of how many when the list was cut", async () => {
    const box = await answered("runs", {
      runs: [row, row],
      truncated: true,
      returned: 2,
      total: 9,
    });
    expect(box.textContent).toContain("Latest 2 of 9");
  });

  it("says it was cut when the total is unknown", async () => {
    const box = await answered("runs", { runs: [row], truncated: true, returned: 1 });
    expect(box.textContent).toMatch(/Latest 1/);
  });

  it("shows no cut note for a list that was not cut", async () => {
    const box = await answered("runs", { runs: [row], truncated: false });
    expect(box.textContent).not.toContain("Latest");
  });
});

// Shapes that are not a bare array, {items}, or one hosted list plus its cut markers.
const OTHER_SHAPES: [string, unknown][] = [
  [
    "usage report",
    { sessions: [{ sessionId: "s1" }], days: [{ day: "d" }], totals: { costUsd: 42 } },
  ],
  ["usage report, no days", { sessions: [row], totals: { costUsd: 42 } }],
  ["inbox with unread", { messages: [row], humanUnread: 7 }],
  ["context answer", { total: 1, truncated: false, rows: [row], groups: [] }],
  ["list with a note", { runs: [row], note: "from the Mac" }],
  ["structured note", { sessions: [row], note: { text: "x", at: 1 } }],
  ["structured note on items", { items: [row], note: { text: "x" } }],
  ["items with siblings", { items: [row], generated_at: "today", totals: { n: 1 } }],
  ["structured generated_at", { runs: [row], generated_at: { iso: "today" } }],
  ["two lists", { runs: [row], sessions: [row] }],
  ["list plus a count", { runs: [row], count: 1 }],
  ["list under another key", { entries: [row] }],
  ["list under a nested key", { data: { runs: [row] } }],
  ["marker keys only", { truncated: true, returned: 0, total: 0 }],
  ["list that is not an array", { runs: "none" }],
  ["list of numbers", { runs: [1, 2, 3] }],
  ["empty object", {}],
  ["summary object", { note: "a summary", generated_at: "today", cost: 3 }],
  ["string", "text"],
  ["number", 42],
  ["null", null],
  ["bare array of numbers", [1, 2]],
  ["bare array", [row]],
  ["empty array", []],
  ["items", { items: [row] }],
];

describe("every other shape renders as before", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it.each(OTHER_SHAPES)("%s", async (_name, value) => {
    const box = await loaded(value);
    expect(box.innerHTML).toBe(`<h2>data/x.json</h2>${baseHtml(value)}`);
  });

  it("keeps the totals of a live usage report as before", async () => {
    const report = { sessions: [{ sessionId: "s1" }], days: [], totals: { costUsd: 42 } };
    const box = await answered("sessionUsage", report);
    expect(box.innerHTML).toContain(baseHtml(report));
    expect(box.textContent).toContain("42");
  });
});
