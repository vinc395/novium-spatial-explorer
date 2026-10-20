import { describe, expect, it } from "vitest";
import { hexToRgb, mixChannels } from "./viewer/morphology";

describe("morphology mixing", () => {
  const gray = new Map([
    ["dapi", Uint8Array.from([0, 255, 128, 255])],
    ["bnd", Uint8Array.from([0, 0, 255, 255])],
  ]);
  const blue = { id: "dapi", rgb: [0, 0, 255] as [number, number, number], gain: 1, opacity: 1 };
  const green = { id: "bnd", rgb: [0, 255, 0] as [number, number, number], gain: 1, opacity: 1 };

  it("leaves pixels without signal transparent", () => {
    const o = mixChannels(gray, 4, [blue, green]);
    expect([...o.slice(0, 4)]).toEqual([0, 0, 0, 0]);
  });
  it("colours one channel at full strength, alpha = intensity", () => {
    const o = mixChannels(gray, 4, [blue]);
    expect([...o.slice(4, 8)]).toEqual([0, 0, 255, 255]);
    expect([...o.slice(8, 12)]).toEqual([0, 0, 255, 128]); // half intensity: same colour, half alpha
  });
  it("adds channels (blue + green = cyan) and applies brightness and opacity", () => {
    const o = mixChannels(gray, 4, [blue, green]);
    expect([...o.slice(12, 16)]).toEqual([0, 255, 255, 255]);
    const bright = mixChannels(gray, 4, [{ ...blue, gain: 2 }]);
    expect(bright[11]).toBe(255); // 128 x 2 saturates
    const faint = mixChannels(gray, 4, [{ ...blue, opacity: 0.5 }]);
    expect(faint[7]).toBe(128);
  });
  it("parses colours", () => {
    expect(hexToRgb("#3c82ff")).toEqual([60, 130, 255]);
    expect(hexToRgb("nonsense")).toEqual([255, 255, 255]);
  });
});
