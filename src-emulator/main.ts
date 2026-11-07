// SPDX-License-Identifier: MIT
import { EmulatorHost } from "./emulator";
import { encodeFrame, FrameReader } from "./frame";

// stdout carries frames only; anything the worker wants a human to read goes to
// stderr, which the supervisor forwards to its log.
const reader = new FrameReader();
const encoder = new TextEncoder();
const host = new EmulatorHost(
  (reply) => {
    process.stdout.write(encodeFrame(encoder.encode(JSON.stringify(reply))));
  },
  // Budget tests read the worker's memory after a forced collection; the
  // shipped host never asks.
  { stats: process.argv.includes("--test-mode") },
);

process.stdin.on("data", (chunk: Buffer) => {
  let frames: Uint8Array[];
  try {
    frames = reader.push(chunk);
  } catch (error) {
    process.stderr.write(`emulator worker: ${String(error)}\n`);
    process.exit(2);
  }
  for (const frame of frames) host.handleFrame(frame);
});

// The supervisor closing our stdin is the shutdown signal. Replies still
// buffered for the pipe are flushed first.
process.stdin.on("end", () => process.stdout.end(() => process.exit(0)));
