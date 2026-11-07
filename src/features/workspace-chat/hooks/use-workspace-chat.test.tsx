// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

import { act, render, screen, waitFor } from "@/test-utils";

import type {
  WorkspaceChatMessage,
  WorkspaceChatReadResult,
  WorkspaceChatUpdatedPayload,
} from "@/tauri/commands";

// SW-WC-3 — hook tests. We stub the Tauri binding + the event bus so the
// subscribe / cursor paging / metadata-probe seams are exercised in isolation.

const workspaceChatRead =
  vi.fn<
    (ws: string, since?: number | null, limit?: number | null) => Promise<WorkspaceChatReadResult>
  >();

vi.mock("@/tauri/commands", async () => {
  const actual = await vi.importActual<typeof import("@/tauri/commands")>("@/tauri/commands");
  return {
    ...actual,
    workspaceChatRead: (...a: unknown[]) =>
      workspaceChatRead(...(a as Parameters<typeof workspaceChatRead>)),
  };
});

const listeners = new Map<string, Set<(e: { payload: unknown }) => void>>();
function emit(name: string, payload: unknown): void {
  for (const cb of listeners.get(name) ?? []) cb({ payload });
}
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => {
    const set = listeners.get(name) ?? new Set();
    set.add(cb);
    listeners.set(name, set);
    return Promise.resolve(() => set.delete(cb));
  },
}));

import { mergeMessages, useWorkspaceChat } from "./use-workspace-chat";

function chatMsg(id: number, overrides: Partial<WorkspaceChatMessage> = {}): WorkspaceChatMessage {
  return {
    messageId: id,
    workspaceId: "story:ws-1",
    from: "agent-a",
    fromName: "agent a",
    createdAtMs: 1_700_000_000_000 + id * 1000,
    text: `message ${id}`,
    kind: "chat",
    ...overrides,
  };
}

function buildResult(
  workspaceId: string,
  messages: WorkspaceChatMessage[],
  overrides: Partial<WorkspaceChatReadResult> = {},
): WorkspaceChatReadResult {
  return {
    workspaceId,
    messages,
    headId: messages[messages.length - 1]?.messageId ?? 0,
    archived: false,
    ...overrides,
  };
}

function Host({ workspaceId }: { workspaceId: string | null }) {
  const { messages, headId, loading, error, archived } = useWorkspaceChat(workspaceId);
  return (
    <div>
      <span data-testid="count">{messages ? messages.length : "null"}</span>
      <span data-testid="head">{headId}</span>
      <span data-testid="loading">{loading ? "yes" : "no"}</span>
      <span data-testid="error">{error ?? ""}</span>
      <span data-testid="archived">{archived ? "yes" : "no"}</span>
      <ul>
        {(messages ?? []).map((m) => (
          <li key={m.messageId} data-testid="row">
            {m.messageId}:{m.text}
          </li>
        ))}
      </ul>
    </div>
  );
}

