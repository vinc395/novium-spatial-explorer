import { describe, expect, it } from "vitest";
import { maskOverlay, overlayColorKey } from "./data";
import { initialState } from "./store";

describe("maskOverlay", () => {
  it("keeps overlay colours and hides them where the base colouring hides the cell", () => {
    // Cells: shown + overlaid, shown + no overlay value, hidden base category.
    const base = Uint8Array.from([200, 0, 0, 255, 0, 200, 0, 255, 0, 0, 200, 0]);
    const over = Uint8Array.from([0, 0, 255, 180, 9, 9, 9, 0, 0, 0, 255, 255]);
    const out = maskOverlay(base, over);
    expect([...out.slice(0, 4)]).toEqual([0, 0, 255, 180]);
    expect(out[7]).toBe(0);
    expect(out[11]).toBe(0);
    expect([...over.slice(8, 12)]).toEqual([0, 0, 255, 255]); // input untouched
  });
});

describe("overlayColorKey", () => {
  it("ignores drawing-only settings, so sliders do not recolour", () => {
    const o = { ...initialState.overlay, source: "lr:x" };
    const k = overlayColorKey(o);
    expect(overlayColorKey({ ...o, opacity: 0.2, outline: true, outlineWidth: 4, fill: false })).toBe(k);
    expect(overlayColorKey({ ...o, colormap: "viridis" })).not.toBe(k);
    expect(overlayColorKey({ ...o, hidden: new Set(["a"]) })).not.toBe(k);
  });
});
