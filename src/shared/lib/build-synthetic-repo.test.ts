// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";

// SF-8: `buildSyntheticRepo` no longer calls into Tauri — its `teamContentPath`
// field is now resolved synchronously via `pathToTeamContentDirSync`. The
// previous PTS-3 `resolveTeamContentDir` mock retired with that refactor.

import {
  buildSyntheticRepo,
  syntheticRepoKey,
  isSyntheticRepoPath,
  parseSyntheticRepoKey,
  buildSyntheticRepoPathFromPrompt,
  findWorkspaceRepoBySlug,
  workspaceReposForTeam,
  firstRepoPathForTeam,
  syntheticKeyForContextRepoPath,
  syntheticKeyForContentPath,
  bumpKeyForContextFileChange,
} from "./build-synthetic-repo";
import type { Attachment, Org, SourceRepoSlug } from "@/shared/types/domain";
import type { RepoState } from "@/shared/stores/config-store";
import { useOrgStore } from "@/shared/stores/org-store";

function makeAttachment(repo_path: string): Attachment {
  return {
    repo_path,
    remote_url: null,
    attached_at: null,
    tracking_prompt_dismissed_at: null,
    credential_ref: null,
  };
}

function makeOrg(overrides: Partial<Org> = {}): Org {
  return {
    slug: "9north",
    display_name: "9North Studio",
    created_at: "2025-01-01T00:00:00Z",
    remote_url: "https://github.com/9north/context",
    config_flags: {},
    people: [],
    teams: [],
    org_root: "/home/user/.codesense/users/1234/orgs/9north",
    resolved_repository_paths: {
      "default\ngithub.com/acme/api":
        "/home/user/.codesense/users/1234/orgs/9north/default/repos/github.com/acme/api",
      "platform\ngithub.com/acme/api":
        "/home/user/.codesense/users/1234/orgs/9north/platform/repos/github.com/acme/api",
      "platform\ngithub.com/acme/web":
        "/home/user/.codesense/users/1234/orgs/9north/platform/repos/github.com/acme/web",
    },
    ...overrides,
  };
}

