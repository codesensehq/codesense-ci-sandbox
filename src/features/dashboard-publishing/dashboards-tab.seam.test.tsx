// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { openPath, openUrl } from "@tauri-apps/plugin-opener";
import { DashboardsTab } from "./dashboards-tab";
import { useOrgStore } from "@/shared/stores/org-store";
import type { HostedDashboard, InventoryRow } from "./types";
const { eventHandlers } = vi.hoisted(() => ({
  eventHandlers: new Map<string, (event: { payload: unknown }) => void>(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    eventHandlers.set(name, handler);
    return () => eventHandlers.delete(name);
  }),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
  openPath: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn().mockResolvedValue(null) }));
let dashboard: HostedDashboard;
let source = "hosted";
let pendingTakedown = false;
let teamRows: InventoryRow[] = [];
const teamHeaders = [{ org: "acme/context", team: "riverside" }];
beforeEach(() => {
  vi.clearAllMocks();
  source = "hosted";
  pendingTakedown = false;
  teamRows = [];
  eventHandlers.clear();
  useOrgStore.setState({ activeOrgSlug: "acme/context", activeTeamName: "riverside" });
  dashboard = {
    slug: "weekly-metrics",
    title: "Weekly metrics",
    state: "published",
    visibility: "private",
    current_version: 7,
    pushed_at: null,
    checked_at: null,
    share_count: 0,
    shares: [],
    live_queries: [],
    live_streams: ["terminal"],
    live_control: "owner",
    live_actions: null,
  };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "dashboard_live_requests") return [];
    if (command === "dashboard_publisher_key_status")
      return { status: "configured", hint: "csp_…a91f" };
    if (command === "dashboard_inventory") {
      const { localOnly } = args as { localOnly?: boolean };
      return {
        host: "https://share.example.invalid",
        source: localOnly ? "local" : source,
        checkedAt: "2026-10-20T10:00:00Z",
        error: source === "unavailable" ? "Couldn't reach server" : null,
        rows: localOnly
          ? teamRows
          : [
              ...teamRows,
              {
                slug: dashboard.slug,
                title: dashboard.title,
                folder: pendingTakedown ? "/tmp/metrics" : null,
                location: "hosted",
                localState: pendingTakedown ? "unpublished" : null,
                hosted: dashboard,
                pushedAt: null,
                checkedAt: null,
                error: pendingTakedown ? "takedown not confirmed" : null,
                differentKey: false,
                team: null,
              },
            ],
        teams: [],
      };
    }
    if (command === "dashboard_detail")
      return {
        dashboard,
        capabilities: [
          {
            label: "Restart build",
            available: false,
            detail: "Declared for viewers, not available yet.",
          },
        ],
        teams: teamHeaders,
      };
    if (command === "dashboard_share") {
      const { add, remove } = args as { add: string[]; remove: string[] };
      dashboard = {
        ...dashboard,
        shares: dashboard.shares
          .filter((s) => !remove.includes(s.email))
          .concat(add.map((email) => ({ email, invite: "drafted", sent_at: null }))),
      };
      return { slug: dashboard.slug, hostedCopy: null, viewers: [], shares: dashboard.shares };
    }
    if (command === "dashboard_control_links")
      return {
        take: "https://staging.example.invalid/account/control/weekly-metrics",
        end: "https://staging.example.invalid/account",
        expiresHours: 12,
      };
    if (command === "dashboard_move_link")
      return "https://staging.example.invalid/account/move/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    if (command === "dashboard_publisher_key_save") return { status: "refused", hint: "csp_…new1" };
    if (command === "dashboard_publish")
      return { slug: "team-dashboard", hostedCopy: "live", hostedCopyError: null };
    throw new Error(`Unexpected command ${command}`);
  });
});
async function select() {
  fireEvent.click(await screen.findByRole("option", { name: /Weekly metrics/ }));
  await screen.findByRole("button", { name: "Add viewer" });
}
async function notifyDashboardChange(
  path = "/tmp/riverside/content/dashboards/changed/dashboard.yml",
) {
  await act(async () => {
    eventHandlers.get("context-index-changed")?.({
      payload: {
        generation: 2,
        paths: [path],
        orgs: ["acme/context"],
        allOrgs: false,
        truncated: false,
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 240));
  });
}
async function waitForInventoryReady() {
  await waitFor(() => expect(screen.getByRole("button", { name: "Refresh" })).toBeEnabled());
}
it("drives real wrappers for hosted-only viewers and browser control without claiming an email was sent", async () => {
  render(<DashboardsTab />);
  await select();
  expect(screen.queryByRole("button", { name: "Update" })).toBeNull();
  expect(screen.getByText("Declared for viewers, not available yet.")).toBeInTheDocument();
  fireEvent.change(screen.getByRole("textbox", { name: "Viewer email" }), {
    target: { value: "ada@example.com" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Add viewer" }));
  expect(await screen.findByText("ada@example.com")).toBeInTheDocument();
  expect(screen.getByText("Can view · Invite drafted, not sent yet")).toBeInTheDocument();
  expect(invoke).toHaveBeenCalledWith("dashboard_share", {
    slug: "weekly-metrics",
    add: ["ada@example.com"],
    remove: [],
  });
  fireEvent.click(screen.getByRole("button", { name: "Take control in browser" }));
  await waitFor(() =>
    expect(openUrl).toHaveBeenCalledWith(
      "https://staging.example.invalid/account/control/weekly-metrics",
    ),
  );
  fireEvent.click(screen.getByRole("button", { name: "End control in browser" }));
  await waitFor(() =>
    expect(openUrl).toHaveBeenCalledWith("https://staging.example.invalid/account"),
  );
});

function teamDashboard(title: string, slug: string): InventoryRow {
  return {
    slug,
    title,
    folder: `/tmp/riverside/content/dashboards/${slug}`,
    location: "local",
    localState: null,
    hosted: null,
    pushedAt: null,
    checkedAt: null,
    error: null,
    differentKey: false,
    team: {
      org: "acme/context",
      team: "riverside",
      sharedFrom: null,
      relativeFolder: `dashboards/${slug}`,
    },
  };
}

it("lists live team dashboard additions and removals from filtered context-index events", async () => {
  const sharedDashboard = teamDashboard("Org health", "org-health");
  sharedDashboard.team = {
    ...sharedDashboard.team!,
    sharedFrom: "parent",
    relativeFolder: "dashboards/org-health",
  };
  teamRows = [teamDashboard("Sprint 1", "sprint-1"), sharedDashboard];
  render(<DashboardsTab />);
  expect(await screen.findByText("Sprint 1")).toBeInTheDocument();
  expect(screen.getByText("Not published")).toBeInTheDocument();
  expect(screen.getByText("dashboards/sprint-1")).toBeInTheDocument();
  expect(screen.getByText("Shared from parent")).toBeInTheDocument();
  expect(
    screen.getByText("Not published · exported by parent · dashboards/org-health"),
  ).toBeInTheDocument();
  await waitFor(() => expect(eventHandlers.has("context-index-changed")).toBe(true));

  teamRows = [...teamRows, teamDashboard("Sprint 2", "sprint-2")];
  await act(async () => {
    eventHandlers.get("context-index-changed")?.({
      payload: {
        generation: 2,
        paths: ["/tmp/riverside/content/dashboards/sprint-2/dashboard.yml"],
        orgs: ["acme/context"],
        allOrgs: false,
        truncated: false,
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 240));
  });
  expect(await screen.findByText("Sprint 2")).toBeInTheDocument();
  expect(screen.getByText("Sprint 2 added to riverside")).toBeInTheDocument();

  teamRows = teamRows.filter((row) => row.slug !== "sprint-2");
  await act(async () => {
    eventHandlers.get("context-index-changed")?.({
      payload: {
        generation: 3,
        paths: ["/tmp/riverside/content/dashboards/sprint-2/dashboard.yml"],
        orgs: ["acme/context"],
        allOrgs: false,
        truncated: false,
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 240));
  });
  expect(screen.queryByText("Sprint 2", { selector: "span" })).toBeNull();
  expect(screen.getByText("Sprint 2 was removed from riverside")).toBeInTheDocument();
});

it("keeps Different key rows interactive during focus refresh and clears the warning", async () => {
  const different = {
    ...teamDashboard("Owned elsewhere", "different-key"),
    team: null,
    localState: "published" as const,
    differentKey: true,
    folder: "/tmp/different-key",
  };
  const original = vi.mocked(invoke).getMockImplementation()!;
  let hostedCalls = 0;
  let resolveHosted!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementation(async (command, args, options) => {
    if (command === "dashboard_inventory") {
      const localOnly = (args as { localOnly?: boolean }).localOnly;
      if (!localOnly && ++hostedCalls > 1) {
        return new Promise((resolve) => {
          resolveHosted = resolve;
        });
      }
      return {
        host: "https://share.example.invalid",
        source: localOnly ? "local" : "hosted",
        checkedAt: "2026-10-20T10:00:00Z",
        error: null,
        rows: [localOnly ? { ...different, differentKey: false } : different],
        teams: [],
      };
    }
    return original(command, args, options);
  });
  render(<DashboardsTab />);
  expect(await screen.findByText("Different key")).toBeInTheDocument();
  await waitForInventoryReady();
  fireEvent.focus(window);
  await waitFor(() => expect(hostedCalls).toBe(2));
  expect(screen.getByText("Different key")).toBeInTheDocument();
  expect(screen.getByRole("option", { name: /Owned elsewhere/ })).toBeEnabled();
  expect(screen.queryByText("Refreshing…")).toBeNull();
  await act(async () =>
    resolveHosted({
      host: "https://share.example.invalid",
      source: "hosted",
      checkedAt: "2026-10-20T10:01:00Z",
      error: null,
      rows: [{ ...different, differentKey: false }],
      teams: [],
    }),
  );
  await waitForInventoryReady();
  expect(screen.queryByText("Different key")).toBeNull();
  expect(screen.queryByRole("button", { name: "Move to this key" })).toBeNull();
});

it("does not load hosted details during a local-only inventory refresh", async () => {
  teamRows = [teamDashboard("Draft", "draft")];
  render(<DashboardsTab />);
  await waitForInventoryReady();
  fireEvent.click(await screen.findByRole("option", { name: /Draft/ }));
  await notifyDashboardChange();
  expect(invoke).not.toHaveBeenCalledWith("dashboard_detail", expect.anything());
});

it("drains a live local refresh that arrives while selecting a slow hosted row", async () => {
  teamRows = [];
  let resolveDetail!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "dashboard_live_requests") return [];
    if (command === "dashboard_publisher_key_status")
      return { status: "configured", hint: "csp_…a91f" };
    if (command === "dashboard_inventory") {
      const localOnly = (args as { localOnly?: boolean }).localOnly;
      return {
        host: "https://share.example.invalid",
        source: localOnly ? "local" : "hosted",
        checkedAt: "2026-10-20T10:00:00Z",
        error: null,
        teams: teamHeaders,
        rows: localOnly
          ? teamRows
          : [
              {
                slug: dashboard.slug,
                title: dashboard.title,
                folder: null,
                location: "hosted",
                localState: null,
                hosted: dashboard,
                pushedAt: null,
                checkedAt: null,
                error: null,
                differentKey: false,
                team: null,
              },
            ],
      };
    }
    if (command === "dashboard_detail")
      return new Promise((resolve) => {
        resolveDetail = resolve;
      });
    throw new Error(`Unexpected command ${command}`);
  });
  render(<DashboardsTab />);
  await waitForInventoryReady();
  const option = await screen.findByRole("option", { name: /Weekly metrics/ });
  fireEvent.click(option);
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("dashboard_detail", { slug: "weekly-metrics" }),
  );
  teamRows = [teamDashboard("Added while loading", "added")];
  await notifyDashboardChange();
  await act(async () => {
    resolveDetail({ dashboard, capabilities: [] });
  });
  expect(await screen.findByText("Added while loading")).toBeInTheDocument();
});

it("keeps same-slug local folders distinct in the offline inventory fallback", async () => {
  const first = teamDashboard("One", "collision");
  const second = { ...teamDashboard("Two", "collision"), folder: "/tmp/other/collision" };
  teamRows = [first, second];
  render(<DashboardsTab />);
  await waitForInventoryReady();
  expect(await screen.findByText("One")).toBeInTheDocument();
  source = "unavailable";
  fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
  await waitFor(() => expect(screen.getByText(/Couldn't reach/)).toBeInTheDocument());
  const options = screen.getAllByRole("option");
  expect(options.map((option) => option.textContent)).toEqual(
    expect.arrayContaining([expect.stringContaining("One"), expect.stringContaining("Two")]),
  );
  expect(options).toHaveLength(3); // both local folders plus the hosted-only fixture row
  fireEvent.click(screen.getByRole("option", { name: /One/ }));
  fireEvent.click(await screen.findByRole("button", { name: "Open folder" }));
  await waitFor(() => expect(openPath).toHaveBeenLastCalledWith(first.folder));
  fireEvent.click(screen.getByRole("option", { name: /Two/ }));
  fireEvent.click(await screen.findByRole("button", { name: "Open folder" }));
  await waitFor(() => expect(openPath).toHaveBeenLastCalledWith(second.folder));
});

it("restores focus to a surviving row when the focused unselected row is removed live", async () => {
  teamRows = [teamDashboard("First", "first"), teamDashboard("Second", "second")];
  render(<DashboardsTab />);
  await waitForInventoryReady();
  const first = await screen.findByRole("option", { name: /First/ });
  first.focus();
  expect(first).toHaveFocus();
  teamRows = [teamRows[1]!];
  await notifyDashboardChange();
  await waitFor(() => expect(screen.getByRole("option", { name: /Second/ })).toHaveFocus());
});

it("keeps typing focus when a previously focused row is removed live", async () => {
  teamRows = [teamDashboard("First", "first"), teamDashboard("Second", "second")];
  render(<DashboardsTab />);
  await waitForInventoryReady();
  const first = await screen.findByRole("option", { name: /First/ });
  first.focus();
  fireEvent.click(screen.getByRole("button", { name: "Replace" }));
  const field = await screen.findByLabelText("Publishing key", { selector: "input" });
  field.focus();
  expect(field).toHaveFocus();

  teamRows = [teamRows[1]!];
  await notifyDashboardChange();
  expect(field).toHaveFocus();
});

it("does not load hosted details for a selected local draft after initial inventory completes", async () => {
  teamRows = [teamDashboard("Local draft", "local-draft")];
  const original = vi.mocked(invoke).getMockImplementation()!;
  let resolveHosted!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementation((command, args, options) => {
    if (command === "dashboard_inventory" && !(args as { localOnly?: boolean }).localOnly)
      return new Promise((resolve) => {
        resolveHosted = resolve;
      });
    if (command === "dashboard_detail" && (args as { slug: string }).slug === "local-draft")
      return Promise.reject(new Error("This key can't manage this dashboard"));
    return original(command, args, options);
  });
  render(<DashboardsTab />);
  const row = await screen.findByRole("option", { name: /Local draft/ });
  fireEvent.click(row);
  expect(await screen.findByText("Not published yet")).toBeInTheDocument();

  await act(async () =>
    resolveHosted({
      host: "https://share.example.invalid",
      source: "hosted",
      checkedAt: "2026-10-20T10:00:00Z",
      error: null,
      rows: [...teamRows],
      teams: teamHeaders,
    }),
  );
  expect(invoke).not.toHaveBeenCalledWith("dashboard_detail", { slug: "local-draft" });
  expect(screen.queryByText("This key can't manage this dashboard")).toBeNull();
});

it("restores management after publishing the selected draft", async () => {
  const draft = teamDashboard("First publication", "first-publication");
  const hosted = { ...dashboard, slug: draft.slug, title: draft.title };
  let published = false;
  const original = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, args, options) => {
    if (command === "dashboard_inventory") {
      const localOnly = (args as { localOnly?: boolean }).localOnly;
      return {
        host: "https://share.example.invalid",
        source: localOnly ? "local" : "hosted",
        checkedAt: "2026-10-20T10:00:00Z",
        error: null,
        teams: teamHeaders,
        rows: [
          {
            ...draft,
            localState: published ? "published" : null,
            hosted: published ? hosted : null,
            location: published ? "both" : "local",
          },
        ],
      };
    }
    if (command === "dashboard_publish") {
      published = true;
      return { slug: draft.slug, hostedCopy: "live", hostedCopyError: null };
    }
    if (command === "dashboard_detail") return { dashboard: hosted, capabilities: [] };
    return original(command, args, options);
  });
  teamRows = [draft];
  render(<DashboardsTab />);
  await waitForInventoryReady();
  fireEvent.click(await screen.findByRole("option", { name: /First publication/ }));
  fireEvent.click(await screen.findByRole("button", { name: "Publish" }));
  await screen.findByText("Published. Viewers see it on their next load.");
  expect(screen.getByRole("button", { name: "Update" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Unpublish" })).toBeEnabled();
  const viewerEmail = screen.getByRole("textbox", { name: "Viewer email" });
  expect(viewerEmail).toBeEnabled();
  fireEvent.change(viewerEmail, { target: { value: "ada@example.com" } });
  expect(screen.getByRole("button", { name: "Add viewer" })).toBeEnabled();
});

it("renders local dashboards while publishing key verification is pending", async () => {
  teamRows = [teamDashboard("Local before key probe", "local-before-key")];
  const original = vi.mocked(invoke).getMockImplementation()!;
  let resolveKey!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementation((command, args, options) => {
    if (command === "dashboard_publisher_key_status")
      return new Promise((resolve) => {
        resolveKey = resolve;
      });
    return original(command, args, options);
  });
  render(<DashboardsTab />);
  await waitFor(() => expect(resolveKey).toBeTypeOf("function"));
  try {
    await waitFor(
      () => expect(screen.getByRole("option", { name: /Local before key probe/ })).toBeEnabled(),
      { timeout: 100 },
    );
  } finally {
    await act(async () => resolveKey({ status: "configured", hint: "csp_…a91f" }));
  }
});

it("marks a live-added dashboard for ten seconds", async () => {
  teamRows = [teamDashboard("Existing dashboard", "existing")];
  render(<DashboardsTab />);
  await waitForInventoryReady();
  const existing = screen.getByRole("option", { name: /Existing dashboard/ });
  expect(within(existing).queryByText("Added just now")).toBeNull();
  teamRows = [...teamRows, teamDashboard("New dashboard", "new-dashboard")];
  vi.useFakeTimers();
  try {
    await act(async () => {
      eventHandlers.get("context-index-changed")?.({
        payload: {
          generation: 2,
          paths: ["/tmp/riverside/content/dashboards/new-dashboard/dashboard.yml"],
          orgs: ["acme/context"],
          allOrgs: false,
          truncated: false,
        },
      });
      await vi.advanceTimersByTimeAsync(180);
    });
    const added = screen.getByRole("option", { name: /New dashboard/ });
    expect(within(added).getByText("Added just now")).toBeInTheDocument();

    await act(async () => vi.advanceTimersByTimeAsync(10_000));
    expect(within(added).queryByText("Added just now")).toBeNull();
  } finally {
    vi.useRealTimers();
  }
});

it("keeps shared-row arrow navigation in the rendered parent-group order", async () => {
  const alpha = {
    ...teamDashboard("Alpha", "alpha"),
    team: { ...teamDashboard("Alpha", "alpha").team!, sharedFrom: "parent-a" },
  };
  const zulu = {
    ...teamDashboard("Zulu", "zulu"),
    team: { ...teamDashboard("Zulu", "zulu").team!, sharedFrom: "parent-a" },
  };
  const beta = {
    ...teamDashboard("Beta", "beta"),
    team: { ...teamDashboard("Alpha", "alpha").team!, sharedFrom: "parent-b" },
  };
  teamRows = [zulu, alpha, beta];
  render(<DashboardsTab />);
  await waitForInventoryReady();
  const firstRendered = await screen.findByRole("option", { name: /Alpha/ });
  const secondRendered = screen.getByRole("option", { name: /Zulu/ });
  expect(
    firstRendered.compareDocumentPosition(secondRendered) & Node.DOCUMENT_POSITION_FOLLOWING,
  ).toBeTruthy();
  firstRendered.focus();
  fireEvent.keyDown(firstRendered, { key: "ArrowDown" });
  expect(secondRendered).toHaveFocus();
});

it("lets the hosted unpublished state override a stale local published state in its chip", async () => {
  dashboard = { ...dashboard, state: "unpublished" };
  pendingTakedown = false;
  const original = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, args, options) => {
    if (command === "dashboard_inventory") {
      const result = (await original(command, args, options)) as {
        rows: InventoryRow[];
        [key: string]: unknown;
      };
      return {
        ...result,
        rows: result.rows.map((row) =>
          row.slug === dashboard.slug
            ? { ...row, localState: "published", hosted: dashboard }
            : row,
        ),
      };
    }
    return original(command, args, options);
  });
  render(<DashboardsTab />);
  await waitForInventoryReady();
  const option = await screen.findByRole("option", { name: /Weekly metrics/ });
  expect(
    within(option).getAllByText("Unpublished · link no longer works", { exact: true }),
  ).toHaveLength(2);
  expect(within(option).queryByText("Published", { exact: true })).toBeNull();
});

it("moves a Different key dashboard to this Mac's key through browser confirmation", async () => {
  const different = {
    ...teamDashboard("Owned elsewhere", "different-key"),
    team: null,
    localState: "published" as const,
    differentKey: true,
    folder: "/tmp/different-key",
  };
  const original = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, args, options) => {
    if (command === "dashboard_inventory")
      return {
        host: "https://share.example.invalid",
        source: "hosted",
        checkedAt: "now",
        error: null,
        rows: [different],
        teams: [],
      };
    return original(command, args, options);
  });
  render(<DashboardsTab />);
  await waitForInventoryReady();
  fireEvent.click(await screen.findByRole("option", { name: /Owned elsewhere/ }));
  expect(await screen.findByTestId("move-to-key-slot")).toBeInTheDocument();
  expect(
    screen.getByText("Move this dashboard to the key on this Mac. You'll confirm in your browser."),
  ).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Move to this key" }));
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("dashboard_move_link", { slug: different.slug }),
  );
  await waitFor(() =>
    expect(openUrl).toHaveBeenCalledWith(
      "https://staging.example.invalid/account/move/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ),
  );
});

it("keeps local team rows interactive while the initial hosted inventory is pending", async () => {
  teamRows = [teamDashboard("Local draft", "local-draft")];
  const original = vi.mocked(invoke).getMockImplementation()!;
  let resolveHosted!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementation((command, args, options) => {
    if (command === "dashboard_inventory" && !(args as { localOnly?: boolean }).localOnly)
      return new Promise((resolve) => {
        resolveHosted = resolve;
      });
    return original(command, args, options);
  });
  render(<DashboardsTab />);
  const local = await screen.findByRole("option", { name: /Local draft/ });
  expect(local).toBeEnabled();
  fireEvent.click(local);
  expect(await screen.findByText("Not published yet")).toBeInTheDocument();
  await act(async () =>
    resolveHosted({
      host: "https://share.example.invalid",
      source: "hosted",
      checkedAt: "2026-10-20T10:00:00Z",
      error: null,
      rows: [],
      teams: [],
    }),
  );
});

it("attaches a team row before publishing it", async () => {
  const teamRow = teamDashboard("Team dashboard", "team-dashboard");
  teamRows = [teamRow];
  render(<DashboardsTab />);
  fireEvent.click(await screen.findByRole("option", { name: /Team dashboard/ }));
  fireEvent.click(await screen.findByRole("button", { name: "Publish" }));
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("dashboard_publish", { folder: teamRow.folder }),
  );

  const calls = vi.mocked(invoke).mock.calls;
  const attachIndex = calls.findIndex(
    ([command, args]) =>
      command === "dashboard_inventory" && (args as { folder?: string }).folder === teamRow.folder,
  );
  const publishIndex = calls.findIndex(([command]) => command === "dashboard_publish");
  expect(attachIndex).toBeGreaterThanOrEqual(0);
  expect(publishIndex).toBeGreaterThan(attachIndex);
});

it("sends a pasted key once, clears its input, and shows the backend refusal", async () => {
  render(<DashboardsTab />);
  await screen.findByText("Key saved");
  await waitFor(() => expect(screen.getByRole("button", { name: "Replace" })).toBeEnabled());
  fireEvent.click(screen.getByRole("button", { name: "Replace" }));
  const field = screen.getByLabelText("Publishing key", { selector: "input" });
  expect(field).toHaveAttribute("type", "password");
  fireEvent.change(field, { target: { value: "csp_private_new1" } });
  fireEvent.click(screen.getByRole("button", { name: "Save key" }));
  expect(field).toHaveValue("");
  expect(await screen.findByText("The server refused this key")).toBeInTheDocument();
  expect(invoke).toHaveBeenCalledWith("dashboard_publisher_key_save", { key: "csp_private_new1" });
  expect(screen.queryByText("csp_private_new1")).toBeNull();
});
it("labels an undeployed inventory endpoint as local records only", async () => {
  source = "unsupported";
  render(<DashboardsTab />);
  expect(await screen.findByText(/The server list isn't available yet/)).toBeInTheDocument();
  expect(screen.getByRole("heading", { name: "Your dashboards" })).toBeInTheDocument();
});

it("labels team visibility as reserved and private visibility as it is", async () => {
  dashboard = { ...dashboard, visibility: "team" };
  const { unmount } = render(<DashboardsTab />);
  await select();
  expect(
    screen.getByText("Visibility: team (reserved, currently the same as private)"),
  ).toBeTruthy();
  unmount();
  dashboard = { ...dashboard, visibility: "private" };
  render(<DashboardsTab />);
  await select();
  expect(screen.getByText("Visibility: private")).toBeTruthy();
});

it("offers a pending local takedown as Retry unpublish and keeps Update unavailable", async () => {
  pendingTakedown = true;
  render(<DashboardsTab />);
  await select();
  expect(screen.queryByRole("button", { name: "Update" })).toBeNull();
  expect(screen.getByRole("button", { name: "Retry unpublish" })).toBeEnabled();
});

it("keeps Take control disabled on an unpublished dashboard while End control stays available", async () => {
  dashboard = { ...dashboard, state: "unpublished" };
  render(<DashboardsTab />);
  fireEvent.click(await screen.findByRole("option", { name: /Weekly metrics/ }));
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "End control in browser" })).toBeEnabled(),
  );
  expect(screen.getByRole("button", { name: "Take control in browser" })).toBeDisabled();
});

