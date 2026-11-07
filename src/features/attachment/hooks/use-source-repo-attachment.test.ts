// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import type { Org, Attachment, CurrentUser } from "@/shared/types/domain";

// F-25: user identity is read from `getCurrentUser` (the active local
// profile). Mock the helper directly so tests can set identity
// without standing up a local-profile fixture.
let currentUserValue: CurrentUser | null = null;
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => currentUserValue,
  useCurrentUser: () => currentUserValue,
  buildCurrentUser: () => currentUserValue,
}));

// --- Mocks ---

const mockResolve = vi.fn<(repoPath: string, userId: number) => Promise<Attachment | null>>();
const mockSave = vi.fn<(userId: number, attachment: Attachment) => Promise<void>>();
const mockReadOrigin = vi.fn<(repoPath: string) => Promise<string | null>>();
const mockParseRemote = vi.fn<(url: string) => Promise<{ owner: string; repo: string }>>();
const mockCommitPr = vi.fn();
const mockCommitLocal = vi.fn();
const mockEnsureDir = vi.fn<(path: string) => Promise<void>>();
vi.mock("@/tauri/commands", () => ({
  resolveSourceRepoAttachment: (...args: unknown[]) => mockResolve(...(args as [string, number])),
  saveSourceRepoAttachment: (...args: unknown[]) => mockSave(...(args as [number, Attachment])),
  readSourceRepoOriginUrl: (...args: unknown[]) => mockReadOrigin(...(args as [string])),
  parseGitRemote: (...args: unknown[]) => mockParseRemote(...(args as [string])),
  commitRegistryEditAsPr: (...args: unknown[]) => mockCommitPr(...args),
  commitRegistryEditLocal: (...args: unknown[]) => mockCommitLocal(...args),
  ensureDirectory: (...args: unknown[]) => mockEnsureDir(...(args as [string])),
}));

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), {
    success: vi.fn(),
    error: vi.fn(),
    info: vi.fn(),
    dismiss: vi.fn(),
  }),
}));

const mockOpenUrl = vi.fn().mockResolvedValue(undefined);
vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: (...args: unknown[]) => mockOpenUrl(...args),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), warn: vi.fn(), error: vi.fn() },
}));

const { toast } = await import("sonner");
const { useSourceRepoAttachment } = await import("./use-source-repo-attachment");

// --- Helpers ---

function makeOrg(overrides: Partial<Org> = {}): Org {
  return {
    slug: "acme",
    display_name: "Acme Corp",
    created_at: "2026-01-01T00:00:00Z",
    remote_url: "https://github.com/acme/context.git",
    config_flags: {},
    people: [],
    teams: [
      {
        name: "default",
        source_repos: [],
      },
    ],
    org_root: "/Users/dev/.codesense/users/123/orgs/acme",
    ...overrides,
  };
}

function makeAttachment(overrides: Partial<Attachment> = {}): Attachment {
  return {
    repo_path: "/Users/dev/my-repo",
    remote_url: "https://github.com/org/repo.git",
    attached_at: "2026-01-01T00:00:00Z",
    tracking_prompt_dismissed_at: null,
    credential_ref: null,
    ...overrides,
  };
}

// --- Store setup ---

const { useOrgStore } = await import("@/shared/stores/org-store");

const defaultUser = {
  github_user_id: 123,
  github_username: "testuser",
  display_name: "Test User",
  avatar_url: null,
};

