// Section-scoped data loading and derived arrays (per-cell colours, grid images),
// one SectionData per panel slot. Every async result is checked against the slot's
// current section key before it is stored, so nothing leaks across panels/sections.

import { ApiError, getBuffer, listSections, SectionApi, ucellApi } from "./api";
import { decodeCentroids, decodeField, decodeGeneTiles, decodeGridRaster, type FieldColumn, type GridRaster } from "./codecs";
import { colormapLut, MISSING, type RGBA } from "./colors";
import { labelSmoother, SMOOTH_SIGMAS, smoothFactor, valueSmoother } from "./gridSmooth";
import { categoryPalette, type PaletteChoice } from "./palettes";
import { geneIdsByName, getState, panelBySlot, resetPanel, setPanel, setState, setUcell, ucellResultByKey, type ColorBy, type Overlay } from "./store";
import type { BinGrid, FieldInfo, Manifest, UcellJob, UcellResult } from "./types";

export interface SectionData {
  key: string;
  centroids?: Float32Array;
  fields: Map<string, Promise<FieldColumn>>;
  geneTiles: Map<number, Promise<Map<number, number>>>;
  /** Decoded grid rasters by `grid/field` (toggling categories must not refetch). */
  rasters: Map<string, Promise<GridRaster>>;
  /** The last smoothed grid raster (recolouring reuses it). */
  smoothed?: { key: string; out: Promise<ArrayLike<number> | null> };
}

const bySlot = new Map<number, SectionData>();
const keyOf = (slot: number) => bySlot.get(slot)?.key;

export async function loadSections() {
  try {
    const sections = await listSections();
    setState({ sections, sectionsError: undefined });
    return sections;
  } catch (e) {
    setState({ sectionsError: String(e) });
    return [];
  }
}

/** Load `sectionId` into panel `slot` (replacing whatever it showed). */
export async function openSection(slot: number, sectionId: string) {
  const summary = getState().sections.find((s) => s.section_id === sectionId);
  if (!summary?.version) {
    setPanel(slot, { loadError: summary?.error ?? `section ${sectionId} not available` });
    return;
  }
  const api = new SectionApi(sectionId, summary.version);
  const key = `${slot}|${sectionId}@${summary.version}`;
  const old = panelBySlot(slot);
  old?.gridImage?.bitmap.close();
  old?.ucellImage?.bitmap.close();
  bySlot.set(slot, { key, fields: new Map(), geneTiles: new Map(), rasters: new Map() });
  // Until the new manifest arrives the panel draws nothing (not the old section's
  // image size or cells under the new section's tile URLs).
  resetPanel(slot, { sectionId, api });
  try {
    const manifest = await api.manifest();
    if (keyOf(slot) !== key) return;
    setPanel(slot, { manifest });
    // First section loaded: pick sensible shared defaults.
    const s = getState();
    if (!s.colorBy.source) {
      const first = defaultColorField(manifest.cells?.fields ?? []);
      if (first) setState({ colorBy: { ...s.colorBy, source: first.id, range: null, hidden: new Set() } });
    }
    if (!getState().grid.gridId && manifest.grids[0]) {
      const g = manifest.grids[0];
      setState((st) => ({ grid: { ...st.grid, gridId: g.id, fieldId: g.fields[0]?.id, visible: false, hidden: new Set() } }));
    }
    const cbuf = await getBuffer(api.centroidsUrl());
    if (keyOf(slot) !== key) return;
    bySlot.get(slot)!.centroids = decodeCentroids(cbuf);
    setPanel(slot, { centroids: bySlot.get(slot)!.centroids });
    for (const g of getState().genes) loadGeneTileCounts(g.name, slot);
    loadUcellResults(sectionId);
    // The new section can extend the shared category palettes: recolour all panels.
    await Promise.all([recomputeColors(), buildGridImage()]);
  } catch (e) {
    if (keyOf(slot) === key) setPanel(slot, { loadError: String(e) });
  }
}

export function closeSlot(slot: number) {
  panelBySlot(slot)?.gridImage?.bitmap.close();
  panelBySlot(slot)?.ucellImage?.bitmap.close();
  bySlot.delete(slot);
  colorMemo.delete(slot);
}

