// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, cleanup } from "@testing-library/react";
import { useEffect, useRef } from "react";
import { ROW_POPUP_ATTR } from "@/shared/lib/focus-utils";
import { useWorkspaceCycleIntercept } from "./use-workspace-cycle-intercept";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

// The intercept delegates EVERY recognized chord to the registry action's run(),
// so the command behavior lives in exactly one place. The test mocks the registry
// with per-action spies and asserts the capture-phase intercept fires the right
// run() (and the pty mock never sees the keystroke) — without pulling in the real
// actions' store/toast dependencies.
function shortcutFor(id: string): string {
  const map: Record<string, string> = {
    "nav.context-back": "CmdOrCtrl+[",
    "nav.context-forward": "CmdOrCtrl+]",
    "viewer.cycle-workspace-next": "CmdOrCtrl+Alt+]",
    "viewer.cycle-workspace-prev": "CmdOrCtrl+Alt+[",
    "viewer.cycle-epic-next": "CmdOrCtrl+Alt+Shift+]",
    "viewer.cycle-epic-prev": "CmdOrCtrl+Alt+Shift+[",
    "viewer.cycle-terminal-tab-next": "CmdOrCtrl+Shift+]",
    "viewer.cycle-terminal-tab-prev": "CmdOrCtrl+Shift+[",
    "viewer.flip-last-workspace": "CmdOrCtrl+Alt+;",
    "viewer.jump-workspace-1": "CmdOrCtrl+1",
    "viewer.focus-pane-left": "CmdOrCtrl+Alt+ArrowLeft",
    "viewer.focus-pane-right": "CmdOrCtrl+Alt+ArrowRight",
    "viewer.focus-pane-up": "CmdOrCtrl+Alt+ArrowUp",
    "viewer.focus-pane-down": "CmdOrCtrl+Alt+ArrowDown",
    "viewer.hard-close-workspace": "CmdOrCtrl+Shift+Backspace",
  };
  return map[id] ?? "CmdOrCtrl+Alt+unused";
}
vi.mock("@/shared/stores/shortcuts-store", () => ({
  useEffectiveShortcut: (id: string) => shortcutFor(id),
  getEffectiveShortcut: (id: string) => shortcutFor(id),
}));

// One spy per intercepted action id; SHORTCUT_ACTIONS is the mocked registry.
// Hoisted so the vi.mock factory (also hoisted) can reference it safely.
const { INTERCEPTED_IDS, runFor } = vi.hoisted(() => {
  const ids = [
    "nav.context-back",
    "nav.context-forward",
    "viewer.cycle-workspace-next",
    "viewer.cycle-workspace-prev",
    "viewer.cycle-epic-next",
    "viewer.cycle-epic-prev",
    "viewer.cycle-terminal-tab-next",
    "viewer.cycle-terminal-tab-prev",
    "viewer.flip-last-workspace",
    ...Array.from({ length: 9 }, (_, i) => `viewer.jump-workspace-${i + 1}`),
    "viewer.focus-pane-left",
    "viewer.focus-pane-right",
    "viewer.focus-pane-up",
    "viewer.focus-pane-down",
    "viewer.hard-close-workspace",
  ];
  const runs: Record<string, ReturnType<typeof vi.fn<() => void>>> = {};
  return { INTERCEPTED_IDS: ids, runFor: (id: string) => (runs[id] ??= vi.fn<() => void>()) };
});
vi.mock("@/shared/lib/shortcuts/registry", () => ({
  SHORTCUT_ACTIONS: INTERCEPTED_IDS.map((id) => ({
    id,
    scope: id.startsWith("nav.") ? "global" : "workspace",
    run: () => runFor(id)(),
  })),
}));

/**
 * Harness renders a terminal host element (with data-xterm-host) containing
 * a pty-mock child. The pty-mock has a native non-capture keydown listener
 * to regression-guard AC-20: if the capture-phase intercept works, the
 * pty-mock should NOT receive the intercepted keystrokes.
 */
