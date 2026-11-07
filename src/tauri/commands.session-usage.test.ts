// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

const { mockInvoke } = vi.hoisted(() => ({ mockInvoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

import { sessionSetModelRate } from "./commands";

beforeEach(() => mockInvoke.mockReset());

describe("sessionSetModelRate", () => {
  it("sends the rate's camelCase fields under params, with its long prices as one camelCase object", async () => {
    mockInvoke.mockResolvedValueOnce(2);
    await expect(
      sessionSetModelRate({
        modelPrefix: "gpt-6-sol",
        effectiveFrom: "2026-10-17",
        inputPerMtok: 2,
        outputPerMtok: 10,
        cacheReadPerMtok: 0.1,
        long: { fromTokens: 272000, inputPerMtok: 4, outputPerMtok: 15, cacheReadPerMtok: 0.2 },
      }),
    ).resolves.toBe(2);
    // The Rust test set_model_rate_params_read_exactly_what_the_app_sends reads this payload.
    expect(mockInvoke).toHaveBeenCalledWith("session_set_model_rate", {
      params: {
        modelPrefix: "gpt-6-sol",
        effectiveFrom: "2026-10-17",
        inputPerMtok: 2,
        outputPerMtok: 10,
        cacheReadPerMtok: 0.1,
        long: { fromTokens: 272000, inputPerMtok: 4, outputPerMtok: 15, cacheReadPerMtok: 0.2 },
      },
    });
  });

  it("sends null for the prices it was not given", async () => {
    mockInvoke.mockResolvedValueOnce(0);
    await sessionSetModelRate({
      modelPrefix: "gpt-6-sol",
      effectiveFrom: "2026-10-17",
      inputPerMtok: 2,
      outputPerMtok: 10,
    });
    expect(mockInvoke).toHaveBeenCalledWith("session_set_model_rate", {
      params: {
        modelPrefix: "gpt-6-sol",
        effectiveFrom: "2026-10-17",
        inputPerMtok: 2,
        outputPerMtok: 10,
        cacheReadPerMtok: null,
        long: null,
      },
    });
  });

  it("sends a long price without a cache-read price as exactly the fields it was given", async () => {
    mockInvoke.mockResolvedValueOnce(0);
    await sessionSetModelRate({
      modelPrefix: "gpt-6-sol",
      effectiveFrom: "2026-10-17",
      inputPerMtok: 2,
      outputPerMtok: 10,
      long: { fromTokens: 272000, inputPerMtok: 4, outputPerMtok: 15 },
    });
    const sent = mockInvoke.mock.calls[0][1] as { params: { long: object } };
    expect(Object.keys(sent.params.long)).toEqual(["fromTokens", "inputPerMtok", "outputPerMtok"]);
  });
});
