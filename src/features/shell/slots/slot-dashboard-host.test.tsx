// SPDX-License-Identifier: MIT
import { StrictMode, type ComponentProps } from "react";
import { listBuiltinPanes } from "@/tauri/commands";
import { runRenderedDocumentRequest } from "@/shared/hooks/use-rendered-document-bridge";
import { useOrgStore } from "@/shared/stores/org-store";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { SlotDashboardHost } from "./slot-dashboard-host";
import type { DocBadge } from "@/features/dashboard/components/html-viewer";
import { registerPanel, unregisterPanel } from "../right-pane/panel-registry";
import { usePaneStore } from "../right-pane/panes/pane-store";
import { useNativeSlot } from "./native-slot-context";
import { Inbox } from "lucide-react";
vi.mock("@/features/viewer/components/scratch-note-panel", () => ({
  ScratchNotePanel: () => <div>Native Notes</div>,
}));
import { registerBuiltinPanels } from "../right-pane/builtin-panels";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { logger } from "@/shared/lib/logger";
const state = vi.hoisted(() => ({ app: "{}" }));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  listBuiltinPanes: vi.fn(async () => ({ contentRoot: "/ctx", chain: [], panes: [] })),
  loadAppState: vi.fn(async () => state.app),
  saveAppState: vi.fn(async (next: string) => {
    state.app = next;
  }),
}));
const props = vi.hoisted(() => ({
  current: null as any,
  primary: null as any,
  fallback: null as any,
  mounts: 0,
  unmounts: 0,
}));
vi.mock("@/features/dashboard/lib/verb-policy", () => ({
  useDashboardPolicy: () => ({ preset: "open" }),
}));
vi.mock("@/features/dashboard/components/html-viewer", async () => {
  const { useEffect } = await import("react");
  return {
    HtmlViewer: (p: any) => {
      props.current = p;
      if (p.filePath !== "/builtin/panes/agent-list.html") props.primary = p;
      else props.fallback = p;
      useEffect(() => {
        props.mounts++;
        return () => {
          props.unmounts++;
        };
      }, []);
      return (
        <div
          data-testid="dashboard"
          data-visible={String(p.visible)}
          data-presented={String(p.presented)}
        />
      );
    },
  };
});
const entry = {
  id: "inbox",
  title: "Triage inbox",
  scope: "team" as const,
  team: "dev",
  surface: "human-inbox",
  path: "/ctx/panes/inbox.html",
  panesDir: "/ctx/panes",
  panelId: "pane:acme:dev/inbox",
  modified: "2026-10-21T00:00:00Z",
};
const fallbackEntry = {
  ...entry,
  id: "agent-list",
  title: "Agents",
  path: "/builtin/panes/agent-list.html",
  dashboardRef: { scope: "built-in" as const, org: "acme", id: "agent-list" },
};
const fallbackProps = {
  entry: fallbackEntry,
  contentRoot: "/ctx",
  readyWithinMs: 3_000,
};
afterEach(() => {
  cleanup();
  unregisterPanel("scratch");
  usePaneStore.setState({ listing: null, badges: {} });
  vi.useRealTimers();
});
it("falls back after 30 seconds, keeps the built-in during a re-mint, and returns focus after another failure", async () => {
  vi.useFakeTimers();
  registerPanel({
    id: "scratch",
    title: "Inbox",
    icon: Inbox,
    render: () => <div>Native Inbox</div>,
  });
  render(
    <SlotDashboardHost
      slotId="right-pane"
      entry={{ ...entry, id: "notes", surface: "notes" }}
      contentRoot="/ctx"
      visible
      intent={{ conversation: "#qa" }}
    />,
  );
  expect(screen.queryByText("Native Inbox")).toBeNull();
  expect(props.current.frameTitle).toBe("Triage inbox");
  act(() => vi.advanceTimersByTime(30000));
  expect(screen.getByText("Native Inbox")).toBeTruthy();
  const capture = vi.fn().mockResolvedValue(undefined);
  await runRenderedDocumentRequest(
    { requestId: "fallback", path: entry.path, format: "text" },
    capture,
  );
  expect(capture).toHaveBeenCalledWith(
    "fallback",
    null,
    expect.stringMatching(/Pane is showing fallback:.*ready.*reload/),
  );
  fireEvent.click(screen.getByRole("button", { name: "Details" }));
  expect(screen.getByText("timed out waiting for codesense.ready()")).toBeTruthy();
  const reload = screen.getByRole("button", { name: "Reload" });
  reload.focus();
  fireEvent.click(reload);
  expect(screen.getByRole("button", { name: "Reloading…" }).getAttribute("aria-disabled")).toBe(
    "true",
  );
  expect(screen.getByText("Native Inbox")).toBeTruthy();
  expect(props.mounts).toBe(2);
  expect(props.unmounts).toBe(1);
  act(() => vi.advanceTimersByTime(30000));
  expect(screen.getByRole("button", { name: "Reload" })).toBe(document.activeElement);
  const details = screen.getByRole("button", { name: "Details" });
  details.focus();
  fireEvent.click(details);
  expect(document.activeElement).toBe(details);
  expect(screen.getByText(/pane still didn't load/)).toBeTruthy();
});
it("uses the generic slot fallback on a cold frame error and lets late ready recover", () => {
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      fallback={fallbackProps}
    />,
  );
  act(() => props.primary.onDocProblem({ kind: "load", message: "frame stopped responding" }));
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeTruthy();
  act(() => props.primary.onDocLive());
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeNull();
  expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
  view.unmount();
});
it("retries the selected pane from its generic fallback", () => {
  vi.useFakeTimers();
  render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      fallback={fallbackProps}
    />,
  );
  act(() => props.primary.onDocProblem({ kind: "load", message: "frame stopped responding" }));
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeTruthy();
  const beforeRetry = props.mounts;
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));
  expect(props.mounts).toBe(beforeRetry + 1);
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeTruthy();
  act(() => props.primary.onDocLive());
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeNull();
});
it("shows the generic fallback when a live pane later fails", () => {
  render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      fallback={fallbackProps}
    />,
  );
  act(() => props.primary.onDocLive());
  act(() => props.primary.onDocProblem({ kind: "load", message: "frame stopped responding" }));
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeTruthy();
});
it("pauses the shown-cold readiness deadline while hidden and remembers hidden liveness", () => {
  vi.useFakeTimers();
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible={false}
      fallback={fallbackProps}
    />,
  );
  act(() => vi.advanceTimersByTime(4_000));
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeNull();
  act(() => props.current.onDocLive());
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      fallback={fallbackProps}
    />,
  );
  act(() => vi.advanceTimersByTime(4_000));
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeNull();
});
it("does not replace a live frame when its hidden save candidate fails", () => {
  vi.useFakeTimers();
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={1}
      fallback={fallbackProps}
    />,
  );
  act(() => props.current.onDocLive());
  const shown = screen.getByTestId("dashboard");
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={2}
      fallback={fallbackProps}
    />,
  );
  act(() => vi.advanceTimersByTime(150));
  act(() => vi.advanceTimersByTime(4_000));
  expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
  act(() => props.current.onDocProblem({ kind: "load", message: "candidate failed" }));
  expect(shown.isConnected).toBe(true);
  expect(screen.getByTestId("slot-fallback-strip")).toBeTruthy();
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeNull();
});
it("falls back when a cold pane never reports live, however long its shim handshake took", () => {
  vi.useFakeTimers();
  render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      fallback={fallbackProps}
    />,
  );
  expect(props.primary.onDocReady).toBeUndefined();
  act(() => vi.advanceTimersByTime(3_001));
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeTruthy();
});
it("keeps a primary that went live before the fallback was discovered", () => {
  vi.useFakeTimers();
  const view = render(<SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible />);
  act(() => props.current.onDocLive());
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      fallback={fallbackProps}
    />,
  );
  act(() => vi.advanceTimersByTime(10_000));
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeNull();
  expect(screen.getByTestId("dashboard")).toBeTruthy();
});
it("keeps the absolute deadline for a stalled save candidate and keeps the old frame", () => {
  vi.useFakeTimers();
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={1}
      fallback={fallbackProps}
    />,
  );
  act(() => props.current.onDocLive());
  const shown = screen.getByTestId("dashboard");
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={2}
      fallback={fallbackProps}
    />,
  );
  act(() => vi.advanceTimersByTime(150));
  act(() => vi.advanceTimersByTime(29_000));
  expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
  act(() => vi.advanceTimersByTime(1_500));
  expect(screen.getByTestId("slot-fallback-strip")).toBeTruthy();
  expect(shown.isConnected).toBe(true);
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeNull();
});
it("keeps the absolute deadline for a retry made while the fallback is displayed", () => {
  vi.useFakeTimers();
  render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      fallback={fallbackProps}
    />,
  );
  act(() => props.primary.onDocProblem({ kind: "load", message: "frame stopped responding" }));
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));
  act(() => vi.advanceTimersByTime(150));
  act(() => vi.advanceTimersByTime(30_100));
  expect(screen.getByText(/timed out waiting for codesense.ready/)).toBeTruthy();
});
it("keeps a live frame's minted root until its replacement is live", () => {
  vi.useFakeTimers();
  const view = render(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/a" visible refreshKey={1} />,
  );
  act(() => props.current.onDocLive());
  view.rerender(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/b" visible refreshKey={2} />,
  );
  expect(props.current.teamContentPath).toBe("/a");
  act(() => vi.advanceTimersByTime(150));
  expect(props.current.teamContentPath).toBe("/b");
});
it("goes live on a StrictMode cold mount without falling back", () => {
  vi.useFakeTimers();
  render(
    <StrictMode>
      <SlotDashboardHost
        slotId="dock"
        entry={entry}
        contentRoot="/ctx"
        visible
        fallback={fallbackProps}
      />
    </StrictMode>,
  );
  act(() => props.primary.onDocLive());
  act(() => vi.advanceTimersByTime(10_000));
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeNull();
  expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
  expect(screen.getByText("Triage inbox pane loaded.")).toBeTruthy();
});
it("hands a workspace reveal to the visible generic fallback and reports its delivery", () => {
  vi.useFakeTimers();
  const delivered = vi.fn();
  const intent = { kind: "reveal-workspace", workspace: "w1" };
  render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      intent={intent}
      onIntentDelivered={delivered}
      fallback={fallbackProps}
    />,
  );
  act(() => vi.advanceTimersByTime(3_000));
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).not.toBeNull();
  expect(props.fallback.intent).toEqual(intent);
  act(() => props.fallback.onIntentDelivered());
  act(() => props.fallback.onDocLive());
  expect(delivered).toHaveBeenCalledTimes(1);
});
it("leaves a subscribe-only pane in another slot on the existing live lifecycle", () => {
  vi.useFakeTimers();
  render(<SlotDashboardHost slotId="right-pane" entry={entry} contentRoot="/ctx" visible />);
  act(() => props.current.onDocLive());
  act(() => vi.advanceTimersByTime(4_000));
  expect(screen.getByTestId("dashboard")).toBeTruthy();
  expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
  expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeNull();
});
it("shows rejected and plain dashboards in the same standard strip without mounting rejected code", () => {
  const before = props.mounts;
  render(
    <SlotDashboardHost
      slotId="dock"
      entry={{
        ...entry,
        id: "plain",
        surface: undefined,
        status: "rejected",
        rejectedReason: 'unknown capability "oops"',
      }}
      contentRoot="/ctx"
      visible
    />,
  );
  expect(screen.getByRole("status")).toHaveTextContent(
    "Triage inbox pane didn't load — showing the built-in Agents list.",
  );
  expect(props.mounts).toBe(before);
  fireEvent.click(screen.getByRole("button", { name: "Details" }));
  expect(screen.getByText(/Remove or correct/)).toBeTruthy();
});
it("only clears fallback on live and announces success without remounting the retry", () => {
  vi.useFakeTimers();
  render(<SlotDashboardHost slotId="right-pane" entry={entry} contentRoot="/ctx" visible />);
  act(() => vi.advanceTimersByTime(30000));
  fireEvent.click(screen.getByRole("button", { name: "Reload" }));
  const before = props.mounts;
  act(() => props.current.onDocLive());
  expect(screen.queryByRole("button", { name: "Reload" })).toBeNull();
  expect(screen.getByText("Triage inbox pane loaded.")).toBeTruthy();
  expect(props.mounts).toBe(before);
});

