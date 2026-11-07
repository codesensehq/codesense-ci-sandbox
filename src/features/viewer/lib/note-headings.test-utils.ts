// SPDX-License-Identifier: MIT
import { vi } from "vitest";

/**
 * Shared DOM primitives for the note-headings scroll tests (helper unit test +
 * outline + sticky). happy-dom has no layout, so every test that drives the
 * scroll helpers must hand-mock element rects and the scroll port. These build
 * the fiddly bits ONE way so the three suites don't each hand-roll (and drift on)
 * the `getBoundingClientRect` cast and heading-element shape.
 *
 * Deliberately NOT a shared editor fake: the three suites need genuinely
 * different `nodeDOM` policies (lazy any-pos vs eager nullable vs callback), so
 * each assembles its own editor around these primitives rather than through one
 * over-parameterized fixture.
 */

/** Mock an element's viewport rect top (the only field the scroll helpers read).
 *  Internal — callers use `makeHeadingEl` / `makeScrollContainer`. */
function setRectTop(el: HTMLElement, top: number): void {
  el.getBoundingClientRect = () => ({ top }) as DOMRect;
}

/** A heading element at a known viewport top, carrying a `scrollIntoView` spy so
 *  a test can assert the ancestor-walking API is never called (SW-SN-15). `top`
 *  may be a live getter for scroll-spy tests that move the heading after render. */
export function makeHeadingEl(top: number | (() => number), tag = "h1"): HTMLElement {
  const el = document.createElement(tag);
  const readTop = typeof top === "function" ? top : () => top;
  el.getBoundingClientRect = () => ({ top: readTop() }) as DOMRect;
  el.scrollIntoView = vi.fn();
  return el;
}

/** A scroll-port container (`= editor.view.dom`) with a mocked rect top and an
 *  optional starting `scrollTop`. This is the element the jump helper moves. */
export function makeScrollContainer(top = 0, scrollTop = 0): HTMLDivElement {
  const el = document.createElement("div");
  setRectTop(el, top);
  el.scrollTop = scrollTop;
  return el;
}