describe("buildSyntheticRepo", () => {
  it("builds a synthetic RepoState with correct fields", () => {
    const org = makeOrg();
    const slug: SourceRepoSlug = { owner: "acme", repo: "api" };

    const result = buildSyntheticRepo(org, "default", slug, new Map());

    expect(result.repoPath).toBe("workspace:9north:default:acme/api");
    expect(result.repoName).toBe("api (9North Studio)");
    expect(result.contextRepoPath).toBe(
      "/home/user/.codesense/users/1234/orgs/9north/default/repos/github.com/acme/api",
    );
    expect(result.hasRemote).toBe(true);
    expect(result.remoteUrl).toBe("https://github.com/9north/context");
  });

  // SF-8 review iter-1 (AA-AC8): full-shape parity snapshot. AC-8 calls
  // for verifying the sync rewrite produces an identical RepoState shape
  // to the pre-SF-8 async version. The per-field tests above already
  // cover every populated field, but the inline snapshot makes drift
  // visible at a glance — adding a field to RepoState without adding a
  // case here will fail the snapshot and force an explicit acknowledgement.
  it("AC-8 parity: produces the canonical RepoState shape (snapshot)", () => {
    const org = makeOrg();
    const slug: SourceRepoSlug = { owner: "acme", repo: "api" };
    const attachments = new Map<string, Attachment>([
      ["github.com/acme/api", makeAttachment("/Users/dev/acme-api-clone")],
    ]);
    const result = buildSyntheticRepo(org, "default", slug, attachments, {
      mainDoc: "README.md",
    });
    expect(result).toMatchInlineSnapshot(`
      {
        "contextRepoPath": "/home/user/.codesense/users/1234/orgs/9north/default/repos/github.com/acme/api",
        "hasRemote": true,
        "localPath": "/Users/dev/acme-api-clone",
        "mainDoc": "README.md",
        "remoteUrl": "https://github.com/9north/context",
        "repoName": "api (9North Studio)",
        "repoPath": "workspace:9north:default:acme/api",
        "teamContentPath": "/home/user/.codesense/users/1234/orgs/9north/default/content",
      }
    `);
  });

  // PTS-3: teamContentPath populated from Rust's path_to_team_content_dir.
  it("populates teamContentPath from the Rust helper", () => {
    const org = makeOrg();
    const slug: SourceRepoSlug = { owner: "acme", repo: "api" };

    const result = buildSyntheticRepo(org, "default", slug, new Map());

    expect(result.teamContentPath).toBe(
      "/home/user/.codesense/users/1234/orgs/9north/default/content",
    );
  });

  it("populates teamContentPath for a non-default team", () => {
    const org = makeOrg();
    const slug: SourceRepoSlug = { owner: "acme", repo: "web" };

    const result = buildSyntheticRepo(org, "platform", slug, new Map());

    expect(result.teamContentPath).toBe(
      "/home/user/.codesense/users/1234/orgs/9north/platform/content",
    );
  });

  it("handles local-only org (no remote_url)", () => {
    const org = makeOrg({ remote_url: null });
    const slug: SourceRepoSlug = { owner: "local", repo: "proj" };

    const result = buildSyntheticRepo(org, "default", slug, new Map());

    expect(result.hasRemote).toBe(false);
    expect(result.remoteUrl).toBeNull();
  });

  it("encodes team name in repoPath and contextRepoPath", () => {
    const org = makeOrg();
    const slug: SourceRepoSlug = { owner: "acme", repo: "web" };

    const result = buildSyntheticRepo(org, "platform", slug, new Map());

    expect(result.repoPath).toBe("workspace:9north:platform:acme/web");
    expect(result.contextRepoPath).toContain("/platform/repos/github.com/");
  });

  // ===== F-6: localPath population from attachments map =====

  it("populates localPath when the map has a matching host-aware key", () => {
    const org = makeOrg();
    const slug: SourceRepoSlug = { owner: "acme", repo: "api" };
    const attachments = new Map<string, Attachment>([
      ["github.com/acme/api", makeAttachment("/Users/dev/acme-api-clone")],
    ]);

    const result = buildSyntheticRepo(org, "default", slug, attachments);

    expect(result.localPath).toBe("/Users/dev/acme-api-clone");
  });

  it("normalizes slug casing when looking up attachments", () => {
    const org = makeOrg();
    const slug: SourceRepoSlug = { owner: "Acme", repo: "API" };
    const attachments = new Map<string, Attachment>([
      ["github.com/acme/api", makeAttachment("/Users/dev/acme-api-clone")],
    ]);

    const result = buildSyntheticRepo(org, "default", slug, attachments);

    expect(result.localPath).toBe("/Users/dev/acme-api-clone");
  });

  it("sets localPath to null when no attachment matches", () => {
    const org = makeOrg();
    const slug: SourceRepoSlug = { owner: "acme", repo: "ghost-repo" };
    const attachments = new Map<string, Attachment>([
      ["github.com/acme/api", makeAttachment("/Users/dev/api")],
    ]);

    const result = buildSyntheticRepo(org, "default", slug, attachments);

    expect(result.localPath).toBeNull();
  });

  it("resolves same-slug repositories independently by host", () => {
    const org = makeOrg();
    const attachments = new Map<string, Attachment>([
      ["github.com/acme/api", makeAttachment("/src/github-api")],
      ["gitlab.com/acme/api", makeAttachment("/src/gitlab-api")],
    ]);
    const github = buildSyntheticRepo(
      org,
      "default",
      { owner: "acme", repo: "api", provider: "github", host: "github.com" },
      attachments,
    );
    const gitlab = buildSyntheticRepo(
      org,
      "default",
      { owner: "acme", repo: "api", provider: "gitlab", host: "gitlab.com" },
      attachments,
    );
    expect(github.localPath).toBe("/src/github-api");
    expect(gitlab.localPath).toBe("/src/gitlab-api");
    expect(github.repoPath).toBe("workspace:9north:default:acme/api");
    expect(gitlab.repoPath).toBe("workspace:9north:default:gitlab.com/acme/api");
  });

  it("sets localPath to null when caller passes empty map (currentUser-null branch)", () => {
    const org = makeOrg();
    const slug: SourceRepoSlug = { owner: "acme", repo: "api" };

    const result = buildSyntheticRepo(org, "default", slug, new Map());

    expect(result.localPath).toBeNull();
  });

  // ===== R-10: workspace overrides merge =====

  it("merges overrides over defaults", () => {
    const org = makeOrg();
    const slug: SourceRepoSlug = { owner: "acme", repo: "api" };

    const result = buildSyntheticRepo(org, "default", slug, new Map(), {
      mainDoc: "README.md",
    });
    expect(result.mainDoc).toBe("README.md");
  });

  it("uses defaults when overrides are undefined", () => {
    const org = makeOrg();
    const slug: SourceRepoSlug = { owner: "acme", repo: "api" };

    const result = buildSyntheticRepo(org, "default", slug, new Map(), undefined);
    expect(result.mainDoc).toBe("");
  });

  it("uses defaults for missing override fields", () => {
    const org = makeOrg();
    const slug: SourceRepoSlug = { owner: "acme", repo: "api" };

    const result = buildSyntheticRepo(org, "default", slug, new Map(), {});
    expect(result.mainDoc).toBe("");
  });
});

