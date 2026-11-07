// SPDX-License-Identifier: MIT
import { recipePane } from "../../../../../tests/support/builtin-pane-recipe";
import { readFileSync } from "node:fs";
import type { Event as DocEvent } from "happy-dom";
import { resolve } from "node:path";
import { act, cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@/shared/lib/open-management-tab", () => ({ openManagementTab: vi.fn() }));
import {
  resetRoutinesStoreForTests,
  useRoutinesStore,
} from "@/features/routines/stores/routines-store";
import { resetAgentsStoreForTests, useAgentsStore } from "@/features/agents/stores/agents-store";
import { showAgent } from "@/features/agents/components/agents-tab-nav";
import { useLaunchSheetStore } from "@/features/agents/lib/launch-sheet";
const { ipc, listeners } = vi.hoisted(() => ({
  ipc: vi.fn(),
  listeners: new Map<string, Set<(event: { payload: unknown }) => void>>(),
}));
vi.mock("@tauri-apps/api/core", async (original) => ({
  ...(await original<typeof import("@tauri-apps/api/core")>()),
  invoke: (command: string, args: unknown) => ipc(command, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, callback: (event: { payload: unknown }) => void) => {
    const set = listeners.get(name) ?? new Set();
    set.add(callback);
    listeners.set(name, set);
    return () => set.delete(callback);
  }),
  emit: vi.fn(async () => {}),
}));
vi.mock("@/features/dashboard/lib/verb-dispatch", async () =>
  (await import("@/test/verb-dispatch-mock")).openVerbDispatchMock(),
);
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));

import { NoteSwitcherMount } from "@/features/viewer/components/note-switcher-mount";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { usePaneStore } from "./pane-store";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import { dashboardRefKey } from "@/features/dashboard/lib/dashboard-ref";
import {
  runDashboardCommand,
  syncDashboardCommands,
} from "@/features/dashboard/lib/dashboard-commands";
import {
  allShortcutActions,
  dashboardCommandActionId,
  findShortcutAction,
  resolveBindings,
  setDynamicShortcutActions,
} from "@/shared/lib/shortcuts/registry";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { listing } from "./pane-seam-helpers";
import { HtmlViewer } from "@/features/dashboard/components/html-viewer";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { flushFrame } from "@/features/dashboard/lib/frame-lifecycle";
import { SLOT_GRANT_CEILINGS } from "@/features/dashboard/lib/verb-registry.generated";
import { createEditor } from "@/features/notes/editor/kit-entry";
import { recordsHandlers } from "@/app/mock/handlers/records";
import type { MockEventBus } from "@/app/mock/mock-events";
import type { HandlerRegistry } from "@/app/mock/mock-router";
import { useOrgStore } from "@/shared/stores/org-store";
import { frameFixture } from "../../../../../tests/support/fixtures/frame";
import { shimSource } from "../../../../../tests/support/page-shim";
import { bootPaneDoc, el, type as typeField, type PaneDoc } from "./pane-seam-helpers";
import type { FrameRecord, MintFrameArgs } from "@/tauri/commands";

const SOURCE = readFileSync(
  resolve("src/features/shell/right-pane/__fixtures__/notes-pane.html"),
  "utf8",
);
const ORG = "acme/api",
  TEAM = "backend",
  PORT = 9322,
  PATH = "/ctx/private/panes/notes.html";
const NOTES_REF = { scope: "private", org: ORG, id: "notes" } as const;
const BASE = "# Alpha\n\nFirst\n\nLast";
let handlers: HandlerRegistry, appState: string, failWrites: boolean, teamSafe: boolean;
let seed: FrameRecord, p: PaneDoc | undefined;
let agentCommits: Map<string, string>;
let sessionRows: Record<string, unknown>[], allowSend: boolean, sendResult: Record<string, unknown>;
let contextQuery: (query: Record<string, unknown>) => unknown;
const docs: PaneDoc[] = [];
const navigate = vi.fn();
const emit = (name: string, payload: unknown) => {
  for (const callback of listeners.get(name) ?? []) callback({ payload });
};
const raw = (operation: string, args: Record<string, unknown>) =>
  handlers.records_call({
    frameId: seed.frameId,
    operation,
    args: { collection: "notes", ...args },
  }) as any;
const calls = (operation: string) =>
  ipc.mock.calls.filter(
    ([command, args]) => command === "records_call" && args.operation === operation,
  );

beforeEach(async () => {
  liveHub.dispose();
  vi.stubGlobal("IntersectionObserver", undefined);
  listeners.clear();
  ipc.mockReset();
  navigate.mockClear();
  resetAgentsStoreForTests();
  resetRoutinesStoreForTests();
  useLaunchSheetStore.getState().close();
  appState = "{}";
  failWrites = false;
  teamSafe = false;
  allowSend = true;
  sessionRows = [];
  sendResult = { status: "submitted", submitted: true, message: null };
  agentCommits = new Map();
  contextQuery = () => ({ rows: [], total: 0 });
  const fallback: HandlerRegistry = {
    mint_frame: (args) => {
      const frame = frameFixture(args.args as MintFrameArgs, PORT);
      frame.frameId = (args.args as MintFrameArgs).documentPath === "seed" ? "seed" : "test";
      frame.grant = {
        ...frame.grant,
        frameId: frame.frameId,
        principal: "42",
        dashboard: dashboardRefKey(NOTES_REF),
        team: { org: ORG, team: TEAM },
        slot: "right-pane",
        scope: teamSafe ? "team" : "open",
      };
      frame.kind = teamSafe ? "team" : "private";
      frame.effectiveGrant = [...SLOT_GRANT_CEILINGS["right-pane"]].filter(
        (cap) =>
          (!teamSafe || !cap.startsWith("records:")) && (allowSend || cap !== "sessions:send"),
      );
      return frame;
    },
    release_frame: () => null,
    live_hub_subscribe: (args) => ({
      topic: args.topic,
      viewKey: args.viewKey,
      rev: 0,
      snapshot: args.topic === "sessions" ? sessionRows : [],
    }),
    live_hub_unsubscribe: () => null,
    live_hub_page: () => null,
    live_hub_window: () => null,
  };
  handlers = recordsHandlers({ emit } as MockEventBus, fallback);
  seed = (await handlers.mint_frame({
    args: { documentPath: "seed", contentRoot: "/ctx", slot: "right-pane" },
  })) as FrameRecord;
  raw("create", { id: "alpha", fields: { title: "Alpha", pinned: true }, body: BASE });
  raw("create", { id: "beta", fields: { title: "Beta" }, body: "## Destination\n\nSecond note" });
  ipc.mockImplementation(async (command, args = {}) => {
    if (command === "records_call" && args.operation === "write" && failWrites)
      throw new Error("Store unavailable");
    if (handlers[command]) {
      const result = await handlers[command](args);
      if (command === "records_call" && args.operation === "history")
        return (result as any[]).map((revision) =>
          agentCommits.has(revision.commit)
            ? { ...revision, actor: { kind: "agent", name: agentCommits.get(revision.commit) } }
            : revision,
        );
      return result;
    }
    if (command === "get_http_api_port") return PORT;
    if (command === "resolve_frame_grant")
      return { ...seed.grant, frameId: "test", scope: teamSafe ? "team" : "open" };
    if (command === "verb_dispatch") return { status: "admit", callId: "call" };
    if (command === "live_hub_view_key") return "all";
    if (command === "load_app_state") return appState;
    if (command === "save_app_state") {
      appState = args.state;
      return null;
    }
    if (command === "bridge_doc_context")
      return { org: ORG, team: TEAM, layer: "private", contentRoot: "/ctx/content" };
    if (command === "query_context") return contextQuery(args.query ?? {});
    if (command === "bridge_resolve_open_target")
      return { path: args.path, org: ORG, team: TEAM, contentRoot: "/ctx/content" };
    if (command === "local_profile_status") return { active: { principal: "42" } };
    if (command === "agents_list")
      return [{ key: "personal/tidy", scope: "personal", name: "Tidy", archived: false }];
    if (command === "agent_read_sessions") return sessionRows;
    if (command === "send_prompt_to_agent") return sendResult;
    return null;
  });
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: true, status: 200, statusText: "" })),
  );
  useOrgStore.setState({ activeOrgSlug: ORG, activeTeamName: TEAM, orgs: [] });
});
afterEach(async () => {
  cleanup();
  for (const doc of docs.splice(0)) await doc.win.happyDOM.close();
  liveHub.dispose();
  vi.unstubAllGlobals();
});