function fieldColumn(slot: number, id: string): Promise<FieldColumn> {
  const d = bySlot.get(slot)!;
  const p0 = panelBySlot(slot)!;
  let p = d.fields.get(id);
  if (!p) {
    let url: string;
    if (id.startsWith("expr:")) {
      const gid = geneIdsByName(p0.manifest!).get(id.slice(5));
      if (gid === undefined) return Promise.reject(new Error(`gene ${id.slice(5)} not in this section`));
      url = p0.api!.expressionUrl(gid);
    } else if (id.startsWith("ucell:")) {
      if (!getState().ucell.results[p0.sectionId!]?.some((r) => r.key === id.slice(6)))
        return Promise.reject(new Error("not scored for this section yet"));
      url = p0.api!.ucellValuesUrl(id.slice(6));
    } else url = p0.api!.fieldUrl(id);
    p = getBuffer(url).then(decodeField);
    p.catch(() => d.fields.delete(id));
    d.fields.set(id, p);
  }
  return p;
}

/** Fetch a gene's per-tile counts (drives the points/density LOD) for one panel or all panels. */
export function loadGeneTileCounts(name: string, onlySlot?: number) {
  for (const p of getState().panels) {
    if (onlySlot !== undefined && p.slot !== onlySlot) continue;
    const d = bySlot.get(p.slot);
    const gid = p.manifest ? geneIdsByName(p.manifest).get(name) : undefined;
    if (!d || !p.api || gid === undefined) continue;
    const key = d.key;
    let pr = d.geneTiles.get(gid);
    if (!pr) {
      pr = getBuffer(p.api.geneTilesUrl(gid)).then((b) => decodeGeneTiles(b).counts);
      pr.catch(() => d.geneTiles.delete(gid)); // allow a retry after a failure
      d.geneTiles.set(gid, pr);
    }
    pr.then((counts) => {
      if (keyOf(p.slot) !== key) return;
      setPanel(p.slot, (cur) => {
        const map = new Map(cur.geneTileCounts);
        map.set(gid, counts);
        return { geneTileCounts: map };
      });
    }).catch((e) => {
      if (keyOf(p.slot) === key) setState({ notice: `gene tile counts (${p.sectionId}): ${e}` });
    });
  }
}

export interface Legend {
  kind: "none" | "categorical" | "numeric";
  label: string;
  field?: FieldInfo;
  categories?: { code: number; name: string; count: number; color: RGBA }[];
  range?: [number, number];
  colormap?: string;
  log?: boolean;
  nMissing?: number;
  note?: string;
}

export function ucellLabel(r: UcellResult | undefined, key: string): string {
  if (!r) return `UCell result ${key}`;
  const where = r.level === "cell" ? "cell level" : `${r.bin_um} µm bins, pooled transcripts`;
  return `UCell · ${r.signature.name} (${where}, maxRank ${r.coverage.max_rank_effective})`;
}

export function sourceInfo(
  m: Manifest,
  source: string,
): { label: string; field?: FieldInfo; expr?: string; ucell?: string } {
  if (source.startsWith("expr:")) {
    return { label: `${source.slice(5)} expression (transcripts, QV≥20)`, expr: source.slice(5) };
  }
  if (source.startsWith("ucell:")) {
    const key = source.slice(6);
    return { label: ucellLabel(ucellResultByKey(key, m.section_id), key), ucell: key };
  }
  const f = m.cells?.fields.find((x) => x.id === source);
  return { label: f?.label ?? "", field: f };
}

function quantile(sorted: Float32Array, q: number) {
  return sorted[Math.min(sorted.length - 1, Math.max(0, Math.round((sorted.length - 1) * q)))];
}

/** What cells are coloured by when the first section opens: the major cell type when there
 *  is one, otherwise a fine cell type, otherwise the first categorical annotation. */
