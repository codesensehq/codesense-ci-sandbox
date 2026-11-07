// SPDX-License-Identifier: MIT
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const hub = vi.hoisted(() => ({
  callback: null as ((value: any) => void) | null,
  write: vi.fn(async () => ({})),
}));
const noteApi = vi.hoisted(() => ({
  list: vi.fn(),
  read: vi.fn(),
  update: vi.fn(async () => null),
}));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  listNotes: noteApi.list,
  readNote: noteApi.read,
  updateNoteMeta: noteApi.update,
}));
vi.mock("@/features/dashboard/lib/live-hub", () => ({
  liveHub: {
    watch: (_topic: string, _key: string, callback: (value: any) => void) => {
      hub.callback = callback;
      return () => {};
    },
    writeUi: hub.write,
  },
}));
vi.mock("@/features/viewer/components/scratch-note-panel", () => ({
  ScratchNotePanel: () => null,
}));
import { usePaneStore } from "../right-pane/panes/pane-store";
import { NativeSlotProvider } from "./native-slot-context";
import { registerBuiltinPanels } from "../right-pane/builtin-panels";
import { getPanel } from "../right-pane/panel-registry";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
const realSelect = useViewerStore.getState().setActiveNote;
const realLoad = useViewerStore.getState().loadNotes;
const metas = (...ids: string[]) =>
  ids.map((id) => ({ id, title: id, pinned: false, lastOpenedAt: 1 }));
beforeEach(() => {
  useOrgStore.setState({ activeOrgSlug: "acme" });
  useViewerStore.setState({
    notesLoadedScope: { githubUserId: 0, orgSlug: "acme" },
    notes: metas("shared", "intent", "previous", "native-pick", "another-page"),
    scratchNoteLoadStatus: "loaded",
  });
});
it.each([false, true])(
  "shares a real local pick that supersedes native restoration (intent=%s)",
  async (intent) => {
    let release!: (value: { id: string; content: string }) => void;
    const pending = new Promise<{ id: string; content: string }>((resolve) => {
      release = resolve;
    });
    noteApi.read.mockImplementation(async (_scope, id) =>
      id === "alpha" ? pending : { id, content: id },
    );
    useViewerStore.setState({
      setActiveNote: realSelect,
      activeNoteId: "previous",
      scratchNoteContent: "previous",
      notes: metas("previous", "alpha", "beta", "gamma"),
    });
    usePaneStore.setState({ pendingIntents: intent ? { scratch: { noteId: "alpha" } } : {} });
    registerBuiltinPanels();
    render(getPanel("scratch")!.render());
    act(() =>
      hub.callback!({ snapshot: [{ id: "ui:right-pane", noteId: intent ? "previous" : "alpha" }] }),
    );
    await waitFor(() =>
      expect(noteApi.read.mock.calls.some((call) => call[1] === "alpha")).toBe(true),
    );
    try {
      await act(async () => {
        await realSelect("beta");
      });
      expect(useViewerStore.getState().activeNoteId).toBe("beta");
      expect(hub.write).not.toHaveBeenCalled();
    } finally {
      await act(async () => {
        release({ id: "alpha", content: "alpha" });
      });
    }
    await waitFor(() =>
      expect(hub.write).toHaveBeenCalledExactlyOnceWith("ui:right-pane", "all", { noteId: "beta" }),
    );
    expect(usePaneStore.getState().pendingIntents.scratch).toBeUndefined();
    await act(async () => {
      await realSelect("gamma");
    });
    await waitFor(() => expect(hub.write).toHaveBeenCalledTimes(2));
    expect(hub.write).toHaveBeenLastCalledWith("ui:right-pane", "all", { noteId: "gamma" });
  },
);
function Fallback() {
  const intent = usePaneStore((s) => s.pendingIntents["pane:notes"]);
  return (
    <NativeSlotProvider
      slotId="right-pane"
      panelId="pane:notes"
      viewKey="all"
      intent={intent}
      onIntentDelivered={() => intent && usePaneStore.getState().clearIntent("pane:notes", intent)}
    >
      {getPanel("scratch")!.render()}
    </NativeSlotProvider>
  );
}
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
it.each([
  { intent: undefined, initial: "shared", fallback: true },
  { intent: { noteId: "intent" }, initial: "intent", fallback: true },
  { intent: undefined, initial: "shared", fallback: false },
  { intent: { noteId: "intent" }, initial: "intent", fallback: false },
])(
  "restores selection and applies intent once, then shares native picks and follows external changes ($initial)",
  async ({ intent, initial, fallback }) => {
    useViewerStore.setState({
      activeNoteId: "previous",
      setActiveNote: async (id: string) => {
        useViewerStore.setState({ activeNoteId: id });
      },
    });
    registerBuiltinPanels();
    const panel = fallback ? "pane:notes" : "scratch";
    usePaneStore.setState({ pendingIntents: intent ? { [panel]: intent } : {} });
    render(fallback ? <Fallback /> : getPanel("scratch")!.render());
    expect(hub.write).not.toHaveBeenCalled();
    act(() => hub.callback!({ snapshot: [{ id: "ui:right-pane", noteId: "shared" }] }));
    await waitFor(() => expect(useViewerStore.getState().activeNoteId).toBe(initial));
    if (intent) {
      await waitFor(() =>
        expect(hub.write).toHaveBeenCalledExactlyOnceWith("ui:right-pane", "all", {
          noteId: "intent",
        }),
      );
      expect(usePaneStore.getState().pendingIntents[panel]).toBeUndefined();
    } else expect(hub.write).not.toHaveBeenCalled();
    hub.write.mockClear();
    act(() => useViewerStore.setState({ activeNoteId: "native-pick" }));
    await waitFor(() =>
      expect(hub.write).toHaveBeenCalledExactlyOnceWith("ui:right-pane", "all", {
        noteId: "native-pick",
      }),
    );
    act(() => hub.callback!({ snapshot: [{ id: "ui:right-pane", noteId: "another-page" }] }));
    await waitFor(() => expect(useViewerStore.getState().activeNoteId).toBe("another-page"));
    expect(hub.write).toHaveBeenCalledTimes(1);
  },
);

