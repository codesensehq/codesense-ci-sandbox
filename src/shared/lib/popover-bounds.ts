// SPDX-License-Identifier: MIT
export type PopoverBounds = Pick<DOMRect, "top" | "right" | "bottom" | "left">;

export function appPopoverBoundsElements() {
  return {
    content: document.querySelector("[data-testid=workbench]"),
    bar: document.querySelector("[data-workspace-bar]"),
  };
}

/** The shell's usable content above the workspace bar, in viewport coordinates. */
export function appPopoverBounds(): PopoverBounds {
  const elements = appPopoverBoundsElements();
  const content = elements.content?.getBoundingClientRect();
  const bar = elements.bar?.getBoundingClientRect();
  return {
    top: content?.top ?? 0,
    left: content?.left ?? 0,
    right: content?.right ?? window.innerWidth,
    bottom: bar && bar.height > 0 ? bar.top : (content?.bottom ?? window.innerHeight),
  };
}
