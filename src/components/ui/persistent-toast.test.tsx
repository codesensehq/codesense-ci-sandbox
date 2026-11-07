// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useEffect, useState } from "react";
import { act, render, screen } from "@testing-library/react";
import { toast } from "sonner";

import {
  COLLAPSED_TOAST_SELECTOR,
  PERSISTENT_TOAST_COLLAPSE_MS,
  TOAST_CHIP_HEIGHT_PX,
  TOAST_COLLAPSED_ATTR,
  persistent,
} from "./persistent-toast";
import { Toaster } from "./sonner";

// The app's Toaster with sonner's real toast store: a toast that never expires
// shrinks to a one-line chip, and opens again on hover or keyboard focus.

const li = (title: string) => screen.getByText(title).closest("[data-sonner-toast]")!;
const collapsed = (title: string) => li(title).hasAttribute(TOAST_COLLAPSED_ATTR);
const advance = (ms: number) => act(() => void vi.advanceTimersByTime(ms));
const show = async (fn: () => void) => {
  act(fn);
  advance(50); // sonner adds toasts on a timer
  await act(async () => {}); // the toaster sees the new toast
};

beforeEach(() => {
  // Timers only: the toaster watches for toasts with a MutationObserver, whose
  // callbacks ride real microtasks.
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
  render(<Toaster />);
});
afterEach(() => {
  act(() => void toast.dismiss());
  advance(1_000);
  vi.useRealTimers();
});

