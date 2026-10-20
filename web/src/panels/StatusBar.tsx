import { useSyncExternalStore } from "react";
import { getStatus, subscribeStatus } from "../statusBus";

function niceLength(umPerPx: number, targetPx = 110) {
  const raw = umPerPx * targetPx;
  const p = 10 ** Math.floor(Math.log10(raw));
  const n = [1, 2, 5, 10].find((k) => k * p >= raw) ?? 10;
  const um = n * p;
  return { um, px: um / umPerPx };
}

export function StatusBar() {
  const st = useSyncExternalStore(subscribeStatus, getStatus);
  const sb = niceLength(st.umPerPx);
  return (
    <footer className="status">
      <span className="scalebar" aria-label="scale bar">
        <span className="bar" style={{ width: `${sb.px}px` }} />
        {sb.um >= 1000 ? `${sb.um / 1000} mm` : `${sb.um} µm`}
      </span>
      <span>{st.cursor ? `x ${st.cursor[0].toFixed(1)} µm, y ${st.cursor[1].toFixed(1)} µm` : "—"}</span>
      <span>{st.umPerPx < 1 ? `${(1 / st.umPerPx).toFixed(1)} px/µm` : `${st.umPerPx.toFixed(2)} µm/px`}</span>
      <span>cells: {st.lodCells} {st.tilesCells && `· ${st.tilesCells}`}</span>
      <span>transcripts: {st.lodTx} {st.tilesTx && `· ${st.tilesTx}`}</span>
      <span className="fps">{st.fps ? `${st.fps} fps` : "idle"}</span>
    </footer>
  );
}
