// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { createElement, useContext, useLayoutEffect } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { fileSurfaceCall } from "@/tauri/commands";
import { frameFixture } from "../../../../tests/support/fixtures/frame";
import { type HubRow, LiveHub } from "../lib/live-hub";
import { runBridgeCommand } from "./html-bridge";
import { HostMenuLayer } from "@/features/shell/host-menu-layer";
import { HostMenuContext, type HostMenuRequest } from "@/features/shell/host-menu-context";
import { NavFileActionsOwner } from "@/features/dashboard/components/nav-file-actions-owner";
import { buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { readSharedUiState } from "../lib/shared-ui-state";

const base = "team:acme/backend";
const filesKeyLayout = vi.hoisted(() => ({ trustedFields: ["/content", "human"] as unknown[] }));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  fileSurfaceCall: vi.fn(),
  fileSurfacePrepareView: vi.fn(
    async (_frame, args) =>
      `team:acme/backend|files:${JSON.stringify([args.scope || "team", args.team || null, args.storyId || null, args.agentId || null, args.dir || null, ...filesKeyLayout.trustedFields])}`,
  ),
  loadAppState: vi.fn(async () => "{}"),
  saveAppState: vi.fn(async () => {}),
}));
vi.mock("@/features/git-sync/lib/use-nav-source-counts", () => ({
  useNavSourceCounts: () => null,
}));
let hub: LiveHub;
let teardown: (() => void) | undefined;
let hostMenu: ((request: HostMenuRequest) => Promise<unknown>) | undefined;
let presentedHostRequest: HostMenuRequest | undefined;
afterEach(() => {
  cleanup();
  teardown?.();
  hub?.dispose();
  hostMenu = undefined;
  presentedHostRequest = undefined;
  document.body.replaceChildren();
  vi.unstubAllGlobals();
});

