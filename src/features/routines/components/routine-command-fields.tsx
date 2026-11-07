// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";
import { Plus, X } from "lucide-react";
import { Input } from "@/components/ui/input";
import { AppButton as Button } from "@/shared/components/app-button";
import {
  routinePreviewCommand,
  type RoutineCommandPreview,
  type RoutineAfter,
} from "@/tauri/commands";
import type { RoutineSpace } from "../lib/routine-agent-ref";
import { commandArgs, type SentenceDraft } from "../lib/routine-sentence";
import { FIELD, PREVIEW_DEBOUNCE_MS } from "../lib/routine-form-controls";

export function ArgvChips({ argv }: { argv: readonly string[] }) {
  return (
    <span className="inline-flex min-w-0 flex-wrap gap-1">
      {argv.map((arg, i) => (
        <code key={i} className="max-w-full break-all rounded bg-muted px-1.5 py-0.5 text-xs">
          {arg || '""'}
        </code>
      ))}
    </span>
  );
}

export function useCommandPreview({
  draft,
  routine,
  routineKey,
  onBlocked,
  enabled,
}: {
  draft: SentenceDraft;
  routine: RoutineSpace;
  routineKey?: string;
  onBlocked?: (blocked: boolean) => void;
  enabled: boolean;
}) {
  const [preview, setPreview] = useState<{ input: string; value: RoutineCommandPreview } | null>(
    null,
  );
  const input = JSON.stringify({
    command: commandArgs(draft.command),
    after: draft.after,
    space: routine.scope === "team" ? routine.space : null,
    key: routineKey,
  });
  const current = preview?.input === input ? preview.value : null;
  useEffect(() => {
    if (!enabled) {
      onBlocked?.(false);
      return;
    }
    const args = JSON.parse(input) as {
      command: string[];
      after?: RoutineAfter;
      space: string | null;
      key?: string;
    };
    onBlocked?.(true);
    let active = true;
    const timer = setTimeout(async () => {
      if (!args.space) return;
      const at = args.space.lastIndexOf("/");
      try {
        const value = await routinePreviewCommand(
          args.command,
          args.space.slice(0, at),
          args.space.slice(at + 1),
          args.after,
          args.key,
        );
        if (active) {
          setPreview({ input, value });
          onBlocked?.(!!value.error);
        }
      } catch (e) {
        if (active) {
          setPreview({ input, value: { argv: [], error: String(e), warnings: [] } });
          onBlocked?.(true);
        }
      }
    }, PREVIEW_DEBOUNCE_MS);
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [input, onBlocked, enabled]);
  return current;
}

export function RoutineCommandFields({
  draft,
  onChange,
  preview: current,
}: {
  draft: SentenceDraft;
  onChange: (draft: SentenceDraft) => void;
  preview: RoutineCommandPreview | null;
}) {
  const fields = useRef<(HTMLInputElement | null)[]>([]);
  const focus = useRef<number | null>(null);
  useEffect(() => {
    if (focus.current !== null) {
      fields.current[focus.current]?.focus();
      focus.current = null;
    }
  }, [draft.command]);
  const add = (at = draft.command.length - 1) => {
    const command = [...draft.command];
    command.splice(at + 1, 0, "");
    focus.current = at + 1;
    onChange({ ...draft, command });
  };
  const program = draft.command[0] ?? "";
  const error = current?.error;
  return (
    <div className="flex w-full min-w-0 flex-col gap-2" aria-label="Command">
      {draft.command.map((arg, i) => (
        <div key={i} className="flex items-center gap-2">
          <label className="flex min-w-0 flex-1 items-center gap-3">
            <span className="w-20 shrink-0 text-xs text-muted-foreground">
              {i === 0 ? "Program" : `Argument ${i}`}
            </span>
            <Input
              ref={(el) => {
                fields.current[i] = el;
              }}
              aria-label={i === 0 ? "Program" : `Argument ${i}`}
              className={`${FIELD} min-w-0 font-mono`}
              value={arg}
              placeholder={
                i === 0
                  ? "python3, node, sh, or a path like scripts/sync.sh"
                  : "one argument, as typed"
              }
              aria-invalid={(i === 0 && !!error) || undefined}
              aria-describedby={
                i === 0 && error
                  ? "routine-command-error"
                  : i > 0 && arg === "" && i < commandArgs(draft.command).length
                    ? `routine-empty-${i}`
                    : undefined
              }
              onChange={(e) =>
                onChange({
                  ...draft,
                  command: draft.command.map((a, n) => (n === i ? e.target.value : a)),
                })
              }
              onKeyDown={(e) => {
                if (i > 0 && e.key === "Enter" && !e.nativeEvent.isComposing) {
                  e.preventDefault();
                  add(i);
                }
              }}
            />
          </label>
          {i > 0 && (
            <Button
              type="button"
              size="sm"
              variant="ghost"
              aria-label={`Remove argument ${i}`}
              onClick={() => {
                focus.current = i - 1;
                onChange({ ...draft, command: draft.command.filter((_, n) => n !== i) });
              }}
            >
              <X />
            </Button>
          )}
        </div>
      ))}
      <Button type="button" size="sm" variant="ghost" className="self-start" onClick={() => add()}>
        <Plus />
        Add an argument
      </Button>
      {commandArgs(draft.command).map((arg, i) =>
        i > 0 && arg === "" ? (
          <p id={`routine-empty-${i}`} key={i} className="text-xs text-warning-ink">
            Argument {i} is empty; it is passed as an empty argument.
          </p>
        ) : null,
      )}
      {error && (
        <p
          id="routine-command-error"
          role={program ? "alert" : undefined}
          className={`text-xs ${program ? "text-destructive" : "text-muted-foreground"}`}
        >
          {error}
        </p>
      )}
      {error?.includes("no spaces") && (
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="self-start"
          onClick={() =>
            onChange({
              ...draft,
              command: [...program.trim().split(/\s+/), ...draft.command.slice(1)],
            })
          }
        >
          Split into fields
        </Button>
      )}
      {error?.includes("can't run by itself") && (
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="self-start"
          onClick={() =>
            onChange({
              ...draft,
              command: [
                /\.py$/.test(program) ? "python3" : /\.(m?js)$/.test(program) ? "node" : "sh",
                ...draft.command,
              ],
            })
          }
        >
          Run it with{" "}
          {/\.py$/.test(program) ? "python3" : /\.(m?js)$/.test(program) ? "node" : "sh"}
        </Button>
      )}
    </div>
  );
}

export function RoutineCommandReadback({
  preview: current,
}: {
  preview: RoutineCommandPreview | null;
}) {
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-start gap-2 text-xs text-muted-foreground" aria-live="polite">
        <span className="shrink-0">Runs as</span>
        {current ? <ArgvChips argv={current.argv} /> : <span>Checking command…</span>}
      </div>
      {current?.warnings.map((warning, i) => (
        <p key={i} className="text-xs text-warning-ink">
          {warning}
        </p>
      ))}
    </div>
  );
}

export function RoutineCommandHelp({ draft }: { draft: SentenceDraft }) {
  return (
    <p className="max-w-[70ch] text-xs text-muted-foreground">
      Each field is one argument, passed as typed. Nothing goes through a shell, so quotes,{" "}
      <code>|</code> and <code>$</code> stay literal. Fill in <code>{"{{date}}"}</code>,{" "}
      <code>{"{{timestamp}}"}</code>, or a value a page sends, like <code>{"{{itemId}}"}</code>. The
      run succeeds when the command exits 0, and the last 4 KB it prints becomes the run&apos;s
      result. It runs with your permissions, in the routine&apos;s workspace.
      {draft.after && (
        <>
          {" "}
          From the run before: <code>{"{{trigger.result_text}}"}</code>,{" "}
          <code>{"{{trigger.phase}}"}</code>, <code>{"{{trigger.routine}}"}</code>,{" "}
          <code>{"{{trigger.run_id}}"}</code>, and why it ended, <code>{"{{trigger.detail}}"}</code>
          .
        </>
      )}
    </p>
  );
}
