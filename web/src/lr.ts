import type { FieldInfo } from "./types";

/** Field groups of precomputed ligand–receptor results: hundreds of columns per
 *  section, so the inspector loads them only on demand and exports skip them unless asked. */
export const LR_GROUPS = ["lr_score", "lr_hotspot"];

export const isLr = (f: FieldInfo) => LR_GROUPS.includes(f.group);

export interface LrRow {
  id: string;
  /** e.g. "CXCL12 → CXCR4 · diffusion" */
  label: string;
  score: number | null;
  /** "background", "hotspot", "hotspot (HQ)", or null (not estimable / no hotspot column) */
  hotspot: string | null;
}

const HOT_RANK: Record<string, number> = { "hotspot (HQ)": 0, hotspot: 1 };

/** One row per LR score field with a value at this cell, paired with its hotspot field
 *  (`…:score` / `…:hotspot`); hotspots first (HQ before plain), then by score, high first.
 *  Rows without a score (not estimable here) come last. */
export function lrRows(fields: FieldInfo[], values: Record<string, number | string | null>): LrRow[] {
  const ids = new Set(fields.map((f) => f.id));
  const rows: LrRow[] = [];
  for (const f of fields) {
    if (f.group !== "lr_score" || !(f.id in values)) continue;
    const hid = f.id.endsWith(":score") ? `${f.id.slice(0, -6)}:hotspot` : "";
    const h = ids.has(hid) ? values[hid] : null;
    const s = values[f.id];
    rows.push({
      id: f.id,
      label: f.label.replace(/ score$/, ""),
      score: typeof s === "number" ? s : null,
      hotspot: typeof h === "string" ? h : null,
    });
  }
  const rank = (r: LrRow) => (r.score === null ? 3 : (HOT_RANK[r.hotspot ?? ""] ?? 2));
  return rows.sort((a, b) => rank(a) - rank(b) || (b.score ?? 0) - (a.score ?? 0) || a.label.localeCompare(b.label));
}
