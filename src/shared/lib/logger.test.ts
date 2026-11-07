// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";

// Unmock so we can test the real implementation
vi.unmock("@/shared/lib/logger");

// Mock the Tauri plugin before importing logger
const mockError = vi.fn().mockResolvedValue(undefined);
const mockWarn = vi.fn().mockResolvedValue(undefined);
const mockInfo = vi.fn().mockResolvedValue(undefined);
const mockDebug = vi.fn().mockResolvedValue(undefined);
const mockTrace = vi.fn().mockResolvedValue(undefined);
const mockAttachConsole = vi.fn().mockResolvedValue(() => {});

describe("logger — non-Tauri environment (plugin import fails)", () => {
  beforeEach(() => {
    vi.resetModules();
    vi.clearAllMocks();
    // Make the plugin import fail
    vi.doMock("@tauri-apps/plugin-log", () => {
      throw new Error("Not in Tauri");
    });
  });

  it("falls back to console.error", async () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    const { logger } = await import("@/shared/lib/logger");
    logger.error("auth", "sign out failed");
    expect(spy).toHaveBeenCalledWith("[auth] sign out failed");
    spy.mockRestore();
  });

  it("falls back to console.warn", async () => {
    const spy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { logger } = await import("@/shared/lib/logger");
    logger.warn("jira", "heartbeat failed");
    expect(spy).toHaveBeenCalledWith("[jira] heartbeat failed");
    spy.mockRestore();
  });

  it("falls back to console.info", async () => {
    const spy = vi.spyOn(console, "info").mockImplementation(() => {});
    const { logger } = await import("@/shared/lib/logger");
    logger.info("system", "app started");
    expect(spy).toHaveBeenCalledWith("[system] app started");
    spy.mockRestore();
  });

  it("falls back to console.debug for debug and trace", async () => {
    const spy = vi.spyOn(console, "debug").mockImplementation(() => {});
    const { logger } = await import("@/shared/lib/logger");
    logger.debug("git", "fetching");
    logger.trace("git", "payload");
    expect(spy).toHaveBeenCalledWith("[git] fetching");
    expect(spy).toHaveBeenCalledWith("[git] payload");
    spy.mockRestore();
  });
});

describe("logger — Tauri environment (plugin loads)", () => {
  beforeEach(() => {
    vi.resetModules();
    vi.clearAllMocks();
    vi.doMock("@tauri-apps/plugin-log", () => ({
      error: mockError,
      warn: mockWarn,
      info: mockInfo,
      debug: mockDebug,
      trace: mockTrace,
      attachConsole: mockAttachConsole,
    }));
  });

  it("calls plugin info with formatted message", async () => {
    const { logger, initLogging } = await import("@/shared/lib/logger");
    await initLogging();
    logger.info("update", "installed v2.0");
    expect(mockInfo).toHaveBeenCalledWith("[update] installed v2.0");
  });

  it("calls plugin error with formatted message", async () => {
    const { logger, initLogging } = await import("@/shared/lib/logger");
    await initLogging();
    logger.error("watch", "watcher crashed");
    expect(mockError).toHaveBeenCalledWith("[watch] watcher crashed");
  });

  it("formats tag in brackets", async () => {
    const { logger, initLogging } = await import("@/shared/lib/logger");
    await initLogging();
    logger.warn("deps", "git not found");
    expect(mockWarn).toHaveBeenCalledWith("[deps] git not found");
  });
});

describe("initLogging", () => {
  beforeEach(() => {
    vi.resetModules();
    vi.clearAllMocks();
    vi.doMock("@tauri-apps/plugin-log", () => ({
      error: mockError,
      warn: mockWarn,
      info: mockInfo,
      debug: mockDebug,
      trace: mockTrace,
      attachConsole: mockAttachConsole,
    }));
  });

  it("calls attachConsole when plugin is available", async () => {
    const { initLogging } = await import("@/shared/lib/logger");
    await initLogging();
    expect(mockAttachConsole).toHaveBeenCalledOnce();
  });

  it("does not throw when plugin is unavailable", async () => {
    vi.doMock("@tauri-apps/plugin-log", () => {
      throw new Error("Not in Tauri");
    });
    const { initLogging } = await import("@/shared/lib/logger");
    await expect(initLogging()).resolves.toBeUndefined();
  });

  it("is idempotent — repeat calls attach the console only once", async () => {
    const { initLogging } = await import("@/shared/lib/logger");
    // Two eager kicks (main.tsx) + a later await (App boot effect) must share
    // one attachConsole (SF-11 refactor: idempotent init).
    void initLogging();
    await initLogging();
    await initLogging();
    expect(mockAttachConsole).toHaveBeenCalledOnce();
  });
});
