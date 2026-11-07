// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";

// The editor model's save-safety and remote-awareness additions: the local
// baseline hash + conflict refusal, the remote open-file poll, non-UTF-8 files,
// readable errors for typed remote failures, and a remote recreate after a
// file vanished.

const readFileMock = vi.fn();
vi.mock("@/tauri/commands", () => ({
  readFile: (...args: unknown[]) => readFileMock(...args),
  WORKTREE_WRITE_CONFLICT_PREFIX: "conflict:",
}));

import {
  NOT_UTF8_MESSAGE,
  TOO_LARGE_MESSAGE,
  describeFileError,
  isNotUtf8Error,
  sha256Hex,
  useFileEditor,
} from "./use-file-editor";

beforeEach(() => {
  readFileMock.mockReset();
});
afterEach(() => {
  vi.useRealTimers();
});

describe("useFileEditor — local save baseline", () => {
  it("sends the SHA-256 of the disk baseline when asked to", async () => {
    readFileMock.mockResolvedValue("original\n");
    const write = vi.fn().mockResolvedValue(undefined);
    const { result } = renderHook(() => useFileEditor({ path: "/wt/a.ts", size: 9 }, 0, { write }));
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => {
      result.current.setBuffer("edited\n");
      void result.current.save("edited\n");
    });
    await waitFor(() => expect(write).toHaveBeenCalled());
    expect(write.mock.calls[0]?.[2]).toBe(await sha256Hex("original\n"));
    await waitFor(() => expect(result.current.dirty).toBe(false));
    expect(result.current.diskText).toBe("edited\n");
  });

  it("a refused (moved-on-disk) save arms the conflict guard with the disk version", async () => {
    let reads = 0;
    readFileMock.mockImplementation(async () => (++reads === 1 ? "base\n" : "agent wrote\n"));
    const write = vi.fn().mockRejectedValue(new Error("conflict: /wt/a.ts changed on disk"));
    const { result } = renderHook(() => useFileEditor({ path: "/wt/a.ts", size: 5 }, 0, { write }));
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => {
      result.current.setBuffer("mine\n");
      void result.current.save("mine\n");
    });
    await waitFor(() => expect(result.current.stale).toBe(true));
    expect(result.current.showStaleBanner).toBe(true);
    expect(result.current.buffer).toBe("mine\n");
    // A plain save is now blocked (no second write) until the user resolves.
    await act(async () => {
      await result.current.save("mine\n");
    });
    expect(write).toHaveBeenCalledTimes(1);
    // Taking the disk version adopts what the agent wrote.
    await waitFor(() => expect(readFileMock).toHaveBeenCalledTimes(2));
    act(() => result.current.reloadFromDisk());
    expect(result.current.buffer).toBe("agent wrote\n");
    expect(result.current.dirty).toBe(false);
  });
});

describe("useFileEditor — keep mine (merge resolve)", () => {
  it("locks a local keep-mine to the disk text the merge showed", async () => {
    let reads = 0;
    readFileMock.mockImplementation(async () => (++reads === 1 ? "base\n" : "agent wrote\n"));
    const write = vi.fn().mockRejectedValueOnce(new Error("conflict: moved"));
    write.mockResolvedValue(undefined);
    const { result } = renderHook(() => useFileEditor({ path: "/wt/a.ts", size: 5 }, 0, { write }));
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => {
      result.current.setBuffer("mine\n");
      void result.current.save("mine\n");
    });
    await waitFor(() => expect(result.current.stale).toBe(true));
    act(() => result.current.startReconcile());
    await waitFor(() => expect(result.current.reconciling).toBe(true));
    expect(result.current.reconcileDisk).toBe("agent wrote\n");
    await act(async () => {
      await result.current.resolveSave("mine\n");
    });
    expect(write).toHaveBeenLastCalledWith("/wt/a.ts", "mine\n", await sha256Hex("agent wrote\n"));
    expect(result.current.reconciling).toBe(false);
  });

  it("keeps the merge's lock when the refreshed disk side can't be shown", async () => {
    let content = { content: "v1", hash: "h1" };
    const read = vi.fn(async () => content);
    const write = vi.fn().mockRejectedValue({ kind: "conflict", observation: { hash: "h3" } });
    const { result, rerender } = renderHook(
      ({ n }: { n: number }) =>
        useFileEditor({ path: "/r/a.ts", size: 2 }, n, { write, remote: { readWithHash: read } }),
      { initialProps: { n: 0 } },
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => result.current.setBuffer("mine"));
    content = { content: "theirs", hash: "h2" };
    rerender({ n: 1 });
    await waitFor(() => expect(result.current.stale).toBe(true));
    act(() => result.current.startReconcile());
    expect(result.current.reconciling).toBe(true);
    // The machine's file turns binary before "keep mine" lands.
    content = { content: "bin\u0000ary", hash: "h3" };
    await act(async () => {
      await result.current.resolveSave("mine");
    });
    await waitFor(() => expect(result.current.reconcileError).toMatch(/binary or too large/));
    await act(async () => {
      await result.current.resolveSave("mine");
    });
    // Still locked to the version the user reviewed, not the unseen binary.
    expect(write.mock.calls.at(-1)?.[2]).toBe("h2");
    expect(result.current.reconcileDisk).toBe("theirs");
  });
});

