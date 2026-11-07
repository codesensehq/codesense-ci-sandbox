// SPDX-License-Identifier: MIT
import { useCallback, useRef, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { frameFixture } from "../../../../tests/support/fixtures/frame";
import type { FrameRecord, MintFrameArgs } from "@/tauri/commands";
import { useTerminalDragDrop } from "@/shared/hooks/use-terminal-drag-drop";
import { useNavFileDrop } from "../hooks/use-nav-file-drop";
import { acknowledgeFlush, retireAfterFlush } from "../lib/frame-lifecycle";
import { HtmlViewer } from "./html-viewer";

const { ipc, handlers } = vi.hoisted(() => ({
  ipc: vi.fn(),
  handlers: new Set<(event: { payload: unknown }) => void>(),
}));
vi.mock("@tauri-apps/api/core", async (original) => ({
  ...(await original<typeof import("@tauri-apps/api/core")>()),
  invoke: (command: string, args: unknown) => ipc(command, args),
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: (handler: (event: { payload: unknown }) => void) => {
      handlers.add(handler);
      return Promise.resolve(() => handlers.delete(handler));
    },
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}), emit: vi.fn() }));

const ROOT = "/ctx/content";
const paths = ["/Users/me/first file.txt", "/Users/me/second.png"];
const records = new Map<string, FrameRecord>();
let granted = true;
let topmost: Element | null;
beforeEach(() => {
  handlers.clear();
  records.clear();
  granted = true;
  topmost = null;
  ipc.mockImplementation(async (command: string, args: { args?: MintFrameArgs }) => {
    if (command === "get_http_api_port") return 9123;
    if (command === "mint_frame") {
      const frameId = `drop-${records.size}`;
      const record = frameFixture(args.args!);
      Object.assign(record, {
        frameId,
        origin: `http://f-${frameId}.localhost:9123`,
        token: frameId,
        grant: { ...record.grant, frameId },
        effectiveGrant: granted ? ["files:read"] : [],
      });
      records.set(record.origin, record);
      return record;
    }
    if (command === "live_hub_view_key") return "all";
    return null;
  });
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ status: 200 })),
  );
  Object.defineProperty(document, "elementFromPoint", {
    configurable: true,
    value: () => topmost,
  });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function viewer(
  name: string,
  opts: { visible?: boolean; centre?: boolean; slotId?: "file-nav" } = {},
) {
  return (
    <HtmlViewer
      filePath={`${ROOT}/${name}.html`}
      teamContentPath={ROOT}
      pane={
        opts.centre
          ? undefined
          : { contentRoot: ROOT, panesDir: ROOT, fileName: `${name}.html`, slotId: opts.slotId }
      }
      visible={opts.visible}
    />
  );
}
async function endpoint(name: string, ready = true) {
  let frame: HTMLIFrameElement;
  await waitFor(() => {
    frame = Array.from(document.querySelectorAll("iframe")).find((f) =>
      f.src.includes(`${name}.html`),
    )!;
    expect(frame?.src).toContain(".localhost:9123");
  });
  const iframe = frame!;
  iframe.getBoundingClientRect = () => ({ left: 0, top: 0, right: 100, bottom: 100 }) as DOMRect;
  const origin = new URL(iframe.src).origin;
  // happy-dom disables iframe loading, so supply the cross-origin window endpoint.
  const post = vi.fn();
  Object.defineProperty(iframe, "contentWindow", {
    value: { postMessage: post } as unknown as Window,
    configurable: true,
  });
  const announceReady = async () =>
    act(async () => {
      window.dispatchEvent(
        new MessageEvent("message", {
          source: iframe.contentWindow,
          origin,
          data: { type: "codesense:ready", token: records.get(origin)!.token },
        }),
      );
    });
  if (ready) await announceReady();
  const messages = () =>
    post.mock.calls.filter(
      ([data]) => data.type === "codesense:file-drag" || data.type === "codesense:file-drop",
    );
  return { iframe, origin, post, messages, announceReady };
}
async function native(type: "enter" | "over" | "leave" | "drop", target: Element | null) {
  topmost = target;
  await act(async () => {
    const payload =
      type === "leave"
        ? { type }
        : {
            type,
            position: { x: 20, y: 20 },
            ...(type !== "over" ? { paths } : {}),
          };
    for (const handler of handlers) handler({ payload });
  });
}
const drag = (phase: string) => ({ type: "codesense:file-drag", phase });

