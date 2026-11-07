// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, render, screen, waitFor, within } from "@/test-utils";
import userEvent from "@testing-library/user-event";
import fixture from "../../../../tests/fixtures/run-review.json";
import { RunDetailViews } from "./run-detail-views";
import { liveHub, type HubDelta, type HubSnapshot } from "@/features/dashboard/lib/live-hub";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { useOrgStore } from "@/shared/stores/org-store";

// These envelopes are generated and checked by the Rust tests driving the real writers.
const transport = vi.hoisted(() => ({
  events: new Map<string, (event: { payload: unknown }) => void>(),
  values: new Map<string, HubSnapshot>(),
  subscriptions: new Map<string, { topic: string; viewKey: string }>(),
  read: vi.fn(),
  retired: vi.fn(),
}));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<object>()),
  liveHubSubscribe: (...args: unknown[]) => transport.read(...args),
  liveHubUnsubscribe: async (id: string) => {
    transport.subscriptions.delete(id);
  },
  readWeftPendingAssertions: transport.retired,
  readRunEvidence: transport.retired,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, handler: (event: { payload: unknown }) => void) => {
    transport.events.set(name, handler);
    return () => {
      transport.events.delete(name);
    };
  },
}));
const frames = fixture as unknown as (HubSnapshot | HubDelta)[];
const snapshots = () => frames.filter((frame): frame is HubSnapshot => "snapshot" in frame);
const delta = (topic: string, rev: number) =>
  frames.find(
    (frame) => frame.topic === topic && frame.rev === rev && "upserts" in frame,
  ) as HubDelta;
function write(change: HubDelta, deliver = true) {
  const before = transport.values.get(change.topic)!;
  const rows = new Map(before.snapshot.map((row) => [row.id, row]));
  change.removes.forEach((id) => rows.delete(id));
  change.upserts.forEach((row) => rows.set(row.id, row));
  transport.values.set(change.topic, { ...before, rev: change.rev, snapshot: [...rows.values()] });
  if (deliver)
    for (const [subscriptionId, sub] of transport.subscriptions) {
      if (sub.topic === change.topic && sub.viewKey === change.viewKey)
        transport.events.get("codesense:subscription-delta")?.({
          payload: { subscriptionId, baseRev: before.rev, delta: change },
        });
    }
}
const props = {
  runId: "run-1",
  storyId: "SW-A",
  teamKey: "acme/ctx:team",
  output: <div>Output</div>,
};
beforeEach(() => {
  liveHub.dispose();
  transport.events.clear();
  transport.subscriptions.clear();
  transport.values.clear();
  snapshots().forEach((snapshot) => transport.values.set(snapshot.topic, snapshot));
  transport.retired.mockClear();
  transport.read
    .mockReset()
    .mockImplementation(
      async (
        topic: string,
        viewKey: string,
        _since: number | undefined,
        options: { subscriptionId: string },
      ) => {
        transport.subscriptions.set(options.subscriptionId, { topic, viewKey });
        const value = transport.values.get(topic);
        return value?.viewKey === viewKey ? value : { topic, viewKey, rev: 0, snapshot: [] };
      },
    );
  useLocalProfileStore.setState({
    status: {
      active: { principal: "42", displayName: "Tester", machineOwner: null },
      profiles: [],
      privateAuthority: null,
    },
  });
  useOrgStore.setState({ activeOrgSlug: "acme/ctx", activeTeamName: "team" });
});
afterEach(() => liveHub.dispose());

it("activates evidence for an Endorse-only header and follows completion", async () => {
  render(<RunDetailViews {...props} defaultView="endorsement" />);
  await waitFor(() => expect(transport.subscriptions.size).toBe(1));
  await act(async () => write(delta("runEndorsements", 1)));
  await waitFor(() =>
    expect(transport.read.mock.calls.some(([topic]) => topic === "runEvidence")).toBe(true),
  );
  await act(async () => write(delta("runEvidence", 1)));
  await waitFor(() =>
    expect(screen.getByTestId("weft-review-header")).toHaveTextContent("Parser complete"),
  );
});

