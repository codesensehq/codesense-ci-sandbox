// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { createInvokeRouter, NotMockedError } from "./mock-router";

describe("createInvokeRouter", () => {
  it("dispatches a registered business command to its handler and resolves the result", async () => {
    const router = createInvokeRouter({
      load_orgs: () => [{ slug: "acme" }],
    });
    await expect(router("load_orgs", {})).resolves.toEqual([{ slug: "acme" }]);
  });

  it("passes the invoke args through to the handler", async () => {
    const handler = vi.fn(() => "ok");
    const router = createInvokeRouter({ create_team: handler });
    await router("create_team", { name: "Platform" });
    expect(handler).toHaveBeenCalledWith({ name: "Platform" });
  });

  it("always returns a Promise even for a synchronous handler (matches the invoke contract)", () => {
    const router = createInvokeRouter({ ping: () => 1 });
    expect(router("ping", {})).toBeInstanceOf(Promise);
  });

  it("throws a loud NotMockedError naming the command for an unmapped business command", async () => {
    const router = createInvokeRouter({});
    await expect(router("delete_universe", {})).rejects.toBeInstanceOf(NotMockedError);
    await expect(router("delete_universe", {})).rejects.toThrow("not mocked: delete_universe");
  });

  it("treats inert plugin surfaces as no-ops rather than loud stubs (AC-5 honest boundaries)", async () => {
    const router = createInvokeRouter({});
    // window/opener/dialog are runtime surfaces that are intentionally inert.
    await expect(router("plugin:window|set_title", { title: "x" })).resolves.toBeUndefined();
    await expect(router("plugin:opener|open_url", { url: "x" })).resolves.toBeUndefined();
  });

  it("returns a numeric id for plugin:event|listen so listener registration resolves", async () => {
    const router = createInvokeRouter({});
    const id = await router("plugin:event|listen", { event: "x", handler: 3 });
    expect(typeof id).toBe("number");
  });

  it("lets an explicit handler override a plugin default when one is registered", async () => {
    const router = createInvokeRouter({ "plugin:window|set_title": () => "overridden" });
    await expect(router("plugin:window|set_title", {})).resolves.toBe("overridden");
  });

  it("surfaces a handler that throws as a rejected promise", async () => {
    const router = createInvokeRouter({
      register_connector: () => {
        throw new Error("invalid connector");
      },
    });
    await expect(router("register_connector", {})).rejects.toThrow("invalid connector");
  });
});