describe("syntheticRepoKey", () => {
  it("produces the same key as buildSyntheticRepo", () => {
    const slug: SourceRepoSlug = { owner: "acme", repo: "api" };
    const key = syntheticRepoKey("9north", "default", slug);
    expect(key).toBe("workspace:9north:default:acme/api");
  });

  it("normalizes non-GitHub hosts through the canonical remote key", () => {
    expect(
      syntheticRepoKey("9north", "default", {
        owner: "Acme",
        repo: "Api",
        provider: "gitlab",
        host: "GitLab.COM.",
      }),
    ).toBe("workspace:9north:default:gitlab.com/Acme/Api");
  });
});

describe("isSyntheticRepoPath", () => {
  it("returns true for synthetic keys", () => {
    expect(isSyntheticRepoPath("workspace:9north:default:acme/api")).toBe(true);
  });

  it("returns false for legacy paths", () => {
    expect(isSyntheticRepoPath("/Users/me/repos/api")).toBe(false);
  });
});

describe("parseSyntheticRepoKey", () => {
  it("parses a key whose orgSlug is the SS-FU-12 owner/repo form", () => {
    // The orgSlug segment now contains a '/'. Because keys are
    // colon-delimited and the parser splits on ':' (exactly 3 parts),
    // the interior '/' stays inside the first segment and does not shift
    // the team / owner-repo positions.
    expect(parseSyntheticRepoKey("workspace:acme/team-context:default:alderco/codesense")).toEqual({
      orgSlug: "acme/team-context",
      team: "default",
      owner: "alderco",
      repo: "codesense",
    });
  });

  it("still parses a legacy single-segment orgSlug", () => {
    expect(parseSyntheticRepoKey("workspace:9north:default:acme/api")).toEqual({
      orgSlug: "9north",
      team: "default",
      owner: "acme",
      repo: "api",
    });
  });

  it("parses a host-qualified repository identity", () => {
    expect(parseSyntheticRepoKey("workspace:9north:default:gitlab.com/acme/api")).toEqual({
      orgSlug: "9north",
      team: "default",
      host: "gitlab.com",
      owner: "acme",
      repo: "api",
    });
  });

  it("rejects malformed keys", () => {
    expect(parseSyntheticRepoKey("/Users/me/repos/api")).toBeNull(); // not synthetic
    expect(parseSyntheticRepoKey("workspace:a:b:c:d")).toBeNull(); // extra colon segment
    expect(parseSyntheticRepoKey("workspace:org:default:host/owner/sub/repo")).toBeNull();
    expect(parseSyntheticRepoKey("workspace:org:default:noslash")).toBeNull(); // owner/repo missing slash
  });
});