function Harness({
  ptyKey,
  repoPath = "/repo",
}: {
  ptyKey: (e: KeyboardEvent) => void;
  repoPath?: string;
}) {
  const ptyRef = useRef<HTMLDivElement>(null);
  useWorkspaceCycleIntercept(repoPath);
  useEffect(() => {
    const node = ptyRef.current;
    if (!node) return;
    const handler = (e: KeyboardEvent) => ptyKey(e);
    node.addEventListener("keydown", handler);
    return () => node.removeEventListener("keydown", handler);
  }, [ptyKey]);
  return (
    <div data-xterm-host data-repo-path={repoPath} data-testid="terminal-area">
      <div ref={ptyRef} data-testid="pty-mock" tabIndex={-1}>
        pty
      </div>
    </div>
  );
}

/** Harness WITHOUT data-xterm-host — keystrokes should pass through */
function HarnessNoHost({ ptyKey }: { ptyKey: (e: KeyboardEvent) => void }) {
  const ptyRef = useRef<HTMLDivElement>(null);
  useWorkspaceCycleIntercept("/repo");
  useEffect(() => {
    const node = ptyRef.current;
    if (!node) return;
    const handler = (e: KeyboardEvent) => ptyKey(e);
    node.addEventListener("keydown", handler);
    return () => node.removeEventListener("keydown", handler);
  }, [ptyKey]);
  return (
    <div data-testid="non-terminal-area">
      <div ref={ptyRef} data-testid="pty-mock" tabIndex={-1}>
        pty
      </div>
    </div>
  );
}

function press(pty: Element, init: KeyboardEventInit): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
  pty.dispatchEvent(event);
  return event;
}

beforeEach(() => {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "team" });
  useUiStore.setState({ settingsPanelOpen: false });
  useViewerStore.setState({
    workspaces: {
      "acme:team": [{ id: "focused", kind: "generic", cwd: "/repo", status: "ready" }],
    },
    activeWorkspace: { "acme:team": "focused" },
  });
  for (const id of INTERCEPTED_IDS) runFor(id).mockReset();
});

afterEach(() => {
  cleanup();
});

