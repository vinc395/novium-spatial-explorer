import { describe, expect, it } from "vitest";
import { categoricalColor } from "./colors";
import { categoryPalette, isDefaultChoice, loadPalettes, rgbaHex, savePalettes, schemeColor, shuffledOrder } from "./palettes";

const names = Array.from({ length: 30 }, (_, i) => `type ${String(i).padStart(2, "0")}`);

describe("category colours", () => {
  it("keep the original colours by default", () => {
    const p = categoryPalette(names);
    names.forEach((n, i) => expect(p.get(n)).toEqual(categoricalColor(i)));
    expect(isDefaultChoice(undefined) && isDefaultChoice({ scheme: "default", seed: 0, custom: {} })).toBe(true);
  });

  it("shuffle to a fixed reordering of the same colours", () => {
    const order = shuffledOrder(names.length, 42);
    expect([...order].sort((a, b) => a - b)).toEqual(names.map((_, i) => i));
    expect(shuffledOrder(names.length, 42)).toEqual(order);
    expect(shuffledOrder(names.length, 43)).not.toEqual(order);
    const a = categoryPalette(names);
    const b = categoryPalette(names, { scheme: "default", seed: 42, custom: {} });
    const hexes = (m: Map<string, unknown>) => [...m.values()].map((c) => JSON.stringify(c)).sort();
    expect(hexes(b)).toEqual(hexes(a));
    expect(names.filter((n) => JSON.stringify(a.get(n)) !== JSON.stringify(b.get(n))).length).toBeGreaterThan(20);
  });

  it("use a scheme's own colours first, then distinct generated ones", () => {
    const p = categoryPalette(names, { scheme: "okabe", seed: 0, custom: {} });
    expect(rgbaHex(p.get(names[0])!)).toBe("#e69f00");
    expect(rgbaHex(p.get(names[6])!)).toBe("#cc79a7");
    expect(new Set([...p.values()].map((c) => rgbaHex(c))).size).toBe(names.length);
    const r = [0, 1, 2].map((j) => schemeColor("rainbow", j, 3));
    expect(new Set(r.map((c) => rgbaHex(c))).size).toBe(3);
  });

  it("let colours picked by hand win over the scheme and the shuffle", () => {
    const p = categoryPalette(names, { scheme: "set1", seed: 7, custom: { [names[3]]: "#123456" } });
    expect(rgbaHex(p.get(names[3])!)).toBe("#123456");
  });

  it("survive without browser storage", () => {
    expect(loadPalettes()).toEqual({});
    expect(() => savePalettes({ f: { scheme: "kelly", seed: 0, custom: {} } })).not.toThrow();
  });
});
