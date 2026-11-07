// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type Result = { rows: unknown[]; [key: string]: any };
type Handle = (() => void) & { loadMore: () => Promise<Result> };
export type PagingFixture = {
  sdk: {
    queryAll: (
      name: string,
      args: Record<string, unknown>,
      opts?: Record<string, unknown>,
    ) => Promise<Result>;
    subscribeAll: (
      name: string,
      args: Record<string, unknown>,
      everyS: number,
      cb: (r: Result) => void,
      opts?: Record<string, unknown>,
    ) => Handle;
    partialList: (opts: Record<string, unknown>) => {
      stop: () => void;
      loadMore: () => Promise<Result>;
    };
  };
  reply: (a: any) => void;
  fail: (error: string) => void;
  head: (a: any) => Promise<void>;
  args: () => Record<string, unknown>;
  count: () => number;
  cleanup: () => void;
};

// One behavioral contract exercises the real bridge adapters in both existing suites.
export function testAnyListPaging(anyListFixture: () => PagingFixture) {
  describe("any-list paging contract", () => {
    beforeEach(() => vi.useFakeTimers());
    afterEach(() => {
      vi.useRealTimers();
      vi.restoreAllMocks();
    });
    const page = (commits: unknown[], nextCursor?: string) => ({
      commits,
      sessions: [{ sessionId: "fact" }],
      total: 2,
      nextCursor,
    });

    it("pages storyChanges commits by sha and keeps sessions metadata", async () => {
      const h = anyListFixture();
      try {
        const p = h.sdk.queryAll("storyChanges", { storyId: "X", limit: 1 });
        await vi.advanceTimersByTimeAsync(0);
        h.reply(page([{ sha: "a" }], "sc:one"));
        await vi.advanceTimersByTimeAsync(0);
        expect(h.args()).toMatchObject({ storyId: "X", limit: 1, cursor: "sc:one" });
        h.reply(page([{ sha: "b" }]));
        expect(await p).toMatchObject({
          rows: [{ sha: "a" }, { sha: "b" }],
          sessions: [{ sessionId: "fact" }],
          total: 2,
          complete: true,
        });
      } finally {
        h.cleanup();
      }
    });

    it.each(["path", "function"])(
      "uses custom %s selectors, id, total and cursor on an unknown verb",
      async (mode) => {
        const h = anyListFixture();
        try {
          const p = h.sdk.queryAll(
            "customList",
            {},
            {
              rows: mode === "path" ? "data.items" : (a: any) => a.data.items,
              id: mode === "path" ? "key" : (r: any) => r.key,
              cursor: {
                arg: "after",
                next: mode === "path" ? "continuation" : (a: any) => a.continuation,
              },
              total: mode === "path" ? "count" : (a: any) => a.count,
            },
          );
          await vi.advanceTimersByTimeAsync(0);
          h.reply({
            data: { items: [{ key: "a" }], label: "kept" },
            count: 2,
            continuation: "next",
          });
          await vi.advanceTimersByTimeAsync(0);
          expect(h.args()).toMatchObject({ after: "next" });
          expect(h.args()).not.toHaveProperty("cursor");
          h.reply({ data: { items: [{ key: "b" }] }, count: 2 });
          expect(await p).toMatchObject({
            rows: [{ key: "a" }, { key: "b" }],
            total: 2,
            complete: true,
          });
        } finally {
          h.cleanup();
        }
      },
    );

    it("keeps unknown verbs without a rows option on one page", async () => {
      const h = anyListFixture();
      try {
        const p = h.sdk.queryAll("customList", {});
        await vi.advanceTimersByTimeAsync(0);
        h.reply({ rows: [{ id: "one" }], total: 2, nextCursor: "ignored" });
        expect(await p).toMatchObject({ rows: [{ id: "one" }], returned: 1, complete: false });
        expect(h.count()).toBe(1);
      } finally {
        h.cleanup();
      }
    });

    it.each([
      ["id", { id: undefined }],
      ["rows", { rows: undefined }],
      ["total", { total: undefined }],
      ["cursor", { cursor: undefined }],
      ["cursor.arg", { cursor: { arg: undefined } }],
      ["cursor.next", { cursor: { next: undefined } }],
    ])("keeps sessionUsage defaults for undefined %s", async (_name, opts) => {
      const h = anyListFixture();
      try {
        const p = h.sdk.queryAll("sessionUsage", {}, opts as Record<string, unknown>);
        p.catch(() => {});
        await vi.advanceTimersByTimeAsync(0);
        const sessions = [
          { cli: "codex", sessionId: "same" },
          { cli: "claude", sessionId: "same" },
        ];
        h.reply({ sessions, total: 2 });
        expect(await p).toMatchObject({ rows: sessions, total: 2, complete: true });
      } finally {
        h.cleanup();
      }
    });

    it.each(["arg", "next"])(
      "merges a partial cursor %s override with its default",
      async (field) => {
        const h = anyListFixture();
        try {
          const cursor = field === "arg" ? { arg: "after" } : { next: (a: any) => a.continuation };
          const p = h.sdk.queryAll("storyChanges", {}, { cursor });
          p.catch(() => {});
          await vi.advanceTimersByTimeAsync(0);
          h.reply({ ...page([{ sha: "a" }], "tail"), continuation: "tail" });
          await vi.advanceTimersByTimeAsync(0);
          expect(h.args()).toMatchObject({ [field === "arg" ? "after" : "cursor"]: "tail" });
          expect(h.args()).not.toHaveProperty("undefined");
          h.reply(page([{ sha: "b" }]));
          expect(await p).toMatchObject({ rows: [{ sha: "a" }, { sha: "b" }], complete: true });
        } finally {
          h.cleanup();
        }
      },
    );

    it("defaults only to pagers shared by the app and hosted catalog", () => {
      const source = readFileSync(
        join(process.cwd(), "src-tauri/src/dashboard_publish/toolkit/cs-live.js"),
        "utf8",
      );
      const table = source.slice(
        source.indexOf("  var PAGED_LISTS ="),
        source.indexOf("  function pagingValue("),
      );
      const names = new Function(table + "; return Object.keys(PAGED_LISTS);")() as string[];
      expect(names).toEqual(["sessionUsage", "queryContext", "storyChanges"]);
      const catalog = readFileSync(
        join(process.cwd(), "src-tauri/src/dashboard_publish/catalog.rs"),
        "utf8",
      );
      for (const name of names) expect(catalog).toContain('name: "' + name + '"');
    });

    it.each(["page-first", "head-first"])(
      "keeps newest story data with %s arrival",
      async (order) => {
        const h = anyListFixture();
        const results: Result[] = [];
        const stop = h.sdk.subscribeAll(
          "storyChanges",
          { storyId: "X", limit: 1 },
          15,
          (r: Result) => results.push(r),
          { manual: true },
        );
        try {
          await h.head(page([{ sha: "a", text: "old" }], "next"));
          const loading = stop.loadMore();
          await vi.advanceTimersByTimeAsync(0);
          const tail = () => h.reply(page([{ sha: "a", text: "old" }, { sha: "b" }]));
          const head = () => h.head(page([{ sha: "a", text: "new" }], "new-next"));
          if (order === "page-first") {
            tail();
            await loading;
            await head();
          } else {
            await head();
            tail();
            await loading;
          }
          expect(results.at(-1)).toMatchObject({
            rows: [{ sha: "a", text: "new" }, { sha: "b" }],
            complete: true,
          });
        } finally {
          stop();
          h.cleanup();
        }
      },
    );

    it.each(["cursor", "after"])(
      "restarts once on the real storyChanges stale error using %s",
      async (cursorArg) => {
        const h = anyListFixture();
        try {
          const p = h.sdk.queryAll(
            "storyChanges",
            { limit: 1 },
            { cursor: { arg: cursorArg, next: "nextCursor" } },
          );
          await vi.advanceTimersByTimeAsync(0);
          h.reply(page([{ sha: "a" }], "stale"));
          await vi.advanceTimersByTimeAsync(0);
          expect(h.args()[cursorArg]).toBe("stale");
          h.fail("invalid: stale or mismatched story_changes cursor");
          await vi.advanceTimersByTimeAsync(0);
          expect(h.args()).not.toHaveProperty(cursorArg);
          h.reply(page([{ sha: "a" }], "still-stale"));
          await vi.advanceTimersByTimeAsync(0);
          const rejected = expect(p).rejects.toThrow("stale or mismatched");
          h.fail("invalid: stale or mismatched story_changes cursor");
          await rejected;
          expect(h.count()).toBe(4);
        } finally {
          h.cleanup();
        }
      },
    );

    it("polls storyChanges and drops an in-flight page and emits after stop", async () => {
      const h = anyListFixture();
      const results: Result[] = [];
      const stop = h.sdk.subscribeAll("storyChanges", {}, 15, (r: Result) => results.push(r), {
        manual: true,
      });
      try {
        await h.head(page([{ sha: "a" }], "next"));
        await h.head(page([{ sha: "a", text: "polled" }], "next"));
        expect(results.at(-1)?.rows).toEqual([{ sha: "a", text: "polled" }]);
        const loading = stop.loadMore();
        const rejected = expect(loading).rejects.toThrow("stopped");
        await vi.advanceTimersByTimeAsync(0);
        stop();
        await rejected;
        const count = results.length;
        h.reply(page([{ sha: "b" }]));
        await vi.advanceTimersByTimeAsync(30000);
        expect(results).toHaveLength(count);
      } finally {
        stop();
        h.cleanup();
      }
    });

    it("rebinds partialList controls to story Y while story X Load more is pending", async () => {
      const h = anyListFixture();
      const status = document.createElement("p");
      const button = document.createElement("button");
      const rows = vi.fn();
      const bind = (storyId: string) =>
        h.sdk.partialList({
          status,
          button,
          onResult: rows,
          subscribe: (cb: (r: Result) => void) =>
            h.sdk.subscribeAll("storyChanges", { storyId }, 15, cb, { manual: true }),
        });
      const a = bind("X");
      let b: ReturnType<typeof bind> | undefined;
      try {
        await h.head(page([{ sha: "x" }], "x-next"));
        const loading = a.loadMore();
        await vi.advanceTimersByTimeAsync(0);
        a.stop();
        b = bind("Y");
        await h.head({ commits: [{ sha: "y" }], total: 1 });
        await loading;
        const before = [
          status.textContent,
          button.textContent,
          button.disabled,
          button.hidden,
          rows.mock.calls.length,
        ];
        h.reply(page([{ sha: "late-x" }]));
        await vi.advanceTimersByTimeAsync(0);
        expect(rows.mock.calls.at(-1)?.[0]).toMatchObject({ rows: [{ sha: "y" }], complete: true });
        expect([
          status.textContent,
          button.textContent,
          button.disabled,
          button.hidden,
          rows.mock.calls.length,
        ]).toEqual(before);
      } finally {
        a.stop();
        b?.stop();
        h.cleanup();
      }
    });

    it("keeps the paging block byte-identical in app and hosted", () => {
      const paths = [
        "src-tauri/src/http/routes/dashboard_kit/kit.js",
        "src-tauri/src/dashboard_publish/toolkit/cs-live.js",
      ];
      const blocks = paths.map((path) => {
        const s = readFileSync(join(process.cwd(), path), "utf8");
        expect(s).toContain("var PAGED_LISTS =");
        return s.slice(
          s.indexOf("  // Paging state belongs"),
          s.indexOf("  // A small list component"),
        );
      });
      expect(blocks[0]).toBe(blocks[1]);
    });
  });
}
