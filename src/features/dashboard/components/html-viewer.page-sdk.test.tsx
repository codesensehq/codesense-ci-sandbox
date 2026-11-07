// SPDX-License-Identifier: MIT
/** The ready handshake tells the shim's has() what this frame's grant admits. */
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, waitFor } from "@testing-library/react";

import { FrameMessageEvent } from "../../../../tests/support/fixtures/frame";
import { loadShim } from "../../../../tests/support/page-shim";
import type { MintFrameArgs } from "@/tauri/commands";

import { HtmlViewer } from "./html-viewer";
import { PAGE_SDK_FEATURES } from "../lib/page-sdk";
import { VERB_REGISTRY } from "../lib/verb-registry.generated";

const { grant } = vi.hoisted(() => ({ grant: { current: [] as string[] } }));

vi.mock("@/tauri/commands", async (importOriginal) => {
  const frame = await import("../../../../tests/support/fixtures/frame");
  return {
    ...(await importOriginal<typeof import("@/tauri/commands")>()),
    ...frame.frameCommands(),
    mintFrame: async (args: MintFrameArgs) => ({
      ...frame.frameFixture(args),
      effectiveGrant: grant.current,
    }),
    getHttpApiPort: () => Promise.resolve(9123),
    bridgeDocContext: () => Promise.resolve(null),
  };
});

globalThis.fetch = vi.fn().mockResolvedValue({ status: 200 }) as never;
afterEach(cleanup);

const ALL = [...new Set(VERB_REGISTRY.map((verb) => verb.grant))];

async function handshake(effectiveGrant: string[]) {
  grant.current = effectiveGrant;
  render(
    <HtmlViewer filePath="/ctx/acme/a/content/dash.html" teamContentPath="/ctx/acme/a/content" />,
  );
  await waitFor(() => expect(document.querySelector("iframe")?.getAttribute("src")).toBeTruthy());
  const iframe = document.querySelector("iframe")!;
  const postMessage = vi.fn();
  const fakeWindow = { postMessage } as unknown as Window;
  Object.defineProperty(iframe, "contentWindow", { value: fakeWindow, configurable: true });
  act(() => {
    window.dispatchEvent(
      new FrameMessageEvent("message", { data: { type: "codesense:ready" }, source: fakeWindow }),
    );
  });
  await waitFor(() =>
    expect(postMessage.mock.calls.some(([m]) => m.type === "codesense:slot-context")).toBe(true),
  );
  const message = postMessage.mock.calls.find(([m]) => m.type === "codesense:slot-context")![0];
  cleanup();
  return message as { context: unknown; sdk: { features: string[]; verbs: string[] } };
}

describe("codesense.has", () => {
  it("open: the handshake carries every verb and the features, and has() reports them", async () => {
    const message = await handshake(ALL);
    expect(message.sdk.features).toEqual([...PAGE_SDK_FEATURES]);
    expect(message.sdk.verbs.sort()).toEqual(VERB_REGISTRY.map((verb) => verb.name).sort());
    const { codesense, deliver } = loadShim();
    const has = codesense.has as (name: string) => boolean;
    expect(has("stopSession")).toBe(false);
    deliver({ type: "codesense:slot-context", context: message.context, sdk: message.sdk });
    expect(VERB_REGISTRY.every((verb) => has(verb.name))).toBe(true);
    expect(has("virtualList")).toBe(true);
    expect(has("noSuchThing")).toBe(false);
  });

  it("a verb outside a narrowed grant reports false", async () => {
    const message = await handshake(["sessions:read"]);
    expect(message.sdk.verbs).toContain("sessions");
    expect(message.sdk.verbs).not.toContain("stopSession");
    const { codesense, deliver } = loadShim({ kit: true });
    deliver({ type: "codesense:slot-context", context: message.context, sdk: message.sdk });
    const has = codesense.has as (name: string) => boolean;
    const kit = codesense.kit as { has: (name: string) => boolean };
    expect(has("sessions")).toBe(true);
    expect(has("stopSession")).toBe(false);
    expect(kit.has("stopSession")).toBe(false);
    expect(kit.has("subscribe")).toBe(true);
    // The method still exists: has() is advisory and Rust decides each call.
    expect(typeof codesense.stopSession).toBe("function");
  });
});
