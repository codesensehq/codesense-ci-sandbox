// SPDX-License-Identifier: MIT
import { render, screen } from "@testing-library/react";
import { expect, it, vi } from "vitest";
vi.mock("./context-attachments", () => ({
  ContextAttachments: ({ initialCreate }: { initialCreate: boolean }) => (
    <div data-testid="shared-form">{initialCreate ? "create" : "choose"}</div>
  ),
}));
import { OrgPicker } from "./org-picker";
it.each(["home", "create", "join"] as const)(
  "preserves the %s entry intent in the shared form",
  (initialScreen) => {
    render(<OrgPicker canDismiss={false} initialScreen={initialScreen} />);
    expect(screen.getByTestId("shared-form")).toHaveTextContent(
      initialScreen === "create" ? "create" : "choose",
    );
  },
);
