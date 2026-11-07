// SPDX-License-Identifier: MIT
import { useContext, useLayoutEffect } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { HostMenuLayer } from "./host-menu-layer";
import { HostMenuContext, type HostMenuRequest } from "./host-menu-context";
vi.mock("./host-agent-menu", () => ({
  useHostAgentMenu: (id: unknown) =>
    id === "missing"
      ? null
      : {
          row: { sessionId: "agent", handle: "handle", alive: true },
          offlineMachine: null,
          tab: { tab: { id: "tab" } },
          choice: undefined,
          choose: vi.fn(),
        },
  HostAgentRowMenu: () => <div>Native row menu</div>,
  HostAgentPopover: ({ onClose }: { onClose: () => void }) => (
    <button onClick={onClose}>Close quick send</button>
  ),
  HostAgentInboxChat: ({ onClose }: { onClose: () => void }) => (
    <button onClick={onClose}>Close inbox chat</button>
  ),
}));
afterEach(() => {
  cleanup();
  document.querySelectorAll("iframe").forEach((frame) => frame.remove());
  vi.restoreAllMocks();
});
let show: (request: HostMenuRequest) => Promise<unknown>;
function Harness() {
  const request = useContext(HostMenuContext);
  useLayoutEffect(() => {
    show = request;
  }, [request]);
  return null;
}
function setup() {
  const frame = document.createElement("iframe");
  document.body.append(frame);
  const post = vi.fn();
  Object.defineProperty(frame, "contentWindow", { value: { postMessage: post } });
  const run = vi.fn().mockResolvedValue({ status: "cancelled" });
  render(
    <HostMenuLayer>
      <Harness />
    </HostMenuLayer>,
  );
  return {
    frame,
    post,
    run,
    request: {
      type: "menu" as const,
      frame,
      origin: "http://frame.test",
      anchorId: "more",
      anchorRect: { x: 10, y: 20, width: 30, height: 20 },
      row: { kind: "agent", sessionId: "agent" },
      run,
    },
  };
}
it("resolves a dashboard pick with its row and returns focus to its anchor", async () => {
  const { request, post } = setup();
  let answer!: Promise<unknown>;
  act(() => {
    answer = show({ ...request, items: [{ id: "inspect", label: "Inspect" }] });
  });
  fireEvent.click(await screen.findByRole("menuitem", { name: "Inspect" }));
  await expect(answer).resolves.toEqual({ itemId: "inspect", row: request.row });
  expect(post).toHaveBeenCalledWith(
    { type: "codesense:focus-return", anchorId: "more" },
    "http://frame.test",
  );
});
it("dismisses the previous menu when a second opens", async () => {
  const { request } = setup();
  let first!: Promise<unknown>;
  act(() => {
    first = show({ ...request, items: [{ id: "first", label: "First" }] });
  });
  await screen.findByRole("menuitem", { name: "First" });
  act(() => {
    void show({ ...request, items: [{ id: "second", label: "Second" }] });
  });
  await expect(first).resolves.toEqual({ closed: true });
  expect(await screen.findByRole("menuitem", { name: "Second" })).toBeInTheDocument();
});
it("rejects unknown built-ins, including relaunch", async () => {
  const { request } = setup();
  await expect(
    show({ ...request, items: [{ id: "builtin:relaunch", label: "Again" }] }),
  ).rejects.toThrow('unknown built-in "builtin:relaunch"');
});
it("uses the trusted Stop label and waits for the host action to settle", async () => {
  const { request, run, post } = setup();
  let done!: (answer: unknown) => void;
  run.mockReturnValue(
    new Promise((resolve) => {
      done = resolve;
    }),
  );
  let answer!: Promise<unknown>;
  act(() => {
    answer = show({
      ...request,
      items: [{ id: "builtin:stop", label: "Custom Stop", icon: "zap" }],
    });
  });
  const stop = await screen.findByRole("menuitem", { name: "Stop…" });
  expect(stop).toHaveAttribute("data-variant", "destructive");
  expect(screen.queryByText("Custom Stop")).toBeNull();
  fireEvent.click(stop);
  expect(run).toHaveBeenCalledWith("stopSession", { handle: "handle" }, { confirmInHost: true });
  expect(post).not.toHaveBeenCalled();
  await act(async () => done({ status: "cancelled" }));
  await expect(answer).resolves.toEqual({
    itemId: "builtin:stop",
    row: request.row,
    builtin: true,
  });
});
it("does not turn a dashboard item labelled Stop into a built-in", async () => {
  const { request, run } = setup();
  let answer!: Promise<unknown>;
  act(() => {
    answer = show({ ...request, items: [{ id: "plain", label: "Stop" }] });
  });
  fireEvent.click(await screen.findByRole("menuitem", { name: "Stop" }));
  await expect(answer).resolves.toEqual({ itemId: "plain", row: request.row });
  expect(run).not.toHaveBeenCalled();
});