it("uses the cold launch frame when its source identity refreshes before first presentation", async () => {
  vi.useFakeTimers();
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible={false}
      refreshKey={1}
    />,
  );
  const initialMounts = props.mounts;
  const coldReady = props.current.onDocLive;
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible={false}
      refreshKey={2}
    />,
  );

  act(() => coldReady());
  view.rerender(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={2} />,
  );
  await act(async () => vi.advanceTimersByTimeAsync(150));

  expect(props.mounts).toBe(initialMounts);
  expect(screen.getByText("Triage inbox pane loaded.")).toBeTruthy();
  expect(screen.queryByRole("status")).toBeNull();
});

it("retries a corrected cold pane after it failed while hidden", async () => {
  vi.useFakeTimers();
  const correctedEntry = { ...entry, modified: "2026-10-23T13:00:00Z" };
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible={false}
      refreshKey={1}
    />,
  );
  const initialMounts = props.mounts;
  act(() => props.current.onDocProblem({ kind: "script", message: "SyntaxError" }));
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={correctedEntry}
      contentRoot="/ctx"
      visible={false}
      refreshKey={2}
    />,
  );
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={correctedEntry}
      contentRoot="/ctx"
      visible
      refreshKey={2}
    />,
  );
  act(() => vi.advanceTimersByTime(150));
  expect(props.mounts).toBe(initialMounts + 1);
  act(() => props.current.onDocLive());
  expect(screen.queryByRole("button", { name: "Reload" })).toBeNull();
});

