// SPDX-License-Identifier: MIT
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import * as api from "@/tauri/commands";
import { useDashboardManager } from "./use-dashboard-manager";
import type { DashboardDetail, DashboardInventory, InventoryRow } from "./types";
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@/tauri/commands", () => ({
  dashboardPublisherKeyStatus: vi.fn(),
  dashboardPublisherKeySave: vi.fn(),
  dashboardPublisherKeyRemove: vi.fn(),
  dashboardInventory: vi.fn(),
  dashboardDetail: vi.fn(),
  dashboardPublish: vi.fn(),
  dashboardUnpublish: vi.fn(),
  dashboardShare: vi.fn(),
}));
const inventory: DashboardInventory = {
  host: "https://share.example.invalid",
  source: "hosted" as const,
  checkedAt: "2026-10-20T10:00:00Z",
  error: null,
  rows: [],
  teams: [],
};
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(api.dashboardPublisherKeyStatus).mockResolvedValue({
    status: "configured",
    hint: "csp_…a91f",
  });
  vi.mocked(api.dashboardInventory).mockResolvedValue(inventory);
});
it("keeps confirmed inventory visibly stale after a failed refresh", async () => {
  const { result } = renderHook(useDashboardManager);
  await waitFor(() => expect(result.current.busy).toBe(null));
  vi.mocked(api.dashboardInventory).mockResolvedValue({
    ...inventory,
    source: "unavailable",
    error: "Couldn't reach server",
    checkedAt: "later",
  });
  await act(() => result.current.refresh());
  expect(result.current.inventory?.source).toBe("stale");
  expect(result.current.inventory?.checkedAt).toBe(inventory.checkedAt);
});
it("refreshes hosted inventory when the app regains focus", async () => {
  const { result } = renderHook(useDashboardManager);
  await waitFor(() => expect(result.current.busy).toBe(null));
  await waitFor(() => expect(result.current.inventory?.source).toBe("hosted"));
  vi.mocked(api.dashboardInventory).mockClear();
  vi.mocked(api.dashboardPublisherKeyStatus).mockClear();

  await act(async () => {
    window.dispatchEvent(new Event("focus"));
  });

  await waitFor(() => expect(api.dashboardInventory).toHaveBeenCalledWith(undefined, false));
  expect(api.dashboardPublisherKeyStatus).toHaveBeenCalledTimes(1);
});
it("keeps loaded dashboard rows usable while a focus refresh is pending", async () => {
  const row = {
    slug: "local-report",
    title: "Local report",
    folder: "/tmp/local-report",
    localState: "published",
    hosted: null,
  } as unknown as InventoryRow;
  vi.mocked(api.dashboardInventory).mockImplementation((_folder, localOnly) =>
    Promise.resolve(
      localOnly ? { ...inventory, source: "local", rows: [row] } : { ...inventory, rows: [row] },
    ),
  );
  const { result } = renderHook(useDashboardManager);
  await waitFor(() => expect(result.current.busy).toBe(null));
  await waitFor(() => expect(result.current.inventory?.rows[0]?.slug).toBe(row.slug));

  let resolveStatus!: (value: Awaited<ReturnType<typeof api.dashboardPublisherKeyStatus>>) => void;
  vi.mocked(api.dashboardPublisherKeyStatus).mockReturnValue(
    new Promise((resolve) => {
      resolveStatus = resolve;
    }),
  );
  act(() => window.dispatchEvent(new Event("focus")));

  await waitFor(() => expect(api.dashboardPublisherKeyStatus).toHaveBeenCalledTimes(2));
  expect(result.current.busy).toBe(null);
  expect(result.current.inventory?.rows[0]?.slug).toBe(row.slug);
  await act(async () => resolveStatus({ status: "configured", hint: "csp_…a91f" }));
});
it("does not claim success while a publication is pending or refused", async () => {
  const { result } = renderHook(useDashboardManager);
  await waitFor(() => expect(result.current.busy).toBe(null));
  let resolve!: (value: Awaited<ReturnType<typeof api.dashboardPublish>>) => void;
  vi.mocked(api.dashboardPublish).mockReturnValue(
    new Promise((r) => {
      resolve = r;
    }),
  );
  let pending!: Promise<void>;
  act(() => {
    pending = result.current.publish({ slug: "metrics", folder: "/tmp/metrics" } as never);
  });
  expect(result.current.message).toBe("");
  expect(result.current.busy).toBe("Publishing…");
  await act(async () => {
    resolve({
      slug: "metrics",
      hostedCopy: "auth_failed",
      hostedCopyError: "Fix key",
      viewers: [],
      shares: [],
    });
    await pending;
  });
  expect(result.current.error).toContain("Didn't publish");
  expect(result.current.message).toBe("");
});
it("clears prior publisher inventory on a key replacement even when the server is offline", async () => {
  const { result } = renderHook(useDashboardManager);
  await waitFor(() => expect(result.current.busy).toBe(null));
  vi.mocked(api.dashboardPublisherKeySave).mockResolvedValue({
    status: "unavailable",
    hint: "csp_…new1",
  });
  vi.mocked(api.dashboardInventory).mockResolvedValue({
    ...inventory,
    source: "unavailable",
    error: "offline",
    rows: [],
  });
  await act(() => result.current.saveKey("csp_private_new1"));
  expect(result.current.inventory?.source).toBe("unavailable");
  expect(result.current.selected).toBe(null);
  expect(result.current.key?.status).toBe("unavailable");
});

