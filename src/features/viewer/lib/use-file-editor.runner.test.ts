// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";

// The React shell over the buffer machine: it turns inputs into events and
// runs the machine's commands. The rules themselves are model-checked in
// buffer-machine.test.ts; these check the wiring.

const readFileMock = vi.fn();
vi.mock("@/tauri/commands", () => ({
  readFile: (...args: unknown[]) => readFileMock(...args),
  WORKTREE_WRITE_CONFLICT_PREFIX: "conflict:",
}));

import { LOCAL_ABSENT_TOKEN, TOO_LARGE_MESSAGE, sha256Hex, useFileEditor } from "./use-file-editor";

afterEach(() => {
  readFileMock.mockReset();
  vi.useRealTimers();
});

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("useFileEditor — the IO runner", () => {
  it("StrictMode issues a single first read, and it lands", async () => {
    readFileMock.mockResolvedValue("hello");
    const { result } = renderHook(
      () => useFileEditor({ path: "/wt/a.ts", size: 5 }, 0, { write: vi.fn() }),
      { reactStrictMode: true },
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    expect(result.current.buffer).toBe("hello");
    expect(readFileMock).toHaveBeenCalledTimes(1);
  });

  it("a read landing after unmount changes nothing and writes nothing", async () => {
    const read = deferred<string>();
    readFileMock.mockReturnValue(read.promise);
    const write = vi.fn();
    const { result, unmount } = renderHook(() =>
      useFileEditor({ path: "/wt/a.ts", size: 5 }, 0, { write }),
    );
    unmount();
    await act(async () => read.resolve("late"));
    expect(result.current.state.kind).toBe("loading");
    expect(write).not.toHaveBeenCalled();
  });

  it("a path move drops the old path's read and carries unsaved edits to the new one", async () => {
    let first = true;
    const old = deferred<string>();
    readFileMock.mockImplementation((p: string) => {
      if (first) {
        first = false;
        return Promise.resolve("base");
      }
      return p.startsWith("/old") ? old.promise : Promise.resolve("base");
    });
    const { result, rerender } = renderHook(
      ({ path, n }: { path: string; n: number }) =>
        useFileEditor({ path, size: 4 }, n, { write: vi.fn() }),
      { initialProps: { path: "/old/a.ts", n: 0 } },
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => result.current.setBuffer("mine"));
    rerender({ path: "/old/a.ts", n: 1 }); // a read on the old path goes out
    rerender({ path: "/new/a.ts", n: 1 }); // the root moves
    await waitFor(() => expect(readFileMock).toHaveBeenLastCalledWith("/new/a.ts"));
    await act(async () => old.resolve("stale old-path text"));
    expect(result.current.buffer).toBe("mine");
    expect(result.current.dirty).toBe(true);
    expect(result.current.stale).toBe(false);
  });

  it("the remote poll turns a moved hash into one read", async () => {
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
    await waitFor(() => expect(result.current.buffer).toBe("v1"));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1200);
    });
    expect(read).toHaveBeenCalledTimes(1);
    content = { content: "v2", hash: "h2" };
    current = "h2";
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1200);
    });
    await waitFor(() => expect(result.current.buffer).toBe("v2"));
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("edits typed during a save are saved next (trailing flush)", async () => {
    readFileMock.mockResolvedValue("base");
    const first = deferred<void>();
    const write = vi.fn().mockReturnValueOnce(first.promise).mockResolvedValue(undefined);
    const { result } = renderHook(() => useFileEditor({ path: "/wt/a.ts", size: 4 }, 0, { write }));
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    act(() => {
      result.current.setBuffer("one");
      void result.current.save("one");
    });
    act(() => result.current.setBuffer("one two"));
    await act(async () => first.resolve());
    await waitFor(() => expect(write).toHaveBeenCalledTimes(2));
    expect(write.mock.calls[1]?.slice(1)).toEqual(["one two", await sha256Hex("one")]);
    await waitFor(() => expect(result.current.dirty).toBe(false));
  });
});

