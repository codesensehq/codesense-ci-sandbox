// SPDX-License-Identifier: MIT
import { stripPipelineExtension } from "./pipeline-utils";

/**
 * Human-facing labels for story cards. The scanner hands us a file stem
 * (`ll-214-refund-endpoint-idempotency-keys`); cards read
 * `LL-214 — Refund endpoint idempotency keys` instead, with the filename kept
 * for the hover tooltip.
 */

const ID_TITLE_SEPARATOR = " — ";

/** A leading `<letters>(-<letters>)*-<digits>[suffix]` segment, e.g. `sw-pl-22`, `ll-214`, `w-5b`. */
const STEM_ID_PREFIX = /^((?:[A-Za-z]+-)+\d+[a-z]?)(?=$|[-_.])/;

/** Separator characters allowed between an id and the text that follows it. */
const LEADING_SEPARATORS = /^[\s:.\-–—_]+/;

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Uppercased id derived from the stem's leading id segment, or `undefined`. */
function deriveStoryId(stem: string): string | undefined {
  const match = stem.match(STEM_ID_PREFIX);
  return match ? match[1].toUpperCase() : undefined;
}

/** Drop `id` (case-insensitive) and any separators after it from the start of `text`. */
function stripLeadingId(text: string, id: string | undefined): string {
  if (!id) return text;
  const prefix = new RegExp(`^${escapeRegExp(id)}(?=$|[\\s:.\\-–—_])`, "i");
  if (!prefix.test(text)) return text;
  return text.replace(prefix, "").replace(LEADING_SEPARATORS, "");
}

function capitalizeFirst(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/**
 * Turn a file stem into a readable title: drop the id prefix (the explicit
 * `storyId` when it matches, otherwise one derived from the stem), turn
 * dashes/underscores into spaces, and capitalize the first letter.
 */
export function humanizeStoryStem(stem: string, storyId?: string): string {
  const withoutId = stripLeadingId(stem, storyId ?? deriveStoryId(stem));
  const words = withoutId.replace(/[-_]+/g, " ").replace(/\s+/g, " ").trim();
  return capitalizeFirst(words);
}

/** Clean a body heading: drop a leading `Story ` word and the story id if it repeats it. */
function cleanHeading(heading: string, storyId: string | undefined): string {
  const trimmed = heading.trim();
  if (!storyId) return trimmed;
  const withoutStoryWord = trimmed.replace(/^story\s+/i, "");
  const withoutId = stripLeadingId(withoutStoryWord, storyId);
  return withoutId === withoutStoryWord ? trimmed : withoutId.trim();
}

export type StoryDisplayTitleInput = {
  /** Frontmatter `story_id`, when present. */
  storyId?: string;
  /** Body title (markdown first H1, HTML `<title>`/`<h1>`), when fetched. */
  heading?: string | null;
  /** Metadata title (frontmatter `title:` / HTML `cs:title`), when fetched. Wins over `heading`. */
  titleMeta?: string | null;
  /** File name or stem of the story file. */
  fileName: string;
};

/**
 * The declared title text, without the id: the metadata title when present,
 * then the body heading; a leading repeat of the id is dropped from either. A
 * declared title that is only the id (e.g. `title: SW-LN-34`) falls through.
 * `undefined` when neither declares one.
 */
export function resolveDeclaredStoryTitle({
  storyId,
  heading,
  titleMeta,
}: Pick<StoryDisplayTitleInput, "storyId" | "heading" | "titleMeta">): string | undefined {
  const id = storyId?.trim() || undefined;
  return (
    [titleMeta, heading]
      .map((declared) => (declared?.trim() ? cleanHeading(declared, id) : ""))
      .find(Boolean) || undefined
  );
}

/**
 * `<story_id> — <title>`. Title is the metadata title when present, then the
 * body heading, otherwise the humanized stem; a leading repeat of the id is
 * dropped from either. With no id the title shows alone; with no title, the id.
 */
export function formatStoryDisplayTitle({
  storyId,
  heading,
  titleMeta,
  fileName,
}: StoryDisplayTitleInput): string {
  const stem = stripPipelineExtension(fileName);
  const id = storyId?.trim() || deriveStoryId(stem);
  const title =
    resolveDeclaredStoryTitle({ storyId: id, heading, titleMeta }) || humanizeStoryStem(stem, id);
  if (id && title) return `${id}${ID_TITLE_SEPARATOR}${title}`;
  return id || title || stem;
}

/**
 * The metadata title the scanner already carries: frontmatter `title:` (or HTML
 * `cs:title`) lands in `openTags.title`, so surfaces that list many stories read
 * it with no file fetch.
 */
export function storyMetadataTitle(story: {
  openTags?: Record<string, string[]> | null;
}): string | undefined {
  return story.openTags?.title?.find((value) => value.trim())?.trim();
}