export function defaultColorField(fields: FieldInfo[]): FieldInfo | undefined {
  const cat = (id: string) => fields.find((f) => f.id === id && f.kind === "categorical");
  return cat("major_cell_type") ?? cat("fine_cell_type") ?? fields.find((f) => f.kind === "categorical" && f.group !== "cells");
}

/** One palette per field across all open panels: categories are coloured by their
 *  index in the sorted union of names, so a name has the same colour in every panel
 *  (in the field's chosen scheme and order, with any colours picked by hand). */
function unionPalette(lists: (string[] | undefined)[], choice?: PaletteChoice): Map<string, RGBA> {
  const names = [...new Set(lists.flatMap((l) => l ?? []))].sort();
  return categoryPalette(names, choice);
}

function fieldPalette(fieldId: string): Map<string, RGBA> {
  return unionPalette(
    getState().panels.map((p) => p.manifest?.cells?.fields.find((f) => f.id === fieldId)?.categories?.map((c) => c.name)),
    getState().palettes[fieldId],
  );
}

/** The colour choices of a grid field, kept apart from cell fields of the same name. */
export const gridPaletteKey = (gridId: string, fieldId: string) => `grid:${gridId}/${fieldId}`;

function gridPalette(gridId: string, fieldId: string): Map<string, RGBA> {
  return unionPalette(
    getState().panels.map((p) =>
      p.manifest?.grids.find((g) => g.id === gridId)?.fields.find((f) => f.id === fieldId)?.categories?.map((c) => c.name),
    ),
    getState().palettes[gridPaletteKey(gridId, fieldId)],
  );
}

const colorJobs = new Map<number, number>();

interface Colourized {
  key: string;
  colors: Uint8Array;
  legend: Legend;
  /** false when the source could not be loaded (retried on the next recolour) */
  ok?: boolean;
}
/** Last base / overlay colouring per slot, so an opacity change only re-blends. */
const colorMemo = new Map<number, { base?: Colourized; overlay?: Colourized }>();

/** Rebuild per-cell RGBA arrays from the shared colour-by settings (one panel or all). */
export async function recomputeColors(onlySlot?: number) {
  await Promise.resolve(); // never setState synchronously from inside a store listener
  const slots = getState().panels.map((p) => p.slot).filter((s) => onlySlot === undefined || s === onlySlot);
  await Promise.all(slots.map((s) => recomputeSlot(s)));
}

/**
 * Colour every cell of a slot by one source. The base colouring shows missing values
 * in grey; an overlay leaves them transparent (alpha 0) so the base shows through,
 * and with `fadeLow` a numeric overlay's alpha follows the scaled value.
 */
function colorKey(slot: number, cb: ColorBy, overlay: { fadeLow: boolean } | null): string {
  // Category palettes are unions over the open sections, so they are part of the key.
  const sections = getState().panels.map((x) => x.manifest?.section_id ?? "").join(",");
  const palette = getState().palettes[cb.source] ?? null;
  return JSON.stringify([keyOf(slot), sections, cb.source, cb.colormap, cb.range, cb.log, [...cb.hidden].sort(), overlay?.fadeLow ?? null, palette, overlay ? null : cb.hideMissing]);
}

