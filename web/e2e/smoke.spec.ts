import { expect, test } from "@playwright/test";

/** Pick an option in a grouped source picker ("Colour cells by", "Overlay") by its value. */
async function chooseSource(page: import("@playwright/test").Page, label: string, value: string) {
  await page.getByRole("button", { name: label, exact: true }).click();
  const list = page.getByRole("listbox", { name: label, exact: true });
  if (value) {
    const filter = list.getByRole("searchbox");
    if (await filter.count()) await filter.fill(value);
    else {
      const closed = list.locator(".picker-group-head[aria-expanded=false]");
      while (await closed.count()) await closed.first().click();
    }
  }
  await list.locator(`[data-source="${value}"]`).click();
  await expect(list).toHaveCount(0);
}

// Synthetic sections only (see scripts/verify.sh): A = rotated H&E + annotations, B = mirrored, legacy columns.

test("section loads, gene search, cell click, selection export, section switch", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.goto("/?section=A");
  await expect(page.getByLabel("Section")).toHaveValue("A");
  await expect(page.locator(".legend-title").first()).toContainText("fine_cell_type");
  await expect(page.locator("canvas")).toBeVisible();
  await expect(page.locator(".overlay.error")).toHaveCount(0);

  // Gene search adds a chip and updates the URL.
  await page.getByLabel("Search genes").fill("CD68");
  await page.getByLabel("Search genes").press("Enter");
  await expect(page.locator(".chips li")).toHaveCount(1);
  await expect(page).toHaveURL(/genes=CD68/);

  // Click near the centre of the canvas until a cell is hit (cells are dense in the fixture).
  const box = (await page.locator("canvas").boundingBox())!;
  let picked = false;
  for (const [dx, dy] of [[0, 0], [6, 0], [0, 6], [-6, 0], [0, -6], [10, 10]]) {
    const resp = page.waitForResponse((r) => r.url().includes("/cells/key/"), { timeout: 3000 }).catch(() => null);
    await page.mouse.click(box.x + box.width / 2 + dx, box.y + box.height / 2 + dy);
    if (await resp) {
      picked = true;
      break;
    }
  }
  expect(picked).toBe(true);
  await expect(page.locator(".inspector code")).toHaveText(/^[a-p]{8}-1$/);
  await expect(page.locator(".topgenes li").first()).toBeVisible();

  // LR values are not part of the click; they load when "LR at this cell" is opened.
  await expect(page.locator(".inspector > table th", { hasText: /\((sender|receiver)\)/ })).toHaveCount(0);
  const lrReq = page.waitForResponse((r) => r.url().includes("/cells/key/") && r.url().includes("groups=lr_score"));
  await page.locator(".lr-at-cell summary").click();
  expect((await lrReq).ok()).toBe(true);
  await expect(page.getByRole("table", { name: "LR at this cell" }).locator("tr").first()).toBeVisible();
  await expect(page.locator(".lr-at-cell summary")).toContainText(/scored here/);

  // Add to selection and export with original IDs; LR columns only when ticked.
  await page.getByRole("button", { name: "Add current" }).click();
  await expect(page.getByRole("heading", { name: /Selection \(1\)/ })).toBeVisible();
  const fs = await import("node:fs");
  const exportCsv = async () => {
    const download = page.waitForEvent("download");
    await page.getByRole("button", { name: "Export CSV" }).click();
    return fs.readFileSync((await (await download).path())!, "utf8");
  };
  const csv = await exportCsv();
  expect(csv.split("\n")[0]).toMatch(/^section_id,cell_id,x_um,y_um,/);
  expect(csv).toMatch(/\nA,[a-p]{8}-1,/);
  expect(csv.split("\n")[0]).not.toMatch(/lr:/);
  await page.getByLabel(/include the \d+ LR columns/).check();
  expect((await exportCsv()).split("\n")[0]).toMatch(/lr:/);

  // Switching section clears selection and genes (no cross-section leakage).
  await page.getByLabel("Section").selectOption("B");
  await expect(page).toHaveURL(/section=B/);
  await expect(page.getByRole("heading", { name: /Selection \(0\)/ })).toBeVisible();
  // Genes are a shared setting (by name) and stay selected across sections.
  await expect(page.locator(".chips li")).toHaveCount(1);
  expect(errors).toEqual([]);
});

test("API rejects traversal and unknown sections", async ({ request }) => {
  expect((await request.get("/api/v1/sections/..%2F..%2Fetc/manifest")).status()).toBe(400);
  expect((await request.get("/api/v1/sections/NOPE/manifest")).status()).toBe(404);
  expect((await request.get("/healthz")).ok()).toBe(true);
});