async function open(intent?: Record<string, unknown>, waitReady = true, source = SOURCE) {
  const mount = render(
    <HtmlViewer
      filePath={PATH}
      teamContentPath="/ctx/content"
      onNavigate={navigate}
      pane={{ contentRoot: "/ctx/content", panesDir: "/ctx/private/panes", fileName: "notes.html" }}
      intent={intent}
      parentPanelId={`pane:private:${ORG}/notes`}
    />,
  );
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain("/pane/"));
  const frame = document.querySelector("iframe")!;
  const html = source
    .replace('<script type="module">', "<script>")
    .replace(
      'import { createEditor } from "/kit/v1/editor.js";',
      "const createEditor = window.createEditor;",
    );
  const pane = bootPaneDoc(frame, html, undefined, false, {
    shim: shimSource(),
    beforeScripts: (win) => {
      Object.assign(win, { createEditor, TextEncoder, ResizeObserver });
      Object.defineProperty(win.HTMLElement.prototype, "scrollIntoView", {
        value: () => {},
        configurable: true,
      });
    },
  });
  docs.push(pane);
  p = pane;
  if (!teamSafe && waitReady)
    await waitFor(() =>
      expect((el(pane, "note-title") as HTMLInputElement).value).toBe(
        intent?.noteId === "beta" ? "Beta" : "Alpha",
      ),
    );
  if (!teamSafe && waitReady)
    await waitFor(() =>
      expect(pane.doc.querySelector(".scratch-note-editor")?.getAttribute("contenteditable")).toBe(
        "true",
      ),
    );
  return { pane, frame, mount };
}
async function edit(text: string, waitUnsaved = true) {
  const root = p!.doc.querySelector<HTMLElement>(".scratch-note-editor")!;
  await act(async () => {
    const paragraphs = root.querySelectorAll("p");
    paragraphs[paragraphs.length - 1].textContent = text;
    fireEvent.input(root);
  });
  if (waitUnsaved) await waitFor(() => expect(el(p!, "save-state").textContent).toBe("Unsaved"));
}
async function saved(text: string) {
  await waitFor(() => expect(raw("read", { id: "alpha" }).body).toContain(text), { timeout: 2500 });
}
async function click(id: string) {
  await act(async () => el(p!, id).click());
}
async function deliver(frame: HTMLIFrameElement, data: Record<string, unknown>) {
  await act(async () => frame.contentWindow!.postMessage(data, "*"));
}

function replyFor(command: string) {
  const request = [...p!.sent].reverse().find((message) => message.command === command)!;
  return new Promise<void>((resolve) => {
    const receive = (event: DocEvent) => {
      if (!("data" in event)) return;
      const data = event.data as { type?: string; id?: unknown };
      if (data.type === "codesense:invoke-result" && data.id === request.id) {
        p!.win.removeEventListener("message", receive);
        queueMicrotask(resolve);
      }
    };
    p!.win.addEventListener("message", receive);
  });
}

