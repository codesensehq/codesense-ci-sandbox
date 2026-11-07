// SPDX-License-Identifier: MIT
/**
 * Seam test for a pane whose process ends. A headless xterm, the real input
 * route and pre-attach buffer, the real TerminalManager (which delivers
 * `pty-exit` to the pane), the real status listener (which records the exit in
 * the agent-state store), the real ended-bar controller and bar, the real tab
 * marker, the real viewer store's respawn (keying the pane by its lives, as the
 * workspace body does) and the real launcher — composed the way
 * `ShellTerminalPanel` composes them. Only the Tauri boundary is faked: events
 * are emitted by hand, and `terminal_respawn_tab` is a promise the test settles.
 */
import { readFileSync } from "fs";
import { resolve } from "path";
import { act, fireEvent, render, screen } from "@/test-utils";
import userEvent from "@testing-library/user-event";
import { Terminal } from "@xterm/xterm";
import { useEffect, useRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

interface RespawnCall {
  session: string;
  spawn: Record<string, unknown>;
  scope: Record<string, unknown> | undefined;
  resolve: (extraArgs: string[], removeArgs?: string[]) => void;
  reject: (why: string) => void;
}

const h = vi.hoisted(() => ({
  handlers: new Map<string, Set<(e: { payload: unknown }) => void>>(),
  respawns: [] as RespawnCall[],
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    let set = h.handlers.get(name);
    if (!set) h.handlers.set(name, (set = new Set()));
    set.add(cb);
    return () => set.delete(cb);
  }),
}));
// This suite exercises other inputs; ActivityV1 recovery has its own seam tests.
vi.mock("@/shared/hooks/use-terminal-activity", () => ({ useTerminalActivity: () => {} }));
vi.mock("@/tauri/commands", () => ({
  LOCAL_MACHINE_ID: "local",
  dismissAgentMark: vi.fn((sessionId: string, crashId: string) => {
    projectSessionStatus(sessionId, "done", { crashId });
    return Promise.resolve({ dismissed: true });
  }),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  sessionsHaveLiveAgent: vi.fn(async (queries: { ptyId: string }[]) =>
    queries.map((q) => ({ ptyId: q.ptyId, alive: true })),
  ),
}));
// The host's respawn verb: a promise the test settles.
vi.mock("@/features/viewer/lib/cli-workspace/transport/terminal-layout-source", async () => {
  const { terminalLayoutSourceMock } = await import("@/test/terminal-layout-source-mock");
  return {
    ...terminalLayoutSourceMock,
    terminalRespawnTab: (
      machineId: string,
      session: string,
      spawn: Record<string, unknown>,
      scope?: Record<string, unknown>,
    ) =>
      new Promise((res, rej) => {
        expect(machineId).toBe("local");
        h.respawns.push({
          session,
          spawn,
          scope,
          resolve: (extraArgs, removeArgs) =>
            res({ entry: { session, state: { kind: "live" } }, extraArgs, removeArgs }),
          reject: (why) => rej(new Error(why)),
        });
      }),
  };
});

import { createTerminalManager } from "@/shared/terminal/terminal-manager";
import { useTerminalStatusEvents } from "@/shared/hooks/use-terminal-status-events";
import { resetLivenessPollerForTests } from "@/shared/hooks/use-agent-liveness-monitor";
import { createPtyInputBuffer, type PtyInputBuffer } from "@/shared/lib/pty-input-buffer";
import { useLaunchSheetStore } from "@/features/agents/lib/launch-sheet";
import type { AgentSessionRow } from "@/tauri/commands";
import { projectSessionStatus } from "@/test/session-projection-fixture";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import {
  selectHiddenTerminalTabs,
  selectTabBySessionId,
  selectWorkspaceTabs,
  useViewerStore,
  type TerminalTab,
} from "@/shared/stores/viewer-store";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import { restartHiddenTab } from "../lib/terminal-close";
import { useTerminalExit } from "../hooks/use-terminal-exit";
import { routeTerminalInput } from "../lib/terminal-input-gate";
import {
  buildAgentLaunchCommand,
  connectAgentSession,
  launchAgentLineInShell,
  recordAgentLaunch,
  type LaunchDeps,
  type RelaunchDeps,
  type RespawnTab,
} from "@/shared/lib/agent-session-launcher";
import type { InjectDeps } from "@/shared/lib/agent-session-injection";
import { SessionStatusMark } from "./session-status-mark";
import {
  ENDED_BAR_ARM_MS,
  paneEndShown,
  TerminalEndedAnnouncer,
  TerminalEndedBar,
} from "./terminal-ended-bar";
import { PaneStartupOverlay } from "./pane-startup-overlay";
import {
  PANE_SPINNER_DELAY_MS,
  PANE_START_TIMEOUT_MS,
  usePaneStartup,
  type PaneStartup,
} from "../hooks/use-pane-startup";

const golden = JSON.parse(
  readFileSync(
    resolve(__dirname, "../../../../src-tauri/src/fixtures/agent-state/pty-exit-payloads.json"),
    "utf-8",
  ),
) as {
  payloads: Record<
    "clean" | "failed" | "sigkill" | "stopped",
    { exit_code: number | null; signal: string | null; requested: boolean }
  >;
};

const TEAM = "acme:default";
const WS = "ws-1";
let SID = "shell-1";

function emit(name: string, payload: unknown) {
  const set = h.handlers.get(name);
  if (!set?.size) throw new Error(`no handler registered for ${name}`);
  act(() => {
    for (const cb of [...set]) cb({ payload });
  });
}
const ptyExit = (which: keyof typeof golden.payloads) =>
  emit("pty-exit", { ...golden.payloads[which], session_id: SID });
const hook = (hook_event: string, status: string) =>
  emit("terminal-status-update", { tab_session_id: SID, status, hook_event });
const commandEnd = (exit_code: number) => {
  emit("pty-osc133", { session_id: SID, kind: "command-end", exit_code });
  if (exit_code !== 0 && useAgentStateStore.getState().bySession[SID]?.state === "crashed")
    act(() => projectSessionStatus(SID, "crashed", { crashId: `exit-${exit_code}` }));
};