it("applies a same-path save received while a cold frame is hidden and still loading", async () => {
  vi.useFakeTimers();
  const savedEntry = { ...entry, modified: "2026-10-23T13:01:00Z" };
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible={false}
      refreshKey={1}
    />,
  );
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible={false}
      refreshKey={2}
    />,
  );
  const mounts = props.mounts;
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={savedEntry}
      contentRoot="/ctx"
      visible={false}
      refreshKey={2}
    />,
  );
  const coldReady = props.current.onDocLive;
  act(() => coldReady());
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={savedEntry}
      contentRoot="/ctx"
      visible
      refreshKey={2}
    />,
  );
  act(() => vi.advanceTimersByTime(150));
  expect(props.mounts).toBe(mounts + 1);
});

it("double-buffers a visible source save before the cold frame first reports live", async () => {
  vi.useFakeTimers();
  const savedEntry = { ...entry, modified: "2026-10-23T13:02:00Z" };
  const view = render(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={1} />,
  );
  const initialMounts = props.mounts;
  const coldReady = props.current.onDocLive;
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={savedEntry}
      contentRoot="/ctx"
      visible
      refreshKey={2}
    />,
  );
  act(() => vi.advanceTimersByTime(150));
  expect(props.mounts).toBe(initialMounts + 1);
  expect(screen.getAllByTestId("dashboard")).toHaveLength(2);
  expect(screen.queryByRole("status")).toBeNull();
  act(() => coldReady());
  expect(screen.queryByText("Triage inbox pane loaded.")).toBeNull();
  act(() => props.current.onDocLive());
  expect(screen.getByText("Triage inbox pane loaded.")).toBeTruthy();
});

it("keeps a pending save scheduled when only the parent callback changes", () => {
  vi.useFakeTimers();
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={1}
      onBadge={() => {}}
    />,
  );
  act(() => props.current.onDocLive());
  const initialMounts = props.mounts;
  const savedEntry = { ...entry, modified: "2026-10-23T13:01:00Z" };
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={savedEntry}
      contentRoot="/ctx"
      visible
      refreshKey={2}
      onBadge={() => {}}
    />,
  );
  act(() => vi.advanceTimersByTime(50));
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={savedEntry}
      contentRoot="/ctx"
      visible
      refreshKey={2}
      onBadge={() => {}}
    />,
  );
  act(() => vi.advanceTimersByTime(200));
  expect(props.mounts, "replacement must mount after the debounce").toBe(initialMounts + 1);
});

it("uses native recovery when a candidate fails over a never-ready cold pane", () => {
  vi.useFakeTimers();
  const native = () => <div>Native Recovery</div>;
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={1}
      renderNative={native}
    />,
  );
  const savedEntry = { ...entry, modified: "2026-10-23T13:02:00Z" };
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={savedEntry}
      contentRoot="/ctx"
      visible
      refreshKey={2}
      renderNative={native}
    />,
  );
  act(() => vi.advanceTimersByTime(150));
  act(() => props.current.onDocProblem({ kind: "script", message: "SyntaxError in new source" }));
  expect(
    screen.queryByText("Native Recovery"),
    "never-ready predecessor must not suppress recovery",
  ).not.toBeNull();
});

it("double-buffers a visible cold refresh when the source mtime is unchanged", () => {
  vi.useFakeTimers();
  const view = render(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={1} />,
  );
  const initialMounts = props.mounts;
  const ready = props.current.onDocLive;
  view.rerender(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={2} />,
  );
  act(() => vi.advanceTimersByTime(150));
  expect(props.mounts).toBe(initialMounts + 1);
  expect(screen.getAllByTestId("dashboard")).toHaveLength(2);
  expect(screen.queryByRole("status")).toBeNull();
  act(() => ready());
  expect(screen.queryByText("Triage inbox pane loaded.")).toBeNull();
  act(() => props.current.onDocLive());
  expect(screen.getByText("Triage inbox pane loaded.")).toBeTruthy();
  expect(screen.queryByRole("status")).toBeNull();
});

it("applies a changed pane source revision before a cold frame is ready when mtime is unavailable", () => {
  vi.useFakeTimers();
  const entryWithoutMtime = { ...entry, modified: undefined };
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entryWithoutMtime}
      contentRoot="/ctx"
      visible
      refreshKey={1}
    />,
  );
  const initialMounts = props.mounts;
  const coldReady = props.current.onDocLive;
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entryWithoutMtime}
      contentRoot="/ctx"
      visible
      refreshKey={2}
    />,
  );
  act(() => vi.advanceTimersByTime(150));

  expect(props.mounts).toBe(initialMounts + 1);
  expect(screen.getAllByTestId("dashboard")).toHaveLength(2);
  expect(screen.queryByRole("status")).toBeNull();
  act(() => coldReady());
  expect(screen.queryByText("Triage inbox pane loaded.")).toBeNull();
  act(() => props.current.onDocLive());
  expect(screen.getByText("Triage inbox pane loaded.")).toBeTruthy();
});

it("a source change after a Reload whose read changed nothing still reloads the page", async () => {
  vi.useFakeTimers();
  const onReload = vi.fn(async () => entry);
  const view = render(
    <SlotDashboardHost
      slotId="right-pane"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={1}
      onReload={onReload}
    />,
  );
  act(() => vi.advanceTimersByTime(30000));
  fireEvent.click(screen.getByRole("button", { name: "Reload" }));
  await act(async () => {});
  expect(onReload).toHaveBeenCalledTimes(1);
  const before = props.mounts;
  const reloadKey = props.current.refreshKey;
  view.rerender(
    <SlotDashboardHost
      slotId="right-pane"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={2}
      onReload={onReload}
    />,
  );
  act(() => vi.advanceTimersByTime(150));
  expect(props.mounts).toBe(before + 1);
  expect(props.current.refreshKey).not.toBe(reloadKey);
});

it.each(["roles", "humanStatus"])(
  "drops an old viewer's delayed %s failure after a source refresh but shows current failures",
  async (verb) => {
    const view = render(
      <SlotDashboardHost
        slotId="right-pane"
        entry={entry}
        contentRoot="/ctx"
        visible
        refreshKey={1}
      />,
    );
    act(() => props.current.onDocLive());
    const oldProblem = props.current.onDocProblem;
    const oldAttempt = props.current.refreshKey;
    const message = `${verb}: denied: frame released`;
    let rejectBridge!: (error: Error) => void;
    const pendingBridge = new Promise<never>((_resolve, reject) => {
      rejectBridge = reject;
    }).catch((error: Error) =>
      oldProblem({ kind: "bridge", message: `${verb}: ${error.message}` }),
    );

    view.rerender(
      <SlotDashboardHost
        slotId="right-pane"
        entry={entry}
        contentRoot="/ctx"
        visible
        refreshKey={2}
      />,
    );
    await act(async () => new Promise((done) => setTimeout(done, 160)));
    expect(props.current.refreshKey).not.toBe(oldAttempt);
    act(() => props.current.onDocLive());
    await act(async () => {
      rejectBridge(new Error("denied: frame released"));
      await pendingBridge;
    });
    expect(screen.queryByTestId("pane-dev-banner")).not.toBeInTheDocument();

    act(() => props.current.onDocProblem({ kind: "bridge", message }));
    expect(screen.getByTestId("pane-dev-banner")).toHaveTextContent(`Call failed: ${message}`);
  },
);