describe("useFileEditor — remote open-file poll", () => {
  it("re-reads a clean buffer when the machine-side hash moves", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let content = { content: "v1", hash: "h1" };
    const read = vi.fn(async () => content);
    let current = "h1";
    const inspectHash = vi.fn(async () => current);
    const write = vi.fn();
    const { result } = renderHook(() =>
      useFileEditor({ path: "/r/a.ts", size: 2 }, 0, {
        write,
        remote: { readWithHash: read, inspectHash },
        pollMs: 500,
      }),
    );
    await waitFor(() => expect(result.current.buffer).toBe("v1"));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(600);
    });
    expect(inspectHash).toHaveBeenCalledWith("/r/a.ts");
    expect(read).toHaveBeenCalledTimes(1); // unchanged hash → no re-read
    content = { content: "v2 from the machine", hash: "h2" };
    current = "h2";
    await act(async () => {
      await vi.advanceTimersByTimeAsync(600);
    });
    await waitFor(() => expect(result.current.buffer).toBe("v2 from the machine"));
    expect(result.current.dirty).toBe(false);
  });

  it("arms the conflict guard instead when the buffer is dirty", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let content = { content: "v1", hash: "h1" };
    const read = vi.fn(async () => content);
    let current = "h1";
    const { result } = renderHook(() =>
      useFileEditor({ path: "/r/a.ts", size: 2 }, 0, {
        write: vi.fn(),
        remote: { readWithHash: read, inspectHash: async () => current },
        pollMs: 500,
      }),
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => result.current.setBuffer("my edit"));
    content = { content: "theirs", hash: "h2" };
    current = "h2";
    await act(async () => {
      await vi.advanceTimersByTimeAsync(600);
    });
    await waitFor(() => expect(result.current.stale).toBe(true));
    expect(result.current.buffer).toBe("my edit");
  });
});

describe("useFileEditor — unreadable files and errors", () => {
  it("shows a placeholder (never an editable buffer) for non-UTF-8 bytes", async () => {
    readFileMock.mockRejectedValue("stream did not contain valid UTF-8");
    const { result } = renderHook(() =>
      useFileEditor({ path: "/wt/latin1.txt", size: 10 }, 0, { write: vi.fn() }),
    );
    await waitFor(() =>
      expect(result.current.state).toEqual({ kind: "placeholder", message: NOT_UTF8_MESSAGE }),
    );
  });

  it("recognises the remote adapter's typed decode error", () => {
    expect(isNotUtf8Error({ kind: "notUtf8" })).toBe(true);
    expect(isNotUtf8Error(new Error("boom"))).toBe(false);
  });

  it("describes typed remote errors instead of [object Object]", () => {
    expect(describeFileError({ kind: "transportFailure", message: "pipe closed" })).toBe(
      "pipe closed",
    );
    expect(describeFileError({ kind: "disconnected" })).toMatch(/remote machine/);
    expect(describeFileError({ kind: "boundsExceeded" })).toMatch(/4 MiB/);
  });
});