// Regression: tiles that leave the view and come back must get fresh deck.gl layers
// (reusing a finalized layer threw "deck.gl: assertion failed").
test("panning away and back re-creates tile layers without errors", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  page.on("console", (m) => {
    if (m.type() === "error" && !m.text().includes("uniform block")) errors.push(m.text());
  });
  await page.goto("/?section=A&genes=CD68,CD3E");
  await expect(page.locator(".chips li")).toHaveCount(2);
  const status = () => page.evaluate(() => (window as unknown as { __xenium: { status: () => { lodTx: string; lodCells: string } } }).__xenium.status());
  const fly = (x: number, y: number) =>
    page.evaluate(([x, y]) => (window as unknown as { __xenium: { viewerApi: { flyTo: (x: number, y: number, z: number, d: number) => void } } }).__xenium.viewerApi.flyTo(x, y, 0.5, 0), [x, y]);
  for (const x of [540, 5000, 540, 5000, 540]) {
    await fly(x, 380);
    await page.waitForTimeout(1200);
  }
  expect((await status()).lodTx).toMatch(/^points/);
  expect((await status()).lodCells).toBe("polygons");
  await expect(page.locator(".render-error")).toHaveCount(0);
  await expect(page.locator(".overlay.error")).toHaveCount(0);
  expect(errors).toEqual([]);
});

// Grid (niche) categories can be shown/hidden from the legend, like cell categories.
test("grid legend categories toggle and change the drawn layer", async ({ page }) => {
  await page.goto("/?section=A");
  await expect(page.locator(".legend-title").first()).toContainText("fine_cell_type");
  const gridBox = page.locator("section", { has: page.getByRole("heading", { name: "Segmentation-free grids" }) });
  await gridBox.getByRole("checkbox").first().check();
  await gridBox.getByLabel("Grid field").selectOption("niche_label");
  // Hide the cells so the canvas shows the grid over the H&E.
  await page.getByRole("checkbox", { name: "fill" }).uncheck();
  await page.getByRole("checkbox", { name: "outline" }).uncheck();
  const legend = page.locator(".legend", { hasText: "niches: niche_label" });
  await expect(legend).toBeVisible();
  const n = await legend.locator(".cats li").count();
  expect(n).toBeGreaterThan(2);
  await page.waitForTimeout(800);
  const canvas = page.locator("canvas");
  const before = await canvas.screenshot();

  await legend.getByRole("button", { name: /^show only / }).first().click();
  await expect(legend.locator(".cats li.off")).toHaveCount(n - 1);
  await legend.getByRole("button", { name: "hide all" }).click();
  await expect(legend.locator(".cats li.off")).toHaveCount(n);
  await page.waitForTimeout(800);
  const hiddenShot = await canvas.screenshot();
  expect(Buffer.compare(before, hiddenShot)).not.toBe(0);

  await legend.getByRole("button", { name: "show all" }).click();
  await expect(legend.locator(".cats li.off")).toHaveCount(0);
  await page.waitForTimeout(800);
  expect(Buffer.compare(before, await canvas.screenshot())).toBe(0);
});