it("keeps one absolute deadline including time spent minting", () => {
  vi.useFakeTimers();
  render(<SlotDashboardHost slotId="right-pane" entry={entry} contentRoot="/ctx" visible />);
  act(() => vi.advanceTimersByTime(4000));
  expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
  act(() => vi.advanceTimersByTime(25999));
  expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
  act(() => vi.advanceTimersByTime(1));
  expect(screen.getByTestId("slot-fallback-strip")).toBeTruthy();
});

it("consumes the original reveal intent when the native fallback writes its selection before Reload", async () => {
  vi.useFakeTimers();
  function NativeSelection() {
    const slot = useNativeSlot();
    return <button onClick={() => slot?.write({ conversation: "#b" })}>Choose B</button>;
  }
  registerPanel({
    id: "scratch",
    title: "Inbox",
    icon: Inbox,
    render: () => <NativeSelection />,
  });
  const delivered = vi.fn();
  const intent = { conversation: "#a" };
  render(
    <SlotDashboardHost
      slotId="right-pane"
      entry={{ ...entry, id: "notes", surface: "notes" }}
      contentRoot="/ctx"
      visible
      intent={intent}
      onIntentDelivered={delivered}
    />,
  );
  await act(async () => {});
  act(() => props.current.onFrameViewKey("all"));
  act(() => vi.advanceTimersByTime(30000));
  fireEvent.click(screen.getByRole("button", { name: "Choose B" }));
  expect(delivered).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole("button", { name: "Reload" }));
  expect(props.current.intent).toBeUndefined();
});

it("a Notes load failure shows the strip and real native selection at the shared note, then retries at its latest pick", async () => {
  registerBuiltinPanels();
  await liveHub.writeUi("ui:right-pane", "all", { noteId: "shared-note" });
  useOrgStore.setState({ activeOrgSlug: "acme" });
  useViewerStore.setState({
    notesLoadedScope: { githubUserId: 0, orgSlug: "acme" },
    scratchNoteLoadStatus: "loaded",
    notes: ["shared-note", "native-pick", "previous"].map((id) => ({
      id,
      title: id,
      pinned: false,
      lastOpenedAt: 1,
    })),
    activeNoteId: "previous",
    setActiveNote: async (id: string) => {
      useViewerStore.setState({ activeNoteId: id });
    },
  });
  render(
    <SlotDashboardHost
      slotId="right-pane"
      entry={{ ...entry, id: "notes", title: "Notes", surface: "notes" }}
      contentRoot="/ctx"
      visible
    />,
  );
  act(() => props.current.onFrameViewKey("all"));
  act(() => props.current.onDocProblem({ kind: "load-error", message: "Forced load failure" }));
  expect(screen.getByTestId("slot-fallback-strip")).toBeInTheDocument();
  expect(screen.getByText("Native Notes")).toBeInTheDocument();
  await waitFor(() => expect(useViewerStore.getState().activeNoteId).toBe("shared-note"));
  act(() => useViewerStore.setState({ activeNoteId: "native-pick" }));
  await waitFor(async () =>
    expect((await liveHub.snapshot("ui:right-pane", "all")).snapshot[0]).toMatchObject({
      noteId: "native-pick",
    }),
  );
  fireEvent.click(screen.getByRole("button", { name: "Reload" }));
  expect(props.current.intent).toBeUndefined();
  act(() => props.current.onDocLive());
  expect(screen.queryByText("Native Notes")).not.toBeInTheDocument();
  liveHub.dispose();
});
it.each(["team", "private"] as const)(
  "a failed %s Inbox mounts the bundled Inbox under the strip and a failed built-in has a plain body",
  async (scope) => {
    vi.useFakeTimers();
    const builtin = {
      ...entry,
      title: "Inbox",
      scope: "private" as const,
      team: undefined,
      panelId: "pane:private:acme/inbox",
      path: "/builtin-panes/panes/inbox.html",
      panesDir: "/builtin-panes/panes",
      dashboardRef: { scope: "built-in" as const, org: "acme", id: "inbox" },
    };
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "dev" });
    vi.mocked(listBuiltinPanes).mockResolvedValue({
      contentRoot: "/ctx",
      chain: [],
      panes: [builtin],
      builtinInbox: builtin,
    });
    render(
      <SlotDashboardHost
        slotId="right-pane"
        entry={{ ...entry, scope, title: "Inbox" }}
        contentRoot="/ctx"
        visible
        intent={{ conversation: "#qa" }}
      />,
    );
    await act(async () =>
      props.current.onDocProblem({ kind: "script", message: "broken team copy" }),
    );
    expect(screen.getByRole("status")).toHaveTextContent(
      "Inbox pane didn't load — showing the built-in Inbox.",
    );
    expect(props.current.filePath).toBe(builtin.path);
    expect(props.current.pane.dashboardRef.scope).toBe("built-in");
    expect(props.current.intent).toEqual({ conversation: "#qa" });
    act(() => props.current.onDocProblem({ kind: "script", message: "broken bundled copy" }));
    expect(screen.getByText("Your messages are safe.")).toBeTruthy();
    expect(
      screen.getByText(
        "New mail still arrives. To read an agent's mail now, open its session's inbox.",
      ),
    ).toBeTruthy();
  },
);

it("keeps dock retry activity while its native fallback is on screen", () => {
  vi.useFakeTimers();
  render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      renderNative={() => <div>Agents list</div>}
    />,
  );
  act(() => vi.advanceTimersByTime(30000));
  expect(screen.getByText("Agents list")).toBeTruthy();
  expect(props.current.visible).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Reload" }));
  expect(props.current.pane.slotId).toBe("dock");
  expect(props.current.visible).toBe(true);
  act(() => props.current.onDocLive());
  expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
});

it("keeps cold startup neutral past three seconds and expires at an absolute thirty seconds", () => {
  vi.useFakeTimers();
  render(<SlotDashboardHost slotId="file-nav" entry={entry} contentRoot="/ctx" visible />);
  expect(screen.getByTestId("pane-startup-placeholder")).toBeInTheDocument();
  act(() => vi.advanceTimersByTime(4000));
  expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
  act(() => vi.advanceTimersByTime(25999));
  expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
  act(() => vi.advanceTimersByTime(1));
  expect(screen.getByTestId("slot-fallback-strip")).toBeInTheDocument();
});