describe("findWorkspaceRepoBySlug", () => {
  function makeRepoState(
    orgSlug: string,
    teamName: string,
    owner: string,
    repo: string,
    localPath: string | null = null,
  ): RepoState {
    return {
      repoPath: `workspace:${orgSlug}:${teamName}:${owner}/${repo}`,
      repoName: `${repo} (${orgSlug})`,
      contextRepoPath: `/home/user/.codesense/users/1234/orgs/${orgSlug}/${teamName}/repos/${owner}/${repo}`,
      teamContentPath: `/home/user/.codesense/users/1234/orgs/${orgSlug}/${teamName}/content`,
      hasRemote: true,
      remoteUrl: `https://github.com/${orgSlug}/context`,
      mainDoc: "",
      localPath,
    };
  }

  const TEAM_9NORTH_DEFAULT = { orgSlug: "9north", teamName: "default" };

  beforeEach(() => {
    useOrgStore.setState({ workspaceRepos: {} });
  });

  it("returns the matching RepoState for an exact-case slug", () => {
    const repo = makeRepoState("9north", "default", "acme", "api", "/clones/acme-api");
    useOrgStore.setState({
      workspaceRepos: { [repo.repoPath]: repo },
    });

    expect(findWorkspaceRepoBySlug("acme/api", TEAM_9NORTH_DEFAULT)).toEqual(repo);
  });

  it("matches case-insensitively", () => {
    const repo = makeRepoState("9north", "default", "acme", "api", "/clones/acme-api");
    useOrgStore.setState({
      workspaceRepos: { [repo.repoPath]: repo },
    });

    expect(findWorkspaceRepoBySlug("Acme/API", TEAM_9NORTH_DEFAULT)).toEqual(repo);
  });

  it("matches when stored slug has uppercase letters (AllWidgets/AllWidgets regression)", () => {
    // Regression for the half-normalized matcher: production `syntheticRepoKey`
    // preserves slug case from teams.yml, so the stored key can be
    // `workspace:9north:default:AllWidgets/AllWidgets`. Both sides must be
    // lowercased for the match to succeed.
    const repo = makeRepoState(
      "9north",
      "default",
      "AllWidgets",
      "AllWidgets",
      "/clones/allwidgets",
    );
    useOrgStore.setState({
      workspaceRepos: { [repo.repoPath]: repo },
    });

    expect(findWorkspaceRepoBySlug("AllWidgets/AllWidgets", TEAM_9NORTH_DEFAULT)).toEqual(repo);
    expect(findWorkspaceRepoBySlug("allwidgets/allwidgets", TEAM_9NORTH_DEFAULT)).toEqual(repo);
    expect(findWorkspaceRepoBySlug("ALLWIDGETS/ALLWIDGETS", TEAM_9NORTH_DEFAULT)).toEqual(repo);
  });

  it("returns undefined when no match", () => {
    const repo = makeRepoState("9north", "default", "acme", "api");
    useOrgStore.setState({
      workspaceRepos: { [repo.repoPath]: repo },
    });

    expect(findWorkspaceRepoBySlug("other/repo", TEAM_9NORTH_DEFAULT)).toBeUndefined();
  });

  it("returns undefined when workspaceRepos is empty", () => {
    useOrgStore.setState({ workspaceRepos: {} });

    expect(findWorkspaceRepoBySlug("acme/api", TEAM_9NORTH_DEFAULT)).toBeUndefined();
  });

  it("scopes lookups by team — same slug in another team is not returned (T.4 Round 5 cross-team collision regression)", () => {
    // Both teams independently track `acme/api`. The pre-Round-5 implementation
    // would have returned whichever entry was first in insertion order; the
    // re-signed helper must restrict to the requested team.
    const repoTeamA = makeRepoState("9north", "team-a", "acme", "api", "/clones/team-a-api");
    const repoTeamB = makeRepoState("9north", "team-b", "acme", "api", "/clones/team-b-api");
    useOrgStore.setState({
      workspaceRepos: {
        [repoTeamA.repoPath]: repoTeamA,
        [repoTeamB.repoPath]: repoTeamB,
      },
    });

    expect(findWorkspaceRepoBySlug("acme/api", { orgSlug: "9north", teamName: "team-a" })).toEqual(
      repoTeamA,
    );
    expect(findWorkspaceRepoBySlug("acme/api", { orgSlug: "9north", teamName: "team-b" })).toEqual(
      repoTeamB,
    );
  });

  it("returns undefined when slug exists only in other teams (cross-team isolation)", () => {
    const otherTeamRepo = makeRepoState("9north", "other-team", "acme", "api", "/clones/other-api");
    useOrgStore.setState({
      workspaceRepos: { [otherTeamRepo.repoPath]: otherTeamRepo },
    });

    // Lookup against `9north:default` should NOT cross over to `9north:other-team`.
    expect(findWorkspaceRepoBySlug("acme/api", TEAM_9NORTH_DEFAULT)).toBeUndefined();
  });

  it("matches the `_none` team segment for null teamName", () => {
    const repo = makeRepoState("9north", "_none", "acme", "api", "/clones/acme-api");
    useOrgStore.setState({
      workspaceRepos: { [repo.repoPath]: repo },
    });

    expect(findWorkspaceRepoBySlug("acme/api", { orgSlug: "9north", teamName: null })).toEqual(
      repo,
    );
    // Lookup with a real team name does not match the `_none` bucket.
    expect(
      findWorkspaceRepoBySlug("acme/api", { orgSlug: "9north", teamName: "default" }),
    ).toBeUndefined();
  });

  it("selects a host-qualified collision exactly while legacy lookup prefers GitHub", () => {
    const github = makeRepoState("9north", "default", "acme", "api", "/clones/github");
    const gitlab = {
      ...makeRepoState("9north", "default", "acme", "api", "/clones/gitlab"),
      repoPath: "workspace:9north:default:gitlab.com/acme/api",
    };
    useOrgStore.setState({
      workspaceRepos: { [github.repoPath]: github, [gitlab.repoPath]: gitlab },
    });

    expect(findWorkspaceRepoBySlug("gitlab.com/acme/api", TEAM_9NORTH_DEFAULT)).toEqual(gitlab);
    expect(findWorkspaceRepoBySlug("acme/api", TEAM_9NORTH_DEFAULT)).toEqual(github);
    expect(
      buildSyntheticRepoPathFromPrompt({
        teamKey: "9north:default",
        sourceRepoSlug: "gitlab.com/acme/api",
      }),
    ).toBe(gitlab.repoPath);
  });
});

