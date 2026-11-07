// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { Editor } from "@tiptap/core";
import StarterKit from "@tiptap/starter-kit";
import { Markdown } from "tiptap-markdown";
import { serializeWithBlankLines, type MarkdownDocSerializer } from "./scratch-blank-lines";
import { csLinkOptions } from "./cs-link";

// SW-SN-9 Task 1 / R1: the load-bearing round-trip, proven through a REAL tiptap
// + tiptap-markdown editor (not a mock) — the only layer where the `cs:` scheme
// can actually be stripped. Mirrors scratch-blank-lines.integration.test.ts.

function makeEditor(content: string, link: object | false) {
  return new Editor({
    extensions: [
      StarterKit.configure({ link } as Parameters<typeof StarterKit.configure>[0]),
      Markdown,
    ],
    content,
  });
}

function serialize(editor: Editor): string {
  const s = (editor.storage as unknown as { markdown: { serializer: MarkdownDocSerializer } })
    .markdown.serializer;
  return serializeWithBlankLines(editor.state.doc, editor.schema, s);
}

const CS = "cs:story:SW-SN-2";

describe("cs: link round-trip (SW-SN-9 R1)", () => {
  it("serializes a cs: link to markdown `[label](cs:…)`", () => {
    const editor = makeEditor("<p>see it</p>", csLinkOptions);
    editor.chain().setTextSelection({ from: 1, to: 8 }).setLink({ href: CS }).run();
    expect(serialize(editor)).toContain(`(${CS})`);
    editor.destroy();
  });

  it("survives save→reload→save unchanged with the cs config (the fix)", () => {
    const editor = makeEditor(`<p><a href="${CS}">label</a></p>`, csLinkOptions);
    const stored = serialize(editor);
    expect(stored).toContain(CS);

    const reloaded = makeEditor(stored, csLinkOptions);
    const restored = serialize(reloaded);
    expect(restored).toBe(stored);
    expect(restored).toContain(CS); // scheme preserved across the round-trip

    editor.destroy();
    reloaded.destroy();
  });

  it("NEGATIVE CONTROL: default Link config DROPS the cs: scheme on reload", () => {
    // With Link's default protocols (no `cs`), isAllowedUri rejects the href on
    // parse, so reloading the same markdown loses the mark — this is the exact
    // failure csLinkOptions prevents. Proves the test genuinely exercises the
    // sanitizer, not just a happy path.
    const stored = `[label](${CS})`;
    const reloadedDefault = makeEditor(stored, {}); // default Link opts (no cs protocol)
    expect(serialize(reloadedDefault)).not.toContain(CS);
    reloadedDefault.destroy();
  });

  it("preserves a note (cs:file with a #hash) losslessly alongside prose + blank lines", () => {
    const href = "cs:file:_bmad-output/a/b.md#sec";
    const editor = makeEditor(`<p><a href="${href}">b</a></p><p></p><p>tail</p>`, csLinkOptions);
    const stored = serialize(editor);
    const reloaded = makeEditor(stored, csLinkOptions);
    expect(serialize(reloaded)).toBe(stored);
    expect(stored).toContain(href);
    editor.destroy();
    reloaded.destroy();
  });
});
