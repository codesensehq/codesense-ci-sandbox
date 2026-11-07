// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { Terminal } from "@xterm/xterm";
import { Terminal as HeadlessTerminal } from "@xterm/headless";
import { ATTR_FLAGS, referenceBuffer } from "../buffer-dump";
import {
  PARITY_TERMINAL_OPTIONS,
  headlessFrom,
  writeParsed,
} from "../__test-utils__/headless-from";
import { loadPtylog, replayPtylog, type PtylogFixture } from "../__fixtures__/ptylog";

const FIXTURES: PtylogFixture[] = ["codex-tui", "claude-ink", "append-only"];

describe("referenceBuffer", () => {
  it.each(FIXTURES)(
    "%s: a display xterm and the headless reference dump identically",
    async (name) => {
      const display = new Terminal(PARITY_TERMINAL_OPTIONS);
      await replayPtylog(loadPtylog(name), {
        write: (data) => writeParsed(display, data),
        resize: (cols, rows) => display.resize(cols, rows),
      });
      const dumped = referenceBuffer(display);
      display.dispose();
      expect(dumped).toEqual(await headlessFrom(name));
    },
  );

  it("encodes a row's SGR runs as merged {start, end, fg, bg, flags} with trailing blanks omitted", async () => {
    const term = new HeadlessTerminal({ cols: 20, rows: 2, allowProposedApi: true });
    await writeParsed(term, "\x1b[1;31mab\x1b[0m\x1b[44mcd\x1b[0;4;7mef\x1b[0m   ");
    const { attrs, lines } = referenceBuffer(term);
    term.dispose();
    const P16 = 1 << 24;
    const DEFAULT = -1;
    expect(lines[0]).toBe("abcdef   ");
    expect(attrs[0]).toEqual([
      { start: 0, end: 2, fg: P16 | 1, bg: DEFAULT, flags: ATTR_FLAGS.bold },
      { start: 2, end: 4, fg: DEFAULT, bg: P16 | 4, flags: 0 },
      {
        start: 4,
        end: 6,
        fg: DEFAULT,
        bg: DEFAULT,
        flags: ATTR_FLAGS.underline | ATTR_FLAGS.inverse,
      },
    ]);
    expect(attrs[1]).toEqual([]);
  });

  it("dumps the alternate screen while a full-screen TUI holds it", async () => {
    const records = loadPtylog("codex-tui");
    const altEnter = records.findIndex(
      (r) => r.kind === "output" && new TextDecoder().decode(r.data).includes("\x1b[?1049h"),
    );
    expect(altEnter).toBeGreaterThanOrEqual(0);
    const cut = await headlessFrom(records.slice(0, altEnter + 1));
    expect(cut.activeBuffer).toBe("alternate");
    expect(cut.alt).not.toBeNull();
    expect(cut.alt?.lines).toHaveLength(cut.rows);
  });

  it("reports a hidden cursor (DECTCEM)", async () => {
    const term = new HeadlessTerminal({ cols: 10, rows: 2, allowProposedApi: true });
    await writeParsed(term, "\x1b[?25l");
    expect(referenceBuffer(term).cursorVisible).toBe(false);
    await writeParsed(term, "\x1b[?25h");
    expect(referenceBuffer(term).cursorVisible).toBe(true);
    term.dispose();
  });
});