describe("workspaceReposForTeam", () => {
  // Minimal RepoState fixture — only fields the filter inspects matter.
  const repoState = (repoPath: string): RepoState =>
    ({
      repoPath,
      // The remaining RepoState fields aren't read by `workspaceReposForTeam`;
      // cast through `unknown` keeps the fixture compact.
    }) as unknown as RepoState;

  it("filters workspaceRepos to entries owned by the given team", () => {
    const repos = {
      "workspace:acme:default:acme/web": repoState("workspace:acme:default:acme/web"),
      "workspace:acme:default:acme/api": repoState("workspace:acme:default:acme/api"),
      "workspace:acme:mobile:acme/ios": repoState("workspace:acme:mobile:acme/ios"),
      "workspace:9north:default:9north/site": repoState("workspace:9north:default:9north/site"),
    };

    const result = workspaceReposForTeam(repos, "acme:default");
    expect(result.map((r) => r.repoPath)).toEqual([
      "workspace:acme:default:acme/web",
      "workspace:acme:default:acme/api",
    ]);
  });

  it("returns empty when the team has no entries", () => {
    const repos = {
      "workspace:acme:default:acme/web": repoState("workspace:acme:default:acme/web"),
    };
    expect(workspaceReposForTeam(repos, "acme:mobile")).toEqual([]);
  });

  it("returns empty for the NO_TEAM_SENTINEL teamKey", () => {
    const repos = {
      "workspace:acme:default:acme/web": repoState("workspace:acme:default:acme/web"),
    };
    // Sentinel parses to teamName === null; no synthetic key matches null team.
    expect(workspaceReposForTeam(repos, "_no_team:_none")).toEqual([]);
  });

  it("returns empty when workspaceRepos is empty", () => {
    expect(workspaceReposForTeam({}, "acme:default")).toEqual([]);
  });

  it("ignores legacy non-synthetic repoPath keys", () => {
    const repos = {
      "/Users/dev/repos/legacy": repoState("/Users/dev/repos/legacy"),
      "workspace:acme:default:acme/web": repoState("workspace:acme:default:acme/web"),
    };
    expect(workspaceReposForTeam(repos, "acme:default").map((r) => r.repoPath)).toEqual([
      "workspace:acme:default:acme/web",
    ]);
  });
});

