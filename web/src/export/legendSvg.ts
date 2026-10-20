// The legends of the active panel as one SVG (vector, editable in Illustrator or
// Inkscape): what each colour means, for the view export or on its own.

import { colormapStops, type RGBA } from "../colors";
import type { Legend } from "../data";

export interface LegendPart {
  /** e.g. "Overlay", "Morphology"; the legend's own label follows it */
  heading?: string;
  legend?: Legend;
  /** category names not drawn (left out of the legend) */
  hidden?: Set<string>;
  /** morphology channels: name and colour */
  channels?: { name: string; color: string }[];
}

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const num = (x: number) => (Math.abs(x) >= 1000 || (Math.abs(x) < 0.01 && x !== 0) ? x.toExponential(2) : x.toFixed(2));
const rgb = (c: RGBA) => `rgb(${c[0]},${c[1]},${c[2]})`;

const FONT = "Arial, Helvetica, sans-serif";
const ROW = 16;

/** SVG source of the given legends, stacked top to bottom (empty parts are skipped). */
export function legendSvg(parts: LegendPart[]): string {
  const body: string[] = [];
  const defs: string[] = [];
  let y = 14;
  let widest = 160;
  const text = (x: number, yy: number, s: string, attrs = "") => {
    widest = Math.max(widest, x + s.length * 6.4);
    body.push(`<text x="${x}" y="${yy}"${attrs}>${esc(s)}</text>`);
  };
  parts.forEach((part, pi) => {
    const lg = part.legend;
    const title = [part.heading, lg && lg.kind !== "none" ? lg.label : ""].filter(Boolean).join(" · ");
    if (!part.channels?.length && (!lg || lg.kind === "none")) return;
    if (body.length) y += 10;
    text(0, y, title, ' font-weight="bold" font-size="12"');
    y += 8;
    if (part.channels?.length) {
      for (const c of part.channels) {
        body.push(`<rect x="0" y="${y}" width="12" height="12" fill="${esc(c.color)}" stroke="#00000026"/>`);
        text(18, y + 10, c.name);
        y += ROW;
      }
      return;
    }
    if (lg!.kind === "numeric") {
      const stops = colormapStops(lg!.colormap ?? "viridis");
      const id = `ramp${pi}`;
      defs.push(
        `<linearGradient id="${id}" x1="0" x2="1" y1="0" y2="0">${stops
          .map((c, i) => `<stop offset="${(i / (stops.length - 1)).toFixed(4)}" stop-color="${c}"/>`)
          .join("")}</linearGradient>`,
      );
      body.push(`<rect x="0" y="${y}" width="180" height="12" fill="url(#${id})" stroke="#00000026"/>`);
      y += 24;
      const [lo, hi] = lg!.range ?? [0, 1];
      text(0, y, num(lo));
      body.push(`<text x="180" y="${y}" text-anchor="end">${esc(num(hi))}</text>`);
      if (lg!.log) {
        y += ROW;
        text(0, y, "log1p scale", ' fill="#666"');
      }
      y += 6;
      return;
    }
    for (const c of lg!.categories ?? []) {
      if (part.hidden?.has(c.name)) continue;
      body.push(`<rect x="0" y="${y}" width="12" height="12" fill="${rgb(c.color)}" stroke="#00000026"/>`);
      text(18, y + 10, c.name);
      y += ROW;
    }
  });
  const w = Math.ceil(widest + 4);
  const h = Math.ceil(y + 6);
  return [
    `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="-2 0 ${w} ${h}" font-family="${FONT}" font-size="11" fill="#222">`,
    defs.length ? `<defs>${defs.join("")}</defs>` : "",
    ...body,
    "</svg>",
    "",
  ].join("\n");
}