it("shows session status and signs a viewer out through the Tauri wrapper", async () => {
  dashboard = {
    ...dashboard,
    session_days: 30,
    shares: [
      {
        id: "shr_viewer",
        email: "viewer@example.com",
        invite: "sent",
        sent_at: null,
        sessions: 2,
        last_seen_at: Math.floor(Date.now() / 1000) - 3 * 3600,
      },
    ],
  };
  const original = vi.mocked(invoke).getMockImplementation()!;
  vi.mocked(invoke).mockImplementation(async (command, args, options) => {
    if (command === "dashboard_viewer_sign_out") {
      dashboard = {
        ...dashboard,
        shares: dashboard.shares.map((v) => ({ ...v, sessions: 0, last_seen_at: null })),
      };
      return;
    }
    return original(command, args, options);
  });
  render(<DashboardsTab />);
  await select();
  expect(screen.getByText(/Viewers stay signed in for 30 days/)).toBeInTheDocument();
  expect(screen.getByText("Signed in on 2 devices · last active 3 hours ago")).toHaveAttribute(
    "title",
  );
  fireEvent.click(screen.getByRole("button", { name: "Sign out everywhere" }));
  expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus();
  fireEvent.click(screen.getByRole("button", { name: /^Sign out$/ }));
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("dashboard_viewer_sign_out", {
      slug: dashboard.slug,
      shareId: "shr_viewer",
    }),
  );
  expect(await screen.findByText("Invited · not joined yet")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Sign out everywhere" })).toBeNull();
});