function NativeTarget({
  kind,
  onDrop,
  children,
}: {
  kind: "nav" | "terminal";
  onDrop: (...args: unknown[]) => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const visible = useRef(true);
  const disposed = useRef(false);
  const inactive = useRef<HTMLElement | null>(null);
  const getEl = useCallback(() => (kind === "nav" ? ref.current : null), [kind]);
  const terminal = useTerminalDragDrop(
    onDrop,
    visible,
    disposed,
    kind === "terminal" ? ref : inactive,
  );
  const nav = useNavFileDrop({ getScrollElement: getEl, onDrop });
  return (
    <div ref={ref} data-testid="native-target" data-highlight={terminal || !!nav}>
      {children}
    </div>
  );
}

describe("OS drops at the real HtmlViewer endpoint", () => {
  it("routes enter/over/leave and drop only to the topmost pane, without paths before drop", async () => {
    render(
      <>
        {viewer("a")}
        {viewer("b")}
      </>,
    );
    const a = await endpoint("a");
    const b = await endpoint("b");
    await native("enter", a.iframe);
    await native("over", a.iframe);
    await native("over", b.iframe);
    await native("drop", b.iframe);
    expect(a.messages()).toEqual([
      [drag("enter"), a.origin],
      [drag("over"), a.origin],
      [drag("leave"), a.origin],
    ]);
    expect(b.messages()).toEqual([
      [drag("enter"), b.origin],
      [{ type: "codesense:file-drop", paths }, b.origin],
      [drag("leave"), b.origin],
    ]);
    await native("enter", b.iframe);
    await native("leave", null);
    expect(b.messages().slice(-2)).toEqual([
      [drag("enter"), b.origin],
      [drag("leave"), b.origin],
    ]);
  });

  it("leaves when covered, hidden or unmounted and never delivers an unready drop", async () => {
    const mounted = render(viewer("a"));
    const a = await endpoint("a", false);
    await native("drop", a.iframe);
    expect(a.messages()).toEqual([]);
    await a.announceReady();
    await native("enter", a.iframe);
    await native("over", document.body);
    await native("drop", document.body);
    expect(a.messages()).toEqual([
      [drag("enter"), a.origin],
      [drag("leave"), a.origin],
    ]);
    for (const opts of [{ visible: false }]) {
      mounted.rerender(viewer("a"));
      await native("enter", a.iframe);
      mounted.rerender(viewer("a", opts));
      expect(a.messages().at(-1)).toEqual([drag("leave"), a.origin]);
      const count = a.messages().length;
      await native("drop", a.iframe);
      expect(a.messages()).toHaveLength(count);
    }
    mounted.rerender(viewer("a"));
    await native("enter", a.iframe);
    mounted.unmount();
    expect(a.messages().at(-1)).toEqual([drag("leave"), a.origin]);
    const count = a.messages().length;
    await native("drop", a.iframe);
    expect(a.messages()).toHaveLength(count);
    expect(handlers.size).toBe(0);
  });

  it("routes drops to a file-nav pane", async () => {
    render(viewer("nav", { slotId: "file-nav" }));
    const nav = await endpoint("nav");
    await native("enter", nav.iframe);
    await native("over", nav.iframe);
    await native("drop", nav.iframe);
    expect(nav.messages()).toEqual([
      [drag("enter"), nav.origin],
      [drag("over"), nav.origin],
      [{ type: "codesense:file-drop", paths }, nav.origin],
      [drag("leave"), nav.origin],
    ]);
  });

  it("leaves a retiring pane and ignores drops while its frame is still mounted", async () => {
    render(viewer("a"));
    const a = await endpoint("a");
    const frameId = records.get(a.origin)!.frameId;
    a.post.mockImplementation((message) => {
      if (message.type === "codesense:flush") acknowledgeFlush(frameId, message.id);
    });
    await native("enter", a.iframe);
    await act(async () => {
      await retireAfterFlush(
        (f) => f.frameId === frameId,
        () => {},
      );
    });
    expect(a.iframe.isConnected).toBe(true);
    expect(a.messages().at(-1)).toEqual([drag("leave"), a.origin]);
    const count = a.messages().length;
    await native("drop", a.iframe);
    expect(a.messages()).toHaveLength(count);
  });

  it.each(["ungranted", "centre"])("does not expose paths to a %s frame", async (kind) => {
    granted = kind !== "ungranted";
    render(viewer("a", { centre: kind === "centre" }));
    const a = await endpoint("a");
    await native("enter", a.iframe);
    await native("drop", a.iframe);
    expect(a.messages()).toEqual([]);
  });

  it.each(["terminal", "nav"] as const)("never cross-fires with a %s drop target", async (kind) => {
    const onDrop = vi.fn();
    const mounted = render(
      <NativeTarget kind={kind} onDrop={onDrop}>
        {viewer("a")}
      </NativeTarget>,
    );
    const container = mounted.getByTestId("native-target");
    container.getBoundingClientRect = () =>
      ({ left: 0, top: 0, right: 200, bottom: 200 }) as DOMRect;
    const a = await endpoint("a");
    await native("enter", a.iframe);
    expect(container.dataset.highlight).toBe("false");
    await native("drop", a.iframe);
    expect(onDrop).not.toHaveBeenCalled();
    expect(a.messages()).toContainEqual([{ type: "codesense:file-drop", paths }, a.origin]);
    const count = a.messages().length;
    await native("enter", container);
    expect(container.dataset.highlight).toBe("true");
    await native("drop", container);
    expect(onDrop).toHaveBeenCalledOnce();
    expect(a.messages()).toHaveLength(count);
  });
});