it("drops an initial inventory completion after the publishing key is replaced", async () => {
  const oldInventory = {
    ...inventory,
    rows: [{ slug: "old-publisher", title: "Old publisher" }],
  } as unknown as DashboardInventory;
  const newInventory = {
    ...inventory,
    rows: [{ slug: "new-publisher", title: "New publisher" }],
  } as unknown as DashboardInventory;
  let hostedCalls = 0;
  let resolveInitial!: (value: typeof inventory) => void;
  vi.mocked(api.dashboardPublisherKeySave).mockResolvedValue({
    status: "configured",
    hint: "csp_…new1",
  });
  vi.mocked(api.dashboardInventory).mockImplementation((_folder, localOnly) => {
    if (localOnly) return Promise.resolve({ ...inventory, source: "local" });
    hostedCalls += 1;
    if (hostedCalls === 1)
      return new Promise((resolve) => {
        resolveInitial = resolve;
      });
    return Promise.resolve(newInventory);
  });
  const { result } = renderHook(useDashboardManager);
  await waitFor(() => expect(result.current.busy).toBe(null));
  await waitFor(() => expect(hostedCalls).toBe(1));

  await act(() => result.current.saveKey("csp_replacement_key"));
  expect(result.current.inventory?.rows[0]?.slug).toBe("new-publisher");

  await act(async () => resolveInitial(oldInventory));
  expect(result.current.key?.hint).toBe("csp_…new1");
  expect(result.current.inventory?.rows[0]?.slug).toBe("new-publisher");
});

it("preserves hosted confirmation time during a local-only refresh", async () => {
  const confirmedAt = "2026-10-20T10:00:00Z";
  const localAt = "2026-10-20T11:00:00Z";
  let localCalls = 0;
  vi.mocked(api.dashboardInventory).mockImplementation((_folder, localOnly) => {
    if (localOnly) {
      localCalls += 1;
      return Promise.resolve({
        ...inventory,
        source: "local",
        checkedAt: localCalls === 1 ? confirmedAt : localAt,
      });
    }
    return Promise.resolve({
      ...inventory,
      checkedAt: confirmedAt,
    });
  });
  const { result } = renderHook(useDashboardManager);
  await waitFor(() => expect(result.current.inventory?.source).toBe("hosted"));

  vi.mocked(api.dashboardInventory).mockImplementation((_folder, localOnly) =>
    Promise.resolve(
      localOnly
        ? { ...inventory, source: "local", checkedAt: localAt }
        : { ...inventory, source: "unavailable", error: "offline", checkedAt: localAt },
    ),
  );
  await act(() => result.current.refresh());
  expect(result.current.inventory?.source).toBe("stale");
  expect(result.current.inventory?.checkedAt).toBe(confirmedAt);

  await act(() => result.current.refreshLocal());
  expect(result.current.inventory?.source).toBe("stale");
  expect(result.current.inventory?.checkedAt).toBe(confirmedAt);
});