it("opens the inbox chat popover for a page and returns focus to its anchor on close", async () => {
  const { request, post } = setup();
  let answer!: Promise<unknown>;
  await act(async () => {
    answer = show({ ...request, type: "popover", kind: "agent-inbox-chat", sessionId: "agent" });
  });
  fireEvent.click(await screen.findByRole("button", { name: "Close inbox chat" }));
  await expect(answer).resolves.toEqual({ closed: true });
  expect(post).toHaveBeenCalledWith(
    { type: "codesense:focus-return", anchorId: "more" },
    "http://frame.test",
  );
  await expect(show({ ...request, type: "popover", kind: "agent-relaunch" })).rejects.toThrow(
    'unknown popover "agent-relaunch"',
  );
});

it("an unresolved agent gets dashboard built-ins rather than an empty menu", async () => {
  const { request } = setup();
  const reload = vi.fn();
  act(() => {
    void show({
      ...request,
      type: "default",
      row: { kind: "agent", sessionId: "missing" },
      dashboard: { reload },
    });
  });
  fireEvent.click(await screen.findByRole("menuitem", { name: "Reload pane" }));
  expect(reload).toHaveBeenCalledOnce();
  expect(screen.queryByText("Edit source")).toBeNull();
});
it("a frame reload cancels its menu without returning focus into the new document", async () => {
  const { request, post } = setup();
  const lifetime = new AbortController();
  let result!: Promise<unknown>;
  act(() => {
    result = show({ ...request, signal: lifetime.signal, items: [{ id: "item", label: "Item" }] });
  });
  await screen.findByRole("menuitem", { name: "Item" });
  act(() => lifetime.abort());
  await expect(result).resolves.toEqual({ closed: true });
  expect(post).not.toHaveBeenCalled();
  expect(screen.queryByRole("menu")).toBeNull();
});

it("routes a keyboard file action straight to the native owner without a second menu", async () => {
  const native = await import("./native-menu-openers");
  const open = vi.spyOn(native, "openNativeMenu").mockReturnValue(() => {});
  const { request } = setup();
  const row = { kind: "file", path: "/team/file.md" };
  let answer!: Promise<unknown>;
  act(() => {
    answer = show({ ...request, teamKey: "acme:team", row, builtinAction: "builtin:rename" });
  });
  await expect(answer).resolves.toEqual({ itemId: "builtin:rename", row, builtin: true });
  expect(open).toHaveBeenCalledWith(
    expect.objectContaining({ action: "rename", row, teamKey: "acme:team" }),
  );
  expect(screen.queryByRole("menu")).toBeNull();
});

it("routes builtin:new-file through the registered native file dialog owner", async () => {
  const native = await import("./native-menu-openers");
  const open = vi.spyOn(native, "openNativeMenu").mockReturnValue(() => {});
  const { request } = setup();
  let answer!: Promise<unknown>;
  act(() => {
    answer = show({
      ...request,
      row: undefined,
      teamKey: "acme:team",
      contentRoot: "/team/content",
      builtinAction: "builtin:new-file",
    });
  });
  await expect(answer).resolves.toMatchObject({ itemId: "builtin:new-file", builtin: true });
  expect(open).toHaveBeenCalledWith(
    expect.objectContaining({ action: "new-file", row: { path: "/team/content", kind: "folder" } }),
  );
});

it("routes explicit native surfaces to their registered menu opener", async () => {
  const native = await import("./native-menu-openers");
  let requestSeen: Parameters<typeof native.openNativeMenu>[0] | undefined;
  vi.spyOn(native, "openNativeMenu").mockImplementation((request) => {
    requestSeen = request;
    return () => {};
  });
  const { request } = setup();
  let answer!: Promise<unknown>;
  act(() => {
    answer = show({
      ...request,
      type: "native",
      nativeSurface: "nav-filter",
      row: { kind: "nav-filter" },
    });
  });
  await waitFor(() => expect(requestSeen).toBeDefined());
  expect(requestSeen?.surface).toBe("nav-filter");
  expect(screen.queryByRole("menu")).toBeNull();
  requestSeen?.onClose();
  await expect(answer).resolves.toEqual({ closed: true });
});