test("multiview: two sections, linked zoom, active panel, shared colours", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.goto("/?sections=A,B&genes=CD68");
  await expect(page.getByLabel("Number of panels")).toHaveValue("2");
  await expect(page.getByLabel("Section for panel 1")).toHaveValue("A");
  await expect(page.getByLabel("Section for panel 2")).toHaveValue("B");
  const zooms = () => page.evaluate(() => (window as unknown as { __xenium: { zooms: () => Record<string, number> } }).__xenium.zooms());
  await expect.poll(async () => Object.keys(await zooms()).length).toBe(2);
  const z0 = await zooms();
  expect(z0.p0).toBeCloseTo(z0.p1, 6); // fit uses one common scale

  const box = (await page.locator("canvas").boundingBox())!;
  const left = { x: box.x + box.width * 0.25, y: box.y + box.height / 2 };
  const right = { x: box.x + box.width * 0.75, y: box.y + box.height / 2 };
  await page.mouse.move(left.x, left.y);
  await page.mouse.wheel(0, -300);
  await expect.poll(async () => (await zooms()).p0).toBeGreaterThan(z0.p0 + 0.1);
  let z1 = await zooms();
  expect(z1.p1).toBeCloseTo(z1.p0, 6); // linked

  await page.getByRole("checkbox", { name: "link zoom" }).uncheck();
  await page.mouse.move(left.x, left.y);
  await page.mouse.wheel(0, -300);
  await expect.poll(async () => (await zooms()).p0).toBeGreaterThan(z1.p0 + 0.1);
  z1 = await zooms();
  expect(Math.abs(z1.p0 - z1.p1)).toBeGreaterThan(0.1); // independent
  // Ticking it again links them immediately, at the active (left) panel's zoom.
  // (Each wheel step below must move the shared zoom: a panel that last zoomed itself
  // used to restart from its own old zoom after the other panel had zoomed.)
  await page.getByRole("checkbox", { name: "link zoom" }).check();
  await expect.poll(async () => (await zooms()).p1).toBeCloseTo(z1.p0, 6);
  // ...and they stay linked when zooming either panel.
  await page.mouse.move(right.x, right.y);
  await page.mouse.wheel(0, 300);
  await expect.poll(async () => (await zooms()).p1).toBeLessThan(z1.p0 - 0.1);
  let z2 = await zooms();
  expect(z2.p0).toBeCloseTo(z2.p1, 6);
  await page.getByRole("checkbox", { name: "link zoom" }).uncheck();
  await page.mouse.move(left.x, left.y);
  await page.mouse.wheel(0, -300);
  await expect.poll(async () => (await zooms()).p0).toBeGreaterThan(z2.p0 + 0.1);
  z1 = await zooms();

  // Alternate panels with linked zoom: every wheel step moves both panels.
  await page.getByRole("checkbox", { name: "link zoom" }).check();
  for (const [pt, dy] of [[right, 600], [left, -500], [right, -300], [left, -300], [right, 300]] as const) {
    const before = (await zooms()).p0;
    await page.mouse.move(pt.x, pt.y);
    await page.mouse.wheel(0, dy);
    await expect.poll(async () => Math.abs((await zooms()).p0 - before)).toBeGreaterThan(0.5);
    const z = await zooms();
    expect(z.p1).toBeCloseTo(z.p0, 6);
    expect(Math.sign(z.p0 - before)).toBe(dy < 0 ? 1 : -1);
  }

  // Clicking in the right panel makes it active; the inspector follows.
  for (const [dx, dy] of [[0, 0], [8, 0], [0, 8], [-8, 0], [12, 12]]) {
    const resp = page.waitForResponse((r) => r.url().includes("/sections/B/cells/key/"), { timeout: 3000 }).catch(() => null);
    await page.mouse.click(right.x + dx, right.y + dy);
    if (await resp) break;
  }
  await expect(page.locator(".panel-frame.active .panel-head select")).toHaveValue("B");
  await expect(page.getByRole("heading", { name: /Selected cell · B/ })).toBeVisible();
  await expect(page.locator(".inspector code")).toHaveText(/^[a-p]{8}-1$/);

  // The same category name has the same colour in both panels (a field both sections have).
  // The cell click folded the legend to make room for the cell's details: open it again.
  const legendHead = page.getByRole("button", { name: /^Legend/ });
  await expect(legendHead).toHaveAttribute("aria-expanded", "false");
  await legendHead.click();
  await chooseSource(page, "Colour cells by", "segmentation_method");
  const swatch = async (name: string) => page.locator(".legend .cats li", { hasText: name }).first().locator("input.sw-pick").inputValue();
  const name = (await page.locator(".legend .cats li .name").first().innerText()).trim();
  const inB = await swatch(name);
  await page.locator(".panel-frame").nth(0).locator(".panel-head").click({ position: { x: 4, y: 4 } });
  await expect(page.locator(".panel-frame.active .panel-head select")).toHaveValue("A");
  expect(await swatch(name)).toBe(inB);

  await page.getByLabel("Number of panels").selectOption("1");
  await expect(page.locator(".panel-head")).toHaveCount(0);
  await expect(page).toHaveURL(/section=A/);
  expect(errors).toEqual([]);
});

test("multiview: switching a panel's section keeps the other panel's view and the linked zoom", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  type Hook = { zooms: () => Record<string, number>; targets: () => Record<string, [number, number]> };
  const cams = () => page.evaluate(() => {
    const x = (window as unknown as { __xenium: Hook }).__xenium;
    return { z: x.zooms(), t: x.targets() };
  });
  await page.goto("/?sections=A,B");
  await expect.poll(async () => Object.keys((await cams()).z).length).toBe(2);
  const fitted = await cams();

  // Zoom into the left panel (linked zoom carries it to the right panel).
  const box = (await page.locator("canvas").boundingBox())!;
  await page.mouse.move(box.x + box.width * 0.2, box.y + box.height * 0.4);
  await page.mouse.wheel(0, -300);
  await expect.poll(async () => (await cams()).z.p0).toBeGreaterThan(fitted.z.p0 + 0.5);
  const before = await cams();

  // Switch the right panel from B to A: it is re-centred on A at the shared zoom,
  // and the left panel is not refitted.
  await page.getByLabel("Section for panel 2").selectOption("A");
  await expect.poll(async () => (await cams()).t.p1[0]).toBeCloseTo(fitted.t.p0[0], 3);
  const after = await cams();
  expect(after.z.p0).toBeCloseTo(before.z.p0, 6);
  expect(after.t.p0[0]).toBeCloseTo(before.t.p0[0], 3);
  expect(after.t.p0[1]).toBeCloseTo(before.t.p0[1], 3);
  expect(after.z.p1).toBeCloseTo(before.z.p0, 6);

  // Pressing F refits everything.
  await page.keyboard.press("f");
  await expect.poll(async () => (await cams()).z.p0).toBeLessThan(before.z.p0 - 0.5);
  expect(errors).toEqual([]);
});

