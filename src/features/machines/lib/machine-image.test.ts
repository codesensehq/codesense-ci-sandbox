// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  MAX_SOURCE_PIXELS,
  checkImageFile,
  imageSizeFromHeader,
  shrinkImageFile,
} from "./machine-image";

const bytes = (...parts: (number[] | string)[]) =>
  new Uint8Array(
    parts.flatMap((p) => (typeof p === "string" ? [...p].map((c) => c.charCodeAt(0)) : p)),
  );
const be32 = (n: number) => [(n >>> 24) & 255, (n >>> 16) & 255, (n >>> 8) & 255, n & 255];
const be16 = (n: number) => [(n >> 8) & 255, n & 255];
const le16 = (n: number) => [n & 255, (n >> 8) & 255];
const le24 = (n: number) => [n & 255, (n >> 8) & 255, (n >> 16) & 255];

const png = (w: number, h: number) =>
  bytes(
    [0x89],
    "PNG",
    [0x0d, 0x0a, 0x1a, 0x0a],
    be32(13),
    "IHDR",
    be32(w),
    be32(h),
    [8, 6, 0, 0, 0],
  );
const jpeg = (w: number, h: number) =>
  bytes(
    [0xff, 0xd8],
    // An APP1 (EXIF) segment before the frame, as cameras write.
    [0xff, 0xe1],
    be16(10),
    "Exif\0\0xx",
    [0xff, 0xc2],
    be16(17),
    [8],
    be16(h),
    be16(w),
    [3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1],
  );
const webpVp8x = (w: number, h: number) =>
  bytes(
    "RIFF",
    [0, 0, 0, 0],
    "WEBP",
    "VP8X",
    [10, 0, 0, 0],
    [0, 0, 0, 0],
    le24(w - 1),
    le24(h - 1),
  );
const webpVp8 = (w: number, h: number) =>
  bytes(
    "RIFF",
    [0, 0, 0, 0],
    "WEBP",
    "VP8 ",
    [0, 0, 0, 0],
    [0, 0, 0],
    [0x9d, 0x01, 0x2a],
    le16(w),
    le16(h),
  );
const webpVp8l = (w: number, h: number) => {
  const wm = w - 1;
  const hm = h - 1;
  return bytes(
    "RIFF",
    [0, 0, 0, 0],
    "WEBP",
    "VP8L",
    [0, 0, 0, 0],
    [0x2f],
    [wm & 0xff, ((wm >> 8) & 0x3f) | ((hm & 0x3) << 6), (hm >> 2) & 0xff, (hm >> 10) & 0x0f],
  );
};
const gif = (w: number, h: number) => bytes("GIF89a", le16(w), le16(h), [0, 0, 0]);

describe("imageSizeFromHeader", () => {
  it.each([
    ["PNG", png(3000, 2000)],
    ["JPEG", jpeg(3000, 2000)],
    ["WebP (VP8X)", webpVp8x(3000, 2000)],
    ["WebP (VP8)", webpVp8(3000, 2000)],
    ["WebP (VP8L)", webpVp8l(3000, 2000)],
    ["GIF", gif(3000, 2000)],
  ])("reads a %s's size without decoding it", (_, header) => {
    expect(imageSizeFromHeader(header)).toEqual({ width: 3000, height: 2000 });
  });

  it("knows nothing of other bytes", () => {
    expect(imageSizeFromHeader(bytes("<svg xmlns"))).toBeNull();
    expect(imageSizeFromHeader(new Uint8Array())).toBeNull();
  });
});

const file = (header: Uint8Array, type: string) => new File([header], "pic", { type });

describe("checkImageFile", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("refuses a decompression bomb before anything decodes it", async () => {
    const decode = vi.fn();
    vi.stubGlobal("createImageBitmap", decode);
    const bomb = file(png(50_000, 50_000), "image/png");
    await expect(shrinkImageFile(bomb, "#0d1117")).rejects.toThrow(/too many pixels/);
    expect(decode).not.toHaveBeenCalled();
    expect(50_000 * 50_000).toBeGreaterThan(MAX_SOURCE_PIXELS);
  });

  it("accepts a large but sane photo", async () => {
    await expect(checkImageFile(file(jpeg(6000, 4000), "image/jpeg"))).resolves.toEqual({
      width: 6000,
      height: 4000,
    });
  });

  it("says it couldn't read the size of a JPEG whose frame is past the scan, or a zero-size image", async () => {
    // An APP1 segment far larger than the scanned start pushes the frame out.
    const big = new Uint8Array(600 * 1024);
    big.set([0xff, 0xd8]);
    for (let i = 2; i + 4 <= big.length; i += 2 + 0xffff) big.set([0xff, 0xe1, 0xff, 0xff], i);
    await expect(checkImageFile(file(big, "image/jpeg"))).rejects.toThrow(
      /Couldn't read this image's size/,
    );
    await expect(checkImageFile(file(png(0, 400), "image/png"))).rejects.toThrow(
      /Couldn't read this image's size/,
    );
  });

  it("refuses formats whose size it can't read, and non-images", async () => {
    await expect(checkImageFile(file(bytes("<svg/>"), "image/svg+xml"))).rejects.toThrow(
      /PNG, JPEG, WebP or GIF/,
    );
    await expect(checkImageFile(file(bytes("hi"), "text/plain"))).rejects.toThrow(/image file/);
  });
});

describe("shrinkImageFile", () => {
  afterEach(() => vi.restoreAllMocks());

  it("paints the terminal's ground under the image, so transparency never turns black", async () => {
    const calls: string[] = [];
    const ctx = {
      set fillStyle(v: string) {
        calls.push(`fill:${v}`);
      },
      fillRect: () => calls.push("fillRect"),
      drawImage: () => calls.push("draw"),
    };
    vi.stubGlobal(
      "createImageBitmap",
      vi.fn(async () => ({ width: 3200, height: 1600, close: () => calls.push("close") })),
    );
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(
      ctx as unknown as CanvasRenderingContext2D,
    );
    // An engine without WebP encoding (Safari): JPEG, which has no alpha.
    vi.spyOn(HTMLCanvasElement.prototype, "toDataURL").mockImplementation(function (
      this: HTMLCanvasElement,
      type?: string,
    ) {
      calls.push(`encode:${type}:${this.width}x${this.height}`);
      return type === "image/jpeg" ? "data:image/jpeg;base64,AAAA" : "data:image/png;base64,AAAA";
    });
    const out = await shrinkImageFile(file(png(3200, 1600), "image/png"), "#1a1f2b");
    expect(out).toBe("data:image/jpeg;base64,AAAA");
    expect(calls.slice(0, 3)).toEqual(["fill:#1a1f2b", "fillRect", "draw"]);
    expect(calls).toContain("encode:image/jpeg:1600x800");
    expect(calls).toContain("close");
    vi.unstubAllGlobals();
  });
});
