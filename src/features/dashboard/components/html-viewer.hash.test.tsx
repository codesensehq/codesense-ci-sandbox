// SPDX-License-Identifier: MIT
import { afterEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { HtmlViewer } from "./html-viewer";
vi.mock("@/tauri/commands", async (original) => {
  const fixture = await import("../../../../tests/support/fixtures/frame");
  return {
    ...(await original<typeof import("@/tauri/commands")>()),
    ...fixture.frameCommands(),
    getHttpApiPort: () => Promise.resolve(9123),
    bridgeDocContext: () => Promise.resolve(null),
  };
});
globalThis.fetch = vi.fn().mockResolvedValue({ status: 200 }) as never;
afterEach(cleanup);
it("forwards initial and repeated same-tab hashes on the iframe without changing its base URL", async () => {
  const consumed = vi.fn(),
    props = {
      filePath: "/ctx/a/trail.html",
      teamContentPath: "/ctx/a",
      onPendingHashConsumed: consumed,
    };
  const view = render(<HtmlViewer {...props} pendingHash="#story/SV-X" />);
  await waitFor(() => expect(document.querySelector("iframe")?.src).toBeTruthy());
  const frame = document.querySelector("iframe")!;
  fireEvent.load(frame);
  await waitFor(() => expect(new URL(frame.src).hash).toBe("#story/SV-X"));
  const base = frame.src.split("#")[0];
  expect(consumed).toHaveBeenCalled();
  view.rerender(<HtmlViewer {...props} pendingHash={null} />);
  act(() => {
    frame.src = base + "#session/lead";
  });
  view.rerender(<HtmlViewer {...props} pendingHash="#story/SV-X" />);
  await waitFor(() => expect(new URL(frame.src).hash).toBe("#story/SV-X"));
  expect(frame.src.split("#")[0]).toBe(base);
});
it("reapplies the same hash when a newer request arrives before earlier consumption", async () => {
  const props = { filePath: "/ctx/a/trail.html", teamContentPath: "/ctx/a" };
  const view = render(
    <HtmlViewer {...props} pendingHash="#story/SV-X" onPendingHashConsumed={vi.fn()} />,
  );
  await waitFor(() => expect(document.querySelector("iframe")?.src).toBeTruthy());
  const frame = document.querySelector("iframe")!;
  fireEvent.load(frame);
  const base = frame.src.split("#")[0];
  act(() => {
    frame.src = base + "#session/lead";
  });
  const consumed = vi.fn();
  view.rerender(
    <HtmlViewer {...props} pendingHash="#story/SV-X" onPendingHashConsumed={consumed} />,
  );
  await waitFor(() => expect(new URL(frame.src).hash).toBe("#story/SV-X"));
  expect(consumed).toHaveBeenCalled();
});
it("retains a consumed fragment across viewer eviction and remount", async () => {
  const props = {
    filePath: "/ctx/a/trail.html",
    teamContentPath: "/ctx/a",
    viewStateKey: "route-remount::/ctx/a/trail.html",
  };
  const first = render(
    <HtmlViewer {...props} pendingHash="#session/lead/turn/37" onPendingHashConsumed={vi.fn()} />,
  );
  await waitFor(() => expect(document.querySelector("iframe")?.src).toBeTruthy());
  fireEvent.load(document.querySelector("iframe")!);
  first.rerender(<HtmlViewer {...props} pendingHash={null} />);
  first.unmount();
  render(<HtmlViewer {...props} />);
  await waitFor(() =>
    expect(new URL(document.querySelector("iframe")!.src).hash).toBe("#session/lead/turn/37"),
  );
});
it("retains authenticated document route changes for the next mount", async () => {
  const { FrameMessageEvent } = await import("../../../../tests/support/fixtures/frame");
  const props = {
    filePath: "/ctx/a/route.html",
    teamContentPath: "/ctx/a",
    viewStateKey: "route-events::/ctx/a/route.html",
  };
  const view = render(<HtmlViewer {...props} />);
  await waitFor(() => expect(document.querySelector("iframe")?.src).toBeTruthy());
  const frame = document.querySelector("iframe")!;
  act(() => {
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        source: frame.contentWindow,
        data: { type: "codesense:route-changed", hash: "#session/lead?cursor=17" },
      }),
    );
  });
  view.unmount();
  render(<HtmlViewer {...props} />);
  await waitFor(() =>
    expect(new URL(document.querySelector("iframe")!.src).hash).toBe("#session/lead?cursor=17"),
  );
});
it("delivers every identical fragment request to the authenticated existing document", async () => {
  const { FrameMessageEvent } = await import("../../../../tests/support/fixtures/frame");
  const props = { filePath: "/ctx/a/same-fragment.html", teamContentPath: "/ctx/a" };
  const consumed = vi.fn(),
    view = render(
      <HtmlViewer
        {...props}
        pendingHash="#session/lead/turn/37"
        onPendingHashConsumed={consumed}
      />,
    );
  await waitFor(() => expect(document.querySelector("iframe")?.src).toBeTruthy());
  const frame = document.querySelector("iframe")!;
  const postMessage = vi.fn(),
    fakeWindow = { postMessage, location: { href: frame.src } } as unknown as Window;
  Object.defineProperty(frame, "contentWindow", { value: fakeWindow, configurable: true });
  act(() => {
    window.dispatchEvent(
      new FrameMessageEvent("message", { source: fakeWindow, data: { type: "codesense:ready" } }),
    );
    fireEvent.load(frame);
  });
  await waitFor(() =>
    expect(
      postMessage.mock.calls.filter(([message]) => message.type === "codesense:navigate"),
    ).toHaveLength(1),
  );
  view.rerender(<HtmlViewer {...props} pendingHash={null} onPendingHashConsumed={consumed} />);
  view.rerender(
    <HtmlViewer {...props} pendingHash="#session/lead/turn/37" onPendingHashConsumed={vi.fn()} />,
  );
  await waitFor(() =>
    expect(
      postMessage.mock.calls.filter(([message]) => message.type === "codesense:navigate"),
    ).toHaveLength(2),
  );
  const routes = postMessage.mock.calls.filter(
    ([message]) => message.type === "codesense:navigate",
  );
  expect(routes[0][0]).toMatchObject({ hash: "#session/lead/turn/37", token: "frame-token" });
  expect(routes[1][0].nonce).toBeGreaterThan(routes[0][0].nonce);
  expect(document.querySelector("iframe")).toBe(frame);
});
it("does not replay the previous saved route on an internal fragment load", async () => {
  const { FrameMessageEvent } = await import("../../../../tests/support/fixtures/frame");
  render(<HtmlViewer filePath="/ctx/a/internal-route.html" teamContentPath="/ctx/a" />);
  await waitFor(() => expect(document.querySelector("iframe")?.src).toBeTruthy());
  const frame = document.querySelector("iframe")!,
    base = frame.src.split("#")[0];
  const fakeWindow = {
    postMessage: vi.fn(),
    location: { href: base + "#story/SV-X" },
  } as unknown as Window;
  Object.defineProperty(frame, "contentWindow", { value: fakeWindow, configurable: true });
  act(() => {
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        source: fakeWindow,
        data: { type: "codesense:route-changed", hash: "#session/lead/turn/37" },
      }),
    );
    fireEvent.load(frame);
  });
  expect(fakeWindow.location.href).toBe(base + "#story/SV-X");
});

