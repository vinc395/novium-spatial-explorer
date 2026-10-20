// "Export view": the panel windows only (no side panels) as an image file at a chosen
// resolution, and their legend as a separate file.

import { activePanel, getState, morphSettings } from "../store";
import { viewerApi, type Capture } from "../viewer/Viewer";
import { encodePdf, encodeTiff, jpegWithDpi, pngWithDpi } from "./encode";
import { legendSvg, type LegendPart } from "./legendSvg";

export type ExportFormat = "png" | "jpeg" | "tiff" | "pdf";

export interface ExportOptions {
  format: ExportFormat;
  /** × the screen's pixels: 1 (as shown) up to 4 */
  scale: number;
  /** written into the file: sets its printed size */
  dpi: number;
  background: "theme" | "white" | "black" | "transparent";
  scaleBar: boolean;
  /** section ID in each panel's corner */
  labels: boolean;
  panels: "all" | "active";
  legend: "svg" | "png" | "none";
}

export const DEFAULT_EXPORT: ExportOptions = {
  format: "png",
  scale: 2,
  dpi: 300,
  background: "theme",
  scaleBar: true,
  labels: false,
  panels: "all",
  legend: "svg",
};

export const FORMATS: Record<ExportFormat, { label: string; ext: string; alpha: boolean }> = {
  png: { label: "PNG (lossless)", ext: "png", alpha: true },
  jpeg: { label: "JPEG (small file)", ext: "jpg", alpha: false },
  tiff: { label: "TIFF (lossless, for journals)", ext: "tif", alpha: true },
  pdf: { label: "PDF (lossless image on a page)", ext: "pdf", alpha: true },
};

const STORE_KEY = "novium.export.v1";

export function loadExportOptions(): ExportOptions {
  try {
    const v = JSON.parse(localStorage.getItem(STORE_KEY) ?? "{}") as Partial<ExportOptions>;
    const o = { ...DEFAULT_EXPORT, ...v };
    return o.format in FORMATS ? o : DEFAULT_EXPORT;
  } catch {
    return DEFAULT_EXPORT;
  }
}

export function saveExportOptions(o: ExportOptions) {
  try {
    localStorage.setItem(STORE_KEY, JSON.stringify(o));
  } catch {
    /* not remembered */
  }
}

/** The legends of the active panel, as shown in the right panel (hidden categories left out). */
export function legendParts(): LegendPart[] {
  const s = getState();
  const p = activePanel(s);
  const parts: LegendPart[] = [];
  if (p.legend) parts.push({ legend: p.legend, hidden: s.colorBy.hidden });
  if (s.overlay.source && p.overlayLegend) parts.push({ heading: "Overlay", legend: p.overlayLegend, hidden: s.overlay.hidden });
  const chans = (p.manifest?.images ?? [])
    .filter((i) => i.channel)
    .map((i) => ({ name: i.channel!.name, set: morphSettings(s.morph, i) }))
    .filter((c) => c.set.on)
    .map((c) => ({ name: c.name, color: c.set.color }));
  if (chans.length) parts.push({ heading: "Morphology", channels: chans });
  if (s.ucell.heat.key && s.ucell.heat.visible && p.ucellImage) parts.push({ heading: "UCell heatmap", legend: p.ucellImage.legend });
  if (s.grid.visible && p.gridImage) parts.push({ heading: "Grid", legend: p.gridImage.legend, hidden: s.grid.hidden });
  return parts;
}