interface Harness {
  term: Terminal;
  /** What reached the (fake) PTY, in order: typed input and injected lines. */
  sent: string[];
  onEnd: ReturnType<typeof vi.fn<(opts: { dead: boolean }) => void>>;
  /** The launcher's injection seam: the real shell-gated injector writing to
   *  the fake PTY, or a line that never goes in (`lineFails`). */
  relaunchDeps: RelaunchDeps;
  lineFails: boolean;
  /** The tab's exit as its layout entry has it (after a reload, the only one). */
  layoutExit: TerminalTab["exit"];
  /** How the next pane's attach goes: it paints at once (the default), or it
   *  stays pending so the startup overlay runs its course. */
  attach: "painted" | "pending";
  /** The mounted pane's startup clock (`markFailed` stands in for a failed attach). */
  probe: { startup: PaneStartup | null };
  onStartup: (startup: PaneStartup) => void;
}

/** The session's display owner: it hears `pty-exit` and hands it to the pane. */
const manager = createTerminalManager();

/** The panel's composition of the exit pieces, around a headless xterm. */
function Pane({ hx }: { hx: Harness }) {
  const exitedRef = useRef(false);
  const tileRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const bufferRef = useRef<PtyInputBuffer | null>(null);
  const toBarRef = useRef<() => void>(() => {});

  useEffect(() => {
    const buffer = createPtyInputBuffer((d) => hx.sent.push(d), {
      scheduleFlush: (f) => f(),
    });
    buffer.markReady();
    bufferRef.current = buffer;
    // The panel's input route: to the shell while it runs; once it is gone,
    // scroll keys scroll and any other key goes back to the ended bar.
    const sub = hx.term.onData((data) =>
      routeTerminalInput(
        data,
        { exited: exitedRef.current, locked: false },
        {
          write: (d) => bufferRef.current?.write(d),
          term: hx.term,
          toBar: () => toBarRef.current(),
        },
      ),
    );
    return () => {
      sub.dispose();
      buffer.dispose();
    };
  }, [hx]);

  // The panel's respawn: the viewer store's, for this tab.
  const respawn: RespawnTab = (request) =>
    useViewerStore.getState().respawnTerminalTab(TEAM, SID, WS, {
      ...request.shell,
      scope: request.scope,
    });
  const startup = usePaneStartup(true);
  const { onStartup } = hx;
  useEffect(() => onStartup(startup), [onStartup, startup]);
  const paintAtOnce = hx.attach === "painted";
  const { markReady, markRendered } = startup;
  useEffect(() => {
    if (!paintAtOnce) return;
    markReady();
    markRendered();
  }, [paintAtOnce, markReady, markRendered]);
  const {
    markExited,
    claimFocus,
    bar: endedBar,
  } = useTerminalExit({
    sessionId: SID,
    exit: hx.layoutExit,
    exitedRef,
    containerRef: tileRef,
    focusTerminal: () => inputRef.current?.focus(),
    respawn,
    endTab: hx.onEnd,
    relaunchDeps: hx.relaunchDeps,
  });
  useEffect(() => {
    toBarRef.current = claimFocus;
  }, [claimFocus]);
  // The panel's arbitration: one "not running" state at a time.
  const shown = paneEndShown(endedBar, startup.phase);
  const bar = shown.bar;
  useEffect(() => {
    void manager.ensure(SID, { fontSizePx: 12 });
    return manager.listen(SID, { exit: markExited });
  }, [markExited]);

  return (
    <div>
      <SessionStatusMark sessionId={SID} data-testid="tab-dot" />
      <div ref={tileRef}>
        {/* Stands in for xterm's helper textarea: where keyboard focus sits. */}
        <textarea ref={inputRef} data-testid="terminal-input" />
        {shown.overlay && (
          <PaneStartupOverlay
            phase={startup.phase}
            failure={startup.failure}
            started
            background="#000"
            onRetry={() => {}}
          />
        )}
        {bar && <TerminalEndedBar {...bar} />}
        <TerminalEndedAnnouncer bar={bar} />
      </div>
      {/* The panel's attach / reveal focus, which the pane hands to the bar while ended. */}
      <button type="button" data-testid="reveal" onClick={claimFocus} />
    </div>
  );
}

/** The workspace body: the pane keyed by its tab's lives, so a new process in
 *  the tab (a respawn) gets a new pane. */
function Body({ hx }: { hx: Harness }) {
  const lives = useViewerStore((s) => s.terminalLives[SID] ?? 0);
  // The tab's exit as its layout has it, when the layout holds the tab.
  const exit = useViewerStore((s) => selectTabBySessionId(s, SID)?.exit);
  return <Pane key={lives} hx={exit ? { ...hx, layoutExit: exit } : hx} />;
}

/** The injector's view of the fake PTY: the shell is at its prompt. */
function ptyInject(hx: Harness): InjectDeps {
  return {
    sessionExists: () => true,
    getAiStatus: () => undefined,
    isShellIdle: async () => true,
    write: async (_sid, text) => {
      hx.sent.push(text);
    },
    submit: async (_sid, text) => {
      hx.sent.push(text);
      return { submitted: true };
    },
  };
}

const unmounts: (() => void)[] = [];

async function mount(
  opts: { layoutExit?: TerminalTab["exit"]; attach?: Harness["attach"] } = {},
): Promise<Harness> {
  const hx: Harness = {
    term: new Terminal({ rows: 10, cols: 60, allowProposedApi: true }),
    sent: [],
    onEnd: vi.fn<(opts: { dead: boolean }) => void>(),
    lineFails: false,
    layoutExit: opts.layoutExit,
    attach: opts.attach ?? "painted",
    probe: { startup: null },
    onStartup: (startup) => {
      hx.probe.startup = startup;
    },
    relaunchDeps: {
      launchAgentLine: (sid, line) =>
        hx.lineFails
          ? Promise.resolve({ ok: false, reason: "agent-busy" } as const)
          : launchAgentLineInShell(sid, line, ptyInject(hx)),
    },
  };
  const Status = () => {
    useTerminalStatusEvents();
    return null;
  };
  const r = render(
    <>
      <Status />
      <Body hx={hx} />
    </>,
  );
  unmounts.push(() => {
    r.unmount();
    manager.dispose(SID);
    hx.term.dispose();
  });
  // Let the async `listen` registrations land.
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
  screen.getByTestId("terminal-input").focus();
  return hx;
}

/** Type into the terminal as the user would (xterm's onData funnel). */
function type(hx: Harness, data: string) {
  act(() => hx.term.input(data, true));
}

/** Let the bar's Enter arm (time passes since it took focus). */
function arm() {
  vi.setSystemTime(Date.now() + ENDED_BAR_ARM_MS);
}

/** Whether the test runs on fake timeouts (the startup overlay's clock). */
let fakeTimeouts = false;

