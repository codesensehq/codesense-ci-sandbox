// SPDX-License-Identifier: MIT
// @vitest-environment node
import fs from "fs";
import os from "os";
import path from "path";
import { describe, expect, it } from "vitest";
import { resolveConfig } from "vite";
import { worktreeMarker, WORKTREE_MARKER_PATH } from "./vite-plugin-worktree-marker";

type Handler = (req: { url?: string }, res: FakeRes, next: () => void) => void;
interface FakeRes {
  body?: string;
  headers: Record<string, string>;
  setHeader(k: string, v: string): void;
  end(b: string): void;
}

function serveMarker(root: string, url: string) {
  const plugin = worktreeMarker(root);
  let handler!: Handler;
  (plugin.configureServer as (s: unknown) => void)({
    middlewares: { use: (h: Handler) => (handler = h) },
  });
  const res: FakeRes = {
    headers: {},
    setHeader(k, v) {
      this.headers[k] = v;
    },
    end(b) {
      this.body = b;
    },
  };
  let nexted = false;
  handler({ url }, res, () => (nexted = true));
  return { res, nexted };
}

describe("worktreeMarker plugin", () => {
  it("serves the realpath'd root and pid", () => {
    const real = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "cs-wt-")));
    const link = `${real}-link`;
    fs.symlinkSync(real, link);
    const { res } = serveMarker(link, WORKTREE_MARKER_PATH);
    expect(JSON.parse(res.body!)).toEqual({ root: real, pid: process.pid });
    fs.rmSync(link);
    fs.rmSync(real, { recursive: true });
  });

  it("passes other URLs through", () => {
    expect(serveMarker("/tmp", "/index.html").nexted).toBe(true);
  });

  it("is dev-server only: apply is serve", () => {
    expect(worktreeMarker("/tmp").apply).toBe("serve");
  });

  it("is absent from a production build of the app config", async () => {
    const build = await resolveConfig({ configFile: path.resolve("vite.config.ts") }, "build");
    expect(build.plugins.map((p) => p.name)).not.toContain("cs-worktree-marker");
    const serve = await resolveConfig({ configFile: path.resolve("vite.config.ts") }, "serve");
    expect(serve.plugins.map((p) => p.name)).toContain("cs-worktree-marker");
  });

  it("no source file other than the plugin and guard mentions the marker path", () => {
    const src = fs.readdirSync("src", { recursive: true, encoding: "utf8" });
    const hits = src
      .filter((f) => /\.(tsx?|css|html)$/.test(f))
      .filter((f) => fs.readFileSync(path.join("src", f), "utf8").includes("__cs-worktree"));
    expect(hits).toEqual([]);
  });
});
