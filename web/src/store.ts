// Minimal external store (no extra dependency). Components subscribe with
// useStore(selector); high-frequency viewport updates never go through React.
//
// Multiview: `panels` (1-4) each show one section with its own loaded data;
// display settings (layers, colour-by, genes, grid) are shared by all panels and
// are keyed by *names* (gene names, category names) so they mean the same thing in
// every section even though ids and category codes differ between sections.

import { useSyncExternalStore } from "react";
import type { BinGrid, CellDetails, GridInfo, ImageInfo, Manifest, SectionSummary, Signature, UcellJob, UcellParams, UcellResult } from "./types";
import type { SectionApi } from "./api";
import { loadTheme, type ThemeChoice } from "./theme";
import { DEFAULT_CHOICE, loadPalettes, type PaletteChoice } from "./palettes";
import type { Legend } from "./data";

export interface GridImage {
  key: string;
  bitmap: ImageBitmap;
  grid: GridInfo;
  legend: Legend;
}

/** A selected gene, by name (resolved to a per-section id when drawing). */
export interface GeneSel {
  name: string;
  color: string;
}

export interface ColorBy {
  /** field id, `expr:<gene name>`, or "" for a single colour */
  source: string;
  colormap: string;
  /** null = auto (p01..p99 for fields, 0..p99 for expression) */
  range: [number, number] | null;
  log: boolean;
  /** hidden category names */
  hidden: Set<string>;
  /** cells without a value are not drawn (instead of grey); the overlay never draws them */
  hideMissing: boolean;
}

/**
 * A second colour source drawn as its own layer above "Colour cells by" (e.g. an LR
 * score over cell types), with its own fill and outline settings.
 */
export interface Overlay extends ColorBy {
  /** draw the overlay colour as a fill above the cells */
  fill: boolean;
  /** draw it as a cell outline above the cells */
  outline: boolean;
  /** 0..1, opacity of the overlay fill */
  opacity: number;
  /** overlay outline width in screen pixels */
  outlineWidth: number;
  /** numeric overlays: alpha follows the scaled value, so low values fade out */
  fadeLow: boolean;
}

/** Display settings of one morphology channel (shared by name across panels). */
export interface MorphChannel {
  on: boolean;
  /** #rrggbb */
  color: string;
  /** gain on the stored 8-bit intensities (1 = as ingested) */
  brightness: number;
  /** 0..1 */
  opacity: number;
}

/** A UCell heatmap raster for one panel (bin-level result, or cell scores averaged per bin). */
export interface UcellImage {
  key: string;
  bitmap: ImageBitmap;
  grid: BinGrid;
  legend: Legend;
}

export interface UcellState {
  signatures: Signature[];
  error?: string;
  /** signature ids ticked for scoring */
  selected: string[];
  params: UcellParams;
  level: "cell" | "bin";
  binUm: number;
  minTranscripts: number;
  /** jobs started from this browser (latest known status) */
  jobs: UcellJob[];
  /** stored results of the live index version, by section id */
  results: Record<string, UcellResult[]>;
  /**
   * Heatmap: `mode` "cell-mean" averages a cell-level result over `binUm` squares
   * (by centroid); "bin" draws a bin-level result as computed. Keyed by result key,
   * which is the same in every section.
   */
  heat: { key?: string; mode: "cell-mean" | "bin"; binUm: number; visible: boolean; opacity: number; colormap: string };
}

export interface PanelData {
  /** stable slot index (0..3) used in layer/view ids */
  slot: number;
  sectionId?: string;
  api?: SectionApi;
  manifest?: Manifest;
  loadError?: string;
  /** XU centroids, index = cell_key - 1 */
  centroids?: Float32Array;
  /** RGBA per cell, index = cell_key - 1 */
  cellColors?: Uint8Array;
  colorVersion: number;
  legend?: Legend;
  /** RGBA per cell for the overlay layer (alpha 0 = not drawn) */
  overlayColors?: Uint8Array;
  overlayLegend?: Legend;
  ucellImage?: UcellImage;
  gridImage?: GridImage;
  /** gene id (this section) -> (tile key -> transcript count) for LOD decisions */
  geneTileCounts: Map<number, Map<number, number>>;
  selection: Set<number>;
  selected?: number;
  details?: CellDetails;
  detailsError?: string;
}

