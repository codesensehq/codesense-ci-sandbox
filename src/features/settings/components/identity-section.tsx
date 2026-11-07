// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { AgentFaceView } from "@/features/viewer/components/agent-identity";
import { useAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { parseFaceRef } from "@/shared/lib/avatar-packs";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { inboxSenderIdentity } from "@/features/weft/lib/inbox-sender-identity";
import { rowMatches, type SettingRowSpec } from "@/features/settings/lib/row-matches";

import { SectionHeader } from "./section-header";
import { SettingRow } from "./setting-row";

export const IDENTITY_ROW_SPECS: SettingRowSpec[] = [
  {
    key: "displayName",
    label: "Your name",
    keywords: ["name", "display", "identity", "me", "human", "profile", "author"],
  },
  {
    key: "avatar",
    label: "Your avatar",
    keywords: ["avatar", "face", "picture", "image", "upload", "pack", "identity", "custom"],
  },
];

export const IDENTITY_SECTION_SPECS: readonly SettingRowSpec[] = IDENTITY_ROW_SPECS;

const MAX_UPLOAD_BYTES = 512 * 1024;
const ACCEPT = "image/png,image/webp,image/svg+xml";

export function IdentitySection({ query = "" }: { query?: string }) {
  const activeProfile = useLocalProfileStore((s) => s.status?.active ?? null);
  const setIdentity = useLocalProfileStore((s) => s.setIdentity);
  const currentUser = useCurrentUser();
  const avatars = useAvatarPacks();

  const [name, setName] = useState(activeProfile?.displayName ?? "");
  const [savingName, setSavingName] = useState(false);
  const [uploading, setUploading] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setName(activeProfile?.displayName ?? "");
  }, [activeProfile?.principal, activeProfile?.displayName]);

  const previewFace = useMemo(() => {
    const displayName = currentUser?.display_name ?? name ?? "";
    const identity = inboxSenderIdentity(
      "human",
      displayName,
      undefined,
      {
        displayName,
        face: currentUser?.identity_avatar ?? null,
      },
      avatars.faces,
    );
    return identity.face;
  }, [currentUser?.display_name, currentUser?.identity_avatar, name, avatars.faces]);

  const chosenRef = currentUser?.identity_avatar ?? null;
  const chosenFace = chosenRef && parseFaceRef(chosenRef) ? chosenRef : null;

  if (!activeProfile) {
    if (query && !"identity name avatar you".includes(query.toLowerCase())) return null;
    return (
      <section className="space-y-3 p-4">
        <SectionHeader id="identity">Identity</SectionHeader>
        <p className="text-sm text-muted-foreground">Choose a local profile to edit identity.</p>
      </section>
    );
  }

  const showName = rowMatches(IDENTITY_ROW_SPECS[0], query);
  const showAvatar = rowMatches(IDENTITY_ROW_SPECS[1], query);
  if (!showName && !showAvatar) return null;

  const trimmedName = name.trim();
  const nameDirty = trimmedName.length > 0 && trimmedName !== activeProfile.displayName;

  const saveName = async () => {
    if (!nameDirty) return;
    setSavingName(true);
    try {
      await setIdentity({ displayName: trimmedName });
      toast.success("Name saved");
    } catch (e) {
      toast.error(`Could not save your name: ${String(e)}`);
    } finally {
      setSavingName(false);
    }
  };

  const pickFace = async (ref: string | null) => {
    try {
      await setIdentity({ avatar: ref ? { kind: "pack", ref } : { kind: "clear" } });
    } catch (e) {
      toast.error(`Could not change your avatar: ${String(e)}`);
    }
  };

  const uploadFile = async (file: File) => {
    if (file.size > MAX_UPLOAD_BYTES) {
      toast.error(
        `This image is ${(file.size / 1024).toFixed(0)} KB; the limit is ${MAX_UPLOAD_BYTES / 1024} KB.`,
      );
      return;
    }
    setUploading(true);
    try {
      const buffer = await file.arrayBuffer();
      await setIdentity({ avatar: { kind: "custom", bytes: new Uint8Array(buffer) } });
      toast.success("Avatar updated");
    } catch (e) {
      toast.error(`Could not upload the image: ${String(e)}`);
    } finally {
      setUploading(false);
      if (fileInputRef.current) fileInputRef.current.value = "";
    }
  };

  return (
    <section className="space-y-3 p-4" data-testid="identity-section">
      <SectionHeader id="identity">Identity</SectionHeader>
      <p className="text-sm text-muted-foreground">
        Your name and face in CodeSense. Private to this profile on this machine; never shared with
        your team.
      </p>

      {showName && (
        <SettingRow label="Your name">
          <div className="flex items-center gap-2">
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  void saveName();
                }
              }}
              aria-label="Your display name"
              data-testid="identity-name-input"
              className="min-w-0 flex-1 rounded border border-border bg-background px-3 py-1.5 text-sm outline-none focus:border-ring"
              placeholder="e.g. Ada Lovelace"
              maxLength={80}
            />
            <Button
              type="button"
              size="sm"
              disabled={!nameDirty || savingName}
              onClick={() => void saveName()}
              data-testid="identity-name-save"
            >
              Save
            </Button>
          </div>
          <p className="mt-1 text-xs text-muted-foreground">
            Shown on your messages, the identity menu and anywhere you appear.
          </p>
        </SettingRow>
      )}

      {showAvatar && (
        <SettingRow label="Your avatar">
          <div className="flex flex-col gap-3">
            <div className="flex items-center gap-3">
              <AgentFaceView face={previewFace} size={48} />
              <div className="flex min-w-0 flex-col gap-1 text-sm">
                <span className="truncate font-medium">{currentUser?.display_name ?? name}</span>
                <span className="text-xs text-muted-foreground">
                  {chosenRef
                    ? chosenRef.startsWith("data:")
                      ? "Custom upload"
                      : chosenRef
                    : "Initials of your name"}
                </span>
              </div>
              <div className="ml-auto flex gap-2">
                <input
                  ref={fileInputRef}
                  type="file"
                  accept={ACCEPT}
                  className="hidden"
                  onChange={(e) => {
                    const file = e.target.files?.[0];
                    if (file) void uploadFile(file);
                  }}
                  data-testid="identity-avatar-file"
                />
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  disabled={uploading}
                  onClick={() => fileInputRef.current?.click()}
                  data-testid="identity-avatar-upload"
                >
                  {uploading ? "Uploading…" : "Upload image"}
                </Button>
                {chosenRef && (
                  <Button
                    type="button"
                    size="sm"
                    variant="ghost"
                    onClick={() => void pickFace(null)}
                    data-testid="identity-avatar-clear"
                  >
                    Clear
                  </Button>
                )}
              </div>
            </div>
            <p className="text-xs text-muted-foreground">
              Pick a face from a pack, or upload a square PNG, WebP or SVG up to{" "}
              {MAX_UPLOAD_BYTES / 1024} KB.
            </p>
            <div className="space-y-3">
              {avatars.packs.map((pack) => (
                <div key={pack.id}>
                  <p className="mb-1 text-xs font-medium text-muted-foreground">
                    {pack.name}
                    {pack.scope === "personal" && (
                      <span className="ml-1 text-[var(--ag-faint)]">· only you</span>
                    )}
                  </p>
                  <div
                    className="flex flex-wrap gap-1.5"
                    data-testid={`identity-avatar-pack-${pack.id}`}
                  >
                    {pack.faces.map((face) => {
                      const selected = chosenFace === face.ref;
                      return (
                        <button
                          key={face.ref}
                          type="button"
                          aria-label={face.label ?? face.id}
                          aria-pressed={selected}
                          onClick={() => void pickFace(face.ref)}
                          data-testid={`identity-avatar-face-${face.ref}`}
                          className={`rounded ring-offset-2 ring-offset-background transition-shadow hover:ring-2 hover:ring-ring ${
                            selected ? "ring-2 ring-ring" : ""
                          }`}
                        >
                          <AgentFaceView
                            face={{ kind: "image", ref: face.ref, src: face.src }}
                            size={32}
                          />
                        </button>
                      );
                    })}
                  </div>
                </div>
              ))}
            </div>
          </div>
        </SettingRow>
      )}
    </section>
  );
}