describe("useFileEditor — restored drafts and gone files", () => {
  it("a restored draft can't save until disk is checked; then it saves locked to its base", async () => {
    const check = deferred<string>();
    readFileMock.mockReturnValue(check.promise);
    const write = vi.fn().mockResolvedValue(undefined);
    const { result } = renderHook(() =>
      useFileEditor({ path: "/wt/a.ts", size: 4 }, 0, {
        write,
        restore: { buffer: "mine", lastSaved: "base" },
      }),
    );
    let saved = false;
    act(() => {
      void result.current.save("mine").then(() => (saved = true));
    });
    expect(write).not.toHaveBeenCalled();
    await act(async () => check.resolve("base"));
    await waitFor(() => expect(saved).toBe(true));
    expect(write).toHaveBeenCalledWith("/wt/a.ts", "mine", await sha256Hex("base"));
  });

  it("a restored draft whose file is gone saves only while the file is still absent", async () => {
    readFileMock.mockRejectedValue(new Error("No such file or directory (os error 2)"));
    const write = vi.fn().mockResolvedValue(undefined);
    const { result } = renderHook(() =>
      useFileEditor({ path: "/wt/a.ts", size: 4 }, 0, {
        write,
        restore: { buffer: "mine", lastSaved: "base" },
      }),
    );
    await waitFor(() => expect(readFileMock).toHaveBeenCalled());
    await act(async () => {
      await result.current.save("mine");
    });
    expect(write).toHaveBeenCalledWith("/wt/a.ts", "mine", LOCAL_ABSENT_TOKEN);
  });

  it("a restored draft listed over the size limit stays in view, can't save, and Reload discards to the placeholder", async () => {
    const write = vi.fn();
    const { result } = renderHook(() =>
      useFileEditor({ path: "/wt/big.log", size: 50_000_000 }, 0, {
        write,
        restore: { buffer: "mine", lastSaved: "base" },
      }),
    );
    expect(result.current.buffer).toBe("mine");
    expect(result.current.showStaleBanner).toBe(true);
    await act(async () => {
      await result.current.save("mine");
    });
    expect(write).not.toHaveBeenCalled();
    expect(readFileMock).not.toHaveBeenCalled();
    act(() => result.current.reloadFromDisk());
    expect(result.current.state).toEqual({ kind: "placeholder", message: TOO_LARGE_MESSAGE });
  });
});

describe("useFileEditor — a seed is a draft with an unverified base", () => {
  it("edits typed before the read keep, with no conflict when disk matches the seed", async () => {
    const read = deferred<string>();
    readFileMock.mockReturnValue(read.promise);
    const { result } = renderHook(() =>
      useFileEditor({ path: "/c/a.md", size: null }, 0, { write: vi.fn(), seed: () => "seed" }),
    );
    expect(result.current.state.kind).toBe("editable");
    act(() => result.current.setBuffer("typed"));
    await act(async () => read.resolve("seed"));
    expect(result.current.buffer).toBe("typed");
    expect(result.current.stale).toBe(false);
  });

  it("edits typed over a stale seed are kept as a conflict, and the save is refused", async () => {
    const read = deferred<string>();
    readFileMock.mockReturnValue(read.promise);
    const write = vi.fn();
    const { result } = renderHook(() =>
      useFileEditor({ path: "/c/a.md", size: null }, 0, { write, seed: () => "seed" }),
    );
    act(() => result.current.setBuffer("typed"));
    await act(async () => read.resolve("newer on disk"));
    expect(result.current.buffer).toBe("typed");
    expect(result.current.stale).toBe(true);
    await act(async () => {
      await result.current.save("typed");
    });
    expect(write).not.toHaveBeenCalled();
  });
});

