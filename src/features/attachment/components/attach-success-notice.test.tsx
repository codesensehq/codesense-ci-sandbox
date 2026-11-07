// SPDX-License-Identifier: MIT
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { CONTEXT_LANDS_ATTACHED, CONTEXT_LANDS_MANAGED } from "@/shared/lib/onboarding-copy";

import { AttachSuccessNotice } from "./attach-success-notice";

describe("AttachSuccessNotice", () => {
  it("says the repo is attached and where an attached working copy's context lands", () => {
    render(<AttachSuccessNotice label="me/app" managed={false} />);
    expect(screen.getByRole("status")).toHaveTextContent("Attached me/app");
    expect(screen.getByTestId("attach-success")).toHaveTextContent(CONTEXT_LANDS_ATTACHED);
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("uses the managed copy for a clone CodeSense made", () => {
    render(<AttachSuccessNotice label="me/app" managed />);
    expect(screen.getByTestId("attach-success")).toHaveTextContent(CONTEXT_LANDS_MANAGED);
  });
});