describe("firstRepoPathForTeam", () => {
  const repoState = (repoPath: string): RepoState => ({ repoPath }) as unknown as RepoState;

  it("returns the first (canonical-order) repoPath for the team", () => {
    const repos = {
      "workspace:acme:default:acme/web": repoState("workspace:acme:default:acme/web"),
      "workspace:acme:default:acme/api": repoState("workspace:acme:default:acme/api"),
      "workspace:acme:mobile:acme/ios": repoState("workspace:acme:mobile:acme/ios"),
    };
    expect(firstRepoPathForTeam(repos, "acme:default")).toBe("workspace:acme:default:acme/web");
  });

  it("returns undefined when the team has no source repo", () => {
    const repos = {
      "workspace:acme:default:acme/web": repoState("workspace:acme:default:acme/web"),
    };
    expect(firstRepoPathForTeam(repos, "acme:mobile")).toBeUndefined();
    expect(firstRepoPathForTeam({}, "acme:default")).toBeUndefined();
  });
});

describe("syntheticKeyForContextRepoPath", () => {
  // SW-FX-16: the `context-file-change` payload carries the surface-independent
  // `context_repo_path` (= the synthetic repo's `contextRepoPath`). Resolving it
  // back to the synthetic key lets worktree-surface edits — whose emitted
  // `repo_path` is `ws.cwd`, matching no `localPath` — reach the tree/viewer
  // subscription keyed on the synthetic.
  function makeRepoState(
    orgSlug: string,
    teamName: string,
    owner: string,
    repo: string,
  ): RepoState {
    return {
      repoPath: `workspace:${orgSlug}:${teamName}:${owner}/${repo}`,
      repoName: `${repo} (${orgSlug})`,
      contextRepoPath: `/home/user/.codesense/users/1234/orgs/${orgSlug}/${teamName}/repos/${owner}/${repo}`,
      teamContentPath: `/home/user/.codesense/users/1234/orgs/${orgSlug}/${teamName}/content`,
      hasRemote: true,
      remoteUrl: `https://github.com/${orgSlug}/context`,
      mainDoc: "",
      localPath: null,
    };
  }

  beforeEach(() => {
    useOrgStore.setState({ workspaceRepos: {} });
  });

  it("returns the synthetic repoPath of the workspace repo that owns the context dir", () => {
    const repo = makeRepoState("9north", "default", "acme", "api");
    useOrgStore.setState({ workspaceRepos: { [repo.repoPath]: repo } });

    expect(syntheticKeyForContextRepoPath(repo.contextRepoPath)).toBe(
      "workspace:9north:default:acme/api",
    );
  });

  it("resolves the correct team when the same slug exists in two teams", () => {
    const teamA = makeRepoState("9north", "team-a", "acme", "api");
    const teamB = makeRepoState("9north", "team-b", "acme", "api");
    useOrgStore.setState({
      workspaceRepos: { [teamA.repoPath]: teamA, [teamB.repoPath]: teamB },
    });

    // contextRepoPath is unique per (org, team, owner, repo), so the match is
    // exact — no cross-team bleed.
    expect(syntheticKeyForContextRepoPath(teamB.contextRepoPath)).toBe(
      "workspace:9north:team-b:acme/api",
    );
  });

  it("returns null when no workspace repo owns the context dir (e.g. _codesense handler)", () => {
    const repo = makeRepoState("9north", "default", "acme", "api");
    useOrgStore.setState({ workspaceRepos: { [repo.repoPath]: repo } });

    expect(
      syntheticKeyForContextRepoPath("/home/user/.codesense/users/1234/orgs/9north/_codesense"),
    ).toBeNull();
  });

  it("returns null for undefined or empty input", () => {
    expect(syntheticKeyForContextRepoPath(undefined)).toBeNull();
    expect(syntheticKeyForContextRepoPath("")).toBeNull();
  });

  it("returns null when workspaceRepos is empty", () => {
    expect(syntheticKeyForContextRepoPath("/any/context/path")).toBeNull();
  });
});

