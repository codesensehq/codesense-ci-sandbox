// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { render, screen } from "@/test-utils";
import { PREVIEW_RUNTIME_NOTE, REGISTERED_AGENT_CLIS } from "@/shared/lib/agent-runtimes";

import { PreviewBadge, PreviewNote, previewNoteRef } from "./preview-badge";

describe.each(REGISTERED_AGENT_CLIS)("PreviewBadge for $runtime", ({ runtime, preview }) => {
  it("shows the label exactly when the registry offers the CLI as a preview", () => {
    render(<PreviewBadge cli={runtime} />);
    expect(screen.queryByText("Preview") !== null).toBe(preview);
  });

  it("inside a control is a plain pill; the control points at the note", () => {
    render(
      <button type="button" aria-describedby={previewNoteRef("note", runtime)}>
        {runtime} <PreviewBadge cli={runtime} />
        <PreviewNote id="note" cli={runtime} />
      </button>,
    );
    const control = screen.getByRole("button");
    if (preview) {
      expect(screen.getByTestId("preview-badge").tagName).toBe("SPAN");
      expect(control).toHaveAccessibleName(`${runtime} Preview`);
      expect(control).toHaveAccessibleDescription(PREVIEW_RUNTIME_NOTE);
    } else {
      expect(control).toHaveAccessibleName(runtime);
      expect(control).not.toHaveAttribute("aria-describedby");
    }
  });

  it("standing alone is a button named by its word and described by its note", () => {
    render(<PreviewBadge cli={runtime} standalone />);
    const badge = screen.queryByRole("button", { name: "Preview" });
    expect(badge !== null).toBe(preview);
    if (badge) expect(badge).toHaveAccessibleDescription(PREVIEW_RUNTIME_NOTE);
  });
});
