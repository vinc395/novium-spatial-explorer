// Smooth drawing of grids (display only; the grid values are unchanged), the way niche
// maps are often drawn: each label's indicator is interpolated with a cubic B-spline onto a
// raster F times finer, and each pixel shows the label with the largest value. This is
// scipy.ndimage.zoom(indicator, F, order=3, mode="grid-constant", grid_mode=True) per label
// followed by an argmax (checked against SciPy in gridSmooth.test.ts); outside the grid
// every indicator is 0. The spline interpolates, so every grid point keeps its own label
// at its centre. Missing points (code 0) count as a label of their own, compared last, so
// the tissue edge is smoothed too. Numeric grids: the values, and where they are present,
// are interpolated the same way.
//
// The work is split into steps (`step(0)` … `step(steps - 1)`, in order), so a caller can
// spread it over several tasks and keep the page responsive.

const Z = Math.sqrt(3) - 2; // the pole of the cubic B-spline prefilter

/** Cubic B-spline weights of the coefficients i-1, i, i+1, i+2 at t ∈ [0, 1) past i. */
function weights(t: number): number[] {
  const s = 1 - t, t2 = t * t, t3 = t2 * t;
  return [(s * s * s) / 6, (3 * t3 - 6 * t2 + 4) / 6, (-3 * t3 + 3 * t2 + 3 * t + 1) / 6, t3 / 6];
}

interface Tap {
  /** the first of the four coefficients, relative to the cell */
  first: number;
  w: number[];
}

/** Per output pixel k of a cell, its coefficients per axis. Pixel k sits u = (k + ½)/F − ½
 * cells from the cell's centre. */
function taps(F: number): Tap[] {
  return Array.from({ length: F }, (_, k) => {
    const u = (k + 0.5) / F - 0.5;
    const shift = u < 0 ? -1 : 0;
    return { first: shift - 1, w: weights(u - shift) };
  });
}

/** In place: the B-spline coefficients of `n` samples `a[off + k·step]` that continue as
 * zeros beyond both ends (exact for any number of zeros there). */
function prefilterLine(a: Float32Array, off: number, n: number, step: number) {
  const end = off + (n - 1) * step;
  let c = 0;
  for (let i = off; i <= end; i += step) a[i] = c = 6 * a[i] + Z * c;
  a[end] = c = (Z / (Z * Z - 1)) * c;
  for (let i = end - step; i >= off; i -= step) a[i] = c = Z * (c - a[i]);
}

/** In place: the B-spline coefficients of a PW×PH array (zero beyond it): rows, then all
 * columns at once (the same recursion, row by row). */
function prefilter2d(a: Float32Array, PW: number, PH: number) {
  for (let y = 0; y < PH; y++) prefilterLine(a, y * PW, PW, 1);
  for (let x = 0; x < PW; x++) a[x] *= 6;
  for (let r = PW; r < PW * PH; r += PW) for (let x = 0; x < PW; x++) a[r + x] = 6 * a[r + x] + Z * a[r - PW + x];
  const last = PW * (PH - 1), k = Z / (Z * Z - 1);
  for (let x = 0; x < PW; x++) a[last + x] *= k;
  for (let r = last - PW; r >= 0; r -= PW) for (let x = 0; x < PW; x++) a[r + x] = Z * (a[r + PW + x] - a[r + x]);
}

/** Upsampling factor for a grid: about 2 µm per pixel, at most `maxPixels` pixels in all
 * and `maxSide` per side (a texture size every WebGL2 GPU takes). */
export function smoothFactor(width: number, height: number, spacingUm: number, maxPixels = 16_000_000, maxSide = 4096): number {
  let f = Math.max(1, Math.min(10, Math.round(spacingUm / 2)));
  while (f > 1 && (width * height * f * f > maxPixels || Math.max(width, height) * f > maxSide)) f--;
  return f;
}

export interface Smoother<T> {
  /** (W·F)×(H·F), row-major; complete after the last step. */
  out: T;
  steps: number;
  /** Step i of `steps`; run them in order. */
  step(i: number): void;
}

/** Smooths label codes (W×H, row-major, 0 = missing) to F× the resolution. Grids keep a
 * border of two cells around them (`P…` sizes) so every spline coefficient a pixel reads
 * is stored. */
