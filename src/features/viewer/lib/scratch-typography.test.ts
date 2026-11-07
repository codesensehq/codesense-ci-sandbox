// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

// SW-SN-8 AC-5: the Mac-native SF typography must be SCOPED to
// `.scratch-note-editor` and must NOT change the app-wide Geist default. This
// test-locks the scope so a future edit can't silently globalize the font swap.

const scratchCss = readFileSync(resolve(__dirname, "../../../styles/scratch-note.css"), "utf8");
const indexCss = readFileSync(resolve(__dirname, "../../../index.css"), "utf8");

describe("scratch-note typography scope (SW-SN-8 AC-5)", () => {
  it("applies the system SF stack scoped to the notes pane, used by the editor", () => {
    const block = (selector: string) =>
      scratchCss.slice(
        scratchCss.indexOf(selector),
        scratchCss.indexOf("}", scratchCss.indexOf(selector)),
      );
    const pane = block(".nt-pane,\n.nt-pop {");
    expect(pane).toContain("-apple-system");
    expect(pane).toContain('"SF Pro Text"');
    expect(block(".scratch-note-editor {")).toContain("font-family: var(--nt-font-note)");
  });

  it("writes at a 14px base of its own, not the viewer's font size", () => {
    const editor = scratchCss.slice(
      scratchCss.indexOf(".scratch-note-editor {"),
      scratchCss.indexOf("}", scratchCss.indexOf(".scratch-note-editor {")),
    );
    expect(editor).toContain("font-size: 14px");
    const panel = readFileSync(resolve(__dirname, "../components/scratch-note-panel.tsx"), "utf8");
    expect(panel).not.toContain("viewerFontSize");
  });

  it("caps the measure on the editor", () => {
    expect(scratchCss).toContain("max-width: 68ch");
  });

  it("leaves the global Geist --font-sans untouched (not a global swap)", () => {
    // index.css keeps Geist as the app default...
    expect(indexCss).toContain('--font-sans: "Geist Variable"');
    // ...and does not pull in the SF system stack globally.
    expect(indexCss).not.toContain("-apple-system");
  });
});
