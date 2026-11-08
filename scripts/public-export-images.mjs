// SPDX-License-Identifier: MIT
// Image metadata for the public export: strips it (exporter) and finds leftovers (scanner).
// Keep-lists, never denylists: anything not listed is metadata and is stripped.
//   PNG: IHDR PLTE IDAT IEND, tRNS gAMA cHRM sRGB iCCP sBIT pHYs bKGD, APNG acTL fcTL fdAT. CRCs are checked.
//   WebP: VP8 VP8L VP8X ALPH ANIM ANMF ICCP (ANMF frames keep ALPH/VP8/VP8L only).
//   JPEG: the coding segments, APP0 JFIF without a thumbnail, APP2 ICC_PROFILE, APP14 Adobe.
//   GIF: images, graphic control and the NETSCAPE2.0/ANIMEXTS1.0 loop extension.
//   ICO/CUR: PNG images (checked as PNG) and BITMAPINFOHEADER DIBs. ICNS: icon elements (PNG ones checked as PNG), TOC, icnV.
// Bytes after the end of an image are metadata too. Pure JS. An image that cannot be parsed throws, so callers fail closed.
import zlib from "node:zlib";

const PNG_SIG = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const PNG_KEEP = new Set([
  "IHDR",
  "PLTE",
  "IDAT",
  "IEND",
  "tRNS",
  "gAMA",
  "cHRM",
  "sRGB",
  "iCCP",
  "sBIT",
  "pHYs",
  "bKGD",
  "acTL",
  "fcTL",
  "fdAT",
]);
const WEBP_KEEP = new Set(["VP8 ", "VP8L", "VP8X", "ALPH", "ANIM", "ANMF", "ICCP"]);
const WEBP_FRAME_KEEP = new Set(["ALPH", "VP8 ", "VP8L"]);
const JPEG_APP_KEEP = new Map([
  [0xe0, "JFIF\0"],
  [0xe2, "ICC_PROFILE\0"],
  [0xee, "Adobe"],
]);
// SOF0-3/5-7/9-11/13-15, DHT, DAC, DQT, DNL, DRI
const JPEG_CODING = new Set([
  0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf, 0xc4, 0xcc, 0xdb,
  0xdc, 0xdd,
]);
const GIF_APP_KEEP = new Set(["NETSCAPE2.0", "ANIMEXTS1.0"]);
const ICNS_KEEP = new Set([
  "ICON",
  "ICN#",
  "icm#",
  "icm4",
  "icm8",
  "ics#",
  "ics4",
  "ics8",
  "is32",
  "s8mk",
  "icl4",
  "icl8",
  "il32",
  "l8mk",
  "ich#",
  "ich4",
  "ich8",
  "ih32",
  "h8mk",
  "it32",
  "t8mk",
  "icp4",
  "icp5",
  "icp6",
  "ic04",
  "ic05",
  "ic07",
  "ic08",
  "ic09",
  "ic10",
  "ic11",
  "ic12",
  "ic13",
  "ic14",
  "icsb",
  "icsB",
  "sb24",
  "SB24",
  "TOC ",
  "icnV",
]);
const JP2_SIG = Buffer.from([0, 0, 0, 0x0c, 0x6a, 0x50, 0x20, 0x20, 0x0d, 0x0a, 0x87, 0x0a]);

export function imageKind(buf) {
  if (buf.length >= 8 && buf.subarray(0, 8).equals(PNG_SIG)) return "png";
  if (buf.length >= 3 && buf[0] === 0xff && buf[1] === 0xd8 && buf[2] === 0xff) return "jpeg";
  if (
    buf.length >= 12 &&
    buf.toString("latin1", 0, 4) === "RIFF" &&
    buf.toString("latin1", 8, 12) === "WEBP"
  )
    return "webp";
  if (buf.length >= 6 && /^GIF8[79]a$/.test(buf.toString("latin1", 0, 6))) return "gif";
  if (buf.length >= 8 && buf.toString("latin1", 0, 4) === "icns") return "icns";
  if (
    buf.length >= 22 &&
    buf.readUInt16LE(0) === 0 &&
    [1, 2].includes(buf.readUInt16LE(2)) &&
    buf.readUInt16LE(4) > 0 &&
    buf[9] === 0
  )
    return "ico";
  return null;
}

export const IMAGE_EXT = /\.(png|apng|jpe?g|jfif|webp|gif|ico|cur|icns)$/i;

class Unparsable extends Error {}

