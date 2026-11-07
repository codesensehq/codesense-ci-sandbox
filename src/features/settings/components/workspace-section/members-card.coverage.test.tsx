// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act, waitFor } from "@/test-utils";
import type { CurrentUser, Org, Person } from "@/shared/types/domain";

// ---------- Mocks ----------

const mockUpdatePersonInStore = vi.fn();
const mockAddPersonToStore = vi.fn();
const storeState = {
  orgs: [] as Org[],
  activeOrgSlug: null as string | null,
  stateLoaded: true,
  updatePersonInStore: mockUpdatePersonInStore,
  addPersonToStore: mockAddPersonToStore,
};
vi.mock("@/shared/stores/org-store", () => ({
  useOrgStore: <T,>(selector: (s: typeof storeState) => T) => selector(storeState),
}));

let currentUserValue: CurrentUser | null = null;
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => currentUserValue,
  useCurrentUser: () => currentUserValue,
  buildCurrentUser: () => currentUserValue,
}));

function makeCurrentUser(github_user_id: number): CurrentUser {
  return {
    github_user_id,
    github_username: "test",
    display_name: "Test",
    avatar_url: null,
  };
}

// SW-OB-8: members-card commits through the single local-commit seam.
const mockCommitRegistryEdit = vi.fn();
vi.mock("@/shared/lib/commit-registry-edit", () => ({
  commitRegistryEdit: (...args: unknown[]) => mockCommitRegistryEdit(...args),
}));

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), info: vi.fn(), success: vi.fn() },
}));

const mockOpenUrl = vi.fn().mockResolvedValue(undefined);
vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: (...args: unknown[]) => mockOpenUrl(...args),
}));

const { toast } = await import("sonner");
const { MembersCard } = await import("./members-card");

// ---------- Fixtures ----------

function person(id: number, username: string, display_name: string): Person {
  return {
    github_user_id: id,
    github_username: username,
    display_name,
    avatar_url: null,
  };
}

function makeOrg(overrides: Partial<Org> = {}, teams: Array<{ name: string }> = []): Org {
  return {
    slug: "acme",
    display_name: "Acme",
    created_at: "2026-01-01T00:00:00Z",
    remote_url: "https://github.com/acme/context.git",
    config_flags: {},
    people: [],
    teams: teams.map((t) => ({ ...t, source_repos: [] })),
    org_root: "",
    ...overrides,
  };
}

beforeEach(() => {
  currentUserValue = makeCurrentUser(42);
  mockCommitRegistryEdit.mockReset();
  mockUpdatePersonInStore.mockReset();
  mockAddPersonToStore.mockReset();
  vi.mocked(toast.error).mockClear();
  vi.mocked(toast.info).mockClear();
  vi.mocked(toast.success).mockClear();
});

// ---------- Card layout (AC1) ----------

describe("MembersCard — card layout", () => {
  it("renders inside a SettingsCard with data-testid", () => {
    render(<MembersCard org={makeOrg({ people: [] })} query="" />);
    expect(screen.getByTestId("settings-card-members")).toBeInTheDocument();
  });

  it("card header shows 'Members' title with count", () => {
    const org = makeOrg({ people: [person(1, "alice", "Alice"), person(2, "bob", "Bob")] });
    render(<MembersCard org={org} query="" />);
    const card = screen.getByTestId("settings-card-members");
    expect(card).toHaveTextContent("Members");
    expect(card).toHaveTextContent("(2)");
  });
});

// ---------- Member items with avatar initials (AC2) ----------