describe("SW-WC-3 · useWorkspaceChat", () => {
  beforeEach(() => {
    listeners.clear();
    workspaceChatRead.mockReset();
  });

  it("loads the tail on mount with limit=50 (initial subscribe + read)", async () => {
    workspaceChatRead.mockResolvedValueOnce(buildResult("story:ws-1", [chatMsg(1), chatMsg(2)]));
    render(<Host workspaceId="story:ws-1" />);
    await waitFor(() => expect(screen.getByTestId("count")).toHaveTextContent("2"));
    expect(workspaceChatRead).toHaveBeenCalledWith("story:ws-1", null, 50);
    expect(screen.getByTestId("head")).toHaveTextContent("2");
  });

  it("re-reads with since=lastHeadId on a nudge event (cursor paging)", async () => {
    workspaceChatRead.mockResolvedValueOnce(buildResult("story:ws-1", [chatMsg(1), chatMsg(2)]));
    render(<Host workspaceId="story:ws-1" />);
    await waitFor(() => expect(screen.getByTestId("count")).toHaveTextContent("2"));

    workspaceChatRead.mockResolvedValueOnce(buildResult("story:ws-1", [chatMsg(3)], { headId: 3 }));
    await act(async () => {
      emit("codesense:workspace-chat-updated", {
        workspaceId: "story:ws-1",
        headId: 3,
      } satisfies WorkspaceChatUpdatedPayload);
    });

    await waitFor(() => expect(screen.getByTestId("count")).toHaveTextContent("3"));
    // Second call used since=2 (the previous headId) + no explicit limit cap.
    expect(workspaceChatRead).toHaveBeenNthCalledWith(2, "story:ws-1", 2, null);
    expect(screen.getByTestId("head")).toHaveTextContent("3");
    const rows = screen.getAllByTestId("row").map((n) => n.textContent);
    expect(rows).toEqual(["1:message 1", "2:message 2", "3:message 3"]);
  });

  it("drains every delta page before moving the shared transcript cursor", async () => {
    workspaceChatRead.mockResolvedValueOnce(buildResult("story:ws-1", [chatMsg(1)]));
    render(<Host workspaceId="story:ws-1" />);
    await waitFor(() => expect(screen.getByTestId("count")).toHaveTextContent("1"));
    workspaceChatRead.mockResolvedValueOnce(
      buildResult("story:ws-1", [chatMsg(2), chatMsg(3)], { headId: 5 }),
    );
    workspaceChatRead.mockResolvedValueOnce(buildResult("story:ws-1", [chatMsg(4), chatMsg(5)]));
    await act(async () =>
      emit("codesense:workspace-chat-updated", { workspaceId: "story:ws-1", headId: 5 }),
    );
    await waitFor(() => expect(screen.getByTestId("count")).toHaveTextContent("5"));
    expect(workspaceChatRead).toHaveBeenNthCalledWith(3, "story:ws-1", 3, null);
  });

  it("ignores a nudge whose headId doesn't advance (metadata-probe compatible)", async () => {
    workspaceChatRead.mockResolvedValueOnce(buildResult("story:ws-1", [chatMsg(1)], { headId: 1 }));
    render(<Host workspaceId="story:ws-1" />);
    await waitFor(() => expect(screen.getByTestId("count")).toHaveTextContent("1"));

    await act(async () => {
      emit("codesense:workspace-chat-updated", {
        workspaceId: "story:ws-1",
        headId: 1,
      });
    });
    // Still exactly one call — no re-read because head didn't advance.
    expect(workspaceChatRead).toHaveBeenCalledTimes(1);
  });

  it("ignores a nudge for a different workspace", async () => {
    workspaceChatRead.mockResolvedValueOnce(buildResult("story:ws-1", [chatMsg(1)]));
    render(<Host workspaceId="story:ws-1" />);
    await waitFor(() => expect(screen.getByTestId("count")).toHaveTextContent("1"));

    await act(async () => {
      emit("codesense:workspace-chat-updated", {
        workspaceId: "story:other",
        headId: 99,
      });
    });
    expect(workspaceChatRead).toHaveBeenCalledTimes(1);
  });

  it("resets state and re-reads when the workspace swaps", async () => {
    workspaceChatRead.mockResolvedValueOnce(
      buildResult("story:ws-1", [chatMsg(1), chatMsg(2)], { headId: 2 }),
    );
    const { rerender } = render(<Host workspaceId="story:ws-1" />);
    await waitFor(() => expect(screen.getByTestId("count")).toHaveTextContent("2"));

    workspaceChatRead.mockResolvedValueOnce(
      buildResult(
        "story:ws-2",
        [chatMsg(10, { workspaceId: "story:ws-2", from: "agent-b", fromName: "b" })],
        { headId: 10 },
      ),
    );
    rerender(<Host workspaceId="story:ws-2" />);
    // The sync reset means `count` returns to "null" immediately; a re-read lands next.
    await waitFor(() => expect(screen.getByTestId("count")).toHaveTextContent("1"));
    expect(screen.getByTestId("head")).toHaveTextContent("10");
    expect(workspaceChatRead).toHaveBeenNthCalledWith(2, "story:ws-2", null, 50);
  });

  it("surfaces the engine's archived flag", async () => {
    workspaceChatRead.mockResolvedValueOnce(
      buildResult("story:ws-1", [chatMsg(1)], { archived: true }),
    );
    render(<Host workspaceId="story:ws-1" />);
    await waitFor(() => expect(screen.getByTestId("archived")).toHaveTextContent("yes"));
  });

  it("records the error message when the initial read fails", async () => {
    workspaceChatRead.mockRejectedValueOnce(new Error("workspace chat engine not installed"));
    render(<Host workspaceId="story:ws-1" />);
    await waitFor(() =>
      expect(screen.getByTestId("error")).toHaveTextContent("workspace chat engine not installed"),
    );
    expect(screen.getByTestId("count")).toHaveTextContent("null");
  });

  it("renders nothing (null messages) when the workspaceId is null", async () => {
    render(<Host workspaceId={null} />);
    expect(screen.getByTestId("count")).toHaveTextContent("null");
    expect(workspaceChatRead).not.toHaveBeenCalled();
  });
});

describe("SW-WC-3 · mergeMessages (dedup helper)", () => {
  it("appends new ids and drops duplicates by messageId", () => {
    const a = chatMsg(1);
    const b = chatMsg(2);
    const bDup = chatMsg(2, { text: "oops" });
    const c = chatMsg(3);
    const result = mergeMessages([a, b], [bDup, c]);
    expect(result.map((m) => m.messageId)).toEqual([1, 2, 3]);
    // The original b is preserved — duplicates don't overwrite.
    expect(result[1].text).toBe("message 2");
  });

  it("returns the same reference when nothing is added", () => {
    const a = chatMsg(1);
    const prev = [a];
    expect(mergeMessages(prev, [chatMsg(1, { text: "dup" })])).toBe(prev);
    expect(mergeMessages(prev, [])).toBe(prev);
  });
});
