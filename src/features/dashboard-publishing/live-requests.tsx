// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import {
  dashboardLiveConfirm,
  dashboardLiveDecline,
  dashboardLiveRequests,
  dashboardLiveTurnOff,
  type DashboardLiveRequest,
} from "@/tauri/commands";

const POLL_MS = 5000;

function what(r: DashboardLiveRequest) {
  const parts = [];
  if (r.terminals) {
    // `team` is reserved and is currently the same as private: only invited viewers get in.
    const who =
      r.shown?.audience === "team"
        ? "only you (team visibility is reserved, currently the same as private)"
        : "only you";
    parts.push(
      `shows the output of any terminal in its team, including ones started later (secrets masked; seen by ${who} and any viewers added)`,
    );
  }
  if (r.control) parts.push("lets its owner type into those terminals");
  if (r.widerAudience)
    parts.push(
      "sets visibility to team (reserved, currently the same as private: no one new can see it)",
    );
  if (r.viewersToAdd) {
    const addresses = r.shown?.shareAdd.join(", ");
    parts.push(
      `adds ${r.viewersToAdd} viewer${r.viewersToAdd === 1 ? "" : "s"}${addresses ? ` (${addresses})` : ""}`,
    );
  }
  return parts.length ? parts.join(", ") : "answers the queries in its dashboard.yml";
}

/** SV-PBF-286: live data waiting for a person's confirmation, and the kill switch. */
export function LiveRequests() {
  const [rows, setRows] = useState<DashboardLiveRequest[]>([]);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const load = useCallback(() => {
    dashboardLiveRequests()
      .then(setRows)
      .catch((e) => setError(String(e)));
  }, []);
  useEffect(() => {
    load();
    const timer = setInterval(load, POLL_MS);
    return () => clearInterval(timer);
  }, [load]);
  const act = async (slug: string, work: () => Promise<unknown>) => {
    setBusy(slug);
    setError("");
    try {
      await work();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy("");
      load();
    }
  };
  const visible = rows.filter((r) => r.waiting || r.liveOn);
  if (!visible.length && !error) return null;
  return (
    <section aria-label="Live data" className="mb-4 space-y-2 rounded-lg border border-border p-3">
      <h3 className="font-semibold">Live data</h3>
      {visible.map((r) => (
        <div
          key={r.slug}
          className="space-y-1 border-t border-border pt-2 first:border-t-0 first:pt-0"
        >
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-medium">&ldquo;{r.title}&rdquo;</span>
            {r.waiting ? (
              <span className="text-xs text-muted-foreground">
                Live data is paused until you confirm
              </span>
            ) : (
              <span className="text-xs text-muted-foreground">Live data is on</span>
            )}
          </div>
          {r.waiting && (
            <p className="text-muted-foreground">
              {r.by === "human" ? "A change" : "An agent"} asked to turn on live data that {what(r)}
              .
            </p>
          )}
          <div className="flex gap-2">
            {r.waiting && (
              <>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={!!busy || !r.shown}
                  onClick={() => {
                    const shown = r.shown;
                    if (shown) void act(r.slug, () => dashboardLiveConfirm(r.slug, shown));
                  }}
                >
                  Turn on live data
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={!!busy}
                  onClick={() => void act(r.slug, () => dashboardLiveDecline(r.slug))}
                >
                  Decline
                </Button>
              </>
            )}
            {r.liveOn && (
              <Button
                size="sm"
                variant="outline"
                disabled={!!busy}
                onClick={() => void act(r.slug, () => dashboardLiveTurnOff(r.slug))}
              >
                Turn off live data
              </Button>
            )}
          </div>
        </div>
      ))}
      {error && (
        <p role="alert" className="text-destructive">
          {error}
        </p>
      )}
    </section>
  );
}