it("waits for the active org's actual notes load and ignores shared notes outside that model", async () => {
  let release!: (rows: ReturnType<typeof metas>) => void;
  noteApi.list.mockImplementation(
    () =>
      new Promise((resolve) => {
        release = resolve;
      }),
  );
  noteApi.read.mockImplementation(async (_scope, id) => ({ id, content: `body of ${id}` }));
  useViewerStore.setState({
    setActiveNote: realSelect,
    loadNotes: realLoad,
    notes: metas("a-alpha", "a-beta"),
    activeNoteId: "a-beta",
    scratchNoteContent: "A beta",
    emptyScratchReapDone: true,
  });
  usePaneStore.setState({ pendingIntents: {} });
  useOrgStore.setState({ activeOrgSlug: "other" });
  const loading = realLoad();
  await waitFor(() => expect(noteApi.list).toHaveBeenCalled());
  registerBuiltinPanels();
  render(getPanel("scratch")!.render());
  act(() => hub.callback!({ snapshot: [{ id: "ui:right-pane", noteId: "a-alpha" }] }));
  await act(async () => Promise.resolve());
  expect(noteApi.read).not.toHaveBeenCalled();
  expect(hub.write).not.toHaveBeenCalled();
  await act(async () => {
    release(metas("b-note"));
    await loading;
  });
  expect(useViewerStore.getState()).toMatchObject({
    notesLoadedScope: { orgSlug: "other" },
    activeNoteId: "b-note",
    scratchNoteContent: "body of b-note",
  });
  expect(noteApi.read.mock.calls.map((call) => call[1])).toEqual(["b-note"]);
  act(() => hub.callback!({ snapshot: [{ id: "ui:right-pane", noteId: "new-note" }] }));
  await act(async () => Promise.resolve());
  expect(noteApi.read.mock.calls.map((call) => call[1])).toEqual(["b-note"]);
  act(() => useViewerStore.setState({ notes: metas("b-note", "new-note") }));
  await waitFor(() => expect(useViewerStore.getState().activeNoteId).toBe("new-note"));
  expect(noteApi.read.mock.calls.map((call) => call[1])).toEqual(["b-note", "new-note"]);
});
