// SPDX-License-Identifier: MIT
import { useMemo } from "react";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { useRetiringFrames } from "./use-retiring-frames";
import { acknowledgeFlush, registerFrame } from "../lib/frame-lifecycle";
const off: Array<() => void> = [];
afterEach(() => {
  cleanup();
  off.splice(0).forEach((dispose) => dispose());
  vi.useRealTimers();
});
function Host({ open, count = 1 }: { open: boolean; count?: number }) {
  const values = useMemo(
    () => (open ? (count > 1 ? ["frame", "other"] : ["frame"]) : []),
    [open, count],
  );
  const retained = useRetiringFrames(
    values,
    (id) => id,
    (frame, id) => frame.frameId === id,
  );
  return (
    <>
      {retained.map((id) => (
        <div key={id} data-testid="frame" hidden={!open} inert={!open} />
      ))}
    </>
  );
}
it("keeps the same hidden inert host on close until its ack, then unmounts", async () => {
  const post = vi.fn();
  off.push(registerFrame({ frameId: "frame", dashboard: "fixture", post, hide: vi.fn() }));
  const view = render(<Host open />);
  const original = view.getByTestId("frame");
  view.rerender(<Host open={false} />);
  expect(view.getByTestId("frame")).toBe(original);
  expect(original).toHaveAttribute("hidden");
  expect(original).toHaveAttribute("inert");
  expect(post.mock.calls[0][0].type).toBe("codesense:flush");
  await act(async () => acknowledgeFlush("frame", post.mock.calls[0][0].id));
  await waitFor(() => expect(view.queryByTestId("frame")).toBeNull());
  expect(post.mock.calls.at(-1)?.[0].type).toBe("codesense:teardown");
});

it("waits for the outgoing flush before reminting a returning frame", async () => {
  const post = vi.fn(),
    remint = vi.fn();
  off.push(registerFrame({ frameId: "frame", dashboard: "fixture", post, hide: vi.fn(), remint }));
  const view = render(<Host open />);
  view.rerender(<Host open={false} />);
  expect(post.mock.calls[0][0].type).toBe("codesense:flush");
  view.rerender(<Host open />);
  expect(remint).not.toHaveBeenCalled();
  await act(async () => acknowledgeFlush("frame", post.mock.calls[0][0].id));
  await waitFor(() => expect(remint).toHaveBeenCalledOnce());
  expect(view.getByTestId("frame")).toBeInTheDocument();
});

it.each(["ack", "deadline"])(
  "keeps the pending remint across list changes until %s",
  async (settlement) => {
    vi.useFakeTimers();
    const post = vi.fn(),
      remint = vi.fn();
    off.push(
      registerFrame({ frameId: "frame", dashboard: "fixture", post, hide: vi.fn(), remint }),
    );
    const view = render(<Host open />);
    view.rerender(<Host open={false} />);
    view.rerender(<Host open />);
    view.rerender(<Host open count={2} />);
    await act(async () => {
      if (settlement === "ack") acknowledgeFlush("frame", post.mock.calls[0][0].id);
      else await vi.advanceTimersByTimeAsync(300);
    });
    expect(remint).toHaveBeenCalledOnce();
    expect(post.mock.calls.filter(([message]) => message.type === "codesense:flush")).toHaveLength(
      1,
    );
  },
);

it("removes a returning frame if it closes again before its flush settles", async () => {
  const post = vi.fn(),
    remint = vi.fn();
  off.push(registerFrame({ frameId: "frame", dashboard: "fixture", post, hide: vi.fn(), remint }));
  const view = render(<Host open />);
  view.rerender(<Host open={false} />);
  view.rerender(<Host open />);
  view.rerender(<Host open count={2} />);
  view.rerender(<Host open={false} />);
  await act(async () => acknowledgeFlush("frame", post.mock.calls[0][0].id));
  expect(remint).not.toHaveBeenCalled();
  expect(view.queryAllByTestId("frame")).toHaveLength(0);
});

it("does not remint after the whole host unmounts", async () => {
  const post = vi.fn(),
    remint = vi.fn();
  off.push(registerFrame({ frameId: "frame", dashboard: "fixture", post, hide: vi.fn(), remint }));
  const view = render(<Host open />);
  view.rerender(<Host open={false} />);
  view.rerender(<Host open />);
  view.unmount();
  await act(async () => acknowledgeFlush("frame", post.mock.calls[0][0].id));
  expect(remint).not.toHaveBeenCalled();
});