it("shows native glyphs and counts for radio groups", async () => {
  const { request } = setup();
  act(() => {
    void show({
      ...request,
      items: [
        {
          id: "ready",
          label: "Ready to start",
          icon: { kind: "stageRail", role: "not_started", ready: true },
          count: 12,
          radioGroup: "lens",
          checked: true,
        },
        {
          id: "needs",
          label: "Needs you",
          icon: { kind: "statusMark", mark: "question", size: 6, still: true },
          count: 2,
          radioGroup: "lens",
          checked: false,
        },
      ],
    });
  });
  const ready = await screen.findByRole("menuitemradio", { name: "Ready to start 12" });
  expect(ready).toHaveAttribute("aria-checked", "true");
  expect(ready.querySelector('[data-rail="ready"]')).not.toBeNull();
  expect(
    (await screen.findByRole("menuitemradio", { name: "Needs you 2" })).querySelector(
      '[data-mark="question"]',
    ),
  ).not.toBeNull();
  // The menu's svg:size-4 rule must not resize a native glyph's intrinsic SVG.
  expect(ready.querySelector('[data-rail="ready"]')?.closest("span.contents")?.className).toContain(
    "size-auto",
  );
  const mark = screen
    .getByRole("menuitemradio", { name: "Needs you 2" })
    .querySelector('[data-mark="question"]');
  expect(mark).toHaveStyle({ width: "6px", height: "6px" });
  expect(mark?.parentElement).toHaveClass("cs-mark-still");
});
it("returns a segmented option's id through the usual menu result", async () => {
  const { request } = setup();
  let result!: Promise<unknown>;
  act(() => {
    result = show({
      ...request,
      items: [
        {
          id: "arrange",
          label: "Arrange",
          segmented: [
            { id: "folders", label: "Folders", checked: true },
            { id: "stage", label: "Stage", checked: false },
          ],
        },
      ],
    });
  });
  fireEvent.click(await screen.findByRole("menuitemradio", { name: "Stage" }));
  await expect(result).resolves.toEqual({ itemId: "stage", row: request.row });
});

it.each([
  ["folders", "stage"],
  ["stage", "folders"],
])("shows %s as the checked segment with its own selected style", async (checked, other) => {
  const { request } = setup();
  act(() => {
    void show({
      ...request,
      items: [
        {
          id: "arrange",
          label: "Arrange",
          segmented: [
            { id: "folders", label: "Folders", checked: checked === "folders" },
            { id: "stage", label: "Stage", checked: checked === "stage" },
          ],
        },
      ],
    });
  });
  const name = (id: string) => (id === "folders" ? "Folders" : "Stage");
  const on = await screen.findByRole("menuitemradio", { name: name(checked) });
  const off = screen.getByRole("menuitemradio", { name: name(other) });
  expect(on).toHaveAttribute("data-state", "checked");
  expect(off).toHaveAttribute("data-state", "unchecked");
  // Visible selection must not depend on keyboard focus: a checked-state style on every segment.
  expect(on.className).toContain("data-[state=checked]:bg-accent");
  expect(on.className).toContain("data-[state=checked]:font-medium");
});

it.each([
  {
    items: [{ id: "actions", segmented: [{ id: "builtin:rename", label: "Custom", icon: "zap" }] }],
  },
  {
    items: [
      { id: "other", radioGroup: "action" },
      { id: "builtin:delete", radioGroup: "action", label: "Custom" },
    ],
  },
])("refuses reserved built-in actions in grouped choices", async ({ items }) => {
  const { validateMenuItems } = await import("./builtin-menu-items");
  expect(() => validateMenuItems(items)).toThrow(/built-in.*group/i);
  const { request } = setup();
  await expect(show({ ...request, items })).rejects.toThrow(/built-in.*group/i);
  expect(screen.queryByText("Custom")).toBeNull();
});

it("keeps an enabled radio sibling selectable when the first choice is disabled", async () => {
  const { request } = setup();
  let answer!: Promise<unknown>;
  act(() => {
    answer = show({
      ...request,
      items: [
        { id: "unavailable", label: "Unavailable worktree", enabled: false, radioGroup: "source" },
        { id: "team", label: "Team", enabled: true, radioGroup: "source" },
      ],
    });
  });
  expect(
    await screen.findByRole("menuitemradio", { name: "Unavailable worktree" }),
  ).toHaveAttribute("data-disabled");
  const team = await screen.findByRole("menuitemradio", { name: "Team" });
  expect(team).not.toHaveAttribute("data-disabled");
  fireEvent.click(team);
  await expect(answer).resolves.toEqual({ itemId: "team", row: request.row });
});
