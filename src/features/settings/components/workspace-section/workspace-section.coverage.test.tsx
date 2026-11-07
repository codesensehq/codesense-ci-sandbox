// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act, fireEvent } from "@testing-library/react";
import type { CurrentUser, Org } from "@/shared/types/domain";

// ---------- Mocks ----------

// Mock org-store: hand-roll a minimal selector-compatible store so we
// don't need to rehydrate the real one across test cases.
const storeState = {
  orgs: [] as Org[],
  activeOrgSlug: null as string | null,
  stateLoaded: true,
};
vi.mock("@/shared/stores/org-store", () => ({
  useOrgStore: <T,>(selector: (s: typeof storeState) => T) => selector(storeState),
}));

// F-25: user identity is read from `useCurrentUser` (the active local profile).
let currentUserValue: CurrentUser | null = {
  github_user_id: 42,
  github_username: "test",
  display_name: "Test",
  avatar_url: null,
};
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => currentUserValue,
  useCurrentUser: () => currentUserValue,
  buildCurrentUser: () => currentUserValue,
}));

// SW-OB-8: workspace registry edits commit through the single
// local-commit seam. Spy on it for the rename happy-path test.
const mockCommitRegistryEdit = vi.fn();
vi.mock("@/shared/lib/commit-registry-edit", () => ({
  commitRegistryEdit: (...args: unknown[]) => mockCommitRegistryEdit(...args),
}));

// `@/tauri/commands` is still imported by sibling components in the
// section tree (e.g. the attach-repo dialog + source-repo pill); stub
// only the surface they touch so nothing hits the real Tauri bridge.
vi.mock("@/tauri/commands", () => ({
  parseGitRemote: vi.fn(),
  checkPathExists: vi.fn().mockResolvedValue(true),
  deleteSourceRepoAttachment: vi.fn(),
}));

// Sonner — spy on the three levels we fire.
vi.mock("sonner", () => ({
  toast: { error: vi.fn(), info: vi.fn(), success: vi.fn() },
}));

// SS-9: spy on the logger so AC-19's TODO(SS-10) warn is assertable when
// "Manage installed content →" is clicked.
const mockLoggerWarn = vi.fn();
vi.mock("@/shared/lib/logger", () => ({
  logger: { warn: mockLoggerWarn, info: vi.fn(), error: vi.fn(), debug: vi.fn() },
}));

// openUrl — spy; the remote-link row calls it on click.
const mockOpenUrl = vi.fn().mockResolvedValue(undefined);
vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: (...args: unknown[]) => mockOpenUrl(...args),
}));

const { toast } = await import("sonner");
const { WorkspaceSection } = await import("./workspace-section");

// ---------- Fixtures ----------

function makeOrg(overrides: Partial<Org> = {}): Org {
  return {
    slug: "acme",
    display_name: "Acme Workspace",
    created_at: "2026-01-01T00:00:00Z",
    remote_url: "https://github.com/acme/context.git",
    config_flags: {},
    people: [],
    teams: [],
    org_root: "",
    ...overrides,
  };
}

function setStore(partial: Partial<typeof storeState>) {
  Object.assign(storeState, partial);
}

beforeEach(() => {
  storeState.orgs = [];
  storeState.activeOrgSlug = null;
  storeState.stateLoaded = true;
  currentUserValue = {
    github_user_id: 42,
    github_username: "test",
    display_name: "Test",
    avatar_url: null,
  };
  vi.mocked(toast.info).mockClear();
  vi.mocked(toast.error).mockClear();
  vi.mocked(toast.success).mockClear();
  mockOpenUrl.mockClear();
  mockCommitRegistryEdit.mockReset();
});

afterEach(() => {
  vi.useRealTimers();
});

// ---------- AC1 — 3-card layout ----------