it("keeps the newer selection when initial background detail completes late", async () => {
  const first = {
    slug: "first-hosted",
    title: "First hosted",
    folder: "/tmp/first-hosted",
    localState: "published",
    hosted: null,
  } as unknown as InventoryRow;
  const second = {
    slug: "second-hosted",
    title: "Second hosted",
    folder: "/tmp/second-hosted",
    localState: "published",
    hosted: null,
  } as unknown as InventoryRow;
  const firstDetail = {
    dashboard: { slug: first.slug, shares: [] },
    capabilities: [],
  } as unknown as DashboardDetail;
  const secondDetail = {
    dashboard: { slug: second.slug, shares: [] },
    capabilities: [],
  } as unknown as DashboardDetail;
  let resolveInitial!: (value: DashboardInventory) => void;
  let resolveStaleDetail!: (value: DashboardDetail) => void;
  let firstDetailCalls = 0;
  const initialInventory = { ...inventory, source: "local" as const, rows: [first, second] };
  vi.mocked(api.dashboardInventory).mockImplementation((_folder, localOnly) => {
    if (localOnly) return Promise.resolve(initialInventory);
    return new Promise((resolve) => {
      resolveInitial = resolve;
    });
  });
  vi.mocked(api.dashboardDetail).mockImplementation((slug) => {
    if (slug === first.slug && ++firstDetailCalls === 2)
      return new Promise((resolve) => {
        resolveStaleDetail = resolve;
      });
    return Promise.resolve(slug === first.slug ? firstDetail : secondDetail);
  });
  const { result } = renderHook(useDashboardManager);
  await waitFor(() => expect(resolveInitial).toBeTypeOf("function"));

  await act(() => result.current.select(first));
  expect(result.current.detail?.dashboard.slug).toBe(first.slug);
  await act(async () => resolveInitial({ ...inventory, source: "hosted", rows: [first, second] }));
  await waitFor(() => expect(firstDetailCalls).toBe(2));
  await act(() => result.current.select(second));
  expect(result.current.detail?.dashboard.slug).toBe(second.slug);

  await act(async () => resolveStaleDetail(firstDetail));
  expect(result.current.detail?.dashboard.slug).toBe(second.slug);
});

