// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

// --- Mocks ---

const mockOpenDialog = vi.fn<() => Promise<string | null>>();
const mockAttachClone = vi.fn<(call: unknown) => Promise<unknown>>();

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (...args: unknown[]) => mockOpenDialog(...(args as [])),
}));

vi.mock("@/tauri/commands", () => ({
  attachSourceRepoClone: (call: unknown) => mockAttachClone(call),
  isAddRepoError: (e: unknown) => e instanceof Error && "kind" in e,
}));

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), {
    success: vi.fn(),
    error: vi.fn(),
    info: vi.fn(),
  }),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), warn: vi.fn(), error: vi.fn() },
}));

const mockBumpAttachmentsVersion = vi.fn();
const mockConsumeRetry = vi.fn();
const mockDismissNoClone = vi.fn();
let mockWorkspaceRepos: Record<string, { repoPath: string; localPath: string | null }> = {};
// T.4 AC-3: noClonePromptRepo is `{teamKey, sourceRepoSlug} | null`.
let mockNoClonePromptRepo: { teamKey: string; sourceRepoSlug: string } | null = {
  teamKey: "acme:default",
  sourceRepoSlug: "owner/repo",
};
let storeListeners: Array<(s: unknown) => void> = [];

vi.mock("@/shared/stores/org-store", () => {
  const getState = () => ({
    workspaceRepos: mockWorkspaceRepos,
    bumpAttachmentsVersion: mockBumpAttachmentsVersion,
  });
  const store = (sel?: (s: unknown) => unknown) => (sel ? sel(getState()) : getState());
  store.getState = getState;
  store.subscribe = (listener: (s: unknown) => void) => {
    storeListeners.push(listener);
    return () => {
      storeListeners = storeListeners.filter((l) => l !== listener);
    };
  };
  return { useOrgStore: store };
});

// F-25: saveAndClose reads identity via getCurrentUser (the active local profile).
const mockCurrentUser = {
  github_user_id: 42,
  github_username: "testuser",
  display_name: "testuser",
  avatar_url: null,
};
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => mockCurrentUser,
  useCurrentUser: () => mockCurrentUser,
  buildCurrentUser: () => mockCurrentUser,
}));

vi.mock("@/shared/stores/viewer-store", () => {
  const getState = () => ({
    consumeNoClonePromptRetry: mockConsumeRetry,
    dismissNoClonePrompt: mockDismissNoClone,
    noClonePromptRepo: mockNoClonePromptRepo,
  });
  const store = (sel?: (s: unknown) => unknown) => (sel ? sel(getState()) : getState());
  store.getState = getState;
  return {
    useViewerStore: store,
    // T.4 AC-3: dialog imports this helper to derive `{teamKey, sourceRepoSlug}`
    // from `targetRepoPath` for matching against the prompt.
    promptIdentityFromRepoPath: (repoPath: string) => {
      if (!repoPath.startsWith("workspace:")) return null;
      const parts = repoPath.slice("workspace:".length).split(":");
      if (parts.length !== 3) return null;
      const [orgSlug, team, ownerRepo] = parts;
      const slash = ownerRepo.split("/");
      if (slash.length !== 2) return null;
      return {
        teamKey: `${orgSlug}:${team}`,
        sourceRepoSlug: `${slash[0]}/${slash[1]}`.toLowerCase(),
      };
    },
  };
});

// Trigger a store update (notifies all subscribers).
function emitStoreUpdate(): void {
  const snapshot = {
    workspaceRepos: mockWorkspaceRepos,
    bumpAttachmentsVersion: mockBumpAttachmentsVersion,
  };
  for (const listener of [...storeListeners]) listener(snapshot);
}

const { AttachLocalCloneDialog } = await import("./attach-local-clone-dialog");

const DEFAULT_PROPS = {
  owner: "owner",
  repo: "repo",
  displayLabel: "owner/repo",
  targetRepoPath: "workspace:acme:default:owner/repo",
  open: true,
  onOpenChange: vi.fn(),
};

