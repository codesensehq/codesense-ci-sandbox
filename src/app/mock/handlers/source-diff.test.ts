// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { PublishFile } from "@/features/git-sync/lib/types";
import { sourceDiffHandlers } from "./source-diff";

describe("source diff mock handlers", () => {
  it("serves the same change set locally and remotely, narrowed for HEAD", async () => {
    const h = sourceDiffHandlers("");
    const local = (await h.source_worktree_changeset({})) as PublishFile[];
    const remote = (await h.source_remote_changeset({ baseRef: undefined })) as PublishFile[];
    const head = (await h.source_remote_changeset({ baseRef: "HEAD" })) as PublishFile[];
    expect(remote).toEqual(local);
    expect(head.length).toBeGreaterThan(0);
    expect(head.length).toBeLessThan(local.length);
  });

  it("answers a redline for every listed file", async () => {
    const h = sourceDiffHandlers("");
    for (const f of (await h.source_worktree_changeset({})) as PublishFile[]) {
      expect(h.source_file_redline({ filePath: f.path })).toMatchObject({ status: f.status });
    }
  });

  it("models the empty and error worlds", async () => {
    expect(await sourceDiffHandlers("?diff=empty").source_worktree_changeset({})).toEqual([]);
    await expect(sourceDiffHandlers("?diff=error").source_remote_changeset({})).rejects.toMatch(
      /not a git repository/,
    );
  });

  it("models an out-of-date terminal server on the machine path only", async () => {
    const h = sourceDiffHandlers("?diff=stale");
    await expect(h.source_remote_changeset({})).rejects.toMatchObject({
      kind: "transportFailure",
      message: expect.stringMatching(/terminal server is too old/),
    });
    expect(((await h.source_worktree_changeset({})) as PublishFile[]).length).toBeGreaterThan(0);
  });

  it("models a large change set: 300 files, one over 5k lines, each with a redline", async () => {
    const h = sourceDiffHandlers("?diff=large");
    const files = (await h.source_worktree_changeset({})) as PublishFile[];
    expect(files.length).toBeGreaterThanOrEqual(300);
    const big = files.find((f) => f.path === "src/generated/table.ts");
    expect(big).toBeDefined();
    const r = h.source_file_redline({ filePath: big!.path }) as { after: string };
    expect(r.after.split("\n").length).toBeGreaterThan(5000);
    expect(h.source_file_redline({ filePath: files[299].path })).toBeDefined();
  });

  it("includes a mode-only change with identical content", async () => {
    const h = sourceDiffHandlers("");
    const files = (await h.source_worktree_changeset({})) as PublishFile[];
    const modeOnly = files.find((f) => f.badges?.includes("mode") && !f.added && !f.removed);
    expect(modeOnly).toBeDefined();
    const r = h.source_file_redline({ filePath: modeOnly!.path }) as {
      before: string;
      after: string;
    };
    expect(r.before).toBe(r.after);
  });
});