async function colorize(slot: number, cb: ColorBy, overlay: { fadeLow: boolean } | null): Promise<Colourized | null> {
  const p = panelBySlot(slot)!;
  const d = bySlot.get(slot)!;
  const m = p.manifest!;
  const n = d.centroids!.length / 2;
  const key = colorKey(slot, cb, overlay);
  const colors = new Uint8Array(n * 4);
  const info = sourceInfo(m, cb.source);
  const missing: RGBA = overlay || cb.hideMissing ? [0, 0, 0, 0] : MISSING;
  const single = () => {
    if (!overlay) for (let i = 0; i < n; i++) colors.set([90, 160, 230, 255], i * 4);
  };
  if (!cb.source) {
    single();
    return { key, colors, legend: { kind: "none", label: "Single colour" } };
  }
  if (!info.field && info.expr === undefined && info.ucell === undefined) {
    single();
    return { key, colors, legend: { kind: "none", label: `“${cb.source}” is not available in ${p.sectionId}` } };
  }
  let col: FieldColumn;
  try {
    col = await fieldColumn(slot, cb.source);
  } catch (e) {
    if (keyOf(slot) !== d.key) return null;
    single();
    return { key, colors, ok: false, legend: { kind: "none", label: `${info.label}: not available in ${p.sectionId} (${e instanceof Error ? e.message : e})` } };
  }
  if (keyOf(slot) !== d.key) return null;
  if (col.kind === "categorical") {
    const cats = info.field?.categories ?? [];
    const byName = fieldPalette(cb.source);
    const palette = cats.map((c) => byName.get(c.name) ?? MISSING);
    const hiddenCode = cats.map((c) => cb.hidden.has(c.name));
    let nMissing = 0;
    for (let i = 0; i < n; i++) {
      const c = col.codes[i];
      if (c === 0) nMissing++;
      const rgba = c === 0 ? missing : hiddenCode[c - 1] ? ([0, 0, 0, 0] as RGBA) : palette[c - 1];
      colors.set(rgba, i * 4);
    }
    return {
      key,
      colors,
      legend: {
        kind: "categorical",
        label: info.label,
        field: info.field,
        categories: cats.map((c, i) => ({ code: i + 1, name: c.name, count: c.count, color: palette[i] })),
        nMissing,
      },
    };
  }
  const v = col.values;
  let range = cb.range;
  let nMissing = 0;
  const tf = (x: number) => (cb.log ? Math.log1p(Math.max(0, x)) : x);
  if (!range) {
    const finite = Float32Array.from(v.filter((x) => Number.isFinite(x)));
    finite.sort();
    if (finite.length) {
      const lo = info.expr !== undefined || info.ucell !== undefined ? 0 : quantile(finite, 0.01);
      const hi = quantile(finite, 0.99);
      range = [lo, hi > lo ? hi : lo + 1];
    } else range = [0, 1];
  }
  const [a, b] = [tf(range[0]), tf(range[1])];
  const span = b - a || 1;
  const lut = colormapLut(cb.colormap);
  for (let i = 0; i < n; i++) {
    const x = v[i];
    if (!Number.isFinite(x)) {
      colors.set(missing, i * 4);
      nMissing++;
      continue;
    }
    const t = Math.max(0, Math.min(255, Math.round(((tf(x) - a) / span) * 255)));
    colors.set(lut.subarray(t * 4, t * 4 + 4), i * 4);
    if (overlay?.fadeLow) colors[i * 4 + 3] = t;
  }
  return {
    key,
    colors,
    legend: {
      kind: "numeric",
      label: info.label,
      field: info.field,
      range,
      colormap: cb.colormap,
      log: cb.log,
      nMissing,
      note: cb.range
        ? "manual range (all panels)"
        : info.expr !== undefined || info.ucell !== undefined
          ? "auto: 0 – p99 (this section)"
          : "auto: p01 – p99 (this section)",
    },
  };
}

/** The overlay settings that change its colours (fill/outline/opacity only change drawing). */
export function overlayColorKey(o: Overlay): string {
  return JSON.stringify([o.source, o.colormap, o.range, o.log, [...o.hidden].sort(), o.fadeLow]);
}

/**
 * Overlay colours for drawing: the overlay is shown only on cells the base colouring
 * shows, so hidden categories stay hidden (alpha 0).
 */
export function maskOverlay(bc: Uint8Array, oc: Uint8Array): Uint8Array {
  const out = new Uint8Array(oc);
  for (let i = 3; i < out.length; i += 4) if (!bc[i]) out[i] = 0;
  return out;
}

