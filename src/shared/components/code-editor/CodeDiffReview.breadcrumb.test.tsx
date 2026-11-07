// SPDX-License-Identifier: MIT
/**
 * SLW-DV-8 (AC-2) — the sticky scope breadcrumb in `<CodeDiffReview>`. The bar is
 * gated on the merge-view hook's `breadcrumb`; here we drive that value directly
 * (real scope resolution needs CM layout + a parse tree, which jsdom can't
 * produce and which `scope-breadcrumb.test.ts` already covers against real
 * grammars) to prove the render gate and the label formatting.
 */
import { render, screen } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";

import { CodeDiffReview } from "./CodeDiffReview";
import { makeCodeMergeViewResult } from "./code-editor-test-utils";
import type { UseCodeMergeViewResult } from "./use-code-merge-view";

let breadcrumbState: UseCodeMergeViewResult["breadcrumb"] = null;

vi.mock("./use-code-merge-view", () => ({
  useCodeMergeView: (): UseCodeMergeViewResult =>
    makeCodeMergeViewResult({ breadcrumb: breadcrumbState }),
}));

vi.mock("./use-merge-view-chrome", () => ({
  useMergeViewChrome: () => ({
    theme: "light",
    fontSize: 13,
    language: "typescript",
    container: null,
    containerCallback: () => {},
    themeClass: "",
  }),
}));

describe("CodeDiffReview — scope breadcrumb (SLW-DV-8 AC-2)", () => {
  it("renders the chain joined by a separator when a scope is present", () => {
    breadcrumbState = ["class Widget", "method render"];
    render(<CodeDiffReview before="a" after="b" path="x.ts" />);
    const bar = screen.getByTestId("code-diff-breadcrumb");
    expect(bar).toHaveTextContent("class Widget › method render");
  });

  it("renders nothing when there is no derivable scope (null)", () => {
    breadcrumbState = null;
    render(<CodeDiffReview before="a" after="b" path="x.ts" />);
    expect(screen.queryByTestId("code-diff-breadcrumb")).not.toBeInTheDocument();
  });

  it("renders nothing for an empty chain", () => {
    breadcrumbState = [];
    render(<CodeDiffReview before="a" after="b" path="x.ts" />);
    expect(screen.queryByTestId("code-diff-breadcrumb")).not.toBeInTheDocument();
  });
});