async function bootSeam(
  rejectChild = false,
  rowsFor?: (dir: string) => HubRow[] | Promise<HubRow[]>,
  initialUi: Record<string, unknown> = { expanded: [], scope: "team" },
) {
  filesKeyLayout.trustedFields = ["/content", "human"];
  const html = readFileSync("src/features/shell/slots/builtin/files-tree.html", "utf8");
  document.body.innerHTML = html.slice(html.indexOf("<body>") + 6, html.indexOf("<script>"));
  const dirs = [
    "",
    "/content/one",
    "/content/one/two",
    "/content/one/two/three",
    "/content/one/two/three/four",
  ];
  let rejected = false;
  const read = vi.fn(
    async (
      topic: string,
      viewKey: string,
      _since?: number,
      _readOptions?: { subscriptionId: string },
    ) => {
      const dir = JSON.parse(viewKey.slice(viewKey.indexOf("|files:") + 7))[4] || "";
      const level = dirs.indexOf(dir);
      if (rejectChild && level === 2 && !rejected) {
        rejected = true;
        throw new Error("Couldn't read directory");
      }
      const path = dirs[level + 1] || dir + "/leaf.md";
      return {
        topic,
        viewKey,
        rev: 1,
        snapshot: rowsFor
          ? await rowsFor(dir)
          : [
              {
                id: path,
                path,
                name: path.split("/").pop(),
                kind: level < 4 ? "dir" : "file",
                root: "/content",
                relativePath: path.slice(9),
              },
            ],
      };
    },
  );
  let saved = initialUi;
  const readUi = vi.fn(async () => saved);
  const events = new Map<string, (event: { payload: unknown }) => void>();
  const unsubscribe = vi.fn(async () => {});
  hub = new LiveHub({
    unsubscribe,
    read,
    listen: async (name, callback) => {
      events.set(name, callback);
      return () => {};
    },
    readUi,
    writeUi: async (_, patch) => (saved = { ...saved, ...patch }),
  });
  const frame = frameFixture({ documentPath: "/pane.html", contentRoot: "/", slot: "file-nav" });
  const menuFrame = document.createElement("iframe");
  document.body.append(menuFrame);
  let slotChanged: (context: unknown) => void = () => {};
  (window as any).__codesenseProxy = { token: frame.token };
  const live = hub.attach(
    frame.frameId,
    base,
    (data) =>
      window.dispatchEvent(
        new MessageEvent("message", {
          source: window.parent,
          origin: "tauri://localhost",
          data: { token: frame.token, ...data },
        }),
      ),
    true,
  );
  const cs = {
    invoke: vi.fn((verb: string, args: unknown) =>
      runBridgeCommand(verb, args, { frame, liveFrame: async () => live }),
    ),
    showMenu: (args: unknown) =>
      runBridgeCommand("showMenu", args, {
        frame,
        liveFrame: async () => live,
        presentation: (_verb, request) => {
          if (!hostMenu) throw new Error("host menu layer is not mounted");
          presentedHostRequest = {
            ...(request as Partial<HostMenuRequest>),
            type: (request as Partial<HostMenuRequest>).type ?? "menu",
            frame: menuFrame,
            origin: "tauri://localhost",
            teamKey: "acme:backend",
            contentRoot: "/content",
            run: async () => {},
          } as HostMenuRequest;
          return hostMenu(presentedHostRequest);
        },
      }),
    onVisibility: (callback: (visible: boolean) => void) => {
      callback(true);
      return () => {};
    },
    onInboxChanged: () => () => {},
    commands: { register: async () => {} },
    onCommand: () => {},
    onFocusIn: () => {},
    onFlush: () => {},
    onTeardown: (callback: () => void) => {
      teardown = callback;
    },
    onSlotContext: (callback: typeof slotChanged) => {
      slotChanged = callback;
    },
    theme: async () => {},
    ready: () => {},
  };
  const invoke = cs.invoke;
  (window as any).codesense = cs;
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  for (const file of [
    "src-tauri/src/http/routes/live_hub.js",
    "src-tauri/src/http/routes/dashboard_kit/kit.js",
    "src-tauri/src/http/routes/dashboard_kit/virtual-list.js",
  ])
    window.eval(readFileSync(file, "utf8"));
  window.eval(html.slice(html.indexOf("<script>") + 8, html.indexOf("</script>")));
  expect(readUi).not.toHaveBeenCalled();
  expect(read).not.toHaveBeenCalled();
  slotChanged({ team: { org: "acme", team: "backend" } });
  return {
    dirs,
    read,
    readUi,
    unsubscribe,
    invoke,
    select: (path: string) => live.writeUi("ui:file-nav", { selected: path }),
    delta: (dir: string, upserts: HubRow[], removes: string[], rev = 2) => {
      const viewKey = `team:acme/backend|files:${JSON.stringify(["team", null, null, null, dir || null, "/content", "human"])}`;
      const subscriptionId =
        read.mock.calls.find(([, key]) => key === viewKey)?.[3]?.subscriptionId ||
        "missing-nested-view";
      events.get("codesense:subscription-delta")!({
        payload: {
          subscriptionId,
          baseRev: rev - 1,
          delta: { topic: "files", viewKey, rev, upserts, removes },
        },
      });
    },
    resync: (dir: string, trustedFields?: unknown[]) => {
      const preparedKey = read.mock.calls[0][1];
      const [base, serialized] = preparedKey.split("|files:");
      const pageFields = JSON.parse(serialized).slice(0, 5);
      pageFields[4] = dir || null;
      if (trustedFields) filesKeyLayout.trustedFields = trustedFields;
      const viewKey = `${base}|files:${JSON.stringify([...pageFields, ...(trustedFields || JSON.parse(serialized).slice(5))])}`;
      window.dispatchEvent(
        new MessageEvent("message", {
          source: window.parent,
          origin: "tauri://localhost",
          data: { token: frame.token, type: "codesense:resync", topic: "files", viewKey },
        }),
      );
    },
    events,
  };
}

function HostMenuCapture() {
  const showMenu = useContext(HostMenuContext);
  useLayoutEffect(() => {
    hostMenu = showMenu;
  }, [showMenu]);
  return null;
}

// The nav owner stays mounted when the Files dashboard replaces the native body.
function NavOwnerWithoutBody() {
  return createElement(NavFileActionsOwner, {
    teamKey: "acme:backend",
    tree: buildTree("", { absoluteDirPath: "/content", stories: [] }),
    repoPath: "/content",
    selectedPath: null,
    onFileSelect: () => {},
    renderNative: () => null,
  });
}

it("opens the native filter from the pane button and writes its lens to shared ui:file-nav state", async () => {
  await bootSeam();
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "backend" });
  useViewerStore.setState({ navLens: {} });
  render(
    createElement(
      HostMenuLayer,
      null,
      createElement(NavOwnerWithoutBody),
      createElement(HostMenuCapture),
    ),
  );

  await waitFor(() => expect(hostMenu).toBeTypeOf("function"));
  fireEvent.click(document.getElementById("view")!);

  const done = await screen.findByTestId("left-nav-lens-done");
  expect(presentedHostRequest).toMatchObject({
    type: "native",
    nativeSurface: "nav-filter",
    anchorId: "view",
    row: { kind: "nav-filter" },
  });
  fireEvent.click(done);

  await waitFor(() => expect(useViewerStore.getState().navLens["acme:backend"]?.lens).toBe("done"));
  await expect(readSharedUiState(JSON.stringify(["ui:file-nav", base]))).resolves.toMatchObject({
    lens: "done",
  });
});