it.each(["pending", "saved"])(
  "a %s fragment owns load positioning instead of stale scroll restoration",
  async (mode) => {
    const { FrameMessageEvent } = await import("../../../../tests/support/fixtures/frame");
    const props = { filePath: `/ctx/a/anchor-${mode}.html`, teamContentPath: "/ctx/a" };
    render(<HtmlViewer {...props} pendingHash={mode === "pending" ? "#section" : undefined} />);
    await waitFor(() => expect(document.querySelector("iframe")?.src).toBeTruthy());
    const frame = document.querySelector("iframe")!;
    const postMessage = vi.fn(),
      fakeWindow = { postMessage, location: { href: frame.src } } as unknown as Window;
    Object.defineProperty(frame, "contentWindow", { value: fakeWindow, configurable: true });
    act(() => {
      window.dispatchEvent(
        new FrameMessageEvent("message", {
          source: fakeWindow,
          data: { type: "codesense:scroll", y: 250 },
        }),
      );
      if (mode === "saved")
        window.dispatchEvent(
          new FrameMessageEvent("message", {
            source: fakeWindow,
            data: { type: "codesense:route-changed", hash: "#section" },
          }),
        );
      fireEvent.load(frame);
      window.dispatchEvent(
        new FrameMessageEvent("message", { source: fakeWindow, data: { type: "codesense:ready" } }),
      );
    });
    expect(
      postMessage.mock.calls.filter(([message]) => message.type === "codesense:restore-scroll"),
    ).toEqual([]);
  },
);
