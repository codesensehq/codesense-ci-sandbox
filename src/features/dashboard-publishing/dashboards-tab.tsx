// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Copy, ExternalLink, FolderOpen, RefreshCw, X } from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import { openPath, openUrl } from "@tauri-apps/plugin-opener";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { dashboardControlLinks, dashboardMoveLink } from "@/tauri/commands";
import { readBuildChannel } from "@/features/license/channel";
import { useOrgStore } from "@/shared/stores/org-store";
import { LiveRequests } from "./live-requests";
import { useDashboardManager } from "./use-dashboard-manager";
import { dashboardRowKey, groupDashboardRows, sharedRowsByParent } from "./dashboard-groups";
import type { InventoryRow, Viewer } from "./types";

type Manager = ReturnType<typeof useDashboardManager>;
function time(value?: string | null) {
  return value ? new Date(value).toLocaleString() : "not recorded";
}
function stateText(row: InventoryRow) {
  if (row.differentKey) return "Published with a different key";
  if (row.error === "takedown not confirmed")
    return "Unpublish not confirmed · the link may still work";
  const state = row.hosted?.state ?? row.localState;
  if (state === "draft") return "Draft, not published";
  if (state === "unpublished")
    return row.error ? "Unpublish not confirmed" : "Unpublished · link no longer works";
  if (state === "publishing") return "Publication not confirmed";
  if (row.team && state !== "published") return "Not published";
  return `Published${row.hosted?.current_version ? ` v${row.hosted.current_version}` : " (local record)"}`;
}
function isPublished(row: InventoryRow) {
  return row.hosted ? row.hosted.state === "published" : row.localState === "published";
}
function chipLabel(row: InventoryRow) {
  if (row.team?.sharedFrom) return "Shared from parent";
  if (row.differentKey) return "Different key";
  if (row.hosted && row.hosted.state !== "published") return stateText(row);
  if (row.team) return isPublished(row) ? "Published" : "Not published";
  if (isPublished(row)) return "Published";
  if (row.location === "hosted") return "Hosted only";
  return stateText(row);
}
function chipClass(row: InventoryRow) {
  const base =
    "inline-flex shrink-0 items-center gap-1.5 rounded-md border border-border px-2 py-0.5 text-xs";
  if (row.team?.sharedFrom) return `${base} text-primary`;
  if (row.differentKey) return `${base} text-[var(--cs-status-needs-you)]`;
  if (isPublished(row)) return base;
  return `${base} text-muted-foreground`;
}
function chipDotClass(row: InventoryRow) {
  if (row.team?.sharedFrom) return "size-2 rounded-full bg-primary";
  if (row.differentKey) return "size-2 rounded-full bg-[var(--cs-status-needs-you)]";
  if (isPublished(row)) return "size-2 rounded-full bg-[var(--cs-status-done)]";
  return "size-2 rounded-full border border-current";
}
function publishedSubline(row: InventoryRow) {
  const details = [stateText(row)];
  if (row.hosted?.share_count) details.push(`${row.hosted.share_count} viewers`);
  const pushedAt = row.hosted?.pushed_at ?? row.pushedAt;
  if (pushedAt) details.push(`pushed ${time(pushedAt)}`);
  return details.join(" · ");
}
function dashboardSubline(row: InventoryRow) {
  if (row.team?.sharedFrom) {
    const state = row.differentKey
      ? stateText(row)
      : isPublished(row)
        ? stateText(row)
        : "Not published";
    return [state, `exported by ${row.team.sharedFrom}`, row.team.relativeFolder].join(" · ");
  }
  if (row.team && !isPublished(row)) return row.team.relativeFolder;
  if (!row.team && !row.folder && isPublished(row)) return "No folder on this Mac";
  return publishedSubline(row);
}
function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="mt-6 space-y-2">
      <h3 className="text-sm font-semibold">{title}</h3>
      {children}
    </section>
  );
}
function Confirm({
  children,
  label,
  onConfirm,
  onCancel,
  disabled,
}: {
  children: ReactNode;
  label: string;
  onConfirm: () => void;
  onCancel: () => void;
  disabled: boolean;
}) {
  const cancel = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    cancel.current?.focus();
  }, []);
  return (
    <div
      className="space-y-3 rounded-md border border-destructive/40 p-3"
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          onCancel();
        }
      }}
    >
      <p>{children}</p>
      <div className="flex gap-2">
        <Button size="sm" variant="destructive" disabled={disabled} onClick={onConfirm}>
          {label}
        </Button>
        <Button ref={cancel} size="sm" variant="outline" disabled={disabled} onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </div>
  );
}
function KeyPanel({ manager: m }: { manager: Manager }) {
  const field = useRef<HTMLInputElement>(null);
  const [edit, setEdit] = useState(false);
  const [show, setShow] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const expanded = edit || m.key?.status === "missing" || m.key?.status === "refused";
  const label = !m.key
    ? "Checking key…"
    : {
        configured: "Key saved",
        missing: "No publishing key.",
        refused: "The server refused this key",
        unavailable: "Couldn't check the key",
      }[m.key.status];
  return (
    <section aria-label="Publishing key" className="space-y-3 rounded-lg border border-border p-3">
      <div className="flex flex-wrap items-center gap-2">
        <span
          aria-hidden="true"
          className={`size-2 rounded-full ${m.key?.status === "configured" ? "bg-[var(--cs-status-done)]" : m.key?.status === "refused" ? "bg-destructive" : "bg-muted-foreground"}`}
        />
        <h2 className="font-semibold">{label}</h2>
        <span className="font-mono text-xs text-muted-foreground">{m.key?.hint}</span>
        <div className="ml-auto flex gap-1">
          {m.key?.hint && (
            <>
              <Button
                variant="ghost"
                size="sm"
                disabled={!!m.busy}
                onClick={() => {
                  setEdit(true);
                  setTimeout(() => field.current?.focus(), 0);
                }}
              >
                Replace
              </Button>
              <Button
                variant="ghost"
                size="sm"
                disabled={!!m.busy}
                onClick={() => setConfirm(true)}
              >
                Remove
              </Button>
            </>
          )}
        </div>
      </div>
      {m.key?.status === "missing" && (
        <p className="text-muted-foreground">
          Paste the key you were given to publish dashboards from CodeSense.
        </p>
      )}
      {m.key?.status === "refused" && (
        <p className="text-destructive">It may have been revoked or mistyped. Paste a new key.</p>
      )}
      {m.key?.status === "unavailable" && (
        <p className="text-muted-foreground">
          The saved key was kept. Check your connection and Keychain, then{" "}
          <button
            className="underline underline-offset-4"
            disabled={!!m.busy}
            onClick={() => void m.refresh()}
          >
            try again
          </button>
          .
        </p>
      )}
      {expanded && (
        <form
          className="space-y-2"
          onSubmit={(event) => {
            event.preventDefault();
            const value = field.current?.value ?? "";
            const saving = m.saveKey(value);
            if (field.current) field.current.value = "";
            setShow(false);
            void saving.then(() => setEdit(false));
          }}
        >
          <div className="flex flex-wrap gap-2">
            <Input
              ref={field}
              aria-label="Publishing key"
              type={show ? "text" : "password"}
              autoComplete="off"
              spellCheck={false}
              placeholder="csp_…"
              disabled={!!m.busy}
              className="min-w-32 flex-1"
            />
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={!!m.busy}
              onClick={() => setShow(!show)}
            >
              {show ? "Hide" : "Show"}
            </Button>
            <Button type="submit" size="sm" disabled={!!m.busy}>
              {m.busy === "Saving…" ? m.busy : "Save key"}
            </Button>
          </div>
          <p className="text-xs text-muted-foreground">
            Saved in your macOS Keychain. CodeSense never shows it again or puts it in files, logs
            or links.
          </p>
          {edit && (
            <p className="text-xs text-muted-foreground">
              Dashboards published with the old key stay with the old key. This key can only manage
              its own.
            </p>
          )}
        </form>
      )}
      {confirm && (
        <Confirm
          disabled={!!m.busy}
          label="Remove key"
          onCancel={() => setConfirm(false)}
          onConfirm={() => {
            void m.removeKey().then(() => setConfirm(false));
          }}
        >
          Remove the publishing key? Published dashboards stay online. You won't be able to publish,
          update or manage viewers from this Mac until you add a key.
        </Confirm>
      )}
    </section>
  );
}
function inviteText(viewer: Viewer) {
  if (viewer.invite === "drafted")
    return viewer.added_at && Date.now() / 1000 - viewer.added_at < 60
      ? "Sending invite…"
      : "Invite drafted, not sent yet";
  if (viewer.invite === "waiting_limit") return "Invite waiting (daily limit)";
  if (viewer.invite === "suppressed") return "Won't receive email";
  if (viewer.invite === "sent")
    return viewer.sent_at
      ? `Invite sent ${new Date(viewer.sent_at * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short" })}`
      : "Invite sent";
  if (viewer.invite === "none") return "No invite email";
  return "Invite status unknown";
}
function lastActive(value: number) {
  const minutes = Math.floor(Math.max(0, Date.now() / 1000 - value) / 60);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} ${minutes === 1 ? "minute" : "minutes"} ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} ${hours === 1 ? "hour" : "hours"} ago`;
  const days = Math.floor(hours / 24);
  return days === 1 ? "yesterday" : `${days} days ago`;
}
function sessionText(viewer: Viewer) {
  if (viewer.sessions == null) return "Sign-in status unavailable";
  if (viewer.sessions === 0) {
    if (viewer.joined) return "Joined · not signed in right now";
    return viewer.invite === "suppressed" ? "Not joined yet" : "Invited · not joined yet";
  }
  const devices = `${viewer.sessions} ${viewer.sessions === 1 ? "device" : "devices"}`;
  const active =
    viewer.last_seen_at == null ? "" : ` · last active ${lastActive(viewer.last_seen_at)}`;
  return viewer.joined
    ? `Joined · signed in on ${devices}${active}`
    : `Signed in on ${devices}${active}`;
}
function Details({
  row,
  manager: m,
  onClose,
}: {
  row: InventoryRow;
  manager: Manager;
  onClose: () => void;
}) {
  const [confirm, setConfirm] = useState<string | null>(null);
  const [email, setEmail] = useState("");
  const [localError, setLocalError] = useState("");
  const [copied, setCopied] = useState(false);
  const [moving, setMoving] = useState(false);
  const copiedTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(copiedTimer.current), []);
  const dashboard = m.detail?.dashboard ?? row.hosted;
  const published =
    dashboard?.state === "published" || (!dashboard && row.localState === "published");
  const known = !!m.detail && !m.detailError && !row.differentKey;
  const inviteRefresh = useRef(new Set<string>());
  useEffect(() => {
    const fresh =
      dashboard?.shares.filter(
        (v) =>
          v.invite === "drafted" &&
          v.id &&
          v.added_at &&
          Date.now() / 1000 - v.added_at < 60 &&
          !inviteRefresh.current.has(v.id),
      ) ?? [];
    if (!fresh.length) return;
    const timer = setTimeout(() => {
      for (const viewer of fresh) inviteRefresh.current.add(viewer.id!);
      void m.refresh();
    }, 4000);
    return () => clearTimeout(timer);
  }, [dashboard?.shares, m]);
  const pendingTakedown = row.error === "takedown not confirmed";
  const canWrite =
    !m.busy && m.key?.status === "configured" && !row.differentKey && (!published || known);
  const url = published && m.inventory?.host ? `${m.inventory.host}/d/${row.slug}` : null;
  const externally = async (work: () => Promise<unknown>) => {
    setLocalError("");
    try {
      await work();
    } catch (e) {
      setLocalError(String(e));
    }
  };
  return (
    <aside
      aria-label={row.title}
      className="min-w-0 space-y-2 border-t border-border p-6 lg:w-[46%] lg:border-t-0 lg:border-l"
      onKeyDown={(event) => {
        if (event.key === "Escape" && !m.busy) {
          event.stopPropagation();
          onClose();
        }
      }}
    >
      <div className="flex items-start justify-between gap-2">
        <div>
          <h2 className="font-semibold">{row.title}</h2>
          <p className="mt-1 text-xs text-muted-foreground">
            {row.slug} · {stateText(row)}
          </p>
          {row.team && (
            <p className="mt-1 text-xs text-muted-foreground">
              {row.team.sharedFrom ? "Shared from parent" : `${row.team.team} team`} ·{" "}
              {row.team.relativeFolder}
            </p>
          )}
        </div>
        <Button
          size="icon-sm"
          variant="ghost"
          aria-label="Close dashboard details"
          disabled={!!m.busy}
          onClick={onClose}
        >
          <X />
        </Button>
      </div>
      {row.differentKey && (
        <div
          data-testid="move-to-key-slot"
          className="space-y-2 rounded-md border border-border p-3"
        >
          <p>Move this dashboard to the key on this Mac. You'll confirm in your browser.</p>
          <Button
            size="sm"
            variant="outline"
            disabled={moving || !!m.busy || m.key?.status !== "configured"}
            onClick={() =>
              void externally(async () => {
                setMoving(true);
                try {
                  await openUrl(await dashboardMoveLink(row.slug));
                } finally {
                  setMoving(false);
                }
              })
            }
          >
            Move to this key
          </Button>
        </div>
      )}
      {m.detailError && (
        <p role="alert" className="text-destructive">
          {m.detailError}{" "}
          <button className="underline" disabled={!!m.busy} onClick={() => void m.select(row)}>
            Try again
          </button>
        </p>
      )}
      {localError && (
        <p role="alert" className="text-destructive">
          {localError}
        </p>
      )}
      <div aria-live="polite" className="text-muted-foreground">
        {m.busy || m.message}
      </div>
      {m.error && (
        <p role="alert" className="text-destructive">
          {m.error}
        </p>
      )}
      <Section title="Link">
        {url ? (
          <>
            <p className="break-all rounded-md bg-muted px-2 py-1.5 font-mono text-xs">{url}</p>
            <div className="flex gap-2">
              <Button
                variant="outline"
                size="sm"
                onClick={() =>
                  void externally(async () => {
                    await navigator.clipboard.writeText(url);
                    setCopied(true);
                    copiedTimer.current = setTimeout(() => setCopied(false), 1500);
                  })
                }
              >
                <Copy />
                {copied ? "Copied" : "Copy link"}
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => void externally(() => openUrl(url))}
              >
                Open
                <ExternalLink />
              </Button>
            </div>
          </>
        ) : (
          <p className="text-muted-foreground">No link: this dashboard isn't published.</p>
        )}
      </Section>
      <Section title="Publishing">
        {published ? (
          <p className="text-muted-foreground">
            {dashboard?.current_version ? `Version ${dashboard.current_version} · ` : ""}pushed{" "}
            {time(dashboard?.pushed_at ?? row.pushedAt)} · last checked{" "}
            {time(dashboard?.checked_at ?? row.checkedAt)}
          </p>
        ) : (
          <p className="text-muted-foreground">Not published yet</p>
        )}
        <p className="text-muted-foreground">
          Visibility:{" "}
          {dashboard?.visibility === "team"
            ? "team (reserved, currently the same as private)"
            : (dashboard?.visibility ?? "set in the folder's manifest")}
        </p>
        {!row.folder && (
          <p className="text-muted-foreground">
            No folder for this dashboard on this Mac. Open the source folder to update it.
          </p>
        )}
        <div className="flex flex-wrap gap-2">
          {row.folder && (
            <Button
              size="sm"
              variant="outline"
              onClick={() => void externally(() => openPath(row.folder!))}
            >
              <FolderOpen />
              Open folder
            </Button>
          )}
          {row.folder && !pendingTakedown && (
            <Button size="sm" disabled={!canWrite} onClick={() => void m.publish(row)}>
              {m.busy === "Publishing…"
                ? m.busy
                : published
                  ? "Update"
                  : row.localState === "unpublished"
                    ? "Publish again"
                    : "Publish"}
            </Button>
          )}
          {(published ||
            row.localState === "publishing" ||
            (!!row.error && row.localState === "unpublished")) && (
            <Button
              size="sm"
              variant="ghost"
              className="text-destructive"
              disabled={!canWrite}
              onClick={() => setConfirm("unpublish")}
            >
              {pendingTakedown ? "Retry unpublish" : "Unpublish"}
            </Button>
          )}
        </div>
        {m.key?.status !== "configured" && (
          <p className="text-xs text-muted-foreground">
            {m.key?.status === "missing"
              ? "Add a publishing key to publish."
              : "Check or replace the publishing key to manage this dashboard."}
          </p>
        )}
        {row.error && (
          <p className="text-destructive">
            Last operation failed: {row.error}. Check the key or open the source folder, then try
            again.
          </p>
        )}
        {confirm === "unpublish" && (
          <Confirm
            disabled={!!m.busy}
            label={m.busy === "Unpublishing…" ? m.busy : "Unpublish"}
            onCancel={() => setConfirm(null)}
            onConfirm={() => void m.unpublish(row).then(() => setConfirm(null))}
          >
            Unpublish {row.title}? The link stops working and every viewer loses access right away.
            Your folder on this Mac stays.
          </Confirm>
        )}
      </Section>
      <Section title={`Viewers${known ? ` (${m.detail!.dashboard.shares.length})` : ""}`}>
        <p className="text-muted-foreground">
          When you add someone, the CodeSense operator reviews the address, then CodeSense emails
          them an invite. They join with that email, with no password, and then see this dashboard.
          Daily invite limits apply.
        </p>
        <p className="text-xs text-muted-foreground">
          Viewers stay signed in for {dashboard?.session_days ?? 90} days after their last visit,
          and at most 90 days after they sign in. Set <code>viewers.session_days</code> in
          dashboard.yml to change it.
        </p>
        {!published ? (
          <p className="text-muted-foreground">
            This dashboard isn't published, so it can't have viewers.
          </p>
        ) : !known ? (
          <p className="text-muted-foreground">
            Viewer access couldn't be confirmed. Refresh or fix the key to manage viewers.
          </p>
        ) : (
          <>
            {m.detail!.dashboard.shares.map((viewer) => (
              <div key={viewer.email} className="space-y-1 border-t border-border py-2">
                <div className="flex items-start gap-2">
                  <p className="min-w-0 flex-1 break-all">{viewer.email}</p>
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={!canWrite}
                    onClick={() => setConfirm(viewer.email)}
                  >
                    Remove
                  </Button>
                </div>
                <p
                  className="text-xs text-muted-foreground"
                  title={
                    viewer.sent_at ? new Date(viewer.sent_at * 1000).toLocaleString() : undefined
                  }
                >
                  Can view ·{" "}
                  {viewer.invite === "waiting_limit" && (
                    <span aria-hidden="true" className="text-amber-600">
                      ●{" "}
                    </span>
                  )}
                  {viewer.invite === "suppressed" && <span aria-hidden="true">○ </span>}
                  {inviteText(viewer)}
                </p>
                {viewer.invite === "waiting_limit" && (
                  <p className="text-xs text-muted-foreground">
                    Not sent: a daily invite limit has been reached. It's saved and goes out when
                    the limit resets tomorrow. They can sign in now at codesense.ai/signin with this
                    email.
                  </p>
                )}
                {viewer.invite === "suppressed" && (
                  <p className="text-xs text-muted-foreground">
                    This person asked CodeSense not to email them. They can still sign in at
                    codesense.ai/signin with this email.
                  </p>
                )}
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <p
                    className="text-xs text-muted-foreground"
                    title={
                      viewer.last_seen_at == null
                        ? undefined
                        : new Date(viewer.last_seen_at * 1000).toLocaleString()
                    }
                  >
                    {sessionText(viewer)}
                  </p>
                  {!!viewer.sessions && viewer.id && (
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={!canWrite}
                      onClick={() => setConfirm(`signout:${viewer.email}`)}
                    >
                      Sign out everywhere
                    </Button>
                  )}
                </div>
                {confirm === `signout:${viewer.email}` && viewer.id && (
                  <Confirm
                    disabled={!!m.busy}
                    label={m.busy === "Signing out…" ? m.busy : "Sign out"}
                    onCancel={() => setConfirm(null)}
                    onConfirm={() =>
                      void m.signOutViewer(row, viewer.id!).then(() => setConfirm(null))
                    }
                  >
                    Sign {viewer.email} out on every device? They stay a viewer and can sign in
                    again with a code from their email.
                  </Confirm>
                )}
                {confirm === viewer.email && (
                  <Confirm
                    disabled={!!m.busy}
                    label={m.busy === "Removing…" ? m.busy : "Remove"}
                    onCancel={() => setConfirm(null)}
                    onConfirm={() =>
                      void m.share(row, [], [viewer.email]).then(() => setConfirm(null))
                    }
                  >
                    Remove {viewer.email}? They lose access now, and any view they have open closes.
                  </Confirm>
                )}
              </div>
            ))}
            <form
              className="flex flex-wrap gap-2"
              onSubmit={(event) => {
                event.preventDefault();
                void m.share(row, [email], []).then(() => setEmail(""));
              }}
            >
              <Input
                type="email"
                required
                aria-label="Viewer email"
                placeholder="email@example.com"
                value={email}
                onChange={(event) => setEmail(event.target.value)}
                disabled={!canWrite}
                className="min-w-32 flex-1"
              />
              <Button size="sm" disabled={!canWrite || !email.trim()}>
                {m.busy === "Adding…" ? m.busy : "Add viewer"}
              </Button>
            </form>
          </>
        )}
      </Section>
      <Section title="What viewers can do">
        {known ? (
          <ul>
            {m.detail!.capabilities.map((cap) => (
              <li key={cap.label} className="flex justify-between gap-3 py-1">
                <span>{cap.label}</span>
                <span className="max-w-[65%] text-right text-xs text-muted-foreground">
                  {cap.detail}
                </span>
              </li>
            ))}
          </ul>
        ) : (
          <p className="text-muted-foreground">
            Refresh this dashboard to confirm its live capabilities.
          </p>
        )}
        <p className="text-xs text-muted-foreground">
          Viewers never get terminal input, agent launch or publishing.
        </p>
      </Section>
      <Section title="Control">
        {dashboard?.live_control === "owner" ? (
          <>
            <h4 className="font-medium">Control from the web</h4>
            <p className="text-muted-foreground">
              Take control opens your account in the browser. Sign in and confirm it's you to use
              this dashboard's owner controls for 12 hours. CodeSense doesn't hold the grant, so it
              can't show whether control is active. Your publishing key must be linked to your
              account for this to work.
            </p>
            <p className="text-muted-foreground">
              End this dashboard's grant from your account page. It also ends on its own after 12
              hours.
            </p>
            <div className="flex flex-wrap gap-2">
              <Button
                variant="outline"
                size="sm"
                disabled={!known || !published || !!m.busy}
                onClick={() =>
                  void externally(async () => openUrl((await dashboardControlLinks(row.slug)).take))
                }
              >
                Take control in browser
                <ExternalLink />
              </Button>
              <Button
                variant="outline"
                size="sm"
                disabled={!known || !!m.busy}
                onClick={() =>
                  void externally(async () => openUrl((await dashboardControlLinks(row.slug)).end))
                }
              >
                End control in browser
                <ExternalLink />
              </Button>
            </div>
          </>
        ) : (
          <p className="text-muted-foreground">
            {known || !published
              ? "This dashboard has no owner controls."
              : "Refresh to confirm this dashboard's owner controls."}
          </p>
        )}
      </Section>
    </aside>
  );
}

export function DashboardsTab() {
  const m = useDashboardManager();
  const activeOrg = useOrgStore((state) => state.activeOrgSlug);
  const activeTeam = useOrgStore((state) => state.activeTeamName);
  const [folderError, setFolderError] = useState("");
  const [liveAnnouncement, setLiveAnnouncement] = useState("");
  const [recentlyAdded, setRecentlyAdded] = useState<Set<string>>(() => new Set());
  const addedRowTimers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const rows = m.inventory?.rows ?? [];
  const groups = useMemo(
    () =>
      groupDashboardRows(
        m.inventory ?? {
          host: null,
          source: "local",
          checkedAt: "",
          error: null,
          rows: [],
          teams: [],
        },
        { org: activeOrg, team: activeTeam },
      ),
    [m.inventory, activeOrg, activeTeam],
  );
  const orderedRows = useMemo(
    () =>
      groups.flatMap((group) => {
        if (group.kind !== "team") return group.rows;
        return [...group.rows, ...sharedRowsByParent(group).flatMap(([, rows]) => rows)];
      }),
    [groups],
  );
  const rowKeys = useMemo(() => orderedRows.map(dashboardRowKey), [orderedRows]);
  const selectedKey = m.selected?.key ?? null;
  const row = rows.find((item) => dashboardRowKey(item) === selectedKey);
  const rowRefs = useRef(new Map<string, HTMLButtonElement>());
  const focusedRowKey = useRef<string | null>(null);
  const [pendingFocusKey, setPendingFocusKey] = useState<string | null>(null);
  const previousRows = useRef<InventoryRow[] | null>(null);
  const previousKeys = useRef<string[]>([]);
  const hasSeenInitialInventory = useRef(false);
  useEffect(
    () => () => {
      for (const timer of addedRowTimers.current.values()) clearTimeout(timer);
      addedRowTimers.current.clear();
    },
    [],
  );
  useEffect(() => {
    if (!m.inventory) {
      previousRows.current = rows;
      previousKeys.current = rowKeys;
      return;
    }
    if (!hasSeenInitialInventory.current) {
      hasSeenInitialInventory.current = true;
      previousRows.current = rows;
      previousKeys.current = rowKeys;
      return;
    }
    const previous = previousRows.current;
    if (previous) {
      const priorKeys = new Set(previous.map(dashboardRowKey));
      const nextKeys = new Set(rows.map(dashboardRowKey));
      const addedRows = rows.filter((item) => item.team && !priorKeys.has(dashboardRowKey(item)));
      const removed = previous.find((item) => item.team && !nextKeys.has(dashboardRowKey(item)));
      const added = addedRows[0];
      if (added?.team) setLiveAnnouncement(`${added.title} added to ${added.team.team}`);
      else if (removed?.team)
        setLiveAnnouncement(`${removed.title} was removed from ${removed.team.team}`);
      for (const item of addedRows) {
        const key = dashboardRowKey(item);
        const previousTimer = addedRowTimers.current.get(key);
        if (previousTimer) clearTimeout(previousTimer);
        setRecentlyAdded((current) => new Set(current).add(key));
        addedRowTimers.current.set(
          key,
          setTimeout(() => {
            addedRowTimers.current.delete(key);
            setRecentlyAdded((current) => {
              const next = new Set(current);
              next.delete(key);
              return next;
            });
          }, 10_000),
        );
      }
    }
    previousRows.current = rows;
    const removedSelection = !!selectedKey && !rowKeys.includes(selectedKey);
    const removedFocus = !!focusedRowKey.current && !rowKeys.includes(focusedRowKey.current);
    const removedKey = removedSelection ? selectedKey : removedFocus ? focusedRowKey.current : null;
    if (removedKey) {
      const oldIndex = previousKeys.current.indexOf(removedKey);
      const nextKey = rowKeys[Math.min(Math.max(oldIndex, 0), rowKeys.length - 1)];
      if (removedSelection) m.close();
      if (nextKey) setPendingFocusKey(nextKey);
    }
    previousKeys.current = rowKeys;
  }, [m.inventory, rows, rowKeys, selectedKey]);
  useEffect(() => {
    if (!pendingFocusKey || m.busy) return;
    const target = rowRefs.current.get(pendingFocusKey);
    if (target) {
      target.focus();
      setPendingFocusKey(null);
    }
  }, [pendingFocusKey, m.busy, rowKeys]);
  const close = () => {
    const key = selectedKey;
    m.close();
    if (key) requestAnimationFrame(() => rowRefs.current.get(key)?.focus());
  };
  const stale = m.inventory?.source === "stale";
  const renderDashboardRow = (item: InventoryRow) => {
    const key = dashboardRowKey(item);
    const index = rowKeys.indexOf(key);
    const selected = key === selectedKey;
    const subline = dashboardSubline(item);
    const addedFolder = recentlyAdded.has(key) ? item.team?.relativeFolder : null;
    return (
      <button
        key={key}
        ref={(element) => {
          if (element) rowRefs.current.set(key, element);
          else rowRefs.current.delete(key);
        }}
        onFocus={() => {
          focusedRowKey.current = key;
        }}
        role="option"
        aria-selected={selected}
        tabIndex={selectedKey ? (selected ? 0 : -1) : index === 0 ? 0 : -1}
        disabled={!!m.busy && m.busy !== "Refreshing dashboards…"}
        className={`block w-full border-t border-border px-3 py-2.5 text-left hover:bg-muted focus-visible:outline-2 focus-visible:outline-ring disabled:opacity-60 ${selected ? "bg-accent/10" : ""}`}
        onClick={() => void m.select(item, key)}
        onKeyDown={(event) => {
          let target: number | undefined;
          if (event.key === "ArrowDown") target = (index + 1) % rowKeys.length;
          if (event.key === "ArrowUp") target = (index + rowKeys.length - 1) % rowKeys.length;
          if (event.key === "Home") target = 0;
          if (event.key === "End") target = rowKeys.length - 1;
          if (target !== undefined) {
            event.preventDefault();
            rowRefs.current.get(rowKeys[target])?.focus();
          }
        }}
      >
        <span className="flex flex-wrap items-baseline justify-between gap-2">
          <span className="min-w-0 font-semibold">
            {item.title}{" "}
            <span className="font-mono text-xs font-normal text-muted-foreground">{item.slug}</span>
          </span>
          <span className={chipClass(item)}>
            <span aria-hidden="true" className={chipDotClass(item)} />
            {chipLabel(item)}
          </span>
        </span>
        <span className="mt-1 block text-xs text-muted-foreground">
          {addedFolder ? (
            <>
              <span>Added just now</span> · <span>{addedFolder}</span>
            </>
          ) : (
            subline
          )}
        </span>
        {item.error && (
          <span className="mt-1 block text-xs text-destructive">
            Last operation failed: {item.error}. Open details to retry.
          </span>
        )}
      </button>
    );
  };
  return (
    <div
      data-testid="dashboards-tab"
      className="flex h-full flex-col overflow-auto bg-background text-sm text-foreground lg:flex-row"
      onKeyDown={(event) => {
        if (event.key === "Escape" && row && !m.busy) {
          event.stopPropagation();
          close();
        }
      }}
    >
      <section className="min-w-0 flex-1 space-y-5 p-6">
        <header>
          <h1 className="text-lg font-semibold">Dashboards</h1>
          <p className="mt-1 text-muted-foreground">
            Publish dashboards and choose who can see them.
          </p>
          <p className="mt-3 text-xs text-muted-foreground">
            {m.inventory?.host
              ? `Publishing to ${m.inventory.host}`
              : "No dashboard host configured in this build"}
            {readBuildChannel().channel === "beta-staging" && " · staging"} · This Mac · Publishing
            key scope
          </p>
        </header>
        <KeyPanel manager={m} />
        <div>
          <div className="mb-2 flex flex-wrap items-center gap-2">
            <h2 className="font-semibold">Your dashboards</h2>
            {m.inventory && (
              <span className="text-xs text-muted-foreground">
                {stale ? "last checked" : "checked"} {time(m.inventory.checkedAt)}
              </span>
            )}
            <Button
              variant="ghost"
              size="sm"
              className="ml-auto"
              disabled={!!m.busy}
              onClick={() => void m.refresh()}
            >
              <RefreshCw />
              Refresh
            </Button>
          </div>
          {m.inventory?.source === "unsupported" && (
            <p className="mb-3 rounded-md bg-muted p-3">
              The server list isn't available yet, so this shows only dashboards this Mac has
              published or opened. Others published with this key won't appear.
            </p>
          )}
          {m.inventory?.error && (
            <p role="alert" className="mb-3 rounded-md bg-muted p-3">
              {stale
                ? `Couldn't reach ${m.inventory.host}. Showing what it said at ${time(m.inventory.checkedAt)}; changes since then may be missing.`
                : m.inventory.error}{" "}
              <button className="underline" disabled={!!m.busy} onClick={() => void m.refresh()}>
                Try again
              </button>
            </p>
          )}
          <LiveRequests />
          {!m.inventory && m.busy ? (
            <div aria-label="Loading dashboards" className="space-y-2">
              {[0, 1, 2].map((i) => (
                <div key={i} className="h-12 rounded-md bg-muted" />
              ))}
            </div>
          ) : rows.length || groups.length ? (
            <div
              role="listbox"
              aria-label="Dashboards"
              className="overflow-clip rounded-lg border border-border"
              onBlur={(event) => {
                if (
                  event.relatedTarget &&
                  !event.currentTarget.contains(event.relatedTarget as Node)
                )
                  focusedRowKey.current = null;
              }}
            >
              {groups.map((group, groupIndex) => {
                if (group.kind === "attached")
                  return (
                    <div key={group.key} role="group" aria-labelledby="dashboard-attached-heading">
                      <div
                        id="dashboard-attached-heading"
                        className="sticky top-0 z-10 flex items-baseline gap-2 border-t border-border bg-sidebar px-3 py-2 first:border-t-0"
                      >
                        <span className="font-semibold">Attached folders and hosted</span>
                        <span className="ml-auto text-xs text-muted-foreground">
                          {group.rows.length}
                        </span>
                      </div>
                      {group.rows.map(renderDashboardRow)}
                    </div>
                  );
                const headingId = `dashboard-team-heading-${groupIndex}`;
                return (
                  <div key={group.key} role="group" aria-labelledby={headingId}>
                    <div
                      id={headingId}
                      className="sticky top-0 z-10 flex flex-wrap items-baseline gap-2 border-t border-border bg-sidebar px-3 py-2 first:border-t-0"
                    >
                      <span className="font-semibold">{group.team}</span>
                      <span className="font-mono text-xs text-muted-foreground">{group.org}</span>
                      {group.active && (
                        <span className="rounded border border-border px-1.5 text-[11px] leading-4 text-muted-foreground">
                          Active team
                        </span>
                      )}
                      <span className="ml-auto text-xs text-muted-foreground">
                        {group.rows.length + group.sharedRows.length}
                      </span>
                    </div>
                    {group.rows.map(renderDashboardRow)}
                    {sharedRowsByParent(group).map(([parent, parentRows]) => (
                      <div key={parent}>
                        <div className="border-t border-border px-3 pb-0.5 pt-1.5 text-xs text-muted-foreground">
                          Shared with {group.team} by {parent}
                        </div>
                        {parentRows.map(renderDashboardRow)}
                      </div>
                    ))}
                    {!group.rows.length && !group.sharedRows.length && (
                      <p className="px-3 py-2 text-muted-foreground">
                        No dashboards in {group.team} yet. Ask an agent to make one in{" "}
                        <code>dashboards/</code>.
                      </p>
                    )}
                  </div>
                );
              })}
            </div>
          ) : (
            <div className="rounded-lg border border-dashed border-border p-5">
              <h3 className="font-semibold">No dashboards yet.</h3>
              <p className="mt-1 text-muted-foreground">
                Ask an agent to make one, or open a dashboard folder. Dashboards published with this
                key also appear here.
              </p>
            </div>
          )}
        </div>
        <Button
          variant="outline"
          size="sm"
          disabled={!!m.busy}
          onClick={() => {
            setFolderError("");
            void open({ directory: true, multiple: false, title: "Open dashboard folder" })
              .then((folder) => (typeof folder === "string" ? m.openFolder(folder) : undefined))
              .catch((e) => setFolderError(String(e)));
          }}
        >
          <FolderOpen />
          Open dashboard folder
        </Button>
        <p className="-mt-4 text-xs text-muted-foreground">
          For a dashboard folder outside your teams' content.
        </p>
        <div aria-live="polite" className="sr-only">
          {liveAnnouncement}
        </div>
        <div aria-live="polite" className="text-muted-foreground">
          {!row && (m.busy || m.message)}
        </div>
        {((!row && m.error) || folderError) && (
          <p role="alert" className="text-destructive">
            {m.error || folderError}{" "}
            <button className="underline" disabled={!!m.busy} onClick={() => void m.refresh()}>
              Try again
            </button>
          </p>
        )}
      </section>
      {row && <Details key={dashboardRowKey(row)} row={row} manager={m} onClose={close} />}
    </div>
  );
}
