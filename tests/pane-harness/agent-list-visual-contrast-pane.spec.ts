// SPDX-License-Identifier: MIT
import { writeFileSync } from "node:fs";
import { checkPaneContrast } from "../conformance/checks";
import { AGENT_LIST_PANE } from "./agent-list-fixtures";
import { test, expect } from "./harness";
import { previewWorld } from "./agent-list-visual-fixtures";
const LIVE = process.env.AGP9_CONTRAST_PANE || AGENT_LIST_PANE;
const themes = ["codesense", "nord", "dracula", "github", "catppuccin", "confluence"];
const schemes = ["light", "dark"] as const;
const arranges = ["teams", "manager"] as const;
for (const id of themes)
  for (const scheme of schemes)
    for (const arrange of arranges) {
      test(`contrast ${id} ${scheme} ${arrange}`, async ({ mountPane }, testInfo) => {
        const pane = await mountPane(LIVE, {
          ...previewWorld({ arrange }),
          theme: { id: id as any, scheme },
        });
        await expect(pane.frame.locator(".al-row").first()).toBeVisible();
        const vars = await pane.page.evaluate(
          async ({ id, scheme }) => {
            const { themeTokens } = await import("/src/themes/index.ts" as any);
            return themeTokens(id, scheme);
          },
          { id, scheme },
        );
        await pane.setTheme({ id: id as any, scheme: scheme === "dark" ? "light" : "dark" });
        await pane.frame.evaluate(() => new Promise((r) => setTimeout(r, 200)));
        await pane.setTheme({ id: id as any, scheme, vars });
        await pane.frame.evaluate(() => new Promise((r) => setTimeout(r, 500)));
        const probe = await pane.frame.evaluate(() => [
          getComputedStyle(document.body).backgroundColor,
          getComputedStyle(document.body).color,
        ]);
        const res = await pane.frame.evaluate(() => {
          const cv = document.createElement("canvas");
          cv.width = cv.height = 1;
          const cx = cv.getContext("2d", { willReadFrequently: true })!;
          const rgba = (c: string): number[] => {
            cx.clearRect(0, 0, 1, 1);
            cx.fillStyle = "#000";
            cx.fillStyle = c;
            cx.fillRect(0, 0, 1, 1);
            const d = cx.getImageData(0, 0, 1, 1).data;
            return [d[0], d[1], d[2], d[3] / 255];
          };
          const over = (top: number[], bot: number[]) => {
            const a = top[3];
            return [
              top[0] * a + bot[0] * (1 - a),
              top[1] * a + bot[1] * (1 - a),
              top[2] * a + bot[2] * (1 - a),
              1,
            ];
          };
          const bgOf = (el: Element | null): number[] => {
            const layers: number[][] = [];
            for (let e = el; e; e = e.parentElement) {
              const c = rgba(getComputedStyle(e).backgroundColor);
              if (c[3] > 0) layers.push(c);
              if (c[3] >= 1) break;
            }
            let base = [255, 255, 255, 1];
            if (!layers.length || layers[layers.length - 1][3] < 1) {
              const cs = getComputedStyle(document.documentElement).colorScheme;
              base = cs.includes("dark") && !cs.includes("light") ? [0, 0, 0, 1] : base;
            }
            for (let i = layers.length - 1; i >= 0; i--) base = over(layers[i], base);
            return base;
          };
          const opac = (el: Element | null) => {
            let o = 1;
            for (let e = el; e; e = e.parentElement) o *= parseFloat(getComputedStyle(e).opacity);
            return o;
          };
          const lum = (c: number[]) => {
            const f = (v: number) => {
              v /= 255;
              return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
            };
            return 0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2]);
          };
          const ratio = (a: number[], b: number[]) => {
            const x = lum(a),
              y = lum(b);
            return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
          };
          const fails: any[] = [];
          const textRatios: number[] = [];
          const workspaceCounts: number[] = [];
          const selectors: Record<
            string,
            { ratio: number; selector: string; ink: string; background: number[] }
          > = {};
          const roleInk: Record<string, string> = {};
          const swatch = document.createElement("span");
          swatch.style.cssText = "position:fixed;visibility:hidden";
          document.querySelector(".al")!.append(swatch);
          for (const role of ["muted", "faint"]) {
            swatch.style.color = `var(--al-${role})`;
            roleInk[role] = JSON.stringify(rgba(getComputedStyle(swatch).color));
          }
          swatch.remove();
          const record = (
            key: string,
            el: Element,
            cr: number,
            ink: string,
            background: number[],
          ) => {
            if (!selectors[key] || cr < selectors[key].ratio)
              selectors[key] = {
                ratio: cr,
                selector: `${el.tagName.toLowerCase()}.${[...el.classList].join(".")}`,
                ink,
                background,
              };
          };
          let checked = 0;
          const w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
          const seen = new Set<Element>();
          for (let n = w.nextNode(); n; n = w.nextNode()) {
            if (!n.textContent!.trim()) continue;
            const el = n.parentElement!;
            if (el.closest("defs, mask, clipPath")) continue;
            if (seen.has(el)) continue;
            seen.add(el);
            const cs = getComputedStyle(el);
            if (cs.visibility === "hidden" || cs.display === "none") continue;
            const r = el.getBoundingClientRect();
            if (!r.width || !r.height || r.bottom < 0 || r.top > innerHeight) continue;
            const fs = parseFloat(cs.fontSize);
            if (fs >= 18) continue;
            const bg = bgOf(el);
            const fg0 = rgba(cs.color);
            const o = opac(el);
            const fg = over([fg0[0], fg0[1], fg0[2], fg0[3] * o], bg);
            const cr = ratio(fg, bg);
            checked++;
            textRatios.push(cr);
            if (el.matches(".al-ws-count, .al-ws-tail, .al-ws-tail .al-hot")) {
              workspaceCounts.push(cr);
              record("workspaceCount", el, cr, cs.color, bg);
            }
            if (el.matches(".al-ws-count[data-needs], .al-ws-tail .al-hot"))
              record("needsWorkspaceCount", el, cr, cs.color, bg);
            for (const role of ["muted", "faint"])
              if (JSON.stringify(fg0) === roleInk[role]) record(role, el, cr, cs.color, bg);
            if (cr < 4.5)
              fails.push({
                cls:
                  (el.className && typeof el.className === "string"
                    ? el.className.split(" ")[0]
                    : el.tagName.toLowerCase()) || el.tagName.toLowerCase(),
                text: n.textContent!.trim().slice(0, 24),
                ratio: +cr.toFixed(2),
                fs,
              });
          }
          const nonText: { element: string; ratio: number }[] = [];
          for (const el of document.body.querySelectorAll("*")) {
            const cs = getComputedStyle(el),
              rect = el.getBoundingClientRect();
            if (
              !rect.width ||
              !rect.height ||
              cs.visibility !== "visible" ||
              opac(el) !== 1 ||
              el.closest(
                '[disabled], [aria-disabled="true"], [inert], defs, mask, clipPath, pattern, symbol',
              )
            )
              continue;
            const element = [el, el.parentElement, el.parentElement?.parentElement]
              .filter(Boolean)
              .map(
                (e) =>
                  `${e!.tagName}.${[...e!.classList].join(".")}${e!.getAttribute("data-mark") ? `[data-mark=${e!.getAttribute("data-mark")}]` : ""}`,
              )
              .join(" < ");
            const add = (ink: string, bg: number[]) => {
              const c = rgba(ink);
              if (c[3]) {
                const cr = ratio(over(c, bg), bg);
                nonText.push({ element, ratio: cr });
                if (el.matches("rect")) record("rect", el, cr, ink, bg);
              }
            };
            if (el.matches('button, input, textarea, select, [role="button"], a[href]')) {
              for (const side of ["top", "right", "bottom", "left"]) {
                if (
                  parseFloat(cs.getPropertyValue(`border-${side}-width`)) &&
                  !["none", "hidden"].includes(cs.getPropertyValue(`border-${side}-style`))
                )
                  add(cs.getPropertyValue(`border-${side}-color`), bgOf(el.parentElement));
              }
            }
            if (
              el instanceof SVGElement &&
              el.matches("path, rect, circle, ellipse, line, polyline, polygon")
            ) {
              for (const ink of [cs.fill, cs.stroke]) if (ink !== "none") add(ink, bgOf(el));
            }
          }
          const tree: number[] = [];
          for (const t of document.querySelectorAll(".al-tree, .al-guide")) {
            const cs = getComputedStyle(t);
            const r = t.getBoundingClientRect();
            if (!r.height) continue;
            const bg = bgOf(t.parentElement);
            const o = opac(t);
            if (t.matches(".al-guide")) {
              const c = rgba(cs.backgroundColor);
              tree.push(+ratio(over([c[0], c[1], c[2], c[3] * o], bg), bg).toFixed(2));
            }
            for (const side of ["borderLeft", "borderBottom"] as const) {
              if (parseFloat((cs as any)[side + "Width"]) <= 0) continue;
              const c = rgba((cs as any)[side + "Color"]);
              tree.push(+ratio(over([c[0], c[1], c[2], c[3] * o], bg), bg).toFixed(2));
            }
          }
          return {
            checked,
            selectors,
            textMin: Math.min(...textRatios),
            workspaceCountMin: workspaceCounts.length ? Math.min(...workspaceCounts) : null,
            nonTextMin: Math.min(...nonText.map((sample) => sample.ratio)),
            nonTextFailures: nonText.filter((sample) => sample.ratio < 3),
            fails,
            treeMin: tree.length ? Math.min(...tree) : null,
            treeN: tree.length,
          };
        });
        const conformance = await checkPaneContrast(pane.frame, `${id}/${scheme}/${arrange}`);
        writeFileSync(
          testInfo.outputPath("contrast.json"),
          JSON.stringify({ id, scheme, arrange, probe, ...res, conformance }, null, 2),
        );
        await testInfo.attach("contrast", {
          body: JSON.stringify({ id, scheme, arrange, probe, ...res }),
          contentType: "application/json",
        });
        expect(res.checked).toBeGreaterThan(0);
        expect(res.treeN).toBeGreaterThan(0);
        expect(res.treeMin).toBeGreaterThanOrEqual(1.5);
        expect(res.nonTextFailures).toEqual([]);
        expect(res.fails).toEqual([]);
        expect(conformance).toEqual([]);
      });
    }
