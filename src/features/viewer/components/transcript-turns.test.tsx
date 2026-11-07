// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { render, screen } from "@/test-utils";

import type { SessionTranscript } from "@/shared/lib/session-transcript";

import { TranscriptTurns } from "./transcript-turns";

describe("TranscriptTurns — rich renderer (SW-OR-16 AC-3)", () => {
  it("marks a tool call that reported an error", () => {
    const t: SessionTranscript = {
      turns: [
        {
          role: "assistant",
          tool_calls: [{ name: "Bash", input: "ls", result: "nope", is_error: true }],
        },
      ],
    };
    render(<TranscriptTurns transcript={t} />);
    expect(screen.getByTestId("tool-call-error")).toBeInTheDocument();
  });

  it("windows a pathological turn count to the tail with a visible (non-silent) notice", () => {
    // 250 turns > MAX_RENDERED_TURNS (200): only the latest 200 mount, with a notice for the rest —
    // the FE half of "no UI-thread block": DOM nodes stay bounded, nothing is silently dropped.
    const turns = Array.from({ length: 250 }, (_, i) => ({
      role: "assistant",
      text: `turn ${i}`,
    }));
    render(<TranscriptTurns transcript={{ turns }} />);
    expect(screen.getByTestId("transcript-windowed-notice")).toHaveTextContent(
      "50 earlier turns hidden",
    );
    expect(screen.getAllByTestId("transcript-turn")).toHaveLength(200);
    // The tail is what's kept (it holds the final answer/outcome).
    expect(screen.getByText("turn 249")).toBeInTheDocument();
    expect(screen.queryByText("turn 0")).not.toBeInTheDocument();
  });

  it("renders no windowing notice for a small transcript", () => {
    render(<TranscriptTurns transcript={{ turns: [{ role: "assistant", text: "hi" }] }} />);
    expect(screen.queryByTestId("transcript-windowed-notice")).not.toBeInTheDocument();
    expect(screen.getAllByTestId("transcript-turn")).toHaveLength(1);
  });
});