describe("MembersCard — member items", () => {
  it("renders every person as a member item with initial circle", () => {
    const org = makeOrg({
      people: [person(1, "alice", "Alice"), person(2, "bob", "Bob")],
    });
    render(<MembersCard org={org} query="" />);
    expect(screen.getByTestId("settings-member-pill-alice")).toBeInTheDocument();
    expect(screen.getByTestId("settings-member-pill-bob")).toBeInTheDocument();
    // Avatar initial rendered
    const aliceItem = screen.getByTestId("settings-member-pill-alice");
    expect(aliceItem).toHaveTextContent("A");
    expect(aliceItem).toHaveTextContent("@alice");
  });

  it("SW-OB-7: no lead ★ badge is rendered (lead/member distinction removed)", () => {
    const org = makeOrg({ people: [person(1, "alice", "Alice"), person(2, "bob", "Bob")] }, [
      { name: "backend" },
    ]);
    render(<MembersCard org={org} query="" />);
    expect(screen.getByTestId("settings-member-pill-alice")).not.toHaveTextContent("★");
    expect(screen.getByTestId("settings-member-pill-bob")).not.toHaveTextContent("★");
  });

  it("shows an empty message when people is empty", () => {
    render(<MembersCard org={makeOrg({ people: [] })} query="" />);
    expect(screen.getByTestId("settings-members-empty")).toBeInTheDocument();
  });

  it("member pill names itself via the visible @username", () => {
    const org = makeOrg({ people: [person(1, "alice", "Alice Aliceson")] });
    render(<MembersCard org={org} query="" />);
    const itemButton = screen.getByTestId("settings-member-pill-alice").querySelector("button");
    expect(itemButton).toHaveTextContent("@alice");
    expect(itemButton).not.toHaveAttribute("title");
  });
});

// ---------- Add flow (SW-ID-2) ----------

// ---------- Handle-less teammate crash-floor (SW-FG-4b AC-7) ----------

describe("MembersCard — handle-less teammate (SW-FG-4b AC-7)", () => {
  const handleLess: Person = {
    github_user_id: 500,
    github_username: null,
    display_name: "Grace",
    avatar_url: null,
  };

  it("renders a handle-less member by display name — no broken @ / github.com/null", () => {
    // Crash floor + the SW-FG-4b guard: a handle-less teammate renders by display
    // name (not an empty "@" or a github.com/null link) and keys its pill on the
    // id. The polished 'No code host linked' affordance is FG-4c.
    const org = makeOrg({ people: [person(1, "alice", "Alice"), handleLess] });
    expect(() => render(<MembersCard org={org} query="" />)).not.toThrow();
    const card = screen.getByTestId("settings-card-members");
    expect(card).toHaveTextContent("(2)");
    expect(card).toHaveTextContent("@alice");
    // The handle-less teammate shows their display name and keys its pill on id.
    expect(card).toHaveTextContent("Grace");
    expect(screen.getByTestId("settings-member-pill-500")).toBeInTheDocument();
  });

  it("self-linking records the identity locally like any teammate (no service sign-in)", async () => {
    currentUserValue = makeCurrentUser(500);
    const self = { ...handleLess, id: "person-500", identities: [] };
    const linked = {
      ...self,
      identities: [{ provider: "github", handle: "grace", provider_user_id: null }],
    };
    mockCommitRegistryEdit.mockResolvedValueOnce({ person: linked });
    render(<MembersCard org={makeOrg({ people: [self] })} query="" />);
    fireEvent.click(screen.getByRole("button", { name: "Link a code host for Grace" }));
    fireEvent.change(screen.getByLabelText("Handle"), { target: { value: "grace" } });
    fireEvent.click(screen.getByRole("button", { name: "Link" }));
    await waitFor(() =>
      expect(mockCommitRegistryEdit).toHaveBeenCalledWith(500, "acme", {
        type: "AddIdentityToPerson",
        person_id: "person-500",
        provider: "github",
        handle: "grace",
        provider_user_id: null,
      }),
    );
    expect(mockUpdatePersonInStore).toHaveBeenCalledWith("acme", linked);
  });

  it("keeps an add-another-provider affordance after an identity is linked", () => {
    const linked: Person = {
      ...handleLess,
      id: "person-500",
      identities: [{ provider: "gitlab", handle: "grace", provider_user_id: null }],
    };
    render(<MembersCard org={makeOrg({ people: [linked] })} query="" />);
    fireEvent.contextMenu(screen.getByRole("button", { name: /Grace/ }));
    expect(screen.getByRole("menuitem", { name: "Add another provider" })).toBeInTheDocument();
  });

  it("links another teammate by stable id and replaces that person in the store", async () => {
    const stable = { ...handleLess, id: "person-500", identities: [] };
    const linked = {
      ...stable,
      identities: [{ provider: "gitlab", handle: "grace", provider_user_id: null }],
    };
    mockCommitRegistryEdit.mockResolvedValueOnce({ person: linked });
    render(<MembersCard org={makeOrg({ people: [stable] })} query="" />);
    fireEvent.click(screen.getByRole("button", { name: "Link a code host for Grace" }));
    fireEvent.change(screen.getByLabelText("Handle"), { target: { value: "grace" } });
    fireEvent.click(screen.getByRole("button", { name: "Link" }));
    await waitFor(() =>
      expect(mockCommitRegistryEdit).toHaveBeenCalledWith(42, "acme", {
        type: "AddIdentityToPerson",
        person_id: "person-500",
        provider: "github",
        handle: "grace",
        provider_user_id: null,
      }),
    );
    expect(mockUpdatePersonInStore).toHaveBeenCalledWith("acme", linked);
  });
});

