// SPDX-License-Identifier: MIT
import { test, expect } from "@playwright/test";

/**
 * Basic accessibility E2E tests.
 *
 * Validates foundational a11y requirements: lang attribute,
 * keyboard focus, unique IDs, alt text, and color contrast.
 *
 * Tags: @P2 @E2E
 */
test.describe("@P2 @E2E Accessibility", () => {
  test("html element has lang attribute", async ({ page }) => {
    // Given: the app is loaded
    await page.goto("/");

    // Then: the html element should declare a language
    const lang = await page.locator("html").getAttribute("lang");
    expect(lang).toBeTruthy();
    expect(lang).toBe("en");
  });

  test("interactive buttons are keyboard focusable", async ({ page }) => {
    // Given: the app is loaded and has rendered content
    await page.goto("/");
    await page.waitForLoadState("domcontentloaded");

    // When: we find all button elements on the page
    const buttons = page.locator("button");
    const count = await buttons.count();

    // Then: buttons must exist and be focusable via Tab
    expect(count).toBeGreaterThan(0);

    // Focus the first button by tabbing into the page
    await page.keyboard.press("Tab");

    // Keep tabbing until we land on a button (up to 20 tabs)
    let foundFocusedButton = false;
    for (let i = 0; i < 20; i++) {
      const activeTag = await page.evaluate(() => {
        return document.activeElement?.tagName?.toLowerCase();
      });
      if (activeTag === "button") {
        foundFocusedButton = true;
        break;
      }
      await page.keyboard.press("Tab");
    }

    expect(foundFocusedButton).toBe(true);
  });

  test("no duplicate IDs on the page", async ({ page }) => {
    // Given: the app is loaded
    await page.goto("/");
    await page.waitForLoadState("domcontentloaded");

    // When: we collect all elements with an id attribute
    const duplicates = await page.evaluate(() => {
      const allElements = document.querySelectorAll("[id]");
      const idCounts = new Map<string, number>();

      allElements.forEach((el) => {
        const id = el.id;
        if (id) {
          idCounts.set(id, (idCounts.get(id) ?? 0) + 1);
        }
      });

      const dupes: string[] = [];
      idCounts.forEach((count, id) => {
        if (count > 1) {
          dupes.push(`${id} (${count}x)`);
        }
      });

      return dupes;
    });

    // Then: there should be no duplicate IDs
    expect(duplicates).toEqual([]);
  });

  test("images have alt text if any exist", async ({ page }) => {
    // Given: the app is loaded
    await page.goto("/");
    await page.waitForLoadState("domcontentloaded");

    // When: we find all img elements
    const images = page.locator("img");
    const count = await images.count();

    // Then: every image should have an alt attribute (can be empty string for decorative)
    for (let i = 0; i < count; i++) {
      const alt = await images.nth(i).getAttribute("alt");
      const src = await images.nth(i).getAttribute("src");
      // alt must be present (null means attribute is missing entirely)
      expect(alt, `Image ${src ?? i} is missing alt attribute`).not.toBeNull();
    }
    // If no images exist, test passes trivially
  });

  test("text is visible against background — basic contrast check", async ({ page }) => {
    // Given: the app is loaded with dark theme
    await page.addInitScript(() => {
      localStorage.setItem("theme", "dark");
    });
    await page.goto("/");
    await page.waitForLoadState("domcontentloaded");

    // When: we sample the body background and text colors
    const colors = await page.evaluate(() => {
      const body = document.body;
      const style = window.getComputedStyle(body);
      return {
        bg: style.backgroundColor,
        color: style.color,
      };
    });

    // Then: background and foreground should not be identical
    // (a basic sanity check — not a full WCAG contrast calculation)
    expect(colors.bg).not.toBe(colors.color);

    // And: both should be valid color values (not empty or transparent for bg)
    expect(colors.bg).toBeTruthy();
    expect(colors.color).toBeTruthy();
    expect(colors.bg).not.toBe("rgba(0, 0, 0, 0)"); // not transparent
  });

  test("text is visible against background in light theme", async ({ page }) => {
    // Given: the app is loaded with light theme
    await page.addInitScript(() => {
      localStorage.setItem("theme", "light");
    });
    await page.goto("/");
    await page.waitForLoadState("domcontentloaded");

    // When: we sample the body background and text colors
    const colors = await page.evaluate(() => {
      const body = document.body;
      const style = window.getComputedStyle(body);
      return {
        bg: style.backgroundColor,
        color: style.color,
      };
    });

    // Then: background and foreground should not be identical
    expect(colors.bg).not.toBe(colors.color);

    // And: both should be valid non-transparent values
    expect(colors.bg).toBeTruthy();
    expect(colors.color).toBeTruthy();
    expect(colors.bg).not.toBe("rgba(0, 0, 0, 0)");
  });

  test("page title is set", async ({ page }) => {
    // Given: the app is loaded
    await page.goto("/");

    // Then: the page should have a meaningful title (not blank)
    const title = await page.title();
    expect(title).toBeTruthy();
    expect(title.length).toBeGreaterThan(0);
  });
});