it("same-document churn keeps readiness, while a new document starts independently not-ready", () => {
  vi.useFakeTimers();
  render(<SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible />);
  act(() => props.current.onPageStart("frame-a/document-a"));
  act(() => props.current.onDocLive());
  act(() => props.current.onPageStart("frame-a/document-a"));
  act(() => vi.advanceTimersByTime(31000));
  expect(screen.queryByTestId("pane-startup-placeholder")).toBeNull();
  expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
  act(() => props.current.onPageStart("frame-a/document-b"));
  expect(screen.getByTestId("pane-startup-placeholder")).toBeInTheDocument();
  act(() => vi.advanceTimersByTime(30000));
  expect(screen.getByTestId("slot-fallback-strip")).toBeInTheDocument();
});

it("keeps the last live viewer mounted during a source recovery", async () => {
  vi.useFakeTimers();
  const view = render(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={1} />,
  );
  act(() => props.current.onDocLive());
  const previous = screen.getByTestId("dashboard");
  view.rerender(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={2} />,
  );
  expect(previous.isConnected).toBe(true);
  act(() => vi.advanceTimersByTime(150));
  expect(screen.getAllByTestId("dashboard")).toHaveLength(2);
  expect(screen.getByText("Updating…")).toBeInTheDocument();
  act(() => props.current.onDocLive());
  await act(async () => vi.advanceTimersByTimeAsync(32));
  expect(screen.getAllByTestId("dashboard")).toHaveLength(1);
  expect(previous.isConnected).toBe(false);
});

it("logs each fallback transition, including initial rejection and timeout, with its attempt identity", () => {
  vi.useFakeTimers();
  const log = vi.spyOn(logger, "warn").mockImplementation(() => {});
  log.mockClear();
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={{ ...entry, status: "rejected", rejectedReason: "unavailable capability" }}
      contentRoot="/ctx"
      visible
    />,
  );
  expect(log.mock.calls.filter(([tag]) => tag === "slot-fallback")).toHaveLength(1);
  const rejected = JSON.parse(log.mock.calls.find(([tag]) => tag === "slot-fallback")![1]);
  expect(rejected).toMatchObject({ slot: "dock", ref: entry.path, attempt: 1, reason: "rejected" });
  expect(rejected.elapsedMs).toEqual(expect.any(Number));
  view.unmount();
  log.mockClear();
  render(<SlotDashboardHost slotId="file-nav" entry={entry} contentRoot="/ctx" visible />);
  act(() => vi.advanceTimersByTime(30000));
  expect(log.mock.calls.filter(([tag]) => tag === "slot-fallback")).toHaveLength(1);
  expect(JSON.parse(log.mock.calls.find(([tag]) => tag === "slot-fallback")![1])).toMatchObject({
    slot: "file-nav",
    ref: entry.path,
    attempt: 1,
    reason: "timeout",
  });
  log.mockRestore();
});

it("keeps prior content and its current visibility while async Reload runs inside the absolute deadline", async () => {
  vi.useFakeTimers();
  let resolve!: (next: typeof entry) => void;
  const reload = vi.fn(
    () =>
      new Promise<typeof entry>((done) => {
        resolve = done;
      }),
  );
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={1}
      onReload={reload}
    />,
  );
  act(() => props.current.onDocLive());
  const previous = screen.getByTestId("dashboard");
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={2}
      onReload={reload}
    />,
  );
  expect(previous.isConnected).toBe(true);
  fireEvent.keyDown(screen.getByRole("button", { name: "Triage inbox pane options" }), {
    key: "Enter",
  });
  fireEvent.click(screen.getByRole("menuitem", { name: "Reload pane" }));
  expect(reload).toHaveBeenCalledOnce();
  expect(previous.isConnected).toBe(true);
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible={false}
      refreshKey={2}
      onReload={reload}
    />,
  );
  expect(previous).toHaveAttribute("data-visible", "false");
  act(() => vi.advanceTimersByTime(30000));
  expect(screen.getByTestId("slot-fallback-strip")).toBeInTheDocument();
  await act(async () => resolve(entry));
  expect(previous.isConnected).toBe(true);
  expect(previous.parentElement).not.toHaveAttribute("inert");
});

it("freezes the shown frame immediately, debounces saves and swaps only on the candidate's ready", async () => {
  vi.useFakeTimers();
  const badge = vi.fn();
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={1}
      onBadge={badge}
    />,
  );
  act(() => props.current.onDocLive());
  act(() => props.current.onBadge({ count: 7, severity: "info" }));
  const shown = screen.getByTestId("dashboard");
  const oldReady = props.current.onDocLive;
  const mounts = props.mounts;
  const save = (refreshKey: number) =>
    view.rerender(
      <SlotDashboardHost
        slotId="dock"
        entry={entry}
        contentRoot="/ctx"
        visible
        refreshKey={refreshKey}
        onBadge={badge}
      />,
    );
  save(2);
  expect(shown).toHaveAttribute("data-presented", "true");
  expect(shown.parentElement).toHaveClass("pointer-events-none");
  expect(shown.parentElement).not.toHaveAttribute("inert");
  expect(shown.parentElement).not.toHaveClass("opacity-0");
  act(() => vi.advanceTimersByTime(40));
  save(3);
  act(() => vi.advanceTimersByTime(40));
  save(4);
  act(() => vi.advanceTimersByTime(149));
  expect(props.mounts).toBe(mounts);
  act(() => vi.advanceTimersByTime(1));
  expect(props.mounts).toBe(mounts + 1);
  const candidate = screen.getAllByTestId("dashboard")[1];
  expect(candidate).toHaveAttribute("data-visible", "true");
  expect(candidate).toHaveAttribute("data-presented", "false");
  expect(candidate.parentElement).toHaveAttribute("aria-hidden", "true");
  expect(candidate.parentElement).toHaveAttribute("inert");
  act(() => props.current.onBadge({ count: 9, severity: "info" }));
  expect(badge).toHaveBeenLastCalledWith({ count: 7, severity: "info" });
  act(() => oldReady());
  expect(shown).toHaveAttribute("data-presented", "true");
  act(() => props.current.onDocLive());
  expect(candidate).toHaveAttribute("data-presented", "true");
  expect(badge).toHaveBeenLastCalledWith({ count: 9, severity: "info" });
  await act(async () => vi.advanceTimersByTimeAsync(32));
  expect(shown.isConnected).toBe(false);
});

it.each(["script", "timeout"])(
  "a failed %s candidate keeps the shown pane under the strip",
  (reason) => {
    vi.useFakeTimers();
    const view = render(
      <SlotDashboardHost
        slotId="dock"
        entry={entry}
        contentRoot="/ctx"
        visible
        refreshKey={1}
        renderNative={() => <div>Native replacement</div>}
      />,
    );
    act(() => props.current.onDocLive());
    const shown = screen.getByTestId("dashboard");
    view.rerender(
      <SlotDashboardHost
        slotId="dock"
        entry={entry}
        contentRoot="/ctx"
        visible
        refreshKey={2}
        renderNative={() => <div>Native replacement</div>}
      />,
    );
    act(() => vi.advanceTimersByTime(150));
    if (reason === "script")
      act(() => props.current.onDocProblem({ kind: "script", message: "SyntaxError" }));
    else {
      act(() => vi.advanceTimersByTime(1000));
      expect(screen.getByText("Updating…")).toBeInTheDocument();
      act(() => vi.advanceTimersByTime(28849));
      expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
      act(() => vi.advanceTimersByTime(1));
    }
    expect(shown.isConnected).toBe(true);
    expect(shown).toHaveAttribute("data-presented", "true");
    expect(shown.parentElement).not.toHaveClass("opacity-0");
    expect(screen.getAllByTestId("dashboard")).toHaveLength(1);
    expect(screen.getByTestId("slot-fallback-strip")).toHaveTextContent(
      "keeping the previous version",
    );
    expect(screen.queryByText("Native replacement")).toBeNull();
  },
);