/** A typed refusal as `attachSourceRepoClone` rejects with it. */
function refusal(kind: string, fields: Record<string, unknown>): Error {
  return Object.assign(new Error(`${kind} refusal`), { kind, ...fields });
}

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  vi.clearAllMocks();
  mockOpenDialog.mockReset();
  mockAttachClone.mockReset();
  // The Rust core accepts the pick unless a test says otherwise.
  mockAttachClone.mockResolvedValue({});
  mockWorkspaceRepos = {};
  mockNoClonePromptRepo = { teamKey: "acme:default", sourceRepoSlug: "owner/repo" };
  storeListeners = [];
});

// Radix FocusScope queues its unmount auto-focus on a setTimeout. Under fake
// timers that callback stays pending after unmount, and the real interval
// behind `shouldAdvanceTime` can fire it after happy-dom is torn down, where
// `new CustomEvent` is no longer happy-dom's Event and `dispatchEvent` throws.
// Unmount, flush that timer while the DOM is live, then restore real timers.
afterEach(() => {
  cleanup();
  act(() => {
    vi.runOnlyPendingTimers();
  });
  vi.useRealTimers();
});

describe("AttachLocalCloneDialog — matching origin happy path", () => {
  it("saves attachment, bumps version, and consumes retry once localPath populates", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/Users/dev/owner-repo");

    const onOpenChange = vi.fn();
    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} onOpenChange={onOpenChange} />);

    await user.click(screen.getByRole("button", { name: /browse/i }));

    // Save fires + version bump happens before the watcher waits.
    await waitFor(() => expect(mockAttachClone).toHaveBeenCalled());
    // The picked folder goes to the Rust core as-is; it checks the working
    // tree and origin.
    expect(mockAttachClone).toHaveBeenCalledWith({
      githubUserId: 42,
      repo: { owner: "owner", repo: "repo", provider: "github", host: null },
      path: "/Users/dev/owner-repo",
      // No connections in the mock, but the user IS signed in → falls back
      // to the active user's id (42), consistent with the silent-record hook.
    });
    expect(mockBumpAttachmentsVersion).toHaveBeenCalledTimes(1);

    // Simulate the soft-refresh populating localPath.
    mockWorkspaceRepos["workspace:acme:default:owner/repo"] = {
      repoPath: "workspace:acme:default:owner/repo",
      localPath: "/Users/dev/owner-repo",
    };
    emitStoreUpdate();

    await waitFor(() => expect(mockConsumeRetry).toHaveBeenCalledTimes(1));
    expect(mockDismissNoClone).not.toHaveBeenCalled();
  });
});

describe("AttachLocalCloneDialog — SW-OB-3 copy + confirmation", () => {
  it("AC-3: states what happens next (the shared LOCAL_CLONE_EXPLAINER copy)", () => {
    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} />);
    expect(
      screen.getByText(
        /your local checkout is what codesense shows alongside the team's context\./i,
      ),
    ).toBeInTheDocument();
  });

  it("AC-2: a successful attach confirms via the shared helper, naming owner/repo", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/Users/dev/owner-repo");
    const { toast } = await import("sonner");

    // Omit targetRepoPath so no 5s auto-retry watcher runs during the assert.
    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} targetRepoPath={undefined} />);
    await user.click(screen.getByRole("button", { name: /browse/i }));

    await waitFor(() => expect(mockAttachClone).toHaveBeenCalled());
    expect(toast.success).toHaveBeenCalledWith(
      expect.stringContaining("Attached your local checkout of owner/repo"),
    );
  });
});

describe("AttachLocalCloneDialog — mismatched origin confirmation", () => {
  it("rejects the same owner/repo on another provider without an override", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/Users/dev/forked-clone");
    mockAttachClone.mockRejectedValue(
      refusal("originMismatch", {
        expected: "github.com/owner/repo",
        actual: "github.com/other/repo",
      }),
    );

    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} />);
    await user.click(screen.getByRole("button", { name: /browse/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(
      "That clone is github.com/other/repo, but this team's repo is github.com/owner/repo.",
    );
    expect(screen.queryByRole("button", { name: /attach anyway/i })).not.toBeInTheDocument();
    expect(mockBumpAttachmentsVersion).not.toHaveBeenCalled();
    // The picker stays reachable for another try.
    expect(screen.getByRole("button", { name: /browse/i })).toBeInTheDocument();
  });

  it("shows an inline error and stays in picker phase on save failure", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/Users/dev/owner-repo");
    mockAttachClone.mockRejectedValue(new Error("disk full"));

    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} />);
    await user.click(screen.getByRole("button", { name: /browse/i }));

    await waitFor(() => expect(mockAttachClone).toHaveBeenCalled());
    // bumpAttachmentsVersion must NOT fire on save failure
    expect(mockBumpAttachmentsVersion).not.toHaveBeenCalled();
    // Error surface to user
    await waitFor(() =>
      expect(screen.getByText(/couldn't save the attachment: disk full/i)).toBeInTheDocument(),
    );
  });
});

