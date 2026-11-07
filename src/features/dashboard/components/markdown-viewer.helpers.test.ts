// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { scrollToAnchor, resolveInternalHref } from "./markdown-viewer";

// ---------------------------------------------------------------------------
// scrollToAnchor — pure DOM helper. Tests run against happy-dom.
// ---------------------------------------------------------------------------

describe("scrollToAnchor", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });

  it("scrolls a matching [id=...] element into view", () => {
    const container = document.createElement("div");
    const target = document.createElement("h2");
    target.id = "executive-summary";
    target.textContent = "Executive Summary";
    container.appendChild(target);
    document.body.appendChild(container);

    const spy = vi.spyOn(target, "scrollIntoView").mockImplementation(() => {});
    const result = scrollToAnchor("#executive-summary", container);
    expect(result).toBe(true);
    expect(spy).toHaveBeenCalledWith({ behavior: "instant", block: "start" });
  });

  it("accepts a hash without the leading #", () => {
    const container = document.createElement("div");
    const target = document.createElement("h2");
    target.id = "x";
    container.appendChild(target);
    const spy = vi.spyOn(target, "scrollIntoView").mockImplementation(() => {});

    expect(scrollToAnchor("x", container)).toBe(true);
    expect(spy).toHaveBeenCalled();
  });

  it("returns false when container is null", () => {
    expect(scrollToAnchor("#foo", null)).toBe(false);
  });

  it("returns false on empty hash", () => {
    const container = document.createElement("div");
    expect(scrollToAnchor("", container)).toBe(false);
    expect(scrollToAnchor("#", container)).toBe(false);
  });

  it("returns false when no matching id exists", () => {
    const container = document.createElement("div");
    expect(scrollToAnchor("#missing", container)).toBe(false);
  });

  it("escapes ids with special characters via CSS.escape", () => {
    const container = document.createElement("div");
    const target = document.createElement("h2");
    // Valid id with a dot, which breaks naive bracket-attribute selectors
    // if not escaped. `CSS.escape` handles this cleanly.
    target.id = "section.with.dots";
    container.appendChild(target);
    const spy = vi.spyOn(target, "scrollIntoView").mockImplementation(() => {});

    expect(scrollToAnchor("#section.with.dots", container)).toBe(true);
    expect(spy).toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// resolveInternalHref — reads data-internal-href + original fragment
// ---------------------------------------------------------------------------

describe("resolveInternalHref", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });

  it("returns path + hash when anchor has both", () => {
    const a = document.createElement("a");
    a.setAttribute("href", "other.md#section");
    a.dataset.internalHref = "/repo/docs/other.md";
    document.body.appendChild(a);

    expect(resolveInternalHref(a)).toEqual({
      path: "/repo/docs/other.md",
      hash: "#section",
    });
  });

  it("returns just path when no fragment", () => {
    const a = document.createElement("a");
    a.setAttribute("href", "other.md");
    a.dataset.internalHref = "/repo/docs/other.md";
    document.body.appendChild(a);

    expect(resolveInternalHref(a)).toEqual({ path: "/repo/docs/other.md" });
  });

  it("traverses ancestors to find the anchor (closest semantics)", () => {
    const a = document.createElement("a");
    a.setAttribute("href", "other.md");
    a.dataset.internalHref = "/repo/docs/other.md";
    const inner = document.createElement("span");
    a.appendChild(inner);
    document.body.appendChild(a);

    // Click target is the inner span — helper must walk up.
    expect(resolveInternalHref(inner)).toEqual({ path: "/repo/docs/other.md" });
  });

  it("returns null when no internal-href anchor ancestor", () => {
    const div = document.createElement("div");
    document.body.appendChild(div);
    expect(resolveInternalHref(div)).toBeNull();
  });

  it("returns null when anchor has no data-internal-href (external link)", () => {
    const a = document.createElement("a");
    a.setAttribute("href", "https://example.com");
    document.body.appendChild(a);
    expect(resolveInternalHref(a)).toBeNull();
  });
});
