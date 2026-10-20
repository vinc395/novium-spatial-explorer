import { describe, expect, it } from "vitest";
import { legendSvg } from "./export/legendSvg";

describe("legend SVG", () => {
  it("lists the shown categories, ramps and channels, with names escaped", () => {
    const svg = legendSvg([
      {
        legend: {
          kind: "categorical",
          label: "cell type",
          categories: [
            { code: 1, name: "T & NK", count: 5, color: [255, 0, 0, 255] },
            { code: 2, name: "B <cell>", count: 3, color: [0, 0, 255, 255] },
            { code: 3, name: "switched off", count: 1, color: [0, 255, 0, 255] },
          ],
        },
        hidden: new Set(["switched off"]),
      },
      { heading: "Overlay", legend: { kind: "numeric", label: "score", range: [0, 2.5], colormap: "magma", log: true } },
      { heading: "Morphology", channels: [{ name: "DAPI", color: "#4682ff" }] },
      { legend: { kind: "none", label: "Single colour" } },
    ]);
    expect(svg.startsWith("<svg xmlns=")).toBe(true);
    expect(svg).toContain("T &amp; NK");
    expect(svg).toContain("B &lt;cell&gt;");
    expect(svg).not.toContain("switched off");
    expect(svg).toContain('fill="rgb(255,0,0)"');
    expect(svg).toContain("Overlay · score");
    expect(svg).toMatch(/<linearGradient[^>]*>.*#000004.*#fcfdbf/);
    expect(svg).toContain("log1p scale");
    expect(svg).toContain(">2.50<");
    expect(svg).toContain('fill="#4682ff"');
    expect(svg).not.toContain("Single colour");
    // Every element closed.
    expect((svg.match(/<(rect|text|stop)\b/g) ?? []).length).toBe((svg.match(/(\/>|<\/text>)/g) ?? []).length);
  });
});