describe("syntheticKeyForContentPath", () => {
  // SW-NR-1: resolve an absolute content-layer path (a watcher-reported team
  // content edit, under `{org_root}/{team}/content`) to ANY one owning
  // team synthetic key — feeding it back through `bumpFileRefreshForSourceRepo`
  // fans out to every same-(org, team) sibling.
  function makeRepoState(
    orgSlug: string,
    teamName: string,
    owner: string,
    repo: string,
  ): RepoState {
    return {
      repoPath: `workspace:${orgSlug}:${teamName}:${owner}/${repo}`,
      repoName: `${repo} (${orgSlug})`,
      contextRepoPath: `/home/user/.codesense/users/1234/orgs/${orgSlug}/${teamName}/repos/${owner}/${repo}`,
      teamContentPath: `/home/user/.codesense/users/1234/orgs/${orgSlug}/${teamName}/content`,
      hasRemote: true,
      remoteUrl: `https://github.com/${orgSlug}/context`,
      mainDoc: "",
      localPath: null,
    };
  }

  beforeEach(() => {
    useOrgStore.setState({ workspaceRepos: {} });
  });

  it("resolves a content path under a team dir to that team's synthetic key", () => {
    const repo = makeRepoState("9north", "default", "acme", "api");
    useOrgStore.setState({ workspaceRepos: { [repo.repoPath]: repo } });

    const written = `${repo.teamContentPath}/_bmad-output/epics/03/SW-NR-1.md`;
    expect(syntheticKeyForContentPath(written)).toBe("workspace:9north:default:acme/api");
  });

  it("routes to the correct team when two teams have distinct content dirs", () => {
    const teamA = makeRepoState("9north", "team-a", "acme", "api");
    const teamB = makeRepoState("9north", "team-b", "acme", "api");
    useOrgStore.setState({
      workspaceRepos: { [teamA.repoPath]: teamA, [teamB.repoPath]: teamB },
    });

    expect(syntheticKeyForContentPath(`${teamB.teamContentPath}/x/y.md`)).toBe(
      "workspace:9north:team-b:acme/api",
    );
  });

  it("returns null when no team owns the path", () => {
    const repo = makeRepoState("9north", "default", "acme", "api");
    useOrgStore.setState({ workspaceRepos: { [repo.repoPath]: repo } });

    expect(syntheticKeyForContentPath("/some/unrelated/place/file.md")).toBeNull();
  });

  it("is path-boundary safe — `/content` must not match a `/content-archive` sibling", () => {
    const repo = makeRepoState("9north", "default", "acme", "api");
    useOrgStore.setState({ workspaceRepos: { [repo.repoPath]: repo } });

    // Same prefix string, different directory — must NOT resolve.
    expect(syntheticKeyForContentPath(`${repo.teamContentPath}-archive/x.md`)).toBeNull();
    // Exact-match on the content root itself DOES resolve (defensive edge).
    expect(syntheticKeyForContentPath(repo.teamContentPath)).toBe(
      "workspace:9north:default:acme/api",
    );
  });

  it("skips entries with an empty teamContentPath (non-synthetic own-repo rows)", () => {
    const ownRepo = makeRepoState("9north", "default", "acme", "api");
    ownRepo.teamContentPath = "";
    useOrgStore.setState({ workspaceRepos: { [ownRepo.repoPath]: ownRepo } });

    // An empty root must never prefix-match (would swallow every path).
    expect(syntheticKeyForContentPath("/anything/at/all.md")).toBeNull();
  });

  it("returns null for empty/undefined input", () => {
    expect(syntheticKeyForContentPath(undefined)).toBeNull();
    expect(syntheticKeyForContentPath("")).toBeNull();
  });
});

