// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { sourceRepoRemoteKey } from "./source-repo-key";

describe("sourceRepoRemoteKey", () => {
  it("defaults legacy rows to GitHub and normalizes case", () => {
    expect(sourceRepoRemoteKey({ owner: "Acme", repo: "Api" })).toBe("github.com/acme/api");
  });

  it("separates identical slugs on different hosts", () => {
    const github = sourceRepoRemoteKey({
      owner: "acme",
      repo: "api",
      provider: "github",
      host: "github.com",
    });
    const gitlab = sourceRepoRemoteKey({
      owner: "acme",
      repo: "api",
      provider: "gitlab",
      host: "GitLab.com.",
    });
    expect(github).toBe("github.com/acme/api");
    expect(gitlab).toBe("gitlab.com/acme/api");
    expect(gitlab).not.toBe(github);
  });

  it("rejects non-GitHub DTOs that lost their host", () => {
    expect(() => sourceRepoRemoteKey({ owner: "acme", repo: "api", provider: "gitlab" })).toThrow(
      "host is required",
    );
  });

  it("preserves Bitbucket and runtime-host identities", () => {
    expect(
      sourceRepoRemoteKey({
        owner: "Acme",
        repo: "Api",
        provider: "bitbucket",
        host: "bitbucket.org",
      }),
    ).toBe("bitbucket.org/acme/api");
    expect(
      sourceRepoRemoteKey({
        owner: "Acme",
        repo: "Api",
        provider: "gitea",
        host: "Git.Example.COM.",
      }),
    ).toBe("git.example.com/acme/api");
  });

  it("normalizes every trailing root dot and rejects malformed hosts", () => {
    expect(
      sourceRepoRemoteKey({
        owner: "acme",
        repo: "api",
        provider: "gitlab",
        host: " gitlab.com.. ",
      }),
    ).toBe("gitlab.com/acme/api");
    expect(() =>
      sourceRepoRemoteKey({ owner: "acme", repo: "api", provider: "gitea", host: "bad host" }),
    ).toThrow("host is invalid");
  });

  it("rejects owner/repo segments the backend domain rejects", () => {
    for (const slug of [
      { owner: " acme", repo: "api" },
      { owner: "acme", repo: "api/child" },
      { owner: "..", repo: "api" },
      { owner: "acme", repo: "api\n" },
    ]) {
      expect(() => sourceRepoRemoteKey(slug)).toThrow(/source repo (owner|repo) is invalid/);
    }
  });
});
