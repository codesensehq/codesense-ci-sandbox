// SPDX-License-Identifier: MIT
import { FrameMessageEvent } from "../../../../tests/support/fixtures/frame";
/**
 * SW-DNS-2: a dashboard's `prefs` belong to the dashboard instance the host
 * renders. A team pane and a private pane of the same id (and a team and a
 * private centre dashboard) keep separate settings, end to end through the real
 * host → action gate → bridge path.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, cleanup, render, waitFor } from "@testing-library/react";

import { logger } from "@/shared/lib/logger";

import { HtmlViewer } from "./html-viewer";
const { nativeDispatch } = vi.hoisted(() => ({
  nativeDispatch: vi.fn(async () => ({ status: "admit", callId: "call" })),
}));
vi.mock("../lib/verb-dispatch", () => ({
  verbDispatch: nativeDispatch,
  verbSettle: vi.fn(async () => undefined),
  verbAnswer: vi.fn(),
}));
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));

const { store } = vi.hoisted(() => ({ store: { raw: "{}" } }));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  bridgeDocContext: async () => null,
  ...(await import("../../../../tests/support/fixtures/frame")).frameCommands(),
  getHttpApiPort: () => Promise.resolve(9123),
  loadAppState: vi.fn(async () => store.raw),
  saveAppState: vi.fn(async (s: string) => {
    store.raw = s;
  }),
}));

globalThis.fetch = vi.fn().mockResolvedValue({ status: 200 }) as never;

beforeEach(() => {
  store.raw = JSON.stringify({ viewer: {} });
});
afterEach(() => cleanup());

const TEAM_ROOT = "/ctx/acme/a/content";

async function mount(props: Parameters<typeof HtmlViewer>[0]) {
  const before = document.querySelectorAll("iframe").length;
  render(<HtmlViewer {...props} />);
  await waitFor(() =>
    expect(document.querySelectorAll("iframe")[before]?.getAttribute("src")).toBeTruthy(),
  );
  const iframe = document.querySelectorAll("iframe")[before]!;
  const postMessage = vi.fn();
  const win = { postMessage } as unknown as Window;
  Object.defineProperty(iframe, "contentWindow", { value: win, configurable: true });
  let id = 0;
  const call = async (command: string, args?: unknown) => {
    const my = ++id;
    act(() => {
      window.dispatchEvent(
        new FrameMessageEvent("message", {
          data: { type: "codesense:invoke", id: my, command, args },
          source: win,
        }),
      );
    });
    await waitFor(() =>
      expect(
        postMessage.mock.calls.some(([m]) => m.type === "codesense:invoke-result" && m.id === my),
      ).toBe(true),
    );
    const [reply] = postMessage.mock.calls.find(
      ([m]) => m.type === "codesense:invoke-result" && m.id === my,
    )!;
    if (!reply.ok) throw new Error(reply.error);
    return reply.data;
  };
  return { call };
}

describe("HtmlViewer prefs (SW-DNS-2)", () => {
  it("a team pane and a private pane of the same id keep their own settings", async () => {
    const teamPane = await mount({
      filePath: "/ctx/acme/a/panes/pipeline.html",
      teamContentPath: TEAM_ROOT,
      pane: { contentRoot: TEAM_ROOT, panesDir: "/ctx/acme/a/panes", fileName: "pipeline.html" },
    });
    const privatePane = await mount({
      filePath: "/scratch/orgs/acme/panes/pipeline.html",
      teamContentPath: TEAM_ROOT,
      pane: {
        contentRoot: TEAM_ROOT,
        panesDir: "/scratch/orgs/acme/panes",
        fileName: "pipeline.html",
      },
    });
    await teamPane.call("setPrefs", { details: "off" });
    await privatePane.call("setPrefs", { details: "full" });
    expect(await teamPane.call("prefs")).toEqual({ details: "off" });
    expect(await privatePane.call("prefs")).toEqual({ details: "full" });
  });

  it("a team and a private centre dashboard keep their own settings, in the user's app state only", async () => {
    const team = await mount({ filePath: `${TEAM_ROOT}/board.html`, teamContentPath: TEAM_ROOT });
    const priv = await mount({
      filePath: "/scratch/orgs/acme/content/board.html",
      teamContentPath: TEAM_ROOT,
    });
    await team.call("setPrefs", { a: 1 });
    expect(await priv.call("prefs")).toEqual({});
    const saved = JSON.parse(store.raw);
    expect(Object.keys(saved.dashboardPrefs)).toEqual([`doc:${TEAM_ROOT}/board.html`]);
  });

  it("setPrefs has its own budget (not the metadata one) and writes no audit line", async () => {
    const doc = await mount({ filePath: `${TEAM_ROOT}/busy.html`, teamContentPath: TEAM_ROOT });
    vi.mocked(logger.info).mockClear();
    // More than the 10-per-10 s metadata budget, all admitted.
    for (let i = 0; i < 30; i++) await doc.call("setPrefs", { i });
    nativeDispatch.mockResolvedValueOnce({
      status: "deny",
      reason: "denied: prefs budget",
    } as never);
    await expect(doc.call("setPrefs", { i: "one too many" })).rejects.toThrow(
      /^denied: prefs budget/,
    );
    const audited = vi
      .mocked(logger.info)
      .mock.calls.filter(
        ([target, line]) => target === "dashboard-action" && String(line).includes("setPrefs"),
      );
    expect(audited).toHaveLength(0);
    expect(await doc.call("prefs")).toEqual({ i: 29 });
  });
});
