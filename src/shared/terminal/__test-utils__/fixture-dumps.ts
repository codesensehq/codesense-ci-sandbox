// SPDX-License-Identifier: MIT
/**
 * The checked-in dumps (`tests/fixtures/terminal/<name>.dump.json`) the PTY
 * layer's emulator tests compare the backend worker against, so the Rust side
 * needs no JS reference at test time. `pnpm run fixtures:dump` writes them; a
 * vitest check fails when one is stale.
 */
import { resolve } from "node:path";
import { type ReferenceBuffer } from "../buffer-dump";
import { type PtylogFixture, type PtylogRecord } from "../__fixtures__/ptylog";
import { headlessFrom } from "./headless-from";

/** Recordings the backend replays through a fake PTY. */
export const DUMPED_RECORDINGS: PtylogFixture[] = ["codex-tui", "claude-ink", "tui-fixture"];

/** An invalid byte mid-line, as the display receives it: the host delivers
 *  U+FFFD in its place. The PTY-side test writes `\xff` where this has U+FFFD. */
export const INVALID_UTF8_DELIVERED = "before � after\r\nnext line";

/** Every dump, by file name (without `.dump.json`). */
export async function fixtureDumps(): Promise<Map<string, ReferenceBuffer>> {
  const dumps = new Map<string, ReferenceBuffer>();
  for (const name of DUMPED_RECORDINGS) dumps.set(name, await headlessFrom(name));
  const delivered = new TextEncoder().encode(INVALID_UTF8_DELIVERED);
  const invalid: PtylogRecord[] = [
    { kind: "output", seqEnd: delivered.byteLength, data: delivered },
  ];
  dumps.set("invalid-utf8", await headlessFrom(invalid));
  return dumps;
}

export function dumpPath(name: string): string {
  return resolve(__dirname, "../../../../tests/fixtures/terminal", `${name}.dump.json`);
}

/** A dump's file content: stable key order, one trailing newline. */
export function serializeDump(dump: ReferenceBuffer): string {
  return `${JSON.stringify(dump)}\n`;
}
