// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { parseScrollIntent } from "./scroll-keys";

function makeKey(overrides: Partial<KeyboardEventInit> & { key: string }): KeyboardEvent {
  return new KeyboardEvent("keydown", overrides);
}

describe("parseScrollIntent", () => {
  // ── Cmd/Ctrl+arrows → top / bottom ──────────────────────────────

  it("Cmd+Up → top", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowUp", metaKey: true }))).toEqual({
      action: "top",
    });
  });

  it("Cmd+Down → bottom", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowDown", metaKey: true }))).toEqual({
      action: "bottom",
    });
  });

  it("Ctrl+Up → top (cross-platform)", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowUp", ctrlKey: true }))).toEqual({
      action: "top",
    });
  });

  it("Ctrl+Down → bottom (cross-platform)", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowDown", ctrlKey: true }))).toEqual({
      action: "bottom",
    });
  });

  // ── Alt+arrows → lines ─────────────────────────────────────────

  it("Alt+Up → lines -10", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowUp", altKey: true }))).toEqual({
      action: "lines",
      delta: -10,
    });
  });

  it("Alt+Down → lines +10", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowDown", altKey: true }))).toEqual({
      action: "lines",
      delta: 10,
    });
  });

  // ── PageUp / PageDown → pages ──────────────────────────────────

  it("PageUp → pages -1", () => {
    expect(parseScrollIntent(makeKey({ key: "PageUp" }))).toEqual({
      action: "pages",
      delta: -1,
    });
  });

  it("PageDown → pages +1", () => {
    expect(parseScrollIntent(makeKey({ key: "PageDown" }))).toEqual({
      action: "pages",
      delta: 1,
    });
  });

  // ── Precedence: cmd takes priority over alt ────────────────────

  it("Cmd+Alt+Up → top (cmd wins over alt)", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowUp", metaKey: true, altKey: true }))).toEqual({
      action: "top",
    });
  });

  it("Cmd+Alt+Down → bottom (cmd wins over alt)", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowDown", metaKey: true, altKey: true }))).toEqual({
      action: "bottom",
    });
  });

  // ── !cmd guards ────────────────────────────────────────────────

  it("Cmd+PageUp → null (!cmd guard)", () => {
    expect(parseScrollIntent(makeKey({ key: "PageUp", metaKey: true }))).toBeNull();
  });

  it("Cmd+PageDown → null (!cmd guard)", () => {
    expect(parseScrollIntent(makeKey({ key: "PageDown", metaKey: true }))).toBeNull();
  });

  // ── Shift modifier → null (selection extending) ────────────────

  it("Shift+Cmd+Up → null", () => {
    expect(
      parseScrollIntent(makeKey({ key: "ArrowUp", metaKey: true, shiftKey: true })),
    ).toBeNull();
  });

  it("Shift+Alt+Up → null", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowUp", altKey: true, shiftKey: true }))).toBeNull();
  });

  it("Shift+PageUp → null", () => {
    expect(parseScrollIntent(makeKey({ key: "PageUp", shiftKey: true }))).toBeNull();
  });

  it("Shift+Up → null", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowUp", shiftKey: true }))).toBeNull();
  });

  // ── Non-scroll keys → null ─────────────────────────────────────

  it("plain ArrowUp → null", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowUp" }))).toBeNull();
  });

  it("plain ArrowDown → null", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowDown" }))).toBeNull();
  });

  it("Home → null", () => {
    expect(parseScrollIntent(makeKey({ key: "Home" }))).toBeNull();
  });

  it("End → null", () => {
    expect(parseScrollIntent(makeKey({ key: "End" }))).toBeNull();
  });

  it("plain letter → null", () => {
    expect(parseScrollIntent(makeKey({ key: "j" }))).toBeNull();
  });

  it("Cmd+Left → null (not a scroll key)", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowLeft", metaKey: true }))).toBeNull();
  });

  it("Cmd+Right → null (not a scroll key)", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowRight", metaKey: true }))).toBeNull();
  });

  it("Enter → null", () => {
    expect(parseScrollIntent(makeKey({ key: "Enter" }))).toBeNull();
  });

  it("Alt+Left → null (horizontal, not scroll)", () => {
    expect(parseScrollIntent(makeKey({ key: "ArrowLeft", altKey: true }))).toBeNull();
  });
});
