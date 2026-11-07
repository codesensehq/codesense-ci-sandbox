// SPDX-License-Identifier: MIT
/**
 * SLW-DV-5 (AC-4) — the "Jump to latest" pill in `<CodeDiffReview>`. The pill is
 * gated on the merge-view hook's `live.paused`; here we drive that state directly
 * (a real scroll-up needs CM layout, which jsdom can't produce) to prove the
 * render gate and the click wiring, which the CM-backed suite can't reach.
 */
import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";

import { CodeDiffReview } from "./CodeDiffReview";
import { makeCodeMergeViewResult } from "./code-editor-test-utils";
import type { UseCodeMergeViewResult } from "./use-code-merge-view";

const jumpToLatest = vi.fn();
let liveState: UseCodeMergeViewResult["live"] = null;

vi.mock("./use-code-merge-view", () => ({
  useCodeMergeView: (): UseCodeMergeViewResult => makeCodeMergeViewResult({ live: liveState }),
}));

vi.mock("./use-merge-view-chrome", () => ({
  useMergeViewChrome: () => ({
    theme: "light",
    fontSize: 13,
    language: "plain",
    container: null,
    containerCallback: () => {},
    themeClass: "",
  }),
}));

describe("CodeDiffReview — jump-to-latest pill (SLW-DV-5 AC-4)", () => {
  it("renders the pill when live.paused and the click re-engages follow", () => {
    jumpToLatest.mockClear();
    liveState = { paused: true, jumpToLatest };
    render(<CodeDiffReview before="a" after="b" path="x.ts" autoFollow />);

    const pill = screen.getByTestId("code-diff-jump-to-latest");
    expect(pill).toBeInTheDocument();
    fireEvent.click(pill);
    expect(jumpToLatest).toHaveBeenCalledTimes(1);
  });

  it("hides the pill when not paused", () => {
    liveState = { paused: false, jumpToLatest };
    render(<CodeDiffReview before="a" after="b" path="x.ts" autoFollow />);
    expect(screen.queryByTestId("code-diff-jump-to-latest")).not.toBeInTheDocument();
  });

  it("hides the pill outside review live-mode (live === null)", () => {
    liveState = null;
    render(<CodeDiffReview before="a" after="b" path="x.ts" />);
    expect(screen.queryByTestId("code-diff-jump-to-latest")).not.toBeInTheDocument();
  });
});
