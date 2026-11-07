/**
 * The `.ptylog` byte-fixture loader: a recorded terminal stream, in stream
 * order, as the dev recorder in the PTY layer writes it
 * (`CODESENSE_PTY_RECORD=<dir>`, debug builds only).
 *
 *   O \t <seq_end> \t <base64 delivered slice>   — output ending at byte `seq_end`
 *   R \t <at_seq>  \t <cols> \t <rows>           — a PTY resize applied at byte `at_seq`
 *
 * DOM-free and framework-free, so the display parity tests and the backend
 * emulator tests replay the same corpus the same way.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

export interface PtylogOutput {
  kind: "output";
  /** The stream's absolute byte offset at the end of this delivery. */
  seqEnd: number;
  data: Uint8Array;
}

export interface PtylogResize {
  kind: "resize";
  /** The stream's absolute byte offset when the resize was applied. */
  atSeq: number;
  cols: number;
  rows: number;
}

export type PtylogRecord = PtylogOutput | PtylogResize;

/**
 * The checked-in recordings under `tests/fixtures/terminal/`. `tui-fixture` is
 * generated, not recorded: the seeded cursor-addressed TUI in the PTY layer.
 */
export type PtylogFixture = "codex-tui" | "claude-ink" | "append-only" | "tui-fixture";

function decodeBase64(text: string): Uint8Array {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

function integer(field: string | undefined, what: string, lineNo: number): number {
  if (field === undefined || !/^\d+$/.test(field)) {
    throw new Error(`ptylog line ${lineNo}: ${what} is not an unsigned integer: ${field}`);
  }
  return Number(field);
}

/** Parse a recording. Throws on any malformed line, naming it. */
export function parsePtylog(text: string): PtylogRecord[] {
  const records: PtylogRecord[] = [];
  const lines = text.split("\n");
  if (lines[lines.length - 1] === "") lines.pop();
  lines.forEach((line, index) => {
    const lineNo = index + 1;
    const fields = line.split("\t");
    if (fields[0] === "O" && fields.length === 3) {
      records.push({
        kind: "output",
        seqEnd: integer(fields[1], "seq_end", lineNo),
        data: decodeBase64(fields[2]),
      });
    } else if (fields[0] === "R" && fields.length === 4) {
      records.push({
        kind: "resize",
        atSeq: integer(fields[1], "at_seq", lineNo),
        cols: integer(fields[2], "cols", lineNo),
        rows: integer(fields[3], "rows", lineNo),
      });
    } else {
      throw new Error(`ptylog line ${lineNo}: not an O or R record`);
    }
  });
  return records;
}

/** The checked-in recording's path. */
export function ptylogPath(name: PtylogFixture): string {
  return resolve(__dirname, "../../../../tests/fixtures/terminal", `${name}.ptylog`);
}

/** Read and parse one of the checked-in recordings. */
export function loadPtylog(name: PtylogFixture): PtylogRecord[] {
  return parsePtylog(readFileSync(ptylogPath(name), "utf8"));
}

/** Where a replay sends the stream: a display, an emulator, or a reference. */
export interface PtylogTarget {
  write(data: Uint8Array): void | Promise<void>;
  resize(cols: number, rows: number): void | Promise<void>;
}

/**
 * Replay a recording into `target`: every output and every resize, in stream
 * order, each resize at its recorded byte offset. The byte offset is checked as
 * the replay goes, so a recording whose resize does not sit where the stream
 * stands fails loudly instead of replaying at the wrong point.
 */
export async function replayPtylog(
  records: readonly PtylogRecord[],
  target: PtylogTarget,
): Promise<void> {
  let applied = 0;
  for (const record of records) {
    if (record.kind === "output") {
      await target.write(record.data);
      applied = record.seqEnd;
    } else {
      if (record.atSeq < applied) {
        throw new Error(`resize at byte ${record.atSeq} recorded after byte ${applied}`);
      }
      await target.resize(record.cols, record.rows);
    }
  }
}