/** Let chained async work (respawn → remount → inject) settle. */
async function flush() {
  await act(async () => {
    if (fakeTimeouts) await vi.advanceTimersByTimeAsync(20);
    else await new Promise((r) => setTimeout(r, 20));
    for (let i = 0; i < 20; i++) await Promise.resolve();
  });
}

/** The host answers the latest `terminal_respawn_tab`. */
async function respawned(extraArgs: string[] = [], removeArgs?: string[]) {
  const call = h.respawns.at(-1);
  if (!call) throw new Error("no respawn asked for");
  await act(async () => call.resolve(extraArgs, removeArgs));
  await flush();
}
async function respawnRefused(why: string) {
  const call = h.respawns.at(-1);
  if (!call) throw new Error("no respawn asked for");
  await act(async () => call.reject(why));
  await flush();
}

const bar = () => screen.queryByTestId("terminal-ended-bar");
const dot = () => screen.queryByTestId("tab-dot");

/** A dashboard-verb launch: the panel's own shell, the launcher's default line. */
const VERB_SPEC = { agentPath: "/usr/local/bin/claude" };
const VERB_LINE = buildAgentLaunchCommand("/usr/local/bin/claude");
/** A launch the launcher chose the shell's rules for, with a scoped MCP config. */
const SCOPED_SPEC = {
  agentPath: "/usr/local/bin/claude",
  args: [],
  shell: { cwd: "/ctx/root", envRemove: ["ANTHROPIC_API_KEY"], mcpAllow: ["buildkite"] },
};

beforeEach(() => {
  useAgentListProjectionStore.setState({ sessions: [] });
  vi.useFakeTimers({ toFake: ["Date"] });
  fakeTimeouts = false;
  SID = "shell-1";
  h.respawns = [];
  useLaunchSheetStore.getState().close();
  useAgentListProjectionStore.setState({ sessions: [] });
  resetLivenessPollerForTests();
  h.handlers.clear();
  useViewerStore.setState({
    aiStatusBySession: {},
    progressBySession: {},
    terminalLives: {},
    // Each test starts with no layout (a describe that seeds one installs its own).
    terminalLayout: { hydrated: false, admission: {}, members: {}, byKey: {} },
    workspaces: { [TEAM]: [{ id: WS, kind: "generic", cwd: "/work", status: "ready" }] },
  });
  useAgentStateStore.setState({
    bySession: {},
    exitBySession: {},
    launchBySession: {},
    relaunchBySession: {},
  });
});

afterEach(() => {
  for (const u of unmounts.splice(0)) u();
  resetLivenessPollerForTests();
  vi.useRealTimers();
});