async function recomputeSlot(slot: number) {
  const job = (colorJobs.get(slot) ?? 0) + 1;
  colorJobs.set(slot, job);
  const p = panelBySlot(slot);
  const d = bySlot.get(slot);
  if (!p?.manifest || !d?.centroids) return;
  const key = d.key;
  const st = getState();
  const ov = st.overlay;
  const memo = colorMemo.get(slot) ?? {};
  // Unchanged sources are reused, so an opacity change only redoes the blend below.
  const reuse = (old: Colourized | undefined, cb: ColorBy, o: { fadeLow: boolean } | null) =>
    old && old.ok !== false && old.key === colorKey(slot, cb, o) ? Promise.resolve(old) : colorize(slot, cb, o);
  const base = await reuse(memo.base, st.colorBy, null);
  const over = ov.source ? await reuse(memo.overlay, ov, { fadeLow: ov.fadeLow }) : undefined;
  if (!base || over === null || colorJobs.get(slot) !== job || keyOf(slot) !== key) return;
  colorMemo.set(slot, { base, overlay: over });
  setPanel(slot, (cur) => ({
    cellColors: base.colors,
    overlayColors: over ? maskOverlay(base.colors, over.colors) : undefined,
    colorVersion: cur.colorVersion + 1,
    legend: base.legend,
    overlayLegend: over?.legend,
  }));
}

/** RGBA image for the shared grid selection: one pixel per grid cell, or smoothed onto
 * about 2 µm pixels (gridSmooth.ts) when `grid.smooth` is on. Built only while the grid
 * is shown. */
export async function buildGridImage(onlySlot?: number) {
  await Promise.resolve();
  const slots = getState().panels.map((p) => p.slot).filter((s) => onlySlot === undefined || s === onlySlot);
  await Promise.all(slots.map((s) => buildGridSlot(s)));
}

/** A grid raster smoothed to F× the resolution (after a Gaussian of `sigma` cells), in
 * slices of about 12 ms so the page stays responsive; null when it stops being wanted
 * before it is done. */
function smoothRaster(d: SectionData, key: string, r: GridRaster, F: number, sigma: number, wanted: () => boolean): Promise<ArrayLike<number> | null> {
  if (d.smoothed?.key === key) return d.smoothed.out;
  const out = (async () => {
    const c = r.column;
    const sm = c.kind === "categorical" ? labelSmoother(c.codes, r.width, r.height, F, sigma) : valueSmoother(c.values, r.width, r.height, F, sigma);
    for (let i = 0; i < sm.steps; ) {
      await new Promise((res) => setTimeout(res, 0));
      if (!wanted()) return null;
      const t0 = performance.now();
      do sm.step(i++);
      while (i < sm.steps && performance.now() - t0 < 12);
    }
    return sm.out;
  })();
  const entry = { key, out };
  d.smoothed = entry;
  const drop = () => {
    if (d.smoothed === entry) d.smoothed = undefined;
  };
  out.then((v) => v || drop(), drop);
  return out;
}

/** One RGBA colour as a pixel of a Uint32Array view on ImageData (platform byte order). */
const packRgba = (c: ArrayLike<number>) => new Uint32Array(Uint8ClampedArray.from(c).buffer)[0];

