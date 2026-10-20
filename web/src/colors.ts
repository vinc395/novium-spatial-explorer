// Palettes. Categorical colours are generated for any number of categories
// (no fixed 64-class cap); continuous colormaps are sampled lookup tables.

export type RGBA = [number, number, number, number];

const BASE: RGBA[] = [
  [31, 119, 180, 255], [255, 127, 14, 255], [44, 160, 44, 255], [214, 39, 40, 255], [148, 103, 189, 255],
  [140, 86, 75, 255], [227, 119, 194, 255], [127, 127, 127, 255], [188, 189, 34, 255], [23, 190, 207, 255],
  [174, 199, 232, 255], [255, 187, 120, 255], [152, 223, 138, 255], [255, 152, 150, 255], [197, 176, 213, 255],
  [196, 156, 148, 255], [247, 182, 210, 255], [199, 199, 199, 255], [219, 219, 141, 255], [158, 218, 229, 255],
];

function hsl(h: number, s: number, l: number): RGBA {
  const a = s * Math.min(l, 1 - l);
  const f = (n: number) => {
    const k = (n + h / 30) % 12;
    return Math.round(255 * (l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1))));
  };
  return [f(0), f(8), f(4), 255];
}

/** Distinct-ish colour for category index i (0-based) out of n. */
export function categoricalColor(i: number): RGBA {
  if (i < BASE.length) return BASE[i];
  const j = i - BASE.length;
  const hue = (j * 137.508) % 360; // golden angle
  const light = [0.45, 0.62, 0.35, 0.72][j % 4];
  const sat = [0.65, 0.8, 0.55][Math.floor(j / 4) % 3];
  return hsl(hue, sat, light);
}

export const MISSING: RGBA = [160, 160, 160, 110];

const STOPS: Record<string, string[]> = {
  viridis: ["#440154", "#482878", "#3e4989", "#31688e", "#26828e", "#1f9e89", "#35b779", "#6ece58", "#b5de2b", "#fde725"],
  magma: ["#000004", "#180f3d", "#440f76", "#721f81", "#9e2f7f", "#cd4071", "#f1605d", "#fd9668", "#feca8d", "#fcfdbf"],
  inferno: ["#000004", "#1b0c41", "#4a0c6b", "#781c6d", "#a52c60", "#cf4446", "#ed6925", "#fb9b06", "#f7d13d", "#fcffa4"],
  rdbu: ["#053061", "#2166ac", "#4393c3", "#92c5de", "#d1e5f0", "#fddbc7", "#f4a582", "#d6604d", "#b2182b", "#67001f"],
  greys: ["#f7f7f7", "#d9d9d9", "#bdbdbd", "#969696", "#737373", "#525252", "#252525", "#000000"],
  plasma: ["#0d0887", "#41049d", "#6a00a8", "#8f0da4", "#b12a90", "#cc4778", "#e16462", "#f2844b", "#fca636", "#fcce25", "#f0f921"],
  cividis: ["#00224e", "#123570", "#3b496c", "#575d6d", "#707173", "#8a8678", "#a59c74", "#c3b369", "#e1cc55", "#fee838"],
  turbo: ["#30123b", "#4145ab", "#4675ed", "#39a2fc", "#1bcfd4", "#24eca6", "#61fc6c", "#a4fc3b", "#d1e834", "#f3c63a", "#fe9b2d", "#f36315", "#d93806", "#b11901", "#7a0403"],
  blues: ["#f7fbff", "#deebf7", "#c6dbef", "#9ecae1", "#6baed6", "#4292c6", "#2171b5", "#08519c", "#08306b"],
  reds: ["#fff5f0", "#fee0d2", "#fcbba1", "#fc9272", "#fb6a4a", "#ef3b2c", "#cb181d", "#a50f15", "#67000d"],
};
export const COLORMAPS = Object.keys(STOPS);
export const DIVERGING = new Set(["rdbu"]);

function hex(h: string): [number, number, number] {
  return [parseInt(h.slice(1, 3), 16), parseInt(h.slice(3, 5), 16), parseInt(h.slice(5, 7), 16)];
}

const lutCache = new Map<string, Uint8Array>();
/** 256-entry RGBA lookup table. */
export function colormapLut(name: string): Uint8Array {
  const hit = lutCache.get(name);
  if (hit) return hit;
  const stops = (STOPS[name] ?? STOPS.viridis).map(hex);
  const lut = new Uint8Array(256 * 4);
  for (let i = 0; i < 256; i++) {
    const t = (i / 255) * (stops.length - 1);
    const a = Math.floor(t);
    const b = Math.min(a + 1, stops.length - 1);
    const f = t - a;
    for (let c = 0; c < 3; c++) lut[i * 4 + c] = Math.round(stops[a][c] * (1 - f) + stops[b][c] * f);
    lut[i * 4 + 3] = 255;
  }
  lutCache.set(name, lut);
  return lut;
}

/** The colour stops ("#rrggbb", low to high) of a colormap. */
export function colormapStops(name: string): string[] {
  return STOPS[name] ?? STOPS.viridis;
}

export function cssGradient(name: string): string {
  return `linear-gradient(to right, ${(STOPS[name] ?? STOPS.viridis).join(",")})`;
}

export const GENE_COLORS: string[] = [
  "#e6194b", "#3cb44b", "#ffe119", "#4363d8", "#f58231", "#911eb4", "#42d4f4", "#f032e6",
  "#bfef45", "#fabed4", "#469990", "#dcbeff", "#9a6324", "#fffac8", "#800000", "#aaffc3",
];

export function hexToRgba(h: string, a = 255): RGBA {
  const [r, g, b] = hex(h);
  return [r, g, b, a];
}

export function rgbaCss(c: RGBA): string {
  return `rgba(${c[0]},${c[1]},${c[2]},${(c[3] / 255).toFixed(3)})`;
}