it("ships one private dashboard and uses the shared kit editor", () => {
  expect(SOURCE).toContain('name="codesense:surface" content="notes"');
  expect(SOURCE).toContain('name="codesense:slots" content="right-pane"');
  expect(SOURCE).toContain(
    "records:read:own records:write:own sessions:read sessions:send context:read",
  );
  expect(SOURCE).toContain('from "/kit/v1/editor.js"');
});
it("creates a note, selects its title, types and autosaves through the real shim and bridge", async () => {
  await open();
  await click("new-note");
  await waitFor(() => expect(calls("create")).toHaveLength(1));
  await waitFor(() => expect(p!.doc.activeElement?.id).toBe("note-title"));
  typeField(p!, "New title", "note-title");
  fireEvent.blur(el(p!, "note-title"));
  await waitFor(() => expect(calls("setFields").length).toBeGreaterThan(0));
  await edit("Written here");
  await waitFor(() => expect(calls("write")).toHaveLength(1), { timeout: 2500 });
  expect(calls("write")[0][1].args).toMatchObject({
    collection: "notes",
    base: "",
    body: "Written here",
  });
  await waitFor(() => expect(el(p!, "save-state").textContent).toBe("Saved"));
});
it("does not write or normalize an unchanged body", async () => {
  const { frame } = await open();
  await flushFrame("test");
  expect(calls("write")).toHaveLength(0);
  expect(raw("read", { id: "alpha" }).body).toBe(BASE);
  expect(frame).toBeDefined();
});
it("merges an MCP write during typing and adopts the merged body", async () => {
  await open();
  await edit("Your last");
  await act(async () => {
    const result = raw("write", {
      id: "alpha",
      base: BASE,
      body: "# Alpha\n\nAgent first\n\nLast",
    });
    agentCommits.set(result.rev, "Kai");
  });
  await saved("Your last");
  expect(raw("read", { id: "alpha" }).body).toContain("Agent first");
  await waitFor(() =>
    expect(p!.doc.querySelector(".scratch-note-editor")?.textContent).toContain("Agent first"),
  );
});
it("flush on close keeps the last keystroke before the host acknowledges", async () => {
  const { frame } = await open();
  await edit("Last keystroke");
  await flushFrame("test");
  expect(raw("read", { id: "alpha" }).body).toContain("Last keystroke");
  expect(p!.sent.some((message) => message.type === "codesense:flush-ack")).toBe(true);
  expect(frame).toBeDefined();
});
it("shows Retry and keeps the unsent mirror through a reload", async () => {
  const { mount } = await open();
  failWrites = true;
  await edit("Keep this draft");
  await waitFor(() => expect(el(p!, "save-error").hidden).toBe(false), { timeout: 2500 });
  expect(el(p!, "save-state").textContent).toBe("Not saved");
  await waitFor(() => expect(appState).toContain("Keep this draft"));
  mount.unmount();
  await open();
  await waitFor(() => expect(el(p!, "recovery").hidden).toBe(false));
  await click("recover-draft");
  failWrites = false;
  await click("retry-save");
  await saved("Keep this draft");
  await waitFor(() => expect(JSON.parse(appState).dashboardPrefs).not.toEqual({}));
  expect(appState).not.toContain('"mirror":{"id"');
});
it("opens another note through a [[ link", async () => {
  raw("write", { id: "alpha", body: "[Beta](cs:note:beta)" });
  await open();
  await act(async () => p!.doc.querySelector<HTMLAnchorElement>(".scratch-note-editor a")!.click());
  await waitFor(() => expect((el(p!, "note-title") as HTMLInputElement).value).toBe("Beta"));
});
it("routes a note-and-heading intent through the host and kit heading navigation", async () => {
  const { frame } = await open();
  await deliver(frame, {
    type: "codesense:intent",
    intent: { noteId: "beta", heading: "destination" },
  });
  await waitFor(() => expect((el(p!, "note-title") as HTMLInputElement).value).toBe("Beta"));
  expect(p!.doc.querySelector(".scratch-note-editor h2")?.textContent).toBe("Destination");
});
it("restores an earlier version and undoes one revision through records", async () => {
  const first = raw("history", { id: "alpha" })[0].commit;
  raw("write", { id: "alpha", body: "Changed" });
  await open();
  await click("history-button");
  const entries = p!.doc.querySelectorAll<HTMLElement>("#sheet-history .nt-history-entry");
  await act(async () => entries[1].querySelector<HTMLButtonElement>("button")!.click());
  await waitFor(() => expect(raw("read", { id: "alpha" }).body).toBe(BASE));
  expect(calls("restore")[0][1].args).toMatchObject({ commit: first, undo: false });
  await waitFor(() =>
    expect(p!.doc.querySelectorAll("#sheet-history .nt-history-entry").length).toBeGreaterThan(1),
  );
  await act(async () =>
    p!.doc
      .querySelectorAll<HTMLButtonElement>("#sheet-history .nt-history-entry")[0]
      .querySelectorAll("button")[1]
      .click(),
  );
  await waitFor(() => expect(calls("restore")).toHaveLength(2));
  expect(calls("restore")[1][1].args.undo).toBe(true);
});
it("reviews an agent change with removed lines in place and remembers Done", async () => {
  await open();
  await act(async () => {
    const result = raw("write", { id: "alpha", body: "# Alpha\n\nLast" });
    agentCommits.set(result.rev, "Kai");
  });
  await waitFor(() => expect(el(p!, "changes-banner").hidden).toBe(false));
  await click("review-changes");
  await waitFor(() => expect(p!.doc.querySelector(".nt-removed")?.textContent).toBe("First"));
  await click("done-changes");
  await waitFor(() => expect(el(p!, "changes-banner").hidden).toBe(true));
  expect(p!.doc.querySelector(".nt-removed")).toBeNull();
  await waitFor(() => expect(appState).toContain('"seen":{"alpha"'));
  await click("agent-menu");
  expect(el(p!, "float").querySelector(".nt-row-sub")?.textContent).toBe("1");
});
it("a team-safe copy cannot list or read private notes", async () => {
  teamSafe = true;
  await open();
  await waitFor(() => expect(el(p!, "notice").textContent).toContain("denied"));
  expect(p!.doc.querySelectorAll(".nt-note-row")).toHaveLength(0);
  expect(calls("read")).toHaveLength(0);
});
it("renames in the title, pins a row and deletes only after the pane confirmation", async () => {
  await open();
  typeField(p!, "Renamed", "note-title");
  fireEvent.keyDown(el(p!, "note-title"), { key: "Enter" });
  await waitFor(() => expect(raw("read", { id: "alpha" }).fields.title).toBe("Renamed"));
  await click("note-menu");
  await act(async () =>
    [...p!.doc.querySelectorAll<HTMLButtonElement>("#float button")]
      .find((row) => row.textContent === "Unpin")!
      .click(),
  );
  await waitFor(() => expect(raw("read", { id: "alpha" }).fields.pinned).toBe(false));
  await click("note-menu");
  await act(async () =>
    [...p!.doc.querySelectorAll<HTMLButtonElement>("#float button")]
      .find((row) => row.textContent === "Delete…")!
      .click(),
  );
  expect(calls("delete")).toHaveLength(0);
  expect(el(p!, "delete-backdrop").hidden).toBe(false);
  await click("cancel-delete");
  expect(calls("delete")).toHaveLength(0);
  await click("note-menu");
  await act(async () =>
    [...p!.doc.querySelectorAll<HTMLButtonElement>("#float button")]
      .find((row) => row.textContent === "Delete…")!
      .click(),
  );
  await click("confirm-delete");
  await waitFor(() => expect(calls("delete")).toHaveLength(1));
});
it("claims pane commands, leaves other chords alone and gives Bold to the editor", async () => {
  await open();
  const root = p!.doc.querySelector<HTMLElement>(".scratch-note-editor")!;
  const key = (target: HTMLElement, key: string, extra = {}) => {
    const event = new p!.win.KeyboardEvent("keydown", {
      key,
      metaKey: true,
      bubbles: true,
      cancelable: true,
      ...extra,
    });
    target.dispatchEvent(event as never);
    return event.defaultPrevented;
  };
  expect(key(root, "b")).toBe(true);
  expect(el(p!, "notes").dataset.drawer).toBe("false");
  expect(key(root, "k")).toBe(false);
  expect(key(el(p!, "find-note"), "o")).toBe(true);
  await waitFor(() => expect(el(p!, "float").hidden).toBe(false));
  const escape = new p!.win.KeyboardEvent("keydown", {
    key: "Escape",
    bubbles: true,
    cancelable: true,
  });
  el(p!, "palette-search").dispatchEvent(escape as never);
  expect(escape.defaultPrevented).toBe(true);
  expect(el(p!, "float").hidden).toBe(true);
});
it("searches body text and creates a note from a search without matches", async () => {
  await open();
  typeField(p!, "Second note", "list-search");
  await waitFor(() => expect(p!.doc.querySelectorAll(".nt-note-row")).toHaveLength(1));
  expect(p!.doc.querySelector(".nt-note-row")?.getAttribute("data-note-id")).toBe("beta");
  typeField(p!, "Fresh title", "list-search");
  await waitFor(() => expect(p!.doc.querySelectorAll(".nt-note-row")).toHaveLength(0));
  fireEvent.keyDown(el(p!, "list-search"), { key: "Enter" });
  await waitFor(() => expect(calls("create")).toHaveLength(1));
  expect(calls("create")[0][1].args.fields.title).toBe("Fresh title");
});
it("applies the two layout breakpoints and shows history in the full-width side column", async () => {
  const { frame } = await open();
  for (const [width, layout] of [
    [719, "narrow"],
    [720, "split"],
    [1079, "split"],
    [1080, "full"],
  ]) {
    await deliver(frame, {
      type: "codesense:slot-context",
      context: { width, presentation: "docked" },
    });
    await waitFor(() => expect(el(p!, "notes").dataset.layout).toBe(layout));
  }
  await click("history-button");
  expect(el(p!, "history-sheet").hidden).toBe(true);
  await waitFor(() => expect(el(p!, "side-history").hidden).toBe(false));
});
it("flushes when the real host hides the pane", async () => {
  const { mount } = await open();
  await edit("Hidden last key");
  mount.rerender(
    <HtmlViewer
      filePath={PATH}
      teamContentPath="/ctx/content"
      pane={{ contentRoot: "/ctx/content", panesDir: "/ctx/private/panes", fileName: "notes.html" }}
      visible={false}
    />,
  );
  await saved("Hidden last key");
});
it("flushes the final edit on body blur", async () => {
  await open();
  await edit("Blurred last key");
  await act(async () => fireEvent.focusOut(p!.doc.querySelector(".scratch-note-editor")!));
  await saved("Blurred last key");
});
it("offers titles before body hits in the in-pane note palette", async () => {
  await open();
  await click("find-note");
  await waitFor(() => expect(p!.doc.getElementById("palette-search")).not.toBeNull());
  typeField(p!, "Second note", "palette-search");
  await waitFor(() => expect(el(p!, "palette-results").textContent).toContain("Beta"));
  await act(async () =>
    p!.doc.querySelector<HTMLButtonElement>("#palette-results button")!.click(),
  );
  await waitFor(() => expect((el(p!, "note-title") as HTMLInputElement).value).toBe("Beta"));
});
it("keeps new typing and a remote edit when a previous save is still in flight", async () => {
  await open();
  const invoke = ipc.getMockImplementation()!;
  let release!: () => void,
    held = false;
  const gate = new Promise<void>((resolve) => (release = resolve));
  ipc.mockImplementation(async (command, args) => {
    const result = await invoke(command, args);
    if (command === "records_call" && args.operation === "write" && !held) {
      held = true;
      await gate;
    }
    return result;
  });
  await edit("Your first edit");
  await act(async () => fireEvent.focusOut(p!.doc.querySelector(".scratch-note-editor")!));
  await waitFor(() => expect(held).toBe(true));
  await act(async () =>
    raw("write", { id: "alpha", body: "# Alpha\n\nAgent first\n\nYour first edit" }),
  );
  await edit("Your final edit", false);
  await act(async () => release());
  await saved("Your final edit");
  expect(raw("read", { id: "alpha" }).body).toContain("Agent first");
  await waitFor(() => expect(el(p!, "save-state").textContent).toBe("Saved"));
});
it("omits an oversized full draft instead of recovering a truncated document", async () => {
  const original = "# Alpha\n\n" + "x".repeat(10 * 1024) + "\n\nTail marker";
  raw("write", { id: "alpha", body: original });
  const { mount } = await open();
  failWrites = true;
  await edit("Tail marker edited");
  await act(async () => fireEvent.focusOut(p!.doc.querySelector(".scratch-note-editor")!));
  await waitFor(() => expect(appState).toContain('"mirror"'));
  const entry = Object.values(JSON.parse(appState).dashboardPrefs)[0] as {
    data: { notes: { mirror: unknown } };
  };
  expect(entry.data.notes.mirror).toBeNull();
  expect(new TextEncoder().encode(JSON.stringify(entry.data)).length).toBeLessThanOrEqual(
    16 * 1024,
  );
  expect(el(p!, "save-error").hidden).toBe(false);
  expect(el(p!, "retry-save").hidden).toBe(false);
  expect(p!.doc.querySelector(".scratch-note-editor")?.textContent).toContain("Tail marker edited");
  mount.unmount();
  await open();
  expect(el(p!, "recovery").hidden).toBe(true);
  expect(raw("read", { id: "alpha" }).body).toBe(original);
});