async function buildGridSlot(slot: number) {
  const s = getState();
  const p = panelBySlot(slot);
  const m = p?.manifest;
  const g = m?.grids.find((x) => x.id === s.grid.gridId);
  const f = g?.fields.find((x) => x.id === s.grid.fieldId);
  const d = bySlot.get(slot);
  if (!p || !m || !g || !f || !p.api || !d) {
    if (p?.gridImage) {
      p.gridImage.bitmap.close();
      setPanel(slot, { gridImage: undefined });
    }
    return;
  }
  if (!s.grid.visible) return;
  // The colours as drawn (the palette is shared with the other panels, so opening a
  // section can change it), or the colormap.
  const look = (st: typeof s) => {
    if (f.kind !== "categorical") return st.grid.colormap;
    const byName = gridPalette(g.id, f.id);
    return (f.categories ?? []).map((c) => (st.grid.hidden.has(c.name) ? "-" : String(byName.get(c.name) ?? MISSING))).join(";");
  };
  const want = (st: typeof s) => `${keyOf(slot)}|${st.grid.gridId}|${st.grid.fieldId}|${st.grid.smooth}|${st.grid.smoothLevel}|${look(st)}`;
  const key = want(s);
  if (p.gridImage?.key === key) return; // e.g. only the opacity changed
  const rkey = `${g.id}/${f.id}`;
  let rp = d.rasters.get(rkey);
  if (!rp) {
    rp = getBuffer(p.api.gridUrl(g.id, f.id)).then(decodeGridRaster);
    rp.catch(() => d.rasters.delete(rkey));
    d.rasters.set(rkey, rp);
  }
  const r = await rp;
  const F = s.grid.smooth ? smoothFactor(r.width, r.height, g.spacing_um) : 1;
  const level = s.grid.smoothLevel;
  let data: ArrayLike<number> = r.column.kind === "categorical" ? r.column.codes : r.column.values;
  if (F > 1) {
    const wanted = () => {
      const st = getState();
      return bySlot.get(slot) === d && st.grid.visible && st.grid.smooth && st.grid.smoothLevel === level && st.grid.gridId === g.id && st.grid.fieldId === f.id;
    };
    const sm = await smoothRaster(d, `${rkey}|${F}|${level}`, r, F, SMOOTH_SIGMAS[level] ?? 0, wanted);
    if (!sm) return;
    data = sm;
  }
  const n = r.width * F * r.height * F;
  const img = new ImageData(r.width * F, r.height * F);
  const px = new Uint32Array(img.data.buffer);
  let legend: Legend;
  if (r.column.kind === "categorical") {
    const cats = f.categories ?? [];
    const byName = gridPalette(g.id, f.id);
    const palette = cats.map((c) => byName.get(c.name) ?? MISSING);
    // Pixel per code; 0 (transparent) for missing and hidden labels.
    const packed = new Uint32Array(Math.max(cats.length, r.column.nCategories) + 1);
    cats.forEach((c, i) => {
      if (!s.grid.hidden.has(c.name)) packed[i + 1] = packRgba(palette[i]);
    });
    for (let i = 0; i < n; i++) px[i] = packed[data[i]];
    legend = {
      kind: "categorical",
      label: `${g.label}: ${f.label}`,
      field: f,
      categories: cats.map((c, i) => ({ code: i + 1, name: c.name, count: c.count, color: palette[i] })),
    };
  } else {
    const lo = f.stats?.min ?? 0;
    const hi = f.stats?.max ?? 1;
    const span = hi - lo || 1;
    const lut = new Uint32Array(Uint8Array.from(colormapLut(s.grid.colormap)).buffer);
    for (let i = 0; i < n; i++) {
      const x = data[i];
      if (!Number.isFinite(x)) continue;
      px[i] = lut[Math.max(0, Math.min(255, Math.round(((x - lo) / span) * 255)))];
    }
    legend = { kind: "numeric", label: `${g.label}: ${f.label}`, field: f, range: [lo, hi], colormap: s.grid.colormap, note: "min – max (this section)" };
  }
  const bitmap = await createImageBitmap(img);
  if (key !== want(getState())) {
    bitmap.close();
    return;
  }
  panelBySlot(slot)?.gridImage?.bitmap.close();
  setPanel(slot, { gridImage: { key, bitmap, grid: g, legend } });
}

export function nextGeneColor(): string {
  const used = new Set(getState().genes.map((g) => g.color));
  const palette = ["#e6194b", "#3cb44b", "#4363d8", "#f58231", "#911eb4", "#42d4f4", "#f032e6", "#bfef45", "#469990", "#9a6324", "#800000", "#000075", "#ffe119", "#fabed4", "#dcbeff", "#aaffc3"];
  return palette.find((c) => !used.has(c)) ?? palette[getState().genes.length % palette.length];
}

// ---------------------------------------------------------------- UCell

export async function loadSignatures() {
  try {
    const signatures = await ucellApi.signatures();
    const ids = new Set(signatures.map((x) => x.id));
    setUcell((u) => ({ signatures, selected: u.selected.filter((id) => ids.has(id)), error: undefined }));
  } catch (e) {
    const old = e instanceof ApiError && e.status === 404;
    setUcell({ error: old ? "This server predates UCell scoring: restart `xenium-web serve` with the new build." : `Signatures: ${e instanceof Error ? e.message : e}` });
  }
}

