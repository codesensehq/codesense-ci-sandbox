// SPDX-License-Identifier: MIT
import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TeamAgentIdentity } from "@/features/viewer/hooks/use-team-agent-identities";
import type { agentStatusLook } from "../lib/note-agents";
import { SendPicker, type SendPickerProps } from "./send-picker";

let agents: TeamAgentIdentity[] = [];
vi.mock("../lib/note-agents", () => ({
  useNoteAgents: () => ({ agents, statusOf: () => "running", subagentsOf: () => 0 }),
  agentStatusLook: (): ReturnType<typeof agentStatusLook> => ({
    word: "running",
    mark: "thinking",
  }),
}));
vi.mock("@/features/viewer/components/agent-identity", () => ({ AgentFaceView: () => null }));

function agent(handle: string): TeamAgentIdentity {
  return { label: handle, row: { handle, sessionId: handle, alive: true } } as TeamAgentIdentity;
}
function picker() {
  const props: SendPickerProps = {
    open: true,
    anchor: { top: 0, left: 0, width: 1, height: 1 },
    placeLabel: "Team",
    includeContext: false,
    onIncludeContextChange: vi.fn(),
    onSendToAgent: vi.fn(),
    onStartNew: vi.fn(),
    onPasteTerminal: vi.fn(),
    onClose: vi.fn(),
  };
  return { props, ...render(<SendPicker {...props} />) };
}
beforeEach(() => {
  agents = [agent("Alpha"), agent("Beta"), agent("Gamma")];
});

describe("SendPicker deliberate selection", () => {
  it("sends to the chosen agent after an agent above it ends", () => {
    const { props, rerender } = picker();
    const input = screen.getByRole("textbox", { name: "Send to" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    const chosen = agents[1];
    agents = agents.slice(1);
    rerender(<SendPicker {...props} />);
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onSendToAgent).toHaveBeenCalledWith(chosen);
  });

  it("ignores a stationary mouse enter and follows mouse movement", () => {
    const { props } = picker();
    const gamma = screen.getByRole("option", { name: /Gamma/ });
    fireEvent.mouseEnter(gamma);
    expect(screen.getByRole("option", { name: /Alpha/ })).toHaveAttribute("aria-selected", "true");
    fireEvent.mouseMove(gamma);
    fireEvent.keyDown(screen.getByRole("textbox", { name: "Send to" }), { key: "Enter" });
    expect(props.onSendToAgent).toHaveBeenCalledWith(agents[2]);
  });

  it("keeps fallback actions selected across roster changes and resets on typing", () => {
    const { props, rerender } = picker();
    const input = screen.getByRole("textbox", { name: "Send to" });
    for (let i = 0; i < 3; i++) fireEvent.keyDown(input, { key: "ArrowDown" });
    agents = agents.slice(1);
    rerender(<SendPicker {...props} />);
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onStartNew).toHaveBeenCalledOnce();
    fireEvent.change(input, { target: { value: "Gamma" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onSendToAgent).toHaveBeenCalledWith(agents[1]);
  });

  it("reveals the first agent after a chosen lower agent ends in a scrolled list", () => {
    agents = Array.from({ length: 10 }, (_, i) => agent(`Agent ${i}`));
    const { props, rerender } = picker();
    const options = screen.getAllByRole("option");
    screen.getByRole("listbox", { name: "Agents" }).scrollTop = 150;
    fireEvent.mouseMove(options[9]);
    const scrollFirst = vi.spyOn(options[0], "scrollIntoView");
    scrollFirst.mockClear();
    agents = agents.slice(0, 9);
    rerender(<SendPicker {...props} />);
    expect(options[0]).toHaveAttribute("aria-selected", "true");
    expect(scrollFirst).toHaveBeenCalledWith({ block: "nearest" });
    scrollFirst.mockRestore();
    fireEvent.keyDown(screen.getByRole("textbox", { name: "Send to" }), { key: "Enter" });
    expect(props.onSendToAgent).toHaveBeenCalledWith(agents[0]);
  });

  it("falls back to the first row when the chosen agent disappears", () => {
    const { props, rerender } = picker();
    const input = screen.getByRole("textbox", { name: "Send to" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    agents = [agents[0], agents[2]];
    rerender(<SendPicker {...props} />);
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onSendToAgent).toHaveBeenCalledWith(agents[0]);
  });
});
