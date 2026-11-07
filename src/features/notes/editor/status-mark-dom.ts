// SPDX-License-Identifier: MIT
import { statusMarkDrawing } from "@/shared/lib/status-mark-svg";
import type { StatusMarkKind } from "@/shared/lib/status-mark";

let nextMask = 0;
/** The native status mark, mounted without a framework runtime. */
export function statusMarkDom(mark: StatusMarkKind): HTMLElement {
  const span = document.createElement("span");
  span.className = `cs-mark cs-mark-${mark}`;
  span.dataset.mark = mark;
  span.setAttribute("aria-hidden", "true");
  span.style.width = span.style.height = "7px";
  const drawing = statusMarkDrawing(mark, 7, `nt-mark-${++nextMask}`);
  span.innerHTML = `<svg width="7" height="7" viewBox="0 0 7 7" overflow="visible"${drawing.className ? ` class="${drawing.className}"` : ""}>${drawing.html}</svg>`;
  return span;
}
