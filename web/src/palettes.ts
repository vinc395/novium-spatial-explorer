// Categorical colour schemes and the viewer's per-field colour choices (scheme, shuffle,
// colours picked by hand). Choices are shared by every panel, by "Colour cells by" and
// by the overlay, and remembered in this browser only.

import { categoricalColor, hexToRgba, type RGBA } from "./colors";

export interface PaletteChoice {
  /** a key of CATEGORICAL_SCHEMES */
  scheme: string;
  /** 0 = the scheme's order; otherwise the seed of a shuffled order */
  seed: number;
  /** colours picked by hand per category name ("#rrggbb"); they win over the scheme */
  custom: Record<string, string>;
}

/** Categories beyond a scheme's own colours get generated, still distinct, colours. */
export const CATEGORICAL_SCHEMES: Record<string, { label: string; colors?: string[] }> = {
  default: { label: "Default (Tableau 20 + generated)" },
  kelly: {
    label: "Kelly (20, high contrast)",
    colors: ["#f3c300", "#875692", "#f38400", "#a1caf1", "#be0032", "#c2b280", "#848482", "#008856", "#e68fac", "#0067a5",
      "#f99379", "#604e97", "#f6a600", "#b3446c", "#dcd300", "#882d17", "#8db600", "#654522", "#e25822", "#2b3d26"],
  },
  okabe: { label: "Okabe–Ito (7, colour-blind safe)", colors: ["#e69f00", "#56b4e9", "#009e73", "#f0e442", "#0072b2", "#d55e00", "#cc79a7"] },
  set1: { label: "Set1 (9)", colors: ["#e41a1c", "#377eb8", "#4daf4a", "#984ea3", "#ff7f00", "#ffff33", "#a65628", "#f781bf", "#999999"] },
  dark2: { label: "Dark2 (8)", colors: ["#1b9e77", "#d95f02", "#7570b3", "#e7298a", "#66a61e", "#e6ab02", "#a6761d", "#666666"] },
  paired: {
    label: "Paired (12)",
    colors: ["#a6cee3", "#1f78b4", "#b2df8a", "#33a02c", "#fb9a99", "#e31a1c", "#fdbf6f", "#ff7f00", "#cab2d6", "#6a3d9a", "#ffff99", "#b15928"],
  },
  set3: {
    label: "Set3 (12, pastel)",
    colors: ["#8dd3c7", "#ffffb3", "#bebada", "#fb8072", "#80b1d3", "#fdb462", "#b3de69", "#fccde5", "#d9d9d9", "#bc80bd", "#ccebc5", "#ffed6f"],
  },
  rainbow: { label: "Rainbow (spread over all categories)" },
};

export const DEFAULT_CHOICE: PaletteChoice = { scheme: "default", seed: 0, custom: {} };

function hslToRgba(h: number, s: number, l: number): RGBA {
  const a = s * Math.min(l, 1 - l);
  const f = (k0: number) => {
    const k = (k0 + h / 30) % 12;
    return Math.round(255 * (l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1))));
  };
  return [f(0), f(8), f(4), 255];
}

/** Colour number `j` (0-based) of a scheme used for `n` categories. */
export function schemeColor(scheme: string, j: number, n: number): RGBA {
  if (scheme === "rainbow") return hslToRgba((j * 360) / Math.max(1, n), 0.75, 0.55);
  const own = CATEGORICAL_SCHEMES[scheme]?.colors;
  if (!own) return categoricalColor(j);
  // Past the scheme's own colours: the generated ones (skipping the default's fixed 20).
  return j < own.length ? hexToRgba(own[j]) : categoricalColor(20 + j - own.length);
}

/** Deterministic shuffle of 0..n-1 (mulberry32). */
export function shuffledOrder(n: number, seed: number): number[] {
  const order = Array.from({ length: n }, (_, i) => i);
  let t = seed >>> 0;
  const rnd = () => {
    t = (t + 0x6d2b79f5) >>> 0;
    let r = Math.imul(t ^ (t >>> 15), 1 | t);
    r ^= r + Math.imul(r ^ (r >>> 7), 61 | r);
    return ((r ^ (r >>> 14)) >>> 0) / 4294967296;
  };
  for (let i = n - 1; i > 0; i--) {
    const k = Math.floor(rnd() * (i + 1));
    [order[i], order[k]] = [order[k], order[i]];
  }
  return order;
}

/** Colour per category name: `names` is the sorted union over the open panels. */
export function categoryPalette(names: string[], choice: PaletteChoice = DEFAULT_CHOICE): Map<string, RGBA> {
  const n = names.length;
  const order = choice.seed ? shuffledOrder(n, choice.seed) : null;
  return new Map(
    names.map((name, i) => {
      const hand = choice.custom[name];
      return [name, hand ? hexToRgba(hand) : schemeColor(choice.scheme, order ? order[i] : i, n)];
    }),
  );
}

export function isDefaultChoice(c: PaletteChoice | undefined): boolean {
  return !c || (c.scheme === "default" && !c.seed && Object.keys(c.custom).length === 0);
}

export function rgbaHex(c: RGBA): string {
  return `#${[c[0], c[1], c[2]].map((v) => v.toString(16).padStart(2, "0")).join("")}`;
}

const STORE_KEY = "novium.palettes.v1";

/** Colour choices remembered in this browser (empty if storage is unavailable). */
export function loadPalettes(): Record<string, PaletteChoice> {
  try {
    const raw = localStorage.getItem(STORE_KEY);
    const v = raw ? JSON.parse(raw) : {};
    const out: Record<string, PaletteChoice> = {};
    for (const [k, c] of Object.entries(v as Record<string, Partial<PaletteChoice>>)) {
      if (!c || typeof c !== "object") continue;
      const custom = Object.fromEntries(
        Object.entries(c.custom ?? {}).filter(([, h]) => typeof h === "string" && /^#[0-9a-f]{6}$/i.test(h)),
      );
      out[k] = {
        scheme: typeof c.scheme === "string" && c.scheme in CATEGORICAL_SCHEMES ? c.scheme : "default",
        seed: Number.isInteger(c.seed) ? (c.seed as number) : 0,
        custom,
      };
    }
    return out;
  } catch {
    return {};
  }
}

export function savePalettes(p: Record<string, PaletteChoice>) {
  try {
    const keep = Object.fromEntries(Object.entries(p).filter(([, c]) => !isDefaultChoice(c)));
    localStorage.setItem(STORE_KEY, JSON.stringify(keep));
  } catch {
    // private window or blocked storage: the choice lasts for this page only
  }
}
