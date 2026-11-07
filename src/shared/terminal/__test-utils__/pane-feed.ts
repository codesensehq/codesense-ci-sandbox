// SPDX-License-Identifier: MIT
/**
 * Turn a recording into the steps a live pane sees: output as the strings the
 * stream events carry (UTF-8 decoded across deliveries), and resizes in place.
 */
import type { PtylogRecord } from "../__fixtures__/ptylog";

export type PaneStep =
  | { kind: "output"; data: string }
  | { kind: "resize"; cols: number; rows: number };

export function paneSteps(records: readonly PtylogRecord[]): PaneStep[] {
  const decoder = new TextDecoder();
  return records.map((record) =>
    record.kind === "output"
      ? { kind: "output", data: decoder.decode(record.data, { stream: true }) }
      : { kind: "resize", cols: record.cols, rows: record.rows },
  );
}

/** Split `steps` into `parts` runs of about equal length, in order. */
export function chunked<T>(steps: readonly T[], parts: number): T[][] {
  const size = Math.ceil(steps.length / parts);
  const chunks: T[][] = [];
  for (let i = 0; i < steps.length; i += size) chunks.push(steps.slice(i, i + size));
  return chunks;
}
