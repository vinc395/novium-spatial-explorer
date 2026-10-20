// Lasso selection: which cells have their centroid inside a freehand polygon.

/** Ray-casting point-in-polygon test (polygon as [x, y] world coordinates). */
export function pointInPolygon(x: number, y: number, poly: [number, number][]): boolean {
  let inside = false;
  for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
    const [xi, yi] = poly[i];
    const [xj, yj] = poly[j];
    if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) inside = !inside;
  }
  return inside;
}

/**
 * 1-based cell keys whose centroid lies inside `poly`. Cells drawn fully transparent
 * (hidden categories, alpha 0 in `colors`) are skipped, as they are for clicking.
 */
export function cellsInPolygon(centroids: Float32Array, colors: Uint8Array | undefined, poly: [number, number][]): number[] {
  if (poly.length < 3) return [];
  let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
  for (const [x, y] of poly) {
    x0 = Math.min(x0, x);
    y0 = Math.min(y0, y);
    x1 = Math.max(x1, x);
    y1 = Math.max(y1, y);
  }
  const out: number[] = [];
  const n = centroids.length / 2;
  for (let i = 0; i < n; i++) {
    const x = centroids[2 * i];
    const y = centroids[2 * i + 1];
    if (x < x0 || x > x1 || y < y0 || y > y1) continue;
    if (colors && colors[i * 4 + 3] === 0) continue;
    if (pointInPolygon(x, y, poly)) out.push(i + 1);
  }
  return out;
}

/** Combine a lasso result with the current selection. */
export function applyLasso(current: Set<number>, keys: number[], mode: "replace" | "add" | "remove"): Set<number> {
  if (mode === "replace") return new Set(keys);
  const next = new Set(current);
  for (const k of keys) {
    if (mode === "add") next.add(k);
    else next.delete(k);
  }
  return next;
}
