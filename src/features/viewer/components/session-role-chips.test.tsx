// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import userEvent from "@testing-library/user-event";
import { render, screen } from "@/test-utils";
import { openRoleEditor } from "@/features/agents/stores/role-editor-store";
import { SessionRoleChips } from "./session-role-chips";

vi.mock("@/features/agents/stores/role-editor-store", () => ({ openRoleEditor: vi.fn() }));
vi.mock("@/shared/components/queue-role", () => ({
  QueueRole: ({ role }: { role: string }) => <span>{role}</span>,
}));
beforeEach(() => vi.clearAllMocks());

it.each(["{Enter}", " "])(
  "opens the role editor from the keyboard with %s without activating the agent row",
  async (key) => {
    const reveal = vi.fn();
    const user = userEvent.setup();
    render(
      <div onClick={reveal} onKeyDown={reveal}>
        <button type="button">Show agent</button>
        <SessionRoleChips
          holds={[
            {
              role: "reviewer",
              label: "Reviewer",
              teamKey: "acme:backend",
              instanceId: "session-a",
              generation: 1,
            },
          ]}
        />
      </div>,
    );
    const chip = screen.getByRole("button", { name: "reviewer" });
    chip.focus();
    await user.keyboard(key);
    expect(openRoleEditor).toHaveBeenCalledWith("acme:backend", "reviewer");
    expect(reveal).not.toHaveBeenCalled();
  },
);