export interface AppState {
  sections: SectionSummary[];
  sectionsError?: string;
  /** Non-fatal message shown in the header (dismissible). */
  notice?: string;
  panels: PanelData[];
  /** index into `panels` of the panel the side panels refer to */
  active: number;
  /** all panels share one zoom level (pan stays independent) */
  linkZoom: boolean;
  /** colour theme: follow the system, or a fixed choice */
  theme: ThemeChoice;
  /** lasso selection mode (dragging draws a selection outline instead of panning) */
  lasso: boolean;
  /** morphology channel settings by channel name (absent = defaults, off) */
  morph: Record<string, MorphChannel>;
  /** master opacity of the mixed morphology layer (0..1), on top of each channel's own */
  morphOpacity: number;
  he: { visible: boolean; opacity: number };
  cells: {
    fill: boolean;
    outline: boolean;
    centroids: "auto" | "on" | "off";
    nuclei: boolean;
    opacity: number;
    outlineWidth: number;
  };
  colorBy: ColorBy;
  /** source "" = no overlay */
  overlay: Overlay;
  /** category colours chosen per categorical field id (scheme, shuffle, picked by hand) */
  palettes: Record<string, PaletteChoice>;
  /** open (true) or folded panel sections by id, once changed in this session */
  folds: Record<string, boolean>;
  /** fold the legend (and open "Selected cell") when a cell is clicked */
  foldLegendOnClick: boolean;
  /** the side panels shown (hidden ones give the tissue more room) */
  sidePanels: { left: boolean; right: boolean };
  ucell: UcellState;
  transcripts: {
    visible: boolean;
    pointSize: number;
    opacity: number;
    qvMin: number;
    showUnassigned: boolean;
    mode: "auto" | "points" | "density";
  };
  genes: GeneSel[];
  /** `hidden`: category names not drawn (categorical grid fields only). */
  grid: { gridId?: string; fieldId?: string; visible: boolean; opacity: number; colormap: string; hidden: Set<string> };
}

export function emptyPanel(slot: number): PanelData {
  return { slot, colorVersion: 0, geneTileCounts: new Map(), selection: new Set() };
}

export const initialState: AppState = {
  sections: [],
  panels: [emptyPanel(0)],
  active: 0,
  linkZoom: true,
  theme: loadTheme(),
  lasso: false,
  morph: {},
  morphOpacity: 1,
  he: { visible: true, opacity: 1 },
  cells: { fill: true, outline: true, centroids: "auto", nuclei: false, opacity: 0.45, outlineWidth: 1 },
  colorBy: { source: "", colormap: "viridis", range: null, log: false, hidden: new Set(), hideMissing: false },
  overlay: { source: "", colormap: "magma", range: null, log: false, hidden: new Set(), hideMissing: false, fill: true, outline: false, opacity: 0.6, outlineWidth: 2, fadeLow: true },
  palettes: loadPalettes(),
  folds: {},
  foldLegendOnClick: loadPref("novium:foldLegendOnClick", true),
  sidePanels: { left: loadPref("novium:panel:left", true), right: loadPref("novium:panel:right", true) },
  ucell: {
    signatures: [],
    selected: [],
    params: { max_rank: 1500, w_neg: 1, missing_genes: "impute" },
    level: "cell",
    binUm: 50,
    minTranscripts: 10,
    jobs: [],
    results: {},
    heat: { mode: "cell-mean", binUm: 50, visible: true, opacity: 0.75, colormap: "magma" },
  },
  transcripts: { visible: true, pointSize: 3, opacity: 0.9, qvMin: 20, showUnassigned: true, mode: "auto" },
  genes: [],
  grid: { visible: true, opacity: 0.5, colormap: "viridis", hidden: new Set() },
};

let state: AppState = initialState;
const listeners = new Set<() => void>();

export function getState(): AppState {
  return state;
}

export function setState(patch: Partial<AppState> | ((s: AppState) => Partial<AppState>)) {
  const p = typeof patch === "function" ? patch(state) : patch;
  state = { ...state, ...p };
  listeners.forEach((l) => l());
}

/** Patch one panel (by slot), keeping all others unchanged. */
/** Replace a panel's whole state (switching its section): nothing of the previous section,
 *  such as its manifest, centroids or colours, may survive into the next one. */
export function resetPanel(slot: number, patch: Partial<PanelData>) {
  setState((s) => ({ panels: s.panels.map((p) => (p.slot === slot ? { ...emptyPanel(slot), ...patch } : p)) }));
}