it("discards a superseded candidate and ignores its delayed ready", async () => {
  vi.useFakeTimers();
  const view = render(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={1} />,
  );
  act(() => props.current.onDocLive());
  const shown = screen.getByTestId("dashboard");
  const save = (refreshKey: number) =>
    view.rerender(
      <SlotDashboardHost
        slotId="dock"
        entry={entry}
        contentRoot="/ctx"
        visible
        refreshKey={refreshKey}
      />,
    );
  save(2);
  act(() => vi.advanceTimersByTime(150));
  const staleReady = props.current.onDocLive;
  const stale = screen.getAllByTestId("dashboard")[1];
  save(3);
  expect(stale.isConnected).toBe(false);
  act(() => staleReady());
  expect(shown).toHaveAttribute("data-presented", "true");
  act(() => vi.advanceTimersByTime(150));
  expect(screen.getAllByTestId("dashboard")).toHaveLength(2);
  act(() => props.current.onDocLive());
  await act(async () => vi.advanceTimersByTimeAsync(32));
  expect(shown.isConnected).toBe(false);
});

it("unfreezes if hidden during debounce, defers the candidate until shown, and cleans up mid-freeze", () => {
  vi.useFakeTimers();
  const view = render(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={1} />,
  );
  act(() => props.current.onDocLive());
  const shown = screen.getByTestId("dashboard");
  const mounts = props.mounts;
  view.rerender(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={2} />,
  );
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible={false}
      refreshKey={2}
    />,
  );
  act(() => vi.advanceTimersByTime(200));
  expect(props.mounts).toBe(mounts);
  expect(shown.parentElement).not.toHaveAttribute("inert");
  view.rerender(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={2} />,
  );
  act(() => vi.advanceTimersByTime(150));
  expect(props.mounts).toBe(mounts + 1);
  view.unmount();
  act(() => vi.advanceTimersByTime(31000));
  expect(screen.queryByTestId("dashboard")).toBeNull();
});

it("prepares a candidate intent without consuming it before swap, and ignores a discarded candidate's delivery", () => {
  vi.useFakeTimers();
  const delivered = vi.fn();
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={1}
      onIntentDelivered={delivered}
    />,
  );
  act(() => props.current.onDocLive());
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={2}
      intent={{ conversation: "#a" }}
      onIntentDelivered={delivered}
    />,
  );
  act(() => vi.advanceTimersByTime(150));
  expect(props.current.prepareIntent).toBe(true);
  const candidateDelivered = props.current.onIntentDelivered;
  act(() => candidateDelivered());
  expect(delivered).not.toHaveBeenCalled();
  act(() => props.current.onDocLive());
  expect(delivered).toHaveBeenCalledTimes(1);
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={3}
      onIntentDelivered={delivered}
    />,
  );
  act(() => vi.advanceTimersByTime(150));
  const discardedDelivery = props.current.onIntentDelivered;
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={4}
      onIntentDelivered={delivered}
    />,
  );
  act(() => discardedDelivery());
  expect(delivered).toHaveBeenCalledTimes(1);
});

it("routes a shown pane's manual menu Reload through the buffer immediately", () => {
  vi.useFakeTimers();
  render(<SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible />);
  act(() => props.current.onDocLive());
  const shown = screen.getByTestId("dashboard");
  const mounts = props.mounts;
  expect(props.current.onReloadPane).toEqual(expect.any(Function));
  act(() => props.current.onReloadPane());
  expect(props.mounts).toBe(mounts + 1);
  expect(shown.isConnected).toBe(true);
  expect(shown.parentElement).toHaveClass("pointer-events-none");
  expect(shown.parentElement).not.toHaveAttribute("inert");
});

it("keeps a cold document's reveal intent pending until its own readiness", () => {
  const delivered = vi.fn();
  render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      intent={{ conversation: "#a" }}
      onIntentDelivered={delivered}
    />,
  );
  act(() => props.current.onIntentDelivered());
  expect(delivered).not.toHaveBeenCalled();
  expect(props.current.intent).toEqual({ conversation: "#a" });
  act(() => props.current.onDocLive());
  expect(delivered).toHaveBeenCalledTimes(1);
});

it("a corrected save gets its own absolute deadline while the previous version stays shown", () => {
  vi.useFakeTimers();
  const view = render(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={1} />,
  );
  act(() => props.current.onDocLive());
  view.rerender(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={2} />,
  );
  act(() => vi.advanceTimersByTime(29000));
  view.rerender(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={3} />,
  );
  act(() => vi.advanceTimersByTime(150));
  act(() => props.current.onPageStart("corrected-document"));
  act(() => vi.advanceTimersByTime(29849));
  expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
  act(() => vi.advanceTimersByTime(1));
  expect(screen.getByTestId("slot-fallback-strip")).toBeInTheDocument();
});

it("a rejected saved declaration keeps the working frame and never mounts rejected code", () => {
  const view = render(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={1} />,
  );
  act(() => props.current.onDocLive());
  const shown = screen.getByTestId("dashboard");
  const mounts = props.mounts;
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={{ ...entry, status: "rejected", rejectedReason: "unknown capability" }}
      contentRoot="/ctx"
      visible
      refreshKey={2}
    />,
  );
  expect(shown.isConnected).toBe(true);
  expect(shown).toHaveAttribute("data-presented", "true");
  expect(screen.getByTestId("slot-fallback-strip")).toHaveTextContent(
    "keeping the previous version",
  );
  expect(props.mounts).toBe(mounts);
});

it("acknowledges a new retained-pane intent using its current store identity after a failed save", () => {
  vi.useFakeTimers();
  const original = { conversation: "#first" };
  const newer = { conversation: "#second" };
  const store = usePaneStore.getState();
  store.setIntent(entry.panelId, original);
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={1}
      intent={original}
      onIntentDelivered={() => store.clearIntent(entry.panelId, original)}
    />,
  );
  act(() => props.current.onDocLive());
  act(() => props.current.onIntentDelivered());
  expect(usePaneStore.getState().pendingIntents[entry.panelId]).toBeUndefined();
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={2}
      onIntentDelivered={() => store.clearIntent(entry.panelId, original)}
    />,
  );
  act(() => vi.advanceTimersByTime(150));
  const discarded = props.current.onIntentDelivered;
  act(() => props.current.onDocProblem({ kind: "script", message: "broken replacement" }));
  store.setIntent(entry.panelId, newer);
  view.rerender(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={2}
      intent={newer}
      onIntentDelivered={() => store.clearIntent(entry.panelId, newer)}
    />,
  );
  act(() => discarded());
  expect(usePaneStore.getState().pendingIntents[entry.panelId]).toBe(newer);
  expect(props.current.intent).toBe(newer);
  act(() => props.current.onIntentDelivered());
  expect(usePaneStore.getState().pendingIntents[entry.panelId]).toBeUndefined();
});

