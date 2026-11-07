// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@/test-utils";
import type { MachineSummary } from "@/tauri/commands";
import { useRef, useState } from "react";
import {
  MachineAppearanceButton,
  useMachineAppearanceOpen,
  useSettleAppearanceRequest,
} from "./machine-appearance-popover";
import {
  __resetMachineAppearanceForTest,
  requestMachineAppearance,
  restoreMachineAppearance,
  setMachineImage,
  useMachineAppearanceStore,
} from "../lib/machine-appearance";
import { CUSTOM_LIGHTNESS, defaultMachineColor, oklchLightness } from "../lib/machine-color";

const BOX: MachineSummary = {
  machineId: "box-1",
  kind: "ssh",
  label: "codesense-test-1",
  scope: { kind: "personal" },
  declaration: null,
  binding: null,
  canPublish: true,
  status: { state: "connected", workspaces: null },
  attention: 0,
  capabilities: { agentCallbacks: true },
};
const prefs = () => useMachineAppearanceStore.getState().prefs["box-1"];

beforeEach(async () => {
  localStorage.clear();
  const data = new Map<string, string>();
  __resetMachineAppearanceForTest({
    get: async (k) => data.get(k),
    put: async (k, v) => void data.set(k, v),
    delete: async (k) => void data.delete(k),
  });
  await restoreMachineAppearance("alice");
});
afterEach(() => __resetMachineAppearanceForTest());

function Harness() {
  const [open, setOpen] = useState(false);
  return <MachineAppearanceButton machine={BOX} open={open} onOpenChange={setOpen} />;
}

function Row({ ids = ["box-1"] }: { ids?: string[] | null }) {
  const ref = useRef<HTMLDivElement>(null);
  useSettleAppearanceRequest(ids);
  const [open, setOpen] = useMachineAppearanceOpen("box-1", ref);
  return (
    <div ref={ref} data-testid="row">
      <MachineAppearanceButton machine={BOX} open={open} onOpenChange={setOpen} />
    </div>
  );
}

function open() {
  render(<Harness />);
  fireEvent.click(screen.getByRole("button", { name: "Appearance for codesense-test-1" }));
}

describe("MachineAppearanceButton", () => {
  it("picks a palette color by name and returns to the default", () => {
    open();
    const pick = defaultMachineColor("box-1") === "#f778ba" ? "Teal" : "Pink";
    fireEvent.click(screen.getByRole("button", { name: pick }));
    expect(prefs()?.color).toMatch(/^#[0-9a-f]{6}$/);
    expect(screen.getByRole("button", { name: pick }).getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: "Use default" }));
    expect(prefs()).toBeUndefined();
  });

  it("previews a custom pick while dragging and saves it only when the picker commits", () => {
    open();
    const picker = screen.getByLabelText("Custom color") as HTMLInputElement;
    fireEvent.input(picker, { target: { value: "#000010" } });
    // Dragging: previewed here, the machine's terminals untouched.
    expect(prefs()).toBeUndefined();
    expect(picker.value).toBe("#000010");
    fireEvent.change(picker, { target: { value: "#000010" } });
    const l = oklchLightness(prefs()!.color!);
    expect(l).toBeGreaterThanOrEqual(CUSTOM_LIGHTNESS.min - 0.02);
  });

  it("keeps a pick still in progress when the popover closes first (macOS color panel)", () => {
    open();
    fireEvent.input(screen.getByLabelText("Custom color"), { target: { value: "#f778ba" } });
    expect(prefs()).toBeUndefined();
    fireEvent.keyDown(screen.getByTestId("machine-appearance-box-1"), { key: "Escape" });
    expect(prefs()?.color).toBe("#f778ba");
  });

  it("closing with no pick in progress changes nothing", () => {
    open();
    fireEvent.keyDown(screen.getByTestId("machine-appearance-box-1"), { key: "Escape" });
    expect(prefs()).toBeUndefined();
  });

  it("scrolls the asked-for row into view before opening it", async () => {
    const original = Element.prototype.scrollIntoView;
    const scroll = vi.fn();
    Element.prototype.scrollIntoView = scroll;
    try {
      render(<Row />);
      act(() => requestMachineAppearance("box-1"));
      expect(scroll).toHaveBeenCalledWith({ block: "nearest" });
      expect(scroll.mock.contexts[0]).toBe(screen.getByTestId("row"));
      expect(await screen.findByTestId("machine-appearance-box-1")).toBeInTheDocument();
    } finally {
      Element.prototype.scrollIntoView = original;
    }
  });

  it("drops an ask for a machine the loaded list doesn't have — no surprise popover later", () => {
    render(<Row ids={["other"]} />);
    act(() => requestMachineAppearance("box-1"));
    expect(useMachineAppearanceStore.getState().requested).toBeNull();
    expect(screen.queryByTestId("machine-appearance-box-1")).not.toBeInTheDocument();
  });

  it("drops the ask when the Machines list goes away", () => {
    const view = render(<Row ids={null} />);
    act(() => requestMachineAppearance("box-1"));
    view.unmount();
    expect(useMachineAppearanceStore.getState().requested).toBeNull();
  });

  it("opens when a remote tab's menu asks for this machine", async () => {
    render(<Row />);
    expect(screen.queryByTestId("machine-appearance-box-1")).not.toBeInTheDocument();
    act(() => requestMachineAppearance("box-1"));
    expect(await screen.findByTestId("machine-appearance-box-1")).toBeInTheDocument();
    fireEvent.keyDown(screen.getByTestId("machine-appearance-box-1"), { key: "Escape" });
    expect(useMachineAppearanceStore.getState().requested).toBeNull();
  });

  it("gives the dot a 24px target", () => {
    render(<Harness />);
    expect(
      screen.getByRole("button", { name: "Appearance for codesense-test-1" }).className,
    ).toContain("size-6");
  });

  it("shows the image's strength once one is set, and removes it", async () => {
    open();
    expect(screen.queryByLabelText("Strength")).not.toBeInTheDocument();
    await act(() => setMachineImage("box-1", "data:image/webp;base64,UklGRg=="));
    const slider = screen.getByLabelText("Strength");
    fireEvent.change(slider, { target: { value: "15" } });
    expect(prefs()?.imageOpacity).toBeCloseTo(0.15);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    });
    expect(useMachineAppearanceStore.getState().images["box-1"]).toBeUndefined();
    expect(screen.queryByLabelText("Strength")).not.toBeInTheDocument();
  });

  it("previews the machine's terminal with its watermark", () => {
    open();
    expect(screen.getByTestId("machine-appearance-preview-box-1")).toBeInTheDocument();
    expect(screen.getByTestId("machine-watermark-box-1").textContent).toBe("codesense-test-1");
  });
});