it("preserves an unfinished title across an agent body refresh", async () => {
  await open();
  el(p!, "note-title").focus();
  typeField(p!, "Title draft", "note-title");
  await act(async () => raw("write", { id: "alpha", body: "Agent changed the body" }));
  await waitFor(() =>
    expect(p!.doc.querySelector(".scratch-note-editor")?.textContent).toContain("Agent changed"),
  );
  expect((el(p!, "note-title") as HTMLInputElement).value).toBe("Title draft");
  fireEvent.blur(el(p!, "note-title"));
  await waitFor(() => expect(raw("read", { id: "alpha" }).fields.title).toBe("Title draft"));
});
it("reviews an older agent revision outside the history sheet and undoes that revision", async () => {
  const agent = raw("write", { id: "alpha", body: "# Alpha\n\nAgent first\n\nLast" }).rev;
  agentCommits.set(agent, "Kai");
  raw("write", { id: "alpha", body: "# Alpha\n\nAgent first\n\nHuman last" });
  await open();
  await click("history-button");
  const entry = p!.doc.querySelectorAll<HTMLElement>("#sheet-history .nt-history-entry")[1];
  await act(async () => entry.querySelector<HTMLButtonElement>("button")!.click());
  await waitFor(() => expect(el(p!, "done-changes").hidden).toBe(false));
  expect(el(p!, "history-sheet").hidden).toBe(true);
  await click("undo-changes");
  await waitFor(() => expect(calls("restore")).toHaveLength(1));
  expect(calls("restore")[0][1].args).toMatchObject({ commit: agent, undo: true });
  await waitFor(() => expect(el(p!, "changes-banner").hidden).toBe(true));
  expect(p!.doc.querySelector(".nt-removed")).toBeNull();
});
it("retains the accumulated notes after paging beyond two hundred records", async () => {
  for (let i = 0; i < 199; i++)
    raw("create", { id: `extra-${i}`, fields: { title: `Extra ${i}` }, body: "" });
  await open();
  await waitFor(() => expect(p!.doc.querySelectorAll(".nt-note-row")).toHaveLength(201));
  expect(p!.doc.querySelector('[data-note-id="alpha"]')).not.toBeNull();
  expect(p!.doc.querySelector('[data-note-id="beta"]')).not.toBeNull();
});
it("adopts an agent edit committed while the local save response is in flight", async () => {
  await open();
  const invoke = ipc.getMockImplementation()!;
  let release!: () => void,
    held = false;
  const gate = new Promise<void>((resolve) => (release = resolve));
  ipc.mockImplementation(async (command, args) => {
    const result = await invoke(command, args);
    if (command === "records_call" && args.operation === "write" && !held) {
      held = true;
      await gate;
    }
    return result;
  });
  await edit("Your saved edit");
  await act(async () => fireEvent.focusOut(p!.doc.querySelector(".scratch-note-editor")!));
  await waitFor(() => expect(held).toBe(true));
  await act(async () => raw("write", { id: "alpha", body: "Agent's newer body" }));
  await act(async () => release());
  await waitFor(() =>
    expect(p!.doc.querySelector(".scratch-note-editor")?.textContent).toContain(
      "Agent's newer body",
    ),
  );
  expect(el(p!, "save-state").textContent).toBe("Saved");
});
it("finishes switching notes when an agent updates the previous note during the read", async () => {
  const { frame } = await open();
  const invoke = ipc.getMockImplementation()!;
  let release!: () => void,
    held = false;
  const gate = new Promise<void>((resolve) => (release = resolve));
  ipc.mockImplementation(async (command, args) => {
    const result = await invoke(command, args);
    if (command === "records_call" && args.operation === "read" && args.args.id === "beta") {
      held = true;
      await gate;
    }
    return result;
  });
  const reads = calls("read").filter(([, args]) => args.args.id === "alpha").length;
  await deliver(frame, { type: "codesense:intent", intent: { noteId: "beta" } });
  await waitFor(() => expect(held).toBe(true));
  await act(async () => raw("write", { id: "alpha", body: "Agent update during navigation" }));
  await waitFor(() =>
    expect(calls("read").filter(([, args]) => args.args.id === "alpha").length).toBeGreaterThan(
      reads,
    ),
  );
  await act(async () => release());
  await waitFor(() => expect((el(p!, "note-title") as HTMLInputElement).value).toBe("Beta"));
});

it("saves a focused title before Cmd+N creates and selects another note", async () => {
  await open();
  el(p!, "note-title").focus();
  typeField(p!, "Rename before new note", "note-title");
  fireEvent.keyDown(el(p!, "note-title"), { key: "n", metaKey: true });
  await waitFor(() => expect(calls("create")).toHaveLength(1));
  expect(raw("read", { id: "alpha" }).fields.title).toBe("Rename before new note");
  await waitFor(() => expect((el(p!, "note-title") as HTMLInputElement).value).toBe(""));
});

it("recovers a failed draft with its original base and preserves a later agent edit", async () => {
  const { mount } = await open();
  failWrites = true;
  await edit("Your recovered last");
  await act(async () => raw("write", { id: "alpha", body: "# Alpha\n\nAgent first\n\nLast" }));
  await waitFor(() => expect(appState).toContain("Your recovered last"), { timeout: 2500 });
  const mirrored = Object.values(JSON.parse(appState).dashboardPrefs)[0] as {
    data: { notes: { mirror: { base: string } } };
  };
  expect(mirrored.data.notes.mirror.base).toBe(BASE);
  mount.unmount();
  await open();
  await waitFor(() => expect(el(p!, "recovery").hidden).toBe(false));
  await click("recover-draft");
  failWrites = false;
  await click("retry-save");
  await saved("Your recovered last");
  expect(raw("read", { id: "alpha" }).body).toContain("Agent first");
  await waitFor(() => expect(el(p!, "save-state").textContent).toBe("Saved"));
});
it.each(["rename", "pin"])(
  "preserves a newer title while a %s response is pending",
  async (operation) => {
    await open();
    const invoke = ipc.getMockImplementation()!;
    let release!: () => void,
      held = false;
    const gate = new Promise<void>((resolve) => (release = resolve));
    ipc.mockImplementation(async (command, args) => {
      const result = await invoke(command, args);
      if (command === "records_call" && args.operation === "setFields" && !held) {
        held = true;
        await gate;
      }
      return result;
    });
    if (operation === "rename") {
      el(p!, "note-title").focus();
      typeField(p!, "Submitted title", "note-title");
      fireEvent.keyDown(el(p!, "note-title"), { key: "Enter" });
    } else {
      await click("note-menu");
      await act(async () =>
        [...p!.doc.querySelectorAll<HTMLButtonElement>("#float button")]
          .find((row) => row.textContent === "Unpin")!
          .click(),
      );
    }
    await waitFor(() => expect(held).toBe(true));
    el(p!, "note-title").focus();
    typeField(p!, "Newer local title", "note-title");
    const response = replyFor("records.setFields");
    await act(async () => {
      release();
      await response;
    });
    await waitFor(() =>
      expect((el(p!, "note-title") as HTMLInputElement).value).toBe("Newer local title"),
    );
    await act(async () => fireEvent.blur(el(p!, "note-title")));
    await waitFor(() =>
      expect(raw("read", { id: "alpha" }).fields.title).toBe("Newer local title"),
    );
  },
);
it("Done remembers the reviewed revision while a newer agent revision stays unread", async () => {
  const first = raw("write", { id: "alpha", body: "# Alpha\n\nAgent first\n\nLast" }).rev;
  agentCommits.set(first, "Kai");
  await open();
  await waitFor(() => expect(el(p!, "changes-banner").hidden).toBe(false));
  await click("review-changes");
  await waitFor(() => expect(el(p!, "done-changes").hidden).toBe(false));
  let second: string;
  await act(async () => {
    second = raw("write", { id: "alpha", body: "# Alpha\n\nAgent second\n\nLast" }).rev;
    agentCommits.set(second, "Mira");
  });
  await waitFor(() => expect(el(p!, "changes-copy").textContent).toContain("Mira"));
  await click("done-changes");
  await waitFor(() => {
    const entry = Object.values(JSON.parse(appState).dashboardPrefs)[0] as {
      data: { notes: { seen: Record<string, string> } };
    };
    expect(entry.data.notes.seen.alpha).toBe(first);
  });
  await waitFor(() => expect(el(p!, "changes-banner").hidden).toBe(false));
  expect(el(p!, "review-changes").hidden).toBe(false);
});
it("clears the passive agent tint when your own saved revision becomes latest", async () => {
  const revision = raw("write", { id: "alpha", body: "# Alpha\n\nAgent first\n\nLast" }).rev;
  agentCommits.set(revision, "Kai");
  await open();
  await waitFor(() => expect(p!.doc.querySelector(".nt-chg")).not.toBeNull());
  await edit("Your last");
  await act(async () => fireEvent.focusOut(p!.doc.querySelector(".scratch-note-editor")!));
  await saved("Your last");
  await waitFor(() => expect(el(p!, "changes-banner").hidden).toBe(true));
  expect(p!.doc.querySelector(".nt-chg")).toBeNull();
});
it("loads context beyond the first five hundred rows for links and the link picker", async () => {
  const late = [
    {
      title: "Late story",
      fields: { story_id: "SV-LATE" },
      relPath: "stories/late.md",
      path: "/ctx/content/stories/late.md",
    },
    { title: "Late file", fields: {}, relPath: "docs/late.md", path: "/ctx/content/docs/late.md" },
  ];
  contextQuery = (query) =>
    query.cursor
      ? { rows: late, total: 502 }
      : {
          rows: Array.from({ length: 500 }, (_, i) => ({
            title: `Document ${i}`,
            fields: {},
            relPath: `docs/${i}.md`,
            path: `/ctx/content/docs/${i}.md`,
          })),
          total: 502,
          nextCursor: "context-page-2",
        };
  raw("write", {
    id: "alpha",
    body: "[Open story](cs:story:SV-LATE)\n\n[Open file](cs:file:docs/late.md)\n\nLast",
  });
  await open();
  await waitFor(() =>
    expect(
      ipc.mock.calls.some(
        ([command, args]) => command === "query_context" && args.query.cursor === "context-page-2",
      ),
    ).toBe(true),
  );
  await edit("[[Late");
  await waitFor(() => expect(el(p!, "suggestions").textContent).toContain("Late story"));
  expect(el(p!, "suggestions").textContent).toContain("Late file");
  fireEvent.keyDown(p!.doc.querySelector(".scratch-note-editor")!, { key: "Escape" });
  for (const label of ["Open story", "Open file"]) {
    await act(async () =>
      [...p!.doc.querySelectorAll<HTMLAnchorElement>(".scratch-note-editor a")]
        .find((node) => node.textContent === label)!
        .click(),
    );
  }
  await waitFor(() =>
    expect(
      ipc.mock.calls.filter(([command]) => command === "bridge_resolve_open_target"),
    ).toHaveLength(2),
  );
  expect(
    ipc.mock.calls
      .filter(([command]) => command === "bridge_resolve_open_target")
      .map(([, args]) => args.path),
  ).toEqual(late.map((row) => row.path));
  await waitFor(() => expect(navigate).toHaveBeenCalledTimes(2));
});