describe("useWorkspaceCycleIntercept — every intercepted chord delegates to the registry run()", () => {
  it("Cmd+Alt+] → cycle-workspace-next run(), pty never sees it", () => {
    const ptyKey = vi.fn();
    const { getByTestId } = render(<Harness ptyKey={ptyKey} />);
    const event = press(getByTestId("pty-mock"), { key: "]", metaKey: true, altKey: true });
    expect(runFor("viewer.cycle-workspace-next")).toHaveBeenCalledTimes(1);
    expect(runFor("viewer.cycle-workspace-prev")).not.toHaveBeenCalled();
    expect(ptyKey).not.toHaveBeenCalled();
    expect(event.defaultPrevented).toBe(true);
  });

  it("Cmd+Alt+[ → cycle-workspace-prev run()", () => {
    const ptyKey = vi.fn();
    const { getByTestId } = render(<Harness ptyKey={ptyKey} />);
    press(getByTestId("pty-mock"), { key: "[", metaKey: true, altKey: true });
    expect(runFor("viewer.cycle-workspace-prev")).toHaveBeenCalledTimes(1);
    expect(ptyKey).not.toHaveBeenCalled();
  });

  it("Cmd+Alt+Shift+] → cycle-epic-next run() (not workspace-cycle — Shift disambiguates)", () => {
    const ptyKey = vi.fn();
    const { getByTestId } = render(<Harness ptyKey={ptyKey} />);
    const event = press(getByTestId("pty-mock"), {
      key: "]",
      metaKey: true,
      altKey: true,
      shiftKey: true,
    });
    expect(runFor("viewer.cycle-epic-next")).toHaveBeenCalledTimes(1);
    expect(runFor("viewer.cycle-workspace-next")).not.toHaveBeenCalled();
    expect(ptyKey).not.toHaveBeenCalled();
    expect(event.defaultPrevented).toBe(true);
  });

  it("Cmd+Shift+] → cycle-terminal-tab-next run() (not epic — no Alt)", () => {
    const ptyKey = vi.fn();
    const { getByTestId } = render(<Harness ptyKey={ptyKey} />);
    const event = press(getByTestId("pty-mock"), { key: "]", metaKey: true, shiftKey: true });
    expect(runFor("viewer.cycle-terminal-tab-next")).toHaveBeenCalledTimes(1);
    expect(runFor("viewer.cycle-epic-next")).not.toHaveBeenCalled();
    expect(runFor("viewer.cycle-workspace-next")).not.toHaveBeenCalled();
    expect(ptyKey).not.toHaveBeenCalled();
    expect(event.defaultPrevented).toBe(true);
  });

  it("Cmd+] and Cmd+[ → context forward and back, the pty never sees them", () => {
    const ptyKey = vi.fn();
    const { getByTestId } = render(<Harness ptyKey={ptyKey} />);
    const forward = press(getByTestId("pty-mock"), {
      key: "]",
      code: "BracketRight",
      metaKey: true,
    });
    press(getByTestId("pty-mock"), { key: "[", code: "BracketLeft", metaKey: true });
    expect(runFor("nav.context-forward")).toHaveBeenCalledTimes(1);
    expect(runFor("nav.context-back")).toHaveBeenCalledTimes(1);
    expect(runFor("viewer.cycle-terminal-tab-next")).not.toHaveBeenCalled();
    expect(ptyKey).not.toHaveBeenCalled();
    expect(forward.defaultPrevented).toBe(true);
    // With Shift the same keys cycle tabs, not the context.
    press(getByTestId("pty-mock"), {
      key: "}",
      code: "BracketRight",
      metaKey: true,
      shiftKey: true,
    });
    expect(runFor("nav.context-forward")).toHaveBeenCalledTimes(1);
    expect(runFor("viewer.cycle-terminal-tab-next")).toHaveBeenCalledTimes(1);
  });

  it("Cmd+Shift+[ → cycle-terminal-tab-prev run()", () => {
    const ptyKey = vi.fn();
    const { getByTestId } = render(<Harness ptyKey={ptyKey} />);
    press(getByTestId("pty-mock"), { key: "[", metaKey: true, shiftKey: true });
    expect(runFor("viewer.cycle-terminal-tab-prev")).toHaveBeenCalledTimes(1);
    expect(runFor("viewer.cycle-epic-prev")).not.toHaveBeenCalled();
  });

  it("Cmd+Alt+; → flip-last-workspace run()", () => {
    const ptyKey = vi.fn();
    const { getByTestId } = render(<Harness ptyKey={ptyKey} />);
    press(getByTestId("pty-mock"), { key: ";", metaKey: true, altKey: true });
    expect(runFor("viewer.flip-last-workspace")).toHaveBeenCalledTimes(1);
    expect(ptyKey).not.toHaveBeenCalled();
  });

  it("Cmd+1 → jump-workspace-1 run()", () => {
    const ptyKey = vi.fn();
    const { getByTestId } = render(<Harness ptyKey={ptyKey} />);
    press(getByTestId("pty-mock"), { key: "1", metaKey: true });
    expect(runFor("viewer.jump-workspace-1")).toHaveBeenCalledTimes(1);
    expect(ptyKey).not.toHaveBeenCalled();
  });

  it("Cmd+Opt+ArrowLeft → focus-pane-left run(), xterm never sees the arrow", () => {
    const ptyKey = vi.fn();
    const { getByTestId } = render(<Harness ptyKey={ptyKey} />);
    const event = press(getByTestId("pty-mock"), { key: "ArrowLeft", metaKey: true, altKey: true });
    expect(runFor("viewer.focus-pane-left")).toHaveBeenCalledTimes(1);
    expect(ptyKey).not.toHaveBeenCalled();
    expect(event.defaultPrevented).toBe(true);
  });

  it("Cmd+Shift+Backspace → hard-close run(), xterm never sees Backspace", () => {
    const ptyKey = vi.fn();
    const { getByTestId } = render(<Harness ptyKey={ptyKey} />);
    const event = press(getByTestId("pty-mock"), {
      key: "Backspace",
      metaKey: true,
      shiftKey: true,
    });
    expect(runFor("viewer.hard-close-workspace")).toHaveBeenCalledTimes(1);
    expect(ptyKey).not.toHaveBeenCalled();
    expect(event.defaultPrevented).toBe(true);
  });

  it("does NOT intercept Cmd+Shift+Backspace outside [data-xterm-host] (bubble handles it)", () => {
    const ptyKey = vi.fn();
    const { getByTestId } = render(<HarnessNoHost ptyKey={ptyKey} />);
    press(getByTestId("pty-mock"), { key: "Backspace", metaKey: true, shiftKey: true });
    expect(runFor("viewer.hard-close-workspace")).not.toHaveBeenCalled();
    expect(ptyKey).toHaveBeenCalledTimes(1);
  });

  it("does NOT intercept unrelated keystrokes — pty mock receives them normally", () => {
    const ptyKey = vi.fn();
    const { getByTestId } = render(<Harness ptyKey={ptyKey} />);
    press(getByTestId("pty-mock"), { key: "a" });
    expect(runFor("viewer.cycle-workspace-next")).not.toHaveBeenCalled();
    expect(ptyKey).toHaveBeenCalledTimes(1);
  });

  it("does NOT intercept cycle keystrokes outside [data-xterm-host] — falls through to bubble", () => {
    const ptyKey = vi.fn();
    const { getByTestId } = render(<HarnessNoHost ptyKey={ptyKey} />);
    press(getByTestId("pty-mock"), { key: "]", metaKey: true, altKey: true });
    expect(runFor("viewer.cycle-workspace-next")).not.toHaveBeenCalled();
    expect(ptyKey).toHaveBeenCalledTimes(1);
  });

  it("does NOT intercept inside [data-xterm-host] with a different data-repo-path", () => {
    const ptyKey = vi.fn();
    function HarnessWrongRepo({ ptyKey: pk }: { ptyKey: (e: KeyboardEvent) => void }) {
      const ptyRef = useRef<HTMLDivElement>(null);
      useWorkspaceCycleIntercept("/repo");
      useEffect(() => {
        const node = ptyRef.current;
        if (!node) return;
        const handler = (e: KeyboardEvent) => pk(e);
        node.addEventListener("keydown", handler);
        return () => node.removeEventListener("keydown", handler);
      }, [pk]);
      return (
        <div data-xterm-host data-repo-path="/other-repo" data-testid="terminal-area">
          <div ref={ptyRef} data-testid="pty-mock" tabIndex={-1}>
            pty
          </div>
        </div>
      );
    }
    const { getByTestId } = render(<HarnessWrongRepo ptyKey={ptyKey} />);
    press(getByTestId("pty-mock"), { key: "]", metaKey: true, altKey: true });
    expect(runFor("viewer.cycle-workspace-next")).not.toHaveBeenCalled();
    expect(ptyKey).toHaveBeenCalledTimes(1);
  });
});