describe("WorkspaceSection — 3-card layout (AC1)", () => {
  it("renders the Workspace section header with id=settings-section-workspace", () => {
    const { container } = render(<WorkspaceSection query="" />);
    const header = container.querySelector("#settings-section-workspace");
    expect(header).toBeInTheDocument();
    expect(header?.textContent).toBe("Workspace");
  });

  it("renders Config, Members, and Teams cards when org is active", () => {
    setStore({ stateLoaded: true, activeOrgSlug: "acme", orgs: [makeOrg()] });
    render(<WorkspaceSection query="" />);
    expect(screen.getByTestId("settings-card-config")).toBeInTheDocument();
    expect(screen.getByTestId("settings-card-members")).toBeInTheDocument();
    expect(screen.getByTestId("settings-card-teams")).toBeInTheDocument();
  });

  it("does not render the old placeholder", () => {
    render(<WorkspaceSection query="" />);
    expect(screen.queryByTestId("settings-workspace-placeholder")).not.toBeInTheDocument();
  });

  // SLW-NC-3: the "Manage installed content →" button + its install-workbench
  // modal were retired once the nav absorbed installed content (NC-1/NC-2), so
  // the button no longer renders and its testid is gone.
  it("does not render the retired 'Manage installed content →' button", () => {
    setStore({ stateLoaded: true, activeOrgSlug: "acme", orgs: [makeOrg()] });
    render(<WorkspaceSection query="" />);
    expect(screen.queryByTestId("settings-manage-installed-content")).not.toBeInTheDocument();
  });
});

// ---------- AC-P2-3 / AC-P2-3b ----------

describe("WorkspaceSection — empty / skeleton states", () => {
  it("shows 'no active workspace' when the store is loaded but slug is null", () => {
    setStore({ stateLoaded: true, activeOrgSlug: null });
    render(<WorkspaceSection query="" />);
    expect(screen.getByTestId("settings-workspace-no-active")).toBeInTheDocument();
  });

  it("shows a skeleton when slug is set but orgs haven't hydrated yet", () => {
    setStore({ stateLoaded: false, activeOrgSlug: "acme", orgs: [] });
    render(<WorkspaceSection query="" />);
    expect(screen.getByTestId("settings-workspace-skeleton")).toBeInTheDocument();
    expect(screen.queryByTestId("settings-workspace-no-active")).not.toBeInTheDocument();
  });

  it("shows a skeleton when slug is set post-rehydrate but orgs hasn't caught up", () => {
    setStore({ stateLoaded: true, activeOrgSlug: "acme", orgs: [] });
    render(<WorkspaceSection query="" />);
    expect(screen.getByTestId("settings-workspace-skeleton")).toBeInTheDocument();
  });
});

// ---------- AC-P2-4 / AC-P2-5 / AC-P2-8 / AC-P2-10 / AC-P2-11 ----------

