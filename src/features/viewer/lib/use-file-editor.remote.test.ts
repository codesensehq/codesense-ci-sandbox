// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";

// DESIGN-remote-phase-2.md §3 — library-level coverage of the remote adapter
// path through useFileEditor: reads thread an initial hash, writes receive
// `expectedHash`, and a `Conflict` from the write triggers a merge-view seed
// via a follow-up read (hook never silently drops the user's buffer).

const readFileMock = vi.fn();
vi.mock("@/tauri/commands", () => ({
  readFile: (...args: unknown[]) => readFileMock(...args),
  WORKTREE_WRITE_CONFLICT_PREFIX: "conflict:",
}));

import { isRemoteConflict, sha256Hex, useFileEditor } from "./use-file-editor";

describe("useFileEditor — remote adapter", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    readFileMock.mockReset();
  });

  it("isRemoteConflict matches the Rust RemoteSourceError Conflict shape", () => {
    expect(isRemoteConflict({ kind: "conflict", observation: { hash: "a".repeat(64) } })).toBe(
      true,
    );
    expect(isRemoteConflict({ kind: "transportFailure", message: "oops" })).toBe(false);
    expect(isRemoteConflict(null)).toBe(false);
    expect(isRemoteConflict("conflict")).toBe(false);
    expect(isRemoteConflict({ kind: "conflict" })).toBe(false);
  });

  it("remote read seeds buffer from the adapter, not readFile", async () => {
    const read = vi.fn().mockResolvedValue({ content: "hello", hash: "h0" });
    const write = vi.fn().mockResolvedValue({ hash: "h1" });
    const { result } = renderHook(() =>
      useFileEditor({ path: "/wt/a.ts", size: 5 }, 0, { write, remote: { readWithHash: read } }),
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    expect(read).toHaveBeenCalledWith("/wt/a.ts");
    expect(readFileMock).not.toHaveBeenCalled();
    expect(result.current.buffer).toBe("hello");
  });

  it("remote save threads the initial read's hash as expectedHash", async () => {
    const read = vi.fn().mockResolvedValue({ content: "a", hash: "h0" });
    const write = vi.fn().mockResolvedValue({ hash: "h1" });
    const { result } = renderHook(() =>
      useFileEditor({ path: "/wt/a.ts", size: 1 }, 0, { write, remote: { readWithHash: read } }),
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    // Fire the save WITHOUT awaiting (the trailing-flush loop would spin on the
    // mock promise chain forever); then poll for the call we care about.
    act(() => {
      result.current.setBuffer("ab");
      void result.current.save("ab");
    });
    await waitFor(() => expect(write).toHaveBeenCalled());
    const firstCall = write.mock.calls[0];
    expect(firstCall?.[0]).toBe("/wt/a.ts");
    expect(firstCall?.[1]).toBe("ab");
    expect(firstCall?.[2]).toBe("h0");
  });

  it("a refused remote save reads disk and shows the conflict", async () => {
    let readCount = 0;
    const read = vi.fn(async () => {
      readCount += 1;
      if (readCount === 1) return { content: "a", hash: "h0" };
      return { content: "AGENT-WROTE-THIS", hash: "h2" };
    });
    const write = vi.fn().mockRejectedValue({ kind: "conflict", observation: { hash: "h2" } });
    const { result } = renderHook(() =>
      useFileEditor({ path: "/wt/a.ts", size: 1 }, 0, { write, remote: { readWithHash: read } }),
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => {
      result.current.setBuffer("mine");
      void result.current.save("mine");
    });
    await waitFor(() => expect(result.current.stale).toBe(true), { timeout: 2000 });
    // The dirty buffer survived the conflict (AC-2: never silently drop edits).
    expect(result.current.buffer).toBe("mine");
    expect(result.current.dirty).toBe(true);
    // Reload takes the agent's content, synchronously.
    act(() => result.current.reloadFromDisk());
    expect(result.current.buffer).toBe("AGENT-WROTE-THIS");
  });

  // DESIGN-remote-phase-2.md B-5 — the save catch exhaustively dispatches
  // every RemoteSourceError kind to its spec'd behavior.
  it("Disconnected variant surfaces a user-facing message, buffer stays dirty", async () => {
    const read = vi.fn().mockResolvedValue({ content: "a", hash: "h0" });
    const write = vi.fn().mockRejectedValue({ kind: "disconnected" });
    const { result } = renderHook(() =>
      useFileEditor({ path: "/wt/a.ts", size: 1 }, 0, { write, remote: { readWithHash: read } }),
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => {
      result.current.setBuffer("mine");
      void result.current.save("mine");
    });
    await waitFor(() => expect(result.current.saveError).toMatch(/Can't reach the remote machine/));
    expect(result.current.dirty).toBe(true);
  });

  it("Unauthorized variant surfaces the revoked-authority message", async () => {
    const read = vi.fn().mockResolvedValue({ content: "a", hash: "h0" });
    const write = vi.fn().mockRejectedValue({ kind: "unauthorized" });
    const { result } = renderHook(() =>
      useFileEditor({ path: "/wt/a.ts", size: 1 }, 0, { write, remote: { readWithHash: read } }),
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => {
      result.current.setBuffer("mine");
      void result.current.save("mine");
    });
    await waitFor(() => expect(result.current.saveError).toMatch(/authority revoked/i));
    expect(result.current.dirty).toBe(true);
  });

  it("a save refused because the file is gone recreates it, asserting it is still absent", async () => {
    let reads = 0;
    const read = vi.fn(async () => {
      reads += 1;
      if (reads === 1) return { content: "a", hash: "h0" };
      throw { kind: "notFound" };
    });
    const write = vi
      .fn()
      .mockRejectedValueOnce({ kind: "notFound" })
      .mockResolvedValue({ hash: "h1" });
    const { result } = renderHook(() =>
      useFileEditor({ path: "/wt/a.ts", size: 1 }, 0, { write, remote: { readWithHash: read } }),
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => {
      result.current.setBuffer("mine");
      void result.current.save("mine");
    });
    await waitFor(() => expect(read).toHaveBeenCalledTimes(2));
    expect(result.current.buffer).toBe("mine");
    await act(async () => {
      await result.current.save("mine");
    });
    // `undefined` asks the machine to create the file only if it is still absent.
    expect(write).toHaveBeenLastCalledWith("/wt/a.ts", "mine", undefined);
    expect(result.current.dirty).toBe(false);
  });

  it("BoundsExceeded variant surfaces the 4 MiB cap guidance", async () => {
    const read = vi.fn().mockResolvedValue({ content: "a", hash: "h0" });
    const write = vi.fn().mockRejectedValue({ kind: "boundsExceeded" });
    const { result } = renderHook(() =>
      useFileEditor({ path: "/wt/a.ts", size: 1 }, 0, { write, remote: { readWithHash: read } }),
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => {
      result.current.setBuffer("mine");
      void result.current.save("mine");
    });
    await waitFor(() => expect(result.current.saveError).toMatch(/4 MiB/i));
  });

  it("TransportFailure variant surfaces the carried message", async () => {
    const read = vi.fn().mockResolvedValue({ content: "a", hash: "h0" });
    const write = vi.fn().mockRejectedValue({
      kind: "transportFailure",
      message: "custom transport error",
    });
    const { result } = renderHook(() =>
      useFileEditor({ path: "/wt/a.ts", size: 1 }, 0, { write, remote: { readWithHash: read } }),
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => {
      result.current.setBuffer("mine");
      void result.current.save("mine");
    });
    await waitFor(() => expect(result.current.saveError).toBe("custom transport error"));
  });

  // H-4/H-8 — a second ⌘S after a refused save, while the read that settles
  // it is still out, waits for that read instead of writing again.
  it("a refused save blocks a second write until disk has been read", async () => {
    let followUpResolver!: (v: { content: string; hash: string }) => void;
    const read = vi
      .fn()
      .mockResolvedValueOnce({ content: "a", hash: "h0" })
      .mockImplementation(() => new Promise((r) => (followUpResolver = r)));
    const write = vi.fn().mockRejectedValue({ kind: "conflict", observation: { hash: "h2" } });
    const { result } = renderHook(() =>
      useFileEditor({ path: "/wt/a.ts", size: 1 }, 0, { write, remote: { readWithHash: read } }),
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => {
      result.current.setBuffer("mine");
      void result.current.save("mine");
    });
    await waitFor(() => expect(read).toHaveBeenCalledTimes(2));
    act(() => {
      void result.current.save("mine-again");
    });
    await Promise.resolve();
    expect(write).toHaveBeenCalledTimes(1);
    await act(async () => followUpResolver({ content: "AGENT-WROTE-THIS", hash: "h2" }));
    expect(result.current.stale).toBe(true);
    expect(write).toHaveBeenCalledTimes(1);
  });

  // DESIGN-remote-phase-2.md H-9 — a file switch clears the baseline hash so
  // the first save on the new file doesn't thread the prior file's hash.
  it("file-switch clears the optimistic-lock baseline", async () => {
    const read = vi
      .fn()
      .mockResolvedValueOnce({ content: "a", hash: "h0" })
      .mockResolvedValueOnce({ content: "b", hash: "hNEW" });
    const write = vi.fn().mockResolvedValue({ hash: "hAFTER" });
    const { result, rerender } = renderHook(
      ({ path }: { path: string }) =>
        useFileEditor({ path, size: 1 }, 0, { write, remote: { readWithHash: read } }),
      { initialProps: { path: "/wt/a.ts" } },
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    // Switch files BEFORE saving.
    rerender({ path: "/wt/b.ts" });
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    // Save now — expectedHash should be hNEW (b.ts's read hash), NOT h0.
    act(() => {
      result.current.setBuffer("edited");
      void result.current.save("edited");
    });
    await waitFor(() => expect(write).toHaveBeenCalled());
    const call = write.mock.calls[0];
    expect(call?.[0]).toBe("/wt/b.ts");
    expect(call?.[2]).toBe("hNEW");
  });

  it("local writes are locked too: the SHA-256 of the base text", async () => {
    readFileMock.mockResolvedValue("a");
    const write = vi.fn().mockResolvedValue(undefined);
    const { result } = renderHook(() => useFileEditor({ path: "/wt/a.ts", size: 1 }, 0, { write }));
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => result.current.setBuffer("ab"));
    await act(async () => {
      await result.current.save("ab");
    });
    expect(write).toHaveBeenCalledWith("/wt/a.ts", "ab", await sha256Hex("a"));
  });
});