it("queries an uncached story or file target while context paging is still pending", async () => {
  const story = {
    title: "Late story",
    fields: { story_id: "SV-LATE" },
    relPath: "stories/late.md",
    path: "/ctx/content/stories/late.md",
  };
  const file = {
    title: "Late file",
    fields: {},
    relPath: "docs/late.md",
    path: "/ctx/content/docs/late.md",
  };
  let release!: (value: unknown) => void;
  const page = new Promise((resolve) => (release = resolve));
  contextQuery = (query) => {
    if ((query.match as { story_id?: string } | undefined)?.story_id === "SV-LATE")
      return { rows: [story], total: 1 };
    if ((query.where as { pathPrefix?: string } | undefined)?.pathPrefix === "docs/late.md")
      return { rows: [file], total: 1 };
    if (query.cursor) return page;
    return { rows: [], total: 2, nextCursor: "pending-page" };
  };
  raw("write", {
    id: "alpha",
    body: "[Open story](cs:story:SV-LATE)\n\n[Open file](cs:file:docs/late.md)",
  });
  await open();
  for (const label of ["Open story", "Open file"]) {
    await act(async () =>
      [...p!.doc.querySelectorAll<HTMLAnchorElement>(".scratch-note-editor a")]
        .find((node) => node.textContent === label)!
        .click(),
    );
  }
  await waitFor(() => expect(navigate).toHaveBeenCalledTimes(2));
  expect(navigate.mock.calls.map(([path]) => path)).toEqual([story.path, file.path]);
  await act(async () => release({ rows: [story, file], total: 2 }));
});

it("ignores a passive comparison that arrives after your newer history cleared the tint", async () => {
  await open();
  const invoke = ipc.getMockImplementation()!;
  let release!: () => void,
    held = false;
  const gate = new Promise<void>((resolve) => (release = resolve));
  ipc.mockImplementation(async (command, args) => {
    const result = await invoke(command, args);
    if (command === "records_call" && args.operation === "readAt" && !held) {
      held = true;
      await gate;
    }
    return result;
  });
  await act(async () => {
    const revision = raw("write", { id: "alpha", body: "# Alpha\n\nAgent first\n\nLast" }).rev;
    agentCommits.set(revision, "Kai");
  });
  await waitFor(() => expect(held).toBe(true));
  await edit("Your newer last");
  await act(async () => fireEvent.focusOut(p!.doc.querySelector(".scratch-note-editor")!));
  await saved("Your newer last");
  await waitFor(() => expect(el(p!, "changes-banner").hidden).toBe(true));
  const response = replyFor("records.readAt");
  await act(async () => {
    release();
    await response;
  });
  expect(el(p!, "changes-banner").hidden).toBe(true);
  expect(p!.doc.querySelector(".nt-chg")).toBeNull();
});
it("restarts a stale context cursor once and refreshes the source when the link picker opens", async () => {
  const old = {
    title: "Old document",
    fields: {},
    relPath: "docs/old.md",
    path: "/ctx/content/docs/old.md",
  };
  const repaired = {
    title: "Repaired document",
    fields: {},
    relPath: "docs/repaired.md",
    path: "/ctx/content/docs/repaired.md",
  };
  let firstPages = 0,
    added = false;
  contextQuery = (query) => {
    if (query.cursor === "old-generation")
      throw new Error(
        "results changed since this page was fetched; restart paging without a cursor",
      );
    if (query.cursor === "current-generation") return { rows: [], total: 1 };
    firstPages++;
    if (firstPages === 1) return { rows: [old], total: 501, nextCursor: "old-generation" };
    if (firstPages === 2) return { rows: [old], total: 1, nextCursor: "current-generation" };
    return { rows: added ? [repaired] : [old], total: 1 };
  };
  await open();
  await waitFor(() =>
    expect(
      ipc.mock.calls.some(
        ([command, args]) =>
          command === "query_context" && args.query.cursor === "current-generation",
      ),
    ).toBe(true),
  );
  expect(firstPages).toBe(2);
  added = true;
  await edit("[[Repaired");
  await waitFor(() => expect(el(p!, "suggestions").textContent).toContain("Repaired document"));
  expect(firstPages).toBe(3);
  await act(async () =>
    fireEvent.keyDown(p!.doc.querySelector(".scratch-note-editor")!, { key: "Tab" }),
  );
  await waitFor(() =>
    expect(
      p!.doc.querySelector<HTMLAnchorElement>(".scratch-note-editor a")?.getAttribute("href"),
    ).toBe("cs:file:docs/repaired.md"),
  );
});

it.each([false, true])(
  "keeps the highlighted note through a pending context refresh (changed: %s)",
  async (changed) => {
    raw("create", { id: "gamma", fields: { title: "Gamma" }, body: "Third note" });
    let release!: (value: unknown) => void,
      queries = 0;
    const gate = new Promise((resolve) => (release = resolve));
    contextQuery = () => (++queries === 1 ? { rows: [], total: 0 } : gate);
    await open();
    await edit("[[");
    await waitFor(() => expect(queries).toBe(2));
    const root = p!.doc.querySelector<HTMLElement>(".scratch-note-editor")!;
    await act(async () => fireEvent.keyDown(root, { key: "ArrowDown" }));
    const highlighted = p!.doc.querySelector<HTMLElement>('#suggestions [aria-selected="true"]')!;
    const chosen = highlighted.querySelector(".nt-row-label")!.textContent;
    expect(highlighted.id).toBe("note-link-opt-1");
    const response = replyFor("queryContext");
    await act(async () => {
      release({
        rows: changed
          ? [
              {
                title: "New document",
                fields: {},
                relPath: "docs/new.md",
                path: "/ctx/content/docs/new.md",
              },
            ]
          : [],
        total: changed ? 1 : 0,
      });
      await response;
      await new Promise<void>((resolve) => p!.win.requestAnimationFrame(() => resolve()));
    });
    expect(
      p!.doc.querySelector('#suggestions [aria-selected="true"] .nt-row-label')?.textContent,
    ).toBe(chosen);
    if (!changed)
      expect(p!.doc.querySelector('#suggestions [aria-selected="true"]')).toBe(highlighted);
    else expect(el(p!, "suggestions").textContent).toContain("New document");
    await act(async () => fireEvent.keyDown(root, { key: "Tab" }));
    await waitFor(() =>
      expect(
        p!.doc.querySelector<HTMLAnchorElement>(".scratch-note-editor a")?.getAttribute("href"),
      ).toBe(`cs:note:${chosen === "Beta" ? "beta" : "gamma"}`),
    );
  },
);