describe("a plain shell tab whose shell exits", () => {
  for (const order of ["reason-first", "exit-first", "restored"] as const) {
    it(`shows the host engine-loss reason through the existing ended bar (${order})`, async () => {
      const reason =
        "Hermes Agent's engine stopped, so this session was ended to keep CodeSense's identity checks safe. Relaunch to continue.";
      recordAgentLaunch(SID, VERB_SPEC, VERB_LINE);
      if (order === "restored") {
        projectSessionStatus(SID, "crashed", { crashId: "engine-loss", endReason: reason });
        await mount({ layoutExit: { code: null, signal: "SIGKILL" } });
      } else {
        await mount();
        if (order === "exit-first") ptyExit("sigkill");
        act(() =>
          projectSessionStatus(SID, "crashed", { crashId: "engine-loss", endReason: reason }),
        );
        if (order === "reason-first") ptyExit("sigkill");
      }
      expect(bar()).toHaveTextContent(reason);
      expect(screen.getByTestId("terminal-ended-relaunch")).toBeVisible();
      expect(bar()).toHaveAttribute("data-outcome", "crashed");
      expect(screen.getByTestId("terminal-ended-restart")).toBeVisible();
      expect(screen.getByTestId("terminal-ended-close")).toBeVisible();
    });
  }

  it("exit 0: the ended bar with the code, the bar focused (not a button), the tab marked ended", async () => {
    await mount();
    ptyExit("clean");

    expect(bar()).toHaveAttribute("data-outcome", "ended");
    expect(bar()).toHaveTextContent("Ended · exit 0");
    expect(bar()).toHaveFocus();
    expect(screen.getByTestId("terminal-ended-restart")).toHaveAttribute(
      "aria-keyshortcuts",
      "Enter",
    );
    expect(screen.getByTestId("terminal-ended-close")).toBeInTheDocument();
    expect(screen.queryByTestId("terminal-ended-relaunch")).toBeNull();
    expect(dot()).toHaveAttribute("data-mark", "ended");
    expect(dot()).toHaveAccessibleName("Ended · exit 0");
    // Announced politely, through a live region that was already there.
    expect(screen.getByTestId("terminal-ended-announce-polite")).toHaveTextContent(
      "Ended · exit 0",
    );
    expect(screen.getByTestId("terminal-ended-announce-assertive")).toHaveTextContent("");
  });

  it("a plain nonzero exit (`false; exit`) just ended: grey, not crashed", async () => {
    await mount();
    ptyExit("failed");
    expect(bar()).toHaveAttribute("data-outcome", "ended");
    expect(bar()).toHaveTextContent("Ended · exit 5");
    expect(dot()).toHaveAttribute("data-mark", "ended");
  });

  it("`exit` after a Ctrl-C'd command (130) just ended: grey, not crashed", async () => {
    await mount();
    emit("pty-exit", { session_id: SID, exit_code: 130, signal: null, requested: false });
    expect(bar()).toHaveAttribute("data-outcome", "ended");
    expect(bar()).toHaveTextContent("Ended · exit 130");
    expect(dot()).toHaveAttribute("data-mark", "ended");
  });

  it("`kill -9`: the crashed bar, named, the tab marked crashed, announced assertively", async () => {
    await mount();
    ptyExit("sigkill");
    expect(bar()).toHaveAttribute("data-outcome", "crashed");
    expect(bar()).toHaveTextContent("Crashed · SIGKILL");
    expect(dot()).toHaveAttribute("data-mark", "crashed");
    expect(dot()).toHaveAccessibleName("Crashed · SIGKILL");
    expect(screen.getByTestId("terminal-ended-announce-assertive")).toHaveTextContent(
      "Crashed · SIGKILL",
    );
  });

  it("the bar does not take focus from elsewhere (an unfocused pane)", async () => {
    await mount();
    const outside = document.createElement("button");
    document.body.appendChild(outside);
    outside.focus();
    ptyExit("clean");
    expect(bar()).toBeInTheDocument();
    expect(outside).toHaveFocus();
    outside.remove();
  });

  it("typing in the ended terminal neither restarts it nor reaches the dead shell", async () => {
    const hx = await mount();
    ptyExit("clean");
    act(() => screen.getByTestId("terminal-input").focus());
    type(hx, "l");
    type(hx, "s\r");
    expect(h.respawns).toEqual([]);
    expect(hx.sent).toEqual([]);
    expect(bar()).toBeInTheDocument();
  });

  it("typing through the death (`echo hi; make deploy⏎`): nothing after it runs, nothing restarts", async () => {
    const user = userEvent.setup();
    const hx = await mount();
    type(hx, "echo hi; ");
    ptyExit("sigkill");
    // The rest of the line lands on the bar that just took focus.
    await user.keyboard("make deploy{Enter}");

    expect(h.respawns).toEqual([]);
    expect(hx.sent).toEqual(["echo hi; "]);
    expect(bar()).toBeInTheDocument();
    expect(screen.getByTestId("terminal-ended-hint")).toHaveAttribute("data-shown", "true");
    expect(screen.getByTestId("terminal-ended-hint")).toHaveTextContent(
      "Press ⏎ to restart · Esc to scroll",
    );
    // Even well after the bar armed, Space and letters press nothing.
    arm();
    await user.keyboard(" x ");
    expect(h.respawns).toEqual([]);
  });

  it("a held Enter never restarts: not before the bar arms, not on its repeats", async () => {
    await mount();
    ptyExit("clean");
    fireEvent.keyDown(bar()!, { key: "Enter" });
    expect(h.respawns).toEqual([]);
    arm();
    fireEvent.keyDown(bar()!, { key: "Enter", repeat: true });
    fireEvent.keyDown(bar()!, { key: "Enter", repeat: true });
    expect(h.respawns).toEqual([]);
  });

  it("⏎ after the bar arms restarts the tab in place; the new pane takes the first keystroke intact", async () => {
    const hx = await mount();
    ptyExit("failed");
    arm();

    const notPrevented = fireEvent.keyDown(bar()!, { key: "Enter" });
    expect(notPrevented).toBe(false); // consumed by the bar
    expect(h.respawns).toHaveLength(1);
    // The same tab, a plain shell in its workspace's directory.
    expect(h.respawns[0].session).toBe(SID);
    // Always a fresh shell for the tab, so its ring can carry the history.
    expect(h.respawns[0].spawn).toMatchObject({ kind: "cold", cwd: "/work" });
    expect(h.respawns[0].scope).toBeUndefined();

    await respawned();
    // A new pane on the new process: no bar, no ended mark.
    expect(bar()).toBeNull();
    expect(dot()).toBeNull();
    type(hx, "l");
    type(hx, "s");
    expect(hx.sent).toEqual(["l", "s"]);
  });

  it("a crash's mark is the still red square whether or not the tab was seen", async () => {
    await mount();
    ptyExit("sigkill");
    expect(dot()).toHaveAttribute("data-mark", "crashed");
    expect(dot()!.className).toContain("cs-mark-crashed");
    act(() => useAgentStateStore.getState().markSeen(SID, Date.now() + 1));
    expect(dot()).toHaveAttribute("data-mark", "crashed");
    expect(dot()!.querySelector(".cs-mark-spin, .cs-mark-halo")).toBeNull();
  });

  it("a clean end's mark is the hollow grey square", async () => {
    await mount();
    ptyExit("clean");
    expect(dot()).toHaveAttribute("data-mark", "ended");
    expect(dot()!.className).toContain("cs-mark-ended");
  });

  it("clicking Restart restarts too", async () => {
    await mount();
    ptyExit("clean");
    fireEvent.click(screen.getByTestId("terminal-ended-restart"));
    expect(h.respawns).toHaveLength(1);
  });

  it("a restart the host refuses leaves the bar up, saying why", async () => {
    const hx = await mount();
    ptyExit("clean");
    fireEvent.click(screen.getByTestId("terminal-ended-restart"));
    await respawnRefused("spawn_pty: no such directory");
    expect(bar()).toHaveTextContent("Restart failed · spawn_pty: no such directory");
    type(hx, "x");
    expect(hx.sent).toEqual([]);
  });

  it("Close ends the tab at once (its process is gone)", async () => {
    const hx = await mount();
    ptyExit("clean");
    fireEvent.click(screen.getByTestId("terminal-ended-close"));
    expect(hx.onEnd).toHaveBeenCalledWith({ dead: true });
    expect(h.respawns).toEqual([]);
  });

  it("an exit CodeSense asked for (End) shows no bar: its host removes the tab", async () => {
    await mount();
    ptyExit("stopped");
    expect(bar()).toBeNull();
    expect(dot()).toBeNull();
  });

  it("after Esc, scroll keys scroll the scrollback and any other key goes back to the bar", async () => {
    const hx = await mount();
    for (let i = 0; i < 60; i++) hx.term.write(`line ${i}\r\n`);
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    ptyExit("clean");
    expect(bar()).toHaveFocus();
    expect(bar()).toHaveAttribute("aria-describedby", screen.getByTestId("terminal-ended-hint").id);

    fireEvent.keyDown(bar()!, { key: "Escape" });
    expect(screen.getByTestId("terminal-input")).toHaveFocus();
    const bottom = hx.term.buffer.active.viewportY;
    type(hx, "\x1b[5~"); // PageUp
    expect(hx.term.buffer.active.viewportY).toBeLessThan(bottom);
    expect(screen.getByTestId("terminal-input")).toHaveFocus();

    type(hx, "q");
    expect(bar()).toHaveFocus();
    expect(hx.sent).toEqual([]);
    expect(h.respawns).toEqual([]);
  });
});