describe("persistent toasts collapse to a chip", () => {
  it("shows in full, then collapses after the delay", async () => {
    await show(() =>
      toast("Update ready", persistent({ id: "u", description: "v2 is ready to install." })),
    );
    expect(collapsed("Update ready")).toBe(false);
    advance(PERSISTENT_TOAST_COLLAPSE_MS);
    expect(collapsed("Update ready")).toBe(true);
  });

  it("opens again on hover, and collapses again after the pointer leaves", async () => {
    await show(() => toast("Update ready", persistent({ id: "u" })));
    advance(PERSISTENT_TOAST_COLLAPSE_MS);
    const el = li("Update ready");
    act(() => void el.dispatchEvent(new MouseEvent("mouseenter")));
    expect(collapsed("Update ready")).toBe(false);
    advance(PERSISTENT_TOAST_COLLAPSE_MS * 2);
    expect(collapsed("Update ready")).toBe(false); // stays open while hovered
    act(() => void el.dispatchEvent(new MouseEvent("mouseleave")));
    advance(PERSISTENT_TOAST_COLLAPSE_MS - 1);
    expect(collapsed("Update ready")).toBe(false);
    advance(1);
    expect(collapsed("Update ready")).toBe(true);
  });

  it("opens again on keyboard focus, and collapses once focus leaves", async () => {
    await show(() =>
      toast(
        "Track acme/app?",
        persistent({ id: "t", action: { label: "Accept", onClick: () => {} } }),
      ),
    );
    advance(PERSISTENT_TOAST_COLLAPSE_MS);
    expect(collapsed("Track acme/app?")).toBe(true);
    const el = li("Track acme/app?") as HTMLElement;
    act(() => el.focus());
    expect(collapsed("Track acme/app?")).toBe(false);
    advance(PERSISTENT_TOAST_COLLAPSE_MS * 2);
    expect(collapsed("Track acme/app?")).toBe(false);
    act(() => el.blur());
    advance(PERSISTENT_TOAST_COLLAPSE_MS);
    expect(collapsed("Track acme/app?")).toBe(true);
  });

  it("a toast re-issued while collapsed opens with its new content, then collapses again", async () => {
    await show(() => toast("Update ready", persistent({ id: "u", description: "v2" })));
    advance(PERSISTENT_TOAST_COLLAPSE_MS);
    expect(collapsed("Update ready")).toBe(true);
    await show(() => toast("Update ready", persistent({ id: "u", description: "v3" })));
    expect(screen.getByText("v3")).toBeInTheDocument();
    expect(collapsed("Update ready")).toBe(false);
    advance(PERSISTENT_TOAST_COLLAPSE_MS);
    expect(collapsed("Update ready")).toBe(true);
  });

  it("content changing in place (a download's progress) keeps it collapsed", async () => {
    const progress: { set: (n: number) => void } = { set: () => {} };
    function Progress() {
      const [pct, set] = useState(0);
      useEffect(() => {
        progress.set = set;
      }, []);
      return (
        <div data-cs-toast-body>
          <p>Downloading v2</p>
          <p>{pct}% · 1 MB / 9 MB</p>
        </div>
      );
    }
    await show(() => toast.custom(() => <Progress />, persistent({ id: "d" })));
    advance(PERSISTENT_TOAST_COLLAPSE_MS);
    expect(collapsed("Downloading v2")).toBe(true);
    for (const pct of [10, 40, 80]) {
      act(() => progress.set(pct));
      await act(async () => {});
      expect(screen.getByText(`${pct}% · 1 MB / 9 MB`)).toBeInTheDocument();
      expect(collapsed("Downloading v2")).toBe(true);
    }
    // Issued again under its id (Check for updates): it opens.
    await show(() => toast.custom(() => <Progress />, persistent({ id: "d" })));
    expect(collapsed("Downloading v2")).toBe(false);
  });

  it("the re-measure guard reads the height property, never sonner's --initial-height", async () => {
    await show(() => toast("Update ready", persistent({ id: "u" })));
    advance(PERSISTENT_TOAST_COLLAPSE_MS);
    const el = li("Update ready") as HTMLElement;
    el.style.setProperty("--initial-height", "auto");
    expect(el.getAttribute("style")).toContain("--initial-height: auto");
    expect(el.matches(COLLAPSED_TOAST_SELECTOR)).toBe(true);
  });

  it("sonner's re-measure sees the full toast, so it reopens at full height", async () => {
    await show(() => toast("Update ready", persistent({ id: "u", description: "v2" })));
    advance(PERSISTENT_TOAST_COLLAPSE_MS);
    const el = li("Update ready") as HTMLElement;
    expect(el.matches(COLLAPSED_TOAST_SELECTOR)).toBe(true);
    // What sonner does to measure a toast whose content changed.
    const before = el.style.height;
    el.style.height = "auto";
    expect(el.matches(COLLAPSED_TOAST_SELECTOR)).toBe(false);
    el.style.height = before;
    expect(el.matches(COLLAPSED_TOAST_SELECTOR)).toBe(true);
    // The chip is clamped with max-height, never a height sonner would read back.
    const css = [...document.querySelectorAll("style")].map((n) => n.textContent).join("\n");
    expect(css).toContain(`${COLLAPSED_TOAST_SELECTOR} {`);
    expect(css).toContain(`max-height: ${TOAST_CHIP_HEIGHT_PX}px`);
    expect(css).not.toMatch(new RegExp(`[^-]height: ${TOAST_CHIP_HEIGHT_PX}px`));
  });

  it("a chip keeps its description and buttons in the page, only out of sight", async () => {
    await show(() =>
      toast(
        "Track acme/app?",
        persistent({ id: "t", description: "d", action: { label: "Accept", onClick: () => {} } }),
      ),
    );
    advance(PERSISTENT_TOAST_COLLAPSE_MS);
    expect(screen.getByRole("button", { name: "Accept" })).toBeInTheDocument();
    const css = [...document.querySelectorAll("style")].map((n) => n.textContent).join("\n");
    expect(css).toContain("clip-path:inset(50%)");
    expect(css).not.toMatch(/display:\s*none/);
  });

  it("leaves a transient toast alone", async () => {
    await show(() => toast("Copied", { duration: 60_000 }));
    advance(PERSISTENT_TOAST_COLLAPSE_MS * 2);
    expect(collapsed("Copied")).toBe(false);
  });

  it("persistent() keeps the caller's class and never expires", () => {
    const a = persistent({ className: "x" });
    expect(a.duration).toBe(Number.POSITIVE_INFINITY);
    expect(a.className).toMatch(/^x cs-toast-persistent cs-toast-issue-\d+$/);
    expect(persistent({}).className).not.toBe(persistent({}).className);
  });
});
