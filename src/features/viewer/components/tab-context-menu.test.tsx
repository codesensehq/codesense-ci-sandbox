// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { TabContextMenu } from "./tab-context-menu";
import type { AgentLook } from "./agent-identity-menus";
import { BUILTIN_AVATARS } from "@/shared/hooks/use-avatar-packs";

const baseProps = {
  contextMenu: { x: 100, y: 50, tabId: "W-5" },
  onClose: vi.fn(),
  onHideTab: vi.fn(),
  onEndTab: vi.fn(),
  onEndOtherTabs: vi.fn(),
  onEndAllTabs: vi.fn(),
};

describe("TabContextMenu", () => {
  it("renders nothing when contextMenu is null", () => {
    const { container } = render(<TabContextMenu {...baseProps} contextMenu={null} />);
    expect(container.innerHTML).toBe("");
  });

  it("offers exactly the two close verbs — Hide, End, End Others, End All — and no Close", () => {
    render(<TabContextMenu {...baseProps} />);
    expect(screen.getByText("Hide")).toBeInTheDocument();
    expect(screen.getByText("End")).toBeInTheDocument();
    expect(screen.getByText("End Others")).toBeInTheDocument();
    expect(screen.getByText("End All")).toBeInTheDocument();
    expect(screen.queryByText(/Close|background|Minimize/)).not.toBeInTheDocument();
  });

  it.each([
    ["Hide", "onHideTab", ["W-5"]],
    ["End", "onEndTab", ["W-5"]],
    ["End Others", "onEndOtherTabs", ["W-5"]],
    ["End All", "onEndAllTabs", []],
  ] as const)("%s calls %s and closes the menu", (label, handler, args) => {
    const spy = vi.fn();
    const onClose = vi.fn();
    render(<TabContextMenu {...baseProps} onClose={onClose} {...{ [handler]: spy }} />);
    fireEvent.click(screen.getByText(label));
    expect(spy).toHaveBeenCalledWith(...args);
    expect(onClose).toHaveBeenCalled();
  });

  it("shows the tile's doc and the editor only when passed, and runs them", () => {
    const { unmount } = render(<TabContextMenu {...baseProps} />);
    expect(screen.queryByText(/Open .* Doc|Open in/)).not.toBeInTheDocument();
    unmount();

    const open = vi.fn();
    const onOpenInEditor = vi.fn();
    const onClose = vi.fn();
    render(
      <TabContextMenu
        {...baseProps}
        onClose={onClose}
        doc={{ label: "Open Story Doc", run: open }}
        editor={{ label: "Open in VS Code", run: onOpenInEditor }}
      />,
    );
    fireEvent.click(screen.getByText("Open Story Doc"));
    expect(open).toHaveBeenCalled();
    fireEvent.click(screen.getByText("Open in VS Code"));
    expect(onOpenInEditor).toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("offers Restart only when passed (an ended tab), with the tab id", () => {
    const { unmount } = render(<TabContextMenu {...baseProps} />);
    expect(screen.queryByText("Restart")).not.toBeInTheDocument();
    unmount();
    const onRestartTab = vi.fn();
    render(<TabContextMenu {...baseProps} onRestartTab={onRestartTab} />);
    fireEvent.click(screen.getByRole("menuitem", { name: "Restart" }));
    expect(onRestartTab).toHaveBeenCalledWith("W-5");
  });

  it("announces the Rename and New terminal shortcuts apart from the items' names", () => {
    render(
      <TabContextMenu
        {...baseProps}
        onRenameTab={vi.fn()}
        renameKeystroke={{ display: "F2", aria: "F2" }}
        onNewTab={vi.fn()}
        newTabKeystroke={{ display: "⌘T", aria: "Meta+T" }}
      />,
    );
    const rename = screen.getByRole("menuitem", { name: "Rename" });
    expect(rename).toHaveAttribute("aria-keyshortcuts", "F2");
    expect(rename).toHaveTextContent("F2");
    expect(screen.getByRole("menuitem", { name: "New terminal" })).toHaveAttribute(
      "aria-keyshortcuts",
      "Meta+T",
    );
  });

  it("puts End, End Others and End All apart from the rest, as the destructive group", () => {
    render(<TabContextMenu {...baseProps} onRestartTab={vi.fn()} />);
    const menu = screen.getByRole("menu");
    const order = [...menu.querySelectorAll('[role="menuitem"], [role="separator"]')].map((el) =>
      el.getAttribute("role") === "separator" ? "|" : el.textContent,
    );
    expect(order.slice(-6)).toEqual(["Restart", "Hide", "|", "End", "End Others", "End All"]);
  });

  it("renders no Exit / kill wording (End is the only kill verb)", () => {
    render(<TabContextMenu {...baseProps} />);
    expect(screen.queryByText(/Exit/)).not.toBeInTheDocument();
    expect(screen.queryByText(/kill terminals/i)).not.toBeInTheDocument();
  });

  // SW-EW-46 (D2): the Add to all tabs / Remove from all tabs verb.
  it("hides the pane verb when the tile cannot grid (not expanded)", () => {
    render(<TabContextMenu {...baseProps} canGrid={false} onPaneOut={vi.fn()} />);
    expect(screen.queryByText("Add to all tabs")).not.toBeInTheDocument();
    expect(screen.queryByText("Remove from all tabs")).not.toBeInTheDocument();
  });

  it("shows 'Add to all tabs' for a non-paned tab (≥2 tabs) and calls onPaneOut", () => {
    const onPaneOut = vi.fn();
    const onClose = vi.fn();
    render(
      <TabContextMenu
        {...baseProps}
        onClose={onClose}
        canGrid
        tabCount={2}
        panedTabIds={[]}
        onPaneOut={onPaneOut}
        onDemotePane={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByText("Add to all tabs"));
    expect(onPaneOut).toHaveBeenCalledWith("W-5");
    expect(onClose).toHaveBeenCalled();
  });

  it("hides 'Add to all tabs' when there is only one tab (paning out is meaningless)", () => {
    render(
      <TabContextMenu {...baseProps} canGrid tabCount={1} panedTabIds={[]} onPaneOut={vi.fn()} />,
    );
    expect(screen.queryByText("Add to all tabs")).not.toBeInTheDocument();
  });

  it("offers 'Remove from all tabs' for a lone paned cell regardless of tab count (no dead-end)", () => {
    const onDemotePane = vi.fn();
    render(
      <TabContextMenu
        {...baseProps}
        canGrid
        tabCount={1}
        panedTabIds={["W-5"]}
        onDemotePane={onDemotePane}
      />,
    );
    fireEvent.click(screen.getByText("Remove from all tabs"));
    expect(onDemotePane).toHaveBeenCalledWith("W-5");
  });

  it("shows 'Remove from all tabs' for a paned tab and calls onDemotePane", () => {
    const onDemotePane = vi.fn();
    render(
      <TabContextMenu
        {...baseProps}
        canGrid
        panedTabIds={["W-5"]}
        onPaneOut={vi.fn()}
        onDemotePane={onDemotePane}
      />,
    );
    expect(screen.queryByText("Add to all tabs")).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("Remove from all tabs"));
    expect(onDemotePane).toHaveBeenCalledWith("W-5");
  });

  // Rename + New terminal — present only when wired.
  it("hides Rename / New terminal when their handlers are not provided", () => {
    render(<TabContextMenu {...baseProps} />);
    expect(screen.queryByText("Rename")).not.toBeInTheDocument();
    expect(screen.queryByText("New terminal")).not.toBeInTheDocument();
  });

  it("shows 'Rename' and calls onRenameTab with the tab id, then closes", () => {
    const onRenameTab = vi.fn();
    const onClose = vi.fn();
    render(<TabContextMenu {...baseProps} onClose={onClose} onRenameTab={onRenameTab} />);
    fireEvent.click(screen.getByText("Rename"));
    expect(onRenameTab).toHaveBeenCalledWith("W-5");
    expect(onClose).toHaveBeenCalled();
  });

  it("shows 'New terminal' and calls onNewTab, then closes", () => {
    const onNewTab = vi.fn();
    const onClose = vi.fn();
    render(<TabContextMenu {...baseProps} onClose={onClose} onNewTab={onNewTab} />);
    fireEvent.click(screen.getByText("New terminal"));
    expect(onNewTab).toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });
});

describe("TabContextMenu — an agent tab's Role and Face", () => {
  const look = (over: Partial<AgentLook> = {}): AgentLook => ({
    choice: undefined,
    avatars: BUILTIN_AVATARS,
    onChoose: vi.fn(),
    ...over,
  });
  const item = (name: string) => screen.getByRole("menuitem", { name });
  const openByPointer = (name: string) =>
    fireEvent.pointerDown(item(name), { button: 0, pointerType: "mouse" });

  it("offers no Role or Face on a plain shell tab", () => {
    render(<TabContextMenu {...baseProps} />);
    expect(screen.queryByRole("menuitem", { name: "Role" })).toBeNull();
    expect(screen.queryByRole("menuitem", { name: "Face" })).toBeNull();
  });

  it("offers Role and Face as submenus on an agent tab", () => {
    render(<TabContextMenu {...baseProps} agentLook={look()} />);
    expect(item("Role")).toHaveAttribute("aria-haspopup", "menu");
    expect(item("Face")).toHaveAttribute("aria-haspopup", "menu");
    expect(item("Role")).toHaveAttribute("aria-expanded", "false");
  });

  it("sets the agent's role from Role and closes the menu", async () => {
    const onChoose = vi.fn();
    const onClose = vi.fn();
    render(<TabContextMenu {...baseProps} onClose={onClose} agentLook={look({ onChoose })} />);
    openByPointer("Role");
    fireEvent.click(await screen.findByRole("menuitemradio", { name: "Reviewer" }));
    expect(onChoose).toHaveBeenCalledWith({ role: "reviewer" });
    expect(onClose).toHaveBeenCalled();
  });

  it("sets a face from any pack, and Auto returns it", async () => {
    const onChoose = vi.fn();
    const { unmount } = render(<TabContextMenu {...baseProps} agentLook={look({ onChoose })} />);
    openByPointer("Face");
    fireEvent.click(await screen.findByRole("menuitem", { name: "Funny originals" }));
    fireEvent.click(await screen.findByRole("menuitemradio", { name: "Raccoon in a hard hat" }));
    expect(onChoose).toHaveBeenCalledWith({ face: "builtin-funny:hardhat-raccoon" });
    unmount();

    render(
      <TabContextMenu
        {...baseProps}
        agentLook={look({ onChoose, choice: { face: "builtin-funny:hardhat-raccoon" } })}
      />,
    );
    openByPointer("Face");
    fireEvent.click(await screen.findByRole("menuitemradio", { name: "Auto" }));
    expect(onChoose).toHaveBeenLastCalledWith({ face: null });
  });

  it("is keyboard operable: ↓ moves past a submenu item, → opens it, ← closes it", async () => {
    const onChoose = vi.fn();
    render(<TabContextMenu {...baseProps} agentLook={look({ onChoose })} />);
    const role = item("Role");
    const face = item("Face");
    role.focus();
    fireEvent.keyDown(role, { key: "ArrowDown" });
    expect(document.activeElement).toBe(face);
    expect(face).toHaveAttribute("aria-expanded", "false");

    fireEvent.keyDown(face, { key: "ArrowUp" });
    expect(document.activeElement).toBe(role);
    fireEvent.keyDown(role, { key: "ArrowRight" });
    const reviewer = await screen.findByRole("menuitemradio", { name: "Reviewer" });
    expect(role).toHaveAttribute("aria-expanded", "true");
    const submenu = reviewer.closest<HTMLElement>("[role=menu]")!;
    fireEvent.keyDown(submenu, { key: "ArrowLeft" });
    await waitFor(() => expect(role).toHaveAttribute("aria-expanded", "false"));
    expect(screen.queryByRole("menuitemradio", { name: "Reviewer" })).toBeNull();

    fireEvent.keyDown(role, { key: "Enter" });
    fireEvent.click(await screen.findByRole("menuitemradio", { name: "QA" }));
    expect(onChoose).toHaveBeenCalledWith({ role: "qa" });
  });
});

describe("SW-AGR-6: Remind this agent…", () => {
  it("is offered for an agent tab, after Face, and opens the Remind popover for that tab", () => {
    const onRemind = vi.fn();
    const onClose = vi.fn();
    const look: AgentLook = { choice: undefined, avatars: BUILTIN_AVATARS, onChoose: vi.fn() };
    render(
      <TabContextMenu {...baseProps} onClose={onClose} agentLook={look} onRemind={onRemind} />,
    );
    const items = screen.getAllByRole("menuitem").map((el) => el.textContent ?? "");
    const face = items.findIndex((t) => t.startsWith("Face"));
    expect(items[face + 1]).toBe("Remind this agent…");
    fireEvent.click(screen.getByTestId("tab-menu-remind"));
    expect(onRemind).toHaveBeenCalledWith("W-5");
    expect(onClose).toHaveBeenCalled();
  });

  it("is not offered for a plain shell tab or a headless run", () => {
    render(<TabContextMenu {...baseProps} />);
    expect(screen.queryByText("Remind this agent…")).toBeNull();
  });
});
