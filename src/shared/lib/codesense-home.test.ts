// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { homeDir } from "@tauri-apps/api/path";
import {
  deriveOrgRoot,
  getCodesenseHome,
  initCodesenseHome,
  setCodesenseHomeForTests,
} from "./codesense-home";
import { logger } from "./logger";

vi.mock("@/shared/lib/logger", () => ({
  logger: { warn: vi.fn(), info: vi.fn(), error: vi.fn(), debug: vi.fn() },
}));

vi.mock("@tauri-apps/api/path", () => ({ homeDir: vi.fn() }));

beforeEach(() => {
  vi.mocked(logger.warn).mockClear();
  vi.mocked(homeDir).mockReset();
  setCodesenseHomeForTests(null);
});

describe("the CodeSense home", () => {
  it("is ~/.codesense from the OS home, the same join Rust's codesense_home_dir makes", async () => {
    vi.mocked(homeDir).mockResolvedValue("/Users/me/");
    await expect(initCodesenseHome()).resolves.toBe("/Users/me/.codesense");
    expect(getCodesenseHome()).toBe("/Users/me/.codesense");
  });

  it("is never parsed from a context repo path", async () => {
    // A repo cloned at ~/src/users/bob/orgs/acme once parsed to ~/src.
    vi.mocked(homeDir).mockResolvedValue("/Users/me");
    await initCodesenseHome();
    expect(getCodesenseHome()).toBe("/Users/me/.codesense");
  });

  it("is read once and stays null when the OS home cannot be read", async () => {
    vi.mocked(homeDir).mockRejectedValue(new Error("no home"));
    await expect(initCodesenseHome()).resolves.toBeNull();
    await initCodesenseHome();
    expect(homeDir).toHaveBeenCalledTimes(1);
    expect(getCodesenseHome()).toBeNull();
    expect(logger.warn).toHaveBeenCalledWith("codesense-home", expect.stringContaining("no home"));
  });
});

describe("deriveOrgRoot", () => {
  it("strips the flat team/marker suffix from a full contextRepoPath (SLW-IN-7)", () => {
    expect(
      deriveOrgRoot("/home/dev/.codesense/users/42/orgs/acme/default/repos/alderco/codesense"),
    ).toBe("/home/dev/.codesense/users/42/orgs/acme");
    // A flat content path strips to the same org root.
    expect(
      deriveOrgRoot("/home/dev/.codesense/users/42/orgs/acme/default/content/impl/F-1.md"),
    ).toBe("/home/dev/.codesense/users/42/orgs/acme");
  });

  it("passes through an org root unchanged", () => {
    expect(deriveOrgRoot("/home/dev/.codesense/users/42/orgs/acme")).toBe(
      "/home/dev/.codesense/users/42/orgs/acme",
    );
  });

  it("passes through a path with no layer marker unchanged", () => {
    expect(deriveOrgRoot("/some/random/path")).toBe("/some/random/path");
  });

  it("anchors on the FIRST layer marker after the org slug (SLW-IN-7)", () => {
    // The team is the segment before the first marker; deeper marker-named
    // dirs under it don't re-anchor.
    expect(
      deriveOrgRoot("/home/dev/.codesense/users/42/orgs/acme/default/content/sub/install/x"),
    ).toBe("/home/dev/.codesense/users/42/orgs/acme");
  });

  it("passes through a path that begins with a marker (no org root before team)", () => {
    // A leading `/content/...` has no team+org-root before it → unchanged.
    expect(deriveOrgRoot("/content/x")).toBe("/content/x");
    expect(deriveOrgRoot("/default/content/x")).toBe("/default/content/x");
  });

  it("does NOT warn for an org-root path (trailing /orgs/{slug})", () => {
    deriveOrgRoot("/home/dev/.codesense/users/42/orgs/acme");
    expect(logger.warn).not.toHaveBeenCalled();
  });

  it("warns when the path has neither a layer marker nor a trailing /orgs/{slug}", () => {
    deriveOrgRoot("/some/random/path");
    expect(logger.warn).toHaveBeenCalledWith(
      "codesense-home",
      expect.stringContaining("/some/random/path"),
    );
  });
});