const sender = (handle = "ada", name = "Ada") => ({
  id: handle,
  handle,
  sessionId: handle,
  kind: "cli",
  alive: true,
  team: { org: ORG, team: TEAM },
  rowNames: { primary: name },
  state: "idle",
});
async function agentPicker() {
  await click("agent-menu");
  await act(async () =>
    [...p!.doc.querySelectorAll<HTMLButtonElement>("#float button")]
      .find((b) => b.textContent === "Send to a running agent…")!
      .click(),
  );
  await waitFor(() => expect(p!.doc.querySelector("[data-agent=ada]")).not.toBeNull());
}
it("sends the note to a running agent and remembers the context preference", async () => {
  sessionRows = [sender(), { ...sender("ended", "Ended"), alive: false }];
  await open();
  await agentPicker();
  expect(p!.doc.querySelector("[data-agent=ended]")).toBeNull();
  const check = p!.doc.querySelector<HTMLInputElement>(".nt-send-context input")!;
  expect(check.checked).toBe(true);
  await act(async () => check.click());
  await act(async () => p!.doc.querySelector<HTMLButtonElement>("[data-agent=ada]")!.click());
  await waitFor(() =>
    expect(ipc.mock.calls.some(([cmd]) => cmd === "send_prompt_to_agent")).toBe(true),
  );
  const call = ipc.mock.calls.find(([cmd]) => cmd === "send_prompt_to_agent")![1];
  expect(call.handle).toBe("ada");
  expect(call.text).toContain("notes_read");
  expect(call.text).toContain("alpha in org acme/api");
  await waitFor(() => expect(el(p!, "float").hidden).toBe(true));
  await agentPicker();
  expect(p!.doc.querySelector<HTMLInputElement>(".nt-send-context input")!.checked).toBe(false);
  expect(el(p!, "float").textContent).toContain("Start a new agent with this");
  expect(el(p!, "float").textContent).not.toMatch(/terminal|Organize|Routine/i);
});
it("keeps failed delivery visible without adding an agent chip", async () => {
  sessionRows = [sender()];
  sendResult = { status: "refused", submitted: false, message: "Agent is unavailable" };
  await open();
  await agentPicker();
  await act(async () => p!.doc.querySelector<HTMLButtonElement>("[data-agent=ada]")!.click());
  await waitFor(() => expect(el(p!, "notice").textContent).toBe("Agent is unavailable"));
  expect(el(p!, "float").hidden).toBe(false);
  expect(p!.doc.querySelector(".nt-agent-chip")).toBeNull();
});
it("hides sending when the host denies sendPrompt", async () => {
  allowSend = false;
  sessionRows = [sender()];
  await open();
  await click("agent-menu");
  expect(el(p!, "notes").dataset.canSend).toBe("false");
  expect(el(p!, "float").textContent).toContain("Changes by agents");
  expect(el(p!, "float").textContent).not.toContain("Send to running");
});
it("uses the full layout in an overlay and leaves final Escape to the host", async () => {
  const { frame } = await open();
  await deliver(frame, {
    type: "codesense:slot-context",
    context: { width: 800, presentation: "overlay" },
  });
  await waitFor(() => expect(el(p!, "notes").dataset.layout).toBe("full"));
  const escape = new p!.win.KeyboardEvent("keydown", {
    key: "Escape",
    bubbles: true,
    cancelable: true,
  });
  p!.doc.body.dispatchEvent(escape as never);
  expect(escape.defaultPrevented).toBe(false);
  await deliver(frame, {
    type: "codesense:slot-context",
    context: { width: 800, presentation: "docked" },
  });
  await waitFor(() => expect(el(p!, "notes").dataset.layout).toBe("split"));
});
it.each(["Keep yours", "Keep theirs", "Keep both"])(
  "resolves an overlap with %s and writes the chosen body",
  async (choice) => {
    await open();
    const previous = ipc.getMockImplementation()!;
    let inject = true;
    ipc.mockImplementation(async (command, args) => {
      if (inject && command === "records_call" && args.operation === "write") {
        inject = false;
        const merged = "Before\nYours\nTheirs\nAfter";
        return {
          ...(await previous(command, { ...args, args: { ...args.args, body: merged } })),
          overlaps: [{ ours: "Yours\n", theirs: "Theirs\n", at: 2 }],
        };
      }
      return previous(command, args);
    });
    await edit("Yours");
    await click("history-button");
    await waitFor(() => expect(el(p!, "overlap-groups").hidden).toBe(false), { timeout: 2500 });
    const writes = calls("write").length;
    await act(async () =>
      [...p!.doc.querySelectorAll<HTMLButtonElement>("#overlap-groups button")]
        .find((b) => b.textContent === choice)!
        .click(),
    );
    await waitFor(() => expect(calls("write").length).toBeGreaterThan(writes));
    const chosen = calls("write").at(-1)![1].args.body;
    expect(chosen).toBe(
      choice === "Keep yours"
        ? "Before\nYours\nAfter"
        : choice === "Keep theirs"
          ? "Before\nTheirs\nAfter"
          : "Before\nYours\nTheirs\nAfter",
    );
    await waitFor(() => expect(el(p!, "overlap-groups").hidden).toBe(true));
  },
);
it("delegates row context menus while preserving text context menus", async () => {
  await open();
  const row = p!.doc.querySelector<HTMLElement>("[data-note-id=alpha]")!;
  fireEvent.contextMenu(row, { clientX: 40, clientY: 50 });
  await waitFor(() => expect(p!.sent.some((m) => m.command === "showMenu")).toBe(true));
  const message = p!.sent.find((m) => m.command === "showMenu")!;
  expect(message.args).toMatchObject({
    row: { noteId: "alpha" },
    items: [
      { id: "pin", label: "Unpin" },
      { id: "rename", label: "Rename" },
      { id: "delete", label: "Delete…" },
    ],
  });
  await new Promise((resolve) => p!.win.setTimeout(resolve, 0));
  const sentBefore = p!.sent.length;
  el(p!, "note-title").focus();
  const event = new p!.win.MouseEvent("contextmenu", {
    bubbles: true,
    cancelable: true,
    clientX: 40,
    clientY: 50,
  });
  el(p!, "note-title").dispatchEvent(event as never);
  expect(event.defaultPrevented).toBe(true);
  await waitFor(() =>
    expect(p!.sent.slice(sentBefore)).toContainEqual(
      expect.objectContaining({ type: "codesense:contextmenu", target: "text" }),
    ),
  );
  expect(p!.sent.slice(sentBefore).filter((m) => m.command === "showMenu")).toHaveLength(0);
});

it.each([
  ["Keep yours", "ours\nshared\n"],
  ["Keep theirs", "theirs\nshared\n"],
])(
  "resolves interleaved union lines with %s without changing surrounding text",
  async (choice, chosen) => {
    await open();
    const previous = ipc.getMockImplementation()!;
    let inject = true;
    ipc.mockImplementation(async (command, args) => {
      if (inject && command === "records_call" && args.operation === "write") {
        inject = false;
        return {
          ...(await previous(command, {
            ...args,
            args: { ...args.args, body: "Before\nours\ntheirs\nshared\nAfter" },
          })),
          overlaps: [{ ours: "ours\nshared\n", theirs: "theirs\nshared\n", at: 2 }],
        };
      }
      return previous(command, args);
    });
    await edit("ours");
    await waitFor(() => expect(el(p!, "overlap-groups").hidden).toBe(false), { timeout: 2500 });
    await act(async () =>
      [...p!.doc.querySelectorAll<HTMLButtonElement>("#overlap-groups button")]
        .find((button) => button.textContent === choice)!
        .click(),
    );
    await waitFor(() =>
      expect(raw("read", { id: "alpha" }).body).toBe("Before\n" + chosen + "After"),
    );
    expect(el(p!, "overlap-groups").hidden).toBe(true);
  },
);

it.each(["Keep yours", "Keep theirs"])(
  "refuses %s after the Today conflict changes instead of resolving a duplicate template",
  async (choice) => {
    const merged =
      "# Today\n\nReview PRs\n\nRun tests\n\n# Checklist template\n\nReview PRs\n\nRun tests\n\nTemplate end";
    raw("write", { id: "alpha", body: merged });
    await open();
    const previous = ipc.getMockImplementation()!;
    let inject = true;
    ipc.mockImplementation(async (command, args) => {
      if (inject && command === "records_call" && args.operation === "write") {
        inject = false;
        return {
          ...(await previous(command, { ...args, args: { ...args.args, body: merged } })),
          overlaps: [{ ours: "Review PRs\n\n", theirs: "Run tests\n\n", at: 3 }],
        };
      }
      return previous(command, args);
    });
    await edit("Trigger the merge");
    await waitFor(() => expect(el(p!, "overlap-groups").hidden).toBe(false), { timeout: 2500 });
    await waitFor(() => expect(el(p!, "save-state").textContent).toBe("Saved"));
    const root = p!.doc.querySelector<HTMLElement>(".scratch-note-editor")!;
    await act(async () => {
      root.querySelector("p")!.textContent = "Review release PRs";
      fireEvent.input(root);
      fireEvent.focusOut(root);
    });
    await saved("Review release PRs");
    await waitFor(() => expect(el(p!, "save-state").textContent).toBe("Saved"));
    const edited = raw("read", { id: "alpha" }).body;
    const writes = calls("write").length;
    await act(async () =>
      [...p!.doc.querySelectorAll<HTMLButtonElement>("#overlap-groups button")]
        .find((button) => button.textContent === choice)!
        .click(),
    );
    await waitFor(() => expect(el(p!, "notice").textContent).toContain("This passage changed."));
    expect(calls("write")).toHaveLength(writes);
    expect(raw("read", { id: "alpha" }).body).toBe(edited);
    expect(edited).toContain("# Checklist template\n\nReview PRs\n\nRun tests");
    expect(el(p!, "overlap-groups").hidden).toBe(false);
  },
);

