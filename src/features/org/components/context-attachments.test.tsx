// SPDX-License-Identifier: MIT
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { useOrgStore } from "@/shared/stores/org-store";
import { useFirstRunStore } from "@/shared/stores/first-run-store";
import { ContextAttachments } from "./context-attachments";
import { useOrgDiscovery } from "../hooks/use-org-discovery";
import { ManageWorkspacesTab } from "../workspaces-virtual-tab";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { endLocalSetup } from "../lib/local-setup-completion";

let principal = "one";
vi.mock("@/shared/lib/get-current-user", () => ({
  useCurrentUser: () => ({ principal, github_user_id: 1 }),
  getCurrentUser: () => ({ principal, github_user_id: 1 }),
}));
const api = vi.hoisted(() => ({
  attach: vi.fn(),
  list: vi.fn(),
  detach: vi.fn(),
  create: vi.fn(),
  teams: vi.fn(),
  open: vi.fn(),
  discover: vi.fn(),
  load: vi.fn(),
  guest: vi.fn(),
  listeners: new Map<string, Set<() => void>>(),
}));
vi.mock("@/features/shell/manage-teams-virtual-tab", () => ({ openManageTeams: api.teams }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: api.open }));
vi.mock("@tauri-apps/api/path", () => ({ homeDir: async () => "/Users/me" }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, callback: () => void) => {
    const listeners = api.listeners.get(name) ?? new Set<() => void>();
    listeners.add(callback);
    api.listeners.set(name, listeners);
    return () => listeners.delete(callback);
  }),
}));
vi.mock("@/shared/api/org", async (original) => ({
  ...(await original<typeof import("@/shared/api/org")>()),
  discoverLocalOrgs: api.discover,
}));
vi.mock("@/shared/api/registry", async (original) => ({
  ...(await original<typeof import("@/shared/api/registry")>()),
  loadOrg: api.load,
}));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  createLocalOrganization: api.create,
  attachContextRepository: api.attach,
  listContextAttachments: api.list,
  detachContextRepository: api.detach,
  getGuestSession: api.guest,
  ensureCodesenseDocs: vi.fn(async () => {
    throw new Error("no bundled docs in this test");
  }),
  // SW-ODC-11: no bundled sample repo in this test.
  ensureSampleRepo: vi.fn(async () => {
    throw new Error("no bundled sample repo in this test");
  }),
}));
beforeEach(() => {
  vi.clearAllMocks();
  endLocalSetup();
  api.listeners.clear();
  api.discover.mockResolvedValue([]);
  api.guest.mockRejectedValue(new Error("Guest refresh unavailable"));
  principal = "one";
  api.list.mockResolvedValue([]);
  useOrgStore.setState({ orgs: [], activeOrgSlug: null, stateLoaded: true });
  useViewerStore.setState({ stateLoaded: true });
  useFirstRunStore.setState({ guest: false });
});
it("connects the chosen checkout after the inline confirm and selects its declared context repo", async () => {
  api.open.mockResolvedValue("/dirty/existing");
  api.attach.mockResolvedValue({
    loaded_org: {
      org: { slug: "declared", display_name: "Team", org_root: "/canonical/existing", teams: [] },
    },
  });
  render(<ContextAttachments />);
  fireEvent.click(screen.getByRole("button", { name: "Connect a context repo…" }));
  await screen.findByText("Connect existing?");
  expect(api.open).toHaveBeenCalledWith(
    expect.objectContaining({ title: "Choose your team's context repo folder", directory: true }),
  );
  expect(screen.getByTestId("context-repo-connect-confirm")).toHaveTextContent(
    "CodeSense will read and write files in /dirty/existing. It won't run git — you or your agents do.",
  );
  // Nothing is connected until the user confirms.
  expect(api.attach).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Connect" }));
  await waitFor(() => expect(useOrgStore.getState().activeOrgSlug).toBe("declared"));
  expect(api.attach).toHaveBeenCalledWith("/dirty/existing");
  expect(useOrgStore.getState().orgs[0]).toMatchObject({
    org_root: "/canonical/existing",
    is_attached: true,
  });
  expect(api.guest).not.toHaveBeenCalled();
});
it("shows an unavailable registration and detaches by identity without deleting files", async () => {
  api.list.mockResolvedValue([
    {
      slug: "gone",
      display_name: "Gone",
      org_root: "/missing",
      availability_error: "Checkout is unavailable",
    },
  ]);
  api.detach.mockResolvedValue(undefined);
  render(<ContextAttachments />);
  expect(await screen.findByRole("alert")).toHaveTextContent("Checkout is unavailable");
  fireEvent.click(screen.getByRole("button", { name: "Disconnect Gone" }));
  await waitFor(() => expect(api.detach).toHaveBeenCalledWith("gone"));
  expect(api.attach).not.toHaveBeenCalled();
});
it("a folder that isn't a context repo says so and offers another folder or a new repo", async () => {
  api.open.mockResolvedValue("/just/code");
  api.attach.mockRejectedValue(
    new Error("Cannot read selected context's org.yml: No such file or directory"),
  );
  render(<ContextAttachments />);
  fireEvent.click(screen.getByRole("button", { name: "Connect a context repo…" }));
  fireEvent.click(await screen.findByRole("button", { name: "Connect" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "This folder isn't a CodeSense context repo. Pick the folder you cloned, or create a new one.",
  );
  expect(useOrgStore.getState().activeOrgSlug).toBeNull();
  expect(screen.getByRole("button", { name: "Choose another folder" })).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Create a new context repo…" }));
  expect(screen.getByRole("form", { name: "Create a new context repo" })).toBeInTheDocument();
});

it("reports other connect errors verbatim without selecting a context repo", async () => {
  api.open.mockResolvedValue("/taken");
  api.attach.mockRejectedValue(
    new Error("This checkout is already attached under another principal"),
  );
  render(<ContextAttachments />);
  fireEvent.click(screen.getByRole("button", { name: "Connect a context repo…" }));
  fireEvent.click(await screen.findByRole("button", { name: "Connect" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "already attached under another principal",
  );
  expect(useOrgStore.getState().activeOrgSlug).toBeNull();
});

it("refresh clears stale errors and rechecks a restored checkout", async () => {
  api.list.mockRejectedValueOnce(new Error("Registration unreadable"));
  render(<ContextAttachments />);
  expect(await screen.findByRole("alert")).toHaveTextContent("Registration unreadable");
  api.list.mockResolvedValue([
    { slug: "restored", display_name: "Restored", org_root: "/restored", availability_error: null },
  ]);
  fireEvent.click(screen.getByRole("button", { name: "Refresh context repos" }));
  expect(await screen.findByText("Restored")).toBeInTheDocument();
  expect(screen.queryByRole("alert")).toBeNull();
});
it("hides the previous principal's rows immediately and discards their late refresh", async () => {
  api.list.mockResolvedValue([
    { slug: "private", display_name: "Old account context", org_root: "/private" },
  ]);
  const view = render(<ContextAttachments />);
  expect(await screen.findByText("Old account context")).toBeInTheDocument();
  let finishOld: (rows: unknown[]) => void = () => {};
  api.list.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finishOld = resolve;
      }),
  );
  fireEvent.click(screen.getByRole("button", { name: "Refresh context repos" }));
  api.list.mockResolvedValue([]);
  principal = "two";
  view.rerender(<ContextAttachments />);
  expect(screen.queryByText("Old account context")).toBeNull();
  await act(async () =>
    finishOld([{ slug: "private", display_name: "Late old context", org_root: "/private" }]),
  );
  expect(screen.queryByText("Late old context")).toBeNull();
});

it("creates at ~/CodeSense/<name> by default, selects it and opens team creation", async () => {
  api.create.mockResolvedValue({
    loaded_org: {
      org: {
        slug: "local/my-org",
        display_name: "My Org",
        org_root: "/Users/me/CodeSense/my-org",
        teams: [],
      },
    },
  });
  render(<ContextAttachments />);
  fireEvent.click(screen.getByRole("button", { name: "Create a new context repo…" }));
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: " My Org " } });
  await waitFor(() =>
    expect(screen.getByLabelText("Location")).toHaveValue("/Users/me/CodeSense/my-org"),
  );
  fireEvent.click(screen.getByRole("button", { name: "Create and connect" }));
  await waitFor(() =>
    expect(api.create).toHaveBeenCalledWith("My Org", "/Users/me/CodeSense/my-org", false),
  );
  await waitFor(() => expect(useOrgStore.getState().activeOrgSlug).toBe("local/my-org"));
  expect(api.teams).toHaveBeenCalledWith("create");
});

