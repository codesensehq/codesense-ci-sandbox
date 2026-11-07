// SPDX-License-Identifier: MIT
import { create } from "zustand";

import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { MAX_IMAGE_DATA_URL_CHARS } from "./machine-image";

/**
 * How the user dresses each remote Machine: its color (tints its workspace
 * pills, tab glyphs and terminals) and an optional background image drawn
 * faintly behind its terminals. A personal, cosmetic choice: kept per profile
 * on this computer, never shared with the team.
 *
 * The color and image opacity live in this browser's storage; the image —
 * already shrunk by `shrinkImageFile` — lives in the app's
 * IndexedDB, which has room for pictures. When either store is missing or
 * blocked a Machine just wears its default look.
 */

export interface MachineAppearancePrefs {
  /** The user's color, `#rrggbb`; absent = the Machine's default. */
  color?: string;
  /** How strongly the background image shows (see {@link IMAGE_OPACITY}). */
  imageOpacity?: number;
  /** A background image is saved for this Machine. */
  hasImage?: boolean;
}

interface MachineAppearanceState {
  prefs: Record<string, MachineAppearancePrefs>;
  /** Loaded background images, as `data:` URLs, by Machine. */
  images: Record<string, string>;
  /** A Machine whose appearance was asked for from elsewhere (a tab's menu):
   *  its Machines row opens its Appearance popover. */
  requested: string | null;
}

export const useMachineAppearanceStore = create<MachineAppearanceState>(() => ({
  prefs: {},
  images: {},
  requested: null,
}));

/** Ask the Machines tab to open `machineId`'s appearance (`null` once shown). */
export function requestMachineAppearance(machineId: string | null): void {
  useMachineAppearanceStore.setState({ requested: machineId });
}

/** How strongly a background image shows. Capped so dim terminal text stays
 *  readable over the image's brightest pixel (see `MachineCanvas`). */
export const IMAGE_OPACITY = { min: 0.04, max: 0.18, default: 0.1 } as const;

const STORAGE_PREFIX = "codesense.machine-appearance.";
/** The storage key's owner while no profile is active (first run, a profile
 *  store that hasn't answered yet): choices made then are kept, not lost. */
const NO_PROFILE = "_local";
/** Machines remembered per profile; past it the least recently changed go. */
export const MAX_MACHINES = 256;

/** Where background images are kept. */
export interface MachineImageStore {
  get(key: string): Promise<string | undefined>;
  put(key: string, dataUrl: string): Promise<void>;
  delete(key: string): Promise<void>;
}

function request<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error ?? new Error("storage request failed"));
  });
}

/** The app's IndexedDB store of machine background images. */
function indexedDbImageStore(): MachineImageStore {
  let db: Promise<IDBDatabase> | null = null;
  const open = () => {
    db ??= new Promise((resolve, reject) => {
      if (typeof indexedDB === "undefined") {
        reject(new Error("This app can't store images here"));
        return;
      }
      const req = indexedDB.open("codesense-machine-appearance", 1);
      req.onupgradeneeded = () => req.result.createObjectStore("images");
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error ?? new Error("Couldn't open image storage"));
    });
    db.catch(() => {
      db = null;
    });
    return db;
  };
  const store = async (mode: IDBTransactionMode) =>
    (await open()).transaction("images", mode).objectStore("images");
  return {
    async get(key) {
      const value: unknown = await request((await store("readonly")).get(key));
      return typeof value === "string" ? value : undefined;
    },
    async put(key, dataUrl) {
      await request((await store("readwrite")).put(dataUrl, key));
    },
    async delete(key) {
      await request((await store("readwrite")).delete(key));
    },
  };
}

let imageStore: MachineImageStore = indexedDbImageStore();
/** The profile whose choices are loaded; `undefined` until the first restore. */
let owner: string | null | undefined = undefined;

const ownerKey = (who: string | null | undefined) => who ?? NO_PROFILE;
const imageKey = (who: string | null | undefined, machineId: string) =>
  `${ownerKey(who)}\u0000${machineId}`;

function clampOpacity(v: number): number {
  return Math.min(IMAGE_OPACITY.max, Math.max(IMAGE_OPACITY.min, v));
}

