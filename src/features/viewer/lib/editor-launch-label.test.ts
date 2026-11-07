// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { editorLaunchLabel } from "./editor-launch-label";

// SLW-WDE-5 §B — the friendly editor label is NOT in `settings` (which holds the
// id, e.g. "code"); it lives in the async detect-tools result. This pure helper
// derives the control label, degrading to a generic string until detection
// resolves or when the editor is unknown — never inventing a name settings can't
// back, never blocking.
describe("editorLaunchLabel (SLW-WDE-5 §B)", () => {
  const DETECTED = [
    { id: "code", label: "VS Code" },
    { id: "cursor", label: "Cursor" },
  ];

  it("uses the DETECTED friendly label when the configured id matches", () => {
    expect(editorLaunchLabel("code", "", DETECTED)).toBe("Open in VS Code");
    expect(editorLaunchLabel("cursor", "", DETECTED)).toBe("Open in Cursor");
  });

  it("uses the custom command string for editor:'custom'", () => {
    expect(editorLaunchLabel("custom", "nvim", DETECTED)).toBe("Open in nvim");
  });

  it("falls back to generic for a blank custom command", () => {
    expect(editorLaunchLabel("custom", "   ", DETECTED)).toBe("Open in editor");
    expect(editorLaunchLabel("custom", "", DETECTED)).toBe("Open in editor");
  });

  it("falls back to generic when no editor is configured", () => {
    expect(editorLaunchLabel("", "", DETECTED)).toBe("Open in editor");
  });

  it("falls back to generic when the id isn't (yet) in the detected list", () => {
    // detection hasn't resolved, or the editor is no longer installed
    expect(editorLaunchLabel("code", "", [])).toBe("Open in editor");
    expect(editorLaunchLabel("zed", "", DETECTED)).toBe("Open in editor");
  });
});
