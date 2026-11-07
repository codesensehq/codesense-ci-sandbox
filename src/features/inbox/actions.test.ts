// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { toast } from "sonner";

vi.mock("sonner", () => ({ toast: vi.fn() }));
vi.mock("@/features/weft/lib/locate-agent", () => ({ locateAgentByAgentId: vi.fn() }));
const sendAsHuman = vi.hoisted(() => vi.fn());
vi.mock("./api", async (orig) => ({ ...(await orig<typeof import("./api")>()), sendAsHuman }));

import { sendFromInbox, teamFor } from "./actions";
import { FX, fixtureMessages } from "./fixtures";
import { HUMAN, buildDirectory } from "./model";

describe("teamFor", () => {
  const A = { org: "o", team: "a" };
  const B = { org: "o", team: "b" };
  const FALLBACK = { org: "o", team: "active" };
  const [first, second] = fixtureMessages();
  // The human's latest message went to someone in team B.
  const messages = [
    { ...first!, team: A, from: FX.impl, to: HUMAN },
    { ...second!, team: B, from: HUMAN, to: FX.rev, toRole: null },
  ];

  it("files a message with a party where the latest message with it lives", () => {
    expect(teamFor(FX.impl, messages, FALLBACK)).toEqual(A);
    expect(teamFor(FX.rev, messages, FALLBACK)).toEqual(B);
    expect(teamFor(FX.qa, messages, FALLBACK)).toEqual(FALLBACK);
  });

  it("sends a note to yourself to the active team, not wherever you last wrote", () => {
    expect(teamFor(HUMAN, messages, FALLBACK)).toEqual(FALLBACK);
  });
});

describe("sendFromInbox", () => {
  const A = { org: "o", team: "a" };
  const B = { org: "o", team: "b" };
  const [first, second] = fixtureMessages();
  const messages = [
    { ...first!, team: A, from: FX.impl, to: HUMAN, toRole: null },
    { ...second!, team: B, from: HUMAN, to: null, toRole: "reviewer" },
  ];
  const dir = buildDirectory([], messages, {});
  const send = (opts: Partial<Parameters<typeof sendFromInbox>[0]>) =>
    sendFromInbox({ to: FX.impl, text: "hi", messages, fallbackTeam: A, dir, roles: [], ...opts });

  it("files a reply in the team of the message it answers", async () => {
    sendAsHuman.mockResolvedValue({ status: "sent", messageId: 1 });
    const answered = { ...first!, team: B, id: 42 };
    expect(await send({ inReplyTo: answered })).toBe(true);
    expect(sendAsHuman).toHaveBeenCalledWith(B, FX.impl, "hi", 42);
  });

  it("files a #role message where that queue's latest message lives", async () => {
    sendAsHuman.mockResolvedValue({ status: "sent", messageId: 1 });
    expect(teamFor("#reviewer", messages, A)).toEqual(B);
    await send({ to: "#reviewer" });
    expect(sendAsHuman).toHaveBeenCalledWith(B, "#reviewer", "hi", null);
  });

  it("keeps the first line's indentation and drops only blank lines around the message", async () => {
    sendAsHuman.mockResolvedValue({ status: "sent", messageId: 1 });
    await send({ text: "\n\n    indented();\n  - item\n\n" });
    expect(sendAsHuman).toHaveBeenCalledWith(A, FX.impl, "    indented();\n  - item", null);
    sendAsHuman.mockClear();
    expect(await send({ text: "  \n \n" })).toBe(false);
    expect(sendAsHuman).not.toHaveBeenCalled();
  });

  it("reports a refused send as not sent, and tells the surface why, once: no toast as well", async () => {
    sendAsHuman.mockResolvedValue({ status: "invalid", message: "no such session" });
    const onFailed = vi.fn();
    vi.mocked(toast).mockClear();
    expect(await send({ onFailed })).toBe(false);
    expect(onFailed).toHaveBeenCalledWith("no such session", false);
    expect(toast).not.toHaveBeenCalled();
  });

  it("tells the surface a send that could not reach the backend can be tried again", async () => {
    sendAsHuman.mockRejectedValue(new Error("connection reset"));
    const onFailed = vi.fn();
    expect(await send({ onFailed })).toBe(false);
    expect(onFailed).toHaveBeenCalledWith("connection reset", true);
    // No team to send in is no better on a second try.
    onFailed.mockClear();
    expect(await send({ onFailed, fallbackTeam: null, to: FX.qa })).toBe(false);
    expect(onFailed).toHaveBeenCalledWith("no team to send it in", false);
  });

  it("a quiet send to a role still says where it queued, and one to an idle agent that it woke it", async () => {
    sendAsHuman.mockResolvedValue({ status: "sent", messageId: 1 });
    vi.mocked(toast).mockClear();
    expect(await send({ quiet: true, to: "#reviewer" })).toBe(true);
    expect(toast).toHaveBeenCalledExactlyOnceWith(
      "Queued on #reviewer. No live holder yet; it stays queued until an agent claims it.",
    );
    vi.mocked(toast).mockClear();
    const idle = buildDirectory(
      [
        {
          handle: FX.impl,
          displayName: "impl",
          state: "idle",
          machine: null,
          role: null,
          launchedBy: null,
          unread: 0,
          oldestUnreadAt: null,
        } as never,
      ],
      messages,
      {},
    );
    expect(await send({ quiet: true, dir: idle })).toBe(true);
    expect(toast).toHaveBeenCalledExactlyOnceWith("Delivered. impl was idle and has been woken.");
  });

  it("toasts a failure when no surface says it", async () => {
    sendAsHuman.mockRejectedValue(new Error("offline"));
    vi.mocked(toast).mockClear();
    expect(await send({})).toBe(false);
    expect(toast).toHaveBeenCalledExactlyOnceWith("Could not send: offline");
  });

  const offlineDir = (machine: string) =>
    buildDirectory(
      [
        {
          handle: FX.impl,
          displayName: "impl",
          state: "offline",
          machine,
          role: null,
          launchedBy: null,
          unread: 0,
          oldestUnreadAt: null,
        } as never,
      ],
      messages,
      {},
    );

  it("a quiet send says nothing once it reaches the agent, but says when it waits for its machine", async () => {
    sendAsHuman.mockResolvedValue({ status: "sent", messageId: 1 });
    vi.mocked(toast).mockClear();
    expect(await send({ quiet: true })).toBe(true);
    expect(toast).not.toHaveBeenCalled();
    expect(await send({ quiet: true, dir: offlineDir("build-box") })).toBe(true);
    expect(toast).toHaveBeenCalledExactlyOnceWith(
      "Queued for impl. It's delivered once build-box reconnects.",
    );
  });

  it("names This Mac by its label, never as a remote machine", () => {
    const dir = offlineDir("This Mac");
    expect(dir.get(FX.impl)?.machine).toBeNull();
  });

  it("confirms a message to a session whose machine is offline as queued until it reconnects", async () => {
    sendAsHuman.mockResolvedValue({ status: "sent", messageId: 1 });
    const offline = buildDirectory(
      [
        {
          handle: FX.impl,
          displayName: "impl",
          state: "offline",
          machine: "build-box",
          role: null,
          launchedBy: null,
          unread: 0,
          oldestUnreadAt: null,
        } as never,
      ],
      messages,
      {},
    );
    vi.mocked(toast).mockClear();
    expect(await send({ dir: offline })).toBe(true);
    expect(toast).toHaveBeenCalledWith(expect.stringMatching(/^Queued for .*build-box reconnects/));
  });
});