it.each([false, true])(
  "expands four levels through the shipped SDK, real bridge and LiveHub (rejected child=%s)",
  async (rejectChild) => {
    const { dirs, read, readUi } = await bootSeam(rejectChild);
    for (const dir of dirs.slice(1, rejectChild ? 3 : undefined)) {
      await vi.waitFor(() => expect(document.querySelector(`[data-path="${dir}"]`)).toBeTruthy());
      document.querySelector<HTMLElement>(`[data-path="${dir}"]`)!.click();
    }
    if (rejectChild) {
      await vi.waitFor(() =>
        expect(document.querySelector(".file-row.error")?.textContent).toContain("Couldn't load"),
      );
      expect(document.querySelector(".skeleton")).toBeNull();
      document.querySelector<HTMLButtonElement>(".file-row.error button")!.click();
      for (const dir of dirs.slice(3)) {
        await vi.waitFor(() => expect(document.querySelector(`[data-path="${dir}"]`)).toBeTruthy());
        document.querySelector<HTMLElement>(`[data-path="${dir}"]`)!.click();
      }
    }
    await vi.waitFor(() =>
      expect(
        document.querySelector('[data-path="/content/one/two/three/four/leaf.md"]'),
      ).toBeTruthy(),
    );
    expect(new Set(read.mock.calls.map(([, key]) => key)).size).toBe(5);
    expect(readUi).toHaveBeenCalledWith('["ui:file-nav","team:acme/backend"]');
  },
);

it("restores expanded folders and loads their children through the shipped SDK before ready", async () => {
  const expanded = [
    "/content/one",
    "/content/one/two",
    "/content/one/two/three",
    "/content/one/two/three/four",
  ];
  const { read, dirs } = await bootSeam(false, undefined, { expanded, scope: "team" });
  await vi.waitFor(() =>
    expect(
      document.querySelector('[data-path="/content/one/two/three/four/leaf.md"]'),
    ).toBeTruthy(),
  );
  expect(new Set(read.mock.calls.map(([, key]) => key)).size).toBe(dirs.length);
});

it("refreshes only the matching files view when a seven-field Rust resync arrives", async () => {
  let refreshed = false;
  const pane = await bootSeam(false, () => [
    directory(refreshed ? "/content/two" : "/content/one"),
  ]);
  await vi.waitFor(() => expect(document.querySelector('[data-path="/content/one"]')).toBeTruthy());
  const before = pane.read.mock.calls.length;

  pane.resync("/content/other");
  await new Promise((resolve) => setTimeout(resolve, 30));
  expect(pane.read).toHaveBeenCalledTimes(before);

  refreshed = true;
  pane.resync("", ["/new-content", "human:new-cache-key"]);
  await vi.waitFor(() => expect(pane.read.mock.calls.length).toBeGreaterThan(before));
  await vi.waitFor(() => expect(document.querySelector('[data-path="/content/two"]')).toBeTruthy());
  expect(pane.read.mock.calls.length).toBe(before + 1);
});

it("discovers a matching leaf below unopened nonmatching ancestors through live SDK subscriptions and the real bridge", async () => {
  const dirs = ["", "/content/one", "/content/one/two", "/content/one/two/three"];
  const rows = (dir: string) => {
    const index = dirs.indexOf(dir);
    return index < 3 ? [directory(dirs[index + 1])] : [file(dir + "/meeting.md")];
  };
  const pane = await bootSeam(false, rows);
  await vi.waitFor(() => expect(document.querySelector('[data-path="/content/one"]')).toBeTruthy());
  query("meeting");
  expect(document.getElementById("empty")?.textContent).not.toContain('Nothing matches "meeting"');
  await vi.waitFor(() =>
    expect(document.querySelector('[data-path="/content/one/two/three/meeting.md"]')).toBeTruthy(),
  );
  expect(
    pane.read.mock.calls.map(
      ([, key]) => JSON.parse(key.slice(key.indexOf("|files:") + 7))[4] || "",
    ),
  ).toEqual(dirs);
});
it("bounds live filter discovery to two loads and completes a changed query after retiring pending reads", async () => {
  const pending: Array<{ dir: string; resolve: (rows: HubRow[]) => void }> = [];
  let inFlight = 0;
  let peak = 0;
  const rows = async (dir: string): Promise<HubRow[]> => {
    if (!dir) return [directory("/content/one")];
    if (dir === "/content/one") return ["a", "b", "c"].map((name) => directory(dir + "/" + name));
    inFlight++;
    peak = Math.max(peak, inFlight);
    try {
      return await new Promise((resolve) => pending.push({ dir, resolve }));
    } finally {
      inFlight--;
    }
  };
  const pane = await bootSeam(false, rows);
  await vi.waitFor(() => expect(document.querySelector('[data-path="/content/one"]')).toBeTruthy());
  query("meeting");
  await vi.waitFor(() => expect(pending).toHaveLength(2));
  query("changed-query");
  expect(pending).toHaveLength(2);
  pending
    .slice(0, 2)
    .forEach(({ dir, resolve }) => resolve([file(dir + "/changed-query-stale.md")]));
  await vi.waitFor(() => expect(pending).toHaveLength(4));
  expect(document.querySelector('[data-path$="changed-query-stale.md"]')).toBeNull();
  pending[2].resolve([file(pending[2].dir + "/changed-query.md")]);
  pending[3].resolve([]);
  await vi.waitFor(() => expect(pending).toHaveLength(5));
  pending[4].resolve([]);
  await vi.waitFor(() =>
    expect(document.querySelector('[data-path$="/changed-query.md"]')).toBeTruthy(),
  );
  expect(peak).toBe(2);
  query("");
  await vi.waitFor(() => expect(pane.unsubscribe).toHaveBeenCalled());
  const count = pane.read.mock.calls.length;
  await new Promise((resolve) => setTimeout(resolve, 30));
  expect(pane.read.mock.calls).toHaveLength(count);
});