test("overlay: its own layer above the cell colours, with fill, outline and dots", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  type Hook = { layerIds: () => string[]; allLayerIds: () => string[]; viewerApi: { flyTo: (x: number, y: number, z: number) => void } };
  const all = () => page.evaluate(() => (window as unknown as { __xenium: Hook }).__xenium.allLayerIds().join(" "));
  const top = () => page.evaluate(() => (window as unknown as { __xenium: Hook }).__xenium.layerIds());
  await page.goto("/?section=A");
  await chooseSource(page, "Colour cells by", "fine_cell_type");
  await page.evaluate(() => (window as unknown as { __xenium: Hook }).__xenium.viewerApi.flyTo(300, 250, 1.6));
  await chooseSource(page, "Overlay", "lr:CXCL8_CXCR2:receiver");
  const legend = page.getByLabel("Overlay legend");
  await expect(legend).toContainText("CXCL8_CXCR2 (receiver)");
  await expect(legend).toContainText("Overlay · fill 60% · low values fade");
  await expect(legend).toContainText("base colour shows");
  // The base legend is still the cell types.
  await expect(page.locator(".legend-title").first()).toContainText("fine_cell_type");
  // A separate fill layer per tile, above the base fill.
  await expect.poll(all).toMatch(/ovfill-/);
  const ids = (await all()).split(" ");
  expect(ids.findIndex((i) => i.includes("ovfill-"))).toBeGreaterThan(ids.findIndex((i) => i.includes("fill-") && !i.includes("ovfill")));

  const overlaySection = page.locator("section", { has: page.getByRole("heading", { name: "Overlay" }) });
  await overlaySection.locator('input[type="range"]').first().fill("0.3");
  await expect(legend).toContainText("fill 30%");
  await page.getByLabel("Overlay outline").check();
  await overlaySection.locator('input[type="range"]').nth(1).fill("3");
  await expect(legend).toContainText("fill 30% · outline 3 px");
  await expect.poll(all).toMatch(/ovline-/);
  await page.getByLabel("Overlay fill").uncheck();
  await expect.poll(all).not.toMatch(/ovfill-/);
  await expect(legend).toContainText("Overlay · outline 3 px");

  // With centroids shown, the overlay adds a smaller dot per cell.
  await page.getByLabel("centroids").selectOption("on");
  await expect.poll(top).toContain("p0:overlay-centroids");

  await chooseSource(page, "Overlay", "");
  await expect(legend).toHaveCount(0);
  await expect.poll(all).not.toMatch(/ovline-|overlay-centroids/);
  expect(errors).toEqual([]);
});

