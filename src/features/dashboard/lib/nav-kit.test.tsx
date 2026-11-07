// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeAll, expect, it } from "vitest";
import { StageRailShape } from "@/shared/components/stage-rail";
import { StatusMark } from "@/shared/components/status-mark";
import { FileKindIcon, FILE_KINDS, type FileKind } from "@/shared/lib/icons/file-kind";
import type { StatusRole } from "@/tauri/commands";
import { STATUS_MARKS, type StatusMarkKind } from "@/shared/lib/status-mark";

type Kit = {
  stageRail(role: StatusRole | undefined, ready?: boolean): HTMLElement;
  statusMark(mark: StatusMarkKind, options?: { size?: 6 | 8; still?: boolean }): HTMLElement;
  fileKindIcon(kind: FileKind): SVGElement;
  navGlyph(name: "changes" | "history" | "grouped"): SVGElement;
  fileKindOf(path: string): FileKind;
  stripViewerExtension(name: string): string;
  stripPipelineExtension(name: string): string;
  inLens(lens: string, story: { role?: string; mark?: StatusMarkKind; ready?: boolean }): boolean;
  countLenses(
    stories: Iterable<{ role?: string; mark?: StatusMarkKind; ready?: boolean }>,
  ): Record<string, number>;
  stageGroupOf(story: { role?: string; mark?: StatusMarkKind }): string;
  compareNavNames(a: string, b: string): number;
};
let kit: Kit;
beforeAll(() => {
  const scope = { codesense: {} };
  new Function(
    "window",
    "document",
    readFileSync("src-tauri/src/http/routes/dashboard_kit/kit.js", "utf8"),
  )(scope, document);
  kit = (scope.codesense as { kit: Kit }).kit;
});
function dom(html: string) {
  const template = document.createElement("template");
  template.innerHTML = html;
  return template.content.firstElementChild!;
}
function normalized(node: Element) {
  return node.outerHTML.replace(/id="[^"]+"/g, 'id="mask"').replace(/url\(#[^)]+\)/g, "url(#mask)");
}
for (const role of [undefined, "not_started", "active", "review", "blocked", "done"] as const)
  for (const ready of [false, true])
    it(`stage rail matches native: ${role}, ready=${ready}`, () => {
      expect(normalized(kit.stageRail(role, ready))).toBe(
        normalized(dom(renderToStaticMarkup(<StageRailShape role={role} ready={ready} />))),
      );
    });
for (const kind of FILE_KINDS)
  it(`file kind matches native: ${kind}`, () => {
    expect(normalized(kit.fileKindIcon(kind))).toBe(
      normalized(dom(renderToStaticMarkup(<FileKindIcon kind={kind} />))),
    );
  });
for (const mark of STATUS_MARKS)
  for (const size of [6, 8] as const)
    for (const still of [false, true])
      it(`status mark matches native: ${mark}, ${size}px, still=${still}`, () => {
        const native = renderToStaticMarkup(
          still ? (
            <span className="cs-mark-still inline-flex">
              <StatusMark mark={mark} size={size} />
            </span>
          ) : (
            <StatusMark mark={mark} size={size} />
          ),
        );
        expect(normalized(kit.statusMark(mark, { size, still }))).toBe(normalized(dom(native)));
      });

for (const name of ["changes", "history", "grouped"] as const)
  it(`nav glyph matches native: ${name}`, async () => {
    const native = await import("@/features/dashboard/components/status-nav/nav-filter-field");
    const Component =
      name === "changes"
        ? native.ChangesIcon
        : name === "history"
          ? native.HistoryIcon
          : native.GroupedIcon;
    expect(normalized(kit.navGlyph(name))).toBe(
      normalized(dom(renderToStaticMarkup(<Component />))),
    );
  });