it("keeps a confirmed unpublish ahead of an older detail request for the same selection", async () => {
  const publishedDashboard = {
    slug: "same-selection",
    title: "Same selection",
    state: "published",
    visibility: "private",
    current_version: 1,
    pushed_at: null,
    checked_at: null,
    share_count: 1,
    shares: [{ email: "viewer@example.com", invite: "sent", sent_at: null }],
    live_queries: [],
    live_streams: [],
    live_control: null,
    live_actions: null,
  } as unknown as DashboardDetail["dashboard"];
  const unpublishedDashboard = {
    ...publishedDashboard,
    state: "unpublished",
    current_version: null,
    share_count: 0,
    shares: [],
  } as unknown as DashboardDetail["dashboard"];
  const row = {
    slug: publishedDashboard.slug,
    title: publishedDashboard.title,
    folder: "/tmp/same-selection",
    location: "local",
    localState: "published",
    hosted: null,
    pushedAt: null,
    checkedAt: null,
    error: null,
    differentKey: false,
    team: null,
  } as InventoryRow;
  const publishedDetail = { dashboard: publishedDashboard, capabilities: [] } as DashboardDetail;
  const unpublishedDetail = {
    dashboard: unpublishedDashboard,
    capabilities: [],
  } as DashboardDetail;
  let published = true;
  let hostedCalls = 0;
  let detailCalls = 0;
  let resolveInitial!: (value: DashboardInventory) => void;
  let resolveOldDetail!: (value: DashboardDetail) => void;
  const currentInventory = (localOnly: boolean): DashboardInventory => ({
    ...inventory,
    source: localOnly ? "local" : "hosted",
    rows: [
      {
        ...row,
        location: localOnly ? "local" : "both",
        localState: published ? "published" : "unpublished",
        hosted: localOnly ? null : published ? publishedDashboard : unpublishedDashboard,
      },
    ],
  });
  vi.mocked(api.dashboardInventory).mockImplementation((_folder, localOnly) => {
    if (localOnly) return Promise.resolve(currentInventory(true));
    if (++hostedCalls === 1)
      return new Promise((resolve) => {
        resolveInitial = resolve;
      });
    return Promise.resolve(currentInventory(false));
  });
  vi.mocked(api.dashboardDetail).mockImplementation(() => {
    if (++detailCalls === 2)
      return new Promise((resolve) => {
        resolveOldDetail = resolve;
      });
    return Promise.resolve(published ? publishedDetail : unpublishedDetail);
  });
  vi.mocked(api.dashboardUnpublish).mockImplementation(async () => {
    published = false;
    return {
      slug: row.slug,
      hostedCopy: "removed",
      hostedCopyError: null,
      viewers: [],
      shares: [],
    };
  });
  const { result } = renderHook(useDashboardManager);
  await waitFor(() => expect(resolveInitial).toBeTypeOf("function"));
  await act(() => result.current.select(row));
  expect(result.current.detail?.dashboard.state).toBe("published");
  await act(async () => resolveInitial(currentInventory(false)));
  await waitFor(() => expect(detailCalls).toBe(2));

  await act(() => result.current.unpublish(row));
  expect(result.current.message).toBe("Unpublished. The link no longer works.");
  expect(result.current.inventory?.rows[0].hosted?.state).toBe("unpublished");
  expect(result.current.detail?.dashboard.state).toBe("unpublished");

  await act(async () => resolveOldDetail(publishedDetail));
  expect(result.current.inventory?.rows[0].hosted?.state).toBe("unpublished");
  expect(result.current.detail?.dashboard.state).toBe("unpublished");
});

it("re-reads access after a lost removal response and confirms removal from the refreshed list", async () => {
  const viewer = { email: "ada@example.com", invite: "drafted" as const, sent_at: null };
  const dashboard = { slug: "metrics", state: "published", shares: [viewer] } as never;
  vi.mocked(api.dashboardDetail).mockResolvedValue({ dashboard, capabilities: [] });
  const { result } = renderHook(useDashboardManager);
  await waitFor(() => expect(result.current.busy).toBe(null));
  await act(() => result.current.select({ slug: "metrics", hosted: dashboard } as never));
  vi.mocked(api.dashboardShare).mockRejectedValue(new Error("No response"));
  vi.mocked(api.dashboardDetail).mockResolvedValue({
    dashboard: { slug: "metrics", state: "published", shares: [] } as never,
    capabilities: [],
  });
  await act(() => result.current.share({ slug: "metrics" } as never, [], [viewer.email]));
  expect(api.dashboardInventory).toHaveBeenCalledTimes(3);
  expect(result.current.detail?.dashboard.shares).toEqual([]);
  expect(result.current.message).toContain("confirmed by refreshed list");
  expect(result.current.error).not.toContain("still have access");
});

it("leaves removal unconfirmed when the response and refreshed reads are both unavailable", async () => {
  const dashboard = {
    slug: "metrics",
    state: "published",
    shares: [{ email: "ada@example.com", invite: null, sent_at: null }],
  } as never;
  vi.mocked(api.dashboardDetail).mockResolvedValue({ dashboard, capabilities: [] });
  const { result } = renderHook(useDashboardManager);
  await waitFor(() => expect(result.current.busy).toBe(null));
  await act(() => result.current.select({ slug: "metrics", hosted: dashboard } as never));
  vi.mocked(api.dashboardShare).mockRejectedValue(new Error("No response"));
  vi.mocked(api.dashboardInventory).mockRejectedValue(new Error("Offline"));
  vi.mocked(api.dashboardDetail).mockRejectedValue(new Error("Offline"));
  await act(() => result.current.share({ slug: "metrics" } as never, [], ["ada@example.com"]));
  expect(result.current.message).toBe("");
  expect(result.current.error).toContain("Couldn't confirm viewer removal");
  expect(result.current.error).not.toContain("still have access");
});