function read(key: string): Record<string, MachineAppearancePrefs> {
  try {
    const raw = localStorage.getItem(key);
    const parsed: unknown = raw && raw.length <= 64 * 1024 ? JSON.parse(raw) : null;
    if (!parsed || typeof parsed !== "object") return {};
    const out: Record<string, MachineAppearancePrefs> = {};
    // Newest last (see `update`), so a list over the cap keeps the newest.
    for (const [id, value] of Object.entries(parsed).slice(-MAX_MACHINES)) {
      const v = value as Record<string, unknown> | null;
      if (!v || typeof v !== "object") continue;
      const prefs: MachineAppearancePrefs = {};
      if (typeof v.color === "string" && /^#[0-9a-f]{6}$/i.test(v.color)) prefs.color = v.color;
      if (typeof v.imageOpacity === "number" && Number.isFinite(v.imageOpacity))
        prefs.imageOpacity = clampOpacity(v.imageOpacity);
      if (v.hasImage === true) prefs.hasImage = true;
      if (Object.keys(prefs).length > 0) out[id] = prefs;
    }
    return out;
  } catch {
    return {};
  }
}

function save(who: string | null | undefined, prefs: Record<string, MachineAppearancePrefs>) {
  try {
    localStorage.setItem(STORAGE_PREFIX + ownerKey(who), JSON.stringify(prefs));
  } catch {
    // Storage full or blocked: the choice holds until the app closes.
  }
}

/** `prefs` with `machineId` changed by `change` and moved to the end (the most
 *  recently changed); over {@link MAX_MACHINES}, the oldest are dropped, their
 *  images with them. */
function changed(
  who: string | null | undefined,
  prefs: Record<string, MachineAppearancePrefs>,
  machineId: string,
  change: (prev: MachineAppearancePrefs) => MachineAppearancePrefs,
): Record<string, MachineAppearancePrefs> {
  const value = change(prefs[machineId] ?? {});
  const next = { ...prefs };
  delete next[machineId];
  if (!Object.values(value).every((v) => v === undefined)) next[machineId] = value;
  const ids = Object.keys(next);
  for (const old of ids.slice(0, Math.max(0, ids.length - MAX_MACHINES))) {
    if (next[old].hasImage) void imageStore.delete(imageKey(who, old)).catch(() => undefined);
    delete next[old];
  }
  return next;
}

function update(
  machineId: string,
  change: (prev: MachineAppearancePrefs) => MachineAppearancePrefs,
) {
  const next = changed(owner, useMachineAppearanceStore.getState().prefs, machineId, change);
  useMachineAppearanceStore.setState({ prefs: next });
  save(owner, next);
}

/** Switch to `next`'s saved appearances (a profile switch, or the first
 *  load); its images arrive as they load. */
export async function restoreMachineAppearance(next: string | null): Promise<void> {
  if (next === owner) return;
  owner = next;
  const prefs = read(STORAGE_PREFIX + ownerKey(next));
  useMachineAppearanceStore.setState({ prefs, images: {} });
  const wanted = Object.entries(prefs).filter(([, p]) => p.hasImage);
  await Promise.all(
    wanted.map(async ([id]) => {
      const key = imageKey(next, id);
      const dataUrl = await imageStore.get(key).catch(() => undefined);
      if (!dataUrl || owner !== next) return;
      useMachineAppearanceStore.setState((s) => ({ images: { ...s.images, [id]: dataUrl } }));
    }),
  );
}

/** Set `machineId`'s color (`null` returns it to its default). */
export function setMachineColor(machineId: string, color: string | null): void {
  update(machineId, (p) => ({ ...p, color: color ?? undefined }));
}

/** How strongly `machineId`'s background image shows. */
export function setMachineImageOpacity(machineId: string, opacity: number): void {
  update(machineId, (p) => ({ ...p, imageOpacity: clampOpacity(opacity) }));
}

/** The words for a failed image save: a full store says what to do about it. */
function saveError(e: unknown): Error {
  const name = e instanceof DOMException || e instanceof Error ? e.name : "";
  if (name === "QuotaExceededError")
    return new Error(
      "There's no room left to keep this image — remove another machine's background, or choose a smaller image",
    );
  return new Error("Couldn't keep this image on this computer — try again");
}

/** Save `dataUrl` (from `shrinkImageFile`) as `machineId`'s background. The
 *  profile it was picked under keeps it, even if the profile switches while
 *  it saves. */
export async function setMachineImage(machineId: string, dataUrl: string): Promise<void> {
  if (!dataUrl.startsWith("data:image/")) throw new Error("That file isn't an image");
  if (dataUrl.length > MAX_IMAGE_DATA_URL_CHARS) throw new Error("That image is too large to use");
  const who = owner;
  try {
    await imageStore.put(imageKey(who, machineId), dataUrl);
  } catch (e) {
    throw saveError(e);
  }
  const mark = (p: MachineAppearancePrefs) => ({ ...p, hasImage: true });
  if (owner !== who) {
    // The profile changed mid-save: record it under the profile that picked it.
    save(who, changed(who, read(STORAGE_PREFIX + ownerKey(who)), machineId, mark));
    return;
  }
  useMachineAppearanceStore.setState((s) => ({ images: { ...s.images, [machineId]: dataUrl } }));
  update(machineId, mark);
}

/** Drop `machineId`'s background image. */
export async function removeMachineImage(machineId: string): Promise<void> {
  await imageStore.delete(imageKey(owner, machineId)).catch(() => undefined);
  useMachineAppearanceStore.setState((s) => {
    const images = { ...s.images };
    delete images[machineId];
    return { images };
  });
  update(machineId, (p) => ({ ...p, hasImage: undefined, imageOpacity: undefined }));
}

/** Forget everything about a Machine that was removed. */
export function forgetMachineAppearance(machineId: string): void {
  const had = useMachineAppearanceStore.getState().prefs[machineId];
  if (!had) return;
  if (had.hasImage) void removeMachineImage(machineId);
  update(machineId, () => ({}));
}

export interface MachineAppearance {
  /** The background image, a `data:` URL, when one is set and loaded. */
  image: string | null;
  imageOpacity: number;
}

/** `machineId`'s background image and how strongly it shows. */
export function useMachineBackground(machineId: string): MachineAppearance {
  const image = useMachineAppearanceStore((s) => s.images[machineId] ?? null);
  const opacity = useMachineAppearanceStore((s) => s.prefs[machineId]?.imageOpacity);
  return { image, imageOpacity: opacity ?? IMAGE_OPACITY.default };
}

let wired = false;
/** Follow the active profile. Idempotent. */
export function wireMachineAppearance(): void {
  if (wired) return;
  wired = true;
  const follow = () =>
    void restoreMachineAppearance(
      useLocalProfileStore.getState().status?.active?.principal ?? null,
    );
  follow();
  useLocalProfileStore.subscribe(follow);
}

/** Test-only: use `store` for images and forget the profile. */
export function __resetMachineAppearanceForTest(store?: MachineImageStore): void {
  if (store) imageStore = store;
  owner = undefined;
  useMachineAppearanceStore.setState({ prefs: {}, images: {}, requested: null });
}