function need(ok, msg) {
  if (!ok) throw new Unparsable(msg);
}

// Each parser returns { meta: [{ what, offset }], parts: [Buffer] }; parts concatenated are the image without its metadata.

function png(buf, base = 0) {
  need(buf.length >= 8 && buf.subarray(0, 8).equals(PNG_SIG), "png: bad signature");
  const meta = [];
  const parts = [buf.subarray(0, 8)];
  let o = 8;
  for (let first = true; ; first = false) {
    need(o + 12 <= buf.length, "png: truncated chunk");
    const len = buf.readUInt32BE(o);
    const type = buf.toString("latin1", o + 4, o + 8);
    const end = o + 12 + len;
    need(len <= 0x7fffffff && end <= buf.length, `png: chunk ${type} overruns the file`);
    need(
      zlib.crc32(buf.subarray(o + 4, end - 4)) === buf.readUInt32BE(end - 4),
      `png: bad CRC in chunk ${type}`,
    );
    need(!first || type === "IHDR", "png: first chunk is not IHDR");
    if (PNG_KEEP.has(type)) parts.push(buf.subarray(o, end));
    else meta.push({ what: `png ${type}`, offset: base + o });
    o = end;
    if (type === "IEND") break;
  }
  if (o < buf.length) meta.push({ what: "png bytes after IEND", offset: base + o });
  return { meta, parts };
}

function jpegAppKept(buf, marker, start, end) {
  const ident = JPEG_APP_KEEP.get(marker);
  if (!ident || buf.toString("latin1", start + 4, start + 4 + ident.length) !== ident) return false;
  // a JFIF thumbnail is a second picture: drop the segment (APP0 is optional)
  return marker !== 0xe0 || (start + 18 <= end && buf[start + 16] === 0 && buf[start + 17] === 0);
}

function jpeg(buf) {
  need(buf[0] === 0xff && buf[1] === 0xd8, "jpeg: no SOI");
  const meta = [];
  const parts = [buf.subarray(0, 2)];
  let o = 2;
  for (;;) {
    need(o < buf.length && buf[o] === 0xff, "jpeg: expected a marker");
    while (buf[o] === 0xff) o++;
    need(o < buf.length, "jpeg: truncated marker");
    const marker = buf[o];
    const start = o - 1;
    o++;
    if (marker === 0xd9) {
      parts.push(buf.subarray(start, o));
      break;
    }
    if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) {
      parts.push(buf.subarray(start, o));
      continue;
    }
    need(o + 2 <= buf.length, "jpeg: truncated segment");
    const len = buf.readUInt16BE(o);
    need(
      len >= 2 && o + len <= buf.length,
      `jpeg: segment ${marker.toString(16)} overruns the file`,
    );
    o += len;
    if (marker === 0xda) {
      // entropy-coded data runs to the next marker that is not a stuffed 0x00 or a restart marker
      while (
        o + 1 < buf.length &&
        !(buf[o] === 0xff && buf[o + 1] !== 0x00 && !(buf[o + 1] >= 0xd0 && buf[o + 1] <= 0xd7))
      )
        o++;
      need(o + 1 < buf.length, "jpeg: no EOI");
      parts.push(buf.subarray(start, o));
    } else if ((marker >= 0xe0 && marker <= 0xef) || marker === 0xfe) {
      if (jpegAppKept(buf, marker, start, o)) parts.push(buf.subarray(start, o));
      else
        meta.push({
          what: `jpeg ${marker === 0xfe ? "COM" : "APP" + (marker - 0xe0)}`,
          offset: start,
        });
    } else {
      // an unknown coding segment cannot be dropped without breaking the picture
      need(JPEG_CODING.has(marker), `jpeg: unsupported marker ${marker.toString(16)}`);
      parts.push(buf.subarray(start, o));
    }
  }
  if (o < buf.length) meta.push({ what: "jpeg bytes after EOI", offset: o });
  return { meta, parts };
}

function riffChunks(buf, from, to, where) {
  const out = [];
  let o = from;
  while (o < to) {
    need(o + 8 <= to, `${where}: truncated chunk`);
    const type = buf.toString("latin1", o, o + 4);
    const len = buf.readUInt32LE(o + 4);
    const end = o + 8 + len + (len & 1);
    need(end <= to, `${where}: chunk ${type} overruns`);
    out.push({ type, o, end });
    o = end;
  }
  return out;
}

