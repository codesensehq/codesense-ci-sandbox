// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

const openAgentCap = vi.hoisted(() => vi.fn());
vi.mock("@/features/agents/agents-virtual-tab", () => ({ openAgentCap }));
const content = vi.hoisted(() => vi.fn((_props: unknown) => null));
vi.mock("@/features/weft/components/inbox-message-content", () => ({
  InboxMessageContent: (props: unknown) => content(props),
}));

import { fixtureMessages } from "../fixtures";
import { HUMAN } from "../model";
import { MessageBody } from "./primitives";

describe("MessageBody", () => {
  const base = fixtureMessages()[0]!;
  const html = {
    ...base,
    contentType: "html" as const,
    via: null,
    toRole: null,
    text: "<form></form>",
  };

  it("renders an agent's HTML to the human as its live dashboard", () => {
    render(<MessageBody m={{ ...html, from: "agent-7", to: HUMAN, id: 42 }} liveDashboards />);
    expect(content).toHaveBeenLastCalledWith(
      expect.objectContaining({ live: { team: base.team, messageId: 42, sender: "agent-7" } }),
    );
  });

  it("keeps HTML between agents, or from the human, static", () => {
    render(<MessageBody m={{ ...html, from: "agent-7", to: HUMAN }} />);
    // Outside the human's own inbox (a tab, an agent's page, Mailspool, Wire): static.
    expect(content).toHaveBeenLastCalledWith(expect.objectContaining({ live: null }));
    render(<MessageBody m={{ ...html, from: "agent-7", to: "agent-8" }} liveDashboards />);
    expect(content).toHaveBeenLastCalledWith(expect.objectContaining({ live: null }));
    render(<MessageBody m={{ ...html, from: HUMAN, to: "agent-8" }} liveDashboards />);
    expect(content).toHaveBeenLastCalledWith(expect.objectContaining({ live: null }));
  });
});

it("opens a spend Needs-you message at its saved agent cap anchor", async () => {
  const m = {
    ...fixtureMessages()[0]!,
    onClick: { type: "goto-agent" as const, agent_id: "acme/api/backend/worker#cap" },
  };
  render(<MessageBody m={m} />);
  fireEvent.click(screen.getByRole("button", { name: "Review spend cap" }));
  await waitFor(() => expect(openAgentCap).toHaveBeenCalledWith("acme/api/backend/worker"));
});

it("a routine rate notice preserves the routine cap destination", async () => {
  const base = fixtureMessages()[0]!;
  const target = "acme/api/backend/worker#routine:acme/api/backend/daily";
  render(
    <MessageBody
      m={{ ...base, via: null, onClick: { type: "goto-agent", agent_id: `${target}#cap` } }}
    />,
  );
  fireEvent.click(screen.getAllByRole("button", { name: "Review spend cap" }).at(-1)!);
  await waitFor(() => expect(openAgentCap).toHaveBeenCalledWith(target));
});