test("UCell: create a signature, score, colour, compare niches, heatmap, export", async ({ page, request }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  page.on("dialog", (d) => d.accept());
  const layerIds = () => page.evaluate(() => (window as unknown as { __xenium: { layerIds: () => string[] } }).__xenium.layerIds());
  const name = `Myeloid ${Date.now()}`;
  await page.goto("/?section=A");
  const panel = page.getByLabel("Gene signatures");
  await panel.getByRole("button", { name: "New signature" }).click();
  await page.getByLabel("Signature name").fill(name);
  await page.getByLabel("Signature genes").fill("CD68 CXCL8 IL1B il7r-");
  const cov = page.getByLabel("Gene coverage");
  await expect(cov).toContainText("3/3 positive, 0/1 negative genes in the panel (30 genes)");
  await cov.getByRole("button", { name: "→ IL7R" }).click();
  await expect(page.getByLabel("Signature genes")).toHaveValue("CD68 CXCL8 IL1B IL7R-");
  await expect(cov).toContainText("1/1 negative");
  await page.getByRole("button", { name: "Create" }).click();
  await expect(panel.getByLabel(`select ${name}`)).toBeChecked();

  // Score at cell level; the result appears for this section.
  await panel.getByRole("button", { name: /^Score 1 signature/ }).click();
  const results = page.locator(".ucell-results li", { hasText: name });
  await expect(results.filter({ hasText: "cells" })).toHaveCount(1, { timeout: 20_000 });
  const row = results.filter({ hasText: "cells" });
  await row.getByRole("button", { name: "colour cells" }).click();
  await expect(page.locator(".legend-title").first()).toContainText(`UCell · ${name} (cell level, maxRank 30)`);

  // Per cell type / niche comparison.
  await row.getByRole("button", { name: "by niche / type" }).click();
  const sum = page.getByLabel("UCell by category");
  await sum.getByLabel("Compare by").selectOption("field:parent_cell_type");
  await expect.poll(() => sum.locator("tbody tr").count()).toBeGreaterThanOrEqual(5);
  await expect(sum).toContainText("join: cell field");

  // Heatmap of cell scores averaged per bin.
  await row.getByRole("button", { name: "heatmap" }).click();
  await expect.poll(layerIds).toContain("p0:ucell-heat");
  await expect(page.getByLabel("UCell heatmap legend")).toContainText("mean of cell-level UCell per 50 µm bin");

  // Parquet export of the picked result.
  await row.getByLabel(`pick ${name}`).check();
  const href = await page.getByRole("link", { name: /Export Parquet \(1\)/ }).getAttribute("href");
  expect(href).toContain("/ucell/export.parquet?keys=");
  const pq = await request.get(href!);
  expect(pq.status()).toBe(200);
  expect(pq.headers()["content-type"]).toBe("application/vnd.apache.parquet");

  // Bin-level scores are a separate result with their own heatmap.
  await panel.locator(".ucell-settings summary").click();
  await page.getByLabel("UCell level").selectOption("bin");
  await panel.getByRole("button", { name: /^Score 1 signature/ }).click();
  const binRow = results.filter({ hasText: "50 µm bins" });
  await expect(binRow).toHaveCount(1, { timeout: 20_000 });
  await binRow.getByRole("button", { name: "heatmap" }).click();
  await expect(page.getByLabel("UCell heatmap legend")).toContainText("pooled per 50 µm bin");

  // Delete the stored bin-level scores: the row and its heatmap disappear.
  await binRow.getByRole("button", { name: /^delete .* scores$/ }).click();
  await expect(binRow).toHaveCount(0);
  await expect(page.getByLabel("UCell heatmap legend")).toHaveCount(0);
  await expect(results.filter({ hasText: "cells" })).toHaveCount(1);

  // Delete the signature (stored scores remain listed for this index version).
  await panel.locator("li", { hasText: name }).getByRole("button", { name: "edit" }).click();
  await page.getByRole("button", { name: "delete", exact: true }).click();
  await expect(panel.getByLabel(`select ${name}`)).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("colour picker: groups start collapsed and open on demand or when filtering", async ({ page }) => {
  await page.goto("/?section=A");
  await page.getByRole("button", { name: "Colour cells by", exact: true }).click();
  const list = page.getByRole("listbox", { name: "Colour cells by", exact: true });
  // The group holding the current choice is open; the others are collapsed.
  await expect(list.getByRole("button", { name: /^Annotations/ })).toHaveAttribute("aria-expanded", "true");
  const lr = list.getByRole("button", { name: /LR scores/ });
  await expect(lr).toHaveAttribute("aria-expanded", "false");
  await expect(list.locator('[data-source="lr:CCL3_CCR5:sender"]')).toHaveCount(0);
  await lr.click();
  await expect(list.locator('[data-source="lr:CCL3_CCR5:sender"]')).toBeVisible();
  // Typing opens every group with matches and shows only those.
  await list.getByRole("searchbox").fill("sender");
  await expect(list.getByRole("option")).toHaveCount(3); // "none" + 2 senders
  await expect(list).toContainText(/2 of \d+ match/);
  await page.keyboard.press("Escape");
  await expect(list).toHaveCount(0);
});

test("theme: follows the system by default; Light and Dark switch the page and are remembered", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto("/?section=A");
  const bg = () => page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  const attr = () => page.evaluate(() => document.documentElement.dataset.theme ?? "system");
  await expect(page.getByLabel("Theme")).toHaveValue("system");
  const light = await bg();
  await page.emulateMedia({ colorScheme: "dark" });
  await expect.poll(bg).not.toBe(light); // System follows the OS
  const dark = await bg();
  await page.getByLabel("Theme").selectOption("light");
  await expect.poll(attr).toBe("light");
  await expect.poll(bg).toBe(light); // explicit Light wins over a dark OS
  await page.emulateMedia({ colorScheme: "light" });
  await page.getByLabel("Theme").selectOption("dark");
  await expect.poll(bg).toBe(dark); // explicit Dark wins over a light OS
  await page.reload();
  await expect(page.getByLabel("Theme")).toHaveValue("dark");
  await expect.poll(bg).toBe(dark);
  await page.getByLabel("Theme").selectOption("system");
  await expect.poll(attr).toBe("system");
  await expect.poll(bg).toBe(light);
});

test("right panel sections fold away and stay folded", async ({ page }) => {
  await page.goto("/?section=A");
  const head = page.getByRole("button", { name: /^Legend/ });
  await expect(head).toHaveAttribute("aria-expanded", "true");
  await expect(page.locator(".legend-title").first()).toBeVisible();
  await head.click();
  await expect(head).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator(".legend-title")).toHaveCount(0);
  await page.reload();
  await expect(page.getByRole("button", { name: /^Legend/ })).toHaveAttribute("aria-expanded", "false");
  await page.getByRole("button", { name: /^Legend/ }).click();
  await expect(page.locator(".legend-title").first()).toBeVisible();
});

