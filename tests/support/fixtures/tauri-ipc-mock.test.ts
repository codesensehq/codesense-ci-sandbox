import { afterEach, describe, expect, it } from "vitest";

import { getTauriMockInitScript, type TauriMockCall } from "./tauri-ipc-mock";

interface MockInternals {
  calls: TauriMockCall[];
}

function install(overrides: Record<string, unknown> = {}) {
  window.eval(getTauriMockInitScript(overrides));
  return window.__TAURI_INTERNALS__.invoke as (
    cmd: string,
    args?: Record<string, unknown>,
  ) => Promise<unknown>;
}

afterEach(() => {
  delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  delete (window as unknown as { __TAURI_EVENT_PLUGIN_INTERNALS__?: unknown })
    .__TAURI_EVENT_PLUGIN_INTERNALS__;
  delete (window as unknown as { __TAURI_MOCK__?: unknown }).__TAURI_MOCK__;
});

describe("Tauri IPC mock protocol", () => {
  it("records invocations and resolves ordinary overrides", async () => {
    const invoke = install({ custom_command: { ok: true } });

    await expect(invoke("custom_command", { value: 3 })).resolves.toEqual({ ok: true });
    expect((window as unknown as { __TAURI_MOCK__: MockInternals }).__TAURI_MOCK__.calls).toEqual([
      { cmd: "custom_command", args: { value: 3 } },
    ]);
  });

  it("redacts credential-shaped arguments recursively", async () => {
    const invoke = install({ custom_command: null });

    await invoke("custom_command", {
      password: "one",
      credential: "two",
      authorization: "three",
      nested: { api_key: "four" },
    });

    expect((window as unknown as { __TAURI_MOCK__: MockInternals }).__TAURI_MOCK__.calls).toEqual([
      {
        cmd: "custom_command",
        args: {
          password: "[REDACTED]",
          credential: "[REDACTED]",
          authorization: "[REDACTED]",
          nested: { api_key: "[REDACTED]" },
        },
      },
    ]);
  });

  it("throws typed override payloads", async () => {
    const rejection = { kind: "invalid", detail: { code: "fixture" } };
    const invoke = install({ custom_command: { __tauriReject: rejection } });

    await expect(invoke("custom_command")).rejects.toEqual(rejection);
  });
});