/** Refresh the stored results of one section (or of every open section). */
export async function loadUcellResults(sectionId?: string) {
  const done = new Set<string>();
  for (const p of getState().panels) {
    if (!p.api || !p.sectionId || done.has(p.sectionId) || (sectionId && p.sectionId !== sectionId)) continue;
    done.add(p.sectionId);
    try {
      const results = await p.api.ucellResults();
      setUcell((u) => ({ results: { ...u.results, [p.sectionId!]: results } }));
    } catch {
      // UCell unavailable on this server: the signatures panel shows why.
    }
  }
}

/**
 * Delete one stored result of a section, then refresh. Colouring, overlay and heatmap
 * that used it are cleared unless another open section still has it.
 */
export async function deleteUcellResult(sectionId: string, key: string) {
  const p = getState().panels.find((x) => x.sectionId === sectionId && x.api);
  if (!p?.api) return;
  await p.api.ucellDelete(key);
  for (const x of getState().panels) if (x.sectionId === sectionId) bySlot.get(x.slot)?.fields.delete(`ucell:${key}`);
  await loadUcellResults(sectionId);
  const st = getState();
  const stillUsed = st.panels.some((x) => x.sectionId && st.ucell.results[x.sectionId]?.some((r) => r.key === key));
  const src = `ucell:${key}`;
  if (!stillUsed) {
    if (st.colorBy.source === src) setState({ colorBy: { ...st.colorBy, source: "", range: null, hidden: new Set() } });
    if (st.overlay.source === src) setState({ overlay: { ...getState().overlay, source: "", range: null, hidden: new Set() } });
    if (st.ucell.heat.key === key) setUcell((u) => ({ heat: { ...u.heat, key: undefined } }));
  }
  recomputeColors();
}

const polling = new Set<string>();
const finished = (j: UcellJob) => j.status !== "queued" && j.status !== "running";

function upsertJob(j: UcellJob) {
  setUcell((u) => {
    const jobs = u.jobs.filter((x) => x.id !== j.id);
    return { jobs: [j, ...jobs].slice(0, 30) };
  });
}

/** Follow a job until it ends, then load its section's results. */
export function trackJob(job: UcellJob) {
  upsertJob(job);
  if (finished(job)) {
    if (job.status === "done") loadUcellResults(job.section_id);
    return;
  }
  if (polling.has(job.id)) return;
  polling.add(job.id);
  const tick = async () => {
    try {
      const j = await ucellApi.job(job.id);
      upsertJob(j);
      if (finished(j)) {
        polling.delete(j.id);
        if (j.status === "done") loadUcellResults(j.section_id);
        return;
      }
    } catch (e) {
      polling.delete(job.id);
      setUcell({ error: `job ${job.id}: ${e instanceof Error ? e.message : e}` });
      return;
    }
    setTimeout(tick, 400);
  };
  setTimeout(tick, 250);
}

/** Score the ticked signatures in every open section (one job per section). */
export async function scoreSelected() {
  const st = getState();
  const u = st.ucell;
  if (!u.selected.length) return;
  setUcell({ error: undefined });
  const seen = new Set<string>();
  for (const p of st.panels) {
    if (!p.api || !p.manifest || !p.sectionId || seen.has(p.sectionId)) continue;
    seen.add(p.sectionId);
    try {
      const job = await p.api.ucellSubmit({
        signature_ids: u.selected,
        params: u.params,
        level: u.level,
        bin_um: u.binUm,
        min_transcripts: u.minTranscripts,
      });
      trackJob(job);
    } catch (e) {
      setUcell({ error: `${p.sectionId}: ${e instanceof Error ? e.message : e}` });
    }
  }
}

/** The square grid the server uses for bins (origin on multiples of the bin size). */
export function binGridFor(b: [number, number, number, number], binUm: number): BinGrid {
  const ox = Math.floor(b[0] / binUm) * binUm;
  const oy = Math.floor(b[1] / binUm) * binUm;
  return {
    origin_um: [ox, oy],
    bin_um: binUm,
    width: Math.max(1, Math.ceil((b[2] - ox) / binUm)),
    height: Math.max(1, Math.ceil((b[3] - oy) / binUm)),
  };
}

