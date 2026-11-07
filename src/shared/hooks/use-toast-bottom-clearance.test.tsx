// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/react";

import { TOAST_BOTTOM_CLEARANCE_VAR } from "@/components/ui/toaster-placement";
import { useToastBottomClearance } from "./use-toast-bottom-clearance";

function Strip({ h }: { h: number }) {
  const ref = useToastBottomClearance();
  return <div ref={ref} data-h={h} />;
}

const clearance = () => document.documentElement.style.getPropertyValue(TOAST_BOTTOM_CLEARANCE_VAR);

beforeEach(() => {
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (
    this: HTMLElement,
  ) {
    return Number(this.dataset.h ?? 0);
  });
});
afterEach(() => vi.restoreAllMocks());

describe("toasts clear the bottom chrome", () => {
  it("publishes the strip's height, the tallest of several, and 0 once it goes", () => {
    const tray = render(<Strip h={32} />);
    expect(clearance()).toBe("32px");
    // A backgrounded team's hidden tray measures 0 and doesn't pull toasts down.
    const hidden = render(<Strip h={0} />);
    expect(clearance()).toBe("32px");
    const wrapped = render(<Strip h={96} />);
    expect(clearance()).toBe("96px");
    wrapped.unmount();
    expect(clearance()).toBe("32px");
    tray.unmount();
    hidden.unmount();
    expect(clearance()).toBe("0px");
  });
});