describe("an agent tab", () => {
  it.each([
    {
      transition: "on-on",
      prompt: "Review the code — keep 'quotes' and $TOKEN\n  next line\tunchanged",
    },
    { transition: "on-off", prompt: "--no-daemon" },
    { transition: "off-on", prompt: "Implement this task\nwith token=example1234 and -- flags" },
  ])(
    "R6 prompt operands survive two real relaunches: $transition",
    async ({ transition, prompt }) => {
      const hooks = (label: string) => [
        "-c",
        `hooks.PreToolUse=[{hooks=[{command="${label} # codesense-git-ai"}]}]`,
        "-c",
        `hooks.state={capture={trusted_hash="${label}"}}`,
      ];
      const options = ["--no-daemon", "exec", "-c", 'mcp_servers.x.env.TOKEN="dummy-value"'];
      const operands = ["--", prompt];
      let previousHooks = transition === "off-on" ? [] : hooks("old");
      let args = [...options, ...previousHooks, ...operands];
      recordAgentLaunch(
        SID,
        {
          agentPath: "/bin/codex",
          args,
          shell: { cwd: "/ctx/root", envRemove: ["OPENAI_API_KEY"], mcpAllow: [], agent: true },
        },
        buildAgentLaunchCommand("/bin/codex", args),
      );
      const hx = await mount();
      for (let round = 0; round < 2; round++) {
        ptyExit("clean");
        fireEvent.click(screen.getByTestId("terminal-ended-relaunch"));
        await flush();
        expect(h.respawns[round].scope?.launchArgs).toEqual(args);
        const freshHooks = transition === "on-off" ? [] : hooks(`fresh-${round}`);
        await respawned(["--no-daemon", ...freshHooks], previousHooks);
        const expected = [...options, ...freshHooks, ...operands];
        const recorded = useAgentStateStore.getState().launchBySession[SID];
        expect(recorded?.spec?.args).toEqual(expected);
        const actual = recorded!.spec!.args!;
        expect(actual.slice(actual.indexOf("--"))).toEqual(operands);
        for (const flag of freshHooks.filter((arg) => arg.startsWith("hooks."))) {
          expect(actual.indexOf(flag)).toBeLessThan(actual.indexOf("--"));
        }
        const line = buildAgentLaunchCommand("/bin/codex", expected);
        expect(hx.sent.at(-1)).toBe(` ${line}\r`);
        expect(recorded?.line).toBe(line);
        args = actual;
        previousHooks = freshHooks;
      }
    },
  );

  it("R6 an absent edit also inserts fresh scope flags before the prompt", async () => {
    const args = ["exec", "--", "Keep this prompt\nexactly as given"];
    recordAgentLaunch(
      SID,
      {
        agentPath: "/bin/codex",
        args,
        shell: { cwd: "/ctx/root", envRemove: [], mcpAllow: [] },
      },
      buildAgentLaunchCommand("/bin/codex", args),
    );
    const hx = await mount();
    ptyExit("clean");
    fireEvent.click(screen.getByTestId("terminal-ended-relaunch"));
    await flush();
    await respawned(["--no-daemon", "-c", "scope=1"]);
    const expected = ["--no-daemon", "exec", "-c", "scope=1", ...args.slice(1)];
    expect(hx.sent).toEqual([` ${buildAgentLaunchCommand("/bin/codex", expected)}\r`]);
    expect(useAgentStateStore.getState().launchBySession[SID]?.spec?.args).toEqual(args);
  });

  it.each(["on-off", "off-on", "changed-origin"])(
    "R5 real viewer relaunch applies owned capture edit: %s",
    async (transition) => {
      const secret = ["ghp_", "abcdefghijklmnopqrstuvwxyz0123"].join("");
      const userArgs = ["exec", "-c", `mcp_servers.x.env.TOKEN="${secret}"`, "token=example1234"];
      const oldHooks = [
        "-c",
        'hooks.PreToolUse=[{hooks=[{command="old # codesense-git-ai"}]}]',
        "-c",
        'hooks.state={old={trusted_hash="old-hash"}}',
      ];
      const freshHooks = [
        "-c",
        'hooks.PreToolUse=[{hooks=[{command="fresh # codesense-git-ai"}]}]',
        "-c",
        'hooks.state={fresh={trusted_hash="fresh-hash"}}',
      ];
      const wasOn = transition !== "off-on";
      const args = [...userArgs, ...(wasOn ? oldHooks : [])];
      recordAgentLaunch(
        SID,
        {
          agentPath: "/bin/codex",
          args,
          shell: { cwd: "/ctx/root", envRemove: ["OPENAI_API_KEY"], mcpAllow: [], agent: true },
        },
        buildAgentLaunchCommand("/bin/codex", args),
      );
      const hx = await mount();
      ptyExit("clean");
      fireEvent.click(screen.getByTestId("terminal-ended-relaunch"));
      await flush();
      expect(h.respawns[0].scope?.launchArgs).toEqual(args);
      const removeArgs = wasOn ? oldHooks : [];
      const extraArgs = ["--no-daemon", ...(transition === "off-on" ? freshHooks : [])];
      await respawned(extraArgs, removeArgs);
      const expectedArgs = ["--no-daemon", ...userArgs, ...extraArgs.slice(1)];
      const expectedLine = buildAgentLaunchCommand("/bin/codex", expectedArgs);
      expect(hx.sent).toEqual([` ${expectedLine}\r`]);
      const recorded = useAgentStateStore.getState().launchBySession[SID];
      expect(recorded?.spec?.args).toEqual(expectedArgs);
      expect(recorded?.line).toBe(expectedLine);
      expect(recorded?.spec?.args?.slice(1, userArgs.length + 1)).toEqual(userArgs);
      ptyExit("clean");
      fireEvent.click(screen.getByTestId("terminal-ended-relaunch"));
      await flush();
      expect(h.respawns[1].scope?.launchArgs).toEqual(expectedArgs);
    },
  );

  it("an agent tab relaunches through the launcher: same env strip, same account, fresh scope, leading space", async () => {
    // The original launch, through the real launcher with fake host seams.
    const opened: unknown[] = [];
    const deps: LaunchDeps = {
      findExisting: async () => undefined,
      provision: async () => {},
      openTab: async (_team, _ws, _tab, shell) => {
        opened.push(shell);
      },
      closeTab: () => {},
      launchAgentLine: async () => ({ ok: true }),
      projectScopeConfig: async () => ({ extraArgs: [], environmentBinding: null }),
    };
    const intent = {
      runtime: "claude" as const,
      overrideAccountId: "acct-7",
      overrideAccountGeneration: 3,
    };
    const { sessionId } = await connectAgentSession(
      {
        teamKey: TEAM,
        orgRoot: "/ctx/root",
        agentPath: "",
        role: "test-agent",
        workspaceKey: "test-agent",
        prepare: async () => ({
          agentPath: "/usr/local/bin/claude",
          args: ["--permission-mode", "plan"],
          mcpAllow: ["buildkite"],
          extraArgs: ["--mcp-config", "/tmp/scope/first.json"],
          envRemove: ["ANTHROPIC_API_KEY"],
          account: intent,
          environmentBinding: "binding-1",
        }),
      },
      deps,
    );
    SID = sessionId;
    const hx = await mount();
    hook("UserPromptSubmit", "running");
    ptyExit("sigkill");

    const relaunch = screen.getByTestId("terminal-ended-relaunch");
    expect(relaunch).toHaveTextContent("Relaunch claude");
    expect(relaunch).toHaveAttribute("title", expect.stringContaining("not resumed"));
    fireEvent.click(relaunch);
    await flush();

    // The host starts the new process with the launch's own shell rules…
    const [call] = h.respawns;
    expect(opened[0]).toMatchObject({ cwd: "/ctx/root", envRemove: ["ANTHROPIC_API_KEY"] });
    expect(call.spawn).toMatchObject({
      kind: "cold",
      cwd: "/ctx/root",
      envRemove: ["ANTHROPIC_API_KEY"],
      account: intent,
    });
    // …and projects its scope afresh, against the launch's own CLI arguments.
    expect(call.scope).toEqual({
      cwd: "/ctx/root",
      mcpAllow: ["buildkite"],
      bridgeTools: null,
      account: intent,
      binary: "claude",
      launchArgs: ["--permission-mode", "plan"],
      agent: true,
    });
    await respawned(["--mcp-config", "/tmp/scope/fresh.json"]);

    // Typed by the launcher's injector: leading space (kept out of history), fresh flags.
    const line = buildAgentLaunchCommand("/usr/local/bin/claude", [
      "--permission-mode",
      "plan",
      "--mcp-config",
      "/tmp/scope/fresh.json",
    ]);
    expect(hx.sent).toEqual([` ${line}\r`]);
    expect(useAgentStateStore.getState().launchBySession[SID]?.line).toBe(line);
    expect(bar()).toBeNull();
  });

  it.each([[], ["who_am_i"]].map((bridgeTools) => ({ bridgeTools })))(
    "Relaunch forwards captured Codex bridge policy $bridgeTools to the native respawn",
    async ({ bridgeTools }) => {
      recordAgentLaunch(
        SID,
        {
          agentPath: "/bin/codex",
          args: [],
          shell: {
            cwd: "/ctx/root",
            envRemove: ["OPENAI_API_KEY"],
            mcpAllow: [],
            agent: true,
            bridgeTools,
          },
        },
        "'codex'",
      );
      const hx = await mount();
      ptyExit("clean");
      fireEvent.click(screen.getByTestId("terminal-ended-relaunch"));
      await flush();
      expect(h.respawns[0].scope).toMatchObject({ binary: "codex", agent: true, bridgeTools });
      await respawned(["-c", "mcp_servers={}"]);
      expect(hx.sent).toEqual([" '/bin/codex' '-c' 'mcp_servers={}'\r"]);
    },
  );

  it("a dashboard-verb tab relaunches in a plain shell, through the same injector", async () => {
    const hx = await mount();
    recordAgentLaunch(SID, VERB_SPEC, VERB_LINE);
    hook("UserPromptSubmit", "running");
    ptyExit("sigkill");
    fireEvent.click(screen.getByTestId("terminal-ended-relaunch"));
    await flush();
    expect(h.respawns[0].spawn).toMatchObject({ kind: "cold", cwd: "/work" });
    expect(h.respawns[0].scope).toBeUndefined();
    await respawned();
    expect(hx.sent).toEqual([` ${VERB_LINE}\r`]);
  });

  it("Restart on a role-tagged agent tab keeps its shell rules (the API key stays stripped)", async () => {
    await mount();
    recordAgentLaunch(SID, SCOPED_SPEC, "'claude'");
    ptyExit("sigkill");
    fireEvent.click(screen.getByTestId("terminal-ended-restart"));
    expect(h.respawns[0].spawn).toMatchObject({
      kind: "cold",
      cwd: "/ctx/root",
      envRemove: ["ANTHROPIC_API_KEY"],
    });
    expect(h.respawns[0].scope).toBeUndefined();
  });

  it("an agent the user typed (no known launch spec): Restart only", async () => {
    await mount();
    hook("UserPromptSubmit", "running");
    ptyExit("sigkill");
    expect(screen.getByTestId("terminal-ended-restart")).toBeInTheDocument();
    expect(screen.queryByTestId("terminal-ended-relaunch")).toBeNull();
  });

  it("a relaunch the host refuses fails on the bar and types nothing", async () => {
    const hx = await mount();
    recordAgentLaunch(SID, SCOPED_SPEC, "'claude'");
    ptyExit("sigkill");
    fireEvent.click(screen.getByTestId("terminal-ended-relaunch"));
    await flush();
    await respawnRefused("layout: session shell-1 is still running");
    expect(bar()).toHaveTextContent("Relaunch failed · layout: session shell-1 is still running");
    expect(hx.sent).toEqual([]);
  });

  it("a relaunch whose line never runs: the new pane keeps the shell usable and says so", async () => {
    const hx = await mount();
    recordAgentLaunch(SID, VERB_SPEC, VERB_LINE);
    ptyExit("sigkill");
    hx.lineFails = true;
    fireEvent.click(screen.getByTestId("terminal-ended-relaunch"));
    await flush();
    await respawned();
    // The failure outlives the old pane.
    expect(bar()).toHaveAttribute("data-kind", "agent");
    expect(bar()).toHaveTextContent("Relaunch failed · the shell never reached its prompt");
    type(hx, "x");
    expect(hx.sent).toEqual(["x"]);
  });

  it("the bar's actions are disabled while a relaunch is in flight", async () => {
    const hx = await mount();
    recordAgentLaunch(SID, SCOPED_SPEC, "'claude'");
    ptyExit("sigkill");
    arm();
    fireEvent.click(screen.getByTestId("terminal-ended-relaunch"));
    await flush();
    // The host has not answered yet.
    expect(bar()).toHaveAttribute("aria-busy", "true");
    for (const id of [
      "terminal-ended-restart",
      "terminal-ended-relaunch",
      "terminal-ended-close",
    ]) {
      expect(screen.getByTestId(id)).toBeDisabled();
    }
    fireEvent.keyDown(bar()!, { key: "Enter" });
    fireEvent.click(screen.getByTestId("terminal-ended-relaunch"));
    await flush();
    expect(h.respawns).toHaveLength(1);
    expect(hx.onEnd).not.toHaveBeenCalled();
  });

  it("the agent crashes inside a live shell: a bar that keeps the shell usable", async () => {
    const hx = await mount();
    recordAgentLaunch(SID, VERB_SPEC, VERB_LINE);
    hook("UserPromptSubmit", "running");
    commandEnd(137);

    expect(bar()).toHaveAttribute("data-kind", "agent");
    expect(bar()).toHaveTextContent("claude crashed · exit 137");
    expect(screen.queryByTestId("terminal-ended-restart")).toBeNull();
    // Focus stays in the live shell, and typing still reaches it.
    expect(screen.getByTestId("terminal-input")).toHaveFocus();
    type(hx, "x");
    expect(hx.sent).toEqual(["x"]);

    // Enter on the bar itself relaunches nothing; on the focused button it does.
    act(() => bar()!.focus());
    arm();
    fireEvent.keyDown(bar()!, { key: "Enter" });
    await flush();
    expect(hx.sent).toEqual(["x"]);
    const relaunch = screen.getByTestId("terminal-ended-relaunch");
    act(() => relaunch.focus());
    fireEvent.keyDown(relaunch, { key: "Enter" });
    await flush();
    // Into the live shell, by the launcher's injector (leading space), no respawn.
    expect(hx.sent).toEqual(["x", ` ${VERB_LINE}\r`]);
    expect(h.respawns).toEqual([]);
  });

  it("the shell then exits cleanly: the tab stays crashed, in the same words on bar and marker", async () => {
    await mount();
    hook("UserPromptSubmit", "running");
    commandEnd(137);
    ptyExit("clean");
    expect(bar()).toHaveAttribute("data-kind", "session");
    expect(bar()).toHaveAttribute("data-outcome", "crashed");
    expect(bar()).toHaveTextContent("Crashed · exit 0");
    expect(dot()).toHaveAttribute("data-mark", "crashed");
    expect(dot()).toHaveAccessibleName("Crashed · exit 0");
  });

  it("a crash the user dismissed does not outlive it: a clean shell exit then ended", async () => {
    await mount();
    hook("UserPromptSubmit", "running");
    commandEnd(137);
    fireEvent.click(screen.getByTestId("terminal-ended-dismiss"));
    ptyExit("clean");
    expect(bar()).toHaveAttribute("data-outcome", "ended");
    expect(bar()).toHaveTextContent("Ended · exit 0");
  });

  it("a relaunch into a live shell that fails says so", async () => {
    const hx = await mount();
    recordAgentLaunch(SID, VERB_SPEC, VERB_LINE);
    hook("UserPromptSubmit", "running");
    commandEnd(137);
    hx.lineFails = true;
    fireEvent.click(screen.getByTestId("terminal-ended-relaunch"));
    await flush();
    expect(bar()).toHaveTextContent("Relaunch failed · the shell never reached its prompt");
    fireEvent.click(screen.getByTestId("terminal-ended-close"));
    expect(hx.onEnd).toHaveBeenCalledWith({ dead: false });
  });

  it("an agent the user typed crashes inside a live shell: no Relaunch", async () => {
    await mount();
    hook("UserPromptSubmit", "running");
    commandEnd(137);
    expect(bar()).toHaveTextContent("Agent crashed · exit 137");
    expect(screen.queryByTestId("terminal-ended-relaunch")).toBeNull();
  });

  it("a dismissed agent crash stays dismissed when the pane remounts", async () => {
    await mount();
    hook("UserPromptSubmit", "running");
    commandEnd(137);
    fireEvent.click(screen.getByTestId("terminal-ended-dismiss"));
    expect(bar()).toBeNull();
    for (const u of unmounts.splice(0)) u();
    await mount();
    expect(bar()).toBeNull();
  });

  it("a clean agent exit in a live shell shows no bar", async () => {
    await mount();
    hook("UserPromptSubmit", "running");
    commandEnd(0);
    expect(bar()).toBeNull();
  });
});

