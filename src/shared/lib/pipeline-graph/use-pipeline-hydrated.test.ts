// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { usePipelineHydrated } from "./use-pipeline-hydrated";
import { useConfigStore } from "@/shared/stores/config-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

describe("usePipelineHydrated", () => {
  beforeEach(() => {
    useConfigStore.setState({ initialized: false });
    useViewerStore.setState({ stateLoaded: false, scopeRecoveryComplete: false });
  });

  it("returns false when nothing has hydrated", () => {
    const { result } = renderHook(() => usePipelineHydrated());
    expect(result.current).toBe(false);
  });

  it("returns false when only config is ready", () => {
    act(() => {
      useConfigStore.setState({ initialized: true });
    });
    const { result } = renderHook(() => usePipelineHydrated());
    expect(result.current).toBe(false);
  });

  it("returns false when only config + viewer are ready", () => {
    act(() => {
      useConfigStore.setState({ initialized: true });
      useViewerStore.setState({ stateLoaded: true });
    });
    const { result } = renderHook(() => usePipelineHydrated());
    expect(result.current).toBe(false);
  });

  it("returns true once all three signals are ready", () => {
    act(() => {
      useConfigStore.setState({ initialized: true });
      useViewerStore.setState({ stateLoaded: true, scopeRecoveryComplete: true });
    });
    const { result } = renderHook(() => usePipelineHydrated());
    expect(result.current).toBe(true);
  });

  it("flips back to false if scopeRecoveryComplete is reset", () => {
    act(() => {
      useConfigStore.setState({ initialized: true });
      useViewerStore.setState({ stateLoaded: true, scopeRecoveryComplete: true });
    });
    const { result, rerender } = renderHook(() => usePipelineHydrated());
    expect(result.current).toBe(true);

    act(() => {
      useViewerStore.setState({ scopeRecoveryComplete: false });
    });
    rerender();
    expect(result.current).toBe(false);
  });
});