it("updates mounted Endorse, Evidence, Findings and header from actual writer envelopes", async () => {
  const user = userEvent.setup();
  const { container } = render(
    <>
      <RunDetailViews {...props} defaultView="evidence" />
      <RunDetailViews {...props} runId="run-2" defaultView="output" />
    </>,
  );
  const first = within(container.children[0] as HTMLElement);
  const other = within(container.children[1] as HTMLElement);
  await waitFor(() =>
    expect(transport.read.mock.calls.map(([topic]) => topic)).toContain("runEvidence"),
  );
  await waitFor(() => expect(first.getByTestId("run-evidence-unavailable")).toBeInTheDocument());
  await act(async () => write(delta("runEndorsements", 1)));
  await waitFor(() => expect(first.getByRole("tab", { name: "Endorse (1)" })).toBeInTheDocument());
  await user.click(first.getByTestId("weft-review-send-back"));
  await user.type(first.getByTestId("weft-review-note"), "Keep this draft");
  await user.click(first.getByRole("tab", { name: "Findings" }));
  const findings = first.getByTestId("findings-empty").parentElement;
  await act(async () => write(delta("runEvidence", 1)));
  await waitFor(() =>
    expect(first.getByTestId("findings-row")).toHaveTextContent("Parser complete"),
  );
  expect(first.getByTestId("weft-review-header")).toHaveTextContent("Parser complete");
  expect(first.getByTestId("weft-review-note")).toHaveValue("Keep this draft");
  await user.click(first.getByRole("tab", { name: "Evidence" }));
  expect(first.getByTestId("run-evidence-header")).toBeVisible();
  await user.click(first.getByRole("tab", { name: "Endorse (1)" }));
  expect(first.getByTestId("endorsement-panel")).toHaveTextContent("Ready to review");
  await act(async () => write(delta("runEndorsements", 2)));
  await waitFor(() => expect(first.getByTestId("endorsement-empty")).toBeInTheDocument());
  expect(first.queryByTestId("weft-review-header")).not.toBeInTheDocument();
  expect(other.queryByTestId("weft-review-header")).not.toBeInTheDocument();
  expect(other.queryByRole("tab", { name: /Endorse \(/ })).not.toBeInTheDocument();
  expect(findings?.isConnected).toBe(true);
  expect(transport.retired).not.toHaveBeenCalled();
  expect(
    transport.read.mock.calls.filter(
      ([topic, key]) => topic === "runEndorsements" && key === delta("runEndorsements", 1).viewKey,
    ),
  ).toHaveLength(1);
});

it("hides, changes and reveals without a remount or hidden reads; a revision gap resyncs", async () => {
  const user = userEvent.setup();
  const { rerender } = render(<RunDetailViews {...props} defaultView="findings" visible />);
  await waitFor(() => expect(transport.subscriptions.size).toBe(2));
  await act(async () => {
    write(delta("runEndorsements", 1));
    write(delta("runEvidence", 1));
  });
  await waitFor(() =>
    expect(screen.getByTestId("findings-row")).toHaveTextContent("Parser complete"),
  );
  await user.click(screen.getByTestId("weft-review-send-back"));
  const note = screen.getByTestId("weft-review-note");
  await user.type(note, "Draft survives");
  note.focus();
  const panel = screen.getByTestId("findings-row");
  rerender(<RunDetailViews {...props} defaultView="findings" visible={false} />);
  await waitFor(() => expect(transport.subscriptions.size).toBe(0));
  const reads = transport.read.mock.calls.length;
  await act(async () => write(delta("runEvidence", 2)));
  expect(transport.read).toHaveBeenCalledTimes(reads);
  expect(panel).toHaveTextContent("Parser complete");
  rerender(<RunDetailViews {...props} defaultView="findings" visible />);
  await waitFor(() => expect(panel).toHaveTextContent("Hidden update"));
  expect(screen.getByTestId("findings-row")).toBe(panel);
  expect(screen.getByTestId("weft-review-note")).toBe(note);
  expect(note).toHaveValue("Draft survives");
  expect(note).toHaveFocus();
  // Transport loss leaves the UI at rev 2 while the backend advances. The next real
  // envelope has a mismatched baseRev, so LiveHub must recover from its cached view.
  const current = transport.values.get("runEvidence")!;
  transport.values.set("runEvidence", { ...current, rev: 3 });
  await act(async () => write({ ...delta("runEvidence", 2), rev: 4 }));
  await waitFor(() =>
    expect(
      transport.read.mock.calls.some(([topic, , since]) => topic === "runEvidence" && since === 2),
    ).toBe(true),
  );
  expect(transport.retired).not.toHaveBeenCalled();
});