function download(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

const toBlob = (c: HTMLCanvasElement, type: string, quality?: number) =>
  new Promise<Blob>((ok, fail) => c.toBlob((b) => (b ? ok(b) : fail(new Error(`the browser could not encode ${type}`))), type, quality));

/** A scale bar of a round length, about 12% of the panel width, bottom left. */
function drawScaleBar(g: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, umPerPx: number) {
  if (!Number.isFinite(umPerPx) || umPerPx <= 0) return;
  const raw = umPerPx * w * 0.12;
  const p = 10 ** Math.floor(Math.log10(raw));
  const um = ([1, 2, 5, 10].find((k) => k * p >= raw) ?? 10) * p;
  const len = um / umPerPx;
  const unit = Math.max(1, Math.round(Math.min(w, h) / 400));
  const m = 14 * unit;
  const bx = x + m, by = y + h - m;
  const label = um >= 1000 ? `${um / 1000} mm` : `${um} µm`;
  g.save();
  g.fillStyle = "#000";
  g.fillRect(bx - unit, by - 4 * unit - unit, len + 2 * unit, 4 * unit + 2 * unit);
  g.fillStyle = "#fff";
  g.fillRect(bx, by - 4 * unit, len, 4 * unit);
  g.font = `bold ${12 * unit}px Arial, Helvetica, sans-serif`;
  g.lineWidth = 3 * unit;
  g.strokeStyle = "#000";
  g.strokeText(label, bx, by - 8 * unit);
  g.fillText(label, bx, by - 8 * unit);
  g.restore();
}

function drawLabel(g: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, text: string) {
  const unit = Math.max(1, Math.round(Math.min(w, h) / 400));
  g.save();
  g.font = `bold ${13 * unit}px Arial, Helvetica, sans-serif`;
  g.lineWidth = 3 * unit;
  g.strokeStyle = "#000";
  g.fillStyle = "#fff";
  g.strokeText(text, x + 12 * unit, y + 22 * unit);
  g.fillText(text, x + 12 * unit, y + 22 * unit);
  g.restore();
}

function themeBackground(): string {
  const el = document.querySelector("main");
  const c = el ? getComputedStyle(el).backgroundColor : "";
  return c && c !== "rgba(0, 0, 0, 0)" ? c : "#ffffff";
}

/** The composed image: crop, background, scale bars, labels. */
export function compose(cap: Capture, o: ExportOptions, activeSlot: number): HTMLCanvasElement {
  const panels = o.panels === "active" ? cap.panels.filter((p) => p.slot === activeSlot) : cap.panels;
  const x0 = Math.min(...panels.map((p) => p.rect[0])), y0 = Math.min(...panels.map((p) => p.rect[1]));
  const x1 = Math.max(...panels.map((p) => p.rect[0] + p.rect[2])), y1 = Math.max(...panels.map((p) => p.rect[1] + p.rect[3]));
  const out = document.createElement("canvas");
  out.width = x1 - x0;
  out.height = y1 - y0;
  const g = out.getContext("2d")!;
  const transparent = o.background === "transparent" && FORMATS[o.format].alpha;
  if (!transparent) {
    g.fillStyle = o.background === "white" ? "#ffffff" : o.background === "black" ? "#000000" : themeBackground();
    g.fillRect(0, 0, out.width, out.height);
  }
  g.drawImage(cap.canvas, x0, y0, out.width, out.height, 0, 0, out.width, out.height);
  for (const p of panels) {
    const [x, y, w, h] = [p.rect[0] - x0, p.rect[1] - y0, p.rect[2], p.rect[3]];
    if (o.scaleBar) drawScaleBar(g, x, y, w, h, p.umPerPx);
    if (o.labels && p.sectionId) drawLabel(g, x, y, w, h, p.sectionId);
  }
  return out;
}

export async function encodeImage(c: HTMLCanvasElement, o: ExportOptions): Promise<Blob> {
  const alpha = o.background === "transparent";
  if (o.format === "png") return new Blob([pngWithDpi(new Uint8Array(await (await toBlob(c, "image/png")).arrayBuffer()), o.dpi) as BlobPart], { type: "image/png" });
  if (o.format === "jpeg") return new Blob([jpegWithDpi(new Uint8Array(await (await toBlob(c, "image/jpeg", 0.92)).arrayBuffer()), o.dpi) as BlobPart], { type: "image/jpeg" });
  const img = c.getContext("2d")!.getImageData(0, 0, c.width, c.height);
  if (o.format === "tiff") return new Blob([(await encodeTiff(img, o.dpi, alpha)) as BlobPart], { type: "image/tiff" });
  return new Blob([(await encodePdf(img, o.dpi, alpha)) as BlobPart], { type: "application/pdf" });
}

/** The legend as SVG (vector) or as a PNG rendered from it at `scale`. */
export async function legendFile(kind: "svg" | "png", scale = 2): Promise<Blob | null> {
  const parts = legendParts();
  if (!parts.length) return null;
  const svg = legendSvg(parts);
  if (kind === "svg") return new Blob([svg], { type: "image/svg+xml" });
  const url = URL.createObjectURL(new Blob([svg], { type: "image/svg+xml" }));
  try {
    const img = new Image();
    img.src = url;
    await img.decode();
    const c = document.createElement("canvas");
    c.width = Math.ceil(img.width * scale);
    c.height = Math.ceil(img.height * scale);
    const g = c.getContext("2d")!;
    g.scale(scale, scale);
    g.drawImage(img, 0, 0);
    return await toBlob(c, "image/png");
  } finally {
    URL.revokeObjectURL(url);
  }
}

function baseName(o: ExportOptions): string {
  const s = getState();
  const ids = (o.panels === "active" ? [activePanel(s)] : s.panels).map((p) => p.sectionId ?? "view");
  return ids.join("+");
}

export function exportLegendOnly() {
  return legendFile("svg").then((b) => {
    if (!b) throw new Error("no legend to export");
    download(b, `${baseName({ ...DEFAULT_EXPORT, panels: "active" })}_legend.svg`);
  });
}

/** Render, compose and save the view (and its legend): the file names written. */
export async function exportView(o: ExportOptions, onProgress: (msg: string) => void): Promise<string[]> {
  if (!viewerApi.capture) throw new Error("the viewer is not ready");
  onProgress("Rendering…");
  const cap = await viewerApi.capture(o.scale, onProgress);
  const img = compose(cap, o, activePanel().slot);
  onProgress(`Writing ${FORMATS[o.format].ext.toUpperCase()} (${img.width} × ${img.height} px)…`);
  const blob = await encodeImage(img, o);
  const base = baseName(o);
  const names = [`${base}_view_${img.width}x${img.height}.${FORMATS[o.format].ext}`];
  download(blob, names[0]);
  if (o.legend !== "none") {
    const lb = await legendFile(o.legend, cap.ratio);
    if (lb) {
      names.push(`${base}_legend.${o.legend}`);
      download(lb, names[1]);
    }
  }
  return names;
}