describe("bumpKeyForContextFileChange (G-8)", () => {
  const repo: RepoState = {
    repoPath: "workspace:9north:default:acme/api",
    repoName: "api (9north)",
    contextRepoPath: "/home/u/.codesense/users/1/orgs/9north/default/repos/acme/api",
    teamContentPath: "/home/u/.codesense/users/1/orgs/9north/default/content",
    hasRemote: true,
    remoteUrl: "https://github.com/9north/context",
    mainDoc: "",
    localPath: null,
  };

  beforeEach(() => {
    useOrgStore.setState({ workspaceRepos: { [repo.repoPath]: repo } });
  });

  it("routes a workspace subscriber's change by its context repo", () => {
    expect(
      bumpKeyForContextFileChange({
        repo_path: "/wt/api",
        context_repo_path: repo.contextRepoPath,
        file_path: `${repo.teamContentPath}/epic/S-1.md`,
      }),
    ).toBe(repo.repoPath);
  });

  // The team content watch fires with no workspace subscriber (only the team's
  // pipeline/board are open): the change names no repo, only the file — route it
  // to the team that owns the content dir so its tree rescans.
  it("routes a team-level change (no repo) by the content path it touched", () => {
    expect(
      bumpKeyForContextFileChange({
        repo_path: "",
        context_repo_path: "",
        file_path: `${repo.teamContentPath}/orphan-epic/ORPH-1.md`,
      }),
    ).toBe(repo.repoPath);
  });

  // SW-XS-20: a parent team's file changed while its child team is open — the
  // child's nav shows that file, so the child team rescans.
  it("routes a parent team's change to the team that inherits it", () => {
    expect(
      bumpKeyForContextFileChange({
        repo_path: "",
        context_repo_path: "",
        file_path: "/home/u/.codesense/users/1/orgs/9north/platform/content/rules/r.md",
        inherited_by: repo.teamContentPath,
      }),
    ).toBe(repo.repoPath);
  });

  it("falls back to the emitted repo path, and yields nothing when nothing names an owner", () => {
    expect(bumpKeyForContextFileChange({ repo_path: "/org/_codesense" })).toBe("/org/_codesense");
    expect(bumpKeyForContextFileChange({ repo_path: "", file_path: "/elsewhere/x.md" })).toBeNull();
  });
});