function riffChunk(type, data) {
  const h = Buffer.alloc(8);
  h.write(type, 0, "latin1");
  h.writeUInt32LE(data.length, 4);
  return Buffer.concat([h, data, data.length & 1 ? Buffer.alloc(1) : Buffer.alloc(0)]);
}

function webp(buf) {
  need(buf.readUInt32LE(4) + 8 === buf.length, "webp: RIFF size does not match the file");
  const meta = [];
  const body = [];
  let flagsAt = -1;
  for (const c of riffChunks(buf, 12, buf.length, "webp")) {
    if (!WEBP_KEEP.has(c.type)) {
      meta.push({ what: `webp ${c.type.trim()}`, offset: c.o });
      continue;
    }
    if (c.type === "ANMF") {
      need(c.end - c.o >= 24, "webp: short ANMF");
      const len = buf.readUInt32LE(c.o + 4);
      const frame = [buf.subarray(c.o + 8, c.o + 24)];
      let dropped = false;
      for (const s of riffChunks(buf, c.o + 24, c.o + 8 + len, "webp ANMF")) {
        if (WEBP_FRAME_KEEP.has(s.type)) frame.push(buf.subarray(s.o, s.end));
        else {
          meta.push({ what: `webp ANMF ${s.type.trim()}`, offset: s.o });
          dropped = true;
        }
      }
      body.push(dropped ? riffChunk("ANMF", Buffer.concat(frame)) : buf.subarray(c.o, c.end));
      continue;
    }
    if (c.type === "VP8X") {
      need(c.end - c.o >= 18, "webp: short VP8X");
      flagsAt = 12 + body.reduce((n, b) => n + b.length, 0) + 8;
    }
    body.push(buf.subarray(c.o, c.end));
  }
  if (!meta.length) return { meta, parts: [buf] };
  const out = Buffer.concat([Buffer.from(buf.subarray(0, 12)), ...body]);
  out.writeUInt32LE(out.length - 8, 4);
  if (flagsAt >= 0) out[flagsAt] &= ~0x0c; // VP8X: clear the EXIF (0x08) and XMP (0x04) flags
  return { meta, parts: [out] };
}

function gifSubBlocks(buf, o) {
  for (;;) {
    need(o < buf.length, "gif: truncated sub-blocks");
    const n = buf[o++];
    if (n === 0) return o;
    o += n;
  }
}

function gif(buf) {
  need(buf.length >= 13, "gif: truncated header");
  const meta = [];
  let o = 13;
  if (buf[10] & 0x80) o += 3 << ((buf[10] & 7) + 1);
  need(o <= buf.length, "gif: truncated colour table");
  const parts = [buf.subarray(0, o)];
  for (;;) {
    need(o < buf.length, "gif: no trailer");
    const start = o;
    const b = buf[o];
    if (b === 0x3b) {
      o++;
      parts.push(buf.subarray(start, o));
      break;
    }
    if (b === 0x2c) {
      need(o + 11 <= buf.length, "gif: truncated image descriptor");
      const packed = buf[o + 9];
      o += 10;
      if (packed & 0x80) o += 3 << ((packed & 7) + 1);
      o = gifSubBlocks(buf, o + 1); // LZW minimum code size, then the image data
      parts.push(buf.subarray(start, o));
      continue;
    }
    need(b === 0x21 && o + 2 <= buf.length, `gif: unexpected block ${b.toString(16)}`);
    const label = buf[o + 1];
    o = gifSubBlocks(buf, o + 2);
    const app =
      label === 0xff && buf[start + 2] === 11
        ? buf.toString("latin1", start + 3, start + 14)
        : null;
    if (label === 0xf9 || GIF_APP_KEEP.has(app)) parts.push(buf.subarray(start, o));
    else
      meta.push({
        what: `gif extension ${label === 0xfe ? "comment" : label === 0x01 ? "plain text" : app ? `application ${app}` : label.toString(16)}`,
        offset: start,
      });
  }
  if (o < buf.length) meta.push({ what: "gif bytes after the trailer", offset: o });
  return { meta, parts };
}