describe("useWorkspaceCycleIntercept — which views listen", () => {
  /** One team view's intercept: listening for `repoPath` while active, for nothing when null. */
  function View({ repoPath }: { repoPath: string | null }) {
    useWorkspaceCycleIntercept(repoPath);
    return null;
  }
  function Host({ children }: { children?: React.ReactNode }) {
    return (
      <div data-xterm-host data-repo-path="/repo">
        <div data-testid="pty-mock" tabIndex={-1}>
          pty
        </div>
        {children}
      </div>
    );
  }

  it("a view whose path turns null stops listening", () => {
    const { getByTestId, rerender } = render(
      <>
        <View repoPath="/repo" />
        <Host />
      </>,
    );
    press(getByTestId("pty-mock"), { key: "]", metaKey: true, altKey: true });
    expect(runFor("viewer.cycle-workspace-next")).toHaveBeenCalledTimes(1);
    rerender(
      <>
        <View repoPath={null} />
        <Host />
      </>,
    );
    const event = press(getByTestId("pty-mock"), { key: "]", metaKey: true, altKey: true });
    expect(runFor("viewer.cycle-workspace-next")).toHaveBeenCalledTimes(1);
    expect(event.defaultPrevented).toBe(false);
  });

  it("with two mounted views on the same host path, only the active one runs a chord", () => {
    // Both views have the same host path; only the active view passes it.
    const { getByTestId } = render(
      <>
        <View repoPath="/repo" />
        <View repoPath={null} />
        <Host />
      </>,
    );
    press(getByTestId("pty-mock"), { key: "]", metaKey: true, altKey: true });
    expect(runFor("viewer.cycle-workspace-next")).toHaveBeenCalledTimes(1);
  });

  it("does nothing while focus is in a row popup, even one inside the terminal host", () => {
    const { getByTestId } = render(
      <>
        <View repoPath="/repo" />
        <Host>
          <div {...{ [ROW_POPUP_ATTR]: "" }}>
            <textarea data-testid="in-popup" />
          </div>
        </Host>
      </>,
    );
    const inPopup = getByTestId("in-popup");
    inPopup.focus();
    for (const init of [
      { key: "]", metaKey: true, altKey: true },
      { key: "1", metaKey: true },
      { key: "ArrowLeft", metaKey: true, altKey: true },
    ]) {
      expect(press(inPopup, init).defaultPrevented).toBe(false);
    }
    for (const id of INTERCEPTED_IDS) expect(runFor(id), id).not.toHaveBeenCalled();
  });
});

