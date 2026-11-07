// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render } from "@testing-library/react";
const watchers = vi.hoisted(() => ({
  inputs: vi.fn(),
  owners: vi.fn(),
  facts: vi.fn(),
  stopInputs: vi.fn(),
  stopOwners: vi.fn(),
}));
vi.mock("../lib/agent-list-inputs", () => ({ watchAgentListInputs: watchers.inputs }));
vi.mock("../lib/agent-list-projection", () => ({ watchAgentList: watchers.owners }));
vi.mock("./use-agent-list-facts", () => ({ useAgentListFactsMirror: watchers.facts }));
import { useAgentRosterListener } from "./use-agent-roster-listener";
function Root() {
  useAgentRosterListener();
  return null;
}
describe("agent projection app-root lifecycle", () => {
  it("owns both subscriptions and releases both when unmounted", () => {
    watchers.inputs.mockReturnValue(watchers.stopInputs);
    watchers.owners.mockReturnValue(watchers.stopOwners);
    const root = render(<Root />);
    expect(watchers.inputs).toHaveBeenCalledTimes(1);
    expect(watchers.owners).toHaveBeenCalledTimes(1);
    expect(watchers.facts).toHaveBeenCalledTimes(1);
    root.unmount();
    expect(watchers.stopInputs).toHaveBeenCalledTimes(1);
    expect(watchers.stopOwners).toHaveBeenCalledTimes(1);
  });
});