it.each(["script", "timeout"])("a failed cold %s attempt cannot leave a shell badge", (failure) => {
  vi.useFakeTimers();
  usePaneStore.getState().setBadge(entry.panelId, null);
  render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      onBadge={(badge) => usePaneStore.getState().setBadge(entry.panelId, badge)}
    />,
  );
  act(() => props.current.onBadge({ count: 7, severity: "info" }));
  expect(usePaneStore.getState().badges[entry.panelId]).toBeUndefined();
  if (failure === "script")
    act(() => props.current.onDocProblem({ kind: "script", message: "initialization failed" }));
  else act(() => vi.advanceTimersByTime(30000));
  expect(screen.getByTestId("slot-fallback-strip")).toBeTruthy();
  expect(usePaneStore.getState().badges[entry.panelId]).toBeUndefined();
});

it("publishes a cached cold badge only when that document becomes live", () => {
  const badge = vi.fn();
  render(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible onBadge={badge} />,
  );
  act(() => props.current.onBadge({ count: 7, severity: "info" }));
  expect(badge).not.toHaveBeenCalled();
  act(() => props.current.onDocLive());
  expect(badge).toHaveBeenLastCalledWith({ count: 7, severity: "info" });
});

it("keeps the working retained badge through a candidate failure and later updates", () => {
  vi.useFakeTimers();
  const badge = vi.fn();
  const body = (refreshKey: number) => (
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={refreshKey}
      onBadge={badge}
    />
  );
  const view = render(body(1));
  act(() => props.current.onDocLive());
  act(() => props.current.onBadge({ count: 7, severity: "info" }));
  view.rerender(body(2));
  act(() => vi.advanceTimersByTime(150));
  act(() => props.current.onBadge({ count: 9, severity: "info" }));
  act(() => props.current.onDocProblem({ kind: "script", message: "broken candidate" }));
  expect(badge).toHaveBeenLastCalledWith({ count: 7, severity: "info" });
  act(() => props.current.onBadge({ count: 8, severity: "info" }));
  expect(badge).toHaveBeenLastCalledWith({ count: 8, severity: "info" });
});

it("keeps the live built-in Inbox badge when an unavailable failure callback changes", async () => {
  const builtin = {
    ...entry,
    title: "Inbox",
    scope: "private" as const,
    team: undefined,
    panelId: "pane:private:acme/inbox",
    path: "/builtin-panes/panes/inbox.html",
    panesDir: "/builtin-panes/panes",
    dashboardRef: { scope: "built-in" as const, org: "acme", id: "inbox" },
  };
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "dev" });
  usePaneStore.getState().setBadge(entry.panelId, null);
  vi.mocked(listBuiltinPanes).mockResolvedValue({
    contentRoot: "/ctx",
    chain: [],
    panes: [builtin],
    builtinInbox: builtin,
  });

  const host = (onBadge: (badge: DocBadge | null) => void) => (
    <SlotDashboardHost
      slotId="right-pane"
      entry={{ ...entry, scope: "team", title: "Inbox" }}
      contentRoot="/ctx"
      visible
      unavailable={{ kind: "connection", technical: "pane list unavailable" }}
      onBadge={(badge) => onBadge(badge)}
    />
  );
  const publishBadge = (badge: DocBadge | null) =>
    usePaneStore.getState().setBadge(entry.panelId, badge);
  const view = render(host(publishBadge));
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });

  expect(screen.getByTestId("slot-fallback-strip")).toBeTruthy();
  expect(props.current.filePath).toBe(builtin.path);
  act(() => props.current.onDocLive());
  act(() => props.current.onBadge({ count: 7, severity: "info" }));
  expect(usePaneStore.getState().badges[entry.panelId]).toEqual({ count: 7, severity: "info" });

  view.rerender(host((badge) => usePaneStore.getState().setBadge(entry.panelId, badge)));

  expect(usePaneStore.getState().badges[entry.panelId]).toEqual({ count: 7, severity: "info" });
});

// SV-PLG-57: dev builds run every mount effect twice, and the host's live attempt must not
// move ahead of the frame it mounted.
const strictHost = (extra: Partial<ComponentProps<typeof SlotDashboardHost>> = {}) => (
  <StrictMode>
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      refreshKey={1}
      {...extra}
    />
  </StrictMode>
);
const expectNoFallback = () => {
  expect(screen.queryByRole("button", { name: "Reload" })).toBeNull();
  expect(document.querySelector("[data-slot-id]")).not.toHaveAttribute("aria-busy");
};

it("goes live on the cold frame's first attempt under StrictMode with no fallback", () => {
  vi.useFakeTimers();
  const warn = vi.spyOn(logger, "warn");
  warn.mockClear();
  render(strictHost());
  expect(document.querySelector("[data-slot-id]")).toHaveAttribute("aria-busy", "true");
  act(() => props.current.onDocLive());
  expect(screen.getByText("Triage inbox pane loaded.")).toBeTruthy();
  act(() => vi.advanceTimersByTime(30000));
  expectNoFallback();
  expect(warn).not.toHaveBeenCalledWith("slot-fallback", expect.anything());
});

it("accepts the cold frame's live after the boot source refresh under StrictMode", async () => {
  vi.useFakeTimers();
  const view = render(strictHost());
  const coldLive = props.current.onDocLive;
  view.rerender(
    strictHost({ refreshKey: 2, entry: { ...entry, modified: "2026-10-23T13:00:00Z" } }),
  );
  await act(async () => vi.advanceTimersByTimeAsync(150));
  act(() => coldLive());
  act(() => props.current.onDocLive());
  expect(screen.getByText("Triage inbox pane loaded.")).toBeTruthy();
  act(() => vi.advanceTimersByTime(30000));
  expectNoFallback();
});

it("goes live when only the mounted frame reports ready after the boot source refresh under StrictMode", async () => {
  vi.useFakeTimers();
  const view = render(strictHost());
  view.rerender(
    strictHost({ refreshKey: 2, entry: { ...entry, modified: "2026-10-23T13:00:00Z" } }),
  );
  await act(async () => vi.advanceTimersByTimeAsync(150));
  act(() => props.current.onDocLive());
  expect(screen.getByText("Triage inbox pane loaded.")).toBeTruthy();
  act(() => vi.advanceTimersByTime(30000));
  expectNoFallback();
});

