// SPDX-License-Identifier: MIT
import { CONTEXT_ATTACHMENTS_REFRESH } from "../lib/context-attachment-events";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";

import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { CODESENSE_DOCS_SLUG, CODESENSE_DOCS_TEAM } from "@/shared/lib/codesense-docs";
import { buildTeamKey } from "@/shared/lib/team-key";
import { useOrgDiscovery } from "./use-org-discovery";

// These bounds mirror the constants in use-org-discovery.ts (kept in sync here
// so the test asserts the actual watchdog windows).
const ORG_DISCOVERY_TIMEOUT_MS = 8_000;
const ORG_LOAD_TIMEOUT_MS = 6_000;

const events = vi.hoisted(() => ({ changed: undefined as (() => Promise<void>) | undefined }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((_name, callback) => {
    events.changed = callback;
    return Promise.resolve(() => {});
  }),
}));
const ensureDocsMock = vi.hoisted(() => vi.fn());
const ensureSampleRepoMock = vi.hoisted(() => vi.fn());
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  getGuestSession: vi.fn(async () => true),
  ensureCodesenseDocs: ensureDocsMock,
  ensureSampleRepo: ensureSampleRepoMock,
}));
const toastError = vi.hoisted(() => vi.fn());
const toastDismiss = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({ toast: { error: toastError, dismiss: toastDismiss } }));
const discoverMock = vi.hoisted(() => vi.fn());
const loadOrgMock = vi.hoisted(() => vi.fn());

vi.mock("@/shared/api/org", () => ({ discoverLocalOrgs: discoverMock }));
vi.mock("@/shared/api/registry", () => ({ loadOrg: loadOrgMock }));
let currentUserValue: { github_user_id: number } | null = { github_user_id: 42 };
vi.mock("@/shared/lib/get-current-user", () => ({
  useCurrentUser: () => currentUserValue,
}));

function resetOrgStore() {
  // The persisted session has been restored (nothing persisted unless a test says so).
  useOrgStore.setState({ orgs: [], activeOrgSlug: null, activeTeamName: null, stateLoaded: true });
  useViewerStore.setState({ stateLoaded: true });
}