describe("useFileEditor — a file past the machine's size limit", () => {
  it("shows the too-large placeholder, not a read error with a Retry that can't succeed", async () => {
    const readWithHash = vi.fn().mockRejectedValue({ kind: "boundsExceeded" });
    const { result } = renderHook(() =>
      useFileEditor({ path: "/wt/huge.log", size: null }, 0, {
        write: vi.fn(),
        remote: { readWithHash, inspectHash: vi.fn() },
      }),
    );
    await waitFor(() => expect(result.current.state.kind).toBe("placeholder"));
    expect(result.current.state).toEqual({ kind: "placeholder", message: TOO_LARGE_MESSAGE });
  });
});

describe("useFileEditor — a re-read of the open file never stomps unsaved edits", () => {
  it("keeps a restored dirty draft when the tree later reports the file's size", async () => {
    readFileMock.mockResolvedValue("base\n");
    const restore = { buffer: "my edit\n", lastSaved: "base\n" };
    const { result, rerender } = renderHook(
      ({ size }: { size: number | null }) =>
        useFileEditor({ path: "/wt/a.ts", size }, 0, { write: vi.fn(), restore }),
      { initialProps: { size: null as number | null } },
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    expect(result.current.buffer).toBe("my edit\n");
    rerender({ size: 5 });
    await waitFor(() => expect(readFileMock).toHaveBeenCalledTimes(2));
    await act(async () => {});
    expect(result.current.buffer).toBe("my edit\n");
    expect(result.current.dirty).toBe(true);
    expect(result.current.stale).toBe(false);
  });

  it("arms the guard (never adopts) when disk moved under edits and the size changes", async () => {
    let disk = "base\n";
    readFileMock.mockImplementation(async () => disk);
    const { result, rerender } = renderHook(
      ({ size }: { size: number | null }) =>
        useFileEditor({ path: "/wt/a.ts", size }, 0, { write: vi.fn() }),
      { initialProps: { size: 5 as number | null } },
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => result.current.setBuffer("mine\n"));
    disk = "agent wrote more\n";
    rerender({ size: 17 });
    await waitFor(() => expect(result.current.stale).toBe(true));
    expect(result.current.buffer).toBe("mine\n");
  });
});

describe("useFileEditor — remote poll while merging", () => {
  it("a machine edit during the merge only flags it (no read); leaving re-checks once", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let content = { content: "v1", hash: "h1" };
    const read = vi.fn(async () => content);
    let current = "h1";
    const { result, rerender } = renderHook(
      ({ n }: { n: number }) =>
        useFileEditor({ path: "/r/a.ts", size: 2 }, n, {
          write: vi.fn(),
          remote: { readWithHash: read, inspectHash: async () => current },
          pollMs: 500,
        }),
      { initialProps: { n: 0 } },
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => result.current.setBuffer("mine"));
    content = { content: "theirs", hash: "h2" };
    current = "h2";
    rerender({ n: 1 });
    await waitFor(() => expect(result.current.stale).toBe(true));
    act(() => result.current.startReconcile());
    const readsAtMerge = read.mock.calls.length;
    content = { content: "bin\u0000ary", hash: "h3" };
    current = "h3";
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });
    expect(read).toHaveBeenCalledTimes(readsAtMerge);
    expect(result.current.reconcileDisk).toBe("theirs");
    act(() => result.current.cancelReconcile());
    await waitFor(() => expect(read).toHaveBeenCalledTimes(readsAtMerge + 1));
  });
});

// The per-bug race cases from review rounds 3–5 are covered by the buffer
// machine's model check (buffer-machine.test.ts) and the seeded hook smoke
// test (use-file-editor.races.test.ts).

describe("useFileEditor — review round 6", () => {
  it("Reload onto a disk that turned binary discards the edits: no save writes them back", async () => {
    let disk = "base\n";
    readFileMock.mockImplementation(async () => disk);
    const write = vi.fn().mockResolvedValue(undefined);
    const { result, rerender } = renderHook(
      ({ nonce }: { nonce: number }) =>
        useFileEditor({ path: "/wt/a.ts", size: 5 }, nonce, { write }),
      { initialProps: { nonce: 0 } },
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => result.current.setBuffer("mine\n"));
    disk = "bin\u0000ary";
    rerender({ nonce: 1 });
    await waitFor(() => expect(result.current.stale).toBe(true));
    act(() => result.current.reloadFromDisk());
    expect(result.current.state.kind).toBe("placeholder");
    expect(result.current.getDraft().dirty).toBe(false);
    await act(async () => {
      await result.current.save("mine\n");
    });
    expect(write).not.toHaveBeenCalled();
  });
});
