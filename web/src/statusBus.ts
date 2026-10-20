// High-frequency viewer status (cursor, zoom, LOD, tile counts, fps) kept out of
// React state; the status bar subscribes and repaints at most once per frame.

export interface ViewerStatus {
  cursor?: [number, number];
  umPerPx: number;
  lodCells: string;
  lodTx: string;
  tilesCells: string;
  tilesTx: string;
  fps: number;
}

let status: ViewerStatus = { umPerPx: 1, lodCells: "", lodTx: "", tilesCells: "", tilesTx: "", fps: 0 };
const ls = new Set<() => void>();
let scheduled = false;

export function setStatus(p: Partial<ViewerStatus>) {
  status = { ...status, ...p };
  if (!scheduled) {
    scheduled = true;
    requestAnimationFrame(() => {
      scheduled = false;
      ls.forEach((l) => l());
    });
  }
}
export function getStatus() {
  return status;
}
export function subscribeStatus(l: () => void): () => void {
  ls.add(l);
  return () => {
    ls.delete(l);
  };
}
