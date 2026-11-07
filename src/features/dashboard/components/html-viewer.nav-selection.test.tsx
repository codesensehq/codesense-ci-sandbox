// SPDX-License-Identifier: MIT
import { FrameMessageEvent } from "../../../../tests/support/fixtures/frame";
/** Navigation selection travels in the automatic slot-context lifecycle. */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, cleanup, render, waitFor } from "@testing-library/react";

import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { HtmlViewer } from "./html-viewer";
import { clearNavSelectionCaches, NAV_SELECTION_DEBOUNCE_MS } from "./nav-selection";

const ROOT_A = "/ctx/acme/a/content";
const ROOT_B = "/ctx/acme/b/content";
const PRIVATE = "/ctx/acme/private/content";
const STORY = `${ROOT_A}/epics/billing/BIL-2.md`;
const OTHER = `${ROOT_A}/epics/billing/BIL-3.md`;

const { mockQuery } = vi.hoisted(() => ({ mockQuery: vi.fn() }));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  ...(await import("../../../../tests/support/fixtures/frame")).frameCommands(),
  getHttpApiPort: () => Promise.resolve(9123),
  bridgeDocContext: (p: string) =>
    Promise.resolve(
      p.startsWith(ROOT_A)
        ? { org: "acme", team: "a", layer: "team", contentRoot: ROOT_A }
        : p.startsWith(ROOT_B)
          ? { org: "acme", team: "b", layer: "team", contentRoot: ROOT_B }
          : p.startsWith(PRIVATE)
            ? { org: "acme", team: null, layer: "private", contentRoot: PRIVATE }
            : null,
    ),
  queryContext: (...args: unknown[]) => mockQuery(...args),
}));

globalThis.fetch = vi.fn().mockResolvedValue({ status: 200 }) as never;

beforeEach(() => {
  clearNavSelectionCaches();
  mockQuery.mockReset();
  mockQuery.mockImplementation(async (q: { where?: { pathPrefix?: string; kinds?: string[] } }) => {
    const p = q.where?.pathPrefix;
    if (p === STORY || p === OTHER) {
      return {
        rows: [
          {
            id: p === STORY ? "BIL-2" : "BIL-3",
            path: p,
            relPath: p.slice(ROOT_A.length + 1),
            kind: "story",
            title: "t",
            fields: {},
            relationships: { parentEpic: "BIL", folderEpic: "billing" },
          },
        ],
        total: 1,
      };
    }
    return { rows: [], total: 0 };
  });
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "a" });
  useViewerStore.setState({
    activeTab: { "acme:a": STORY },
    drillPaths: {},
  });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

type Posted = { type: string; context?: { selection?: unknown }; selection?: unknown };

async function renderDoc(props: { filePath: string; pane?: boolean; panesDir?: string }) {
  const before = document.querySelectorAll("iframe").length;
  render(
    <HtmlViewer
      filePath={props.filePath}
      teamContentPath={ROOT_A}
      pane={
        props.pane
          ? {
              contentRoot: ROOT_A,
              panesDir: props.panesDir ?? "/ctx/acme/a/panes",
              fileName: "pipeline.html",
            }
          : undefined
      }
    />,
  );
  await waitFor(() =>
    expect(document.querySelectorAll("iframe")[before]?.getAttribute("src")).toBeTruthy(),
  );
  const iframe = document.querySelectorAll("iframe")[before]!;
  const postMessage = vi.fn();
  const fakeWindow = { postMessage } as unknown as Window;
  Object.defineProperty(iframe, "contentWindow", { value: fakeWindow, configurable: true });
  const send = (data: Record<string, unknown>) =>
    act(() => {
      window.dispatchEvent(new FrameMessageEvent("message", { data, source: fakeWindow }));
    });
  await send({ type: "codesense:ready" });
  const navPushes = () => {
    let previous = "";
    return postMessage.mock.calls.flatMap(([m, origin]) => {
      if (m.type !== "codesense:slot-context" || !m.context.selection) return [];
      const next = JSON.stringify(m.context.selection);
      if (next === previous) return [];
      previous = next;
      return [{ m: { ...m, selection: m.context.selection } as Posted, origin }];
    });
  };
  return { send, navPushes, postMessage };
}

const settle = () =>
  act(async () => {
    await new Promise((r) => setTimeout(r, NAV_SELECTION_DEBOUNCE_MS + 40));
  });

