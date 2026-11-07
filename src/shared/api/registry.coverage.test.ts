// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";

import type {
  FileLoadError,
  LoadedOrg,
  RegistryFile,
  FileLoadErrorKind,
} from "@/shared/types/registry";

const { mockInvoke, mockOpenPath } = vi.hoisted(() => ({
  mockInvoke: vi.fn(),
  mockOpenPath: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mockInvoke,
}));

vi.mock("@tauri-apps/plugin-opener", () => ({
  openPath: mockOpenPath,
}));

import { loadOrg, openRegistryFileForRepair } from "./registry";

function cleanLoadedOrg(): LoadedOrg {
  return {
    slug: "alpha-corp",
    root: "/tmp/orgs/alpha-corp",
    org: {
      slug: "alpha-corp",
      display_name: "Alpha Corp",
      created_at: "2026-01-01T00:00:00Z",
      remote_url: "git@github.com:alpha/context.git",
      config_flags: {},
      people: [
        {
          github_user_id: 1,
          github_username: "alice",
          display_name: "Alice",
          avatar_url: null,
        },
      ],
      teams: [
        {
          name: "backend",
          source_repos: [{ owner: "alderco", repo: "codesense" }],
        },
      ],
      org_root: "",
    },
    partial: {
      org_yml: {
        slug: "alpha-corp",
        display_name: "Alpha Corp",
        created_at: "2026-01-01T00:00:00Z",
        config_flags: {},
      },
      people_yml: {
        people: [
          {
            github_user_id: 1,
            github_username: "alice",
            display_name: "Alice",
            avatar_url: null,
          },
        ],
      },
      teams_yml: {
        teams: [
          {
            name: "backend",
            source_repos: [{ owner: "alderco", repo: "codesense" }],
          },
        ],
      },
    },
    file_errors: [],
  };
}

function brokenLoadedOrg(): LoadedOrg {
  const file: RegistryFile = "TeamsYml";
  const kind: FileLoadErrorKind = "Parse";
  const err: FileLoadError = {
    file,
    path: "/tmp/orgs/alpha-corp/teams.yml",
    kind,
    message: "teams.yml failed to parse at line 3, column 1: expected sequence",
  };
  const clean = cleanLoadedOrg();
  return {
    ...clean,
    org: null,
    partial: { ...clean.partial, teams_yml: null },
    file_errors: [err],
  };
}

describe("loadOrg", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("invokes load_org_cmd with remoteUrl = null for local-only orgs", async () => {
    const fixture = cleanLoadedOrg();
    mockInvoke.mockResolvedValueOnce(fixture);
    const result = await loadOrg("personal-foo", "/tmp/orgs/foo", null);
    expect(mockInvoke).toHaveBeenCalledTimes(1);
    expect(mockInvoke).toHaveBeenCalledWith("load_org_cmd", {
      slug: "personal-foo",
      orgRoot: "/tmp/orgs/foo",
      remoteUrl: null,
    });
    expect(result).toBe(fixture);
  });

  it("passes the remote URL through for work-org loads", async () => {
    const fixture = cleanLoadedOrg();
    mockInvoke.mockResolvedValueOnce(fixture);
    await loadOrg("alpha", "/tmp/orgs/alpha", "git@github.com:alpha-corp/context.git");
    expect(mockInvoke).toHaveBeenCalledWith("load_org_cmd", {
      slug: "alpha",
      orgRoot: "/tmp/orgs/alpha",
      remoteUrl: "git@github.com:alpha-corp/context.git",
    });
  });

  it("passes the active Principal instead of a provider account id", async () => {
    mockInvoke.mockResolvedValueOnce(cleanLoadedOrg());
    await loadOrg("alpha", "/tmp/orgs/alpha", null, "principal-42");
    expect(mockInvoke).toHaveBeenCalledWith("load_org_cmd", {
      slug: "alpha",
      orgRoot: "/tmp/orgs/alpha",
      remoteUrl: null,
      principal: "principal-42",
    });
  });
});

describe("openRegistryFileForRepair", () => {
  beforeEach(() => {
    mockOpenPath.mockReset();
  });

  it("calls openPath once with the exact file path", async () => {
    mockOpenPath.mockResolvedValueOnce(undefined);
    await openRegistryFileForRepair("/tmp/orgs/foo/teams.yml");
    expect(mockOpenPath).toHaveBeenCalledTimes(1);
    expect(mockOpenPath).toHaveBeenCalledWith("/tmp/orgs/foo/teams.yml");
  });
});

describe("LoadedOrg type fixtures round-trip without any casts", () => {
  it("accepts a fully-valid LoadedOrg", () => {
    const clean: LoadedOrg = cleanLoadedOrg();
    expect(clean.org).not.toBeNull();
    expect(clean.file_errors).toHaveLength(0);
  });

  it("accepts a partially-broken LoadedOrg", () => {
    const broken: LoadedOrg = brokenLoadedOrg();
    expect(broken.org).toBeNull();
    expect(broken.file_errors).toHaveLength(1);
    expect(broken.file_errors[0].file).toBe<RegistryFile>("TeamsYml");
    expect(broken.file_errors[0].kind).toBe<FileLoadErrorKind>("Parse");
  });

  it("accepts a ReadError error kind", () => {
    const err: FileLoadError = {
      file: "OrgYml",
      path: "/tmp/orgs/alpha/org.yml",
      kind: "ReadError",
      message: "permission denied",
    };
    const loaded: LoadedOrg = {
      slug: "alpha",
      root: "/tmp/orgs/alpha",
      org: null,
      partial: {
        org_yml: null,
        people_yml: null,
        teams_yml: null,
      },
      file_errors: [err],
    };
    expect(loaded.file_errors[0].kind).toBe<FileLoadErrorKind>("ReadError");
  });
});