it("starts from the CodeSense example: it already has a team, so team creation stays closed", async () => {
  api.create.mockResolvedValue({
    loaded_org: {
      org: {
        slug: "local/acme",
        display_name: "Acme",
        org_root: "/Users/me/CodeSense/acme",
        teams: [{ name: "getting-started", source_repos: [] }],
      },
    },
  });
  render(<ContextAttachments initialCreate />);
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Acme" } });
  fireEvent.click(screen.getByLabelText(/Start from the CodeSense example/));
  await waitFor(() =>
    expect(screen.getByLabelText("Location")).toHaveValue("/Users/me/CodeSense/acme"),
  );
  fireEvent.click(screen.getByRole("button", { name: "Create and connect" }));
  await waitFor(() =>
    expect(api.create).toHaveBeenCalledWith("Acme", "/Users/me/CodeSense/acme", true),
  );
  await waitFor(() => expect(useOrgStore.getState().activeOrgSlug).toBe("local/acme"));
  expect(api.teams).not.toHaveBeenCalled();
});

it("a chosen location follows the folder picker, not the name", async () => {
  api.open.mockResolvedValue("/chosen/parent");
  api.create.mockResolvedValue({
    loaded_org: {
      org: {
        slug: "local/my-org",
        display_name: "My Org",
        org_root: "/chosen/parent/my-org",
        teams: [],
      },
    },
  });
  render(<ContextAttachments />);
  fireEvent.click(screen.getByRole("button", { name: "Create a new context repo…" }));
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: " My Org " } });
  fireEvent.click(screen.getByRole("button", { name: "Choose…" }));
  await waitFor(() =>
    expect(screen.getByLabelText("Location")).toHaveValue("/chosen/parent/my-org"),
  );
  fireEvent.click(screen.getByRole("button", { name: "Create and connect" }));
  await waitFor(() =>
    expect(api.create).toHaveBeenCalledWith("My Org", "/chosen/parent/my-org", false),
  );
});