export function labelSmoother(codes: ArrayLike<number>, W: number, H: number, F: number): Smoother<Uint8Array | Uint16Array | Uint32Array> {
  const OW = W * F, PW = W + 4, PH = H + 4;
  const OUTSIDE = 0xffffffff;
  const pad = new Uint32Array(PW * PH).fill(OUTSIDE);
  let maxCode = 0;
  for (let y = 0; y < H; y++) {
    for (let x = 0; x < W; x++) {
      const c = codes[y * W + x];
      pad[(y + 2) * PW + x + 2] = c;
      if (c > maxCode) maxCode = c;
    }
  }
  const n = OW * H * F;
  const out = maxCode < 256 ? new Uint8Array(n) : maxCode < 65536 ? new Uint16Array(n) : new Uint32Array(n);
  const has = new Uint8Array(maxCode + 1);
  for (let i = 0; i < W * H; i++) has[codes[i]] = 1;
  const present: number[] = [];
  for (let c = 0; c <= maxCode; c++) if (has[c]) present.push(c);
  // The spline coefficients of each label's indicator (the first steps).
  const coef: Float32Array[] = new Array(maxCode + 1);
  const makeCoef = (l: number) => {
    const a = new Float32Array(PW * PH);
    for (let i = 0; i < PW * PH; i++) if (pad[i] === l) a[i] = 1;
    prefilter2d(a, PW, PH);
    coef[l] = a;
  };
  // Pixel (kx, ky) of a cell reads 4×4 coefficients around it: where they start relative
  // to the cell's 5×5 window, and their weights.
  const pre = taps(F);
  const start = new Int32Array(F * F);
  const wts = new Float64Array(F * F * 16);
  for (let ky = 0; ky < F; ky++) {
    for (let kx = 0; kx < F; kx++) {
      const p = ky * F + kx;
      start[p] = (pre[ky].first + 2) * PW + pre[kx].first + 2;
      for (let j = 0; j < 4; j++) for (let i = 0; i < 4; i++) wts[p * 16 + j * 4 + i] = pre[ky].w[j] * pre[kx].w[i];
    }
  }
  // Candidates for a cell: the labels within three cells (a label further away weighs
  // too little to win). When the cells within two all agree, so does every pixel.
  const labels = new Uint32Array(49);
  const rank = (l: number) => (l === 0 ? Infinity : l); // equal values: lower label first, missing last
  const row = (gy: number) => {
    for (let gx = 0; gx < W; gx++) {
      let nl = 0, inner = -1, uniform = true;
      for (let wy = -3; wy <= 3; wy++) {
        const y = gy + 2 + wy;
        if (y < 0 || y >= PH) continue;
        for (let wx = -3; wx <= 3; wx++) {
          const x = gx + 2 + wx;
          if (x < 0 || x >= PW) continue;
          const c = pad[y * PW + x];
          if (c === OUTSIDE) continue;
          if (Math.abs(wx) <= 2 && Math.abs(wy) <= 2) {
            if (inner < 0) inner = c;
            else if (c !== inner) uniform = false;
          }
          let l = 0;
          while (l < nl && labels[l] !== c) l++;
          if (l === nl) labels[nl++] = c;
        }
      }
      if (uniform) {
        for (let ky = 0; ky < F; ky++) out.fill(inner, (gy * F + ky) * OW + gx * F, (gy * F + ky) * OW + gx * F + F);
        continue;
      }
      for (let ky = 0; ky < F; ky++) {
        for (let kx = 0; kx < F; kx++) {
          const p = ky * F + kx, base = gy * PW + gx + start[p], w = p * 16;
          let best = -Infinity, bestL = 0;
          for (let c = 0; c < nl; c++) {
            const l = labels[c], a = coef[l];
            let v = 0;
            for (let j = 0; j < 4; j++) {
              const r = base + j * PW, q = w + j * 4;
              v += wts[q] * a[r] + wts[q + 1] * a[r + 1] + wts[q + 2] * a[r + 2] + wts[q + 3] * a[r + 3];
            }
            v = Math.fround(v); // compared as float32, as SciPy's output is
            if (v > best || (v === best && rank(l) < rank(bestL))) {
              best = v;
              bestL = l;
            }
          }
          out[(gy * F + ky) * OW + gx * F + kx] = bestL;
        }
      }
    }
  };
  return { out, steps: present.length + H, step: (i) => (i < present.length ? makeCoef(present[i]) : row(i - present.length)) };
}

