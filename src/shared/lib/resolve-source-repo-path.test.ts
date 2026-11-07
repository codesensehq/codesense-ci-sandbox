// SPDX-License-Identifier: MIT
import { describe, expect, it, vi, beforeEach } from "vitest";
import type { RepoState } from "@/shared/stores/config-store";
import { resolveSourceRepoPath, resolveRealRepoPath } from "./resolve-source-repo-path";
import { resolveRepo } from "./resolve-repo";

vi.mock("./resolve-repo", () => ({
  resolveRepo: vi.fn(),
}));

const resolveRepoMock = resolveRepo as unknown as ReturnType<typeof vi.fn>;

function makeRepo(overrides: Partial<RepoState>): RepoState {
  return {
    repoPath: "/Users/dev/my-repo",
    repoName: "my-repo",
    contextRepoPath: "/Users/dev/.codesense/context",
    teamContentPath: "",
    hasRemote: true,
    remoteUrl: "https://github.com/dev/my-repo",
    mainDoc: "",
    ...overrides,
  };
}

describe("resolveSourceRepoPath", () => {
  it("returns repoPath for a legacy repo (real path, no localPath)", () => {
    const repo = makeRepo({ repoPath: "/Users/dev/legacy-repo" });
    expect(resolveSourceRepoPath(repo)).toBe("/Users/dev/legacy-repo");
  });

  it("returns localPath for a synthetic repo with a matching attachment", () => {
    const repo = makeRepo({
      repoPath: "workspace:codesense:default:alderco/codesense",
      localPath: "/Users/dev/codesense",
    });
    expect(resolveSourceRepoPath(repo)).toBe("/Users/dev/codesense");
  });

  it("returns null for a synthetic repo with localPath: null (no attachment)", () => {
    const repo = makeRepo({
      repoPath: "workspace:codesense:default:alderco/codesense",
      localPath: null,
    });
    expect(resolveSourceRepoPath(repo)).toBeNull();
  });

  it("returns null for a synthetic repo with localPath: undefined (defensive)", () => {
    const repo = makeRepo({
      repoPath: "workspace:codesense:default:alderco/codesense",
      localPath: undefined,
    });
    expect(resolveSourceRepoPath(repo)).toBeNull();
  });

  it("returns repoPath for a legacy repo even if localPath is explicitly null", () => {
    // Defensive: a legacy persisted entry must never have its real-path
    // repoPath shadowed by an accidental null localPath.
    const repo = makeRepo({
      repoPath: "/Users/dev/legacy-repo",
      localPath: null,
    });
    expect(resolveSourceRepoPath(repo)).toBe("/Users/dev/legacy-repo");
  });

  it("R5B-P6: returns null for a synthetic repo with empty-string localPath", () => {
    // Defensive: empty string is never a valid filesystem path. Treat as
    // null so the AC-8 modal surfaces instead of feeding `cwd: ""` to
    // terminal spawn.
    const repo = makeRepo({
      repoPath: "workspace:codesense:default:alderco/codesense",
      localPath: "",
    });
    expect(resolveSourceRepoPath(repo)).toBeNull();
  });

  it("R5B-P6: returns repoPath for a legacy repo even if localPath is empty string", () => {
    // A legacy persisted entry must never have its real-path repoPath
    // shadowed by an empty-string localPath either.
    const repo = makeRepo({
      repoPath: "/Users/dev/legacy-repo",
      localPath: "",
    });
    expect(resolveSourceRepoPath(repo)).toBe("/Users/dev/legacy-repo");
  });
});

describe("resolveRealRepoPath", () => {
  beforeEach(() => {
    resolveRepoMock.mockReset();
  });

  it("returns localPath for a synthetic key with an attached clone", () => {
    resolveRepoMock.mockReturnValue(
      makeRepo({
        repoPath: "workspace:codesense:default:alderco/codesense",
        localPath: "/Users/dev/codesense",
      }),
    );
    expect(resolveRealRepoPath("workspace:codesense:default:alderco/codesense")).toBe(
      "/Users/dev/codesense",
    );
  });

  it("returns null for a synthetic key with no attachment", () => {
    resolveRepoMock.mockReturnValue(
      makeRepo({
        repoPath: "workspace:codesense:default:alderco/codesense",
        localPath: null,
      }),
    );
    expect(resolveRealRepoPath("workspace:codesense:default:alderco/codesense")).toBeNull();
  });

  it("returns null for an unregistered synthetic key (resolveRepo returns undefined)", () => {
    resolveRepoMock.mockReturnValue(undefined);
    expect(resolveRealRepoPath("workspace:codesense:default:alderco/codesense")).toBeNull();
  });

  it("returns the path unchanged for a legacy real-path key", () => {
    resolveRepoMock.mockReturnValue(undefined);
    expect(resolveRealRepoPath("/Users/dev/legacy-repo")).toBe("/Users/dev/legacy-repo");
  });

  it("returns repoPath for a legacy key found in the store (no localPath)", () => {
    resolveRepoMock.mockReturnValue(makeRepo({ repoPath: "/Users/dev/legacy-repo" }));
    expect(resolveRealRepoPath("/Users/dev/legacy-repo")).toBe("/Users/dev/legacy-repo");
  });
});