function ico(buf) {
  const count = buf.readUInt16LE(4);
  const head = 6 + 16 * count;
  need(head <= buf.length, "ico: truncated directory");
  const meta = [];
  const images = [];
  for (let i = 0; i < count; i++) {
    const e = 6 + 16 * i;
    const size = buf.readUInt32LE(e + 8);
    const off = buf.readUInt32LE(e + 12);
    need(
      off >= head && off + size <= buf.length && size >= 40,
      `ico: image ${i} overruns the file`,
    );
    const data = buf.subarray(off, off + size);
    let kept;
    if (data.subarray(0, 8).equals(PNG_SIG)) {
      const p = png(data, off);
      meta.push(...p.meta);
      kept = Buffer.concat(p.parts);
    } else {
      need(
        data.readUInt32LE(0) === 40,
        `ico: image ${i} is neither PNG nor a BITMAPINFOHEADER DIB`,
      );
      kept = data;
    }
    images.push({ e, off, size, kept });
  }
  const sorted = [...images].sort((a, b) => a.off - b.off);
  let at = head;
  for (const im of sorted) {
    need(im.off >= at, "ico: images overlap");
    if (im.off > at) meta.push({ what: "ico bytes outside the images", offset: at });
    at = im.off + im.size;
  }
  if (at < buf.length) meta.push({ what: "ico bytes outside the images", offset: at });
  if (!meta.length) return { meta, parts: [buf] };
  const dir = Buffer.from(buf.subarray(0, head));
  const parts = [dir];
  let pos = head;
  for (const im of images) {
    dir.writeUInt32LE(im.kept.length, im.e + 8);
    dir.writeUInt32LE(pos, im.e + 12);
    parts.push(im.kept);
    pos += im.kept.length;
  }
  return { meta, parts };
}

function icnsElement(type, data) {
  const h = Buffer.alloc(8);
  h.write(type, 0, "latin1");
  h.writeUInt32BE(data.length + 8, 4);
  return Buffer.concat([h, data]);
}

function icns(buf) {
  need(buf.readUInt32BE(4) === buf.length, "icns: length does not match the file");
  const meta = [];
  const kept = [];
  let changed = false;
  let toc = false;
  for (let o = 8; o < buf.length; ) {
    need(o + 8 <= buf.length, "icns: truncated element");
    const type = buf.toString("latin1", o, o + 4);
    const len = buf.readUInt32BE(o + 4);
    need(
      len >= 8 && o + len <= buf.length,
      `icns: element ${JSON.stringify(type)} overruns the file`,
    );
    const data = buf.subarray(o + 8, o + len);
    if (!ICNS_KEEP.has(type)) {
      meta.push({ what: `icns element ${JSON.stringify(type)}`, offset: o });
      changed = true;
    } else if (type === "TOC ") toc = true;
    else if (data.subarray(0, 8).equals(PNG_SIG)) {
      const p = png(data, o + 8);
      meta.push(...p.meta);
      changed ||= p.meta.length > 0;
      kept.push({ type, data: Buffer.concat(p.parts) });
    } else {
      need(
        !data.subarray(0, 12).equals(JP2_SIG),
        `icns: JPEG 2000 element ${type} is not supported`,
      );
      kept.push({ type, data });
    }
    o += len;
  }
  if (!changed) return { meta, parts: [buf] };
  const els = kept.map((k) => icnsElement(k.type, k.data));
  if (toc) {
    const t = Buffer.alloc(8 * kept.length);
    kept.forEach((k, i) => {
      t.write(k.type, 8 * i, "latin1");
      t.writeUInt32BE(k.data.length + 8, 8 * i + 4);
    });
    els.unshift(icnsElement("TOC ", t));
  }
  const head = Buffer.from("icns\0\0\0\0", "latin1");
  head.writeUInt32BE(8 + els.reduce((n, b) => n + b.length, 0), 4);
  return { meta, parts: [head, ...els] };
}

const PARSERS = { png, jpeg, webp, gif, ico, icns };

function parse(buf) {
  const kind = imageKind(buf);
  try {
    if (!kind) throw new Unparsable("not a PNG, JPEG, WebP, GIF, ICO or ICNS image");
    return PARSERS[kind](buf);
  } catch (e) {
    if (e instanceof Unparsable || e instanceof RangeError)
      throw new Error(`unparsable image: ${e.message}`);
    throw e;
  }
}

// Returns the metadata still present: [{ what, offset }]. Throws when the image cannot be parsed.
export function findImageMetadata(buf) {
  return parse(buf).meta;
}

// Returns the image with its metadata removed (the same buffer when there is none). Throws when
// unparsable, and when the stripped image still has metadata.
export function stripImageMetadata(buf) {
  const { meta, parts } = parse(buf);
  if (!meta.length) return buf;
  const out = Buffer.concat(parts);
  const left = findImageMetadata(out);
  if (left.length)
    throw new Error(`image metadata left after stripping: ${left.map((x) => x.what).join(", ")}`);
  return out;
}
