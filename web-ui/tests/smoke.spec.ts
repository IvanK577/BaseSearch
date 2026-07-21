import { expect, test } from "@playwright/test";

// End-to-end smoke of the browser workspace against a running server with a
// seeded database.

// Analytics/risk gate whole-database scans behind an explicit consent button.
// Wait for either the consent screen or the loaded page, then click through.
async function enterWholeDb(page: import("@playwright/test").Page) {
  const wholeDb = page.locator("button", { hasText: /whole database/i });
  const loaded = page.locator(".tab, .stat-card").first();
  await expect(wholeDb.or(loaded)).toBeVisible();
  if (await wholeDb.isVisible()) {
    await wholeDb.click();
  }
}

test("workspace shell loads", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator(".brand-name")).toHaveText("Base Search");
  await expect(page.locator(".nav-item")).toHaveCount(8);
});

test("search returns results and opens a record", async ({ page }) => {
  await page.goto("/#/search");
  await page.fill(".searchbar input.input", "coffee");
  await page.click(".searchbar button.btn-primary");
  const firstRow = page.locator("table.grid tbody tr").first();
  await expect(firstRow).toBeVisible();
  await firstRow.click();
  await expect(page.locator(".drawer")).toBeVisible();
});

test("empty search does not reuse the previous query", async ({ page }) => {
  await page.goto("/#/search");
  await page.fill(".searchbar input.input", "coffee");
  await page.click(".searchbar button.btn-primary");
  const found = page.locator(".muted strong");
  await expect(found).toBeVisible();
  const narrow = await found.textContent();

  await page.fill(".searchbar input.input", "");
  await page.click(".searchbar button.btn-primary");
  // The count must change to the full database rather than showing the stale
  // "coffee" result count.
  await expect(found).not.toHaveText(narrow ?? "");
});

test("import screen renders without crashing", async ({ page }) => {
  await page.goto("/#/imports");
  await expect(page.locator(".dropzone")).toBeVisible();
  await expect(page.locator(".panel", { hasText: "History" }).first()).toBeVisible();
});

test("analytics shows real numbers for the current query", async ({ page }) => {
  await page.goto("/#/search");
  await page.fill(".searchbar input.input", "coffee");
  await page.click(".searchbar button.btn-primary");
  await expect(page.locator("table.grid tbody tr").first()).toBeVisible();

  // Navigate within the SPA so the working query is preserved.
  await page.click('a.nav-item[href="#/analytics"]');
  await expect(page.locator(".stat-card").first()).toBeVisible();
  const values = await page.locator(".stat-value").allTextContents();
  // At least one headline number must be non-zero.
  expect(values.some((v) => /[1-9]/.test(v))).toBeTruthy();
});

test("analytics and export keep using the last applied query", async ({ page }) => {
  await page.goto("/#/search");
  await page.fill(".searchbar input.input", "coffee");
  await Promise.all([
    page.waitForResponse((response) => response.url().includes("/search")),
    page.click(".searchbar button.btn-primary"),
  ]);
  await expect(page.locator("table.grid tbody tr").first()).toBeVisible();

  // Editing the draft must not silently change the dataset shown elsewhere.
  await page.fill(".searchbar input.input", "Lenovo");

  const analyticsRequestPromise = page.waitForRequest((request) =>
    request.url().includes("/analytics"),
  );
  await page.click('a.nav-item[href="#/analytics"]');
  const analyticsBody = (await analyticsRequestPromise).postDataJSON() as {
    query: { text: string };
  };
  expect(analyticsBody.query.text).toBe("coffee");

  const exportCountPromise = page.waitForRequest((request) =>
    request.url().includes("/count"),
  );
  await page.click('a.nav-item[href="#/exports"]');
  const exportBody = (await exportCountPromise).postDataJSON() as {
    query: { text: string };
  };
  expect(exportBody.query.text).toBe("coffee");
});

test("clicking a month drills in without an error banner", async ({ page }) => {
  await page.goto("/#/analytics");
  // No query yet: analyze the whole database so the monthly chart appears.
  await enterWholeDb(page);
  const firstCol = page.locator(".chart-col").first();
  await expect(firstCol).toBeVisible();

  // A date field only accepts a range filter; clicking a month must build a
  // valid range (not `StartsWith`) so analytics never errors out.
  await Promise.all([
    page.waitForResponse((r) => r.url().includes("/analytics")),
    firstCol.click(),
  ]);
  await expect(page.locator(".banner")).toHaveCount(0);
  // The filter collapsed the range to the single clicked month.
  await expect(page.locator(".chart-col")).toHaveCount(1);
});

test("analytics company section is a sortable table with export", async ({ page }) => {
  await page.goto("/#/analytics");
  await enterWholeDb(page);
  await page.locator(".tab", { hasText: "Companies" }).click();
  await expect(page.locator("button", { hasText: "Export CSV" }).first()).toBeVisible();
  const nameHeader = page.locator("thead th", { hasText: "Name" }).first();
  await nameHeader.click();
  await expect(nameHeader).toContainText(/[▲▼]/);
});

