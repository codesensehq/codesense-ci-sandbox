// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";

import { MetadataChip } from "./metadata-chip";

afterEach(cleanup);

describe("MetadataChip (SW-VE-16 R3 Clean minimal chrome)", () => {
  it("`open=false`: resting button uses `--ibx-border` border + `--ibx-card` bg", () => {
    render(
      <MetadataChip
        open={false}
        onToggle={vi.fn()}
        regionId="metadata-band-region"
        metadataValid
      />,
    );
    const btn = screen.getByRole("button", { name: "Document metadata" });
    expect(btn).toHaveAttribute("aria-expanded", "false");
    const html = btn.outerHTML;
    // The resting chrome anchors to SW-IB1 chip tokens, NOT a parallel palette.
    expect(html).toContain("--ibx-border");
    expect(html).toContain("--ibx-card");
    // Must NOT be the active-surface accent-ring/wash at rest.
    expect(html).not.toContain("--ibx-accent-line");
    expect(html).not.toContain("--ibx-accent-wash");
  });

  it("`open=true`: open button lifts with a neutral border and shadow", () => {
    render(<MetadataChip open onToggle={vi.fn()} regionId="metadata-band-region" metadataValid />);
    const btn = screen.getByRole("button", { name: "Document metadata" });
    expect(btn).toHaveAttribute("aria-expanded", "true");
    const html = btn.outerHTML;
    expect(btn.className).toContain("aria-expanded:bg-[var(--ibx-lifted)]");
    expect(btn.className).toContain("aria-expanded:shadow-[0_1px_2px_var(--ibx-shadow)]");
    expect(html).not.toContain("--ibx-accent-wash");
  });

  it("`metadataValid={false}` colors the validity dot amber (nav parity)", () => {
    const { container } = render(
      <MetadataChip
        open={false}
        onToggle={vi.fn()}
        regionId="metadata-band-region"
        metadataValid={false}
      />,
    );
    expect(screen.getByRole("button", { name: "Document metadata, invalid" })).toHaveAttribute(
      "title",
      "Metadata · invalid",
    );
    const dot = container.querySelector("button > span.rounded-full");
    expect(dot).not.toBeNull();
    const html = (dot as HTMLElement).outerHTML;
    // The invalid-metadata color MUST route through `--ibx-amber` (dual-theme, routes to
    // `--cs-status-modified`) — a regression swapping to `--ibx-accent` or dropping the
    // amber signal would fail here, keeping the chip in parity with the nav's amber badge.
    expect(html).toContain("--ibx-amber");
    expect(html).not.toContain("--ibx-green");
  });

  it("`metadataValid={true}` omits the validity dot for healthy frontmatter", () => {
    const { container } = render(
      <MetadataChip
        open={false}
        onToggle={vi.fn()}
        regionId="metadata-band-region"
        metadataValid
      />,
    );
    const dot = container.querySelector("button > span");
    expect(dot).not.toHaveClass("rounded-full");
    expect(container.innerHTML).not.toContain("--ibx-green");
    expect(container.querySelector(".lucide-tags")).toBeInTheDocument();
    expect(container.querySelector(".lucide-chevron-down")).toBeNull();
  });

  it("clicking the chip fires onToggle", () => {
    const onToggle = vi.fn();
    render(
      <MetadataChip
        open={false}
        onToggle={onToggle}
        regionId="metadata-band-region"
        metadataValid
      />,
    );
    screen.getByRole("button", { name: "Document metadata" }).click();
    expect(onToggle).toHaveBeenCalledTimes(1);
  });
});

it("preserves its accessible name and shared rectangular recipe when labels collapse", () => {
  render(<MetadataChip open={false} onToggle={vi.fn()} regionId="metadata-band-region" />);
  const button = screen.getByRole("button", { name: "Document metadata" });
  expect(button).toHaveAttribute("aria-controls", "metadata-band-region");
  expect(button).toHaveClass("rounded", "h-[22px]");
  expect(button).not.toHaveClass("rounded-full");
  expect(screen.getByText("Metadata")).toHaveClass("viewer-header-label");
});