describe("useOrgDiscovery — boot-gate watchdog (SW-FX-75)", () => {
  beforeEach(() => {
    useOrgStore.setState({ discoveryComplete: false });
    vi.useFakeTimers();
    vi.clearAllMocks();
    resetOrgStore();
    // Default: the docs repo is unavailable (placement failed), as before SW-OC-16.
    ensureDocsMock.mockRejectedValue(new Error("no bundled docs in this test"));
    // SW-ODC-11: default to no sample repo (same posture as docs — unavailable).
    ensureSampleRepoMock.mockRejectedValue(new Error("no bundled sample repo in this test"));
    currentUserValue = { github_user_id: 42 };
  });

  it("SW-FU-B3: completes with no current user (App routes to Login, not an endless splash)", () => {
    currentUserValue = null;
    const { result } = renderHook(() => useOrgDiscovery());
    expect(result.current.discoveryComplete).toBe(true);
    expect(discoverMock).not.toHaveBeenCalled();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("completes discovery on the happy path", async () => {
    discoverMock.mockResolvedValue([{ slug: "acme", org_root: "/r", remote_url: "u" }]);
    loadOrgMock.mockResolvedValue({ org: { slug: "acme" } });

    const { result } = renderHook(() => useOrgDiscovery());
    expect(result.current.discoveryComplete).toBe(false);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    expect(result.current.discoveryComplete).toBe(true);
    expect(useOrgStore.getState().orgs).toHaveLength(1);
  });

  it("reports descriptor exhaustion and can retry the org without restarting", async () => {
    discoverMock.mockResolvedValue([{ slug: "acme", org_root: "/r", remote_url: null }]);
    loadOrgMock
      .mockResolvedValueOnce({
        org: null,
        file_errors: [{ kind: "TransientReadError", message: "too many open files; retry" }],
      })
      .mockResolvedValue({ org: { slug: "acme", teams: [] } });
    renderHook(() => useOrgDiscovery());
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(toastError).toHaveBeenCalledWith(
      expect.stringContaining("too many open files"),
      expect.objectContaining({
        id: "org-discovery-open-files",
        action: expect.objectContaining({ label: "Retry" }),
      }),
    );
    await act(async () => {
      toastError.mock.calls[0][1].action.onClick();
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(useOrgStore.getState().orgs.map((o) => o.slug)).toEqual(["acme"]);
    expect(toastDismiss).toHaveBeenCalledWith("org-discovery-open-files");
  });

  it("reports discovery EMFILE with a retry action", async () => {
    discoverMock
      .mockRejectedValueOnce("cannot read orgs directory: too many open files")
      .mockResolvedValue([{ slug: "acme", org_root: "/r", remote_url: null }]);
    loadOrgMock.mockResolvedValue({ org: { slug: "acme", teams: [] } });
    renderHook(() => useOrgDiscovery());
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(toastError).toHaveBeenCalledWith(
      expect.stringContaining("too many open files"),
      expect.objectContaining({ action: expect.anything() }),
    );
    await act(async () => {
      toastError.mock.calls[0][1].action.onClick();
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(useOrgStore.getState().orgs).toHaveLength(1);
  });

  it("skips a hung loadOrg and still completes discovery (no splash wedge)", async () => {
    discoverMock.mockResolvedValue([
      { slug: "hangs", org_root: "/r", remote_url: "u" },
      { slug: "ok", org_root: "/r2", remote_url: "u2" },
    ]);
    // First org never settles; second resolves normally.
    loadOrgMock
      .mockImplementationOnce(() => new Promise(() => {}))
      .mockResolvedValueOnce({ org: { slug: "ok" } });

    const { result } = renderHook(() => useOrgDiscovery());
    expect(result.current.discoveryComplete).toBe(false);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(ORG_LOAD_TIMEOUT_MS + 50);
    });

    expect(result.current.discoveryComplete).toBe(true);
    // The healthy org still loaded; the hung one was skipped.
    expect(useOrgStore.getState().orgs.map((o) => o.slug)).toEqual(["ok"]);
  });

  it("proceeds with no orgs when discoverLocalOrgs hangs", async () => {
    loadOrgMock.mockResolvedValue({ org: { slug: "unused" } });
    discoverMock.mockImplementation(() => new Promise(() => {})); // never settles

    const { result } = renderHook(() => useOrgDiscovery());
    expect(result.current.discoveryComplete).toBe(false);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(ORG_DISCOVERY_TIMEOUT_MS + 50);
    });

    expect(result.current.discoveryComplete).toBe(true);
    expect(useOrgStore.getState().orgs).toHaveLength(0);
  });
  it("loads the canonical attached root and refreshes/selects after a local MCP notification", async () => {
    discoverMock.mockResolvedValue([]);
    const { result } = renderHook(() => useOrgDiscovery());
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current.discoveryComplete).toBe(true);
    discoverMock.mockResolvedValue([
      { slug: "local", org_root: "/custom/dirty-checkout", remote_url: null, is_attached: true },
    ]);
    loadOrgMock.mockResolvedValue({
      org: { slug: "local", teams: [], org_root: "/custom/dirty-checkout" },
    });
    await act(async () => {
      await events.changed?.();
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(loadOrgMock).toHaveBeenCalledWith("local", "/custom/dirty-checkout", null, null);
    expect(useOrgStore.getState().activeOrgSlug).toBe("local");
    expect(useOrgStore.getState().orgs[0].is_attached).toBe(true);
  });
  it("refresh discovers a restored checkout without reattachment or restarting", async () => {
    discoverMock.mockResolvedValue([
      { slug: "restored", org_root: "/restored", availability_error: "Missing", is_attached: true },
    ]);
    renderHook(() => useOrgDiscovery());
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(loadOrgMock).not.toHaveBeenCalled();
    discoverMock.mockResolvedValue([
      {
        slug: "restored",
        org_root: "/restored",
        remote_url: null,
        availability_error: null,
        is_attached: true,
      },
    ]);
    loadOrgMock.mockResolvedValue({ org: { slug: "restored", teams: [], org_root: "/restored" } });
    await act(async () => {
      window.dispatchEvent(new Event(CONTEXT_ATTACHMENTS_REFRESH));
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(useOrgStore.getState().activeOrgSlug).toBe("restored");
    expect(loadOrgMock).toHaveBeenCalledWith("restored", "/restored", null, null);
  });
});

describe("useOrgDiscovery — launch landing (SW-OC-16)", () => {
  const DOCS_ROOT = "/home/.codesense/users/guest/codesense-docs";
  const README = `${DOCS_ROOT}/getting-started/content/README.html`;
  const docsSummary = {
    slug: CODESENSE_DOCS_SLUG,
    org_root: DOCS_ROOT,
    remote_url: null,
    is_attached: true,
  };
  const docsOrg = {
    slug: CODESENSE_DOCS_SLUG,
    display_name: "CodeSense Docs",
    org_root: DOCS_ROOT,
    teams: [{ name: CODESENSE_DOCS_TEAM, source_repos: [] }],
  };
  const ownSummary = {
    slug: "local/acme",
    org_root: "/work/acme",
    remote_url: null,
    is_attached: true,
  };
  const ownOrg = {
    slug: "local/acme",
    display_name: "Acme",
    org_root: "/work/acme",
    teams: [{ name: "billing", source_repos: [] }],
  };

  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
    resetOrgStore();
    useViewerStore.setState({
      openedTeams: [],
      activeTab: {},
      openTabs: {},
      layoutMode: null,
      terminalVisible: false,
    });
    currentUserValue = { github_user_id: 42 };
    ensureDocsMock.mockResolvedValue({
      org_slug: CODESENSE_DOCS_SLUG,
      org_root: DOCS_ROOT,
      team: CODESENSE_DOCS_TEAM,
      readme_path: README,
      placement: "placed",
    });
    loadOrgMock.mockImplementation(async (slug: string) => ({
      org: slug === CODESENSE_DOCS_SLUG ? docsOrg : ownOrg,
    }));
  });
  afterEach(() => vi.useRealTimers());

  async function settle() {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
  }

  it("first launch: places the docs repo before discovery and lands in it with the README open", async () => {
    discoverMock.mockImplementation(async () => {
      // The docs repo is placed (and connected) before discovery reads the list.
      expect(ensureDocsMock).toHaveBeenCalledTimes(1);
      return [docsSummary];
    });
    renderHook(() => useOrgDiscovery());
    await settle();
    const org = useOrgStore.getState();
    expect(org.activeOrgSlug).toBe(CODESENSE_DOCS_SLUG);
    expect(org.activeTeamName).toBe(CODESENSE_DOCS_TEAM);
    const teamKey = buildTeamKey(CODESENSE_DOCS_SLUG, CODESENSE_DOCS_TEAM);
    const viewer = useViewerStore.getState();
    expect(viewer.openedTeams).toContain(teamKey);
    expect(viewer.activeTab[teamKey]).toBe(README);
  });

  it("the README landing keeps a full-screen CLI on: it is not the user's open", async () => {
    useViewerStore.setState({ layoutMode: "console", terminalVisible: true });
    discoverMock.mockResolvedValue([docsSummary]);
    renderHook(() => useOrgDiscovery());
    await settle();
    const teamKey = buildTeamKey(CODESENSE_DOCS_SLUG, CODESENSE_DOCS_TEAM);
    expect(useViewerStore.getState().activeTab[teamKey]).toBe(README);
    expect(useViewerStore.getState().layoutMode).toBe("console");
  });

  it("later launch: reopens the last opened context repo, not the docs, and opens no README", async () => {
    useOrgStore.setState({
      activeOrgSlug: "local/acme",
      activeTeamName: "billing",
      lastTeamByOrgSlug: { "local/acme": "billing" },
    });
    discoverMock.mockResolvedValue([docsSummary, ownSummary]);
    renderHook(() => useOrgDiscovery());
    await settle();
    expect(useOrgStore.getState().activeOrgSlug).toBe("local/acme");
    expect(useOrgStore.getState().activeTeamName).toBe("billing");
    expect(
      useViewerStore.getState().activeTab[buildTeamKey(CODESENSE_DOCS_SLUG, CODESENSE_DOCS_TEAM)],
    ).toBeUndefined();
  });

  it("never picks the docs before the session is restored, however long that takes", async () => {
    useOrgStore.setState({ stateLoaded: false });
    discoverMock.mockResolvedValue([docsSummary, ownSummary]);
    const { result } = renderHook(() => useOrgDiscovery());
    await settle();
    // Discovery itself completes (no splash wedge)...
    expect(result.current.discoveryComplete).toBe(true);
    // ...but nothing is chosen (or persisted over the last-opened repo) yet, even long after.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000);
    });
    expect(useOrgStore.getState().activeOrgSlug).toBeNull();
    expect(
      useViewerStore.getState().activeTab[buildTeamKey(CODESENSE_DOCS_SLUG, CODESENSE_DOCS_TEAM)],
    ).toBeUndefined();
    // The restore lands: the last-opened repo wins, the docs README never opened.
    await act(async () => {
      useOrgStore.setState({
        activeOrgSlug: "local/acme",
        activeTeamName: "billing",
        stateLoaded: true,
      });
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(useOrgStore.getState().activeOrgSlug).toBe("local/acme");
    expect(
      useViewerStore.getState().activeTab[buildTeamKey(CODESENSE_DOCS_SLUG, CODESENSE_DOCS_TEAM)],
    ).toBeUndefined();
  });

  it("a restore that proves nothing was opened before is a first launch", async () => {
    useOrgStore.setState({ stateLoaded: false });
    discoverMock.mockResolvedValue([docsSummary]);
    renderHook(() => useOrgDiscovery());
    await settle();
    expect(useOrgStore.getState().activeOrgSlug).toBeNull();
    await act(async () => {
      useOrgStore.setState({ stateLoaded: true });
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(useOrgStore.getState().activeOrgSlug).toBe(CODESENSE_DOCS_SLUG);
    expect(
      useViewerStore.getState().activeTab[buildTeamKey(CODESENSE_DOCS_SLUG, CODESENSE_DOCS_TEAM)],
    ).toBe(README);
  });

  it("a last-opened context repo that is gone falls back to the docs without the first-launch README", async () => {
    useOrgStore.setState({ activeOrgSlug: "local/deleted", activeTeamName: "x" });
    discoverMock.mockResolvedValue([docsSummary]);
    renderHook(() => useOrgDiscovery());
    await settle();
    expect(useOrgStore.getState().activeOrgSlug).toBe(CODESENSE_DOCS_SLUG);
    expect(
      useViewerStore.getState().activeTab[buildTeamKey(CODESENSE_DOCS_SLUG, CODESENSE_DOCS_TEAM)],
    ).toBeUndefined();
  });

  it("a failed docs placement never blocks discovery of the user's own context repos", async () => {
    ensureDocsMock.mockRejectedValue(new Error("resource missing"));
    discoverMock.mockResolvedValue([ownSummary]);
    const { result } = renderHook(() => useOrgDiscovery());
    await settle();
    expect(result.current.discoveryComplete).toBe(true);
    expect(useOrgStore.getState().activeOrgSlug).toBe("local/acme");
  });
});