describe("notes commands", () => {
  const commandId = (id: string) => dashboardCommandActionId(dashboardRefKey(NOTES_REF), id);
  function install() {
    const base = listing({
      org: ORG,
      team: TEAM,
      root: "/ctx",
      id: "notes",
      title: "Notes",
      privateCopy: true,
      surface: "notes",
    });
    const meta = /name="codesense:commands"\s+content='([^']*)'/.exec(SOURCE);
    const capabilities = /name="codesense:capabilities"\s+content="([^"]*)"/
      .exec(SOURCE)![1]
      .split(/\s+/);
    base.panes[0].declaration = {
      slots: ["right-pane"],
      capabilities,
      commands: meta ? JSON.parse(meta[1]) : [],
    };
    usePaneStore.setState({ teamKey: `${ORG}:${TEAM}`, listing: base, knownEntries: {} });
    useDashboardProfileStore.setState({
      org: ORG,
      profile: {
        id: "default",
        name: "Default",
        posture: "",
        slots: { "right-pane": [NOTES_REF] },
        cliLayout: "default",
        chrome: {},
      },
    });
    syncDashboardCommands();
  }
  beforeEach(() => {
    useShortcutsStore.setState({ shortcuts: {}, stateLoaded: true });
    install();
  });
  afterEach(() => {
    usePaneStore.setState({ listing: null, knownEntries: {} });
    syncDashboardCommands();
    setDynamicShortcutActions([]);
  });

  it("registers four commands with only New note bound and command access enabled", () => {
    const actions = allShortcutActions().filter((a) => a.command?.dashboardTitle === "Notes");
    expect(actions.map((a) => a.label)).toEqual([
      "New note",
      "Find note…",
      "Go to heading…",
      "Toggle notes list",
    ]);
    expect(usePaneStore.getState().listing!.panes[0].declaration!.capabilities).toContain(
      "commands:run",
    );
    expect(actions.every((a) => !a.command?.humanOnly)).toBe(true);
    const bindings = resolveBindings({});
    expect(bindings.get(commandId("new"))?.keystroke).toBe("CmdOrCtrl+N");
    for (const id of ["find", "goto-heading", "toggle-list"])
      expect(bindings.get(commandId(id))).toMatchObject({
        keystroke: "",
        unbound: { kind: "none" },
      });
  });

  it("delivers each host command to its pane action", async () => {
    await open();
    act(() => runDashboardCommand(commandId("find")));
    await waitFor(() =>
      expect(p!.doc.querySelector<HTMLInputElement>("#palette-search")?.placeholder).toBe(
        "Find a note",
      ),
    );
    act(() => runDashboardCommand(commandId("goto-heading")));
    await waitFor(() =>
      expect(p!.doc.querySelector<HTMLInputElement>("#palette-search")?.placeholder).toBe(
        "Go to heading",
      ),
    );
    expect(el(p!, "palette-results").textContent).toContain("Alpha");
    act(() => runDashboardCommand(commandId("toggle-list")));
    await waitFor(() => expect(el(p!, "notes").dataset.drawer).toBe("true"));
    act(() => runDashboardCommand(commandId("toggle-list")));
    await waitFor(() => expect(el(p!, "notes").dataset.drawer).toBe("false"));
    act(() => runDashboardCommand(commandId("new")));
    await waitFor(() => expect(calls("create")).toHaveLength(1));
    await waitFor(() => expect(p!.doc.activeElement?.id).toBe("note-title"));
  });

  it("⌘O keeps the native switcher and its newer intent wins over an earlier selection echo", async () => {
    const invoke = ipc.getMockImplementation()!;
    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    let waiting = false;
    ipc.mockImplementation(async (command, args) => {
      if (
        !waiting &&
        command === "save_app_state" &&
        args.state.includes('"liveHubUi"') &&
        args.state.includes('"noteId":"alpha"')
      ) {
        waiting = true;
        await held;
      }
      return invoke(command, args);
    });
    const { frame, mount } = await open();
    await waitFor(() => expect(waiting).toBe(true));
    useViewerStore.setState({
      notes: [{ id: "beta", title: "Beta", pinned: false, lastOpenedAt: 0 }],
    });
    act(() => findShortcutAction("nav.open-note-switcher")!.run());
    const chooser = render(<NoteSwitcherMount isActiveView={true} />);
    fireEvent.click(chooser.getByRole("option", { name: /Beta/ }));
    const pending = usePaneStore.getState().pendingIntents[`pane:private:${ORG}/notes`];
    expect(pending).toMatchObject({ noteId: "beta" });
    expect(useUiStore.getState().noteSwitcherOpen).toBe(false);
    mount.rerender(
      <HtmlViewer
        filePath={PATH}
        teamContentPath="/ctx/content"
        onNavigate={navigate}
        pane={{
          contentRoot: "/ctx/content",
          panesDir: "/ctx/private/panes",
          fileName: "notes.html",
        }}
        parentPanelId={`pane:private:${ORG}/notes`}
        intent={pending}
      />,
    );
    await waitFor(() =>
      expect((el(p!, "note-title") as HTMLInputElement).value).toBe("Beta"),
    ).finally(release);
    await waitFor(async () =>
      expect((await liveHub.snapshot("ui:right-pane", "all")).snapshot[0]).toMatchObject({
        noteId: "beta",
      }),
    );
    expect((el(p!, "note-title") as HTMLInputElement).value).toBe("Beta");
    expect(frame).toBeDefined();
  });

  it("settles readiness after an initial note read fails so commands can still run", async () => {
    const invoke = ipc.getMockImplementation()!;
    let failRead = true;
    ipc.mockImplementation(async (command, args) => {
      if (
        command === "records_call" &&
        args.operation === "read" &&
        args.args.id === "alpha" &&
        failRead
      ) {
        failRead = false;
        throw new Error("Note no longer exists");
      }
      return invoke(command, args);
    });
    await open(undefined, false);
    await waitFor(() => expect(el(p!, "notice").textContent).toBe("Note no longer exists"));
    act(() => runDashboardCommand(commandId("new")));
    await waitFor(() => expect(calls("create")).toHaveLength(1));
    await waitFor(() => expect(p!.doc.activeElement?.id).toBe("note-title"));
  });

  it("runs a command posted before load once after the pane is ready", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    const invoke = ipc.getMockImplementation()!;
    ipc.mockImplementation(async (command, args) => {
      if (command === "load_app_state") await gate;
      return invoke(command, args);
    });
    const { pane, frame } = await open(undefined, false);
    const received = vi.fn();
    pane.win.codesense.onCommand(received);
    await deliver(frame, { type: "codesense:command", id: "new", dispatchId: "before-load" });
    await waitFor(() => expect(received).toHaveBeenCalledTimes(1));
    expect(calls("create")).toHaveLength(0);
    await deliver(frame, {
      type: "codesense:command",
      id: "goto-heading",
      dispatchId: "heading-before-load",
    });
    await waitFor(() => expect(received).toHaveBeenCalledTimes(2));
    await act(async () => release());
    await waitFor(() => expect(calls("create")).toHaveLength(1));
    await waitFor(() =>
      expect(pane.doc.querySelector<HTMLInputElement>("#palette-search")?.placeholder).toBe(
        "Go to heading",
      ),
    );
    await act(async () => emit("codesense:records-changed", {}));
    expect(calls("create")).toHaveLength(1);
  });

  it.each([
    ["n", false],
    ["o", false],
    ["o", true],
    ["b", false],
  ])("consumes its claimed ⌘%s (shift: %s) before the app bridge", async (key, shiftKey) => {
    await open();
    const event = new p!.win.KeyboardEvent("keydown", {
      key,
      code: `Key${key.toUpperCase()}`,
      metaKey: true,
      shiftKey,
      bubbles: true,
      cancelable: true,
    });
    p!.doc.body.dispatchEvent(event as never);
    expect(event.defaultPrevented).toBe(true);
    await act(async () => new Promise((resolve) => setTimeout(resolve, 20)));
    expect(p!.sent.filter((m) => m.type === "codesense:shortcut")).toHaveLength(0);
  });
});

it("restores the shared note, publishes local picks, and follows native picks without echo", async () => {
  await liveHub.writeUi("ui:right-pane", "all", { noteId: "beta" });
  await open(undefined, false);
  await waitFor(() => expect((el(p!, "note-title") as HTMLInputElement).value).toBe("Beta"));
  expect(p!.sent.filter((m) => m.command === "setUiState")).toHaveLength(0);
  const alpha = [...p!.doc.querySelectorAll<HTMLButtonElement>(".nt-note-open")].find((b) =>
    b.textContent?.includes("Alpha"),
  )!;
  fireEvent.click(alpha);
  await waitFor(() => expect(p!.sent.filter((m) => m.command === "setUiState")).toHaveLength(1));
  await waitFor(async () =>
    expect((await liveHub.snapshot("ui:right-pane", "all")).snapshot[0]).toMatchObject({
      noteId: "alpha",
    }),
  );
  await liveHub.writeUi("ui:right-pane", "all", { noteId: "beta" });
  await waitFor(() => expect((el(p!, "note-title") as HTMLInputElement).value).toBe("Beta"));
  expect(p!.sent.filter((m) => m.command === "setUiState")).toHaveLength(1);
});
it("lets an explicit reveal intent win over the shared note", async () => {
  await liveHub.writeUi("ui:right-pane", "all", { noteId: "beta" });
  await open({ noteId: "alpha" });
  await waitFor(async () =>
    expect((await liveHub.snapshot("ui:right-pane", "all")).snapshot[0]).toMatchObject({
      noteId: "alpha",
    }),
  );
});