describe("useFileEditor — review round 5", () => {
  it("a file the user saved past the view limit stays editable when it is read back", async () => {
    const big = "x".repeat(1_600_000);
    readFileMock.mockResolvedValueOnce("small");
    const write = vi.fn(async (_p: string, c: string) => {
      readFileMock.mockResolvedValue(c);
    });
    const { result, rerender } = renderHook(
      ({ size }: { size: number }) => useFileEditor({ path: "/wt/a.txt", size }, 0, { write }),
      { initialProps: { size: 5 } },
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    await act(async () => {
      await result.current.save(big);
    });
    rerender({ size: big.length }); // the tree relists the new, larger size
    await waitFor(() => expect(readFileMock).toHaveBeenCalledTimes(2));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(result.current.state.kind).toBe("editable");
    expect(result.current.dirty).toBe(false);
  });

  it("a failed read for a polled hash change is retried by the next tick", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let fail = false;
    let content = { content: "v1", hash: "h1" };
    const read = vi.fn(async () => {
      if (fail) throw { kind: "disconnected" };
      return content;
    });
    let current = "h1";
    const { result } = renderHook(() =>
      useFileEditor({ path: "/r/a.ts", size: 2 }, 0, {
        write: vi.fn(),
        remote: { readWithHash: read, inspectHash: async () => current },
        pollMs: 500,
      }),
    );
    await waitFor(() => expect(result.current.buffer).toBe("v1"));
    content = { content: "v2", hash: "h2" };
    current = "h2";
    fail = true;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(700);
    });
    fail = false;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1200);
    });
    await waitFor(() => expect(result.current.buffer).toBe("v2"));
  });

  it("a refused save that turns out to need nothing says so, rather than silently not saving", async () => {
    readFileMock.mockResolvedValue("base");
    const write = vi.fn().mockRejectedValueOnce("conflict: moved").mockResolvedValue(undefined);
    const { result } = renderHook(() => useFileEditor({ path: "/wt/a.ts", size: 4 }, 0, { write }));
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    await act(async () => {
      await result.current.save("mine");
    });
    await waitFor(() => expect(readFileMock).toHaveBeenCalledTimes(2));
    expect(result.current.stale).toBe(false);
    expect(result.current.saveError).toMatch(/save again/i);
  });
});

describe("useFileEditor — reads that cost (review round 6)", () => {
  it("a remote dirty buffer over unviewable disk is not re-read on every poll tick", async () => {
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
    await waitFor(() => expect(result.current.buffer).toBe("v1"));
    act(() => result.current.setBuffer("mine"));
    content = { content: "bin\u0000ary", hash: "h2" };
    current = "h2";
    await act(async () => {
      await vi.advanceTimersByTimeAsync(700);
    });
    await waitFor(() => expect(read).toHaveBeenCalledTimes(2));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3000);
    });
    expect(read).toHaveBeenCalledTimes(2);
    expect(result.current.stale).toBe(true);
  });

  it("an open file listed over the limit is re-read when its size changes, not on every change", async () => {
    readFileMock.mockResolvedValueOnce("small");
    const write = vi.fn(async (_p: string, c: string) => {
      readFileMock.mockResolvedValue(c);
    });
    const big = "x".repeat(1_600_000);
    const { result, rerender } = renderHook(
      ({ size, n }: { size: number; n: number }) =>
        useFileEditor({ path: "/wt/a.txt", size }, n, { write }),
      { initialProps: { size: 5, n: 0 } },
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    await act(async () => {
      await result.current.save(big);
    });
    rerender({ size: big.length, n: 0 });
    await waitFor(() => expect(readFileMock).toHaveBeenCalledTimes(2));
    rerender({ size: big.length, n: 1 });
    rerender({ size: big.length, n: 2 });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(readFileMock).toHaveBeenCalledTimes(2);
    expect(result.current.state.kind).toBe("editable");
  });
});

