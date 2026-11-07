// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { flushRefreshDebounce } from "./refresh-debounce";

describe("flushRefreshDebounce (SF-3 Bug B, Option B Set-aware)", () => {
  it("emits ONE bump with the full Set when N>1 paths are present", () => {
    const bump = vi.fn();
    const warn = vi.fn();

    const paths = new Set<string>(["/a/foo.md", "/a/bar.md", "/a/baz.md"]);

    flushRefreshDebounce("repo-key", paths, bump, { warn });

    expect(bump).toHaveBeenCalledTimes(1);
    expect(bump).toHaveBeenCalledWith("repo-key", paths);
    expect(warn).not.toHaveBeenCalled();
  });

  it("emits one bump with a singleton Set when the set has exactly one path", () => {
    const bump = vi.fn();
    const warn = vi.fn();

    const paths = new Set<string>(["/a/only.md"]);

    flushRefreshDebounce("repo-key", paths, bump, { warn });

    expect(bump).toHaveBeenCalledTimes(1);
    expect(bump).toHaveBeenCalledWith("repo-key", paths);
    expect(warn).not.toHaveBeenCalled();
  });

  it("emits one broad bump + warns when the set is undefined (no paths added during window)", () => {
    const bump = vi.fn();
    const warn = vi.fn();

    flushRefreshDebounce("repo-key", undefined, bump, { warn });

    expect(bump).toHaveBeenCalledTimes(1);
    expect(bump).toHaveBeenCalledWith("repo-key", undefined);
    expect(warn).toHaveBeenCalledTimes(1);
    expect(warn.mock.calls[0][0]).toBe("refresh-debounce");
  });

  it("emits one broad bump + warns when the set is empty", () => {
    const bump = vi.fn();
    const warn = vi.fn();

    flushRefreshDebounce("repo-key", new Set<string>(), bump, { warn });

    expect(bump).toHaveBeenCalledTimes(1);
    expect(bump).toHaveBeenCalledWith("repo-key", undefined);
    expect(warn).toHaveBeenCalledTimes(1);
  });
});
