// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";

import { MarkdownPreviewPane } from "./markdown-preview-pane";

describe("MarkdownPreviewPane (SW-VE-10)", () => {
  it("renders the buffer as formatted prose (headings, bold)", () => {
    render(<MarkdownPreviewPane content={"# Title\n\n**bold** text"} proseInvert={false} />);
    expect(screen.getByRole("heading", { name: "Title" })).toBeInTheDocument();
    expect(screen.getByText("bold").tagName).toBe("STRONG");
  });

  it("renders GFM tables (shared plugin set)", () => {
    const table = "| a | b |\n| - | - |\n| 1 | 2 |\n";
    render(<MarkdownPreviewPane content={table} proseInvert={false} />);
    expect(screen.getByRole("table")).toBeInTheDocument();
  });

  it("parses and HIDES frontmatter — body only, never a second frontmatter surface", () => {
    render(
      <MarkdownPreviewPane content={"---\ntitle: secret\n---\n\n# Body"} proseInvert={false} />,
    );
    expect(screen.getByRole("heading", { name: "Body" })).toBeInTheDocument();
    expect(screen.queryByText(/title: secret/)).not.toBeInTheDocument();
  });

  it("applies the dark:prose-invert class only when proseInvert (default theme)", () => {
    const { container, rerender } = render(<MarkdownPreviewPane content="x" proseInvert />);
    expect(container.querySelector(".md-viewer-prose")?.className).toContain("dark:prose-invert");
    rerender(<MarkdownPreviewPane content="x" proseInvert={false} />);
    expect(container.querySelector(".md-viewer-prose")?.className).not.toContain(
      "dark:prose-invert",
    );
  });
});