/** Mean of finite cell values per bin (by centroid); NaN where no cell has a value. */
export function binMeans(centroids: Float32Array, values: Float32Array, g: BinGrid): { mean: Float32Array; n: Uint32Array } {
  const sum = new Float64Array(g.width * g.height);
  const n = new Uint32Array(g.width * g.height);
  for (let i = 0; i < values.length; i++) {
    const v = values[i];
    if (!Number.isFinite(v)) continue;
    const cx = Math.floor((centroids[2 * i] - g.origin_um[0]) / g.bin_um);
    const cy = Math.floor((centroids[2 * i + 1] - g.origin_um[1]) / g.bin_um);
    if (cx < 0 || cy < 0 || cx >= g.width || cy >= g.height) continue;
    sum[cy * g.width + cx] += v;
    n[cy * g.width + cx]++;
  }
  const mean = new Float32Array(sum.length);
  for (let i = 0; i < sum.length; i++) mean[i] = n[i] ? sum[i] / n[i] : NaN;
  return { mean, n };
}

export async function buildUcellHeat(onlySlot?: number) {
  await Promise.resolve();
  const slots = getState().panels.map((p) => p.slot).filter((s) => onlySlot === undefined || s === onlySlot);
  await Promise.all(slots.map((s) => buildHeatSlot(s)));
}

async function buildHeatSlot(slot: number) {
  const s = getState();
  const h = s.ucell.heat;
  const p = panelBySlot(slot);
  const d = bySlot.get(slot);
  const clear = () => {
    if (panelBySlot(slot)?.ucellImage) {
      panelBySlot(slot)!.ucellImage!.bitmap.close();
      setPanel(slot, { ucellImage: undefined });
    }
  };
  if (!h.key || !h.visible || !p?.manifest || !p.api || !d?.centroids) return clear();
  const result = s.ucell.results[p.sectionId!]?.find((r) => r.key === h.key);
  const want = (st: typeof s) =>
    `${keyOf(slot)}|${JSON.stringify(st.ucell.heat)}|${st.ucell.results[p.sectionId!]?.find((r) => r.key === h.key)?.computed_unix ?? "-"}`;
  const key = want(s);
  if (p.ucellImage?.key === key) return;
  if (!result) return clear();
  let grid: BinGrid;
  let vals: Float32Array;
  let note: string;
  if (result.level === "bin") {
    const raster = await getBuffer(p.api.ucellValuesUrl(result.key)).then(decodeGridRaster);
    if (raster.column.kind !== "numeric") return clear();
    grid = result.grid!;
    vals = raster.column.values;
    note = `UCell of transcripts pooled per ${result.bin_um} µm bin (≥ ${result.min_transcripts} transcripts)`;
  } else {
    const col = await fieldColumn(slot, `ucell:${result.key}`);
    if (col.kind !== "numeric") return clear();
    grid = binGridFor(p.manifest.bounds_um, h.binUm);
    vals = binMeans(d.centroids, col.values, grid).mean;
    note = `mean of cell-level UCell per ${h.binUm} µm bin (cells by centroid)`;
  }
  const finite = Float32Array.from(vals.filter((x) => Number.isFinite(x))).sort();
  const hi = finite.length ? Math.max(quantile(finite, 0.99), 1e-6) : 1;
  const lut = colormapLut(h.colormap);
  const img = new ImageData(grid.width, grid.height);
  for (let i = 0; i < vals.length; i++) {
    const x = vals[i];
    if (!Number.isFinite(x)) continue;
    const t = Math.max(0, Math.min(255, Math.round((x / hi) * 255)));
    img.data.set(lut.subarray(t * 4, t * 4 + 4), i * 4);
  }
  const bitmap = await createImageBitmap(img);
  if (key !== want(getState())) {
    bitmap.close();
    return;
  }
  panelBySlot(slot)?.ucellImage?.bitmap.close();
  setPanel(slot, {
    ucellImage: {
      key,
      bitmap,
      grid,
      legend: { kind: "numeric", label: ucellLabel(result, result.key), range: [0, hi], colormap: h.colormap, note },
    },
  });
}