test("index panel shows only the configured sample information", async ({ page }) => {
  await page.goto("/?section=A");
  const info = page.getByLabel("Sample information");
  await expect(info).toContainText("patient");
  await expect(info).toContainText("P-SYN-1");
  await expect(info).toContainText("Age");
  await expect(page.locator("body")).not.toContainText("do-not-show");
  await page.getByLabel("Section", { exact: true }).selectOption("B");
  await expect(page.getByLabel("Sample information")).toHaveCount(0); // none configured for B
});

test("lasso selects the cells inside the outline; Shift adds, Alt removes, Esc stops", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.goto("/?section=A");
  await expect(page.locator(".legend-title").first()).toBeVisible();
  const box = (await page.locator("canvas").boundingBox())!;
  const count = async () =>
    Number(((await page.getByRole("button", { name: /^Selection \(/ }).innerText()).match(/\(([\d,]+)\)/) ?? ["", "0"])[1].replace(/,/g, ""));
  const lassoAt = async (cx: number, cy: number, r: number, mod?: "Shift" | "Alt") => {
    if (mod) await page.keyboard.down(mod);
    await page.mouse.move(cx + r, cy);
    await page.mouse.down();
    for (let a = 0; a <= 2 * Math.PI + 0.01; a += Math.PI / 12) await page.mouse.move(cx + r * Math.cos(a), cy + r * Math.sin(a));
    await page.mouse.up();
    if (mod) await page.keyboard.up(mod);
  };
  const lasso = page.getByRole("button", { name: "Lasso" });
  await lasso.click();
  await expect(lasso).toHaveAttribute("aria-pressed", "true");
  const z0 = await page.evaluate(() => (window as unknown as { __xenium: { targets: () => Record<string, number[]> } }).__xenium.targets().p0);
  const cx = box.x + box.width * 0.35, cy = box.y + box.height * 0.5;
  await lassoAt(cx, cy, 70);
  await expect.poll(count).toBeGreaterThan(0);
  const first = await count();
  // The drag drew a lasso instead of panning the view.
  const z1 = await page.evaluate(() => (window as unknown as { __xenium: { targets: () => Record<string, number[]> } }).__xenium.targets().p0);
  expect(z1).toEqual(z0);
  await lassoAt(cx + 220, cy, 70, "Shift");
  await expect.poll(count).toBeGreaterThan(first);
  const both = await count();
  await lassoAt(cx, cy, 70, "Alt");
  await expect.poll(count).toBeLessThan(both);
  await page.keyboard.press("Escape");
  await expect(lasso).toHaveAttribute("aria-pressed", "false");
  await expect(page.getByRole("button", { name: "Export CSV" })).toBeEnabled();
  expect(errors).toEqual([]);
});

