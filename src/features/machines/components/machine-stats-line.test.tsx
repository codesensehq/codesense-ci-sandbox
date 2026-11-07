// SPDX-License-Identifier: MIT
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { MachineSummary } from "@/tauri/commands";
import { MachineStatsLine } from "./machine-stats-line";

const machine = {
  machineId: "local",
  label: "This Mac",
  kind: "local",
  status: { state: "connected" },
} as MachineSummary;
const sample = {
  load1: 3.04,
  cpuCount: 8,
  memUsedBytes: 12.1e9,
  memTotalBytes: 16e9,
  diskFreeBytes: 84e9,
  diskTotalBytes: 460e9,
  diskPath: "/workspaces",
  sampledAt: Date.now(),
};

describe("Machine resource line", () => {
  it("shows self-reported values and the workspace path in the title", () => {
    render(<MachineStatsLine machine={{ ...machine, ...sample }} />);
    const line = screen.getByText(/Load 38% of 8 cores/);
    expect(line).toHaveTextContent("Memory 12.1 / 16 GB · 84 GB free of 460 GB · reported");
    expect(line.title).toMatch(/^Reported by This Mac at .* \(\/workspaces\)$/);
    expect(line).toHaveClass("text-[11px]", "text-muted-foreground", "tabular-nums");
  });
  it("does not invent a sample when fields have not arrived", () => {
    const { container } = render(<MachineStatsLine machine={machine} />);
    expect(container).toBeEmptyDOMElement();
  });
  it("names the terminal server update for an unsupported server", () => {
    render(<MachineStatsLine machine={{ ...machine, machineStatsSupported: false }} />);
    expect(
      screen.getByText("Load, memory and disk unknown · update the terminal server"),
    ).toBeVisible();
  });
  it("hides cached fields on a disconnected machine", () => {
    const { container } = render(
      <MachineStatsLine machine={{ ...machine, ...sample, status: { state: "disconnected" } }} />,
    );
    expect(container).toBeEmptyDOMElement();
  });
});