it("captures history and workspace digits inside a generic repo-less workspace", () => {
  const ptyKey = vi.fn();
  const { getByTestId } = render(<Harness ptyKey={ptyKey} repoPath="" />);
  press(getByTestId("pty-mock"), { key: "[", metaKey: true });
  press(getByTestId("pty-mock"), { key: "1", metaKey: true });
  expect(runFor("nav.context-back")).toHaveBeenCalledOnce();
  expect(runFor("viewer.jump-workspace-1")).toHaveBeenCalledOnce();
  expect(ptyKey).not.toHaveBeenCalled();
});

it.each(["cm-editor", "ProseMirror"])(
  "leaves bracket indentation to a %s editor embedded in the CLI",
  (className) => {
    const { getByTestId } = render(<Harness ptyKey={vi.fn()} />);
    const editor = document.createElement("div");
    editor.className = className;
    editor.innerHTML = "<textarea></textarea>";
    getByTestId("terminal-area").appendChild(editor);
    for (const key of ["[", "]"]) {
      expect(
        press(editor.querySelector("textarea")!, { key, metaKey: true }).defaultPrevented,
      ).toBe(false);
    }
    expect(runFor("nav.context-back")).not.toHaveBeenCalled();
    expect(runFor("nav.context-forward")).not.toHaveBeenCalled();
  },
);

it("leaves a workspace chord to the terminal when there is no focused workspace", () => {
  useViewerStore.setState({ workspaces: {}, activeWorkspace: {} });
  const ptyKey = vi.fn();
  const { getByTestId } = render(<Harness ptyKey={ptyKey} />);
  const event = press(getByTestId("pty-mock"), { key: "]", metaKey: true, altKey: true });
  expect(event.defaultPrevented).toBe(false);
  expect(ptyKey).toHaveBeenCalledOnce();
  expect(runFor("viewer.cycle-workspace-next")).not.toHaveBeenCalled();
});