describe("a pane shown after its process ended", () => {
  function recordExit(which: keyof typeof golden.payloads) {
    const p = golden.payloads[which];
    useAgentStateStore.getState().recordPtyExit(SID, {
      code: p.exit_code,
      signal: p.signal,
      requested: p.requested,
    });
  }

  it("mounting on an exit recorded earlier shows the bar, and keystrokes stay out of the dead shell", async () => {
    recordExit("failed");
    const hx = await mount();
    expect(bar()).toHaveTextContent("Ended · exit 5");
    type(hx, "x");
    expect(hx.sent).toEqual([]);
    expect(h.respawns).toEqual([]);
  });

  it("focus headed for the terminal (attach, reveal) goes to the bar while ended", async () => {
    recordExit("clean");
    await mount();
    expect(bar()).not.toHaveFocus();
    fireEvent.click(screen.getByTestId("reveal"));
    expect(bar()).toHaveFocus();
  });

  it("after a reload only the tab's layout exit is left: a signal crashed, a code ended", async () => {
    await mount({ layoutExit: { code: null, signal: "SIGKILL" } });
    expect(bar()).toHaveAttribute("data-outcome", "crashed");
    expect(bar()).toHaveTextContent("Crashed · SIGKILL");
    for (const u of unmounts.splice(0)) u();
    await mount({ layoutExit: { code: 130, signal: null } });
    expect(bar()).toHaveAttribute("data-outcome", "ended");
    expect(bar()).toHaveTextContent("Ended · exit 130");
  });
});

