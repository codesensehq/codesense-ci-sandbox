// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";

import { LaunchTargetPicker } from "./launch-target-picker";

const fields = { storyId: "", repo: "", branch: "", path: "" };

describe("LaunchTargetPicker", () => {
  it("the repo field's example names no real org", () => {
    render(
      <LaunchTargetPicker
        kind="repoBranch"
        fields={fields}
        onKindChange={vi.fn()}
        onFieldChange={vi.fn()}
        disabled={false}
      />,
    );
    expect(screen.getByTestId("launch-field-repo")).toHaveAttribute("placeholder", "acme/api");
  });
});
