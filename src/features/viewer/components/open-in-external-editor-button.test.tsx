// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

import { TooltipProvider } from "@/components/ui/tooltip";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { resetDetectedEditorsCache } from "@/shared/hooks/use-detected-editors";

// The whole action delegates to the existing, tested seam (SLW-WDE-5 §C / AC-2):
// it already guards the unconfigured case + `.catch`-toasts a failure. We assert
// the button calls it with the worktree cwd — nothing more.
const openInEditorFromCwdMock = vi.fn();
vi.mock("@/shared/lib/open-in-editor", () => ({
  openInEditorFromCwd: (cwd: string) => openInEditorFromCwdMock(cwd),
}));

// §B — the friendly label comes from the async detect-tools result, not settings.
const detectToolsMock = vi.fn();
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  detectTools: () => detectToolsMock(),
}));

import { OpenInExternalEditorButton } from "./open-in-external-editor-button";

function renderButton() {
  return render(
    <TooltipProvider>
      <OpenInExternalEditorButton cwd="/wt" wsId="w1" />
    </TooltipProvider>,
  );
}

beforeEach(() => {
  resetDetectedEditorsCache();
  openInEditorFromCwdMock.mockReset();
  detectToolsMock.mockReset();
  detectToolsMock.mockResolvedValue({
    terminals: [],
    editors: [{ id: "code", label: "VS Code" }],
    default_terminal: "",
    default_editor: "code",
  });
  useViewerStore.getState().updateSettings({ editor: "", editorCustomCmd: "" });
});

describe("OpenInExternalEditorButton (SLW-WDE-5)", () => {
  it("AC-2/§C: clicking delegates to openInEditorFromCwd(cwd)", () => {
    renderButton();
    fireEvent.click(screen.getByTestId("source-open-external-editor-w1"));
    expect(openInEditorFromCwdMock).toHaveBeenCalledWith("/wt");
  });

  it("§C/RD-4: renders even when NO editor is configured (guided toast lives in the seam)", () => {
    useViewerStore.getState().updateSettings({ editor: "", editorCustomCmd: "" });
    renderButton();
    const btn = screen.getByTestId("source-open-external-editor-w1");
    // generic, never bare "Open editor" (that's WDE-1's in-app control, §A)
    expect(btn).toHaveAttribute("aria-label", "Open in editor");
    fireEvent.click(btn);
    expect(openInEditorFromCwdMock).toHaveBeenCalledWith("/wt");
  });

  it("§A/§B: upgrades the label to the detected editor name once detection resolves", async () => {
    useViewerStore.getState().updateSettings({ editor: "code", editorCustomCmd: "" });
    renderButton();
    await waitFor(() =>
      expect(screen.getByTestId("source-open-external-editor-w1")).toHaveAttribute(
        "aria-label",
        "Open in VS Code",
      ),
    );
  });

  it("§B: uses the custom command as the label for editor:'custom'", async () => {
    useViewerStore.getState().updateSettings({ editor: "custom", editorCustomCmd: "nvim" });
    renderButton();
    await waitFor(() =>
      expect(screen.getByTestId("source-open-external-editor-w1")).toHaveAttribute(
        "aria-label",
        "Open in nvim",
      ),
    );
  });

  it("§A: is NOT labeled bare 'Open editor' (disambiguated from WDE-1's in-app control)", () => {
    useViewerStore.getState().updateSettings({ editor: "code", editorCustomCmd: "" });
    renderButton();
    expect(screen.queryByRole("button", { name: "Open editor" })).not.toBeInTheDocument();
  });
});