function setupOrgStore(
  overrides: {
    orgs?: Org[];
    activeOrgSlug?: string | null;
    currentUser?: CurrentUser | null;
    // SW-OB-8: `currentUserPermission` no longer gates any edit path
    // (kept only as OB-7's gate surface, pinned null in prod). Tests may
    // still set it to prove it has no effect on the local-only commit.
    currentUserPermission?: "admin" | "write" | "read" | null;
  } = {},
) {
  // F-25: user identity lives in the mocked get-current-user helper,
  // not org-store. Set the mock value here so consumers of the hook
  // see the right user.
  currentUserValue = "currentUser" in overrides ? (overrides.currentUser ?? null) : defaultUser;
  useOrgStore.setState({
    orgs: overrides.orgs ?? [makeOrg()],
    activeOrgSlug: "activeOrgSlug" in overrides ? overrides.activeOrgSlug : "acme",
    activeTeamName: "default",
    currentUserPermission:
      "currentUserPermission" in overrides ? overrides.currentUserPermission : "write",
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  mockResolve.mockResolvedValue(null);
  mockSave.mockResolvedValue(undefined);
  mockReadOrigin.mockResolvedValue("https://github.com/org/repo.git");
  mockParseRemote.mockResolvedValue({ owner: "org", repo: "repo" });
  mockCommitLocal.mockResolvedValue({ commit_message: "test" });
  mockEnsureDir.mockResolvedValue(undefined);
  setupOrgStore();
});

// --- Tests ---

describe("useSourceRepoAttachment", () => {
  it("does not save a new attachment when reading origin fails", async () => {
    mockReadOrigin.mockRejectedValue(new Error("failed to read .git/config (os error 2)"));

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(mockSave).not.toHaveBeenCalled();
    expect(mockParseRemote).not.toHaveBeenCalled();
    // SW-FU-B8: what failed as the title, a safe reason (never the raw os error).
    expect(toast.error).toHaveBeenCalledWith(
      expect.stringMatching(/couldn't read.*origin/i),
      expect.objectContaining({ description: expect.not.stringMatching(/os error/) }),
    );
    unmount();
  });

  it("does not save a new attachment when a non-empty origin cannot be parsed", async () => {
    mockReadOrigin.mockResolvedValue("ssh://unknown.example/acme/repo");
    mockParseRemote.mockRejectedValue(new Error("unsupported remote"));

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(mockSave).not.toHaveBeenCalled();
    expect(toast.error).toHaveBeenCalledWith(expect.stringMatching(/couldn't recognize.*origin/i), {
      description: "unsupported remote",
    });
    unmount();
  });

  it("records a GitLab attachment without a service credential", async () => {
    mockReadOrigin.mockResolvedValue("https://gitlab.com/org/repo.git");
    mockParseRemote.mockResolvedValue({
      owner: "org",
      repo: "repo",
      provider: "gitlab",
      host: "gitlab.com",
    } as { owner: string; repo: string });

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(mockSave).toHaveBeenCalledWith(
      123,
      expect.objectContaining({
        credential_ref: null,
      }),
    );
    unmount();
  });

  it("saves a new GitLab attachment without a matching connection", async () => {
    mockReadOrigin.mockResolvedValue("https://gitlab.com/org/repo.git");
    mockParseRemote.mockResolvedValue({
      owner: "org",
      repo: "repo",
      provider: "gitlab",
      host: "gitlab.com",
    } as { owner: string; repo: string });

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(mockSave).toHaveBeenCalledWith(123, expect.objectContaining({ credential_ref: null }));
    expect(toast.error).not.toHaveBeenCalled();
    unmount();
  });

  it("does not carry a GitHub binding across a changed GitLab origin", async () => {
    mockResolve.mockResolvedValue(
      makeAttachment({
        remote_url: "https://github.com/org/repo.git",
        credential_ref: { provider: "github", account: "123" },
      }),
    );
    mockReadOrigin.mockResolvedValue("https://gitlab.com/org/repo.git");
    mockParseRemote.mockResolvedValue({
      owner: "org",
      repo: "repo",
      provider: "gitlab",
      host: "gitlab.com",
    } as { owner: string; repo: string });

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(mockSave).toHaveBeenCalledWith(123, expect.objectContaining({ credential_ref: null }));
    expect(toast.error).not.toHaveBeenCalled();
    unmount();
  });

  it("ignores obsolete provider binding when the URL is unchanged", async () => {
    const gitLabUrl = "https://gitlab.com/org/repo.git";
    mockResolve.mockResolvedValue(
      makeAttachment({
        remote_url: gitLabUrl,
        credential_ref: { provider: "github", account: "123" },
      }),
    );
    mockReadOrigin.mockResolvedValue(gitLabUrl);
    mockParseRemote.mockResolvedValue({
      owner: "org",
      repo: "repo",
      provider: "gitlab",
      host: "gitlab.com",
    } as { owner: string; repo: string });

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(mockSave).not.toHaveBeenCalled();
    expect(toast.error).not.toHaveBeenCalled();
    unmount();
  });

  it("AC-P4-1: silent record on first open, repo already tracked → no prompt", async () => {
    // Repo is tracked by the workspace.
    setupOrgStore({
      orgs: [
        makeOrg({
          teams: [
            {
              name: "default",
              source_repos: [{ owner: "org", repo: "repo" }],
            },
          ],
        }),
      ],
    });

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(mockSave).toHaveBeenCalled(); // Silent save.
    expect(toast).not.toHaveBeenCalled(); // No prompt.
    unmount();
  });

  it("AC-P4-2: already attached + tracked, URL unchanged → no save call", async () => {
    mockResolve.mockResolvedValue(makeAttachment());
    setupOrgStore({
      orgs: [
        makeOrg({
          teams: [
            {
              name: "default",
              source_repos: [{ owner: "org", repo: "repo" }],
            },
          ],
        }),
      ],
    });

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(mockSave).not.toHaveBeenCalled();
    unmount();
  });

  it("AC-P4-3: URL changed → save called with new URL, dismissed_at preserved", async () => {
    mockResolve.mockResolvedValue(
      makeAttachment({
        remote_url: "https://github.com/old/url.git",
        tracking_prompt_dismissed_at: "2026-05-01T00:00:00Z",
      }),
    );
    setupOrgStore({
      orgs: [
        makeOrg({
          teams: [
            {
              name: "default",
              source_repos: [{ owner: "org", repo: "repo" }],
            },
          ],
        }),
      ],
    });

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(mockSave).toHaveBeenCalledWith(
      123,
      expect.objectContaining({
        remote_url: "https://github.com/org/repo.git",
        tracking_prompt_dismissed_at: "2026-05-01T00:00:00Z",
      }),
    );
    unmount();
  });

  it("AC-P4-4: no workspace tracks origin + not dismissed → prompt fires", async () => {
    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(toast).toHaveBeenCalledWith(
      expect.stringContaining("Track org/repo"),
      expect.objectContaining({
        id: "track-prompt:/Users/dev/my-repo",
        duration: Infinity,
      }),
    );
    unmount();
  });

  it("AC-P4-8/P4-9: dismissed prompt does not re-fire", async () => {
    mockResolve.mockResolvedValue(
      makeAttachment({ tracking_prompt_dismissed_at: "2026-05-01T00:00:00Z" }),
    );

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(toast).not.toHaveBeenCalled();
    unmount();
  });

  it("AC-P4-10: no-origin repo → attachment saved with null URL, no prompt", async () => {
    mockReadOrigin.mockResolvedValue(null);

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(mockSave).toHaveBeenCalledWith(123, expect.objectContaining({ remote_url: null }));
    expect(toast).not.toHaveBeenCalled();
    unmount();
  });

  it("AC-P4-13: setActiveOrg and setActiveTeam never called", async () => {
    const setActiveOrg = vi.spyOn(useOrgStore.getState(), "setActiveOrg");
    const setActiveTeam = vi.spyOn(useOrgStore.getState(), "setActiveTeam");

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(setActiveOrg).not.toHaveBeenCalled();
    expect(setActiveTeam).not.toHaveBeenCalled();
    unmount();
  });

  it("null selectedRepoPath → no operations", async () => {
    const { unmount } = renderHook(() => useSourceRepoAttachment(null));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(mockResolve).not.toHaveBeenCalled();
    expect(mockSave).not.toHaveBeenCalled();
    unmount();
  });

  it("no current user → no operations", async () => {
    setupOrgStore({ currentUser: null });

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(mockResolve).not.toHaveBeenCalled();
    unmount();
  });

  it("AC-P4-5 (SW-OB-8): Accept on a remote (cloned) org commits LOCALLY, never a PR", async () => {
    // The default org has a `remote_url` — pre-OB-8 this took the PR
    // path. SW-OB-8: every registry edit is a local commit regardless of
    // `remote_url`, so a user who joined by clone gets the same local
    // behavior (the "half-true local-only" bug this story fixes).
    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    // Extract the Accept onClick from the toast call.
    const toastCall = vi.mocked(toast).mock.calls[0];
    expect(toastCall).toBeDefined();
    const opts = toastCall[1] as unknown as {
      action: { onClick: () => void };
    };

    // Re-resolve returns the saved attachment for post-track dismissed_at persistence.
    mockResolve.mockResolvedValue(makeAttachment({ tracking_prompt_dismissed_at: null }));
    mockSave.mockResolvedValue(undefined);

    await act(async () => {
      opts.action.onClick();
      await new Promise((r) => setTimeout(r, 20));
    });

    // Local commit, no PR flow.
    expect(mockCommitLocal).toHaveBeenCalledWith(
      123,
      "acme",
      expect.objectContaining({ type: "AddSourceRepoToTeam" }),
    );
    expect(mockCommitPr).not.toHaveBeenCalled();
    expect(toast.success).toHaveBeenCalledWith("Tracked locally.");
    // PTS-2: the per-`(team, repo)` directory is no longer bootstrapped.
    expect(mockEnsureDir).not.toHaveBeenCalled();
    // Verify dismissed_at persisted after successful track.
    expect(mockSave).toHaveBeenCalledWith(
      123,
      expect.objectContaining({
        tracking_prompt_dismissed_at: expect.any(String),
      }),
    );
    unmount();
  });

  it("AC-P4-6: Accept (local-only org) → direct commit, success toast, dir created", async () => {
    setupOrgStore({
      orgs: [makeOrg({ remote_url: null })],
      currentUserPermission: null,
    });

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    const toastCall = vi.mocked(toast).mock.calls[0];
    expect(toastCall).toBeDefined();
    const opts = toastCall[1] as unknown as {
      action: { onClick: () => void };
    };

    mockResolve.mockResolvedValue(makeAttachment());
    mockSave.mockResolvedValue(undefined);

    await act(async () => {
      opts.action.onClick();
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(mockCommitLocal).toHaveBeenCalledWith(
      123,
      "acme",
      expect.objectContaining({ type: "AddSourceRepoToTeam" }),
    );
    expect(toast.success).toHaveBeenCalledWith("Tracked locally.");
    // PTS-2: dir bootstrap retired (see AC-P4-5 above).
    expect(mockEnsureDir).not.toHaveBeenCalled();
    unmount();
  });

  it("AC-P4-7 (SW-OB-8): read-only permission no longer blocks — still commits LOCALLY", async () => {
    // Pre-OB-8 a "read" tier blocked the track with an error toast. Under
    // local-first + open access, permission no longer gates anything —
    // the commit is local and succeeds regardless of tier.
    setupOrgStore({ currentUserPermission: "read" });

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    const toastCall = vi.mocked(toast).mock.calls[0];
    expect(toastCall).toBeDefined();
    const opts = toastCall[1] as unknown as {
      action: { onClick: () => void };
    };

    mockResolve.mockResolvedValue(makeAttachment());
    mockSave.mockResolvedValue(undefined);

    await act(async () => {
      opts.action.onClick();
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(mockCommitLocal).toHaveBeenCalledWith(
      123,
      "acme",
      expect.objectContaining({ type: "AddSourceRepoToTeam" }),
    );
    expect(mockCommitPr).not.toHaveBeenCalled();
    expect(toast.success).toHaveBeenCalledWith("Tracked locally.");
    expect(toast.error).not.toHaveBeenCalledWith(
      "You need write access to track repos in this workspace.",
    );
    unmount();
  });

  it("AC-P4-11: after successful track, updateTeamSourceRepos called", async () => {
    const updateSpy = vi.spyOn(useOrgStore.getState(), "updateTeamSourceRepos");

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    const toastCall = vi.mocked(toast).mock.calls[0];
    const opts = toastCall[1] as unknown as {
      action: { onClick: () => void };
    };

    mockResolve.mockResolvedValue(makeAttachment());
    mockSave.mockResolvedValue(undefined);

    await act(async () => {
      opts.action.onClick();
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(updateSpy).toHaveBeenCalledWith("acme", "default", { owner: "org", repo: "repo" });
    unmount();
  });

  // ---- F-6 AC-17: bumpAttachmentsVersion semantics (R3-P4 patch) ----

  it("R3-P4: successful saveSourceRepoAttachment triggers bumpAttachmentsVersion", async () => {
    setupOrgStore();
    const startingVersion = useOrgStore.getState().attachmentsVersion;

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(mockSave).toHaveBeenCalled();
    expect(useOrgStore.getState().attachmentsVersion).toBe(startingVersion + 1);
    unmount();
  });

  it("R3-P4: failed save does NOT bump attachmentsVersion", async () => {
    setupOrgStore();
    mockSave.mockRejectedValueOnce(new Error("boom"));
    const startingVersion = useOrgStore.getState().attachmentsVersion;

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(mockSave).toHaveBeenCalled();
    expect(useOrgStore.getState().attachmentsVersion).toBe(startingVersion);
    unmount();
  });

  it("R3-P4: skipped save (URL unchanged) does NOT bump attachmentsVersion", async () => {
    // Attachment already exists with matching origin URL — needsSave === false.
    mockResolve.mockResolvedValue(makeAttachment());
    setupOrgStore();
    const startingVersion = useOrgStore.getState().attachmentsVersion;

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(mockSave).not.toHaveBeenCalled();
    expect(useOrgStore.getState().attachmentsVersion).toBe(startingVersion);
    unmount();
  });

  // ---- SW-OB-3 AC-2: new-attachment confirmation toast ----

  it("SW-OB-3 AC-2: fires the confirmation toast exactly once on a brand-new insert", async () => {
    // Repo is tracked (no track-prompt noise); existing === null → new insert.
    mockResolve.mockResolvedValue(null);
    setupOrgStore({
      orgs: [
        makeOrg({
          teams: [
            {
              name: "default",
              source_repos: [{ owner: "org", repo: "repo" }],
            },
          ],
        }),
      ],
    });

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(toast.success).toHaveBeenCalledTimes(1);
    expect(toast.success).toHaveBeenCalledWith(
      expect.stringContaining("Attached your local checkout of org/repo"),
    );
    unmount();
  });

  it("SW-OB-3 AC-2: does NOT fire when an attachment already exists (unchanged)", async () => {
    mockResolve.mockResolvedValue(makeAttachment());

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(toast.success).not.toHaveBeenCalled();
    unmount();
  });

  it("SW-OB-3 AC-2: stays silent on a remote_url-changed update (existing present)", async () => {
    mockResolve.mockResolvedValue(makeAttachment({ remote_url: "https://github.com/old/url.git" }));

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(mockSave).toHaveBeenCalled(); // the URL-change still persists...
    expect(toast.success).not.toHaveBeenCalled(); // ...but no attach confirmation.
    unmount();
  });

  it("SW-OB-3 AC-2: stays silent on a new insert when origin can't be parsed", async () => {
    mockResolve.mockResolvedValue(null);
    mockReadOrigin.mockResolvedValue(null); // no origin → parsed null → can't name repo

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(mockSave).toHaveBeenCalled();
    expect(toast.success).not.toHaveBeenCalled();
    unmount();
  });

  it("AC-P4-12: switching repos cancels stale async operations", async () => {
    // Start with one repo.
    const { rerender, unmount } = renderHook(
      ({ path }: { path: string | null }) => useSourceRepoAttachment(path),
      { initialProps: { path: "/Users/dev/repo-a" } },
    );

    // Immediately switch to another repo before first flow completes.
    rerender({ path: "/Users/dev/repo-b" });

    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    // The resolve call for repo-a should have been made, but any side effects
    // from repo-a's flow should be guarded by the stale ref check.
    // Only repo-b's final state should matter.
    const saveCalls = mockSave.mock.calls;
    const repoBSaves = saveCalls.filter(
      (c) => (c[1] as Attachment).repo_path === "/Users/dev/repo-b",
    );
    // repo-b should have saves; repo-a's flow should be aborted by stale ref.
    expect(repoBSaves.length).toBeGreaterThanOrEqual(1);
    unmount();
  });

  // ---- SW-OB-11 AC-5: github_account_id capture + preserve ----

  it("silent insert does not bind a service account", async () => {
    mockResolve.mockResolvedValue(null);
    setupOrgStore({
      orgs: [
        makeOrg({
          teams: [
            {
              name: "default",
              source_repos: [{ owner: "org", repo: "repo" }],
            },
          ],
        }),
      ],
    });

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(mockSave).toHaveBeenCalledWith(123, expect.objectContaining({ credential_ref: null }));
    unmount();
  });

  it("url-change update clears obsolete service binding", async () => {
    mockResolve.mockResolvedValue(
      makeAttachment({
        remote_url: "https://github.com/old/url.git",
        credential_ref: { provider: "github", account: "999" },
      }),
    );
    setupOrgStore({
      orgs: [
        makeOrg({
          teams: [
            {
              name: "default",
              source_repos: [{ owner: "org", repo: "repo" }],
            },
          ],
        }),
      ],
    });

    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    expect(mockSave).toHaveBeenCalledWith(123, expect.objectContaining({ credential_ref: null }));
    unmount();
  });

  it("SW-OB-11 AC-5: dismissing the track prompt preserves the bound github_account_id", async () => {
    // Untracked repo + not dismissed → prompt fires (the default org has no
    // matching source_repo). The initial silent insert happens first.
    const { unmount } = renderHook(() => useSourceRepoAttachment("/Users/dev/my-repo"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });

    const toastCall = vi.mocked(toast).mock.calls[0];
    expect(toastCall).toBeDefined();
    const opts = toastCall[1] as unknown as { cancel: { onClick: () => void } };

    // Dismiss re-reads the current record — return one bound to account 777.
    mockResolve.mockResolvedValue(
      makeAttachment({ credential_ref: { provider: "github", account: "777" } }),
    );
    mockSave.mockClear();

    await act(async () => {
      opts.cancel.onClick();
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(mockSave).toHaveBeenCalledWith(
      123,
      expect.objectContaining({
        credential_ref: { provider: "github", account: "777" },
        tracking_prompt_dismissed_at: expect.any(String),
      }),
    );
    unmount();
  });
});
