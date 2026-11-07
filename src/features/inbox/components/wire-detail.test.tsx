// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

vi.mock("@/features/weft/components/inbox-message-content", () => ({
  InboxMessageContent: () => null,
}));

import { FX, fixtureMessages } from "../fixtures";
import { buildDirectory } from "../model";
import { InboxViewProvider } from "./inbox-view-context";
import { WireDetail } from "./wire-view";

describe("WireDetail on a saved agent's page", () => {
  it("answers its own session, not another agent's queue the session wrote to", () => {
    const m = {
      ...fixtureMessages()[0]!,
      from: FX.impl,
      to: null,
      toRole: "reviewer",
      replyTo: null,
      via: null,
    };
    const onReply = vi.fn();
    const dir = buildDirectory([], [m], {});
    render(
      <InboxViewProvider
        value={{
          me: "human",
          dir,
          messages: [m],
          roles: [],
          held: ["implementer"],
          fallbackTeam: m.team,
          source: "live",
          replyTo: onReply,
          goTo: () => {},
        }}
      >
        <WireDetail
          m={m}
          me={new Set([FX.impl])}
          dir={dir}
          held={["implementer"]}
          onClose={() => {}}
          onReply={onReply}
          onGo={() => {}}
        />
      </InboxViewProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: /Reply/ }));
    expect(onReply).toHaveBeenCalledWith(FX.impl, m);
  });
});
