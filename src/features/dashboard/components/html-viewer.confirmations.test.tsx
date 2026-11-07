// SPDX-License-Identifier: MIT
import { frameFixture } from "../../../../tests/support/fixtures/frame";
import type { FrameRecord, MintFrameArgs } from "@/tauri/commands";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import userEvent from "@testing-library/user-event";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
const mock = vi.hoisted(() => ({
  dispatch: vi.fn(),
  answer: vi.fn(),
  settle: vi.fn(),
  send: vi.fn(),
  kill: vi.fn(),
  close: vi.fn(),
  removeWorktree: vi.fn(),
  inbox: vi.fn(),
  replyRecipient: vi.fn(),
  worktreePath: "/worktrees/S-1",
  location: { org: "acme", team: "riverside", layer: "team" },
  state: "{}",
}));
vi.mock("../lib/verb-dispatch", () => ({
  verbDispatch: mock.dispatch,
  verbAnswer: mock.answer,
  verbSettle: mock.settle,
}));
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));
vi.mock("./dashboard-launch-controller", () => ({
  useRegisterDashboardLaunchController: () => ({
    sendToSession: mock.send,
    closeWorkspace: mock.close,
  }),
}));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  getHttpApiPort: async () => 9123,
  mintFrame: async (args: MintFrameArgs) => {
    const frame = frameFixture(args);
    frame.kind = mock.location.layer === "private" ? "private" : "team";
    frame.frameId = crypto.randomUUID();
    frame.origin = `http://f-${frame.frameId}.localhost:9123`;
    frame.token = `token-${frame.frameId}`;
    frame.grant = {
      ...frame.grant,
      frameId: frame.frameId,
      team: { org: "acme", team: "riverside" },
    };
    framesByOrigin.set(frame.origin, frame);
    return frame;
  },
  releaseFrame: async () => {},
  resolveFrameGrant: async (origin: string, token: string) => {
    const frame = framesByOrigin.get(origin);
    if (!frame || frame.token !== token) throw new Error("denied: frame identity");
    return frame.grant;
  },
  bridgeDocContext: async () => mock.location,
  loadAppState: async () => mock.state,
  saveAppState: async (s: string) => {
    mock.state = s;
  },
  dashboardKillByHandle: mock.kill,
  wvRosterForSession: async () => null,
  listManagedWorktrees: async () => ({
    rows: [{ kind: "story", storyId: "S-1", path: mock.worktreePath, repoPath: "/repo" }],
  }),
  removeWorktree: mock.removeWorktree,
  agentReadInbox: mock.inbox,
  inboxReplyRecipient: mock.replyRecipient,
}));
import { HtmlViewer } from "./html-viewer";
import { VerbConfirmationProvider } from "./verb-confirmation-provider";
import { EndConfirmDialog } from "@/features/viewer/components/end-confirm-dialog";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
const framesByOrigin = new Map<string, FrameRecord>();
const path = "/ctx/riverside/board.html";
const args = { target: "S-1", text: "Please review this change" };
beforeEach(() => {
  vi.clearAllMocks();
  framesByOrigin.clear();
  mock.state = "{}";
  mock.location.layer = "team";
  mock.dispatch.mockResolvedValue({ status: "pending", confirmId: "confirm", allowAlways: false });
  mock.answer.mockResolvedValue({ callId: "call", args });
  mock.settle.mockResolvedValue(undefined);
  mock.send.mockResolvedValue({ delivered: true });
  mock.close.mockResolvedValue({ closed: true });
  mock.removeWorktree.mockResolvedValue(undefined);
  mock.inbox.mockResolvedValue({ messages: [], humanUnread: 0 });
  mock.replyRecipient.mockResolvedValue(null);
  useViewerStore.setState({ workspaces: {}, softClosedWorkspaces: {} });
  globalThis.fetch = vi.fn().mockResolvedValue({ status: 200 });
  useAgentListProjectionStore.setState({
    sessions: [
      { handle: "S-1", sessionId: "pty", kind: "story", displayName: "Reviewer" },
    ] as never,
  });
});
afterEach(cleanup);
async function mount(frameCount = 1) {
  const View = ({ shown = true }: { shown?: boolean }) => (
    <VerbConfirmationProvider>
      {shown &&
        Array.from({ length: frameCount }, (_, index) => (
          <HtmlViewer
            key={index}
            filePath={index === 0 ? path : `/ctx/riverside/board-${index}.html`}
            teamContentPath="/ctx/riverside"
          />
        ))}
      <EndConfirmDialog />
    </VerbConfirmationProvider>
  );
  const view = render(<View />);
  await waitFor(() =>
    expect(document.querySelector("iframe")?.getAttribute("src")).toContain("9123"),
  );
  await waitFor(() =>
    expect(
      [...document.querySelectorAll("iframe")].every((frame) =>
        frame.getAttribute("src")?.includes("9123"),
      ),
    ).toBe(true),
  );
  const frames = [...document.querySelectorAll("iframe")].map((frame) => {
    const postMessage = vi.fn();
    const source = { postMessage } as unknown as Window;
    Object.defineProperty(frame, "contentWindow", { value: source, configurable: true });
    const call = (command = "sendToSession", values: unknown = args, rpc = false) =>
      act(() => {
        window.dispatchEvent(
          new MessageEvent("message", {
            source,
            origin: new URL(frame.src).origin,
            data: rpc
              ? {
                  token: framesByOrigin.get(new URL(frame.src).origin)!.token,
                  jsonrpc: "2.0",
                  id: 1,
                  method: "tools/call",
                  params: { name: command, arguments: values },
                }
              : {
                  token: framesByOrigin.get(new URL(frame.src).origin)!.token,
                  type: "codesense:invoke",
                  id: 1,
                  command,
                  args: values,
                },
          }),
        );
      });
    return { frame, postMessage, source, call };
  });
  return { ...frames[0], frames, unmountFrame: () => view.rerender(<View shown={false} />) };
}
const armed = () => new Promise((resolve) => setTimeout(resolve, 510));
describe("viewer → native admission → host confirmation", () => {
  it.each([
    [
      "story launch",
      "openWorkspace",
      {
        target: {
          kind: "story",
          storyId: "S-42",
          agent: "reviewer",
          machineId: "machine-7",
          session: "review",
        },
        initialPrompt: "Review the implementation",
        headed: false,
        sessionMode: "new",
        futureOption: "unknown keys stay visible",
      },
    ],
    [
      "branch launch",
      "openWorkspace",
      {
        target: { kind: "branch", repo: "/repo", branch: "feat/review", agent: "branch-reviewer" },
        reviewPrompt: "Check the changes",
        headed: false,
        operationId: "review-42",
      },
    ],
    [
      "send as you",
      "sendAsHuman",
      {
        to: "role:reviewer",
        text: "Please review",
        replyTo: "role:lead",
        attachFile: "stories/S-42.md",
        contentType: "markdown",
        summary: "Review request",
        inReplyTo: 17,
      },
    ],
    [
      "reply as you",
      "replyAsHuman",
      {
        messageId: 42,
        text: "The review is ready",
        org: "acme",
        team: "riverside",
        contentType: "plain",
        summary: "Review ready",
      },
    ],
  ] as const)("%s shows every supplied argument", async (_name, verb, values) => {
    const doc = await mount();
    doc.call(verb, values);
    await screen.findByRole("dialog");
    const details = within(screen.getByRole("region", { name: "Details" }));
    const assertValues = (value: unknown) => {
      if (value && typeof value === "object") Object.values(value).forEach(assertValues);
      else expect(details.getByText(String(value), { exact: true })).toBeTruthy();
    };
    assertValues(values);
    if (verb === "sendAsHuman")
      expect(screen.getByText(/Send as you to role:reviewer/)).toBeTruthy();
    if (verb === "replyAsHuman")
      expect(
        screen.getByText(/Reply as you to message 42 \(recipient could not be resolved\)/),
      ).toBeTruthy();
    fireEvent.click(screen.getByTestId("verb-confirmation-cancel"));
    await waitFor(() => expect(mock.answer).toHaveBeenCalledWith("confirm", false));
  });
  it.each(["role:lead", null])(
    "reply recipient uses Reply-To %s or the sender",
    async (replyTo) => {
      mock.replyRecipient.mockResolvedValue(replyTo ?? "reviewer-1");
      const doc = await mount();
      doc.call("replyAsHuman", { messageId: 42, text: "Ready" });
      await screen.findByRole("dialog");
      expect(
        screen.getByText((text) => text.endsWith(`Reply as you to ${replyTo ?? "reviewer-1"}.`)),
      ).toBeTruthy();
      expect(mock.replyRecipient).toHaveBeenCalledWith({ org: "acme", team: "riverside" }, 42);
      expect(mock.inbox).not.toHaveBeenCalled();
      fireEvent.click(screen.getByTestId("verb-confirmation-cancel"));
    },
  );
  it("previews an older message's Reply-To outside the newest 500", async () => {
    mock.inbox.mockResolvedValue({
      messages: Array.from({ length: 500 }, (_, index) => ({ messageId: index + 43 })),
      humanUnread: 0,
    });
    mock.replyRecipient.mockResolvedValue("role:original-recipient");
    const doc = await mount();
    doc.call("replyAsHuman", { messageId: 42, text: "Answer the older request" });
    await screen.findByRole("dialog");
    expect(screen.getByText(/Reply as you to role:original-recipient/)).toBeTruthy();
    expect(mock.replyRecipient).toHaveBeenCalledWith({ org: "acme", team: "riverside" }, 42);
    expect(mock.inbox).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("verb-confirmation-cancel"));
  });
  it("a failed lookup does not claim the reply goes to the sender", async () => {
    mock.replyRecipient.mockRejectedValue(new Error("Inbox unavailable"));
    const doc = await mount();
    doc.call("replyAsHuman", { messageId: 42, text: "Ready" });
    await screen.findByRole("dialog");
    expect(
      screen.getByText(/Reply as you to message 42 \(recipient could not be resolved\)/),
    ).toBeTruthy();
    expect(screen.queryByText(/message 42's sender/)).toBeNull();
    fireEvent.click(screen.getByTestId("verb-confirmation-cancel"));
  });
  it("long values expand completely without losing their argument", async () => {
    const long = "Review this carefully. ".repeat(30) + "Final instruction.";
    const doc = await mount();
    doc.call("openWorkspace", { storyId: "S-42", initialPrompt: long });
    await screen.findByRole("dialog");
    expect(screen.queryByText(long)).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Expand Initial prompt" }));
    expect(screen.getByText(long)).toBeTruthy();
    fireEvent.click(screen.getByTestId("verb-confirmation-cancel"));
  });
  it("names the resolved dashboard and kind, suspends frames, arms after 500 ms, and confirms", async () => {
    const doc = await mount();
    doc.call();
    await screen.findByRole("dialog");
    expect(screen.getByText(/Requested by.*board.html.*team riverside/)).toBeTruthy();
    expect(screen.getByText(args.text)).toBeTruthy();
    expect(doc.frame.inert).toBe(true);
    expect(doc.frame.style.pointerEvents).toBe("none");
    expect(screen.getByRole("button", { name: "Allow once" })).toBeTruthy();
    expect(screen.queryByText(/Always/)).toBeNull();
    expect(screen.queryByText(/don't ask again/i)).toBeNull();
    fireEvent.click(screen.getByTestId("verb-confirmation-confirm"));
    expect(mock.send).not.toHaveBeenCalled();
    await armed();
    fireEvent.click(screen.getByTestId("verb-confirmation-confirm"));
    await waitFor(() => expect(mock.send).toHaveBeenCalled());
    await waitFor(() =>
      expect(doc.postMessage).toHaveBeenCalledWith(
        expect.objectContaining({ data: { status: "confirmed", result: { delivered: true } } }),
        expect.any(String),
      ),
    );
    expect(mock.answer).toHaveBeenCalledWith("confirm", true);
    expect(doc.frame.inert).toBe(false);
  });
  it("Cancel is focused; Enter cancels and leaves the session running", async () => {
    const doc = await mount();
    doc.call();
    await screen.findByRole("dialog");
    const cancel = screen.getByTestId("verb-confirmation-cancel");
    await waitFor(() => expect(document.activeElement).toBe(cancel));
    await userEvent.keyboard("{Enter}");
    await waitFor(() => expect(mock.answer).toHaveBeenCalledWith("confirm", false));
    expect(mock.send).not.toHaveBeenCalled();
    expect(mock.kill).not.toHaveBeenCalled();
    await waitFor(() =>
      expect(doc.postMessage).toHaveBeenCalledWith(
        expect.objectContaining({ data: { status: "cancelled" } }),
        expect.any(String),
      ),
    );
  });
  it("each queued request starts a new dialog with Cancel focused and a fresh arm", async () => {
    mock.dispatch
      .mockResolvedValueOnce({ status: "pending", confirmId: "first", allowAlways: false })
      .mockResolvedValueOnce({ status: "pending", confirmId: "second", allowAlways: false });
    const doc = await mount(2);
    doc.call();
    await screen.findByRole("dialog");
    doc.frames[1].call("sendToSession", { target: "S-2", text: "Second queued send" });
    await waitFor(() => expect(mock.dispatch).toHaveBeenCalledTimes(2));
    await armed();
    const firstConfirm = screen.getByTestId("verb-confirmation-confirm");
    await userEvent.click(firstConfirm);
    await screen.findByText("Second queued send");
    const secondConfirm = screen.getByTestId("verb-confirmation-confirm");
    expect(secondConfirm).not.toBe(firstConfirm);
    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByTestId("verb-confirmation-cancel")),
    );
    fireEvent.click(secondConfirm);
    expect(mock.answer).not.toHaveBeenCalledWith("second", true);
    await armed();
    await userEvent.keyboard("{Enter}");
    await waitFor(() => expect(mock.answer).toHaveBeenCalledWith("second", false));
    expect(mock.send).toHaveBeenCalledTimes(1);
  });
  it("routine cancellation names the routine and does not describe a launch", async () => {
    const doc = await mount();
    doc.call("routineCancel", { key: "weekly-review" });
    await screen.findByRole("dialog", { name: "Cancel routine run?" });
    expect(screen.getByText(/Routine weekly-review/)).toBeTruthy();
    expect(screen.queryByText(/launch/i)).toBeNull();
    fireEvent.click(screen.getByTestId("verb-confirmation-cancel"));
    await waitFor(() => expect(mock.answer).toHaveBeenCalledWith("confirm", false));
  });
  it("a pending call and dialog survive the frame unmounting", async () => {
    const doc = await mount();
    doc.call();
    await screen.findByRole("dialog");
    doc.unmountFrame();
    expect(screen.getByRole("dialog")).toBeTruthy();
    await armed();
    fireEvent.click(screen.getByTestId("verb-confirmation-confirm"));
    await waitFor(() => expect(mock.send).toHaveBeenCalled());
  });
  it("MCP Apps holds tools/call until the same host confirmation resolves", async () => {
    const doc = await mount();
    doc.call("sendToSession", args, true);
    await screen.findByRole("dialog");
    expect(doc.postMessage).not.toHaveBeenCalledWith(
      expect.objectContaining({ id: 1 }),
      expect.anything(),
    );
    await armed();
    fireEvent.click(screen.getByTestId("verb-confirmation-confirm"));
    await waitFor(() =>
      expect(doc.postMessage).toHaveBeenCalledWith(
        expect.objectContaining({
          jsonrpc: "2.0",
          id: 1,
          result: expect.objectContaining({
            isError: false,
            structuredContent: { status: "confirmed", result: { delivered: true } },
          }),
        }),
        new URL(doc.frame.src).origin,
      ),
    );
  });
  it("uses the native Stop dialog and cancelling never kills", async () => {
    const doc = await mount();
    doc.call("stopSession", { handle: "S-1" });
    await screen.findByRole("alertdialog");
    expect(screen.getByText(/Stop.*Reviewer/)).toBeTruthy();
    expect(within(screen.getByRole("region", { name: "Details" })).getByText("S-1")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(mock.answer).toHaveBeenCalledWith("confirm", false));
    expect(mock.kill).not.toHaveBeenCalled();
  });
  it("uses the native terminal End dialog for close", async () => {
    const doc = await mount();
    doc.call("closeTerminalTab", { handle: "S-1" });
    await screen.findByTestId("end-confirm-dialog");
    expect(screen.getByText(/Requested by.*board.html/)).toBeTruthy();
    expect(within(screen.getByRole("region", { name: "Details" })).getByText("S-1")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(mock.answer).toHaveBeenCalledWith("confirm", false));
  });
  it.each([false, true])(
    "workspace deletion discloses its path and discardUnsaved=%s",
    async (discardUnsaved) => {
      useViewerStore.setState({
        workspaces: {
          "acme:riverside": [
            {
              id: "workspace-1",
              storyId: "S-1",
              kind: "story",
              cwd: mock.worktreePath,
              status: "ready",
            },
          ],
        },
      });
      const values = { target: "S-1", removeWorktree: true, discardUnsaved };
      mock.answer.mockResolvedValue({ callId: "call", args: values });
      const doc = await mount();
      doc.call("closeWorkspace", values);
      await screen.findByTestId("end-confirm-dialog");
      expect(screen.getByRole("alertdialog", { name: "End and delete worktree?" })).toBeTruthy();
      expect(screen.getByTestId("end-confirm-note")).toHaveTextContent(mock.worktreePath);
      if (discardUnsaved) {
        expect(screen.getByTestId("end-confirm-note")).toHaveTextContent(
          /uncommitted or unsaved changes.*lost/i,
        );
      } else {
        expect(screen.getByTestId("end-confirm-note")).not.toHaveTextContent(
          /uncommitted or unsaved changes.*lost/i,
        );
      }
      expect(mock.removeWorktree).not.toHaveBeenCalled();
      await armed();
      await userEvent.click(screen.getByRole("button", { name: "End and delete worktree" }));
      await waitFor(() =>
        expect(mock.removeWorktree).toHaveBeenCalledWith(
          "/repo",
          mock.worktreePath,
          discardUnsaved,
          true,
        ),
      );
      expect(mock.close).toHaveBeenCalledWith("S-1");
      await waitFor(() =>
        expect(doc.postMessage).toHaveBeenCalledWith(
          expect.objectContaining({
            data: { status: "confirmed", result: { closed: true, worktreeRemoved: true } },
          }),
          expect.any(String),
        ),
      );
    },
  );
  it("a workspace close with neither destructive flag keeps the ordinary End dialog", async () => {
    const values = { target: "S-1" };
    mock.answer.mockResolvedValue({ callId: "call", args: values });
    const doc = await mount();
    doc.call("closeWorkspace", values);
    await screen.findByTestId("end-confirm-dialog");
    expect(screen.getByRole("alertdialog", { name: "End workspace?" })).toBeTruthy();
    expect(screen.queryByTestId("end-confirm-note")).toBeNull();
    expect(screen.queryByText(/delete worktree|changes.*lost/i)).toBeNull();
    await armed();
    await userEvent.click(screen.getByRole("button", { name: "End" }));
    await waitFor(() => expect(mock.close).toHaveBeenCalledWith("S-1"));
    expect(mock.removeWorktree).not.toHaveBeenCalled();
  });
  it("open admission executes without a host dialog", async () => {
    mock.dispatch.mockResolvedValue({ status: "admit", callId: "call" });
    const doc = await mount();
    doc.call();
    await waitFor(() => expect(mock.send).toHaveBeenCalled());
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });
  it("private prompt-once Stop offers Always and saves only that dashboard's override", async () => {
    mock.location.layer = "private";
    mock.dispatch.mockResolvedValue({ status: "pending", confirmId: "confirm", allowAlways: true });
    mock.answer.mockResolvedValue({ callId: "call", args: { handle: "S-1" } });
    const doc = await mount();
    doc.call("stopSession", { handle: "S-1" });
    await screen.findByRole("alertdialog");
    fireEvent.click(screen.getByRole("checkbox"));
    await armed();
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() =>
      expect(JSON.parse(mock.state).dashboardPolicy.perDashboard[path].stopSession).toBe("never"),
    );
  });
});
