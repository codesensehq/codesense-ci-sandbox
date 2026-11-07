// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, afterEach } from "vitest";
import { createPtyWriteBuffer } from "./pty-write-buffer";

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("createPtyWriteBuffer", () => {
  it("coalesces multiple appends into a single write per frame", () => {
    let rafCallback: (() => void) | null = null;
    vi.stubGlobal("requestAnimationFrame", (cb: () => void) => {
      rafCallback = cb;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());

    const write = vi.fn();
    const buf = createPtyWriteBuffer(write);

    buf.append("hello ");
    buf.append("world");

    expect(write).not.toHaveBeenCalled();

    rafCallback!();

    expect(write).toHaveBeenCalledTimes(1);
    expect(write).toHaveBeenCalledWith("hello world");
  });

  it("does not schedule multiple rAFs for rapid appends", () => {
    const rafSpy = vi.fn(() => 1);
    vi.stubGlobal("requestAnimationFrame", rafSpy);
    vi.stubGlobal("cancelAnimationFrame", vi.fn());

    const buf = createPtyWriteBuffer(vi.fn());
    buf.append("a");
    buf.append("b");
    buf.append("c");

    expect(rafSpy).toHaveBeenCalledTimes(1);
  });

  it("schedules a new rAF after the previous one fires", () => {
    const callbacks: (() => void)[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: () => void) => {
      callbacks.push(cb);
      return callbacks.length;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());

    const write = vi.fn();
    const buf = createPtyWriteBuffer(write);

    buf.append("frame1");
    callbacks[0]();
    expect(write).toHaveBeenCalledWith("frame1");

    buf.append("frame2");
    callbacks[1]();
    expect(write).toHaveBeenCalledWith("frame2");
    expect(write).toHaveBeenCalledTimes(2);
  });

  it("flush cancels the pending frame and writes synchronously", () => {
    const cancelRaf = vi.fn();
    vi.stubGlobal("requestAnimationFrame", () => 42);
    vi.stubGlobal("cancelAnimationFrame", cancelRaf);

    const write = vi.fn();
    const buf = createPtyWriteBuffer(write);

    buf.append("pending data");
    buf.flush();

    expect(cancelRaf).toHaveBeenCalledWith(42);
    expect(write).toHaveBeenCalledTimes(1);
    expect(write).toHaveBeenCalledWith("pending data");
  });

  it("flush is a no-op when the buffer is empty", () => {
    vi.stubGlobal("requestAnimationFrame", () => 1);
    const cancelRaf = vi.fn();
    vi.stubGlobal("cancelAnimationFrame", cancelRaf);

    const write = vi.fn();
    const buf = createPtyWriteBuffer(write);
    buf.flush();

    expect(cancelRaf).not.toHaveBeenCalled();
    expect(write).not.toHaveBeenCalled();
  });

  it("separate instances do not share buffer state", () => {
    const callbacks: (() => void)[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: () => void) => {
      callbacks.push(cb);
      return callbacks.length;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());

    const write1 = vi.fn();
    const write2 = vi.fn();
    const buf1 = createPtyWriteBuffer(write1);
    const buf2 = createPtyWriteBuffer(write2);

    buf1.append("A");
    buf2.append("B");

    callbacks.forEach((cb) => cb());

    expect(write1).toHaveBeenCalledWith("A");
    expect(write2).toHaveBeenCalledWith("B");
    expect(write1).toHaveBeenCalledTimes(1);
    expect(write2).toHaveBeenCalledTimes(1);
  });

  it("flushes on a timer while the document is hidden and writes everything", () => {
    const raf = vi.fn(() => 1);
    vi.stubGlobal("requestAnimationFrame", raf);
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    try {
      const write = vi.fn();
      const buf = createPtyWriteBuffer(write);
      buf.append("one ");
      buf.append("two ");
      buf.append("three");
      vi.advanceTimersByTime(1000);
      expect(raf).not.toHaveBeenCalled();
      expect(write).toHaveBeenCalledTimes(1);
      expect(write).toHaveBeenCalledWith("one two three");
    } finally {
      visibility.mockRestore();
      vi.useRealTimers();
    }
  });

  it("keeps the bytes of a failed write for the next flush, in order", () => {
    vi.stubGlobal("requestAnimationFrame", () => 1);
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const written: string[] = [];
    let fail = true;
    const buf = createPtyWriteBuffer((d) => {
      if (fail) throw new Error("renderer gone");
      written.push(d);
    });
    buf.append("a");
    buf.flush();
    fail = false;
    buf.append("b");
    buf.flush();
    expect(written).toEqual(["ab"]);
  });
});