describe("HtmlViewer nav selection (SW-DNS-1)", () => {
  it("automatically sends selection without a watch protocol", async () => {
    const { navPushes } = await renderDoc({ filePath: `${ROOT_A}/dash.html` });
    act(() => useViewerStore.setState({ activeTab: { "acme:a": OTHER } }));
    await settle();
    expect(navPushes().at(-1)?.m.selection).toMatchObject({ file: { id: "BIL-3" } });
    expect(mockQuery).toHaveBeenCalled();
  });

  it("answers readiness with the current selection, pinned to the doc's origin (AC1)", async () => {
    const { navPushes } = await renderDoc({ filePath: `${ROOT_A}/dash.html` });
    await waitFor(() => expect(navPushes()).toHaveLength(1));
    const [{ m, origin }] = navPushes();
    expect(origin).toBe("http://f-test.localhost:9123");
    expect(m.selection).toMatchObject({
      org: "acme",
      team: "a",
      file: { path: STORY, kind: "story", id: "BIL-2", epic: "BIL" },
      drill: null,
    });
  });

  it("coalesces a burst into one push and never repeats an unchanged selection (AC2)", async () => {
    const { navPushes } = await renderDoc({ filePath: `${ROOT_A}/dash.html` });
    await waitFor(() => expect(navPushes()).toHaveLength(1));
    act(() => useViewerStore.setState({ activeTab: { "acme:a": OTHER } }));
    act(() => useViewerStore.setState({ activeTab: { "acme:a": STORY } }));
    act(() => useViewerStore.setState({ activeTab: { "acme:a": OTHER } }));
    await settle();
    await waitFor(() => expect(navPushes()).toHaveLength(2));
    expect(navPushes()[1].m.selection).toMatchObject({ file: { id: "BIL-3" } });
    // An unrelated store write, and re-selecting the same file, push nothing.
    act(() => useViewerStore.setState({ activeTab: { "acme:a": OTHER }, rightPaneOpen: true }));
    await settle();
    expect(navPushes()).toHaveLength(2);
  });

  it("a centre doc of team A keeps reading team A while team B is on screen (AC4)", async () => {
    const { navPushes } = await renderDoc({ filePath: `${ROOT_A}/dash.html` });
    await waitFor(() => expect(navPushes()).toHaveLength(1));
    act(() => {
      useOrgStore.setState({ activeTeamName: "b" });
      useViewerStore.setState({ activeTab: { "acme:a": STORY, "acme:b": `${ROOT_B}/x.md` } });
    });
    await settle();
    expect(navPushes()).toHaveLength(1);
    expect(JSON.stringify(navPushes())).not.toContain(ROOT_B);
  });

  it("a pane follows the team on screen, and a path outside it reads as null (AC4)", async () => {
    const { navPushes } = await renderDoc({
      filePath: "/ctx/acme/a/panes/pipeline.html",
      pane: true,
    });
    await waitFor(() => expect(navPushes()).toHaveLength(1));
    act(() => useViewerStore.setState({ activeTab: { "acme:a": `${ROOT_B}/leak.md` } }));
    await settle();
    await waitFor(() => expect(navPushes()).toHaveLength(2));
    expect(navPushes()[1].m.selection).toMatchObject({ team: "a", file: null });
    expect(JSON.stringify(navPushes())).not.toContain("leak.md");
  });

  it("a private dashboard and a team dashboard side by side both read the team in view (AC4)", async () => {
    const team = await renderDoc({ filePath: `${ROOT_A}/dash.html` });
    const priv = await renderDoc({ filePath: `${PRIVATE}/my-dash.html` });
    const privPane = await renderDoc({
      filePath: "/ctx/acme/private/panes/pipeline.html",
      pane: true,
      panesDir: "/ctx/acme/private/panes",
    });
    for (const d of [team, priv, privPane]) {
      await waitFor(() => expect(d.navPushes()).toHaveLength(1));
      expect(d.navPushes()[0].m.selection).toMatchObject({
        org: "acme",
        team: "a",
        file: { path: STORY, id: "BIL-2" },
      });
    }
  });

  it("a first read never waits: with the index down the doc still gets an answer", async () => {
    mockQuery.mockRejectedValue(new Error("index down"));
    const { navPushes } = await renderDoc({ filePath: `${ROOT_A}/dash.html` });
    await waitFor(() => expect(navPushes()).toHaveLength(1));
    // The path is known; only its identity is missing.
    expect(navPushes()[0].m.selection).toMatchObject({
      file: { path: STORY, kind: null, id: null },
    });
  });

  it("an off-screen doc resolves nothing until shown, then catches up once", async () => {
    const { rerender } = render(
      <HtmlViewer filePath={`${ROOT_A}/hidden.html`} teamContentPath={ROOT_A} visible />,
    );
    await waitFor(() => expect(document.querySelector("iframe")?.getAttribute("src")).toBeTruthy());
    const iframe = document.querySelector("iframe")!;
    const postMessage = vi.fn();
    const win = { postMessage } as unknown as Window;
    Object.defineProperty(iframe, "contentWindow", { value: win, configurable: true });
    const send = (data: Record<string, unknown>) =>
      act(() => {
        window.dispatchEvent(new FrameMessageEvent("message", { data, source: win }));
      });
    const pushes = () => {
      let previous = "";
      return postMessage.mock.calls
        .filter(([m]) => {
          if (m.type !== "codesense:slot-context" || !m.context.selection) return false;
          const key = JSON.stringify(m.context.selection);
          if (key === previous) return false;
          previous = key;
          return true;
        })
        .map(([m, origin]) => [{ ...m, selection: m.context.selection }, origin]);
    };
    await send({ type: "codesense:ready" });
    await waitFor(() => expect(pushes()).toHaveLength(1));
    rerender(
      <HtmlViewer filePath={`${ROOT_A}/hidden.html`} teamContentPath={ROOT_A} visible={false} />,
    );
    const calls = mockQuery.mock.calls.length;
    act(() => useViewerStore.setState({ activeTab: { "acme:a": OTHER } }));
    await settle();
    expect(pushes()).toHaveLength(1);
    expect(mockQuery.mock.calls.length).toBe(calls);
    rerender(<HtmlViewer filePath={`${ROOT_A}/hidden.html`} teamContentPath={ROOT_A} visible />);
    await settle();
    await waitFor(() => expect(pushes()).toHaveLength(2));
    expect(pushes()[1][0].selection).toMatchObject({ file: { id: "BIL-3" } });
  });
});
