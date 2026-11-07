// SPDX-License-Identifier: MIT
/**
 * What a file's text says about how it should be edited: its line ending and
 * its indentation. The editor adopts both so a save writes the file back the
 * way it was written — CodeMirror otherwise joins lines with "\n", which would
 * silently convert every line of a CRLF file on the first save.
 */

export type LineEnding = "\n" | "\r\n";

/**
 * CRLF only when EVERY line break is CRLF. CodeMirror's line-separator facet
 * then splits on "\r\n" alone, so a stray "\n" in a mixed file would render
 * inside a line; mixed files edit as LF instead.
 */
export function detectLineEnding(text: string): LineEnding {
  let crlf = 0;
  let lf = 0;
  for (let i = text.indexOf("\n"); i !== -1; i = text.indexOf("\n", i + 1)) {
    if (i > 0 && text.charCodeAt(i - 1) === 13) crlf++;
    else lf++;
  }
  return crlf > 0 && lf === 0 ? "\r\n" : "\n";
}

export function lineEndingLabel(eol: LineEnding): "LF" | "CRLF" {
  return eol === "\r\n" ? "CRLF" : "LF";
}

export interface Indentation {
  useTabs: boolean;
  /** Spaces per level (or the tab render width when `useTabs`). */
  size: number;
}

/** Only the first lines are sampled: indentation style is settled early and
 *  this runs on every file open. */
const SAMPLE_LINES = 2000;

/**
 * Infer indentation from leading whitespace. Tabs win when more indented lines
 * start with a tab than with spaces. For spaces, the most common indent step
 * between consecutive indented lines is the unit (2/4/8 preferred), which is
 * robust to the odd deep continuation line. `fallback` answers files with no
 * indented lines at all.
 */
export function detectIndentation(text: string, fallback: Indentation): Indentation {
  let tabLines = 0;
  let spaceLines = 0;
  const steps = new Map<number, number>();
  let previous = 0;
  let count = 0;
  let start = 0;
  while (start <= text.length && count < SAMPLE_LINES) {
    let end = text.indexOf("\n", start);
    if (end === -1) end = text.length;
    const line = text.slice(start, end);
    start = end + 1;
    count++;
    if (!line.trim()) continue;
    if (line.startsWith("\t")) {
      tabLines++;
      previous = 0;
      continue;
    }
    let spaces = 0;
    while (spaces < line.length && line.charCodeAt(spaces) === 32) spaces++;
    // A lone space before `*` is a block-comment continuation, not indentation.
    if (spaces > 0 && line[spaces] === "*" && spaces % 2 === 1) continue;
    if (spaces > 0) spaceLines++;
    const step = Math.abs(spaces - previous);
    if (step > 0 && step <= 8) steps.set(step, (steps.get(step) ?? 0) + 1);
    previous = spaces;
  }
  if (tabLines === 0 && spaceLines === 0) return fallback;
  if (tabLines > spaceLines) return { useTabs: true, size: fallback.size };
  let best = fallback.size;
  let bestVotes = 0;
  for (const [step, votes] of steps) {
    // Prefer the conventional widths on a tie.
    const weighted = votes + (step === 2 || step === 4 ? 0.5 : 0);
    if (weighted > bestVotes) {
      best = step;
      bestVotes = weighted;
    }
  }
  return { useTabs: false, size: best };
}

export function indentationLabel(indent: Indentation): string {
  return indent.useTabs ? `Tab Size: ${indent.size}` : `Spaces: ${indent.size}`;
}
