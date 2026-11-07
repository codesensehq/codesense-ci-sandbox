import { statSync } from "node:fs";
import { Terminal } from "@xterm/headless";
import { describe, expect, it } from "vitest";
import {
  loadPtylog,
  parsePtylog,
  ptylogPath,
  replayPtylog,
  type PtylogFixture,
  type PtylogRecord,
} from "./ptylog";

const FIXTURES: PtylogFixture[] = ["codex-tui", "claude-ink", "append-only", "tui-fixture"];

/** The pattern `ESC [ <body>`: a control sequence. */
function csi(body: string): RegExp {
  return new RegExp(`\u001b\\[${body}`);
}

/** Every resize whose offset runs backwards or past the output after it. */
function resizeOrderViolations(records: readonly PtylogRecord[]): string[] {
  const violations: string[] = [];
  let lastAt = -1;
  records.forEach((record, index) => {
    if (record.kind !== "resize") return;
    if (record.atSeq < lastAt) violations.push(`R#${index} at ${record.atSeq} < ${lastAt}`);
    lastAt = record.atSeq;
    const next = records.slice(index + 1).find((r) => r.kind === "output");
    if (next && next.kind === "output" && record.atSeq > next.seqEnd) {
      violations.push(`R#${index} at ${record.atSeq} > next O ${next.seqEnd}`);
    }
  });
  return violations;
}

function text(records: readonly PtylogRecord[]): string {
  const decoder = new TextDecoder();
  return records
    .map((r) => (r.kind === "output" ? decoder.decode(r.data, { stream: true }) : ""))
    .join("");
}

describe("the recorded terminal corpus", () => {
  it.each(FIXTURES)("%s parses, keeps resizes in stream order, and fits 1 MB", (name) => {
    const records = loadPtylog(name);
    expect(records.some((r) => r.kind === "output")).toBe(true);
    expect(resizeOrderViolations(records)).toEqual([]);
    expect(statSync(ptylogPath(name)).size).toBeLessThanOrEqual(1024 * 1024);
  });

  it.each(FIXTURES)("%s is one contiguous stream: each delivery ends where it says", (name) => {
    const outputs = loadPtylog(name).filter((r) => r.kind === "output");
    for (let i = 1; i < outputs.length; i++) {
      expect(outputs[i].seqEnd - outputs[i - 1].seqEnd).toBe(outputs[i].data.length);
    }
  });

  it.each(["codex-tui", "claude-ink", "tui-fixture"] as const)(
    "%s holds a resize with output after it (the redraw the resize triggers)",
    (name) => {
      const records = loadPtylog(name);
      const resizes = records.flatMap((r, i) => (r.kind === "resize" ? [i] : []));
      expect(resizes.length).toBeGreaterThanOrEqual(1);
      expect(records.slice(resizes[0] + 1).some((r) => r.kind === "output")).toBe(true);
    },
  );

  it("codex-tui redraws in place by absolute cursor address on the alternate screen", () => {
    const stream = text(loadPtylog("codex-tui"));
    expect(stream).toMatch(csi(String.raw`\d+;\d+H`));
    expect(stream).toContain("\x1b[?1049h");
    expect(stream).toContain("\x1b[?1049l");
  });

  it("claude-ink redraws in place by relative cursor moves and line erases", () => {
    const stream = text(loadPtylog("claude-ink"));
    expect(stream).toMatch(csi(String.raw`\d+A`));
    expect(stream).toMatch(csi("2?K"));
  });

  it("append-only is numbered lines only: no cursor addressing", () => {
    const stream = text(loadPtylog("append-only"));
    expect(stream).toContain("\r\n3000\r\n");
    expect(stream).not.toMatch(csi(String.raw`\d+;\d+H`));
    expect(stream).not.toMatch(csi(String.raw`\d+A`));
  });

  it("tui-fixture replays into a headless xterm and ends on its final frame", async () => {
    // The generator's start size; the stream's own resizes take it from there.
    const term = new Terminal({ cols: 80, rows: 24, allowProposedApi: true });
    await replayPtylog(loadPtylog("tui-fixture"), {
      write: (data) => new Promise<void>((done) => term.write(data, done)),
      resize: (cols, rows) => term.resize(cols, rows),
    });
    const buffer = term.buffer.active;
    const row = (y: number) => buffer.getLine(y)?.translateToString(true) ?? "";
    const screen = Array.from({ length: term.rows }, (_, y) => row(buffer.viewportY + y));

    expect(buffer.type).toBe("normal");
    expect([term.cols, term.rows]).toEqual([120, 32]);
    expect(screen[0]).toMatch(/^tui-fixture 120x32 frame \d+ +\|$/);
    expect(screen[31]).toBe("[120x32] tui-fixture end");
    for (let y = 0; y < term.rows; y++) {
      expect(buffer.getLine(buffer.viewportY + y)?.isWrapped, `row ${y} wrapped`).toBe(false);
    }
    term.dispose();
  });

  it("the corpus holds no account identity or session ids", () => {
    for (const name of FIXTURES) {
      const stream = text(loadPtylog(name));
      expect(stream).not.toMatch(/@(?!example\.invalid)[\w-]+\.[a-z]{2,}/i);
      expect(stream).not.toMatch(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[1-9a-f]/);
      expect(stream).not.toMatch(/\/Users\//);
    }
  });
});

describe("parsePtylog", () => {
  it("reads output and resize records in file order", () => {
    const records = parsePtylog("O\t2\taGk=\nR\t2\t80\t24\nO\t3\tIQ==\n");
    expect(records).toEqual([
      { kind: "output", seqEnd: 2, data: new Uint8Array([104, 105]) },
      { kind: "resize", atSeq: 2, cols: 80, rows: 24 },
      { kind: "output", seqEnd: 3, data: new Uint8Array([33]) },
    ]);
  });

  it("names the malformed line", () => {
    expect(() => parsePtylog("O\t2\taGk=\nX\t1\n")).toThrow(/line 2/);
    expect(() => parsePtylog("R\t-1\t80\t24\n")).toThrow(/at_seq/);
  });

  it("the order check catches a resize recorded out of stream order", () => {
    const bad = parsePtylog("O\t5\taGVsbG8=\nR\t9\t80\t24\nO\t7\taGk=\nR\t3\t90\t30\n");
    expect(resizeOrderViolations(bad)).toEqual(["R#1 at 9 > next O 7", "R#3 at 3 < 9"]);
  });
});

describe("replayPtylog", () => {
  it("applies every write and resize in stream order", async () => {
    const calls: string[] = [];
    await replayPtylog(parsePtylog("O\t2\taGk=\nR\t2\t80\t24\nO\t3\tIQ==\n"), {
      write: (data) => void calls.push(`w${data.length}`),
      resize: (cols, rows) => void calls.push(`r${cols}x${rows}`),
    });
    expect(calls).toEqual(["w2", "r80x24", "w1"]);
  });

  it("refuses a resize recorded behind bytes already applied", async () => {
    const target = { write: () => undefined, resize: () => undefined };
    await expect(
      replayPtylog(parsePtylog("O\t5\taGVsbG8=\nR\t3\t80\t24\n"), target),
    ).rejects.toThrow(/resize at byte 3 recorded after byte 5/);
  });
});
