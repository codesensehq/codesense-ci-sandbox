// SPDX-License-Identifier: MIT
/**
 * The reference side of every parity test: a fresh `@xterm/headless` terminal
 * fed a recording's output and resizes in stream order, dumped through the same
 * `referenceBuffer` the display and the backend emulator use. No React, no DOM.
 */
import { Terminal } from "@xterm/headless";
import { referenceBuffer, type ReferenceBuffer } from "../buffer-dump";
import { activateTerminalParser, TERMINAL_PARSER_OPTIONS } from "../terminal-parser-options";
import {
  loadPtylog,
  replayPtylog,
  type PtylogFixture,
  type PtylogRecord,
} from "../__fixtures__/ptylog";

/** The size every checked-in recording started at (tests/fixtures/terminal/README.md). */
export const PTYLOG_START = { cols: 100, rows: 30 } as const;

/** Terminal options both sides of a parity check are built with. */
export const PARITY_TERMINAL_OPTIONS = {
  ...PTYLOG_START,
  ...TERMINAL_PARSER_OPTIONS,
} as const;

/** A fresh headless terminal parsing exactly as the display does. */
export function parityTerminal(options: { cols?: number; rows?: number } = {}): Terminal {
  const term = new Terminal({ ...PARITY_TERMINAL_OPTIONS, ...options });
  activateTerminalParser(term);
  return term;
}

/** Write `data` and resolve once xterm has parsed it (its write is async). */
export function writeParsed(
  term: { write(data: string | Uint8Array, cb?: () => void): void },
  data: string | Uint8Array,
): Promise<void> {
  return new Promise((resolve) => term.write(data, resolve));
}

/** A fresh headless terminal fed `records` (or a checked-in fixture), dumped. */
export async function headlessFrom(
  recording: PtylogFixture | readonly PtylogRecord[],
): Promise<ReferenceBuffer> {
  const records = typeof recording === "string" ? loadPtylog(recording) : recording;
  const term = parityTerminal();
  try {
    await replayPtylog(records, {
      write: (data) => writeParsed(term, data),
      resize: (cols, rows) => term.resize(cols, rows),
    });
    return referenceBuffer(term);
  } finally {
    term.dispose();
  }
}
