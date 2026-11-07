// SPDX-License-Identifier: MIT
import { useTerminalThemeConfig } from "@/themes/use-terminal-theme";
import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type RefObject,
} from "react";
import { Check, ImagePlus, Plus } from "lucide-react";

import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { getErrorMessage } from "@/shared/lib/error-message";
import type { MachineSummary } from "@/tauri/commands";
import {
  IMAGE_OPACITY,
  removeMachineImage,
  requestMachineAppearance,
  setMachineColor,
  setMachineImage,
  setMachineImageOpacity,
  useMachineAppearanceStore,
  useMachineBackground,
} from "../lib/machine-appearance";
import { ACCEPTED_IMAGE_TYPES, shrinkImageFile } from "../lib/machine-image";
import {
  MACHINE_PALETTE,
  clampMachineColor,
  defaultMachineColor,
  useMachineColor,
} from "../lib/machine-color";
import { MachineCanvas, machineWatermarkText, tintPaneBackground } from "./machine-canvas";

/**
 * A remote machine's appearance: its color (a palette swatch or a custom
 * pick) and an optional background image for its terminals, with how strongly
 * it shows, over a live preview in the user's own terminal theme. Personal:
 * only this computer sees it. Opened from the color dot on its Machines row,
 * the row's "Appearance…" action, or a remote tab's menu.
 */
