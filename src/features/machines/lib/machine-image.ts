// SPDX-License-Identifier: MIT
/**
 * Preparing a picked file as a Machine's terminal background: its size read
 * from the file's own header (so a "decompression bomb" — a tiny file that
 * claims a gigantic canvas — is refused before anything decodes it), then
 * shrunk and re-encoded into a `data:` URL small enough to keep.
 */

/** Longest edge a background image is shrunk to before it is saved. */
export const MAX_IMAGE_EDGE = 1600;
/** Largest saved image, as the length of its `data:` URL (≈ 1.1 MB of image). */
export const MAX_IMAGE_DATA_URL_CHARS = 1_500_000;
/** Largest file the picker accepts before shrinking. */
export const MAX_SOURCE_BYTES = 25 * 1024 * 1024;
/** Most pixels a picked image may claim (40 MP: a 7,700 × 5,200 photo). */
export const MAX_SOURCE_PIXELS = 40_000_000;
/** The formats whose header size we can read — and so the ones accepted. */
export const ACCEPTED_IMAGE_TYPES = "image/png,image/jpeg,image/webp,image/gif";
/** How much of a file the size is looked for in (a JPEG's EXIF can be large). */
const HEADER_SCAN_BYTES = 512 * 1024;

export interface ImageSize {
  width: number;
  height: number;
}

const ascii = (b: Uint8Array, at: number, text: string) =>
  b.length >= at + text.length && [...text].every((c, i) => b[at + i] === c.charCodeAt(0));
const u16be = (b: Uint8Array, at: number) => (b[at] << 8) | b[at + 1];
const u16le = (b: Uint8Array, at: number) => b[at] | (b[at + 1] << 8);
const u24le = (b: Uint8Array, at: number) => b[at] | (b[at + 1] << 8) | (b[at + 2] << 16);
const u32be = (b: Uint8Array, at: number) =>
  ((b[at] << 24) >>> 0) + (b[at + 1] << 16) + (b[at + 2] << 8) + b[at + 3];

/** JPEG start-of-frame markers (every SOFn but DHT C4, JPG C8, DAC CC). */
const JPEG_SOF = new Set([
  0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf,
]);

/** The pixel size a PNG, JPEG, WebP or GIF header declares; `null` when the
 *  bytes are none of those or the size isn't within them. */
export function imageSizeFromHeader(b: Uint8Array): ImageSize | null {
  // PNG: signature, then IHDR (width, height as big-endian u32).
  if (b.length >= 24 && b[0] === 0x89 && ascii(b, 1, "PNG") && ascii(b, 12, "IHDR"))
    return { width: u32be(b, 16), height: u32be(b, 20) };
  // GIF: logical screen size (little-endian u16).
  if (ascii(b, 0, "GIF8") && b.length >= 10) return { width: u16le(b, 6), height: u16le(b, 8) };
  // WebP: RIFF container, first chunk is VP8 / VP8L / VP8X.
  if (ascii(b, 0, "RIFF") && ascii(b, 8, "WEBP")) {
    if (ascii(b, 12, "VP8 ") && b.length >= 30)
      return { width: u16le(b, 26) & 0x3fff, height: u16le(b, 28) & 0x3fff };
    if (ascii(b, 12, "VP8L") && b.length >= 25) {
      const [b0, b1, b2, b3] = [b[21], b[22], b[23], b[24]];
      return {
        width: 1 + (((b1 & 0x3f) << 8) | b0),
        height: 1 + (((b3 & 0x0f) << 10) | (b2 << 2) | ((b1 & 0xc0) >> 6)),
      };
    }
    if (ascii(b, 12, "VP8X") && b.length >= 30)
      return { width: 1 + u24le(b, 24), height: 1 + u24le(b, 27) };
    return null;
  }
  // JPEG: walk the marker segments to the first start-of-frame.
  if (b[0] === 0xff && b[1] === 0xd8) {
    let i = 2;
    while (i + 9 < b.length) {
      if (b[i] !== 0xff) {
        i++;
        continue;
      }
      const marker = b[i + 1];
      if (JPEG_SOF.has(marker)) return { width: u16be(b, i + 7), height: u16be(b, i + 5) };
      // Standalone markers carry no length.
      if (marker === 0xff || marker === 0x01 || (marker >= 0xd0 && marker <= 0xd8)) {
        i += marker === 0xff ? 1 : 2;
        continue;
      }
      i += 2 + u16be(b, i + 2);
    }
  }
  return null;
}

/** Whether `b` starts like a PNG, JPEG, WebP or GIF — whatever its size says. */
export function hasImageSignature(b: Uint8Array): boolean {
  return (
    (b[0] === 0x89 && ascii(b, 1, "PNG")) ||
    (b[0] === 0xff && b[1] === 0xd8) ||
    (ascii(b, 0, "RIFF") && ascii(b, 8, "WEBP")) ||
    ascii(b, 0, "GIF8")
  );
}

/** Throws the sentence a user can act on when `file` can't be a background:
 *  not an image we read, too big a file, or too many pixels to decode. */
export async function checkImageFile(file: Blob): Promise<ImageSize> {
  if (!file.type.startsWith("image/")) throw new Error("Choose an image file (PNG, JPEG, WebP…)");
  if (file.size > MAX_SOURCE_BYTES)
    throw new Error("That image is over 25 MB — choose a smaller one");
  const head = new Uint8Array(await file.slice(0, HEADER_SCAN_BYTES).arrayBuffer());
  const size = imageSizeFromHeader(head);
  if (!size || size.width < 1 || size.height < 1)
    throw new Error(
      hasImageSignature(head)
        ? // A JPEG whose frame header sits past the scanned start (a huge
          // embedded preview), or a header declaring no pixels.
          "Couldn't read this image's size — open it in an image editor and save it again as PNG or JPEG"
        : "Choose a PNG, JPEG, WebP or GIF image",
    );
  if (size.width * size.height > MAX_SOURCE_PIXELS)
    throw new Error(
      `That image is ${size.width} × ${size.height} — too many pixels. Choose one under 40 megapixels`,
    );
  return size;
}

/**
 * `file`, shrunk so its longest edge is at most {@link MAX_IMAGE_EDGE}px and
 * re-encoded (WebP where the engine can, else JPEG) over `ground` — the
 * terminal's background — so a transparent image keeps the terminal's color
 * instead of turning black in a format without transparency.
 */
export async function shrinkImageFile(file: File, ground: string): Promise<string> {
  await checkImageFile(file);
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(file);
  } catch {
    throw new Error("That image couldn't be read — try a PNG or JPEG");
  }
  const scale = Math.min(1, MAX_IMAGE_EDGE / Math.max(bitmap.width, bitmap.height));
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(bitmap.width * scale));
  canvas.height = Math.max(1, Math.round(bitmap.height * scale));
  const ctx = canvas.getContext("2d");
  if (!ctx) {
    bitmap.close();
    throw new Error("That image couldn't be prepared");
  }
  ctx.fillStyle = ground;
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
  bitmap.close();
  let dataUrl = canvas.toDataURL("image/webp", 0.8);
  if (!dataUrl.startsWith("data:image/webp")) dataUrl = canvas.toDataURL("image/jpeg", 0.82);
  if (dataUrl.length > MAX_IMAGE_DATA_URL_CHARS)
    throw new Error("That image is still too large after shrinking — choose a simpler one");
  return dataUrl;
}