describe("one restart at a time, from the bar or the tray", () => {
  installTerminalLayoutHarness();

  /** The tab, ended, in its host's layout: shown on the tile, or Hidden. */
  function seedEnded(hidden: boolean) {
    seedTerminalTabs(TEAM, WS, [
      { session: "other", name: "other" },
      {
        session: SID,
        name: "t1",
        exit: { code: null, signal: "SIGKILL" },
        ...(hidden ? { placement: "background" as const, tags: { hidden: "tab" } } : {}),
      },
    ]);
  }

  it("a tray Restart racing the bar's Restart asks the host once", async () => {
    await mount();
    seedEnded(true);
    ptyExit("sigkill");
    expect(selectHiddenTerminalTabs(useViewerStore.getState(), TEAM, WS).map((t) => t.id)).toEqual([
      SID,
    ]);
    // The tray menu twice (it has no busy state of its own), then the bar.
    act(() => restartHiddenTab(TEAM, WS, SID));
    act(() => restartHiddenTab(TEAM, WS, SID));
    fireEvent.click(screen.getByTestId("terminal-ended-restart"));
    await flush();
    expect(h.respawns).toHaveLength(1);
    await respawned();
    expect(selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS).map((t) => t.id)).toContain(
      SID,
    );
  });

  it("the new pane shows no stale ended bar, though the host's announcement is still to come", async () => {
    await mount();
    seedEnded(false);
    ptyExit("sigkill");
    expect(bar()).toBeInTheDocument();
    fireEvent.click(screen.getByTestId("terminal-ended-restart"));
    await flush();
    // The host answered; its layout change has not arrived.
    await respawned();
    expect(selectTabBySessionId(useViewerStore.getState(), SID)?.exit).toBeUndefined();
    expect(bar()).toBeNull();
    expect(dot()).toBeNull();
  });

  it("while the host starts the new process the ended record stays: no flicker to ended", async () => {
    await mount();
    seedEnded(false);
    ptyExit("sigkill");
    fireEvent.click(screen.getByTestId("terminal-ended-restart"));
    await flush();
    expect(h.respawns).toHaveLength(1);
    // In flight: the tab still reads crashed, the record intact.
    expect(useAgentStateStore.getState().exitBySession[SID]?.outcome).toBe("crashed");
    expect(dot()).toHaveAttribute("data-mark", "crashed");
    await respawned();
    expect(useAgentStateStore.getState().exitBySession[SID]).toBeUndefined();
  });

  it("a new process that exits before the host's answer keeps its own exit", async () => {
    await mount();
    seedEnded(false);
    ptyExit("sigkill");
    fireEvent.click(screen.getByTestId("terminal-ended-restart"));
    await flush();
    // The respawned shell dies at once; its exit lands before the reply.
    ptyExit("failed");
    await respawned();
    expect(useAgentStateStore.getState().exitBySession[SID]).toMatchObject({
      code: 5,
      outcome: "done",
    });
  });

  it("a refused restart keeps the ended record: the tab still reads crashed", async () => {
    await mount();
    seedEnded(false);
    ptyExit("sigkill");
    fireEvent.click(screen.getByTestId("terminal-ended-restart"));
    await flush();
    await respawnRefused("layout: session shell-1 is still running");
    expect(useAgentStateStore.getState().exitBySession[SID]?.outcome).toBe("crashed");
    expect(dot()).toHaveAttribute("data-mark", "crashed");
    expect(bar()).toHaveTextContent("Restart failed · layout: session shell-1 is still running");
  });
});