it("says joined only for a viewer who opened it through their account", async () => {
  const hoursAgo = Math.floor(Date.now() / 1000) - 3 * 3600;
  dashboard.shares = [
    {
      id: "a",
      email: "joined@example.com",
      invite: "sent",
      sent_at: null,
      sessions: 2,
      last_seen_at: hoursAgo,
      joined: true,
    },
    {
      id: "b",
      email: "idle@example.com",
      invite: "sent",
      sent_at: null,
      sessions: 0,
      joined: true,
    },
    {
      id: "c",
      email: "carry@example.com",
      invite: "sent",
      sent_at: null,
      sessions: 1,
      last_seen_at: hoursAgo,
      joined: false,
    },
    {
      id: "d",
      email: "new@example.com",
      invite: "sent",
      sent_at: null,
      sessions: 0,
      joined: false,
    },
    {
      id: "e",
      email: "quiet@example.com",
      invite: "suppressed",
      sent_at: null,
      sessions: 0,
      joined: false,
    },
    { id: "f", email: "unknown@example.com", invite: "sent", sent_at: null, sessions: null },
  ];
  render(<DashboardsTab />);
  await select();
  expect(
    screen.getByText("Joined · signed in on 2 devices · last active 3 hours ago"),
  ).toBeInTheDocument();
  expect(screen.getByText("Joined · not signed in right now")).toBeInTheDocument();
  expect(screen.getByText("Signed in on 1 device · last active 3 hours ago")).toBeInTheDocument();
  expect(screen.getByText("Invited · not joined yet")).toBeInTheDocument();
  expect(screen.getByText("Not joined yet")).toBeInTheDocument();
  expect(screen.getByText("Sign-in status unavailable")).toBeInTheDocument();
  expect(screen.getAllByRole("button", { name: "Sign out everywhere" })).toHaveLength(2);
});

