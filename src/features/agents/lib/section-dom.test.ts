// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it } from "vitest";

import { isShown, sectionIn } from "./section-dom";

afterEach(() => {
  document.body.innerHTML = "";
});

function editor(hidden: boolean): HTMLElement {
  const wrap = document.createElement("div");
  if (hidden) wrap.hidden = true;
  wrap.innerHTML = '<section data-editor><div id="ed-permissions">p</div></section>';
  document.body.append(wrap);
  return wrap.querySelector("[data-editor]")!;
}

describe("sectionIn", () => {
  it("finds a section inside the given editor, never another editor's with the same id", () => {
    const first = editor(true);
    const second = editor(false);
    expect(sectionIn(second, "permissions")).toBe(second.querySelector("#ed-permissions"));
    expect(sectionIn(second, "permissions")).not.toBe(first.querySelector("#ed-permissions"));
  });

  it("is null when the editor has no such section, or there is no editor", () => {
    expect(sectionIn(editor(false), "tools")).toBeNull();
    expect(sectionIn(null, "tools")).toBeNull();
  });
});

describe("isShown", () => {
  it("is false under a hidden ancestor (a retained team's editor) and true otherwise", () => {
    expect(isShown(editor(true))).toBe(false);
    expect(isShown(editor(false))).toBe(true);
    expect(isShown(null)).toBe(false);
  });
});