describe("the ended bar and the pane's startup overlay: one state at a time", () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["Date", "setTimeout", "clearTimeout"] });
    fakeTimeouts = true;
  });
  const overlay = () => screen.queryByTestId("pane-startup-overlay");
  const wait = (ms: number) => act(() => void vi.advanceTimersByTime(ms));

  it("a session that ends before its pane painted shows the ended bar, never Failed · Retry", async () => {
    await mount({ attach: "pending" });
    wait(PANE_SPINNER_DELAY_MS);
    expect(overlay()).toHaveAttribute("data-phase", "starting");
    ptyExit("sigkill");
    expect(bar()).toHaveTextContent("Crashed · SIGKILL");
    expect(overlay()).toBeNull();
    // The start clock running out does not turn an ended session into a failed start.
    wait(PANE_START_TIMEOUT_MS);
    expect(overlay()).toBeNull();
    expect(screen.queryByTestId("pane-startup-retry")).toBeNull();
    expect(bar()).toHaveAttribute("data-outcome", "crashed");
  });

  it("an attach that failed shows Retry and no bar — until the session truly ends", async () => {
    const hx = await mount({ attach: "pending" });
    act(() => hx.probe.startup?.markFailed("Couldn't attach the terminal: host unreachable"));
    expect(overlay()).toHaveAttribute("data-phase", "failed");
    // An agent crash in a shell the pane cannot show: Retry wins, no bar.
    hook("UserPromptSubmit", "running");
    commandEnd(137);
    expect(bar()).toBeNull();
    expect(screen.getByTestId("pane-startup-retry")).toBeInTheDocument();
    // The shell itself ends: ended beats failed.
    ptyExit("clean");
    expect(bar()).toHaveAttribute("data-kind", "session");
    expect(bar()).toHaveTextContent("Crashed · exit 0");
    expect(overlay()).toBeNull();
  });

  it("after Restart the new pane gets the normal startup feedback, and no bar", async () => {
    const hx = await mount();
    ptyExit("clean");
    expect(bar()).toHaveTextContent("Ended · exit 0");
    expect(overlay()).toBeNull();
    hx.attach = "pending";
    fireEvent.click(screen.getByTestId("terminal-ended-restart"));
    await respawned();
    expect(bar()).toBeNull();
    wait(PANE_SPINNER_DELAY_MS);
    expect(overlay()).toHaveAttribute("data-phase", "starting");
    expect(overlay()).toHaveTextContent("Starting shell…");
  });
});

describe("context-worktree ended tabs", () => {
  installTerminalLayoutHarness();
  function contextTab(agent: boolean) {
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [
          {
            id: WS,
            kind: "context",
            cwd: "/work/default/content",
            status: "ready",
            contextWorktreeId: "cw_one",
          },
        ],
      },
    });
    seedTerminalTabs(TEAM, WS, [{ session: SID, name: "Reviewer" }]);
    if (agent) {
      useAgentListProjectionStore.setState({
        sessions: [
          {
            sessionId: SID,
            handle: "acme/default/reviewer",
            displayName: "Review pass",
            agentRevision: "acme/default/reviewer@7",
            role: "qa",
            queueRole: "reviewer",
            launchedBy: "lead",
            startedWith: "/resume",
            avatar: "fun:robot",
          } as AgentSessionRow,
        ],
      });
      recordAgentLaunch(SID, VERB_SPEC, VERB_LINE);
    }
  }
  for (const action of ["restart", "relaunch"] as const) {
    it(`${action} opens the sheet in that worktree with the managed session's fields`, async () => {
      contextTab(true);
      await mount();
      ptyExit("clean");
      fireEvent.click(screen.getByTestId(`terminal-ended-${action}`));
      expect(useLaunchSheetStore.getState().request).toEqual({
        teamKey: TEAM,
        where: "team-workspace",
        contextWorktreeId: "cw_one",
        agentKey: "acme/default/reviewer",
        name: "Review pass",
        role: "qa",
        queueRole: "reviewer",
        managerHandle: "lead",
        task: "/resume",
        face: "fun:robot",
      });
      expect(h.respawns).toHaveLength(0);
    });
  }
  it("a plain shell in a context worktree restarts its terminal", async () => {
    contextTab(false);
    useAgentListProjectionStore.setState({
      sessions: [{ sessionId: SID, kind: "live-pty", handle: SID } as AgentSessionRow],
    });
    await mount();
    ptyExit("clean");
    fireEvent.click(screen.getByTestId("terminal-ended-restart"));
    expect(useLaunchSheetStore.getState().request).toBeNull();
    expect(h.respawns).toHaveLength(1);
    await respawned();
  });
});