it("shows waiting and suppressed invitations with visible help", async () => {
  dashboard.shares = [
    { email: "waiting@example.com", invite: "waiting_limit", sent_at: null, sessions: 0 },
    { email: "quiet@example.com", invite: "suppressed", sent_at: null, sessions: 0 },
  ];
  render(<DashboardsTab />);
  await select();
  expect(screen.getByText(/Invite waiting \(daily limit\)/)).toBeInTheDocument();
  expect(
    screen.getByText(/It's saved and goes out when the limit resets tomorrow/),
  ).toBeInTheDocument();
  expect(screen.getByText(/Won't receive email/)).toBeInTheDocument();
  expect(screen.getByText(/This person asked CodeSense not to email them/)).toBeInTheDocument();
  expect(screen.getAllByText(/codesense\.ai\/signin with this\s+email/)).toHaveLength(2);
  expect(screen.queryByRole("button", { name: "dashboard link" })).toBeNull();
  expect(screen.queryByText(/CodeSense staff/)).toBeNull();
});

it("refetches a just-added invitation once after four seconds", async () => {
  dashboard.shares = [
    {
      id: "new-share",
      email: "new@example.com",
      invite: "drafted",
      added_at: Date.now() / 1000,
      sent_at: null,
      sessions: 0,
    },
  ];
  render(<DashboardsTab />);
  await select();
  expect(screen.getByText(/Sending invite…/)).toBeInTheDocument();
  const calls = vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === "dashboard_detail").length;
  dashboard = {
    ...dashboard,
    shares: [{ ...dashboard.shares[0]!, invite: "sent", sent_at: Math.floor(Date.now() / 1000) }],
  };
  await waitFor(() => expect(screen.getByText(/Can view · Invite sent/)).toBeInTheDocument(), {
    timeout: 6000,
  });
  expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === "dashboard_detail")).toHaveLength(
    calls + 1,
  );
});
