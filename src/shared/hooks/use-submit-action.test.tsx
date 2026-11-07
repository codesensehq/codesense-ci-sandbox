// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn() },
}));

const { toast } = await import("sonner");
const { useSubmitAction } = await import("./use-submit-action");

function Harness({ action, onDone }: { action: () => Promise<void>; onDone?: () => void }) {
  const { submitting, submit } = useSubmitAction();
  return (
    <div>
      <span data-testid="submitting">{String(submitting)}</span>
      <button
        onClick={() => void submit(action, { success: "ok!", errorPrefix: "nope", onDone })}
        data-testid="go"
      >
        go
      </button>
    </div>
  );
}

beforeEach(() => {
  vi.mocked(toast.success).mockClear();
  vi.mocked(toast.error).mockClear();
});

describe("useSubmitAction (SW-GT-6 · T8)", () => {
  it("runs the action, toasts success, and calls onDone", async () => {
    const action = vi.fn().mockResolvedValue(undefined);
    const onDone = vi.fn();
    render(<Harness action={action} onDone={onDone} />);

    await act(async () => {
      fireEvent.click(screen.getByTestId("go"));
    });

    expect(action).toHaveBeenCalledOnce();
    expect(toast.success).toHaveBeenCalledWith("ok!");
    expect(onDone).toHaveBeenCalledOnce();
    expect(screen.getByTestId("submitting")).toHaveTextContent("false");
  });

  it("toasts a prefixed error and does NOT call onDone on failure", async () => {
    const action = vi.fn().mockRejectedValue(new Error("boom"));
    const onDone = vi.fn();
    render(<Harness action={action} onDone={onDone} />);

    await act(async () => {
      fireEvent.click(screen.getByTestId("go"));
    });

    expect(toast.error).toHaveBeenCalledWith("nope: boom");
    expect(toast.success).not.toHaveBeenCalled();
    expect(onDone).not.toHaveBeenCalled();
    expect(screen.getByTestId("submitting")).toHaveTextContent("false");
  });

  it("guards re-entry — a second submit while in flight is a no-op", async () => {
    let resolve!: () => void;
    const action = vi.fn(
      () =>
        new Promise<void>((r) => {
          resolve = r;
        }),
    );
    render(<Harness action={action} />);

    // First click: in flight, not yet resolved.
    await act(async () => {
      fireEvent.click(screen.getByTestId("go"));
    });
    expect(screen.getByTestId("submitting")).toHaveTextContent("true");

    // Second click while submitting: ignored.
    await act(async () => {
      fireEvent.click(screen.getByTestId("go"));
    });
    expect(action).toHaveBeenCalledOnce();

    await act(async () => {
      resolve();
    });
    expect(screen.getByTestId("submitting")).toHaveTextContent("false");
    expect(toast.success).toHaveBeenCalledOnce();
  });
});
