// SPDX-License-Identifier: MIT
/**
 * Worker wire framing: every message is a 4-byte big-endian payload length
 * followed by that many payload bytes. The Rust side (`codesense-pty-core`
 * `emulator::wire`) uses the same layout, so the two must change together.
 */

/** Largest payload either side will accept. A bigger length means the stream is corrupt. */
export const MAX_FRAME_BYTES = 64 * 1024 * 1024;

const HEADER_BYTES = 4;

export function encodeFrame(payload: Uint8Array): Uint8Array {
  if (payload.byteLength > MAX_FRAME_BYTES) {
    throw new Error(`frame of ${payload.byteLength} bytes exceeds ${MAX_FRAME_BYTES}`);
  }
  const frame = new Uint8Array(HEADER_BYTES + payload.byteLength);
  new DataView(frame.buffer).setUint32(0, payload.byteLength, false);
  frame.set(payload, HEADER_BYTES);
  return frame;
}

/**
 * Reassembles frames from arbitrarily split stdin chunks. A pipe read can end
 * mid-header or mid-payload, so nothing is decoded until the whole frame is here.
 */
export class FrameReader {
  private buffered: Uint8Array = new Uint8Array(0);

  push(chunk: Uint8Array): Uint8Array[] {
    this.buffered = concat(this.buffered, chunk);
    const frames: Uint8Array[] = [];
    let offset = 0;
    while (this.buffered.byteLength - offset >= HEADER_BYTES) {
      const view = new DataView(this.buffered.buffer, this.buffered.byteOffset + offset);
      const length = view.getUint32(0, false);
      if (length > MAX_FRAME_BYTES) {
        throw new Error(`frame length ${length} exceeds ${MAX_FRAME_BYTES}`);
      }
      const end = offset + HEADER_BYTES + length;
      if (this.buffered.byteLength < end) break;
      frames.push(this.buffered.slice(offset + HEADER_BYTES, end));
      offset = end;
    }
    this.buffered = this.buffered.slice(offset);
    return frames;
  }

  /** Bytes of an incomplete frame still waiting for the rest of it. */
  get pendingBytes(): number {
    return this.buffered.byteLength;
  }
}

function concat(a: Uint8Array, b: Uint8Array): Uint8Array {
  if (a.byteLength === 0) return b;
  const out = new Uint8Array(a.byteLength + b.byteLength);
  out.set(a, 0);
  out.set(b, a.byteLength);
  return out;
}