describe("WorkspaceSection — config rows", () => {
  function mountWithOrg(org: Org, query = "") {
    setStore({ stateLoaded: true, activeOrgSlug: org.slug, orgs: [org] });
    return render(<WorkspaceSection query={query} />);
  }

  it("renders display name from org.display_name (admin permission → editable)", () => {
    mountWithOrg(makeOrg({ display_name: "Acme Workspace" }));
    const input = screen.getByTestId("settings-workspace-name-input") as HTMLInputElement;
    expect(input.value).toBe("Acme Workspace");
  });

  it("SW-OB-7: name is editable even at the former read-only tier (capability open)", () => {
    mountWithOrg(makeOrg());
    expect(screen.getByTestId("settings-workspace-name-input")).toBeInTheDocument();
    expect(screen.queryByTestId("settings-workspace-name-readonly")).not.toBeInTheDocument();
  });

  it("renders display name as editable for null permission (local-only)", () => {
    mountWithOrg(makeOrg({ remote_url: null }));
    expect(screen.getByTestId("settings-workspace-name-input")).toBeInTheDocument();
  });

  it("SW-OB-7: name is editable for 'write' tier too (no admin gate)", () => {
    mountWithOrg(makeOrg());
    expect(screen.getByTestId("settings-workspace-name-input")).toBeInTheDocument();
    expect(screen.queryByTestId("settings-workspace-name-readonly")).not.toBeInTheDocument();
  });

  it("remote row shows github.com/owner/repo link when remote_url is set", () => {
    mountWithOrg(makeOrg({ remote_url: "https://github.com/acme/context.git" }));
    const link = screen.getByTestId("settings-workspace-remote-link");
    expect(link).toHaveTextContent("github.com/acme/context");
  });

  it("remote row clicking opens the raw remote_url (preserves .git)", () => {
    mountWithOrg(makeOrg({ remote_url: "https://github.com/acme/context.git" }));
    fireEvent.click(screen.getByTestId("settings-workspace-remote-link"));
    expect(mockOpenUrl).toHaveBeenCalledWith("https://github.com/acme/context.git");
  });

  it("remote row shows an SSH origin as plain text with the full URL as its tooltip", () => {
    mountWithOrg(makeOrg({ remote_url: "git@github.com:acme/context.git" }));
    const text = screen.getByTestId("settings-workspace-remote-text");
    expect(text).toHaveTextContent("github.com/acme/context");
    expect(text).toHaveAttribute("title", "git@github.com:acme/context.git");
    expect(screen.queryByTestId("settings-workspace-remote-link")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /github\.com/i })).not.toBeInTheDocument();
  });

  it("remote row shows an ssh:// origin as plain text", () => {
    mountWithOrg(makeOrg({ remote_url: "ssh://github.com:22/acme/context.git" }));
    const text = screen.getByTestId("settings-workspace-remote-text");
    expect(text).toHaveTextContent("github.com:22/acme/context");
    expect(text).toHaveAttribute("title", "ssh://github.com:22/acme/context.git");
    expect(screen.queryByTestId("settings-workspace-remote-link")).not.toBeInTheDocument();
  });

  it("remote row reads 'Local repository' with no sync action when remote_url is null", () => {
    mountWithOrg(makeOrg({ remote_url: null }));
    expect(screen.getByText("Local repository")).toBeInTheDocument();
    expect(screen.queryByTestId("settings-workspace-remote-link")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /remote sync/i })).not.toBeInTheDocument();
  });

  it("does not expose legacy OAuth configuration", () => {
    mountWithOrg(makeOrg());
    expect(screen.queryByTestId("settings-workspace-oauth")).not.toBeInTheDocument();
    expect(screen.queryByText("Using default app")).not.toBeInTheDocument();
  });

  it("search filter 'name' shows only the Name row", () => {
    mountWithOrg(makeOrg(), "name");
    expect(screen.getByTestId("settings-workspace-name-input")).toBeInTheDocument();
    expect(screen.queryByTestId("settings-workspace-remote-link")).not.toBeInTheDocument();
    expect(screen.queryByTestId("settings-workspace-oauth")).not.toBeInTheDocument();
  });

  it("search filter 'oauth' does not expose retired configuration", () => {
    mountWithOrg(makeOrg(), "oauth");
    expect(screen.queryByTestId("settings-workspace-name-input")).not.toBeInTheDocument();
    expect(screen.queryByTestId("settings-workspace-remote-link")).not.toBeInTheDocument();
    expect(screen.queryByTestId("settings-workspace-oauth")).not.toBeInTheDocument();
  });

  it("empty query renders the name and remote config rows", () => {
    mountWithOrg(makeOrg(), "");
    expect(screen.getByTestId("settings-workspace-name-input")).toBeInTheDocument();
    expect(screen.getByTestId("settings-workspace-remote-link")).toBeInTheDocument();
    expect(screen.queryByTestId("settings-workspace-oauth")).not.toBeInTheDocument();
  });
});

// ---------- AC-P2-13 / AC-P2-14 ----------