test("morphology: channels listed, switched on with colour, brightness and opacity", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  const all = () => page.evaluate(() => (window as unknown as { __xenium: { allLayerIds: () => string[] } }).__xenium.allLayerIds().join(" "));
  await page.goto("/?section=A");
  // Header logo; no "Sign out" on a server without sign-in.
  await expect(page.getByRole("img", { name: "NOVIUM" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Sign out" })).toHaveCount(0);
  const sec = page.getByLabel("Morphology", { exact: true });
  for (const n of ["DAPI", "ATP1A1/CD45/E-Cadherin", "18S", "alphaSMA/Vimentin"]) await expect(sec.getByLabel(`Show ${n}`)).toBeVisible();
  // Off by default: no layer, no tile requests.
  expect(await all()).not.toMatch(/morph-A/);
  const tiles = page.waitForResponse((r) => r.url().includes("/images/morph-dapi/") && r.ok());
  await sec.getByLabel("Show DAPI").check();
  await tiles;
  await expect.poll(all).toMatch(/p0:morph-A@/);
  const dapi = sec.getByLabel("Channel DAPI");
  await dapi.locator('input[type="range"]').first().fill("2");
  await dapi.locator('input[type="range"]').nth(1).fill("0.5");
  await sec.getByLabel("DAPI colour").fill("#ff0000");
  const legend = page.getByLabel("Morphology legend");
  await expect(legend).toContainText("DAPI");
  await expect(legend).toContainText("2.00× · 50%");
  // Master opacity: one opacity for the whole mixed layer, shown in the legend.
  const opacity = () =>
    page.evaluate(() => (window as unknown as { __xenium: { layerOpacity: (p: string) => number | undefined } }).__xenium.layerOpacity(":morph-A@"));
  expect(await opacity()).toBe(1);
  await sec.getByLabel("Master opacity").fill("0.4");
  await expect.poll(opacity).toBeCloseTo(0.4);
  await expect(legend).toContainText("opacity 40%");
  // A second channel joins the same composite layer.
  await sec.getByLabel("Show 18S").check();
  await expect(legend).toContainText("18S");
  await sec.getByLabel("Show DAPI").uncheck();
  await sec.getByLabel("Show 18S").uncheck();
  await expect.poll(all).not.toMatch(/p0:morph-A@/);
  await expect(legend).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("legend: colour schemes, shuffle, a colour picked per type (remembered), cells without a value", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  type Hook = { visibleFill: (part: string) => [string, number, number][] };
  const shown = () =>
    page.evaluate(() => (window as unknown as { __xenium: Hook }).__xenium.visibleFill("fill-").reduce((a, [id, on]) => (id.startsWith("ov") ? a : a + on), 0));
  await page.goto("/?section=A");
  await chooseSource(page, "Colour cells by", "fine_cell_type");
  const legend = page.locator(".legend").first();
  const swatches = legend.locator("input.sw-pick");
  await expect(swatches.first()).toBeVisible();
  const colours = () => swatches.evaluateAll((els) => els.map((e) => (e as HTMLInputElement).value));
  const original = await colours();

  // Another scheme, then a shuffle: the same colours in another order.
  await legend.getByLabel("Colour scheme").selectOption("okabe");
  await expect.poll(colours).toContain("#e69f00");
  const okabe = await colours();
  await legend.getByRole("button", { name: "shuffle" }).click();
  await expect.poll(colours).not.toEqual(okabe);
  expect([...(await colours())].sort()).toEqual([...okabe].sort());

  // One colour picked by hand; remembered in this browser across a reload.
  await swatches.first().fill("#123456");
  await expect(legend).toContainText("1 picked by hand");
  await page.reload();
  await expect(page.locator(".legend").first().locator("input.sw-pick").first()).toHaveValue("#123456");
  await page.locator(".legend").first().getByRole("button", { name: "reset colours" }).click();
  await expect.poll(colours).toEqual(original);

  // Cells without a value: grey by default, or not drawn at all.
  await chooseSource(page, "Colour cells by", "lr:CXCL8_CXCR2:receiver");
  const toggle = page.locator(".legend").first().getByLabel(/show the [\d,]+ cells without a value/);
  await expect(toggle).toBeChecked();
  await expect.poll(shown).toBeGreaterThan(0);
  const withGrey = await shown();
  await toggle.uncheck();
  await expect.poll(shown).toBeLessThan(withGrey);
  await toggle.check();
  await expect.poll(shown).toBe(withGrey);
  // A numeric legend offers the colour maps.
  await page.locator(".legend").first().getByLabel("Colour scheme").selectOption("plasma");
  await expect(page.locator(".legend .ramp").first()).toHaveAttribute("style", /13, 8, 135|#0d0887/);

  // The overlay legend has the same colour controls for a categorical overlay.
  await chooseSource(page, "Overlay", "fine_cell_type");
  const ol = page.getByLabel("Overlay legend");
  await expect(ol.getByLabel("Colour scheme")).toBeVisible();
  await expect(ol.locator("input.sw-pick").first()).toBeVisible();
  expect(errors).toEqual([]);
});

test("morphology: one switch shows or hides every channel", async ({ page }) => {
  await page.goto("/?section=A");
  const sec = page.getByLabel("Morphology", { exact: true });
  const all = sec.getByLabel("Show all channels");
  await all.check();
  for (const n of ["DAPI", "ATP1A1/CD45/E-Cadherin", "18S", "alphaSMA/Vimentin"]) await expect(sec.getByLabel(`Show ${n}`)).toBeChecked();
  await sec.getByLabel("Show 18S").uncheck();
  await expect(all).not.toBeChecked();
  expect(await all.evaluate((e) => (e as HTMLInputElement).indeterminate)).toBe(true);
  // Partly on: a click turns every channel on, the next one every channel off.
  await all.click();
  await expect(sec.getByLabel("Show 18S")).toBeChecked();
  await all.click();
  for (const n of ["DAPI", "ATP1A1/CD45/E-Cadherin", "18S", "alphaSMA/Vimentin"]) await expect(sec.getByLabel(`Show ${n}`)).not.toBeChecked();
});

test("cells can be clicked with fill and outline off; the legend folds; side panels hide", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  type Hook = { allLayerIds: () => string[] };
  const all = () => page.evaluate(() => (window as unknown as { __xenium: Hook }).__xenium.allLayerIds().join(" "));
  await page.goto("/?section=A");
  await expect(page.locator(".legend-title").first()).toBeVisible();
  await page.getByLabel("fill", { exact: true }).uncheck();
  await page.getByLabel("outline", { exact: true }).uncheck();
  const legendHead = page.getByRole("button", { name: /^Legend/ });
  await expect(legendHead).toHaveAttribute("aria-expanded", "true");
  const box = (await page.locator("canvas").boundingBox())!;
  let picked = false;
  for (const [dx, dy] of [[0, 0], [6, 0], [0, 6], [-6, 0], [0, -6], [10, 10]]) {
    const resp = page.waitForResponse((r) => r.url().includes("/cells/key/"), { timeout: 3000 }).catch(() => null);
    await page.mouse.click(box.x + box.width / 2 + dx, box.y + box.height / 2 + dy);
    if (await resp) {
      picked = true;
      break;
    }
  }
  expect(picked).toBe(true);
  await expect(page.locator(".inspector code")).toHaveText(/^[a-p]{8}-1$/);
  // Only the selected cell is outlined, and the legend folded to make room.
  await expect.poll(all).toMatch(/\bsel-/);
  await expect(legendHead).toHaveAttribute("aria-expanded", "false");
  await expect(page.getByRole("button", { name: /^Selected cell/ })).toHaveAttribute("aria-expanded", "true");
  // Switched off, a click leaves the legend open.
  await page.getByLabel("fold the legend when a cell is clicked").uncheck();
  await legendHead.click();
  await page.mouse.click(box.x + box.width / 2 + 12, box.y + box.height / 2 + 12);
  await page.waitForTimeout(500);
  await expect(legendHead).toHaveAttribute("aria-expanded", "true");

  // Side panels: [ and ] or the header buttons; the canvas takes the room.
  const w0 = (await page.locator("canvas").boundingBox())!.width;
  await page.keyboard.press("[");
  await expect(page.locator(".panel.left")).toBeHidden();
  await page.getByRole("button", { name: "Right panel" }).click();
  await expect(page.locator(".panel.right")).toBeHidden();
  await expect.poll(async () => (await page.locator("canvas").boundingBox())!.width).toBeGreaterThan(w0 + 500);
  await page.keyboard.press("]");
  await page.getByRole("button", { name: "Left panel" }).click();
  await expect(page.locator(".panel.left")).toBeVisible();
  await expect(page.locator(".panel.right")).toBeVisible();
  expect(errors).toEqual([]);
});

test("export view: the panel windows as PNG, TIFF or PDF at a chosen resolution, the legend as SVG", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  const fs = await import("node:fs");
  const files: { name: string; bytes: Buffer }[] = [];
  page.on("download", async (d) => files.push({ name: d.suggestedFilename(), bytes: fs.readFileSync((await d.path())!) }));
  await page.goto("/?section=A");
  await chooseSource(page, "Colour cells by", "fine_cell_type");
  const cssW = await page.locator("canvas").evaluate((c) => (c as HTMLCanvasElement).clientWidth);
  const dpr = await page.evaluate(() => window.devicePixelRatio || 1);

  await page.getByRole("button", { name: "Export", exact: true }).click();
  const dlg = page.getByRole("dialog", { name: "Export view" });
  await dlg.getByLabel("Format").selectOption("png");
  await dlg.getByLabel("Resolution").selectOption("2");
  await dlg.getByLabel("DPI").selectOption("300");
  await dlg.getByLabel("Legend file").selectOption("svg");
  await expect(dlg.getByLabel("Output size")).toContainText(`${(Math.round(cssW * dpr * 2)).toLocaleString("en-US")} ×`);
  await dlg.getByRole("button", { name: "Export", exact: true }).click();
  await expect(dlg.getByRole("status")).toContainText("Saved", { timeout: 60_000 });
  await expect.poll(() => files.length).toBe(2);
  const png = files.find((f) => f.name.endsWith(".png"))!;
  expect(png.name).toMatch(/^A_view_\d+x\d+\.png$/);
  expect(png.bytes.readUInt32BE(16)).toBe(Math.round(cssW * dpr * 2)); // IHDR width
  const phys = png.bytes.indexOf("pHYs");
  expect(phys).toBeGreaterThan(0);
  expect(png.bytes.readUInt32BE(phys + 4)).toBe(11811); // 300 dpi in pixels per metre
  const svg = files.find((f) => f.name.endsWith("_legend.svg"))!.bytes.toString("utf8");
  expect(svg).toMatch(/^<svg xmlns=/);
  expect(svg).toContain("fine_cell_type");

  await dlg.getByLabel("Format").selectOption("tiff");
  await dlg.getByLabel("Resolution").selectOption("1");
  await dlg.getByLabel("Legend file").selectOption("none");
  await dlg.getByRole("button", { name: "Export", exact: true }).click();
  await expect.poll(() => files.length, { timeout: 60_000 }).toBe(3);
  const tif = files[2].bytes;
  expect(tif.subarray(0, 4).toString("latin1")).toBe("II*\u0000");

  await dlg.getByLabel("Format").selectOption("pdf");
  await dlg.getByRole("button", { name: "Export", exact: true }).click();
  await expect.poll(() => files.length, { timeout: 60_000 }).toBe(4);
  expect(files[3].bytes.subarray(0, 8).toString("latin1")).toBe("%PDF-1.4");
  await dlg.getByRole("button", { name: "Close" }).click();
  await expect(dlg).toHaveCount(0);

  // The legend alone, from the legend section.
  await page.getByRole("button", { name: "save legend as SVG" }).click();
  await expect.poll(() => files.length).toBe(5);
  expect(files[4].name).toBe("A_legend.svg");
  expect(errors).toEqual([]);
});
