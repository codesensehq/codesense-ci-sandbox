// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  IMAGE_OPACITY,
  MAX_MACHINES,
  __resetMachineAppearanceForTest,
  forgetMachineAppearance,
  removeMachineImage,
  restoreMachineAppearance,
  setMachineColor,
  setMachineImage,
  setMachineImageOpacity,
  useMachineAppearanceStore,
  type MachineImageStore,
} from "./machine-appearance";
import { MAX_IMAGE_DATA_URL_CHARS } from "./machine-image";

/** An in-memory stand-in for the app's IndexedDB image store. */
function memoryImages(): MachineImageStore & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    get: async (k) => data.get(k),
    put: async (k, v) => void data.set(k, v),
    delete: async (k) => void data.delete(k),
  };
}

const IMG = "data:image/webp;base64,UklGRg==";
let images: ReturnType<typeof memoryImages>;
const state = () => useMachineAppearanceStore.getState();

beforeEach(async () => {
  localStorage.clear();
  images = memoryImages();
  __resetMachineAppearanceForTest(images);
  await restoreMachineAppearance("alice");
});
afterEach(() => __resetMachineAppearanceForTest());

describe("machine appearance", () => {
  it("a color, image and opacity survive a relaunch", async () => {
    setMachineColor("box", "#39c5cf");
    await setMachineImage("box", IMG);
    setMachineImageOpacity("box", 0.15);
    // A relaunch: nothing in memory, everything read back from storage.
    __resetMachineAppearanceForTest();
    expect(state().prefs).toEqual({});
    await restoreMachineAppearance("alice");
    expect(state().prefs.box).toEqual({ color: "#39c5cf", imageOpacity: 0.15, hasImage: true });
    expect(state().images.box).toBe(IMG);
  });

  it("keeps each profile's choices apart", async () => {
    setMachineColor("box", "#39c5cf");
    await setMachineImage("box", IMG);
    await restoreMachineAppearance("bob");
    expect(state().prefs).toEqual({});
    expect(state().images).toEqual({});
    setMachineColor("box", "#f778ba");
    await restoreMachineAppearance("alice");
    expect(state().prefs.box?.color).toBe("#39c5cf");
    expect(state().images.box).toBe(IMG);
  });

  it("holds the opacity to its range", () => {
    setMachineImageOpacity("box", 5);
    expect(state().prefs.box?.imageOpacity).toBe(IMAGE_OPACITY.max);
    setMachineImageOpacity("box", 0);
    expect(state().prefs.box?.imageOpacity).toBe(IMAGE_OPACITY.min);
  });

  it("refuses an image that is too large or not an image", async () => {
    await expect(
      setMachineImage("box", `data:image/png;base64,${"A".repeat(MAX_IMAGE_DATA_URL_CHARS)}`),
    ).rejects.toThrow(/too large/);
    await expect(setMachineImage("box", "data:text/html,<b>x</b>")).rejects.toThrow(
      /isn't an image/,
    );
    expect(images.data.size).toBe(0);
    expect(state().prefs.box).toBeUndefined();
  });

  it("removing the image drops it from storage and the pane", async () => {
    await setMachineImage("box", IMG);
    setMachineImageOpacity("box", 0.15);
    await removeMachineImage("box");
    expect(images.data.size).toBe(0);
    expect(state().images.box).toBeUndefined();
    expect(state().prefs.box).toBeUndefined();
  });

  it("a removed machine is forgotten", async () => {
    setMachineColor("box", "#39c5cf");
    await setMachineImage("box", IMG);
    forgetMachineAppearance("box");
    await Promise.resolve();
    expect(state().prefs.box).toBeUndefined();
    expect(JSON.parse(localStorage.getItem("codesense.machine-appearance.alice")!)).toEqual({});
  });

  it("keeps choices made before any profile is active", async () => {
    __resetMachineAppearanceForTest(images);
    await restoreMachineAppearance(null);
    setMachineColor("box", "#39c5cf");
    __resetMachineAppearanceForTest(images);
    await restoreMachineAppearance(null);
    expect(state().prefs.box?.color).toBe("#39c5cf");
  });

  it("past the cap, forgets the least recently changed machines (and their images)", async () => {
    await setMachineImage("m0", IMG);
    for (let i = 1; i <= MAX_MACHINES; i++) setMachineColor(`m${i}`, "#39c5cf");
    expect(Object.keys(state().prefs)).toHaveLength(MAX_MACHINES);
    expect(state().prefs.m0).toBeUndefined();
    expect(state().prefs[`m${MAX_MACHINES}`]).toBeDefined();
    await Promise.resolve();
    expect(images.data.size).toBe(0);
    // Changing one again makes it the newest.
    setMachineColor("m1", "#f778ba");
    setMachineColor("extra", "#f778ba");
    expect(state().prefs.m1).toBeDefined();
    expect(state().prefs.m2).toBeUndefined();
  });

  it("an image saved as the profile switches lands with the profile that picked it", async () => {
    let release!: () => void;
    const slow: typeof images = {
      ...images,
      put: (k, v) =>
        new Promise<void>((r) => {
          release = () => {
            images.data.set(k, v);
            r();
          };
        }),
    };
    __resetMachineAppearanceForTest(slow);
    await restoreMachineAppearance("alice");
    const saving = setMachineImage("box", IMG);
    await restoreMachineAppearance("bob");
    release();
    await saving;
    expect(state().images).toEqual({});
    expect(state().prefs.box).toBeUndefined();
    __resetMachineAppearanceForTest(images);
    await restoreMachineAppearance("alice");
    expect(state().prefs.box?.hasImage).toBe(true);
    expect(state().images.box).toBe(IMG);
  });

  it("says what to do when there's no room left for an image", async () => {
    __resetMachineAppearanceForTest({
      ...images,
      put: async () => {
        throw new DOMException("full", "QuotaExceededError");
      },
    });
    await restoreMachineAppearance("alice");
    await expect(setMachineImage("box", IMG)).rejects.toThrow(/no room left/);
    expect(state().prefs.box).toBeUndefined();
  });

  it("ignores corrupt saved data", async () => {
    localStorage.setItem(
      "codesense.machine-appearance.carol",
      JSON.stringify({ box: { color: "red", imageOpacity: "x" }, ok: { color: "#abcdef" } }),
    );
    await restoreMachineAppearance("carol");
    expect(state().prefs).toEqual({ ok: { color: "#abcdef" } });
  });

  it("a missing image reads as no image", async () => {
    localStorage.setItem(
      "codesense.machine-appearance.dave",
      JSON.stringify({ box: { hasImage: true } }),
    );
    await restoreMachineAppearance("dave");
    expect(state().images).toEqual({});
  });
});