it("ignores an unmounted frame's readiness, keeping its pending intent", () => {
  const delivered = vi.fn();
  const error = vi.spyOn(console, "error").mockImplementation(() => {});
  const view = render(
    <SlotDashboardHost
      slotId="dock"
      entry={entry}
      contentRoot="/ctx"
      visible
      intent={{ conversation: "#a" }}
      onIntentDelivered={delivered}
    />,
  );
  const frame = props.current;
  act(() => frame.onIntentDelivered());
  view.unmount();
  act(() => frame.onDocLive());
  act(() => frame.onDocProblem({ kind: "connection", message: "late" }));
  act(() => frame.onPageStart("late-doc"));
  expect(delivered).not.toHaveBeenCalled();
  expect(error).not.toHaveBeenCalled();
  error.mockRestore();
});

it("keeps the save candidate path intact under StrictMode", () => {
  vi.useFakeTimers();
  const view = render(strictHost());
  act(() => props.current.onDocLive());
  const oldLive = props.current.onDocLive;
  view.rerender(strictHost({ refreshKey: 2 }));
  act(() => vi.advanceTimersByTime(150));
  expect(screen.getAllByTestId("dashboard")).toHaveLength(2);
  act(() => oldLive());
  expect(screen.getAllByTestId("dashboard")).toHaveLength(2);
  act(() => props.current.onDocLive());
  act(() => vi.advanceTimersByTime(30000));
  expectNoFallback();
});

it("goes live after an in-app reload and a team switch remount under StrictMode", () => {
  vi.useFakeTimers();
  const view = render(strictHost());
  view.unmount();
  // In-app reload: a fresh host over the same pane.
  const reloaded = render(strictHost());
  act(() => props.current.onDocLive());
  act(() => vi.advanceTimersByTime(30000));
  expectNoFallback();
  reloaded.unmount();
  // Team switch: the incoming team's host.
  render(
    <StrictMode>
      <SlotDashboardHost
        key="other"
        slotId="dock"
        entry={{ ...entry, team: "ops", panelId: "pane:acme:ops/inbox" }}
        contentRoot="/ctx2"
        teamKey="acme/ops"
        visible
      />
    </StrictMode>,
  );
  act(() => props.current.onDocLive());
  act(() => vi.advanceTimersByTime(30000));
  expectNoFallback();
});

it("goes live when the entry churns faster than the source debounce during a visible cold mount", async () => {
  vi.useFakeTimers();
  const view = render(
    <SlotDashboardHost slotId="dock" entry={entry} contentRoot="/ctx" visible refreshKey={1} />,
  );
  for (let i = 0; i < 4; i++) {
    view.rerender(
      <SlotDashboardHost
        slotId="dock"
        entry={{ ...entry, modified: `2026-10-23T13:0${i}:00Z` }}
        contentRoot="/ctx"
        visible
        refreshKey={2 + i}
      />,
    );
    await act(async () => vi.advanceTimersByTimeAsync(60));
  }
  await act(async () => vi.advanceTimersByTimeAsync(300));
  act(() => props.current.onDocLive());
  expect(screen.getByText("Triage inbox pane loaded.")).toBeTruthy();
  act(() => vi.advanceTimersByTime(30000));
  expectNoFallback();
});

it("mounts the pending source candidate while the parent re-renders with a new onBadge every 100ms", async () => {
  vi.useFakeTimers();
  const host = (refreshKey: number) => (
    <SlotDashboardHost
      slotId="right-pane"
      entry={{ ...entry }}
      contentRoot="/ctx"
      visible
      refreshKey={refreshKey}
      onBadge={() => {}}
    />
  );
  const view = render(host(1));
  view.rerender(host(2));
  for (let i = 0; i < 6; i++) {
    await act(async () => vi.advanceTimersByTimeAsync(100));
    view.rerender(host(2));
  }
  await act(async () => vi.advanceTimersByTimeAsync(150));
  expect(screen.getAllByTestId("dashboard")).toHaveLength(2);
  act(() => props.current.onDocLive());
  expect(screen.getByText("Triage inbox pane loaded.")).toBeTruthy();
  act(() => vi.advanceTimersByTime(30000));
  expectNoFallback();
});

const reloadHost = (e: typeof entry, visible: boolean, onReload: () => Promise<typeof entry>) => (
  <div>
    <div data-panel-header-row="" />
    <SlotDashboardHost
      slotId="right-pane"
      entry={e}
      contentRoot="/ctx"
      visible={visible}
      refreshKey={1}
      onReload={onReload}
    />
  </div>
);

it("restores header focus when a Reload that refreshed a failed page's source goes live", async () => {
  vi.useFakeTimers();
  const next = { ...entry, modified: "2026-10-22T00:00:00Z" };
  const onReload = vi.fn(async () => next);
  const view = render(reloadHost(entry, true, onReload));
  act(() => vi.advanceTimersByTime(30000));
  const reload = screen.getByRole("button", { name: "Reload" });
  reload.focus();
  fireEvent.click(reload);
  await act(async () => {});
  // Only the source's modified time changes, as the harness onReload delivers it.
  view.rerender(reloadHost(next, true, onReload));
  act(() => vi.advanceTimersByTime(150));
  act(() => props.current.onDocLive());
  expect(document.querySelector("[data-panel-header-row]")).toBe(document.activeElement);
});

it("does not restore header focus when a Reload is cancelled by hiding before it goes live", async () => {
  vi.useFakeTimers();
  const next = { ...entry, modified: "2026-10-22T00:00:00Z" };
  const onReload = vi.fn(async () => next);
  const view = render(reloadHost(entry, true, onReload));
  act(() => vi.advanceTimersByTime(30000));
  const reload = screen.getByRole("button", { name: "Reload" });
  reload.focus();
  fireEvent.click(reload);
  await act(async () => {});
  view.rerender(reloadHost(next, false, onReload));
  view.rerender(reloadHost(next, true, onReload));
  act(() => vi.advanceTimersByTime(150));
  act(() => props.current.onDocLive());
  expect(document.activeElement).not.toBe(document.querySelector("[data-panel-header-row]"));
});

it.each([
  ["moved outside the slot", true],
  ["still in the slot", false],
])("after a source-refresh Reload fails with focus %s", async (_name, moveOut) => {
  vi.useFakeTimers();
  const next = { ...entry, modified: "2026-10-22T00:00:00Z" };
  const onReload = vi.fn(async () => next);
  const view = render(
    <>
      <input aria-label="terminal" />
      {reloadHost(entry, true, onReload)}
    </>,
  );
  act(() => vi.advanceTimersByTime(30000));
  const reload = screen.getByRole("button", { name: "Reload" });
  reload.focus();
  fireEvent.click(reload);
  await act(async () => {});
  view.rerender(
    <>
      <input aria-label="terminal" />
      {reloadHost(next, true, onReload)}
    </>,
  );
  act(() => vi.advanceTimersByTime(150));
  const terminal = screen.getByLabelText("terminal");
  if (moveOut) terminal.focus();
  act(() => vi.advanceTimersByTime(30000));
  if (moveOut) expect(document.activeElement).toBe(terminal);
  else expect(document.activeElement?.textContent).toBe("Reload");
});
