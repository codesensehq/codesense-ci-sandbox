// SPDX-License-Identifier: MIT
import { act, cleanup, render, screen, waitFor, fireEvent } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { LiveHub, type HubDelta, type HubSnapshot } from "./live-hub";
const api = vi.hoisted(() => ({
  prepare: vi.fn(),
  save: vi.fn(async (_raw: string, _patch?: boolean) => {}),
  mirror: vi.fn(async (_state: unknown) => {}),
}));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  configurationPrepare: api.prepare,
  saveAppState: api.save,
  pushViewerState: api.mirror,
  detectTools: vi.fn(async () => ({ editors: [], terminals: [] })),
}));
const boot = vi.hoisted(() => ({ hub: undefined as unknown, principal: null as string | null }));
vi.mock("@/shared/lib/get-current-user", async (original) => {
  const actual = await original<typeof import("@/shared/lib/get-current-user")>();
  return {
    ...actual,
    useCurrentUser: () =>
      boot.principal
        ? ({ principal: boot.principal } as ReturnType<typeof actual.getCurrentUser>)
        : null,
  };
});
vi.mock("./live-hub", async (original) => {
  const actual = await original<typeof import("./live-hub")>();
  return {
    ...actual,
    liveHub: new Proxy({}, { get: (_target, key) => (boot.hub as never)[key] }),
  };
});
vi.mock("@/shared/lib/local-profile-session", async (original) => {
  const actual = await original<typeof import("@/shared/lib/local-profile-session")>();
  return { ...actual, rendererPrincipal: async () => boot.principal ?? actual.rendererPrincipal() };
});
vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn(async () => "test") }));
vi.mock("next-themes", () => ({ useTheme: () => ({ theme: "system", setTheme: vi.fn() }) }));
vi.mock("@/features/shell/right-pane/panel-menu", () => ({ rebuildNativeMenu: vi.fn() }));
import {
  connectPrefs,
  ensurePrefsConnection,
  readPrefsState,
  resetPrefsConnection,
  usePrefsStore,
} from "./prefs-client";
import { bindPrefsConsumers, usePrefsLive } from "./prefs-live";
import { useViewerStore, resetViewerPrefsPersistence } from "@/shared/stores/viewer-store";
import { useEffectiveShortcut, useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { DashboardsSection } from "@/features/settings/components/dashboards-section";
import { useOrgStore } from "@/shared/stores/org-store";
import { serializeAppStateWrite } from "@/shared/lib/app-state-write";
import { flushShortcutsForProfileSwitch } from "@/shared/stores/shortcuts-store";
import { flushViewerForProfileSwitch } from "@/shared/stores/viewer-store";
import { GeneralSection } from "@/features/settings/components/general-section";

type RecordedEvent = {
  event: string;
  payload: HubDelta | { subscriptionId: string; baseRev: number; delta: HubDelta };
};
// Produced AND compared by the native test driving the managed writer and registered ingress.
const trace = JSON.parse(readFileSync(resolve(__dirname, "prefs-writer-trace.json"), "utf8")) as {
  initial: HubSnapshot[];
  deltas: RecordedEvent[];
  final: HubSnapshot[];
  other: HubSnapshot;
  gap: { view: HubSnapshot; events: RecordedEvent[] };
};
let hub: LiveHub;
let views: Map<string, HubSnapshot>;
let listeners: Map<string, (event: { payload: unknown }) => void>;
let subscriptions: Map<string, string>;
type PrefsRead = (
  topic: string,
  key: string,
  since?: number,
  options?: { subscriptionId: string },
) => Promise<HubSnapshot>;
let read: ReturnType<typeof vi.fn<PrefsRead>>;
let connection: ReturnType<typeof connectPrefs>;
let stop: () => void;
function Consumer() {
  const chord = useEffectiveShortcut("settings.open");
  const active = useViewerStore((state) => state.activeWorkspace["acme:dev"] ?? "none");
  return (
    <>
      <GeneralSection query="line wrap" />
      <DashboardsSection />
      <output aria-label="effective chord">{chord}</output>
      <output aria-label="workspace">{active}</output>
    </>
  );
}
function deliver(event: RecordedEvent) {
  const payload = event.payload;
  listeners.get(event.event)?.({
    payload:
      "delta" in payload
        ? { ...payload, subscriptionId: subscriptions.get(payload.delta.viewKey) }
        : payload,
  });
}
function advance(events = trace.deltas) {
  for (const event of events) {
    if (event.event === "codesense:delta") {
      const delta = event.payload as HubDelta;
      const before = views.get(delta.viewKey)!;
      const rows = new Map(before.snapshot.map((row) => [row.id, row]));
      for (const id of delta.removes) rows.delete(id);
      for (const row of delta.upserts) rows.set(row.id, row);
      views.set(delta.viewKey, { ...before, rev: delta.rev, snapshot: [...rows.values()] });
    }
    deliver(event);
  }
}
beforeEach(async () => {
  resetViewerPrefsPersistence();
  useViewerStore.setState({
    stateLoaded: true,
    settings: useViewerStore.getInitialState().settings,
    workspaces: {},
    activeWorkspace: {},
  });
  useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
  usePrefsStore.setState({
    principal: null,
    global: {},
    owned: {},
    globalReady: false,
    ownedReady: false,
  });
  views = new Map(trace.initial.map((view) => [view.viewKey, structuredClone(view)]));
  listeners = new Map();
  subscriptions = new Map();
  read = vi.fn(
    async (_topic: string, key: string, _since?: number, options?: { subscriptionId: string }) => {
      if (options) subscriptions.set(key, options.subscriptionId);
      return structuredClone(views.get(key)!);
    },
  );
  hub = new LiveHub({
    read,
    listen: async (event, listener) => {
      listeners.set(event, listener);
      return () => {};
    },
    unsubscribe: async () => {},
    readUi: async () => ({}),
    writeUi: async () => ({}),
  });
  api.prepare.mockReset().mockImplementation(async (_topic, principal, _org, scope) => {
    const key = scope === "global" ? "prefs:global" : `prefs:${principal}:native`;
    return structuredClone(views.get(key)!);
  });
  boot.hub = hub;
  boot.principal = null;
  stop = bindPrefsConsumers();
  connection = connectPrefs("42", hub);
  await connection.live;
});
afterEach(() => {
  resetPrefsConnection();
  connection.dispose();
  stop();
  hub.dispose();
  cleanup();
});
it("streams real managed writes and outside file ingress into mounted settings, policy, workspace and effective chord consumers", async () => {
  render(<Consumer />);
  expect(screen.getByTestId("settings-editor-line-wrap-off")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  expect(screen.getByRole("combobox", { name: "Dashboard action policy" })).toHaveValue("open");
  await act(async () => advance());
  await waitFor(() =>
    expect(screen.getByTestId("settings-editor-line-wrap-on")).toHaveAttribute(
      "aria-checked",
      "true",
    ),
  );
  expect(screen.getByRole("combobox", { name: "Dashboard action policy" })).toHaveValue(
    "team-safe",
  );
  expect(screen.getByLabelText("effective chord")).toHaveTextContent("CmdOrCtrl+Shift+Y");
  expect(screen.getByLabelText("workspace")).toHaveTextContent("second");
  expect(useViewerStore.getState().settings.sidebarWidth).toBe(333);
});
it("reconciles live workspace choices when the current workspace also has runtime agent state", async () => {
  render(<Consumer />);
  const current = useViewerStore.getState().workspaces["acme:dev"];
  useViewerStore.setState({
    workspaces: {
      "acme:dev": current.map((workspace) => ({
        ...workspace,
        statusError: "Transient host detail",
      })),
    },
  });
  await act(async () => advance());
  await waitFor(() =>
    expect(
      useViewerStore.getState().workspaces["acme:dev"].map((workspace) => workspace.id),
    ).toContain("second"),
  );
});
it("does no hidden consumer recompute and catches up from its revision on reveal", async () => {
  render(<Consumer />);
  connection.visible(false);
  await act(async () => advance());
  expect(screen.getByTestId("settings-editor-line-wrap-off")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  expect(screen.getByRole("combobox", { name: "Dashboard action policy" })).toHaveValue("open");
  await act(async () => connection.visible(true));
  await waitFor(() =>
    expect(screen.getByRole("combobox", { name: "Dashboard action policy" })).toHaveValue(
      "team-safe",
    ),
  );
  expect(screen.getByLabelText("effective chord")).toHaveTextContent("CmdOrCtrl+Shift+Y");
  expect(
    read.mock.calls.some(
      ([, key, since]) => key === "prefs:global" && since === trace.initial[0].rev,
    ),
  ).toBe(true);
});
it("recovers an actual native retention gap with a snapshot while consumers remain mounted", async () => {
  render(<Consumer />);
  await act(async () => advance());
  views.set(trace.gap.view.viewKey, structuredClone(trace.gap.view));
  await act(async () => trace.gap.events.forEach(deliver));
  await waitFor(() =>
    expect(screen.getByTestId("settings-editor-line-wrap-off")).toHaveAttribute(
      "aria-checked",
      "true",
    ),
  );
  expect(
    read.mock.calls.some(
      ([, key, since]) => key === "prefs:global" && since === trace.final[0].rev,
    ),
  ).toBe(true);
});
it("switches principals without leaking policy or workspace state, retaining global preferences and bindings", async () => {
  render(<Consumer />);
  await act(async () => advance());
  views.set(trace.other.viewKey, structuredClone(trace.other));
  await act(async () => {
    connection.dispose();
    connection = connectPrefs("43", hub);
    await connection.live;
  });
  expect(screen.getByLabelText("workspace")).toHaveTextContent("other");
  expect(screen.getByRole("combobox", { name: "Dashboard action policy" })).toHaveValue("open");
  expect(screen.getByLabelText("effective chord")).toHaveTextContent("CmdOrCtrl+Shift+Y");
  expect(screen.getByTestId("settings-editor-line-wrap-on")).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await act(async () => trace.deltas.forEach(deliver));
  expect(screen.getByLabelText("workspace")).toHaveTextContent("other");
  expect(screen.getByRole("combobox", { name: "Dashboard action policy" })).toHaveValue("open");
  expect(usePrefsStore.getState().owned.viewer).not.toHaveProperty("settings");
  expect(usePrefsStore.getState().owned).not.toHaveProperty("shortcuts");
});
it("retires the read-once paths from live consumers", () => {
  const viewer = readFileSync(resolve(__dirname, "../../../shared/stores/viewer-store.ts"), "utf8");
  expect(viewer.slice(viewer.indexOf("  loadPersistedState: async"))).not.toContain(
    "await loadAppState()",
  );
  const shortcuts = readFileSync(
    resolve(__dirname, "../../../shared/stores/shortcuts-store.ts"),
    "utf8",
  );
  expect(shortcuts.slice(shortcuts.indexOf("  loadPersistedState: async"))).not.toContain(
    "await loadAppState()",
  );
  const dashboards = readFileSync(
    resolve(__dirname, "../../settings/components/dashboards-section.tsx"),
    "utf8",
  );
  expect(dashboards).not.toContain("readDashboardPolicy");
});

it("retains accepted local settings and chords while unrelated native ingress arrives", async () => {
  render(<Consumer />);
  let release!: () => void;
  const barrier = serializeAppStateWrite(
    () =>
      new Promise<void>((resolve) => {
        release = resolve;
      }),
  );
  await waitFor(() => expect(release).toBeDefined());
  try {
    fireEvent.click(screen.getByTestId("settings-editor-line-wrap-auto"));
    act(() => {
      expect(
        useShortcutsStore.getState().setShortcut("settings.open", "CmdOrCtrl+Shift+Z").ok,
      ).toBe(true);
    });
    await act(async () => advance());
    await waitFor(() => expect(useViewerStore.getState().settings.sidebarWidth).toBe(333));
    expect(screen.getByTestId("settings-editor-line-wrap-auto")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(screen.getByLabelText("effective chord")).toHaveTextContent("CmdOrCtrl+Shift+Z");
    expect(useViewerStore.getState().settings.sidebarWidth).toBe(333);
  } finally {
    release();
    await barrier;
    await flushViewerForProfileSwitch();
    await flushShortcutsForProfileSwitch();
  }
  expect(
    api.save.mock.calls.some(
      ([raw, patch]) =>
        patch === true &&
        JSON.parse(raw).shortcuts?.shortcuts?.["settings.open"] === "CmdOrCtrl+Shift+Z",
    ),
  ).toBe(true);
});
it("keeps the native viewer mirror current with live settings and workspace focus", async () => {
  render(<Consumer />);
  api.mirror.mockClear();
  await act(async () => advance());
  await waitFor(() => expect(api.mirror).toHaveBeenCalled());
  const latest = api.mirror.mock.calls.at(-1)![0] as {
    settings: { editorLineWrap: string };
    activeWorkspace: Record<string, string>;
  };
  expect(latest.settings.editorLineWrap).toBe("on");
  expect(latest.activeWorkspace["acme:dev"]).toBe("second");
});

it("boots from the prepared snapshot while the window is hidden, then catches up from its revision on reveal", async () => {
  connection.dispose();
  resetPrefsConnection();
  usePrefsStore.setState({
    principal: null,
    global: {},
    owned: {},
    globalReady: false,
    ownedReady: false,
  });
  useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
  boot.principal = "42";
  const hidden = ensurePrefsConnection("42");
  hidden.visible(false);
  read.mockClear();

  const loaded = await readPrefsState();
  await useShortcutsStore.getState().loadPersistedState();

  expect(loaded.viewer?.settings).toMatchObject({ editorLineWrap: "off" });
  expect(useShortcutsStore.getState().stateLoaded).toBe(true);
  expect(usePrefsStore.getState()).toMatchObject({ globalReady: true, ownedReady: true });
  expect(useViewerStore.getState().settings.editorLineWrap).toBe("off");
  expect(usePrefsStore.getState().owned.viewer).not.toHaveProperty("settings");
  expect(read).not.toHaveBeenCalled();

  await act(async () => advance());
  expect(useViewerStore.getState().settings.editorLineWrap).toBe("off");
  expect(read).not.toHaveBeenCalled();

  await act(async () => hidden.visible(true));
  await waitFor(() => expect(useViewerStore.getState().settings.editorLineWrap).toBe("on"));
  expect(useViewerStore.getState().settings.sidebarWidth).toBe(333);
  expect(
    read.mock.calls.some(
      ([, key, since]) => key === "prefs:global" && since === trace.initial[0].rev,
    ),
  ).toBe(true);
});
it("switches principals while hidden without waiting on frame delivery", async () => {
  connection.dispose();
  resetPrefsConnection();
  views.set(trace.other.viewKey, structuredClone(trace.other));
  const first = connectPrefs("42", hub);
  first.visible(false);
  await first.ready;
  first.dispose();
  const second = connectPrefs("43", hub);
  second.visible(false);
  await second.ready;
  expect(usePrefsStore.getState()).toMatchObject({ principal: "43", ownedReady: true });
  expect(usePrefsStore.getState().owned.viewer).not.toHaveProperty("settings");
  second.dispose();
});

function PrefsBoot() {
  usePrefsLive();
  return null;
}
it("cold-boots the viewer while the document is hidden: stateLoaded flips on the prepared snapshot before any reveal", async () => {
  connection.dispose();
  resetPrefsConnection();
  usePrefsStore.setState({
    principal: null,
    global: {},
    owned: {},
    globalReady: false,
    ownedReady: false,
  });
  useViewerStore.setState({
    stateLoaded: false,
    persistedStateRestored: false,
    settings: useViewerStore.getInitialState().settings,
    workspaces: {},
    activeWorkspace: {},
  });
  useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
  boot.principal = "42";
  // Unrelated readiness the viewer loader waits on: orgs loaded, no active team repos pending.
  useOrgStore.setState({
    orgs: [{ slug: "acme" }] as never,
    workspaceRepos: {},
    activeOrgSlug: null,
  });
  const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
  try {
    expect(useViewerStore.getState().stateLoaded).toBe(false);
    read.mockClear();
    render(<PrefsBoot />);
    await useViewerStore.getState().loadPersistedState();
    await useShortcutsStore.getState().loadPersistedState();

    expect(useViewerStore.getState().stateLoaded).toBe(true);
    expect(useViewerStore.getState().persistedStateRestored).toBe(true);
    expect(useShortcutsStore.getState().stateLoaded).toBe(true);
    expect(useViewerStore.getState().settings.editorLineWrap).toBe("off");
    expect(read).not.toHaveBeenCalled();

    visibility.mockReturnValue("visible");
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await waitFor(() => expect(useViewerStore.getState().settings.editorLineWrap).toBe("off"));
    expect(
      read.mock.calls.some(
        ([, key, since]) => key === "prefs:global" && since === trace.initial[0].rev,
      ),
    ).toBe(true);
  } finally {
    visibility.mockRestore();
    useOrgStore.setState(useOrgStore.getInitialState());
  }
});
it("observes a failed preparation once, leaving no unhandled live rejection", async () => {
  connection.dispose();
  const unhandled: unknown[] = [];
  const listener = (reason: unknown) => unhandled.push(reason);
  process.on("unhandledRejection", listener);
  try {
    api.prepare.mockRejectedValue(new Error("unavailable: profile changed"));
    const failing = connectPrefs("42", hub);
    await expect(failing.ready).rejects.toThrow("profile changed");
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(unhandled).toEqual([]);
    // Callers that await delivery still see the failure.
    await expect(failing.live).rejects.toThrow("profile changed");
  } finally {
    process.off("unhandledRejection", listener);
  }
});