test("drilling a month shows a removable filter chip", async ({ page }) => {
  await page.goto("/#/analytics");
  await enterWholeDb(page);
  const firstCol = page.locator(".chart-col").first();
  await expect(firstCol).toBeVisible();
  await Promise.all([
    page.waitForResponse((r) => r.url().includes("/analytics")),
    firstCol.click(),
  ]);
  const chip = page.locator(".chip").first();
  await expect(chip).toBeVisible();
  await expect(page.locator("button", { hasText: "Clear all" })).toBeVisible();
  await Promise.all([
    page.waitForResponse((r) => r.url().includes("/analytics")),
    chip.locator("button").click(),
  ]);
  await expect(page.locator(".chip")).toHaveCount(0);
});

test("report tab renders a printable summary", async ({ page }) => {
  await page.goto("/#/analytics");
  await enterWholeDb(page);
  await page.locator(".tab", { hasText: "Report" }).click();
  await expect(
    page.locator(".section-title", { hasText: "Report for the current query" }),
  ).toBeVisible();
  await expect(page.locator("button", { hasText: "Print / Save PDF" })).toBeVisible();
  await expect(page.locator(".stat-card").first()).toBeVisible();
});

test("compare tab shows a difference table", async ({ page }) => {
  await page.goto("/#/analytics");
  await enterWholeDb(page);
  await page.locator(".tab", { hasText: "Compare" }).click();
  await page.fill("input.input[placeholder]", "Lenovo");
  await page.locator("button.btn-primary", { hasText: "Compare" }).click();
  await expect(page.locator(".section-title", { hasText: "Difference" })).toBeVisible();
  await expect(page.locator("table.grid tbody tr").first()).toBeVisible();
});

test("price risk screen analyzes and reports honestly", async ({ page }) => {
  await page.goto("/#/risk");
  await enterWholeDb(page);
  await expect(page.locator(".stat-card", { hasText: "Flagged rows" })).toBeVisible();
  // Small seed data may not produce robust cohorts; the screen must either
  // list flagged rows or say explicitly that no robust signal was found —
  // never render a blank area.
  await expect(
    page
      .locator("table.grid tbody tr")
      .first()
      .or(page.locator(".empty-state", { hasText: /No robust price-risk signals/i })),
  ).toBeVisible();
});

test("clicking a column header sorts the results", async ({ page }) => {
  await page.goto("/#/search");
  await Promise.all([
    page.waitForResponse((r) => r.url().includes("/search")),
    page.click(".searchbar button.btn-primary"),
  ]);
  await expect(page.locator("table.grid tbody tr").first()).toBeVisible();

  const idx = await valueIndex(page);
  const firstValue = async () =>
    numeric(
      (await page
        .locator("table.grid tbody tr")
        .first()
        .locator("td")
        .nth(idx)
        .textContent()) ?? "",
    );
  const header = page.locator("thead th", { hasText: "Value USD" });

  await Promise.all([
    page.waitForResponse((r) => r.url().includes("/search")),
    header.click(),
  ]);
  await expect(header).toContainText("▲");
  await page.waitForTimeout(150);
  const asc = await firstValue();

  await Promise.all([
    page.waitForResponse((r) => r.url().includes("/search")),
    header.click(),
  ]);
  await expect(header).toContainText("▼");
  await page.waitForTimeout(150);
  const desc = await firstValue();

  expect(desc).toBeGreaterThanOrEqual(asc);
});

test("EDRPOU link opens the company dossier", async ({ page }) => {
  await page.goto("/#/search");
  await page.click(".searchbar button.btn-primary");
  await expect(page.locator("table.grid tbody tr").first()).toBeVisible();
  const link = page.locator("table.grid tbody .link-cell").first();
  await link.click();
  await expect(page.locator(".section-title", { hasText: "Company dossier" })).toBeVisible();
});

async function valueIndex(page: import("@playwright/test").Page): Promise<number> {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll("table.grid thead th")).findIndex((t) =>
      /Value USD/.test(t.textContent ?? ""),
    ),
  );
}

function numeric(text: string): number {
  return parseFloat(text.replace(/[^0-9.\-]/g, "")) || 0;
}

for (const [name, size] of [
  ["mobile", { width: 375, height: 812 }],
  ["tablet", { width: 768, height: 1024 }],
  ["desktop", { width: 1280, height: 800 }],
] as const) {
  test(`no full-page horizontal overflow on ${name}`, async ({ page }) => {
    await page.setViewportSize(size);
    await page.goto("/#/search");
    const overflow = await page.evaluate(
      () =>
        document.documentElement.scrollWidth -
        document.documentElement.clientWidth,
    );
    expect(overflow).toBeLessThanOrEqual(0);
  });
}
