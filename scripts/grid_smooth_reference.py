#!/usr/bin/env python3
# Writes web/src/gridSmooth.scipy.json, the reference the smooth grid drawing
# (web/src/gridSmooth.ts) is tested against: each label's indicator zoomed with SciPy's
# cubic spline, then the largest per pixel (missing last). Synthetic labels only.
#   python3 scripts/grid_smooth_reference.py web/src/gridSmooth.scipy.json   (needs numpy, scipy)
import base64, json, sys
import numpy as np
import scipy
from scipy import ndimage as ndi

def interpolate(G, ncls, F):
    H, W = G.shape
    best = np.full((H * F, W * F), -9.0, np.float32); arg = np.zeros((H * F, W * F), np.int16)
    for k in range(ncls):
        m = (G == k)
        if not m.any():
            continue
        g = ndi.zoom(m.astype(np.float32), F, order=3, mode="grid-constant", grid_mode=True, prefilter=True)
        upd = g > best; best[upd] = g[upd]; arg[upd] = k
    return arg

def labels(W, H, K, seed, margin):
    rng = np.random.default_rng(seed)
    pts = rng.uniform([0, 0], [W, H], size=(int(W * H / 40), 2)); lab = rng.integers(0, K, len(pts))
    yy, xx = np.mgrid[0:H, 0:W]
    d = (xx[..., None] - pts[:, 0]) ** 2 + (yy[..., None] - pts[:, 1]) ** 2
    G = lab[d.argmin(-1)]
    specks = rng.random((H, W)) < 0.03
    G = np.where(specks, rng.integers(0, K, (H, W)), G)
    if margin:  # missing outside a ragged ellipse
        r = ((xx - W / 2) / (W * 0.46)) ** 2 + ((yy - H / 2) / (H * 0.44)) ** 2 + rng.normal(0, 0.06, (H, W))
        G = np.where(r > 1, -1, G)
    return G

cases = []
for W, H, K, seed, margin, F in [(48, 36, 6, 1, True, 5), (48, 36, 6, 1, True, 3), (30, 22, 4, 7, False, 5), (40, 28, 12, 3, True, 2)]:
    G = labels(W, H, K, seed, margin)
    arg = interpolate(np.where(G >= 0, G, K), K + 1, F)
    codes = np.where(G >= 0, G + 1, 0).astype(np.uint8)          # viewer codes: label k -> k + 1, missing -> 0
    exp = np.where(arg < K, arg + 1, 0).astype(np.uint8)
    cases.append({"W": W, "H": H, "F": F, "codes": base64.b64encode(codes.tobytes()).decode(), "expected": base64.b64encode(exp.tobytes()).decode()})
json.dump({"source": f"scipy {scipy.__version__} ndimage.zoom(order=3, mode='grid-constant', grid_mode=True) per label, then argmax (missing as the last label); synthetic labels", "cases": cases}, open(sys.argv[1], "w"))
print("ok", scipy.__version__, [ (c["W"], c["H"], c["F"]) for c in cases])
