// SPDX-License-Identifier: MIT
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { LiveHub, type HubDelta, type HubSnapshot } from "./live-hub";
const api = vi.hoisted(() => ({ prepare: vi.fn() }));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  configurationPrepare: api.prepare,
  loadWorkspaceConfig: vi.fn(async () => ({ repos: {} })),
  saveWorkspaceConfig: vi.fn(async () => {}),
}));
import { connectConfiguration } from "./configuration-live";
import { useDashboardProfileStore } from "./dashboard-profile";
import { useConfigStore, resetWorkspaceConfigView } from "@/shared/stores/config-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useTeamContentContext } from "@/shared/hooks/use-team-content-context";
import { setCodesenseHomeForTests } from "@/shared/lib/codesense-home";
import { RepoSection } from "@/features/settings/components/repo-section";
import type { Org } from "@/shared/types/domain";
import { singleSlotChoice } from "@/features/shell/slots/single-slot-dashboard";

type Event = {
  event: string;
  payload: HubDelta | { subscriptionId: string; baseRev: number; delta: HubDelta };
};
// Generated AND compared by the Rust test driving write_profile_patch, save_patch and scratch ingress.
const trace = JSON.parse(
  readFileSync(resolve(__dirname, "configuration-writer-trace.json"), "utf8"),
) as {
  initial: HubSnapshot[];
  deltas: Event[];
  final: HubSnapshot[];
};
let hub: LiveHub;
let views: Map<string, HubSnapshot>;
let listeners: Map<string, (event: { payload: unknown }) => void>;
let read: ReturnType<
  typeof vi.fn<
    (
      topic: string,
      key: string,
      since?: number,
      options?: { subscriptionId: string },
    ) => Promise<HubSnapshot>
  >
>;
let posted: Record<string, unknown>[];
let subscriptionIds: Map<string, string>;
let connection: ReturnType<typeof connectConfiguration> | undefined;
const viewKey = (topic: string, principal = "42", org = "acme/repo") =>
  topic === "profile" ? `profile:${principal}:${org}` : `workspaceConfig:${principal}:native`;