export function setPanel(slot: number, patch: Partial<PanelData> | ((p: PanelData) => Partial<PanelData>)) {
  setState((s) => ({
    panels: s.panels.map((p) => (p.slot === slot ? { ...p, ...(typeof patch === "function" ? patch(p) : patch) } : p)),
  }));
}

/** A boolean preference remembered in this browser (best effort). */
function loadPref(key: string, fallback: boolean): boolean {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : v === "1";
  } catch {
    return fallback;
  }
}

export function savePref(key: string, v: boolean) {
  try {
    localStorage.setItem(key, v ? "1" : "0");
  } catch {
    /* private window etc.: not remembered */
  }
}

/** Open or fold panel sections by id (remembered in this browser). */
export function setFolds(patch: Record<string, boolean>) {
  for (const [id, open] of Object.entries(patch)) savePref(`novium:open:${id}`, open);
  setState((s) => ({ folds: { ...s.folds, ...patch } }));
}

export function setSidePanel(side: "left" | "right", shown: boolean) {
  savePref(`novium:panel:${side}`, shown);
  setState((s) => ({ sidePanels: { ...s.sidePanels, [side]: shown } }));
}

/** Change one field's category colours (`null` = back to the default scheme). */
export function setPalette(fieldId: string, patch: Partial<PaletteChoice> | null) {
  setState((s) => {
    const palettes = { ...s.palettes };
    if (patch === null) delete palettes[fieldId];
    else palettes[fieldId] = { ...DEFAULT_CHOICE, ...(palettes[fieldId] as PaletteChoice | undefined), ...patch };
    return { palettes };
  });
}

/** Pick one category's colour by hand ("#rrggbb"). */
export function setCategoryColor(fieldId: string, name: string, hex: string) {
  const cur = state.palettes[fieldId];
  setPalette(fieldId, { custom: { ...cur?.custom, [name]: hex } });
}

export function panelBySlot(slot: number): PanelData | undefined {
  return state.panels.find((p) => p.slot === slot);
}

export function activePanel(s: AppState = state): PanelData {
  return s.panels[Math.min(s.active, s.panels.length - 1)];
}

export function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

export function useStore<T>(sel: (s: AppState) => T): T {
  return useSyncExternalStore(subscribe, () => sel(state));
}

/** Gene name -> id map for a manifest (cached). */
const geneIndex = new WeakMap<Manifest, Map<string, number>>();
export function geneIdsByName(m: Manifest): Map<string, number> {
  let idx = geneIndex.get(m);
  if (!idx) {
    idx = new Map((m.transcripts?.genes ?? []).map((g) => [g.name, g.id]));
    geneIndex.set(m, idx);
  }
  return idx;
}

/** The shared gene selection resolved to this panel's gene ids (missing genes dropped). */
export function panelGenes(p: PanelData, genes: GeneSel[]): { id: number; name: string; color: string }[] {
  if (!p.manifest) return [];
  const idx = geneIdsByName(p.manifest);
  return genes.flatMap((g) => {
    const id = idx.get(g.name);
    return id === undefined ? [] : [{ id, name: g.name, color: g.color }];
  });
}

export function setUcell(patch: Partial<UcellState> | ((u: UcellState) => Partial<UcellState>)) {
  setState((s) => ({ ucell: { ...s.ucell, ...(typeof patch === "function" ? patch(s.ucell) : patch) } }));
}

/** A stored UCell result by key, from any open section (keys are section-independent). */
export function ucellResultByKey(key: string, sectionId?: string): UcellResult | undefined {
  const r = state.ucell.results;
  if (sectionId) {
    const hit = r[sectionId]?.find((x) => x.key === key);
    if (hit) return hit;
  }
  for (const list of Object.values(r)) {
    const hit = list.find((x) => x.key === key);
    if (hit) return hit;
  }
  return undefined;
}

const hex2 = (v: number) => v.toString(16).padStart(2, "0");

/** Settings for a channel: what the user set, else off with the ingest's suggested colour. */
export function morphSettings(morph: Record<string, MorphChannel>, img: ImageInfo): MorphChannel {
  const name = img.channel?.name ?? img.label;
  const c = img.channel?.color ?? [255, 255, 255];
  return morph[name] ?? { on: false, color: `#${hex2(c[0])}${hex2(c[1])}${hex2(c[2])}`, brightness: 1, opacity: 1 };
}