describe("WorkspaceSection — rename input validation + in-flight disable", () => {
  function mountAdmin(org: Org = makeOrg()) {
    setStore({ stateLoaded: true, activeOrgSlug: org.slug, orgs: [org] });
    return render(<WorkspaceSection query="" />);
  }

  it("empty-input rename surfaces a toast and reverts to committed value", async () => {
    vi.useFakeTimers();
    mountAdmin(makeOrg({ display_name: "Acme" }));
    const input = screen.getByTestId("settings-workspace-name-input") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.blur(input);
    await act(async () => {
      await vi.runAllTimersAsync();
    });
    expect(toast.error).toHaveBeenCalledWith(expect.stringContaining("Workspace name required"));
    expect(input.value).toBe("Acme");
  });

  it("no-op rename (trim-equal to committed) does not toast", async () => {
    vi.useFakeTimers();
    mountAdmin(makeOrg({ display_name: "Acme" }));
    const input = screen.getByTestId("settings-workspace-name-input") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "  Acme  " } });
    fireEvent.blur(input);
    await act(async () => {
      await vi.runAllTimersAsync();
    });
    // Neither error nor the Part-3 stub-info surface.
    expect(toast.error).not.toHaveBeenCalled();
    expect(toast.info).not.toHaveBeenCalledWith(expect.stringContaining("Rename is wired"));
  });

  it("valid rename disables the input during the in-flight commit", async () => {
    mountAdmin(makeOrg({ display_name: "Old Name" }));
    // Hold the Tauri invoke in a pending state until we resolve it.
    let resolvePr: (() => void) | undefined;
    mockCommitRegistryEdit.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          resolvePr = resolve;
        }),
    );
    const input = screen.getByTestId("settings-workspace-name-input") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "New Name" } });
    await act(async () => {
      fireEvent.blur(input);
    });
    // While commit is pending, the input is disabled.
    expect(input.disabled).toBe(true);
    // Resolve the commit; input re-enables.
    await act(async () => {
      resolvePr!();
      await Promise.resolve();
    });
    expect(input.disabled).toBe(false);
    // SW-OB-8: routed through the local seam — (userId, orgSlug, edit),
    // no remoteUrl / defaultBranch, no PR.
    expect(mockCommitRegistryEdit).toHaveBeenCalledWith(42, "acme", {
      type: "RenameWorkspace",
      new_display_name: "New Name",
    });
  });
});

// ---------- SW-XS-11 AC-1 — no sync controls, one explanatory line ----------

describe("WorkspaceSection — how agents get context (SW-XS-11)", () => {
  it("states the §8.4 line once, as plain muted text with no control", () => {
    setStore({ stateLoaded: true, activeOrgSlug: "acme", orgs: [makeOrg()] });
    render(<WorkspaceSection query="" />);
    const note = screen.getByTestId("settings-workspace-context-note");
    expect(note.tagName).toBe("P");
    expect(note).toHaveClass("text-muted-foreground");
    expect(note.textContent).toBe(
      "Agents get the context their definition declares when they start, and pull or publish more through CodeSense tools. Nothing syncs in the background.",
    );
    expect(note.querySelector("input, button, select, [role='switch'], [role='radio']")).toBeNull();
  });

  it.each(["sync", "cadence", "hydration", "reverse sync"])(
    "a search for %j finds the line, and no cadence or hydration control",
    (query) => {
      setStore({ stateLoaded: true, activeOrgSlug: "acme", orgs: [makeOrg()] });
      render(<WorkspaceSection query={query} />);
      expect(screen.getByTestId("settings-workspace-context-note")).toBeInTheDocument();
      for (const id of [
        "settings-sync-cadence",
        "settings-hydration-mode",
        "settings-reverse-sync-default-on-demand",
        "settings-forward-sync-default-on-demand",
      ]) {
        expect(screen.queryByTestId(id)).toBeNull();
      }
      expect(screen.queryByRole("radiogroup", { name: /cadence|hydration/i })).toBeNull();
    },
  );

  it("an unrelated search hides the line", () => {
    setStore({ stateLoaded: true, activeOrgSlug: "acme", orgs: [makeOrg()] });
    render(<WorkspaceSection query="members" />);
    expect(screen.queryByTestId("settings-workspace-context-note")).toBeNull();
  });
});