function Consumer() {
  const retained = useTeamContentContext("acme:dev");
  useDashboardProfileStore((s) => s.profile);
  useDashboardProfileStore((s) => s.org);
  const activeOrg = useOrgStore((s) => s.activeOrgSlug);
  const teamKey = activeOrg === "acme/repo" ? "acme:dev" : "other:dev";
  const convention = useConfigStore((s) => s.teamMainDocPreferences[teamKey]?.mainDoc ?? "");
  return (
    <>
      <output aria-label="slot">{singleSlotChoice("dock")?.id ?? "native"}</output>
      <output aria-label="main doc">{convention}</output>
      <output aria-label="retained convention">
        {retained.kind === "ready" ? retained.source.mainDoc : "unavailable"}
      </output>
      <RepoSection repo={null} query="" teamKey={teamKey} />
    </>
  );
}
function advance(events = trace.deltas, deliver = true) {
  for (const event of events) {
    if (event.event === "codesense:delta") {
      const delta = event.payload as HubDelta;
      const before = views.get(delta.viewKey)!;
      const rows = new Map(before.snapshot.map((row) => [row.id, row]));
      for (const id of delta.removes) rows.delete(id);
      for (const row of delta.upserts) rows.set(row.id, row);
      views.set(delta.viewKey, { ...before, rev: delta.rev, snapshot: [...rows.values()] });
    }
    if (deliver) deliverEvent(event);
  }
}
function deliverEvent(event: Event) {
  const payload = event.payload;
  listeners.get(event.event)?.({
    payload:
      "delta" in payload
        ? { ...payload, subscriptionId: subscriptionIds.get(payload.delta.viewKey) }
        : payload,
  });
}
beforeEach(() => {
  resetWorkspaceConfigView();
  setCodesenseHomeForTests("/settings");
  views = new Map(trace.initial.map((view) => [view.viewKey, structuredClone(view)]));
  listeners = new Map();
  subscriptionIds = new Map();
  read = vi.fn(async (_topic, key, _since, options?: { subscriptionId: string }) => {
    if (options) subscriptionIds.set(key, options.subscriptionId);
    return structuredClone(views.get(key)!);
  });
  hub = new LiveHub({
    read,
    listen: async (event, listener) => {
      listeners.set(event, listener);
      return () => {};
    },
    readUi: async () => ({}),
    writeUi: async () => ({}),
  });
  posted = [];
  const attach = hub.attach.bind(hub);
  vi.spyOn(hub, "attach").mockImplementation((id, key, post, ...options) =>
    attach(
      id,
      key,
      (message) => {
        posted.push({ owner: id, ...message });
        post(message);
      },
      ...options,
    ),
  );
  api.prepare.mockReset().mockImplementation(async (topic, principal, org) => {
    const key = viewKey(topic, principal, org);
    if (!views.has(key)) views.set(key, { topic, viewKey: key, rev: 0, snapshot: [] });
    return structuredClone(views.get(key)!);
  });
  useOrgStore.setState({
    orgs: [
      {
        slug: "acme",
        display_name: "Acme",
        org_root: "/plans",
        created_at: "",
        remote_url: null,
        config_flags: {},
        people: [],
        teams: [{ name: "dev", source_repos: [] }],
      } as Org,
    ],
    discoveryComplete: true,
    activeOrgSlug: "acme/repo",
    activeTeamName: "dev",
    workspaceRepos: {},
  });
});
afterEach(() => {
  cleanup();
  connection?.dispose();
  hub.dispose();
  resetWorkspaceConfigView();
});
async function mount() {
  connection = connectConfiguration("42", "acme/repo", hub);
  await connection.ready;
  useConfigStore.setState({
    teamMainDocPreferences: { "acme:dev": { kind: "ready", mainDoc: "" } },
  });
  render(<Consumer />);
}
it("updates mounted slot selection and team convention from real writer and file-ingress messages", async () => {
  await mount();
  await act(async () => advance());
  await waitFor(() => expect(screen.getByLabelText("slot")).toHaveTextContent("agents"));
  expect(screen.getByLabelText("main doc")).toHaveTextContent("Outside.md");
  expect(screen.getByLabelText("Team main doc")).toHaveValue("Outside.md");
  expect(useDashboardProfileStore.getState().profile?.name).toBe("Newer name");
  expect(useDashboardProfileStore.getState().profile?.chrome.newer).toBe(true);
});
it("keeps a focused main-doc draft when the real writers publish a newer convention", async () => {
  await mount();
  const input = screen.getByLabelText("Team main doc");
  input.focus();
  const { fireEvent } = await import("@testing-library/react");
  fireEvent.change(input, { target: { value: "My draft.md" } });
  await act(async () => advance());
  await waitFor(() => expect(screen.getByLabelText("main doc")).toHaveTextContent("Outside.md"));
  expect(screen.getByLabelText("Team main doc")).toBe(input);
  expect(input).toHaveFocus();
  expect(input).toHaveValue("My draft.md");
});
it("retains mounted team conventions when the same principal rebinds to another org", async () => {
  await mount();
  await act(async () => advance());
  connection!.dispose();
  await act(async () => {
    useOrgStore.setState({ activeOrgSlug: "other/repo" });
    connection = connectConfiguration("42", "other/repo", hub);
    await connection.ready;
  });
  expect(screen.getByLabelText("retained convention")).toHaveTextContent("Outside.md");
  expect(screen.getByLabelText("main doc")).toBeEmptyDOMElement();
});
it("does no consumer work while hidden and catches up on reveal", async () => {
  await mount();
  connection!.visible(false);
  expect(posted.filter((message) => message.type === "codesense:stale")).toHaveLength(2);
  const reads = read.mock.calls.length;
  await act(async () => advance());
  expect(read).toHaveBeenCalledTimes(reads);
  expect(posted.filter((message) => message.type === "codesense:stale")).toHaveLength(2);
  expect(screen.getByLabelText("slot")).toHaveTextContent("native");
  expect(screen.getByLabelText("main doc")).toBeEmptyDOMElement();
  await act(async () => connection!.visible(true));
  await waitFor(() => expect(screen.getByLabelText("slot")).toHaveTextContent("agents"));
  expect(screen.getByLabelText("main doc")).toHaveTextContent("Outside.md");
});
it("recovers a missed revision from the actual hub snapshot before updating mounted consumers", async () => {
  await mount();
  advance(trace.deltas, false);
  const latest = trace.deltas
    .filter((event) => event.event === "codesense:subscription-delta")
    .at(-1)!;
  await act(async () => deliverEvent(latest));
  await waitFor(() => expect(screen.getByLabelText("main doc")).toHaveTextContent("Outside.md"));
  expect(
    read.mock.calls.some(([, key, since]) => key === viewKey("workspaceConfig") && since === 0),
  ).toBe(true);
});
it.each(["principal", "org"])(
  "retires the prior %s view and rejects its late deliveries",
  async (boundary) => {
    await mount();
    await act(async () => advance());
    connection!.dispose();
    await act(async () => {
      useOrgStore.setState({ activeOrgSlug: boundary === "org" ? "other/repo" : "acme/repo" });
      connection = connectConfiguration(
        boundary === "principal" ? "99" : "42",
        boundary === "org" ? "other/repo" : "acme/repo",
        hub,
      );
      await connection.ready;
    });
    await act(async () => {
      for (const event of trace.deltas) deliverEvent(event);
    });
    expect(screen.getByLabelText("slot")).toHaveTextContent("native");
    expect(useDashboardProfileStore.getState().profile).toBeNull();
    expect(screen.getByLabelText("main doc")).toBeEmptyDOMElement();
  },
);

it("uses a distinct subscription owner when returning to a retained org", async () => {
  connection = connectConfiguration("42", "acme/repo", hub);
  await connection.ready;
  const first = read.mock.calls.find(([topic]) => topic === "profile")![3]!.subscriptionId;
  connection.dispose();
  connection = connectConfiguration("42", "other/repo", hub);
  await connection.ready;
  connection.dispose();
  connection = connectConfiguration("42", "acme/repo", hub);
  await connection.ready;
  const latest = read.mock.calls.filter(([topic]) => topic === "profile").at(-1)![3]!
    .subscriptionId;
  expect(latest).not.toBe(first);
});

it("keeps a warm org's profile across a switch away and resolves it at once on return", async () => {
  await mount();
  await act(async () => advance());
  await waitFor(() => expect(useDashboardProfileStore.getState().profile?.name).toBe("Newer name"));
  connection!.dispose();
  const other = connectConfiguration("42", "other/repo", hub);
  // The new org has no profile yet and shows none; the previous org's is kept, not reset.
  expect(useDashboardProfileStore.getState().profile).toBeNull();
  expect(useDashboardProfileStore.getState().warm["acme/repo"]?.name).toBe("Newer name");
  other.dispose();
  const back = connectConfiguration("42", "acme/repo", hub);
  // Synchronously, before the profile topic answers.
  expect(useDashboardProfileStore.getState().profile?.name).toBe("Newer name");
  connection = back;
  await back.ready;
});

it("starts a different principal with no profiles from the previous one", async () => {
  await mount();
  await act(async () => advance());
  connection!.dispose();
  connection = connectConfiguration("99", "other/repo", hub);
  expect(useDashboardProfileStore.getState().warm).toEqual({});
  await connection.ready;
});
