// SPDX-License-Identifier: MIT
/**
 * SW-OR-16 — the rich turn-by-turn renderer (AC-3): the panel's structured BODY MODE, fed the
 * already-parsed `SessionTranscript` (never raw NDJSON). Renders each turn's prose, its tool calls as
 * COLLAPSED-by-default disclosures (native `<details>` — zero JS state, so a big transcript never
 * mounts thousands of open bodies), and a per-turn usage badge. It is a body rendering *mode*, not a
 * second panel: `AgentResultBody` picks it when turns exist and falls back to the plain `<pre>` when
 * they don't.
 *
 * UI-thread guard (the FE half of "no beachball"): the input is already bounded (~1 MiB) trusted-side,
 * tool bodies stay collapsed, and a pathological turn count is WINDOWED to the tail (which holds the
 * outcome) with a visible, non-silent notice — so the React render never janks.
 */
import type { SessionTranscript, ToolCall, TranscriptTurn } from "@/shared/lib/session-transcript";

/** Cap on rendered turns — bounds DOM nodes even if a run emits an extreme number of tiny turns. The
 *  TAIL is kept (it holds the final answer/outcome), with a visible notice for what was dropped. */
const MAX_RENDERED_TURNS = 200;

/** A compact `↓in ↑out` token badge, or nothing when the turn reported no usage. */
function UsageBadge({ turn }: { turn: TranscriptTurn }) {
  const usage = turn.usage;
  if (!usage || (usage.input_tokens == null && usage.output_tokens == null)) return null;
  const parts: string[] = [];
  if (usage.input_tokens != null) parts.push(`↓${usage.input_tokens}`);
  if (usage.output_tokens != null) parts.push(`↑${usage.output_tokens}`);
  return (
    <span
      className="text-[10px] font-mono text-muted-foreground rounded bg-muted/60 px-1 py-0.5"
      data-testid="turn-usage-badge"
      title="tokens in / out"
    >
      {parts.join(" ")}
    </span>
  );
}

/** One tool call: a collapsed disclosure whose summary is the tool name (+ an error marker), and
 *  whose body shows the rendered input and, when present, the result. Bounded height so a large
 *  result scrolls rather than pushing the layout. */
function ToolCallDisclosure({ call }: { call: ToolCall }) {
  return (
    <details className="rounded border border-border bg-muted/30" data-testid="tool-call">
      <summary className="cursor-pointer select-none px-2 py-1 text-xs font-mono flex items-center gap-1.5">
        <span className="text-muted-foreground">🔧</span>
        <span className="text-foreground">{call.name}</span>
        {call.is_error && (
          <span
            className="text-destructive"
            data-testid="tool-call-error"
            title="tool reported an error"
          >
            ●
          </span>
        )}
      </summary>
      <div className="px-2 pb-2 flex flex-col gap-1.5">
        {call.input != null && call.input.length > 0 && (
          <div>
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Input</div>
            <pre className="text-[11px] font-mono whitespace-pre-wrap break-words bg-background/60 rounded p-1.5 max-h-64 overflow-auto">
              {call.input}
            </pre>
          </div>
        )}
        {call.result != null && call.result.length > 0 && (
          <div>
            <div className="text-[10px] uppercase tracking-wide text-muted-foreground">Result</div>
            <pre className="text-[11px] font-mono whitespace-pre-wrap break-words bg-background/60 rounded p-1.5 max-h-64 overflow-auto">
              {call.result}
            </pre>
          </div>
        )}
      </div>
    </details>
  );
}

/** One turn: role label + usage badge, the prose, then any tool calls. */
function TurnRow({ turn }: { turn: TranscriptTurn }) {
  return (
    <div
      className="flex flex-col gap-1.5 border-t border-border pt-2 first:border-t-0"
      data-testid="transcript-turn"
    >
      <div className="flex items-center gap-2">
        <span
          className="text-[10px] uppercase tracking-wide text-muted-foreground"
          data-testid="turn-role"
        >
          {turn.role}
        </span>
        <UsageBadge turn={turn} />
      </div>
      {turn.text != null && turn.text.length > 0 && (
        <p className="text-xs text-foreground whitespace-pre-wrap break-words">{turn.text}</p>
      )}
      {turn.tool_calls && turn.tool_calls.length > 0 && (
        <div className="flex flex-col gap-1">
          {turn.tool_calls.map((call, i) => (
            <ToolCallDisclosure key={i} call={call} />
          ))}
        </div>
      )}
    </div>
  );
}

/** The rich transcript body. Windows to the last [`MAX_RENDERED_TURNS`] turns (with a visible notice)
 *  so an extreme turn count can never jank the render. */
export function TranscriptTurns({ transcript }: { transcript: SessionTranscript }) {
  const all = transcript.turns;
  const hiddenCount = Math.max(0, all.length - MAX_RENDERED_TURNS);
  const shown = hiddenCount > 0 ? all.slice(all.length - MAX_RENDERED_TURNS) : all;
  return (
    <div className="flex flex-col gap-2 bg-muted/40 rounded p-2" data-testid="agent-result-turns">
      {hiddenCount > 0 && (
        <div
          className="text-[11px] italic text-muted-foreground"
          data-testid="transcript-windowed-notice"
        >
          {hiddenCount} earlier {hiddenCount === 1 ? "turn" : "turns"} hidden — showing the latest{" "}
          {MAX_RENDERED_TURNS}.
        </div>
      )}
      {shown.map((turn, i) => (
        <TurnRow key={i} turn={turn} />
      ))}
    </div>
  );
}
