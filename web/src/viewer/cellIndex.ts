// Nearest-cell lookup on the CPU, for clicks where no cell shape is drawn (fill and
// outline off, or zoomed out without dots): a uniform grid over the centroids, built
// once per centroid array.

interface Grid {
  size: number;
  buckets: Map<string, number[]>;
}

const grids = new WeakMap<Float32Array, Map<number, Grid>>();

function gridFor(centroids: Float32Array, size: number): Grid {
  let bySize = grids.get(centroids);
  if (!bySize) grids.set(centroids, (bySize = new Map()));
  let g = bySize.get(size);
  if (!g) {
    const buckets = new Map<string, number[]>();
    for (let i = 0; i < centroids.length / 2; i++) {
      const k = `${Math.floor(centroids[2 * i] / size)},${Math.floor(centroids[2 * i + 1] / size)}`;
      const b = buckets.get(k);
      if (b) b.push(i);
      else buckets.set(k, [i]);
    }
    g = { size, buckets };
    bySize.set(size, g);
  }
  return g;
}

/**
 * Key (1-based) of the cell whose centroid is nearest to (x, y) µm and within `radius`
 * µm, or undefined. Cells drawn fully transparent in `colors` (hidden categories, cells
 * without a value when those are hidden) are skipped, as they are for picking.
 */
export function nearestCellKey(centroids: Float32Array, x: number, y: number, radius: number, colors?: Uint8Array): number | undefined {
  const size = Math.max(1, radius);
  const g = gridFor(centroids, size);
  const gx = Math.floor(x / size), gy = Math.floor(y / size);
  let best: number | undefined;
  let bestD = radius * radius + 1e-9; // on a tie the first one found stays
  for (let dx = -1; dx <= 1; dx++)
    for (let dy = -1; dy <= 1; dy++) {
      for (const i of g.buckets.get(`${gx + dx},${gy + dy}`) ?? []) {
        if (colors && colors[i * 4 + 3] === 0) continue;
        const ex = centroids[2 * i] - x, ey = centroids[2 * i + 1] - y;
        const d = ex * ex + ey * ey;
        if (d < bestD) {
          bestD = d;
          best = i + 1;
        }
      }
    }
  return best;
}
