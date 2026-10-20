import { describe, expect, it } from "vitest";
import { isLr, lrRows } from "./lr";
import type { FieldInfo } from "./types";

const f = (id: string, group: string, label = id) => ({ id, group, label, kind: "numeric" }) as FieldInfo;

describe("LR values at a cell", () => {
  const fields = [
    f("cell_area", "cells"),
    f("cs:diff:A:score", "lr_score", "L1 → R1 · diffusion score"),
    f("cs:diff:A:hotspot", "lr_hotspot"),
    f("cs:diff:B:score", "lr_score", "L2 → R2 · diffusion score"),
    f("cs:diff:B:hotspot", "lr_hotspot"),
    f("cs:cont:C:score", "lr_score", "L3 → R3 · contact score"),
    f("cs:cont:C:hotspot", "lr_hotspot"),
    f("cs:diff:D:score", "lr_score", "L4 → R4 · diffusion score"),
    f("lr:P1:sender", "lr_score", "P1 (sender)"),
  ];

  it("pairs scores with hotspots and puts hotspots, then high scores, first", () => {
    const rows = lrRows(fields, {
      "cs:diff:A:score": 0.5,
      "cs:diff:A:hotspot": "background",
      "cs:diff:B:score": 0.2,
      "cs:diff:B:hotspot": "hotspot",
      "cs:cont:C:score": 0.1,
      "cs:cont:C:hotspot": "hotspot (HQ)",
      "cs:diff:D:score": null,
      "lr:P1:sender": 0.9,
    });
    expect(rows.map((r) => [r.label, r.score, r.hotspot])).toEqual([
      ["L3 → R3 · contact", 0.1, "hotspot (HQ)"],
      ["L2 → R2 · diffusion", 0.2, "hotspot"],
      ["P1 (sender)", 0.9, null],
      ["L1 → R1 · diffusion", 0.5, "background"],
      ["L4 → R4 · diffusion", null, null],
    ]);
  });

  it("lists only fields that came back and tells LR fields apart", () => {
    expect(lrRows(fields, { cell_area: 3 })).toEqual([]);
    expect(fields.filter(isLr).length).toBe(8);
  });
});
