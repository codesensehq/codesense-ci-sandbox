// SPDX-License-Identifier: MIT
/**
 * Unit-test geometry for resizable panel groups. The test DOM lays nothing out,
 * so every element measures 0×0 and a divider can never be dragged. Installed,
 * every element measures as a flex layout of its panel group would: the root
 * fills a {@link VIEWPORT}, a group's children share its box along its axis by
 * their `flex-grow` (a panel not sized yet takes an equal share), and anything
 * else fills its parent. An element the app's CSS hides (`display:none` — a
 * tile collapsed by a zoom, a hidden divider) measures as the browser reports
 * it: a 0×0 box at the window origin, taking no share of its group. Call
 * {@link installPanelGeometry} at a test file's top level.
 */

import { afterAll, beforeAll } from "vitest";

export const VIEWPORT = { width: 1200, height: 800 };

function flexGrow(el: Element): number {
  const grow = /flex-grow:\s*([\d.]+)/.exec(el.getAttribute("style") ?? "");
  if (grow) return Number(grow[1]);
  return el.hasAttribute("data-panel") ? 1 : 0;
}

/** Hidden by the app's stylesheet, which the test DOM doesn't load: the zoom's
 *  collapsed tiles (index.css) and Tailwind's `hidden`. */
function displayNone(el: Element): boolean {
  return el.matches('[data-panel][data-zoom="collapsed"], .hidden');
}

function rectOf(el: Element): DOMRect {
  const parent = el.parentElement;
  if (!parent || el === document.body) return new DOMRect(0, 0, VIEWPORT.width, VIEWPORT.height);
  if (displayNone(el)) return new DOMRect(0, 0, 0, 0);
  const box = rectOf(parent);
  if (box.width === 0 && box.height === 0) return box;
  if (!parent.hasAttribute("data-group")) return box;
  const vertical = /flex-direction:\s*column/.test(parent.getAttribute("style") ?? "");
  const siblings = [...parent.children].filter((c) => !displayNone(c));
  const total = siblings.reduce((sum, c) => sum + flexGrow(c), 0) || 1;
  let before = 0;
  for (const c of siblings) {
    if (c === el) break;
    before += flexGrow(c);
  }
  const share = flexGrow(el) / total;
  return vertical
    ? new DOMRect(box.x, box.y + (before / total) * box.height, box.width, share * box.height)
    : new DOMRect(box.x + (before / total) * box.width, box.y, share * box.width, box.height);
}

const MEASURED = ["offsetWidth", "offsetHeight", "offsetLeft", "offsetTop"] as const;

export function installPanelGeometry(): void {
  const saved = new Map<string, PropertyDescriptor | undefined>();
  let savedRect: typeof Element.prototype.getBoundingClientRect;
  beforeAll(() => {
    savedRect = Element.prototype.getBoundingClientRect;
    Element.prototype.getBoundingClientRect = function (this: Element) {
      return rectOf(this);
    };
    const read: Record<(typeof MEASURED)[number], (r: DOMRect) => number> = {
      offsetWidth: (r) => r.width,
      offsetHeight: (r) => r.height,
      offsetLeft: (r) => r.x,
      offsetTop: (r) => r.y,
    };
    for (const prop of MEASURED) {
      saved.set(prop, Object.getOwnPropertyDescriptor(HTMLElement.prototype, prop));
      Object.defineProperty(HTMLElement.prototype, prop, {
        configurable: true,
        get(this: HTMLElement) {
          return read[prop](rectOf(this));
        },
      });
    }
    // The panel library reads `ariaDisabled` as null when the attribute is absent.
    saved.set("ariaDisabled", Object.getOwnPropertyDescriptor(Element.prototype, "ariaDisabled"));
    Object.defineProperty(Element.prototype, "ariaDisabled", {
      configurable: true,
      get(this: Element) {
        return this.getAttribute("aria-disabled");
      },
    });
  });
  afterAll(() => {
    Element.prototype.getBoundingClientRect = savedRect;
    for (const [prop, descriptor] of saved) {
      const target = prop === "ariaDisabled" ? Element.prototype : HTMLElement.prototype;
      if (descriptor) Object.defineProperty(target, prop, descriptor);
      else delete (target as unknown as Record<string, unknown>)[prop];
    }
  });
}

/** Drag the divider `separator` by `dx`,`dy` pixels, as a pointer would. */
export function dragSeparator(separator: HTMLElement, dx: number, dy: number): void {
  const r = separator.getBoundingClientRect();
  const x = r.x + r.width / 2;
  const y = r.y + r.height / 2;
  const at = (px: number, py: number) => ({ clientX: px, clientY: py, pointerId: 1, button: 0 });
  separator.dispatchEvent(
    new PointerEvent("pointerdown", { bubbles: true, buttons: 1, ...at(x, y) }),
  );
  separator.dispatchEvent(
    new PointerEvent("pointermove", { bubbles: true, buttons: 1, ...at(x + dx, y + dy) }),
  );
  separator.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, ...at(x + dx, y + dy) }));
}