describe("MembersCard — add person", () => {
  function openAddDialog() {
    fireEvent.click(screen.getByTestId("settings-members-add-button"));
  }

  it("renders the Add button (open access — always available)", () => {
    render(<MembersCard org={makeOrg({ people: [] })} query="" />);
    expect(screen.getByTestId("settings-members-add-button")).toBeInTheDocument();
  });

  it("AC-5: empty-state copy invites adding the first teammate", () => {
    render(<MembersCard org={makeOrg({ people: [] })} query="" />);
    expect(screen.getByTestId("settings-members-empty")).toHaveTextContent(
      "Add your first teammate",
    );
  });

  it("an invalid name disables submit and shows a hint (no commit)", () => {
    render(<MembersCard org={makeOrg({ people: [] })} query="" />);
    openAddDialog();
    fireEvent.change(screen.getByTestId("settings-members-add-input"), {
      target: { value: "a".repeat(101) },
    });
    expect(screen.getByTestId("settings-members-add-invalid")).toHaveTextContent(
      "at most 100 characters",
    );
    expect(screen.getByTestId("settings-members-add-confirm")).toBeDisabled();
    expect(mockCommitRegistryEdit).not.toHaveBeenCalled();
  });

  it("an untouched field is the resting state — no hint, submit disabled", () => {
    render(<MembersCard org={makeOrg({ people: [] })} query="" />);
    openAddDialog();
    expect(screen.queryByTestId("settings-members-add-invalid")).not.toBeInTheDocument();
    expect(screen.getByTestId("settings-members-add-confirm")).toBeDisabled();
  });

  it("a local commit failure surfaces an error toast and keeps the dialog open", async () => {
    mockCommitRegistryEdit.mockRejectedValueOnce(new Error("io: people.yml is read-only"));
    render(<MembersCard org={makeOrg({ people: [] })} query="" />);
    openAddDialog();
    fireEvent.change(screen.getByTestId("settings-members-add-input"), {
      target: { value: "Ghost" },
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-members-add-confirm"));
    });
    expect(toast.error).toHaveBeenCalledWith(expect.stringContaining("Failed to add Ghost"));
    expect(screen.getByTestId("settings-members-add-error")).toHaveTextContent("read-only");
    expect(screen.getByTestId("settings-members-add-input")).toHaveValue("Ghost");
    expect(mockAddPersonToStore).not.toHaveBeenCalled();
  });

  it("adds by name and inserts the returned stable person without closing on failure", async () => {
    const added: Person = {
      id: "person-1",
      github_user_id: 0,
      github_username: null,
      display_name: "Grace Hopper",
      avatar_url: null,
      identities: [],
    };
    mockCommitRegistryEdit.mockResolvedValueOnce({
      applied: true,
      message: "added",
      person: added,
    });
    render(<MembersCard org={makeOrg({ people: [] })} query="" />);
    openAddDialog();
    fireEvent.change(screen.getByTestId("settings-members-add-input"), {
      target: { value: "Grace Hopper" },
    });
    await act(async () => fireEvent.click(screen.getByTestId("settings-members-add-confirm")));
    expect(mockCommitRegistryEdit).toHaveBeenCalledWith(42, "acme", {
      type: "AddPersonByName",
      display_name: "Grace Hopper",
    });
    expect(mockAddPersonToStore).toHaveBeenCalledWith("acme", added);
  });
});