describe("SW-FU-C1: attach errors never show a typed prefix", () => {
  it("a typed save failure shows its reason in words", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/Users/dev/owner-repo");
    mockAttachClone.mockRejectedValue(new Error("denied: the settings file is locked"));

    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} />);
    await user.click(screen.getByRole("button", { name: /browse/i }));

    await waitFor(() =>
      expect(
        screen.getByText("Couldn't save the attachment: the settings file is locked"),
      ).toBeInTheDocument(),
    );
    expect(screen.queryByText(/denied:/)).not.toBeInTheDocument();
  });
});

describe("AttachLocalCloneDialog — already-populated fast path", () => {
  it("consumes retry immediately when localPath is already non-null at watcher start", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    // Pre-populate the store so waitForLocalPathPopulated's fast-return hits.
    mockWorkspaceRepos["workspace:acme:default:owner/repo"] = {
      repoPath: "workspace:acme:default:owner/repo",
      localPath: "/Users/dev/owner-repo",
    };
    mockOpenDialog.mockResolvedValue("/Users/dev/owner-repo");

    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} />);
    await user.click(screen.getByRole("button", { name: /browse/i }));

    await waitFor(() => expect(mockConsumeRetry).toHaveBeenCalledTimes(1));
  });
});

describe("AttachLocalCloneDialog — timeout fallback (R2-P3 patch)", () => {
  it("calls dismissNoClonePrompt only if the prompt still targets the same repo", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/Users/dev/owner-repo");

    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} />);
    await user.click(screen.getByRole("button", { name: /browse/i }));

    await waitFor(() => expect(mockAttachClone).toHaveBeenCalled());

    // Soft-refresh never populates localPath — fast-forward 5s past the timeout.
    await vi.advanceTimersByTimeAsync(5100);

    await waitFor(() => expect(mockDismissNoClone).toHaveBeenCalledTimes(1));
    expect(mockConsumeRetry).not.toHaveBeenCalled();
  });

  it("does NOT dismiss if a fresh no-clone prompt for a different repo surfaced during the wait", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/Users/dev/owner-repo");

    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} />);
    await user.click(screen.getByRole("button", { name: /browse/i }));

    await waitFor(() => expect(mockAttachClone).toHaveBeenCalled());

    // Simulate the user triggering an unrelated no-clone action.
    // T.4 AC-3: prompt identity is `{teamKey, sourceRepoSlug}`.
    mockNoClonePromptRepo = { teamKey: "acme:default", sourceRepoSlug: "other/repo" };

    await vi.advanceTimersByTimeAsync(5100);

    // R2-P3: don't clobber the unrelated prompt.
    expect(mockDismissNoClone).not.toHaveBeenCalled();
  });
});