/** The spline of the indicator of [0, n) along one axis, at the n·F output positions. */
function inside(n: number, F: number, pre: Tap[]): Float32Array {
  const a = new Float32Array(n + 4).fill(1, 2, n + 2);
  prefilterLine(a, 0, n + 4, 1);
  const o = new Float32Array(n * F);
  for (let g = 0; g < n; g++) {
    for (let k = 0; k < F; k++) {
      const { first, w } = pre[k], b = g + first + 2;
      o[g * F + k] = w[0] * a[b] + w[1] * a[b + 1] + w[2] * a[b + 2] + w[3] * a[b + 3];
    }
  }
  return o;
}

/** Smooths values (W×H, row-major, NaN = missing) to F× the resolution: value × present
 * and present are interpolated, each pixel shows their ratio, and it is missing where
 * missing would win as a label (where present weighs less than the rest of the grid). */
export function valueSmoother(values: ArrayLike<number>, W: number, H: number, F: number): Smoother<Float32Array> {
  const OW = W * F, PW = W + 4, PH = H + 4;
  const out = new Float32Array(OW * H * F);
  const v = new Float32Array(PW * PH), p = new Float32Array(PW * PH);
  for (let y = 0; y < H; y++) {
    for (let x = 0; x < W; x++) {
      const val = values[y * W + x];
      if (Number.isFinite(val)) {
        v[(y + 2) * PW + x + 2] = val;
        p[(y + 2) * PW + x + 2] = 1;
      }
    }
  }
  const pre = taps(F);
  // The grid's own indicator is separable: its spline along each axis.
  const gx = inside(W, F, pre), gy = inside(H, F, pre);
  // v and p along x, for every row of coefficients.
  const vx = new Float32Array(PH * OW), px = new Float32Array(PH * OW);
  const alongX = (a: Float32Array, ax: Float32Array, r: number) => {
    for (let g = 0; g < W; g++) {
      for (let k = 0; k < F; k++) {
        const { first, w } = pre[k], b = r * PW + g + first + 2;
        ax[r * OW + g * F + k] = w[0] * a[b] + w[1] * a[b + 1] + w[2] * a[b + 2] + w[3] * a[b + 3];
      }
    }
  };
  const step = (i: number) => {
    if (i < 2) return prefilter2d(i === 0 ? v : p, PW, PH);
    i -= 2;
    if (i < PH) {
      alongX(v, vx, i);
      alongX(p, px, i);
      return;
    }
    i -= PH; // the output rows of grid row i
    for (let k = 0; k < F; k++) {
      const { first, w } = pre[k], oy = i * F + k, b = (i + first + 2) * OW, g = gy[oy];
      for (let ox = 0; ox < OW; ox++) {
        const P = w[0] * px[b + ox] + w[1] * px[b + OW + ox] + w[2] * px[b + 2 * OW + ox] + w[3] * px[b + 3 * OW + ox];
        const V = w[0] * vx[b + ox] + w[1] * vx[b + OW + ox] + w[2] * vx[b + 2 * OW + ox] + w[3] * vx[b + 3 * OW + ox];
        out[oy * OW + ox] = P > 0 && 2 * P >= g * gx[ox] ? V / P : NaN;
      }
    }
  };
  return { out, steps: 2 + PH + H, step };
}

function run<T>(s: Smoother<T>): T {
  for (let i = 0; i < s.steps; i++) s.step(i);
  return s.out;
}

/** Label codes (W×H, 0 = missing) at F× the resolution, in one go. */
export const smoothLabels = (codes: ArrayLike<number>, W: number, H: number, F: number) => run(labelSmoother(codes, W, H, F));

/** Values (W×H, NaN = missing) at F× the resolution, in one go. */
export const smoothValues = (values: ArrayLike<number>, W: number, H: number, F: number) => run(valueSmoother(values, W, H, F));
