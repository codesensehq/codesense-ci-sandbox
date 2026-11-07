// SPDX-License-Identifier: MIT
import { describe, it, expect, afterEach } from "vitest";
import { render, cleanup } from "@testing-library/react";

import {
  useDiagramContainer,
  useSelfClearingRef,
  removeMermaidOrphan,
} from "./use-diagram-container";

afterEach(() => cleanup());

function Probe() {
  const ref = useDiagramContainer();
  return <div ref={ref} data-testid="diagram" />;
}

function SectionProbe() {
  const ref = useSelfClearingRef<HTMLElement>();
  return <section ref={ref} data-testid="section" />;
}

describe("RF-perf-15 (F-2/F-3, DRY-1): useDiagramContainer", () => {
  it("clears the container innerHTML on unmount so the SVG backing is released", () => {
    const { unmount } = render(<Probe />);
    const el = document.querySelector<HTMLDivElement>('[data-testid="diagram"]')!;
    // Simulate the async render writing an SVG into the container.
    el.innerHTML = "<svg><rect /></svg>";
    expect(el.innerHTML).not.toBe("");

    unmount();

    // Cleanup runs before React detaches the node → the captured element is cleared.
    expect(el.innerHTML).toBe("");
  });
});

describe("SR-8: useSelfClearingRef (generalized to any HTMLElement)", () => {
  it("clears an imperatively-filled <section>'s innerHTML on unmount", () => {
    const { unmount } = render(<SectionProbe />);
    const el = document.querySelector<HTMLElement>('[data-testid="section"]')!;
    // Simulate BlockSection's imperative innerHTML / morphdom write.
    el.innerHTML = "<p>heavy morphdom-built subtree</p>";
    expect(el.innerHTML).not.toBe("");

    unmount();

    // Native backing of the detached subtree is released by construction.
    expect(el.innerHTML).toBe("");
  });
});

describe("RF-perf-15 (F-2): removeMermaidOrphan", () => {
  it("removes both the `<id>` and `d<id>` detached measuring nodes", () => {
    const a = document.createElement("div");
    a.id = "mermaid-abc";
    const b = document.createElement("div");
    b.id = "dmermaid-abc";
    document.body.append(a, b);

    removeMermaidOrphan("mermaid-abc");

    expect(document.getElementById("mermaid-abc")).toBeNull();
    expect(document.getElementById("dmermaid-abc")).toBeNull();
  });

  it("is a no-op for a null id", () => {
    expect(() => removeMermaidOrphan(null)).not.toThrow();
  });
});