describe("AttachLocalCloneDialog — the core's refusals read in plain words", () => {
  it("a folder with no origin remote", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/Users/dev/not-a-repo");
    mockAttachClone.mockRejectedValue(refusal("noOrigin", { path: "/Users/dev/not-a-repo" }));

    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} />);
    await user.click(screen.getByRole("button", { name: /browse/i }));

    // AC-11: plain-language copy, path named.
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "No git remote is set on /Users/dev/not-a-repo",
    );
    expect(mockBumpAttachmentsVersion).not.toHaveBeenCalled();
  });

  it("a folder inside a clone names the clone's root to pick", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/Users/dev/owner-repo/src");
    mockAttachClone.mockRejectedValue(
      refusal("notRepoRoot", { path: "/Users/dev/owner-repo/src", root: "/Users/dev/owner-repo" }),
    );

    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} />);
    await user.click(screen.getByRole("button", { name: /browse/i }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "/Users/dev/owner-repo/src is inside the git repository at /Users/dev/owner-repo. Pick /Users/dev/owner-repo itself.",
    );
  });

  it("an invalid remote reports its parsing reason", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/Users/dev/repo");
    mockAttachClone.mockRejectedValue(
      refusal("unsupportedRemote", { url: "https://bad_host/a/b", reason: "invalid hostname" }),
    );

    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} />);
    await user.click(screen.getByRole("button", { name: /browse/i }));

    // Keep the actionable parsing failure, without implying a provider login requirement.
    expect(await screen.findByRole("alert")).toHaveTextContent(
      '"https://bad_host/a/b" cannot identify this repository: invalid hostname',
    );
  });

  // SS.5 AC-5 / AC-11: a non-git folder is refused with a specific error, in the
  // designed alert callout, and the picker stays reachable.
  it("a folder that is not a git working tree", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/Users/dev/not-a-git-dir");
    mockAttachClone.mockRejectedValue(
      refusal("notAGitRepository", { path: "/Users/dev/not-a-git-dir" }),
    );

    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} />);
    await user.click(screen.getByRole("button", { name: /browse/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(
      "That folder isn't a git repository — /Users/dev/not-a-git-dir has no .git directory.",
    );
    expect(mockBumpAttachmentsVersion).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /browse/i })).toBeInTheDocument();
  });
});

describe("AttachLocalCloneDialog — SW-LN-29 success state (where context lands)", () => {
  it("AC-1: an attached clone says context lands in its worktrees and stays open", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/Users/dev/owner-repo");
    const onOpenChange = vi.fn();
    render(
      <AttachLocalCloneDialog
        {...DEFAULT_PROPS}
        targetRepoPath={undefined}
        onOpenChange={onOpenChange}
      />,
    );
    await user.click(screen.getByRole("button", { name: /browse/i }));

    const success = await screen.findByTestId("attach-success");
    expect(success).toHaveTextContent(
      "Agents you start in worktrees CodeSense opens for this repo get the context their definition declares. Your main checkout isn't touched.",
    );
    expect(onOpenChange).not.toHaveBeenCalledWith(false);
    expect(screen.queryByRole("button", { name: /run an agent on/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /open a worktree/i })).toBeNull();
  });

  it("Done closes the dialog", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/x/sources/owner/repo");
    const onOpenChange = vi.fn();
    render(
      <AttachLocalCloneDialog
        {...DEFAULT_PROPS}
        targetRepoPath={undefined}
        onOpenChange={onOpenChange}
      />,
    );
    await user.click(screen.getByRole("button", { name: /browse/i }));
    await screen.findByTestId("attach-success");
    await user.click(screen.getByRole("button", { name: "Done" }));
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });
});

describe("AttachLocalCloneDialog — SW-LN-29 success vs. a retried no-clone action", () => {
  it("closes (no success state) when a pending no-clone action was retried", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/x/sources/owner/repo");
    mockWorkspaceRepos = {
      "workspace:acme:default:owner/repo": { repoPath: "x", localPath: "/x/sources/owner/repo" },
    };
    const onOpenChange = vi.fn();
    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} onOpenChange={onOpenChange} />);
    await user.click(screen.getByRole("button", { name: /browse/i }));
    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false));
    expect(mockConsumeRetry).toHaveBeenCalledTimes(1);
    expect(screen.queryByTestId("attach-success")).toBeNull();
  });

  it("a targeted attach with no pending retry (e.g. the repo pill) still shows the success state", async () => {
    mockNoClonePromptRepo = null;
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    mockOpenDialog.mockResolvedValue("/x/sources/owner/repo");
    mockWorkspaceRepos = {
      "workspace:acme:default:owner/repo": { repoPath: "x", localPath: "/x/sources/owner/repo" },
    };
    const onOpenChange = vi.fn();
    render(<AttachLocalCloneDialog {...DEFAULT_PROPS} onOpenChange={onOpenChange} />);
    await user.click(screen.getByRole("button", { name: /browse/i }));
    expect(await screen.findByTestId("attach-success")).toBeInTheDocument();
    expect(onOpenChange).not.toHaveBeenCalledWith(false);
  });
});
