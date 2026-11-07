// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { render, screen } from "@/test-utils";

import { CODESENSE_DOCS_SLUG } from "@/shared/lib/codesense-docs";
import { ContextRepoMark, REPO_HUES, repoHue, repoInitials } from "./context-repo-mark";

describe("ContextRepoMark", () => {
  it("takes two initials from a multi-word name, else the first two letters", () => {
    expect(repoInitials("Acme Widgets")).toBe("AW");
    expect(repoInitials("acme-context")).toBe("AC");
    expect(repoInitials("Bolt")).toBe("BO");
    expect(repoInitials("  ")).toBe("?");
  });

  it("keeps a repo's hue stable and always picks from the curated set", () => {
    expect(repoHue("acme")).toBe(repoHue("acme"));
    const hues = new Set(["acme", "bolt", "codesense", "zebra", "widgets"].map(repoHue));
    for (const h of hues) expect(REPO_HUES).toContain(h);
    expect(hues.size).toBeGreaterThan(1);
  });

  it("renders initials as generated content (no text node) and the docs repo as a book", () => {
    const { rerender } = render(<ContextRepoMark slug="acme" name="Acme Widgets" />);
    const mark = screen.getByTestId("context-repo-mark");
    expect(mark).toHaveAttribute("data-initials", "AW");
    expect(mark.textContent).toBe("");
    rerender(<ContextRepoMark slug={CODESENSE_DOCS_SLUG} name="CodeSense" />);
    expect(screen.getByTestId("context-repo-mark")).not.toHaveAttribute("data-initials");
    expect(screen.getByTestId("context-repo-mark").querySelector("svg")).not.toBeNull();
  });
});
