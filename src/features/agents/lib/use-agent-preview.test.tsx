// SPDX-License-Identifier: MIT
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentForm, ResolvedAgentView } from "@/tauri/commands";

const h = vi.hoisted(() => ({
  agentPreview: vi.fn(),
  handlers: [] as Array<() => void>,
  unlisten: vi.fn(),
}));

vi.mock("@/tauri/commands", () => ({ agentPreview: h.agentPreview }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, handler: () => void) => {
    if (event === "dep-status-changed") h.handlers.push(handler);
    return h.unlisten;
  }),
}));

import { PREVIEW_DEBOUNCE_MS, useAgentPreview } from "./use-agent-preview";

const form = { draft: { name: "Dev", runtime: "claude" } } as unknown as AgentForm;

function resolvedWith(codes: string[]): ResolvedAgentView {
  return {
    warnings: codes.map((code) => ({ code, message: code })),
  } as unknown as ResolvedAgentView;
}

describe("useAgentPreview", () => {
  beforeEach(() => {
    h.agentPreview.mockReset();
    h.handlers.length = 0;
    h.unlisten.mockReset();
  });

  it("resolves again when the dependency status changes, so a CLI warning is not stale", async () => {
    h.agentPreview
      .mockResolvedValueOnce({ outcome: "resolved", resolved: resolvedWith([]), servers: [] })
      .mockResolvedValueOnce({
        outcome: "resolved",
        resolved: resolvedWith(["cli_version_unqualified"]),
        servers: [],
      });
    const { result } = renderHook(() => useAgentPreview(form, undefined));
    await waitFor(() => expect(result.current.status).toBe("resolved"), {
      timeout: PREVIEW_DEBOUNCE_MS * 5,
    });
    expect(h.agentPreview).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(h.handlers).toHaveLength(1));

    await act(async () => h.handlers[0]());

    await waitFor(
      () => {
        expect(h.agentPreview).toHaveBeenCalledTimes(2);
        const shown = result.current.status === "resolved" ? result.current.resolved : null;
        expect(shown?.warnings.map((w) => w.code)).toEqual(["cli_version_unqualified"]);
      },
      { timeout: PREVIEW_DEBOUNCE_MS * 5 },
    );
  });

  it("stops listening when the editor unmounts", async () => {
    h.agentPreview.mockResolvedValue({
      outcome: "resolved",
      resolved: resolvedWith([]),
      servers: [],
    });
    const { unmount } = renderHook(() => useAgentPreview(form, undefined));
    await waitFor(() => expect(h.handlers).toHaveLength(1));
    unmount();
    await waitFor(() => expect(h.unlisten).toHaveBeenCalled());
  });
});