describe("useFileEditor — reads that cost, without going blind (review round 7)", () => {
  it("a remote dirty buffer over non-UTF-8 disk is not re-read on every poll tick", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let fail = false;
    const read = vi.fn(async () => {
      if (fail) throw { kind: "notUtf8", hash: "h2" };
      return { content: "v1", hash: "h1" };
    });
    let current = "h1";
    const { result } = renderHook(() =>
      useFileEditor({ path: "/r/a.ts", size: 2 }, 0, {
        write: vi.fn(),
        remote: { readWithHash: read, inspectHash: async () => current },
        pollMs: 500,
      }),
    );
    await waitFor(() => expect(result.current.buffer).toBe("v1"));
    act(() => result.current.setBuffer("mine"));
    fail = true;
    current = "h2";
    await act(async () => {
      await vi.advanceTimersByTimeAsync(700);
    });
    await waitFor(() => expect(read).toHaveBeenCalledTimes(2));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3000);
    });
    expect(read).toHaveBeenCalledTimes(2);
    expect(result.current.stale).toBe(true);
    expect(result.current.buffer).toBe("mine");
  });

  it("an open local file listed over the limit still sees a same-size change (trailing re-check)", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    readFileMock.mockResolvedValueOnce("small");
    const write = vi.fn(async (_p: string, c: string) => {
      readFileMock.mockResolvedValue(c);
    });
    const big = "x".repeat(1_600_000);
    const { result, rerender } = renderHook(
      ({ size, n }: { size: number; n: number }) =>
        useFileEditor({ path: "/wt/a.txt", size }, n, { write }),
      { initialProps: { size: 5, n: 0 } },
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    await act(async () => {
      await result.current.save(big);
    });
    rerender({ size: big.length, n: 0 }); // relisted over the limit: read
    await waitFor(() => expect(readFileMock).toHaveBeenCalledTimes(2));
    expect(result.current.state.kind).toBe("editable");
    readFileMock.mockResolvedValue("y".repeat(1_600_000)); // same size, other bytes
    rerender({ size: big.length, n: 1 });
    rerender({ size: big.length, n: 2 });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10);
    });
    expect(readFileMock).toHaveBeenCalledTimes(2); // throttled, not read per change
    expect(result.current.state.kind).toBe("editable");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(6000);
    });
    await waitFor(() => expect(readFileMock).toHaveBeenCalledTimes(3));
    // The change was seen: a clean buffer over too-large disk is a placeholder.
    await waitFor(() => expect(result.current.state.kind).not.toBe("editable"));
  });

  it("an open remote file listed over the limit is re-read when its hash moves, not otherwise", async () => {
    let version = { content: "small", hash: "h0" };
    const read = vi.fn(async () => version);
    let current = "h0";
    const inspect = vi.fn(async () => current);
    const write = vi.fn(async (_p: string, c: string) => {
      version = { content: c, hash: "h1" };
      current = "h1";
      return { hash: "h1" };
    });
    const big = "x".repeat(1_600_000);
    const { result, rerender } = renderHook(
      ({ size, n }: { size: number; n: number }) =>
        useFileEditor({ path: "/r/a.txt", size }, n, {
          write,
          remote: { readWithHash: read, inspectHash: inspect },
          pollMs: 0,
        }),
      { initialProps: { size: 5, n: 0 } },
    );
    await waitFor(() => expect(result.current.state.kind).toBe("editable"));
    await act(async () => {
      await result.current.save(big);
    });
    rerender({ size: big.length, n: 0 }); // relisted over the limit: read
    await waitFor(() => expect(read).toHaveBeenCalledTimes(2));
    rerender({ size: big.length, n: 1 }); // nothing moved: inspect only
    await waitFor(() => expect(inspect).toHaveBeenCalledTimes(1));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(read).toHaveBeenCalledTimes(2);
    expect(result.current.state.kind).toBe("editable");
    version = { content: "y".repeat(1_600_000), hash: "h2" };
    current = "h2";
    rerender({ size: big.length, n: 2 }); // in-place change, same listed size
    await waitFor(() => expect(read).toHaveBeenCalledTimes(3));
    await waitFor(() => expect(result.current.state.kind).not.toBe("editable"));
  });
});