it("uses the real bridge to open native launcher and personal routine drafts with native task text", async () => {
  // The launcher opens only for a team the page may see, as the active team always is in the app.
  useOrgStore.setState({ orgs: [{ slug: ORG, teams: [{ name: TEAM }] }] as never });
  await open();
  await click("agent-menu");
  const menu = el(p!, "float");
  expect([...menu.querySelectorAll("button")].map((b) => b.textContent)).toEqual([
    "Organize with an agent…",
    "Send to a running agent…",
    "Set up a routine…",
    "Changes by agents",
  ]);
  expect(menu.textContent).toContain("This note");
  expect(menu.textContent).toContain("Keep notes organized");
  expect(menu.textContent).toContain(
    "Uses your agents and the normal launch. Results land in your Inbox.",
  );
  fireEvent.click(menu.querySelector("button")!);
  const context = `The note: “Alpha” (note id alpha in org ${ORG}). Read it with notes_read and change it with notes_write (pass that id and org), not by editing its file.`;
  await waitFor(() =>
    expect(useLaunchSheetStore.getState().request).toEqual({
      teamKey: `${ORG}:${TEAM}`,
      task: `Organize this note into clear sections. Keep every useful detail, preserve the author's intent, and turn actionable work into a checklist.\n\n${context}\n\nWhen you're done, post a short summary of what you changed to my inbox.`,
    }),
  );
  await click("agent-menu");
  fireEvent.click(
    [...el(p!, "float").querySelectorAll("button")].find(
      (b) => b.textContent === "Set up a routine…",
    )!,
  );
  await waitFor(() =>
    expect(useRoutinesStore.getState().createSeed).toEqual({
      draft: {
        name: "Tidy my notes",
        task: `Tidy this note: fix spelling and formatting, remove repetition, and improve clarity without changing its meaning.\n\n${context}`,
        when: "weekday 18:00",
      },
      audience: "personal",
    }),
  );
  await click("agent-menu");
  fireEvent.click(
    [...el(p!, "float").querySelectorAll("button")].find(
      (b) => b.textContent === "Send to a running agent…",
    )!,
  );
  await waitFor(() =>
    expect(
      [...el(p!, "float").querySelectorAll("button")].some(
        (b) => b.textContent === "Start a new agent with this",
      ),
    ).toBe(true),
  );
  fireEvent.click(
    [...el(p!, "float").querySelectorAll("button")].find(
      (b) => b.textContent === "Start a new agent with this",
    )!,
  );
  await waitFor(() =>
    expect(useLaunchSheetStore.getState().request?.task).toBe(`Read my note. ${context}`),
  );
});

it("offers the native routine entry from the empty state", async () => {
  raw("delete", { id: "alpha" });
  raw("delete", { id: "beta" });
  await open(undefined, false);
  await waitFor(() => expect(el(p!, "empty-routine").hidden).toBe(false));
  expect(el(p!, "empty-routine").textContent).toContain(
    "Want an agent to keep these tidy every evening?",
  );
  await click("empty-setup-routine");
  await waitFor(() =>
    expect(useRoutinesStore.getState().createSeed).toMatchObject({
      audience: "personal",
      draft: { name: "Tidy my notes" },
    }),
  );
  expect(useAgentsStore.getState().selectedKey).toBeNull();
  expect(useRoutinesStore.getState().creating).toBe(false);
  expect(useRoutinesStore.getState().createSeed?.draft.agent).toBeUndefined();
  act(() => showAgent("personal/chosen"));
  expect(useRoutinesStore.getState()).toMatchObject({
    routineOwner: "personal/chosen",
    creating: true,
    createSeed: { audience: "personal", draft: { name: "Tidy my notes", when: "weekday 18:00" } },
  });
});

it("reports ready and applies intent while the shared UI snapshot is still loading", async () => {
  const snapshot = liveHub.snapshot.bind(liveHub);
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  const spy = vi.spyOn(liveHub, "snapshot").mockImplementation(async (topic, key) => {
    if (topic === "ui:right-pane") await held;
    return snapshot(topic, key);
  });
  try {
    await open({ noteId: "beta" }, false);
    await waitFor(() => expect(p!.sent.some((m) => m.type === "codesense:live")).toBe(true));
    await waitFor(() =>
      expect(
        p!.sent.some(
          (m) => m.command === "subscribe" && (m.args as any)?.topic === "ui:right-pane",
        ),
      ).toBe(true),
    );
    const messages = p!.sent;
    expect(messages.findIndex((m) => m.type === "codesense:live")).toBeLessThan(
      messages.findIndex(
        (m) => m.command === "subscribe" && (m.args as any)?.topic === "ui:right-pane",
      ),
    );
    await waitFor(() => expect((el(p!, "note-title") as HTMLInputElement).value).toBe("Beta"));
  } finally {
    release();
    spy.mockRestore();
  }
});

it("does not publish a selection after teardown interrupts a note open", async () => {
  const original = ipc.getMockImplementation()!;
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let waiting = false;
  ipc.mockImplementation(async (command, args) => {
    if (command === "records_call" && args.operation === "history" && args.args.id === "beta") {
      waiting = true;
      await held;
    }
    return original(command, args);
  });
  const { frame } = await open({ noteId: "beta" }, false);
  await waitFor(() => expect(waiting).toBe(true));
  const tornDown = vi.fn();
  p!.win.codesense.onTeardown(tornDown);
  await deliver(frame, { type: "codesense:teardown" });
  await waitFor(() => expect(tornDown).toHaveBeenCalledTimes(1));
  release();
  await new Promise((resolve) => setTimeout(resolve, 100));
  expect(p!.sent.filter((m) => m.command === "setUiState")).toHaveLength(0);
});

it("keeps the reveal intent ahead of the initial shared callback while Beta history is pending", async () => {
  await liveHub.writeUi("ui:right-pane", "all", { noteId: "alpha" });
  const snapshot = liveHub.snapshot.bind(liveHub);
  let releaseSnapshot!: () => void;
  const snapshotHeld = new Promise<void>((resolve) => {
    releaseSnapshot = resolve;
  });
  const snapshotSpy = vi.spyOn(liveHub, "snapshot").mockImplementation(async (topic, key) => {
    if (topic === "ui:right-pane") await snapshotHeld;
    return snapshot(topic, key);
  });
  let releaseDelivery!: () => void;
  const deliveryHeld = new Promise<void>((resolve) => {
    releaseDelivery = resolve;
  });
  let delivered = false;
  const original = ipc.getMockImplementation()!;
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let waiting = false;
  ipc.mockImplementation(async (command, args) => {
    if (command === "records_call" && args.operation === "history" && args.args.id === "beta") {
      waiting = true;
      await held;
    }
    return original(command, args);
  });
  try {
    await open({ noteId: "beta" }, false);
    const subscribe = p!.win.codesense.subscribe;
    p!.win.codesense.subscribe = async (topic, options) => {
      const result = await subscribe(topic, options);
      if (topic === "ui:right-pane") {
        delivered = true;
        await deliveryHeld;
      }
      return result;
    };
    await waitFor(() => expect(waiting).toBe(true));
    await waitFor(() =>
      expect(
        p!.sent.some(
          (m) => m.command === "subscribe" && (m.args as any)?.topic === "ui:right-pane",
        ),
      ).toBe(true),
    );
    releaseSnapshot();
    await waitFor(() => expect(delivered).toBe(true));
    await act(async () => new Promise((resolve) => setTimeout(resolve, 80)));
    expect((el(p!, "note-title") as HTMLInputElement).value).toBe("Beta");
  } finally {
    release();
    releaseSnapshot();
    releaseDelivery();
    snapshotSpy.mockRestore();
  }
  await waitFor(async () =>
    expect((await liveHub.snapshot("ui:right-pane", "all")).snapshot[0]).toMatchObject({
      noteId: "beta",
    }),
  );
});

it("applies the shipped Notes recipe and keeps pin metadata, row identity and refresh live", async () => {
  await open(undefined, true, recipePane("notes"));
  expect([...p!.doc.querySelectorAll(".nt-group")].map((row) => row.textContent)).toEqual([
    "Pinned (1)",
    "Recent",
  ]);
  const identity = p!.doc.querySelector('[data-note-id="alpha"]')?.getAttribute("data-note-id");
  await click("note-menu");
  await act(async () =>
    [...p!.doc.querySelectorAll<HTMLButtonElement>("#float button")]
      .find((row) => row.textContent === "Unpin")!
      .click(),
  );
  await waitFor(() => expect(raw("read", { id: "alpha" }).fields.pinned).toBe(false));
  await waitFor(() => expect(p!.doc.querySelector(".nt-group")?.textContent).toBe("Recent"));
  await act(async () => raw("setFields", { id: "beta", fields: { pinned: true } }));
  await waitFor(() => expect(p!.doc.querySelector(".nt-group")?.textContent).toBe("Pinned (1)"));
  expect(p!.doc.querySelector('[data-note-id="alpha"]')?.getAttribute("data-note-id")).toBe(
    identity,
  );
  expect(p!.doc.querySelectorAll(".nt-note-row")).toHaveLength(2);
});
