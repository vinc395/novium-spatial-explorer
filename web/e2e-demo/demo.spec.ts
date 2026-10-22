import { expect, test } from "@playwright/test";

async function chooseSource(page: import("@playwright/test").Page, label: string, value: string) {
  await page.getByRole("button", { name: label, exact: true }).click();
  const list = page.getByRole("listbox", { name: label, exact: true });
  const filter = list.getByRole("searchbox");
  if (await filter.count()) await filter.fill(value);
  else {
    const closed = list.locator(".picker-group-head[aria-expanded=false]");
    while (await closed.count()) await closed.first().click();
  }
  await list.locator(`[data-source="${value}"]`).click();
}

test("the online demo works from static files alone", async ({ page }) => {
  const errors: string[] = [];
  const failed: string[] = [];
  const served = new Map<string, number>();
  page.on("pageerror", (e) => errors.push(String(e)));
  page.on("response", (r) => {
    const m = r.url().match(/\/api\/(sections\/[^/]+\/)?([a-z]+)/);
    if (m) served.set(m[2], (served.get(m[2]) ?? 0) + 1);
    if (r.status() >= 400) failed.push(`${r.status()} ${r.url()}`);
  });
  await page.goto("index.html");
  await expect(page.locator(".demo-badge")).toContainText("Demo");
  await expect(page.getByLabel("Section")).toHaveValue("DEMO1");
  await expect(page.locator(".legend-title").first()).toContainText("fine_cell_type");
  await expect.poll(() => served.get("images") ?? 0).toBeGreaterThan(0);

  // A cell click: details, and the LR values on demand.
  const box = (await page.locator("canvas").boundingBox())!;
  let picked = false;
  for (const [dx, dy] of [[0, 0], [8, 0], [0, 8], [-8, 0], [12, 12]]) {
    const resp = page.waitForResponse((r) => r.url().includes("/cells/key/"), { timeout: 3000 }).catch(() => null);
    await page.mouse.click(box.x + box.width / 2 + dx, box.y + box.height / 2 + dy);
    if (await resp) {
      picked = true;
      break;
    }
  }
  expect(picked, `no cell picked; failed requests: ${failed.join(", ")}`).toBe(true);
  await expect(page.locator(".inspector code")).toHaveText(/^[a-p]{8}-1$/);
  await page.locator(".lr-at-cell summary").click();
  await expect(page.getByRole("table", { name: "LR at this cell" }).locator("tr").first()).toBeVisible();
  await expect(page.locator(".inspector > table th", { hasText: /\((sender|receiver)\)/ })).toHaveCount(0);

  // Transcripts of a searched gene, morphology channels, colour by an LR score.
  await page.getByLabel("Search genes").fill("CD68");
  await page.getByLabel("Search genes").press("Enter");
  await expect.poll(() => served.get("transcripts") ?? 0).toBeGreaterThan(0);
  await page.getByLabel("Show all channels").check();
  await chooseSource(page, "Colour cells by", "lr:CXCL8_CXCR2:receiver");
  // The cell click folded the legend to make room for the cell's details.
  await page.getByRole("button", { name: /^Legend/ }).click();
  await expect(page.locator(".legend-title").first()).toContainText("CXCL8_CXCR2");

  // The niche grid, drawn smoothed by default.
  const gridBox = page.locator("section", { has: page.getByRole("heading", { name: "Segmentation-free grids" }) });
  await gridBox.getByRole("checkbox").first().check();
  await gridBox.getByLabel("Grid field").selectOption("niche_label");
  await expect(page.locator(".legend", { hasText: "niches: niche_label" })).toBeVisible();
  await expect(gridBox.getByRole("checkbox", { name: "smooth" })).toBeChecked();
  await expect(gridBox).toContainText(/drawn smoothed onto [\d.]+ µm pixels/);
  await gridBox.getByLabel(/Strength/).fill("3");
  await expect(gridBox).toContainText(/after a Gaussian of σ [\d.]+ µm/);
  expect(served.get("grids") ?? 0).toBeGreaterThan(0);

  // Side by side with the second section.
  await page.getByLabel("Number of panels").selectOption("2");
  await page.locator(".panel-frame").nth(1).locator("select").selectOption("DEMO2");
  await expect(page.locator(".panel-frame").nth(1).locator("select")).toHaveValue("DEMO2");

  // Server-only features say so; the figure export (in the browser) works.
  await expect(page.getByLabel("Gene signatures")).toContainText("not part of this online demo");
  await page.getByRole("button", { name: "Export", exact: true }).click();
  const dlg = page.getByRole("dialog", { name: "Export view" });
  await dlg.getByLabel("Format").selectOption("png");
  await dlg.getByLabel("Resolution").selectOption("1");
  const dl = page.waitForEvent("download");
  await dlg.getByRole("button", { name: "Export", exact: true }).click();
  expect((await dl).suggestedFilename()).toMatch(/^DEMO1\+DEMO2_view_\d+x\d+\.png$/);

  expect(failed).toEqual([]);
  expect(errors).toEqual([]);
});