const directory = (path: string): HubRow => ({
  id: path,
  path,
  name: path.split("/").pop()!,
  kind: "dir",
  root: "/content",
});
const file = (path: string): HubRow => ({ ...directory(path), kind: "file" });
function query(value: string) {
  const field = document.getElementById("filter") as HTMLInputElement;
  field.value = value;
  field.dispatchEvent(new Event("input"));
}
it("keeps nested filtered results live across rename, add and remove deltas without new input", async () => {
  const notes = "/content/one/notes";
  const meeting = file(notes + "/meeting.md");
  const rows = (dir: string) =>
    dir === ""
      ? [directory("/content/one")]
      : dir === "/content/one"
        ? [directory(notes)]
        : [meeting];
  vi.mocked(fileSurfaceCall).mockImplementation(async (_operation, args: any) => ({
    rows: rows(args.dir),
  }));
  const pane = await bootSeam(false, rows);
  await vi.waitFor(() => expect(document.querySelector('[data-path="/content/one"]')).toBeTruthy());
  query("meeting");
  await vi.waitFor(() =>
    expect(document.querySelector(`[data-path="${meeting.path}"]`)).toBeTruthy(),
  );
  const renamed = file(notes + "/meeting-updated.md");
  const added = file(notes + "/meeting-extra.md");
  pane.delta(notes, [renamed, added], [meeting.id]);
  await vi.waitFor(() =>
    expect(document.querySelector(`[data-path="${renamed.path}"]`)).toBeTruthy(),
  );
  expect(document.querySelector(`[data-path="${meeting.path}"]`)).toBeNull();
  expect(document.querySelector(`[data-path="${added.path}"]`)).toBeTruthy();
  pane.delta(notes, [], [renamed.id], 3);
  await vi.waitFor(() =>
    expect(document.querySelector(`[data-path="${renamed.path}"]`)).toBeNull(),
  );
  expect(document.querySelector(`[data-path="${added.path}"]`)).toBeTruthy();
});
it("settles subscription counts when a filtered-out nested tab is selected across cache acknowledgements", async () => {
  const notes = "/content/one/notes";
  const rows = (dir: string) =>
    dir === ""
      ? [directory("/content/one")]
      : dir === "/content/one"
        ? [directory(notes)]
        : [file(notes + "/meeting.md"), file(notes + "/other.md")];
  vi.mocked(fileSurfaceCall).mockImplementation(async (_operation, args: any) => ({
    rows: rows(args.dir),
  }));
  const pane = await bootSeam(false, rows);
  await vi.waitFor(() => expect(document.querySelector('[data-path="/content/one"]')).toBeTruthy());
  query("meeting");
  await vi.waitFor(() =>
    expect(document.querySelector(`[data-path="${notes}/meeting.md"]`)).toBeTruthy(),
  );
  await pane.select(notes + "/other.md");
  await new Promise((resolve) => setTimeout(resolve, 100));
  const requests = () =>
    pane.invoke.mock.calls.filter(([verb]) => verb === "subscribe" || verb === "unsubscribe")
      .length;
  const settled = requests();
  await new Promise((resolve) => setTimeout(resolve, 900));
  expect(requests()).toBe(settled);
  expect(pane.read.mock.calls.length).toBeLessThanOrEqual(3);
  expect(document.querySelector(`[data-path="${notes}/other.md"]`)).toBeNull();
});