it("preserves actionable create errors without selecting and cancels back to the actions", async () => {
  api.create.mockRejectedValue(
    new Error(
      "Context repo created at /new, but it is not connected. Use Connect a context repo… with this folder to retry.",
    ),
  );
  render(<ContextAttachments initialCreate />);
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "New" } });
  fireEvent.change(screen.getByLabelText("Location"), { target: { value: "/new" } });
  fireEvent.click(screen.getByRole("button", { name: "Create and connect" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("not connected");
  expect(useOrgStore.getState().activeOrgSlug).toBeNull();
  expect(api.teams).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(screen.queryByRole("form")).toBeNull();
  expect(screen.getByRole("button", { name: "Connect a context repo…" })).toBeInTheDocument();
});

it("retires an in-flight create completion after a profile transition", async () => {
  let resolve!: (value: unknown) => void;
  api.create.mockReturnValue(
    new Promise((done) => {
      resolve = done;
    }),
  );
  const view = render(<ContextAttachments initialCreate />);
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Old" } });
  fireEvent.change(screen.getByLabelText("Location"), { target: { value: "/old" } });
  fireEvent.click(screen.getByRole("button", { name: "Create and connect" }));
  principal = "two";
  view.rerender(<ContextAttachments initialCreate />);
  await act(async () => resolve({ loaded_org: { org: { slug: "local/old", teams: [] } } }));
  expect(useOrgStore.getState().activeOrgSlug).toBeNull();
  expect(api.teams).not.toHaveBeenCalled();
});

it.each(["event-first", "response-first"])(
  "durably completes creation once when discovery unmounts the form: %s",
  async (order) => {
    let finish!: (value: unknown) => void;
    api.create.mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    const org = { slug: "local/durable", display_name: "Durable", org_root: "/durable", teams: [] };
    function ColdStart() {
      useOrgDiscovery();
      const count = useOrgStore((s) => s.orgs.length);
      return count ? <p>Workspace loaded</p> : <ContextAttachments initialCreate />;
    }
    render(<ColdStart />);
    await waitFor(() => expect(api.discover).toHaveBeenCalled());
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Durable" } });
    fireEvent.change(screen.getByLabelText("Location"), { target: { value: "/durable" } });
    fireEvent.click(screen.getByRole("button", { name: "Create and connect" }));
    api.discover.mockResolvedValue([{ ...org, is_attached: true }]);
    api.load.mockResolvedValue({ org });
    const event = async () => {
      await act(async () => {
        for (const listener of api.listeners.get("context-attachments-changed") ?? []) listener();
      });
      await screen.findByText("Workspace loaded");
    };
    if (order === "event-first") await event();
    await act(async () => finish({ loaded_org: { org } }));
    if (order === "response-first") await event();
    expect(useOrgStore.getState().activeOrgSlug).toBe("local/durable");
    expect(api.teams).toHaveBeenCalledTimes(1);
    expect(api.teams).toHaveBeenCalledWith("create");
    expect(api.guest).not.toHaveBeenCalled();
  },
);

it("blocks competing mutations and enclosing dismissals while creation is admitted", async () => {
  let finish!: (value: unknown) => void;
  api.create.mockReturnValue(
    new Promise((resolve) => {
      finish = resolve;
    }),
  );
  api.list.mockResolvedValue([
    { slug: "existing", display_name: "Existing", org_root: "/existing" },
  ]);
  useUiStore.setState({ workspacesScreen: "create" });
  render(<ManageWorkspacesTab />);
  await screen.findByRole("button", { name: "Disconnect Existing" });
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Busy" } });
  fireEvent.change(screen.getByLabelText("Location"), { target: { value: "/busy" } });
  fireEvent.click(screen.getByRole("button", { name: "Create and connect" }));
  for (const name of ["Disconnect Existing", "Cancel"]) {
    for (const button of screen.getAllByRole("button", { name })) {
      expect(button).toBeDisabled();
      fireEvent.click(button);
    }
  }
  expect(api.attach).not.toHaveBeenCalled();
  expect(api.detach).not.toHaveBeenCalled();
  await act(async () =>
    finish({
      loaded_org: {
        org: { slug: "local/busy", display_name: "Busy", org_root: "/busy", teams: [] },
      },
    }),
  );
});