// ---------- Context menu (right-click remove — capability now open, SW-OB-7) ----------

describe("MembersCard — right-click menu", () => {
  function openMenuFor(username: string) {
    const item = screen.getByTestId(`settings-member-pill-${username}`).querySelector("button")!;
    fireEvent.contextMenu(item);
  }

  it("admin sees 'Remove from workspace' for non-self members", () => {
    render(<MembersCard org={makeOrg({ people: [person(1, "alice", "Alice")] })} query="" />);
    openMenuFor("alice");
    expect(screen.getByTestId("settings-member-remove-alice")).toBeInTheDocument();
  });

  it("admin does NOT see 'Remove' for themselves", () => {
    currentUserValue = makeCurrentUser(1);
    render(<MembersCard org={makeOrg({ people: [person(1, "alice", "Alice")] })} query="" />);
    openMenuFor("alice");
    expect(screen.queryByTestId("settings-member-remove-alice")).not.toBeInTheDocument();
  });

  it("SW-OB-7: any permission tier now sees Remove for non-self members (capability open)", () => {
    // Previously 'write' could not remove members (admin-gated). Under open
    // access the gate always allows, so Remove is available regardless of tier.
    render(<MembersCard org={makeOrg({ people: [person(1, "alice", "Alice")] })} query="" />);
    openMenuFor("alice");
    expect(screen.getByRole("menu")).toBeInTheDocument();
    expect(screen.getByTestId("settings-member-remove-alice")).toBeInTheDocument();
  });
});

// ---------- Remove flow ----------

describe("MembersCard — remove flow", () => {
  it("confirm commits RemovePersonById LOCALLY using the stable person id", async () => {
    mockCommitRegistryEdit.mockResolvedValueOnce(undefined);
    render(<MembersCard org={makeOrg({ people: [person(99, "alice", "Alice")] })} query="" />);
    const item = screen.getByTestId("settings-member-pill-alice").querySelector("button")!;
    fireEvent.contextMenu(item);
    fireEvent.click(screen.getByTestId("settings-member-remove-alice"));
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-member-remove-confirm"));
    });
    // Local seam: (userId, orgSlug, edit) — no remoteUrl / defaultBranch.
    expect(mockCommitRegistryEdit).toHaveBeenCalledWith(42, "acme", {
      type: "RemovePersonById",
      person_id: "99",
    });
    expect(toast.success).toHaveBeenCalledWith("Removed Alice");
  });
});

// ---------- Search filter ----------

describe("MembersCard — search filter", () => {
  it("hides entire card when query doesn't match", () => {
    render(<MembersCard org={makeOrg({ people: [person(1, "alice", "Alice")] })} query="oauth" />);
    expect(screen.queryByTestId("settings-card-members")).not.toBeInTheDocument();
  });

  it("shows card when 'members' keyword matches", () => {
    render(
      <MembersCard org={makeOrg({ people: [person(1, "alice", "Alice")] })} query="members" />,
    );
    expect(screen.getByTestId("settings-card-members")).toBeInTheDocument();
  });
});

// ---------- NB-6: virtualized path (above 30-member threshold) ----------

describe("MembersCard — virtualized list above threshold", () => {
  it("renders every member pill when count > 30 (virtualized branch)", () => {
    const people = Array.from({ length: 31 }, (_, i) =>
      person(i + 1, `user${i + 1}`, `User ${i + 1}`),
    );
    const { container } = render(<MembersCard org={makeOrg({ people })} query="" />);

    // Container is still queryable by testid in both branches.
    const list = screen.getByTestId("settings-members-list");
    expect(list).toBeInTheDocument();

    // All 31 MemberItems should materialize under the test-env offsetHeight shim,
    // which sizes overflow-y-auto containers to 2000px so the virtualizer's
    // overscan window covers small fixtures comfortably.
    const pills = container.querySelectorAll('[data-testid^="settings-member-pill-"]');
    expect(pills.length).toBe(31);
    expect(screen.getByTestId("settings-member-pill-user1")).toBeInTheDocument();
    expect(screen.getByTestId("settings-member-pill-user31")).toBeInTheDocument();
  });
});