export function MachineAppearanceButton({
  machine,
  open,
  onOpenChange,
}: {
  machine: MachineSummary;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const id = machine.machineId;
  const savedColor = useMachineColor(id);
  const custom = useMachineAppearanceStore((s) => s.prefs[id]?.color);
  const { image, imageOpacity } = useMachineBackground(id);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // A custom pick in progress: previewed here only, saved when the picker
  // commits (its native `change`), so the Machine's open terminals aren't
  // re-themed on every step of a drag.
  const [draft, setDraft] = useState<string | null>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const colorLabelId = useId();
  const imageLabelId = useId();
  const strengthId = useId();
  const theme = useTerminalThemeConfig().theme;
  const previewColor = (draft && clampMachineColor(draft)) || savedColor;
  const previewBg = tintPaneBackground(theme.background!, previewColor);
  const isPalette = MACHINE_PALETTE.some((p) => p.hex === previewColor);

  // Commit on the picker's native `change` (React's onChange is every `input`).
  // A ref callback: the picker mounts with the popover, after this renders.
  const customRef = useCallback(
    (input: HTMLInputElement | null) => {
      if (!input) return;
      const commit = () => {
        const next = clampMachineColor(input.value);
        if (next) setMachineColor(id, next);
        setDraft(null);
      };
      input.addEventListener("change", commit);
      return () => input.removeEventListener("change", commit);
    },
    [id],
  );

  const pickFile = async (file: File | undefined) => {
    if (!file) return;
    setError(null);
    setBusy(true);
    try {
      await setMachineImage(id, await shrinkImageFile(file, theme.background!));
    } catch (e) {
      setError(getErrorMessage(e));
    } finally {
      setBusy(false);
      if (fileRef.current) fileRef.current.value = "";
    }
  };

  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        if (!next) {
          // macOS's color panel fires its `change` only when the panel itself
          // closes — possibly after this popover. A pick still in progress is
          // the user's choice: keep it rather than lose it.
          const pending = draft && clampMachineColor(draft);
          if (pending) setMachineColor(id, pending);
          setError(null);
          setDraft(null);
        }
        onOpenChange(next);
      }}
    >
      <PopoverTrigger asChild>
        <button
          type="button"
          data-testid={`machine-color-${id}`}
          aria-label={`Appearance for ${machine.label}`}
          title="Color and background"
          className="-m-1 inline-flex size-6 shrink-0 items-center justify-center rounded-full hover:bg-accent focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring"
        >
          <span className="size-2.5 rounded-full" style={{ backgroundColor: savedColor }} />
        </button>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        className="w-72 gap-3"
        data-testid={`machine-appearance-${id}`}
        aria-label={`Appearance for ${machine.label}`}
      >
        <div className="flex items-baseline justify-between gap-2">
          <span className="truncate text-sm font-medium">{machine.label}</span>
          <span className="shrink-0 text-[11px] text-muted-foreground">Only on this computer</span>
        </div>

        {/* What its terminals will look like, in the user's terminal theme. */}
        <div
          className="relative h-16 overflow-hidden rounded-md border border-border font-mono text-[11px] leading-4"
          style={{ backgroundColor: previewBg, color: theme.foreground }}
          data-testid={`machine-appearance-preview-${id}`}
        >
          <div className="relative z-0 p-2">
            <div>
              <span style={{ color: theme.green }}>~</span> $ git status
            </div>
            <div style={{ color: theme.brightBlack }}>On branch main · nothing to commit</div>
          </div>
          <MachineCanvas
            machineId={id}
            hostname={machineWatermarkText(machine.label, machine.declaration?.host)}
            background={previewBg}
            color={previewColor}
          />
        </div>

        <div className="flex flex-col gap-1.5">
          <div className="flex items-center justify-between">
            <span id={colorLabelId} className="text-xs font-medium">
              Color
            </span>
            {custom && (
              <button
                type="button"
                className="text-[11px] text-muted-foreground hover:text-foreground hover:underline"
                onClick={() => setMachineColor(id, null)}
              >
                Use default
              </button>
            )}
          </div>
          <div role="group" aria-labelledby={colorLabelId} className="flex flex-wrap gap-1.5">
            {MACHINE_PALETTE.map((swatch) => {
              const selected = swatch.hex === previewColor;
              const isDefault = swatch.hex === defaultMachineColor(id);
              return (
                <button
                  key={swatch.hex}
                  type="button"
                  aria-label={isDefault ? `${swatch.name} (default)` : swatch.name}
                  aria-pressed={selected}
                  data-testid={`machine-swatch-${swatch.name.toLowerCase()}`}
                  onClick={() => setMachineColor(id, isDefault ? null : swatch.hex)}
                  className="flex size-6 items-center justify-center rounded-full ring-offset-2 ring-offset-popover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring aria-pressed:ring-2 aria-pressed:ring-foreground/70"
                  style={{ backgroundColor: swatch.hex }}
                >
                  {selected && <Check size={12} strokeWidth={3} className="text-[#0d1117]" />}
                </button>
              );
            })}
            <label
              title="Custom color"
              className={`relative flex size-6 cursor-pointer items-center justify-center rounded-full border border-dashed border-muted-foreground/60 ring-offset-2 ring-offset-popover focus-within:ring-2 focus-within:ring-ring ${
                !isPalette ? "ring-2 ring-foreground/70" : ""
              }`}
              style={
                !isPalette ? { backgroundColor: previewColor, borderStyle: "solid" } : undefined
              }
            >
              <input
                ref={customRef}
                type="color"
                aria-label="Custom color"
                data-testid="machine-swatch-custom"
                // While dragging, the picker shows what the user is choosing
                // (never snapped back to the clamped value mid-drag).
                value={draft ?? savedColor}
                onChange={(e) => setDraft(e.target.value)}
                className="absolute inset-0 cursor-pointer opacity-0"
              />
              {isPalette ? (
                <Plus size={12} aria-hidden className="text-muted-foreground" />
              ) : (
                <Check size={12} strokeWidth={3} className="text-[#0d1117]" aria-hidden />
              )}
            </label>
          </div>
        </div>

        <div className="flex flex-col gap-1.5">
          <span id={imageLabelId} className="text-xs font-medium">
            Terminal background
          </span>
          <input
            ref={fileRef}
            type="file"
            accept={ACCEPTED_IMAGE_TYPES}
            className="sr-only"
            tabIndex={-1}
            aria-hidden
            data-testid={`machine-image-input-${id}`}
            onChange={(e) => void pickFile(e.target.files?.[0])}
          />
          <div className="flex items-center gap-2" role="group" aria-labelledby={imageLabelId}>
            {image && (
              <img
                src={image}
                alt=""
                className="h-8 w-12 shrink-0 rounded border border-border object-cover"
              />
            )}
            <button
              type="button"
              disabled={busy}
              onClick={() => fileRef.current?.click()}
              className="inline-flex h-7 items-center gap-1.5 rounded-md border border-border px-2 text-xs hover:bg-accent disabled:opacity-50"
            >
              <ImagePlus size={13} aria-hidden />
              {busy ? "Preparing…" : image ? "Replace…" : "Choose image…"}
            </button>
            {image && (
              <button
                type="button"
                onClick={() => void removeMachineImage(id)}
                className="text-xs text-muted-foreground hover:text-foreground hover:underline"
              >
                Remove
              </button>
            )}
          </div>
          {image && (
            <div className="flex items-center gap-2">
              <label htmlFor={strengthId} className="text-[11px] text-muted-foreground">
                Strength
              </label>
              <input
                id={strengthId}
                type="range"
                min={IMAGE_OPACITY.min * 100}
                max={IMAGE_OPACITY.max * 100}
                step={1}
                value={Math.round(imageOpacity * 100)}
                aria-valuetext={`${Math.round(imageOpacity * 100)}%`}
                onChange={(e) => setMachineImageOpacity(id, Number(e.target.value) / 100)}
                className="min-w-0 flex-1 accent-[var(--color-primary)]"
              />
              <span className="w-8 text-right text-[11px] tabular-nums text-muted-foreground">
                {Math.round(imageOpacity * 100)}%
              </span>
            </div>
          )}
          {error && (
            <p role="alert" className="text-[11px] text-destructive">
              {error}
            </p>
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}

/** Whether `machineId`'s appearance popover is open: its own state, or asked
 *  for by a remote tab's menu — then its row (`rowRef`) is scrolled into view
 *  before the popover anchors to it. The ask is cleared when it closes. */
export function useMachineAppearanceOpen(
  machineId: string,
  rowRef?: RefObject<HTMLElement | null>,
): [boolean, (open: boolean) => void] {
  const [local, setLocal] = useState(false);
  const requested = useMachineAppearanceStore((s) => s.requested === machineId);
  useLayoutEffect(() => {
    if (requested) rowRef?.current?.scrollIntoView?.({ block: "nearest" });
  }, [requested, rowRef]);
  const setOpen = useCallback(
    (open: boolean) => {
      setLocal(open);
      if (!open && useMachineAppearanceStore.getState().requested === machineId)
        requestMachineAppearance(null);
    },
    [machineId],
  );
  return [local || requested, setOpen];
}

/** Drop an appearance request no row can answer: the Machines list has loaded
 *  without that machine, or the list goes away. A stale ask never pops a
 *  popover open later. */
export function useSettleAppearanceRequest(machineIds: readonly string[] | null): void {
  const requested = useMachineAppearanceStore((s) => s.requested);
  useEffect(() => {
    if (requested && machineIds && !machineIds.includes(requested)) requestMachineAppearance(null);
  }, [requested, machineIds]);
  useEffect(() => () => requestMachineAppearance(null), []);
}
