// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { abbreviatePromptPath, formatPromptPath } from "./prompt-path";

const HOME = "/Users/dev";
const fmt = (path: string, home: string | null = HOME) =>
  formatPromptPath(abbreviatePromptPath(path, home));

describe("abbreviatePromptPath", () => {
  it("says $HOME as ~ and keeps the last two segments behind an ellipsis", () => {
    const p = abbreviatePromptPath(
      "/Users/dev/.codesense/users/1234567/_worker/acme/team-context/default/demo-child",
      HOME,
    );
    expect(p).toMatchObject({ head: "~/…/", parent: "default", leaf: "demo-child", elided: true });
    expect(formatPromptPath(p)).toBe("~/…/default/demo-child");
  });

  it("shows home itself, and short paths under it, whole", () => {
    expect(fmt("/Users/dev")).toBe("~");
    expect(abbreviatePromptPath("/Users/dev", HOME)).toMatchObject({ head: "", leaf: "~" });
    expect(fmt("/Users/dev/code")).toBe("~/code");
    expect(fmt("/Users/dev/code/app")).toBe("~/code/app");
    expect(fmt("/Users/dev/code/app/")).toBe("~/code/app");
  });

  it("abbreviates home only on a segment boundary", () => {
    expect(fmt("/Users/devx/code")).toBe("/…/devx/code");
    expect(fmt("/Users/de")).toBe("/Users/de");
  });

  it("handles the root and paths outside home", () => {
    expect(fmt("/")).toBe("/");
    expect(abbreviatePromptPath("/", HOME)).toMatchObject({ head: "", parent: null, leaf: "/" });
    expect(fmt("/etc")).toBe("/etc");
    expect(fmt("/usr/local")).toBe("/usr/local");
    expect(fmt("/usr/local/bin")).toBe("/…/local/bin");
    expect(fmt("/Users/dev", null)).toBe("/Users/dev");
  });

  it("abbreviates a remote Machine's own home", () => {
    expect(fmt("/home/ubuntu/work/codesense/src", "/home/ubuntu")).toBe("~/…/codesense/src");
    expect(fmt("/home/ubuntu/work", "/home/ubuntu/")).toBe("~/work");
    // This Mac's home never applies to a remote path.
    expect(fmt("/home/ubuntu/work/a", HOME)).toBe("/…/work/a");
  });

  it("never drops the current directory, however long its name", () => {
    const long = "a-very-long-directory-name-that-will-not-fit-in-any-caption-at-all";
    const p = abbreviatePromptPath(`/Users/dev/x/y/${long}`, HOME);
    expect(p.leaf).toBe(long);
    expect(p.parent).toBe("y");
    expect(p.full).toBe(`/Users/dev/x/y/${long}`);
  });

  it("ignores a home of / and collapses doubled separators", () => {
    expect(fmt("/srv/app", "/")).toBe("/srv/app");
    expect(fmt("/Users//dev//code//app/x", HOME)).toBe("~/…/app/x");
  });
});
