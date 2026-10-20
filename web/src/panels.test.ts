import { describe, expect, it } from "vitest";
import { emptyPanel, getState, resetPanel, setPanel, setState } from "./store";
import type { Manifest } from "./types";

describe("switching a panel's section", () => {
  it("drops everything of the previous section until the new one loads", () => {
    setState({ panels: [emptyPanel(0)] });
    setPanel(0, {
      sectionId: "A",
      manifest: { section_id: "A" } as Manifest,
      centroids: new Float32Array(4),
      cellColors: new Uint8Array(8),
      loadError: "old error",
      selection: new Set([1, 2]),
    });
    resetPanel(0, { sectionId: "B" });
    const p = getState().panels[0];
    expect(p.sectionId).toBe("B");
    expect(p.manifest).toBeUndefined();
    expect(p.centroids).toBeUndefined();
    expect(p.cellColors).toBeUndefined();
    expect(p.loadError).toBeUndefined();
    expect(p.selection.size).toBe(0);
  });
});
